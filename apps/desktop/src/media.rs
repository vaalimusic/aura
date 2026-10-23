//! Read-only GSMTC observer on a dedicated MTA thread. It never controls a player.
#[cfg(target_os = "windows")]
mod platform {
    use aura_audio::Source;
    use aura_media::{Info, Playback, Snapshot};
    use std::{
        collections::HashSet,
        future::IntoFuture,
        sync::{
            atomic::{AtomicBool, AtomicU32, Ordering},
            mpsc, Arc,
        },
        time::{Duration, Instant},
    };
    use sysinfo::{Pid, ProcessRefreshKind, RefreshKind, System};
    use windows::{
        Foundation::TypedEventHandler,
        Media::Control::{
            GlobalSystemMediaTransportControlsSession as Session,
            GlobalSystemMediaTransportControlsSessionManager as Manager,
            GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
        },
        Storage::Streams::{DataReader, InputStreamOptions},
        Win32::{
            Foundation::{CloseHandle, HANDLE},
            Storage::Packaging::Appx::GetApplicationUserModelId,
            System::{
                Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
                WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED},
            },
        },
    };
    type Result<T> = aura_media::Result<T>;
    fn wait<F: IntoFuture>(rt: &tokio::runtime::Runtime, operation: F) -> Result<F::Output> {
        Ok(rt.block_on(async {
            tokio::time::timeout(Duration::from_secs(3), operation.into_future()).await
        })?)
    }
    struct Com;
    impl Com {
        fn new() -> Result<Self> {
            // SAFETY: this dedicated thread owns one balanced WinRT MTA initialization.
            unsafe {
                RoInitialize(RO_INIT_MULTITHREADED)?;
            }
            Ok(Self)
        }
    }
    impl Drop for Com {
        fn drop(&mut self) {
            unsafe {
                RoUninitialize();
            }
        }
    }
    struct Process(HANDLE);
    impl Drop for Process {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
    fn app_id(pid: u32) -> Option<String> {
        // SAFETY: query-only handle, bounded UTF-16 buffer, all pointers valid for each call.
        unsafe {
            let process = Process(OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?);
            let mut size = 0;
            let _ = GetApplicationUserModelId(process.0, &mut size, None);
            if size == 0 || size > 4096 {
                return None;
            }
            let mut value = vec![0u16; size as usize];
            if GetApplicationUserModelId(
                process.0,
                &mut size,
                Some(windows::core::PWSTR(value.as_mut_ptr())),
            )
            .0 != 0
            {
                return None;
            }
            Some(String::from_utf16_lossy(
                &value[..value.len().saturating_sub(1)],
            ))
        }
    }
    fn process_matches(source: &Source) -> bool {
        let system = System::new_with_specifics(
            RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()),
        );
        let Some(process) = system.process(Pid::from_u32(source.pid)) else {
            return false;
        };
        if process.start_time() != source.started || process.name().to_string_lossy() != source.name
        {
            return false;
        }
        let mut source_root = process;
        for _ in 0..32 {
            let Some(parent) = source_root
                .parent()
                .and_then(|pid| system.process(pid))
                .filter(|p| p.name().eq_ignore_ascii_case(source_root.name()))
            else {
                break;
            };
            source_root = parent;
        }
        let mut roots = HashSet::new();
        for (_, process) in system
            .processes()
            .iter()
            .filter(|(_, p)| p.name().to_string_lossy() == source.name)
        {
            let mut root = process;
            for _ in 0..32 {
                let Some(parent) = root
                    .parent()
                    .and_then(|pid| system.process(pid))
                    .filter(|p| p.name().eq_ignore_ascii_case(root.name()))
                else {
                    break;
                };
                root = parent;
            }
            roots.insert(root.pid().as_u32());
        }
        roots.len() == 1 && roots.contains(&source_root.pid().as_u32())
    }
    struct Bound {
        session: Session,
        media: i64,
        playback: i64,
        timeline: i64,
    }
    impl Drop for Bound {
        fn drop(&mut self) {
            let _ = self.session.RemoveMediaPropertiesChanged(self.media);
            let _ = self.session.RemovePlaybackInfoChanged(self.playback);
            let _ = self.session.RemoveTimelinePropertiesChanged(self.timeline);
        }
    }
    fn bind(
        session: Session,
        notify: &mpsc::SyncSender<()>,
        flags: &Arc<AtomicU32>,
    ) -> Result<Bound> {
        let (tx, bits) = (notify.clone(), flags.clone());
        let media = session.MediaPropertiesChanged(&TypedEventHandler::new(move |_, _| {
            bits.fetch_or(1, Ordering::Release);
            let _ = tx.try_send(());
            Ok(())
        }))?;
        let (tx, bits) = (notify.clone(), flags.clone());
        let playback = match session.PlaybackInfoChanged(&TypedEventHandler::new(move |_, _| {
            bits.fetch_or(2, Ordering::Release);
            let _ = tx.try_send(());
            Ok(())
        })) {
            Ok(token) => token,
            Err(e) => {
                let _ = session.RemoveMediaPropertiesChanged(media);
                return Err(e.into());
            }
        };
        let (tx, bits) = (notify.clone(), flags.clone());
        let timeline =
            match session.TimelinePropertiesChanged(&TypedEventHandler::new(move |_, _| {
                bits.fetch_or(4, Ordering::Release);
                let _ = tx.try_send(());
                Ok(())
            })) {
                Ok(token) => token,
                Err(e) => {
                    let _ = session.RemoveMediaPropertiesChanged(media);
                    let _ = session.RemovePlaybackInfoChanged(playback);
                    return Err(e.into());
                }
            };
        Ok(Bound {
            session,
            media,
            playback,
            timeline,
        })
    }
    fn properties(rt: &tokio::runtime::Runtime, session: &Session, app: &str) -> Result<Snapshot> {
        let properties = wait(rt, session.TryGetMediaPropertiesAsync()?)??;
        let info = Info {
            title: aura_media::clean(&properties.Title()?.to_string_lossy()),
            artist: aura_media::clean(&properties.Artist()?.to_string_lossy()),
            album: aura_media::clean(&properties.AlbumTitle()?.to_string_lossy()),
            app: aura_media::clean(app),
            playback: Playback::Unknown,
            position_ms: 0,
            duration_ms: 0,
        };
        let artwork = (|| -> Result<_> {
            let reference = properties.Thumbnail()?;
            let stream = wait(rt, reference.OpenReadAsync()?)??;
            let size = stream.Size()?;
            if size == 0 || size > 1024 * 1024 {
                return Err("Thumbnail size limit".into());
            }
            let reader = DataReader::CreateDataReader(&stream)?;
            reader.SetInputStreamOptions(InputStreamOptions::None)?;
            if wait(rt, reader.LoadAsync(size as u32)?)?? != size as u32 {
                return Err("Incomplete thumbnail".into());
            }
            let mut bytes = vec![0; size as usize];
            reader.ReadBytes(&mut bytes)?;
            aura_media::thumbnail(&bytes)
        })()
        .ok();
        Ok(Snapshot {
            info: Some(info),
            artwork,
        })
    }
    fn timeline(session: &Session, snapshot: &mut Snapshot) {
        if let Some(info) = &mut snapshot.info {
            info.playback = session
                .GetPlaybackInfo()
                .and_then(|s| s.PlaybackStatus())
                .map(|state| match state {
                    Status::Playing => Playback::Playing,
                    Status::Paused => Playback::Paused,
                    Status::Stopped | Status::Closed => Playback::Stopped,
                    _ => Playback::Unknown,
                })
                .unwrap_or_default();
            if let Ok(time) = session.GetTimelineProperties() {
                if let (Ok(start), Ok(end), Ok(position)) =
                    (time.StartTime(), time.EndTime(), time.Position())
                {
                    let max = 7 * 24 * 3600 * 1000;
                    info.duration_ms =
                        ((end.Duration.saturating_sub(start.Duration)).max(0) as u64 / 10000)
                            .min(max);
                    info.position_ms =
                        ((position.Duration.saturating_sub(start.Duration)).max(0) as u64 / 10000)
                            .min(max);
                    if info.playback == Playback::Playing {
                        if let (Ok(updated), Ok(now)) = (
                            time.LastUpdatedTime(),
                            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH),
                        ) {
                            let now_ticks =
                                116_444_736_000_000_000i128 + now.as_nanos() as i128 / 100;
                            let elapsed = now_ticks - updated.UniversalTime as i128;
                            if (0..300_000_000).contains(&elapsed) {
                                let rate = session
                                    .GetPlaybackInfo()
                                    .and_then(|p| p.PlaybackRate())
                                    .and_then(|r| r.Value())
                                    .unwrap_or(1.0)
                                    .clamp(0.0, 4.0);
                                info.position_ms = info
                                    .position_ms
                                    .saturating_add((elapsed as f64 / 10000.0 * rate) as u64)
                                    .min(if info.duration_ms > 0 {
                                        info.duration_ms
                                    } else {
                                        max
                                    });
                            }
                        }
                    }
                }
            }
        }
    }
    pub fn observe(
        source: Source,
        stop: Arc<AtomicBool>,
        mut emit: impl FnMut(Snapshot) + Send + 'static,
    ) {
        while !stop.load(Ordering::Acquire) {
            let result = (|| -> Result<()> {
                let _com = Com::new()?;
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()?;
                let manager = wait(&runtime, Manager::RequestAsync()?)??;
                let (tx, rx) = mpsc::sync_channel(1);
                let flags = Arc::new(AtomicU32::new(8));
                let (notify, bits) = (tx.clone(), flags.clone());
                let token = manager.SessionsChanged(&TypedEventHandler::new(move |_, _| {
                    bits.fetch_or(8, Ordering::Release);
                    let _ = notify.try_send(());
                    Ok(())
                }))?;
                struct ManagerGuard(Manager, i64);
                impl Drop for ManagerGuard {
                    fn drop(&mut self) {
                        let _ = self.0.RemoveSessionsChanged(self.1);
                    }
                }
                let _manager_guard = ManagerGuard(manager.clone(), token);
                let mut bound: Option<Bound> = None;
                let mut last_scan = Instant::now() - Duration::from_secs(5);
                let mut last_properties = last_scan;
                let mut last_timeline = last_scan;
                let mut snapshot = Snapshot::default();
                let mut previous = Snapshot::default();
                while !stop.load(Ordering::Acquire) {
                    let _ = rx.recv_timeout(Duration::from_millis(200));
                    let changes = flags.swap(0, Ordering::AcqRel);
                    let mut rebound = false;
                    if changes & 8 != 0 || last_scan.elapsed() >= Duration::from_secs(3) {
                        last_scan = Instant::now();
                        let sessions = manager.GetSessions()?;
                        let sessions: Vec<_> = sessions.into_iter().collect();
                        let ids: Vec<_> = sessions
                            .iter()
                            .map(|s| {
                                s.SourceAppUserModelId()
                                    .map(|v| v.to_string_lossy())
                                    .unwrap_or_default()
                            })
                            .collect();
                        let app = app_id(source.pid);
                        let matched = aura_media::unique_session(
                            &ids,
                            app.as_deref(),
                            &source.name,
                            process_matches(&source),
                        );
                        let selected = matched.map(|i| sessions[i].clone());
                        if selected.as_ref() != bound.as_ref().map(|b| &b.session) {
                            bound = None;
                            snapshot = Snapshot::default();
                            if let Some(session) = selected {
                                bound = Some(bind(session, &tx, &flags)?);
                                rebound = true;
                            }
                        }
                    }
                    if let Some(bound) = &bound {
                        if rebound
                            || changes & 1 != 0
                            || last_properties.elapsed() >= Duration::from_secs(30)
                        {
                            last_properties = Instant::now();
                            snapshot = properties(&runtime, &bound.session, &source.name)
                                .unwrap_or_default();
                        }
                        if rebound
                            || changes != 0
                            || last_timeline.elapsed() >= Duration::from_secs(1)
                        {
                            last_timeline = Instant::now();
                            timeline(&bound.session, &mut snapshot);
                        }
                    }
                    if snapshot != previous {
                        previous = snapshot.clone();
                        emit(snapshot.clone());
                    }
                }
                Ok(())
            })();
            emit(Snapshot::default());
            if result.is_ok() {
                break;
            }
            for _ in 0..20 {
                if stop.load(Ordering::Acquire) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        }
    }
    pub fn probe(source: Source) -> Result<Snapshot> {
        let _com = Com::new()?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()?;
        let manager = wait(&runtime, Manager::RequestAsync()?)??;
        let sessions: Vec<_> = manager.GetSessions()?.into_iter().collect();
        let ids: Vec<_> = sessions
            .iter()
            .map(|s| {
                s.SourceAppUserModelId()
                    .map(|v| v.to_string_lossy())
                    .unwrap_or_default()
            })
            .collect();
        let id = app_id(source.pid);
        let Some(index) =
            aura_media::unique_session(&ids, id.as_deref(), &source.name, process_matches(&source))
        else {
            return Ok(Snapshot::default());
        };
        let mut snapshot = properties(&runtime, &sessions[index], &source.name)?;
        timeline(&sessions[index], &mut snapshot);
        Ok(snapshot)
    }

    /// Read-only diagnostic: return every Windows media session, even when it
    /// cannot be mapped safely to the selected audio process.
    pub fn sessions() -> Result<Vec<String>> {
        let _com = Com::new()?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()?;
        let manager = wait(&runtime, Manager::RequestAsync()?)??;
        let mut output = Vec::new();
        for (index, session) in manager.GetSessions()?.into_iter().enumerate() {
            let app = session
                .SourceAppUserModelId()
                .map(|value| value.to_string_lossy())
                .unwrap_or_else(|_| "<no app id>".to_owned());
            let result = (|| -> Result<Info> {
                let properties = wait(&runtime, session.TryGetMediaPropertiesAsync()?)??;
                let playback = match session.GetPlaybackInfo()?.PlaybackStatus()? {
                    Status::Playing => Playback::Playing,
                    Status::Paused => Playback::Paused,
                    Status::Stopped | Status::Closed => Playback::Stopped,
                    _ => Playback::Unknown,
                };
                Ok(Info {
                    title: aura_media::clean(&properties.Title()?.to_string_lossy()),
                    artist: aura_media::clean(&properties.Artist()?.to_string_lossy()),
                    album: aura_media::clean(&properties.AlbumTitle()?.to_string_lossy()),
                    app: app.clone(),
                    playback,
                    position_ms: 0,
                    duration_ms: 0,
                })
            })();
            match result {
                Ok(info) => output.push(format!("Session {}: {:?}", index + 1, info)),
                Err(error) => output.push(format!(
                    "Session {}: app={:?}; metadata read failed: {}",
                    index + 1,
                    app,
                    error
                )),
            }
        }
        Ok(output)
    }
}
#[cfg(target_os = "windows")]
pub use platform::{observe, probe, sessions};
