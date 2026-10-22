//! Picture-truth clips (node-graph M2 step 9c): a clip whose picture holds
//! what its fields can't (an effect's mix or mask, a port its grade
//! publishes) keeps that picture as its truth, and its fields are a view of
//! it. Writes through the fields are mapped onto the picture or refused;
//! copies keep their own picture; undo puts back the clip as it was.

use klipp_core::{MediaId, RationalTime};
use klipp_effects::BuiltinEffect as E;
use klipp_graph_format::lifted::{types as lt, ClipPicture};
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_format::{NodeId, ParamValue, PortBinding};
use klipp_graph_lift::ops::GraphOp;
use klipp_nodes::{Edge, Node, NodeGraph, NodeKind, NodeParams};
use klipp_timeline::{Clip, GradeAccess};
use serde_json::Value;

#[allow(dead_code)]
mod support;
use support::picture_ops_check::{effects, picture};

fn at(c: &Clip, ids: impl IntoIterator<Item = NodeId>) -> NodePath {
    NodePath::new(Root::Clip { clip: c.id.0 }, ids)
}

fn saved(c: &Clip) -> Value {
    serde_json::to_value(klipp_timeline::picture_v1::ClipPictureV1::of(c)).unwrap()
}

fn node(kind: NodeKind) -> Node {
    Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params: NodeParams::default(), keyframes: Default::default(), output_link: None, input_links: Vec::new() }
}

/// A media clip with a blur and a vignette, graded by a corrector, with a
/// qualifier beside it reading the source: the matte a mask publishes.
/// Gives the clip, the qualifier and the corrector.
fn clip() -> (Clip, NodeId, NodeId) {
    let t = |s: f64| RationalTime::from_secs_f64(s, 24);
    let mut c = Clip::media(MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
    c.push_effect(E::GaussianBlur { radius: 3.0 });
    c.push_effect(E::Vignette { amount: 0.5, size: 0.5, softness: 0.5, roundness: 0.5 });
    let mut g = NodeGraph::default();
    let corrector = g.ensure_corrector();
    let source = g.nodes.iter().find(|n| n.kind == NodeKind::Source).expect("a source").id;
    let qualifier = node(NodeKind::Qualifier);
    g.edges.push(Edge { from_node: source, from_port: 0, to_node: qualifier.id, to_port: 0 });
    let q = qualifier.id;
    g.nodes.push(qualifier);
    c.set_grade(g);
    (c, q, corrector)
}

fn grade_group(p: &ClipPicture) -> NodeId {
    p.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP).expect("a grade group").id
}

/// The ops that publish the qualifier's matte from the grade, mask the
/// blur with it and mix the blur at a half: none of which a clip's fields
/// can hold.
fn masking(c: &Clip, qualifier: NodeId) -> Vec<GraphOp> {
    let p = picture(c);
    let (g, blur) = (grade_group(&p), effects(&p)[0]);
    vec![
        GraphOp::PublishOutput { group: at(c, [g]), endpoint: PortBinding { node: qualifier, port: "out".into() }, key: "matte_selection".into(), label: "Selection".into() },
        GraphOp::Connect { from: at(c, [g]), from_port: "matte_selection".into(), to: at(c, [blur]), to_port: "mask".into() },
        GraphOp::SetMix { path: at(c, [blur]), mix: Some(0.5) },
    ]
}

/// A clip made picture-truth by [`masking`], with its blur's node.
fn masked() -> (Clip, NodeId, NodeId, NodeId) {
    let (mut c, qualifier, corrector) = clip();
    let blur = effects(&picture(&c))[0];
    let ops = masking(&c, qualifier);
    c.apply_picture_ops(&ops).unwrap_or_else(|e| panic!("the masking batch: {e}"));
    (c, qualifier, corrector, blur)
}

fn node_of(p: &ClipPicture, id: NodeId) -> &klipp_graph_format::Node {
    p.graph.nodes.iter().find(|n| n.id == id).unwrap_or_else(|| panic!("{id} isn't in the picture"))
}

/// What only the picture holds is all there: the blur's mix, its mask
/// from the grade's published port, and the port, bound to the qualifier.
fn holds_the_mask(p: &ClipPicture, blur: NodeId, qualifier: NodeId) {
    assert!(p.is_truth(), "marked picture-truth");
    assert_eq!(node_of(p, blur).mix, Some(0.5), "the blur's mix");
    let g = grade_group(p);
    assert!(p.graph.edges.iter().any(|e| e.from == (g, "matte_selection".into()) && e.to == (blur, "mask".into())), "the blur's mask, from the grade's port");
    let interface = &node_of(p, g).group.as_ref().unwrap().interface;
    assert_eq!(interface.len(), 1);
    assert_eq!(interface[0].binding.node, qualifier, "the port publishes the qualifier");
}

#[test]
fn a_mask_and_a_mix_make_the_clip_picture_truth_and_its_fields_a_view() {
    let (c, _, _) = clip();
    assert!(!c.is_picture_truth());
    let before = saved(&c);
    let (m, qualifier, _, blur) = masked();
    holds_the_mask(&picture(&m), blur, qualifier);
    assert!(m.is_picture_truth());
    // The fields are the picture as far as they hold it: the same list and
    // grade, with no mix or mask in them.
    let view = saved(&m);
    assert_eq!(view["builtin_effects"], before["builtin_effects"]);
    assert!(m.grade().nodes.iter().any(|n| n.kind == NodeKind::Qualifier), "the grade keeps the qualifier");
}

#[test]
fn ops_on_a_picture_truth_clip_keep_its_mask_and_its_ids() {
    let (mut c, qualifier, _, blur) = masked();
    let p = picture(&c);
    let vignette = effects(&p)[1];
    // A parameter, a move and a removal, each through the ops.
    c.apply_picture_ops(&[GraphOp::SetParam { path: at(&c, [blur]), key: "radius".into(), value: ParamValue::Float(5.0) }]).unwrap();
    c.apply_picture_ops(&[GraphOp::MoveAfter { path: at(&c, [blur]), after: vignette }]).unwrap();
    assert!(matches!(c.builtin_effects()[1], E::GaussianBlur { radius } if radius == 5.0), "{:?}", c.builtin_effects());
    holds_the_mask(&picture(&c), blur, qualifier);
    c.apply_picture_ops(&[GraphOp::RemoveNode { path: at(&c, [vignette]) }]).unwrap();
    assert_eq!(c.builtin_effects().len(), 1);
    holds_the_mask(&picture(&c), blur, qualifier);
    // The mix changed and taken off again: still picture-truth.
    c.apply_picture_ops(&[GraphOp::SetMix { path: at(&c, [blur]), mix: Some(0.25) }]).unwrap();
    assert_eq!(node_of(&picture(&c), blur).mix, Some(0.25));
}

/// The four write paths that aren't ops yet (picture_write_paths), and a
/// keyframe edit, on a picture-truth clip: mapped onto the picture where
/// what only it holds survives the write, refused otherwise.
#[test]
fn field_writes_on_a_picture_truth_clip_map_onto_its_picture() {
    let (mut c, qualifier, corrector, blur) = masked();
    // A grade closure (the Colour page): an exposure on the corrector.
    assert!(c.set_grade_param(corrector, "exposure", 0.7));
    assert!(klipp_timeline::take_picture_refusal().is_none());
    holds_the_mask(&picture(&c), blur, qualifier);
    assert_eq!(c.grade().nodes.iter().find(|n| n.id == corrector).unwrap().params.exposure, Some(0.7));
    // A switch, a value, a key: the same nodes, the mask kept.
    let vignette = c.effect_id_at(1).unwrap();
    assert!(c.set_effect_enabled(vignette, false));
    assert!(c.replace_effect(vignette, E::Vignette { amount: 0.9, size: 0.5, softness: 0.5, roundness: 0.5 }));
    c.edit_opacity_keyframes(|k| k.push(klipp_core::Keyframe { time_secs: 1.0, value: 0.5, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }));
    assert!(klipp_timeline::take_picture_refusal().is_none());
    let p = picture(&c);
    holds_the_mask(&p, blur, qualifier);
    assert!(!node_of(&p, effects(&p)[1]).enabled, "the vignette switched off in the picture");
    assert!(c.opacity_keyframes().len() == 1);
    // A whole list that keeps the blur where it is: mapped.
    let list = c.builtin_effects().to_vec();
    c.set_effect_list(list, vec![true, true]);
    assert!(klipp_timeline::take_picture_refusal().is_none());
    holds_the_mask(&picture(&c), blur, qualifier);
}

#[test]
fn a_field_write_that_takes_away_what_the_picture_holds_is_refused() {
    let (c, _, corrector, _) = masked();
    let refusals: [(&str, Box<dyn Fn(&mut Clip)>); 3] = [
        ("effect list", Box::new(|c: &mut Clip| c.set_effect_list(vec![E::Vignette { amount: 0.5, size: 0.5, softness: 0.5, roundness: 0.5 }], vec![true]))),
        ("matte_selection", Box::new(|c: &mut Clip| {
            let q = c.grade().nodes.iter().find(|n| n.kind == NodeKind::Qualifier).unwrap().id;
            c.remove_grade_node(q);
        })),
        ("effect list", Box::new(|c: &mut Clip| {
            let blur = c.effect_id_at(0).unwrap();
            c.remove_effect(blur);
        })),
    ];
    for (names, write) in refusals {
        let mut tried = c.clone();
        let _ = corrector;
        write(&mut tried);
        let why = klipp_timeline::take_picture_refusal().unwrap_or_else(|| panic!("{names}: went through"));
        assert!(why.contains(&c.id.0.to_string()) && why.contains(names), "{why}");
        assert_eq!(saved(&tried), saved(&c), "{names}: the fields are as they were");
        assert_eq!(picture(&tried), picture(&c), "{names}: the picture is as it was");
    }
}

#[test]
fn undo_across_becoming_picture_truth_puts_the_clip_back_whole() {
    let (mut c, qualifier, _) = clip();
    let before = c.clone();
    let lifted = picture(&before);
    let edit = c.apply_picture_ops(&masking(&c, qualifier)).unwrap();
    let mut undone = c.clone();
    undone.restore_picture(&edit.before);
    assert!(!undone.is_picture_truth());
    assert_eq!(saved(&undone), saved(&before));
    assert_eq!(picture(&undone), lifted);
    assert_eq!(undone.has_lifted_picture(), before.has_lifted_picture());
    undone.restore_picture(&edit.after);
    assert!(undone.is_picture_truth());
    assert_eq!(picture(&undone), picture(&c));
}

#[test]
fn a_copy_of_a_picture_truth_clip_keeps_its_own_picture() {
    let (c, qualifier, _, blur) = masked();
    // A duplicate, edited: the original is untouched, and the other way.
    let mut copy = c.clone();
    copy.apply_picture_ops(&[GraphOp::SetMix { path: at(&copy, [blur]), mix: Some(0.9) }]).unwrap();
    assert_eq!(node_of(&picture(&c), blur).mix, Some(0.5));
    assert_eq!(node_of(&picture(&copy), blur).mix, Some(0.9));
    // Split, as a cut does: each half its own id, its own picture, the
    // keys cut between them, the mask on both.
    let mut track = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
    track.clips.push(c.clone());
    let (left, right) = track.split_at(c.id, klipp_core::Timecode::from_secs_f64(2.0, 24)).expect("a split");
    assert!(klipp_timeline::take_picture_refusal().is_none());
    for half in [&left, &right] {
        assert!(half.is_picture_truth(), "a half keeps the picture");
        holds_the_mask(&picture(half), blur, qualifier);
    }
    let mut left = left;
    left.apply_picture_ops(&[GraphOp::SetMix { path: at(&left, [blur]), mix: Some(0.1) }]).unwrap();
    assert_eq!(node_of(&picture(&right), blur).mix, Some(0.5), "editing one half leaves the other");
    assert_eq!(node_of(&picture(&c), blur).mix, Some(0.5));
}

/// T1 (decision §10): a picture-truth clip keeps its node ids through
/// structural ops and through a save and reload: its picture as the file
/// stores it, and its fields as the file's clip record, read back.
#[test]
fn a_picture_truth_clip_keeps_node_ids_through_structural_ops_and_save() {
    let (mut c, qualifier, corrector, blur) = masked();
    let p = picture(&c);
    let (grade, vignette) = (grade_group(&p), effects(&p)[1]);
    // Add a sharpen after the vignette, move the blur after it, take the
    // vignette away.
    let sharpen_id = uuid::Uuid::from_u128(0x5a);
    let mut sharpen = node_of(&p, vignette).clone();
    sharpen.id = sharpen_id;
    if let Some(l) = sharpen.legacy.as_mut() {
        l.slot = None;
        l.payload = serde_json::to_value(E::Sharpen { amount: 0.5 }).unwrap();
        l.keyframes = None;
        l.modifiers = Default::default();
    }
    c.apply_picture_ops(&[GraphOp::AddNode { after: at(&c, [vignette]), node: Box::new(sharpen) }]).unwrap();
    c.apply_picture_ops(&[GraphOp::MoveAfter { path: at(&c, [blur]), after: sharpen_id }]).unwrap();
    c.apply_picture_ops(&[GraphOp::RemoveNode { path: at(&c, [vignette]) }]).unwrap();
    let after = picture(&c);
    assert_eq!(effects(&after), vec![sharpen_id, blur], "the sharpen keeps the id it was added with, the blur its own");
    assert_eq!(grade_group(&after), grade);
    assert!(c.grade().nodes.iter().any(|n| n.id == corrector), "the grade's nodes keep theirs");
    holds_the_mask(&after, blur, qualifier);

    // Saved and read back: the clip's record (its fields), and its picture
    // as the file stores it.
    let record = serde_json::to_string(&c).unwrap();
    let stored = serde_json::to_string(&after).unwrap();
    let versions = c.picture_v2().unwrap().versions.clone();
    let mut reloaded: Clip = serde_json::from_str(&record).unwrap();
    assert!(!reloaded.is_picture_truth(), "the record alone is the fields");
    reloaded.adopt_stored_picture(klipp_timeline::LiftedPicture { picture: serde_json::from_str(&stored).unwrap(), versions }).unwrap();
    assert!(reloaded.is_picture_truth());
    assert_eq!(picture(&reloaded), after, "every node id, the mask and the mix, as saved");
    // And ops on the reloaded clip find the same nodes.
    reloaded.apply_picture_ops(&[GraphOp::SetMix { path: at(&reloaded, [blur]), mix: Some(0.75) }]).unwrap();
    assert_eq!(node_of(&picture(&reloaded), blur).mix, Some(0.75));
}

/// Amendment 12: a write through the fields on a picture-truth clip (a
/// grade closure, `set_grade`, the effect mutators, a whole list) maps onto
/// its picture where what only the picture holds survives it, and is
/// otherwise refused, naming the clip and the edit; it never overwrites the
/// picture through the fields.
#[test]
fn a_field_write_on_a_picture_truth_clip_maps_to_ops_or_is_refused() {
    let (mut c, qualifier, corrector, blur) = masked();
    // Mapped: a grade exposure (the Colour page's closure, and set_grade).
    let mut graded = c.grade().clone();
    graded.nodes.iter_mut().find(|n| n.id == corrector).unwrap().params.exposure = Some(0.4);
    c.set_grade(graded);
    assert!(klipp_timeline::take_picture_refusal().is_none());
    holds_the_mask(&picture(&c), blur, qualifier);
    let p = picture(&c);
    let body = &node_of(&p, grade_group(&p)).group.as_ref().unwrap().graph;
    assert!(serde_json::to_string(body).unwrap().contains("0.4"), "the exposure is in the picture's grade");
    // Mapped: an effect's value and switch.
    let vignette = c.effect_id_at(1).unwrap();
    assert!(c.set_effect_enabled(vignette, false));
    assert!(klipp_timeline::take_picture_refusal().is_none());
    holds_the_mask(&picture(&c), blur, qualifier);
    // Refused: a grade without the published node, and a list without the
    // masked effect; the clip as it was, the reason naming clip and edit.
    for (what, write) in [
        ("a grade edit", Box::new(|c: &mut Clip| c.remove_grade_node(qualifier)) as Box<dyn Fn(&mut Clip)>),
        ("the effect list", Box::new(|c: &mut Clip| c.set_effect_list(vec![E::Sharpen { amount: 0.5 }], vec![true]))),
    ] {
        let mut tried = c.clone();
        write(&mut tried);
        let why = klipp_timeline::take_picture_refusal().unwrap_or_else(|| panic!("{what} went through"));
        assert!(why.contains(&c.id.0.to_string()) && why.contains(what), "{why}");
        assert_eq!(saved(&tried), saved(&c), "{what}: the fields as they were");
        assert_eq!(picture(&tried), picture(&c), "{what}: the picture as it was");
    }
}

/// A whole-list write (`set_effect_list`: undo of a list edit, a render
/// copy given its effects back, a placed kind's list write) gives every
/// entry a fresh id, so the list carries no effect's identity: an entry
/// keeps the node at its place when it is of the same type. Two blurs
/// swapped by such a write leave the mix at the place, on the blur now
/// there; a type the place didn't hold takes no node, and a write that
/// leaves the masked node nowhere is refused.
#[test]
fn a_whole_list_write_keeps_nodes_by_place_and_type() {
    let (mut c, qualifier, _, blur) = masked();
    // The blur first, mixed and masked; a second, plain blur after it.
    let list = vec![E::GaussianBlur { radius: 3.0 }, E::GaussianBlur { radius: 8.0 }];
    c.set_effect_list(list, vec![true, true]);
    assert!(klipp_timeline::take_picture_refusal().is_none());
    holds_the_mask(&picture(&c), blur, qualifier);
    // Swapped: the mixed node keeps its place, so it carries the other blur.
    c.set_effect_list(vec![E::GaussianBlur { radius: 8.0 }, E::GaussianBlur { radius: 3.0 }], vec![true, true]);
    assert!(klipp_timeline::take_picture_refusal().is_none());
    let p = picture(&c);
    holds_the_mask(&p, blur, qualifier);
    assert_eq!(effects(&p)[0], blur, "the masked node is the first entry still");
    assert!(matches!(c.builtin_effects()[0], E::GaussianBlur { radius } if radius == 8.0));
    // Another type at the masked place: nothing keeps the node, refused.
    let before = picture(&c);
    c.set_effect_list(vec![E::Sharpen { amount: 0.5 }, E::GaussianBlur { radius: 3.0 }], vec![true, true]);
    assert!(klipp_timeline::take_picture_refusal().is_some(), "a sharpen can't take the blur's mask");
    assert_eq!(picture(&c), before);
}

/// A node beside the clip's chain reads the clip's source or the grade's
/// picture, which the renderer gives it; wired from anything else (here,
/// an effect's output) it is refused at the op, naming the node and the
/// port, and the clip is as it was.
#[test]
fn a_side_node_wired_from_what_the_renderer_cannot_give_is_refused() {
    use klipp_graph_format::ports::{input_key, output_key};
    let (c, ..) = clip();
    let p = picture(&c);
    let fx = effects(&p);
    let mut window = node_of(&p, fx[0]).clone();
    window.id = uuid::Uuid::from_u128(0x5e1de);
    window.type_id = klipp_graph_format::types::LEGACY_NODE.into();
    window.legacy = Some(klipp_graph_format::Legacy { slot: None, payload: serde_json::json!({"kind": "Mask", "label": "Window", "params": {}}), keyframes: Some(serde_json::json!({"tracks": []})), modifiers: Default::default() });
    let beside = GraphOp::AddNodeBeside { at: at(&c, [fx[0]]), node: Box::new(window.clone()) };
    let from_effect = GraphOp::Connect { from: at(&c, [fx[1]]), from_port: "out".into(), to: at(&c, [window.id]), to_port: input_key("Mask", 0) };
    let into_mask = GraphOp::Connect { from: at(&c, [window.id]), from_port: output_key("Mask", 0), to: at(&c, [fx[0]]), to_port: "mask".into() };
    let mut tried = c.clone();
    let (index, why) = tried.apply_picture_ops_indexed(&[beside.clone(), from_effect, into_mask.clone()]).unwrap_err();
    assert_eq!(index, Some(1), "{why}");
    assert!(why.contains(&window.id.to_string()) && why.contains(input_key("Mask", 0).as_str()) && why.contains(&fx[1].to_string()), "{why}");
    assert_eq!(saved(&tried), saved(&c));
    assert!(!tried.is_picture_truth());
    // From the source, it goes through.
    let source = p.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SOURCE).unwrap().id;
    let from_source = GraphOp::Connect { from: at(&c, [source]), from_port: "out".into(), to: at(&c, [window.id]), to_port: input_key("Mask", 0) };
    tried.apply_picture_ops(&[beside, from_source, into_mask]).unwrap_or_else(|e| panic!("from the source: {e}"));
    assert!(tried.is_picture_truth());
}
