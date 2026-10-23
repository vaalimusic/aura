//! Authenticated libp2p transport and Circuit Relay v2 for Aura.
pub use libp2p::{identity, Multiaddr, PeerId};
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub mod audio;
pub mod key;
pub mod node;
pub mod nodes;
pub mod public;
pub mod social;
pub async fn probe(relay: Multiaddr) -> Result<u64> {
    let (_, frames) = probe_nodes(&nodes::Nodes::new(vec![relay])?).await?;
    Ok(frames)
}

/// Probe the actual startup fallback path without capturing or playing user audio.
pub async fn probe_nodes(nodes: &nodes::Nodes) -> Result<(Multiaddr, u64)> {
    use std::{
        sync::{
            atomic::{AtomicU64, Ordering},
            Arc,
        },
        time::Duration,
    };
    let (cancel, stopped) = tokio::sync::watch::channel(false);
    let server = audio::Server::connect_nodes(
        nodes,
        identity::Keypair::generate_ed25519(),
        stopped.clone(),
    )
    .await?;
    let selected = server.relay_address().clone();
    let invite = audio::Invitation::parse(&server.invitation().to_link()?)?;
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    let host = tokio::spawn(server.run(rx, stopped.clone(), |_| {}));
    let received = Arc::new(AtomicU64::new(0));
    let observed = received.clone();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let mut ready_tx = Some(ready_tx);
    let listener = tokio::spawn(audio::receive(
        invite,
        identity::Keypair::generate_ed25519(),
        stopped,
        move |bytes| {
            if bytes.len() == 64 && bytes[..4] == *b"AUR1" {
                observed.fetch_add(1, Ordering::Relaxed);
            }
            true
        },
        move || {
            if let Some(tx) = ready_tx.take() {
                let _ = tx.send(());
            }
        },
        |_| {},
    ));
    let result = async {
        tokio::time::timeout(Duration::from_secs(20), ready_rx).await??;
        for seq in 0..50u64 {
            let mut frame = vec![0u8; 64];
            frame[..4].copy_from_slice(b"AUR1");
            frame[4..12].copy_from_slice(&seq.to_be_bytes());
            tx.send(frame).await?;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    }
    .await;
    let _ = cancel.send(true);
    let host_result = host.await?;
    let client_result = listener.await?;
    result?;
    host_result?;
    client_result?;
    let frames = received.load(Ordering::Relaxed);
    if frames < 40 {
        return Err(format!("Relay delivered only {frames}/50 frames").into());
    }
    Ok((selected, frames))
}
