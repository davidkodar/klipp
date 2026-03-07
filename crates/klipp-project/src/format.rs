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
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ClipJson {
    pub id: String,
    pub media_id: String,
    pub timeline_in: i64,
    pub timeline_out: i64,
    pub source_in: i64,
    pub source_out: i64,
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
    pub fps: FpsJson,
    pub resolution: ResolutionJson,
    pub video_codec: String,
    pub audio_codec: String,
    pub has_audio: bool,
    pub file_size_bytes: u64,
}
