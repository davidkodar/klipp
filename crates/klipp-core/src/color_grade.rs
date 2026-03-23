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
