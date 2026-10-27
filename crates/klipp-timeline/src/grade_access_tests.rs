use super::*;
use klipp_core::{MediaId, RationalTime};
use klipp_nodes::NodeParams;

fn t(secs: f64) -> RationalTime {
    RationalTime::from_secs_f64(secs, 24)
}

fn node(kind: NodeKind) -> Node {
    Node {
        id: NodeId::new_v4(),
        kind,
        label: String::new(),
        enabled: true,
        params: NodeParams::default(),
        keyframes: Default::default(),
        output_link: None,
        input_links: Vec::new(),
    }
}

/// A grade with a Corrector, and that Corrector's id.
fn graded() -> (NodeGraph, NodeId) {
    let mut g = NodeGraph::default();
    let id = g.ensure_corrector();
    (g, id)
}

fn exposure(g: &NodeGraph, id: NodeId) -> Option<f32> {
    g.nodes.iter().find(|n| n.id == id).and_then(|n| n.params.exposure)
}

#[test]
fn each_always_graded_owner_reads_and_writes_its_own_field() {
    let (g, id) = graded();
    let mut clip = Clip::media(MediaId::new(), t(0.0), t(1.0), t(0.0), t(1.0));
    let mut version = GradeVersion { name: "A".into(), node_graph: Default::default() };
    let mut look = Look { id: uuid::Uuid::new_v4(), name: "Warm".into(), node_graph: NodeGraph::default() };

    clip.set_grade(g.clone());
    version.set_grade(g.clone());
    look.set_grade(g.clone());
    assert_eq!(clip.picture.view().node_graph.0, g);
    assert_eq!(version.node_graph.0, g);
    assert_eq!(look.node_graph, g);

    assert!(clip.set_grade_param(id, "exposure", 0.5));
    assert!(version.set_grade_param(id, "exposure", 0.25));
    assert!(look.set_grade_param(id, "exposure", 0.125));
    assert_eq!(exposure(&clip.picture.view().node_graph.0, id), Some(0.5));
    assert_eq!(exposure(&version.node_graph.0, id), Some(0.25));
    assert_eq!(exposure(&look.node_graph, id), Some(0.125));
    assert_eq!((exposure(clip.grade(), id), exposure(version.grade(), id), exposure(look.grade(), id)), (Some(0.5), Some(0.25), Some(0.125)));
}

#[test]
fn each_slot_reads_and_writes_its_own_field() {
    let (g, id) = graded();
    let mut seq = Sequence::new_default();
    let mut group = GradeGroup::new("Night");
    let mut fx = TrackEffects::default();

    seq.timeline_grade_slot().set(Some(g.clone()));
    group.stage_slot(GradeStage::Pre).set(Some(g.clone()));
    fx.grade_slot().set(Some(g.clone()));
    assert_eq!(seq.timeline_graph.as_ref().map(|g| &g.0), Some(&g));
    assert_eq!((group.pre.as_ref().map(|g| &g.0), group.post.as_ref().map(|g| &g.0)), (Some(&g), None), "the pre stage, not the post");
    assert_eq!(fx.grade.as_ref().map(|g| &g.0), Some(&g));

    assert!(seq.timeline_grade_slot().set_param(id, "exposure", 0.5));
    assert!(group.stage_slot(GradeStage::Pre).set_param(id, "exposure", 0.25));
    assert!(fx.grade_slot().set_param(id, "exposure", 0.125));
    assert_eq!(seq.timeline_grade().and_then(|g| exposure(g, id)), Some(0.5));
    assert_eq!(group.stage(GradeStage::Pre).and_then(|g| exposure(g, id)), Some(0.25));
    assert_eq!(fx.track_grade().and_then(|g| exposure(g, id)), Some(0.125));

    group.stage_slot(GradeStage::Post).set(Some(g.clone()));
    group.stage_slot(GradeStage::Pre).set(None);
    assert_eq!((group.pre.as_ref().map(|g| &g.0), group.post.as_ref().map(|g| &g.0)), (None, Some(&g)));
}

#[test]
fn an_absent_grade_takes_no_edit_and_says_so() {
    let mut seq = Sequence::new_default();
    let mut slot = seq.timeline_grade_slot();
    assert_eq!(slot.edit(|_| 7), None);
    assert!(!slot.set_param(NodeId::new_v4(), "exposure", 1.0));
    assert!(!slot.insert_node(None, node(NodeKind::Exposure)));
    assert!(!slot.connect(NodeId::new_v4(), 0, NodeId::new_v4(), 0));
    slot.remove_node(NodeId::new_v4());
    slot.disconnect(NodeId::new_v4(), 0);
    assert!(seq.timeline_graph.is_none(), "nothing started a grade");
    // As the drain starts one on a first drag.
    assert_eq!(seq.timeline_grade_slot().edit_or_insert(|g| g.nodes.len()), NodeGraph::default().nodes.len());
    assert!(seq.timeline_graph.is_some());
}

#[test]
fn a_version_and_a_look_are_made_holding_their_grade() {
    let (g, _) = graded();
    let v = GradeVersion::new("A", g.clone());
    let id = uuid::Uuid::new_v4();
    let l = Look::new(id, "Warm", g.clone());
    assert_eq!((v.name.as_str(), v.grade()), ("A", &g));
    assert_eq!((l.id, l.name.as_str(), l.grade()), (id, "Warm", &g));
}

#[test]
fn edit_grade_gives_its_closures_result_back() {
    let mut clip = Clip::gap(t(0.0), t(1.0));
    let n = clip.edit_grade(|g| {
        g.nodes.push(node(NodeKind::Exposure));
        g.nodes.len()
    });
    assert_eq!(n, clip.picture.view().node_graph.0.nodes.len());
    let mut fx = TrackEffects { grade: Some(Default::default()), ..Default::default() };
    assert_eq!(fx.grade_slot().edit(|g| g.output_node_id), Some(fx.grade.as_ref().map(|g| &g.0).unwrap().output_node_id));
}

#[test]
fn a_param_is_found_inside_a_compound_as_the_drain_finds_it() {
    let (inner, id) = graded();
    let mut compound = node(NodeKind::Compound);
    compound.params.compound_subgraph = Some(inner);
    let mut clip = Clip::gap(t(0.0), t(1.0));
    clip.view_mut().node_graph.0.nodes.push(compound);
    assert!(clip.set_grade_param(id, "exposure", 0.5));
    let sub = clip.picture.view().node_graph.0.nodes.iter().find_map(|n| n.params.compound_subgraph.as_ref()).unwrap();
    assert_eq!(exposure(sub, id), Some(0.5));
    assert!(!clip.set_grade_param(NodeId::new_v4(), "exposure", 0.5), "no such node");
}

#[test]
fn a_node_inserted_after_another_takes_over_what_it_fed() {
    let mut clip = Clip::gap(t(0.0), t(1.0));
    let g = &clip.picture.view().node_graph.0;
    let (source, out) = (g.nodes[0].id, g.output_node_id);
    let feeding_out = g.input_of(out, 0).unwrap();
    let new = node(NodeKind::Saturation);
    let id = new.id;
    assert!(clip.insert_grade_node(Some(feeding_out), new));
    let g = clip.grade();
    assert_eq!(g.input_of(out, 0), Some(id), "it feeds the Output now");
    assert_eq!(g.input_of(id, 0), Some(feeding_out));
    assert_eq!(g.nodes.iter().position(|n| n.id == id), g.nodes.iter().position(|n| n.id == feeding_out).map(|i| i + 1));
    assert!(g.topo_sort().is_ok());
    // Unwired, as the Colour page adds one; and nothing for a missing node.
    let loose = node(NodeKind::Exposure);
    let loose_id = loose.id;
    assert!(clip.insert_grade_node(None, loose));
    assert!(clip.grade().edges.iter().all(|e| e.from_node != loose_id && e.to_node != loose_id));
    assert!(!clip.insert_grade_node(Some(NodeId::new_v4()), node(NodeKind::Exposure)));
    assert_ne!(source, id);
}

#[test]
fn a_removed_node_takes_its_wires_place_and_empty_frames() {
    let (mut g, id) = graded();
    g.node_positions.insert(id, klipp_nodes::NodePos { x: 1.0, y: 2.0 });
    g.frames.push(klipp_nodes::NodeFrame {
        id: klipp_nodes::NodeFrameId::new_v4(),
        label: "Primary".into(),
        description: String::new(),
        color_rgba: 0,
        contained_node_ids: [id].into_iter().collect(),
        recipe: None,
    });
    let mut look = Look { id: uuid::Uuid::new_v4(), name: "Warm".into(), node_graph: g };
    look.remove_grade_node(id);
    let g = look.grade();
    assert!(g.nodes.iter().all(|n| n.id != id));
    assert!(g.edges.iter().all(|e| e.from_node != id && e.to_node != id));
    assert!(!g.node_positions.contains_key(&id));
    assert!(g.frames.is_empty(), "the frame held only it");
}

#[test]
fn a_wire_replaces_the_ports_old_one_and_a_loop_is_refused() {
    let (g, id) = graded();
    let mut version = GradeVersion { name: "A".into(), node_graph: g.into() };
    let source = version.grade().nodes[0].id;
    let out = version.grade().output_node_id;
    let sat = node(NodeKind::Saturation);
    let sat_id = sat.id;
    assert!(version.insert_grade_node(Some(id), sat));
    // Corrector's input now from the Saturation after it: a loop.
    let before = version.grade().clone();
    assert!(!version.connect_grade(sat_id, 0, id, 0));
    assert_eq!(*version.grade(), before, "a refused wire changes nothing");
    // The Output straight from the source: its old wire goes.
    assert!(version.connect_grade(source, 0, out, 0));
    assert_eq!(version.grade().input_of(out, 0), Some(source));
    assert_eq!(version.grade().edges.iter().filter(|e| e.to_node == out && e.to_port == 0).count(), 1);
    version.disconnect_grade(out, 0);
    assert_eq!(version.grade().input_of(out, 0), None);
}

#[test]
fn a_layer_mixer_grows_to_the_port_it_is_wired_on() {
    let mut clip = Clip::gap(t(0.0), t(1.0));
    let source = clip.grade().nodes[0].id;
    let mixer = node(NodeKind::LayerMixer);
    let mid = mixer.id;
    assert!(clip.insert_grade_node(None, mixer));
    assert!(clip.connect_grade(source, 0, mid, 3));
    let n = clip.grade().nodes.iter().find(|n| n.id == mid).unwrap();
    assert_eq!(n.params.layer_mixer.as_ref().map(|m| m.inputs.len()), Some(4));
}

/// A grade is saved as the graph itself, alone and as an optional grade,
/// and read back from what the graph saved: files are as they were (#1106).
#[test]
fn a_grade_saves_and_reads_as_its_graph() {
    let mut g = NodeGraph::default();
    g.ensure_corrector();
    let graph_json = serde_json::to_string(&g).unwrap();
    assert_eq!(serde_json::to_string(&crate::Grade::new(g.clone())).unwrap(), graph_json);
    assert_eq!(serde_json::to_string(&Some(crate::Grade::new(g.clone()))).unwrap(), serde_json::to_string(&Some(&g)).unwrap());
    let back: crate::Grade = serde_json::from_str(&graph_json).unwrap();
    assert_eq!(back, crate::Grade::new(g));
}
