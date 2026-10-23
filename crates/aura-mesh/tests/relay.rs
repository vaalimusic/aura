use aura_mesh::{identity, Multiaddr};
use futures::StreamExt;
use libp2p::{multiaddr::Protocol, swarm::SwarmEvent};

async fn node() -> (tokio::task::JoinHandle<()>, Multiaddr) {
    let key = identity::Keypair::generate_ed25519();
    let peer = key.public().to_peer_id();
    let mut swarm = aura_mesh::node::build(key).unwrap();
    swarm
        .listen_on("/ip4/127.0.0.1/tcp/0".parse().unwrap())
        .unwrap();
    let (ready, ready_rx) = tokio::sync::oneshot::channel();
    let mut ready = Some(ready);
    let node = tokio::spawn(async move {
        loop {
            if let SwarmEvent::NewListenAddr { address, .. } = swarm.select_next_some().await {
                swarm.add_external_address(address.clone());
                if let Some(tx) = ready.take() {
                    let _ = tx.send(address.with(Protocol::P2p(peer)));
                }
            }
        }
    });
    (node, ready_rx.await.unwrap())
}

#[tokio::test]
async fn authorized_metadata_updates_without_audio_and_clears() {
    use aura_media::{Info, Playback, Snapshot};
    use aura_mesh::audio::{receive_with_metadata, Server};
    use std::time::Duration;
    use tokio::sync::{mpsc, watch};
    let (node, relay) = node().await;
    let (stop, stopped) = watch::channel(false);
    let (_frames, rx) = mpsc::channel(8);
    let (metadata_tx, metadata) = watch::channel(Snapshot::default());
    let server = Server::connect(
        relay,
        identity::Keypair::generate_ed25519(),
        stopped.clone(),
    )
    .await
    .unwrap();
    let invite = server.invitation().clone();
    let host = tokio::spawn(server.run_with_metadata(rx, stopped.clone(), metadata, |_| {}));
    let (events, mut events_rx) = mpsc::channel(8);
    let client = tokio::spawn(receive_with_metadata(
        invite,
        identity::Keypair::generate_ed25519(),
        stopped,
        |_| true,
        || {},
        |_| {},
        move |snapshot| {
            let _ = events.try_send(snapshot);
        },
    ));
    let initial = tokio::time::timeout(Duration::from_secs(5), events_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(initial, Snapshot::default());
    for title in ["Первый трек", "Новый трек"] {
        let snapshot = Snapshot {
            info: Some(Info {
                title: title.into(),
                artist: "Друг".into(),
                album: "Альбом".into(),
                app: "Плеер".into(),
                playback: Playback::Paused,
                position_ms: 12000,
                duration_ms: 100000,
            }),
            artwork: None,
        };
        metadata_tx.send(snapshot.clone()).unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), events_rx.recv())
                .await
                .unwrap()
                .unwrap(),
            snapshot
        );
    }
    metadata_tx.send(Snapshot::default()).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), events_rx.recv())
            .await
            .unwrap()
            .unwrap(),
        Snapshot::default()
    );
    stop.send(true).unwrap();
    host.await.unwrap().unwrap();
    client.await.unwrap().unwrap();
    node.abort();
}

#[tokio::test]
async fn relayed_streams_work_without_direct_listener_ports() {
    let (node, address) = node().await;
    let result = aura_mesh::probe(address).await;
    node.abort();
    assert!(result.is_ok(), "{result:?}");
}

#[tokio::test]
async fn invalid_capability_is_denied_and_stop_revokes_an_authorized_stream() {
    use aura_mesh::audio::{receive, Invitation, Server};
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    use std::time::Duration;
    let (node, address) = node().await;
    let (host_cancel, host_stop) = tokio::sync::watch::channel(false);
    let server = Server::connect(
        address,
        identity::Keypair::generate_ed25519(),
        host_stop.clone(),
    )
    .await
    .unwrap();
    let invite = server.invitation().clone();
    let mut wrong = serde_json::to_value(&invite).unwrap();
    wrong["token"] = URL_SAFE_NO_PAD.encode([0; 32]).into();
    let wrong = Invitation::parse(&format!(
        "aura://v2/{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&wrong).unwrap())
    ))
    .unwrap();
    let (frames, rx) = tokio::sync::mpsc::channel(8);
    let host = tokio::spawn(server.run(rx, host_stop, |_| {}));
    let (_client_cancel, client_stop) = tokio::sync::watch::channel(false);
    let denied = tokio::time::timeout(
        Duration::from_secs(5),
        receive(
            wrong.clone(),
            identity::Keypair::generate_ed25519(),
            client_stop.clone(),
            |_| panic!("Unauthorized audio"),
            || panic!("Unauthorized Ready"),
            |_| {},
        ),
    )
    .await
    .unwrap();
    assert!(denied.is_err());
    let denied = tokio::time::timeout(
        Duration::from_secs(5),
        aura_mesh::audio::receive_with_nodes_metadata(
            wrong,
            identity::Keypair::generate_ed25519(),
            aura_mesh::nodes::Nodes::new(vec![server_address(&invite)]).unwrap(),
            client_stop.clone(),
            |_| panic!("Unauthorized audio"),
            || panic!("Unauthorized Ready"),
            |_| {},
            |_| {},
            |_| panic!("Denied capabilities must not be retried"),
        ),
    )
    .await
    .unwrap();
    assert!(denied.is_err());
    let (ready, ready_rx) = tokio::sync::oneshot::channel();
    let mut ready = Some(ready);
    let nodes = aura_mesh::nodes::Nodes::new(vec![server_address(&invite)]).unwrap();
    let listener = tokio::spawn(aura_mesh::audio::receive_with_nodes_metadata(
        invite,
        identity::Keypair::generate_ed25519(),
        nodes,
        client_stop,
        |_| true,
        move || {
            if let Some(tx) = ready.take() {
                let _ = tx.send(());
            }
        },
        |_| {},
        |_| {},
        |_| panic!("An intentional Stop must not trigger recovery"),
    ));
    tokio::time::timeout(Duration::from_secs(5), ready_rx)
        .await
        .unwrap()
        .unwrap();
    frames.send(vec![1; 64]).await.unwrap();
    host_cancel.send(true).unwrap();
    host.await.unwrap().unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(3), listener)
        .await
        .unwrap()
        .unwrap()
        .is_err());
    node.abort();
}

fn server_address(invite: &aura_mesh::audio::Invitation) -> Multiaddr {
    let value = serde_json::to_value(invite).unwrap();
    let mut address: Multiaddr = value["address"].as_str().unwrap().parse().unwrap();
    address.pop();
    address.pop();
    address
}

#[test]
fn malformed_invitations_are_rejected() {
    use aura_mesh::audio::Invitation;
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    for input in ["", "aura://v2/AA", &"x".repeat(4097)] {
        assert!(Invitation::parse(input).is_err());
    }
    let invalid = serde_json::json!({"address":"/ip4/127.0.0.1/tcp/45071","token":URL_SAFE_NO_PAD.encode([0;32])});
    assert!(Invitation::parse(&format!(
        "aura://v2/{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&invalid).unwrap())
    ))
    .is_err());
}
