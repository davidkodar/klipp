use klipp_media::MediaPool;
use klipp_project::{load_project, save_project};
use klipp_timeline::{Sequence, Track, TrackKind};
use std::path::{Path, PathBuf};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("klipp-subtitle-source-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn stored_source(dir: &Path) -> serde_json::Value {
    let project: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("project.json")).unwrap()).unwrap();
    project["sequences"][0]["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["kind"] == "Subtitle")
        .unwrap()["source_file"]
        .clone()
}

#[test]
fn subtitle_source_uses_project_relative_paths_and_survives_relocation() {
    let root = Scratch::new();
    let old = root.0.join("old.klipp");
    let moved = root.0.join("moved.klipp");
    let mut seq = Sequence::new_default();
    let mut track = Track::new("Captions", TrackKind::Subtitle);
    // It need not be online: the import provenance is still useful.
    track.source_file = Some(old.join("captions/interview.srt"));
    let id = track.id;
    seq.tracks.push(track);
    save_project(&old, &[seq], &MediaPool::new()).unwrap();
    assert_eq!(stored_source(&old), "captions/interview.srt");
    std::fs::rename(&old, &moved).unwrap();
    let (mut seqs, media, ..) = load_project(&moved).unwrap();
    let track = seqs[0].tracks.iter_mut().find(|t| t.id == id).unwrap();
    assert_eq!(
        track.source_file,
        Some(moved.join("captions/interview.srt"))
    );
    // An outside source stays outside, using the same path rules as media.
    track.source_file = Some(root.0.join("external.vtt"));
    save_project(&moved, &seqs, &media).unwrap();
    assert_eq!(stored_source(&moved), "../external.vtt");
    let (seqs, ..) = load_project(&moved).unwrap();
    assert_eq!(
        seqs[0]
            .tracks
            .iter()
            .find(|t| t.id == id)
            .unwrap()
            .source_file,
        Some(root.0.join("external.vtt"))
    );
}

#[test]
fn tracks_without_a_source_keep_their_project_bytes() {
    let dir = Scratch::new();
    let mut seq = Sequence::new_default();
    seq.tracks.push(Track::new("Captions", TrackKind::Subtitle));
    save_project(&dir.0, &[seq], &MediaPool::new()).unwrap();
    let stable = || {
        std::fs::read_to_string(dir.0.join("project.json"))
            .unwrap()
            .lines()
            .filter(|l| {
                !l.trim_start().starts_with("\"generation\":")
                    && !l.trim_start().starts_with("\"saved_at\":")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let before = stable();
    assert!(!before.contains("\"source_file\""));
    let (seqs, media, ..) = load_project(&dir.0).unwrap();
    assert!(seqs[0].tracks.iter().all(|t| t.source_file.is_none()));
    save_project(&dir.0, &seqs, &media).unwrap();
    assert_eq!(stable(), before);
}
