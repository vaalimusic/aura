use crate::{
    jitter::{Jitter, Playout},
    *,
};
use ::windows::Win32::Media::Audio::{IAudioCaptureClient, AUDCLNT_BUFFERFLAGS_SILENT};
use std::{
    collections::{BTreeSet, VecDeque},
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        mpsc::Receiver,
    },
    time::{Duration, Instant},
};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};
use wasapi::{DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat};
mod activation;

struct Com;
impl Com {
    fn new() -> Result<Self> {
        wasapi::initialize_mta().ok()?;
        Ok(Self)
    }
}
impl Drop for Com {
    fn drop(&mut self) {
        wasapi::deinitialize();
    }
}

fn system() -> System {
    System::new_with_specifics(RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing()))
}

/// Audio sessions on render endpoints only. Parent grouping never crosses executable names.
pub fn sources() -> Result<Vec<Source>> {
    let _com = Com::new()?;
    let system = system();
    let enumerator = DeviceEnumerator::new()?;
    let devices = enumerator.get_device_collection(&Direction::Render)?;
    let mut pids = BTreeSet::new();
    for i in 0..devices.get_nbr_devices()? {
        let device = devices.get_device_at_index(i)?;
        let Ok(manager) = device.get_iaudiosessionmanager() else {
            continue;
        };
        let sessions = manager.get_audiosessionenumerator()?;
        for n in 0..sessions.get_count()? {
            let session = sessions.get_session(n)?;
            let Ok(pid) = session.get_process_id() else {
                continue;
            };
            if pid == 0 || pid == std::process::id() {
                continue;
            }
            let Some(mut process) = system.process(Pid::from_u32(pid)) else {
                continue;
            };
            for _ in 0..32 {
                let Some(parent) = process.parent().and_then(|id| system.process(id)) else {
                    break;
                };
                if parent.pid().as_u32() == std::process::id() {
                    break;
                }
                if parent.name() != process.name() {
                    break;
                }
                process = parent;
            }
            pids.insert(process.pid());
        }
    }
    let mut result: Vec<_> = pids
        .into_iter()
        .filter_map(|pid| {
            system.process(pid).map(|p| Source {
                pid: pid.as_u32(),
                name: p.name().to_string_lossy().into_owned(),
                started: p.start_time(),
            })
        })
        .collect();
    result.sort_by(|a, b| a.name.cmp(&b.name).then(a.pid.cmp(&b.pid)));
    Ok(result)
}

fn same_process(source: &Source, system: &System) -> bool {
    system.process(Pid::from_u32(source.pid)).is_some_and(|p| {
        p.start_time() == source.started && p.name().to_string_lossy() == source.name
    })
}

/// Runs on its own MTA thread. `emit` must never block on a slow network consumer.
pub fn capture(
    source: &Source,
    stop: &AtomicBool,
    mut ready: impl FnMut(),
    mut emit: impl FnMut(EncodedFrame),
    mut report: impl FnMut(AudioStats),
) -> Result<()> {
    let _com = Com::new()?;
    let mut process_system = system();
    if !same_process(source, &process_system) {
        return Err("Выбранный процесс завершён или изменился. Обновите список.".into());
    }
    let mut client = activation::process_client(source.pid)
        .map_err(|e| format!("Process Loopback недоступен (нужен Windows build 20348+): {e}"))?;
    if stop.load(Ordering::Acquire) {
        return Ok(());
    }
    let format = WaveFormat::new(32, 32, &SampleType::Float, SAMPLE_RATE as usize, 2, None);
    client.initialize_client(
        &format,
        &Direction::Capture,
        &StreamMode::EventsShared {
            autoconvert: true,
            buffer_duration_hns: 200_000,
        },
    )?;
    let event = client.set_get_eventhandle()?;
    // Read natively so SILENT buffers are zero-filled without dereferencing a null pointer.
    // SAFETY: GetService returns a reference-counted interface owned on this MTA thread.
    let reader: IAudioCaptureClient = unsafe { client.as_iaudioclient().GetService()? };
    let mut encoder = Encoder::new()?;
    let mut pending = VecDeque::with_capacity(FRAME_VALUES * 2);
    let mut stats = AudioStats::default();
    let mut last_report = Instant::now();
    let mut previous_frames = 0;
    let mut last_process_check = Instant::now();
    client.start_stream()?;
    ready();
    let result = (|| -> Result<()> {
        while !stop.load(Ordering::Acquire) {
            match event.wait_for_event(20) {
                Ok(()) | Err(wasapi::WasapiError::EventTimeout) => {}
                Err(e) => return Err(e.into()),
            }
            if stop.load(Ordering::Acquire) {
                break;
            }
            loop {
                // SAFETY: valid initialized capture interface, all acquired buffers are released below.
                let count = unsafe { reader.GetNextPacketSize()? };
                if count == 0 {
                    break;
                }
                let mut data = std::ptr::null_mut();
                let mut frames = 0;
                let mut flags = 0;
                unsafe {
                    reader.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                }
                let values = frames as usize * CHANNELS;
                let packet_result = (|| -> Result<()> {
                    if values > FRAME_VALUES * 100 {
                        return Err("Unexpected capture packet size".into());
                    }
                    if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                        pending.extend(std::iter::repeat_n(0.0, values));
                    } else {
                        if data.is_null() && values != 0 {
                            return Err("Invalid WASAPI buffer".into());
                        }
                        if values != 0 {
                            // SAFETY: GetBuffer guarantees frames*blockalign accessible bytes until ReleaseBuffer.
                            let bytes = unsafe { std::slice::from_raw_parts(data, values * 4) };
                            pending.extend(
                                bytes
                                    .as_chunks::<4>()
                                    .0
                                    .iter()
                                    .map(|v| f32::from_le_bytes(*v))
                                    .map(|v| {
                                        if v.is_finite() {
                                            v.clamp(-1.0, 1.0)
                                        } else {
                                            0.0
                                        }
                                    }),
                            );
                        }
                    }
                    Ok(())
                })();
                unsafe {
                    reader.ReleaseBuffer(frames)?;
                }
                packet_result?;
                while pending.len() >= FRAME_VALUES {
                    let pcm: Vec<_> = pending.drain(..FRAME_VALUES).collect();
                    stats.rms = rms(&pcm);
                    let frame = encoder.encode(&pcm, stats.frames)?;
                    stats.frames += 1;
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    emit(frame);
                }
            }
            if last_report.elapsed() >= Duration::from_millis(200) {
                if stats.frames == previous_frames {
                    stats.rms = 0.0;
                }
                previous_frames = stats.frames;
                report(stats);
                last_report = Instant::now();
            }
            if last_process_check.elapsed() >= Duration::from_secs(1) {
                process_system.refresh_processes_specifics(
                    ProcessesToUpdate::Some(&[Pid::from_u32(source.pid)]),
                    true,
                    ProcessRefreshKind::nothing(),
                );
                if !same_process(source, &process_system) {
                    return Err("Источник завершил работу. Эфир остановлен.".into());
                }
                last_process_check = Instant::now();
            }
        }
        Ok(())
    })();
    let _ = client.stop_stream();
    result
}

/// Device clock drives playout; bounded network queue and jitter buffer cap latency.
pub fn playback(
    rx: Receiver<EncodedFrame>,
    stop: &AtomicBool,
    volume: &AtomicU8,
    ready: impl FnMut(),
    report: impl FnMut(AudioStats),
) -> Result<()> {
    let gain = AtomicU8::new(100);
    playback_with_gain(rx, stop, volume, &gain, ready, report)
}

/// Plays one stream with an independent transition gain for Smart Feed crossfades.
pub fn playback_with_gain(
    rx: Receiver<EncodedFrame>,
    stop: &AtomicBool,
    volume: &AtomicU8,
    transition_gain: &AtomicU8,
    mut ready: impl FnMut(),
    mut report: impl FnMut(AudioStats),
) -> Result<()> {
    let _com = Com::new()?;
    let device = DeviceEnumerator::new()?.get_default_device(&Direction::Render)?;
    let mut client = device.get_iaudioclient()?;
    let format = WaveFormat::new(32, 32, &SampleType::Float, SAMPLE_RATE as usize, 2, None);
    client.initialize_client(
        &format,
        &Direction::Render,
        &StreamMode::EventsShared {
            autoconvert: true,
            buffer_duration_hns: 200_000,
        },
    )?;
    let event = client.set_get_eventhandle()?;
    let writer = client.get_audiorenderclient()?;
    let mut decoder = Decoder::new()?;
    let mut jitter = Jitter::default();
    let mut pending = VecDeque::<f32>::new();
    let mut stats = AudioStats::default();
    let mut last_report = Instant::now();
    let mut ready_sent = false;
    client.start_stream()?;
    let result = (|| -> Result<()> {
        while !stop.load(Ordering::Acquire) {
            match event.wait_for_event(20) {
                Ok(()) | Err(wasapi::WasapiError::EventTimeout) => {}
                Err(e) => return Err(e.into()),
            }
            for _ in 0..64 {
                match rx.try_recv() {
                    Ok(frame) => jitter.push(frame),
                    Err(_) => break,
                }
            }
            let available = client.get_available_space_in_frames()? as usize;
            if available > FRAME_SAMPLES * 100 {
                return Err("Unexpected render buffer size".into());
            }
            while pending.len() < available * CHANNELS {
                let pcm = match jitter.pop() {
                    Playout::Wait => vec![0.0; FRAME_VALUES],
                    Playout::Packet(p) => {
                        stats.frames += 1;
                        decoder.decode(Some(&p))?
                    }
                    Playout::Conceal => decoder.decode(None)?,
                };
                stats.rms = rms(&pcm);
                pending.extend(pcm);
            }
            let gain = f32::from(volume.load(Ordering::Relaxed))
                * f32::from(transition_gain.load(Ordering::Relaxed))
                / 10_000.0;
            let bytes: Vec<u8> = pending
                .drain(..available * CHANNELS)
                .flat_map(|v| (v * gain).clamp(-1.0, 1.0).to_le_bytes())
                .collect();
            if stop.load(Ordering::Acquire) {
                break;
            }
            writer.write_to_device(available, &bytes, None)?;
            if !ready_sent && stats.frames > 0 {
                ready();
                ready_sent = true;
            }
            if last_report.elapsed() >= Duration::from_millis(200) {
                stats.dropped = jitter.dropped;
                stats.concealed = jitter.concealed;
                stats.buffer_ms = jitter.buffer_ms();
                report(stats);
                last_report = Instant::now();
            }
        }
        Ok(())
    })();
    let _ = client.stop_stream();
    result
}

/// Quiet synthetic source for an explicit local device smoke test, never microphone input.
pub fn test_tone(duration: Duration) -> Result<()> {
    let _com = Com::new()?;
    let device = DeviceEnumerator::new()?.get_default_device(&Direction::Render)?;
    let mut client = device.get_iaudioclient()?;
    client.initialize_client(
        &WaveFormat::new(32, 32, &SampleType::Float, SAMPLE_RATE as usize, 2, None),
        &Direction::Render,
        &StreamMode::EventsShared {
            autoconvert: true,
            buffer_duration_hns: 200_000,
        },
    )?;
    let event = client.set_get_eventhandle()?;
    let writer = client.get_audiorenderclient()?;
    client.start_stream()?;
    let start = Instant::now();
    let mut frame = 0u64;
    while start.elapsed() < duration {
        match event.wait_for_event(20) {
            Ok(()) | Err(wasapi::WasapiError::EventTimeout) => {}
            Err(e) => return Err(e.into()),
        }
        let available = client.get_available_space_in_frames()? as usize;
        let bytes: Vec<_> = (0..available)
            .flat_map(|_| {
                let value = (frame as f32 * 440.0 * std::f32::consts::TAU / SAMPLE_RATE as f32)
                    .sin()
                    * 0.005;
                frame += 1;
                [value.to_le_bytes(), value.to_le_bytes()].concat()
            })
            .collect();
        writer.write_to_device(available, &bytes, None)?;
    }
    client.stop_stream()?;
    Ok(())
}
