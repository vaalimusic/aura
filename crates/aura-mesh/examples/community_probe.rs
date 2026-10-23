//! Explicit live fault probe. Uses only temporary identities; never a desktop profile.
//! Stop/restore only Aura Node services when the corresponding READY message appears.
use aura_mesh::{identity, nodes::Nodes, social, Multiaddr, Result};
use aura_social::{Intent, Store};
use tokio::{
    sync::mpsc,
    task::JoinHandle,
    time::{timeout, Duration},
};

struct ClientTask(JoinHandle<Result<()>>);
impl Drop for ClientTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn store(path: &std::path::Path, key: &identity::Keypair, contacts: &[String]) -> Result<Store> {
    let mut store = Store::open(path, &key.public().to_peer_id().to_string())?;
    for id in contacts {
        store.add(id)?;
        store.observe(id, "Проверка", Intent::Approved)?;
    }
    Ok(store)
}

fn client(
    key: identity::Keypair,
    store: Store,
    nodes: Nodes,
) -> (
    ClientTask,
    mpsc::Sender<social::Command>,
    mpsc::UnboundedReceiver<social::Event>,
) {
    let (commands, rx) = mpsc::channel(16);
    let (events, event_rx) = mpsc::unbounded_channel();
    let task = ClientTask(tokio::spawn(social::run_with_nodes(
        key,
        store,
        nodes,
        rx,
        move |event| {
            let _ = events.send(event);
        },
    )));
    (task, commands, event_rx)
}

async fn snapshot(
    rx: &mut mpsc::UnboundedReceiver<social::Event>,
    phase: &str,
    check: impl Fn(&social::Snapshot) -> bool,
) -> Result<()> {
    timeout(Duration::from_secs(90), async {
        while let Some(event) = rx.recv().await {
            match event {
                social::Event::Snapshot(state) if check(&state) => return Ok(()),
                social::Event::Notice(error) => eprintln!("Probe notice: {error}"),
                _ => {}
            }
        }
        Err("Probe social worker stopped".into())
    })
    .await
    .map_err(|_| format!("Timed out in phase: {phase}"))?
}

fn online(state: &social::Snapshot, id: &str) -> bool {
    state
        .friends
        .iter()
        .any(|friend| friend.contact.id == id && friend.online)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let primary: Multiaddr = args
        .get(1)
        .ok_or("Usage: community_probe <primary> <backup>")?
        .parse()?;
    let backup: Multiaddr = args.get(2).ok_or("Backup node missing")?.parse()?;
    let nodes = Nodes::new(vec![primary, backup.clone()])?;
    let temp = tempfile::tempdir()?;
    let a = identity::Keypair::generate_ed25519();
    let b = identity::Keypair::generate_ed25519();
    let c = identity::Keypair::generate_ed25519();
    let d = identity::Keypair::generate_ed25519();
    let aid = a.public().to_peer_id().to_string();
    let bid = b.public().to_peer_id().to_string();
    let cid = c.public().to_peer_id().to_string();
    let did = d.public().to_peer_id().to_string();
    let sa = store(&temp.path().join("a.json"), &a, std::slice::from_ref(&bid))?;
    let sb = store(
        &temp.path().join("b.json"),
        &b,
        &[aid.clone(), cid.clone(), did.clone()],
    )?;
    let sc = store(&temp.path().join("c.json"), &c, std::slice::from_ref(&bid))?;
    let sd = store(&temp.path().join("d.json"), &d, std::slice::from_ref(&bid))?;
    let (_a, _ac, mut ar) = client(a, sa, nodes.clone());
    let (_b, _bc, mut br) = client(b, sb, nodes.clone());
    println!("Waiting for two public-node reservations and authenticated friend presence");
    snapshot(&mut ar, "initial A", |s| {
        s.nodes_connected == 2 && online(s, &bid)
    })
    .await?;
    snapshot(&mut br, "initial B", |s| {
        s.nodes_connected == 2 && online(s, &aid)
    })
    .await?;
    println!("READY_STOP_PRIMARY");
    snapshot(&mut ar, "primary failure A", |s| {
        s.nodes_connected == 1 && s.connected
    })
    .await?;
    snapshot(&mut br, "primary failure B", |s| {
        s.nodes_connected == 1 && s.connected
    })
    .await?;
    let (_c, _cc, mut cr) = client(c, sc, Nodes::new(vec![backup.clone()])?);
    snapshot(&mut cr, "new client via backup", |s| {
        s.connected && online(s, &bid)
    })
    .await?;
    snapshot(&mut br, "B sees new client", |s| {
        s.nodes_connected == 1 && online(s, &cid)
    })
    .await?;
    let (selected, frames) = aura_mesh::probe_nodes(&nodes).await?;
    if selected != backup {
        return Err("Expected audio fallback to the backup node".into());
    }
    println!("BACKUP_DATA_PASS frames={frames}/50; new client joined while primary was stopped");
    println!("READY_RESTORE_PRIMARY");
    snapshot(&mut ar, "primary restored A", |s| {
        s.nodes_connected == 2 && online(s, &bid)
    })
    .await?;
    snapshot(&mut br, "primary restored B", |s| {
        s.nodes_connected == 2 && online(s, &aid)
    })
    .await?;
    println!("READY_STOP_BOTH");
    snapshot(&mut ar, "both nodes offline A", |s| {
        !s.connected && s.nodes_connected == 0
    })
    .await?;
    snapshot(&mut br, "both nodes offline B", |s| {
        !s.connected && s.nodes_connected == 0
    })
    .await?;
    let (_d, _dc, mut dr) = client(d, sd, nodes);
    snapshot(&mut dr, "new client offline", |s| !s.connected).await?;
    println!("READY_RESTORE_BOTH");
    snapshot(&mut ar, "network recovered A", |s| {
        s.nodes_connected == 2 && online(s, &bid)
    })
    .await?;
    snapshot(&mut br, "network recovered B", |s| {
        s.nodes_connected == 2 && online(s, &did)
    })
    .await?;
    snapshot(&mut dr, "new client recovered", |s| {
        s.nodes_connected == 2 && online(s, &bid)
    })
    .await?;
    println!("COMMUNITY_LIVE_PASS: two nodes, primary loss, backup data, new-client join, primary return, full outage, automatic recovery; isolated profiles");
    Ok(())
}
