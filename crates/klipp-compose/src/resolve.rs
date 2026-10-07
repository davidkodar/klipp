//! A clip's effects for one frame, read from its lifted picture (#1089,
//! phase 2b M2, step 10c).
//!
//! The lifted picture holds the clip's chain in render order: the grade,
//! the display conversion, OFX plug-ins, the built-in effects, and the
//! placement where the transform goes. [`resolve_effects`] walks it from
//! the display conversion to the output and resolves each enabled effect
//! node for the frame: its payload decoded, its own animation applied
//! (the node's `anim`, keyed by parameter name), then what the frame
//! gives it (`frame_inputs`) and the ordering rules M2 keeps
//! (`place_and_finish`). It answers what [`crate::render::clip_effects`] answers
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
use crate::render::{frame_inputs, place_and_finish};

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
    let t = crate::render::eval_clip_time(clip, time);
    let ctx = eval::eval_context(clip, time, fps);
    let mut fx = Vec::new();
    let mut pinned = false;
    let chain = chain(picture)?;
    let start = chain.iter().position(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).map_or(0, |i| i + 1);
    for node in &chain[start..] {
        let kind = node.type_id.as_str();
        let effect_node = kind == types::LEGACY_EFFECT;
        // The fixed placement is the first enabled Transform, where it was.
        let fixed = kind == lt::PLACEMENT && node.legacy.is_some();
        if !node.enabled || !(effect_node || fixed) {
            continue;
        }
        let payload = &node.legacy.as_ref().ok_or_else(|| format!("effect node {} has no payload", node.id))?.payload;
        let mut effect: BuiltinEffect = serde_json::from_value(payload.clone()).map_err(|e| format!("effect node {}: {e}", node.id))?;
        EffectNodeAnim::of(node)?.animate(&mut effect, t, &ctx);
        frame_inputs(clip, &mut effect, t, fps, pin, &mut pinned);
        fx.push(effect);
    }
    Ok(place_and_finish(clip, fx, t, &ctx, resolved))
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
