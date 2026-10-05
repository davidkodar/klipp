// Frozen from crates/klipp-core/src/keyframe.rs at 7d309e9f. Do not follow app type changes.
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Interpolation {
    #[default]
    Linear,
    Step,
    Bezier,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Keyframe {
    pub time_secs: f64,
    pub value: f32,
    pub interpolation: Interpolation,
    #[serde(default)]
    pub tangent_out: (f64, f32),
    #[serde(default)]
    pub tangent_in: (f64, f32),
}
