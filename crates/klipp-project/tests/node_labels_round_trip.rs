//! Colour labels on graph nodes (#1753) are part of the project: a grade
//! node's, a picture effect's and a timeline grade node's come back as
//! they went in; one whose node is gone is not written; a sequence with
//! none writes none.

use klipp_graph_format::path::{NodePath, Root};
use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::{grade_group_id, GradeAccess, NodeLabel, Sequence};

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("klipp_node_labels_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn node_labels_come_back_and_dead_ones_are_dropped() {
    let dir = scratch("trip");
    let mut seq = Sequence::new_default();
    let mut clip = klipp_timeline::test_clip::TestClip::media(2.0).effect(klipp_effects::BuiltinEffect::GaussianBlur { radius: 2.0 }).build();
    let corrector = clip.edit_grade(|g| g.ensure_corrector());
    let blur = clip.effect_node_at(false, 0).expect("the blur's node");
    let mut timeline = klipp_nodes::NodeGraph::default();
    let tone = timeline.ensure_corrector();
    seq.timeline_grade_slot().set(Some(timeline));
    let at = |nodes: Vec<uuid::Uuid>| NodePath::new(Root::Clip { clip: clip.id.0 }, nodes);
    let kept = vec![
        NodeLabel { node: at(vec![grade_group_id(clip.id), corrector]), colour: 1 },
        NodeLabel { node: at(vec![blur]), colour: 5 },
        NodeLabel { node: NodePath::new(Root::TimelineGrade { sequence: seq.id.0 }, [tone]), colour: 6 },
    ];
    let gone = NodeLabel { node: at(vec![grade_group_id(clip.id), uuid::Uuid::new_v4()]), colour: 3 };
    seq.tracks[0].clips.push(clip);
    seq.node_labels = kept.iter().cloned().chain([gone]).collect();

    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project(&dir).unwrap();
    assert_eq!(sequences[0].node_labels, kept, "each label back, the dead one dropped");
    for label in &kept {
        assert!(sequences[0].holds_node(&label.node), "{label:?} names a node of the loaded sequence");
    }

    save_project(&dir, &[Sequence::new_default()], &MediaPool::new()).unwrap();
    let written = std::fs::read_to_string(dir.join("project.json")).unwrap();
    assert!(!written.contains("node_labels"), "none written for a sequence without labels");
    let _ = std::fs::remove_dir_all(&dir);
}
