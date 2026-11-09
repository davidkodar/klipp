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

/// A clip's grade versions come back with their names, which is active, and
/// each grade's wiring: a key and window through a combiner in one, a plain
/// grade in the other.
#[test]
fn grade_versions_come_back_with_their_wiring() {
    let t = |s: f64| RationalTime::from_secs_f64(s, 24);
    let mut clip = Clip::media(MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
    let clip_id = clip.id;
    let mut keyed = NodeGraph::default();
    let correction = keyed.ensure_corrector();
    assert!(keyed.limit_with_qualifier(correction, NodeId::new_v4()));
    keyed.ensure_corrector_window();
    clip.edit_grade_versions(|vs| {
        vs.push(klipp_timeline::GradeVersion::new(String::from("Keyed"), keyed.clone()));
        vs.push(klipp_timeline::GradeVersion::new(String::from("Plain"), NodeGraph::default()));
    });
    clip.set_active_grade_version(Some(1));
    let mut seq = Sequence::new_default();
    seq.tracks.iter_mut().find(|t| t.kind == TrackKind::Video).unwrap().clips.push(clip);
    let dir = std::env::temp_dir().join(format!("klipp_grade_versions_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project(&dir).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let back = sequences[0].all_clips().find(|c| c.id == clip_id).expect("the clip");
    assert_eq!(back.active_grade_version(), Some(1));
    let versions = back.grade_versions();
    assert_eq!(versions.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(), ["Keyed", "Plain"]);
    assert_eq!(sorted(&versions[0].grade().edges), sorted(&keyed.edges), "the keyed version's wires");
    assert_eq!(versions[0].grade().nodes.iter().filter(|n| n.kind == NodeKind::CombineMattes).count(), 1);
}

/// Hue/Sat curves with points, including ones at the curve's very ends, and
/// a Warper's twelve sectors, come back to the bit.
#[test]
fn hue_sat_curves_and_the_warper_come_back_exact() {
    let t = |s: f64| RationalTime::from_secs_f64(s, 24);
    let mut clip = Clip::media(MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
    let clip_id = clip.id;
    let curves = klipp_core::HueSatCurves {
        hue_vs_hue: vec![[0.0, 0.5], [0.0833, 0.62], [0.1667, 0.5], [1.0, 0.5]],
        hue_vs_sat: vec![[0.3, 0.5], [0.33333334, 0.1], [0.36, 0.5]],
        lum_vs_sat: vec![[0.0, 0.25], [1.0, 0.75]],
        ..Default::default()
    };
    let mut warper = klipp_core::ColorWarperParams::default();
    warper.hue_shifts[3] = 12.5;
    warper.sat_shifts[11] = -0.35;
    clip.edit_grade(|g| {
        let c = g.ensure_corrector();
        let n = g.nodes.iter_mut().find(|n| n.id == c).unwrap();
        n.params.huesat = Some(curves.clone());
        n.params.huesat_mix = Some(0.7);
        n.params.warper_params = Some(warper.clone());
    });
    let want = clip.grade().clone();
    let mut seq = Sequence::new_default();
    seq.tracks.iter_mut().find(|t| t.kind == TrackKind::Video).unwrap().clips.push(clip);
    let dir = std::env::temp_dir().join(format!("klipp_huesat_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project(&dir).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let back = sequences[0].all_clips().find(|c| c.id == clip_id).expect("the clip").grade().clone();
    let params = |g: &NodeGraph| g.nodes.iter().find(|n| n.kind == NodeKind::Corrector).map(|n| (n.params.huesat.clone(), n.params.huesat_mix, n.params.warper_params.clone()));
    assert_eq!(params(&back), params(&want));
}
