//! Mattes on a clip's picture (#1746): a window, a gradient, a roto shape,
//! a qualifier, a luma key or a difference key, made beside the picture's chain from
//! the clip's source, its matte feeding an effect's mask. The grade's own
//! node kinds, run by the same executor: the renderer reads one beside the
//! chain as a side matte (`klipp_compose::resolve::MaskSource::Side`).
//!
//! The Picture view's Mattes menu adds them ([`add_ops`]); the effect they
//! mask shows each one's settings under its "Masked by" chip.
//!
//! Its Sources menu adds, the same way, a picture a Merge lays over the
//! clip's ([`source_ops`]): a generator's solid or gradient.

use klipp_core::{DifferenceKeySettings, LumaKeySettings, Mask, MaskShape, MaskStack, QualifierSettings};
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::op::GraphOp;
use klipp_graph_format::path::NodePath;
use klipp_graph_format::ports::{input_key, output_key};
use klipp_graph_format::{Graph, Node, NodeId};
use klipp_nodes::{NodeKind, NodeParams};

/// A matte the Picture view makes: its menu tag, its name, the grade kind
/// it is, and the settings it starts with.
pub struct Matte {
    pub tag: &'static str,
    pub name: &'static str,
    pub kind: NodeKind,
    params: fn() -> NodeParams,
}

fn window(shape: MaskShape) -> NodeParams {
    NodeParams { masks: Some(MaskStack { masks: vec![Mask { shape, feather: 0.05, ..Mask::default() }] }), ..NodeParams::default() }
}

/// Each matte, in the menu's order. A key starts on, so it acts as it is
/// added: the qualifier selects the reds until its range is picked
/// (#2576); the luma key selects the highlights; the difference key
/// selects everything until a clean plate is chosen.
pub const MATTES: &[Matte] = &[
    Matte { tag: "matte.window", name: "Window", kind: NodeKind::Mask, params: || window(MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.3 }) },
    Matte {
        tag: "matte.rectangle",
        name: "Rectangle Window",
        kind: NodeKind::Mask,
        params: || window(MaskShape::Rectangle { center_x: 0.5, center_y: 0.5, width: 0.5, height: 0.4, rotation: 0.0 }),
    },
    Matte { tag: "matte.gradient", name: "Gradient", kind: NodeKind::Mask, params: || window(MaskShape::LinearGradient { start_x: 0.2, start_y: 0.5, end_x: 0.8, end_y: 0.5 }) },
    Matte {
        tag: "matte.roto",
        name: "Roto Shape",
        kind: NodeKind::Mask,
        params: || window(MaskShape::polygon(vec![(0.5, 0.3), (0.7, 0.5), (0.5, 0.7), (0.3, 0.5)])),
    },
    Matte {
        tag: "matte.qualifier",
        name: "Qualifier",
        kind: NodeKind::Qualifier,
        params: || NodeParams { qualifier: Some(QualifierSettings { enabled: true, ..QualifierSettings::default() }), ..NodeParams::default() },
    },
    Matte {
        tag: "matte.luma_key",
        name: "Luma Key",
        kind: NodeKind::LumaKey,
        params: || NodeParams { luma_key: Some(LumaKeySettings { enabled: true, low: 0.6, high: 1.0, soft: 0.1, invert: false }), ..NodeParams::default() },
    },
    Matte {
        tag: "matte.difference_key",
        name: "Difference Key",
        kind: NodeKind::DifferenceKey,
        params: || NodeParams { difference_key: Some(DifferenceKeySettings { enabled: true, ..DifferenceKeySettings::default() }), ..NodeParams::default() },
    },
];

/// The matte the menu tag `tag` names.
pub fn matte(tag: &str) -> Option<&'static Matte> {
    MATTES.iter().find(|m| m.tag == tag)
}

/// The ops that add `m` beside `graph`'s chain at `level` (its top, where
/// the clip's source is), reading the source, at `position` in the editor,
/// and its matte into `masks`' mask port when that is an effect taking one
/// that nothing feeds yet. `None` when the level has no source.
pub fn add_ops(graph: &Graph, level: &NodePath, m: &Matte, id: NodeId, position: (f32, f32), masks: Option<NodeId>) -> Option<Vec<GraphOp>> {
    let at = |node: NodeId| NodePath::new(level.root, level.nodes.iter().copied().chain([node]));
    let source = graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SOURCE)?.id;
    let kind = serde_json::to_value(m.kind).ok()?.as_str()?.to_owned();
    let node: Node = crate::inspector::grade_view::new_grade_node(m.kind, id);
    let mut ops = vec![
        GraphOp::AddNodeBeside { at: at(source), node: Box::new(node) },
        GraphOp::Connect { from: at(source), from_port: "out".into(), to: at(id), to_port: input_key(&kind, 0) },
        GraphOp::SetNodeMeta { path: at(id), label: Some(m.name.to_owned()), position: Some(serde_json::json!({"x": position.0, "y": position.1})) },
    ];
    // Its starting settings, each structure whole (a mask stack, a key's).
    let params = serde_json::to_value((m.params)()).ok()?;
    for (key, value) in params.as_object()? {
        ops.push(GraphOp::SetParam { path: at(id), key: key.as_str().into(), value: klipp_graph_format::ParamValue::Text(value.to_string()) });
    }
    if let Some(target) = masks.filter(|t| takes_a_mask(graph, *t)) {
        ops.push(GraphOp::Connect { from: at(id), from_port: output_key(&kind, 0), to: at(target), to_port: "mask".into() });
    }
    Some(ops)
}

/// A picture the Sources menu makes beside the chain (#1746): its menu
/// tag, its name, and the effect that draws it, saved as `kind`.
pub struct Source {
    pub tag: &'static str,
    pub name: &'static str,
    kind: &'static str,
    effect: fn() -> klipp_effects::BuiltinEffect,
}

/// Each source, in the menu's order.
pub const SOURCES: &[Source] = &[
    Source {
        tag: "source.solid",
        name: "Solid Colour",
        kind: "generator",
        effect: || klipp_effects::BuiltinEffect::Generator { shape: 0, color_a: [40, 90, 200, 255], color_b: [255, 255, 255, 255], angle: 0.0, center_x: 0.5, center_y: 0.5, radius: 0.5 },
    },
    Source {
        tag: "source.gradient",
        name: "Gradient",
        kind: "generator",
        effect: || klipp_effects::BuiltinEffect::Generator { shape: 1, color_a: [20, 30, 90, 255], color_b: [250, 180, 90, 255], angle: 30.0, center_x: 0.5, center_y: 0.5, radius: 0.5 },
    },
    // Another media item's picture; the item is picked on its card.
    Source {
        tag: "source.media",
        name: "Media",
        kind: "media_source",
        effect: || klipp_effects::BuiltinEffect::MediaSource { media: String::new(), offset: 0.0 },
    },
    // What the tracks below the clip show under it.
    Source {
        tag: "source.below",
        name: "Clip Below",
        kind: "clip_below",
        effect: || klipp_effects::BuiltinEffect::ClipBelow {},
    },
];

/// The source the menu tag `tag` names.
pub fn source(tag: &str) -> Option<&'static Source> {
    SOURCES.iter().find(|s| s.tag == tag)
}

/// The ops that add `s` beside `graph`'s chain at `level`, reading the
/// clip's source (which it replaces), at `position`, and its picture into
/// `merge`'s `fg` when that is a Merge nothing feeds yet. `None` when the
/// level has no source.
pub fn source_ops(graph: &Graph, level: &NodePath, s: &Source, id: NodeId, position: (f32, f32), merge: Option<NodeId>) -> Option<Vec<GraphOp>> {
    let at = |node: NodeId| NodePath::new(level.root, level.nodes.iter().copied().chain([node]));
    let source = graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SOURCE)?.id;
    let mut node = crate::inspector::stack_view::effect_node(s.kind, &(s.effect)())?;
    node.id = id;
    let mut ops = vec![
        GraphOp::AddNodeBeside { at: at(source), node: Box::new(node) },
        GraphOp::Connect { from: at(source), from_port: "out".into(), to: at(id), to_port: "in".into() },
        GraphOp::SetNodeMeta { path: at(id), label: Some(s.name.to_owned()), position: Some(serde_json::json!({"x": position.0, "y": position.1})) },
    ];
    if let Some(target) = merge.filter(|t| takes_a_picture(graph, *t)) {
        ops.push(GraphOp::Connect { from: at(id), from_port: "out".into(), to: at(target), to_port: "fg".into() });
    }
    Some(ops)
}

/// Whether `node` is a Merge whose `fg` nothing feeds.
pub fn takes_a_picture(graph: &Graph, node: NodeId) -> bool {
    let Some(n) = graph.nodes.iter().find(|n| n.id == node) else { return false };
    klipp_graph_lift::effect_params::tag(n) == Some("merge") && !graph.edges.iter().any(|e| e.to == (node, "fg".into()))
}

/// Whether `node` is an effect with a mask port nothing feeds.
pub fn takes_a_mask(graph: &Graph, node: NodeId) -> bool {
    let Some(n) = graph.nodes.iter().find(|n| n.id == node) else { return false };
    let mixable = klipp_graph_lift::effect_params::tag(n).and_then(klipp_effects::registry::effect_type).is_some_and(|t| t.desc.mixable);
    mixable && !graph.edges.iter().any(|e| e.to == (node, "mask".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_graph_format::path::Root;
    use klipp_timeline::test_clip::TestClip;
    use klipp_timeline::Clip;

    fn clip() -> Clip {
        let t = |s| klipp_core::RationalTime::from_secs_f64(s, 25);
        TestClip::of(Clip::media(klipp_core::MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0))).effect(klipp_effects::BuiltinEffect::GaussianBlur { radius: 2.0 }).build()
    }

    /// Each matte, added for the clip's blur, is beside the chain reading
    /// the source, its matte in the blur's mask port, and the renderer
    /// reads it as a side matte.
    #[test]
    fn each_matte_masks_the_effect_it_is_added_for() {
        for m in MATTES {
            let mut c = clip();
            let graph = c.picture_v2().unwrap().picture.graph.clone();
            let blur = graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("gaussian_blur")).unwrap().id;
            let id = NodeId::new_v4();
            let level = NodePath::new(Root::Clip { clip: c.id.0 }, []);
            let ops = add_ops(&graph, &level, m, id, (10.0, 20.0), Some(blur)).unwrap();
            c.apply_picture_ops(&ops).unwrap_or_else(|e| panic!("{}: {e}", m.name));
            let graph = &c.picture_v2().unwrap().picture.graph;
            assert!(graph.edges.iter().any(|e| e.from.0 == id && e.to == (blur, "mask".into())), "{}: wired to the blur's mask", m.name);
            assert!(!takes_a_mask(graph, blur), "{}: the blur's mask is fed", m.name);
            let t = klipp_core::RationalTime::from_secs_f64(1.0, 25);
            let fx = klipp_compose::resolve::resolve_effects_blended(&c, &c.picture_v2().unwrap().picture, t, 25.0, None, None, 1.0).unwrap();
            let mask = fx.iter().find_map(|(_, b)| b.as_ref().and_then(|b| b.mask.clone()));
            assert!(matches!(mask, Some(klipp_compose::resolve::MaskSource::Side { input: klipp_compose::resolve::SideInput::Source, .. })), "{}: {mask:?}", m.name);
        }
    }

    /// A gradient from the Sources menu, with a merge picked, is the
    /// merge's picture (#1746): beside the chain, reading the source, wired
    /// into its `fg`, and the renderer reads it there.
    #[test]
    fn a_source_added_with_a_merge_picked_is_its_picture() {
        let t = |s| klipp_core::RationalTime::from_secs_f64(s, 25);
        let mut c = TestClip::of(Clip::media(klipp_core::MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0))).effect(klipp_effects::BuiltinEffect::Merge { blend_mode: klipp_effects::BlendMode::Normal, opacity: 0.8 }).build();
        let graph = c.picture_v2().unwrap().picture.graph.clone();
        let merge = graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("merge")).unwrap().id;
        assert!(takes_a_picture(&graph, merge));
        let id = NodeId::new_v4();
        let ops = source_ops(&graph, &NodePath::new(Root::Clip { clip: c.id.0 }, []), source("source.gradient").unwrap(), id, (0.0, 0.0), Some(merge)).unwrap();
        c.apply_picture_ops(&ops).unwrap();
        let graph = &c.picture_v2().unwrap().picture.graph;
        assert!(!takes_a_picture(graph, merge), "the merge's picture is fed");
        let fx = klipp_compose::resolve::resolve_effects_blended(&c, &c.picture_v2().unwrap().picture, t(1.0), 25.0, None, None, 1.0).unwrap();
        let fg = fx.iter().find_map(|(_, b)| b.as_ref().and_then(|b| b.fg.clone()));
        assert!(matches!(fg, Some(klipp_compose::resolve::MaskSource::Side { input: klipp_compose::resolve::SideInput::Source, .. })), "{fg:?}");
    }

    /// The Sources menu's Media adds a media source with no item yet,
    /// wired into the merge picked, which the render reads once an item
    /// is chosen on its card (#1746).
    #[test]
    fn a_media_source_added_with_a_merge_picked_waits_for_its_item() {
        let t = |s| klipp_core::RationalTime::from_secs_f64(s, 25);
        let mut c = TestClip::of(Clip::media(klipp_core::MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0))).effect(klipp_effects::BuiltinEffect::Merge { blend_mode: klipp_effects::BlendMode::Normal, opacity: 1.0 }).build();
        let graph = c.picture_v2().unwrap().picture.graph.clone();
        let merge = graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("merge")).unwrap().id;
        let id = NodeId::new_v4();
        let ops = source_ops(&graph, &NodePath::new(Root::Clip { clip: c.id.0 }, []), source("source.media").unwrap(), id, (0.0, 0.0), Some(merge)).unwrap();
        c.apply_picture_ops(&ops).unwrap();
        let graph = &c.picture_v2().unwrap().picture.graph;
        let node = graph.nodes.iter().find(|n| n.id == id).unwrap();
        assert_eq!(klipp_graph_lift::effect_params::tag(node), Some("media_source"));
        assert!(graph.edges.iter().any(|e| e.from.0 == id && e.to == (merge, "fg".into())));
        assert!(klipp_compose::resolve::media_sources(&c).is_empty(), "no item yet: nothing to read");
        let item = klipp_core::MediaId::new().0.to_string();
        c.apply_picture_ops(&[GraphOp::SetParam { path: NodePath::new(Root::Clip { clip: c.id.0 }, [id]), key: "media".into(), value: klipp_graph_format::ParamValue::Text(item.clone()) }]).unwrap();
        assert_eq!(klipp_compose::resolve::media_sources(&c), vec![(item, 0.0)]);
    }

    /// The Sources menu's Clip Below adds what is below the clip, wired
    /// into the merge picked: the clip is drawn over what is below it.
    #[test]
    fn a_clip_below_added_with_a_merge_picked_draws_the_clip_over_what_is_below() {
        let t = |s| klipp_core::RationalTime::from_secs_f64(s, 25);
        let mut c = TestClip::of(Clip::media(klipp_core::MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0))).effect(klipp_effects::BuiltinEffect::Merge { blend_mode: klipp_effects::BlendMode::Screen, opacity: 1.0 }).build();
        assert!(!klipp_compose::resolve::reads_below(&c));
        let graph = c.picture_v2().unwrap().picture.graph.clone();
        let merge = graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("merge")).unwrap().id;
        let id = NodeId::new_v4();
        let ops = source_ops(&graph, &NodePath::new(Root::Clip { clip: c.id.0 }, []), source("source.below").unwrap(), id, (0.0, 0.0), Some(merge)).unwrap();
        c.apply_picture_ops(&ops).unwrap();
        let graph = &c.picture_v2().unwrap().picture.graph;
        assert_eq!(graph.nodes.iter().find(|n| n.id == id).and_then(klipp_graph_lift::effect_params::tag), Some("clip_below"));
        assert!(graph.edges.iter().any(|e| e.from.0 == id && e.to == (merge, "fg".into())));
        assert!(klipp_compose::resolve::reads_below(&c));
    }

    /// Added with nothing to mask, a matte stands beside the chain, read by
    /// nothing until it is wired.
    #[test]
    fn a_matte_with_nothing_to_mask_stands_alone() {
        let mut c = clip();
        let graph = c.picture_v2().unwrap().picture.graph.clone();
        let id = NodeId::new_v4();
        let ops = add_ops(&graph, &NodePath::new(Root::Clip { clip: c.id.0 }, []), &MATTES[0], id, (0.0, 0.0), None).unwrap();
        c.apply_picture_ops(&ops).unwrap();
        let graph = &c.picture_v2().unwrap().picture.graph;
        assert!(graph.nodes.iter().any(|n| n.id == id));
        assert!(!graph.edges.iter().any(|e| e.from.0 == id));
    }
}
