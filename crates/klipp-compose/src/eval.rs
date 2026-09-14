//! Evaluating a clip's animated parameters at a timeline time: keyframes,
//! modifier stacks, transforms (with transform links, tracker targets and
//! glyph-anchored focal points) and opacity.
//!
//! Moved from klipp-engine so the viewer and export evaluate a clip
//! identically.

/// The modifier-evaluation context for `clip` at timeline `time`: clip
/// time relative to the clip's start (clamped at zero), duration, seed.
pub fn eval_context(clip: &klipp_timeline::Clip, time: klipp_core::Timecode, fps: f64) -> klipp_core::EvalContext {
    klipp_core::EvalContext {
        clip_time_secs: (time - clip.timeline_in).to_secs_f64().max(0.0),
        clip_duration_secs: clip.source_out.to_secs_f64() - clip.source_in.to_secs_f64(),
        timeline_time_secs: time.to_secs_f64(),
        fps,
        seed: clip.id.0.as_u64_pair().0,
    }
}

/// Evaluate a parameter with optional modifier stack, falling back to plain keyframe evaluation.
pub fn eval_with_modifiers(
    keyframes: &[klipp_core::Keyframe],
    modifiers: &std::collections::HashMap<klipp_core::ParamId, klipp_core::ModifierStack>,
    param: &klipp_core::ParamId,
    time_secs: f64,
    default: f32,
    ctx: &klipp_core::EvalContext,
) -> f32 {
    match modifiers.get(param) {
        Some(stack) if !stack.modifiers.is_empty() => {
            klipp_core::evaluate_param(keyframes, stack, time_secs, default, ctx)
        }
        _ => klipp_core::keyframe::evaluate(keyframes, time_secs, default),
    }
}

/// Resolved transform tuple: `(position_x, position_y, scale_x,
/// scale_y, rotation_deg, anchor_x, anchor_y)`. Matches the argument
/// order of `klipp_effects::transform::apply` and the seven scalar
/// fields the title / V2 paths feed into `BuiltinEffect::Transform`.
pub type ResolvedTransform = (f32, f32, f32, f32, f32, f32, f32);

/// Evaluate a single clip's *own* transform — keyframes + modifiers
/// resolved against the clip's `BuiltinEffect::Transform` defaults
/// (or the conventional `(0,0,1,1,0,0.5,0.5)` if no Transform effect
/// exists). This is the pre-link version; callers that respect
/// `transform_link` should go through `resolve_clip_transform`
/// instead.
pub fn evaluate_clip_own_transform(
    clip: &klipp_timeline::Clip,
    clip_time_secs: f64,
    eval_ctx: &klipp_core::EvalContext,
) -> ResolvedTransform {
    let tdef = clip.builtin_effects.iter()
        .find_map(|e| match e {
            klipp_effects::BuiltinEffect::Transform {
                position_x, position_y, scale_x, scale_y, rotation, anchor_x, anchor_y, ..
            } => Some((*position_x, *position_y, *scale_x, *scale_y, *rotation, *anchor_x, *anchor_y)),
            _ => None,
        })
        .unwrap_or((0.0, 0.0, 1.0, 1.0, 0.0, 0.5, 0.5));
    let px = eval_with_modifiers(&clip.transform_pos_x_keyframes, &clip.param_modifiers, &klipp_core::ParamId::PositionX, clip_time_secs, tdef.0, eval_ctx);
    let py = eval_with_modifiers(&clip.transform_pos_y_keyframes, &clip.param_modifiers, &klipp_core::ParamId::PositionY, clip_time_secs, tdef.1, eval_ctx);
    let sc = eval_with_modifiers(&clip.transform_scale_keyframes, &clip.param_modifiers, &klipp_core::ParamId::Scale, clip_time_secs, tdef.2, eval_ctx);
    let ro = eval_with_modifiers(&clip.transform_rotation_keyframes, &clip.param_modifiers, &klipp_core::ParamId::Rotation, clip_time_secs, tdef.4, eval_ctx);
    // Scale is single-axis in our model — same value on x and y.
    (px, py, sc, sc, ro, tdef.5, tdef.6)
}

/// Compose `own` with `target` per the link's mode and per-axis
/// inherit flags. Anchor inheritance is always Absolute regardless
/// of mode (see the `LinkMode` doc comment for why).
pub fn compose_transform(
    own: ResolvedTransform,
    target: ResolvedTransform,
    link: &klipp_timeline::TransformLink,
) -> ResolvedTransform {
    use klipp_timeline::LinkMode;
    let (own_px, own_py, own_sx, own_sy, own_ro, own_ax, own_ay) = own;
    let (t_px, t_py, t_sx, t_sy, t_ro, t_ax, t_ay) = target;
    let (px, py) = match (link.inherit_position, link.mode) {
        (false, _) => (own_px, own_py),
        (true, LinkMode::Absolute) => (t_px, t_py),
        (true, LinkMode::Additive) => (own_px + t_px, own_py + t_py),
    };
    let (sx, sy) = match (link.inherit_scale, link.mode) {
        (false, _) => (own_sx, own_sy),
        (true, LinkMode::Absolute) => (t_sx, t_sy),
        (true, LinkMode::Additive) => (own_sx * t_sx, own_sy * t_sy),
    };
    let ro = match (link.inherit_rotation, link.mode) {
        (false, _) => own_ro,
        (true, LinkMode::Absolute) => t_ro,
        (true, LinkMode::Additive) => own_ro + t_ro,
    };
    let (ax, ay) = if link.inherit_anchor { (t_ax, t_ay) } else { (own_ax, own_ay) };
    (px, py, sx, sy, ro, ax, ay)
}

/// Resolve a clip's transform with `transform_link` chase + cache.
/// `cache` is a per-frame scratch keyed by ClipId — passes itself by
/// `&mut` so a recursive resolve hits the cache after the first
/// climb. Cycle protection: command-time validation prevents real
/// cycles, so the recursion is bounded by chain length; we also
/// add a depth cap (64) as belt-and-suspenders against a corrupt
/// project file.
///
/// `glyph_cache` + `title_renderer` + `(frame_w, frame_h)` thread
/// through for the glyph-anchored focal-point resolution path
/// (HZ-NEXT-GLYPH-LOCK). When a clip's `BuiltinEffect::Transform`
/// carries a `FocalRef::Glyph`, the resolver looks up the
/// referenced title clip's glyph layout (via the shared
/// `klipp_title::ensure_glyph_positions` cache), composes through
/// the title's own resolved transform, and overrides the static
/// `anchor_x` / `anchor_y` with the resolved glyph centroid.
/// Failed resolution (clip missing, kind changed, char_index
/// out of range) falls back to the static fields silently.
pub fn resolve_clip_transform(
    clip: &klipp_timeline::Clip,
    seq: &klipp_timeline::Sequence,
    playhead: klipp_core::Timecode,
    fps: f64,
    cache: &mut std::collections::HashMap<klipp_core::ClipId, ResolvedTransform>,
    glyph_cache: &mut std::collections::HashMap<klipp_core::ClipId, klipp_title::GlyphCacheEntry>,
    title_renderer: &mut klipp_title::TitleRenderer,
    frame_w: u32,
    frame_h: u32,
) -> ResolvedTransform {
    resolve_clip_transform_depth(clip, seq, playhead, fps, cache, glyph_cache, title_renderer, frame_w, frame_h, 0)
}

pub fn resolve_clip_transform_depth(
    clip: &klipp_timeline::Clip,
    seq: &klipp_timeline::Sequence,
    playhead: klipp_core::Timecode,
    fps: f64,
    cache: &mut std::collections::HashMap<klipp_core::ClipId, ResolvedTransform>,
    glyph_cache: &mut std::collections::HashMap<klipp_core::ClipId, klipp_title::GlyphCacheEntry>,
    title_renderer: &mut klipp_title::TitleRenderer,
    frame_w: u32,
    frame_h: u32,
    depth: u32,
) -> ResolvedTransform {
    if let Some(cached) = cache.get(&clip.id) { return *cached; }
    let clip_time = (playhead - clip.timeline_in).to_secs_f64().max(0.0);
    let eval_ctx = klipp_core::EvalContext {
        clip_time_secs: clip_time,
        clip_duration_secs: (clip.source_out.to_secs_f64() - clip.source_in.to_secs_f64()),
        timeline_time_secs: playhead.to_secs_f64(),
        fps,
        seed: clip.id.0.as_u64_pair().0,
    };
    let mut own = evaluate_clip_own_transform(clip, clip_time, &eval_ctx);

    // Glyph-anchored focal-point override. Reads `focal_ref` off the
    // first `Transform` builtin on this clip; resolves to clip-space
    // `(focal_x, focal_y)` via the title's glyph layout + the
    // title's own resolved transform. Failed resolution falls back
    // to the static `anchor_x` / `anchor_y` (already in `own.5/6`).
    let focal_ref = clip.builtin_effects.iter().find_map(|e| match e {
        klipp_effects::BuiltinEffect::Transform { focal_ref, .. } => focal_ref.as_ref().copied(),
        _ => None,
    });
    if let Some(fr) = focal_ref {
        if let Some((fx, fy)) = resolve_focal_ref_to_clip_space(
            &fr, seq, playhead, fps, cache, glyph_cache, title_renderer, frame_w, frame_h, depth + 1,
        ) {
            own.5 = fx;
            own.6 = fy;
        }
    }

    let result = match (&clip.transform_link, depth < 64) {
        (Some(link), true) => {
            // Two link-target shapes:
            //
            //   `Clip(id)` — recurse into the linked clip's resolved
            //     transform (today's behaviour) and compose per the
            //     link's mode + per-axis flags.
            //
            //   `Tracker { clip_id, point_index }` — synthesise a
            //     position-only `ResolvedTransform` from the tracker's
            //     per-frame output. The synthetic target has identity
            //     scale/rotation/anchor and translation derived from
            //     the tracker's normalised position, so the existing
            //     `compose_transform` honours the link's
            //     inherit-position-only / inherit-scale-only flags
            //     unchanged. Resolution failure (host clip missing,
            //     point_index out of range, before start_frame, past
            //     end of positions) → silent fallback to `own` —
            //     same contract as the Glyph focal-ref path.
            let target_xform: Option<ResolvedTransform> = match &link.target {
                klipp_timeline::TransformLinkTarget::Clip(id) => {
                    seq.tracks.iter()
                        .flat_map(|t| t.clips.iter())
                        .find(|c| c.id == *id)
                        .map(|target_clip| {
                            resolve_clip_transform_depth(
                                target_clip, seq, playhead, fps, cache,
                                glyph_cache, title_renderer, frame_w, frame_h, depth + 1,
                            )
                        })
                }
                klipp_timeline::TransformLinkTarget::Tracker { clip_id, point_index } => {
                    seq.tracks.iter()
                        .flat_map(|t| t.clips.iter())
                        .find(|c| c.id == *clip_id)
                        .and_then(|host_clip| {
                            tracker_position_at_playhead(host_clip, *point_index, playhead, fps)
                        })
                        .map(|(tx, ty)| synthesise_tracker_transform(tx, ty, frame_w, frame_h))
                }
            };
            match target_xform {
                None => own, // resolution failed → silent fallback
                Some(t) => compose_transform(own, t, link),
            }
        }
        _ => own,
    };
    cache.insert(clip.id, result);
    result
}

/// Resolve a [`klipp_effects::FocalRef`] to a clip-space `(focal_x, focal_y)` in
/// `[0, 1]` for use as the effective anchor on the clip carrying
/// the reference. `None` return means resolution failed — caller
/// falls back to the clip's static `anchor_x` / `anchor_y`.
///
/// Failure paths (all return `None`):
///
/// * Title clip not in the active sequence (removed since picker time)
/// * Referenced clip is no longer a Title kind (kind changed)
/// * `char_index` is past the end of the title's current glyph layout
///   (text shortened)
///
/// Success path: glyph centroid in title's frame pixels →
/// normalised by `(frame_w, frame_h)` → composed through the title
/// clip's own resolved transform (so a title that's been moved /
/// scaled / link-following lands its glyph centroid at the
/// post-transform screen position) → clamped to `[0, 1]`.
///
/// Rotation through the title's transform is NOT applied for v1 —
/// rotated titles are rare and the math adds visible complexity for
/// little gain. Filed under HZ-NEXT-GLYPH-LOCK followups if a real
/// use case shows up.
pub fn resolve_focal_ref_to_clip_space(
    focal_ref: &klipp_effects::FocalRef,
    seq: &klipp_timeline::Sequence,
    playhead: klipp_core::Timecode,
    fps: f64,
    transform_cache: &mut std::collections::HashMap<klipp_core::ClipId, ResolvedTransform>,
    glyph_cache: &mut std::collections::HashMap<klipp_core::ClipId, klipp_title::GlyphCacheEntry>,
    title_renderer: &mut klipp_title::TitleRenderer,
    frame_w: u32,
    frame_h: u32,
    depth: u32,
) -> Option<(f32, f32)> {
    if depth >= 64 {
        // Defence-in-depth against pathological project files —
        // matches the transform_link recursion cap.
        return None;
    }
    match focal_ref {
        klipp_effects::FocalRef::Glyph { title_clip_id, char_index, fallback_xy: _ } => {
            // Wrap the raw uuid in ClipId at the boundary (FocalRef
            // carries `uuid::Uuid` because klipp-effects can't
            // depend on klipp-core; see the type-level note on
            // `klipp_effects::FocalRef`).
            let cid = klipp_core::ClipId(*title_clip_id);
            let title_clip = seq.tracks.iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == cid)?;
            let td = title_clip.title_data()?;

            let positions = klipp_title::ensure_glyph_positions(
                glyph_cache, cid, title_renderer, td, frame_w, frame_h,
            );
            let cp = positions.get(*char_index as usize)?;

            // Glyph centroid in title's frame pixels.
            let centroid_px = (cp.x + cp.advance * 0.5, cp.y + cp.height * 0.5);

            // Title's own resolved transform — recurses into
            // resolve_clip_transform_depth which uses the same
            // transform_cache for memoisation. Title rarely has a
            // focal_ref of its own, but if it did the recursion
            // bounded by the depth cap above.
            let title_xform = resolve_clip_transform_depth(
                title_clip, seq, playhead, fps, transform_cache,
                glyph_cache, title_renderer, frame_w, frame_h, depth + 1,
            );

            // Apply the title's anchor + scale + position to the
            // glyph centroid. The Transform model is:
            //   (px, py, sx, sy, ro, ax, ay) where `ax/ay` are
            //   normalised pivots and `position` / `scale` are
            //   pixel-domain offsets / multipliers around the pivot.
            //
            // Mapping a glyph at `(gx, gy)` frame-px through the
            // title's transform (rotation skipped for v1):
            //
            //   anchor_px = (ax * frame_w, ay * frame_h)
            //   relative   = centroid_px - anchor_px
            //   scaled     = relative * (sx, sy)   (uniform)
            //   screen_px  = anchor_px + scaled + (position_x, position_y)
            //
            // Then normalise by frame to land in clip-space [0, 1].
            //
            // TODO(HZ-NEXT-GLYPH-LOCK followup): rotation (the `_ro`
            // slot below) is intentionally dropped here. The
            // canonical hyperzoom workflow has the title sitting flat
            // (no rotation on the matte source), so the simplification
            // is invisible in practice. Restoring rotation requires
            // applying a 2D rotation matrix to (dx, dy) BEFORE the
            // scale step — straightforward but adds a sin/cos per
            // resolved frame for what is, today, no observable
            // benefit. Revisit when a real "kinetic typography fly-in
            // through a rotated letter" use case shows up.
            let (px, py, sx, _sy, _ro, ax, ay) = title_xform;
            let fw = frame_w.max(1) as f32;
            let fh = frame_h.max(1) as f32;
            let ax_px = ax * fw;
            let ay_px = ay * fh;
            let dx = centroid_px.0 - ax_px;
            let dy = centroid_px.1 - ay_px;
            let screen_x = ax_px + dx * sx + px;
            let screen_y = ay_px + dy * sx + py;
            Some((
                (screen_x / fw).clamp(0.0, 1.0),
                (screen_y / fh).clamp(0.0, 1.0),
            ))
        }
        klipp_effects::FocalRef::Tracker { clip_id, point_index, fallback_xy: _ } => {
            // HZ-MATTE-NEXT-TRACKER-DRIVER commit 2 — live tracker
            // resolution. Look up the tracker's per-frame position
            // and return it as the focal point. Tracker positions
            // are already in normalised clip-space `[0, 1]` (see
            // `klipp_core::TrackPoint::positions` doc), so no
            // composition-through-title-transform is needed (unlike
            // the Glyph arm above) — we go straight to the focal
            // value, bypassing the transform_cache reentry.
            let cid = klipp_core::ClipId(*clip_id);
            let host_clip = seq.tracks.iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == cid)?;
            tracker_position_at_playhead(host_clip, *point_index, playhead, fps)
        }
    }
}

/// Build a position-only [`ResolvedTransform`] from a tracker's
/// per-frame `(x, y)` output. Used by
/// [`resolve_clip_transform_depth`] when a `transform_link.target`
/// is `Tracker { ... }`.
///
/// Tracker positions are in normalised clip-space `[0, 1]`. The
/// `compose_transform` step that consumes this synthetic target
/// expects pixel-domain `position_x` / `position_y` (centre-relative)
/// — so we convert with `(tracker - 0.5) * frame_dim`. Identity
/// for scale (1.0) / rotation (0.0) / anchor (0.5, 0.5) so a link
/// configured with `inherit_position=true, inherit_scale=false,
/// inherit_rotation=false` correctly transports only the tracker
/// position; the consumer's own scale/rotation/anchor survive.
pub fn synthesise_tracker_transform(tracker_x_norm: f32, tracker_y_norm: f32, frame_w: u32, frame_h: u32) -> ResolvedTransform {
    let pos_x = (tracker_x_norm - 0.5) * frame_w as f32;
    let pos_y = (tracker_y_norm - 0.5) * frame_h as f32;
    (pos_x, pos_y, 1.0, 1.0, 0.0, 0.5, 0.5)
}

/// Look up a tracker's `(focal_x, focal_y)` in normalised clip-
/// space at the given timeline `playhead`. The tracker lives on
/// `host_clip.track_points[point_index]`, and its `positions`
/// vector indexes from `start_frame` (not from `clip.timeline_in`)
/// so the lookup is:
///
/// ```text
///   playhead_secs    = (playhead - host_clip.timeline_in).to_secs_f64()
///   playhead_frame   = (playhead_secs * fps).floor() as i64
///   pos_idx          = playhead_frame - tp.start_frame   (signed!)
///   if pos_idx < 0:                  → before start, return None
///   if pos_idx >= positions.len():   → past end,    return None
///   else:                             positions[pos_idx]
/// ```
///
/// The signed `pos_idx` matters: `start_frame` is `usize` but the
/// playhead can land before it (negative offset), and that should
/// fall back silently — not panic on `usize` underflow.
///
/// Returns `None` for any failure (point_index out of range,
/// before `start_frame`, past end of `positions`, or empty
/// `positions` vec). Caller treats `None` as "use static
/// fallback" — same contract as the Glyph arm.
pub fn tracker_position_at_playhead(
    host_clip: &klipp_timeline::Clip,
    point_index: u32,
    playhead: klipp_core::Timecode,
    fps: f64,
) -> Option<(f32, f32)> {
    let tp = host_clip.track_points.get(point_index as usize)?;
    if tp.positions.is_empty() {
        return None;
    }
    // Frame index relative to the host clip's timeline_in. fps
    // matches what the rest of the resolver uses; falls out of
    // the engine's per-frame call.
    let clip_local_secs = (playhead - host_clip.timeline_in).to_secs_f64();
    let playhead_frame = (clip_local_secs * fps).floor() as i64;
    let pos_idx_signed = playhead_frame - tp.start_frame as i64;
    if pos_idx_signed < 0 {
        return None; // before tracker started
    }
    let pos_idx = pos_idx_signed as usize;
    let (x, y) = *tp.positions.get(pos_idx)?; // past end → None
    Some((x as f32, y as f32))
}


/// Evaluate the effective clip-level opacity at a timeline position.
/// Combines the static `BuiltinEffect::Opacity` value with opacity keyframes
/// and any modifier stack on `ParamId::Opacity`. Returns a value in [0.0, 1.0].
pub fn evaluate_clip_opacity(
    clip: &klipp_timeline::Clip,
    playhead: klipp_core::Timecode,
    fps: f64,
) -> f32 {
    let opacity_default = clip
        .builtin_effects
        .iter()
        .find_map(|e| match e {
            klipp_effects::BuiltinEffect::Opacity { value } => Some(*value),
            _ => None,
        })
        .unwrap_or(1.0);
    let clip_relative_secs = (playhead - clip.timeline_in).to_secs_f64().max(0.0);
    let eval_ctx = klipp_core::EvalContext {
        clip_time_secs: clip_relative_secs,
        clip_duration_secs: (clip.source_out.to_secs_f64() - clip.source_in.to_secs_f64()),
        timeline_time_secs: playhead.to_secs_f64(),
        fps,
        seed: clip.id.0.as_u64_pair().0,
    };
    eval_with_modifiers(
        &clip.opacity_keyframes,
        &clip.param_modifiers,
        &klipp_core::ParamId::Opacity,
        clip_relative_secs,
        opacity_default,
        &eval_ctx,
    )
    .clamp(0.0, 1.0)
}

/// Evaluate an effect param with optional modifier stack.
pub fn eval_effect_with_modifiers(
    keyframes: &[klipp_core::Keyframe],
    modifiers: &std::collections::HashMap<klipp_core::ParamId, klipp_core::ModifierStack>,
    effect_index: usize,
    param_name: &str,
    time_secs: f64,
    default: f32,
    ctx: &klipp_core::EvalContext,
) -> f32 {
    let param = klipp_core::ParamId::Effect {
        effect_index,
        param_name: param_name.to_string(),
    };
    match modifiers.get(&param) {
        Some(stack) if !stack.modifiers.is_empty() => {
            klipp_core::evaluate_param(keyframes, stack, time_secs, default, ctx)
        }
        _ => klipp_core::keyframe::evaluate(keyframes, time_secs, default),
    }
}


#[cfg(test)]
mod tracker_resolution_tests {
    //! HZ-MATTE-NEXT-TRACKER-DRIVER — engine resolver tests.
    //!
    //! Pins the load-bearing pieces of commit 2:
    //!
    //! * `tracker_position_at_playhead` — the boundary behaviour at
    //!   `start_frame`, before / at / mid / exactly-end / one-past-
    //!   end, plus the empty-positions and missing-point-index
    //!   cases. Audit handoff implementation note (iii).
    //!
    //! * `TransformLinkTarget::Tracker` resolves through
    //!   `resolve_clip_transform_depth` and synthesises a position-
    //!   only `ResolvedTransform` with identity scale/rotation/anchor
    //!   so the per-axis inherit flags work cleanly.
    //!
    //! * Behavior-emerges-from-composition: a matte source clip with
    //!   `transform_link.target = Tracker(...)` produces a tracker-
    //!   driven matte composition without any `MatteBinding` code
    //!   path being touched. Audit handoff implementation note (ii).
    use super::{
        resolve_clip_transform, synthesise_tracker_transform,
        tracker_position_at_playhead, ResolvedTransform,
    };
    use klipp_core::{ClipId, MediaId, RationalTime, TrackPoint};
    use klipp_timeline::{
        Clip, MatteBinding, Sequence, TransformLink, TransformLinkTarget, Track, TrackKind,
    };
    use std::collections::HashMap;

    fn tc(frames: i64) -> RationalTime { RationalTime::from_frames(frames, 24) }

    /// Build a TrackPoint with the given `start_frame` and a known
    /// 4-position vector at frames `[start_frame .. start_frame+4]`:
    ///   frame 0 → (0.10, 0.10)
    ///   frame 1 → (0.20, 0.20)
    ///   frame 2 → (0.30, 0.30)
    ///   frame 3 → (0.40, 0.40)
    fn tp_with_positions(start_frame: usize) -> TrackPoint {
        // `TrackPoint::default()` produces a fresh uuid + safe
        // defaults; we override only the fields the tests assert
        // on. Avoids pulling `uuid` into klipp-engine's dev-deps
        // just to build test fixtures.
        
        TrackPoint {
            positions: vec![(0.10, 0.10), (0.20, 0.20), (0.30, 0.30), (0.40, 0.40)],
            start_frame,
            ..Default::default()
        }
    }

    fn host_clip_with_tracker(start_frame: usize) -> Clip {
        let mut c = Clip::media(MediaId::new(), tc(0), tc(120), tc(0), tc(120));
        c.track_points.push(tp_with_positions(start_frame));
        c
    }

    // ─────── Boundary 5-case (audit handoff note iii) ───────

    #[test]
    fn tracker_lookup_before_start_frame_returns_none() {
        // start_frame=10. Playhead at frame 5 → before tracker
        // started → None (caller falls back to static anchor).
        let host = host_clip_with_tracker(10);
        let out = tracker_position_at_playhead(&host, 0, tc(5), 24.0);
        assert!(out.is_none(), "before start_frame → None");
    }

    #[test]
    fn tracker_lookup_at_start_frame_returns_first_position() {
        // start_frame=10. Playhead at frame 10 → positions[0].
        let host = host_clip_with_tracker(10);
        let out = tracker_position_at_playhead(&host, 0, tc(10), 24.0).unwrap();
        assert!((out.0 - 0.10).abs() < 1e-5);
        assert!((out.1 - 0.10).abs() < 1e-5);
    }

    #[test]
    fn tracker_lookup_mid_range_returns_indexed_position() {
        // Playhead at frame 12 → pos_idx=2 → positions[2] = (0.30, 0.30).
        let host = host_clip_with_tracker(10);
        let out = tracker_position_at_playhead(&host, 0, tc(12), 24.0).unwrap();
        assert!((out.0 - 0.30).abs() < 1e-5);
        assert!((out.1 - 0.30).abs() < 1e-5);
    }

    #[test]
    fn tracker_lookup_exactly_at_end_returns_last_position() {
        // 4 positions, indices 0..=3. start_frame=10, playhead at
        // frame 13 → pos_idx=3 → positions[3] = (0.40, 0.40).
        let host = host_clip_with_tracker(10);
        let out = tracker_position_at_playhead(&host, 0, tc(13), 24.0).unwrap();
        assert!((out.0 - 0.40).abs() < 1e-5);
        assert!((out.1 - 0.40).abs() < 1e-5);
    }

    #[test]
    fn tracker_lookup_one_past_end_returns_none() {
        // pos_idx=4 is past the end (positions.len() == 4).
        let host = host_clip_with_tracker(10);
        let out = tracker_position_at_playhead(&host, 0, tc(14), 24.0);
        assert!(out.is_none(), "one past end → None");
    }

    #[test]
    fn tracker_lookup_missing_point_index_returns_none() {
        // Defensive — point_index out of range means stale dispatch
        // (tracker removed since FocalRef was set).
        let host = host_clip_with_tracker(10);
        let out = tracker_position_at_playhead(&host, 99, tc(11), 24.0);
        assert!(out.is_none());
    }

    #[test]
    fn tracker_lookup_empty_positions_returns_none() {
        let mut host = host_clip_with_tracker(0);
        host.track_points[0].positions.clear();
        let out = tracker_position_at_playhead(&host, 0, tc(0), 24.0);
        assert!(out.is_none());
    }

    // ─── synthesise_tracker_transform (position-only) ───

    #[test]
    fn synthetic_tracker_transform_is_position_only_centre_relative() {
        // tracker_x=0.5, tracker_y=0.5 → centre of frame → (0, 0)
        // pixel offset; identity scale/rotation/anchor.
        let t = synthesise_tracker_transform(0.5, 0.5, 1920, 1080);
        let (px, py, sx, sy, ro, ax, ay) = t;
        assert!(px.abs() < 1e-5);
        assert!(py.abs() < 1e-5);
        assert!((sx - 1.0).abs() < 1e-5);
        assert!((sy - 1.0).abs() < 1e-5);
        assert!(ro.abs() < 1e-5);
        assert!((ax - 0.5).abs() < 1e-5);
        assert!((ay - 0.5).abs() < 1e-5);
    }

    #[test]
    fn synthetic_tracker_transform_offsets_from_centre() {
        // tracker (0.75, 0.25) → +25% right, -25% up of frame.
        // (0.75-0.5) * 1920 = +480 px; (0.25-0.5) * 1080 = -270 px.
        let t = synthesise_tracker_transform(0.75, 0.25, 1920, 1080);
        assert!((t.0 - 480.0).abs() < 1e-3);
        assert!((t.1 - -270.0).abs() < 1e-3);
    }

    // ─── transform_link.target = Tracker through resolve_clip_transform ───

    fn seq_with(clips: Vec<Clip>) -> Sequence {
        let mut s = Sequence::new_default();
        let mut tr = Track::new("V1", TrackKind::Video);
        tr.clips = clips;
        s.tracks.push(tr);
        s
    }

    fn resolve(clip: &Clip, seq: &Sequence, playhead: RationalTime) -> ResolvedTransform {
        let mut tcache = HashMap::new();
        let mut gcache = HashMap::new();
        let mut tr = klipp_title::TitleRenderer::new();
        resolve_clip_transform(clip, seq, playhead, 24.0, &mut tcache, &mut gcache, &mut tr, 1920, 1080)
    }

    #[test]
    fn link_to_tracker_resolves_position_at_playhead() {
        // host clip carries the tracker; consumer clip has
        // transform_link.target = Tracker pointing at it.
        let host = host_clip_with_tracker(0);
        let host_id = host.id;
        let mut consumer = Clip::media(MediaId::new(), tc(0), tc(120), tc(0), tc(120));
        consumer.transform_link = Some(TransformLink {
            target: TransformLinkTarget::Tracker { clip_id: host_id, point_index: 0 },
            mode: klipp_timeline::LinkMode::Absolute,
            inherit_scale: false,
            inherit_position: true,
            inherit_rotation: false,
            inherit_anchor: false,
        });
        let consumer_clone = consumer.clone();
        let seq = seq_with(vec![consumer, host]);
        // Playhead at frame 1 → positions[1] = (0.20, 0.20)
        // → centre-relative pixels: (0.20-0.5)*1920 = -576, (0.20-0.5)*1080 = -324
        let out = resolve(&consumer_clone, &seq, tc(1));
        assert!((out.0 - -576.0).abs() < 1e-3, "px = (0.20-0.5)*1920, got {}", out.0);
        assert!((out.1 - -324.0).abs() < 1e-3, "py = (0.20-0.5)*1080, got {}", out.1);
    }

    #[test]
    fn link_to_tracker_falls_back_when_host_clip_missing() {
        let mut consumer = Clip::media(MediaId::new(), tc(0), tc(120), tc(0), tc(120));
        consumer.transform_link = Some(TransformLink {
            target: TransformLinkTarget::Tracker { clip_id: ClipId::new(), point_index: 0 },
            mode: klipp_timeline::LinkMode::Absolute,
            inherit_scale: true,
            inherit_position: true,
            inherit_rotation: true,
            inherit_anchor: true,
        });
        let consumer_clone = consumer.clone();
        let seq = seq_with(vec![consumer]);
        // Resolution fails → falls back to consumer's own transform
        // (identity here since no Transform builtin).
        let out = resolve(&consumer_clone, &seq, tc(0));
        // Identity own transform: (0,0,1,1,0,0.5,0.5)
        assert!(out.0.abs() < 1e-5);
        assert!(out.1.abs() < 1e-5);
        assert!((out.2 - 1.0).abs() < 1e-5);
    }

    #[test]
    fn link_to_tracker_falls_back_before_start_frame() {
        // start_frame=10, playhead at frame 5 → before start.
        // Resolution fails → fallback to own transform.
        let host = host_clip_with_tracker(10);
        let host_id = host.id;
        let mut consumer = Clip::media(MediaId::new(), tc(0), tc(120), tc(0), tc(120));
        consumer.transform_link = Some(TransformLink {
            target: TransformLinkTarget::Tracker { clip_id: host_id, point_index: 0 },
            mode: klipp_timeline::LinkMode::Absolute,
            inherit_scale: true,
            inherit_position: true,
            inherit_rotation: true,
            inherit_anchor: true,
        });
        let consumer_clone = consumer.clone();
        let seq = seq_with(vec![consumer, host]);
        let out = resolve(&consumer_clone, &seq, tc(5));
        // Identity own transform (no Transform builtin on consumer).
        assert!(out.0.abs() < 1e-5, "fell back to own px=0");
        assert!(out.1.abs() < 1e-5, "fell back to own py=0");
    }

    // ── Note (ii): behavior-emerges-from-composition for matte ──

    #[test]
    fn matte_source_with_tracker_link_is_tracker_driven_via_composition() {
        // Pin the load-bearing audit finding: a matte source clip
        // with transform_link.target = Tracker(...) produces
        // tracker-driven matte composition WITHOUT any MatteBinding
        // code path being touched. The matte source's resolved
        // transform (which the engine reads when extracting the
        // alpha mask) IS the tracker output, by virtue of the
        // existing transform_link composition machinery.
        //
        // We don't render pixels here — we verify the resolver
        // contract: matte_source clip's resolved transform must
        // equal the tracker output (per-frame), with no
        // MatteBinding involvement.
        let host = host_clip_with_tracker(0);
        let host_id = host.id;
        // Mask source clip — pretend it's a matte source someone
        // else (the consumer) is binding via MatteBinding. From
        // the resolver's perspective it's just a clip with a
        // transform_link to a tracker.
        let mut mask_source = Clip::media(MediaId::new(), tc(0), tc(120), tc(0), tc(120));
        mask_source.transform_link = Some(TransformLink {
            target: TransformLinkTarget::Tracker { clip_id: host_id, point_index: 0 },
            mode: klipp_timeline::LinkMode::Absolute,
            inherit_scale: false,
            inherit_position: true,
            inherit_rotation: false,
            inherit_anchor: false,
        });
        // Hypothetical consumer with a MatteBinding pointing at
        // mask_source. The MatteBinding itself is unchanged
        // schema; we're pinning that no matte code path is touched
        // for this contract.
        let mut consumer = Clip::media(MediaId::new(), tc(0), tc(120), tc(0), tc(120));
        consumer.matte_source = Some(MatteBinding::new(mask_source.id));
        let mask_source_clone = mask_source.clone();
        let seq = seq_with(vec![consumer, mask_source, host]);

        // At frame 2 → tracker positions[2] = (0.30, 0.30)
        // → mask_source's resolved px/py = (-0.20*1920, -0.20*1080)
        //   = (-384, -216).
        let out = resolve(&mask_source_clone, &seq, tc(2));
        assert!((out.0 - -384.0).abs() < 1e-3,
            "mask source px follows tracker: expected -384, got {}", out.0);
        assert!((out.1 - -216.0).abs() < 1e-3,
            "mask source py follows tracker: expected -216, got {}", out.1);

        // Pin the integration: the consumer's matte_source binding
        // exists and points at the mask_source — no MatteBinding
        // schema field changed, the matte path will read this same
        // resolved transform when extracting alpha, and the
        // tracker movement transports through naturally. We don't
        // re-derive the matte composition here (out of scope for
        // a unit test) but the consumer's matte_source field is
        // intact and the mask_source's own resolved transform IS
        // tracker-driven — which is the load-bearing claim.
        let consumer_in_seq = seq.tracks.iter().flat_map(|t| t.clips.iter())
            .find(|c| c.matte_source.is_some()).unwrap();
        assert_eq!(
            consumer_in_seq.matte_source.as_ref().unwrap().source_clip_id,
            mask_source_clone.id,
            "MatteBinding shape unchanged — emerges via transform_link, not a new matte field"
        );
    }
}

#[cfg(test)]
mod transform_link_compose_tests {
    //! Pure-function tests for `compose_transform` (per-axis inherit
    //! flags + Absolute/Additive mode) and `resolve_clip_transform`
    //! (chain walk + missing-target fallback). The integration into
    //! `update_overlay_decoders` / `apply_clip_effects` is exercised
    //! at a higher level — these pin the math.
    use super::{compose_transform, resolve_clip_transform, ResolvedTransform};
    use klipp_core::{ClipId, MediaId, RationalTime, Timecode};
    use klipp_timeline::{Clip, LinkMode, Sequence, Track, TrackKind, TransformLink, TransformLinkTarget};
    use klipp_effects::BuiltinEffect;

    fn tc(frames: i64) -> Timecode { RationalTime::from_frames(frames, 24) }

    fn link_all_on(target: ClipId, mode: LinkMode) -> TransformLink {
        TransformLink {
            target: TransformLinkTarget::Clip(target),
            mode,
            inherit_scale: true,
            inherit_position: true,
            inherit_rotation: true,
            inherit_anchor: true,
        }
    }

    fn approx_eq(a: ResolvedTransform, b: ResolvedTransform) {
        let (a0, a1, a2, a3, a4, a5, a6) = a;
        let (b0, b1, b2, b3, b4, b5, b6) = b;
        let eps = 1e-5;
        assert!((a0 - b0).abs() < eps, "px {a0} vs {b0}");
        assert!((a1 - b1).abs() < eps, "py {a1} vs {b1}");
        assert!((a2 - b2).abs() < eps, "sx {a2} vs {b2}");
        assert!((a3 - b3).abs() < eps, "sy {a3} vs {b3}");
        assert!((a4 - b4).abs() < eps, "ro {a4} vs {b4}");
        assert!((a5 - b5).abs() < eps, "ax {a5} vs {b5}");
        assert!((a6 - b6).abs() < eps, "ay {a6} vs {b6}");
    }

    #[test]
    fn compose_absolute_replaces_every_inherited_axis() {
        let own:    ResolvedTransform = (10.0, 20.0, 1.5, 1.5, 30.0, 0.5, 0.5);
        let target: ResolvedTransform = (100.0, 200.0, 2.0, 2.0, 90.0, 0.25, 0.75);
        let link = link_all_on(ClipId::new(), LinkMode::Absolute);
        let out = compose_transform(own, target, &link);
        // Position / scale / rotation come from target. Anchor likewise
        // (always Absolute). Own values are dropped entirely.
        approx_eq(out, (100.0, 200.0, 2.0, 2.0, 90.0, 0.25, 0.75));
    }

    #[test]
    fn compose_additive_adds_pos_rot_and_multiplies_scale() {
        let own:    ResolvedTransform = (10.0, 20.0, 1.5, 1.5, 30.0, 0.5, 0.5);
        let target: ResolvedTransform = (100.0, 200.0, 2.0, 2.0, 90.0, 0.25, 0.75);
        let link = link_all_on(ClipId::new(), LinkMode::Additive);
        let out = compose_transform(own, target, &link);
        // Position adds. Scale multiplies. Rotation adds. Anchor
        // remains Absolute regardless of mode.
        approx_eq(out, (110.0, 220.0, 3.0, 3.0, 120.0, 0.25, 0.75));
    }

    #[test]
    fn compose_per_axis_flags_off_keep_own() {
        let own:    ResolvedTransform = (10.0, 20.0, 1.5, 1.5, 30.0, 0.5, 0.5);
        let target: ResolvedTransform = (100.0, 200.0, 2.0, 2.0, 90.0, 0.25, 0.75);
        let link = TransformLink {
            target: TransformLinkTarget::Clip(ClipId::new()),
            mode: LinkMode::Absolute,
            inherit_scale: false,
            inherit_position: false,
            inherit_rotation: false,
            inherit_anchor: false,
        };
        let out = compose_transform(own, target, &link);
        // Every flag off → composer is the identity over `own`.
        approx_eq(out, own);
    }

    #[test]
    fn compose_mixed_flags_inherit_only_position() {
        let own:    ResolvedTransform = (10.0, 20.0, 1.5, 1.5, 30.0, 0.5, 0.5);
        let target: ResolvedTransform = (100.0, 200.0, 2.0, 2.0, 90.0, 0.25, 0.75);
        let link = TransformLink {
            target: TransformLinkTarget::Clip(ClipId::new()),
            mode: LinkMode::Absolute,
            inherit_scale: false,
            inherit_position: true,
            inherit_rotation: false,
            inherit_anchor: false,
        };
        let out = compose_transform(own, target, &link);
        // Only px/py from target; rest from own.
        approx_eq(out, (100.0, 200.0, 1.5, 1.5, 30.0, 0.5, 0.5));
    }

    fn clip_with_transform(px: f32, py: f32, sc: f32, ro: f32, ax: f32, ay: f32) -> Clip {
        let mut c = Clip::media(MediaId::new(), tc(0), tc(48), tc(0), tc(48));
        c.builtin_effects.push(BuiltinEffect::Transform {
            position_x: px, position_y: py,
            scale_x: sc, scale_y: sc,
            rotation: ro,
            anchor_x: ax, anchor_y: ay,
            rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0,
            focal_ref: None,
        });
        c.builtin_effects_enabled.push(true);
        c
    }

    fn seq_with_clips(clips: Vec<Clip>) -> Sequence {
        let mut s = Sequence::new_default();
        let mut tr = Track::new("V1", TrackKind::Video);
        tr.clips = clips;
        s.tracks.push(tr);
        s
    }

    /// Test wrapper around `resolve_clip_transform` that supplies the
    /// extra glyph-resolver args (cache, renderer, frame dims) with
    /// safe defaults. Lets the existing transform_link compose tests
    /// stay focused on their original concern (link composition)
    /// without each test re-constructing a TitleRenderer.
    fn resolve_for_test(
        c: &Clip, seq: &Sequence,
        cache: &mut std::collections::HashMap<klipp_core::ClipId, ResolvedTransform>,
    ) -> ResolvedTransform {
        let mut glyph_cache = std::collections::HashMap::new();
        let mut tr = klipp_title::TitleRenderer::new();
        resolve_clip_transform(c, seq, tc(0), 24.0, cache, &mut glyph_cache, &mut tr, 1920, 1080)
    }

    #[test]
    fn resolve_no_link_returns_own_transform() {
        let c = clip_with_transform(7.0, 11.0, 1.25, 45.0, 0.5, 0.5);
        let seq = seq_with_clips(vec![c.clone()]);
        let mut cache = std::collections::HashMap::new();
        let out = resolve_for_test(&c, &seq, &mut cache);
        approx_eq(out, (7.0, 11.0, 1.25, 1.25, 45.0, 0.5, 0.5));
    }

    #[test]
    fn resolve_absolute_link_inherits_target_pos_scale_rotation() {
        // child links to parent with all-on Absolute. Resolved values
        // should equal parent's, ignoring child's own.
        let parent = clip_with_transform(100.0, 200.0, 2.0, 90.0, 0.3, 0.7);
        let mut child = clip_with_transform(5.0, 6.0, 0.5, 10.0, 0.5, 0.5);
        child.transform_link = Some(link_all_on(parent.id, LinkMode::Absolute));
        let parent_id = parent.id;
        let child_clone = child.clone();
        let seq = seq_with_clips(vec![parent, child]);
        let mut cache = std::collections::HashMap::new();
        let out = resolve_for_test(&child_clone, &seq, &mut cache);
        approx_eq(out, (100.0, 200.0, 2.0, 2.0, 90.0, 0.3, 0.7));
        // Cache populated for both clips so a second call is O(1).
        assert!(cache.contains_key(&parent_id));
        assert!(cache.contains_key(&child_clone.id));
    }

    #[test]
    fn resolve_additive_link_composes_per_mode() {
        let parent = clip_with_transform(100.0, 200.0, 2.0, 90.0, 0.3, 0.7);
        let mut child = clip_with_transform(5.0, 6.0, 0.5, 10.0, 0.5, 0.5);
        child.transform_link = Some(link_all_on(parent.id, LinkMode::Additive));
        let child_clone = child.clone();
        let seq = seq_with_clips(vec![parent, child]);
        let mut cache = std::collections::HashMap::new();
        let out = resolve_for_test(&child_clone, &seq, &mut cache);
        // pos: 5+100, 6+200; scale: 0.5*2; rot: 10+90; anchor: parent (always Absolute).
        approx_eq(out, (105.0, 206.0, 1.0, 1.0, 100.0, 0.3, 0.7));
    }

    #[test]
    fn resolve_falls_back_to_own_when_target_is_missing() {
        // Stale link: target id doesn't exist in the sequence.
        // Resolver must drop the link composition and return own.
        let mut child = clip_with_transform(5.0, 6.0, 0.5, 10.0, 0.5, 0.5);
        child.transform_link = Some(link_all_on(ClipId::new(), LinkMode::Absolute));
        let child_clone = child.clone();
        let seq = seq_with_clips(vec![child]);
        let mut cache = std::collections::HashMap::new();
        let out = resolve_for_test(&child_clone, &seq, &mut cache);
        approx_eq(out, (5.0, 6.0, 0.5, 0.5, 10.0, 0.5, 0.5));
    }

    #[test]
    fn resolve_three_clip_chain_composes_through_intermediate() {
        // a → b → c (Absolute through). a's resolved transform should
        // equal c's pos/scale/rot.
        let c = clip_with_transform(50.0, 60.0, 4.0, 180.0, 0.1, 0.9);
        let mut b = clip_with_transform(20.0, 30.0, 2.0, 45.0, 0.5, 0.5);
        b.transform_link = Some(link_all_on(c.id, LinkMode::Absolute));
        let mut a = clip_with_transform(1.0, 2.0, 0.5, 5.0, 0.5, 0.5);
        a.transform_link = Some(link_all_on(b.id, LinkMode::Absolute));
        let a_clone = a.clone();
        let seq = seq_with_clips(vec![a, b, c]);
        let mut cache = std::collections::HashMap::new();
        let out = resolve_for_test(&a_clone, &seq, &mut cache);
        approx_eq(out, (50.0, 60.0, 4.0, 4.0, 180.0, 0.1, 0.9));
    }
}
