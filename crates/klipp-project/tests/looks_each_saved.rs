//! A Look that can't be saved doesn't stop the others (#1135): every Look
//! is written, every failure is reported, and the failed Look keeps the
//! file of its last good save.

use klipp_nodes::{Edge, NodeGraph};
use klipp_timeline::Look;

fn temp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("klipp-looks-each-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn look(name: &str, graph: NodeGraph) -> Look {
    Look { id: uuid::Uuid::new_v4(), name: name.into(), node_graph: graph }
}

/// A grade whose last node feeds its first: a loop, which a revision-2
/// grade can't hold.
fn looping() -> NodeGraph {
    let mut g = NodeGraph::default();
    let (first, last) = (g.nodes.first().unwrap().id, g.nodes.last().unwrap().id);
    g.edges.push(Edge { from_node: last, from_port: 0, to_node: first, to_port: 0 });
    assert!(g.topo_sort().is_err(), "the fixture loops");
    g
}

#[test]
fn a_look_that_cant_be_saved_doesnt_stop_the_others() {
    let dir = temp("save");
    let (a, mut b, c) = (look("A", NodeGraph::default()), look("B", NodeGraph::default()), look("C", NodeGraph::default()));
    klipp_project::save_looks(&dir, &[a.clone(), b.clone()]).unwrap();
    let b_file = dir.join("looks").join(format!("{}.json", b.id));
    let b_before = std::fs::read(&b_file).unwrap();

    b.node_graph = looping();
    let err = klipp_project::save_looks(&dir, &[a.clone(), b.clone(), c.clone()]).unwrap_err();
    assert!(err.contains('B'), "the failure names the Look: {err}");
    assert!(dir.join("looks").join(format!("{}.json", c.id)).exists(), "the Look after the failed one is written");
    assert_eq!(std::fs::read(&b_file).unwrap(), b_before, "the failed Look keeps its last good file");
    let looks = klipp_project::load_looks(&dir);
    let mut names: Vec<_> = looks.iter().map(|l| l.name.clone()).collect();
    names.sort();
    assert_eq!(names, ["A", "B", "C"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_side_store_from_a_newer_klipp_says_so() {
    use serde_json::json;
    assert_eq!(klipp_project::side_store_revision_refusal(&json!({ "format_revision": 2 })), None);
    let newer = klipp_project::side_store_revision_refusal(&json!({ "format_revision": 3 })).unwrap();
    assert!(newer.contains("newer Klipp") && !newer.contains("klipp-convert"), "{newer}");
    for older in [json!({ "format_revision": 0 }), json!({})] {
        assert!(klipp_project::side_store_revision_refusal(&older).unwrap().contains("klipp-convert run"));
    }
}
