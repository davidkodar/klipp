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
    /// ISO 8601 timestamp of when the project was first created.
    #[serde(default)]
    pub created_at: Option<String>,
    /// Platform that saved the project (e.g. "linux-x86_64").
    #[serde(default)]
    pub platform: Option<String>,
    /// All sequences in this project. Always at least one.
    pub sequences: Vec<SequenceJson>,
    /// Which sequence was active when the project was last saved.
    #[serde(default)]
    pub active_sequence_id: Option<String>,
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
    #[serde(default)]
    pub last_playhead: Option<TimecodeJson>,
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
    #[serde(default)]
    pub highlights: f32,
    #[serde(default)]
    pub shadows: f32,
    #[serde(default)]
    pub whites: f32,
    #[serde(default)]
    pub blacks: f32,
    #[serde(default)]
    pub lift_x: f32,
    #[serde(default)]
    pub lift_y: f32,
    #[serde(default)]
    pub lift_master: f32,
    #[serde(default)]
    pub gamma_x: f32,
    #[serde(default)]
    pub gamma_y: f32,
    #[serde(default)]
    pub gamma_master: f32,
    #[serde(default)]
    pub gain_x: f32,
    #[serde(default)]
    pub gain_y: f32,
    #[serde(default)]
    pub gain_master: f32,
    #[serde(default)]
    pub offset_x: f32,
    #[serde(default)]
    pub offset_y: f32,
    #[serde(default)]
    pub offset_master: f32,
    #[serde(default)]
    pub lift_sat: f32,
    #[serde(default)]
    pub lift_bri: f32,
    #[serde(default)]
    pub gamma_sat: f32,
    #[serde(default)]
    pub gamma_bri: f32,
    #[serde(default)]
    pub gain_sat: f32,
    #[serde(default)]
    pub gain_bri: f32,
    #[serde(default)]
    pub offset_sat: f32,
    #[serde(default)]
    pub offset_bri: f32,
    #[serde(default)]
    pub hue: f32,
    #[serde(default = "default_one")]
    pub global_mix: f32,
    #[serde(default)]
    pub curves: Option<klipp_timeline::CurveSet>,
    #[serde(default)]
    pub curves_preserve_luma: bool,
    #[serde(default = "default_one")]
    pub curves_mix: f32,
    #[serde(default)]
    pub huesat: Option<klipp_core::HueSatCurves>,
    #[serde(default = "default_one")]
    pub huesat_mix: f32,
    #[serde(default)]
    pub qualifier: Option<klipp_core::QualifierSettings>,
    #[serde(default)]
    pub masks: Option<klipp_core::MaskStack>,
    #[serde(default)]
    pub keyframe_tracks: Option<klipp_core::KeyframeTracks>,
}

fn default_one() -> f32 { 1.0 }

fn default_saturation() -> f32 { 1.0 }

#[derive(Debug, Serialize, Deserialize)]
pub struct EffectJson {
    pub plugin_name: String,
    #[serde(default = "default_visible")]
    pub enabled: bool,
    #[serde(default)]
    pub param_values: HashMap<String, OfxValueJson>,
}

/// OFX parameter value for JSON serialisation.
/// Uses tagged format for unambiguous round-trips.
/// Old projects (untagged) are handled by a custom Deserialize fallback.
#[derive(Debug, Serialize)]
#[serde(tag = "t", content = "v")]
pub enum OfxValueJson {
    Double(f64),
    Double2([f64; 2]),
    Double3([f64; 3]),
    Int(i32),
    Int2([i32; 2]),
    Int3([i32; 3]),
    Bool(bool),
    Choice(i32),
    String(String),
    Rgba([f64; 4]),
    Rgb([f64; 3]),
}

// Custom Deserialize: try tagged first, then fall back to untagged for old projects.
impl<'de> serde::Deserialize<'de> for OfxValueJson {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de;
        use serde_json::Value;

        let v = Value::deserialize(deserializer)?;

        // Tagged format: {"t": "Double", "v": 1.0}
        if let Some(tag) = v.get("t").and_then(|t| t.as_str()) {
            return match tag {
                "Double" => v.get("v").and_then(|x| x.as_f64()).map(OfxValueJson::Double)
                    .ok_or_else(|| de::Error::custom("invalid Double value")),
                "Double2" => serde_json::from_value(v.get("v").cloned().unwrap_or_default())
                    .map(OfxValueJson::Double2).map_err(de::Error::custom),
                "Double3" => serde_json::from_value(v.get("v").cloned().unwrap_or_default())
                    .map(OfxValueJson::Double3).map_err(de::Error::custom),
                "Int" => v.get("v").and_then(|x| x.as_i64()).map(|i| OfxValueJson::Int(i as i32))
                    .ok_or_else(|| de::Error::custom("invalid Int value")),
                "Int2" => serde_json::from_value(v.get("v").cloned().unwrap_or_default())
                    .map(OfxValueJson::Int2).map_err(de::Error::custom),
                "Int3" => serde_json::from_value(v.get("v").cloned().unwrap_or_default())
                    .map(OfxValueJson::Int3).map_err(de::Error::custom),
                "Bool" => v.get("v").and_then(|x| x.as_bool()).map(OfxValueJson::Bool)
                    .ok_or_else(|| de::Error::custom("invalid Bool value")),
                "Choice" => v.get("v").and_then(|x| x.as_i64()).map(|i| OfxValueJson::Choice(i as i32))
                    .ok_or_else(|| de::Error::custom("invalid Choice value")),
                "String" => v.get("v").and_then(|x| x.as_str()).map(|s| OfxValueJson::String(s.to_string()))
                    .ok_or_else(|| de::Error::custom("invalid String value")),
                "Rgba" => serde_json::from_value(v.get("v").cloned().unwrap_or_default())
                    .map(OfxValueJson::Rgba).map_err(de::Error::custom),
                "Rgb" => serde_json::from_value(v.get("v").cloned().unwrap_or_default())
                    .map(OfxValueJson::Rgb).map_err(de::Error::custom),
                _ => Err(de::Error::custom(format!("unknown OfxValueJson tag: {tag}"))),
            };
        }

        // Legacy untagged format: bare JSON values
        if let Some(b) = v.as_bool() {
            return Ok(OfxValueJson::Bool(b));
        }
        if let Some(s) = v.as_str() {
            return Ok(OfxValueJson::String(s.to_string()));
        }
        if let Some(n) = v.as_i64() {
            // Old format stored Int as i64
            return Ok(OfxValueJson::Int(n as i32));
        }
        if let Some(n) = v.as_f64() {
            return Ok(OfxValueJson::Double(n));
        }

        Err(de::Error::custom("cannot parse OfxValueJson"))
    }
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
    #[serde(default)]
    pub added_at: Option<u64>,
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
