//! Media paths survive restoring an auto-save and saving it as the project.
//!
//! The bug this pins: loading from `X_autosave.klipp` left each media path
//! as `…/X_autosave.klipp/../../Videos/a.mov`, and saving that to `X.klipp`
//! stored `../X_autosave.klipp/../../Videos/a.mov`. Once the auto-save
//! folder was deleted the path no longer resolved and every item was
//! offline.

use std::path::{Path, PathBuf};

use klipp_media::{MediaItem, MediaPool};
use klipp_project::{load_project, save_project_with_color, ColorSettings};
use klipp_timeline::Sequence;

fn root(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("klipp_autosave_paths_{label}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("Videos")).unwrap();
    std::fs::create_dir_all(dir.join("Projects")).unwrap();
    dir
}

fn save(dir: &Path, pool: &MediaPool) {
    std::fs::create_dir_all(dir).unwrap();
    save_project_with_color(dir, &[Sequence::new_default()], pool, &ColorSettings::default(), None, None, "")
        .unwrap();
}

fn only_item(dir: &Path) -> MediaItem {
    let (_seqs, pool, ..) = load_project(dir).unwrap();
    let items: Vec<MediaItem> = pool.all().cloned().collect();
    items.into_iter().next().expect("media item survives")
}

#[test]
fn restoring_an_autosave_then_saving_keeps_media_online() {
    let root = root("restore");
    let media = root.join("Videos").join("a.mov");
    std::fs::write(&media, b"not really video").unwrap();
    let autosave = root.join("Projects").join("X_autosave.klipp");
    let project = root.join("Projects").join("X.klipp");

    let mut pool = MediaPool::new();
    pool.add(MediaItem::synthetic_for_test(media.clone(), None));
    save(&autosave, &pool);

    // Restore: load the auto-save, save it as the project, and the
    // auto-save folder goes.
    let (_seqs, restored, ..) = load_project(&autosave).unwrap();
    save(&project, &restored);
    std::fs::remove_dir_all(&autosave).unwrap();

    let item = only_item(&project);
    assert_eq!(item.path, media);
    assert!(!item.offline, "media must resolve after the auto-save is gone");
    let manifest = std::fs::read_to_string(project.join("media").join("manifest.json")).unwrap();
    assert!(!manifest.contains("X_autosave.klipp"), "stored path must not go through the auto-save folder: {manifest}");
    let _ = std::fs::remove_dir_all(&root);
}

/// A project already saved with a path through the deleted auto-save
/// folder opens with its media online.
#[test]
fn a_path_through_a_deleted_autosave_folder_still_resolves() {
    let root = root("legacy");
    let media = root.join("Videos").join("a.mov");
    std::fs::write(&media, b"not really video").unwrap();
    let project = root.join("Projects").join("X.klipp");

    let mut pool = MediaPool::new();
    pool.add(MediaItem::synthetic_for_test(media.clone(), None));
    save(&project, &pool);
    let manifest_path = project.join("media").join("manifest.json");
    let manifest = std::fs::read_to_string(&manifest_path).unwrap();
    assert!(manifest.contains("\"../../Videos/a.mov\""), "{manifest}");
    std::fs::write(
        &manifest_path,
        manifest.replace("\"../../Videos/a.mov\"", "\"../X_autosave.klipp/../../Videos/a.mov\""),
    )
    .unwrap();

    let item = only_item(&project);
    assert_eq!(item.path, media);
    assert!(!item.offline);
    let _ = std::fs::remove_dir_all(&root);
}
