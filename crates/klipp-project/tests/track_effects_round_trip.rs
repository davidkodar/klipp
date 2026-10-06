//! A track's own effects, grade and audio chain (#846) survive a save and
//! a load, a project without them is written as before, and a track
//! effect from a newer Klipp refuses the project by name, as a clip's does.

use klipp_core::AudioEffectInstance;
use klipp_effects::BuiltinEffect;
use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::{Sequence, TrackKind};

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("klipp_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn exposed(ev: f32) -> klipp_nodes::NodeGraph {
    let mut g = klipp_nodes::NodeGraph::default();
    g.nodes.iter_mut().filter(|n| n.kind == klipp_nodes::NodeKind::Corrector).for_each(|n| n.params.exposure = Some(ev));
    g
}

/// A sequence whose first video track has a vignette (switched off) and a
/// grade, and whose first audio track has a compressor.
fn with_track_effects() -> Sequence {
    let mut seq = Sequence::new_default();
    let v = seq.tracks.iter_mut().find(|t| t.kind == TrackKind::Video).unwrap();
    v.effects.set_video_effect_list(vec![BuiltinEffect::Vignette { amount: -0.4, size: 0.8, softness: 0.5, roundness: 0.7 }], vec![false]);
    v.effects.grade_slot().set(Some(exposed(-0.5)));
    let a = seq.tracks.iter_mut().find(|t| t.kind == TrackKind::Audio).unwrap();
    a.effects.audio = vec![AudioEffectInstance { effect_type: "compressor".into(), enabled: false, wet_dry: 0.8, params: vec![("threshold".into(), -18.0)] }];
    seq
}

#[test]
fn a_tracks_effects_survive_a_save_and_a_load() {
    let dir = scratch("track_effects_round_trip");
    let seq = with_track_effects();
    save_project(&dir, std::slice::from_ref(&seq), &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project(&dir).unwrap();
    for (saved, loaded) in seq.tracks.iter().zip(&sequences[0].tracks) {
        assert_eq!(saved.effects, loaded.effects, "track {} keeps its effects", saved.name);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_project_without_track_effects_is_written_as_before() {
    let dir = scratch("no_track_effects");
    save_project(&dir, &[Sequence::new_default()], &MediaPool::new()).unwrap();
    let json = std::fs::read_to_string(dir.join("project.json")).unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    for track in value["sequences"][0]["tracks"].as_array().unwrap() {
        assert!(track.get("effects").is_none(), "no effects key on a plain track: {track}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_track_effect_from_a_newer_klipp_refuses_the_project_by_name() {
    let dir = scratch("unknown_track_effect");
    save_project(&dir, &[with_track_effects()], &MediaPool::new()).unwrap();
    let path = dir.join("project.json");
    let json = std::fs::read_to_string(&path).unwrap();
    assert!(json.contains("\"vignette\""), "the vignette is saved by its tag");
    std::fs::write(&path, json.replace("\"vignette\"", "\"holographic_glint\"")).unwrap();
    let err = load_project(&dir).expect_err("an unknown track effect is refused");
    assert!(err.contains("holographic_glint") && err.contains("track"), "refused by name and by track: {err}");
    let _ = std::fs::remove_dir_all(&dir);
}
