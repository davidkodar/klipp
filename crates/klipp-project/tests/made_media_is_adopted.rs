//! #730: a save copies the media Klipp made or fetched for the project
//! into the project's folder — from the kept made-media folder and from
//! the cache earlier builds wrote to — with its credit file, and points
//! the pool there. Media from anywhere else is left alone.
//!
//! Its own test binary: it points the data and cache folders at scratch
//! folders through the environment, which is process-wide.

use klipp_media::{MediaItem, MediaPool};
use std::path::PathBuf;

#[test]
fn a_save_brings_made_media_into_the_project() {
    let root = std::env::temp_dir().join(format!("klipp-made-media-{}", uuid::Uuid::new_v4()));
    let (data, cache, project, videos) = (root.join("data"), root.join("cache"), root.join("Film.klipp"), root.join("Videos"));
    // Set before anything in this process reads them; the only test here.
    std::env::set_var("XDG_DATA_HOME", &data);
    std::env::set_var("XDG_CACHE_HOME", &cache);
    let write = |p: &PathBuf, bytes: &[u8]| {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    };

    // A library track in the kept folder, with its credit; a recording an
    // earlier build left in the cache; footage of the user's own.
    let song = klipp_media::made::made_dir("audio").join("Artist-Song-1.mp3");
    assert!(song.starts_with(&data), "made media is under the data folder: {}", song.display());
    write(&song, &[1u8; 4096]);
    write(&song.with_extension("klipp-credit.txt"), b"Song by Artist, CC BY");
    let take = cache.join("klipp").join("recordings").join("take.mkv");
    write(&take, &[2u8; 8192]);
    let shot = videos.join("shot.mov");
    write(&shot, &[3u8; 100]);

    let mut pool = MediaPool::default();
    let ids: Vec<_> = [&song, &take, &shot].iter().map(|p| pool.add(MediaItem::synthetic_for_test((*p).clone(), None))).collect();
    std::fs::create_dir_all(&project).unwrap();
    let report = klipp_project::adopt_made_media(&project, &mut pool);
    assert_eq!((report.adopted, report.failed.len()), (2, 0), "{report:?}");

    let at = |i: usize| pool.get(&ids[i]).unwrap().path.clone();
    assert_eq!(at(0), project.join("media").join("audio").join("Artist-Song-1.mp3"));
    assert_eq!(at(1), project.join("media").join("recordings").join("take.mkv"));
    assert_eq!(at(2), shot, "the user's own footage is not copied");
    assert_eq!(std::fs::read(at(0)).unwrap().len(), 4096);
    assert_eq!(std::fs::read(at(1)).unwrap().len(), 8192);
    assert!(at(0).with_extension("klipp-credit.txt").is_file(), "the credit comes with it");
    assert!(song.is_file() && take.is_file(), "copied, not moved: another project may use them");

    // The cache cleared: the project still has its media.
    std::fs::remove_dir_all(&cache).unwrap();
    assert!(at(1).is_file());

    // The next save has nothing to bring.
    assert!(klipp_project::adopt_made_media(&project, &mut pool).is_empty());
    std::fs::remove_dir_all(&root).ok();
}
