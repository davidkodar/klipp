//! A project whose media is outside its folder, copied or moved, still
//! finds it (#2790): each such file is stored by its absolute path too,
//! and an open tries the relative path, then the absolute one. Media in
//! the project's own folder goes with a copy, and the copy uses its own.

use klipp_media::{MediaItem, MediaPool};
use klipp_project::{load_project, save_project};
use klipp_timeline::Sequence;

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// Measured: which files are offline after the copy opens, and where the
/// ones found are.
#[test]
fn a_copied_project_finds_media_outside_its_folder() {
    let root = std::env::temp_dir().join(format!("klipp-media-outside-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (project, copy) = (root.join("work/Film.klipp"), root.join("elsewhere/deeper/Film copy.klipp"));
    let outside = root.join("footage/shot.mov");
    let inside = project.join("media/voice.wav");
    for (file, bytes) in [(&outside, b"picture" as &[u8]), (&inside, b"voice")] {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, bytes).unwrap();
    }
    let mut pool = MediaPool::new();
    let a = pool.add(MediaItem::synthetic_for_test(outside.clone(), None));
    let b = pool.add(MediaItem::synthetic_for_test(inside.clone(), None));
    save_project(&project, &[Sequence::new_default()], &pool).unwrap();
    copy_dir(&project, &copy);

    let (_, loaded, ..) = load_project(&copy).unwrap();
    let (a, b) = (loaded.get(&a).unwrap(), loaded.get(&b).unwrap());
    assert!(!a.offline && a.path == outside, "outside the folder: found where it is ({:?}, offline {})", a.path, a.offline);
    assert!(!b.offline && b.path == copy.join("media/voice.wav"), "inside: the copy's own ({:?})", b.path);
    assert!(b.path.exists());
    // The manifest keeps the absolute path for the outside file alone.
    let manifest = std::fs::read_to_string(project.join("media/manifest.json")).unwrap();
    assert_eq!(manifest.matches("\"absolute_path\"").count(), 1, "{manifest}");
    let _ = std::fs::remove_dir_all(&root);
}
