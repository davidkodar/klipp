//! Projects built by the current writer; never hand-maintained project JSON.
use std::collections::BTreeMap;
use std::path::Path;

use klipp_core::{ClipId, Fps, MediaId, Resolution, SequenceId, Timecode, TrackId};
use klipp_effects::BuiltinEffect;
use klipp_graph_lift::ops::GraphOp;
use klipp_media::{MediaItem, MediaItemKind, MediaPool};
use klipp_timeline::{Clip, EffectEdit, Sequence};
use serde_json::Value;
use uuid::Uuid;

pub const CASES: &[&str] = &["edit", "effects"];

fn id(label: &str) -> Uuid {
    Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("klipp-corpus/v1/{label}").as_bytes(),
    )
}

fn tc(frames: i64) -> Timecode {
    Timecode::from_frames(frames, 24)
}

pub fn generate(root: &Path) -> Result<BTreeMap<String, String>, String> {
    let mut hashes = BTreeMap::new();
    for case in CASES {
        let mut pool = MediaPool::default();
        for (name, audio) in [("motion.mkv", false), ("sync.wav", true)] {
            pool.add(MediaItem {
                path: root.join("media").join(name),
                kind: MediaItemKind::File,
                resolution: Resolution {
                    // Audio uses the model's placeholder dimensions. Zero
                    // dimensions are normalized by the project loader.
                    width: if audio { 1920 } else { 640 },
                    height: if audio { 1080 } else { 360 },
                },
                video_codec: if audio { "" } else { "ffv1" }.into(),
                audio_codec: if audio { "pcm_s16le" } else { "" }.into(),
                has_audio: audio,
                sample_rate: audio.then_some(48_000),
                audio_channels: audio.then_some(2),
                ..MediaItem::new_synthetic_title(
                    MediaId(id(name)),
                    name.into(),
                    tc(48),
                    Fps::film_24(),
                )
            });
        }
        let mut seq = Sequence::new_default();
        seq.id = SequenceId(id(&format!("{case}/sequence")));
        seq.name = format!("Corpus: {case}");
        seq.resolution = Resolution {
            width: 640,
            height: 360,
        };
        for (index, track) in seq.tracks.iter_mut().enumerate() {
            track.id = TrackId(id(&format!("{case}/track/{index}")));
        }
        // A cut at frame 24 with a source discontinuity; the audio crosses it.
        for (index, (start, length, source)) in [(0, 24, 0), (24, 24, 12)].into_iter().enumerate() {
            let mut clip = Clip::media(
                MediaId(id("motion.mkv")),
                tc(start),
                tc(start + length),
                tc(source),
                tc(source + length),
            );
            clip.id = ClipId(id(&format!("{case}/video/{index}")));
            seq.tracks[0].clips.push(clip);
        }
        let mut audio = Clip::media(MediaId(id("sync.wav")), tc(0), tc(48), tc(0), tc(48));
        audio.id = ClipId(id(&format!("{case}/audio")));
        seq.tracks[1].clips.push(audio);
        // Canonicalize the model before the writer derives its graph IDs.
        // Renaming a stored picture after saving would invalidate those IDs.
        let mut model = serde_json::to_value(&seq).map_err(|e| e.to_string())?;
        let mut ids = BTreeMap::new();
        definitions(&model, &mut ids, case);
        replace(&mut model, &ids);
        let mut seq: Sequence = serde_json::from_value(model).map_err(|e| e.to_string())?;
        // Apply graph edits only after canonicalizing the blank model: a clip's
        // own serde is its field view, so it must not round-trip picture truth.
        for (index, clip) in seq.tracks[0].clips.iter_mut().enumerate() {
            if *case == "effects" {
                for (edit_index, edit) in [
                    EffectEdit::Add(&BuiltinEffect::GaussianBlur {
                        radius: 2.0 + index as f32,
                    }),
                    EffectEdit::Add(&BuiltinEffect::Sharpen { amount: 0.7 }),
                    EffectEdit::SetEnabled(1, index == 0),
                ]
                .into_iter()
                .enumerate()
                {
                    let mut ops = clip
                        .effect_edit_ops(edit)
                        .ok_or("corpus effects did not accept picture ops")?;
                    for op in &mut ops {
                        if let GraphOp::AddNode { node, .. } = op {
                            node.id = id(&format!("{case}/video/{index}/effect/{edit_index}"));
                        }
                    }
                    clip.apply_picture_ops(&ops)?;
                }
            }
        }
        let directory = root.join("projects").join(format!("{case}.klipp"));
        klipp_project::save_project(&directory, &[seq], &pool)?;
        canonicalize(&directory, case)?;
        let files = ["project.json", "media/manifest.json"];
        let before: Vec<_> = files
            .iter()
            .map(|name| std::fs::read(directory.join(name)))
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        let (loaded, loaded_pool, ..) = klipp_project::load_project_strict(&directory)
            .map_err(|e| format!("{case}: generated project does not load: {e}"))?;
        if loaded.len() != 1
            || loaded[0].tracks.len() != 2
            || loaded[0].tracks[0].clips.len() != 2
            || loaded[0].tracks[1].clips.len() != 1
            || loaded[0].total_duration() != tc(48)
            || loaded_pool.all().count() != 2
        {
            return Err(format!("{case}: generated edit structure changed"));
        }
        // The project-only unit test intentionally creates no media; full
        // generation must resolve both real files in its relocated directory.
        if root.join("media").is_dir() {
            for item in loaded_pool.all() {
                if item.offline || !item.path.is_file() {
                    return Err(format!("{case}: {} did not resolve", item.filename));
                }
            }
        }
        for (relative, original) in files.into_iter().zip(before) {
            let bytes = std::fs::read(directory.join(relative)).map_err(|e| e.to_string())?;
            if bytes != original {
                return Err(format!("{case}/{relative}: strict load changed the file"));
            }
            hashes.insert(format!("{case}/{relative}"), crate::sha256(&bytes));
        }
    }
    Ok(hashes)
}

// Give auto-created graph IDs stable identities in traversal order. Definitions
// precede references, including keys of position maps; random UUID ordering must
// never choose an identity. Explicit sequence/media/clip IDs are stable already.
fn definitions(value: &Value, ids: &mut BTreeMap<String, String>, case: &str) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(old)) = map.get("id") {
                if Uuid::parse_str(old).is_ok() && !ids.contains_key(old) {
                    ids.insert(
                        old.clone(),
                        id(&format!("{case}/object/{}", ids.len())).to_string(),
                    );
                }
            }
            for child in map.values() {
                definitions(child, ids, case);
            }
        }
        Value::Array(items) => {
            for child in items {
                definitions(child, ids, case);
            }
        }
        _ => {}
    }
}

fn replace(value: &mut Value, ids: &BTreeMap<String, String>) {
    match value {
        Value::String(s) => {
            if let Some(new) = ids.get(s) {
                *s = new.clone();
            }
        }
        Value::Array(items) => {
            for child in items {
                replace(child, ids);
            }
        }
        Value::Object(map) => {
            let old = std::mem::take(map);
            for (key, mut child) in old {
                replace(&mut child, ids);
                map.insert(ids.get(&key).cloned().unwrap_or(key), child);
            }
        }
        _ => {}
    }
}

fn canonicalize(directory: &Path, case: &str) -> Result<(), String> {
    let read = |name: &str| -> Result<Value, String> {
        serde_json::from_slice(&std::fs::read(directory.join(name)).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())
    };
    let mut project = read("project.json")?;
    let mut manifest = read("media/manifest.json")?;
    let items = manifest["items"]
        .as_array_mut()
        .ok_or("manifest items missing")?;
    items.sort_by_key(|v| v["filename"].as_str().unwrap_or("").to_owned());
    for item in items {
        let filename = item["filename"].as_str().ok_or("media filename missing")?.to_owned();
        item["path"] = Value::String(format!("../../media/{filename}"));
        // The media is the corpus's own, beside the projects: found by its
        // relative path, never by where this run's folder is, which differs
        // on every run (#3175). Its fingerprint as a load takes it from the
        // file, so opening it and saving writes the same.
        // Its disk cache key too: this machine's, from the file's path and
        // modified time.
        if let Some(map) = item.as_object_mut() {
            map.remove("absolute_path");
            map.remove("cache_key");
        }
        let media = directory.join("../../media").join(&filename);
        item["fingerprint"] = klipp_media::relink::fingerprint(&media).map_or(Value::Null, Value::from);
    }
    for value in [&mut project, &mut manifest] {
        value["generation"] = Value::String(id(&format!("{case}/generation")).to_string());
    }
    project["saved_at"] = Value::String("2000-01-01T00:00:00Z".into());
    project["created_at"] = Value::String("2000-01-01T00:00:00Z".into());
    project["klipp_version"] = Value::String("corpus".into());
    project["platform"] = Value::String("corpus".into());
    for (name, value) in [("project.json", project), ("media/manifest.json", manifest)] {
        let mut text = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
        text.push('\n');
        std::fs::write(directory.join(name), text).map_err(|e| e.to_string())?;
    }
    Ok(())
}
