//! Hyperzoom transition preset planning.
//!
//! Pure-math helper that turns user-supplied focal point + duration +
//! peak scale into the per-clip Transform anchor and scale-keyframe set
//! that produces a "fly into the focal point" transition. The handler
//! in `app.rs` consumes the plan and writes it to the source and target
//! clips through the existing transform-keyframe machinery —
//! hyperzoom is a *preset*, not a new effect type, so the result is
//! fully editable by the user in the keyframe editor afterwards.
//!
//! Easing is currently `Linear`; the keyframe editor surfaces Bezier
//! handles per-keyframe so the user can refine the ramp by hand. A
//! future enhancement could emit Bezier tangents up-front for a more
//! professional default feel.
//!
//! The visual letter-picker (click a glyph in the title to set the
//! focal point) is also a deferred follow-up; today the handler takes
//! normalised X/Y in `0.0..=1.0` from the inspector spinboxes, with
//! `(0.5, 0.5)` defaulting to the centre of the source clip.

use klipp_core::keyframe::{Interpolation, Keyframe};

/// Inputs to [`plan_hyperzoom`]. Durations are clip-local seconds;
/// focal point is normalised screen coords (`0.0..=1.0`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HyperzoomInput {
    pub source_clip_dur_secs: f64,
    pub target_clip_dur_secs: f64,
    pub focal_x_norm: f32,
    pub focal_y_norm: f32,
    pub duration_secs: f64,
    pub max_scale: f32,
}

/// Output of [`plan_hyperzoom`]. Keyframe times are clip-local seconds
/// (relative to `source_in`, same convention as the rest of the
/// transform-keyframe machinery).
#[derive(Debug, Clone, PartialEq)]
pub struct HyperzoomPlan {
    pub source_anchor: (f32, f32),
    pub source_scale_kfs: Vec<Keyframe>,
    pub target_anchor: (f32, f32),
    pub target_scale_kfs: Vec<Keyframe>,
}

/// Compute the keyframes + anchor for a hyperzoom transition between
/// two clips. All inputs are clamped to safe ranges so a UI bug or a
/// stale palette dispatch can never push impossible values into clip
/// data:
///
/// * `focal_x_norm` / `focal_y_norm` clamp to `0.0..=1.0`
/// * `max_scale` clamps to `>= 1.01` (a peak below 1× wouldn't zoom)
/// * `duration_secs` clamps to `>= 0.05` (50 ms — anything shorter
///   would land both keyframes on top of each other and `upsert` would
///   collapse them)
/// * Short clips (shorter than `duration_secs`) ramp from time 0
///   instead of going negative.
pub fn plan_hyperzoom(input: HyperzoomInput) -> HyperzoomPlan {
    let fx = input.focal_x_norm.clamp(0.0, 1.0);
    let fy = input.focal_y_norm.clamp(0.0, 1.0);
    let max_scale = input.max_scale.max(1.01);
    let dur = input.duration_secs.max(0.05);

    // Source: scale 1.0 → max_scale across the last `dur` seconds.
    // When the source clip is shorter than the requested duration we
    // ramp from t=0 instead — the user gets less ramp but still gets
    // two distinct keyframes that `upsert` won't collapse.
    let src_ramp_start = (input.source_clip_dur_secs - dur).max(0.0);
    let src_ramp_end = input
        .source_clip_dur_secs
        .max(src_ramp_start + 0.05);
    let source_scale_kfs = vec![
        keyframe(src_ramp_start, 1.0),
        keyframe(src_ramp_end, max_scale),
    ];

    // Target: scale max_scale → 1.0 across the first `dur` seconds.
    // Cap at the target's duration so a keyframe never lands past the
    // clip's end.
    let tgt_ramp_end = dur.min(input.target_clip_dur_secs).max(0.05);
    let target_scale_kfs = vec![
        keyframe(0.0, max_scale),
        keyframe(tgt_ramp_end, 1.0),
    ];

    HyperzoomPlan {
        source_anchor: (fx, fy),
        source_scale_kfs,
        target_anchor: (fx, fy),
        target_scale_kfs,
    }
}

fn keyframe(time_secs: f64, value: f32) -> Keyframe {
    Keyframe {
        time_secs,
        value,
        interpolation: Interpolation::Linear,
        tangent_in: (0.0, 0.0),
        tangent_out: (0.0, 0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(focal_x: f32, focal_y: f32, dur: f64, max: f32, src: f64, tgt: f64) -> HyperzoomInput {
        HyperzoomInput {
            source_clip_dur_secs: src,
            target_clip_dur_secs: tgt,
            focal_x_norm: focal_x,
            focal_y_norm: focal_y,
            duration_secs: dur,
            max_scale: max,
        }
    }

    #[test]
    fn plan_centre_focal_point_matches_inputs() {
        let plan = plan_hyperzoom(input(0.5, 0.5, 0.5, 50.0, 3.0, 3.0));
        assert_eq!(plan.source_anchor, (0.5, 0.5));
        assert_eq!(plan.target_anchor, (0.5, 0.5));
    }

    #[test]
    fn plan_source_ramps_from_one_to_max_at_clip_end() {
        // 3 s source, 0.5 s ramp → kf at t=2.5, value=1.0; kf at t=3.0, value=max.
        let plan = plan_hyperzoom(input(0.5, 0.5, 0.5, 50.0, 3.0, 3.0));
        let kfs = &plan.source_scale_kfs;
        assert_eq!(kfs.len(), 2);
        assert!((kfs[0].time_secs - 2.5).abs() < 1e-9);
        assert!((kfs[0].value - 1.0).abs() < 1e-6);
        assert!((kfs[1].time_secs - 3.0).abs() < 1e-9);
        assert!((kfs[1].value - 50.0).abs() < 1e-6);
    }

    #[test]
    fn plan_target_ramps_from_max_to_one_starting_at_zero() {
        // 3 s target, 0.5 s ramp → kf at t=0, value=max; kf at t=0.5, value=1.0.
        let plan = plan_hyperzoom(input(0.5, 0.5, 0.5, 50.0, 3.0, 3.0));
        let kfs = &plan.target_scale_kfs;
        assert_eq!(kfs.len(), 2);
        assert!((kfs[0].time_secs - 0.0).abs() < 1e-9);
        assert!((kfs[0].value - 50.0).abs() < 1e-6);
        assert!((kfs[1].time_secs - 0.5).abs() < 1e-9);
        assert!((kfs[1].value - 1.0).abs() < 1e-6);
    }

    #[test]
    fn plan_clamps_focal_point_to_unit_range() {
        let plan = plan_hyperzoom(input(-0.5, 1.5, 0.5, 50.0, 3.0, 3.0));
        assert_eq!(plan.source_anchor, (0.0, 1.0));
        assert_eq!(plan.target_anchor, (0.0, 1.0));
    }

    #[test]
    fn plan_clamps_max_scale_to_floor() {
        // max < 1.0 would mean "zoom out" — guard against it; floor
        // is 1.01 so the keyframes still differ.
        let plan = plan_hyperzoom(input(0.5, 0.5, 0.5, 0.5, 3.0, 3.0));
        let last_src = plan.source_scale_kfs.last().unwrap();
        assert!(last_src.value >= 1.01);
    }

    #[test]
    fn plan_short_source_does_not_produce_negative_ramp_start() {
        // Source is 0.2 s but duration_secs = 0.5 — ramp must start at
        // t=0, not t=-0.3.
        let plan = plan_hyperzoom(input(0.5, 0.5, 0.5, 50.0, 0.2, 3.0));
        let first_src = &plan.source_scale_kfs[0];
        assert!(first_src.time_secs >= 0.0);
    }

    #[test]
    fn plan_emits_distinct_keyframe_times() {
        // Sanity: the upsert dedup window is 20 ms. Two keyframes
        // closer than that would collapse into one. Default 0.5 s
        // duration is well above the threshold, but check that the
        // duration-clamp floor (50 ms) still produces a useful gap.
        let plan = plan_hyperzoom(input(0.5, 0.5, 0.0, 50.0, 3.0, 3.0));
        let src = &plan.source_scale_kfs;
        let tgt = &plan.target_scale_kfs;
        assert!((src[1].time_secs - src[0].time_secs).abs() > 0.02);
        assert!((tgt[1].time_secs - tgt[0].time_secs).abs() > 0.02);
    }
}
