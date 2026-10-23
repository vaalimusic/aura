use aura_mesh::{audio, identity, nodes::Nodes, social, Multiaddr};
use aura_social::{Intent, Store};
use futures::StreamExt;
use libp2p::{multiaddr::Protocol, swarm::SwarmEvent};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time::{timeout, Duration},
};

struct Task(JoinHandle<()>);
impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn node(key: identity::Keypair, listen: Multiaddr) -> (Task, Multiaddr) {
    let peer = key.public().to_peer_id();
    let mut swarm = aura_mesh::node::build(key).unwrap();
    swarm.listen_on(listen).unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut tx = Some(tx);
    let task = Task(tokio::spawn(async move {
        loop {
            if let SwarmEvent::NewListenAddr { address, .. } = swarm.select_next_some().await {
                swarm.add_external_address(address.clone());
                if let Some(tx) = tx.take() {
                    let _ = tx.send(address.with(Protocol::P2p(peer)));
                }
            }
        }
    }));
    (
        task,
        timeout(Duration::from_secs(5), rx).await.unwrap().unwrap(),
    )
}

async fn snapshot(
    rx: &mut mpsc::UnboundedReceiver<social::Event>,
    predicate: impl Fn(&social::Snapshot) -> bool,
) {
    timeout(Duration::from_secs(35), async {
        while let Some(event) = rx.recv().await {
            if let social::Event::Snapshot(snapshot) = event {
                if predicate(&snapshot) {
                    return;
                }
            }
        }
        panic!("Social worker closed");
    })
    .await
    .expect("Expected community-node state was not observed");
}

fn store(path: &std::path::Path, key: &identity::Keypair, contacts: &[String]) -> Store {
    let mut store = Store::open(path, &key.public().to_peer_id().to_string()).unwrap();
    for id in contacts {
        store.add(id).unwrap();
        store.observe(id, "Друг", Intent::Approved).unwrap();
    }
    store
}

fn client(
    key: identity::Keypair,
    store: Store,
    nodes: Nodes,
) -> (
    Task,
    mpsc::Sender<social::Command>,
    mpsc::UnboundedReceiver<social::Event>,
) {
    let (commands, rx) = mpsc::channel(16);
    let (events, event_rx) = mpsc::unbounded_channel();
    let task = Task(tokio::spawn(async move {
        social::run_with_nodes(key, store, nodes, rx, move |event| {
            let _ = events.send(event);
        })
        .await
        .unwrap();
    }));
    (task, commands, event_rx)
}

#[tokio::test]
async fn presence_survives_primary_failure_and_a_new_client_joins_the_backup() {
    let key1 = identity::Keypair::generate_ed25519();
    let (primary, address1) = node(key1.clone(), "/ip4/127.0.0.1/tcp/0".parse().unwrap()).await;
    let (_backup, address2) = node(
        identity::Keypair::generate_ed25519(),
        "/ip4/127.0.0.1/tcp/0".parse().unwrap(),
    )
    .await;
    let nodes = Nodes::new(vec![address1.clone(), address2.clone()]).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let a = identity::Keypair::generate_ed25519();
    let b = identity::Keypair::generate_ed25519();
    let c = identity::Keypair::generate_ed25519();
    let aid = a.public().to_peer_id().to_string();
    let bid = b.public().to_peer_id().to_string();
    let cid = c.public().to_peer_id().to_string();
    let sa = store(&dir.path().join("a.json"), &a, std::slice::from_ref(&bid));
    let sb = store(&dir.path().join("b.json"), &b, &[aid.clone(), cid.clone()]);
    let sc = store(&dir.path().join("c.json"), &c, std::slice::from_ref(&bid));
    let (_a, _ac, mut ar) = client(a, sa, nodes.clone());
    let (_b, _bc, mut br) = client(b, sb, nodes);
    snapshot(&mut ar, |s| {
        s.nodes_connected == 2 && s.friends.iter().any(|f| f.contact.id == bid && f.online)
    })
    .await;
    snapshot(&mut br, |s| {
        s.nodes_connected == 2 && s.friends.iter().any(|f| f.contact.id == aid && f.online)
    })
    .await;
    primary.0.abort();
    // Wait until shutdown closes sockets before bringing up an independent new client.
    while !primary.0.is_finished() {
        tokio::task::yield_now().await;
    }
    snapshot(&mut ar, |s| s.nodes_connected == 1 && s.connected).await;
    let (_c, _cc, mut cr) = client(c, sc, Nodes::new(vec![address2]).unwrap());
    snapshot(&mut cr, |s| {
        s.connected && s.friends.iter().any(|f| f.contact.id == bid && f.online)
    })
    .await;
    snapshot(&mut br, |s| {
        s.nodes_connected == 1 && s.friends.iter().any(|f| f.contact.id == cid && f.online)
    })
    .await;
    let mut listen = address1;
    listen.pop();
    let (_restored, _) = node(key1, listen).await;
    snapshot(&mut ar, |s| {
        s.nodes_connected == 2 && s.friends.iter().any(|f| f.contact.id == bid && f.online)
    })
    .await;
}

#[tokio::test]
async fn audio_reservation_falls_back_and_cancellation_prevents_connection() {
    let (primary, address1) = node(
        identity::Keypair::generate_ed25519(),
        "/ip4/127.0.0.1/tcp/0".parse().unwrap(),
    )
    .await;
    let (_backup, address2) = node(
        identity::Keypair::generate_ed25519(),
        "/ip4/127.0.0.1/tcp/0".parse().unwrap(),
    )
    .await;
    primary.0.abort();
    while !primary.0.is_finished() {
        tokio::task::yield_now().await;
    }
    let nodes = Nodes::new(vec![address1, address2.clone()]).unwrap();
    let (stop, stopped) = watch::channel(false);
    let server = timeout(
        Duration::from_secs(20),
        audio::Server::connect_nodes(
            &nodes,
            identity::Keypair::generate_ed25519(),
            stopped.clone(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(server.invitation().to_link().is_ok());
    // Check the actual selected relay without exposing or logging the bearer token.
    assert_eq!(server.relay_address(), &address2);
    let invite = server.invitation().clone();
    let (frames, rx) = mpsc::channel(8);
    let host = tokio::spawn(server.run(rx, stopped.clone(), |_| {}));
    let (observed, mut received) = mpsc::unbounded_channel();
    let (ready, ready_rx) = tokio::sync::oneshot::channel();
    let mut ready = Some(ready);
    let listener = tokio::spawn(audio::receive(
        invite,
        identity::Keypair::generate_ed25519(),
        stopped.clone(),
        move |bytes| {
            let _ = observed.send(bytes);
            true
        },
        move || {
            if let Some(ready) = ready.take() {
                let _ = ready.send(());
            }
        },
        |_| {},
    ));
    timeout(Duration::from_secs(8), ready_rx)
        .await
        .unwrap()
        .unwrap();
    for index in 0..8u8 {
        frames.send(vec![index; 64]).await.unwrap();
        assert_eq!(
            timeout(Duration::from_secs(5), received.recv())
                .await
                .unwrap()
                .unwrap(),
            vec![index; 64]
        );
    }
    stop.send(true).unwrap();
    timeout(Duration::from_secs(5), host)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    timeout(Duration::from_secs(5), listener)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        audio::Server::connect_nodes(&nodes, identity::Keypair::generate_ed25519(), stopped)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn playing_audio_recovers_on_backup_and_after_both_nodes_restart() {
    let (primary, address1) = node(
        identity::Keypair::generate_ed25519(),
        "/ip4/127.0.0.1/tcp/0".parse().unwrap(),
    )
    .await;
    let backup_key = identity::Keypair::generate_ed25519();
    let (backup, address2) =
        node(backup_key.clone(), "/ip4/127.0.0.1/tcp/0".parse().unwrap()).await;
    let nodes = Nodes::new(vec![address1, address2.clone()]).unwrap();
    let (stop, stopped) = watch::channel(false);
    let server = audio::Server::connect_nodes(
        &nodes,
        identity::Keypair::generate_ed25519(),
        stopped.clone(),
    )
    .await
    .unwrap();
    let invitation = server.invitation().clone();
    let (frames, rx) = mpsc::channel(8);
    let (metadata_tx, metadata) = watch::channel(aura_media::Snapshot::default());
    let host = tokio::spawn(server.run_with_metadata(rx, stopped.clone(), metadata, |_| {}));
    let mut producer_stop = stopped.clone();
    let producer = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(20));
        let mut sequence = 0u64;
        loop {
            tokio::select! {biased;
                _=producer_stop.changed()=>break,
                _=tick.tick()=>{
                    let mut frame=vec![0;64];frame[..8].copy_from_slice(&sequence.to_be_bytes());
                    let _=frames.try_send(frame);sequence+=1;
                }
            }
        }
    });
    let (events, mut received) = mpsc::unbounded_channel();
    let (recovery, mut recovery_rx) = mpsc::unbounded_channel();
    let (media, mut media_rx) = mpsc::unbounded_channel();
    let listener = tokio::spawn(audio::receive_with_nodes_metadata(
        invitation,
        identity::Keypair::generate_ed25519(),
        nodes,
        stopped.clone(),
        move |bytes| {
            let _ = events.send(bytes);
            true
        },
        || {},
        |_| {},
        move |snapshot| {
            let _ = media.send(snapshot);
        },
        move |value| {
            let _ = recovery.send(value);
        },
    ));
    let first = timeout(Duration::from_secs(10), received.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.len(), 64);
    primary.0.abort();
    while !primary.0.is_finished() {
        tokio::task::yield_now().await;
    }
    assert!(timeout(Duration::from_secs(10), recovery_rx.recv())
        .await
        .unwrap()
        .unwrap());
    while received.try_recv().is_ok() {}
    assert!(!timeout(Duration::from_secs(20), recovery_rx.recv())
        .await
        .unwrap()
        .unwrap());
    let after = timeout(Duration::from_secs(5), received.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        u64::from_be_bytes(after[..8].try_into().unwrap())
            > u64::from_be_bytes(first[..8].try_into().unwrap())
    );
    assert!(
        !host.is_finished(),
        "The original capture/network worker must survive"
    );

    backup.0.abort();
    while !backup.0.is_finished() {
        tokio::task::yield_now().await;
    }
    assert!(timeout(Duration::from_secs(10), recovery_rx.recv())
        .await
        .unwrap()
        .unwrap());
    while received.try_recv().is_ok() {}
    // Leave both nodes offline through a retry, then restore the same pinned identity.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let mut listen = address2;
    listen.pop();
    let (restored, _) = node(backup_key, listen).await;
    assert!(!timeout(Duration::from_secs(30), recovery_rx.recv())
        .await
        .unwrap()
        .unwrap());
    timeout(Duration::from_secs(5), received.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(!host.is_finished());

    let updated = aura_media::Snapshot {
        info: Some(aura_media::Info {
            title: "После восстановления".into(),
            artist: "Автор".into(),
            album: String::new(),
            app: "Плеер".into(),
            playback: aura_media::Playback::Playing,
            position_ms: 1000,
            duration_ms: 10000,
        }),
        artwork: None,
    };
    metadata_tx.send(updated.clone()).unwrap();
    timeout(Duration::from_secs(5), async {
        while let Some(snapshot) = media_rx.recv().await {
            if snapshot == updated {
                return;
            }
        }
        panic!("Metadata did not reconnect");
    })
    .await
    .unwrap();
    restored.0.abort();
    assert!(timeout(Duration::from_secs(10), recovery_rx.recv())
        .await
        .unwrap()
        .unwrap());
    // Stop must cancel a live retry/backoff, rather than waiting for its timeout.
    stop.send(true).unwrap();
    timeout(Duration::from_secs(3), listener)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    timeout(Duration::from_secs(3), host)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    producer.await.unwrap();
}
