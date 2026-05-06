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

/// Per-clip persistent state for the HYPERZOOM inspector section AND
/// the on-canvas focal-point gizmo. Lives in egui's `Memory` keyed by
/// clip-id (see [`hyperzoom_scratch_id`]) so the inspector spinboxes
/// and the viewer crosshair stay in sync without round-tripping
/// through `AppState`. Both `HyperzoomScratch` and
/// `HyperzoomTargetChoice` are `Copy` so reading from `Memory` is a
/// trivial value clone.
#[derive(Debug, Clone, Copy)]
pub struct HyperzoomScratch {
    pub focal_x: f32,
    pub focal_y: f32,
    pub duration_secs: f64,
    pub max_scale: f32,
    pub target_choice: HyperzoomTargetChoice,
    pub easing: HyperzoomEasing,
    pub hold_secs: f64,
    pub add_radial_blur: bool,
    pub radial_blur_amount: f32,
}

impl Default for HyperzoomScratch {
    fn default() -> Self {
        Self {
            focal_x: 0.5,
            focal_y: 0.5,
            duration_secs: 0.5,
            max_scale: 50.0,
            target_choice: HyperzoomTargetChoice::Auto,
            easing: HyperzoomEasing::Linear,
            hold_secs: 0.0,
            add_radial_blur: false,
            // Match the "Zoom Blur" preset baseline so toggling on
            // gives a punchy default — but only fires when the user
            // actually flips the checkbox.
            radial_blur_amount: 15.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HyperzoomTargetChoice {
    /// "Use whatever clip follows this one on the same track." The
    /// `ApplyHyperzoom` handler resolves this at apply time, so the
    /// user can change track contents between configuring and
    /// applying without the inspector going stale.
    Auto,
    /// Explicit pick — the handler will reject if the clip is gone
    /// at apply time.
    Explicit(ClipId),
}

/// Shared egui-`Id` for the per-clip `HyperzoomScratch` slot. Both
/// the inspector and the viewer call this so they read/write the
/// same memory cell — never compute the id ad hoc anywhere else, or
/// the two surfaces will silently desync.
pub fn hyperzoom_scratch_id(clip_id: ClipId) -> egui::Id {
    egui::Id::new(("video:hyperzoom_state", clip_id))
}

/// Read this clip's scratch from egui memory. Returns the default
/// (centre focal point, 0.5 s, 50×, auto target) when no value is
/// stored — that's the same starting point the inspector spinboxes
/// fall back to on first paint, so reading from either side yields
/// the same picture before the user has touched anything.
pub fn read_hyperzoom_scratch(ctx: &egui::Context, clip_id: ClipId) -> HyperzoomScratch {
    ctx.data_mut(|d| d.get_temp(hyperzoom_scratch_id(clip_id)).unwrap_or_default())
}

/// Mutate this clip's scratch in place. Pass a closure so the read /
/// modify / write happens under a single `data_mut` lock — avoids
/// the racy two-call pattern (`read` + later `insert`) when multiple
/// surfaces edit different fields in the same frame.
pub fn update_hyperzoom_scratch(ctx: &egui::Context, clip_id: ClipId, edit: impl FnOnce(&mut HyperzoomScratch)) {
    ctx.data_mut(|d| {
        let id = hyperzoom_scratch_id(clip_id);
        let mut s: HyperzoomScratch = d.get_temp(id).unwrap_or_default();
        edit(&mut s);
        d.insert_temp(id, s);
    });
}

/// Easing curve emitted by [`plan_hyperzoom`] across the ramp window.
///
/// * [`Self::Linear`] — straight line between keyframes. Default. The
///   pre-easing-feature behaviour; existing projects + re-applies
///   stay byte-identical.
/// * [`Self::Bezier`] — symmetric S-curve via Bezier tangents at
///   `±duration/3`. Slow start, accelerate through the middle, slow
///   finish. Reads as the cinematic "rocket" feel on both source
///   ramp-in and target ramp-out.
///
/// The user picks the choice in the inspector's HYPERZOOM section.
/// Always opt-in — Apply Hyperzoom never silently flips a project's
/// existing ramps from Linear to Bezier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HyperzoomEasing {
    #[default]
    Linear,
    Bezier,
}

impl HyperzoomEasing {
    /// Wire-format tag used by [`klipp_core::AppCommand::ApplyHyperzoom`].
    /// 0 = Linear, 1 = Bezier. Mirrors the LinkMode-on-AppCommand
    /// pattern so we don't pull `klipp-ui` types into `klipp-core`.
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Linear => 0,
            Self::Bezier => 1,
        }
    }

    /// Inverse of [`Self::as_u8`]; unknown values fall back to
    /// `Linear` (the safe default — never silently turns on Bezier).
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Bezier,
            _ => Self::Linear,
        }
    }
}

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
    /// Easing across the ramp. Defaults to `Linear` for back-compat;
    /// flip to `Bezier` for an S-curve via the inspector.
    pub easing: HyperzoomEasing,
    /// Optional flat-at-peak segment straddling the cut. The source
    /// holds its last `hold_secs` at peak; the target holds its
    /// first `hold_secs` at peak — total perceived "frozen at peak"
    /// duration is `2 * hold_secs`. Set to `0.0` for the original
    /// two-keyframe ramp.
    pub hold_secs: f64,
    /// When `true`, the planner additionally emits radial-blur amount
    /// keyframes alongside the scale ramp. Defaults to `false` for
    /// back-compat — the standalone `RadialBlur` effect is always
    /// available from the effects library; this is just the
    /// convenience flag that wires it through Apply Hyperzoom.
    pub add_radial_blur: bool,
    /// Peak radial-blur amount (passed to `BuiltinEffect::RadialBlur`'s
    /// `amount` field). Same shape as the scale ramp: source ramps
    /// `0.0 → amount` over the source ramp; target ramps `amount → 0.0`
    /// over the target ramp; held segments stay at peak.
    pub radial_blur_amount: f32,
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
    /// Radial-blur center + per-side amount keyframes when
    /// `HyperzoomInput::add_radial_blur` was true. `None` otherwise so
    /// the handler can skip the radial-blur insertion entirely on
    /// the back-compat path.
    pub radial_blur: Option<RadialBlurPlan>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RadialBlurPlan {
    /// Normalised screen coords — same value as the focal point.
    pub center: (f32, f32),
    /// Peak amount the ramp climbs to (mirrors the input field).
    pub peak_amount: f32,
    /// `BuiltinEffect::RadialBlur::amount` keyframes for the source
    /// clip. Same time-shape as the scale ramp: ramps `0.0 → amount`
    /// over the source ramp window; under hold > 0 a 3rd keyframe
    /// pins the held segment at peak.
    pub source_amount_kfs: Vec<Keyframe>,
    /// Mirror of `source_amount_kfs` for the target side.
    pub target_amount_kfs: Vec<Keyframe>,
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
    let hold = input.hold_secs.max(0.0);
    let easing = input.easing;

    // Source side. Anatomy of the timeline (clip-local seconds):
    //
    //     ramp_start → … (ease) … → peak_kf_time → … (flat) … → clip_end
    //
    // * Without hold (`hold == 0`): peak_kf_time == clip_end, no flat
    //   tail — the plan emits 2 keyframes (back-compat with the
    //   pre-hold-feature output).
    // * With hold (`hold > 0`): the last `hold` seconds of the source
    //   sit at peak, so the cut into the target lands inside a held-
    //   peak window rather than at the apex of the ramp.
    let peak_src = (input.source_clip_dur_secs - hold).max(0.0);
    let src_ramp_start = (peak_src - dur).max(0.0);
    let peak_src = peak_src.max(src_ramp_start + 0.05);
    let source_scale_kfs = build_ramp_keyframes(
        (src_ramp_start, 1.0),
        (peak_src, max_scale),
        easing,
        if hold > 0.0 {
            // Tail-hold: extra flat-at-peak keyframe at clip_end.
            // `>= peak_src + 0.05` so upsert's 20 ms dedup window
            // doesn't fold the two peak keyframes into one.
            Some(input.source_clip_dur_secs.max(peak_src + 0.05))
        } else {
            None
        },
    );

    // Target side, mirror anatomy:
    //
    //     0 → … (flat) … → ramp_start → … (ease) … → ramp_end
    //
    // The hold occupies the head of the target so the held-peak
    // window straddles the cut visually.
    let tgt_ramp_start = hold;
    let tgt_ramp_end = (tgt_ramp_start + dur).min(input.target_clip_dur_secs.max(tgt_ramp_start + dur));
    let tgt_ramp_end = tgt_ramp_end.max(tgt_ramp_start + 0.05);
    let target_scale_kfs = build_ramp_keyframes_target(
        if hold > 0.0 { Some(0.0) } else { None },
        (tgt_ramp_start, max_scale),
        (tgt_ramp_end, 1.0),
        easing,
    );

    // Radial-blur amount keyframes share the source/target ramp
    // window with the scale animation, but the values run 0.0 →
    // peak instead of 1.0 → max_scale. Reusing the same
    // build_ramp_keyframes helpers keeps the time math identical
    // by construction — there's exactly one place where the
    // ramp_start / peak / hold-tail times are computed.
    let radial_blur = if input.add_radial_blur && input.radial_blur_amount > 0.0 {
        let amount = input.radial_blur_amount.max(0.0);
        let src_amount_kfs = build_ramp_keyframes(
            (src_ramp_start, 0.0),
            (peak_src, amount),
            easing,
            if hold > 0.0 {
                Some(input.source_clip_dur_secs.max(peak_src + 0.05))
            } else {
                None
            },
        );
        let tgt_amount_kfs = build_ramp_keyframes_target(
            if hold > 0.0 { Some(0.0) } else { None },
            (tgt_ramp_start, amount),
            (tgt_ramp_end, 0.0),
            easing,
        );
        Some(RadialBlurPlan {
            center: (fx, fy),
            peak_amount: amount,
            source_amount_kfs: src_amount_kfs,
            target_amount_kfs: tgt_amount_kfs,
        })
    } else {
        None
    };

    HyperzoomPlan {
        source_anchor: (fx, fy),
        source_scale_kfs,
        target_anchor: (fx, fy),
        target_scale_kfs,
        radial_blur,
    }
}

/// Build the source-side keyframe sequence:
///
/// * `ramp_start` → `peak`: easing-shaped ramp.
/// * Optional `tail_hold_at` keyframe pinned at the peak value to
///   extend a flat segment after the ramp completes.
///
/// Bezier easing emits an S-curve via symmetric `±dur/3` horizontal
/// tangents on the ramp endpoints. The tail-hold keyframe (when
/// present) is always Linear with zero tangents so the held segment
/// is dead flat — a Bezier flat segment would be redundant and could
/// produce visual jitter at sub-millisecond integration steps.
fn build_ramp_keyframes(
    ramp_start: (f64, f32),
    peak: (f64, f32),
    easing: HyperzoomEasing,
    tail_hold_at: Option<f64>,
) -> Vec<Keyframe> {
    let segment_dur = (peak.0 - ramp_start.0).max(1e-6);
    let mut out = Vec::with_capacity(3);
    out.push(ramp_endpoint_keyframe(ramp_start.0, ramp_start.1, easing, segment_dur, RampEndpoint::Start));
    out.push(ramp_endpoint_keyframe(peak.0, peak.1, easing, segment_dur, RampEndpoint::End));
    if let Some(t) = tail_hold_at {
        out.push(keyframe(t, peak.1));
    }
    out
}

/// Mirror of [`build_ramp_keyframes`] for the target side, where the
/// optional flat segment is at the *head* (frame 0 → ramp_start) and
/// the easing applies to the descending ramp segment.
fn build_ramp_keyframes_target(
    head_hold_at: Option<f64>,
    ramp_start: (f64, f32),
    ramp_end: (f64, f32),
    easing: HyperzoomEasing,
) -> Vec<Keyframe> {
    let segment_dur = (ramp_end.0 - ramp_start.0).max(1e-6);
    let mut out = Vec::with_capacity(3);
    if let Some(t) = head_hold_at {
        out.push(keyframe(t, ramp_start.1));
    }
    out.push(ramp_endpoint_keyframe(ramp_start.0, ramp_start.1, easing, segment_dur, RampEndpoint::Start));
    out.push(ramp_endpoint_keyframe(ramp_end.0, ramp_end.1, easing, segment_dur, RampEndpoint::End));
    out
}

#[derive(Copy, Clone)]
enum RampEndpoint {
    Start,
    End,
}

/// Emit a keyframe at `(time_secs, value)` whose interpolation +
/// tangents match `easing`. Bezier endpoints get symmetric horizontal
/// tangents at `±segment_dur/3` — the standard "smooth" Bezier shape;
/// 1/3 of the segment is the convention shared by After Effects,
/// Resolve, and most NLE keyframe editors.
fn ramp_endpoint_keyframe(
    time_secs: f64,
    value: f32,
    easing: HyperzoomEasing,
    segment_dur: f64,
    which: RampEndpoint,
) -> Keyframe {
    match easing {
        HyperzoomEasing::Linear => keyframe(time_secs, value),
        HyperzoomEasing::Bezier => {
            let third = segment_dur / 3.0;
            let (tangent_in, tangent_out) = match which {
                RampEndpoint::Start => ((0.0, 0.0), (third, 0.0)),
                RampEndpoint::End => ((-third, 0.0), (0.0, 0.0)),
            };
            Keyframe {
                time_secs,
                value,
                interpolation: Interpolation::Bezier,
                tangent_in,
                tangent_out,
            }
        }
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
            .and_then(|c| c.transform_link.as_ref().map(|tl| tl.target.referenced_clip_id()));
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
        // Default constructor used by the original two-keyframe tests:
        // Linear easing, no hold, no radial blur — matches the
        // pre-feature output exactly so those tests stay green.
        HyperzoomInput {
            source_clip_dur_secs: src,
            target_clip_dur_secs: tgt,
            focal_x_norm: focal_x,
            focal_y_norm: focal_y,
            duration_secs: dur,
            max_scale: max,
            easing: HyperzoomEasing::Linear,
            hold_secs: 0.0,
            add_radial_blur: false,
            radial_blur_amount: 0.0,
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

    // ──────────── Easing + hold-at-peak (opt-in extras) ────────────

    fn input_with(
        focal_x: f32, focal_y: f32, dur: f64, max: f32, src: f64, tgt: f64,
        easing: HyperzoomEasing, hold: f64,
    ) -> HyperzoomInput {
        HyperzoomInput {
            source_clip_dur_secs: src,
            target_clip_dur_secs: tgt,
            focal_x_norm: focal_x,
            focal_y_norm: focal_y,
            duration_secs: dur,
            max_scale: max,
            easing,
            hold_secs: hold,
            add_radial_blur: false,
            radial_blur_amount: 0.0,
        }
    }

    #[test]
    fn linear_default_emits_linear_interpolation() {
        // Pin: pre-easing-feature behaviour stays Linear. A regression
        // where Apply Hyperzoom silently flipped to Bezier on existing
        // projects would be a back-compat break.
        let plan = plan_hyperzoom(input(0.5, 0.5, 0.5, 50.0, 3.0, 3.0));
        for kf in &plan.source_scale_kfs {
            assert!(matches!(kf.interpolation, Interpolation::Linear));
            assert_eq!(kf.tangent_in, (0.0, 0.0));
            assert_eq!(kf.tangent_out, (0.0, 0.0));
        }
        for kf in &plan.target_scale_kfs {
            assert!(matches!(kf.interpolation, Interpolation::Linear));
            assert_eq!(kf.tangent_in, (0.0, 0.0));
            assert_eq!(kf.tangent_out, (0.0, 0.0));
        }
    }

    #[test]
    fn bezier_easing_emits_symmetric_horizontal_tangents() {
        // Bezier S-curve: ramp endpoints have horizontal handles
        // (zero value-delta) extending ±dur/3 into the segment.
        let plan = plan_hyperzoom(input_with(0.5, 0.5, 0.6, 50.0, 3.0, 3.0, HyperzoomEasing::Bezier, 0.0));
        let third = 0.6 / 3.0;
        // Source: 2 keyframes (no hold). First = ramp-start, second = peak.
        assert_eq!(plan.source_scale_kfs.len(), 2);
        let s0 = &plan.source_scale_kfs[0];
        let s1 = &plan.source_scale_kfs[1];
        assert!(matches!(s0.interpolation, Interpolation::Bezier));
        assert!(matches!(s1.interpolation, Interpolation::Bezier));
        assert_eq!(s0.tangent_in, (0.0, 0.0));
        assert!((s0.tangent_out.0 - third).abs() < 1e-9);
        assert!(s0.tangent_out.1.abs() < 1e-6);
        assert!((s1.tangent_in.0 + third).abs() < 1e-9, "End-of-ramp pulls tangent left");
        assert!(s1.tangent_in.1.abs() < 1e-6);
        assert_eq!(s1.tangent_out, (0.0, 0.0));
    }

    #[test]
    fn bezier_target_endpoints_mirror_source_shape() {
        // Same handle math, applied to the descending ramp on the
        // target side. The ramp is `max → 1.0` but the tangent
        // shape (horizontal handles ±third) is identical — the
        // descending segment becomes ease-out / settle-into-1.0
        // automatically because of the value direction.
        let plan = plan_hyperzoom(input_with(0.5, 0.5, 0.6, 50.0, 3.0, 3.0, HyperzoomEasing::Bezier, 0.0));
        let third = 0.6 / 3.0;
        assert_eq!(plan.target_scale_kfs.len(), 2);
        let t0 = &plan.target_scale_kfs[0];
        let t1 = &plan.target_scale_kfs[1];
        assert!(matches!(t0.interpolation, Interpolation::Bezier));
        assert!(matches!(t1.interpolation, Interpolation::Bezier));
        assert!((t0.tangent_out.0 - third).abs() < 1e-9);
        assert!((t1.tangent_in.0 + third).abs() < 1e-9);
    }

    #[test]
    fn hold_secs_zero_keeps_two_keyframe_back_compat_shape() {
        // Pin: hold = 0 must produce the same 2-keyframe output as
        // the pre-hold-feature `input()` constructor. Otherwise the
        // existing `plan_source_ramps_from_one_to_max_at_clip_end`
        // and friends would silently change shape under us.
        let baseline = plan_hyperzoom(input(0.5, 0.5, 0.5, 50.0, 3.0, 3.0));
        let with_hold_zero = plan_hyperzoom(input_with(0.5, 0.5, 0.5, 50.0, 3.0, 3.0, HyperzoomEasing::Linear, 0.0));
        assert_eq!(baseline.source_scale_kfs.len(), with_hold_zero.source_scale_kfs.len());
        assert_eq!(baseline.target_scale_kfs.len(), with_hold_zero.target_scale_kfs.len());
        for (a, b) in baseline.source_scale_kfs.iter().zip(&with_hold_zero.source_scale_kfs) {
            assert!((a.time_secs - b.time_secs).abs() < 1e-9);
            assert!((a.value - b.value).abs() < 1e-6);
        }
    }

    #[test]
    fn hold_inserts_third_keyframe_on_each_side() {
        // 3 s clip, 0.5 s ramp, 0.2 s hold. Expected:
        //   source kfs: (clip_dur - hold - dur, 1.0), (clip_dur - hold, max), (clip_dur, max)
        //              = (2.3, 1.0), (2.8, 50.0), (3.0, 50.0)
        //   target kfs: (0.0, max), (hold, max), (hold + dur, 1.0)
        //              = (0.0, 50.0), (0.2, 50.0), (0.7, 1.0)
        let plan = plan_hyperzoom(input_with(0.5, 0.5, 0.5, 50.0, 3.0, 3.0, HyperzoomEasing::Linear, 0.2));
        let s = &plan.source_scale_kfs;
        let t = &plan.target_scale_kfs;
        assert_eq!(s.len(), 3);
        assert_eq!(t.len(), 3);
        assert!((s[0].time_secs - 2.3).abs() < 1e-6);
        assert!((s[0].value - 1.0).abs() < 1e-6);
        assert!((s[1].time_secs - 2.8).abs() < 1e-6);
        assert!((s[1].value - 50.0).abs() < 1e-6);
        assert!((s[2].time_secs - 3.0).abs() < 1e-6);
        assert!((s[2].value - 50.0).abs() < 1e-6);
        assert!((t[0].time_secs - 0.0).abs() < 1e-6);
        assert!((t[0].value - 50.0).abs() < 1e-6);
        assert!((t[1].time_secs - 0.2).abs() < 1e-6);
        assert!((t[1].value - 50.0).abs() < 1e-6);
        assert!((t[2].time_secs - 0.7).abs() < 1e-6);
        assert!((t[2].value - 1.0).abs() < 1e-6);
    }

    #[test]
    fn hold_segment_is_linear_even_under_bezier_easing() {
        // The flat held segment must stay flat — we deliberately keep
        // the hold keyframe Linear with zero tangents so a Bezier
        // S-curve doesn't introduce sub-pixel jitter through the held
        // window. Only the ramp endpoints carry the easing shape.
        let plan = plan_hyperzoom(input_with(0.5, 0.5, 0.5, 50.0, 3.0, 3.0, HyperzoomEasing::Bezier, 0.2));
        let s_tail = plan.source_scale_kfs.last().unwrap();
        let t_head = plan.target_scale_kfs.first().unwrap();
        assert!(matches!(s_tail.interpolation, Interpolation::Linear));
        assert!(matches!(t_head.interpolation, Interpolation::Linear));
        assert_eq!(s_tail.tangent_in, (0.0, 0.0));
        assert_eq!(s_tail.tangent_out, (0.0, 0.0));
        assert_eq!(t_head.tangent_in, (0.0, 0.0));
        assert_eq!(t_head.tangent_out, (0.0, 0.0));
    }

    // ─────────── Radial-blur convenience layer (opt-in) ───────────

    fn input_with_blur(
        easing: HyperzoomEasing, hold: f64,
        add_blur: bool, blur_amount: f32,
    ) -> HyperzoomInput {
        HyperzoomInput {
            source_clip_dur_secs: 3.0,
            target_clip_dur_secs: 3.0,
            focal_x_norm: 0.5,
            focal_y_norm: 0.5,
            duration_secs: 0.5,
            max_scale: 50.0,
            easing,
            hold_secs: hold,
            add_radial_blur: add_blur,
            radial_blur_amount: blur_amount,
        }
    }

    #[test]
    fn radial_blur_disabled_yields_none_in_plan() {
        // Pin: by default no radial-blur entry — handler skips the
        // RadialBlur insertion entirely. Ensures back-compat for
        // every existing Apply Hyperzoom invocation.
        let plan = plan_hyperzoom(input(0.5, 0.5, 0.5, 50.0, 3.0, 3.0));
        assert!(plan.radial_blur.is_none());
    }

    #[test]
    fn radial_blur_zero_amount_yields_none_even_when_enabled() {
        // Toggle on but amount=0: the visual effect is identity, so
        // the planner short-circuits. Avoids inserting a zero-blur
        // RadialBlur builtin that would just round-trip pixels
        // through the GPU/CPU dispatch for nothing.
        let plan = plan_hyperzoom(input_with_blur(HyperzoomEasing::Linear, 0.0, true, 0.0));
        assert!(plan.radial_blur.is_none());
    }

    #[test]
    fn radial_blur_enabled_emits_amount_keyframes_with_scale_timing() {
        // Same time-shape as the scale ramp, values 0 → amount → 0.
        let plan = plan_hyperzoom(input_with_blur(HyperzoomEasing::Linear, 0.0, true, 15.0));
        let rb = plan.radial_blur.expect("radial blur enabled with positive amount");
        assert_eq!(rb.center, (0.5, 0.5));
        assert!((rb.peak_amount - 15.0).abs() < 1e-6);
        // Source: (2.5, 0.0) → (3.0, 15.0) — same times as scale_kfs.
        assert_eq!(rb.source_amount_kfs.len(), 2);
        assert!((rb.source_amount_kfs[0].time_secs - 2.5).abs() < 1e-9);
        assert!(rb.source_amount_kfs[0].value.abs() < 1e-6);
        assert!((rb.source_amount_kfs[1].time_secs - 3.0).abs() < 1e-9);
        assert!((rb.source_amount_kfs[1].value - 15.0).abs() < 1e-6);
        // Target: (0.0, 15.0) → (0.5, 0.0).
        assert_eq!(rb.target_amount_kfs.len(), 2);
        assert!((rb.target_amount_kfs[0].time_secs - 0.0).abs() < 1e-9);
        assert!((rb.target_amount_kfs[0].value - 15.0).abs() < 1e-6);
        assert!((rb.target_amount_kfs[1].time_secs - 0.5).abs() < 1e-9);
        assert!(rb.target_amount_kfs[1].value.abs() < 1e-6);

        // Time-shape parity with scale_kfs — drift between the two
        // would mean the blur ramp leads or lags the zoom ramp,
        // which would feel weirdly disconnected.
        for (a, b) in plan.source_scale_kfs.iter().zip(&rb.source_amount_kfs) {
            assert!((a.time_secs - b.time_secs).abs() < 1e-9);
        }
        for (a, b) in plan.target_scale_kfs.iter().zip(&rb.target_amount_kfs) {
            assert!((a.time_secs - b.time_secs).abs() < 1e-9);
        }
    }

    #[test]
    fn radial_blur_picks_up_hold_segment_three_keyframes() {
        // 3-keyframe shape mirrors the scale ramp under hold > 0.
        let plan = plan_hyperzoom(input_with_blur(HyperzoomEasing::Linear, 0.2, true, 15.0));
        let rb = plan.radial_blur.unwrap();
        assert_eq!(rb.source_amount_kfs.len(), 3);
        assert_eq!(rb.target_amount_kfs.len(), 3);
        // Source held tail at peak amount.
        assert!((rb.source_amount_kfs[1].value - 15.0).abs() < 1e-6);
        assert!((rb.source_amount_kfs[2].value - 15.0).abs() < 1e-6);
        // Target head held at peak amount.
        assert!((rb.target_amount_kfs[0].value - 15.0).abs() < 1e-6);
        assert!((rb.target_amount_kfs[1].value - 15.0).abs() < 1e-6);
    }

    #[test]
    fn radial_blur_inherits_bezier_easing_on_ramp_endpoints() {
        // Bezier flag flows through to the radial-blur amount kfs
        // identically — so the blur ramps with the same S-curve as
        // the zoom and the two motions stay locked.
        let plan = plan_hyperzoom(input_with_blur(HyperzoomEasing::Bezier, 0.0, true, 15.0));
        let rb = plan.radial_blur.unwrap();
        for kf in &rb.source_amount_kfs {
            assert!(matches!(kf.interpolation, Interpolation::Bezier));
        }
        for kf in &rb.target_amount_kfs {
            assert!(matches!(kf.interpolation, Interpolation::Bezier));
        }
    }

    #[test]
    fn hold_clamps_negative_to_zero() {
        // Defensive: negative hold inputs (stale UI state, mis-typed
        // value) clamp to 0 — never produce a 3-keyframe shape with
        // a backward-running tail.
        let plan = plan_hyperzoom(input_with(0.5, 0.5, 0.5, 50.0, 3.0, 3.0, HyperzoomEasing::Linear, -0.5));
        assert_eq!(plan.source_scale_kfs.len(), 2);
        assert_eq!(plan.target_scale_kfs.len(), 2);
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
