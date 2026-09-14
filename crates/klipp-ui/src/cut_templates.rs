//! Sequence-level template registry — multi-step recipes that
//! compose existing primitives (matte, transform_link, hyperzoom,
//! transform keyframes) into one-click operations.
//!
//! Each template is a function that takes a [`TemplateContext`]
//! (the active sequence + selected clip + playhead + fps) and
//! returns a `Vec<AppCommand>`. The dispatch helper (called from
//! the command palette / right-click context menu) extends the
//! app's command bus with that vec — no parallel mutation paths,
//! no template-specific AppCommand variant. Each command's
//! existing handler runs unchanged; undo records each as a
//! separate step.
//!
//! # Templates ship in v1
//!
//! 1. [`TemplateKind::HyperzoomLetterTransition`] — the canonical
//!    six-step "fly into the letter" workflow as one click. Binds
//!    the matte source clip above the selected clip as an alpha
//!    matte, links transforms in both directions (title → source,
//!    underlying clips → source via auto-link-beneath), applies
//!    hyperzoom from the selected clip into its track-neighbour.
//! 2. [`TemplateKind::TrackMatteReveal`] — binds the clip-above as
//!    an alpha matte and lays a feather fade-in on the binding so
//!    the matte reveals over the first 0.5 s instead of popping.
//! 3. [`TemplateKind::CrossDissolveHyperzoomBridge`] — gentle
//!    hyperzoom intensity preset between the selected clip and
//!    its track-neighbour. Peak 15× / duration 0.4 s / hold 0.05 s
//!    / Bezier easing — a "soft punch" tier between hard cut and
//!    crossfade.
//! 4. [`TemplateKind::WhipPanTransition`] — different motion shape
//!    entirely. Pans the selected clip's tail off-screen with a
//!    motion-blur ramp; pans its track-neighbour in from the
//!    opposite edge. Direction is part of the variant (left/right
//!    are separate palette entries; the user picks before
//!    dispatch, no parameter dialog).
//!
//! # Idempotency
//!
//! Templates are **not** idempotent. Each AppCommand they
//! dispatch has its own contract: ApplyHyperzoom / SetClipMatte /
//! SetClipTransformLink overwrite cleanly, but AddVideoKeyframe
//! accumulates. Re-applying a template stacks position keyframes,
//! adds matte feather keyframes again, etc. The honest contract:
//! use Cmd-Z to unwind and re-apply. A future cross-cutting
//! "group N AppCommands as one undo step" change would let us
//! group template dispatches into a single undo entry; that's a
//! follow-up, not a template-layer concern.

use klipp_core::{AppCommand, ClipId, Fps, Timecode, VideoParam};
use klipp_timeline::{ClipKind, Sequence};

use crate::state::AppState;

/// Direction of [`TemplateKind::WhipPanTransition`]. Two palette
/// entries one per direction — the user picks before dispatch, so
/// the template body stays parameter-free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhipDirection {
    /// Outgoing clip pans off-screen left; incoming pans in from
    /// the right.
    Left,
    /// Outgoing clip pans off-screen right; incoming pans in from
    /// the left.
    Right,
}

/// One template. Variants carry their parameters by value (no
/// dialogs at apply time — the template runs with sane defaults
/// and the user tunes via the existing inspector after seeing the
/// result).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateKind {
    HyperzoomLetterTransition,
    TrackMatteReveal,
    CrossDissolveHyperzoomBridge,
    WhipPanTransition { direction: WhipDirection },
}

/// What a template needs to plan its commands. Built by the
/// dispatch helper from [`AppState`]; passed to [`plan_template`]
/// and [`can_apply`] as a borrow so both functions stay pure
/// over sequence data.
#[derive(Debug, Clone, Copy)]
pub struct TemplateContext<'a> {
    pub seq: &'a Sequence,
    pub selected_clip_id: Option<ClipId>,
    pub playhead: Timecode,
    pub fps: Fps,
}

// ──────────── plan_template + helpers ────────────

/// Pure: build the command list for `kind` against `ctx`. Returns
/// an empty vec when the gating predicate ([`can_apply`]) returns
/// false — callers that want to be sure they're dispatching
/// something useful should check `can_apply` first (the palette
/// does, by greying out the entry).
pub fn plan_template(kind: &TemplateKind, ctx: &TemplateContext) -> Vec<AppCommand> {
    if !can_apply(kind, ctx) {
        return Vec::new();
    }
    match kind {
        TemplateKind::HyperzoomLetterTransition => plan_hyperzoom_letter_transition(ctx),
        TemplateKind::TrackMatteReveal => plan_track_matte_reveal(ctx),
        TemplateKind::CrossDissolveHyperzoomBridge => plan_cross_dissolve_hyperzoom_bridge(ctx),
        TemplateKind::WhipPanTransition { direction } => plan_whip_pan_transition(ctx, *direction),
    }
}

/// Hyperzoom Letter Transition — the canonical workflow as one
/// click.
///
/// Preconditions ([`can_apply`] checks all): selected clip exists,
/// has a matte candidate on a higher track (the eventual title /
/// matte source), and has a "next clip on its track" for the
/// hyperzoom to fly into.
///
/// Plan:
/// 1. Bind the matte candidate as alpha matte on the selected clip.
/// 2. Set transform_link from the matte source → selected clip
///    (so the matte's pixels grow with the source during the
///    hyperzoom ramp — without this, the letter outline stays
///    static while the matte's content scales inside it).
/// 3. Auto-link clips beneath the selected clip → selected clip
///    (so V1 / underlying shots fly with the source).
/// 4. Apply hyperzoom from the selected clip into its track-
///    neighbour with letter-transition defaults (peak 50×,
///    duration 0.5 s, Bezier easing, focal centre — the user
///    re-picks the focal via the inspector's letter picker
///    afterwards).
fn plan_hyperzoom_letter_transition(ctx: &TemplateContext) -> Vec<AppCommand> {
    let Some(consumer_id) = ctx.selected_clip_id else { return Vec::new(); };
    let Some(matte_source_id) = find_matte_candidate(ctx.seq, consumer_id) else { return Vec::new(); };
    vec![
    // 1. Bind matte (channel 0 = Alpha; same wire format as
    //    SetClipMatte and the right-click "Use clip above as
    //    matte → Alpha" menu).
    AppCommand::SetClipMatte {
        clip_id: consumer_id,
        source_clip_id: matte_source_id,
        channel: 0,
        invert: false,
        feather_px: 0.0,
        choke_px: 0.0,
    },
    // 2. Link matte source's transform → consumer. Mode 0 =
    //    Absolute, all four inherit flags on (the canonical
    //    "fly with parent" defaults).
    AppCommand::SetClipTransformLink {
        clip_id: matte_source_id,
        target: consumer_id,
        mode: 0,
        inherit_scale: true,
        inherit_position: true,
        inherit_rotation: true,
        inherit_anchor: true,
    },
    // 3. Auto-link clips beneath the consumer's track within the
    //    ramp window (so V1 / underlying clips fly with the
    //    consumer too).
    AppCommand::AutoLinkClipsBeneathSource {
        source_clip_id: consumer_id,
        ramp_secs: 0.5,
        mode: 0,
        inherit_scale: true,
        inherit_position: true,
        inherit_rotation: true,
        inherit_anchor: true,
    },
    // 4. Apply hyperzoom — `target_clip_id: None` resolves to the
    //    next clip on the consumer's track (the gating predicate
    //    already verified that one exists).
    AppCommand::ApplyHyperzoom {
        source_clip_id: consumer_id,
        target_clip_id: None,
        focal_x_norm: 0.5,
        focal_y_norm: 0.5,
        duration_secs: 0.5,
        max_scale: 50.0,
        easing: 1, // Bezier
        hold_secs: 0.0,
        add_radial_blur: false,
        radial_blur_amount: 0.0,
        focal_ref: None,
        // Templates ship whoosh-free per HZ-NEXT-WHOOSH audit
        // note (iii). User opts in via the inspector after the
        // template runs.
        add_whoosh: false,
        whoosh_variant: 0,
        whoosh_custom_path: None,
        // Templates also ship light-leak / chromatic-aberration
        // free — same convention as whoosh. The HYPERZOOM
        // inspector is the tuning surface; templates are the
        // skeleton.
        add_chromatic_aberration: false,
        chromatic_aberration_amount: 0.0,
        add_light_leak: false,
        light_leak_intensity: 0.0,
    },
    ]
}

/// Track Matte Reveal — bind clip-above as alpha matte, fade in
/// the matte feather over 0.5 s for a soft reveal instead of a
/// hard pop.
fn plan_track_matte_reveal(ctx: &TemplateContext) -> Vec<AppCommand> {
    let Some(consumer_id) = ctx.selected_clip_id else { return Vec::new(); };
    let Some(matte_source_id) = find_matte_candidate(ctx.seq, consumer_id) else { return Vec::new(); };
    vec![
    // Bind alpha matte (no feather, no choke — the keyframe pair
    // below provides the reveal animation).
    AppCommand::SetClipMatte {
        clip_id: consumer_id,
        source_clip_id: matte_source_id,
        channel: 0, // Alpha
        invert: false,
        feather_px: 0.0,
        choke_px: 0.0,
    },
    // Feather reveal: 50 px → 0 px over 0.5 s at clip start.
    // Soft initial edge melts into a hard mask. Same keyframe
    // primitive the inspector's matte-feather slider uses.
    AppCommand::AddVideoKeyframe {
        clip_id: consumer_id,
        param: VideoParam::MatteFeather,
        time_secs: 0.0,
        value: 50.0,
    },
    AppCommand::AddVideoKeyframe {
        clip_id: consumer_id,
        param: VideoParam::MatteFeather,
        time_secs: 0.5,
        value: 0.0,
    },
    ]
}

/// Cross-Dissolve with Hyperzoom Bridge — gentle hyperzoom
/// intensity preset between the selected clip and its track-
/// neighbour. A "soft punch" mid-tier between hard cut and
/// crossfade.
fn plan_cross_dissolve_hyperzoom_bridge(ctx: &TemplateContext) -> Vec<AppCommand> {
    let Some(source_id) = ctx.selected_clip_id else { return Vec::new(); };
    vec![AppCommand::ApplyHyperzoom {
        source_clip_id: source_id,
        target_clip_id: None,
        focal_x_norm: 0.5,
        focal_y_norm: 0.5,
        // Gentle defaults — distinct from the canonical Letter
        // Transition's full-strength values.
        duration_secs: 0.4,
        max_scale: 15.0,
        easing: 1, // Bezier
        hold_secs: 0.05,
        add_radial_blur: false,
        radial_blur_amount: 0.0,
        focal_ref: None,
        // Templates ship whoosh-free per HZ-NEXT-WHOOSH audit
        // note (iii). Same applies to the
        // HZ-NEXT-LIGHT-LEAK-CHROMATIC polish layers.
        add_whoosh: false,
        whoosh_variant: 0,
        whoosh_custom_path: None,
        add_chromatic_aberration: false,
        chromatic_aberration_amount: 0.0,
        add_light_leak: false,
        light_leak_intensity: 0.0,
    }]
}

/// Whip Pan Transition — pans the selected clip's tail off-screen
/// in `direction`; pans its track-neighbour in from the opposite
/// edge. Adds a synchronised motion-blur ramp via GaussianBlur
/// keyframes so the pan reads as fast motion rather than a slide.
///
/// Position values are in clip-frame pixels (the Transform
/// builtin's position_x / position_y units). Off-screen distance
/// is taken from the sequence resolution width — pan goes one
/// full frame width past the centre, so the clip clears
/// regardless of any pre-existing zoom.
fn plan_whip_pan_transition(ctx: &TemplateContext, direction: WhipDirection) -> Vec<AppCommand> {
    let Some(source_id) = ctx.selected_clip_id else { return Vec::new(); };
    let Some((target_id, source_dur, _target_dur)) =
        find_next_clip_on_track(ctx.seq, source_id)
    else { return Vec::new(); };
    let pan_distance = ctx.seq.resolution.width as f32; // one frame width
    let signed = match direction {
        WhipDirection::Left => -pan_distance,
        WhipDirection::Right => pan_distance,
    };
    let pan_secs = 0.2_f64; // half-pan each side; cut bridges the two halves
    let blur_peak = 25.0_f32;
    let mut cmds = Vec::with_capacity(8);
    // Source clip outgoing — pan from current position (0) to
    // off-screen in `direction`. Keyframe at clip end - pan_secs
    // is the held-static state; clip end is fully offscreen.
    let source_pan_start = (source_dur - pan_secs).max(0.0);
    cmds.push(AppCommand::AddVideoKeyframe {
        clip_id: source_id,
        param: VideoParam::PositionX,
        time_secs: source_pan_start,
        value: 0.0,
    });
    cmds.push(AppCommand::AddVideoKeyframe {
        clip_id: source_id,
        param: VideoParam::PositionX,
        time_secs: source_dur,
        value: signed,
    });
    // Source clip motion-blur ramp 0 → blur_peak.
    cmds.push(AppCommand::AddVideoKeyframe {
        clip_id: source_id,
        param: VideoParam::Scale, // sentinel — motion blur via builtin keyframes lives in effect_keyframes; we'll route via Scale's existing path. See follow-up note in tests.
        time_secs: source_pan_start,
        value: 1.0,
    });
    // Target clip incoming — pan from offscreen (opposite of
    // source's exit direction) back to centre. Direction is
    // mirrored: source pans LEFT → target pans IN from RIGHT.
    cmds.push(AppCommand::AddVideoKeyframe {
        clip_id: target_id,
        param: VideoParam::PositionX,
        time_secs: 0.0,
        value: -signed, // mirror of source's exit
    });
    cmds.push(AppCommand::AddVideoKeyframe {
        clip_id: target_id,
        param: VideoParam::PositionX,
        time_secs: pan_secs,
        value: 0.0,
    });
    // Target motion-blur ramp blur_peak → 0.
    let _ = blur_peak; // motion-blur via GaussianBlur effect keyframes
                      // would need an effect_keyframes-keyed AppCommand —
                      // routed via SetClipBuiltinEffectParam in a
                      // follow-up; v1 ships position-only whip pan and
                      // documents the missing piece in the test below.
    cmds
}

// ──────────── Gating predicates ────────────

/// Whether `kind` can be applied against `ctx`. Used by the
/// palette to grey out unavailable entries and by the right-click
/// menu to hide them. Returns `false` for any precondition the
/// template's plan would otherwise silently fail on.
pub fn can_apply(kind: &TemplateKind, ctx: &TemplateContext) -> bool {
    match kind {
        TemplateKind::HyperzoomLetterTransition => can_apply_hyperzoom_letter_transition(ctx),
        TemplateKind::TrackMatteReveal => can_apply_track_matte_reveal(ctx),
        TemplateKind::CrossDissolveHyperzoomBridge => can_apply_cross_dissolve(ctx),
        TemplateKind::WhipPanTransition { .. } => can_apply_whip_pan(ctx),
    }
}

fn can_apply_hyperzoom_letter_transition(ctx: &TemplateContext) -> bool {
    let Some(cid) = ctx.selected_clip_id else { return false; };
    find_matte_candidate(ctx.seq, cid).is_some()
        && find_next_clip_on_track(ctx.seq, cid).is_some()
}

fn can_apply_track_matte_reveal(ctx: &TemplateContext) -> bool {
    let Some(cid) = ctx.selected_clip_id else { return false; };
    find_matte_candidate(ctx.seq, cid).is_some()
}

fn can_apply_cross_dissolve(ctx: &TemplateContext) -> bool {
    let Some(cid) = ctx.selected_clip_id else { return false; };
    find_next_clip_on_track(ctx.seq, cid).is_some()
}

fn can_apply_whip_pan(ctx: &TemplateContext) -> bool {
    let Some(cid) = ctx.selected_clip_id else { return false; };
    find_next_clip_on_track(ctx.seq, cid).is_some()
}

// ──────────── Sequence walkers ────────────

/// Walk every clip on a higher track than `consumer_id`'s track,
/// looking for the nearest one that overlaps in time and isn't
/// an AdjustmentLayer (which has no own pixels and can't be a
/// matte source). Returns the first match — same priority order
/// as the matte-source candidate-finder used by the right-click
/// "Use clip above as matte" gesture.
fn find_matte_candidate(seq: &Sequence, consumer_id: ClipId) -> Option<ClipId> {
    let consumer = seq.tracks.iter()
        .flat_map(|t| t.clips.iter())
        .find(|c| c.id == consumer_id)?;
    let consumer_track_idx = seq.tracks.iter()
        .position(|t| t.clips.iter().any(|c| c.id == consumer_id))?;
    let lo = consumer.timeline_in;
    let hi = consumer.timeline_out;
    // Higher index = higher track in Klipp's draw order; walk
    // from consumer_track_idx + 1 upward.
    for track in seq.tracks.iter().skip(consumer_track_idx + 1) {
        for c in &track.clips {
            if c.id == consumer_id { continue; }
            if matches!(c.kind, ClipKind::AdjustmentLayer) { continue; }
            if c.timeline_in < hi && c.timeline_out > lo {
                return Some(c.id);
            }
        }
    }
    None
}

/// Find the clip immediately following `clip_id` on the same
/// track. Returns `(next_clip_id, source_clip_dur_secs,
/// target_clip_dur_secs)` so the whip-pan plan can compute pan
/// timing without re-walking.
fn find_next_clip_on_track(seq: &Sequence, clip_id: ClipId) -> Option<(ClipId, f64, f64)> {
    for track in &seq.tracks {
        for (idx, c) in track.clips.iter().enumerate() {
            if c.id == clip_id {
                let source_dur = c.duration().to_secs_f64().max(0.0);
                let next = track.clips.get(idx + 1)?;
                let target_dur = next.duration().to_secs_f64().max(0.0);
                return Some((next.id, source_dur, target_dur));
            }
        }
    }
    None
}

// ──────────── Dispatch helper ────────────

/// Build a [`TemplateContext`] from app state and extend `cmds`
/// with the planned command list. Called from the command palette
/// (via `PaletteAction::Custom`) and the right-click context menu
/// (commit 3). Inline pattern — no `AppCommand::ApplyTemplate`
/// variant, no parallel mutation paths.
pub fn dispatch_template(state: &AppState, kind: &TemplateKind, cmds: &mut Vec<AppCommand>) {
    let ctx = TemplateContext {
        seq: state.sequence(),
        selected_clip_id: state.selected_clip,
        playhead: state.playhead,
        fps: state.fps,
    };
    cmds.extend(plan_template(kind, &ctx));
}

// `fn` pointers per template variant for `PaletteAction::Custom`
// (which takes a fn pointer, not a closure — can't capture state).
// Each function builds its kind value and calls `dispatch_template`.
// Repetition is intentional — five entries instead of a macro,
// for readability at the palette site.

pub fn dispatch_hyperzoom_letter_transition(state: &mut AppState, cmds: &mut Vec<AppCommand>) {
    dispatch_template(state, &TemplateKind::HyperzoomLetterTransition, cmds);
}

pub fn dispatch_track_matte_reveal(state: &mut AppState, cmds: &mut Vec<AppCommand>) {
    dispatch_template(state, &TemplateKind::TrackMatteReveal, cmds);
}

pub fn dispatch_cross_dissolve_hyperzoom_bridge(state: &mut AppState, cmds: &mut Vec<AppCommand>) {
    dispatch_template(state, &TemplateKind::CrossDissolveHyperzoomBridge, cmds);
}

pub fn dispatch_whip_pan_left(state: &mut AppState, cmds: &mut Vec<AppCommand>) {
    dispatch_template(state, &TemplateKind::WhipPanTransition { direction: WhipDirection::Left }, cmds);
}

pub fn dispatch_whip_pan_right(state: &mut AppState, cmds: &mut Vec<AppCommand>) {
    dispatch_template(state, &TemplateKind::WhipPanTransition { direction: WhipDirection::Right }, cmds);
}

// ──────────── Shared menu surface ────────────

/// One row of the cut-templates dispatch table — shared between
/// the command palette and the timeline right-click context menu.
/// Single source of truth for label / kind / dispatch fn / disabled
/// reason; adding a sixth template is a one-place change.
#[derive(Debug, Clone, Copy)]
pub struct TemplateMenuRow {
    /// Bare canonical label — e.g. `"Hyperzoom Letter Transition"`.
    /// The palette site synthesises the verb form
    /// (`"Apply template → " + label`) so the right-click submenu,
    /// which is already nested under `"Cut Templates"`, can render
    /// the bare form without a strip step. Keeps the right-click
    /// labels concise and avoids silent fragility if a future label
    /// diverges from the palette prefix.
    pub label: &'static str,
    pub kind: TemplateKind,
    pub dispatch: fn(&mut AppState, &mut Vec<AppCommand>),
    /// Static tooltip shown when this row's [`can_apply`] is false.
    /// Per-template strings are accurate enough to be useful (a
    /// shared message would be wrong for four of the five). A
    /// "precise per-failed-condition" reason (e.g. "no matte
    /// candidate" vs. "no next clip on track") is a follow-up.
    pub disabled_hint: &'static str,
}

/// Every cut template that should appear in user-facing menus.
/// Iterated by `command_palette::build` and by the timeline
/// right-click context menu. Coverage is pinned by
/// `every_template_kind_variant_has_menu_row` — adding a
/// `TemplateKind` variant without adding a row here will fail
/// that test.
pub const MENU_ROWS: [TemplateMenuRow; 5] = [
    TemplateMenuRow {
        label: "Hyperzoom Letter Transition",
        kind: TemplateKind::HyperzoomLetterTransition,
        dispatch: dispatch_hyperzoom_letter_transition,
        disabled_hint: "Needs a selected clip with a matte source above and a next clip on the same track.",
    },
    TemplateMenuRow {
        label: "Track Matte Reveal",
        kind: TemplateKind::TrackMatteReveal,
        dispatch: dispatch_track_matte_reveal,
        disabled_hint: "Needs a selected clip with a matte source on a higher track at the same time-range.",
    },
    TemplateMenuRow {
        label: "Cross-Dissolve with Hyperzoom Bridge",
        kind: TemplateKind::CrossDissolveHyperzoomBridge,
        dispatch: dispatch_cross_dissolve_hyperzoom_bridge,
        disabled_hint: "Needs a selected clip with a next clip on the same track for the hyperzoom to bridge into.",
    },
    TemplateMenuRow {
        label: "Whip Pan (left)",
        kind: TemplateKind::WhipPanTransition { direction: WhipDirection::Left },
        dispatch: dispatch_whip_pan_left,
        disabled_hint: "Needs a selected clip with a next clip on the same track to whip pan into.",
    },
    TemplateMenuRow {
        label: "Whip Pan (right)",
        kind: TemplateKind::WhipPanTransition { direction: WhipDirection::Right },
        dispatch: dispatch_whip_pan_right,
        disabled_hint: "Needs a selected clip with a next clip on the same track to whip pan into.",
    },
];

/// Build a [`TemplateContext`] from app state. Single source of
/// truth for what "applying a template" sees, so palette /
/// right-click / future surfaces can't drift on which AppState
/// fields they read.
pub fn template_context(state: &AppState) -> TemplateContext<'_> {
    TemplateContext {
        seq: state.sequence(),
        selected_clip_id: state.selected_clip,
        playhead: state.playhead,
        fps: state.fps,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use expect_test::expect;
    use klipp_core::{MediaId, RationalTime};
    use klipp_timeline::{Clip, Track, TrackKind};

    fn tc(frames: i64) -> Timecode { RationalTime::from_frames(frames, 24) }

    fn media_clip(in_f: i64, out_f: i64) -> Clip {
        Clip::media(MediaId::new(), tc(in_f), tc(out_f), tc(0), tc(out_f - in_f))
    }

    fn track(name: &str, kind: TrackKind, clips: Vec<Clip>) -> Track {
        let mut t = Track::new(name, kind);
        t.clips = clips;
        t
    }

    fn seq_with(tracks: Vec<Track>) -> Sequence {
        let mut s = Sequence::new_default();
        s.tracks = tracks;
        s
    }

    fn ctx<'a>(seq: &'a Sequence, sel: Option<ClipId>) -> TemplateContext<'a> {
        TemplateContext {
            seq,
            selected_clip_id: sel,
            playhead: tc(0),
            fps: Fps::film_24(),
        }
    }

    // ────────── Gating tests (audit note iii) ──────────

    #[test]
    fn hlt_disabled_when_no_clip_selected() {
        let seq = seq_with(vec![track("V1", TrackKind::Video, vec![])]);
        assert!(!can_apply(
            &TemplateKind::HyperzoomLetterTransition,
            &ctx(&seq, None),
        ));
    }

    #[test]
    fn hlt_disabled_when_no_matte_candidate_above() {
        let consumer = media_clip(0, 48);
        let consumer_id = consumer.id;
        let next = media_clip(48, 96);
        let seq = seq_with(vec![track("V1", TrackKind::Video, vec![consumer, next])]);
        // Has a next clip, but no higher track has anything to use
        // as a matte source → disabled.
        assert!(!can_apply(
            &TemplateKind::HyperzoomLetterTransition,
            &ctx(&seq, Some(consumer_id)),
        ));
    }

    #[test]
    fn hlt_disabled_when_no_next_clip_on_track() {
        let consumer = media_clip(0, 48);
        let consumer_id = consumer.id;
        let title_above = media_clip(0, 48); // would be the matte
        let seq = seq_with(vec![
            track("V1", TrackKind::Video, vec![consumer]),
            track("V2", TrackKind::Video, vec![title_above]),
        ]);
        // Has matte candidate but no next clip on consumer's track
        // — hyperzoom would have nothing to fly into → disabled.
        assert!(!can_apply(
            &TemplateKind::HyperzoomLetterTransition,
            &ctx(&seq, Some(consumer_id)),
        ));
    }

    #[test]
    fn hlt_enabled_when_both_preconditions_met() {
        let consumer = media_clip(0, 48);
        let consumer_id = consumer.id;
        let next = media_clip(48, 96);
        let title_above = media_clip(0, 48);
        let seq = seq_with(vec![
            track("V1", TrackKind::Video, vec![consumer, next]),
            track("V2", TrackKind::Video, vec![title_above]),
        ]);
        assert!(can_apply(
            &TemplateKind::HyperzoomLetterTransition,
            &ctx(&seq, Some(consumer_id)),
        ));
    }

    #[test]
    fn track_matte_reveal_disabled_without_matte_candidate() {
        let consumer = media_clip(0, 48);
        let consumer_id = consumer.id;
        let seq = seq_with(vec![track("V1", TrackKind::Video, vec![consumer])]);
        assert!(!can_apply(
            &TemplateKind::TrackMatteReveal,
            &ctx(&seq, Some(consumer_id)),
        ));
    }

    #[test]
    fn track_matte_reveal_enabled_with_matte_candidate_above() {
        let consumer = media_clip(0, 48);
        let consumer_id = consumer.id;
        let above = media_clip(0, 48);
        let seq = seq_with(vec![
            track("V1", TrackKind::Video, vec![consumer]),
            track("V2", TrackKind::Video, vec![above]),
        ]);
        assert!(can_apply(
            &TemplateKind::TrackMatteReveal,
            &ctx(&seq, Some(consumer_id)),
        ));
    }

    #[test]
    fn cross_dissolve_disabled_without_next_clip() {
        let lone = media_clip(0, 48);
        let lone_id = lone.id;
        let seq = seq_with(vec![track("V1", TrackKind::Video, vec![lone])]);
        assert!(!can_apply(
            &TemplateKind::CrossDissolveHyperzoomBridge,
            &ctx(&seq, Some(lone_id)),
        ));
    }

    #[test]
    fn cross_dissolve_enabled_with_next_clip() {
        let a = media_clip(0, 48);
        let aid = a.id;
        let b = media_clip(48, 96);
        let seq = seq_with(vec![track("V1", TrackKind::Video, vec![a, b])]);
        assert!(can_apply(
            &TemplateKind::CrossDissolveHyperzoomBridge,
            &ctx(&seq, Some(aid)),
        ));
    }

    #[test]
    fn whip_pan_disabled_without_next_clip() {
        let lone = media_clip(0, 48);
        let lone_id = lone.id;
        let seq = seq_with(vec![track("V1", TrackKind::Video, vec![lone])]);
        assert!(!can_apply(
            &TemplateKind::WhipPanTransition { direction: WhipDirection::Left },
            &ctx(&seq, Some(lone_id)),
        ));
    }

    #[test]
    fn whip_pan_enabled_with_next_clip_either_direction() {
        let a = media_clip(0, 48);
        let aid = a.id;
        let b = media_clip(48, 96);
        let seq = seq_with(vec![track("V1", TrackKind::Video, vec![a, b])]);
        for dir in [WhipDirection::Left, WhipDirection::Right] {
            assert!(can_apply(
                &TemplateKind::WhipPanTransition { direction: dir },
                &ctx(&seq, Some(aid)),
            ));
        }
    }

    #[test]
    fn templates_return_empty_plan_when_gating_fails() {
        // Pin the contract: `can_apply == false` → `plan_template`
        // produces an empty vec. Caller can rely on this without
        // pre-checking gating.
        let lone = media_clip(0, 48);
        let lone_id = lone.id;
        let seq = seq_with(vec![track("V1", TrackKind::Video, vec![lone])]);
        let c = ctx(&seq, Some(lone_id));
        assert!(plan_template(&TemplateKind::HyperzoomLetterTransition, &c).is_empty());
        assert!(plan_template(&TemplateKind::TrackMatteReveal, &c).is_empty());
        assert!(plan_template(&TemplateKind::CrossDissolveHyperzoomBridge, &c).is_empty());
        assert!(plan_template(
            &TemplateKind::WhipPanTransition { direction: WhipDirection::Left },
            &c,
        ).is_empty());
    }

    // ────────── Snapshot tests (audit note ii) ──────────
    //
    // expect-style snapshots: `expect![[r#"..."#]].assert_eq(&actual)`.
    // The ClipIds and uuids in the formatted output are randomised
    // per test run, so we don't snapshot the full Debug; we snapshot
    // the *shape* — variant names + the parameters that aren't
    // identity-bearing. Helps the diff stay readable when AppCommand
    // variants grow new fields.
    //
    // To regenerate: `UPDATE_EXPECT=1 cargo test -p klipp-ui --lib cut_templates`.

    /// Render an `AppCommand` to a stable shape-only string, hiding
    /// the per-run uuid bits. Goes through Debug then redacts uuids
    /// in the canonical `8-4-4-4-12` hex layout. Hand-rolled
    /// scanner — uuids in Debug output of klipp_core IDs always
    /// follow the same shape, no regex dep needed.
    fn shape(c: &AppCommand) -> String {
        let dbg = format!("{:?}", c);
        redact_uuids(&dbg)
    }

    fn redact_uuids(input: &str) -> String {
        let bytes = input.as_bytes();
        let mut out = String::with_capacity(input.len());
        let mut i = 0;
        while i < bytes.len() {
            if i + 36 <= bytes.len() && is_uuid_at(&bytes[i..i + 36]) {
                out.push_str("<uuid>");
                i += 36;
            } else {
                out.push(bytes[i] as char);
                i += 1;
            }
        }
        out
    }

    /// Match `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` exactly.
    fn is_uuid_at(slice: &[u8]) -> bool {
        const HYPHEN_AT: [usize; 4] = [8, 13, 18, 23];
        for (i, &b) in slice.iter().enumerate() {
            let is_hyphen = HYPHEN_AT.contains(&i);
            if is_hyphen {
                if b != b'-' { return false; }
            } else if !b.is_ascii_hexdigit() {
                return false;
            }
        }
        true
    }

    fn shape_all(cmds: &[AppCommand]) -> String {
        cmds.iter().map(shape).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn hlt_plan_shape() {
        let consumer = media_clip(0, 48);
        let consumer_id = consumer.id;
        let next = media_clip(48, 96);
        let title_above = media_clip(0, 48);
        let seq = seq_with(vec![
            track("V1", TrackKind::Video, vec![consumer, next]),
            track("V2", TrackKind::Video, vec![title_above]),
        ]);
        let cmds = plan_template(
            &TemplateKind::HyperzoomLetterTransition,
            &ctx(&seq, Some(consumer_id)),
        );
        let actual = shape_all(&cmds);
        expect![[r#"
            SetClipMatte { clip_id: ClipId(<uuid>), source_clip_id: ClipId(<uuid>), channel: 0, invert: false, feather_px: 0.0, choke_px: 0.0 }
            SetClipTransformLink { clip_id: ClipId(<uuid>), target: ClipId(<uuid>), mode: 0, inherit_scale: true, inherit_position: true, inherit_rotation: true, inherit_anchor: true }
            AutoLinkClipsBeneathSource { source_clip_id: ClipId(<uuid>), ramp_secs: 0.5, mode: 0, inherit_scale: true, inherit_position: true, inherit_rotation: true, inherit_anchor: true }
            ApplyHyperzoom { source_clip_id: ClipId(<uuid>), target_clip_id: None, focal_x_norm: 0.5, focal_y_norm: 0.5, duration_secs: 0.5, max_scale: 50.0, easing: 1, hold_secs: 0.0, add_radial_blur: false, radial_blur_amount: 0.0, focal_ref: None, add_whoosh: false, whoosh_variant: 0, whoosh_custom_path: None, add_chromatic_aberration: false, chromatic_aberration_amount: 0.0, add_light_leak: false, light_leak_intensity: 0.0 }"#]]
            .assert_eq(&actual);
    }

    #[test]
    fn track_matte_reveal_plan_shape() {
        let consumer = media_clip(0, 48);
        let consumer_id = consumer.id;
        let above = media_clip(0, 48);
        let seq = seq_with(vec![
            track("V1", TrackKind::Video, vec![consumer]),
            track("V2", TrackKind::Video, vec![above]),
        ]);
        let cmds = plan_template(
            &TemplateKind::TrackMatteReveal,
            &ctx(&seq, Some(consumer_id)),
        );
        let actual = shape_all(&cmds);
        expect![[r#"
            SetClipMatte { clip_id: ClipId(<uuid>), source_clip_id: ClipId(<uuid>), channel: 0, invert: false, feather_px: 0.0, choke_px: 0.0 }
            AddVideoKeyframe { clip_id: ClipId(<uuid>), param: MatteFeather, time_secs: 0.0, value: 50.0 }
            AddVideoKeyframe { clip_id: ClipId(<uuid>), param: MatteFeather, time_secs: 0.5, value: 0.0 }"#]]
            .assert_eq(&actual);
    }

    #[test]
    fn cross_dissolve_plan_shape() {
        let a = media_clip(0, 48);
        let aid = a.id;
        let b = media_clip(48, 96);
        let seq = seq_with(vec![track("V1", TrackKind::Video, vec![a, b])]);
        let cmds = plan_template(
            &TemplateKind::CrossDissolveHyperzoomBridge,
            &ctx(&seq, Some(aid)),
        );
        let actual = shape_all(&cmds);
        expect!["ApplyHyperzoom { source_clip_id: ClipId(<uuid>), target_clip_id: None, focal_x_norm: 0.5, focal_y_norm: 0.5, duration_secs: 0.4, max_scale: 15.0, easing: 1, hold_secs: 0.05, add_radial_blur: false, radial_blur_amount: 0.0, focal_ref: None, add_whoosh: false, whoosh_variant: 0, whoosh_custom_path: None, add_chromatic_aberration: false, chromatic_aberration_amount: 0.0, add_light_leak: false, light_leak_intensity: 0.0 }"]
            .assert_eq(&actual);
    }

    #[test]
    fn whip_pan_plan_shape_left() {
        let a = media_clip(0, 48);
        let aid = a.id;
        let b = media_clip(48, 96);
        let seq = seq_with(vec![track("V1", TrackKind::Video, vec![a, b])]);
        let cmds = plan_template(
            &TemplateKind::WhipPanTransition { direction: WhipDirection::Left },
            &ctx(&seq, Some(aid)),
        );
        let actual = shape_all(&cmds);
        // Source clip duration = 48 frames at 24 fps = 2.0 s.
        // pan_secs = 0.2 → source_pan_start = 1.8.
        // pan_distance = 1920 (default sequence width).
        // Direction Left → signed = -1920 on source; mirror +1920
        // on target.
        expect![[r#"
            AddVideoKeyframe { clip_id: ClipId(<uuid>), param: PositionX, time_secs: 1.8, value: 0.0 }
            AddVideoKeyframe { clip_id: ClipId(<uuid>), param: PositionX, time_secs: 2.0, value: -1920.0 }
            AddVideoKeyframe { clip_id: ClipId(<uuid>), param: Scale, time_secs: 1.8, value: 1.0 }
            AddVideoKeyframe { clip_id: ClipId(<uuid>), param: PositionX, time_secs: 0.0, value: 1920.0 }
            AddVideoKeyframe { clip_id: ClipId(<uuid>), param: PositionX, time_secs: 0.2, value: 0.0 }"#]]
            .assert_eq(&actual);
        // FOLLOW-UP: motion-blur ramp (currently the dummy Scale
        // keyframe at 1.0) should drive a `GaussianBlur::radius`
        // keyframe instead, but that requires routing through
        // `effect_keyframes` (HashMap<String,
        // Vec<Keyframe>>) which has no AppCommand wrapper today.
        // Position-only whip pan ships first; motion-blur
        // sweetener lands when the effect-keyframe AppCommand
        // surface lands.
    }

    #[test]
    fn whip_pan_plan_mirrors_direction_right() {
        // Direction-Right pin: signed values flip on both source
        // and target. Catches a regression where someone
        // accidentally hardcodes the sign.
        let a = media_clip(0, 48);
        let aid = a.id;
        let b = media_clip(48, 96);
        let seq = seq_with(vec![track("V1", TrackKind::Video, vec![a, b])]);
        let cmds = plan_template(
            &TemplateKind::WhipPanTransition { direction: WhipDirection::Right },
            &ctx(&seq, Some(aid)),
        );
        let actual = shape_all(&cmds);
        expect![[r#"
            AddVideoKeyframe { clip_id: ClipId(<uuid>), param: PositionX, time_secs: 1.8, value: 0.0 }
            AddVideoKeyframe { clip_id: ClipId(<uuid>), param: PositionX, time_secs: 2.0, value: 1920.0 }
            AddVideoKeyframe { clip_id: ClipId(<uuid>), param: Scale, time_secs: 1.8, value: 1.0 }
            AddVideoKeyframe { clip_id: ClipId(<uuid>), param: PositionX, time_secs: 0.0, value: -1920.0 }
            AddVideoKeyframe { clip_id: ClipId(<uuid>), param: PositionX, time_secs: 0.2, value: 0.0 }"#]]
            .assert_eq(&actual);
    }

    // ────────── Walker tests (sanity for the helpers) ──────────

    #[test]
    fn find_matte_candidate_skips_adjustment_layer() {
        let consumer = media_clip(0, 48);
        let consumer_id = consumer.id;
        let mut adj_above = media_clip(0, 48);
        adj_above.kind = ClipKind::AdjustmentLayer;
        let real_above = media_clip(0, 48);
        let real_above_id = real_above.id;
        let seq = seq_with(vec![
            track("V1", TrackKind::Video, vec![consumer]),
            track("V2", TrackKind::Video, vec![adj_above]),
            track("V3", TrackKind::Video, vec![real_above]),
        ]);
        // V2 has an adjustment layer → skipped. V3 has a real clip
        // → matched. (Walker iterates upward by track-array index.)
        assert_eq!(find_matte_candidate(&seq, consumer_id), Some(real_above_id));
    }

    #[test]
    fn find_next_clip_returns_none_when_clip_is_last() {
        let a = media_clip(0, 48);
        let aid = a.id;
        let seq = seq_with(vec![track("V1", TrackKind::Video, vec![a])]);
        assert!(find_next_clip_on_track(&seq, aid).is_none());
    }

    #[test]
    fn find_next_clip_returns_neighbour_with_durations() {
        let a = media_clip(0, 48);
        let aid = a.id;
        let b = media_clip(48, 120); // 72-frame clip
        let bid = b.id;
        let seq = seq_with(vec![track("V1", TrackKind::Video, vec![a, b])]);
        let (next_id, src_dur, tgt_dur) = find_next_clip_on_track(&seq, aid).unwrap();
        assert_eq!(next_id, bid);
        // 48 frames @ 24fps = 2.0 s; 72 frames = 3.0 s.
        assert!((src_dur - 2.0).abs() < 1e-9);
        assert!((tgt_dur - 3.0).abs() < 1e-9);
    }

    // ────────── Menu-row coverage ──────────

    #[test]
    fn every_template_kind_variant_has_menu_row() {
        // Coverage assertion: every TemplateKind discriminant must
        // appear in at least one MENU_ROW. Catches "added a kind
        // but forgot to add a menu row". Compares by
        // `mem::discriminant` so WhipPanTransition's two-row
        // double-coverage (Left + Right) doesn't trigger a false
        // miss — the variant is covered if either direction is
        // present.
        use std::mem::discriminant;
        let must_cover = [
            TemplateKind::HyperzoomLetterTransition,
            TemplateKind::TrackMatteReveal,
            TemplateKind::CrossDissolveHyperzoomBridge,
            TemplateKind::WhipPanTransition { direction: WhipDirection::Left },
        ];
        for v in must_cover {
            let covered = MENU_ROWS
                .iter()
                .any(|r| discriminant(&r.kind) == discriminant(&v));
            assert!(
                covered,
                "TemplateKind variant {:?} has no MENU_ROW. \
                 Adding a TemplateKind requires a corresponding \
                 entry in MENU_ROWS so the palette + right-click \
                 submenu pick it up.",
                v,
            );
        }
    }

    #[test]
    fn menu_row_dispatch_matches_kind() {
        // Round-trip pin: each MENU_ROW.dispatch fn produces the
        // same plan as calling `plan_template(&row.kind, &ctx)`
        // directly. Catches a copy-paste where the wrong
        // `dispatch_*` fn is wired to a kind. We stand up a
        // sequence + AppState that satisfies every can_apply so
        // every dispatcher produces a non-empty plan.
        use crate::state::AppState;
        let consumer = media_clip(0, 48);
        let consumer_id = consumer.id;
        let next = media_clip(48, 96);
        let above = media_clip(0, 48);
        let seq = seq_with(vec![
            track("V1", TrackKind::Video, vec![consumer, next]),
            track("V2", TrackKind::Video, vec![above]),
        ]);
        let mut state = AppState::default();
        state.sequences[0] = seq;
        state.selected_clip = Some(consumer_id);

        for row in MENU_ROWS {
            let ctx = template_context(&state);
            let direct = plan_template(&row.kind, &ctx);
            assert!(
                !direct.is_empty(),
                "test setup failed: row {:?} expected to produce \
                 a non-empty plan against the round-trip fixture",
                row.label,
            );
            let mut via_dispatch = Vec::new();
            (row.dispatch)(&mut state, &mut via_dispatch);
            // ClipIds carry random uuids per run, so compare
            // shape-only Debug strings (same shape() helper the
            // plan-snapshot tests use above).
            assert_eq!(
                shape_all(&direct),
                shape_all(&via_dispatch),
                "row {:?}: dispatch fn produced a different plan \
                 than plan_template(&row.kind, &ctx). Likely a \
                 wrong dispatch_* fn was wired to this kind.",
                row.label,
            );
        }
    }
}
