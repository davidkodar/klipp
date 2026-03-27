use serde::{Deserialize, Serialize};

/// Interpolation method between keyframes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Interpolation {
    /// Linear interpolation between values.
    #[default]
    Linear,
    /// Hold the previous keyframe's value until the next keyframe.
    Step,
    /// Smooth cubic bezier interpolation (tangents on KeyframePoint).
    Bezier,
}

/// A single keyframe: a value at a specific time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Keyframe {
    /// Time in seconds (relative to clip's source_in).
    pub time_secs: f64,
    /// The parameter value at this time.
    pub value: f32,
    /// Interpolation method to the NEXT keyframe.
    pub interpolation: Interpolation,
    /// Bezier out-tangent: (time_delta, value_delta) relative to this keyframe.
    /// Positive time_delta means the handle extends to the right.
    #[serde(default)]
    pub tangent_out: (f64, f32),
    /// Bezier in-tangent: (time_delta, value_delta) relative to this keyframe.
    /// Negative time_delta means the handle extends to the left.
    #[serde(default)]
    pub tangent_in: (f64, f32),
}

/// Evaluate a keyframe track at a given time (relative to clip source_in).
/// Returns the interpolated value, or `default` if no keyframes exist.
pub fn evaluate(keyframes: &[Keyframe], time_secs: f64, default: f32) -> f32 {
    if keyframes.is_empty() {
        return default;
    }
    // Before first keyframe: use the first keyframe's value (holds backward)
    if time_secs <= keyframes[0].time_secs {
        return keyframes[0].value;
    }
    // After last keyframe: use last keyframe's value (holds)
    let last = &keyframes[keyframes.len() - 1];
    if time_secs >= last.time_secs {
        return last.value;
    }
    // Find surrounding keyframes and interpolate
    for i in 0..keyframes.len() - 1 {
        let a = &keyframes[i];
        let b = &keyframes[i + 1];
        if time_secs >= a.time_secs && time_secs <= b.time_secs {
            let t = if (b.time_secs - a.time_secs).abs() < 1e-9 {
                0.0
            } else {
                (time_secs - a.time_secs) / (b.time_secs - a.time_secs)
            };
            return match a.interpolation {
                Interpolation::Step => a.value,
                Interpolation::Linear => a.value + (b.value - a.value) * t as f32,
                Interpolation::Bezier => {
                    let p0 = a.value;
                    let p3 = b.value;
                    let p1 = p0 + a.tangent_out.1;
                    let p2 = p3 + b.tangent_in.1;
                    let t = t as f32;
                    let mt = 1.0 - t;
                    mt * mt * mt * p0
                        + 3.0 * mt * mt * t * p1
                        + 3.0 * mt * t * t * p2
                        + t * t * t * p3
                }
            };
        }
    }
    default
}

