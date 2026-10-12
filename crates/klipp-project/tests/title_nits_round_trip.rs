//! A sequence's title brightness for HDR deliveries (#1723) is part of
//! the project: a chosen level comes back, the default is not written,
//! and a level from outside the range is held inside it on load.

use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::{Sequence, DEFAULT_TITLE_NITS, TITLE_NITS_RANGE};

fn dir(label: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("klipp_title_nits_{label}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn a_title_brightness_comes_back_with_the_project() {
    let d = dir("choice");
    let mut seq = Sequence::new_default();
    seq.title_nits = 600.0;
    save_project(&d, &[seq], &MediaPool::new()).unwrap();
    let (sequences, ..) = load_project(&d).unwrap();
    assert_eq!(sequences[0].title_nits, 600.0);

    save_project(&d, &[Sequence::new_default()], &MediaPool::new()).unwrap();
    let written = std::fs::read_to_string(d.join("project.json")).unwrap();
    assert!(!written.contains("title_nits"), "the default is left out");
    let (sequences, ..) = load_project(&d).unwrap();
    assert_eq!(sequences[0].title_nits, DEFAULT_TITLE_NITS);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_title_brightness_from_outside_the_range_is_held_inside_it() {
    let d = dir("range");
    let mut seq = Sequence::new_default();
    seq.title_nits = 600.0;
    save_project(&d, &[seq], &MediaPool::new()).unwrap();
    let path = d.join("project.json");
    let written = std::fs::read_to_string(&path).unwrap();
    assert_eq!(written.matches("\"title_nits\": 600.0").count() + written.matches("\"title_nits\":600.0").count(), 1, "{written}");
    for (stored, want) in [("1e9", *TITLE_NITS_RANGE.end()), ("-5", *TITLE_NITS_RANGE.start())] {
        std::fs::write(&path, written.replace("600.0", stored)).unwrap();
        let (sequences, ..) = load_project(&d).unwrap();
        assert_eq!(sequences[0].title_nits, want, "stored {stored}");
    }
    let _ = std::fs::remove_dir_all(&d);
}
