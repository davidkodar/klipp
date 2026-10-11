//! `clip_effects` as the renderer had it, frozen for M1's `render_inputs`
//! oracle (#1068, R28): what today's renderer would draw for a clip, read
//! by today's own code, which the lift's adapter is held to.
//!
//! Phase 2b M2 step 11d deletes the renderer's legacy effects route, and
//! its effect ordering now lives in the plan (`klipp_compose::resolve`).
//! `render_inputs` must keep comparing against the rules as they were, not
//! against whatever the renderer does next, so the functions are copied
//! here verbatim from `crates/klipp-compose/src/render.rs` at 296c17c3
//! (`clip_effects`, `frame_inputs`, `place_and_finish`, `clip_opacity`,
//! `tilt`, `transform_effect`, `eval_clip_time`), only made `pub` and with
//! their imports spelled out. Don't edit them: a change here would move
//! the oracle, not the code under test.

#![allow(dead_code)]

use klipp_compose::eval::{self, ResolvedTransform};
use klipp_core::Timecode;
use klipp_effects::BuiltinEffect;
use klipp_timeline::Clip;

/// A clip's built-in effects evaluated at `time`, with its opacity split
/// out. Keyframes and modifier stacks applied, the transform replaced by
/// the resolved one (own keyframes, link, focal point), disabled and
/// identity effects dropped, `Opacity` removed — it is a compositing
/// weight, returned separately — and `SpeedRamp` removed, which acts on
/// time, not pixels.
pub fn clip_effects(
    clip: &Clip,
    time: Timecode,
    fps: f64,
    resolved: Option<ResolvedTransform>,
    pin: Option<klipp_timeline::surface::Quad>,
) -> (Vec<BuiltinEffect>, f32) {
    let t = eval_clip_time(clip, time);
    let ctx = eval::eval_context(clip, time, fps);
    let enabled = |i: usize| clip.builtin_effects_enabled().get(i).copied().unwrap_or(true);
    let mut fx: Vec<(usize, BuiltinEffect)> =
        clip.builtin_effects().iter().cloned().enumerate().filter(|(i, _)| enabled(*i)).collect();
    let mut pinned = false;
    for (i, effect) in fx.iter_mut() {
        // Every keyframable parameter, from the one list the inspector
        // offers keyframes from. Opacity is the exception: it is the
        // clip's compositing weight (`clip_opacity`).
        if !matches!(effect, BuiltinEffect::Opacity { .. }) {
            effect.animate(&mut |name, v| {
                let kfs = clip.effect_keyframes().get(&format!("{i}:{name}")).map(|v| v.as_slice()).unwrap_or(&[]);
                eval::eval_effect_with_modifiers(kfs, clip.param_modifiers(), *i, name, t, v, &ctx)
            });
        }
        frame_inputs(clip, effect, t, fps, pin, &mut pinned);
    }
    let fx = fx.into_iter().map(|(_, e)| e).collect();
    (place_and_finish(clip, fx, t, &ctx, resolved), clip_opacity(clip, time, fps))
}

/// What one frame gives an animated effect (R16): the film effects' seed,
/// the deflicker gain, a pinned clip's first corner pin, a colour fade's
/// progress.
pub fn frame_inputs(
    clip: &Clip,
    effect: &mut BuiltinEffect,
    t: f64,
    fps: f64,
    pin: Option<klipp_timeline::surface::Quad>,
    pinned: &mut bool,
) {
    match effect {
        // The animated film effects take a fresh pattern per frame: the
        // seed is the timeline frame number, so a still export and a scrub
        // land on the same grain.
        BuiltinEffect::FilmGrain { seed, .. } | BuiltinEffect::OldFilm { seed, .. } | BuiltinEffect::Vhs { seed, .. } => {
            *seed = (t * fps).round().max(0.0) as u32;
        }
        BuiltinEffect::Deflicker { gain, .. } => {
            // Its analysis gives the frame's gain (M4 step 9); none here,
            // as the renderer's own copy says.
            *gain = 1.0;
        }
        // A pinned clip's first corner pin is on its surface.
        BuiltinEffect::CornerPin { tl, tr, br, bl } if pin.is_some() && !*pinned => {
            let q = pin.unwrap_or_default();
            (*tl, *tr, *br, *bl) = (q[0], q[1], q[2], q[3]);
            *pinned = true;
        }
        BuiltinEffect::ColorFade { duration_secs, mode, progress, .. } => {
            let dur = (*duration_secs as f64).max(0.0);
            // `t` is timeline seconds into the clip, so the fade out is
            // measured from the clip's timeline end. Measured from its
            // *source* length, a clip sped up 2× faded out an hour early in
            // the source and never on the timeline.
            let clip_dur = (clip.timeline_out - clip.timeline_in).to_secs_f64().max(0.0);
            *progress = if dur <= 0.0 {
                1.0
            } else {
                match *mode {
                    klipp_effects::ColorFadeMode::In => (t / dur).clamp(0.0, 1.0) as f32,
                    klipp_effects::ColorFadeMode::Out => ((clip_dur - t).max(0.0) / dur).clamp(0.0, 1.0) as f32,
                }
            };
        }
        _ => {}
    }
}

/// The ordering rules on an animated list (R1-R5 as M2 keeps them): the
/// effects that are not pixel effects go, the clip's transform is placed
/// once, and identities are dropped.
pub fn place_and_finish(
    clip: &Clip,
    fx: Vec<BuiltinEffect>,
    t: f64,
    ctx: &klipp_core::EvalContext,
    resolved: Option<ResolvedTransform>,
) -> Vec<BuiltinEffect> {
    // Opacity is a compositing weight and SpeedRamp a matter of time and
    // frame blending; neither is a pixel effect. (SpeedRamp through the
    // CPU chain was an 8-bit round trip that changed nothing else.)
    let mut fx: Vec<BuiltinEffect> = fx
        .into_iter()
        // TemporalDenoise joins them: it ran at the head of the
        // pipeline, before the grade, and running again here would
        // average the graded picture against source neighbours.
        .filter(|e| !matches!(e, BuiltinEffect::Opacity { .. } | BuiltinEffect::SpeedRamp { .. } | BuiltinEffect::TemporalDenoise { .. }))
        .collect();

    let xf = resolved.unwrap_or_else(|| eval::evaluate_clip_own_transform(clip, t, ctx));
    let has_transform = fx.iter().any(|e| matches!(e, BuiltinEffect::Transform { .. }));
    let three_d = tilt(clip, &fx, t);
    let identity = xf == (0.0, 0.0, 1.0, 1.0, 0.0, 0.5, 0.5) && three_d.0 == 0.0 && three_d.1 == 0.0;
    if has_transform {
        // The clip's transform goes where its first Transform effect is,
        // once (#995): every Transform effect used to become it, so two
        // moved, scaled and turned the clip twice. Later ones are dropped
        // here, and `check` says so.
        let mut placed = false;
        fx.retain_mut(|e| {
            if !matches!(e, BuiltinEffect::Transform { .. }) {
                return true;
            }
            if placed {
                return false;
            }
            *e = transform_effect(xf, three_d);
            placed = true;
            true
        });
    } else if !identity {
        // A shadow or a glow draws in the transparent margin the
        // transform leaves around a placed layer, so it has to run after
        // the transform that made the margin. Everything else — rounded
        // corners included — runs before it, with the picture, so it
        // scales with the layer.
        let at = fx.iter().position(klipp_effects::draws_outside_the_picture).unwrap_or(fx.len());
        fx.insert(at, transform_effect(xf, three_d));
    }
    fx.retain(|e| !klipp_effects::is_identity_effect(e));
    fx
}

/// A clip's compositing weight at `time`: its own last `Opacity` effect
/// over its opacity keyframes and modifiers (else those alone, over 1),
/// times each Opacity its track added. A render copy carries its track's
/// effects after the clip's own (`bake_track_effects`); a track's Opacity
/// weighs the clip's resolved opacity rather than standing in for it
/// (#1078).
pub fn clip_opacity(clip: &Clip, time: Timecode, fps: f64) -> f32 {
    let t = eval_clip_time(clip, time);
    let ctx = eval::eval_context(clip, time, fps);
    let enabled = |i: usize| clip.builtin_effects_enabled().get(i).copied().unwrap_or(true);
    let own = clip.own_effects().map_or(clip.builtin_effects().len(), |o| o.builtin_effects.len());
    let param = |i: usize, name: &str, v: f32| {
        let kfs = clip.effect_keyframes().get(&format!("{i}:{name}")).map(|v| v.as_slice()).unwrap_or(&[]);
        eval::eval_effect_with_modifiers(kfs, clip.param_modifiers(), i, name, t, v, &ctx)
    };
    let mut opacity = None;
    let mut track_opacity = 1.0f32;
    for (i, effect) in clip.builtin_effects().iter().enumerate().filter(|(i, _)| enabled(*i)) {
        let BuiltinEffect::Opacity { value } = effect else { continue };
        if i >= own {
            track_opacity *= param(i, "value", *value).clamp(0.0, 1.0);
        } else {
            // Clip opacity keyframes / modifiers first, then the effect's
            // own parameter keyframes — the viewer's order.
            let base = eval::eval_with_modifiers(clip.opacity_keyframes(), clip.param_modifiers(), &klipp_core::ParamId::Opacity, t, *value, &ctx);
            opacity = Some(param(i, "value", base));
        }
    }
    let opacity = opacity.unwrap_or_else(|| {
        eval::eval_with_modifiers(clip.opacity_keyframes(), clip.param_modifiers(), &klipp_core::ParamId::Opacity, t, 1.0, &ctx)
    });
    opacity.clamp(0.0, 1.0) * track_opacity
}

/// A clip's 3D tilt and perspective at clip time `t`: the tilt tracks,
/// over whatever its own `Transform` effect says. A tilt with no
/// perspective given is seen in moderate perspective (1), since flat it
/// would only squash the picture rather than lean it.
pub fn tilt(clip: &Clip, fx: &[BuiltinEffect], t: f64) -> (f32, f32, f32) {
    let (rx, ry, persp) = fx
        .iter()
        .find_map(|e| match e {
            BuiltinEffect::Transform { rotation_x, rotation_y, perspective, .. } => Some((*rotation_x, *rotation_y, *perspective)),
            _ => None,
        })
        .unwrap_or((0.0, 0.0, 0.0));
    let rx = klipp_core::keyframe::evaluate(clip.transform_tilt_x_keyframes(), t, rx);
    let ry = klipp_core::keyframe::evaluate(clip.transform_tilt_y_keyframes(), t, ry);
    // Keyframed, or set still in the inspector: either way a tilt.
    let tilted = rx != 0.0 || ry != 0.0 || clip.has_tilt_keyframes();
    (rx, ry, if tilted && persp <= 0.0 { 1.0 } else { persp })
}

/// A `Transform` effect with the resolved values and the clip's tilt.
pub fn transform_effect(xf: ResolvedTransform, (rx, ry, persp): (f32, f32, f32)) -> BuiltinEffect {
    let (px, py, sx, sy, ro, ax, ay) = xf;
    BuiltinEffect::Transform {
        position_x: px, position_y: py, scale_x: sx, scale_y: sy, rotation: ro,
        anchor_x: ax, anchor_y: ay, rotation_x: rx, rotation_y: ry, perspective: persp,
        focal_ref: None,
    }
}

/// Seconds since the clip's start on the timeline; negative before it
/// (a transition's incoming side), clamped to zero for evaluation.
pub fn eval_clip_time(clip: &Clip, time: Timecode) -> f64 {
    (time - clip.timeline_in).to_secs_f64().max(0.0)
}
