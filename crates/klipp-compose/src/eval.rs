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

/// Why a clip's contents can't be edited on the canvas at a moment: its
/// picture is placed in a way the canvas editor can't follow as a flat
/// move (#2014). Each says what to do instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotFollowed {
    /// Tilted in perspective.
    Tilted,
    /// Scaled to nothing, or nearly.
    ScaledAway,
    /// Its transform is linked to another clip's.
    Linked,
    /// Its track's own effects move it again.
    TrackMoves,
    /// An adjustment layer over it moves it again.
    AdjustmentMoves,
    /// A transition that moves or distorts its pictures is running over it.
    InTransition,
}

impl NotFollowed {
    /// The hint the canvas shows in place of the handles.
    pub fn hint(self) -> &'static str {
        match self {
            Self::Tilted => "This title is tilted: edit its layers in the Title panel, or untilt it to drag them here",
            Self::ScaledAway => "This title is scaled to nothing here: move the playhead, or edit its layers in the Title panel",
            Self::Linked => "This title's transform follows another clip's: edit its layers in the Title panel",
            Self::TrackMoves => "This title's track moves it with a Transform: edit its layers in the Title panel",
            Self::AdjustmentMoves => "An adjustment layer above moves this title: edit its layers in the Title panel",
            Self::InTransition => "A transition is moving this title here: move the playhead out of it, or edit its layers in the Title panel",
        }
    }
}

/// Whether `t` places a picture where it is: no move, scale or turn.
fn is_identity(t: ResolvedTransform) -> bool {
    let (px, py, sx, sy, ro, _, _) = t;
    px.abs() < 1e-4 && py.abs() < 1e-4 && (sx - 1.0).abs() < 1e-5 && (sy - 1.0).abs() < 1e-5 && ro.abs() < 1e-4
}

/// Whether a transition of `kind` leaves its pictures where they are:
/// it blends, dips, tints or wipes between them, and nothing in it moves,
/// scales or distorts them. The rest are refused, not guessed.
fn keeps_place(kind: klipp_core::TransitionKind) -> bool {
    use klipp_core::TransitionKind::*;
    matches!(
        kind,
        CrossDissolve | DipToBlack | DipToWhite | FilmDissolve | AdditiveDissolve | LuminanceDissolve | FadeToColor
            | Flash | NegativeFlash | ColorizeFlash | Desaturate | Sepia | HueRotate
            | WipeLeft | WipeRight | WipeUp | WipeDown | CircleWipeIn | CircleWipeOut | ClockWipe | BarnDoorOpen | BarnDoorClose
            | StarWipe | SplitWipeHorizontal | SplitWipeVertical | VenetianBlindHorizontal | VenetianBlindVertical
            | CheckerboardWipe | DiagonalWipeTLBR | DiagonalWipeTRBL | BoxWipeIn | BoxWipeOut
    )
}

/// Whether a Transform effect in a list moves what it is on.
fn moves(fx: &[klipp_effects::BuiltinEffect]) -> bool {
    fx.iter().any(|e| match e {
        klipp_effects::BuiltinEffect::Transform { position_x, position_y, scale_x, scale_y, rotation, rotation_x, rotation_y, .. } => {
            position_x.abs() > 1e-4 || position_y.abs() > 1e-4 || (scale_x - 1.0).abs() > 1e-5 || (scale_y - 1.0).abs() > 1e-5 || rotation.abs() > 1e-4 || rotation_x.abs() > 1e-4 || rotation_y.abs() > 1e-4
        }
        _ => false,
    })
}

/// Where `clip`'s picture is placed on the frame at `playhead`, as the
/// renderer places it, for editing its contents on the canvas (#2014): its
/// own transform, then the sequence's camera seen from its depth, in the
/// sequence's pixels. `Ok(None)`: not moved at all. `Err`: placed in a way
/// the canvas can't follow as a flat move, and why.
pub fn placement_for_editing(clip: &klipp_timeline::Clip, seq: &klipp_timeline::Sequence, playhead: klipp_core::Timecode) -> Result<Option<ResolvedTransform>, NotFollowed> {
    let fps = seq.fps.as_f64();
    if clip.transform_link.is_some() {
        return Err(NotFollowed::Linked);
    }
    let elapsed = (playhead - clip.timeline_in).to_secs_f64().max(0.0);
    let ctx = eval_context(clip, playhead, fps);
    let own = evaluate_clip_own_transform(clip, elapsed, &ctx);
    // A title is picture-truth: its tilt is in its picture, not a list.
    let (rx, ry, _) = crate::render::tilt(clip, &[], elapsed);
    if rx.abs() > 1e-4 || ry.abs() > 1e-4 {
        return Err(NotFollowed::Tilted);
    }
    let (w, h) = (seq.resolution.width, seq.resolution.height);
    let placed = match camera_at(seq, playhead).and_then(|cam| camera_at_depth(cam, clip.depth)) {
        Some(cam) => apply_camera(own, cam, w, h),
        None => own,
    };
    if placed.2.abs() < 1e-3 || placed.3.abs() < 1e-3 {
        return Err(NotFollowed::ScaledAway);
    }
    // What moves it again after its own placement: its track's effects, or
    // an adjustment layer composited over it.
    let layers = crate::render::active_layers(seq, playhead);
    let Some(at) = layers.iter().position(|l| l.clips.iter().any(|c| c.id == clip.id)) else {
        return Ok((!is_identity(placed)).then_some(placed));
    };
    if layers[at].transition {
        let kind = seq.tracks.get(layers[at].track).and_then(|t| crate::render::running_transition(seq, t, playhead));
        if !kind.is_some_and(keeps_place) {
            return Err(NotFollowed::InTransition);
        }
    }
    if let Some(track) = seq.tracks.get(layers[at].track).filter(|t| t.effects.touches_picture()) {
        // The track's effects as the renderer lays them on the clip: baked
        // onto a copy of it, each with its track's flag. A Transform there
        // that its own list hasn't moves it again.
        let mut probe = klipp_timeline::Sequence::new_default();
        let mut alone = klipp_timeline::Track::new(&track.name, track.kind);
        alone.effects = track.effects.clone();
        alone.clips.push(clip.clone());
        probe.tracks = vec![alone];
        probe.bake_track_effects();
        let (before, _) = crate::render::clip_effects(clip, playhead, fps, None, None);
        let (after, _) = crate::render::clip_effects(&probe.tracks[0].clips[0], playhead, fps, None, None);
        if !moves(&before) && moves(&after) {
            return Err(NotFollowed::TrackMoves);
        }
    }
    // Only this sequence's own layers: a precomposition's inner layers
    // follow it in the list, but they adjust its scene, not the frame.
    let own = |c: &klipp_timeline::Clip, track: usize| seq.tracks.get(track).is_some_and(|t| t.clips.iter().any(|x| std::ptr::eq(x, c)));
    for layer in &layers[at + 1..] {
        for c in layer.clips.iter().filter(|c| matches!(c.kind, klipp_timeline::ClipKind::AdjustmentLayer) && own(c, layer.track)) {
            // Its placement keys, or a Transform among its effects.
            let t = (layer.at - c.timeline_in).to_secs_f64().max(0.0);
            let own = evaluate_clip_own_transform(c, t, &eval_context(c, layer.at, layer.fps));
            let (fx, _) = crate::render::clip_effects(c, layer.at, layer.fps, None, None);
            if !is_identity(own) || moves(&fx) {
                return Err(NotFollowed::AdjustmentMoves);
            }
        }
    }
    Ok((!is_identity(placed)).then_some(placed))
}

/// Where a pinned clip's corner pin puts its corners at `time`, in
/// fractions of the frame (#684): the surface's corners on the host's
/// picture shown then, carried through the host's fit and placement
/// (`placement` when the caller resolved it, else the host's own), and
/// the pinned picture's fitted rectangle mapped onto them. `None` when
/// the clip is not pinned, or its surface is gone or untracked, and its
/// corner pin keeps its own corners, the reference frame's.
///
/// The host is the clip carrying the surface under `time`, preferring
/// the one the pin names: a split host's halves both carry it.
pub fn surface_pin_corners(
    clip: &klipp_timeline::Clip,
    seq: &klipp_timeline::Sequence,
    time: klipp_core::Timecode,
    fps: f64,
    placement: &dyn Fn(&klipp_timeline::Clip) -> Option<ResolvedTransform>,
) -> Option<klipp_timeline::surface::Quad> {
    let pin = clip.surface_pin?;
    let carries = |c: &&klipp_timeline::Clip| c.surfaces.iter().any(|s| s.id == pin.surface);
    let under = |c: &&klipp_timeline::Clip| c.timeline_in <= time && time < c.timeline_out;
    let clips = || seq.tracks.iter().flat_map(|t| t.clips.iter());
    let host = clips()
        .find(|c| c.id == pin.host && carries(c) && under(c))
        .or_else(|| clips().find(|c| carries(c) && under(c)))
        .or_else(|| clips().find(|c| c.id == pin.host && carries(c)))?;
    let surface = host.surfaces.iter().find(|s| s.id == pin.surface)?;
    if surface.keys.is_empty() {
        return None;
    }
    let elapsed = (time - host.timeline_in).to_secs_f64();
    let on_source = surface.corners_shown_at(host.source_secs_at(elapsed, fps));
    let frame = [seq.resolution.width, seq.resolution.height];
    let placed = placement(host).unwrap_or_else(|| evaluate_clip_own_transform(host, elapsed.max(0.0), &eval_context(host, time, fps)));
    let on_frame = klipp_timeline::surface::source_quad_in_frame(&on_source, surface.source_size, frame, Some(placed));
    let target = klipp_timeline::surface::pin_target(&on_frame, surface.aspect(), pin.picture_size, pin.fit, pin.inset)?;
    klipp_timeline::surface::pin_frame_corners(&target, pin.picture_size, frame)
}

/// Evaluate a single clip's *own* transform — keyframes + modifiers
/// resolved against the clip's `BuiltinEffect::Transform` defaults
/// (or the conventional `(0,0,1,1,0,0.5,0.5)` if no enabled Transform effect
/// exists). This is the pre-link version; callers that respect
/// `transform_link` should go through `resolve_clip_transform`
/// instead.
pub fn evaluate_clip_own_transform(
    clip: &klipp_timeline::Clip,
    clip_time_secs: f64,
    eval_ctx: &klipp_core::EvalContext,
) -> ResolvedTransform {
    // A picture-truth clip's fields are only a compatibility view. Auto
    // placement can be carried there by a disabled synthetic Transform;
    // its authoritative properties must not be mistaken for a bypassed
    // authored effect (#1207).
    let picture = clip.is_picture_truth().then(|| clip.picture_v2().ok()).flatten();
    // The scale and the aspect its height keeps under it (#1406).
    let tdef = picture.as_ref().map(|p| {
        let p = &p.picture.placement;
        (p.pos_x.unwrap_or(0.0), p.pos_y.unwrap_or(0.0), p.scale.unwrap_or(1.0), p.aspect.unwrap_or(1.0),
            p.rotation.unwrap_or(0.0), p.anchor_x.unwrap_or(0.5), p.anchor_y.unwrap_or(0.5))
    }).unwrap_or_else(|| enabled_transform(clip).into_iter()
        .find_map(|e| match e {
            klipp_effects::BuiltinEffect::Transform {
                position_x, position_y, scale_x, scale_y, rotation, anchor_x, anchor_y, ..
            } => Some((*position_x, *position_y, *scale_x, klipp_graph_lift::names::aspect(Some(*scale_x), Some(*scale_y)).unwrap_or(1.0), *rotation, *anchor_x, *anchor_y)),
            _ => None,
        })
        .unwrap_or((0.0, 0.0, 1.0, 1.0, 0.0, 0.5, 0.5)));
    // Each lane where the clip's placement edits land: a picture-truth
    // clip's in its picture, any other clip's in its fields.
    let lane = |param: klipp_core::ParamId, view: &[klipp_core::Keyframe], default: f32| match clip.truth_lane(&param) {
        Some((keys, modifiers)) => eval_with_modifiers(&keys, &modifiers, &param, clip_time_secs, default, eval_ctx),
        None => eval_with_modifiers(view, clip.param_modifiers(), &param, clip_time_secs, default, eval_ctx),
    };
    let px = lane(klipp_core::ParamId::PositionX, clip.transform_pos_x_keyframes(), tdef.0);
    let py = lane(klipp_core::ParamId::PositionY, clip.transform_pos_y_keyframes(), tdef.1);
    let sc = lane(klipp_core::ParamId::Scale, clip.transform_scale_keyframes(), tdef.2);
    let ro = lane(klipp_core::ParamId::Rotation, clip.transform_rotation_keyframes(), tdef.4);
    // The scale lane keys the width. The height is its own lane where it
    // has keys (#1471); else it keeps the clip's aspect under the width
    // (#1406), so a width keyed to 0 gives a height of 0.
    let follows = sc * tdef.3;
    let own = clip.truth_lane(&klipp_core::ParamId::ScaleY).map_or_else(|| !clip.transform_scale_y_keyframes().is_empty(), |(keys, _)| !keys.is_empty());
    let sy = if own { lane(klipp_core::ParamId::ScaleY, clip.transform_scale_y_keyframes(), follows) } else { follows };
    (px, py, sc, sy, ro, tdef.5, tdef.6)
}

/// Placement and its focal anchor come from the first enabled Transform.
fn enabled_transform(clip: &klipp_timeline::Clip) -> Option<&klipp_effects::BuiltinEffect> {
    clip.builtin_effects().iter().enumerate().find_map(|(i, effect)| {
        (clip.builtin_effects_enabled().get(i).copied().unwrap_or(true)
            && matches!(effect, klipp_effects::BuiltinEffect::Transform { .. })).then_some(effect)
    })
}

/// A camera resolved at one instant: where it looks (fractions of the
/// frame), how far in, and its roll in degrees.
pub type ResolvedCamera = (f32, f32, f32, f32);

/// The sequence's camera at `playhead`, or `None` when there is none.
/// Reading it costs nothing on a sequence without one.
pub fn camera_at(seq: &klipp_timeline::Sequence, playhead: klipp_core::Timecode) -> Option<ResolvedCamera> {
    camera_at_secs(seq, playhead.to_secs_f64())
}

/// [`camera_at`] at any instant in seconds, not only on a frame: the
/// motion blur samples between frames.
pub fn camera_at_secs(seq: &klipp_timeline::Sequence, secs: f64) -> Option<ResolvedCamera> {
    let cam = &seq.camera;
    if cam.is_still() {
        return None;
    }
    // A camera is keyframes over timeline time and nothing else: no
    // modifiers, because it belongs to no clip whose time they could be
    // driven by.
    let t = secs.max(0.0);
    let at = |kfs: &[klipp_core::Keyframe], default: f32| klipp_core::keyframe::evaluate(kfs, t, default);
    Some((
        at(&cam.x_keyframes, 0.5),
        at(&cam.y_keyframes, 0.5),
        at(&cam.zoom_keyframes, 1.0).max(0.01),
        at(&cam.roll_keyframes, 0.0),
    ))
}

/// The camera as a layer at `depth` sees it, or `None` for a layer on
/// the glass (depth 0), which the camera does not move.
///
/// A pinhole: the layer sits `depth` times as far from the camera as the
/// composed plane. The camera's sideways move shifts it by `1/depth` as
/// much, so far layers drift less. The camera's zoom is a dolly — the
/// camera moving in by `m = 1 - 1/zoom` of the plane's distance — and a
/// layer at `depth` grows by `depth / (depth - m)`: a push-in grows near
/// cards more than far ones, which is what makes a drift read as space
/// rather than a flat picture sliding. At depth 1 it is the camera
/// unchanged.
pub fn camera_at_depth(camera: ResolvedCamera, depth: f32) -> Option<ResolvedCamera> {
    if depth <= 0.0 || !depth.is_finite() {
        return None;
    }
    if (depth - 1.0).abs() < 1e-6 {
        return Some(camera);
    }
    let (look_x, look_y, zoom, roll) = camera;
    let dolly = 1.0 - 1.0 / zoom;
    // A layer the camera has reached or passed would blow up; hold it at
    // twenty times its size instead.
    let grow = depth / (depth - dolly).max(depth * 0.05);
    Some((0.5 + (look_x - 0.5) / depth, 0.5 + (look_y - 0.5) / depth, grow, roll))
}

/// Put a layer's own transform behind the camera.
///
/// The engine's transform maps a source pixel to the screen as
/// `anchor + R(ro)·S(s)·(src - anchor) + p`. Laying a second transform
/// `(r2, s2, p2)` over it about the same anchor gives
/// `rotation ro + r2`, `scale s·s2` and `position R(r2)·S(s2)·p + p2` —
/// so a layer already offset from the middle swings and scales with the
/// move instead of sitting still in the corner, which is the whole
/// difference between a camera and every clip being animated in step.
///
/// The camera looking at `(cx, cy)` with zoom `z` and roll `r` is that
/// second transform with `r2 = -r`, `s2 = z` and `p2 = -R(-r)·S(z)·c`.
pub fn apply_camera(own: ResolvedTransform, camera: ResolvedCamera, frame_w: u32, frame_h: u32) -> ResolvedTransform {
    let (look_x, look_y, zoom, roll) = camera;
    let (own_px, own_py, own_sx, own_sy, own_ro, own_ax, own_ay) = own;
    let (cx, cy) = ((look_x - 0.5) * frame_w as f32, (look_y - 0.5) * frame_h as f32);
    let (sin, cos) = (-roll).to_radians().sin_cos();
    let turn = |x: f32, y: f32| (x * cos - y * sin, x * sin + y * cos);
    let (tx, ty) = turn(-cx * zoom, -cy * zoom);
    let (px, py) = turn(own_px * zoom, own_py * zoom);
    (px + tx, py + ty, own_sx * zoom, own_sy * zoom, own_ro - roll, own_ax, own_ay)
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

    // Glyph-anchored focal-point override. Read the picture's placement,
    // or the first enabled Transform for a field clip; resolve to clip-space
    // `(focal_x, focal_y)` via the title's glyph layout + the
    // title's own resolved transform. Failed resolution falls back
    // to the static `anchor_x` / `anchor_y` (already in `own.5/6`).
    let focal_ref = if clip.is_picture_truth() {
        clip.picture_v2().ok().and_then(|p| p.picture.placement.focal_ref.clone()).and_then(|v| serde_json::from_value(v).ok())
    } else {
        enabled_transform(clip).and_then(|e| match e {
            klipp_effects::BuiltinEffect::Transform { focal_ref, .. } => *focal_ref,
            _ => None,
        })
    };
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
///   playhead_frame   = frame_index(playhead_secs, fps)   (floor, with slack)
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
    let playhead_frame = crate::render::frame_index(clip_local_secs, fps);
    let pos_idx_signed = playhead_frame - tp.start_frame as i64;
    if pos_idx_signed < 0 {
        return None; // before tracker started
    }
    let pos_idx = pos_idx_signed as usize;
    let (x, y) = *tp.positions.get(pos_idx)?; // past end → None
    Some((x as f32, y as f32))
}


/// A clip's compositing weight at `time`, a clip's and an adjustment
/// layer's alike: its last enabled `Opacity` effect (the anchor) over its
/// opacity keyframes and modifiers, else those alone over 1; times every
/// other enabled Opacity of its own, each by its own keys (#1386); times
/// each Opacity its track added. A disabled Opacity counts for nothing. A
/// render copy carries its track's effects after the clip's own
/// (`bake_track_effects`); a track's Opacity weighs the clip's resolved
/// opacity rather than standing in for it (#1078). Returns a value in
/// [0.0, 1.0].
pub fn evaluate_clip_opacity(
    clip: &klipp_timeline::Clip,
    time: klipp_core::Timecode,
    fps: f64,
) -> f32 {
    use klipp_effects::BuiltinEffect;
    let t = (time - clip.timeline_in).to_secs_f64().max(0.0);
    let ctx = eval_context(clip, time, fps);
    let enabled = |i: usize| clip.builtin_effects_enabled().get(i).copied().unwrap_or(true);
    let own = clip.baked_own.map_or(clip.builtin_effects().len(), |o| o.effects);
    let param = |i: usize, name: &str, v: f32| {
        let kfs = clip.effect_keyframes().get(&format!("{i}:{name}")).map(|v| v.as_slice()).unwrap_or(&[]);
        eval_effect_with_modifiers(kfs, clip.param_modifiers(), i, name, t, v, &ctx)
    };
    let opacities = || clip.builtin_effects().iter().enumerate().filter(|(i, e)| enabled(*i) && matches!(e, BuiltinEffect::Opacity { .. }));
    let anchor = opacities().filter(|(i, _)| *i < own).map(|(i, _)| i).next_back();
    // The clip's opacity is its picture's, where its opacity edits land:
    // its value and its lane, times every other Opacity. Its modifier stack
    // is the one on `opacity_anim`. Every clip but a render copy whose
    // track's effects, baked on after its own (`baked_own`, M4 step 12f),
    // hold an enabled Opacity: its lift would take the track's as its
    // anchor, where the clip's own is, so it reads the list. A
    // picture-truth clip's opacity is its picture's either way.
    let listed_track_opacity = opacities().any(|(i, _)| i >= own);
    if let (true, Some((keys, modifiers)), Ok(lifted)) = (clip.is_picture_truth() || !listed_track_opacity, clip.truth_lane(&klipp_core::ParamId::Opacity), clip.picture_v2()) {
        let opacity = eval_with_modifiers(&keys, &modifiers, &klipp_core::ParamId::Opacity, t, lifted.picture.opacity, &ctx);
        let mut others = 1.0f32;
        for (i, effect) in opacities() {
            if let (BuiltinEffect::Opacity { value }, false) = (effect, Some(i) == anchor) {
                others *= param(i, "value", *value).clamp(0.0, 1.0);
            }
        }
        return opacity.clamp(0.0, 1.0) * others;
    }
    let mut opacity = None;
    let mut others = 1.0f32;
    for (i, effect) in opacities() {
        let BuiltinEffect::Opacity { value } = effect else { continue };
        if Some(i) == anchor {
            // Clip opacity keyframes / modifiers first, then the effect's
            // own parameter keyframes — the viewer's order.
            let base = eval_with_modifiers(clip.opacity_keyframes(), clip.param_modifiers(), &klipp_core::ParamId::Opacity, t, *value, &ctx);
            opacity = Some(param(i, "value", base));
        } else {
            others *= param(i, "value", *value).clamp(0.0, 1.0);
        }
    }
    let opacity = opacity.unwrap_or_else(|| eval_with_modifiers(clip.opacity_keyframes(), clip.param_modifiers(), &klipp_core::ParamId::Opacity, t, 1.0, &ctx));
    opacity.clamp(0.0, 1.0) * others
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
    resolve_effect_param(keyframes, modifiers.get(&param), time_secs, default, ctx)
}

/// One keyframable parameter of an effect node at `time_secs`, from the
/// node's own animation: its keys, then its modifier stack, else
/// `current`, the value the effect holds (node-graph M2 step 14).
///
/// The node-path form of [`eval_effect_with_modifiers`], which only looks
/// the stack up by the effect's place in the list and comes here: a node
/// carries its own keys and stack, so nothing is addressed by index. The
/// caller passes them typed, converted once when the plan is compiled,
/// never parsed per frame.
pub fn resolve_effect_param(
    keyframes: &[klipp_core::Keyframe],
    modifiers: Option<&klipp_core::ModifierStack>,
    time_secs: f64,
    current: f32,
    ctx: &klipp_core::EvalContext,
) -> f32 {
    match modifiers {
        Some(stack) if !stack.modifiers.is_empty() => {
            klipp_core::evaluate_param(keyframes, stack, time_secs, current, ctx)
        }
        _ => klipp_core::keyframe::evaluate(keyframes, time_secs, current),
    }
}

/// An effect node's keys and modifier stacks, typed once when the plan is
/// compiled (node-graph M2 step 14), by the keyframe name
/// [`BuiltinEffect::animate`](klipp_effects::BuiltinEffect::animate) asks
/// for. The node's `anim` is keyed by the parameter it drives, which the
/// lift resolved through its aliases; reading it back goes through
/// [`klipp_graph_lift::names::keyframe_name`].
#[derive(Clone, Debug, Default)]
pub struct EffectNodeAnim(std::collections::BTreeMap<String, (Vec<klipp_core::Keyframe>, Option<klipp_core::ModifierStack>)>);

impl EffectNodeAnim {
    /// The animation `node` carries. An entry that doesn't read as keys
    /// or a modifier stack is an error naming its parameter: a picture
    /// the lift made always reads.
    pub fn of(node: &klipp_graph_format::Node) -> Result<Self, String> {
        let tag = klipp_graph_lift::effect_params::tag(node).unwrap_or("");
        let mut params = std::collections::BTreeMap::new();
        for (key, anim) in &node.anim {
            let keys = anim
                .keys
                .iter()
                .map(|k| serde_json::from_value(k.clone()))
                .collect::<Result<Vec<klipp_core::Keyframe>, _>>()
                .map_err(|e| format!("{key}'s keys: {e}"))?;
            let stack = anim
                .modifiers
                .as_ref()
                .map(|m| serde_json::from_value(m.clone()))
                .transpose()
                .map_err(|e| format!("{key}'s modifiers: {e}"))?;
            params.insert(klipp_graph_lift::names::keyframe_name(tag, key.as_str()).to_owned(), (keys, stack));
        }
        Ok(Self(params))
    }

    /// Whether the node animates nothing.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// `effect` at `time_secs`: each keyframable parameter from its own
    /// keys and stack, through [`resolve_effect_param`]; a parameter with
    /// neither keeps its value.
    pub fn animate(&self, effect: &mut klipp_effects::BuiltinEffect, time_secs: f64, ctx: &klipp_core::EvalContext) {
        effect.animate(&mut |name, v| match self.0.get(name) {
            Some((keys, stack)) => resolve_effect_param(keys, stack.as_ref(), time_secs, v, ctx),
            None => v,
        });
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
        let c = Clip::media(MediaId::new(), tc(0), tc(48), tc(0), tc(48));
        klipp_timeline::test_clip::TestClip::of(c).effect(BuiltinEffect::Transform {
            position_x: px, position_y: py,
            scale_x: sc, scale_y: sc,
            rotation: ro,
            anchor_x: ax, anchor_y: ay,
            rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0,
            focal_ref: None,
        }).build()
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

#[cfg(test)]
mod camera_tests {
    //! The maths of a camera over the sequence. What separates it from
    //! animating every clip in step is that a layer already off the
    //! middle of the frame moves *with* the camera rather than staying
    //! where it was put.
    use super::{apply_camera, camera_at};

    const MIDDLE: super::ResolvedTransform = (0.0, 0.0, 1.0, 1.0, 0.0, 0.5, 0.5);

    #[test]
    fn a_zoom_carries_a_layer_that_sits_off_the_middle() {
        let off = (100.0, -40.0, 1.0, 1.0, 0.0, 0.5, 0.5);
        let (px, py, sx, _, _, _, _) = apply_camera(off, (0.5, 0.5, 2.0, 0.0), 1920, 1080);
        // Twice as far in, so twice as far out from the middle and twice
        // the size. Animating the clip alone would have left it at 100.
        assert!((px - 200.0).abs() < 1e-3, "{px}");
        assert!((py + 80.0).abs() < 1e-3, "{py}");
        assert!((sx - 2.0).abs() < 1e-6);
    }

    #[test]
    fn looking_right_moves_the_scene_left() {
        let (px, ..) = apply_camera(MIDDLE, (0.6, 0.5, 1.0, 0.0), 1000, 1000);
        assert!((px + 100.0).abs() < 1e-3, "{px}");
        // And looking at the middle changes nothing at all.
        assert_eq!(apply_camera(MIDDLE, (0.5, 0.5, 1.0, 0.0), 1000, 1000), MIDDLE);
    }

    #[test]
    fn a_roll_turns_the_scene_the_other_way_and_swings_what_is_off_centre() {
        let off = (100.0, 0.0, 1.0, 1.0, 0.0, 0.5, 0.5);
        let (px, py, _, _, ro, _, _) = apply_camera(off, (0.5, 0.5, 1.0, 90.0), 1000, 1000);
        assert!((ro + 90.0).abs() < 1e-6, "the picture turns against the camera");
        // A card to the right of the middle swings to the top of frame.
        assert!(px.abs() < 1e-3 && (py + 100.0).abs() < 1e-3, "{px}, {py}");
    }

    #[test]
    fn a_far_layer_drifts_and_grows_less_than_a_near_one() {
        use super::camera_at_depth;
        let cam = (0.6, 0.5, 1.5, -2.0);
        assert_eq!(camera_at_depth(cam, 1.0), Some(cam), "the composed plane sees the camera as it is");
        assert_eq!(camera_at_depth(cam, 0.0), None, "a layer on the glass is not moved");

        let far = camera_at_depth(cam, 3.0).unwrap();
        let near = camera_at_depth(cam, 0.8).unwrap();
        // Looking right a tenth of the frame: a third as far for the far
        // layer, further for the near one.
        assert!((far.0 - (0.5 + 0.1 / 3.0)).abs() < 1e-6);
        assert!(near.0 > 0.6);
        // A 1.5x push is a dolly a third of the way in: the far layer grows
        // by 3 / (3 - 1/3) = 1.125, the near one by more than 1.5.
        assert!((far.2 - 1.125).abs() < 1e-5, "{}", far.2);
        assert!(near.2 > 1.5, "{}", near.2);
        // The roll is the camera's, the same for every layer.
        assert_eq!((far.3, near.3), (-2.0, -2.0));

        // A near layer the camera reaches is held, not blown up.
        let passed = camera_at_depth((0.5, 0.5, 4.0, 0.0), 0.5).unwrap();
        assert!(passed.2.is_finite() && passed.2 <= 20.0 + 1e-3);
    }

    #[test]
    fn a_sequence_with_no_camera_resolves_to_none() {
        let mut seq = klipp_timeline::Sequence::new_default();
        assert!(camera_at(&seq, klipp_core::RationalTime::from_frames(0, 24)).is_none());
        klipp_core::keyframe::upsert_keyframe(&mut seq.camera.zoom_keyframes, 0.0, 1.5);
        let got = camera_at(&seq, klipp_core::RationalTime::from_frames(0, 24)).unwrap();
        // The lists left empty keep their resting values.
        assert_eq!(got, (0.5, 0.5, 1.5, 0.0));
    }
}

#[cfg(test)]
mod resolve_effect_param_tests {
    use super::{eval_effect_with_modifiers, resolve_effect_param};
    use klipp_core::{EvalContext, Keyframe, ModifierStack, ParamId};
    use std::collections::HashMap;

    fn ctx(t: f64) -> EvalContext {
        EvalContext { clip_time_secs: t, clip_duration_secs: 4.0, timeline_time_secs: t + 10.0, fps: 25.0, seed: 932 }
    }

    fn keys() -> Vec<Keyframe> {
        let mut k = Vec::new();
        klipp_core::keyframe::upsert_keyframe(&mut k, 0.0, 1.0);
        klipp_core::keyframe::upsert_keyframe(&mut k, 1.5, 3.0);
        klipp_core::keyframe::upsert_keyframe(&mut k, 3.0, -2.0);
        k
    }

    /// One stack per modifier kind, plus an empty one.
    fn stacks() -> Vec<(&'static str, ModifierStack)> {
        let one = |json: &str| ModifierStack { modifiers: vec![serde_json::from_str(json).expect("a modifier")] };
        vec![
            ("empty", ModifierStack::default()),
            ("oscillate", one(r#"{"type":"oscillate","amplitude":0.5,"frequency":2.0,"phase":0.25,"waveform":"Triangle"}"#)),
            ("wiggle", one(r#"{"type":"wiggle","amplitude":0.7,"frequency":3.0,"blend":"Multiply"}"#)),
            ("throw", one(r#"{"type":"throw","velocity":1.5,"acceleration":-0.25}"#)),
            ("ramp", one(r#"{"type":"ramp","start_value":0.0,"end_value":2.0,"start_norm":0.1,"end_norm":0.9,"blend":"Replace"}"#)),
            ("loop", one(r#"{"type":"loop","period_secs":1.25}"#)),
            ("ping_pong", one(r#"{"type":"ping_pong","period_secs":0.8}"#)),
            ("clamp", one(r#"{"type":"clamp","min":-0.5,"max":2.5}"#)),
        ]
    }

    const TIMES: [f64; 10] = [0.0, 0.2, 0.75, 1.0, 1.5, 1.9, 2.4, 3.0, 3.6, 4.0];

    /// The node-path read is the index lookup's, to the bit, for keys
    /// alone and under every modifier kind.
    #[test]
    fn a_nodes_animation_resolves_as_the_index_lookup_did() {
        let keys = keys();
        for (name, stack) in stacks() {
            let mut by_index = HashMap::new();
            by_index.insert(ParamId::Effect { effect_index: 0, param_name: "radius".into() }, stack.clone());
            for t in TIMES {
                let legacy = eval_effect_with_modifiers(&keys, &by_index, 0, "radius", t, 2.0, &ctx(t));
                let node = resolve_effect_param(&keys, Some(&stack), t, 2.0, &ctx(t));
                assert_eq!(node.to_bits(), legacy.to_bits(), "{name} at {t}: node {node}, index {legacy}");
                // And both are klipp-core's own evaluation, not a copy of
                // it: keys and stack for a stack with modifiers, keys alone
                // for an empty one.
                let core = if stack.modifiers.is_empty() {
                    klipp_core::keyframe::evaluate(&keys, t, 2.0)
                } else {
                    klipp_core::evaluate_param(&keys, &stack, t, 2.0, &ctx(t))
                };
                assert_eq!(node.to_bits(), core.to_bits(), "{name} at {t}");
            }
        }
        // No stack at all is the keys alone.
        for t in TIMES {
            let none = resolve_effect_param(&keys, None, t, 2.0, &ctx(t));
            assert_eq!(none.to_bits(), klipp_core::keyframe::evaluate(&keys, t, 2.0).to_bits());
        }
    }

    /// A node reads its own keys and stack, whatever its place: the
    /// effect at list position 3 animates from its own, and the one at
    /// position 0, which has none, keeps its value and picks up nothing
    /// of position 3's.
    #[test]
    fn a_node_reads_its_own_animation_not_its_place() {
        let (keys, stack) = (keys(), stacks().swap_remove(2).1);
        let mut by_index = HashMap::new();
        by_index.insert(ParamId::Effect { effect_index: 3, param_name: "radius".into() }, stack.clone());
        for t in TIMES {
            let third = resolve_effect_param(&keys, Some(&stack), t, 2.0, &ctx(t));
            assert_eq!(third.to_bits(), eval_effect_with_modifiers(&keys, &by_index, 3, "radius", t, 2.0, &ctx(t)).to_bits());
            assert_eq!(resolve_effect_param(&[], None, t, 2.0, &ctx(t)), 2.0, "position 0 has no animation of its own");
            assert_eq!(eval_effect_with_modifiers(&[], &by_index, 0, "radius", t, 2.0, &ctx(t)), 2.0, "nor does the index lookup give it 3's");
        }
        // The stack is really in play: it moves the value off the keys'.
        assert!(TIMES.iter().any(|&t| resolve_effect_param(&keys, Some(&stack), t, 2.0, &ctx(t)) != klipp_core::keyframe::evaluate(&keys, t, 2.0)));
    }
}

#[cfg(test)]
mod opacity_multiply_tests {
    use super::evaluate_clip_opacity;
    use klipp_core::Timecode;
    use klipp_effects::BuiltinEffect;
    use klipp_timeline::{Clip, ClipKind};

    fn clip(kind: ClipKind, opacities: &[(f32, bool)]) -> Clip {
        let mut b = klipp_timeline::test_clip::TestClip::of(Clip { kind, ..Clip::gap(Timecode::from_frames(0, 24), Timecode::from_frames(48, 24)) });
        for (value, on) in opacities {
            b = b.effect(BuiltinEffect::Opacity { value: *value });
            if !on {
                b = b.off();
            }
        }
        b.build()
    }

    /// Every enabled Opacity multiplies the last, a clip's and an
    /// adjustment layer's alike (#1386); a disabled one counts for nothing,
    /// wherever it sits.
    #[test]
    fn every_enabled_opacity_multiplies_in_on_a_clip_and_an_adjustment_layer() {
        let at = Timecode::from_frames(12, 24);
        for kind in [ClipKind::Gap, ClipKind::AdjustmentLayer] {
            let o = |opacities: &[(f32, bool)]| evaluate_clip_opacity(&clip(kind.clone(), opacities), at, 24.0);
            assert_eq!(o(&[(0.8, true)]), 0.8, "{kind:?}");
            assert!((o(&[(0.5, true), (0.8, true)]) - 0.4).abs() < 1e-6, "{kind:?}: 0.5 × 0.8");
            assert_eq!(o(&[(0.5, false), (0.8, true)]), 0.8, "{kind:?}: a disabled first one");
            assert_eq!(o(&[(0.5, true), (0.8, false)]), 0.5, "{kind:?}: a disabled last one");
        }
    }

    /// A keyed Opacity before the anchor multiplies in by its keys.
    #[test]
    fn a_keyed_opacity_before_the_anchor_multiplies_by_its_keys() {
        let at = Timecode::from_frames(12, 24);
        let mut c = clip(ClipKind::Gap, &[(1.0, true), (1.0, true)]);
        let id = klipp_timeline::test_clip::effect_id(&c, 0);
        c.edit_effect_keyframes_of(id, "value", |k| {
            klipp_core::keyframe::upsert_keyframe(k, 0.0, 0.25);
            klipp_core::keyframe::upsert_keyframe(k, 2.0, 0.25);
        });
        assert!((evaluate_clip_opacity(&c, at, 24.0) - 0.25).abs() < 1e-6, "{}", evaluate_clip_opacity(&c, at, 24.0));
    }
}

#[cfg(test)]
mod placement_for_editing_tests {
    use super::{placement_for_editing, NotFollowed};
    use klipp_timeline::{test_clip::TestClip, Clip, ClipKind, Sequence, TextBox, TitleData, Track, TrackKind};

    fn tc(s: f64) -> klipp_core::Timecode {
        klipp_core::Timecode::from_secs_f64(s, 24)
    }

    fn title() -> Clip {
        Clip { kind: ClipKind::Title(TitleData::single_text(TextBox { text: "Hello".into(), ..Default::default() })), ..Clip::gap(tc(0.0), tc(4.0)) }
    }

    fn held(c: Clip, lanes: &[(&str, f32)]) -> Clip {
        let mut b = TestClip::of(c);
        for (p, v) in lanes {
            b = b.lane(p, 0.0, *v).lane(p, 4.0, *v);
        }
        b.build()
    }

    fn sequence(clips: Vec<(TrackKind, Clip)>) -> Sequence {
        let mut seq = Sequence::new_default();
        seq.tracks = clips.into_iter().enumerate().map(|(i, (kind, c))| {
            let mut t = Track::new(&format!("T{i}"), kind);
            t.clips.push(c);
            t
        }).collect();
        seq
    }

    /// #2014: a title as placed for its canvas editor: unmoved, none; moved,
    /// where the renderer puts it; and each way the editor can't follow,
    /// named.
    #[test]
    fn a_titles_placement_is_the_renderers_or_says_why_not() {
        let plain = title();
        let seq = sequence(vec![(TrackKind::Text, plain.clone())]);
        assert_eq!(placement_for_editing(&plain, &seq, tc(1.0)), Ok(None), "unmoved");

        let moved = held(title(), &[("pos_x", 200.0), ("scale", 0.5)]);
        let seq = sequence(vec![(TrackKind::Text, moved.clone())]);
        let p = placement_for_editing(&moved, &seq, tc(1.0)).unwrap().expect("placed");
        assert!((p.0 - 200.0).abs() < 1e-3 && (p.2 - 0.5).abs() < 1e-5, "{p:?}");

        for (lanes, why) in [(&[("tilt_x", 20.0)][..], NotFollowed::Tilted), (&[("scale", 0.0)][..], NotFollowed::ScaledAway)] {
            let c = held(title(), lanes);
            let seq = sequence(vec![(TrackKind::Text, c.clone())]);
            assert_eq!(placement_for_editing(&c, &seq, tc(1.0)), Err(why), "{lanes:?}");
        }

        // Moved again by its track's own Transform.
        let c = title();
        let mut seq = sequence(vec![(TrackKind::Text, c.clone())]);
        let shift = klipp_effects::BuiltinEffect::Transform { position_x: 30.0, position_y: 0.0, scale_x: 1.0, scale_y: 1.0, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None };
        seq.tracks[0].effects.video_effect_list = klipp_timeline::TrackEffectList::new(vec![shift.clone()], vec![true]);
        assert_eq!(placement_for_editing(&c, &seq, tc(1.0)), Err(NotFollowed::TrackMoves));
        // The same Transform, its flag off: nothing moves it.
        seq.tracks[0].effects.video_effect_list = klipp_timeline::TrackEffectList::new(vec![shift], vec![false]);
        assert_eq!(placement_for_editing(&c, &seq, tc(1.0)), Ok(None), "a disabled Transform");

        // An adjustment layer over it, itself moved.
        let c = title();
        let adj = held(Clip { kind: ClipKind::AdjustmentLayer, ..Clip::gap(tc(0.0), tc(4.0)) }, &[("pos_x", 50.0)]);
        let seq = sequence(vec![(TrackKind::Text, c.clone()), (TrackKind::Video, adj)]);
        // Over it or under it, as the renderer stacks the two tracks.
        let layers = crate::render::active_layers(&seq, tc(1.0));
        let (title_at, adj_at) = (
            layers.iter().position(|l| l.clips.iter().any(|x| x.id == c.id)).unwrap(),
            layers.iter().position(|l| l.clips.iter().any(|x| matches!(x.kind, ClipKind::AdjustmentLayer))).unwrap(),
        );
        let want = if adj_at > title_at { Err(NotFollowed::AdjustmentMoves) } else { Ok(None) };
        assert_eq!(placement_for_editing(&c, &seq, tc(1.0)), want, "the adjustment layer at {adj_at}, the title at {title_at}");
        // And the other way up, so both cases are seen.
        let mut flipped = seq.clone();
        flipped.tracks.reverse();
        let want = if adj_at > title_at { Ok(None) } else { Err(NotFollowed::AdjustmentMoves) };
        assert_eq!(placement_for_editing(&c, &flipped, tc(1.0)), want, "flipped");
        assert!(!NotFollowed::Tilted.hint().is_empty());
    }

    fn key(time_secs: f64, value: f32) -> klipp_core::Keyframe {
        klipp_core::Keyframe { time_secs, value, interpolation: Default::default(), tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0), handles: Default::default() }
    }

    /// #2014: the sequence's camera places a title on the composed plane
    /// (depth 1) as it places the picture; one fixed to the screen (depth
    /// 0) it leaves where it is.
    #[test]
    fn the_camera_places_a_title_at_its_depth() {
        for (depth, want_zoom) in [(1.0, Some(2.0)), (0.0, None)] {
            let c = Clip { depth, ..title() };
            let mut seq = sequence(vec![(TrackKind::Text, c.clone())]);
            seq.camera.zoom_keyframes = vec![key(0.0, 2.0)];
            let got = placement_for_editing(&c, &seq, tc(1.0)).unwrap();
            assert_eq!(got.map(|p| p.2), want_zoom, "depth {depth}: {got:?}");
        }
    }

    /// #2014: inside a transition that moves its pictures (a push) the
    /// canvas can't follow the title; inside one that only blends them (a
    /// dissolve) it can.
    #[test]
    fn a_moving_transition_is_refused_and_a_dissolve_followed() {
        for (kind, want) in [(klipp_timeline::TransitionKind::PushLeft, Err(NotFollowed::InTransition)), (klipp_timeline::TransitionKind::CrossDissolve, Ok(None))] {
            let a = Clip { kind: ClipKind::Title(TitleData::single_text(TextBox { text: "A".into(), ..Default::default() })), ..Clip::gap(tc(0.0), tc(2.0)) };
            let b = Clip { kind: ClipKind::Title(TitleData::single_text(TextBox { text: "B".into(), ..Default::default() })), ..Clip::gap(tc(2.0), tc(4.0)) };
            let mut seq = sequence(vec![(TrackKind::Text, a.clone())]);
            seq.tracks[0].clips.push(b.clone());
            seq.tracks[0].transitions.push(klipp_timeline::Transition {
                id: klipp_core::TransitionId::new(),
                kind,
                duration: tc(1.0),
                clip_a_id: a.id,
                clip_b_id: b.id,
                user_params: Default::default(),
                alignment: Default::default(),
            });
            assert_eq!(placement_for_editing(&b, &seq, tc(2.1)), want, "{kind:?}");
            assert_eq!(placement_for_editing(&b, &seq, tc(3.5)), Ok(None), "{kind:?}: after it");
        }
    }

    /// #2014: an adjustment layer inside a precomposition stacked over the
    /// title adjusts the precomposition's scene, not the title: followed,
    /// the precomposition over the title or under it.
    #[test]
    fn an_adjustment_layer_inside_a_precomposition_is_not_over_the_title() {
        let adj = held(Clip { kind: ClipKind::AdjustmentLayer, ..Clip::gap(tc(0.0), tc(4.0)) }, &[("pos_x", 50.0)]);
        let scene = sequence(vec![(TrackKind::Video, adj)]);
        let scene_id = klipp_core::SequenceId::new();
        let pre = Clip { kind: ClipKind::Sequence { sequence_id: scene_id }, precomp: Some(std::sync::Arc::new(scene)), ..Clip::gap(tc(0.0), tc(4.0)) };
        let c = title();
        let seq = sequence(vec![(TrackKind::Text, c.clone()), (TrackKind::Video, pre)]);
        // The scene's adjustment layer is in play, as a layer of its own.
        assert!(crate::render::active_layers(&seq, tc(1.0)).iter().any(|l| l.clips.iter().any(|x| matches!(x.kind, ClipKind::AdjustmentLayer))));
        let mut flipped = seq.clone();
        flipped.tracks.reverse();
        for s in [&seq, &flipped] {
            assert_eq!(placement_for_editing(&c, s, tc(1.0)), Ok(None));
        }
    }
}

