//! A saved proxy shows the file's content. When the file holds what it held,
//! the proxy still counts, whatever date the file took; when it changed
//! while the project was closed, the proxy does not.

use klipp_media::{MediaItem, MediaPool};
use klipp_project::{load_project_strict, save_project};
use klipp_timeline::{Clip, Sequence};

fn opened_proxy(name: &str, change: Option<&[u8]>) -> (bool, bool) {
    let root = std::env::temp_dir().join(format!("klipp_proxy_of_changed_file_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let (media, proxy, dir) = (root.join("a.mp4"), root.join("a-proxy.mp4"), root.join("p.klipp"));
    std::fs::write(&media, b"the content the proxy shows").unwrap();
    std::fs::write(&proxy, b"a proxy").unwrap();
    let mut item = MediaItem::synthetic_for_test(media.clone(), proxy.to_str());
    item.fingerprint = klipp_media::relink::fingerprint(&media);
    let id = item.id;
    let mut pool = MediaPool::new();
    pool.add(item);
    let t = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    let mut seq = Sequence::new_default();
    seq.tracks[0].clips.push(Clip::media(id, t(0.0), t(1.0), t(0.0), t(1.0)));
    save_project(&dir, &[seq], &pool).unwrap();
    if let Some(bytes) = change {
        std::fs::write(&media, bytes).unwrap();
    }
    let (_, loaded, ..) = load_project_strict(&dir).unwrap();
    let item = loaded.get(&id).unwrap();
    let got = (item.proxy_ready, item.proxy_path.is_some());
    let _ = std::fs::remove_dir_all(&root);
    got
}

#[test]
fn a_proxy_counts_for_the_content_it_shows() {
    assert_eq!(opened_proxy("same", None), (true, true));
    assert_eq!(opened_proxy("rewritten", Some(b"the content the proxy shows")), (true, true), "a new date, the same content");
    assert_eq!(opened_proxy("changed", Some(b"something else altogether")), (false, false));
}
