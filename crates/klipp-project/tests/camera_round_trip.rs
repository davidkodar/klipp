//! The sequence's camera is part of the project: saved with the
//! sequence, and absent — not written at all — when there is none, so a
//! project without one reads exactly as it did before there could be one.

use klipp_core::keyframe::upsert_keyframe;
use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::Sequence;

#[test]
fn a_camera_comes_back_with_the_project_and_no_camera_writes_nothing() {
    let dir = std::env::temp_dir().join(format!("klipp_camera_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let mut seq = Sequence::new_default();
    upsert_keyframe(&mut seq.camera.x_keyframes, 0.0, 0.5);
    upsert_keyframe(&mut seq.camera.x_keyframes, 6.0, 0.35);
    upsert_keyframe(&mut seq.camera.zoom_keyframes, 0.0, 1.0);
    upsert_keyframe(&mut seq.camera.zoom_keyframes, 6.0, 1.25);
    upsert_keyframe(&mut seq.camera.roll_keyframes, 6.0, -2.0);
    let camera = seq.camera.clone();
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project(&dir).unwrap();
    assert_eq!(sequences[0].camera, camera);

    save_project(&dir, &[Sequence::new_default()], &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project(&dir).unwrap();
    assert!(sequences[0].camera.is_still());
    let written = std::fs::read_to_string(dir.join("project.json")).unwrap();
    assert!(written.contains("\"tracks\""), "that is the project file");
    assert!(!written.contains("\"camera\""), "a sequence with no camera writes no camera");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_clip_s_depth_comes_back_with_the_project() {
    let dir = std::env::temp_dir().join(format!("klipp_depth_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let mut seq = Sequence::new_default();
    let solid = klipp_core::GeneratorData::SolidColor { color: [10, 10, 30, 255] };
    let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    let mut far = klipp_timeline::Clip { kind: klipp_timeline::ClipKind::Generator(solid.clone()), ..klipp_timeline::Clip::gap(tc(0.0), tc(2.0)) };
    far.depth = 3.0;
    let near = klipp_timeline::Clip { kind: klipp_timeline::ClipKind::Generator(solid), ..klipp_timeline::Clip::gap(tc(2.0), tc(4.0)) };
    seq.tracks[0].clips = vec![far, near];
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project(&dir).unwrap();
    let depths: Vec<f32> = sequences[0].tracks[0].clips.iter().map(|c| c.depth).collect();
    assert_eq!(depths, vec![3.0, 1.0]);
    let written = std::fs::read_to_string(dir.join("project.json")).unwrap();
    assert_eq!(written.matches("\"depth\"").count(), 1, "only the clip off the plane writes a depth");
    let _ = std::fs::remove_dir_all(&dir);
}
