//! A grade's mixes and opacity set to 0 come back from a save and load as
//! 0, not as "unset", which every reader takes as the default (full mix).

use klipp_media::MediaPool;
use klipp_nodes::NodeKind;
use klipp_project::{load_project, save_project};
use klipp_timeline::{GradeAccess, Sequence, TrackKind};

#[test]
fn a_grade_mix_set_to_zero_saves_and_loads_as_zero() {
    let names = ["global_mix", "curves_mix", "huesat_mix", "lut_opacity", "mix"];
    let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 25);
    let mut clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    let mut graph = klipp_nodes::NodeGraph::default();
    let corrector = graph.ensure_corrector();
    for name in names {
        assert!(graph.nodes.iter_mut().find(|n| n.id == corrector).unwrap().params.apply_override(name, 0.0), "{name}");
    }
    clip.set_grade(graph);
    let id = clip.id;
    let mut seq = Sequence::new_default();
    let v1 = seq.tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap();
    seq.tracks[v1].clips.push(clip);
    let dir = std::env::temp_dir().join(format!("klipp-zero-mix-rt-{}", uuid::Uuid::new_v4()));
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let (loaded, ..) = load_project(&dir).unwrap();
    let clip = loaded[0].all_clips().find(|c| c.id == id).expect("the clip");
    let node = clip.grade().nodes.iter().find(|n| n.kind == NodeKind::Corrector).expect("the Corrector").clone();
    let stored = serde_json::to_value(&node.params).unwrap();
    for name in names {
        assert_eq!(stored[name], serde_json::json!(0.0), "{name}: loaded as {}", stored[name]);
    }
    std::fs::remove_dir_all(dir).unwrap();
}
