//! The manifest, not MediaItem's own serde implementation, owns persistence.
use klipp_core::{ClipSpan, MediaId, MediaMarker, MediaMarkerKind, SyncMethod, SyncedAudio};
use klipp_media::{MediaItem, MediaPool};
use klipp_project::{load_project, save_project};
use klipp_timeline::Sequence;
use std::path::{Path, PathBuf};

const FIELDS: &[&str] = &["log", "markers", "synced_audio", "span", "screen_text", "shot", "truncated_from_secs", "dolby_vision_profile"];

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("klipp-media-state-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

fn item(dir: &Path) -> MediaItem {
    // Deliberately offline: no metadata probe can reconstruct user edits.
    MediaItem::synthetic_for_test(dir.join("unavailable.mov"), None)
}

#[test]
fn authored_state_and_metadata_survive_offline_reopen_and_resave() {
    let dir = Scratch::new();
    let mut original = item(&dir.0);
    original.log.scene = "12B".into();
    original.log.comments = "Keep this take".into();
    original.log.good_take = true;
    original.log.keywords = vec!["exterior".into(), "night".into()];
    original.markers = vec![MediaMarker::note(1.125), MediaMarker::range(MediaMarkerKind::Favourite, 2.5, 4.75)];
    original.synced_audio = Some(SyncedAudio { audio_media_id: MediaId::new(), offset_secs: -0.125, method: SyncMethod::Waveform });
    original.span = Some(ClipSpan { first: MediaId::new(), index: 2, count: 3 });
    original.screen_text = "Camera settings".into();
    original.shot.start_timecode = Some("01:02:03:04".into());
    original.shot.aperture = Some(2.8);
    original.shot.tags = vec![("video:reel".into(), "A001".into())];
    original.metadata_read = true;
    original.truncated_from_secs = Some(12.125);
    original.dolby_vision_profile = Some(5);
    let expected = serde_json::to_value(&original).unwrap();
    let mut pool = MediaPool::new();
    pool.add(original.clone());
    let mut seqs = vec![Sequence::new_default()];
    for _ in 0..2 {
        save_project(&dir.0, &seqs, &pool).unwrap();
        let (loaded_seqs, loaded_pool, ..) = load_project(&dir.0).unwrap();
        let loaded = loaded_pool.get(&original.id).unwrap();
        assert!(loaded.offline);
        assert!(!loaded.metadata_read, "available sources must still be re-probed after reopen");
        let actual = serde_json::to_value(loaded).unwrap();
        for field in FIELDS {
            assert_eq!(actual[*field], expected[*field], "{field} was lost");
        }
        seqs = loaded_seqs;
        pool = loaded_pool;
    }
    // Clearing values must remove them, rather than resurrecting a saved copy.
    let cleared = pool.get_mut(&original.id).unwrap();
    cleared.log = Default::default();
    cleared.markers.clear();
    cleared.synced_audio = None;
    cleared.span = None;
    cleared.screen_text.clear();
    cleared.shot = Default::default();
    cleared.metadata_read = false;
    cleared.truncated_from_secs = None;
    cleared.dolby_vision_profile = None;
    save_project(&dir.0, &seqs, &pool).unwrap();
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.0.join("media/manifest.json")).unwrap()).unwrap();
    for field in FIELDS { assert!(manifest["items"][0].get(*field).is_none(), "empty {field} was written"); }
    let (_, reopened, ..) = load_project(&dir.0).unwrap();
    let defaults = serde_json::to_value(item(&dir.0)).unwrap();
    let actual = serde_json::to_value(reopened.get(&original.id).unwrap()).unwrap();
    for field in FIELDS { assert_eq!(actual[*field], defaults[*field], "cleared {field} came back"); }
}

fn stable_bytes(path: &Path) -> String {
    // The save's generation and clock are expected to change. Keep every
    // other byte, including key order and float spelling, for this proof.
    std::fs::read_to_string(path).unwrap().lines()
        .filter(|line| !line.trim_start().starts_with("\"generation\":") && !line.trim_start().starts_with("\"saved_at\":"))
        .collect::<Vec<_>>().join("\n")
}

#[test]
fn a_manifest_without_the_new_fields_keeps_its_bytes() {
    let dir = Scratch::new();
    let mut pool = MediaPool::new();
    pool.add(item(&dir.0));
    save_project(&dir.0, &[Sequence::new_default()], &pool).unwrap();
    let paths = [dir.0.join("project.json"), dir.0.join("media/manifest.json")];
    let before = paths.each_ref().map(|p| stable_bytes(p));
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(&paths[1]).unwrap()).unwrap();
    for field in FIELDS { assert!(manifest["items"][0].get(*field).is_none()); }
    let (seqs, pool, ..) = load_project(&dir.0).unwrap();
    save_project(&dir.0, &seqs, &pool).unwrap();
    assert_eq!(paths.each_ref().map(|p| stable_bytes(p)), before);
}
