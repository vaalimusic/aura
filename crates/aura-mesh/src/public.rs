//! Opt-in public rooms. Noise protects each hop; the serving node can access audio.
use crate::{audio, identity, nodes::Nodes, Multiaddr, PeerId, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use futures::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, StreamExt};
use libp2p::{multiaddr::Protocol, swarm::SwarmEvent, StreamProtocol};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{
    sync::{broadcast, mpsc, watch},
    task::JoinSet,
    time::{timeout, Instant},
};

pub const PROTOCOL: StreamProtocol = StreamProtocol::new("/aura/public/2");
const LEGACY_PROTOCOL: StreamProtocol = StreamProtocol::new("/aura/public/1");
const MAX_PACKET: usize = aura_media::MAX_MESSAGE + 1;
const MAX_ROOMS: usize = 16;
const MAX_LISTENERS: usize = 32;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Info {
    pub owner: String,
    pub title: String,
    pub author: String,
    pub listeners: usize,
    pub track: String,
    pub artist: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Link {
    pub node: String,
    pub owner: String,
}
impl Link {
    pub fn parse(text: &str) -> Result<Self> {
        if text.len() > 4096 {
            return Err("Public link too large".into());
        }
        let value: Self = serde_json::from_slice(
            &URL_SAFE_NO_PAD.decode(
                text.trim()
                    .strip_prefix("aura://public/v1/")
                    .ok_or("Expected public Aura link")?,
            )?,
        )?;
        Nodes::new(vec![value.node.parse()?])?;
        value.owner.parse::<PeerId>()?;
        Ok(value)
    }
    pub fn to_link(&self) -> Result<String> {
        Ok(format!(
            "aura://public/v1/{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(self)?)
        ))
    }
}
#[derive(Clone)]
pub struct Entry {
    pub info: Info,
    pub link: Link,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum Request {
    List,
    Publish { title: String, author: String },
    Listen { owner: String },
}
#[derive(Serialize, Deserialize)]
enum Response {
    List(Vec<Info>),
    Ready,
    Error(String),
}

pub fn title(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty()
        || value.chars().count() > 80
        || value.len() > 320
        || value.chars().any(char::is_control)
    {
        return Err("Название эфира: от 1 до 80 символов.".into());
    }
    Ok(value.to_string())
}
async fn write_packet(writer: &mut (impl AsyncWrite + Unpin), bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_PACKET {
        return Err("Invalid public packet".into());
    }
    timeout(Duration::from_secs(3), async {
        writer
            .write_all(&(bytes.len() as u32).to_be_bytes())
            .await?;
        writer.write_all(bytes).await?;
        writer.flush().await
    })
    .await??;
    Ok(())
}
async fn read_packet(reader: &mut (impl AsyncRead + Unpin)) -> Result<Vec<u8>> {
    let mut len = [0; 4];
    reader.read_exact(&mut len).await?;
    let size = u32::from_be_bytes(len) as usize;
    if size == 0 || size > MAX_PACKET {
        return Err("Invalid public packet size".into());
    }
    let mut bytes = vec![0; size];
    timeout(Duration::from_secs(5), reader.read_exact(&mut bytes)).await??;
    Ok(bytes)
}
async fn write_json(writer: &mut (impl AsyncWrite + Unpin), value: &impl Serialize) -> Result<()> {
    write_packet(writer, &serde_json::to_vec(value)?).await
}

struct Room {
    info: Info,
    frames: broadcast::Sender<Arc<Vec<u8>>>,
    metadata: Vec<u8>,
    listeners: Arc<AtomicUsize>,
    updated: Instant,
}
type Rooms = Arc<Mutex<HashMap<PeerId, Room>>>;
struct OwnerGuard {
    rooms: Rooms,
    owner: PeerId,
}
impl Drop for OwnerGuard {
    fn drop(&mut self) {
        self.rooms.lock().unwrap().remove(&self.owner);
    }
}
struct ListenerGuard(Arc<AtomicUsize>);
impl Drop for ListenerGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

pub struct Service {
    incoming: libp2p_stream::IncomingStreams,
    legacy: libp2p_stream::IncomingStreams,
    jobs: JoinSet<()>,
    rooms: Rooms,
    rates: HashMap<PeerId, (Instant, u8)>,
}
impl Service {
    pub fn new(mut control: libp2p_stream::Control) -> Result<Self> {
        Ok(Self {
            incoming: control.accept(PROTOCOL)?,
            legacy: control.accept(LEGACY_PROTOCOL)?,
            jobs: JoinSet::new(),
            rooms: Arc::default(),
            rates: HashMap::new(),
        })
    }
    pub async fn next(&mut self) {
        tokio::select! {
            Some(_)=self.jobs.join_next(),if !self.jobs.is_empty()=>{},
            (peer,stream,heartbeat)=async {tokio::select! {
                Some((peer,stream))=self.incoming.next()=>(peer,stream,true),
                Some((peer,stream))=self.legacy.next()=>(peer,stream,false),
            }}=>{
                self.rates.retain(|_,(t,_)|t.elapsed()<Duration::from_secs(10));
                if self.jobs.len()>=96||(!self.rates.contains_key(&peer)&&self.rates.len()>=256){return;}
                let rate=self.rates.entry(peer).or_insert((Instant::now(),0));
                if rate.1>=12{return;}rate.1+=1;
                let rooms=self.rooms.clone();
                self.jobs.spawn(async move{let _=serve(peer,stream,rooms,heartbeat).await;});
            }
        }
    }
}
async fn serve(
    peer: PeerId,
    mut stream: libp2p::Stream,
    rooms: Rooms,
    heartbeat: bool,
) -> Result<()> {
    let bytes = timeout(Duration::from_secs(5), read_packet(&mut stream)).await??;
    if bytes.len() > 4096 {
        return Err("Public request too large".into());
    }
    let request: Request = serde_json::from_slice(&bytes)?;
    match request {
        Request::List => {
            let list = rooms
                .lock()
                .unwrap()
                .values()
                .filter(|room| room.updated.elapsed() < Duration::from_secs(20))
                .map(|room| {
                    let mut info = room.info.clone();
                    info.listeners = room.listeners.load(Ordering::Relaxed);
                    info
                })
                .collect();
            write_json(&mut stream, &Response::List(list)).await?;
            // Keep the connection alive until the remote observes the response.
            let mut ack = [0; 1];
            let _ = timeout(Duration::from_secs(2), stream.read_exact(&mut ack)).await;
        }
        Request::Publish {
            title: label,
            author,
        } => {
            let label = title(&label)?;
            let author = aura_social::name(&author)?;
            let (frames, _) = broadcast::channel(16);
            let listeners = Arc::new(AtomicUsize::new(0));
            let accepted = {
                let mut map = rooms.lock().unwrap();
                if map.len() >= MAX_ROOMS || map.contains_key(&peer) {
                    false
                } else {
                    map.insert(
                        peer,
                        Room {
                            info: Info {
                                owner: peer.to_string(),
                                title: label,
                                author,
                                listeners: 0,
                                track: String::new(),
                                artist: String::new(),
                            },
                            frames: frames.clone(),
                            metadata: Vec::new(),
                            listeners: listeners.clone(),
                            updated: Instant::now(),
                        },
                    );
                    true
                }
            };
            if !accepted {
                write_json(
                    &mut stream,
                    &Response::Error("Узел занят или этот автор уже в эфире.".into()),
                )
                .await?;
                return Ok(());
            }
            let _guard = OwnerGuard {
                rooms: rooms.clone(),
                owner: peer,
            };
            write_json(&mut stream, &Response::Ready).await?;
            let (mut reader, mut writer) = stream.split();
            let ingest = async {
                let mut previous = None;
                let mut window = Instant::now();
                let mut messages = 0;
                let mut media_messages = 0;
                loop {
                    let packet =
                        timeout(Duration::from_secs(15), read_packet(&mut reader)).await??;
                    if window.elapsed() >= Duration::from_secs(1) {
                        window = Instant::now();
                        messages = 0;
                        media_messages = 0;
                    }
                    messages += 1;
                    if messages > 120 {
                        return Err("Public message rate exceeded".into());
                    }
                    match packet[0] {
                        0 => {
                            let _ = frames.send(Arc::new(vec![0]));
                            break;
                        }
                        1 => {
                            if !(14..=1113).contains(&packet.len()) {
                                return Err("Invalid public audio".into());
                            }
                            let _ = frames.send(Arc::new(packet));
                        }
                        2 => {
                            media_messages += 1;
                            if media_messages > 4 {
                                return Err("Public metadata rate exceeded".into());
                            }
                            let snapshot = aura_media::decode(&packet[1..], &mut previous)?;
                            let mut full_metadata = vec![2];
                            full_metadata.extend(aura_media::encode(&snapshot, &mut None)?);
                            let mut map = rooms.lock().unwrap();
                            let room = map.get_mut(&peer).ok_or("Room ended")?;
                            if let Some(info) = snapshot.info {
                                room.info.track = info.title;
                                room.info.artist = info.artist;
                            } else {
                                room.info.track.clear();
                                room.info.artist.clear();
                            }
                            room.metadata = full_metadata;
                            let _ = frames.send(Arc::new(packet));
                        }
                        3 if packet.len() == 1 => {}
                        _ => return Err("Invalid public message".into()),
                    }
                    if let Some(room) = rooms.lock().unwrap().get_mut(&peer) {
                        room.updated = Instant::now();
                    }
                }
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
            };
            let stats = async {
                let mut tick = tokio::time::interval(Duration::from_secs(1));
                loop {
                    tick.tick().await;
                    let mut packet = vec![3];
                    packet.extend_from_slice(
                        &(listeners.load(Ordering::Relaxed) as u32).to_be_bytes(),
                    );
                    write_packet(&mut writer, &packet).await?;
                }
                #[allow(unreachable_code)]
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
            };
            tokio::select! {result=ingest=>result?,result=stats=>result?}
            // Acknowledge explicit Stop while the publisher still drives its swarm.
            write_packet(&mut writer, &[0]).await?;
        }
        Request::Listen { owner } => {
            let owner = owner.parse::<PeerId>()?;
            let subscription = {
                let map = rooms.lock().unwrap();
                map.get(&owner).and_then(|room| {
                    if room.listeners.load(Ordering::Relaxed) >= MAX_LISTENERS {
                        None
                    } else {
                        room.listeners.fetch_add(1, Ordering::Relaxed);
                        Some((
                            room.frames.subscribe(),
                            room.metadata.clone(),
                            ListenerGuard(room.listeners.clone()),
                        ))
                    }
                })
            };
            let Some((mut frames, metadata, _guard)) = subscription else {
                write_json(
                    &mut stream,
                    &Response::Error("Эфир завершён или достигнут лимит слушателей.".into()),
                )
                .await?;
                return Ok(());
            };
            write_json(&mut stream, &Response::Ready).await?;
            let (mut reader, mut writer) = stream.split();
            if !metadata.is_empty() {
                write_packet(&mut writer, &metadata).await?;
            }
            let mut tick = tokio::time::interval(Duration::from_secs(2));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                let mut probe = [0; 1];
                let result = tokio::select! {_=reader.read(&mut probe)=>break,result=frames.recv()=>Some(result),_=tick.tick(),if heartbeat=>None};
                let packet = match result {
                    Some(Ok(packet)) => packet,
                    Some(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                    // A lost publisher is a transport failure, not an intentional Stop.
                    // EOF lets upgraded listeners follow the author to another node.
                    Some(Err(_)) => break,
                    None => Arc::new(vec![3]),
                };
                write_packet(&mut writer, &packet).await?;
                if packet[0] == 0 {
                    let mut ack = [0; 1];
                    let _ = timeout(Duration::from_secs(1), reader.read_exact(&mut ack)).await;
                    break;
                }
            }
        }
    }
    Ok(())
}

async fn drive<T>(
    swarm: &mut libp2p::Swarm<audio::Behaviour>,
    job: impl std::future::Future<Output = Result<T>>,
    stop: &mut watch::Receiver<bool>,
) -> Result<T> {
    if *stop.borrow() {
        return Err("Cancelled".into());
    }
    tokio::pin!(job);
    loop {
        tokio::select! {biased;_=stop.changed()=>return Err("Cancelled".into()),result=&mut job=>return result,_=swarm.select_next_some()=>{}}
    }
}
struct Client {
    swarm: libp2p::Swarm<audio::Behaviour>,
    stream: libp2p::Stream,
    heartbeat: bool,
}
impl Client {
    async fn connect(
        address: Multiaddr,
        key: identity::Keypair,
        request: Request,
        stop: &mut watch::Receiver<bool>,
    ) -> Result<(Self, Response)> {
        Nodes::new(vec![address.clone()])?;
        let Some(Protocol::P2p(peer)) = address.iter().last() else {
            return Err("Missing node identity".into());
        };
        let mut swarm = audio::build(key)?;
        swarm.dial(address)?;
        let connected = async {
            loop {
                match swarm.select_next_some().await {
                    SwarmEvent::ConnectionEstablished { peer_id, .. } if peer_id == peer => {
                        break Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
                    }
                    SwarmEvent::OutgoingConnectionError { error, .. } => break Err(error.into()),
                    _ => {}
                }
            }
        };
        tokio::select! {biased;_=stop.changed()=>return Err("Cancelled".into()),r=timeout(Duration::from_secs(8),connected)=>r??}
        let mut control = swarm.behaviour().streams.new_control();
        let job = async {
            let (mut stream, heartbeat) =
                match timeout(Duration::from_secs(5), control.open_stream(peer, PROTOCOL)).await? {
                    Ok(stream) => (stream, true),
                    Err(libp2p_stream::OpenStreamError::UnsupportedProtocol(_)) => (
                        timeout(
                            Duration::from_secs(5),
                            control.open_stream(peer, LEGACY_PROTOCOL),
                        )
                        .await??,
                        false,
                    ),
                    Err(error) => return Err(error.into()),
                };
            write_json(&mut stream, &request).await?;
            let response = timeout(Duration::from_secs(5), read_packet(&mut stream)).await??;
            let response = serde_json::from_slice(&response)?;
            Ok((stream, response, heartbeat))
        };
        let (stream, response, heartbeat) = drive(&mut swarm, job, stop).await?;
        Ok((
            Self {
                swarm,
                stream,
                heartbeat,
            },
            response,
        ))
    }
}
pub async fn catalog(
    nodes: Nodes,
    key: identity::Keypair,
    stop: watch::Receiver<bool>,
) -> Result<(Vec<Entry>, usize)> {
    let mut jobs = futures::stream::FuturesUnordered::new();
    for node in nodes.addresses() {
        let key = key.clone();
        let mut stopped = stop.clone();
        jobs.push(async move {
            let (mut client, response) =
                Client::connect(node.clone(), key, Request::List, &mut stopped).await?;
            let Response::List(list) = response else {
                return Err("Invalid catalog response".into());
            };
            if list.len() > MAX_ROOMS {
                return Err("Catalog too large".into());
            }
            let mut entries = Vec::new();
            for info in list {
                info.owner.parse::<PeerId>()?;
                title(&info.title)?;
                aura_social::name(&info.author)?;
                if info.track.len() > 1024 || info.artist.len() > 1024 {
                    return Err("Invalid catalog metadata".into());
                }
                entries.push(Entry {
                    link: Link {
                        node: node.to_string(),
                        owner: info.owner.clone(),
                    },
                    info,
                });
            }
            let _ = drive(
                &mut client.swarm,
                async {
                    client.stream.write_all(b"A").await?;
                    client.stream.flush().await?;
                    Ok(())
                },
                &mut stopped,
            )
            .await;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(entries)
        });
    }
    let mut available = 0;
    let mut result = Vec::new();
    while let Some(response) = jobs.next().await {
        if let Ok(entries) = response {
            available += 1;
            result.extend(entries);
        }
    }
    if available == 0 {
        return Err("Каталог недоступен. Проверьте подключение к Aura Nodes.".into());
    }
    result.sort_by(|a, b| a.info.owner.cmp(&b.info.owner));
    result.dedup_by(|a, b| a.info.owner == b.info.owner);
    Ok((result, available))
}

pub struct Publisher {
    client: Client,
    pub link: Link,
    nodes: Nodes,
    key: identity::Keypair,
    label: String,
    author: String,
}
impl Publisher {
    pub async fn connect(
        nodes: Nodes,
        key: identity::Keypair,
        label: String,
        author: String,
        mut stop: watch::Receiver<bool>,
    ) -> Result<Self> {
        let label = title(&label)?;
        aura_social::name(&author)?;
        let owner = key.public().to_peer_id().to_string();
        let mut last = String::new();
        for node in nodes.addresses() {
            if *stop.borrow() {
                return Err("Cancelled".into());
            }
            match Client::connect(
                node.clone(),
                key.clone(),
                Request::Publish {
                    title: label.clone(),
                    author: author.clone(),
                },
                &mut stop,
            )
            .await
            {
                Ok((client, Response::Ready)) => {
                    return Ok(Self {
                        client,
                        link: Link {
                            node: node.to_string(),
                            owner,
                        },
                        nodes,
                        key,
                        label,
                        author,
                    })
                }
                Ok((_, Response::Error(error))) => last = error,
                Ok(_) => last = "Invalid public response".into(),
                Err(error) => last = error.to_string(),
            }
        }
        Err(format!("Не удалось начать публичный эфир: {last}").into())
    }
    pub async fn run(
        self,
        mut frames: mpsc::Receiver<Vec<u8>>,
        mut stop: watch::Receiver<bool>,
        mut metadata: watch::Receiver<aura_media::Snapshot>,
        mut report: impl FnMut(audio::Stats),
    ) -> Result<()> {
        let Self {
            mut client,
            link,
            nodes,
            key,
            label,
            author,
        } = self;
        let mut current = link.node;
        loop {
            let result =
                Self::run_connection(client, &mut frames, &mut stop, &mut metadata, &mut report)
                    .await;
            if *stop.borrow() || frames.is_closed() && frames.is_empty() {
                return Ok(());
            }
            if result.is_ok() {
                return result;
            }
            report(audio::Stats {
                recovering: true,
                ..Default::default()
            });
            let deadline = Instant::now() + Duration::from_secs(60);
            // Start with the next node, rather than repeatedly dialing the failed one.
            let mut routes = nodes.addresses().to_vec();
            if let Some(index) = routes.iter().position(|n| n.to_string() == current) {
                let count = routes.len();
                routes.rotate_left((index + 1) % count);
            }
            let mut attempt = 0;
            loop {
                if Instant::now() >= deadline {
                    return Err("Не удалось восстановить публичный эфир за 60 секунд.".into());
                }
                let node = routes[attempt % routes.len()].clone();
                attempt += 1;
                let mut connecting_stop = stop.clone();
                let reconnect = Client::connect(
                    node.clone(),
                    key.clone(),
                    Request::Publish {
                        title: label.clone(),
                        author: author.clone(),
                    },
                    &mut connecting_stop,
                );
                tokio::pin!(reconnect);
                let connected = loop {
                    tokio::select! { biased;
                        result=&mut reconnect=>break Some(result),
                        _=tokio::time::sleep_until(deadline)=>break None,
                        frame=frames.recv()=>if frame.is_none(){return Ok(());},
                    }
                };
                if *stop.borrow() {
                    return Ok(());
                }
                match connected {
                    Some(Ok((next, Response::Ready))) => {
                        client = next;
                        current = node.to_string();
                        // Capture keeps running during recovery; never replay its old backlog.
                        while frames.try_recv().is_ok() {}
                        report(audio::Stats::default());
                        break;
                    }
                    None => {
                        return Err("Не удалось восстановить публичный эфир за 60 секунд.".into())
                    }
                    _ => {}
                }
                if !wait_retry(&mut stop, deadline).await {
                    return Ok(());
                }
            }
        }
    }
    async fn run_connection(
        client: Client,
        frames: &mut mpsc::Receiver<Vec<u8>>,
        stop: &mut watch::Receiver<bool>,
        metadata: &mut watch::Receiver<aura_media::Snapshot>,
        report: &mut impl FnMut(audio::Stats),
    ) -> Result<()> {
        let Client {
            mut swarm, stream, ..
        } = client;
        let (mut reader, mut writer) = stream.split();
        let output = async {
            let mut tick = tokio::time::interval(Duration::from_secs(2));
            let mut media_tick = tokio::time::interval(Duration::from_millis(500));
            media_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut last_metadata = Instant::now() - Duration::from_secs(1);
            let mut previous_artwork = None;
            let mut meta_open = true;
            let mut pending_meta = true;
            loop {
                if *stop.borrow() {
                    write_packet(&mut writer, &[0]).await?;
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    return Ok(());
                }
                if pending_meta && last_metadata.elapsed() >= Duration::from_millis(500) {
                    let snapshot = metadata.borrow_and_update().clone();
                    let mut packet = vec![2];
                    packet.extend(aura_media::encode(&snapshot, &mut previous_artwork)?);
                    write_packet(&mut writer, &packet).await?;
                    pending_meta = false;
                    last_metadata = Instant::now();
                }
                let packet = tokio::select! {biased;
                    _=stop.changed()=>vec![0],
                    change=metadata.changed(),if meta_open=>{if change.is_err(){meta_open=false;}else{pending_meta=true;}continue;},
                    _=media_tick.tick(),if pending_meta=>continue,
                    frame=frames.recv()=>{let Some(frame)=frame else {return Ok(());};let mut packet=vec![1];packet.extend(frame);packet},
                    _=tick.tick()=>vec![3],
                };
                write_packet(&mut writer, &packet).await?;
                if packet[0] == 0 {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    return Ok(());
                }
            }
        };
        let input = async {
            loop {
                let packet = timeout(Duration::from_secs(10), read_packet(&mut reader)).await??;
                if packet == [0] {
                    return Ok(());
                }
                if packet.len() != 5 || packet[0] != 3 {
                    return Err("Invalid public statistics".into());
                }
                report(audio::Stats {
                    listeners: u32::from_be_bytes(packet[1..].try_into().unwrap()) as usize,
                    ..Default::default()
                });
            }
        };
        // The writer owns cancellation so it can send the room-end marker before closing.
        let job = async {
            tokio::select! {r=output=>r,r=input=>r}
        };
        let (_keep, mut never) = watch::channel(false);
        drive(&mut swarm, job, &mut never).await
    }
}
pub async fn receive(
    link: Link,
    key: identity::Keypair,
    stop: watch::Receiver<bool>,
    emit: impl FnMut(Vec<u8>) -> bool,
    ready: impl FnMut(),
    metadata: impl FnMut(aura_media::Snapshot),
) -> Result<()> {
    let nodes = Nodes::new(vec![link.node.parse()?])?;
    receive_with_nodes(link, key, nodes, stop, emit, ready, metadata, |_| {}).await
}

async fn wait_retry(stop: &mut watch::Receiver<bool>, deadline: Instant) -> bool {
    if *stop.borrow() {
        return false;
    }
    tokio::select! { biased;
        _=stop.changed()=>false,
        _=tokio::time::sleep_until((Instant::now()+Duration::from_secs(1)).min(deadline))=>true,
    }
}

/// Retry the same authenticated author only on the link's pinned node and locally configured nodes.
/// A normal end marker is terminal; transport failures get a bounded recovery window.
#[allow(clippy::too_many_arguments)]
pub async fn receive_with_nodes(
    link: Link,
    key: identity::Keypair,
    nodes: Nodes,
    mut stop: watch::Receiver<bool>,
    mut emit: impl FnMut(Vec<u8>) -> bool,
    mut ready: impl FnMut(),
    mut metadata: impl FnMut(aura_media::Snapshot),
    mut recovering: impl FnMut(bool),
) -> Result<()> {
    let original = link.node.parse::<Multiaddr>()?;
    Nodes::new(vec![original.clone()])?;
    link.owner.parse::<PeerId>()?;
    let mut routes = vec![original];
    routes.extend(
        nodes
            .addresses()
            .iter()
            .filter(|n| n.to_string() != link.node)
            .cloned(),
    );
    let mut deadline = None;
    let mut attempt = 0;
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        let route = routes[attempt % routes.len()].clone();
        attempt += 1;
        let mut connected = false;
        let job = receive_once(
            Link {
                node: route.to_string(),
                owner: link.owner.clone(),
            },
            key.clone(),
            stop.clone(),
            deadline,
            &mut emit,
            || {
                connected = true;
                recovering(false);
                ready();
            },
            &mut metadata,
        );
        let result = job.await;
        if *stop.borrow() {
            return Ok(());
        }
        if result.is_ok() {
            recovering(false);
            return Ok(());
        }
        if !connected && deadline.is_some_and(|until| Instant::now() >= until) {
            return Err("Не удалось восстановить публичный эфир за 60 секунд.".into());
        }
        // A new connection gets a fresh outage budget; failed dials never extend it.
        if connected || deadline.is_none() {
            deadline = Some(Instant::now() + Duration::from_secs(60));
            recovering(true);
        }
        if !wait_retry(&mut stop, deadline.unwrap()).await {
            return Ok(());
        }
    }
}

async fn receive_once(
    link: Link,
    key: identity::Keypair,
    mut stop: watch::Receiver<bool>,
    deadline: Option<Instant>,
    mut emit: impl FnMut(Vec<u8>) -> bool,
    mut ready: impl FnMut(),
    mut metadata: impl FnMut(aura_media::Snapshot),
) -> Result<()> {
    let connect = Client::connect(
        link.node.parse()?,
        key,
        Request::Listen { owner: link.owner },
        &mut stop,
    );
    // Bound only the handshake, never the lifetime of a recovered stream.
    let (mut client, response) = if let Some(until) = deadline {
        tokio::time::timeout_at(until, connect).await??
    } else {
        connect.await?
    };
    match response {
        Response::Ready => {}
        Response::Error(error) => return Err(error.into()),
        _ => return Err("Invalid public response".into()),
    }
    ready();
    let job = async {
        let mut previous = None;
        loop {
            let packet = if client.heartbeat {
                timeout(Duration::from_secs(10), read_packet(&mut client.stream)).await??
            } else {
                read_packet(&mut client.stream).await?
            };
            match packet[0] {
                0 => {
                    let _ = client.stream.write_all(b"E").await;
                    let _ = client.stream.flush().await;
                    return Ok(());
                }
                1 if (14..=1113).contains(&packet.len()) => {
                    emit(packet[1..].to_vec());
                }
                2 => metadata(aura_media::decode(&packet[1..], &mut previous)?),
                3 if client.heartbeat && packet.len() == 1 => {}
                _ => return Err("Invalid public audio message".into()),
            }
        }
    };
    let result = drive(&mut client.swarm, job, &mut stop).await;
    if *stop.borrow() {
        Ok(())
    } else {
        result
    }
}
