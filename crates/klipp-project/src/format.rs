use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct ProjectJson {
    pub version: String,
    pub klipp_version_min: String,
    pub sequence: SequenceJson,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SequenceJson {
    pub id: String,
    pub name: String,
    pub fps: FpsJson,
    pub resolution: ResolutionJson,
    pub tracks: Vec<TrackJson>,
    #[serde(default)]
    pub in_point: Option<TimecodeJson>,
    #[serde(default)]
    pub out_point: Option<TimecodeJson>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimecodeJson {
    pub value: i64,
    pub rate: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FpsJson {
    pub num: u32,
    pub den: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ResolutionJson {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TrackJson {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub clips: Vec<ClipJson>,
    #[serde(default = "default_opacity")]
    pub opacity: f32,
    #[serde(default = "default_volume")]
    pub volume: f32,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub solo: bool,
    #[serde(default = "default_visible")]
    pub visible: bool,
}

fn default_opacity() -> f32 { 1.0 }
fn default_volume() -> f32 { 1.0 }
fn default_visible() -> bool { true }

fn default_rate() -> u32 { 24 }

#[derive(Debug, Serialize, Deserialize)]
pub struct ClipJson {
    pub id: String,
    pub media_id: String,
    pub timeline_in: i64,
    pub timeline_out: i64,
    pub source_in: i64,
    pub source_out: i64,
    #[serde(default = "default_rate")]
    pub rate: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MediaManifestJson {
    pub version: String,
    pub items: Vec<MediaItemJson>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MediaItemJson {
    pub id: String,
    pub path: String,
    pub filename: String,
    pub duration_frames: i64,
    #[serde(default = "default_rate")]
    pub duration_rate: u32,
    pub fps: FpsJson,
    pub resolution: ResolutionJson,
    pub video_codec: String,
    pub audio_codec: String,
    pub has_audio: bool,
    pub file_size_bytes: u64,
    #[serde(default)]
    pub proxy_path: Option<String>,
    #[serde(default)]
    pub proxy_ready: bool,
    #[serde(default)]
    pub default_source_in: Option<TimecodeJson>,
    #[serde(default)]
    pub default_source_out: Option<TimecodeJson>,
}
