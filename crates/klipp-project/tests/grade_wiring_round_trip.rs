//! A grade's wiring is saved with the project and read back whole: a key
//! and a window through a combiner, their places on the canvas, and a
//! wireless link.

use klipp_core::{MediaId, RationalTime};
use klipp_media::MediaPool;
use klipp_nodes::{NodeGraph, NodeId, NodeKind, NodePos};
use klipp_project::{load_project, save_project};
use klipp_timeline::{Clip, GradeAccess, Sequence, TrackKind};

fn sorted<T: std::fmt::Debug>(items: impl IntoIterator<Item = T>) -> Vec<String> {
    let mut all: Vec<String> = items.into_iter().map(|i| format!("{i:?}")).collect();
    all.sort();
    all
}

#[test]
fn a_key_and_a_window_through_a_combiner_come_back_wired_the_same() {
    let t = |s: f64| RationalTime::from_secs_f64(s, 24);
    let mut clip = Clip::media(MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
    let clip_id = clip.id;
    let mut g = NodeGraph::default();
    let correction = g.ensure_corrector();
    let key = NodeId::new_v4();
    assert!(g.limit_with_qualifier(correction, key));
    let window = g.ensure_corrector_window();
    g.node_positions.insert(key, NodePos { x: 12.5, y: -40.0 });
    g.nodes.iter_mut().find(|n| n.id == key).unwrap().output_link = Some("key".into());
    let combiner = g.nodes.iter().find(|n| n.kind == NodeKind::CombineMattes).expect("the combiner").id;
    assert_eq!(g.input_of(correction, 1), Some(combiner));
    clip.set_grade(g.clone());
    let mut seq = Sequence::new_default();
    seq.tracks.iter_mut().find(|t| t.kind == TrackKind::Video).unwrap().clips.push(clip);

    let dir = std::env::temp_dir().join(format!("klipp_grade_wiring_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project(&dir).unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    let back = sequences[0].all_clips().find(|c| c.id == clip_id).expect("the clip").grade().clone();
    assert_eq!(sorted(&back.edges), sorted(&g.edges), "the wires");
    assert_eq!(sorted(back.nodes.iter().map(|n| (n.id, n.kind, n.label.clone(), n.output_link.clone(), n.input_links.clone()))), sorted(g.nodes.iter().map(|n| (n.id, n.kind, n.label.clone(), n.output_link.clone(), n.input_links.clone()))), "the nodes");
    assert_eq!(sorted(back.node_positions.iter().map(|(id, p)| (*id, p.x, p.y))), sorted(g.node_positions.iter().map(|(id, p)| (*id, p.x, p.y))), "their places");
    assert_eq!(back.output_node_id, g.output_node_id);
    assert_eq!(back.input_of(window, 0), None, "the window reads nothing");
}

/// A key saved with Sat Low 0, the old default, reads back 0, not the new
/// key's 0.15: what a saved project shows doesn't move with a default.
#[test]
fn a_key_saved_at_the_old_default_keeps_it() {
    let t = |s: f64| RationalTime::from_secs_f64(s, 24);
    let mut clip = Clip::media(MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
    let clip_id = clip.id;
    let mut key = None;
    clip.edit_grade(|g| {
        let c = g.ensure_corrector();
        let k = NodeId::new_v4();
        assert!(g.limit_with_qualifier(c, k));
        g.nodes.iter_mut().find(|n| n.id == k).unwrap().params.qualifier = Some(klipp_core::QualifierSettings { enabled: true, sat_low: 0.0, ..Default::default() });
        key = Some(k);
    });
    let key = key.unwrap();
    let mut seq = Sequence::new_default();
    seq.tracks.iter_mut().find(|t| t.kind == TrackKind::Video).unwrap().clips.push(clip);
    let dir = std::env::temp_dir().join(format!("klipp_key_default_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project(&dir).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let back = sequences[0].all_clips().find(|c| c.id == clip_id).unwrap().grade().clone();
    let q = back.nodes.iter().find(|n| n.id == key).and_then(|n| n.params.qualifier.clone()).expect("its key");
    assert_eq!(q.sat_low, 0.0);
}
