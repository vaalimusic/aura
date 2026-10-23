//! Explicit device test: captures only a child playing our quiet synthetic tone.
#[cfg(target_os = "windows")]
pub fn run() -> aura_audio::Result<()> {
    use std::{
        sync::{
            atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
            Arc,
        },
        time::Duration,
    };
    use tokio::sync::{mpsc, watch};
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command.args(["--test-tone", "6"]);
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    let child = command.spawn()?;
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let child = Child(child);
    std::thread::sleep(Duration::from_millis(500));
    let source = aura_audio::sources()?
        .into_iter()
        .find(|s| s.pid == child.0.id())
        .ok_or("Synthetic source audio session not found")?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let stop=Arc::new(AtomicBool::new(false));
        let (cancel,stopped)=watch::channel(false);
        let server=aura_network::Server::bind("127.0.0.1:0".parse()?,None)?;
        let invite=aura_network::Invitation::parse(&server.invitation().to_link()?)?;
        let (send,audio)=mpsc::channel(8);
        let net_server=tokio::spawn(server.run(audio,stopped.clone(),|_|{}));
        let (packets,render)=std::sync::mpsc::sync_channel(16);
        let (ready_tx,ready_rx)=tokio::sync::oneshot::channel();let mut ready_tx=Some(ready_tx);
        let net_client=tokio::spawn(aura_network::receive(invite,move|p|packets.try_send(aura_audio::EncodedFrame{sequence:p.sequence,opus:p.payload}).is_ok(),stopped,move||{if let Some(tx)=ready_tx.take(){let _=tx.send(());}},|_|{}));
        let frames=Arc::new(AtomicU64::new(0));let observed=frames.clone();let render_stop=stop.clone();
        let playback=tokio::task::spawn_blocking(move||aura_audio::playback(render,&render_stop,&AtomicU8::new(0),||{},|stats|{if stats.rms>0.0005{observed.store(stats.frames,Ordering::Relaxed);}}));
        let encoded=Arc::new(AtomicU64::new(0));let encoded_job=encoded.clone();let capture_stop=stop.clone();
        let capture=tokio::task::spawn_blocking(move||aura_audio::capture(&source,&capture_stop,||{},|frame|{encoded_job.fetch_add(1,Ordering::Relaxed);let _=send.try_send(aura_network::Packet{sequence:frame.sequence,payload:frame.opus});},|_|{}));
        let connected=tokio::time::timeout(Duration::from_secs(4),ready_rx).await;
        tokio::time::sleep(Duration::from_secs(2)).await;
        stop.store(true,Ordering::Release);let _=cancel.send(true);
        let capture_result=capture.await?;let playback_result=playback.await?;
        let server_result=net_server.await?;let client_result=net_client.await?;
        capture_result?;playback_result?;server_result?;client_result?;connected??;
        let captured=encoded.load(Ordering::Relaxed);let played=frames.load(Ordering::Relaxed);
        if captured<20||played<10{return Err(format!("Device pipeline did not receive enough signal: captured={captured}, decoded={played}").into());}
        println!("Device smoke PASS: process loopback -> Opus -> QUIC/TLS -> jitter -> muted WASAPI playback; captured={captured}, decoded={played}");
        Ok(())
    })
}
#[cfg(not(target_os = "windows"))]
pub fn run() -> aura_audio::Result<()> {
    Err("Device smoke requires Windows".into())
}
