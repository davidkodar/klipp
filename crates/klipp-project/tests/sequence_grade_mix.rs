//! A sequence grade's correction mix and mask channel (#1321) through a
//! real save and a strict reload: set by path on the timeline grade and on
//! a grade group's post-clip stage, saved by `save_project`, held to what
//! the stored body says, read back by `load_project_strict`, run by the
//! renderer's grade, and saved again unchanged. A grade without one saves
//! as it did before #1321.

use klipp_effects::mask_mix::MaskChannel;
use klipp_graph_format::lifted::{Stage, GRADE_MIX_CAPABILITY};
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_lift::ops::GraphOp;
use klipp_graph_lift::sequence_grade_mix;
use klipp_media::MediaPool;
use klipp_nodes::params::EffectBlend;
use klipp_nodes::{NodeGraph, NodeId, NodeKind};
use klipp_project::{load_project_strict, save_project};
use klipp_timeline::{Clip, GradeAccess, GradeGroup, GradeStage, Sequence};
use serde_json::Value;

fn tc(s: f64) -> klipp_core::RationalTime {
    klipp_core::RationalTime::from_secs_f64(s, 24)
}

fn corrector(g: &NodeGraph) -> NodeId {
    g.nodes.iter().find(|n| n.kind == NodeKind::Corrector).unwrap().id
}

/// A sequence with a timeline grade and a grade group whose post stage
/// grades its one member clip, and the two roots.
fn sequence() -> (Sequence, [Root; 2]) {
    let mut seq = Sequence::new_default();
    seq.timeline_grade_slot().set(Some(NodeGraph::default()));
    let mut group = GradeGroup::new("Night");
    group.stage_slot(GradeStage::Post).set(Some(NodeGraph::default()));
    let gid = group.id;
    seq.grade_groups.push(group);
    let mut c = Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    c.grade_group = Some(gid);
    seq.tracks[0].clips.push(c);
    let roots = [Root::TimelineGrade { sequence: seq.id.0 }, Root::GroupStage { group: gid.0, stage: Stage::Post }];
    (seq, roots)
}

/// Each root's Corrector given exposure 0.5 (so the grade changes the
/// picture and runs), mix 0.4 and the alpha channel, by path.
fn graded() -> (Sequence, [Root; 2]) {
    let (mut seq, roots) = sequence();
    for root in roots {
        let path = NodePath::new(root, [corrector(seq.root_grade(root).unwrap().unwrap())]);
        let ops = [
            GraphOp::SetParam { path: path.clone(), key: "exposure".into(), value: klipp_graph_format::ParamValue::Float(0.5) },
            GraphOp::SetMix { path: path.clone(), mix: Some(0.4) },
            GraphOp::SetMaskChannel { path, channel: Some("alpha".into()) },
        ];
        seq.apply_root_grade_ops(&ops, None).unwrap_or_else(|e| panic!("{root:?}: {e}"));
    }
    (seq, roots)
}

/// `seq` saved and read back strictly: the sequence loaded and the file.
fn saved(name: &str, seq: Sequence) -> (Sequence, Value) {
    let dir = std::env::temp_dir().join(format!("klipp_seq_grade_mix_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    save_project(&dir, &[seq], &MediaPool::new()).unwrap_or_else(|e| panic!("{name}: saving: {e}"));
    let file: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("project.json")).unwrap()).unwrap();
    let (sequences, ..) = load_project_strict(&dir).unwrap_or_else(|e| panic!("{name}: a strict reload: {e}"));
    let _ = std::fs::remove_dir_all(&dir);
    (sequences.into_iter().next().expect("the sequence"), file)
}

/// The stored body of `root` in a saved file.
fn body(file: &Value, root: Root) -> Value {
    let seq = &file["sequences"][0];
    match root {
        Root::TimelineGrade { .. } => seq["graph_v2"].clone(),
        Root::GroupStage { .. } => seq["grade_groups"][0]["graph_v2"]["post"].clone(),
        _ => unreachable!(),
    }
}

/// The run setting of `root`'s Corrector.
fn blend(seq: &Sequence, root: Root) -> Option<EffectBlend> {
    let g = seq.root_grade(root).unwrap().unwrap();
    let id = corrector(g);
    g.nodes.iter().find(|n| n.id == id).unwrap().params.correction_blend
}

const SET: EffectBlend = EffectBlend { mix: 0.4, channel: MaskChannel::Alpha };

/// Both roots keep the mix and the channel: in the stored body, on the
/// Corrector, with `requires` naming the capability; read back on the
/// node; and saved again into the same sequences.
#[test]
fn a_sequence_grade_keeps_its_mix_and_channel_through_a_save() {
    let (seq, roots) = graded();
    for root in roots {
        assert_eq!(blend(&seq, root), Some(SET), "{root:?}: set by path");
    }
    let (back, file) = saved("kept", seq);
    for root in roots {
        let mut b = body(&file, root);
        assert_eq!(b["requires"], serde_json::json!([GRADE_MIX_CAPABILITY]), "{root:?}: the body requires it");
        let id = corrector(back.root_grade(root).unwrap().unwrap());
        let read = sequence_grade_mix::read(&mut b).unwrap();
        assert_eq!(read, vec![sequence_grade_mix::Blend { node: id, mix: Some(0.4), channel: Some("alpha".into()) }], "{root:?}: on the Corrector");
        assert!(serde_json::from_value::<klipp_graph_format::Graph>(body(&file, root)).is_err(), "{root:?}: a reader from before refuses it");
        assert_eq!(blend(&back, root), Some(SET), "{root:?}: read back");
    }
    let (_, again) = saved("kept_again", back);
    assert_eq!(again["sequences"], file["sequences"], "saved again unchanged");
}

/// The renderer's grade runs the mix and the channel: the timeline grade's
/// proxy clip lends them, and a member clip's baked grade carries its
/// group's post stage with them.
#[test]
fn the_renderer_runs_a_loaded_sequence_grade_s_mix() {
    let (seq, _) = graded();
    let (mut back, _) = saved("render", seq);
    let proxy = Clip::timeline_grade_of(&back).expect("the timeline grade runs");
    assert!(proxy.grade_to_run().nodes.iter().any(|n| n.params.correction_blend == Some(SET)), "the timeline grade's mix runs");
    back.bake_grade_groups();
    let member = back.tracks[0].clips[0].grade();
    assert!(member.nodes.iter().any(|n| n.params.correction_blend == Some(SET)), "the stage's mix runs in the member's grade");
}

/// A grade without a mix saves as before #1321: no `requires`, no node
/// mix or channel. One set and then put back to whole and red is the same.
#[test]
fn a_sequence_grade_without_a_mix_saves_as_before() {
    let (seq, plain_roots) = sequence();
    let (_, plain) = saved("plain", seq);
    let (mut seq, roots) = graded();
    for root in roots {
        let path = NodePath::new(root, [corrector(seq.root_grade(root).unwrap().unwrap())]);
        let ops = [
            GraphOp::SetParam { path: path.clone(), key: "exposure".into(), value: klipp_graph_format::ParamValue::Float(0.0) },
            GraphOp::SetMix { path: path.clone(), mix: None },
            GraphOp::SetMaskChannel { path, channel: None },
        ];
        seq.apply_root_grade_ops(&ops, None).unwrap();
        assert_eq!(blend(&seq, root), None, "{root:?}: back to whole and red");
    }
    let (_, cleared) = saved("cleared", seq);
    for (file, roots, what) in [(&plain, plain_roots, "never set"), (&cleared, roots, "set and cleared")] {
        for root in roots {
            let mut b = body(file, root);
            assert!(b.get("requires").is_none(), "{what} {root:?}: no requires");
            assert_eq!(sequence_grade_mix::read(&mut b).unwrap(), vec![], "{what} {root:?}: no node holds a mix or channel");
        }
    }
}

/// A correction inside a compound of the timeline grade keeps its mix and
/// channel too: set by path through the compound, in the stored body's
/// group, read back on the subgraph's node, run by the timeline grade,
/// and saved again unchanged.
#[test]
fn a_correction_inside_a_compound_keeps_its_mix() {
    let mut seq = Sequence::new_default();
    let mut grade = NodeGraph::default();
    let inner = NodeGraph::default();
    let inner_corrector = corrector(&inner);
    let mut compound = klipp_nodes::Node { id: NodeId::new_v4(), kind: NodeKind::Compound, label: String::new(), enabled: true, params: Default::default(), keyframes: Default::default(), output_link: None, input_links: vec![] };
    compound.params.compound_subgraph = Some(inner);
    let (at, output) = (compound.id, grade.output_node_id);
    let place = grade.nodes.iter().position(|n| n.id == output).unwrap();
    grade.nodes.insert(place, compound);
    for e in grade.edges.iter_mut().filter(|e| e.to_node == output && e.to_port == 0) {
        e.to_node = at;
    }
    grade.edges.push(klipp_nodes::Edge { from_node: at, from_port: 0, to_node: output, to_port: 0 });
    seq.timeline_grade_slot().set(Some(grade));
    let root = Root::TimelineGrade { sequence: seq.id.0 };
    let path = NodePath::new(root, [at, inner_corrector]);
    let ops = [
        GraphOp::SetParam { path: path.clone(), key: "exposure".into(), value: klipp_graph_format::ParamValue::Float(0.5) },
        GraphOp::SetMix { path: path.clone(), mix: Some(0.4) },
        GraphOp::SetMaskChannel { path, channel: Some("alpha".into()) },
    ];
    seq.apply_root_grade_ops(&ops, None).unwrap();
    let inside = |seq: &Sequence| {
        let g = seq.root_grade(root).unwrap().unwrap();
        let sub = g.nodes.iter().find(|n| n.id == at).unwrap().params.compound_subgraph.as_ref().unwrap();
        sub.nodes.iter().find(|n| n.id == inner_corrector).unwrap().params.correction_blend
    };
    assert_eq!(inside(&seq), Some(SET), "set through the compound");
    let (back, file) = saved("compound", seq);
    let mut b = body(&file, root);
    assert_eq!(b["requires"], serde_json::json!([GRADE_MIX_CAPABILITY]));
    assert_eq!(sequence_grade_mix::read(&mut b).unwrap(), vec![sequence_grade_mix::Blend { node: inner_corrector, mix: Some(0.4), channel: Some("alpha".into()) }]);
    assert_eq!(inside(&back), Some(SET), "read back inside the compound");
    let proxy = Clip::timeline_grade_of(&back).expect("the timeline grade runs");
    let run = proxy.grade_to_run();
    let sub = run.nodes.iter().find(|n| n.id == at).unwrap().params.compound_subgraph.as_ref().unwrap();
    assert!(sub.nodes.iter().any(|n| n.params.correction_blend == Some(SET)), "the compound's correction runs with its mix");
    let (_, again) = saved("compound_again", back);
    assert_eq!(again["sequences"], file["sequences"], "saved again unchanged");
}
