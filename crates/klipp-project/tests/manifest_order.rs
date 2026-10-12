//! The media manifest lists its items in a stable order, by id: the pool
//! is a hash map whose order changes from run to run, and an unchanged
//! project should save the same (#1354).

use klipp_media::{MediaItem, MediaPool};
use klipp_project::save_project;
use klipp_timeline::Sequence;

#[test]
fn the_manifest_lists_media_by_id() {
    let dir = std::env::temp_dir().join(format!("klipp_manifest_order_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut pool = MediaPool::new();
    for i in 0..20 {
        pool.add(MediaItem::synthetic_for_test(dir.join(format!("clip{i}.mov")), None));
    }
    save_project(&dir, &[Sequence::new_default()], &pool).unwrap();
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("media/manifest.json")).unwrap()).unwrap();
    let ids: Vec<&str> = manifest["items"].as_array().unwrap().iter().map(|i| i["id"].as_str().unwrap()).collect();
    assert_eq!(ids.len(), 20);
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted, "the manifest's items by id");
    let _ = std::fs::remove_dir_all(&dir);
}
