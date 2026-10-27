//! A clip's effects for one frame, read from its lifted picture (#1089,
//! phase 2b M2, step 10c).
//!
//! The lifted picture holds the clip's chain in render order: the grade,
//! the display conversion, OFX plug-ins, the built-in effects, and the
//! placement where the transform goes. [`resolve_effects`] walks it from
//! the display conversion to the output and resolves each enabled effect
//! node for the frame: its values read, its own animation applied
//! (the node's `anim`, keyed by parameter name), then what the frame
//! gives it (`frame_inputs`), and the clip's transform where the lift
//! placed it. It answers what [`crate::render::clip_effects`] answers
//! for the same clip and time, so the renderer can take its effects from
//! the picture without a picture changing.
//!
//! Where the clip's transform goes ([`placement_cut`]) is one rule, read by
//! both renderer routes (this and `place_and_finish` on a list) and by the
//! inspector ([`resolved_placement`], M3 step 8), so the inspector never
//! reimplements it.

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
/// M2 step 9c): its mix, and the matte its mask port reads, by channel;
/// and for a Merge, the picture its `fg` port lays over (#1746).
#[derive(Clone, Debug, PartialEq)]
pub struct Blend {
    pub blend: klipp_nodes::params::EffectBlend,
    pub mask: Option<MaskSource>,
    pub fg: Option<MaskSource>,
    /// A grade compound's grade (#1746), for its `GradeCompound` marker:
    /// the executor's compound node runs it.
    pub compound: Option<std::sync::Arc<klipp_nodes::NodeGraph>>,
}

/// Where a masked effect's matte comes from, for the run that draws it;
/// or a Merge's picture (#1746).
#[derive(Clone, Debug, PartialEq)]
pub enum MaskSource {
    /// A node inside the clip's grade, published by the grade group: the
    /// grade run's node of the same id (the lift keeps grade node ids), at
    /// output `port`.
    Grade { node: NodeId, port: u8 },
    /// A node beside the clip's chain (a window, a key), built into the
    /// run, reading `input`, at output `port`; after `lead` (#2758), the
    /// effects before it on its branch, in order, the first reading
    /// `input`: a keyed, moved, blurred picture a merge lays over.
    Side { node: Box<klipp_nodes::Node>, lead: Vec<klipp_nodes::Node>, port: u8, input: SideInput },
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
    /// The picture the clip's grade reads, handed to a run apart from that
    /// grade as a side picture (`SideKey::GradeInput`, #1265): a key's,
    /// drawn in the effect's own plan.
    GradeInput,
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
    resolve_effect_nodes(clip, picture, time, fps, resolved, pin, scale).map(|fx| fx.into_iter().map(|(e, b, _)| (e, b)).collect())
}

/// [`resolve_effects_blended`], each effect with the picture node it came
/// from (M3 step 16a): the placement's for the clip's transform. The node
/// a run of the effects gives it, so an inspection by path names it.
pub fn resolve_effect_nodes(
    clip: &Clip,
    picture: &ClipPicture,
    time: Timecode,
    fps: f64,
    resolved: Option<ResolvedTransform>,
    pin: Option<klipp_timeline::surface::Quad>,
    scale: f32,
) -> Result<Vec<(BuiltinEffect, Option<Blend>, Option<NodeId>)>, String> {
    resolve_effect_nodes_and_identities(clip, picture, time, fps, resolved, pin, scale).map(|(fx, _)| fx)
}

/// What feeds an effect dropped from the run as an identity (#1314):
/// the run's picture before it, which it would pass through unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityFeed {
    /// The grade's output: no effect runs before it.
    Grade,
    /// The output of the run's effect before it, this picture node.
    Node(NodeId),
}

/// [`resolve_effect_nodes`], and each effect node it drops as an identity
/// with what feeds it in the run. One after an effect with no picture
/// node (the list route's) isn't named: nothing could answer for it.
#[allow(clippy::type_complexity)]
pub fn resolve_effect_nodes_and_identities(
    clip: &Clip,
    picture: &ClipPicture,
    time: Timecode,
    fps: f64,
    resolved: Option<ResolvedTransform>,
    pin: Option<klipp_timeline::surface::Quad>,
    scale: f32,
) -> Result<(Vec<(BuiltinEffect, Option<Blend>, Option<NodeId>)>, Vec<(NodeId, IdentityFeed)>), String> {
    let w = walk(clip, picture, time, fps, resolved, pin)?;
    let (mut fx, mut blends) = (w.fx, w.blends);
    let mut nodes: Vec<Option<NodeId>> = w.nodes.into_iter().map(Some).collect();
    match w.cut {
        PlacementCut::Fixed { at } | PlacementCut::Auto { at, .. } => {
            fx.insert(at, transform_effect(w.xf, w.three_d));
            blends.insert(at, None);
            nodes.insert(at, w.placement);
        }
        PlacementCut::Identity => {}
    }
    let (mut kept, mut identities) = (Vec::new(), Vec::new());
    let mut feed = Some(IdentityFeed::Grade);
    for ((e, b), n) in fx.into_iter().zip(blends).zip(nodes) {
        if klipp_effects::is_identity_effect(&e) {
            if let (Some(node), Some(feed)) = (n, feed) {
                identities.push((node, feed));
            }
        } else {
            feed = n.map(IdentityFeed::Node);
            kept.push((e.at_render_scale(scale), b, n));
        }
    }
    Ok((kept, identities))
}

/// The node of `picture` that places it, its placement, when it has one:
/// what a resolved effect list names its move by (#1923).
pub(crate) fn placement_node_of(picture: &ClipPicture) -> Option<NodeId> {
    chain(picture).ok()?.iter().find(|n| n.type_id.as_str() == lt::PLACEMENT).map(|n| n.id)
}

/// Where the clip's transform goes among its pixel effects (R2-R4), the
/// one rule both renderer routes and the inspector read (M3 step 8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlacementCut {
    /// Fixed: at `at`, where the first enabled Transform was.
    Fixed { at: usize },
    /// Automatic: before the first effect drawn outside the picture
    /// (`outside`, then `at == outside`), which draws in the margin the
    /// move leaves; at the end (`at` is the count) when none does.
    Auto { at: usize, outside: Option<usize> },
    /// Nothing to place: an identity move and no fixed Transform.
    Identity,
}

/// The cut for pixel effects `fx`: fixed at `fixed_at` when the clip has a
/// fixed Transform there, else nothing when the move is an identity, else
/// before the first effect drawn outside the picture.
pub fn placement_cut(fx: &[BuiltinEffect], fixed_at: Option<usize>, identity: bool) -> PlacementCut {
    match fixed_at {
        Some(at) => PlacementCut::Fixed { at },
        None if identity => PlacementCut::Identity,
        None => {
            let outside = fx.iter().position(klipp_effects::draws_outside_the_picture);
            PlacementCut::Auto { at: outside.unwrap_or(fx.len()), outside }
        }
    }
}

/// A clip's chain read for one frame: its pixel effects in order, with the
/// node each came from and its blend, and the transform's cut.
struct Walk {
    fx: Vec<BuiltinEffect>,
    nodes: Vec<NodeId>,
    /// The placement node, where the clip's transform is placed.
    placement: Option<NodeId>,
    blends: Vec<Option<Blend>>,
    cut: PlacementCut,
    xf: (f32, f32, f32, f32, f32, f32, f32),
    three_d: (f32, f32, f32),
}

fn walk(clip: &Clip, picture: &ClipPicture, time: Timecode, fps: f64, resolved: Option<ResolvedTransform>, pin: Option<klipp_timeline::surface::Quad>) -> Result<Walk, String> {
    let t = crate::render::eval_clip_time(clip, time);
    let ctx = eval::eval_context(clip, time, fps);
    let mut fx = Vec::new();
    let mut nodes = Vec::new();
    let mut blends: Vec<Option<Blend>> = Vec::new();
    let mut pinned = false;
    // The clip's transform (R2-R4): the placement node, at the first
    // enabled Transform's place (fixed, with that Transform), or automatic.
    let mut placement: Option<(usize, Option<BuiltinEffect>)> = None;
    let mut placement_node: Option<NodeId> = None;
    let chain = chain(picture)?;
    let start = chain.iter().position(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY).map_or(0, |i| i + 1);
    for node in &chain[start..] {
        let kind = node.type_id.as_str();
        if kind == lt::PLACEMENT {
            placement_node = Some(node.id);
            let fixed = match &node.legacy {
                Some(l) if node.enabled => Some(decode(node, &l.payload, t, &ctx)?),
                _ => None,
            };
            placement = Some((fx.len(), fixed));
            continue;
        }
        // A grade in the chain (#1746): its marker, and its grade for the
        // run, which only a plan draws.
        if node.enabled && kind == lt::GRADE_COMPOUND {
            let body = &node.group.as_ref().ok_or_else(|| format!("grade compound {} has no body", node.id))?.graph;
            let run = klipp_timeline::lowered_grade(body)?;
            fx.push(BuiltinEffect::GradeCompound {});
            nodes.push(node.id);
            // Its own mix: what it grades, the picture it read the rest.
            let blend = klipp_nodes::params::EffectBlend { mix: node.mix.unwrap_or(1.0), channel: klipp_effects::mask_mix::MaskChannel::Red };
            blends.push(Some(Blend { blend, mask: None, fg: None, compound: Some(std::sync::Arc::new(run)) }));
            continue;
        }
        // Opacity, a speed ramp, temporal denoise and extra Transforms are
        // other node types, or disabled: none is a pixel effect here.
        if !node.enabled || kind != types::LEGACY_EFFECT {
            continue;
        }
        let mut effect = klipp_graph_lift::effect_params::effect_of(node)?;
        EffectNodeAnim::of(node)?.animate(&mut effect, t, &ctx);
        frame_inputs(clip, &mut effect, t, fps, pin, &mut pinned);
        fx.push(effect);
        nodes.push(node.id);
        let mut blend = blend_of(picture, node, t, &ctx);
        if let Some(b) = blend.as_mut() {
            side_surfaces(clip, b, t, fps);
        }
        blends.push(blend);
    }
    let xf = resolved.unwrap_or_else(|| eval::evaluate_clip_own_transform(clip, t, &ctx));
    let fixed = placement.as_ref().and_then(|(_, f)| f.clone());
    let three_d = tilt(clip, fixed.as_slice(), t);
    let identity = xf == (0.0, 0.0, 1.0, 1.0, 0.0, 0.5, 0.5) && three_d.0 == 0.0 && three_d.1 == 0.0;
    // The lift keeps an automatic placement node at the chain's end; only
    // a fixed one's place says where the move goes. No placement node:
    // nothing is placed.
    let cut = match placement {
        Some((at, Some(_))) => placement_cut(&fx, Some(at), identity),
        Some((_, None)) => placement_cut(&fx, None, identity),
        None => PlacementCut::Identity,
    };
    Ok(Walk { fx, nodes, placement: placement_node, blends, cut, xf, three_d })
}

/// Where `clip`'s placement applies at `time`, as the renderer decides it
/// ([`placement_cut`] over the same walk of its picture): the node the move
/// goes before, or the end, why, and whose node that is (the clip's own,
/// or its track's). `None` when the clip's picture has no placement. What
/// the inspector's Placement row shows (M3 step 8); it never decides it.
///
/// `clip` is the render copy the renderer draws (its track's effects
/// baked after its own, [`klipp_timeline::Sequence::baked_for_render`]),
/// `picture` its lifted picture, and `track` the track it sits on: a node
/// past the clip's own effects is that track's. `resolved` is the move as
/// the renderer resolves it (a parent's, the camera's), or the clip's own
/// when `None`. [`placement_in`] gives all of these from a sequence.
pub fn resolved_placement(
    clip: &Clip,
    picture: &ClipPicture,
    time: Timecode,
    fps: f64,
    resolved: Option<ResolvedTransform>,
    track: Option<klipp_core::TrackId>,
) -> Result<Option<klipp_graph_lift::stack_view::ResolvedPlacement>, String> {
    use klipp_graph_format::lifted::Origin;
    use klipp_graph_format::path::{NodePath, Root, TrackPart};
    use klipp_graph_lift::ids::{entry_id, RootKind};
    use klipp_graph_lift::stack_view::{PlacementBoundary, ResolvedPlacement};
    if !picture.graph.nodes.iter().any(|n| n.type_id.as_str() == lt::PLACEMENT) {
        return Ok(None);
    }
    let w = walk(clip, picture, time, fps, resolved, None)?;
    // Each walked node by the path that names it beyond this copy: the
    // clip's own by the clip, a track's by the track. The copy's track
    // effects follow its own and are given new ids at each bake, so one at
    // slot `own + k` is addressed as the track's `k`th effect.
    let own = clip.baked_own.map(|o| o.effects);
    let mut probe = clip.clone();
    let addresses: Vec<(NodePath, Option<Origin>)> = w
        .nodes
        .iter()
        .map(|id| {
            let origin = picture.graph.nodes.iter().find(|n| n.id == *id).and_then(|n| n.origin);
            let slot = probe.index_of(klipp_timeline::EffectId::of_node(*id));
            match (origin, own, track, slot) {
                (None, Some(own), Some(track), Some(i)) if i >= own => {
                    let k = entry_id(RootKind::Track, track.0, "effects", i - own);
                    (NodePath::new(Root::Track { track: track.0, part: TrackPart::Effects }, [k]), Some(Origin::Track { track: track.0 }))
                }
                (Some(Origin::Track { track }), ..) => (NodePath::new(Root::Track { track, part: TrackPart::Effects }, [*id]), origin),
                _ => (NodePath::new(Root::Clip { clip: clip.id.0 }, [*id]), origin),
            }
        })
        .collect();
    let before = |at: usize| match addresses.get(at) {
        Some((path, origin)) => (PlacementBoundary::Before(path.clone()), *origin),
        None => (PlacementBoundary::End, None),
    };
    let name = |at: usize| {
        let label = w.nodes.get(at).and_then(|id| picture.graph.nodes.iter().find(|n| n.id == *id)).map(|n| n.label.clone()).unwrap_or_default();
        if label.is_empty() { w.fx.get(at).map(effect_name).unwrap_or_default() } else { label }
    };
    let (boundary, provenance, reason) = match w.cut {
        PlacementCut::Fixed { at } => {
            let (b, o) = before(at);
            (b, o, if at < w.fx.len() { format!("Fixed · before {}", name(at)) } else { "Fixed · end".into() })
        }
        PlacementCut::Auto { at, outside: Some(_) } => {
            let (b, o) = before(at);
            (b, o, format!("Auto · before {}, which draws outside the picture", name(at)))
        }
        PlacementCut::Auto { .. } => (PlacementBoundary::End, None, "Auto · end".into()),
        PlacementCut::Identity => (PlacementBoundary::End, None, "Auto · nothing to move".into()),
    };
    let plan_revision = plan_revision(&w, &addresses);
    Ok(Some(ResolvedPlacement { boundary, reason, provenance, plan_revision }))
}

/// [`resolved_placement`] for the clip `id` of `seq` at `time`: on its
/// render copy, its track named. `None` when no track holds `id` or its
/// picture has no placement.
pub fn placement_in(
    seq: &klipp_timeline::Sequence,
    id: klipp_core::ClipId,
    time: Timecode,
    fps: f64,
    resolved: Option<ResolvedTransform>,
) -> Result<Option<klipp_graph_lift::stack_view::ResolvedPlacement>, String> {
    let baked = seq.baked_for_render();
    let Some((track, clip)) = baked.tracks.iter().find_map(|t| t.clips.iter().find(|c| c.id == id).map(|c| (t.id, c))) else {
        return Ok(None);
    };
    let lifted = clip.picture_v2()?;
    resolved_placement(clip, &lifted.picture, time, fps, resolved, Some(track))
}

/// An effect's name as the browser shows it, for a reason a person reads.
fn effect_name(effect: &BuiltinEffect) -> String {
    let tag = serde_json::to_value(effect).ok().and_then(|v| v.get("type").and_then(serde_json::Value::as_str).map(str::to_owned));
    tag.as_deref().and_then(klipp_effects::registry::effect_type).map_or_else(|| tag.unwrap_or_else(|| "an effect".into()), |t| t.desc.name.to_owned())
}

/// A revision of the resolution: it changes whenever the chain the move
/// is placed in does (which effects are drawn, in what order, by which
/// node) or the cut does, and not when a value animates, so a view
/// holding an older one knows to read again.
fn plan_revision(w: &Walk, addresses: &[(klipp_graph_format::path::NodePath, Option<klipp_graph_format::lifted::Origin>)]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for ((path, _), effect) in addresses.iter().zip(&w.fx) {
        path.hash(&mut h);
        effect_name(effect).hash(&mut h);
    }
    format!("{:?}", w.cut).hash(&mut h);
    h.finish()
}

/// `node`'s mix and mask, if it has either.
fn blend_of(picture: &ClipPicture, node: &Node, t: f64, ctx: &klipp_core::EvalContext) -> Option<Blend> {
    use klipp_effects::mask_mix::MaskChannel;
    let edge = picture.graph.edges.iter().find(|e| e.to == (node.id, "mask".into()));
    // A Merge's picture (#1746), from beside the chain as a matte is.
    let fg = picture.graph.edges.iter().find(|e| e.to == (node.id, "fg".into())).map(|e| mask_source(picture, &e.from.0, e.from.1.as_str(), t, ctx));
    if node.mix.is_none() && edge.is_none() && fg.is_none() {
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
        None => mask_source(picture, &e.from.0, e.from.1.as_str(), t, ctx),
    });
    Some(Blend { blend, mask, fg, compound: None })
}

/// The surface pins on `blend`'s branches given their corners at `t`
/// ([`crate::render::surface_inputs`]): a branch's nodes are read alone,
/// without the clip whose surfaces they follow.
fn side_surfaces(clip: &Clip, blend: &mut Blend, t: f64, fps: f64) {
    for source in [&mut blend.mask, &mut blend.fg].into_iter().flatten() {
        if let MaskSource::Side { node, lead, .. } = source {
            for n in lead.iter_mut().chain(std::iter::once(node.as_mut())) {
                for e in &mut n.params.effects {
                    crate::render::surface_inputs(clip, e, t, fps);
                }
            }
        }
    }
}

/// The most nodes a branch beside the chain runs before its last (#2758).
const MAX_BRANCH: usize = 16;

/// `side` (with the `lead` before it on its branch) put into `graph`,
/// reading `feed`'s first output, once: a node already there isn't added
/// or wired again.
pub(crate) fn wire_side(graph: &mut klipp_nodes::NodeGraph, feed: klipp_nodes::NodeId, lead: &[klipp_nodes::Node], side: &klipp_nodes::Node) {
    let mut from = feed;
    for node in lead.iter().chain(std::iter::once(side)) {
        if !graph.nodes.iter().any(|n| n.id == node.id) {
            graph.nodes.push(node.clone());
            graph.edges.push(klipp_nodes::Edge { from_node: from, from_port: 0, to_node: node.id, to_port: 0 });
        }
        from = node.id;
    }
}

/// Where the matte (or a Merge's picture) an edge from `from`'s `port`
/// brings comes from, at clip time `t`.
fn mask_source(picture: &ClipPicture, from: &NodeId, port: &str, t: f64, ctx: &klipp_core::EvalContext) -> MaskSource {
    let by_id = |id: &NodeId| picture.graph.nodes.iter().find(|n| n.id == *id);
    let kind_of = |n: &Node| n.legacy.as_ref().and_then(|l| l.payload.get("kind")).and_then(serde_json::Value::as_str).unwrap_or("").to_owned();
    // Through any reroutes (#1746) to the node that makes it: a reroute
    // passes on what it reads.
    let (mut from, mut port) = (*from, port.to_owned());
    for _ in 0..=picture.graph.nodes.len() {
        let Some(n) = by_id(&from).filter(|n| kind_of(n) == "Reroute") else { break };
        let Some(e) = picture.graph.edges.iter().find(|e| e.to.0 == n.id) else {
            return MaskSource::Unavailable(format!("{} reads nothing", n.id));
        };
        (from, port) = (e.from.0, e.from.1.as_str().to_owned());
    }
    let (from, port) = (&from, port.as_str());
    let Some(producer) = by_id(from) else {
        return MaskSource::Unavailable(format!("{from} isn't in the clip's picture"));
    };
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
    // What it reads, through any reroutes (#1746), and back along its
    // branch (#2758): each node beside the chain it reads, to the clip's
    // source or its grade. A branch that reads the chain past those, or
    // runs in a loop, can't be given.
    let read_by = |id: NodeId| {
        let mut input = picture.graph.edges.iter().find(|e| e.to.0 == id).and_then(|e| by_id(&e.from.0).map(|n| (n, e.from.1.as_str())));
        for _ in 0..=picture.graph.nodes.len() {
            let Some((n, _)) = input.filter(|(n, _)| kind_of(n) == "Reroute") else { break };
            input = picture.graph.edges.iter().find(|e| e.to.0 == n.id).and_then(|e| by_id(&e.from.0).map(|n| (n, e.from.1.as_str())));
        }
        input
    };
    let chain_ids: Vec<NodeId> = chain(picture).map(|c| c.iter().map(|n| n.id).collect()).unwrap_or_default();
    let mut lead: Vec<klipp_nodes::Node> = Vec::new();
    let mut at = producer;
    let input = loop {
        match read_by(at.id) {
            Some((n, "out")) if n.type_id.as_str() == lt::SOURCE => break SideInput::Source,
            Some((n, "out")) if n.type_id.as_str() == lt::GROUP => break SideInput::Graded,
            Some((n, "out")) if !chain_ids.contains(&n.id) && n.id != producer.id && !lead.iter().any(|l| l.id == n.id) && lead.len() < MAX_BRANCH => {
                match side_node(n, t, ctx) {
                    Ok(node) => lead.push(node),
                    Err(why) => return MaskSource::Unavailable(format!("{}: {why}", n.id)),
                }
                at = n;
            }
            Some((n, key)) => return MaskSource::Unavailable(format!("{} reads {} {key}, which the renderer can't give it", at.id, n.id)),
            None => break SideInput::Source,
        }
    };
    lead.reverse();
    let port = match index(producer, port) {
        Ok(port) => port,
        Err(why) => return MaskSource::Unavailable(why),
    };
    match side_node(producer, t, ctx) {
        Ok(node) => MaskSource::Side { node: Box::new(node), lead, port, input },
        Err(why) => MaskSource::Unavailable(format!("{}: {why}", producer.id)),
    }
}

/// The items and offsets `clip`'s media source nodes read (#1746), on
/// its chain or beside it: each enabled one with an item chosen, once.
pub fn media_sources(clip: &Clip) -> Vec<(String, f32)> {
    let Ok(lifted) = clip.picture_v2() else { return Vec::new() };
    let mut found: Vec<(String, f32)> = Vec::new();
    for n in lifted.picture.graph.nodes.iter().filter(|n| n.enabled && klipp_graph_lift::effect_params::tag(n) == Some("media_source")) {
        if let Ok(BuiltinEffect::MediaSource { media, offset }) = klipp_graph_lift::effect_params::effect_of(n) {
            if !media.is_empty() && !found.iter().any(|(m, o)| *m == media && o.to_bits() == offset.to_bits()) {
                found.push((media, offset));
            }
        }
    }
    found
}

/// The titles `clip`'s title source nodes draw (#2758), as their files
/// store them: each enabled one with a title, once.
pub fn title_sources(clip: &Clip) -> Vec<String> {
    let Ok(lifted) = clip.picture_v2() else { return Vec::new() };
    let mut found: Vec<String> = Vec::new();
    for n in lifted.picture.graph.nodes.iter().filter(|n| n.enabled && klipp_graph_lift::effect_params::tag(n) == Some("title_source")) {
        if let Ok(BuiltinEffect::TitleSource { title }) = klipp_graph_lift::effect_params::effect_of(n) {
            if !title.is_empty() && !found.contains(&title) {
                found.push(title);
            }
        }
    }
    found
}

/// Whether `clip`'s picture holds an enabled Clip Below node (#1746): the
/// clip is drawn over what is below it.
pub fn reads_below(clip: &Clip) -> bool {
    clip.picture_v2().is_ok_and(|p| p.picture.graph.nodes.iter().any(|n| n.enabled && klipp_graph_lift::effect_params::tag(n) == Some("clip_below")))
}

/// Whether `clip`'s picture holds an enabled grade compound (#1746).
pub fn has_grade_compound(clip: &Clip) -> bool {
    clip.is_picture_truth() && clip.picture_v2().is_ok_and(|p| p.picture.graph.nodes.iter().any(|n| n.enabled && klipp_graph_lift::grade_compound::is_compound(n)))
}

/// A media source's item id as the pool keys it.
/// The source times `clip`'s enabled difference keys pick from its own
/// media as their clean plates (#2758), in its grade and beside its
/// chain, each once.
pub fn plate_frames(clip: &Clip) -> Vec<f64> {
    let picked = |p: &klipp_nodes::NodeParams| p.difference_key.as_ref().filter(|d| d.enabled).and_then(|d| d.plate_frame);
    use klipp_timeline::GradeAccess;
    let mut found: Vec<f64> = clip.grade().nodes.iter().filter(|n| n.kind == klipp_nodes::NodeKind::DifferenceKey && n.enabled).filter_map(|n| picked(&n.params)).collect();
    if let Ok(lifted) = clip.picture_v2() {
        for n in lifted.picture.graph.nodes.iter().filter(|n| n.enabled) {
            let Some(payload) = n.legacy.as_ref().map(|l| &l.payload) else { continue };
            if payload.get("kind").and_then(serde_json::Value::as_str) != Some("DifferenceKey") {
                continue;
            }
            if let Some(secs) = payload.get("params").and_then(|p| serde_json::from_value::<klipp_nodes::NodeParams>(p.clone()).ok()).as_ref().and_then(picked) {
                found.push(secs);
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    found.retain(|s| seen.insert(s.to_bits()));
    found
}

/// Why a clean frame picked at `secs` (#2758) can't be read, before
/// anything is decoded: a time that isn't one, or one outside the media's
/// `range` of source seconds (when the source knows it; else before 0).
/// `None` when it can. Never moved to the nearest frame.
pub fn clean_frame_refused(secs: f64, range: Option<(f64, f64)>) -> Option<String> {
    if !secs.is_finite() {
        return Some(format!("its clean frame's time ({secs}) isn't a time"));
    }
    let (start, end) = range.unwrap_or((0.0, f64::INFINITY));
    (!(start..end).contains(&secs)).then(|| match range {
        Some(_) => format!("its clean frame at {secs:.3} s is outside the media ({start:.3} to {end:.3} s)"),
        None => format!("its clean frame at {secs:.3} s is before the media's start"),
    })
}

pub fn media_id(media: &str) -> Option<klipp_core::MediaId> {
    media.parse().ok().map(klipp_core::MediaId)
}

/// A node beside the chain as the run's node: through the lift's own way
/// back for a grade's nodes, as the one node of a grade; an effect (a
/// generator, #1746) as an effect node, animated for the frame, and a
/// media source as the side picture node that draws its frame.
fn side_node(n: &Node, t: f64, ctx: &klipp_core::EvalContext) -> Result<klipp_nodes::Node, String> {
    if klipp_graph_lift::effect_params::tag(n).is_none() {
        return klipp_timeline::grade_node_for_run(n);
    }
    // A Transform beside the chain (#2758), a branch's move, holds its
    // entry as a placement's does: read from it.
    let effect = if n.type_id.as_str() == types::LEGACY_TRANSFORM {
        let payload = n.legacy.as_ref().map(|l| &l.payload).ok_or_else(|| format!("{} has no entry", n.id))?;
        decode(n, payload, t, ctx)?
    } else {
        let mut effect = klipp_graph_lift::effect_params::effect_of(n)?;
        EffectNodeAnim::of(n)?.animate(&mut effect, t, ctx);
        effect
    };
    // A media source's frame, or what is below, is handed to the run
    // (#1746).
    let kind = if klipp_effects::draws_a_side_picture(&effect) { klipp_nodes::NodeKind::SidePicture } else { klipp_nodes::NodeKind::Effect };
    Ok(klipp_nodes::Node {
        id: n.id,
        kind,
        label: n.label.clone(),
        enabled: n.enabled,
        params: klipp_nodes::NodeParams { effects: vec![effect], ..Default::default() },
        keyframes: Default::default(),
        output_link: None,
        input_links: Vec::new(),
    })
}

/// A fixed placement's Transform entry as its effect, animated for the
/// frame.
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
