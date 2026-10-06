use klipp_core::{Fps, MediaId, RationalTime};
use klipp_media::{MediaItem, MediaItemKind, MediaPool};
use klipp_project::{load_project, save_project, save_project_full, ColorSettings};
use klipp_timeline::{Clip, ClipKind, Sequence, TitleAsset, TitleData};
use std::collections::HashMap;
use std::path::PathBuf;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("klipp-media-kind-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn check_title(legacy_manifest: bool) {
    let dir = Scratch::new();
    let id = MediaId::new();
    let media = MediaItem::new_synthetic_title(
        id,
        "Opening title".into(),
        RationalTime::from_frames(48, 24),
        Fps::film_24(),
    );
    let sentinel = media.path.clone();
    let mut pool = MediaPool::new();
    pool.add(media);
    let data = TitleData::default();
    let mut clip = Clip::gap(
        RationalTime::from_frames(0, 24),
        RationalTime::from_frames(48, 24),
    );
    clip.kind = ClipKind::Title(data.clone());
    clip.linked_title_asset_id = Some(id);
    let clip_id = clip.id;
    let mut seq = Sequence::new_default();
    seq.tracks[0].clips.push(clip);
    let titles = HashMap::from([(
        id,
        TitleAsset {
            display_name: "Opening title".into(),
            data,
            animation: Some("fade_in_0.5".into()),
        },
    )]);
    save_project_full(
        &dir.0,
        &[seq],
        &pool,
        &ColorSettings::default(),
        None,
        None,
        "",
        0,
        None,
        None,
        None,
        &titles,
    )
    .unwrap();
    if legacy_manifest {
        let path = dir.0.join("media/manifest.json");
        let mut json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        json["items"][0].as_object_mut().unwrap().remove("kind");
        std::fs::write(path, serde_json::to_vec_pretty(&json).unwrap()).unwrap();
    }
    let (seqs, loaded, _, _, _, _, _, _, _, assets) = load_project(&dir.0).unwrap();
    let item = loaded.get(&id).unwrap();
    assert_eq!(item.kind, MediaItemKind::SyntheticTitle);
    assert!(!item.offline, "a title asset has no file to be missing");
    assert_eq!(item.path, sentinel);
    assert_eq!(assets[&id].animation, titles[&id].animation);
    assert_eq!(assets[&id].display_name, titles[&id].display_name);
    let clip = seqs[0]
        .tracks
        .iter()
        .flat_map(|t| &t.clips)
        .find(|c| c.id == clip_id)
        .unwrap();
    assert_eq!(clip.linked_title_asset_id, Some(id));
    assert!(matches!(clip.kind, ClipKind::Title(_)));
}

#[test]
fn linked_title_keeps_its_kind_asset_and_link() {
    check_title(false);
}

#[test]
fn an_existing_title_manifest_without_kind_uses_the_asset_id() {
    check_title(true);
}

#[test]
fn ordinary_media_omits_kind_and_keeps_manifest_bytes() {
    let dir = Scratch::new();
    let mut pool = MediaPool::new();
    let file = MediaItem::synthetic_for_test(dir.0.join("missing.mov"), None);
    let id = file.id;
    pool.add(file);
    let path = dir.0.join("media/manifest.json");
    let stable = || {
        std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .filter(|l| !l.trim_start().starts_with("\"generation\":"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    save_project(&dir.0, &[Sequence::new_default()], &pool).unwrap();
    let before = stable();
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(json["items"][0].get("kind").is_none());
    let (seqs, loaded, ..) = load_project(&dir.0).unwrap();
    assert_eq!(loaded.get(&id).unwrap().kind, MediaItemKind::File);
    assert!(loaded.get(&id).unwrap().offline);
    save_project(&dir.0, &seqs, &loaded).unwrap();
    assert_eq!(stable(), before);
}
