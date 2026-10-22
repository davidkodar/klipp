//! The sequence camera's keys come back from a project as they went in:
//! every list, every moment, the curve out of each key and its handles.
//! A key set from the lane or the curve editor is written by the same
//! path as one set over the control socket, so this is the one place
//! that round trip is checked.

use klipp_core::keyframe::{Interpolation, Keyframe};
use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::Sequence;

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("klipp_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn key(time_secs: f64, value: f32, interpolation: Interpolation, tangent_in: (f64, f32), tangent_out: (f64, f32)) -> Keyframe {
    Keyframe { time_secs, value, interpolation, tangent_in, tangent_out, handles: Default::default() }
}

#[test]
fn camera_keys_with_curves_and_handles_come_back_as_written() {
    let dir = scratch("camera_keys");
    let mut seq = Sequence::new_default();
    // An off-frame moment, a Bezier segment with handles, a hold, and a
    // negative roll: each of the things a key can carry.
    seq.camera.x_keyframes = vec![
        key(0.7, 0.4, Interpolation::Bezier, (0.0, 0.0), (0.3, 0.1)),
        key(2.0, 0.6, Interpolation::Linear, (-0.2, -0.05), (0.0, 0.0)),
    ];
    seq.camera.zoom_keyframes = vec![key(1.0, 1.0, Interpolation::Step, (0.0, 0.0), (0.0, 0.0)), key(3.0, 1.8, Interpolation::Linear, (0.0, 0.0), (0.0, 0.0))];
    seq.camera.roll_keyframes = vec![key(0.0, -12.5, Interpolation::Linear, (0.0, 0.0), (0.0, 0.0))];

    save_project(&dir, &[seq.clone()], &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project(&dir).unwrap();
    assert_eq!(sequences.len(), 1);
    assert_eq!(sequences[0].camera, seq.camera, "the camera reads back key for key");
    assert!(sequences[0].camera.y_keyframes.is_empty(), "a list with no keys stays empty");
    let _ = std::fs::remove_dir_all(&dir);
}
