//! The lift makes today's load-time rewrites once, so a converted project
//! shows what today's loader shows. Each is checked against today's own
//! loader (`load_project`) on the same file.

use klipp_convert::lift::{default_pipeline, lift_clip, ClipKind, LiftCtx};
use klipp_convert::to_v2;
use klipp_graph_format::lifted::{types as lt, LiftNote};
use klipp_graph_format::Graph;
use serde_json::{json, Value};
use std::path::PathBuf;

fn fixture() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../klipp-project/tests/fixtures/pre_layer_fx/project.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Today's loader's view of the fixture's first clip with `node_graph`
/// set (or removed, for `None`), and that clip's JSON.
fn loaded(node_graph: Option<Value>, tag: &str) -> (klipp_nodes::NodeGraph, Value) {
    let mut p = fixture();
    let clip = &mut p["sequences"][0]["tracks"][0]["clips"][0];
    match node_graph {
        Some(g) => clip["node_graph"] = g,
        None => {
            clip.as_object_mut().unwrap().remove("node_graph");
        }
    }
    let clip = clip.clone();
    let dir: PathBuf = std::env::temp_dir().join(format!("klipp-lift-load-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("media")).unwrap();
    std::fs::write(dir.join("project.json"), p.to_string()).unwrap();
    std::fs::write(dir.join("media/manifest.json"), r#"{"version":"1.0.0","items":[],"bins":[]}"#).unwrap();
    let (seqs, ..) = klipp_project::load_project(&dir).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (seqs[0].tracks[0].clips[0].node_graph.clone(), clip)
}

fn lifted_body(clip: &Value) -> (Graph, Vec<LiftNote>) {
    let record = to_v2::clip_to_v2(clip.as_object().unwrap()).unwrap();
    let id = uuid::Uuid::parse_str(clip["id"].as_str().unwrap()).unwrap();
    let (p, notes) = lift_clip(&record, &LiftCtx { clip_id: id, kind: ClipKind::Media }).unwrap();
    let g = p.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP).unwrap();
    (g.group.as_ref().unwrap().graph.clone(), notes)
}

fn kind(n: &klipp_graph_format::Node) -> String {
    n.legacy.as_ref().and_then(|l| l.payload.get("kind")).and_then(Value::as_str).unwrap_or("").to_owned()
}

/// A graph's nodes as kinds in order, and its edges as kind → kind, ids
/// aside (today's warper id is random).
fn shape_today(g: &klipp_nodes::NodeGraph) -> (Vec<String>, Vec<(String, u8, String, u8)>) {
    let k = |id| g.nodes.iter().find(|n| n.id == id).map(|n| format!("{:?}", n.kind)).unwrap_or_default();
    (g.nodes.iter().map(|n| format!("{:?}", n.kind)).collect(), g.edges.iter().map(|e| (k(e.from_node), e.from_port, k(e.to_node), e.to_port)).collect())
}

fn shape_lifted(g: &Graph) -> (Vec<String>, Vec<(String, u8, String, u8)>) {
    let k = |id| g.nodes.iter().find(|n| n.id == id).map(kind).unwrap_or_default();
    let port = |id, key: &klipp_graph_format::PortKey, input: bool| {
        let kd = k(id);
        if input { klipp_graph_format::ports::input_index(&kd, key) } else { klipp_graph_format::ports::output_index(&kd, key) }.unwrap()
    };
    // The fixtures wire every primary, so the lift adds no implicit-read
    // edge and every edge compares one for one.
    let edges = g.edges.iter().map(|e| (k(e.from.0), port(e.from.0, &e.from.1, false), k(e.to.0), port(e.to.0, &e.to.1, true)));
    // The lifted body's `frame` input has no node in today's graph.
    let nodes = g.nodes.iter().filter(|n| n.type_id.as_str() != lt::GROUP_FRAME).map(kind).collect();
    (nodes, edges.collect())
}

fn legacy_chain() -> Value {
    let id = |i: u8| format!("00000000-0000-0000-0000-0000000001{i:02}");
    let kinds = ["Source", "Exposure", "Contrast", "HueSatCurves", "ColorBalance", "Output"];
    let nodes: Vec<Value> = kinds.iter().enumerate().map(|(i, k)| json!({"id": id(i as u8), "kind": k, "label": k, "enabled": true})).collect();
    let edges: Vec<Value> = (0..kinds.len() - 1).map(|i| json!({"from_node": id(i as u8), "from_port": 0, "to_node": id(i as u8 + 1), "to_port": 0})).collect();
    json!({"nodes": nodes, "edges": edges, "output_node_id": id(5)})
}

/// A graph on the old legacy chain gets today's Color Warper, in today's
/// place, wired as today wires it.
#[test]
fn the_color_warper_migration_is_todays() {
    let (today, clip) = loaded(Some(legacy_chain()), "warper");
    let (body, notes) = lifted_body(&clip);
    assert_eq!(shape_lifted(&body), shape_today(&today));
    assert!(notes.iter().any(|n| matches!(n, LiftNote::LoadMigration { .. })));
    // Lifting again gives the same warper id: derived, never random.
    assert_eq!(lifted_body(&clip).0, body);
}

/// A graph with a Corrector is left alone, as today leaves it.
#[test]
fn a_corrector_graph_is_not_migrated() {
    let mut g = legacy_chain();
    g["nodes"][2]["kind"] = json!("Corrector");
    let (today, clip) = loaded(Some(g), "corrector");
    let (body, notes) = lifted_body(&clip);
    assert_eq!(shape_lifted(&body), shape_today(&today));
    assert!(!notes.iter().any(|n| matches!(n, LiftNote::LoadMigration { .. })));
}

/// A clip with no grade graph loads today as the default pipeline; the
/// lift writes that same pipeline out.
#[test]
fn an_absent_grade_is_todays_default_pipeline() {
    let (today, clip) = loaded(None, "default");
    assert_eq!(today.nodes.len(), klipp_nodes::NodeGraph::default().nodes.len(), "today loads NodeGraph::default()");
    let (body, notes) = lifted_body(&clip);
    assert_eq!(shape_lifted(&body), shape_today(&today));
    assert!(notes.iter().any(|n| matches!(n, LiftNote::DefaultPipeline { .. })));
    // And the written default is today's serialised default, ids aside.
    let ours: klipp_nodes::NodeGraph = serde_json::from_value(default_pipeline(uuid::Uuid::nil())).unwrap();
    let theirs = klipp_nodes::defaults::default_graph();
    assert_eq!(shape_today(&ours), shape_today(&theirs));
    let positions = |g: &klipp_nodes::NodeGraph| {
        let mut v: Vec<(String, (i32, i32))> = g.nodes.iter().map(|n| (format!("{:?}", n.kind), g.node_positions.get(&n.id).map(|p| (p.x as i32, p.y as i32)).unwrap_or_default())).collect();
        v.sort();
        v
    };
    assert_eq!(positions(&ours), positions(&theirs));
    let labels = |g: &klipp_nodes::NodeGraph| g.nodes.iter().map(|n| n.label.clone()).collect::<Vec<_>>();
    assert_eq!(labels(&ours), labels(&theirs));
}
