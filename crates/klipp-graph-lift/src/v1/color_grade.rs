// Frozen from crates/klipp-core/src/color_grade.rs at 7d309e9f. Do not follow app type changes.
use super::keyframe::Keyframe;
use serde::{Deserialize, Serialize};
#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub struct CurveSet {
    #[serde(default = "default_curve_points")]
    pub master: Vec<[f32; 2]>,
    #[serde(default = "default_curve_points")]
    pub red: Vec<[f32; 2]>,
    #[serde(default = "default_curve_points")]
    pub green: Vec<[f32; 2]>,
    #[serde(default = "default_curve_points")]
    pub blue: Vec<[f32; 2]>,
}

fn default_curve_points() -> Vec<[f32; 2]> {
    vec![[0.0, 0.0], [1.0, 1.0]]
}

impl Default for CurveSet {
    fn default() -> Self {
        Self {
            master: default_curve_points(),
            red: default_curve_points(),
            green: default_curve_points(),
            blue: default_curve_points(),
        }
    }
}

#[derive(PartialEq, Debug, Clone, Serialize, Deserialize, Default)]
pub struct HueSatCurves {
    #[serde(default)]
    pub hue_vs_hue: Vec<[f32; 2]>,
    #[serde(default)]
    pub hue_vs_sat: Vec<[f32; 2]>,
    #[serde(default)]
    pub hue_vs_lum: Vec<[f32; 2]>,
    #[serde(default)]
    pub lum_vs_sat: Vec<[f32; 2]>,
    #[serde(default)]
    pub sat_vs_sat: Vec<[f32; 2]>,
    #[serde(default)]
    pub orange_vs_sat: Vec<[f32; 2]>,
    #[serde(default)]
    pub sat_vs_lum: Vec<[f32; 2]>,
}

#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub struct QualifierSettings {
    pub enabled: bool,
    pub hue_center: f32,
    pub hue_width: f32,
    pub hue_soft: f32,
    pub sat_low: f32,
    pub sat_high: f32,
    pub sat_soft: f32,
    pub lum_low: f32,
    pub lum_high: f32,
    pub lum_soft: f32,
    pub invert: bool,
    pub exposure: f32,
    pub saturation: f32,
    pub hue_shift: f32,
    pub temperature: f32,
    #[serde(default)]
    pub clean_black: f32,
    #[serde(default)]
    pub clean_white: f32,
    #[serde(default)]
    pub shrink_grow: f32,
    #[serde(default)]
    pub blur: f32,
    /// Keyed by HSL or by RGB (#895).
    #[serde(default)]
    pub mode: QualifierMode,
    #[serde(default = "zero")]
    pub red_low: f32,
    #[serde(default = "one")]
    pub red_high: f32,
    #[serde(default = "zero")]
    pub green_low: f32,
    #[serde(default = "one")]
    pub green_high: f32,
    #[serde(default = "zero")]
    pub blue_low: f32,
    #[serde(default = "one")]
    pub blue_high: f32,
    #[serde(default = "tenth")]
    pub rgb_soft: f32,
}

fn zero() -> f32 {
    0.0
}
fn one() -> f32 {
    1.0
}
fn tenth() -> f32 {
    0.1
}

#[derive(PartialEq, Eq, Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub enum QualifierMode {
    #[default]
    Hsl,
    Rgb,
}

impl Default for QualifierSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            hue_center: 0.0,
            hue_width: 30.0,
            hue_soft: 10.0,
            sat_low: 0.0,
            sat_high: 1.0,
            sat_soft: 0.1,
            lum_low: 0.0,
            lum_high: 1.0,
            lum_soft: 0.1,
            invert: false,
            exposure: 0.0,
            saturation: 1.0,
            hue_shift: 0.0,
            temperature: 0.0,
            clean_black: 0.0,
            clean_white: 0.0,
            shrink_grow: 0.0,
            blur: 0.0,
            mode: QualifierMode::Hsl,
            red_low: 0.0,
            red_high: 1.0,
            green_low: 0.0,
            green_high: 1.0,
            blue_low: 0.0,
            blue_high: 1.0,
            rgb_soft: 0.1,
        }
    }
}

#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub struct LumaKeySettings {
    pub enabled: bool,
    pub low: f32,
    pub high: f32,
    pub soft: f32,
    pub invert: bool,
}

impl Default for LumaKeySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            low: 0.0,
            high: 1.0,
            soft: 0.05,
            invert: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DifferenceKeyMatchSpace {
    #[default]
    Rgb,
    Luminance,
}

#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub struct DifferenceKeySettings {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plate_path: Option<std::path::PathBuf>,
    pub tolerance: f32,
    pub soft: f32,
    pub invert: bool,
    #[serde(default)]
    pub match_space: DifferenceKeyMatchSpace,
}

impl Default for DifferenceKeySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            plate_path: None,
            tolerance: 0.05,
            soft: 0.02,
            invert: false,
            match_space: DifferenceKeyMatchSpace::Rgb,
        }
    }
}

#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub struct SoftClipSettings {
    pub enabled: bool,
    pub threshold: f32,
    pub knee: f32,
}

impl Default for SoftClipSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold: 0.95,
            knee: 0.05,
        }
    }
}

#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub enum MaskShape {
    Circle {
        center_x: f32, // normalized 0-1
        center_y: f32,
        radius: f32, // normalized to frame width
    },
    Rectangle {
        center_x: f32,
        center_y: f32,
        width: f32,
        height: f32,
        rotation: f32, // degrees
    },
    LinearGradient {
        start_x: f32,
        start_y: f32,
        end_x: f32,
        end_y: f32,
    },
    Bezier {
        points: Vec<(f32, f32)>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowMotion {
    pub dx: f32,
    pub dy: f32,
    pub scale: f32,
    pub rotation: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaneMap(pub [f32; 9]);

#[derive(PartialEq, Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CornerKey {
    pub time_secs: f64,
    pub corners: [[f32; 2]; 4],
}

#[derive(PartialEq, Debug, Clone, Default, Serialize, Deserialize)]
pub struct PerspectiveTrack {
    pub from: [[f32; 2]; 4],
    pub keys: Vec<CornerKey>,
}

#[derive(PartialEq, Debug, Clone, Default, Serialize, Deserialize)]
pub struct MaskKeyframes {
    #[serde(default)]
    pub center_x: Vec<Keyframe>,
    #[serde(default)]
    pub center_y: Vec<Keyframe>,
    #[serde(default)]
    pub radius: Vec<Keyframe>,
    #[serde(default)]
    pub width: Vec<Keyframe>,
    #[serde(default)]
    pub height: Vec<Keyframe>,
    #[serde(default)]
    pub rotation: Vec<Keyframe>,
    #[serde(default)]
    pub feather: Vec<Keyframe>,
    #[serde(default)]
    pub opacity: Vec<Keyframe>,
    #[serde(default)]
    pub start_x: Vec<Keyframe>,
    #[serde(default)]
    pub start_y: Vec<Keyframe>,
    #[serde(default)]
    pub end_x: Vec<Keyframe>,
    #[serde(default)]
    pub end_y: Vec<Keyframe>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub track_x: Vec<Keyframe>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub track_y: Vec<Keyframe>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub track_scale: Vec<Keyframe>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub track_rotation: Vec<Keyframe>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_perspective: Option<PerspectiveTrack>,
    #[serde(skip)]
    pub warp: Option<PlaneMap>,
}

#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub struct Mask {
    pub shape: MaskShape,
    pub feather: f32, // 0-1, soft edge width
    pub invert: bool,
    pub opacity: f32, // 0-1
    pub enabled: bool,
    #[serde(default)]
    pub keyframes: MaskKeyframes,
}

impl Default for Mask {
    fn default() -> Self {
        Self {
            shape: MaskShape::Circle {
                center_x: 0.5,
                center_y: 0.5,
                radius: 0.2,
            },
            feather: 0.1,
            invert: false,
            opacity: 1.0,
            enabled: true,
            keyframes: MaskKeyframes::default(),
        }
    }
}

#[derive(PartialEq, Debug, Clone, Serialize, Deserialize, Default)]
pub struct MaskStack {
    pub masks: Vec<Mask>,
}

#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub struct FilmGrainSettings {
    pub enabled: bool,
    pub amount: f32,
    pub size: f32,
    pub shadows: f32,
    pub highlights: f32,
    pub monochrome: bool,
}

impl Default for FilmGrainSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            amount: 0.0,
            size: 1.0,
            shadows: 1.0,
            highlights: 0.5,
            monochrome: true,
        }
    }
}

#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub struct ColorWarperParams {
    #[serde(default)]
    pub hue_shifts: [f32; 12],
    #[serde(default)]
    pub sat_shifts: [f32; 12],
}

impl Default for ColorWarperParams {
    fn default() -> Self {
        Self {
            hue_shifts: [0.0; 12],
            sat_shifts: [0.0; 12],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ColorMatchSettings {
    pub exposure: f32,
    pub temperature: f32,
    pub tint: f32,
    pub saturation: f32,
}

impl Default for ColorMatchSettings {
    fn default() -> Self {
        Self {
            exposure: 0.0,
            temperature: 0.0,
            tint: 0.0,
            saturation: 1.0,
        }
    }
}

#[derive(PartialEq, Debug, Clone, Copy, Serialize, Deserialize)]
pub struct LayerMixerInput {
    pub opacity: f32,
    pub blend_mode: super::effects::BlendMode,
}

impl Default for LayerMixerInput {
    fn default() -> Self {
        Self {
            opacity: 1.0,
            blend_mode: super::effects::BlendMode::Normal,
        }
    }
}
