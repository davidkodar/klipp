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
    resolve_effects_blended(clip, picture, time, fps, resolved, pin, scale).map(|fx| fx.into_iter().map(|(e, _)| e).collect())
}

/// What an effect node of a picture-truth clip is blended by (node-graph
/// M2 step 9c): its mix, and the matte its mask port reads, by channel.
#[derive(Clone, Debug, PartialEq)]
pub struct Blend {
    pub blend: klipp_nodes::params::EffectBlend,
    pub mask: Option<MaskSource>,
}

/// Where a masked effect's matte comes from, for the run that draws it.
#[derive(Clone, Debug, PartialEq)]
pub enum MaskSource {
    /// A node inside the clip's grade, published by the grade group: the
    /// grade run's node of the same id (the lift keeps grade node ids), at
    /// output `port`.
    Grade { node: NodeId, port: u8 },
    /// A node beside the clip's chain (a window, a key), built into the
    /// run, reading `input`, at output `port`.
    Side { node: Box<klipp_nodes::Node>, port: u8, input: SideInput },
    /// Wiring the renderer can't give the matte (a file's; the ops refuse
    /// it): the effect is bypassed, said why.
    Unavailable(String),
}

/// What a node beside the chain reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SideInput {
    /// The clip's source, as its grade reads it.
    Source,
    /// The grade's picture.
    Graded,
}

/// [`resolve_effects_at`], each effect with its [`Blend`] when its node
/// has a mix or a mask (a picture-truth clip's); `None` for every other.
pub fn resolve_effects_blended(
    clip: &Clip,
    picture: &ClipPicture,
    time: Timecode,
    fps: f64,
    resolved: Option<ResolvedTransform>,
    pin: Option<klipp_timeline::surface::Quad>,
    scale: f32,
) -> Result<Vec<(BuiltinEffect, Option<Blend>)>, String> {
    let t = crate::render::eval_clip_time(clip, time);
    let ctx = eval::eval_context(clip, time, fps);
    let mut fx = Vec::new();
    let mut blends: Vec<Option<Blend>> = Vec::new();
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
        blends.push(blend_of(picture, node));
    }
    let xf = resolved.unwrap_or_else(|| eval::evaluate_clip_own_transform(clip, t, &ctx));
    let fixed = placement.as_ref().and_then(|(_, f)| f.clone());
    let three_d = tilt(clip, fixed.as_slice(), t);
    let identity = xf == (0.0, 0.0, 1.0, 1.0, 0.0, 0.5, 0.5) && three_d.0 == 0.0 && three_d.1 == 0.0;
    match placement {
        // Fixed: where the first enabled Transform was.
        Some((at, Some(_))) => {
            fx.insert(at, transform_effect(xf, three_d));
            blends.insert(at, None);
        }
        // Automatic: the lift keeps the node at the chain's end; the move
        // goes before the first effect drawn outside the picture, which
        // draws in the margin the move leaves (R4).
        Some((_, None)) if !identity => {
            let at = fx.iter().position(klipp_effects::draws_outside_the_picture).unwrap_or(fx.len());
            fx.insert(at, transform_effect(xf, three_d));
            blends.insert(at, None);
        }
        _ => {}
    }
    Ok(fx.into_iter().zip(blends).filter(|(e, _)| !klipp_effects::is_identity_effect(e)).map(|(e, b)| (e.at_render_scale(scale), b)).collect())
}

/// `node`'s mix and mask, if it has either.
fn blend_of(picture: &ClipPicture, node: &Node) -> Option<Blend> {
    use klipp_effects::mask_mix::MaskChannel;
    let edge = picture.graph.edges.iter().find(|e| e.to == (node.id, "mask".into()));
    if node.mix.is_none() && edge.is_none() {
        return None;
    }
    // A lifted mask edge with no channel reads red (the migrated read); a
    // new one is given its channel when it is made. A channel this Klipp
    // doesn't know isn't read as another: the mask can't be given.
    let (channel, unknown) = match node.params.get(&klipp_graph_format::ParamKey::from("mask.channel")) {
        None => (MaskChannel::Red, None),
        Some(klipp_graph_format::ParamValue::Text(c)) => match c.parse() {
            Ok(channel) => (channel, None),
            Err(_) => (MaskChannel::Red, Some(format!("its mask channel {c:?} isn't one this Klipp reads"))),
        },
        Some(other) => (MaskChannel::Red, Some(format!("its mask channel {other:?} isn't a channel's name"))),
    };
    let blend = klipp_nodes::params::EffectBlend { mix: node.mix.unwrap_or(1.0), channel };
    let mask = edge.map(|e| match &unknown {
        Some(why) => MaskSource::Unavailable(why.clone()),
        None => mask_source(picture, &e.from.0, e.from.1.as_str()),
    });
    Some(Blend { blend, mask })
}

/// Where the matte an edge from `from`'s `port` brings comes from.
fn mask_source(picture: &ClipPicture, from: &NodeId, port: &str) -> MaskSource {
    let by_id = |id: &NodeId| picture.graph.nodes.iter().find(|n| n.id == *id);
    let Some(producer) = by_id(from) else {
        return MaskSource::Unavailable(format!("{from} isn't in the clip's picture"));
    };
    let kind_of = |n: &Node| n.legacy.as_ref().and_then(|l| l.payload.get("kind")).and_then(serde_json::Value::as_str).unwrap_or("").to_owned();
    // An output this kind doesn't have isn't read as its first.
    let index = |n: &Node, key: &str| klipp_graph_format::ports::output_index(&kind_of(n), &key.into()).ok_or_else(|| format!("{} has no output {key}", n.id));
    if producer.type_id.as_str() == lt::GROUP {
        let Some(body) = producer.group.as_ref() else { return MaskSource::Unavailable("the grade group has no body".into()) };
        let Some(published) = body.interface.iter().find(|p| p.key.as_str() == port) else {
            return MaskSource::Unavailable(format!("the grade group publishes no {port}"));
        };
        let Some(inner) = body.graph.nodes.iter().find(|n| n.id == published.binding.node) else {
            return MaskSource::Unavailable(format!("the grade's {port} is bound to {}, which isn't in it", published.binding.node));
        };
        return match index(inner, published.binding.port.as_str()) {
            Ok(port) => MaskSource::Grade { node: inner.id, port },
            Err(why) => MaskSource::Unavailable(why),
        };
    }
    if chain(picture).is_ok_and(|c| c.iter().any(|n| n.id == producer.id)) {
        return MaskSource::Unavailable(format!("{} is in the clip's chain, not beside it", producer.id));
    }
    // A node beside the chain: what it reads, and the node the run builds.
    let input = picture.graph.edges.iter().find(|e| e.to.0 == producer.id).and_then(|e| by_id(&e.from.0).map(|n| (n, e.from.1.as_str())));
    let input = match input {
        Some((n, "out")) if n.type_id.as_str() == lt::SOURCE => SideInput::Source,
        Some((n, "out")) if n.type_id.as_str() == lt::GROUP => SideInput::Graded,
        Some((n, key)) => return MaskSource::Unavailable(format!("{} reads {} {key}, which the renderer can't give it", producer.id, n.id)),
        None => SideInput::Source,
    };
    let port = match index(producer, port) {
        Ok(port) => port,
        Err(why) => return MaskSource::Unavailable(why),
    };
    match side_node(producer) {
        Ok(node) => MaskSource::Side { node: Box::new(node), port, input },
        Err(why) => MaskSource::Unavailable(format!("{}: {why}", producer.id)),
    }
}

/// A node beside the chain as the run's node: through the lift's own way
/// back for a grade's nodes, as the one node of a grade.
fn side_node(n: &Node) -> Result<klipp_nodes::Node, String> {
    let body = klipp_graph_format::Graph { nodes: vec![n.clone()], output: n.id, ..klipp_graph_format::Graph::empty() };
    let v1 = klipp_graph_lift::to_v1::grade_only(&klipp_graph_lift::delift::grade_only(&body, NodeId::nil())?)?;
    let graph: klipp_nodes::NodeGraph = serde_json::from_value(v1).map_err(|e| e.to_string())?;
    graph.nodes.into_iter().find(|m| m.id == n.id).ok_or_else(|| "the node didn't come back from its record".into())
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
