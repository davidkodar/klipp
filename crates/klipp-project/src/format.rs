use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Serialize, Deserialize)]
pub struct ProjectJson {
    pub version: String,
    pub klipp_version_min: String,
    /// The Klipp build version that wrote this file.
    #[serde(default)]
    pub klipp_version: Option<String>,
    /// ISO 8601 timestamp of when the project was saved.
    #[serde(default)]
    pub saved_at: Option<String>,
    /// Platform that saved the project (e.g. "linux-x86_64").
    #[serde(default)]
    pub platform: Option<String>,
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
    #[serde(default)]
    pub global_lut_path: Option<String>,
    #[serde(default = "default_lut_opacity")]
    pub global_lut_opacity: f32,
    #[serde(default)]
    pub markers: Vec<MarkerJson>,
    #[serde(default = "default_header_width")]
    pub track_header_width: f32,
}

fn default_header_width() -> f32 { 160.0 }
fn default_lut_opacity() -> f32 { 1.0 }

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
    #[serde(default)]
    pub transitions: Vec<TransitionJson>,
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
    #[serde(default)]
    pub pan: f32,
    #[serde(default)]
    pub locked: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TransitionJson {
    pub id: String,
    pub kind: String,
    pub duration: TimecodeJson,
    pub clip_a_id: String,
    pub clip_b_id: String,
}

fn default_opacity() -> f32 { 1.0 }
fn default_volume() -> f32 { 1.0 }
fn default_visible() -> bool { true }

fn default_rate() -> u32 { 24 }

#[derive(Debug, Serialize, Deserialize)]
pub struct ClipJson {
    pub id: String,
    /// Old field — kept for backward compatibility when loading old projects.
    #[serde(default)]
    pub media_id: Option<String>,
    /// New field — used for all new saves.
    #[serde(default)]
    pub kind: Option<ClipKindJson>,
    pub timeline_in: i64,
    pub timeline_out: i64,
    pub source_in: i64,
    pub source_out: i64,
    #[serde(default = "default_rate")]
    pub rate: u32,
    #[serde(default)]
    pub color_grade: Option<ColorGradeJson>,
    #[serde(default)]
    pub effects: Vec<EffectJson>,
    #[serde(default)]
    pub builtin_effects: Option<serde_json::Value>,
    #[serde(default)]
    pub linked_clip_id: Option<String>,
    #[serde(default)]
    pub group_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClipKindJson {
    Media {
        media_id: String,
    },
    Title {
        text: String,
        #[serde(default = "default_font_size")]
        font_size: f32,
        #[serde(default = "default_font_name")]
        font_name: String,
        #[serde(default = "default_color_white")]
        color: [u8; 4],
        #[serde(default = "default_alignment")]
        alignment: String,
        #[serde(default = "default_half")]
        position_x: f32,
        #[serde(default = "default_half")]
        position_y: f32,
        #[serde(default)]
        background: serde_json::Value,
        #[serde(default)]
        shadow: serde_json::Value,
    },
    Gap,
}

fn default_font_size() -> f32 { 72.0 }
fn default_font_name() -> String { "sans-serif".to_string() }
fn default_color_white() -> [u8; 4] { [255, 255, 255, 255] }
fn default_alignment() -> String { "Center".to_string() }
fn default_half() -> f32 { 0.5 }

#[derive(Debug, Serialize, Deserialize)]
pub struct ColorGradeJson {
    pub enabled: bool,
    #[serde(default)]
    pub exposure: f32,
    #[serde(default)]
    pub contrast: f32,
    #[serde(default = "default_saturation")]
    pub saturation: f32,
    #[serde(default)]
    pub temperature: f32,
    #[serde(default)]
    pub tint: f32,
}

fn default_saturation() -> f32 { 1.0 }

#[derive(Debug, Serialize, Deserialize)]
pub struct EffectJson {
    pub plugin_name: String,
    #[serde(default = "default_visible")]
    pub enabled: bool,
    #[serde(default)]
    pub param_values: HashMap<String, OfxValueJson>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OfxValueJson {
    Double(f64),
    Int(i64),
    Bool(bool),
    String(String),
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
    #[serde(default)]
    pub rating: u8,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub user_notes: String,
    #[serde(default)]
    pub bin: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MarkerJson {
    pub id: String,
    pub position: TimecodeJson,
    pub label: String,
    #[serde(default = "default_marker_color_str")]
    pub color: String,
}

fn default_marker_color_str() -> String {
    "Blue".to_string()
}
