//! Explicit network integration probe; isolated temporary profiles, no user's contacts.
use aura_mesh::{
    identity,
    social::{self, Command, Event, Snapshot},
    Multiaddr,
};
use aura_social::{Relation, Store};
use tokio::{
    sync::mpsc,
    time::{timeout, Duration},
};
async fn snapshot(
    rx: &mut mpsc::UnboundedReceiver<Event>,
    predicate: impl Fn(&Snapshot) -> bool,
) -> aura_audio::Result<()> {
    timeout(Duration::from_secs(35), async {
        while let Some(event) = rx.recv().await {
            if let Event::Snapshot(snapshot) = event {
                if predicate(&snapshot) {
                    return Ok(());
                }
            }
        }
        Err("Social actor stopped".into())
    })
    .await?
}
async fn answer(
    rx: &mut mpsc::UnboundedReceiver<Event>,
    request: u64,
) -> aura_audio::Result<std::result::Result<String, String>> {
    timeout(Duration::from_secs(15), async {
        while let Some(event) = rx.recv().await {
            if let Event::Listen(id, result) = event {
                if id == request {
                    return Ok(result);
                }
            }
        }
        Err("No social answer".into())
    })
    .await?
}
pub async fn exercise(relay: Multiaddr, link: String) -> aura_audio::Result<String> {
    let dir = tempfile::tempdir()?;
    let a = identity::Keypair::generate_ed25519();
    let b = identity::Keypair::generate_ed25519();
    let aid = a.public().to_peer_id().to_string();
    let bid = b.public().to_peer_id().to_string();
    let store_a = Store::open(&dir.path().join("a.json"), &aid)?;
    let store_b = Store::open(&dir.path().join("b.json"), &bid)?;
    let (atx, arx) = mpsc::channel(32);
    let (btx, brx) = mpsc::channel(32);
    let (aevents, mut ar) = mpsc::unbounded_channel();
    let (bevents, mut br) = mpsc::unbounded_channel();
    struct Tasks(Vec<tokio::task::JoinHandle<aura_audio::Result<()>>>);
    impl Drop for Tasks {
        fn drop(&mut self) {
            for task in &self.0 {
                task.abort();
            }
        }
    }
    let _tasks = Tasks(vec![
        tokio::spawn(social::run(a, store_a, relay.clone(), arx, move |e| {
            let _ = aevents.send(e);
        })),
        tokio::spawn(social::run(b, store_b, relay, brx, move |e| {
            let _ = bevents.send(e);
        })),
    ]);
    println!("Friends probe: waiting for relay reservations");
    snapshot(&mut ar, |s| s.connected).await?;
    snapshot(&mut br, |s| s.connected).await?;
    println!("Friends probe: sending request");
    atx.send(Command::Rename("Вещатель".into())).await?;
    atx.send(Command::Publish(Some(link.clone()))).await?;
    atx.send(Command::Add(bid.clone())).await?;
    snapshot(&mut br, |s| {
        s.friends
            .iter()
            .any(|f| f.contact.id == aid && f.contact.relation == Relation::Incoming && !f.live)
    })
    .await?;
    btx.send(Command::Listen(aid.clone(), 1)).await?;
    if answer(&mut br, 1).await?.is_ok() {
        return Err("Unapproved contact received capability".into());
    }
    btx.send(Command::Accept(aid.clone())).await?;
    println!("Friends probe: confirming mutual consent");
    snapshot(&mut ar, |s| {
        s.friends
            .iter()
            .any(|f| f.contact.id == bid && f.contact.relation == Relation::Accepted)
    })
    .await?;
    snapshot(&mut br, |s| {
        s.friends
            .iter()
            .any(|f| f.contact.id == aid && f.live && f.online && f.contact.name == "Вещатель")
    })
    .await?;
    btx.send(Command::Listen(aid.clone(), 2)).await?;
    println!("Friends probe: resolving live invitation");
    let resolved = answer(&mut br, 2)
        .await?
        .map_err(|e| format!("Friend lookup failed: {e}"))?;
    if resolved != link {
        return Err("Wrong friend invitation".into());
    }
    atx.send(Command::Publish(None)).await?;
    snapshot(&mut br, |s| {
        s.friends
            .iter()
            .any(|f| f.contact.id == aid && f.online && !f.live)
    })
    .await?;
    btx.send(Command::Listen(aid.clone(), 3)).await?;
    if answer(&mut br, 3).await?.is_ok() {
        return Err("Stopped broadcast still advertised".into());
    }
    atx.send(Command::Publish(Some(link.clone()))).await?;
    snapshot(&mut br, |s| {
        s.friends.iter().any(|f| f.contact.id == aid && f.live)
    })
    .await?;
    btx.send(Command::Listen(aid.clone(), 4)).await?;
    let resolved = answer(&mut br, 4).await?.map_err(|e| e.to_string())?;
    atx.send(Command::Enable(false)).await?;
    snapshot(&mut br, |s| {
        s.friends.iter().any(|f| {
            f.contact.id == aid && !f.online && !f.live && f.contact.relation == Relation::Accepted
        })
    })
    .await?;
    atx.send(Command::Enable(true)).await?;
    atx.send(Command::Publish(Some(link))).await?;
    snapshot(&mut ar, |s| s.connected).await?;
    snapshot(&mut br, |s| {
        s.friends
            .iter()
            .any(|f| f.contact.id == aid && f.online && f.live)
    })
    .await?;
    btx.send(Command::Remove(aid.clone())).await?;
    snapshot(&mut ar, |s| {
        s.friends
            .iter()
            .any(|f| f.contact.id == bid && f.contact.relation == Relation::Declined)
    })
    .await?;
    let persisted = Store::open(&dir.path().join("b.json"), &bid)?;
    if persisted.share(&aid) {
        return Err("Removed friend was persisted as accepted".into());
    }
    println!("Friends smoke PASS: request -> consent -> authenticated presence -> live invitation -> stopped broadcast -> fresh lookup -> offline -> reconnect -> removal; isolated profiles");
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aura_mesh::audio::Server;
    use futures::StreamExt;
    #[tokio::test]
    async fn friends_lifecycle_over_local_relay() {
        let key = identity::Keypair::generate_ed25519();
        let peer = key.public().to_peer_id();
        let mut swarm = aura_mesh::node::build(key).unwrap();
        swarm
            .listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
            .unwrap();
        let (ready, rx) = tokio::sync::oneshot::channel();
        let mut ready = Some(ready);
        let node = tokio::spawn(async move {
            loop {
                if let libp2p::swarm::SwarmEvent::NewListenAddr { address, .. } =
                    swarm.select_next_some().await
                {
                    swarm.add_external_address(address.clone());
                    if let Some(tx) = ready.take() {
                        let _ = tx.send(address.with(libp2p::multiaddr::Protocol::P2p(peer)));
                    }
                }
            }
        });
        let relay = rx.await.unwrap();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let server = Server::connect(
            relay.clone(),
            identity::Keypair::generate_ed25519(),
            stopped.clone(),
        )
        .await
        .unwrap();
        let link = server.invitation().to_link().unwrap();
        let (_frames, frames_rx) = mpsc::channel(8);
        let host = tokio::spawn(server.run(frames_rx, stopped, |_| {}));
        let result = exercise(relay, link).await;
        let _ = stop.send(true);
        let _ = host.await;
        node.abort();
        result.unwrap();
    }
}
