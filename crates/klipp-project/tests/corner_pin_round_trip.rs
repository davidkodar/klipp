//! A corner pin and its keys come back from a project as they went in.
//! The variant is appended after every earlier effect; its corners are
//! plain arrays under `corner_pin`.

use klipp_core::keyframe::Keyframe;
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

#[test]
fn a_pinned_clip_reads_back_pin_and_keys() {
    let dir = scratch("corner_pin");
    let tc = |s: f64| Timecode::from_secs_f64(s, 24);
    let scene = Sequence::new_default();
    let mut outer = Sequence::new_default();
    let mut clip = Clip::sequence(scene.id, tc(0.0), tc(4.0));
    let pin = BuiltinEffect::CornerPin { tl: [0.1, 0.2], tr: [0.9, 0.1], br: [0.7, 0.8], bl: [0.2, 0.95] };
    clip.set_effect_list(clip.builtin_effects().iter().cloned().chain(std::iter::once(pin.clone())).collect(), clip.builtin_effects_enabled().to_vec());
    clip.edit_effect_keyframes(|v| v.insert(
        "0:tr_x".into(),
        vec![
            Keyframe { time_secs: 0.0, value: 0.9, interpolation: Default::default(), tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0), handles: Default::default() },
            Keyframe { time_secs: 2.0, value: 0.6, interpolation: Default::default(), tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0), handles: Default::default() },
        ],
    ));
    let clip_id = clip.id;
    outer.tracks[0].clips = vec![clip];

    save_project(&dir, &[outer, scene], &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project(&dir).unwrap();
    let back = sequences[0].tracks[0].clips.iter().find(|c| c.id == clip_id).expect("the clip");
    assert_eq!(back.builtin_effects(), vec![pin]);
    assert_eq!(back.effect_keyframes().get("0:tr_x").map(|k| k.len()), Some(2));
    assert_eq!(back.effect_keyframes()["0:tr_x"][1].value, 0.6);

    let written = std::fs::read_to_string(dir.join("project.json")).unwrap();
    assert!(written.contains("\"corner_pin\""), "the pin is written under its own name");
    let _ = std::fs::remove_dir_all(&dir);
}
