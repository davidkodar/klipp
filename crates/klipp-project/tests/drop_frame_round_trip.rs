//! Whether a sequence counts drop-frame is part of the project, and a
//! project saved before the question existed has to answer it by its
//! rate rather than be read as having chosen non-drop.
//!
//! That distinction is the whole reason the field is written as an
//! `Option` on disk: absent and `false` mean different things, and
//! conflating them would leave every NTSC project from before this
//! drifting 3.6 s an hour against the clock while looking deliberate.

use klipp_core::Fps;
use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::Sequence;

fn dir(label: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("klipp_df_{label}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn the_choice_comes_back_with_the_project() {
    let d = dir("choice");

    // An NTSC sequence deliberately delivered non-drop: a real choice,
    // and one the project has to remember rather than helpfully undo.
    let mut seq = Sequence::new_default();
    seq.fps = Fps::ntsc_29970();
    seq.drop_frame = false;
    save_project(&d, &[seq], &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project(&d).unwrap();
    assert!(
        !sequences[0].drop_frame,
        "29.97 non-drop is a delivery, not a mistake to correct on load"
    );

    let mut seq = Sequence::new_default();
    seq.fps = Fps::ntsc_29970();
    seq.drop_frame = true;
    save_project(&d, &[seq], &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project(&d).unwrap();
    assert!(sequences[0].drop_frame);

    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_project_from_before_the_field_takes_its_rate_default() {
    let d = dir("legacy");

    // Save a normal project, then strip the field from the JSON to make
    // it look like one written before the field existed.
    let mut seq = Sequence::new_default();
    seq.fps = Fps::ntsc_29970();
    seq.drop_frame = true;
    save_project(&d, &[seq], &MediaPool::new()).unwrap();

    let path = d.join("project.json");
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.contains("drop_frame"), "the choice is written");

    // Delete the key through the JSON rather than by string surgery:
    // it is the last field in its object, so there is no comma to match
    // and a textual strip leaves invalid JSON behind.
    let mut doc: serde_json::Value = serde_json::from_str(&written).unwrap();
    let mut removed = 0;
    if let Some(seqs) = doc.get_mut("sequences").and_then(|v| v.as_array_mut()) {
        for s in seqs {
            if let Some(obj) = s.as_object_mut() {
                if obj.remove("drop_frame").is_some() {
                    removed += 1;
                }
            }
        }
    }
    assert_eq!(removed, 1, "the fixture should have lost exactly one field");
    let stripped = serde_json::to_string_pretty(&doc).unwrap();
    assert!(!stripped.contains("drop_frame"), "the field is gone from the fixture");
    std::fs::write(&path, stripped).unwrap();

    let (sequences, ..) = load_project(&d).unwrap();
    assert!(
        sequences[0].drop_frame,
        "an NTSC project that says nothing counts drop-frame, which is the convention \
         for the rate — reading silence as a deliberate non-drop leaves it drifting"
    );

    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_rate_with_no_drop_frame_does_not_get_one() {
    let d = dir("pal");

    let mut seq = Sequence::new_default();
    seq.fps = Fps::new(25, 1);
    assert!(
        !Sequence::default_drop_frame(seq.fps),
        "25 has nothing to drop"
    );
    seq.drop_frame = Sequence::default_drop_frame(seq.fps);
    save_project(&d, &[seq], &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project(&d).unwrap();
    assert!(!sequences[0].drop_frame);

    // And a 25 sequence that somehow claims drop-frame still shows plain
    // timecode, because the display honours the rate over the flag.
    let mut odd = Sequence::new_default();
    odd.fps = Fps::new(25, 1);
    odd.drop_frame = true;
    assert!(!odd.timecode_display().is_drop());

    let _ = std::fs::remove_dir_all(&d);
}
