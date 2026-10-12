//! A track's grade edited by path, as the Colour workspace edits it
//! through its stand-in (#1408), through a real save and a strict reload:
//! the edit is in the track's grade read back, and saving that again
//! writes the same sequences.

use klipp_graph_format::path::{NodePath, Root, TrackPart};
use klipp_graph_lift::ops::GraphOp;
use klipp_media::MediaPool;
use klipp_nodes::{NodeGraph, NodeKind};
use klipp_project::{load_project_strict, save_project};
use klipp_timeline::{Sequence, TrackKind};
use serde_json::Value;

/// `sequences` saved: the file's sequences, and the sequences read back strictly.
fn saved(name: &str, sequences: &[Sequence]) -> (Value, Vec<Sequence>) {
    let dir = std::env::temp_dir().join(format!("klipp_track_grade_save_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    save_project(&dir, sequences, &MediaPool::new()).unwrap_or_else(|e| panic!("{name}: saving: {e}"));
    let file: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("project.json")).unwrap()).unwrap();
    let (loaded, ..) = load_project_strict(&dir).unwrap_or_else(|e| panic!("{name}: a strict reload: {e}"));
    let _ = std::fs::remove_dir_all(&dir);
    (file["sequences"].clone(), loaded)
}

#[test]
fn a_track_grade_edited_by_path_saves_and_reloads() {
    let mut seq = Sequence::new_default();
    let tid = seq.tracks.iter().find(|t| t.kind == TrackKind::Video).unwrap().id;
    let root = Root::Track { track: tid.0, part: TrackPart::Grade };
    // A first edit on the graph the stand-in was put on with.
    let start = NodeGraph::default();
    let corrector = start.nodes.iter().find(|n| n.kind == NodeKind::Corrector).unwrap().id;
    let edit = GraphOp::SetParam { path: NodePath::new(root, [corrector]), key: "exposure".into(), value: klipp_graph_format::ParamValue::Float(0.6) };
    seq.apply_root_grade_ops(&[edit], Some(&start)).unwrap();
    let (first, loaded) = saved("track", &[seq]);
    let track = loaded[0].tracks.iter().find(|t| t.id == tid).expect("the track");
    let exposure = track.effects.track_grade().and_then(|g| g.nodes.iter().find(|n| n.id == corrector)).and_then(|n| n.params.exposure);
    assert_eq!(exposure, Some(0.6), "the edit read back on the track's grade");
    let (second, _) = saved("track-again", &loaded);
    assert_eq!(first, second, "saved again, the same sequences");
}
