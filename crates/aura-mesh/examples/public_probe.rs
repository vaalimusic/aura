//! Temporary public room with synthetic packets; never captures user audio.
use aura_mesh::{identity, nodes::Nodes, public, Result};
use tokio::{
    sync::{mpsc, watch},
    time::{timeout, Duration},
};

#[tokio::main]
async fn main() -> Result<()> {
    let nodes = Nodes::parse(
        &std::env::args()
            .nth(1)
            .ok_or("Usage: public_probe '<nodes>' [listeners]")?,
    )?;
    let count: usize = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "3".into())
        .parse()?;
    if !(1..=32).contains(&count) {
        return Err("Listeners must be 1..32".into());
    }
    let recovery = std::env::args().nth(3).as_deref() == Some("--recover");
    if recovery && nodes.addresses().len() < 2 {
        return Err("Recovery probe requires at least two pinned nodes".into());
    }
    let (stop, stopped) = watch::channel(false);
    let publisher = public::Publisher::connect(
        nodes.clone(),
        identity::Keypair::generate_ed25519(),
        "Aura · проверка раздачи".into(),
        "Aura diagnostic".into(),
        stopped.clone(),
    )
    .await?;
    let link = publisher.link.clone();
    let (frames, rx) = mpsc::channel(8);
    let (_media, metadata) = watch::channel(aura_media::Snapshot::default());
    let (status, mut status_rx) = mpsc::unbounded_channel();
    let host = tokio::spawn(publisher.run(rx, stopped.clone(), metadata, move |s| {
        let _ = status.send(s.recovering);
    }));
    let mut clients = Vec::new();
    let mut outputs = Vec::new();
    let mut generations = Vec::new();
    let result=async {
        for _ in 0..count {
            let (ready,ready_rx)=tokio::sync::oneshot::channel();let mut ready=Some(ready);
            let (output,receiver)=mpsc::unbounded_channel();outputs.push(receiver);
            let (generation,generation_rx)=watch::channel(0u32);generations.push(generation_rx);let mut number=0;
            clients.push(tokio::spawn(public::receive_with_nodes(link.clone(),identity::Keypair::generate_ed25519(),nodes.clone(),stopped.clone(),move |bytes|{let _=output.send(bytes);true},move ||{number+=1;generation.send_replace(number);if let Some(tx)=ready.take(){let _=tx.send(());}},|_|{},|_|{})));
            timeout(Duration::from_secs(12),ready_rx).await??;
        }
        let (entries,available)=public::catalog(nodes.clone(),identity::Keypair::generate_ed25519(),stopped.clone()).await?;
        let room=entries.iter().find(|e|e.info.owner==link.owner).ok_or("Published room absent from catalog")?;
        if room.info.listeners!=count{return Err("Wrong listener count".into());}
        for sequence in 0..50u8 {
            frames.send(vec![sequence;64]).await?;
            for receiver in &mut outputs {let frame=timeout(Duration::from_secs(5),receiver.recv()).await?.ok_or("Listener closed")?;if frame!=vec![sequence;64]{return Err("Frame mismatch".into());}}
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        println!("PASS: one publisher ingress, {count} independent listeners, 50/50 identical frames each, catalog via {available} node(s)");
        if recovery {
            while status_rx.try_recv().is_ok() {}
            println!("READY_STOP_PRIMARY: stop only the publishing node; keep backup running");
            timeout(Duration::from_secs(60),async {
                while status_rx.recv().await != Some(true) {}
                while status_rx.recv().await != Some(false) {}
                for receiver in &mut generations {
                    while *receiver.borrow_and_update()<2 {receiver.changed().await?;}
                }
                Ok::<_,Box<dyn std::error::Error+Send+Sync>>(())
            }).await??;
            for receiver in &mut outputs {while receiver.try_recv().is_ok() {}}
            for sequence in 100..150u8 {
                frames.send(vec![sequence;64]).await?;
                for receiver in &mut outputs {let frame=timeout(Duration::from_secs(5),receiver.recv()).await?.ok_or("Listener closed after recovery")?;if frame!=vec![sequence;64]{return Err("Recovered frame mismatch".into());}}
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let (entries,_)=public::catalog(nodes.clone(),identity::Keypair::generate_ed25519(),stopped.clone()).await?;
            let recovered=entries.iter().find(|entry|entry.info.owner==link.owner).ok_or("Recovered room absent from catalog")?;
            if recovered.link.node==link.node || recovered.info.listeners!=count {return Err("Room did not move to backup with all listeners".into());}
            println!("PASS: active publisher and {count} listeners recovered on {}; 50/50 new frames each; original link and author retained", recovered.link.node);
        }
        Ok::<_,Box<dyn std::error::Error+Send+Sync>>(())
    }.await;
    let _ = stop.send(true);
    let _ = timeout(Duration::from_secs(5), host).await;
    for client in clients {
        let _ = timeout(Duration::from_secs(5), client).await;
    }
    result?;
    let (_keep, stopped) = watch::channel(false);
    let (entries, _) =
        public::catalog(nodes, identity::Keypair::generate_ed25519(), stopped).await?;
    if entries.iter().any(|e| e.info.owner == link.owner) {
        return Err("Room did not leave catalog after Stop".into());
    }
    println!("PASS: temporary public room removed after Stop");
    Ok(())
}
