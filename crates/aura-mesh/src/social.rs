//! Friend-scoped presence over authenticated Noise streams. Capabilities stay in memory.
use crate::{audio, identity, Multiaddr, PeerId, Result};
use aura_social::{Contact, Intent, Relation, Store};
use futures::{AsyncReadExt, AsyncWriteExt, StreamExt};
use libp2p::{multiaddr::Protocol, swarm::SwarmEvent, Stream, StreamProtocol};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};
use tokio::{
    sync::mpsc,
    task::JoinSet,
    time::{timeout, Instant},
};
const PROTOCOL: StreamProtocol = StreamProtocol::new("/aura/social/1");
const LIMIT: usize = 8192;
#[derive(Clone, PartialEq, Eq)]
pub struct Friend {
    pub contact: Contact,
    pub online: bool,
    pub live: bool,
}
#[derive(Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub name: String,
    pub connected: bool,
    pub nodes_connected: usize,
    pub nodes_total: usize,
    pub friends: Vec<Friend>,
}
pub enum Event {
    Snapshot(Snapshot),
    Notice(String),
    Listen(u64, std::result::Result<String, String>),
}
pub enum Command {
    Add(String),
    Accept(String),
    Remove(String),
    Rename(String),
    Publish(Option<String>),
    Listen(String, u64),
    Enable(bool),
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    name: String,
    intent: Intent,
    invite: Option<String>,
}
fn envelope(store: &Store, peer: &str, published: &Option<String>) -> Wire {
    Wire {
        name: store.data().name.clone(),
        intent: store.intent(peer),
        invite: if store.share(peer) {
            published.clone()
        } else {
            None
        },
    }
}
async fn read(stream: &mut Stream) -> Result<Wire> {
    let mut size = [0; 2];
    stream.read_exact(&mut size).await?;
    let size = u16::from_be_bytes(size) as usize;
    if size == 0 || size > LIMIT {
        return Err("Invalid social message size".into());
    }
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes).await?;
    let wire: Wire = serde_json::from_slice(&bytes)?;
    aura_social::name(&wire.name)?;
    if wire
        .invite
        .as_ref()
        .is_some_and(|s| s.len() > 4096 || !s.starts_with("aura://v2/"))
    {
        return Err("Invalid broadcast invitation".into());
    }
    if let Some(link) = &wire.invite {
        audio::Invitation::parse(link)?;
    }
    Ok(wire)
}
async fn write(stream: &mut Stream, wire: &Wire) -> Result<()> {
    let bytes = serde_json::to_vec(wire)?;
    if bytes.len() > LIMIT {
        return Err("Social message too large".into());
    }
    stream
        .write_all(&(bytes.len() as u16).to_be_bytes())
        .await?;
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}
enum Job {
    Incoming(PeerId, Stream, Wire),
    Response(PeerId, Option<u64>, Result<Wire>),
    Sent,
}
fn connect(swarm: &mut libp2p::Swarm<audio::Behaviour>, nodes: &crate::nodes::Nodes, peer: PeerId) {
    use libp2p::swarm::dial_opts::{DialOpts, PeerCondition};
    let addresses = nodes
        .addresses()
        .iter()
        .cloned()
        .map(|relay| relay.with(Protocol::P2pCircuit).with(Protocol::P2p(peer)))
        .collect();
    let _ = swarm.dial(
        DialOpts::peer_id(peer)
            .condition(PeerCondition::DisconnectedAndNotDialing)
            .addresses(addresses)
            .build(),
    );
}
fn send_request(
    jobs: &mut JoinSet<Job>,
    mut control: libp2p_stream::Control,
    peer: PeerId,
    wire: Wire,
    request: Option<u64>,
) {
    jobs.spawn(async move {
        let result = timeout(Duration::from_secs(6), async {
            let mut stream = control.open_stream(peer, PROTOCOL).await?;
            write(&mut stream, &wire).await?;
            read(&mut stream).await
        })
        .await
        .unwrap_or_else(|_| Err("Друг недоступен.".into()));
        Job::Response(peer, request, result)
    });
}
/// The stable identity belongs exclusively to this swarm; audio uses session identities.
pub async fn run(
    key: identity::Keypair,
    store: Store,
    relay: Multiaddr,
    commands: mpsc::Receiver<Command>,
    emit: impl Fn(Event) + Send + 'static,
) -> Result<()> {
    run_with_nodes(
        key,
        store,
        crate::nodes::Nodes::new(vec![relay])?,
        commands,
        emit,
    )
    .await
}

pub async fn run_with_nodes(
    key: identity::Keypair,
    mut store: Store,
    nodes: crate::nodes::Nodes,
    mut commands: mpsc::Receiver<Command>,
    emit: impl Fn(Event) + Send + 'static,
) -> Result<()> {
    struct Reservation {
        address: Multiaddr,
        listener: Option<libp2p::core::transport::ListenerId>,
        ready: bool,
        retry: Instant,
    }
    let mut reservations: Vec<_> = nodes
        .addresses()
        .iter()
        .cloned()
        .map(|address| Reservation {
            address,
            listener: None,
            ready: false,
            retry: Instant::now(),
        })
        .collect();
    let mut swarm = audio::build(key)?;
    let control = swarm.behaviour().streams.new_control();
    let mut incoming = control.clone().accept(PROTOCOL)?;
    let mut enabled = true;
    let mut published = None;
    let mut jobs = JoinSet::new();
    let mut pending = HashSet::new();
    let mut seen: HashMap<String, (Instant, bool)> = HashMap::new();
    let mut rates: HashMap<PeerId, (Instant, u8)> = HashMap::new();
    let mut tick = tokio::time::interval(Duration::from_secs(4));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut previous = None;
    let mut cursor = 0usize;
    loop {
        let connected = enabled && reservations.iter().any(|node| node.ready);
        tokio::select! {
            command=commands.recv()=>{let Some(command)=command else{break};
                let result=match command {
                    Command::Add(id)=>store.add(&id),Command::Accept(id)=>store.accept(&id),
                    Command::Rename(name)=>store.rename(&name),
                    Command::Remove(id)=>{
                        let result=store.remove(&id);seen.remove(&id);
                        if enabled&&result.is_ok(){if let Ok(peer)=id.parse::<PeerId>(){connect(&mut swarm,&nodes,peer);send_request(&mut jobs,control.clone(),peer,envelope(&store,&id,&None),None);}}
                        result
                    },
                    Command::Publish(value)=>{published=value.filter(|s|audio::Invitation::parse(s).is_ok());Ok(())},
                    Command::Enable(value)=>{enabled=value;published=None;seen.clear();
                        jobs.abort_all();pending.clear();for node in &mut reservations {
                            node.ready=false;node.retry=Instant::now();if let Some(id)=node.listener.take(){swarm.remove_listener(id);}
                        }
                        let peers:Vec<_>=swarm.connected_peers().copied().collect();for peer in peers{let _=swarm.disconnect_peer_id(peer);}Ok(())},
                    Command::Listen(id,request)=>{
                        if enabled&&connected&&store.share(&id)&&jobs.len()<12 {
                            if let Ok(peer)=id.parse::<PeerId>(){connect(&mut swarm,&nodes,peer);send_request(&mut jobs,control.clone(),peer,envelope(&store,&id,&published),Some(request));}
                        }else{emit(Event::Listen(request,Err("Друг или Aura Node недоступен.".into())));}Ok(())
                    }
                };if let Err(error)=result{emit(Event::Notice(error.to_string()));}
            },
            _=tick.tick()=>{
                for node in &mut reservations { if enabled&&!node.ready&&Instant::now()>=node.retry{
                    if let Some(id)=node.listener.take(){swarm.remove_listener(id);}
                    match swarm.listen_on(node.address.clone().with(Protocol::P2pCircuit)){Ok(id)=>node.listener=Some(id),Err(error)=>emit(Event::Notice(error.to_string()))};node.retry=Instant::now()+Duration::from_secs(16);
                }
                }
                if enabled&&connected{
                    let contacts=&store.data().contacts;
                    for offset in 0..contacts.len() {
                        if jobs.len()>=8{break;}
                        let index=(cursor+offset)%contacts.len();let contact=&contacts[index];
                        if matches!(contact.relation,Relation::Blocked|Relation::Declined)||pending.contains(&contact.id){continue;}
                        let peer:PeerId=contact.id.parse()?;connect(&mut swarm,&nodes,peer);
                        pending.insert(contact.id.clone());send_request(&mut jobs,control.clone(),peer,envelope(&store,&contact.id,&published),None);
                    }
                    cursor=cursor.wrapping_add(8);
                }
                rates.retain(|_,(last,_)|last.elapsed()<Duration::from_secs(30));
            },
            event=swarm.select_next_some()=>match event{
                SwarmEvent::NewListenAddr{listener_id,..}=>{
                    if let Some(node)=reservations.iter_mut().find(|node|node.listener==Some(listener_id)){node.ready=enabled;}
                },
                SwarmEvent::ListenerClosed{listener_id,..}|SwarmEvent::ListenerError{listener_id,..}=>{
                    if let Some(node)=reservations.iter_mut().find(|node|node.listener==Some(listener_id)){
                        node.ready=false;if let Some(id)=node.listener.take(){swarm.remove_listener(id);}
                    }
                },
                SwarmEvent::ConnectionClosed{peer_id,num_established:0,..}=>{
                    for node in &mut reservations { if node.address.iter().last()==Some(Protocol::P2p(peer_id)){node.ready=false;} }
                },
                _=>{}
            },
            Some((peer,mut stream))=incoming.next()=>{
                if enabled&&jobs.len()<12&&(rates.contains_key(&peer)||rates.len()<256)&&!rates.get(&peer).is_some_and(|(last,count)|last.elapsed()<Duration::from_secs(1)&&*count>=8) {
                    let rate=rates.entry(peer).or_insert((Instant::now(),0));if rate.0.elapsed()>=Duration::from_secs(1){*rate=(Instant::now(),0);}rate.1+=1;
                    jobs.spawn(async move{match timeout(Duration::from_secs(5),read(&mut stream)).await{Ok(Ok(wire))=>Job::Incoming(peer,stream,wire),_=>Job::Sent}});
                }
            },
            Some(result)=jobs.join_next(),if !jobs.is_empty()=>{match result {
                Ok(Job::Incoming(peer,mut stream,wire)) if enabled=>{
                    let id=peer.to_string();if let Err(error)=store.observe(&id,&wire.name,wire.intent){emit(Event::Notice(error.to_string()));continue;}
                    let allowed=store.share(&id)&&wire.intent==Intent::Approved;
                    if allowed{seen.insert(id.clone(),(Instant::now(),wire.invite.is_some()));}else{seen.remove(&id);}
                    let response=envelope(&store,&id,&published);jobs.spawn(async move{let _=timeout(Duration::from_secs(3),write(&mut stream,&response)).await;Job::Sent});
                },
                Ok(Job::Response(peer,request,result))=>{
                    let id=peer.to_string();pending.remove(&id);
                    let response=match result{
                        Ok(wire) if enabled=>{
                            if let Err(error)=store.observe(&id,&wire.name,wire.intent){Err(error.to_string())}
                            else if store.share(&id)&&wire.intent==Intent::Approved{seen.insert(id,(Instant::now(),wire.invite.is_some()));wire.invite.ok_or_else(||"Друг в сети, но эфир не запущен.".into())}
                            else{seen.remove(&id);Err("Дружба ещё не подтверждена.".into())}
                        },
                        Ok(_)=>Err("Соединение отключено.".into()),Err(error)=>Err(error.to_string()),
                    };if let Some(request)=request{emit(Event::Listen(request,response));}
                },_=>{}
            }},
        }
        let nodes_connected = if enabled {
            reservations.iter().filter(|node| node.ready).count()
        } else {
            0
        };
        let connected = nodes_connected > 0;
        if !connected {
            seen.clear();
        }
        let snapshot = Snapshot {
            name: store.data().name.clone(),
            connected,
            nodes_connected,
            nodes_total: reservations.len(),
            friends: store
                .data()
                .contacts
                .iter()
                .filter(|c| c.relation != Relation::Blocked)
                .map(|contact| {
                    let status = seen
                        .get(&contact.id)
                        .filter(|(time, _)| connected && time.elapsed() < Duration::from_secs(13));
                    Friend {
                        contact: contact.clone(),
                        online: status.is_some(),
                        live: status.is_some_and(|(_, live)| *live),
                    }
                })
                .collect(),
        };
        if previous.as_ref() != Some(&snapshot) {
            previous = Some(snapshot.clone());
            emit(Event::Snapshot(snapshot));
        }
    }
    jobs.abort_all();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capabilities_only_leave_for_accepted_contacts() {
        let path = std::env::temp_dir().join(format!(
            "aura-consent-{}.json",
            identity::Keypair::generate_ed25519().public().to_peer_id()
        ));
        let owner = identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id()
            .to_string();
        let friend = identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id()
            .to_string();
        let mut store = Store::open(&path, &owner).unwrap();
        let link = Some("private".into());
        assert!(envelope(&store, &friend, &link).invite.is_none());
        store.add(&friend).unwrap();
        assert!(envelope(&store, &friend, &link).invite.is_none());
        store.observe(&friend, "Друг", Intent::Approved).unwrap();
        assert_eq!(envelope(&store, &friend, &link).invite, link);
        store.remove(&friend).unwrap();
        assert!(envelope(&store, &friend, &link).invite.is_none());
        std::fs::remove_file(path).unwrap();
    }
}
