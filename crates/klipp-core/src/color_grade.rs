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
