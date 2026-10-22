//! A subtitle track's language (#2223) is saved with the project and
//! read back; a track without one writes nothing for it.

use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::{Sequence, Track, TrackKind};

#[test]
fn a_subtitle_tracks_language_is_saved_and_read_back() {
    let mut seq = Sequence::new_default();
    let mut german = Track::new("Subtitle 1 (German)", TrackKind::Subtitle);
    german.language = Some("de".into());
    seq.tracks.push(german);
    seq.tracks.push(Track::new("Subtitle 2", TrackKind::Subtitle));
    let dir = std::env::temp_dir().join(format!("klipp_track_language_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let written = std::fs::read_to_string(dir.join("project.json")).unwrap();
    let (mut sequences, ..) = load_project(&dir).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let back = sequences.remove(0);
    let language = |name: &str| back.tracks.iter().find(|t| t.name == name).map(|t| t.language.clone());
    assert_eq!(language("Subtitle 1 (German)"), Some(Some("de".into())));
    assert_eq!(language("Subtitle 2"), Some(None));
    assert_eq!(written.matches("\"language\"").count(), 1, "only the track with one writes it");
}
