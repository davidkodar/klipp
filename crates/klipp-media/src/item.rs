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
    /// User rating (0–5 stars)
    #[serde(default)]
    pub rating: u8,
    /// User tags
    #[serde(default)]
    pub tags: Vec<String>,
    /// User notes
    #[serde(default)]
    pub user_notes: String,
    /// Bin name (None = root/unassigned)
    #[serde(default)]
    pub bin: Option<String>,
    /// Unix epoch seconds when item was added to the project (0 for pre-existing items)
    #[serde(default)]
    pub added_at: u64,
    /// Camera log encoding format (for IDT conversion)
    #[serde(default)]
    pub log_format: klipp_core::LogFormat,
    /// Path to skim-optimised file (all-keyframe transcode for instant seeking)
    #[serde(default)]
    pub skim_file: Option<PathBuf>,
    /// Waveform min/max pairs for timeline display. Generated in background.
    #[serde(skip)]
    pub waveform_data: Option<Vec<(f32, f32)>>,
    /// Audio cache entry — set after background conforming completes.
    #[serde(skip)]
    pub audio_cache: Option<crate::audio_cache::AudioCacheEntry>,
}
