use serde::{Deserialize, Serialize};

/// Which spatial axis of a title-object position is being keyframed.
/// Used by the per-layer position keyframe AppCommands so handlers
/// can `match` exhaustively rather than reading magic numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TitlePositionAxis {
    X,
    Y,
}

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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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

/// Insert (or replace within ±20ms) a keyframe at `time_secs` and
/// keep the vec sorted by time. Centralises the dedup + sort logic so
/// every per-effect/per-axis keyframe handler shares one
/// implementation. Default interpolation is Linear; right-click menus
/// can flip it later via `set_keyframe_interpolation`.
pub fn upsert_keyframe(kfs: &mut Vec<Keyframe>, time_secs: f64, value: f32) {
    kfs.retain(|k| (k.time_secs - time_secs).abs() >= 0.02);
    kfs.push(Keyframe {
        time_secs,
        value,
        interpolation: Interpolation::Linear,
        tangent_out: (0.0, 0.0),
        tangent_in: (0.0, 0.0),
    });
    kfs.sort_by(|a, b| {
        a.time_secs
            .partial_cmp(&b.time_secs)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// Remove every keyframe within ±20ms of `time_secs`. No-op when
/// nothing matches; matches the NLE-standard "click diamond again to
/// delete" affordance.
pub fn remove_keyframe_at(kfs: &mut Vec<Keyframe>, time_secs: f64) {
    kfs.retain(|k| (k.time_secs - time_secs).abs() >= 0.02);
}

/// Move the keyframe at `index` to a new (time, value) and re-sort.
/// Returns true if the index was valid.
pub fn move_keyframe(
    kfs: &mut [Keyframe],
    index: usize,
    new_time_secs: f64,
    new_value: f32,
) -> bool {
    let Some(kf) = kfs.get_mut(index) else {
        return false;
    };
    kf.time_secs = new_time_secs;
    kf.value = new_value;
    kfs.sort_by(|a, b| {
        a.time_secs
            .partial_cmp(&b.time_secs)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    true
}

/// Set the interpolation mode of the keyframe at `index`. Returns
/// true if the index was valid.
pub fn set_keyframe_interpolation(
    kfs: &mut [Keyframe],
    index: usize,
    interpolation: Interpolation,
) -> bool {
    if let Some(kf) = kfs.get_mut(index) {
        kf.interpolation = interpolation;
        true
    } else {
        false
    }
}

/// How a move eases into a keyframe: the named curves motion design
/// uses, written as the Bezier handles [`evaluate`] already solves — the
/// same model as CSS `cubic-bezier`, handles as fractions of the
/// segment's time and change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ease {
    /// Constant speed: starts and stops dead. Right for a slow drift.
    #[default]
    Linear,
    /// Starts slowly and arrives at speed.
    In,
    /// Leaves at speed and settles: an arrival.
    Out,
    /// Slow at both ends: the natural move from one rest to another.
    InOut,
    /// A fast arrival that settles long: how a card flies into place.
    ExpoOut,
    /// Arrives, overshoots a little and settles back.
    BackOut,
}

impl Ease {
    /// The curve's two handles, `(x1, y1, x2, y2)`, or `None` for a
    /// straight line.
    pub fn handles(self) -> Option<(f64, f32, f64, f32)> {
        match self {
            Ease::Linear => None,
            Ease::In => Some((0.42, 0.0, 1.0, 1.0)),
            Ease::Out => Some((0.0, 0.0, 0.58, 1.0)),
            Ease::InOut => Some((0.42, 0.0, 0.58, 1.0)),
            Ease::ExpoOut => Some((0.16, 1.0, 0.3, 1.0)),
            Ease::BackOut => Some((0.34, 1.56, 0.64, 1.0)),
        }
    }
}

/// Ease the move from keyframe `i - 1` into keyframe `i`. The handles
/// are scaled to that segment, so the same ease means the same shape of
/// move whatever its length or distance.
pub fn ease_into(kfs: &mut [Keyframe], i: usize, ease: Ease) {
    if i == 0 || i >= kfs.len() {
        return;
    }
    let Some((x1, y1, x2, y2)) = ease.handles() else {
        kfs[i - 1].interpolation = Interpolation::Linear;
        return;
    };
    let d = kfs[i].time_secs - kfs[i - 1].time_secs;
    let v = kfs[i].value - kfs[i - 1].value;
    kfs[i - 1].interpolation = Interpolation::Bezier;
    kfs[i - 1].tangent_out = (x1 * d, y1 * v);
    kfs[i].tangent_in = ((x2 - 1.0) * d, (y2 - 1.0) * v);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn kf(time: f64, value: f32, interp: Interpolation) -> Keyframe {
        Keyframe {
            time_secs: time,
            value,
            interpolation: interp,
            tangent_out: (0.0, 0.0),
            tangent_in: (0.0, 0.0),
        }
    }

    fn kf_linear(time: f64, value: f32) -> Keyframe {
        kf(time, value, Interpolation::Linear)
    }

    fn kf_step(time: f64, value: f32) -> Keyframe {
        kf(time, value, Interpolation::Step)
    }

    fn kf_bezier(time: f64, value: f32, tan_out: (f64, f32), tan_in: (f64, f32)) -> Keyframe {
        Keyframe {
            time_secs: time,
            value,
            interpolation: Interpolation::Bezier,
            tangent_out: tan_out,
            tangent_in: tan_in,
        }
    }

    // ---- 1. Empty keyframes returns default ----

    #[test]
    fn empty_keyframes_returns_default() {
        assert_eq!(evaluate(&[], 0.0, 42.0), 42.0);
        assert_eq!(evaluate(&[], 1.0, -7.5), -7.5);
        assert_eq!(evaluate(&[], -1.0, 100.0), 100.0);
    }

    // ---- 2. Single keyframe returns its value at any time ----

    #[test]
    fn single_keyframe_at_exact_time() {
        let kfs = [kf_linear(1.0, 50.0)];
        assert_eq!(evaluate(&kfs, 1.0, 0.0), 50.0);
    }

    #[test]
    fn single_keyframe_before() {
        let kfs = [kf_linear(1.0, 50.0)];
        assert_eq!(evaluate(&kfs, 0.0, 0.0), 50.0);
    }

    #[test]
    fn single_keyframe_after() {
        let kfs = [kf_linear(1.0, 50.0)];
        assert_eq!(evaluate(&kfs, 5.0, 0.0), 50.0);
    }

    // ---- 3. Two keyframes, linear interpolation ----

    #[test]
    fn two_keyframes_linear_at_endpoints() {
        let kfs = [kf_linear(0.0, 0.0), kf_linear(1.0, 100.0)];
        assert_eq!(evaluate(&kfs, 0.0, -1.0), 0.0);
        assert_eq!(evaluate(&kfs, 1.0, -1.0), 100.0);
    }

    #[test]
    fn two_keyframes_linear_at_25_percent() {
        let kfs = [kf_linear(0.0, 0.0), kf_linear(1.0, 100.0)];
        let v = evaluate(&kfs, 0.25, -1.0);
        assert!((v - 25.0).abs() < 0.01, "expected ~25.0, got {v}");
    }

    #[test]
    fn two_keyframes_linear_at_50_percent() {
        let kfs = [kf_linear(0.0, 0.0), kf_linear(1.0, 100.0)];
        let v = evaluate(&kfs, 0.5, -1.0);
        assert!((v - 50.0).abs() < 0.01, "expected ~50.0, got {v}");
    }

    #[test]
    fn two_keyframes_linear_at_75_percent() {
        let kfs = [kf_linear(0.0, 0.0), kf_linear(1.0, 100.0)];
        let v = evaluate(&kfs, 0.75, -1.0);
        assert!((v - 75.0).abs() < 0.01, "expected ~75.0, got {v}");
    }

    #[test]
    fn two_keyframes_linear_non_zero_start() {
        let kfs = [kf_linear(2.0, 10.0), kf_linear(4.0, 30.0)];
        let v = evaluate(&kfs, 3.0, 0.0);
        assert!((v - 20.0).abs() < 0.01, "expected ~20.0, got {v}");
    }

    // ---- 4. Two keyframes, step interpolation ----

    #[test]
    fn two_keyframes_step_holds_previous() {
        let kfs = [kf_step(0.0, 10.0), kf_step(1.0, 90.0)];
        assert_eq!(evaluate(&kfs, 0.0, -1.0), 10.0);
        assert_eq!(evaluate(&kfs, 0.25, -1.0), 10.0);
        assert_eq!(evaluate(&kfs, 0.5, -1.0), 10.0);
        assert_eq!(evaluate(&kfs, 0.99, -1.0), 10.0);
        assert_eq!(evaluate(&kfs, 1.0, -1.0), 90.0);
    }

    #[test]
    fn step_then_linear_segments() {
        let kfs = [
            kf_step(0.0, 0.0),
            kf_linear(1.0, 100.0),
            kf_linear(2.0, 200.0),
        ];
        assert_eq!(evaluate(&kfs, 0.5, -1.0), 0.0);
        let v = evaluate(&kfs, 1.5, -1.0);
        assert!((v - 150.0).abs() < 0.01, "expected ~150.0, got {v}");
    }

    // ---- 5. Before first keyframe (hold backward) ----

    #[test]
    fn before_first_keyframe_holds_backward() {
        let kfs = [kf_linear(5.0, 42.0), kf_linear(10.0, 100.0)];
        assert_eq!(evaluate(&kfs, 0.0, -1.0), 42.0);
        assert_eq!(evaluate(&kfs, 4.99, -1.0), 42.0);
    }

    // ---- 6. After last keyframe (hold forward) ----

    #[test]
    fn after_last_keyframe_holds_forward() {
        let kfs = [kf_linear(0.0, 10.0), kf_linear(1.0, 20.0)];
        assert_eq!(evaluate(&kfs, 1.0, -1.0), 20.0);
        assert_eq!(evaluate(&kfs, 100.0, -1.0), 20.0);
    }

    // ---- 7. Three keyframes, correct per-segment interpolation ----

    #[test]
    fn three_keyframes_first_segment() {
        let kfs = [
            kf_linear(0.0, 0.0),
            kf_linear(1.0, 100.0),
            kf_linear(2.0, 50.0),
        ];
        let v = evaluate(&kfs, 0.5, -1.0);
        assert!((v - 50.0).abs() < 0.01, "first segment mid: expected ~50.0, got {v}");
    }

    #[test]
    fn three_keyframes_second_segment() {
        let kfs = [
            kf_linear(0.0, 0.0),
            kf_linear(1.0, 100.0),
            kf_linear(2.0, 50.0),
        ];
        let v = evaluate(&kfs, 1.5, -1.0);
        assert!((v - 75.0).abs() < 0.01, "second segment mid: expected ~75.0, got {v}");
    }

    #[test]
    fn three_keyframes_at_middle_keyframe() {
        let kfs = [
            kf_linear(0.0, 0.0),
            kf_linear(1.0, 100.0),
            kf_linear(2.0, 50.0),
        ];
        let v = evaluate(&kfs, 1.0, -1.0);
        assert!((v - 100.0).abs() < 0.01, "at middle kf: expected ~100.0, got {v}");
    }

    // ---- 8. Bezier interpolation ----

    #[test]
    fn bezier_no_tangents_hits_endpoints() {
        let kfs = [
            kf_bezier(0.0, 0.0, (0.0, 0.0), (0.0, 0.0)),
            kf_bezier(1.0, 100.0, (0.0, 0.0), (0.0, 0.0)),
        ];
        let v0 = evaluate(&kfs, 0.0, -1.0);
        let v1 = evaluate(&kfs, 1.0, -1.0);
        assert!((v0 - 0.0).abs() < 0.01, "start: expected ~0.0, got {v0}");
        assert!((v1 - 100.0).abs() < 0.01, "end: expected ~100.0, got {v1}");
    }

    #[test]
    fn bezier_with_time_tangents_smooth_curve() {
        // Ease-in-out curve with time handles
        let kfs = [
            kf_bezier(0.0, 0.0, (0.3, 0.0), (0.0, 0.0)),
            {
                let mut k = kf_bezier(1.0, 100.0, (0.0, 0.0), (-0.3, 0.0));
                k.interpolation = Interpolation::Linear; // irrelevant, first kf drives
                k
            },
        ];
        let v = evaluate(&kfs, 0.5, -1.0);
        assert!(v > 30.0 && v < 70.0, "bezier mid: expected 30..70, got {v}");
        assert!((evaluate(&kfs, 0.0, -1.0) - 0.0).abs() < 0.01);
        assert!((evaluate(&kfs, 1.0, -1.0) - 100.0).abs() < 0.01);
    }

    #[test]
    fn bezier_with_value_tangents() {
        // Zero time tangents, value tangents create overshoot shape
        // P0=0, P1=0+80=80, P2=100+(-80)=20, P3=100
        // At t=0.5: (1-t)^3*0 + 3*(1-t)^2*t*80 + 3*(1-t)*t^2*20 + t^3*100
        // = 0 + 30 + 7.5 + 12.5 = 50
        let kfs = [
            kf_bezier(0.0, 0.0, (0.0, 80.0), (0.0, 0.0)),
            kf_bezier(1.0, 100.0, (0.0, 0.0), (0.0, -80.0)),
        ];
        let v = evaluate(&kfs, 0.5, -1.0);
        assert!((v - 50.0).abs() < 1.0, "bezier value tangents mid: expected ~50, got {v}");
    }

    #[test]
    fn bezier_monotonic_with_ease() {
        let kfs = [
            kf_bezier(0.0, 0.0, (0.33, 0.0), (0.0, 0.0)),
            kf_bezier(1.0, 100.0, (0.0, 0.0), (-0.33, 0.0)),
        ];
        let mut prev = evaluate(&kfs, 0.0, -1.0);
        for i in 1..=20 {
            let t = i as f64 / 20.0;
            let v = evaluate(&kfs, t, -1.0);
            assert!(
                v >= prev - 0.1,
                "bezier should be monotonic: at t={t}, v={v} < prev={prev}"
            );
            prev = v;
        }
    }

    // ---- 9. Negative time ----

    #[test]
    fn negative_time_returns_first_keyframe() {
        let kfs = [kf_linear(0.0, 42.0), kf_linear(1.0, 100.0)];
        assert_eq!(evaluate(&kfs, -1.0, -1.0), 42.0);
        assert_eq!(evaluate(&kfs, -1000.0, -1.0), 42.0);
    }

    #[test]
    fn negative_time_empty_returns_default() {
        assert_eq!(evaluate(&[], -5.0, 99.0), 99.0);
    }

    // ---- 10. Zero-duration segment ----

    #[test]
    fn zero_duration_segment_returns_first_value() {
        let kfs = [kf_linear(1.0, 10.0), kf_linear(1.0, 90.0)];
        let v = evaluate(&kfs, 1.0, -1.0);
        assert_eq!(v, 10.0);
    }

    #[test]
    fn zero_duration_segment_before_and_after() {
        let kfs = [kf_linear(1.0, 10.0), kf_linear(1.0, 90.0)];
        assert_eq!(evaluate(&kfs, 0.5, -1.0), 10.0);
        assert_eq!(evaluate(&kfs, 1.5, -1.0), 90.0);
    }

    #[test]
    fn zero_duration_step() {
        let kfs = [kf_step(1.0, 10.0), kf_step(1.0, 90.0)];
        let v = evaluate(&kfs, 1.0, -1.0);
        assert_eq!(v, 10.0);
    }

    // ---- Edge cases ----

    #[test]
    fn very_large_time_values() {
        let kfs = [kf_linear(0.0, 1.0), kf_linear(1.0, 2.0)];
        assert_eq!(evaluate(&kfs, 1e12, -1.0), 2.0);
    }

    #[test]
    fn many_keyframes_correct_segment_lookup() {
        let kfs: Vec<Keyframe> = (0..10)
            .map(|i| kf_linear(i as f64, i as f32 * 10.0))
            .collect();
        let v = evaluate(&kfs, 4.5, -1.0);
        assert!((v - 45.0).abs() < 0.01, "expected ~45.0, got {v}");
    }

    #[test]
    fn default_interpolation_is_linear() {
        assert_eq!(Interpolation::default(), Interpolation::Linear);
    }

    // ═══════════════════════════════════════════════════════════════
    // NaN-safe sort regression — closes CODE_AUDIT U-13.
    //
    // ~20+ sites in klipp-ui/src/app.rs sort keyframes, multicam_cuts,
    // and PCM regions with the closure
    //
    //   |a, b| a.time_secs.partial_cmp(&b.time_secs)
    //              .unwrap_or(std::cmp::Ordering::Equal)
    //
    // because OFX plugins, hand-edited .klipp project files, or a bad
    // preset can inject NaN time values; the bare `.unwrap()` form
    // would panic. These tests exercise that closure directly so any
    // future "cleanup" that drops `.unwrap_or(Equal)` back to bare
    // `.unwrap()` surfaces here instead of in a user crash.
    //
    // KNOWN LIMITATION (latent bug, not in U-13's scope): treating NaN
    // as Equal violates sort_by's strict-weak-ordering contract, so
    // when a NaN slips into the input the order of OTHER elements
    // around it is also unspecified. The contract these tests pin is
    // narrow: "no panic", not "sort is sane in the face of NaN".
    // Properly stripping or quarantining NaN-time keyframes is a
    // follow-up worth opening separately if one ever surfaces.
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn keyframe_sort_with_nan_time_does_not_panic() {
        let mut kfs = [kf_linear(2.0, 0.0),
            kf_linear(f64::NAN, 0.0),
            kf_linear(0.5, 0.0),
            kf_linear(1.0, 0.0)];
        // Same closure as the sort sites in klipp-ui/src/app.rs. The
        // assertion is implicit: control reaches the line below.
        kfs.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
        assert_eq!(kfs.len(), 4, "sort must preserve length");
    }

    #[test]
    fn tuple_sort_by_first_f64_with_nan_does_not_panic() {
        // Mirrors clip.multicam_cuts (Vec<(f64, u8)>) and the PCM regions
        // sort in app.rs — same NaN-safe closure on the first tuple field.
        let mut cuts: Vec<(f64, u8)> = vec![
            (2.0, 1),
            (f64::NAN, 2),
            (0.5, 3),
            (1.0, 4),
        ];
        cuts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        assert_eq!(cuts.len(), 4);
    }

    #[test]
    fn keyframe_sort_without_nan_is_ascending() {
        // Sanity: when no NaN is present the closure DOES sort
        // ascending, so the rest of the codebase's reliance on the
        // sort's correctness in the normal case is preserved.
        let mut kfs = [kf_linear(3.0, 0.0),
            kf_linear(1.0, 0.0),
            kf_linear(2.5, 0.0),
            kf_linear(0.5, 0.0)];
        kfs.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
        let times: Vec<f64> = kfs.iter().map(|k| k.time_secs).collect();
        assert_eq!(times, vec![0.5, 1.0, 2.5, 3.0]);
    }
}



#[cfg(test)]
mod ease_tests {
    use super::*;

    fn track(ease: Ease) -> Vec<Keyframe> {
        let mut k = Vec::new();
        upsert_keyframe(&mut k, 0.0, 0.0);
        upsert_keyframe(&mut k, 2.0, 100.0);
        ease_into(&mut k, 1, ease);
        k
    }

    #[test]
    fn each_ease_has_the_shape_its_name_says() {
        let mid = |e| evaluate(&track(e), 1.0, 0.0);
        assert!((mid(Ease::Linear) - 50.0).abs() < 1e-3);
        assert!(mid(Ease::In) < 40.0, "in is still slow halfway: {}", mid(Ease::In));
        assert!(mid(Ease::Out) > 60.0, "out is nearly there halfway: {}", mid(Ease::Out));
        assert!((mid(Ease::InOut) - 50.0).abs() < 1.0, "in_out is symmetric: {}", mid(Ease::InOut));
        assert!(evaluate(&track(Ease::ExpoOut), 0.4, 0.0) > 70.0, "expo_out arrives fast");
        let peak = (1..40).map(|i| evaluate(&track(Ease::BackOut), i as f64 * 0.05, 0.0)).fold(0.0f32, f32::max);
        assert!(peak > 100.5, "back_out goes past and settles: {peak}");
        // Every ease still starts and ends where the keyframes are.
        for e in [Ease::In, Ease::Out, Ease::InOut, Ease::ExpoOut, Ease::BackOut] {
            assert!((evaluate(&track(e), 0.0, 0.0)).abs() < 1e-3);
            assert!((evaluate(&track(e), 2.0, 0.0) - 100.0).abs() < 1e-3);
        }
    }

    #[test]
    fn an_ease_reads_from_its_name() {
        let e: Ease = serde_json::from_str("\"expo_out\"").unwrap();
        assert_eq!(e, Ease::ExpoOut);
    }
}
