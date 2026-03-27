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
                    let duration = b.time_secs - a.time_secs;

                    // Value control points (Y dimension)
                    let y0 = a.value;
                    let y3 = b.value;
                    let y1 = y0 + a.tangent_out.1;
                    let y2 = y3 + b.tangent_in.1;

                    // Check if tangent time offsets are effectively zero —
                    // if so, use the simple linear-t evaluation (no Newton-Raphson needed).
                    let tx_out = a.tangent_out.0;
                    let tx_in = b.tangent_in.0;
                    let has_time_tangents = duration.abs() > 1e-9
                        && (tx_out.abs() > 1e-9 || tx_in.abs() > 1e-9);

                    if !has_time_tangents {
                        // Simple cubic Bezier with linear t
                        let t = t as f32;
                        let mt = 1.0 - t;
                        mt * mt * mt * y0
                            + 3.0 * mt * mt * t * y1
                            + 3.0 * mt * t * t * y2
                            + t * t * t * y3
                    } else {
                        // Full 2D Bezier: solve for parameter u where bezier_x(u) = t_linear,
                        // then evaluate bezier_y(u).
                        //
                        // Normalized time control points (X dimension, 0..1 range):
                        // P0.x = 0, P3.x = 1
                        // P1.x = tangent_out.0 / duration
                        // P2.x = 1 + tangent_in.0 / duration  (tangent_in.0 is negative)
                        let x1 = (tx_out / duration).clamp(0.0, 1.0);
                        let x2 = (1.0 + tx_in / duration).clamp(0.0, 1.0);

                        let t_target = t; // already normalized 0..1

                        // Cubic Bezier X(u) = 3(1-u)^2 u x1 + 3(1-u) u^2 x2 + u^3
                        // (since x0=0 and x3=1)
                        // Derivative X'(u) = 3(1-u)^2 x1 + 6(1-u)u(x2-x1) + 3u^2(1-x2)
                        let bezier_x = |u: f64| -> f64 {
                            let mu = 1.0 - u;
                            3.0 * mu * mu * u * x1
                                + 3.0 * mu * u * u * x2
                                + u * u * u
                        };
                        let bezier_x_deriv = |u: f64| -> f64 {
                            let mu = 1.0 - u;
                            3.0 * mu * mu * x1
                                + 6.0 * mu * u * (x2 - x1)
                                + 3.0 * u * u * (1.0 - x2)
                        };

                        // Newton-Raphson: find u where bezier_x(u) = t_target
                        let mut u = t_target; // initial guess
                        for _ in 0..8 {
                            let x_val = bezier_x(u) - t_target;
                            let x_deriv = bezier_x_deriv(u);
                            if x_deriv.abs() < 1e-12 {
                                break;
                            }
                            u -= x_val / x_deriv;
                            u = u.clamp(0.0, 1.0);
                            // Early exit if close enough
                            if (bezier_x(u) - t_target).abs() < 1e-7 {
                                break;
                            }
                        }
                        u = u.clamp(0.0, 1.0);

                        // Evaluate Y (value) at parameter u
                        let u = u as f32;
                        let mu = 1.0 - u;
                        mu * mu * mu * y0
                            + 3.0 * mu * mu * u * y1
                            + 3.0 * mu * u * u * y2
                            + u * u * u * y3
                    }
                }
            };
        }
    }
    default
}

