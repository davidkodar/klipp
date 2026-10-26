//! Measured sweep of the round trips: a sequence built by thirty-odd ops
//! of every sort, then the newest state written in (a tracked surface with
//! its track area, a left-out region and a correction, a measured Clip
//! Normalize), saved and loaded back, saved again; and every op undone,
//! one by one, then redone. Each must give back exactly the state it
//! should, ids and all. Report: `target/sweep/round_trips.txt`.

mod sweep_common;

use klipp_document::apply::DocumentView;
use klipp_document::ops::{self, Op};
use klipp_document::history::EditCommand;
use klipp_testing::sweep::Sweep;
use klipp_timeline::Sequence;
use serde_json::{json, Value};
use sweep_common::*;

/// Items known to fail, and the issue that holds each; 0: found by the
/// sweep, not filed yet.
const KNOWN: &[(&str, u32)] = &[("undo 21 generator", 0), ("redo 14 title", 0)];

/// The sequence as it is stored, to compare by.
fn state(seq: &Sequence) -> Value {
    serde_json::to_value(seq).unwrap()
}

/// `project.json` without the lines a save stamps anew each time.
fn written(dir: &std::path::Path) -> String {
    let text = std::fs::read_to_string(dir.join("project.json")).unwrap_or_default();
    text.lines().filter(|l| !["\"generation\"", "\"saved_at\"", "\"klipp_version\"", "\"platform\""].iter().any(|k| l.trim_start().starts_with(k))).collect::<Vec<_>>().join("\n")
}

#[test]
fn sweep_round_trips() {
    let mut sweep = Sweep::new("round_trips", KNOWN);
    let (seq, mut pool) = fixture();
    let clip = seq.tracks[0].clips[0].id.0.to_string();
    let mut fps = seq.fps;
    let mut sequences = vec![seq];
    // Each op one undo step, as the app applies them: its edits, and the
    // state before it.
    let mut steps: Vec<(String, Vec<EditCommand>, Value)> = Vec::new();
    let mut title = String::new();
    let ops: Vec<Value> = vec![
        json!({"op": "grade", "clip_id": clip, "exposure": 0.3, "temperature": 10.0}),
        json!({"op": "zones", "clip_id": clip, "zone": "highlight", "exposure": -0.4}),
        json!({"op": "qualify", "clip_id": clip, "target": "foliage", "hue_shift": 20.0}),
        json!({"op": "effect", "clip_id": clip, "kind": "vignette", "amount": 0.4}),
        json!({"op": "effect", "clip_id": clip, "kind": "displace", "params": {"amount": 8, "size": 160, "direction": "across"}}),
        json!({"op": "compose", "clip_id": clip, "poses": [{"secs": 0, "x": 0.4}, {"secs": 2, "x": 0.6, "width": 0.8, "ease": "linear"}]}),
        json!({"op": "crop", "clip_id": clip, "left": 0.05}),
        json!({"op": "label", "clip_id": clip, "name": "Harbour", "colour": "blue"}),
        json!({"op": "audio", "clip_id": clip, "volume_db": -3.0, "fade_out": 0.5}),
        json!({"op": "volume", "clip_id": clip, "keys": [{"at": 1.0, "db": 0.0}, {"at": 2.0, "db": -9.0}]}),
        json!({"op": "clip_audio", "clip_id": clip, "effect": "reverb", "params": {"room_size": 0.7}}),
        json!({"op": "clip_audio", "clip_id": clip, "effect": "delay", "params": {"delay_time": 300, "feedback": 0.5}}),
        json!({"op": "clip_audio", "clip_id": clip, "effect": "eq", "params": {"band2_gain": 4, "band3_enabled": false}}),
        json!({"op": "title", "text": "Gold", "at": {"secs": 0.5}, "secs": 2.5}),
        json!({"op": "style", "clip_id": "TITLE", "material": "gold", "three_d": {"look": "bevelled", "keys": {"turn": [{"at": 0, "value": -30}, {"at": 2, "value": 30}]}}}),
        json!({"op": "effect", "clip_id": "TITLE", "kind": "reflection", "params": {"waterline": 0.62, "tint": "#1d3b4f"}}),
        json!({"op": "effect", "clip_id": "TITLE", "kind": "glow", "amount": 0.3}),
        json!({"op": "set_text", "clip_id": "TITLE", "text": "Golden"}),
        json!({"op": "split", "clip_id": clip, "at": {"secs": 3.0}}),
        json!({"op": "transition", "clip_id": "FIRST", "kind": "cross_dissolve", "secs": 0.5}),
        json!({"op": "generator", "data": {"SolidColor": {"color": [16, 24, 32, 255]}}, "at": {"secs": 0}, "secs": 4}),
        json!({"op": "add_track", "kind": "audio", "name": "Music"}),
        json!({"op": "camera", "keys": [{"secs": 0}, {"secs": 3, "zoom": 1.1}]}),
    ];
    for op in ops {
        let mut op = op;
        if op.get("clip_id") == Some(&json!("TITLE")) {
            op["clip_id"] = json!(title);
        }
        // The picture clip that starts the timeline, as it is now.
        if op.get("clip_id") == Some(&json!("FIRST")) {
            let first = sequences[0].tracks.iter().flat_map(|t| &t.clips).filter(|c| c.media_id().is_some()).min_by_key(|c| c.timeline_in);
            op["clip_id"] = json!(first.map(|c| c.id.0.to_string()).unwrap_or_default());
        }
        let name = format!("{} {}", steps.len() + 1, op["op"].as_str().unwrap_or("?"));
        let parsed: Result<Vec<Op>, _> = serde_json::from_value(json!([op]));
        let before = state(&sequences[0]);
        let plan = match parsed.map_err(|e| e.to_string()).and_then(|o| ops::resolve(&sequences[0], &pool, &o).map_err(|r| r.why)) {
            Ok(plan) => plan,
            Err(why) => {
                sweep.check(&format!("{name} applies"), format!("refused: {why}"), false);
                continue;
            }
        };
        let edits: Vec<EditCommand> = plan.steps.iter().map(|s| s.edit.clone()).collect();
        let mut doc = DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps };
        for e in &edits {
            doc.apply_forward(e);
        }
        if op["op"] == "title" {
            title = sequences[0].tracks.iter().flat_map(|t| &t.clips).find(|c| c.title_data().is_some()).map(|c| c.id.0.to_string()).unwrap_or_default();
        }
        steps.push((name, edits, before));
    }
    let end = state(&sequences[0]);

    // Every op undone, last first: each gives back the state before it.
    // One that does not is reported, and the state set right, so each op
    // is judged on its own.
    for (name, edits, before) in steps.iter().rev() {
        let mut doc = DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps };
        for e in edits.iter().rev() {
            doc.apply_inverse(e);
        }
        let now = state(&sequences[0]);
        sweep.check(&format!("undo {name}"), if now == *before { "as before it".into() } else { first_difference(before, &now) }, now == *before);
        if now != *before {
            sequences[0] = serde_json::from_value(before.clone()).unwrap();
        }
    }
    // And redone, first first: each gives the state after it, ids and all.
    let afters: Vec<Value> = steps.iter().skip(1).map(|(_, _, before)| before.clone()).chain([end.clone()]).collect();
    for ((name, edits, _), after) in steps.iter().zip(&afters) {
        let mut doc = DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps };
        for e in edits {
            doc.apply_forward(e);
        }
        let now = state(&sequences[0]);
        sweep.check(&format!("redo {name}"), if now == *after { "as after it".into() } else { first_difference(after, &now) }, now == *after);
        if now != *after {
            sequences[0] = serde_json::from_value(after.clone()).unwrap();
        }
    }

    // The newest state, written in: a tracked surface with its track area,
    // a region left out and a correction by hand; a measured Clip Normalize.
    let host = &mut sequences[0].tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).find(|c| c.media_id().is_some()).expect("a picture clip");
    let quad = [[0.3, 0.3], [0.6, 0.32], [0.62, 0.6], [0.28, 0.58]];
    let moved = [[0.31, 0.3], [0.61, 0.33], [0.63, 0.61], [0.29, 0.58]];
    host.surfaces.push(klipp_timeline::Surface {
        id: uuid::Uuid::new_v4(),
        name: "Sign".into(),
        corners: quad,
        reference_secs: 1.0,
        keys: vec![
            klipp_timeline::SurfaceKey::at_corners(1.0, &quad, &quad).unwrap(),
            klipp_timeline::SurfaceKey { confidence: Some(0.3), state: klipp_timeline::TrackState::Low, ..klipp_timeline::SurfaceKey::at_corners(1.04, &quad, &moved).unwrap() },
            klipp_timeline::SurfaceKey { state: klipp_timeline::TrackState::Manual, ..klipp_timeline::SurfaceKey::at_corners(1.08, &quad, &moved).unwrap() },
        ],
        source_size: [1920, 1080],
        media_fps: 25.0,
        track_area: Some([[0.2, 0.2], [0.7, 0.22], [0.72, 0.7], [0.18, 0.68]]),
        exclude: vec![vec![[0.4, 0.4], [0.5, 0.4], [0.45, 0.5]]],
    });
    host.audio_effects.push(klipp_core::AudioEffectInstance {
        effect_type: "normalize".into(),
        enabled: true,
        wet_dry: 1.0,
        params: vec![("target_lufs".into(), -14.0), ("mode".into(), 1.0), ("measured_lufs".into(), -22.5), ("measured_key_lo".into(), 1234.0), ("measured_key_hi".into(), 5678.0)],
    });
    let built = state(&sequences[0]);

    // Saved and loaded back: the same; saved again: the same file.
    let dir = klipp_testing::scratch::scratch_dir("klipp-sweep-round-trips");
    match klipp_project::save_project(&dir, &sequences, &pool) {
        Err(why) => sweep.check("save", format!("refused: {why}"), false),
        Ok(()) => {
            let first = written(&dir);
            match klipp_project::load_project(&dir) {
                Err(why) => sweep.check("load", format!("refused: {why}"), false),
                Ok((loaded, loaded_pool, ..)) => {
                    let back = state(&loaded[0]);
                    sweep.check("save and load", if back == built { "identical".into() } else { first_difference(&built, &back) }, back == built);
                    let again = dir.join("again");
                    let _ = std::fs::create_dir_all(&again);
                    let same = klipp_project::save_project(&again, &loaded, &loaded_pool).is_ok() && written(&again) == first;
                    sweep.check("save, load, save", if same { "project.json identical" } else { "project.json differs" }, same);
                }
            }
        }
    }

    // A project whose media sits outside its folder, copied elsewhere,
    // still finds it.
    let root = klipp_testing::scratch::scratch_dir("klipp-sweep-media-paths");
    let (media_dir, project, copy) = (root.join("media"), root.join("work/Film.klipp"), root.join("elsewhere/deeper/Film.klipp"));
    let _ = std::fs::create_dir_all(&media_dir);
    let file = media_dir.join("shot.mov");
    let _ = std::fs::write(&file, b"not decoded here");
    let (seq, mut pool) = fixture();
    let id = seq.tracks[0].clips[0].media_id().expect("a media clip");
    if let Some(item) = pool.get_mut(&id) {
        item.path = file.clone();
    }
    let _ = std::fs::create_dir_all(&project);
    let found = klipp_project::save_project(&project, std::slice::from_ref(&seq), &pool).is_ok()
        && copy_dir(&project, &copy).is_ok()
        && klipp_project::load_project(&copy).ok().and_then(|(_, pool, ..)| pool.get(&id).map(|i| i.path.exists())).unwrap_or(false);
    sweep.check("a copied project finds media outside it", if found { "found" } else { "offline" }, found);
    sweep.finish();
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// Where two states first differ, as a JSON path and both values.
fn first_difference(want: &Value, got: &Value) -> String {
    fn walk(path: String, a: &Value, b: &Value) -> Option<String> {
        match (a, b) {
            (Value::Object(x), Value::Object(y)) => {
                for k in x.keys().chain(y.keys()) {
                    let (va, vb) = (x.get(k).unwrap_or(&Value::Null), y.get(k).unwrap_or(&Value::Null));
                    if va != vb {
                        return walk(format!("{path}/{k}"), va, vb);
                    }
                }
                None
            }
            (Value::Array(x), Value::Array(y)) if x.len() == y.len() => x.iter().zip(y).enumerate().find(|(_, (p, q))| p != q).and_then(|(i, (p, q))| walk(format!("{path}/{i}"), p, q)),
            _ => {
                let short = |v: &Value| v.to_string().chars().take(60).collect::<String>();
                Some(format!("{path}: {} != {}", short(a), short(b)))
            }
        }
    }
    walk(String::new(), want, got).unwrap_or_else(|| "differs".into())
}
