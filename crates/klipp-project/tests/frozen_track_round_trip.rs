//! #3207: a frozen track is saved with the project and opens frozen, and a
//! track that isn't says nothing of it, so a file from before freezing, or
//! one a frozen track was thawed in, reads the same.

use klipp_media::MediaPool;
use klipp_project::{load_project, save_project_with_color, ColorSettings};
use klipp_timeline::{Sequence, Track, TrackKind};

fn project_dir(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("klipp_frozen_{}_{}", label, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn saved_and_opened(label: &str, seq: &Sequence) -> (Sequence, serde_json::Value) {
    let dir = project_dir(label);
    save_project_with_color(&dir, std::slice::from_ref(seq), &MediaPool::new(), &ColorSettings::default(), None, None, "").unwrap();
    let file: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("project.json")).unwrap()).unwrap();
    let (mut seqs, ..) = load_project(&dir).unwrap();
    let opened = seqs.remove(0);
    let _ = std::fs::remove_dir_all(&dir);
    (opened, file)
}

#[test]
fn a_frozen_track_opens_frozen() {
    let mut seq = Sequence::new_default();
    seq.tracks[0].frozen = true;
    let mut locks = Track::new("A2", TrackKind::Audio);
    locks.set_lock_while_frozen(true);
    locks.set_frozen(true);
    seq.tracks.push(locks);
    let (opened, _) = saved_and_opened("on", &seq);
    let first = &opened.tracks[0];
    assert!(first.frozen && !first.lock_while_frozen && !first.locked);
    let last = opened.tracks.last().unwrap();
    assert!(last.frozen && last.lock_while_frozen && last.locked, "locked with it");
}

#[test]
fn a_live_track_writes_nothing_of_it() {
    let (opened, file) = saved_and_opened("off", &Sequence::new_default());
    assert!(opened.tracks.iter().all(|t| !t.frozen && !t.lock_while_frozen));
    let text = file.to_string();
    assert!(!text.contains("frozen"), "{text}");
}
