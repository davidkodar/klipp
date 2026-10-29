//! A project saved with names Klipp has since renamed opens through the
//! id migration table, and one naming a grade node this Klipp doesn't
//! know is refused by name, never opened with it dropped (#2888).

use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::{GradeAccess, Sequence, TrackKind};

/// A project with one keyed clip and a grade, saved to a new folder.
fn saved() -> std::path::PathBuf {
    let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 25);
    let mut clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    let key = klipp_effects::BuiltinEffect::ChromaKey { key_color: [0, 255, 0], tolerance: 0.2, soft_edge: 0.37, spill_method: Default::default(), spill_strength: 0.6 };
    clip.apply_effect_edits(&[klipp_timeline::EffectEdit::Add(&key)]).unwrap().unwrap();
    let mut grade = klipp_nodes::NodeGraph::default();
    grade.ensure_masked_secondary(klipp_nodes::NodeKind::Exposure);
    clip.set_grade(grade);
    let mut seq = Sequence::new_default();
    let v1 = seq.tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap();
    seq.tracks[v1].clips.push(clip);
    let dir = std::env::temp_dir().join(format!("klipp-old-ids-{}", uuid::Uuid::new_v4()));
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    dir
}

fn soft_edge(dir: &std::path::Path) -> f32 {
    let (seqs, ..) = load_project(dir).unwrap();
    let clip = seqs[0].all_clips().next().unwrap();
    // The key as the clip's picture holds it.
    let lifted = clip.picture_v2().unwrap();
    let key = lifted.picture.graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("chroma_key")).expect("the key");
    klipp_graph_lift::effect_params::entry_of(key).unwrap()["soft_edge"].as_f64().unwrap() as f32
}

#[test]
fn a_renamed_setting_opens_as_it_is_now_and_saves_so() {
    let dir = saved();
    let file = dir.join("project.json");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("\"soft_edge\""));
    // The key's edge as Klipp first saved it.
    let old = text.replace("\"soft_edge\"", "\"spill_suppression\"");
    std::fs::write(&file, &old).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&old).unwrap();
    klipp_project::migrate_ids::migrate(&mut value).unwrap();
    assert!(!value.to_string().contains("spill_suppression") && value.to_string().contains("\"soft_edge\""), "the table renames it in the file as read");
    assert!((soft_edge(&dir) - 0.37).abs() < 1e-6);
    let (seqs, pool, ..) = load_project(&dir).unwrap();
    save_project(&dir, &seqs, &pool).unwrap();
    assert!(!std::fs::read_to_string(&file).unwrap().contains("spill_suppression"), "saved under its name now");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn an_unknown_grade_node_is_refused_by_name_and_the_file_kept() {
    let dir = saved();
    let file = dir.join("project.json");
    let text = std::fs::read_to_string(&file).unwrap().replacen("\"kind\": \"Exposure\"", "\"kind\": \"Sparkle\"", 1);
    assert!(text.contains("Sparkle"));
    std::fs::write(&file, &text).unwrap();
    let Err(why) = load_project(&dir) else { panic!("opened with an unknown grade node") };
    assert!(why.contains("Sparkle") && why.contains("newer Klipp"), "{why}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), text, "nothing written");
    std::fs::remove_dir_all(dir).unwrap();
}
