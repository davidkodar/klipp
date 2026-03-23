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
