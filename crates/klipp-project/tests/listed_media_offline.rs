//! A project's media item whose file lists other inputs (an HLS playlist
//! or a concat list, named like a video) opens offline: it never enters
//! the pool as media, and the one way Klipp hands a file to ffmpeg, on
//! the command line (a proxy, an analysis) as through its library (an
//! export, playback), refuses it.

use klipp_media::{MediaItem, MediaPool};
use klipp_project::{load_project, save_project};
use klipp_timeline::Sequence;

#[test]
fn a_listed_file_named_as_media_opens_offline() {
    let dir = std::env::temp_dir().join(format!("klipp_listed_media_{}", uuid::Uuid::new_v4()));
    let outside = dir.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("private.mp4"), b"not for this project").unwrap();
    let project = dir.join("project.klipp");
    std::fs::create_dir_all(&project).unwrap();
    let hls = dir.join("clip.mp4");
    std::fs::write(&hls, format!("#EXTM3U\n#EXTINF:1,\nfile://{}\n#EXTINF:1,\nhttp://127.0.0.1:9/x.ts\n#EXT-X-ENDLIST\n", outside.join("private.mp4").display())).unwrap();
    let concat = dir.join("other.mov");
    std::fs::write(&concat, format!("ffconcat version 1.0\nfile '{}'\n", outside.join("private.mp4").display())).unwrap();
    let mut pool = MediaPool::new();
    let ids: Vec<_> = [&hls, &concat].iter().map(|p| {
        let item = MediaItem::synthetic_for_test((*p).clone(), None);
        let id = item.id;
        pool.add(item);
        id
    }).collect();
    save_project(&project, &[Sequence::new_default()], &pool).unwrap();
    let (_, loaded, ..) = load_project(&project).unwrap();
    for id in &ids {
        let item = loaded.get(id).unwrap();
        assert!(item.offline, "{}: offline, not media", item.path.display());
        assert!(klipp_media::input_args(&item.path).is_err(), "no ffmpeg command line takes it");
        assert!(klipp_media::open_input(&item.path).is_err(), "no ffmpeg library open takes it");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
