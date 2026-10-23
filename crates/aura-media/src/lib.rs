//! Bounded, versioned now-playing messages. No URLs, remote fetches or disk cache.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use std::{io::Cursor, sync::Arc};
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub const MAX_MESSAGE: usize = 48 * 1024;
pub const MAX_ART: usize = 24 * 1024;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Playback {
    Playing,
    Paused,
    Stopped,
    #[default]
    Unknown,
}
impl Playback {
    pub fn label(self) -> &'static str {
        match self {
            Self::Playing => "Играет",
            Self::Paused => "Пауза",
            Self::Stopped => "Остановлено",
            Self::Unknown => "Музыка",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Info {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub app: String,
    pub playback: Playback,
    pub position_ms: u64,
    pub duration_ms: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Artwork {
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<[u8]>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub info: Option<Info>,
    pub artwork: Option<Arc<Artwork>>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    version: u8,
    info: Option<Info>,
    cover: Cover,
}
#[derive(Serialize, Deserialize)]
enum Cover {
    Keep,
    Clear,
    Jpeg(String),
}
pub fn clean(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(256).collect()
}
fn validate(info: &Info) -> Result<()> {
    for text in [&info.title, &info.artist, &info.album, &info.app] {
        if text.chars().count() > 256 || text.chars().any(char::is_control) {
            return Err("Invalid media text".into());
        }
    }
    if info.position_ms > 7 * 24 * 3600 * 1000 || info.duration_ms > 7 * 24 * 3600 * 1000 {
        return Err("Invalid media timeline".into());
    }
    Ok(())
}
fn image(bytes: &[u8], max: u32) -> Result<image::DynamicImage> {
    if bytes.is_empty() || bytes.len() > 1024 * 1024 {
        return Err("Thumbnail too large".into());
    }
    let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(max);
    limits.max_image_height = Some(max);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    Ok(reader.decode()?)
}
/// Local Windows thumbnails are decoded with limits and transcoded before transmission.
pub fn thumbnail(bytes: &[u8]) -> Result<Arc<Artwork>> {
    let picture = image(bytes, 4096)?
        .resize_to_fill(160, 160, image::imageops::FilterType::Triangle)
        .to_rgb8();
    let mut jpeg = vec![];
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 70).encode_image(&picture)?;
    artwork(&jpeg)
}
pub fn artwork(jpeg: &[u8]) -> Result<Arc<Artwork>> {
    if jpeg.len() > MAX_ART || !jpeg.starts_with(&[0xff, 0xd8]) {
        return Err("Invalid cover".into());
    }
    let picture = image(jpeg, 160)?.to_rgba8();
    let (width, height) = picture.dimensions();
    Ok(Arc::new(Artwork {
        jpeg: jpeg.into(),
        width,
        height,
        rgba: picture.into_raw().into(),
    }))
}
/// `previous` is per connection, ensuring late joiners always receive a full cover.
pub fn encode(snapshot: &Snapshot, previous: &mut Option<Arc<Artwork>>) -> Result<Vec<u8>> {
    if let Some(info) = &snapshot.info {
        validate(info)?;
    }
    let art = if snapshot.info.is_some() {
        snapshot.artwork.clone()
    } else {
        None
    };
    let cover = if art == *previous {
        Cover::Keep
    } else {
        match &art {
            Some(art) => Cover::Jpeg(STANDARD.encode(&art.jpeg)),
            None => Cover::Clear,
        }
    };
    let bytes = serde_json::to_vec(&Update {
        version: 1,
        info: snapshot.info.clone(),
        cover,
    })?;
    if bytes.len() > MAX_MESSAGE {
        return Err("Metadata message too large".into());
    }
    *previous = art;
    Ok(bytes)
}
pub fn decode(bytes: &[u8], previous: &mut Option<Arc<Artwork>>) -> Result<Snapshot> {
    if bytes.len() > MAX_MESSAGE {
        return Err("Metadata message too large".into());
    }
    let update: Update = serde_json::from_slice(bytes)?;
    if update.version != 1 {
        return Err("Unsupported metadata version".into());
    }
    if let Some(info) = &update.info {
        validate(info)?;
    }
    let art = match update.cover {
        Cover::Keep => previous.clone(),
        Cover::Clear => None,
        Cover::Jpeg(value) => Some(artwork(&STANDARD.decode(value)?)?),
    };
    *previous = if update.info.is_some() { art } else { None };
    Ok(Snapshot {
        info: update.info,
        artwork: previous.clone(),
    })
}
/// Only a unique application session and a unique running process family qualify.
/// Browser session IDs use the app name (for example `Chrome`) instead of the
/// process image name (`chrome.exe`), so compare the normalized executable stem.
pub fn unique_session(
    ids: &[String],
    app_id: Option<&str>,
    exe: &str,
    unique_process: bool,
) -> Option<usize> {
    if !unique_process {
        return None;
    }
    let matches: Vec<_> = ids
        .iter()
        .enumerate()
        .filter(|(_, id)| {
            app_id.is_some_and(|app| !app.is_empty() && id.eq_ignore_ascii_case(app))
                || app_session_matches_exe(id, exe)
        })
        .map(|(i, _)| i)
        .collect();
    if matches.len() == 1 {
        Some(matches[0])
    } else {
        None
    }
}
fn app_session_matches_exe(app: &str, exe: &str) -> bool {
    fn stem(value: &str) -> &str {
        value
            .rsplit_once('.')
            .filter(|(_, ext)| ext.eq_ignore_ascii_case("exe"))
            .map_or(value, |(stem, _)| stem)
    }
    stem(app).eq_ignore_ascii_case(stem(exe))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ambiguous_sources_never_pick_current_or_other_player() {
        let ids = vec!["Spotify.exe".into(), "Chrome".into()];
        assert_eq!(unique_session(&ids, None, "Spotify.exe", true), Some(0));
        assert_eq!(unique_session(&ids, None, "chrome.exe", true), Some(1));
        assert_eq!(unique_session(&ids, None, "unrelated.exe", true), None);
        assert_eq!(unique_session(&ids, None, "Spotify.exe", false), None);
        assert_eq!(
            unique_session(
                &["Spotify.exe".into(), "Spotify.exe".into()],
                None,
                "Spotify.exe",
                true
            ),
            None
        );
        assert_eq!(
            unique_session(&ids, Some("Chrome"), "chrome.exe", true),
            Some(1)
        );
        assert_eq!(
            unique_session(
                &["Spotify.exe".into(), "Spotify".into()],
                None,
                "Spotify.exe",
                true
            ),
            None
        );
    }
    #[test]
    fn cover_is_sent_once_per_listener_and_cleared_on_unknown() {
        let image = image::RgbImage::from_pixel(20, 20, image::Rgb([220, 30, 80]));
        let mut bytes = vec![];
        image::codecs::jpeg::JpegEncoder::new(&mut bytes)
            .encode_image(&image)
            .unwrap();
        let art = thumbnail(&bytes).unwrap();
        let snapshot = Snapshot {
            info: Some(Info {
                title: "Трек".into(),
                artist: "Исполнитель".into(),
                album: String::new(),
                app: "Spotify".into(),
                playback: Playback::Playing,
                position_ms: 3000,
                duration_ms: 120000,
            }),
            artwork: Some(art),
        };
        let mut sent = None;
        let mut received = None;
        let first = encode(&snapshot, &mut sent).unwrap();
        assert_eq!(decode(&first, &mut received).unwrap(), snapshot);
        let next = encode(&snapshot, &mut sent).unwrap();
        assert!(next.len() < first.len() / 2);
        assert_eq!(decode(&next, &mut received).unwrap(), snapshot);
        assert_eq!(
            decode(
                &encode(&Snapshot::default(), &mut sent).unwrap(),
                &mut received
            )
            .unwrap(),
            Snapshot::default()
        );
        assert!(received.is_none());
        assert!(decode(&vec![0; MAX_MESSAGE + 1], &mut received).is_err());
        assert!(artwork(&vec![0; MAX_ART + 1]).is_err());
    }
}
