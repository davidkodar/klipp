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
