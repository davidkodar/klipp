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
use klipp_core::ClipId;
use klipp_timeline::{ClipKind, Sequence};

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

/// Walk every track strictly *below* `source_clip_id`'s track and
/// return the clip-ids whose timeline range overlaps the ramp window
/// `[source.timeline_out - ramp_secs, source.timeline_out]`. The
/// returned clips are the "beneath the source during the ramp" set —
/// candidates for an auto-applied `transform_link` so they fly into
/// the focal point alongside the source.
///
/// Filters out:
///
/// * The source clip itself.
/// * Clips on the source's own track or any track above it (overlay-
///   above-source isn't "beneath" — those clips would land in the
///   user's manual-parent decision instead).
/// * `AdjustmentLayer` / `Gap` / nested `Sequence` clips — none have a
///   meaningful own transform to inherit one onto.
/// * Clips that already have a `transform_link` set — preserves the
///   user's explicit parenting choice. Re-running auto-link is then
///   idempotent.
/// * Clips whose timeline range doesn't overlap the ramp window —
///   they aren't on screen during the zoom, so linking them is a
///   no-op visually but still pollutes the data.
/// * Clips where binding `clip → source` would close a transform-link
///   cycle (defence-in-depth via the same 64-depth walker the
///   `SetClipTransformLink` handler uses; in normal hyperzoom usage
///   the source has no link of its own so cycles can't happen, but a
///   pathological project file shouldn't produce a runtime cycle).
///
/// Returns clip-ids in iteration order (track-major, then clip order
/// within each track) so callers can render any UI list deterministically.
pub fn collect_auto_link_targets(
    seq: &Sequence,
    source_clip_id: ClipId,
    ramp_secs: f64,
) -> Vec<ClipId> {
    // Locate the source clip + its track index. Bail if the source
    // is gone (stale dispatch).
    let mut source_track_idx: Option<usize> = None;
    let mut ramp_start_secs: f64 = 0.0;
    let mut ramp_end_secs: f64 = 0.0;
    for (ti, track) in seq.tracks.iter().enumerate() {
        if let Some(c) = track.clips.iter().find(|c| c.id == source_clip_id) {
            source_track_idx = Some(ti);
            let end = c.timeline_out.to_secs_f64();
            ramp_end_secs = end;
            ramp_start_secs = (end - ramp_secs.max(0.0)).max(0.0);
            break;
        }
    }
    let Some(src_ti) = source_track_idx else {
        return Vec::new();
    };

    let ramp_secs_safe = ramp_secs.max(0.0);
    if ramp_secs_safe < 1e-9 {
        // Zero-length ramp: no clip is "during" it. Don't pretend.
        return Vec::new();
    }

    let mut out = Vec::new();
    for (ti, track) in seq.tracks.iter().enumerate() {
        if ti >= src_ti {
            // Same-track and above-track clips skipped. The source
            // itself is on `src_ti` and its target was handled by the
            // hyperzoom Apply.
            continue;
        }
        for clip in &track.clips {
            if clip.id == source_clip_id {
                continue;
            }
            if matches!(
                clip.kind,
                ClipKind::AdjustmentLayer | ClipKind::Gap | ClipKind::Sequence { .. }
            ) {
                continue;
            }
            if clip.transform_link.is_some() {
                continue;
            }
            // Half-open overlap: a clip ending exactly at ramp_start
            // doesn't visually overlap the ramp (it's gone the frame
            // the ramp begins), and a clip starting exactly at
            // ramp_end is the next shot — handled by the hyperzoom
            // target side.
            let clip_in = clip.timeline_in.to_secs_f64();
            let clip_out = clip.timeline_out.to_secs_f64();
            let overlaps = clip_in < ramp_end_secs && clip_out > ramp_start_secs;
            if !overlaps {
                continue;
            }
            // Cycle-protection: would this binding close a
            // transform-link loop?
            if would_create_link_cycle(seq, clip.id, source_clip_id) {
                continue;
            }
            out.push(clip.id);
        }
    }
    out
}

/// Inline mirror of `app::transform_link_chain_creates_cycle` so the
/// helper crate doesn't need to reach across module boundaries for
/// the predicate. Same 64-link depth cap; same semantics. The two
/// copies are pinned by `transform_link_handler_tests` (in `app.rs`)
/// and the auto-link tests below.
fn would_create_link_cycle(seq: &Sequence, consumer: ClipId, target: ClipId) -> bool {
    if consumer == target {
        return true;
    }
    let mut current = target;
    for _ in 0..64 {
        let next = seq
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .find(|c| c.id == current)
            .and_then(|c| c.transform_link.as_ref().map(|tl| tl.target));
        match next {
            None => return false,
            Some(n) if n == consumer => return true,
            Some(n) => current = n,
        }
    }
    true
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

    // ──────────────── Auto-link target collection ────────────────

    use klipp_core::{MediaId, RationalTime};
    use klipp_timeline::{Clip, Track, TrackKind, TransformLink};

    fn tc(frames: i64) -> RationalTime { RationalTime::from_frames(frames, 24) }

    fn media_clip(timeline_in_f: i64, timeline_out_f: i64) -> Clip {
        // Source-time placeholders: irrelevant for auto-link, the
        // helper keys off timeline_in/timeline_out only.
        Clip::media(MediaId::new(), tc(timeline_in_f), tc(timeline_out_f), tc(0), tc(timeline_out_f - timeline_in_f))
    }

    fn track_with_clips(name: &str, clips: Vec<Clip>) -> Track {
        let mut t = Track::new(name, TrackKind::Video);
        t.clips = clips;
        t
    }

    fn seq_with_tracks(tracks: Vec<Track>) -> Sequence {
        let mut s = Sequence::new_default();
        // `new_default` already supplies Video 1 + Audio 1; replace them
        // wholesale so the test sequence has exactly the tracks the
        // test wants in the order it wants.
        s.tracks = tracks;
        s
    }

    #[test]
    fn auto_link_collects_overlapping_clip_on_lower_track() {
        // V1: shot A spans [0..120 frames @ 24 fps] = 5 s.
        // V2: source title spans [0..120] = 5 s.
        // Ramp: 0.5 s = window [4.5 s .. 5.0 s]. Shot A overlaps.
        let shot_a = media_clip(0, 120);
        let title = media_clip(0, 120);
        let title_id = title.id;
        let shot_a_id = shot_a.id;
        let seq = seq_with_tracks(vec![
            track_with_clips("V1", vec![shot_a]),
            track_with_clips("V2", vec![title]),
        ]);
        let targets = collect_auto_link_targets(&seq, title_id, 0.5);
        assert_eq!(targets, vec![shot_a_id]);
    }

    #[test]
    fn auto_link_skips_clips_above_source_track() {
        // V1: source title (lower).
        // V2: shot above the source (would render ON TOP — not "beneath").
        let title = media_clip(0, 120);
        let title_id = title.id;
        let shot_above = media_clip(0, 120);
        let seq = seq_with_tracks(vec![
            track_with_clips("V1", vec![title]),
            track_with_clips("V2", vec![shot_above]),
        ]);
        let targets = collect_auto_link_targets(&seq, title_id, 0.5);
        assert!(targets.is_empty());
    }

    #[test]
    fn auto_link_skips_clip_already_linked() {
        // Shot A on V1 has an existing transform_link (user manually
        // parented it earlier). Auto-link must not overwrite —
        // running it twice is then idempotent.
        let mut shot_a = media_clip(0, 120);
        shot_a.transform_link = Some(TransformLink::new(klipp_core::ClipId::new()));
        let title = media_clip(0, 120);
        let title_id = title.id;
        let seq = seq_with_tracks(vec![
            track_with_clips("V1", vec![shot_a]),
            track_with_clips("V2", vec![title]),
        ]);
        let targets = collect_auto_link_targets(&seq, title_id, 0.5);
        assert!(targets.is_empty());
    }

    #[test]
    fn auto_link_skips_non_overlapping_clip() {
        // Shot A ends at frame 60 (2.5 s). Source title is at
        // [60..120] (2.5..5 s). Ramp window = [4.5 s .. 5.0 s].
        // Shot A doesn't overlap the ramp window.
        let shot_a = media_clip(0, 60);
        let title = media_clip(60, 120);
        let title_id = title.id;
        let seq = seq_with_tracks(vec![
            track_with_clips("V1", vec![shot_a]),
            track_with_clips("V2", vec![title]),
        ]);
        let targets = collect_auto_link_targets(&seq, title_id, 0.5);
        assert!(targets.is_empty());
    }

    #[test]
    fn auto_link_includes_clip_that_starts_inside_ramp_window() {
        // Shot A starts at frame 110 (≈ 4.58 s) — inside the ramp
        // window of [4.5 .. 5.0]. Should be included even though it
        // covers only part of the ramp.
        let shot_a = media_clip(110, 130);
        let title = media_clip(0, 120);
        let title_id = title.id;
        let shot_a_id = shot_a.id;
        let seq = seq_with_tracks(vec![
            track_with_clips("V1", vec![shot_a]),
            track_with_clips("V2", vec![title]),
        ]);
        let targets = collect_auto_link_targets(&seq, title_id, 0.5);
        assert_eq!(targets, vec![shot_a_id]);
    }

    #[test]
    fn auto_link_skips_adjustment_layer_and_gap() {
        // Both AdjustmentLayer and Gap clip kinds have no own
        // transform — auto-linking either is a useless mutation.
        let mut adj = media_clip(0, 120);
        adj.kind = klipp_timeline::ClipKind::AdjustmentLayer;
        let mut gap = media_clip(0, 120);
        gap.kind = klipp_timeline::ClipKind::Gap;
        let title = media_clip(0, 120);
        let title_id = title.id;
        let seq = seq_with_tracks(vec![
            track_with_clips("V1", vec![adj]),
            track_with_clips("V2", vec![gap]),
            track_with_clips("V3", vec![title]),
        ]);
        let targets = collect_auto_link_targets(&seq, title_id, 0.5);
        assert!(targets.is_empty());
    }

    #[test]
    fn auto_link_skips_self_target_cycle() {
        // Edge case: source title already links → some clip on V1.
        // Auto-link tries to bind that V1 clip → source title; this
        // would close a cycle and must be rejected.
        let shot_a = media_clip(0, 120);
        let shot_a_id = shot_a.id;
        let mut title = media_clip(0, 120);
        title.transform_link = Some(TransformLink::new(shot_a_id));
        let title_id = title.id;
        let seq = seq_with_tracks(vec![
            track_with_clips("V1", vec![shot_a]),
            track_with_clips("V2", vec![title]),
        ]);
        let targets = collect_auto_link_targets(&seq, title_id, 0.5);
        assert!(targets.is_empty(), "rejecting bind would close source ↔ shot_a cycle");
    }

    #[test]
    fn auto_link_returns_empty_when_source_missing() {
        // Stale dispatch (clip removed between dispatch and handler).
        let title = media_clip(0, 120);
        let seq = seq_with_tracks(vec![track_with_clips("V1", vec![title])]);
        let targets = collect_auto_link_targets(&seq, klipp_core::ClipId::new(), 0.5);
        assert!(targets.is_empty());
    }

    #[test]
    fn auto_link_returns_empty_for_zero_ramp() {
        // Pathological ramp_secs (0 or negative). No clip is "during"
        // a zero-length window; the helper bails cleanly.
        let shot_a = media_clip(0, 120);
        let title = media_clip(0, 120);
        let title_id = title.id;
        let seq = seq_with_tracks(vec![
            track_with_clips("V1", vec![shot_a]),
            track_with_clips("V2", vec![title]),
        ]);
        assert!(collect_auto_link_targets(&seq, title_id, 0.0).is_empty());
        assert!(collect_auto_link_targets(&seq, title_id, -1.0).is_empty());
    }
}
