use serde::{Deserialize, Serialize};

/// Per-channel RGB curves. Each channel is a list of control points [x, y] in 0..1.
/// Default has endpoints [[0,0],[1,1]] (identity).
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// Six Hue/Sat curves. Each curve: Vec of [x, y] control points.
/// x = 0..1 (input range), y = 0..1 (0.5 = identity for relative curves).
/// Empty Vec = identity (no change).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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
}

/// HSL qualifier for secondary color correction.
/// Selects a range of hue/saturation/luminance to isolate specific colors.
/// The matte (0-1 per pixel) determines how much the secondary grade is applied.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualifierSettings {
    pub enabled: bool,
    /// Hue center in degrees (0-360)
    pub hue_center: f32,
    /// Hue width in degrees (half-width of the selection)
    pub hue_width: f32,
    /// Hue feather (soft edge, degrees)
    pub hue_soft: f32,
    /// Saturation range (0-1)
    pub sat_low: f32,
    pub sat_high: f32,
    pub sat_soft: f32,
    /// Luminance range (0-1)
    pub lum_low: f32,
    pub lum_high: f32,
    pub lum_soft: f32,
    /// Invert the matte
    pub invert: bool,
    /// Secondary grade adjustment (simpler: just exposure, sat, hue shift)
    pub exposure: f32,
    pub saturation: f32,
    pub hue_shift: f32,
    pub temperature: f32,
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
        }
    }
}

/// Shape-based mask for regional color grading (power windows).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MaskShape {
    Circle {
        center_x: f32, // normalized 0-1
        center_y: f32,
        radius: f32,   // normalized to frame width
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
    /// Freeform bezier path mask. Points are in normalized coords (0-1).
    /// The path is closed (last point connects to first).
    Bezier {
        /// Control points as (x, y) pairs in normalized coordinates.
        points: Vec<(f32, f32)>,
    },
}

/// A single mask with shape, feathering, and blend controls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mask {
    pub shape: MaskShape,
    pub feather: f32,   // 0-1, soft edge width
    pub invert: bool,
    pub opacity: f32,   // 0-1
    pub enabled: bool,
}

impl Default for Mask {
    fn default() -> Self {
        Self {
            shape: MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.2 },
            feather: 0.1,
            invert: false,
            opacity: 1.0,
            enabled: true,
        }
    }
}

/// Stack of masks applied to a clip's color grade.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MaskStack {
    pub masks: Vec<Mask>,
}

impl MaskStack {
    pub fn is_empty(&self) -> bool {
        self.masks.is_empty() || self.masks.iter().all(|m| !m.enabled)
    }
}

/// Film grain overlay settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilmGrainSettings {
    pub enabled: bool,
    /// Grain intensity (0.0 = off, 1.0 = maximum)
    pub amount: f32,
    /// Grain particle size (0.5 = fine, 3.0 = coarse)
    pub size: f32,
    /// How much grain in shadows (0.0 = none, 1.0 = full)
    pub shadows: f32,
    /// How much grain in highlights (0.0 = none, 1.0 = full)
    pub highlights: f32,
    /// Monochrome grain (true) vs colour grain (false)
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

/// Color Warper parameters: 12 hue sectors at 30-degree intervals.
/// Each sector can shift hue (degrees) and saturation (relative offset).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColorWarperParams {
    /// Per-sector hue shifts in degrees. Index 0 = red (0deg), 1 = orange (30deg), etc.
    #[serde(default)]
    pub hue_shifts: [f32; 12],
    /// Per-sector saturation offsets. 0.0 = no change, -1.0 = desaturate, 1.0 = double.
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

impl ColorWarperParams {
    pub fn is_identity(&self) -> bool {
        self.hue_shifts.iter().all(|v| v.abs() < 0.001)
            && self.sat_shifts.iter().all(|v| v.abs() < 0.001)
    }
}

impl HueSatCurves {
    pub fn is_all_identity(&self) -> bool {
        self.hue_vs_hue.is_empty()
            && self.hue_vs_sat.is_empty()
            && self.hue_vs_lum.is_empty()
            && self.lum_vs_sat.is_empty()
            && self.sat_vs_sat.is_empty()
            && self.orange_vs_sat.is_empty()
    }
}
