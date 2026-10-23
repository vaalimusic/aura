//! Owns worker lifetime and cancellation. All results are tagged to reject stale sessions.
use crate::UserEvent;
use aura_audio::{AudioStats, Source};
use aura_network::{Invitation as DirectInvitation, Packet, Stats};
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc,
    },
};
use tokio::{
    runtime::Runtime,
    sync::{mpsc, watch},
    task::JoinHandle,
};
use winit::event_loop::EventLoopProxy;

pub enum Event {
    PublicMode(bool),
    Metadata(aura_media::Snapshot),
    CaptureReady(String),
    Authorized,
    PlaybackReady,
    Reconnecting(bool),
    Audio(AudioStats),
    Network(Stats),
    Ended(Option<String>),
}
#[derive(Clone)]
pub struct PublicBroadcast {
    pub title: String,
    pub author: String,
}

#[allow(clippy::too_many_arguments)]
async fn run_mesh(
    id: u64,
    source: Option<Source>,
    invite: Option<aura_mesh::audio::Invitation>,
    public_broadcast: Option<PublicBroadcast>,
    public_listen: Option<aura_mesh::public::Link>,
    identity: aura_mesh::identity::Keypair,
    nodes: aura_mesh::nodes::Nodes,
    stop: Arc<AtomicBool>,
    cancel: watch::Sender<bool>,
    mut stopped: watch::Receiver<bool>,
    volume: Arc<AtomicU8>,
    transition_gain: Arc<AtomicU8>,
    proxy: EventLoopProxy<UserEvent>,
    metadata: watch::Receiver<aura_media::Snapshot>,
) -> aura_audio::Result<()> {
    let event = move |event| {
        let _ = proxy.send_event(UserEvent::Engine(id, event));
    };
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (
            source,
            invite,
            public_broadcast,
            public_listen,
            identity,
            nodes,
            stop,
            cancel,
            stopped,
            volume,
            event,
            metadata,
        );
        Err("Audio currently requires Windows".into())
    }
    #[cfg(target_os = "windows")]
    {
        let (mut audio, mut network) = if let Some(source) = source {
            enum Host {
                Private(Box<aura_mesh::audio::Server>),
                Public(Box<aura_mesh::public::Publisher>),
            }
            let server = if let Some(public) = public_broadcast {
                Host::Public(Box::new(
                    aura_mesh::public::Publisher::connect(
                        nodes,
                        identity,
                        public.title,
                        public.author,
                        stopped.clone(),
                    )
                    .await?,
                ))
            } else {
                Host::Private(Box::new(
                    match aura_mesh::audio::Server::connect_nodes(&nodes, identity, stopped.clone())
                        .await
                    {
                        Ok(server) => server,
                        Err(_) if stop.load(Ordering::Acquire) => return Ok(()),
                        Err(error) => return Err(error),
                    },
                ))
            };
            let link = match &server {
                Host::Private(s) => s.invitation().to_link()?,
                Host::Public(s) => s.link.to_link()?,
            };
            let (tx, rx) = mpsc::channel(8);
            let stop_audio = stop.clone();
            let audio_event = event.clone();
            let audio = tokio::task::spawn_blocking(move || {
                aura_audio::capture(
                    &source,
                    &stop_audio,
                    || audio_event(Event::CaptureReady(link.clone())),
                    |frame| {
                        let packet = Packet {
                            sequence: frame.sequence,
                            payload: frame.opus,
                        };
                        if let Ok(bytes) = packet.encode() {
                            let _ = tx.try_send(bytes);
                        }
                    },
                    |s| audio_event(Event::Audio(s)),
                )
            });
            let net_event = event.clone();
            let net_stop = stopped.clone();
            let network = tokio::spawn(async move {
                let mut recovering = false;
                let report = move |s: aura_mesh::audio::Stats| {
                    if s.recovering != recovering {
                        recovering = s.recovering;
                        net_event(Event::Reconnecting(recovering));
                    }
                    net_event(Event::Network(Stats {
                        listeners: s.listeners,
                        dropped: s.dropped,
                        rtt_ms: s.rtt_ms,
                        ..Stats::default()
                    }))
                };
                match server {
                    Host::Private(server) => {
                        server
                            .run_with_metadata(rx, net_stop, metadata, report)
                            .await
                    }
                    Host::Public(server) => server.run(rx, net_stop, metadata, report).await,
                }
            });
            (audio, network)
        } else {
            let (tx, rx) = std::sync::mpsc::sync_channel(16);
            let stop_audio = stop.clone();
            let audio_event = event.clone();
            let audio = tokio::task::spawn_blocking(move || {
                aura_audio::playback_with_gain(
                    rx,
                    &stop_audio,
                    &volume,
                    &transition_gain,
                    || audio_event(Event::PlaybackReady),
                    |s| audio_event(Event::Audio(s)),
                )
            });
            let net_event = event.clone();
            let net_stop = stopped.clone();
            let network = tokio::spawn(async move {
                let emit = move |bytes: Vec<u8>| {
                    let Ok(packet) = Packet::decode(&bytes) else {
                        return false;
                    };
                    tx.try_send(aura_audio::EncodedFrame {
                        sequence: packet.sequence,
                        opus: packet.payload,
                    })
                    .is_ok()
                };
                if let Some(link) = public_listen {
                    aura_mesh::public::receive_with_nodes(
                        link,
                        identity,
                        nodes,
                        net_stop,
                        emit,
                        || net_event(Event::Authorized),
                        |snapshot| net_event(Event::Metadata(snapshot)),
                        |recovering| net_event(Event::Reconnecting(recovering)),
                    )
                    .await
                } else {
                    aura_mesh::audio::receive_with_nodes_metadata(
                        invite.ok_or("No mesh invitation")?,
                        identity,
                        nodes,
                        net_stop,
                        emit,
                        || net_event(Event::Authorized),
                        |s| {
                            net_event(Event::Network(Stats {
                                listeners: s.listeners,
                                dropped: s.dropped,
                                rtt_ms: s.rtt_ms,
                                ..Stats::default()
                            }))
                        },
                        |snapshot| net_event(Event::Metadata(snapshot)),
                        |recovering| net_event(Event::Reconnecting(recovering)),
                    )
                    .await
                }
            });
            (audio, network)
        };
        enum Finished {
            Audio(std::result::Result<aura_audio::Result<()>, tokio::task::JoinError>),
            Network(std::result::Result<aura_audio::Result<()>, tokio::task::JoinError>),
            Cancelled,
        }
        let finished = tokio::select! {biased;_=stopped.changed()=>Finished::Cancelled,r=&mut audio=>Finished::Audio(r),r=&mut network=>Finished::Network(r)};
        let was_cancelled = stop.swap(true, Ordering::AcqRel);
        let _ = cancel.send(true);
        let result = match finished {
            Finished::Audio(r) => {
                let other = network.await;
                r??;
                other??;
                Ok(())
            }
            Finished::Network(r) => {
                let other = audio.await;
                other??;
                r??;
                Ok(())
            }
            Finished::Cancelled => {
                let _ = network.await;
                let _ = audio.await;
                Ok(())
            }
        };
        if was_cancelled {
            Ok(())
        } else {
            result
        }
    }
}
pub enum Invitation {
    Direct(DirectInvitation),
    Mesh(aura_mesh::audio::Invitation),
    Public(aura_mesh::public::Link),
}
impl Invitation {
    pub fn parse(text: &str) -> aura_audio::Result<Self> {
        if text.trim().starts_with("aura://public/v1/") {
            Ok(Self::Public(aura_mesh::public::Link::parse(text)?))
        } else if text.trim().starts_with("aura://v2/") {
            Ok(Self::Mesh(aura_mesh::audio::Invitation::parse(text)?))
        } else {
            Ok(Self::Direct(DirectInvitation::parse(text)?))
        }
    }
    pub fn address(&self) -> String {
        match self {
            Self::Direct(i) => i.address().to_string(),
            Self::Mesh(_) => "Aura Node".into(),
            Self::Public(_) => "Публичный эфир".into(),
        }
    }
    pub fn uses_relay(&self) -> bool {
        matches!(self, Self::Mesh(_) | Self::Public(_))
    }
}
struct Session {
    broadcasting: bool,
    id: u64,
    stop: Arc<AtomicBool>,
    cancel: watch::Sender<bool>,
    transition_gain: Arc<AtomicU8>,
    task: JoinHandle<()>,
}
pub struct Engine {
    identity: aura_mesh::identity::Keypair,
    pub public_broadcast: Option<PublicBroadcast>,
    _profile_lock: std::fs::File,
    profile_dir: std::path::PathBuf,
    nodes: aura_mesh::nodes::Nodes,
    pub peer_id: String,
    social: mpsc::Sender<aura_mesh::social::Command>,
    runtime: Runtime,
    session: Option<Session>,
    transition_old: Option<Session>,
    pub generation: u64,
    volume: Arc<AtomicU8>,
}
impl Engine {
    pub fn new(
        proxy: EventLoopProxy<UserEvent>,
        enabled: bool,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let profile = std::env::var_os("AURA_PROFILE_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("LOCALAPPDATA")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| std::path::PathBuf::from(".aura"))
                    .join("Aura")
            });
        std::fs::create_dir_all(&profile)?;
        let profile_lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(profile.join("profile.lock"))?;
        profile_lock
            .try_lock()
            .map_err(|_| "Этот профиль Aura уже открыт в другом процессе.")?;
        let identity =
            aura_mesh::key::load(&profile.join("identity.key")).map_err(|e| e.to_string())?;
        let peer_id = identity.public().to_peer_id().to_string();
        let store = aura_social::Store::open(&profile.join("friends.json"), &peer_id)
            .map_err(|e| e.to_string())?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let (social, commands) = mpsc::channel(64);
        if !enabled {
            let _ = social.try_send(aura_mesh::social::Command::Enable(false));
        }
        let nodes =
            aura_mesh::nodes::Nodes::from_profile(&profile).map_err(|error| error.to_string())?;
        let social_nodes = nodes.clone();
        let public_identity = identity.clone();
        runtime.spawn(async move {
            let errors = proxy.clone();
            if let Err(error) = aura_mesh::social::run_with_nodes(
                identity,
                store,
                social_nodes,
                commands,
                move |event| {
                    let _ = proxy.send_event(UserEvent::Social(event));
                },
            )
            .await
            {
                let _ = errors.send_event(UserEvent::Social(aura_mesh::social::Event::Notice(
                    error.to_string(),
                )));
            }
        });
        Ok(Self {
            identity: public_identity,
            public_broadcast: None,
            _profile_lock: profile_lock,
            profile_dir: profile,
            nodes,
            social,
            peer_id,
            runtime,
            session: None,
            transition_old: None,
            generation: 0,
            volume: Arc::new(AtomicU8::new(65)),
        })
    }
    pub fn social(&self, command: aura_mesh::social::Command) {
        let _ = self.social.try_send(command);
    }
    pub fn public_favorites(&self) -> Vec<(String, String)> {
        crate::public_preferences::load(&self.profile_dir.join("public-favorites.json"))
    }
    pub fn save_public_favorites(&self, values: &[(String, String)]) -> Result<(), String> {
        crate::public_preferences::save(&self.profile_dir.join("public-favorites.json"), values)
    }
    pub fn refresh_public(&self, proxy: EventLoopProxy<UserEvent>) {
        let nodes = self.nodes.clone();
        let key = self.identity.clone();
        self.runtime.spawn(async move {
            let (_stop, stopped) = watch::channel(false);
            let result = aura_mesh::public::catalog(nodes, key, stopped)
                .await
                .map(|(entries, count)| {
                    let rooms = entries
                        .into_iter()
                        .filter_map(|entry| {
                            Some(aura_core::live::PublicRoom {
                                link: entry.link.to_link().ok()?,
                                owner: entry.info.owner,
                                title: entry.info.title,
                                author: entry.info.author,
                                listeners: entry.info.listeners,
                                track: entry.info.track,
                                artist: entry.info.artist,
                            })
                        })
                        .collect();
                    (rooms, count)
                })
                .map_err(|error| error.to_string());
            let _ = proxy.send_event(UserEvent::PublicCatalog(result));
        });
    }
    pub fn volume(&self, value: u8) {
        self.volume.store(value.min(100), Ordering::Relaxed);
    }
    pub fn smart_feed_preferences(&self) -> crate::feed_preferences::Preferences {
        crate::feed_preferences::load(&self.profile_dir.join("smart-feed.txt"))
    }
    pub fn save_smart_feed_preferences(
        &self,
        pinned: &[String],
        excluded: &[String],
        order: &[String],
    ) -> Result<(), String> {
        crate::feed_preferences::save(
            &self.profile_dir.join("smart-feed.txt"),
            pinned,
            excluded,
            order,
        )
    }
    pub fn scan(&self, proxy: EventLoopProxy<UserEvent>) {
        self.runtime.spawn_blocking(move || {
            let result = aura_audio::sources()
                .map(|sources| {
                    sources
                        .into_iter()
                        .map(|s| aura_core::live::Source {
                            pid: s.pid,
                            name: s.name,
                            started: s.started,
                        })
                        .collect()
                })
                .map_err(|e| e.to_string());
            let _ = proxy.send_event(UserEvent::Sources(result));
        });
    }
    pub fn stop(&mut self) {
        self.social(aura_mesh::social::Command::Publish(None));
        for session in [&self.session, &self.transition_old].into_iter().flatten() {
            session.stop.store(true, Ordering::Release);
            let _ = session.cancel.send(true);
        }
    }
    pub fn is_broadcast_session(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.broadcasting && !session.task.is_finished())
    }
    pub fn start(
        &mut self,
        source: Option<Source>,
        invite: Option<Invitation>,
        use_relay: bool,
        proxy: EventLoopProxy<UserEvent>,
    ) {
        self.start_internal(source, invite, use_relay, proxy, false);
    }
    pub fn start_crossfade(
        &mut self,
        invite: Invitation,
        use_relay: bool,
        proxy: EventLoopProxy<UserEvent>,
    ) {
        self.start_internal(None, Some(invite), use_relay, proxy, true);
    }
    fn start_internal(
        &mut self,
        source: Option<Source>,
        invite: Option<Invitation>,
        use_relay: bool,
        proxy: EventLoopProxy<UserEvent>,
        crossfade: bool,
    ) {
        let broadcasting = source.is_some();
        let keep_current = crossfade && self.transition_old.is_none() && self.session.is_some();
        if !keep_current {
            self.stop();
        }
        let mut previous = if keep_current {
            self.session.take().into_iter().collect::<Vec<_>>()
        } else {
            self.session
                .take()
                .into_iter()
                .chain(self.transition_old.take())
                .collect::<Vec<_>>()
        };
        let old_session = if crossfade && previous.len() == 1 {
            Some(previous.remove(0))
        } else {
            None
        };
        self.generation += 1;
        let id = self.generation;
        let stop = Arc::new(AtomicBool::new(false));
        let transition_gain = Arc::new(AtomicU8::new(if old_session.is_some() { 0 } else { 100 }));
        let (cancel, receiver) = watch::channel(false);
        let stop_job = stop.clone();
        let cancel_job = cancel.clone();
        let volume = self.volume.clone();
        let playback_gain = transition_gain.clone();
        let public_broadcast = source.as_ref().and(self.public_broadcast.clone());
        let identity = if public_broadcast.is_some() {
            self.identity.clone()
        } else {
            aura_mesh::identity::Keypair::generate_ed25519()
        };
        let nodes = self.nodes.clone();
        let task = self.runtime.spawn(async move {
            for previous in previous {
                let _ = previous.task.await;
            }
            if stop_job.load(Ordering::Acquire) {
                let _ = proxy.send_event(UserEvent::Engine(id, Event::Ended(None)));
                return;
            }
            let (metadata_tx, metadata_rx) = watch::channel(aura_media::Snapshot::default());
            #[cfg(target_os = "windows")]
            if let Some(source) = source.clone() {
                let observer_stop = stop_job.clone();
                let observer_proxy = proxy.clone();
                let _ = std::thread::Builder::new()
                    .name("aura-media".into())
                    .spawn(move || {
                        crate::media::observe(source, observer_stop, move |snapshot| {
                            metadata_tx.send_replace(snapshot.clone());
                            let _ = observer_proxy
                                .send_event(UserEvent::Engine(id, Event::Metadata(snapshot)));
                        })
                    });
            }
            #[cfg(not(target_os = "windows"))]
            drop(metadata_tx);
            let result = run(
                id,
                source,
                invite,
                public_broadcast,
                use_relay,
                identity,
                nodes,
                stop_job.clone(),
                cancel_job.clone(),
                receiver,
                volume,
                playback_gain,
                proxy.clone(),
                metadata_rx,
            )
            .await;
            let error = result.err().map(|e| e.to_string());
            stop_job.store(true, Ordering::Release);
            let _ = cancel_job.send(true);
            let _ = proxy.send_event(UserEvent::Engine(id, Event::Ended(error)));
        });
        self.session = Some(Session {
            broadcasting,
            id,
            stop,
            cancel,
            transition_gain,
            task,
        });
        self.transition_old = old_session;
    }
    pub fn complete_transition(&mut self) {
        let Some(old) = self.transition_old.take() else {
            return;
        };
        let Some(current) = self.session.as_ref() else {
            self.transition_old = Some(old);
            return;
        };
        let old_gain = old.transition_gain.clone();
        let new_gain = current.transition_gain.clone();
        let old_stop = old.stop.clone();
        let old_cancel = old.cancel.clone();
        self.runtime.spawn(async move {
            for step in 1..=10 {
                let angle = f64::from(step) * std::f64::consts::FRAC_PI_2 / 10.0;
                let new_level = (angle.sin() * 100.0).round() as u8;
                let old_level = (angle.cos() * 100.0).round() as u8;
                old_gain.store(old_level, Ordering::Release);
                new_gain.store(new_level, Ordering::Release);
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            old_stop.store(true, Ordering::Release);
            let _ = old_cancel.send(true);
            let _ = old.task.await;
        });
    }
    pub fn cancel_transition(&mut self) -> bool {
        let Some(old) = self.transition_old.take() else {
            return false;
        };
        if old.task.is_finished() {
            self.runtime.spawn(old.task);
            return false;
        }
        if let Some(current) = self.session.take() {
            current.stop.store(true, Ordering::Release);
            let _ = current.cancel.send(true);
            self.runtime.spawn(current.task);
        }
        old.transition_gain.store(100, Ordering::Release);
        self.generation = old.id;
        self.session = Some(old);
        true
    }
    pub fn invalidate(&mut self) {
        self.stop();
        self.generation += 1;
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        self.stop();
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    id: u64,
    source: Option<Source>,
    invite: Option<Invitation>,
    public_broadcast: Option<PublicBroadcast>,
    use_relay: bool,
    identity: aura_mesh::identity::Keypair,
    nodes: aura_mesh::nodes::Nodes,
    stop: Arc<AtomicBool>,
    cancel: watch::Sender<bool>,
    mut stopped: watch::Receiver<bool>,
    volume: Arc<AtomicU8>,
    transition_gain: Arc<AtomicU8>,
    proxy: EventLoopProxy<UserEvent>,
    metadata: watch::Receiver<aura_media::Snapshot>,
) -> aura_audio::Result<()> {
    let _ = proxy.send_event(UserEvent::Engine(
        id,
        Event::PublicMode(
            public_broadcast.is_some() || matches!(invite, Some(Invitation::Public(_))),
        ),
    ));
    if public_broadcast.is_some()
        || (source.is_some() && use_relay)
        || matches!(invite, Some(Invitation::Mesh(_) | Invitation::Public(_)))
    {
        let (invite, public_listen) = match invite {
            Some(Invitation::Mesh(i)) => (Some(i), None),
            Some(Invitation::Public(i)) => (None, Some(i)),
            _ => (None, None),
        };
        return run_mesh(
            id,
            source,
            invite,
            public_broadcast,
            public_listen,
            identity,
            nodes,
            stop,
            cancel,
            stopped,
            volume,
            transition_gain,
            proxy,
            metadata,
        )
        .await;
    }
    let event = move |e| {
        let _ = proxy.send_event(UserEvent::Engine(id, e));
    };
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (
            source,
            invite,
            stop,
            cancel,
            stopped,
            volume,
            transition_gain,
            event,
            metadata,
        );
        Err("Real audio currently requires Windows".into())
    }
    #[cfg(target_os = "windows")]
    {
        let (mut audio, mut network): (
            JoinHandle<aura_audio::Result<()>>,
            JoinHandle<aura_network::Result<()>>,
        ) = if let Some(source) = source {
            let bind: SocketAddr = std::env::var("AURA_BIND")
                .unwrap_or_else(|_| "0.0.0.0:45070".into())
                .parse()?;
            let advertised = std::env::var("AURA_ADVERTISE")
                .ok()
                .map(|s| s.parse())
                .transpose()?;
            let server = aura_network::Server::bind(bind, advertised)?;
            let link = server.invitation().to_link()?;
            let (tx, rx) = mpsc::channel(8);
            let stop_audio = stop.clone();
            let audio_event = event.clone();
            let drops = Arc::new(std::sync::atomic::AtomicU64::new(0));
            let frame_drops = drops.clone();
            let audio = tokio::task::spawn_blocking(move || {
                aura_audio::capture(
                    &source,
                    &stop_audio,
                    || audio_event(Event::CaptureReady(link.clone())),
                    |frame| {
                        if tx
                            .try_send(Packet {
                                sequence: frame.sequence,
                                payload: frame.opus,
                            })
                            .is_err()
                        {
                            frame_drops.fetch_add(1, Ordering::Relaxed);
                        }
                    },
                    |mut stats| {
                        stats.dropped = drops.load(Ordering::Relaxed);
                        audio_event(Event::Audio(stats));
                    },
                )
            });
            let network_event = event.clone();
            let receiver = stopped.clone();
            let network = tokio::spawn(async move {
                server
                    .run_with_metadata(rx, receiver, metadata, move |s| {
                        network_event(Event::Network(s))
                    })
                    .await
            });
            (audio, network)
        } else {
            let invitation = match invite {
                Some(Invitation::Direct(i)) => i,
                _ => return Err("No direct invitation".into()),
            };
            let (tx, rx) = std::sync::mpsc::sync_channel(16);
            let stop_audio = stop.clone();
            let audio_event = event.clone();
            let audio = tokio::task::spawn_blocking(move || {
                aura_audio::playback_with_gain(
                    rx,
                    &stop_audio,
                    &volume,
                    &transition_gain,
                    || audio_event(Event::PlaybackReady),
                    |s| audio_event(Event::Audio(s)),
                )
            });
            let network_event = event.clone();
            let receiver = stopped.clone();
            let network = tokio::spawn(async move {
                aura_network::receive_with_metadata(
                    invitation,
                    move |p| {
                        tx.try_send(aura_audio::EncodedFrame {
                            sequence: p.sequence,
                            opus: p.payload,
                        })
                        .is_ok()
                    },
                    receiver,
                    || network_event(Event::Authorized),
                    |s| network_event(Event::Network(s)),
                    |snapshot| network_event(Event::Metadata(snapshot)),
                )
                .await
            });
            (audio, network)
        };
        enum Finished {
            Audio(std::result::Result<aura_audio::Result<()>, tokio::task::JoinError>),
            Network(std::result::Result<aura_network::Result<()>, tokio::task::JoinError>),
            Cancelled,
        }
        let finished = tokio::select! {biased;_=stopped.changed()=>Finished::Cancelled,result=&mut audio=>Finished::Audio(result),result=&mut network=>Finished::Network(result)};
        // Close the network immediately even if OS capture is still finishing activation.
        let was_cancelled = stop.swap(true, Ordering::AcqRel);
        let _ = cancel.send(true);
        let result = match finished {
            Finished::Audio(result) => {
                let other = network.await;
                result??;
                other??;
                Ok(())
            }
            Finished::Network(result) => {
                let other = audio.await;
                other??;
                result??;
                Ok(())
            }
            Finished::Cancelled => {
                let _ = network.await;
                let _ = audio.await;
                Ok(())
            }
        };
        if was_cancelled {
            Ok(())
        } else {
            result
        }
    }
}
