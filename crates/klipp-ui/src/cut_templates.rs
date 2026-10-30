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
//!    the matte reveals over the first 0.5 s instead of popping. A
//!    clips recipe file since #2886
//!    (`assets/recipes/cut/track_matte_reveal.klipptemplate`), placed
//!    through the document as one undo step.
//! 3. [`TemplateKind::CrossDissolveHyperzoomBridge`] — gentle
//!    hyperzoom intensity preset between the selected clip and
//!    its track-neighbour. Peak 15× / duration 0.4 s / hold 0.05 s
//!    / Bezier easing — a "soft punch" tier between hard cut and
//!    crossfade.
//! 4. [`TemplateKind::WhipPanTransition`] — different motion shape
//!    entirely. Pans the selected clip's tail off-screen over its
//!    last 0.2 s; pans its track-neighbour in from the opposite edge
//!    over its first. Direction is part of the variant (left/right
//!    are separate palette entries; the user picks before
//!    dispatch, no parameter dialog). Clips recipe files since #2886
//!    (`assets/recipes/cut/whip_pan_{left,right}.klipptemplate`).
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

use klipp_core::{AppCommand, ClipId, Fps, Timecode};
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
    pub pool: &'a klipp_media::MediaPool,
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
        TemplateKind::CrossDissolveHyperzoomBridge => plan_cross_dissolve_hyperzoom_bridge(ctx),
        // Clips recipes (#2886): the document places them.
        TemplateKind::TrackMatteReveal | TemplateKind::WhipPanTransition { .. } => Vec::new(),
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

// ──────────── Gating predicates ────────────

/// Whether `kind` can be applied against `ctx`. Used by the
/// palette to grey out unavailable entries and by the right-click
/// menu to hide them. Returns `false` for any precondition the
/// template's plan would otherwise silently fail on.
/// The built-in clips recipe a template is (#2886), by name: the matte
/// reveal and the whip pans are files; the hyperzoom ones stay code (the
/// hyperzoom planner is).
pub fn recipe_of(kind: &TemplateKind) -> Option<&'static str> {
    match kind {
        TemplateKind::TrackMatteReveal => Some("Track Matte Reveal"),
        TemplateKind::WhipPanTransition { direction: WhipDirection::Left } => Some("Whip Pan (left)"),
        TemplateKind::WhipPanTransition { direction: WhipDirection::Right } => Some("Whip Pan (right)"),
        TemplateKind::HyperzoomLetterTransition | TemplateKind::CrossDissolveHyperzoomBridge => None,
    }
}

/// The recipe op placing Klipp's own clips recipe `name` on `clip`, its
/// clips found from it.
fn recipe_op(name: &str, clip: ClipId) -> Option<klipp_document::ops::Op> {
    use klipp_document::ops::{ClipRef, ClipsRecipe, Op};
    let recipe = klipp_timeline::composition_template::builtin_clip_recipes().into_iter().find(|r| r.name == name)?;
    Some(Op::Recipe { clip_id: ClipRef::Id(clip), name: name.into(), slots: Default::default(), controls: Default::default(), steps: None, build: None, audio: None, clips: Some(ClipsRecipe(Box::new(recipe))) })
}

/// The recipe `name` placed on the selected clip, as the document plans
/// it: one step, or why not.
fn recipe_plan(name: &str, ctx: &TemplateContext) -> Result<klipp_document::ops::Plan, String> {
    let clip = ctx.selected_clip_id.ok_or("select a clip first")?;
    let op = recipe_op(name, clip).ok_or_else(|| format!("Klipp's \"{name}\" recipe is missing"))?;
    klipp_document::ops::resolve(ctx.seq, ctx.pool, std::slice::from_ref(&op)).map_err(|r| r.why)
}

pub fn can_apply(kind: &TemplateKind, ctx: &TemplateContext) -> bool {
    if let Some(name) = recipe_of(kind) {
        return recipe_plan(name, ctx).is_ok();
    }
    match kind {
        TemplateKind::HyperzoomLetterTransition => can_apply_hyperzoom_letter_transition(ctx),
        TemplateKind::CrossDissolveHyperzoomBridge => can_apply_cross_dissolve(ctx),
        TemplateKind::TrackMatteReveal | TemplateKind::WhipPanTransition { .. } => false,
    }
}

fn can_apply_hyperzoom_letter_transition(ctx: &TemplateContext) -> bool {
    let Some(cid) = ctx.selected_clip_id else { return false; };
    find_matte_candidate(ctx.seq, cid).is_some()
        && find_next_clip_on_track(ctx.seq, cid).is_some()
}

fn can_apply_cross_dissolve(ctx: &TemplateContext) -> bool {
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
pub fn dispatch_template(state: &mut AppState, kind: &TemplateKind, cmds: &mut Vec<AppCommand>) {
    if let Some(name) = recipe_of(kind) {
        // A clips recipe (#2886): planned by the document, applied and
        // recorded as one undo step, then the engine told.
        match recipe_plan(name, &template_context(state)) {
            Ok(plan) => {
                for step in plan.steps {
                    let active = state.active_sequence_idx;
                    let _ = klipp_document::apply::DocumentView { sequences: &mut state.sequences, active, media_pool: &mut state.media_pool, fps: &mut state.fps }.apply_forward(&step.edit);
                    state.push_history(step.edit);
                }
                state.dirty = true;
                cmds.push(AppCommand::SequenceUpdated);
            }
            Err(why) => state.toast.show(&why),
        }
        return;
    }
    let ctx = template_context(state);
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
        pool: &state.media_pool,
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
            pool: Box::leak(Box::new(klipp_media::MediaPool::new())),
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
        assert!(plan_template(&TemplateKind::CrossDissolveHyperzoomBridge, &c).is_empty());
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
            // A row that is a clips recipe (#2886) applies it: one undo
            // step named for it, and the engine told.
            if let Some(name) = recipe_of(&row.kind) {
                let mut probe = AppState::default();
                probe.sequences[0] = state.sequence().clone();
                probe.selected_clip = Some(consumer_id);
                let mut via_dispatch = Vec::new();
                (row.dispatch)(&mut probe, &mut via_dispatch);
                assert!(matches!(via_dispatch.as_slice(), [AppCommand::SequenceUpdated]), "row {:?}: {via_dispatch:?}", row.label);
                let entry = probe.take_undo().unwrap_or_else(|| panic!("row {:?}: one undo step", row.label));
                assert!(matches!(&entry.command, klipp_document::history::EditCommand::Named { label, .. } if label == name), "row {:?}", row.label);
                continue;
            }
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

#[cfg(test)]
mod recipe_check {
    //! #2886: the matte reveal and the whip pans as Klipp's own clips
    //! recipes, placed through the document.
    use super::*;
    use klipp_core::{GeneratorData, RationalTime, Resolution};
    use klipp_timeline::{Clip, ClipKind, TextBox, TitleData, Track, TrackKind};

    fn tc(s: f64) -> Timecode {
        RationalTime::from_secs_f64(s, 24)
    }

    fn solid(color: [u8; 4], from: f64, to: f64) -> Clip {
        Clip { kind: ClipKind::Generator(GeneratorData::SolidColor { color }), source_in: tc(0.0), source_out: tc(to - from), ..Clip::gap(tc(from), tc(to)) }
    }

    /// Red then blue on V1, big white text over the red on V2.
    fn scene() -> (Sequence, ClipId) {
        let mut seq = Sequence::new_default();
        seq.resolution = Resolution { width: 320, height: 180 };
        let (red, blue) = (solid([220, 40, 40, 255], 0.0, 2.0), solid([40, 60, 220, 255], 2.0, 4.0));
        let id = red.id;
        let mut v1 = Track::new("V1", TrackKind::Video);
        v1.clips = vec![red, blue];
        let mut v2 = Track::new("V2", TrackKind::Video);
        v2.clips = vec![Clip { kind: ClipKind::Title(TitleData::single_text(TextBox { text: "MATTE".into(), font_size: 300.0, ..Default::default() })), ..Clip::gap(tc(0.0), tc(2.0)) }];
        seq.tracks = vec![v1, v2];
        (seq, id)
    }

    fn by_recipe(seq: &Sequence, kind: &TemplateKind, selected: ClipId) -> Sequence {
        let pool = klipp_media::MediaPool::new();
        let ctx = TemplateContext { seq, pool: &pool, selected_clip_id: Some(selected), playhead: tc(0.0), fps: seq.fps };
        let plan = recipe_plan(recipe_of(kind).unwrap(), &ctx).unwrap();
        let mut seqs = vec![seq.clone()];
        let (mut p, mut f) = (pool.clone(), seq.fps);
        for step in &plan.steps {
            klipp_document::apply::DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut p, fps: &mut f }.apply_forward(&step.edit);
        }
        seqs.remove(0)
    }

    /// #2886: a scaled clip keeps its scale through a whip pan: the
    /// recipe keys its position only (the planner it replaced also keyed
    /// a scale of 1 at the pan's start, which reset it).
    #[test]
    fn a_scaled_clip_keeps_its_scale_through_a_whip_pan() {
        let (mut seq, red) = scene();
        let clip = seq.all_clips_mut().find(|c| c.id == red).unwrap();
        clip.push_effect(klipp_effects::BuiltinEffect::Transform { position_x: 0.0, position_y: 0.0, scale_x: 1.5, scale_y: 1.5, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None });
        assert_eq!(seq.all_clips().find(|c| c.id == red).unwrap().video_param_value(klipp_core::VideoParam::Scale), 1.5);
        for kind in &[TemplateKind::WhipPanTransition { direction: WhipDirection::Left }, TemplateKind::WhipPanTransition { direction: WhipDirection::Right }] {
            let got = by_recipe(&seq, kind, red);
            let clip = got.all_clips().find(|c| c.id == red).unwrap();
            assert!(clip.video_param_keyframes(klipp_core::VideoParam::Scale).is_empty(), "{kind:?}: no Scale key");
            assert_eq!(clip.video_param_value(klipp_core::VideoParam::Scale), 1.5, "{kind:?}: its scale kept");
        }
    }
}
