//! Ephemeral, invitation-authorized direct QUIC audio transport.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use quinn::{Connection, Endpoint};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use subtle::ConstantTimeEq;
use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
    time::{timeout, Instant},
};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const MAGIC: &[u8; 4] = b"AUR1";
const MAX_OPUS: usize = 1100;
const MAX_LISTENERS: usize = 4;

#[derive(Clone, Debug)]
pub struct Packet {
    pub sequence: u64,
    pub payload: Vec<u8>,
}
impl Packet {
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.payload.is_empty() || self.payload.len() > MAX_OPUS {
            return Err("Invalid audio packet size".into());
        }
        let mut bytes = Vec::with_capacity(12 + self.payload.len());
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&self.sequence.to_be_bytes());
        bytes.extend_from_slice(&self.payload);
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if !(13..=12 + MAX_OPUS).contains(&bytes.len()) || &bytes[..4] != MAGIC {
            return Err("Invalid audio packet".into());
        }
        Ok(Self {
            sequence: u64::from_be_bytes(bytes[4..12].try_into()?),
            payload: bytes[12..].to_vec(),
        })
    }
}

/// Secret bearer capability plus pinned ephemeral certificate. Never log this value.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invitation {
    address: SocketAddr,
    certificate: String,
    token: String,
}
impl Invitation {
    pub fn parse(text: &str) -> Result<Self> {
        if text.len() > 4096 {
            return Err("Приглашение слишком длинное".into());
        }
        let text = text
            .trim()
            .strip_prefix("aura://v1/")
            .ok_or("Ожидается приглашение aura://v1/…")?;
        let invite: Self = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(text)?)?;
        if invite.address.port() == 0
            || invite.address.ip().is_unspecified()
            || invite.address.ip().is_multicast()
        {
            return Err("Invalid peer address".into());
        }
        if URL_SAFE_NO_PAD.decode(&invite.token)?.len() != 32 {
            return Err("Invalid invitation capability".into());
        }
        let cert = URL_SAFE_NO_PAD.decode(&invite.certificate)?;
        if cert.is_empty() || cert.len() > 2048 {
            return Err("Invalid invitation certificate".into());
        }
        Ok(invite)
    }
    pub fn to_link(&self) -> Result<String> {
        Ok(format!(
            "aura://v1/{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(self)?)
        ))
    }
    pub fn address(&self) -> SocketAddr {
        self.address
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub listeners: usize,
    pub sent: u64,
    pub received: u64,
    pub dropped: u64,
    pub rtt_ms: u64,
}

pub struct Server {
    endpoint: Endpoint,
    invitation: Invitation,
    token: [u8; 32],
}
impl Server {
    /// Must run inside a Tokio runtime. A fresh certificate and capability are issued each time.
    pub fn bind(bind: SocketAddr, advertise: Option<SocketAddr>) -> Result<Self> {
        let cert = rcgen::generate_simple_self_signed(vec!["aura.local".into()])?;
        let certificate = cert.cert.der().clone();
        let key = PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());
        let mut config =
            quinn::ServerConfig::with_single_cert(vec![certificate.clone()], key.into())?;
        let transport =
            Arc::get_mut(&mut config.transport).ok_or("Shared transport configuration")?;
        transport.max_concurrent_uni_streams(0u8.into());
        transport.max_concurrent_bidi_streams(1u8.into());
        transport.stream_receive_window(128u32.into());
        transport.receive_window(4096u32.into());
        transport.datagram_receive_buffer_size(Some(8192));
        transport.datagram_send_buffer_size(8192);
        transport.max_idle_timeout(Some(Duration::from_secs(10).try_into()?));
        transport.keep_alive_interval(Some(Duration::from_secs(2)));
        let endpoint = Endpoint::server(config, bind)?;
        let local = endpoint.local_addr()?;
        let address = match advertise {
            Some(a) => a,
            None if !local.ip().is_unspecified() => local,
            None => {
                // UDP connect selects a route; it sends no data.
                let route = std::net::UdpSocket::bind("0.0.0.0:0")?;
                route.connect("192.0.2.1:9")?;
                SocketAddr::new(route.local_addr()?.ip(), local.port())
            }
        };
        if address.port() == 0 || address.ip().is_unspecified() {
            return Err("Specify an advertised peer address".into());
        }
        let mut token = [0; 32];
        getrandom::fill(&mut token).map_err(|e| e.to_string())?;
        let invitation = Invitation {
            address,
            certificate: URL_SAFE_NO_PAD.encode(certificate),
            token: URL_SAFE_NO_PAD.encode(token),
        };
        Ok(Self {
            endpoint,
            invitation,
            token,
        })
    }
    pub fn invitation(&self) -> &Invitation {
        &self.invitation
    }
    pub async fn run(
        self,
        audio: mpsc::Receiver<Packet>,
        stop: watch::Receiver<bool>,
        report: impl FnMut(Stats),
    ) -> Result<()> {
        let (_sender, metadata) = watch::channel(aura_media::Snapshot::default());
        self.run_with_metadata(audio, stop, metadata, report).await
    }
    pub async fn run_with_metadata(
        self,
        mut audio: mpsc::Receiver<Packet>,
        mut stop: watch::Receiver<bool>,
        metadata: watch::Receiver<aura_media::Snapshot>,
        mut report: impl FnMut(Stats),
    ) -> Result<()> {
        if *stop.borrow() {
            self.endpoint.close(0u8.into(), b"Stopped");
            return Ok(());
        }
        let mut pending = JoinSet::new();
        let mut metadata_tasks = JoinSet::new();
        let mut peers: Vec<Connection> = vec![];
        let mut stats = Stats::default();
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut window = Instant::now();
        let mut attempts = 0u32;
        loop {
            tokio::select! { biased;
                _=stop.changed()=>break,
                Some(_)=metadata_tasks.join_next(),if !metadata_tasks.is_empty()=>{},
                incoming=self.endpoint.accept()=>{
                    let Some(incoming)=incoming else {break};
                    if window.elapsed()>Duration::from_secs(60) {window=Instant::now();attempts=0;}
                    attempts+=1;
                    if attempts>30 || peers.len()+pending.len()>=MAX_LISTENERS {incoming.refuse();continue;}
                    let token=self.token;
                    pending.spawn(async move {
                        timeout(Duration::from_secs(5),async move {
                            let connection=incoming.await?;
                            let (mut send,mut recv)=connection.accept_bi().await?;
                            let mut auth=[0u8;36];recv.read_exact(&mut auth).await?;
                            if &auth[..4]!=MAGIC || token.ct_eq(&auth[4..]).unwrap_u8()!=1 {
                                connection.close(1u8.into(),b"Access denied");return Err("Access denied".into());
                            }
                            send.write_all(b"OK01").await?;
                            Ok::<_,Box<dyn std::error::Error+Send+Sync>>((connection,send))
                        }).await?
                    });
                },
                Some(result)=pending.join_next(),if !pending.is_empty()=>{
                    if let Ok(Ok((peer,mut stream)))=result {
                        let metadata_peer=peer.clone();peers.push(peer);let mut updates=metadata.clone();
                        metadata_tasks.spawn(async move{let mut previous=None;
                            loop{let snapshot=updates.borrow_and_update().clone();let bytes=aura_media::encode(&snapshot,&mut previous)?;
                                timeout(Duration::from_secs(3),async{stream.write_all(&(bytes.len() as u32).to_be_bytes()).await?;stream.write_all(&bytes).await}).await??;
                                tokio::select!{result=updates.changed()=>if result.is_err(){break;},_=metadata_peer.closed()=>break}
                            }Ok::<_,Box<dyn std::error::Error+Send+Sync>>(())
                        });
                    }
                    stats.listeners=peers.len();report(stats);
                },
                packet=audio.recv()=>{
                    let Some(packet)=packet else {break};
                    let bytes=packet.encode()?;
                    for peer in &peers { if peer.send_datagram(bytes.clone().into()).is_ok() {stats.sent+=1;} else {stats.dropped+=1;} }
                },
                _=tick.tick()=>{
                    peers.retain(|p|p.close_reason().is_none());stats.listeners=peers.len();report(stats);
                }
            }
        }
        // Close every authorized and pending connection, revoking this invitation.
        self.endpoint.close(0u8.into(), b"Broadcast ended");
        pending.abort_all();
        metadata_tasks.abort_all();
        while pending.join_next().await.is_some() {}
        let _ = timeout(Duration::from_secs(1), self.endpoint.wait_idle()).await;
        Ok(())
    }
}

pub async fn receive(
    invitation: Invitation,
    emit: impl FnMut(Packet) -> bool,
    stop: watch::Receiver<bool>,
    ready: impl FnMut(),
    report: impl FnMut(Stats),
) -> Result<()> {
    receive_with_metadata(invitation, emit, stop, ready, report, |_| {}).await
}
pub async fn receive_with_metadata(
    invitation: Invitation,
    mut emit: impl FnMut(Packet) -> bool,
    mut stop: watch::Receiver<bool>,
    mut ready: impl FnMut(),
    mut report: impl FnMut(Stats),
    mut metadata: impl FnMut(aura_media::Snapshot),
) -> Result<()> {
    if *stop.borrow() {
        return Ok(());
    }
    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from(
        URL_SAFE_NO_PAD.decode(&invitation.certificate)?,
    ))?;
    let mut config = quinn::ClientConfig::with_root_certificates(Arc::new(roots))?;
    let mut transport = quinn::TransportConfig::default();
    transport.max_concurrent_bidi_streams(0u8.into());
    transport.max_concurrent_uni_streams(0u8.into());
    transport.datagram_receive_buffer_size(Some(32 * 1200));
    transport.max_idle_timeout(Some(Duration::from_secs(10).try_into()?));
    transport.keep_alive_interval(Some(Duration::from_secs(2)));
    config.transport_config(Arc::new(transport));
    let bind = SocketAddr::new(
        if invitation.address.is_ipv4() {
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        } else {
            IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED)
        },
        0,
    );
    let mut endpoint = Endpoint::client(bind)?;
    endpoint.set_default_client_config(config);
    let session = async {
        let connection = timeout(
            Duration::from_secs(8),
            endpoint.connect(invitation.address, "aura.local")?,
        )
        .await??;
        let (mut send, mut recv) = timeout(Duration::from_secs(3), connection.open_bi()).await??;
        let mut auth = Vec::from(&MAGIC[..]);
        auth.extend_from_slice(&URL_SAFE_NO_PAD.decode(&invitation.token)?);
        send.write_all(&auth).await?;
        send.finish()?;
        let mut response = [0; 4];
        timeout(Duration::from_secs(3), recv.read_exact(&mut response)).await??;
        if &response != b"OK01" {
            return Err::<(), Box<dyn std::error::Error + Send + Sync>>(
                "Broadcast access denied".into(),
            );
        }
        ready();
        let metadata_read = async {
            let result: Result<()> = async {
                let mut previous = None;
                let mut window = Instant::now();
                let mut count = 0;
                loop {
                    let mut size = [0; 4];
                    recv.read_exact(&mut size).await?;
                    let size = u32::from_be_bytes(size) as usize;
                    if size == 0 || size > aura_media::MAX_MESSAGE {
                        return Err("Invalid metadata size".into());
                    }
                    let mut bytes = vec![0; size];
                    timeout(Duration::from_secs(5), recv.read_exact(&mut bytes)).await??;
                    if window.elapsed() >= Duration::from_secs(1) {
                        window = Instant::now();
                        count = 0;
                    }
                    count += 1;
                    if count > 10 {
                        return Err("Metadata rate limit".into());
                    }
                    metadata(aura_media::decode(&bytes, &mut previous)?);
                }
            }
            .await;
            metadata(aura_media::Snapshot::default());
            result
        };
        tokio::pin!(metadata_read);
        let mut metadata_done = false;
        let mut stats = Stats::default();
        let mut last_report = Instant::now();
        loop {
            let bytes = tokio::select! {result=connection.read_datagram()=>result?,_=&mut metadata_read,if !metadata_done=>{metadata_done=true;continue;}};
            let packet = Packet::decode(&bytes)?;
            if emit(packet) {
                stats.received += 1;
            } else {
                stats.dropped += 1;
            }
            if last_report.elapsed() >= Duration::from_millis(250) {
                stats.rtt_ms = connection.rtt().as_millis() as u64;
                report(stats);
                last_report = Instant::now();
            }
        }
    };
    let result = tokio::select! {biased;_=stop.changed()=>Ok(()),result=session=>result};
    endpoint.close(0u8.into(), b"Listener stopped");
    let _ = timeout(Duration::from_secs(1), endpoint.wait_idle()).await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn metadata_is_authorized_independent_of_opus_and_updates() {
        use aura_media::{Info, Playback, Snapshot};
        let server = Server::bind("127.0.0.1:0".parse().unwrap(), None).unwrap();
        let invite = server.invitation().clone();
        let (_frames, rx) = mpsc::channel(8);
        let (stop, stopped) = watch::channel(false);
        let (tx, metadata) = watch::channel(Snapshot::default());
        let host = tokio::spawn(server.run_with_metadata(rx, stopped.clone(), metadata, |_| {}));
        let (events, mut events_rx) = mpsc::channel(8);
        let client = tokio::spawn(receive_with_metadata(
            invite,
            |_| true,
            stopped,
            || {},
            |_| {},
            move |snapshot| {
                let _ = events.try_send(snapshot);
            },
        ));
        assert_eq!(
            timeout(Duration::from_secs(5), events_rx.recv())
                .await
                .unwrap()
                .unwrap(),
            Snapshot::default()
        );
        let snapshot = Snapshot {
            info: Some(Info {
                title: "Трек".into(),
                artist: "Друг".into(),
                album: String::new(),
                app: "Плеер".into(),
                playback: Playback::Playing,
                position_ms: 1000,
                duration_ms: 60000,
            }),
            artwork: None,
        };
        tx.send(snapshot.clone()).unwrap();
        assert_eq!(
            timeout(Duration::from_secs(5), events_rx.recv())
                .await
                .unwrap()
                .unwrap(),
            snapshot
        );
        tx.send(Snapshot::default()).unwrap();
        assert_eq!(
            timeout(Duration::from_secs(5), events_rx.recv())
                .await
                .unwrap()
                .unwrap(),
            Snapshot::default()
        );
        stop.send(true).unwrap();
        host.await.unwrap().unwrap();
        client.await.unwrap().unwrap();
    }
    #[test]
    fn malformed_packets_and_invites_are_rejected() {
        for n in 0..13 {
            assert!(Packet::decode(&vec![0; n]).is_err());
        }
        assert!(Packet::decode(&vec![0; 2000]).is_err());
        let p = Packet {
            sequence: u64::MAX,
            payload: vec![1, 2, 3],
        };
        let q = Packet::decode(&p.encode().unwrap()).unwrap();
        assert_eq!(q.sequence, p.sequence);
        assert_eq!(q.payload, p.payload);
        for invite in ["", "https://example.com", "aura://v1/AA", &"x".repeat(5000)] {
            assert!(Invitation::parse(invite).is_err());
        }
    }
    #[tokio::test]
    async fn wrong_capability_and_wrong_certificate_never_receive_audio() {
        let server = Server::bind("127.0.0.1:0".parse().unwrap(), None).unwrap();
        let invite = server.invitation().clone();
        let (_tx, rx) = mpsc::channel(8);
        let (stop, stopped) = watch::channel(false);
        let task = tokio::spawn(server.run(rx, stopped, |_| {}));
        for wrong_certificate in [false, true] {
            let mut forged = invite.clone();
            if wrong_certificate {
                let other = rcgen::generate_simple_self_signed(vec!["aura.local".into()]).unwrap();
                forged.certificate = URL_SAFE_NO_PAD.encode(other.cert.der());
            } else {
                forged.token = URL_SAFE_NO_PAD.encode([0u8; 32]);
            }
            let (_cancel, stopped) = watch::channel(false);
            let result = timeout(
                Duration::from_secs(5),
                receive(
                    forged,
                    |_| panic!("Unauthorized media"),
                    stopped,
                    || panic!("Unauthorized access"),
                    |_| {},
                ),
            )
            .await
            .unwrap();
            assert!(result.is_err());
        }
        stop.send(true).unwrap();
        task.await.unwrap().unwrap();
    }
    #[tokio::test]
    async fn encrypted_opus_round_trip_and_capability_revocation() {
        let server = Server::bind("127.0.0.1:0".parse().unwrap(), None).unwrap();
        let invite = Invitation::parse(&server.invitation().to_link().unwrap()).unwrap();
        let (tx, rx) = mpsc::channel(8);
        let (stop, stopped) = watch::channel(false);
        let task = tokio::spawn(server.run(rx, stopped, |_| {}));
        let (out, received) = std::sync::mpsc::sync_channel(16);
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let mut ready_tx = Some(ready_tx);
        let (listener_cancel, listener_stop2) = watch::channel(false);
        let listener = tokio::spawn(receive(
            invite.clone(),
            move |p| out.try_send(p).is_ok(),
            listener_stop2,
            move || {
                if let Some(tx) = ready_tx.take() {
                    let _ = tx.send(());
                }
            },
            |_| {},
        ));
        timeout(Duration::from_secs(5), ready_rx)
            .await
            .unwrap()
            .unwrap();
        let mut encoder = aura_audio::Encoder::new().unwrap();
        let mut decoder = aura_audio::Decoder::new().unwrap();
        for sequence in 0..5 {
            let pcm: Vec<_> = (0..aura_audio::FRAME_VALUES)
                .map(|i| ((i / 2) as f32 * 0.06).sin() * 0.1)
                .collect();
            let frame = encoder.encode(&pcm, sequence).unwrap();
            tx.send(Packet {
                sequence,
                payload: frame.opus,
            })
            .await
            .unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        let packets: Vec<_> = received.try_iter().collect();
        assert!(!packets.is_empty());
        assert!(packets
            .into_iter()
            .any(|p| aura_audio::rms(&decoder.decode(Some(&p.payload)).unwrap()) > 0.01));
        stop.send(true).unwrap();
        timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(timeout(Duration::from_secs(3), listener)
            .await
            .unwrap()
            .unwrap()
            .is_err());
        drop(listener_cancel);
    }
}
