#[tokio::main]
async fn main() -> aura_mesh::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_some_and(|s| s == "--probe-nodes") {
        let nodes = aura_mesh::nodes::Nodes::parse(
            args.get(2)
                .ok_or("Usage: aura-node --probe-nodes '<node1>;<node2>'")?,
        )?;
        let mut reachable = 0;
        for node in nodes.addresses() {
            let start = std::time::Instant::now();
            match aura_mesh::probe(node.clone()).await {
                Ok(frames) => {
                    reachable += 1;
                    println!(
                        "PASS {node}: {frames}/50 frames, {} ms",
                        start.elapsed().as_millis()
                    );
                }
                Err(error) => println!("FAIL {node}: {error}"),
            }
        }
        println!("Available nodes: {reachable}/{}", nodes.addresses().len());
        if reachable == 0 {
            return Err("No available Aura Nodes".into());
        }
        return Ok(());
    }
    if args.get(1).is_some_and(|s| s == "--probe") {
        let relay = args
            .get(2)
            .ok_or("Usage: aura-node --probe <relay-multiaddr>")?
            .parse()?;
        let frames = aura_mesh::probe(relay).await?;
        println!("Relay probe PASS: {frames}/50 frames, two independent authenticated peers, end-to-end Noise");
        return Ok(());
    }
    let path =
        std::env::var("AURA_NODE_KEY").unwrap_or_else(|_| "/var/lib/aura-node/identity.key".into());
    let key = aura_mesh::key::load(std::path::Path::new(&path))?;
    let peer = key.public().to_peer_id();
    if std::env::args().any(|s| s == "--identity") {
        println!("{peer}");
        return Ok(());
    }
    let listen = std::env::var("AURA_NODE_LISTEN")
        .unwrap_or_else(|_| "/ip4/0.0.0.0/tcp/45071".into())
        .parse()?;
    let advertise = std::env::var("AURA_NODE_ADVERTISE")
        .unwrap_or_else(|_| "/dns4/aura.ltwo.ru/tcp/45071".into())
        .parse()?;
    let (cancel, stopped) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("SIGTERM handler");
            tokio::select! {_=term.recv()=>{},_=tokio::signal::ctrl_c()=>{}}
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
        let _ = cancel.send(true);
    });
    println!("Aura node {peer}");
    aura_mesh::node::run(
        aura_mesh::node::build(key)?,
        listen,
        advertise,
        stopped,
        |addr| println!("Listening {addr}/p2p/{peer}"),
    )
    .await
}
