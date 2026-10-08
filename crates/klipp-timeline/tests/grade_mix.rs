//! A grade correction's mix and mask channel (M3 step 17a): only the
//! clip's picture keeps them, so setting either makes the clip
//! picture-truth, its picture requiring `grade-mix-1`, which a reader that
//! doesn't know it refuses. A new gate leaves the clip as it was.

use klipp_core::{MediaId, RationalTime};
use klipp_graph_format::lifted::{types as lt, unknown_capability, ClipPicture, GRADE_MIX_CAPABILITY, PICTURE_TRUTH_CAPABILITY};
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_format::{NodeId, ParamValue};
use klipp_graph_lift::ops::GraphOp;
use klipp_nodes::{Edge, Node, NodeGraph, NodeKind, NodeParams};
use klipp_timeline::{Clip, GradeAccess};

fn at(c: &Clip, ids: impl IntoIterator<Item = NodeId>) -> NodePath {
    NodePath::new(Root::Clip { clip: c.id.0 }, ids)
}

/// A media clip graded by a corrector, with a qualifier on the source
/// beside it, not yet gating it. Gives the clip, the qualifier and the
/// corrector.
fn clip() -> (Clip, NodeId, NodeId) {
    let t = |s: f64| RationalTime::from_secs_f64(s, 24);
    let mut c = Clip::media(MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
    let mut g = NodeGraph::default();
    let corrector = g.ensure_corrector();
    let source = g.nodes.iter().find(|n| n.kind == NodeKind::Source).expect("a source").id;
    let qualifier = Node { id: NodeId::new_v4(), kind: NodeKind::Qualifier, label: String::new(), enabled: true, params: NodeParams::default(), keyframes: Default::default(), output_link: None, input_links: Vec::new() };
    g.edges.push(Edge { from_node: source, from_port: 0, to_node: qualifier.id, to_port: 0 });
    let q = qualifier.id;
    g.nodes.push(qualifier);
    c.set_grade(g);
    (c, q, corrector)
}

fn picture(c: &Clip) -> ClipPicture {
    c.picture_v2().unwrap().picture.clone()
}

fn grade_group(c: &Clip) -> NodeId {
    picture(c).graph.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP).expect("a grade group").id
}

fn grade_node(p: &ClipPicture, id: NodeId) -> klipp_graph_format::Node {
    p.graph.nodes.iter().filter_map(|n| n.group.as_ref()).flat_map(|g| g.graph.nodes.iter()).find(|n| n.id == id).expect("in the grade").clone()
}

/// The clip, its corrector mixed at a quarter and reading its matte's
/// alpha, and the corrector.
fn mixed() -> (Clip, NodeId, NodeId) {
    let (mut c, qualifier, corrector) = clip();
    let path = at(&c, [grade_group(&c), corrector]);
    c.apply_picture_ops(&[GraphOp::SetMix { path: path.clone(), mix: Some(0.25) }, GraphOp::SetMaskChannel { path, channel: Some("alpha".into()) }]).unwrap();
    (c, qualifier, corrector)
}

#[test]
fn a_corrections_mix_makes_the_clip_picture_truth_and_requires_grade_mix() {
    let (c, _, corrector) = mixed();
    assert!(c.is_picture_truth());
    let p = picture(&c);
    assert_eq!(p.requires, [PICTURE_TRUTH_CAPABILITY, GRADE_MIX_CAPABILITY]);
    let n = grade_node(&p, corrector);
    assert_eq!(n.mix, Some(0.25));
    assert_eq!(n.params.get(&"mask.channel".into()), Some(&ParamValue::Text("alpha".into())));
    // The renderer's grade carries them as the corrector's run setting;
    // the stored grade, which a copy of it would take, never does.
    let run = c.grade_to_run();
    let blend = run.nodes.iter().find(|n| n.id == corrector).unwrap().params.correction_blend.expect("the corrector's blend");
    assert_eq!((blend.mix, blend.channel), (0.25, klipp_effects::mask_mix::MaskChannel::Alpha));
    assert!(c.grade().nodes.iter().all(|n| n.params.correction_blend.is_none()));
}

#[test]
fn a_reader_that_does_not_know_grade_mix_refuses_the_picture() {
    let (c, _, _) = mixed();
    let p = picture(&c);
    // Today's reader knows it; one from before it doesn't.
    assert_eq!(unknown_capability(&p.requires, klipp_graph_format::lifted::KNOWN_CAPABILITIES), None);
    assert_eq!(unknown_capability(&p.requires, &[PICTURE_TRUTH_CAPABILITY]), Some(GRADE_MIX_CAPABILITY));
    let mut v = serde_json::to_value(&p).unwrap();
    assert_eq!(serde_json::from_value::<ClipPicture>(v.clone()).unwrap(), p, "read whole");
    v["requires"] = serde_json::json!([PICTURE_TRUTH_CAPABILITY, "grade-mix-2"]);
    let err = serde_json::from_value::<ClipPicture>(v).unwrap_err().to_string();
    assert!(err.contains("unsupported picture capability: grade-mix-2"), "{err}");
}

#[test]
fn a_field_write_keeps_a_corrections_mix() {
    let (mut c, _, corrector) = mixed();
    assert!(c.set_grade_param(corrector, "exposure", 0.4), "the corrector takes exposure");
    let p = picture(&c);
    let n = grade_node(&p, corrector);
    assert_eq!(n.mix, Some(0.25), "the mix outlives a write through the fields");
    assert_eq!(n.params.get(&"mask.channel".into()), Some(&ParamValue::Text("alpha".into())));
    assert!(p.requires.iter().any(|r| r == GRADE_MIX_CAPABILITY));
}

#[test]
fn taking_the_settings_off_drops_the_capability_and_undo_puts_them_back() {
    let (mut c, _, corrector) = mixed();
    let path = at(&c, [grade_group(&c), corrector]);
    let edit = c.apply_picture_ops(&[GraphOp::SetMix { path: path.clone(), mix: None }, GraphOp::SetMaskChannel { path, channel: None }]).unwrap();
    let p = picture(&c);
    assert_eq!(p.requires, [PICTURE_TRUTH_CAPABILITY], "no grade-mix left to require");
    assert!(matches!(c.grade_to_run(), std::borrow::Cow::Borrowed(_)), "nothing to add to the run");
    c.restore_picture(&edit.before);
    assert_eq!(grade_node(&picture(&c), corrector).mix, Some(0.25));
}

/// A new gate (a qualifier wired to the corrector's mask) writes no
/// channel: it reads red, as every gate does, and the clip stays a clip
/// whose fields are its truth.
#[test]
fn a_new_qualifier_gate_leaves_the_clip_non_truth() {
    let (mut c, qualifier, corrector) = clip();
    let g = grade_group(&c);
    c.apply_picture_ops(&[GraphOp::Connect { from: at(&c, [g, qualifier]), from_port: "out".into(), to: at(&c, [g, corrector]), to_port: "mask".into() }]).unwrap();
    assert!(!c.is_picture_truth());
    assert_eq!(c.grade().input_of(corrector, 1), Some(qualifier), "the qualifier gates the corrector");
    let n = grade_node(&picture(&c), corrector);
    assert!(n.mix.is_none() && !n.params.contains_key(&"mask.channel".into()));
}

/// A correction inside a compound has no place for a mix or channel yet:
/// setting one is refused, rather than written to fields that drop it.
#[test]
fn a_mix_inside_a_compound_is_refused() {
    let (mut c, _, corrector) = clip();
    let g = grade_group(&c);
    let why = c.apply_picture_ops(&[GraphOp::SetMix { path: at(&c, [g, corrector, corrector]), mix: Some(0.5) }]).unwrap_err();
    assert!(why.contains("inside a compound"), "{why}");
    assert!(!c.is_picture_truth());
}

/// Moving a mixed correction into a compound, through the grade's fields
/// (the node editor's grouping), is refused: the compound would hold it
/// without its mix. The clip stays as it was, and says why.
#[test]
fn grouping_a_mixed_correction_into_a_compound_is_refused() {
    let (mut c, _, corrector) = mixed();
    let before = picture(&c);
    let mut grade = c.grade().clone();
    let at_ = grade.nodes.iter().position(|n| n.id == corrector).unwrap();
    let inner_node = grade.nodes.remove(at_);
    let mut inner = NodeGraph::default();
    inner.nodes.push(inner_node);
    let mut compound = Node { id: NodeId::new_v4(), kind: NodeKind::Compound, label: "Compound".into(), enabled: true, params: NodeParams::default(), keyframes: Default::default(), output_link: None, input_links: Vec::new() };
    compound.params.compound_subgraph = Some(inner);
    for e in &mut grade.edges {
        if e.from_node == corrector { e.from_node = compound.id; }
        if e.to_node == corrector { e.to_node = compound.id; }
    }
    grade.nodes.push(compound);
    c.set_grade(grade);
    assert_eq!(picture(&c), before, "the clip as it was");
    let why = klipp_timeline::take_picture_refusal().expect("the refusal is kept");
    assert!(why.contains("inside a compound"), "{why}");
}

/// A grade version would come back without a correction's mix: the clip
/// says it holds one, which the app refuses to save or switch versions on.
#[test]
fn a_clip_holding_a_grade_mix_says_so() {
    let (c, ..) = clip();
    assert!(!c.holds_grade_mix());
    let (mixed, ..) = mixed();
    assert!(mixed.holds_grade_mix());
}
