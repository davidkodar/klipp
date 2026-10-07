//! A picture-truth clip (node-graph M2 step 9c) rebuilt field by field by
//! an edit keeps its picture: the piece an overwrite insert or a lifted
//! range leaves on the right, a duplicate, and the adjustment layer a
//! compound clip's effects go to when it is taken apart. Each holds the
//! mask and the mix only its picture keeps (a cut's halves are tested in
//! klipp-timeline's `picture_truth.rs`).

use klipp_core::{MediaId, Timecode};
use klipp_effects::BuiltinEffect as E;
use klipp_graph_format::lifted::{types as lt, ClipPicture};
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_format::{NodeId, PortBinding};
use klipp_graph_lift::ops::GraphOp;
use klipp_nodes::{Edge, Node, NodeGraph, NodeKind, NodeParams};
use klipp_timeline::{Clip, GradeAccess, Track, TrackKind};

fn t(s: f64) -> Timecode {
    Timecode::from_secs_f64(s, 24)
}

/// A media clip over 0..4 s whose grade publishes a qualifier's matte,
/// masking its blur, mixed at a half; and the blur's node.
fn masked() -> (Clip, NodeId) {
    let mut c = Clip::media(MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
    c.push_effect(E::GaussianBlur { radius: 3.0 });
    let mut g = NodeGraph::default();
    g.ensure_corrector();
    let source = g.nodes.iter().find(|n| n.kind == NodeKind::Source).unwrap().id;
    let q = Node { id: NodeId::new_v4(), kind: NodeKind::Qualifier, label: String::new(), enabled: true, params: NodeParams::default(), keyframes: Default::default(), output_link: None, input_links: Vec::new() };
    g.edges.push(Edge { from_node: source, from_port: 0, to_node: q.id, to_port: 0 });
    let qualifier = q.id;
    g.nodes.push(q);
    c.set_grade(g);
    let p = c.picture_v2().unwrap().picture.clone();
    let group = p.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP).unwrap().id;
    let blur = p.graph.nodes.iter().find(|n| n.type_id.as_str() == klipp_graph_format::types::LEGACY_EFFECT).unwrap().id;
    let at = |id| NodePath::new(Root::Clip { clip: c.id.0 }, [id]);
    c.apply_picture_ops(&[
        GraphOp::PublishOutput { group: at(group), endpoint: PortBinding { node: qualifier, port: "out".into() }, key: "matte_selection".into(), label: "Selection".into() },
        GraphOp::Connect { from: at(group), from_port: "matte_selection".into(), to: at(blur), to_port: "mask".into() },
        GraphOp::SetMix { path: at(blur), mix: Some(0.5) },
    ])
    .unwrap();
    (c, blur)
}

/// The piece keeps the picture: marked picture-truth, the blur mixed at a
/// half and masked from the grade's port.
fn keeps_the_picture(piece: &Clip, blur: NodeId, what: &str) {
    assert!(klipp_timeline::take_picture_refusal().is_none(), "{what}: refused");
    assert!(piece.is_picture_truth(), "{what}: not picture-truth");
    let p: ClipPicture = piece.picture_v2().unwrap().picture.clone();
    let node = p.graph.nodes.iter().find(|n| n.id == blur).unwrap_or_else(|| panic!("{what}: the blur's node is gone"));
    assert_eq!(node.mix, Some(0.5), "{what}: the mix");
    assert!(p.graph.edges.iter().any(|e| e.to == (blur, "mask".into())), "{what}: the mask");
}

fn track_with(c: &Clip) -> Track {
    let mut track = Track::new("V1", TrackKind::Video);
    track.clips.push(c.clone());
    track
}

#[test]
fn an_overwrite_insert_leaves_the_right_piece_its_picture() {
    let (c, blur) = masked();
    let mut track = track_with(&c);
    let inserted = Clip::media(MediaId::new(), t(1.0), t(2.0), t(0.0), t(1.0));
    super::KlippApp::overwrite_insert_clip_on_track(&mut track, inserted);
    let right = track.clips.iter().find(|x| x.timeline_in == t(2.0)).expect("the right piece");
    keeps_the_picture(right, blur, "overwrite insert");
}

#[test]
fn a_lifted_range_leaves_the_right_piece_its_picture() {
    let (c, blur) = masked();
    let mut track = track_with(&c);
    super::KlippApp::lift_range(&mut track, t(1.0), t(2.0));
    let right = track.clips.iter().find(|x| x.timeline_in == t(2.0)).expect("the right piece");
    keeps_the_picture(right, blur, "lift range");
}

#[test]
fn a_duplicate_keeps_its_own_picture() {
    let (c, blur) = masked();
    let copy = super::clips::duplicate_after(&c);
    assert_ne!(copy.id, c.id);
    keeps_the_picture(&copy, blur, "duplicate");
}

/// What taking a compound clip apart does to its picture: an adjustment
/// layer is built from its fields and takes its picture
/// (`decompose_compound_clip_impl`).
#[test]
fn a_compound_taken_apart_gives_its_adjustment_layer_the_picture() {
    let (c, blur) = masked();
    let mut layer = Clip { kind: klipp_timeline::ClipKind::AdjustmentLayer, ..Clip::gap(t(0.0), t(4.0)) };
    layer.set_effect_list(c.builtin_effects().to_vec(), c.builtin_effects_enabled().to_vec());
    layer.set_grade(c.grade().clone());
    layer.take_picture_truth_of(&c);
    keeps_the_picture(&layer, blur, "compound taken apart");
}
