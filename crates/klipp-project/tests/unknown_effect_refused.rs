//! A reader that meets an effect it does not know refuses the project by
//! name, rather than reading the clip with no effects at all and saving
//! it back that way (#222). The effects it does know still read.

use klipp_core::Timecode;
use klipp_effects::BuiltinEffect;
use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::{Clip, Sequence};

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("klipp_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A project with one clip carrying a crop and a blur, saved by this
/// Klipp; the caller then edits its JSON to look like a newer one.
fn saved_with_two_effects(tag: &str) -> (std::path::PathBuf, String) {
    let dir = scratch(tag);
    let tc = |s: f64| Timecode::from_secs_f64(s, 24);
    let scene = Sequence::new_default();
    let mut outer = Sequence::new_default();
    let mut clip = Clip::sequence(scene.id, tc(0.0), tc(4.0));
    clip.set_effect_list(clip.builtin_effects().iter().cloned().chain(std::iter::once(BuiltinEffect::Crop { left: 0.1, right: 0.0, top: 0.0, bottom: 0.0, feather: 0.0, aspect: 0.0, focus_x: 0.5, focus_y: 0.5})).collect(), clip.builtin_effects_enabled().to_vec());
    clip.set_effect_list(clip.builtin_effects().iter().cloned().chain(std::iter::once(BuiltinEffect::GaussianBlur { radius: 3.0 })).collect(), clip.builtin_effects_enabled().to_vec());
    let clip_id = clip.id.to_string();
    outer.tracks[0].clips = vec![clip];
    save_project(&dir, &[outer, scene], &MediaPool::new()).unwrap();
    (dir, clip_id)
}

#[test]
fn an_effect_from_a_newer_klipp_refuses_the_project_by_name() {
    let (dir, clip_id) = saved_with_two_effects("unknown_effect");
    let path = dir.join("project.json");
    let json = std::fs::read_to_string(&path).unwrap();
    assert!(json.contains("\"gaussian_blur\""));
    std::fs::write(&path, json.replace("\"gaussian_blur\"", "\"holographic_glint\"")).unwrap();

    let err = load_project(&dir).expect_err("an unknown effect is refused");
    assert!(err.contains("holographic_glint"), "refused by name: {err}");
    assert!(err.contains(&clip_id), "and by clip: {err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn effects_that_are_not_a_list_refuse_the_project_too() {
    let (dir, clip_id) = saved_with_two_effects("effects_not_a_list");
    let path = dir.join("project.json");
    let mut value: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let clip = &mut value["sequences"][0]["tracks"][0]["clips"][0];
    assert_eq!(clip["id"], clip_id);
    clip["builtin_effects"] = serde_json::json!(5);
    std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();

    // In a revision-2 file the old list holds only the sentinel: anything
    // else there is a damaged file, refused by clip (#1089).
    let err = load_project(&dir).expect_err("a value that is no list of effects is refused");
    assert!(err.contains(&clip_id) && err.contains("damaged"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_effects_this_klipp_knows_still_read() {
    let (dir, clip_id) = saved_with_two_effects("known_effects");
    let (sequences, ..) = load_project(&dir).unwrap();
    let clip = sequences[0].tracks[0].clips.iter().find(|c| c.id.to_string() == clip_id).unwrap();
    assert_eq!(clip.builtin_effects().len(), 2);
    assert!(matches!(clip.builtin_effects()[1], BuiltinEffect::GaussianBlur { radius } if radius == 3.0));
    let _ = std::fs::remove_dir_all(&dir);
}
