//! Real device pipeline through the public relay, using only our synthetic source.
#[cfg(target_os = "windows")]
pub fn run(relay: aura_mesh::Multiaddr) -> aura_audio::Result<()> {
    use std::{
        sync::{
            atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
            Arc,
        },
        time::Duration,
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    runtime.block_on(async move{
        let stop=Arc::new(AtomicBool::new(false));let(cancel,stopped)=tokio::sync::watch::channel(false);
        let server=aura_mesh::audio::Server::connect(relay.clone(),aura_mesh::identity::Keypair::generate_ed25519(),stopped.clone()).await?;
        // Keep the audio swarm polled while social discovery runs.
        let link=server.invitation().to_link()?;
        let(tx,rx)=tokio::sync::mpsc::channel(8);
        let mut art_png=vec![];{let mut encoder=png::Encoder::new(&mut art_png,8,8);encoder.set_color(png::ColorType::Rgb);encoder.set_depth(png::BitDepth::Eight);encoder.write_header()?.write_image_data(&[110u8,80,190].repeat(64))?;}
        let metadata=aura_media::Snapshot{info:Some(aura_media::Info{title:"Synthetic track".into(),artist:"Aura test".into(),album:"Test album".into(),app:"Synthetic source".into(),playback:aura_media::Playback::Playing,position_ms:0,duration_ms:30000}),artwork:Some(aura_media::thumbnail(&art_png)?)};
        let(metadata_tx,metadata_rx)=tokio::sync::watch::channel(metadata.clone());
        let host=tokio::spawn(server.run_with_metadata(rx,stopped.clone(),metadata_rx,|_|{}));
        let link=crate::social_smoke::exercise(relay,link).await?;
        let invite=aura_mesh::audio::Invitation::parse(&link)?;
        let mut command=std::process::Command::new(std::env::current_exe()?);command.args(["--test-tone","30"]);
        use std::os::windows::process::CommandExt;command.creation_flags(0x08000000);
        struct Child(std::process::Child);impl Drop for Child{fn drop(&mut self){let _=self.0.kill();let _=self.0.wait();}}
        let child=Child(command.spawn()?);tokio::time::sleep(Duration::from_millis(500)).await;
        let source=aura_audio::sources()?.into_iter().find(|s|s.pid==child.0.id()).ok_or("Synthetic audio session not found")?;
        let(packets,render)=std::sync::mpsc::sync_channel(16);let(ready,ready_rx)=tokio::sync::oneshot::channel();let mut ready=Some(ready);
        let(meta_events,mut meta_events_rx)=tokio::sync::mpsc::channel(8);
        let listener=tokio::spawn(aura_mesh::audio::receive_with_metadata(invite,aura_mesh::identity::Keypair::generate_ed25519(),stopped,move|bytes|{
            let Ok(p)=aura_network::Packet::decode(&bytes)else{return false;};packets.try_send(aura_audio::EncodedFrame{sequence:p.sequence,opus:p.payload}).is_ok()
        },move||{if let Some(tx)=ready.take(){let _=tx.send(());}},|_|{},move|snapshot|{let _=meta_events.try_send(snapshot);}));
        let frames=Arc::new(AtomicU64::new(0));let observed=frames.clone();let render_stop=stop.clone();
        let playback=tokio::task::spawn_blocking(move||aura_audio::playback(render,&render_stop,&AtomicU8::new(0),||{},|stats|{if stats.rms>0.0005{observed.store(stats.frames,Ordering::Relaxed);}}));
        let encoded=Arc::new(AtomicU64::new(0));let encoded_job=encoded.clone();let capture_stop=stop.clone();
        let capture=tokio::task::spawn_blocking(move||aura_audio::capture(&source,&capture_stop,||{},|frame|{
            encoded_job.fetch_add(1,Ordering::Relaxed);
            if let Ok(bytes)=(aura_network::Packet{sequence:frame.sequence,payload:frame.opus}).encode(){let _=tx.try_send(bytes);}
        },|_|{}));
        let connected=tokio::time::timeout(Duration::from_secs(15),ready_rx).await;
        let first=tokio::time::timeout(Duration::from_secs(8),meta_events_rx.recv()).await?.ok_or("Metadata stream ended")?;
        if first!=metadata{return Err("Metadata or artwork mismatch through relay".into());}
        let mut changed=metadata;changed.info.as_mut().unwrap().title="Next track".into();changed.info.as_mut().unwrap().playback=aura_media::Playback::Paused;
        metadata_tx.send(changed.clone())?;
        if tokio::time::timeout(Duration::from_secs(8),meta_events_rx.recv()).await?.ok_or("Metadata stream ended")?!=changed{return Err("Metadata update mismatch".into());}
        tokio::time::sleep(Duration::from_secs(3)).await;
        stop.store(true,Ordering::Release);let _=cancel.send(true);
        let cap=capture.await?;let play=playback.await?;let host=host.await?;let listener=listener.await?;
        cap?;play?;host?;listener?;connected??;
        let captured=encoded.load(Ordering::Relaxed);let decoded=frames.load(Ordering::Relaxed);
        if captured<30||decoded<20{return Err(format!("Mesh audio signal insufficient: captured={captured}, decoded={decoded}").into());}
        println!("Mesh audio smoke PASS: synthetic process -> Opus -> public Circuit Relay -> end-to-end Noise -> jitter -> muted WASAPI; captured={captured}, decoded={decoded}");
        println!("Metadata smoke PASS: authenticated title, artist, album, playback, timeline and artwork; track/pause update received independently of Opus");
        Ok(())
    })
}
#[cfg(not(target_os = "windows"))]
pub fn run(_: aura_mesh::Multiaddr) -> aura_audio::Result<()> {
    Err("Device smoke requires Windows".into())
}
