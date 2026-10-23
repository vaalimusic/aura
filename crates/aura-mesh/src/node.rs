use crate::{identity, Multiaddr, Result};
use futures::StreamExt;
use libp2p::{
    connection_limits, identify, kad, noise, ping, relay,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux, Swarm, SwarmBuilder,
};
use std::time::Duration;

#[derive(NetworkBehaviour)]
pub struct Behaviour {
    relay: relay::Behaviour,
    identify: identify::Behaviour,
    ping: ping::Behaviour,
    kad: kad::Behaviour<kad::store::MemoryStore>,
    limits: connection_limits::Behaviour,
    streams: libp2p_stream::Behaviour,
}
pub fn build(key: identity::Keypair) -> Result<Swarm<Behaviour>> {
    let peer = key.public().to_peer_id();
    let mut swarm = SwarmBuilder::with_existing_identity(key)
        .with_tokio()
        .with_tcp(
            tcp::Config::default().nodelay(true),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_dns()?
        .with_behaviour(move |key| {
            let config = relay::Config {
                max_reservations: 32,
                max_reservations_per_peer: 1,
                max_circuits: 16,
                max_circuits_per_peer: 4,
                max_circuit_duration: Duration::from_secs(7200),
                max_circuit_bytes: 256 * 1024 * 1024,
                ..relay::Config::default()
            };
            let mut kad_config = kad::Config::new(libp2p::StreamProtocol::new("/aura/kad/1"));
            // Social discovery records require validation that is not implemented yet.
            kad_config.set_record_filtering(kad::StoreInserts::FilterBoth);
            Behaviour {
                streams: libp2p_stream::Behaviour::new(),
                relay: relay::Behaviour::new(peer, config),
                identify: identify::Behaviour::new(
                    identify::Config::new("/aura/mesh/1".into(), key.public())
                        .with_agent_version("aura-node/0.1.0".into()),
                ),
                ping: ping::Behaviour::new(
                    ping::Config::new().with_interval(Duration::from_secs(15)),
                ),
                kad: kad::Behaviour::with_config(
                    peer,
                    kad::store::MemoryStore::new(peer),
                    kad_config,
                ),
                limits: connection_limits::Behaviour::new(
                    connection_limits::ConnectionLimits::default()
                        .with_max_pending_incoming(Some(16))
                        .with_max_established(Some(64))
                        .with_max_established_per_peer(Some(4)),
                ),
            }
        })?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(120)))
        .build();
    swarm.behaviour_mut().kad.set_mode(Some(kad::Mode::Server));
    Ok(swarm)
}
pub async fn run(
    mut swarm: Swarm<Behaviour>,
    listen: Multiaddr,
    advertise: Multiaddr,
    mut stop: tokio::sync::watch::Receiver<bool>,
    mut announce: impl FnMut(Multiaddr),
) -> Result<()> {
    swarm.add_external_address(advertise);
    swarm.listen_on(listen)?;
    let mut public = crate::public::Service::new(swarm.behaviour().streams.new_control())?;
    loop {
        tokio::select! {
            _=public.next()=>{},
            _=stop.changed()=>break,
            event=swarm.select_next_some()=>match event {
                SwarmEvent::NewListenAddr{address,..}=>announce(address),
                SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Received{peer_id,info,..}))=>{
                    for address in info.listen_addrs.into_iter().take(8){swarm.behaviour_mut().kad.add_address(&peer_id,address);}
                },
                SwarmEvent::ListenerError{error,..}=>return Err(error.into()),
                SwarmEvent::ListenerClosed{..}=>return Err("Aura node listener closed".into()),
                _=>{},
            }
        }
    }
    Ok(())
}
