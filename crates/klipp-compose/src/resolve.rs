//! A clip's effects for one frame, read from its lifted picture (#1089,
//! phase 2b M2, step 10c).
//!
//! The lifted picture holds the clip's chain in render order: the grade,
//! the display conversion, OFX plug-ins, the built-in effects, and the
//! placement where the transform goes. [`resolve_effects`] walks it from
//! the display conversion to the output and resolves each enabled effect
//! node for the frame: its payload decoded, its own animation applied
//! (the node's `anim`, keyed by parameter name), then what the frame
//! gives it (`frame_inputs`), and the clip's transform where the lift
//! placed it. It answers what [`crate::render::clip_effects`] answers
//! for the same clip and time, so the renderer can take its effects from
//! the picture without a picture changing.
//!
//! Effect node ids are derived from the clip and the effect's place in
//! the list: they are not a persistent key, and nothing here keeps one.

use klipp_core::Timecode;
use klipp_effects::BuiltinEffect;
use klipp_graph_format::lifted::{types as lt, ClipPicture};
use klipp_graph_format::{types, Node, NodeId};
use klipp_timeline::Clip;

use crate::eval::{self, EffectNodeAnim, ResolvedTransform};
use crate::render::{frame_inputs, tilt, transform_effect};

/// `clip`'s effects at `time`, from `picture` (its lifted picture), each
/// node's animation applied by [`EffectNodeAnim`]: what `clip_effects`
/// gives for the same clip and time, its opacity apart.
pub fn resolve_effects(
    clip: &Clip,
    picture: &ClipPicture,
    time: Timecode,
    fps: f64,
    resolved: Option<ResolvedTransform>,
    pin: Option<klipp_timeline::surface::Quad>,
) -> Result<Vec<BuiltinEffect>, String> {
    resolve_effects_at(clip, picture, time, fps, resolved, pin, 1.0)
}

/// [`resolve_effects`] for a frame rendered `scale` times the sequence's
/// size (node-graph M2 step 13): each effect's pixel sizes scaled as the
/// effect scales them ([`BuiltinEffect::at_render_scale`], whose rules the
/// registry describes). The renderer scales after resolving until it
/// takes the scale here instead.
pub fn resolve_effects_at(
    clip: &Clip,
    picture: &ClipPicture,
    time: Timecode,
    fps: f64,
    resolved: Option<ResolvedTransform>,
    pin: Option<klipp_timeline::surface::Quad>,
    scale: f32,
) -> Result<Vec<BuiltinEffect>, String> {
    let t = crate::render::eval_clip_time(clip, time);
    let ctx = eval::eval_context(clip, time, fps);
    let mut fx = Vec::new();
    let mut pinned = false;
    // The clip's transform (R2-R4): the placement node, at the first
    // enabled Transform's place (fixed, with that Transform), or automatic.
    let mut placement: Option<(usize, Option<BuiltinEffect>)> = None;
    let chain = chain(picture)?;
    let start = chain.iter().position(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).map_or(0, |i| i + 1);
    for node in &chain[start..] {
        let kind = node.type_id.as_str();
        if kind == lt::PLACEMENT {
            let fixed = match &node.legacy {
                Some(l) if node.enabled => Some(decode(node, &l.payload, t, &ctx)?),
                _ => None,
            };
            placement = Some((fx.len(), fixed));
            continue;
        }
        // Opacity, a speed ramp, temporal denoise and extra Transforms are
        // other node types, or disabled: none is a pixel effect here.
        if !node.enabled || kind != types::LEGACY_EFFECT {
            continue;
        }
        let payload = &node.legacy.as_ref().ok_or_else(|| format!("effect node {} has no payload", node.id))?.payload;
        let mut effect = decode(node, payload, t, &ctx)?;
        frame_inputs(clip, &mut effect, t, fps, pin, &mut pinned);
        fx.push(effect);
    }
    let xf = resolved.unwrap_or_else(|| eval::evaluate_clip_own_transform(clip, t, &ctx));
    let fixed = placement.as_ref().and_then(|(_, f)| f.clone());
    let three_d = tilt(clip, fixed.as_slice(), t);
    let identity = xf == (0.0, 0.0, 1.0, 1.0, 0.0, 0.5, 0.5) && three_d.0 == 0.0 && three_d.1 == 0.0;
    match placement {
        // Fixed: where the first enabled Transform was.
        Some((at, Some(_))) => fx.insert(at, transform_effect(xf, three_d)),
        // Automatic: the lift keeps the node at the chain's end; the move
        // goes before the first effect drawn outside the picture, which
        // draws in the margin the move leaves (R4).
        Some((_, None)) if !identity => {
            let at = fx.iter().position(klipp_effects::draws_outside_the_picture).unwrap_or(fx.len());
            fx.insert(at, transform_effect(xf, three_d));
        }
        _ => {}
    }
    fx.retain(|e| !klipp_effects::is_identity_effect(e));
    Ok(fx.into_iter().map(|e| e.at_render_scale(scale)).collect())
}

/// An effect node's payload as its effect, animated for the frame.
fn decode(node: &Node, payload: &serde_json::Value, t: f64, ctx: &klipp_core::EvalContext) -> Result<BuiltinEffect, String> {
    let mut effect: BuiltinEffect = serde_json::from_value(payload.clone()).map_err(|e| format!("effect node {}: {e}", node.id))?;
    EffectNodeAnim::of(node)?.animate(&mut effect, t, ctx);
    Ok(effect)
}

/// The picture's top-level chain, source to output, following each
/// node's `in` edge back from the output.
fn chain(picture: &ClipPicture) -> Result<Vec<&Node>, String> {
    let g = &picture.graph;
    let by_id: std::collections::HashMap<NodeId, &Node> = g.nodes.iter().map(|n| (n.id, n)).collect();
    let mut out = Vec::with_capacity(g.nodes.len());
    let mut at = g.output;
    loop {
        let node = by_id.get(&at).ok_or_else(|| format!("the chain reaches {at}, which isn't in the picture"))?;
        out.push(*node);
        if out.len() > g.nodes.len() {
            return Err("the picture's chain loops".into());
        }
        match g.edges.iter().find(|e| e.to == (at, "in".into())) {
            Some(e) => at = e.from.0,
            None => break,
        }
    }
    out.reverse();
    Ok(out)
}
