use klipp_core::{Fps, MediaId, Resolution, Timecode};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaItem {
    pub id: MediaId,
    pub path: PathBuf,
    pub filename: String,
    pub duration: Timecode,
    pub fps: Fps,
    pub resolution: Resolution,
    pub video_codec: String,
    pub audio_codec: String,
    pub has_audio: bool,
    pub file_size_bytes: u64,
    #[serde(default)]
    pub offline: bool,
    #[serde(default)]
    pub proxy_path: Option<PathBuf>,
    #[serde(default)]
    pub proxy_ready: bool,
    #[serde(default)]
    pub default_source_in: Option<Timecode>,
    #[serde(default)]
    pub default_source_out: Option<Timecode>,
    /// Waveform min/max pairs for timeline display. Generated in background.
    #[serde(skip)]
    pub waveform_data: Option<Vec<(f32, f32)>>,
    /// Audio cache entry — set after background conforming completes.
    #[serde(skip)]
    pub audio_cache: Option<crate::audio_cache::AudioCacheEntry>,
}
