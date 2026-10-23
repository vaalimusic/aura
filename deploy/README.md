# Aura Node · pilot deployment

Public multiaddr:

```text
/dns4/aura.ltwo.ru/tcp/45071/p2p/12D3KooWGjkf7GrM1GxVmyVobwki2Mr7Uyi5PMtiMGc94vWxePHB
```

Independent backup, deployed 2026-10-22 on `87.242.86.70` (Ubuntu 24.04):

```text
/dns4/aura2.ltwo.ru/tcp/45071/p2p/12D3KooWB3V4copXp6jDuEN8Z1VQg8tnXVMst5uAszKBxfin9vbY
```

Both addresses are shipped in the desktop defaults. Existing files generated with only the original default are upgraded atomically; custom node lists and environment overrides remain unchanged. The second server runs the same restricted systemd service as user `aura`; its current release is `/opt/aura/releases/20261022-a169faaa`. Identity is retained at `/var/lib/aura-node/identity.key` with mode 0600. Administrative credentials are not part of the deployment files.

The node runs on Ubuntu 24.04 under systemd as user `aura`. Existing HAProxy listeners on 80/443/2106/7777 are preserved. Only TCP 45071 was added for Aura. The relay uses libp2p Noise authentication and Circuit Relay v2, not HTTP. A web browser is not a health check for this endpoint.

The Ed25519 node key lives at `/var/lib/aura-node/identity.key` (0600, owner aura). Preserve it across deployments: replacing it changes the pinned relay PeerId. Configuration: `/etc/aura/node.env`; executable: `/opt/aura/current/aura-node`. Neither root credentials nor private keys belong in the repository.

Resource limits: systemd MemoryMax=256M, CPUQuota=100%, TasksMax=64, LimitNOFILE=4096; 32 relay reservations, 16 circuits, 4 circuits per peer, 2-hour / 256-MiB per-circuit caps plus library per-IP/per-peer rate limits. This is a bounded pilot public relay, not an unlimited media CDN. The circuit limits can end a long listening session; automatic reconnect is a subsequent step.

For private broadcasts the node cannot decrypt the inner client-to-client Noise stream. It can observe connection metadata, timing and traffic volume; private access is checked by the broadcaster using the invitation capability. Public rooms use a separate `/aura/public/1` service: the node receives compressed audio over Noise and fans it out over independently encrypted connections, so the serving node can access public audio. The public catalog never contains private invitation capabilities. The server also exposes a Kademlia bootstrap behaviour.

## Native deployment

Install the Rust stable toolchain and a C compiler on Linux, then:

```sh
cargo build --release --locked -p aura-node
sudo bash deploy/install-node.sh
# Allow TCP 45071 in the host/provider firewall if necessary.
```

The installer preserves the existing node key and environment file. It installs a versioned binary and switches the `current` symlink. On this initial VPS deployment the target build directory is `/opt/aura/build/target`, sources are `/opt/aura/build/source`, and the first release is `/opt/aura/releases/20261021-01`.

```sh
systemctl status aura-node
journalctl -u aura-node -n 50 --no-pager
systemctl show aura-node -p MemoryCurrent -p NRestarts
runuser -u aura -- /opt/aura/current/aura-node --identity
```

After deploying a node binary with the probe command, verify from an external host:

```sh
aura-node --probe /dns4/aura.ltwo.ru/tcp/45071/p2p/12D3KooWGjkf7GrM1GxVmyVobwki2Mr7Uyi5PMtiMGc94vWxePHB
```

The probe creates two independent client identities, reserves a circuit and sends 50 framed messages through the VPS. It does not open inbound client listeners or use UDP 45070. The desktop's `--mesh-smoke` additionally tests real process capture, Opus and muted WASAPI playout through this circuit.

## Desktop configuration

Default mode is «Через Aura Node». Both machines must run the new build to exchange `aura://v2/` invitations. Old `aura://v1/` direct QUIC links still work. Click the route selector in the audio card while idle to choose direct mode. For a different node, set `AURA_RELAY` to its full multiaddr including the pinned PeerId.

For multiple nodes, edit `%LOCALAPPDATA%/Aura/nodes.txt` (or `AURA_PROFILE_DIR/nodes.txt`): one full multiaddr per line, up to four independently identified nodes. Restart Aura after editing. `AURA_RELAYS="<node1>;<node2>"` overrides the file; legacy `AURA_RELAY` remains supported. Both desktops must share a reachable configured node. Presence reserves all configured routes; new broadcasts try them in order. Active private and public broadcasts recover through configured reachable nodes; both desktops must share one. See [Community Nodes](../docs/architecture/community-nodes.md).

## Docker Compose for an independent node

From the repository root, copy `deploy/node.env.example` to `deploy/.env`, replace the advertise address with that host's public DNS/IP and TCP port, then run:

```sh
docker compose --env-file deploy/.env -f deploy/compose.yaml up -d --build
docker compose --env-file deploy/.env -f deploy/compose.yaml exec aura-node aura-node --identity
docker compose --env-file deploy/.env -f deploy/compose.yaml logs --tail 50
```

Use the printed public PeerId to complete `/dns4/your-node.example/tcp/45071/p2p/<PeerId>` on the clients. The `node-identity` volume preserves that PeerId across restarts; preserve it when updating the service. The image runs as UID 10001, with a read-only root and a writable identity volume. Compose limits it to 256 MiB RAM, one CPU, 64 processes and 4096 open files; protocol-level relay limits are the same as native deployment. Publishing the port still requires provider/firewall reachability.

To test configured routes from a host with the node binary, use `aura-node --probe-nodes '<node1>;<node2>'`. Each node gets an independent authenticated frame probe; the command prints per-node results and fails if none are available. It never prints bearer invitations or private keys. Two containers on one host test the software but do not protect against loss of that host.

Local container validation: image build, isolated Compose startup, 50/50 authenticated probe frames, UID 10001, key permissions 0600 and identical public PeerId after restart all passed. The temporary test container, network and identity volume were removed. The existing public server separately passed the new node binary's 50-frame probe; its deployment was not changed.

Windows identity is DPAPI-protected at `%LOCALAPPDATA%/Aura/identity.key`, scoped to the current Windows user. `AURA_PROFILE_DIR` can select a separate profile directory for testing. «Мой Aura ID» copies the public PeerId; it never copies the private key.

The friends build reserves a persistent social circuit endpoint under this stable identity and uses `/aura/social/1` for authenticated request/response presence. Saved contacts and consent live only on the clients; this requires no new server endpoint, database or firewall rule. Audio uses a separate ephemeral identity per session to avoid replacing the stable identity's reservation. Both clients must run the friends build. Presence connections consume the existing pilot's circuit quota; the current limits target small groups, not large friend lists.

## Verification · 2026-10-21

- 144 workspace tests passed; strict Clippy and rustfmt checks passed.
- External probe through the public VPS delivered 50/50 framed messages between two independent client identities, without inbound client listeners.
- Release device smoke through the public VPS passed: 202 captured frames, 142 decoded frames with nonzero signal and muted output. Capture starts before listener authorization, so the two counters cover different time windows and are not a packet-loss measurement.
- Invalid capabilities are rejected before Ready/media; broadcaster stop closes an authorized relay stream; corrupted identity files are rejected rather than silently replaced.
- Node restart retained the pinned PeerId. The systemd service is active, key permissions are 0600/aura, and the pre-existing HAProxy service remains active.
- After receiving the relay-enabled build and the two-client test instructions, the user confirmed that it works. This records the successful manual relay test; session duration and end-to-end latency were not measured.
- Friends release probe on the public VPS passed with isolated profiles: request, mutual consent, authenticated presence, current broadcast invitation, broadcast removal, offline expiry, presence reconnect and contact removal. Unapproved contacts cannot request invitations. The resolved invitation then passed real process capture, Opus and muted WASAPI playout: 203 captured / 150 decoded frames with nonzero signal. These counters cover different time windows.
- The friends lifecycle also passes against a local relay in the test suite without public Internet or audio devices. Presence reconnect is implemented; audio reconnect, public discovery, Smart Feed and hole punching remain future work.
- The metadata release probe passed over the same public node: independently authenticated track/artist/album/state/timeline and JPEG cover, then a track/pause update while Opus continued. Synthetic audio passed with muted WASAPI output: 227 captured / 163 decoded frames (different observation windows). Windows GSMTC read the selected Spotify source's actual properties and artwork; an unrelated VirtualBox audio source correctly produced no media metadata. No server change or new port is needed for `/aura/metadata/1`.

## Verification · 2026-10-22 · two independent VPS hosts

- DNS for `aura2.ltwo.ru` resolves to the second host. An external authenticated probe delivered 50/50 test frames through it.
- Four temporary client identities and isolated contact files exercised both public relays. With the primary Aura Node service stopped, a new client using only the backup found its accepted friend; the audio startup fallback path selected the backup and delivered 50/50 frames.
- Restoring the primary service recovered both reservations. Stopping both Aura Node services produced zero ready nodes; restoring them recovered reservations and authenticated friend presence automatically, including a client started during the outage.
- Each controlled stop had a two-minute automatic restoration guard. Both services were verified active afterwards, guards were removed, and the primary host's existing HAProxy service remained active. The second service is enabled at boot, runs as `aura`, and retained the same public PeerId and key permissions after restart.
- The live probe is `cargo run -p aura-mesh --example community_probe -- '<primary>' '<backup>'`. It does not stop remote services itself: `READY_STOP_PRIMARY`, `READY_RESTORE_PRIMARY`, `READY_STOP_BOTH` and `READY_RESTORE_BOTH` coordinate the operator's controlled service actions. It uses no desktop profile, captures no user audio and prints no bearer invitations.
- These tests use independent client swarms on one Windows PC and two real remote VPS hosts. They validate connectivity and fallback, not playback latency on two physical client PCs. Migrating an already running audio circuit after relay loss remains pending.
- Final release checks passed: all workspace tests, strict Clippy and both release builds. After restoring services, `--probe-nodes` delivered 50/50 frames through each public node and reported 2/2 available. The desktop distribution was replaced at its existing `artifacts/aura-desktop.exe` path and verified against the build hash.

### Subsequent desktop audio recovery work

The desktop now reserves its audio identity on configured relays and reconnects a listener with the same invitation/capability after a transport failure. Local integration tests exercise active frame delivery after primary failure, recovery after both relays stop and one returns, metadata updates and cancellation during recovery. Intentional Stop uses an acknowledged end marker and does not trigger recovery. This requires updated desktops on both sides; the existing VPS relay binaries do not need replacement. The earlier real-VPS probe above covered startup fallback and presence, not this new active-audio recovery path. A two-PC real-music failover check remains outstanding.

## Public rooms pilot · 2026-10-22

Both hosts now run `/opt/aura/releases/20261022-public-recovery-d728ba28/aura-node`; identity keys, pinned PeerIds, existing ports and service restrictions were retained. Linux binary SHA256: `d728ba2895dbe6bbfac32eefc2bb9b96b5de6366cf136a87e47f5f6803fe3cd7`. Releases were switched one host at a time, with the previous target retained for rollback. This release includes full-cover caching for late joiners; the publisher coalesces metadata updates to at most two per second and transmits unchanged artwork only once per ingress connection.

The service accepts one authenticated publisher stream and distributes the original frames to listeners. Pilot limits: 16 rooms, 32 listeners per room, 96 active public jobs, 12 new requests/10 seconds/PeerId, bounded rate tracking, 120 ingress messages/second and four metadata messages/second. Queues hold 16 shared packets; slow writers time out independently after three seconds. The node-wide 64-connection cap also covers private/administrative protocol traffic, so these room limits do not promise that every room can reach its maximum simultaneously. Publisher heartbeat is every two seconds; 15 seconds without ingress removes a room. Intentional Stop and EOF remove it immediately.

The desktop merges catalogs from all configured nodes every ten seconds. There is no server-to-server catalog replication or cross-node fanout yet. A public publisher sends to one selected node and automatically reconnects on a configured backup after transport failure, keeping its author identity, title and latest metadata/cover. Capture continues and queued stale frames are dropped. Listeners follow the same author through the original pinned link node and their own configured nodes. Recovery attempts are bounded to 60 seconds after failure detection; explicit Stop and cancellation are terminal. Protocol /aura/public/2 adds two-second listener heartbeats (ten-second read timeout); nodes still accept v1 and clients can fall back to old nodes. Full recovery requires updated nodes and clients because older nodes do not distinguish an unexpected publisher disconnect from Stop. Public links retain their v1 URL format.

Verification: Windows local integration tests covered three listeners, identical frames, metadata changes, counts while silent, cancellation, room cleanup and partial node failure. The Linux image delivered 50/50 frames to each of ten independent listeners. The backup VPS delivered 50/50 to each of three listeners; the primary delivered 50/50 to each of ten with the combined catalog reporting two nodes. Temporary rooms were removed after Stop. Private relay diagnostics then delivered 50/50 through each VPS. These probes use synthetic packets on one Windows client host; they do not measure real two-PC audio latency or maximum VPS capacity.

Run an explicit public fanout probe with `cargo run -p aura-mesh --example public_probe -- '<node1>;<node2>' 10`. It temporarily creates a clearly labelled diagnostic public room and captures no user audio. Shared-node quotas and public traffic egress should be measured before raising the pilot limit; room moderation and global egress throttling remain future work.

Active public recovery verification (2026-10-22): `public_probe '<primary>;<backup>' 3 --recover` delivered 50/50 packets to each of three independent listeners, then printed `READY_STOP_PRIMARY`. With the primary service stopped under an automatic restoration guard, the existing publisher and all three listener workers moved to the backup node and delivered 50/50 new packets each, retaining the original link and author. The probe checked the serving node in the recovered catalog and removed its temporary room after Stop. The primary was restored, restoration timers were removed, both services were checked, and HAProxy on the primary remained active. The test uses synthetic packets on one Windows host; the two-physical-PC music and latency test remains outstanding.

Local integration tests additionally cover total node loss followed by return of a persistent node identity, current metadata/cover restoration, stale-frame discard, quiet healthy listeners, immediate cancellation during recovery and explicit Stop without reconnection. Full workspace tests, strict Clippy and release builds passed. The desktop artifact remains `artifacts/aura-desktop.exe`.
