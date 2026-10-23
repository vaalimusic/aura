//! 48 kHz stereo / 20 ms Opus pipeline. Device capture is explicitly process scoped.
pub mod jitter;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::{capture, playback, playback_with_gain, sources, test_tone};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
pub const FRAME_SAMPLES: usize = 960;
pub const FRAME_VALUES: usize = FRAME_SAMPLES * CHANNELS;
pub const MAX_PACKET: usize = 1100;

#[derive(Clone, Debug)]
pub struct Source {
    pub pid: u32,
    pub name: String,
    pub started: u64,
}
#[derive(Clone, Debug)]
pub struct EncodedFrame {
    pub sequence: u64,
    pub opus: Vec<u8>,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct AudioStats {
    pub frames: u64,
    pub rms: f32,
    pub concealed: u64,
    pub dropped: u64,
    pub buffer_ms: u32,
}

pub struct Encoder(opus::Encoder);
impl Encoder {
    pub fn new() -> Result<Self> {
        let mut encoder = opus::Encoder::new(
            SAMPLE_RATE,
            opus::Channels::Stereo,
            opus::Application::Audio,
        )?;
        encoder.set_bitrate(opus::Bitrate::Bits(128_000))?;
        encoder.set_inband_fec(true)?;
        encoder.set_packet_loss_perc(5)?;
        Ok(Self(encoder))
    }
    pub fn encode(&mut self, pcm: &[f32], sequence: u64) -> Result<EncodedFrame> {
        if pcm.len() != FRAME_VALUES {
            return Err("Expected exactly 20 ms stereo PCM".into());
        }
        let mut bytes = vec![0; MAX_PACKET];
        let size = self.0.encode_float(pcm, &mut bytes)?;
        bytes.truncate(size);
        Ok(EncodedFrame {
            sequence,
            opus: bytes,
        })
    }
}
pub struct Decoder(opus::Decoder);
impl Decoder {
    pub fn new() -> Result<Self> {
        Ok(Self(opus::Decoder::new(
            SAMPLE_RATE,
            opus::Channels::Stereo,
        )?))
    }
    pub fn decode(&mut self, packet: Option<&[u8]>) -> Result<Vec<f32>> {
        if packet.is_some_and(|p| p.is_empty() || p.len() > MAX_PACKET) {
            return Err("Invalid Opus packet size".into());
        }
        let mut pcm = vec![0.0; FRAME_VALUES];
        let frames = self
            .0
            .decode_float(packet.unwrap_or(&[]), &mut pcm, false)?;
        if frames != FRAME_SAMPLES {
            return Err("Only 20 ms Opus frames are accepted".into());
        }
        Ok(pcm)
    }
}
pub fn rms(pcm: &[f32]) -> f32 {
    if pcm.is_empty() {
        return 0.0;
    }
    (pcm.iter().map(|x| x * x).sum::<f32>() / pcm.len() as f32).sqrt()
}

#[cfg(not(target_os = "windows"))]
pub fn sources() -> Result<Vec<Source>> {
    Err("Process audio capture currently requires Windows build 20348+".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opus_round_trip_and_loss_concealment() {
        let mut encoder = Encoder::new().unwrap();
        let mut decoder = Decoder::new().unwrap();
        for frame in 0..10 {
            let pcm: Vec<f32> = (0..FRAME_VALUES)
                .map(|i| {
                    ((frame * FRAME_SAMPLES + i / 2) as f32 * 440.0 * std::f32::consts::TAU
                        / SAMPLE_RATE as f32)
                        .sin()
                        * 0.1
                })
                .collect();
            let packet = encoder.encode(&pcm, frame as u64).unwrap();
            assert!(packet.opus.len() <= MAX_PACKET);
            let decoded = decoder.decode(Some(&packet.opus)).unwrap();
            assert_eq!(decoded.len(), FRAME_VALUES);
            if frame > 0 {
                assert!(rms(&decoded) > 0.02);
            }
        }
        assert_eq!(decoder.decode(None).unwrap().len(), FRAME_VALUES);
        assert!(encoder.encode(&[0.0; 5], 0).is_err());
        assert!(decoder.decode(Some(&[0; MAX_PACKET + 1])).is_err());
    }
}
