use aura_mesh::{identity, nodes::Nodes, public, Multiaddr};
use libp2p::multiaddr::Protocol;
use tokio::{
    sync::{mpsc, watch},
    time::{timeout, Duration},
};

async fn node() -> (tokio::task::JoinHandle<()>, watch::Sender<bool>, Multiaddr) {
    let key = identity::Keypair::generate_ed25519();
    node_at(key, "/ip4/127.0.0.1/tcp/0".parse().unwrap()).await
}
async fn node_at(
    key: identity::Keypair,
    listen: Multiaddr,
) -> (tokio::task::JoinHandle<()>, watch::Sender<bool>, Multiaddr) {
    let peer = key.public().to_peer_id();
    let (stop, stopped) = watch::channel(false);
    let (ready, rx) = tokio::sync::oneshot::channel();
    let mut ready = Some(ready);
    let task = tokio::spawn(async move {
        aura_mesh::node::run(
            aura_mesh::node::build(key).unwrap(),
            listen,
            "/ip4/127.0.0.1/tcp/1".parse().unwrap(),
            stopped,
            move |addr| {
                if let Some(tx) = ready.take() {
                    let _ = tx.send(addr.with(Protocol::P2p(peer)));
                }
            },
        )
        .await
        .unwrap();
    });
    (
        task,
        stop,
        timeout(Duration::from_secs(5), rx).await.unwrap().unwrap(),
    )
}
fn info(title: &str) -> aura_media::Snapshot {
    aura_media::Snapshot {
        info: Some(aura_media::Info {
            title: title.into(),
            artist: "Автор трека".into(),
            album: String::new(),
            app: "Тест".into(),
            playback: aura_media::Playback::Playing,
            position_ms: 0,
            duration_ms: 10000,
        }),
        artwork: Some(aura_media::thumbnail(&base64::Engine::decode(&base64::engine::general_purpose::STANDARD,"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGPYueznfwAHzANYCRGt9wAAAABJRU5ErkJggg==").unwrap()).unwrap()),
    }
}

#[tokio::test]
async fn one_ingress_fans_out_metadata_counts_and_stop_to_independent_listeners() {
    let (node, node_stop, address) = node().await;
    let nodes = Nodes::new(vec![address.clone()]).unwrap();
    let (stop, stopped) = watch::channel(false);
    let owner = identity::Keypair::generate_ed25519();
    let publisher = public::Publisher::connect(
        nodes.clone(),
        owner.clone(),
        "Вечерний эфир".into(),
        "Вещатель".into(),
        stopped.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        publisher.link.owner,
        owner.public().to_peer_id().to_string()
    );
    let link = public::Link::parse(&publisher.link.to_link().unwrap()).unwrap();
    let (frames, rx) = mpsc::channel(8);
    let (metadata_tx, metadata) = watch::channel(info("Первый"));
    let host = tokio::spawn(publisher.run(rx, stopped.clone(), metadata, |_| {}));
    let mut listeners = Vec::new();
    let mut cancellations = Vec::new();
    let mut outputs = Vec::new();
    let mut media_outputs = Vec::new();
    for _ in 0..3 {
        let (cancel, stopped) = watch::channel(false);
        cancellations.push(cancel);
        let (ready, ready_rx) = tokio::sync::oneshot::channel();
        let mut ready = Some(ready);
        let (output, out) = mpsc::unbounded_channel();
        outputs.push(out);
        let (media, media_rx) = mpsc::unbounded_channel();
        media_outputs.push(media_rx);
        listeners.push(tokio::spawn(public::receive(
            link.clone(),
            identity::Keypair::generate_ed25519(),
            stopped,
            move |bytes| {
                let _ = output.send(bytes);
                true
            },
            move || {
                if let Some(tx) = ready.take() {
                    let _ = tx.send(());
                }
            },
            move |snapshot| {
                let _ = media.send(snapshot);
            },
        )));
        timeout(Duration::from_secs(5), ready_rx)
            .await
            .unwrap()
            .unwrap();
    }
    for index in 0..12u8 {
        frames.send(vec![index; 64]).await.unwrap();
        for receiver in &mut outputs {
            assert_eq!(
                timeout(Duration::from_secs(3), receiver.recv())
                    .await
                    .unwrap()
                    .unwrap(),
                vec![index; 64]
            );
        }
    }
    // Browser timeline/property bursts must be coalesced below the server rate limit.
    for index in 0..40 {
        metadata_tx.send(info(&format!("Событие {index}"))).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    metadata_tx.send(info("Второй")).unwrap();
    for receiver in &mut media_outputs {
        timeout(Duration::from_secs(3), async {
            while let Some(snapshot) = receiver.recv().await {
                if snapshot == info("Второй") {
                    return;
                }
            }
            panic!("No metadata");
        })
        .await
        .unwrap();
    }
    let (list, _) = public::catalog(
        nodes.clone(),
        identity::Keypair::generate_ed25519(),
        stopped.clone(),
    )
    .await
    .unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].info.listeners, 3);
    assert_eq!(list[0].info.track, "Второй");
    // Multiple liveness checks on a quiet room must not interrupt healthy listeners.
    tokio::time::sleep(Duration::from_secs(6)).await;
    let (quiet_list, _) = public::catalog(
        nodes.clone(),
        identity::Keypair::generate_ed25519(),
        stopped.clone(),
    )
    .await
    .unwrap();
    assert_eq!(quiet_list[0].info.listeners, 3);
    assert!(listeners.iter().all(|task| !task.is_finished()));
    // A late joiner needs a full cover, even when the last ingress update was a delta.
    let (late_stop, late_stopped) = watch::channel(false);
    let (late_media, mut late_rx) = mpsc::unbounded_channel();
    let late = tokio::spawn(public::receive(
        link.clone(),
        identity::Keypair::generate_ed25519(),
        late_stopped,
        |_| true,
        || {},
        move |snapshot| {
            let _ = late_media.send(snapshot);
        },
    ));
    assert_eq!(
        timeout(Duration::from_secs(5), late_rx.recv())
            .await
            .unwrap()
            .unwrap(),
        info("Второй")
    );
    late_stop.send(true).unwrap();
    timeout(Duration::from_secs(3), late)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    // No new audio is sent: disconnect still releases the listener slot.
    cancellations[0].send(true).unwrap();
    timeout(Duration::from_secs(3), listeners.remove(0))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    timeout(Duration::from_secs(5), async {
        loop {
            let (list, _) = public::catalog(
                nodes.clone(),
                identity::Keypair::generate_ed25519(),
                stopped.clone(),
            )
            .await
            .unwrap();
            if list[0].info.listeners == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    stop.send(true).unwrap();
    timeout(Duration::from_secs(5), host)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    for listener in listeners {
        timeout(Duration::from_secs(5), listener)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    let (_keep, stopped) = watch::channel(false);
    let (list, _) = public::catalog(nodes, identity::Keypair::generate_ed25519(), stopped)
        .await
        .unwrap();
    assert!(list.is_empty());
    node_stop.send(true).unwrap();
    node.await.unwrap();
}

#[tokio::test]
async fn catalog_uses_available_node_when_another_fails() {
    let (node, stop, address) = node().await;
    let missing = format!(
        "/ip4/127.0.0.1/tcp/1/p2p/{}",
        identity::Keypair::generate_ed25519().public().to_peer_id()
    )
    .parse()
    .unwrap();
    let nodes = Nodes::new(vec![missing, address]).unwrap();
    let (_keep, stopped) = watch::channel(false);
    let (rooms, available) = public::catalog(nodes, identity::Keypair::generate_ed25519(), stopped)
        .await
        .unwrap();
    assert!(rooms.is_empty());
    assert_eq!(available, 1);
    stop.send(true).unwrap();
    node.await.unwrap();
}

#[tokio::test]
async fn active_public_stream_recovers_on_backup_and_after_total_outage() {
    let (primary, _primary_stop, primary_addr) = node().await;
    let backup_key = identity::Keypair::generate_ed25519();
    let (backup, _backup_stop, backup_addr) =
        node_at(backup_key.clone(), "/ip4/127.0.0.1/tcp/0".parse().unwrap()).await;
    let nodes = Nodes::new(vec![primary_addr, backup_addr.clone()]).unwrap();
    let (stop, stopped) = watch::channel(false);
    let publisher = public::Publisher::connect(
        nodes.clone(),
        identity::Keypair::generate_ed25519(),
        "Восстановление".into(),
        "Автор".into(),
        stopped.clone(),
    )
    .await
    .unwrap();
    let link = publisher.link.clone();
    let (frames, rx) = mpsc::channel(8);
    let (metadata_tx, metadata) = watch::channel(info("До обрыва"));
    let (stats, mut stats_rx) = mpsc::unbounded_channel();
    let host = tokio::spawn(publisher.run(rx, stopped.clone(), metadata, move |s| {
        let _ = stats.send(s.recovering);
    }));
    let mut listeners = Vec::new();
    let mut media = Vec::new();
    let mut outputs = Vec::new();
    let (_listener_stop, listener_stopped) = watch::channel(false);
    for _ in 0..2 {
        let (output, output_rx) = mpsc::unbounded_channel();
        let (media_tx, media_rx) = mpsc::unbounded_channel();
        outputs.push(output_rx);
        media.push(media_rx);
        listeners.push(tokio::spawn(public::receive_with_nodes(
            link.clone(),
            identity::Keypair::generate_ed25519(),
            nodes.clone(),
            listener_stopped.clone(),
            move |frame| {
                let _ = output.send(frame);
                true
            },
            || {},
            move |s| {
                let _ = media_tx.send(s);
            },
            |_| {},
        )));
    }
    for receiver in &mut media {
        assert_eq!(
            timeout(Duration::from_secs(5), receiver.recv())
                .await
                .unwrap()
                .unwrap(),
            info("До обрыва")
        );
    }
    // No audio is needed to detect a dead node while the player is paused.
    primary.abort();
    let _ = primary.await;
    metadata_tx.send(info("Резервный узел")).unwrap();
    timeout(Duration::from_secs(20), async {
        while stats_rx.recv().await != Some(true) {}
        while stats_rx.recv().await != Some(false) {}
    })
    .await
    .unwrap();
    for receiver in &mut media {
        timeout(Duration::from_secs(20), async {
            while receiver.recv().await != Some(info("Резервный узел")) {}
        })
        .await
        .unwrap();
    }
    frames.send(vec![42; 64]).await.unwrap();
    for receiver in &mut outputs {
        assert_eq!(
            timeout(Duration::from_secs(5), receiver.recv())
                .await
                .unwrap()
                .unwrap(),
            vec![42; 64]
        );
    }
    // Lose the last node, then restore its persistent identity at the same address.
    while stats_rx.try_recv().is_ok() {}
    backup.abort();
    let _ = backup.await;
    timeout(Duration::from_secs(15), async {
        while stats_rx.recv().await != Some(true) {}
    })
    .await
    .unwrap();
    frames.send(vec![33; 64]).await.unwrap(); // stale audio must not be replayed
    metadata_tx.send(info("После восстановления")).unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let mut listen = backup_addr.clone();
    listen.pop();
    let (restored, restored_stop, _) = node_at(backup_key, listen).await;
    for receiver in &mut media {
        timeout(Duration::from_secs(25), async {
            while receiver.recv().await != Some(info("После восстановления")) {}
        })
        .await
        .unwrap();
    }
    frames.send(vec![99; 64]).await.unwrap();
    for receiver in &mut outputs {
        assert_eq!(
            timeout(Duration::from_secs(5), receiver.recv())
                .await
                .unwrap()
                .unwrap(),
            vec![99; 64]
        );
    }
    // Explicit Stop is terminal and removes the room without another recovery loop.
    stop.send(true).unwrap();
    timeout(Duration::from_secs(3), host)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    for listener in listeners {
        timeout(Duration::from_secs(3), listener)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    let (_keep, stopped) = watch::channel(false);
    let (rooms, _) = public::catalog(
        Nodes::new(vec![backup_addr]).unwrap(),
        identity::Keypair::generate_ed25519(),
        stopped,
    )
    .await
    .unwrap();
    assert!(rooms.is_empty());
    restored_stop.send(true).unwrap();
    restored.await.unwrap();
}

#[tokio::test]
async fn cancelling_recovery_stops_publisher_and_listener_promptly() {
    let (node, _node_stop, address) = node().await;
    let nodes = Nodes::new(vec![address]).unwrap();
    let (stop, stopped) = watch::channel(false);
    let publisher = public::Publisher::connect(
        nodes.clone(),
        identity::Keypair::generate_ed25519(),
        "Отмена".into(),
        "Автор".into(),
        stopped.clone(),
    )
    .await
    .unwrap();
    let link = publisher.link.clone();
    let (_frames, rx) = mpsc::channel(8);
    let (_media, metadata) = watch::channel(info("Тишина"));
    let (host_status, mut host_rx) = mpsc::unbounded_channel();
    let host = tokio::spawn(publisher.run(rx, stopped.clone(), metadata, move |s| {
        let _ = host_status.send(s.recovering);
    }));
    let (received, mut received_rx) = mpsc::unbounded_channel();
    let (listener_status, mut listener_rx) = mpsc::unbounded_channel();
    let listener = tokio::spawn(public::receive_with_nodes(
        link,
        identity::Keypair::generate_ed25519(),
        nodes,
        stopped,
        |_| true,
        || {},
        move |s| {
            let _ = received.send(s);
        },
        move |r| {
            let _ = listener_status.send(r);
        },
    ));
    timeout(Duration::from_secs(5), received_rx.recv())
        .await
        .unwrap()
        .unwrap();
    node.abort();
    let _ = node.await;
    timeout(Duration::from_secs(15), async {
        while host_rx.recv().await != Some(true) {}
        while listener_rx.recv().await != Some(true) {}
    })
    .await
    .unwrap();
    stop.send(true).unwrap();
    timeout(Duration::from_secs(2), host)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    timeout(Duration::from_secs(2), listener)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[test]
fn public_links_and_names_are_bounded_and_pinned() {
    assert!(public::title("\n").is_err());
    assert!(public::title(&"a".repeat(81)).is_err());
    assert!(public::Link::parse("aura://public/v1/abc").is_err());
    let link = public::Link {
        node: "/ip4/127.0.0.1/tcp/45071".into(),
        owner: identity::Keypair::generate_ed25519()
            .public()
            .to_peer_id()
            .to_string(),
    };
    assert!(public::Link::parse(&link.to_link().unwrap()).is_err());
}
