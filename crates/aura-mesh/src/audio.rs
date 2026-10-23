//! End-to-end Noise streams over Circuit Relay v2. The relay cannot decrypt audio.
use crate::{identity, Multiaddr, PeerId, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use futures::{AsyncReadExt, AsyncWriteExt, StreamExt};
use libp2p::{
    identify,
    multiaddr::Protocol,
    noise, ping, relay,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux, StreamProtocol, Swarm, SwarmBuilder,
};
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use subtle::ConstantTimeEq;
use tokio::{
    sync::{broadcast, mpsc, watch},
    task::JoinSet,
    time::{timeout, Instant},
};
const AUDIO: StreamProtocol = StreamProtocol::new("/aura/audio/2");
const METADATA: StreamProtocol = StreamProtocol::new("/aura/metadata/1");
const MAGIC: &[u8; 4] = b"AUM2";
const MAX_FRAME: usize = 1112;
pub const DEFAULT_RELAY: &str =
    "/dns4/aura.ltwo.ru/tcp/45071/p2p/12D3KooWGjkf7GrM1GxVmyVobwki2Mr7Uyi5PMtiMGc94vWxePHB";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invitation {
    address: String,
    token: String,
}
impl Invitation {
    pub fn parse(text: &str) -> Result<Self> {
        if text.len() > 4096 {
            return Err("Invitation too large".into());
        }
        let encoded = text
            .trim()
            .strip_prefix("aura://v2/")
            .ok_or("Expected Aura v2 invitation")?;
        let invite: Self = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(encoded)?)?;
        if URL_SAFE_NO_PAD.decode(&invite.token)?.len() != 32 {
            return Err("Invalid capability".into());
        }
        let address: Multiaddr = invite.address.parse()?;
        let parts: Vec<_> = address.iter().collect();
        if !matches!(
            &parts[..],
            [
                Protocol::Ip4(_) | Protocol::Ip6(_) | Protocol::Dns4(_) | Protocol::Dns6(_),
                Protocol::Tcp(1..),
                Protocol::P2p(_),
                Protocol::P2pCircuit,
                Protocol::P2p(_)
            ]
        ) {
            return Err("Invalid relay route".into());
        }
        Ok(invite)
    }
    pub fn to_link(&self) -> Result<String> {
        Ok(format!(
            "aura://v2/{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(self)?)
        ))
    }
    pub fn peer(&self) -> Result<PeerId> {
        match self.address.parse::<Multiaddr>()?.iter().last() {
            Some(Protocol::P2p(peer)) => Ok(peer),
            _ => Err("Missing peer identity".into()),
        }
    }
}
#[derive(Clone, Copy, Default)]
pub struct Stats {
    pub listeners: usize,
    pub dropped: u64,
    pub rtt_ms: u64,
    pub recovering: bool,
}

struct Reservation {
    address: Multiaddr,
    listener: Option<libp2p::core::transport::ListenerId>,
    ready: bool,
    retry: Instant,
}
#[derive(NetworkBehaviour)]
pub(crate) struct Behaviour {
    relay: relay::client::Behaviour,
    identify: identify::Behaviour,
    ping: ping::Behaviour,
    pub(crate) streams: libp2p_stream::Behaviour,
}
pub(crate) fn build(key: identity::Keypair) -> Result<Swarm<Behaviour>> {
    Ok(SwarmBuilder::with_existing_identity(key)
        .with_tokio()
        .with_tcp(
            tcp::Config::default().nodelay(true),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_dns()?
        .with_relay_client(noise::Config::new, yamux::Config::default)?
        .with_behaviour(|key, relay| Behaviour {
            relay,
            identify: identify::Behaviour::new(
                identify::Config::new("/aura/mesh/1".into(), key.public())
                    .with_agent_version("aura-desktop/0.1.0".into()),
            ),
            ping: ping::Behaviour::new(ping::Config::new().with_interval(Duration::from_secs(5))),
            streams: libp2p_stream::Behaviour::new(),
        })?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(120)))
        .build())
}

pub struct Server {
    relay_address: Multiaddr,
    reservations: Vec<Reservation>,
    swarm: Swarm<Behaviour>,
    incoming: libp2p_stream::IncomingStreams,
    metadata_incoming: libp2p_stream::IncomingStreams,
    invitation: Invitation,
    token: [u8; 32],
}
impl Server {
    /// Try configured nodes in order, without starting capture before reservation succeeds.
    pub async fn connect_nodes(
        nodes: &crate::nodes::Nodes,
        key: identity::Keypair,
        stop: watch::Receiver<bool>,
    ) -> Result<Self> {
        let mut failures = Vec::new();
        for address in nodes.addresses() {
            if *stop.borrow() {
                return Err("Cancelled".into());
            }
            match Self::connect(address.clone(), key.clone(), stop.clone()).await {
                Ok(mut server) => {
                    for backup in nodes.addresses().iter().filter(|node| *node != address) {
                        server.reservations.push(Reservation {
                            address: backup.clone(),
                            listener: None,
                            ready: false,
                            retry: Instant::now(),
                        });
                    }
                    return Ok(server);
                }
                Err(error) => failures.push(format!("{address}: {error}")),
            }
        }
        Err(format!("Нет доступных Aura Nodes. {}", failures.join("; ")).into())
    }
    pub async fn connect(
        relay: Multiaddr,
        key: identity::Keypair,
        mut stop: watch::Receiver<bool>,
    ) -> Result<Self> {
        if *stop.borrow() {
            return Err("Cancelled".into());
        }
        crate::nodes::Nodes::new(vec![relay.clone()])?;
        let peer = key.public().to_peer_id();
        let mut swarm = build(key)?;
        let incoming = swarm.behaviour().streams.new_control().accept(AUDIO)?;
        let relay_address = relay.clone();
        let route = relay.with(Protocol::P2pCircuit);
        let listener = swarm.listen_on(route.clone())?;
        let wait = async {
            loop {
                match swarm.select_next_some().await {
                    SwarmEvent::NewListenAddr { address, .. }
                        if address.iter().any(|p| p == Protocol::P2pCircuit) =>
                    {
                        return Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
                    }
                    SwarmEvent::ListenerError { error, .. } => {
                        return Err(format!("Relay reservation: {error}").into())
                    }
                    SwarmEvent::OutgoingConnectionError { error, .. } => {
                        return Err(format!("Relay connection: {error}").into())
                    }
                    _ => {}
                }
            }
        };
        tokio::select! {biased;_=stop.changed()=>return Err("Cancelled".into()),result=timeout(Duration::from_secs(15),wait)=>result??}
        let mut token = [0; 32];
        getrandom::fill(&mut token).map_err(|e| e.to_string())?;
        let invitation = Invitation {
            address: route.with(Protocol::P2p(peer)).to_string(),
            token: URL_SAFE_NO_PAD.encode(token),
        };
        let metadata_incoming = swarm.behaviour().streams.new_control().accept(METADATA)?;
        Ok(Self {
            reservations: vec![Reservation {
                address: relay_address.clone(),
                listener: Some(listener),
                ready: true,
                retry: Instant::now(),
            }],
            relay_address,
            swarm,
            incoming,
            metadata_incoming,
            invitation,
            token,
        })
    }
    pub fn invitation(&self) -> &Invitation {
        &self.invitation
    }
    pub fn relay_address(&self) -> &Multiaddr {
        &self.relay_address
    }
    pub async fn run(
        self,
        frames: mpsc::Receiver<Vec<u8>>,
        stop: watch::Receiver<bool>,
        report: impl FnMut(Stats),
    ) -> Result<()> {
        let (_sender, metadata) = watch::channel(aura_media::Snapshot::default());
        self.run_with_metadata(frames, stop, metadata, report).await
    }
    pub async fn run_with_metadata(
        mut self,
        mut frames: mpsc::Receiver<Vec<u8>>,
        mut stop: watch::Receiver<bool>,
        metadata: watch::Receiver<aura_media::Snapshot>,
        mut report: impl FnMut(Stats),
    ) -> Result<()> {
        if *stop.borrow() {
            return Ok(());
        }
        let (fanout, _) = broadcast::channel::<Vec<u8>>(8);
        let mut tasks = JoinSet::new();
        let mut metadata_tasks = JoinSet::new();
        let mut metadata_attempts = 0;
        let active = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicU64::new(0));
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut window = Instant::now();
        let mut attempts = 0;
        loop {
            tokio::select! {biased;
                _=stop.changed()=>break,
                Some(_)=metadata_tasks.join_next(),if !metadata_tasks.is_empty()=>{},
                Some((_peer,mut stream))=self.metadata_incoming.next()=>{
                    if window.elapsed()>Duration::from_secs(60){window=Instant::now();attempts=0;metadata_attempts=0;}
                    metadata_attempts+=1;if metadata_attempts>30||metadata_tasks.len()>=4{continue;}
                    let token=self.token;let mut updates=metadata.clone();
                    metadata_tasks.spawn(async move{
                        let mut auth=[0;36];timeout(Duration::from_secs(5),stream.read_exact(&mut auth)).await??;
                        if &auth[..4]!=b"AUMD"||token.ct_eq(&auth[4..]).unwrap_u8()!=1{return Err("Metadata access denied".into());}
                        stream.write_all(b"OKMD").await?;let mut previous=None;
                        loop{
                            let snapshot=updates.borrow_and_update().clone();let bytes=aura_media::encode(&snapshot,&mut previous)?;
                            timeout(Duration::from_secs(3),async{stream.write_all(&(bytes.len() as u32).to_be_bytes()).await?;stream.write_all(&bytes).await?;stream.flush().await}).await??;
                            let mut probe=[0;1];tokio::select!{result=updates.changed()=>if result.is_err(){break;},_=stream.read(&mut probe)=>break}
                        }Ok::<_,Box<dyn std::error::Error+Send+Sync>>(())
                    });
                },
                Some(result)=tasks.join_next(),if !tasks.is_empty()=>{let _=result;},
                incoming=self.incoming.next()=>{
                    let Some((_peer,mut stream))=incoming else{break};
                    if window.elapsed()>Duration::from_secs(60){window=Instant::now();attempts=0;metadata_attempts=0;}
                    attempts+=1;if attempts>30||tasks.len()>=4{drop(stream);continue;}
                    let token=self.token;let mut frames=fanout.subscribe();let active=active.clone();let drops=drops.clone();
                    let mut stream_stop=stop.clone();
                    tasks.spawn(async move {
                        let mut auth=[0;36];timeout(Duration::from_secs(5),stream.read_exact(&mut auth)).await??;
                        if &auth[..4]!=MAGIC||token.ct_eq(&auth[4..]).unwrap_u8()!=1{
                            timeout(Duration::from_secs(1),async{stream.write_all(b"NO02").await?;stream.flush().await}).await??;
                            return Err("Access denied".into());
                        }
                        stream.write_all(b"OK02").await?;stream.flush().await?;
                        struct Guard(Arc<AtomicUsize>);impl Drop for Guard{fn drop(&mut self){self.0.fetch_sub(1,Ordering::Relaxed);}}
                        active.fetch_add(1,Ordering::Relaxed);let _guard=Guard(active);
                        loop {
                            let next=tokio::select!{biased;_=stream_stop.changed()=>None,result=frames.recv()=>Some(result)};
                            let bytes=match next {
                                Some(Ok(p))=>p,
                                Some(Err(broadcast::error::RecvError::Lagged(n)))=>{drops.fetch_add(n,Ordering::Relaxed);continue;},
                                _=>{
                                    timeout(Duration::from_millis(400),async{
                                        stream.write_all(&0u16.to_be_bytes()).await?;
                                        stream.flush().await?;
                                        // Keep the swarm alive until the receiver has observed Stop.
                                        let mut ack=[0;1];stream.read_exact(&mut ack).await
                                    }).await??;
                                    break;
                                }
                            };
                            timeout(Duration::from_millis(400),async {
                                stream.write_all(&(bytes.len() as u16).to_be_bytes()).await?;
                                stream.write_all(&bytes).await?;stream.flush().await
                            }).await??;
                        }
                        Ok::<_,Box<dyn std::error::Error+Send+Sync>>(())
                    });
                },
                frame=frames.recv()=>{
                    let Some(frame)=frame else{break};if !(13..=MAX_FRAME).contains(&frame.len()){return Err("Invalid audio frame size".into());}
                    let _=fanout.send(frame);
                },
                event=self.swarm.select_next_some()=>match event {
                    SwarmEvent::NewListenAddr{listener_id,..}=>{
                        if let Some(node)=self.reservations.iter_mut().find(|node|node.listener==Some(listener_id)){node.ready=true;}
                    },
                    SwarmEvent::ConnectionClosed{peer_id,num_established:0,..}=>{
                        for node in &mut self.reservations {
                            if node.address.iter().last()==Some(Protocol::P2p(peer_id)){
                                node.ready=false;node.retry=Instant::now();
                            }
                        }
                    },
                    SwarmEvent::ListenerError{listener_id,..}|SwarmEvent::ListenerClosed{listener_id,..}=>{
                        if let Some(node)=self.reservations.iter_mut().find(|node|node.listener==Some(listener_id)){
                            node.ready=false;
                            if let Some(id)=node.listener.take(){self.swarm.remove_listener(id);}
                        }
                    },
                    _=>{},
                },
                _=tick.tick()=>{
                    for node in &mut self.reservations {
                        if !node.ready&&Instant::now()>=node.retry {
                            if let Some(id)=node.listener.take(){self.swarm.remove_listener(id);}
                            node.listener=self.swarm.listen_on(node.address.clone().with(Protocol::P2pCircuit)).ok();
                            node.retry=Instant::now()+Duration::from_secs(8);
                        }
                    }
                    report(Stats{listeners:active.load(Ordering::Relaxed),dropped:drops.load(Ordering::Relaxed),rtt_ms:0,recovering:!self.reservations.iter().any(|node|node.ready)});
                },
            }
        }
        drop(fanout);
        metadata_tasks.abort_all();
        // Give authenticated listeners an explicit end marker while polling the swarm.
        // A normal Stop must not trigger the transport-failure retry window.
        let _ = timeout(Duration::from_secs(1), async {
            while !tasks.is_empty() {
                tokio::select! {
                    _=tasks.join_next()=>{},
                    _=self.swarm.select_next_some()=>{},
                }
            }
        })
        .await;
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        Ok(())
    }
}

pub async fn receive(
    invite: Invitation,
    key: identity::Keypair,
    stop: watch::Receiver<bool>,
    emit: impl FnMut(Vec<u8>) -> bool,
    ready: impl FnMut(),
    report: impl FnMut(Stats),
) -> Result<()> {
    receive_with_metadata(invite, key, stop, emit, ready, report, |_| {}).await
}

#[derive(Debug)]
struct AudioProtocolError(&'static str);
impl std::fmt::Display for AudioProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for AudioProtocolError {}

/// Reconnect to the same authenticated broadcaster using only locally pinned nodes.
/// The invitation and capability stay unchanged; no discovery or new grant is needed.
#[allow(clippy::too_many_arguments)]
pub async fn receive_with_nodes_metadata(
    invite: Invitation,
    key: identity::Keypair,
    nodes: crate::nodes::Nodes,
    mut stop: watch::Receiver<bool>,
    mut emit: impl FnMut(Vec<u8>) -> bool,
    mut ready: impl FnMut(),
    mut report: impl FnMut(Stats),
    mut metadata: impl FnMut(aura_media::Snapshot),
    mut recovery: impl FnMut(bool),
) -> Result<()> {
    let target = invite.peer()?;
    let mut routes = vec![invite.address.parse::<Multiaddr>()?];
    for node in nodes.addresses() {
        let route = node
            .clone()
            .with(Protocol::P2pCircuit)
            .with(Protocol::P2p(target));
        if !routes.contains(&route) {
            routes.push(route);
        }
    }
    let mut outage = Some(Instant::now());
    let mut recovering = false;
    let mut index = 0;
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        let mut attempt = invite.clone();
        attempt.address = routes[index].to_string();
        let result = receive_with_metadata(
            attempt,
            key.clone(),
            stop.clone(),
            &mut emit,
            || {
                outage = None;
                if recovering {
                    recovering = false;
                    recovery(false);
                }
                ready();
            },
            &mut report,
            &mut metadata,
        )
        .await;
        if *stop.borrow() || result.is_ok() {
            return Ok(());
        }
        let error = result.expect_err("checked above");
        if error.is::<AudioProtocolError>() {
            return Err(error);
        }
        if !recovering {
            recovering = true;
            recovery(true);
        }
        if outage.get_or_insert_with(Instant::now).elapsed() >= Duration::from_secs(60) {
            return Err("Не удалось восстановить эфир через доступные Aura Nodes.".into());
        }
        index = (index + 1) % routes.len();
        // Bound retries even if a remote endpoint immediately closes the stream.
        tokio::select! {biased;
            _=stop.changed()=>return Ok(()),
            _=tokio::time::sleep(Duration::from_millis(750))=>{},
        }
    }
}

pub async fn receive_with_metadata(
    invite: Invitation,
    key: identity::Keypair,
    mut stop: watch::Receiver<bool>,
    mut emit: impl FnMut(Vec<u8>) -> bool,
    mut ready: impl FnMut(),
    mut report: impl FnMut(Stats),
    mut metadata: impl FnMut(aura_media::Snapshot),
) -> Result<()> {
    if *stop.borrow() {
        return Ok(());
    }
    let target = invite.peer()?;
    let mut swarm = build(key)?;
    let mut control = swarm.behaviour().streams.new_control();
    swarm.dial(invite.address.parse::<Multiaddr>()?)?;
    let connect = async {
        loop {
            match swarm.select_next_some().await {
                SwarmEvent::ConnectionEstablished { peer_id, .. } if peer_id == target => {
                    return Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
                }
                SwarmEvent::OutgoingConnectionError { error, .. } => {
                    return Err(format!("Relay dial: {error}").into())
                }
                _ => {}
            }
        }
    };
    tokio::select! {biased;_=stop.changed()=>return Ok(()),result=timeout(Duration::from_secs(15),connect)=>result??}
    let opening = control.open_stream(target, AUDIO);
    let opening = timeout(Duration::from_secs(5), opening);
    tokio::pin!(opening);
    let mut stream = loop {
        tokio::select! {biased;
            _=stop.changed()=>return Ok(()),
            result=&mut opening=>break result??,
            _=swarm.select_next_some()=>{},
        }
    };
    let rtt = Arc::new(AtomicU64::new(0));
    let mut meta_control = swarm.behaviour().streams.new_control();
    let meta_token = invite.token.clone();
    let meta_read = async {
        let result: Result<()> = async {
            let mut stream = timeout(
                Duration::from_secs(5),
                meta_control.open_stream(target, METADATA),
            )
            .await??;
            let mut auth = Vec::from(&b"AUMD"[..]);
            auth.extend_from_slice(&URL_SAFE_NO_PAD.decode(meta_token)?);
            stream.write_all(&auth).await?;
            stream.flush().await?;
            let mut response = [0; 4];
            timeout(Duration::from_secs(5), stream.read_exact(&mut response)).await??;
            if &response != b"OKMD" {
                return Err("Metadata access denied".into());
            }
            let mut previous = None;
            let mut window = Instant::now();
            let mut count = 0;
            loop {
                let mut size = [0; 4];
                stream.read_exact(&mut size).await?;
                let size = u32::from_be_bytes(size) as usize;
                if size == 0 || size > aura_media::MAX_MESSAGE {
                    return Err("Invalid metadata size".into());
                }
                let mut bytes = vec![0; size];
                timeout(Duration::from_secs(5), stream.read_exact(&mut bytes)).await??;
                if window.elapsed() >= Duration::from_secs(1) {
                    window = Instant::now();
                    count = 0;
                }
                count += 1;
                if count > 10 {
                    return Err("Metadata rate limit".into());
                }
                metadata(aura_media::decode(&bytes, &mut previous)?);
            }
        }
        .await;
        metadata(aura_media::Snapshot::default());
        result
    };
    tokio::pin!(meta_read);
    let mut meta_done = false;
    let stream_rtt = rtt.clone();
    let read = async {
        let mut auth = Vec::from(&MAGIC[..]);
        auth.extend_from_slice(&URL_SAFE_NO_PAD.decode(&invite.token)?);
        stream.write_all(&auth).await?;
        stream.flush().await?;
        let mut response = [0; 4];
        timeout(Duration::from_secs(5), stream.read_exact(&mut response)).await??;
        if &response != b"OK02" {
            return Err::<(), Box<dyn std::error::Error + Send + Sync>>(
                AudioProtocolError("Access denied").into(),
            );
        }
        ready();
        let mut stats = Stats::default();
        let mut last = Instant::now();
        loop {
            let mut len = [0; 2];
            stream.read_exact(&mut len).await?;
            let len = u16::from_be_bytes(len) as usize;
            if len == 0 {
                let _ = timeout(Duration::from_millis(400), async {
                    stream.write_all(b"E").await?;
                    stream.flush().await
                })
                .await;
                return Err(AudioProtocolError("Эфир завершён вещателем.").into());
            }
            if !(13..=MAX_FRAME).contains(&len) {
                return Err(AudioProtocolError("Invalid audio frame size").into());
            }
            let mut frame = vec![0; len];
            stream.read_exact(&mut frame).await?;
            if !emit(frame) {
                stats.dropped += 1;
            }
            if last.elapsed() > Duration::from_millis(250) {
                stats.rtt_ms = stream_rtt.load(Ordering::Relaxed);
                report(stats);
                last = Instant::now();
            }
        }
    };
    tokio::pin!(read);
    loop {
        tokio::select! {biased;
            _=stop.changed()=>return Ok(()),
            result=&mut read=>return result,
            _=&mut meta_read,if !meta_done=>meta_done=true,
            event=swarm.select_next_some()=>{
                if let SwarmEvent::Behaviour(BehaviourEvent::Ping(event))=event {
                    if event.peer==target{if let Ok(elapsed)=event.result{rtt.store(elapsed.as_millis() as u64,Ordering::Relaxed);}}
                }
            },
        }
    }
}
