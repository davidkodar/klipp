//! The oracle's generated data (#1061): v1 clip pictures from a seeded
//! generator that reaches the odd cases the conversion must carry, each
//! one a picture today's reader accepts.
//!
//! What it varies, besides the ordinary: a grade graph's nodes with
//! `params` and `keyframes` present or absent, duplicate keyframe tracks,
//! input links with interior and trailing `None`s, edges to ports a kind
//! lacks, two edges into one port, edges to nodes that are not there and
//! an output that names nothing, and a node id used twice; effect lists
//! with flags too short or too long; keyframe and modifier keys that name
//! a slot, name none, or do not parse; audio modifiers beside picture
//! ones; clip keyframe fields absent, empty or full; and every container
//! absent, `null` or empty in the forms the reader allows.
//!
//! Over it, the two properties that prove the conversion loses nothing:
//! `to_v1(to_v2(x)) == canon(x)`, and `to_v2(to_v1(g)) == g` over what
//! `to_v2` makes, for clips, tracks, timeline grades, group stages and
//! grade versions.

use klipp_convert::canon::{canon, Store};
use klipp_convert::verify::{check_clip, check_grade, check_store, check_track, conv_look, conv_preset, conv_still, graph_diff, look, still};
use klipp_convert::{to_v1, to_v2};
use serde_json::{json, Map, Value};

/// A small seeded generator: the same seed, the same pictures.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn chance(&mut self, in_100: u64) -> bool {
        self.below(100) < in_100
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len() as u64) as usize]
    }
}

const KINDS: &[&str] = &["Source", "Output", "Corrector", "Exposure", "Saturation", "Qualifier", "Mask", "Layer", "Blend", "ApplyMatte", "LayerMixer", "RGBSplitter", "RGBCombiner", "ColorMatch", "MaskCutout", "Lut3D", "Reroute"];

fn uuid(r: &mut Rng) -> String {
    format!("00000000-0000-4000-8000-{:012x}", r.below(1 << 40))
}

fn keyframes(r: &mut Rng) -> Value {
    let n = r.below(3);
    Value::Array((0..n).map(|i| json!({ "time_secs": i as f64 * 0.5, "value": r.below(100) as f32 / 10.0, "interpolation": *r.pick(&["Linear", "Step", "Bezier"]) })).collect())
}

fn modifier_stack() -> Value {
    json!({ "modifiers": [] })
}

/// A v1 grade graph, odd shapes and all.
fn grade_graph(r: &mut Rng) -> Value {
    let n = r.below(6) as usize;
    let mut ids: Vec<String> = (0..n).map(|_| uuid(r)).collect();
    if n > 1 && r.chance(10) {
        ids[1] = ids[0].clone(); // a node id used twice
    }
    let nodes: Vec<Value> = ids
        .iter()
        .map(|id| {
            let mut node = json!({ "id": id, "kind": *r.pick(KINDS), "label": if r.chance(30) { "named" } else { "" }, "enabled": r.chance(80) });
            if r.chance(70) {
                node["params"] = if r.chance(50) { json!({}) } else { json!({ "exposure": 0.5, "saturation": null }) };
            }
            if r.chance(70) {
                let tracks: Vec<Value> = (0..r.below(3)).map(|_| json!({ "param_name": "exposure", "keyframes": keyframes(r) })).collect();
                node["keyframes"] = json!({ "tracks": tracks });
            }
            node["output_link"] = if r.chance(20) { json!("bus") } else { Value::Null };
            let links: Vec<Value> = (0..r.below(4)).map(|_| if r.chance(50) { Value::Null } else { json!("bus") }).collect();
            node["input_links"] = Value::Array(links);
            node
        })
        .collect();
    let mut edges = Vec::new();
    for _ in 0..r.below(6) {
        let pick_id = |r: &mut Rng| if ids.is_empty() || r.chance(10) { uuid(r) } else { ids[r.below(ids.len() as u64) as usize].clone() };
        let (from, to) = (pick_id(r), pick_id(r));
        let e = json!({ "from_node": from, "from_port": r.below(3), "to_node": to, "to_port": r.below(9) });
        if r.chance(15) {
            edges.push(e.clone()); // two edges into one port
        }
        edges.push(e);
    }
    let output = if ids.is_empty() || r.chance(10) { uuid(r) } else { ids[0].clone() };
    let mut g = json!({ "nodes": nodes, "edges": edges, "output_node_id": output });
    g["node_positions"] = if let (true, Some(id)) = (r.chance(50), ids.first()) { json!({ id: { "x": 1.0, "y": 2.0 } }) } else { json!({}) };
    g
}

/// The built-in effects the registry describes, as v1 saves them: those
/// a v1 file can hold (types made since, #1746, never were in one).
fn effect_payloads() -> Vec<Value> {
    klipp_effects::registry::effect_types()
        .filter(|t| in_v1(t))
        .map(|t| {
            let mut v = serde_json::to_value((t.sample)()).expect("an effect serialises");
            // A v1 Speed Ramp carries the fields picture revision 4 drops
            // (#1409), which the frozen reader requires.
            if v["type"] == "speed_ramp" {
                for (k, x) in [("ramp_curve", json!(0)), ("ramp_start", json!(0.0)), ("ramp_end", json!(1.0)), ("reverse", json!(false)), ("freeze_at", json!(0.0)), ("freeze", json!(false))] {
                    v[k] = x;
                }
            }
            v
        })
        .collect()
}

/// One v1 clip picture.
fn clip(r: &mut Rng, payloads: &[Value]) -> Value {
    let mut c = json!({ "id": uuid(r), "name": "generated" });
    // node_graph: absent, null or a graph.
    match r.below(3) {
        0 => {}
        1 => c["node_graph"] = Value::Null,
        _ => c["node_graph"] = grade_graph(r),
    }
    // OFX entries: a list, possibly empty, or absent (null is refused).
    if r.chance(70) {
        let n = r.below(3);
        c["effects"] = Value::Array((0..n).map(|i| json!({ "plugin_name": format!("org.example.P{i}"), "enabled": r.chance(70), "param_values": {} })).collect());
    }
    // Built-in effects: absent, null, empty or some.
    let n_fx = match r.below(4) {
        0 => None,
        1 => {
            c["builtin_effects"] = Value::Null;
            None
        }
        2 => {
            c["builtin_effects"] = json!([]);
            Some(0)
        }
        _ => {
            let n = 1 + r.below(4) as usize;
            c["builtin_effects"] = Value::Array((0..n).map(|_| r.pick(payloads).clone()).collect());
            Some(n)
        }
    };
    let slots = n_fx.unwrap_or(0);
    if r.chance(60) {
        let flags = r.below(slots as u64 + 3);
        c["builtin_effects_enabled"] = Value::Array((0..flags).map(|_| Value::Bool(r.chance(70))).collect());
    }
    // Effect keyframes: valid, out of range, unparseable; absent, null or empty.
    match r.below(4) {
        0 => {}
        1 => c["effect_keyframes"] = Value::Null,
        _ => {
            let mut keys = serde_json::Map::new();
            for _ in 0..r.below(4) {
                let k = match r.below(3) {
                    0 if slots > 0 => format!("{}:value", r.below(slots as u64)),
                    1 => format!("{}:radius", slots + 3),
                    _ => "not a key".to_string(),
                };
                keys.insert(k, keyframes(r));
            }
            c["effect_keyframes"] = Value::Object(keys);
        }
    }
    // Modifiers: picture, audio and effect keys, valid or not.
    match r.below(4) {
        0 => {}
        1 => c["param_modifiers"] = Value::Null,
        _ => {
            let mut mods = serde_json::Map::new();
            for _ in 0..r.below(4) {
                let k = match r.below(4) {
                    0 => r.pick(&["opacity", "position_x", "scale", "rotation"]).to_string(),
                    1 => r.pick(&["volume", "pan", "gain"]).to_string(),
                    2 if slots > 0 => format!("effect:{}:value", r.below(slots as u64)),
                    _ => format!("effect:{}:radius", slots + 5),
                };
                mods.insert(k, modifier_stack());
            }
            c["param_modifiers"] = Value::Object(mods);
        }
    }
    for f in ["opacity_keyframes", "transform_pos_x_keyframes", "transform_scale_keyframes", "transform_tilt_x_keyframes"] {
        if r.chance(40) {
            c[f] = if r.chance(30) { json!([]) } else { keyframes(r) };
        }
    }
    c
}

/// The generated clips for `seeds`.
fn clips(seeds: std::ops::Range<u64>) -> Vec<Value> {
    let payloads = effect_payloads();
    seeds.map(|s| clip(&mut Rng(0x9E37_79B9_7F4A_7C15 ^ (s + 1)), &payloads)).collect()
}

#[test]
fn every_generated_clip_is_one_the_reader_accepts() {
    for (i, c) in clips(0..500).iter().enumerate() {
        let typed: Result<klipp_convert::v1::ClipPicture, _> = serde_json::from_value(c.clone());
        assert!(typed.is_ok(), "clip {i} is refused by the frozen reader: {:?}\n{c}", typed.err());
    }
}

#[test]
fn the_generator_is_stable() {
    assert_eq!(clips(0..20), clips(0..20), "the same seeds give the same clips");
}

#[test]
fn canon_is_idempotent_on_generated_clips() {
    for c in clips(0..500) {
        let once = canon(Store::Clip, &c);
        assert_eq!(canon(Store::Clip, &once), once, "canon of a canonical clip changed it:\n{c}");
    }
}

#[test]
fn the_generator_reaches_the_odd_cases() {
    let all = clips(0..500);
    let has = |f: &dyn Fn(&Value) -> bool| all.iter().any(f);
    assert!(has(&|c| c.get("node_graph").is_some_and(Value::is_null)), "a null grade graph");
    assert!(has(&|c| c["node_graph"]["nodes"].as_array().is_some_and(|n| n.len() > 1 && n[0]["id"] == n[1]["id"])), "a node id used twice");
    assert!(has(&|c| c["effect_keyframes"].as_object().is_some_and(|k| k.contains_key("not a key"))), "a keyframe key that does not parse");
    assert!(has(&|c| c["param_modifiers"].as_object().is_some_and(|m| m.contains_key("volume") || m.contains_key("pan"))), "an audio modifier");
    assert!(has(&|c| c["builtin_effects_enabled"].as_array().is_some_and(|f| f.len() > c["builtin_effects"].as_array().map_or(0, Vec::len))), "flags longer than the list");
    assert!(has(&|c| c["node_graph"]["edges"].as_array().is_some_and(|e| e.windows(2).any(|w| w[0] == w[1]))), "two edges into one port");
}

fn obj(v: &Value) -> &Map<String, Value> {
    v.as_object().expect("an object")
}

/// `to_v1(to_v2(x)) == canon(x)`, and `to_v2(to_v1(g)) == g`, for a clip.
fn clip_properties(c: &Value) {
    let record = to_v2::clip_to_v2(obj(c)).unwrap_or_else(|e| panic!("to_v2 refused a clip the reader accepts: {e:?}\n{c}"));
    let record_json = serde_json::to_value(&record).unwrap();
    let back = to_v1::clip(&record_json).unwrap_or_else(|e| panic!("to_v1 refused what to_v2 wrote: {e}\n{record_json}"));
    assert_eq!(back, canon(Store::Clip, c), "to_v1(to_v2(x)) is not canon(x) for\n{c}");
    let mut again = back.clone();
    again["id"] = c["id"].clone();
    let record2 = to_v2::clip_to_v2(obj(&again)).expect("to_v2 takes what to_v1 gives");
    assert!(record2 == record, "to_v2(to_v1(g)) is not g: {}\n{c}", graph_diff(&record2, &record));
}

#[test]
fn a_clip_converts_and_comes_back_whole() {
    for c in clips(0..500) {
        clip_properties(&c);
    }
}

/// A track as v1 saves it: an id, and `effects` holding a grade and a
/// video chain in the forms the reader allows.
fn track(r: &mut Rng, payloads: &[Value]) -> Value {
    let mut fx = json!({});
    match r.below(3) {
        0 => {}
        1 => fx["grade"] = Value::Null,
        _ => fx["grade"] = grade_graph(r),
    }
    if r.chance(70) {
        let n = r.below(4) as usize;
        fx["video"] = Value::Array((0..n).map(|_| r.pick(payloads).clone()).collect());
    }
    if r.chance(50) {
        fx["video_enabled"] = Value::Array((0..r.below(5)).map(|_| Value::Bool(r.chance(70))).collect());
    }
    let mut t = json!({ "id": uuid(r), "name": "V1" });
    if r.chance(90) {
        t["effects"] = fx;
    }
    t
}

#[test]
fn a_track_converts_and_comes_back_whole() {
    let payloads = effect_payloads();
    for s in 0..300 {
        let t = track(&mut Rng(0x51ED_2701 ^ (s + 1)), &payloads);
        let g = to_v2::track_to_v2(obj(&t)).unwrap_or_else(|e| panic!("to_v2 refused a track: {e:?}\n{t}"));
        let back = to_v1::track(&g).unwrap_or_else(|e| panic!("to_v1 refused a track graph: {e}"));
        let raw = t.get("effects").cloned().unwrap_or(json!({}));
        assert_eq!(back, canon(Store::Track, &raw), "track\n{t}");
        let mut again = json!({ "id": t["id"].clone(), "effects": back });
        let g2 = to_v2::track_to_v2(obj(&again)).expect("to_v2 takes what to_v1 gives");
        assert!(g2 == g, "track round trip: {}\n{t}", graph_diff(&g2, &g));
        again.as_object_mut().unwrap().remove("effects");
    }
}

#[test]
fn a_grade_alone_converts_and_comes_back_whole() {
    for s in 0..300 {
        let r = &mut Rng(0x0DDB_A110 ^ (s + 1));
        let g = grade_graph(r);
        let id = uuid(r);
        // A timeline grade, a group stage and a grade version.
        let seq = json!({ "id": id, "timeline_graph": g });
        let timeline = to_v2::timeline_to_v2(obj(&seq)).expect("a timeline grade").expect("present");
        assert_eq!(to_v1::grade_only(&timeline).expect("back"), canon(Store::Graph, &g), "timeline grade\n{g}");
        let group = json!({ "id": id, "pre": g });
        let (pre, post) = to_v2::stages_to_v2(obj(&group)).expect("the stages");
        assert!(post.is_none(), "an unset stage stays unset");
        assert_eq!(to_v1::grade_only(&pre.expect("pre")).expect("back"), canon(Store::Graph, &g));
        let version = json!({ "name": "v2", "node_graph": g });
        let v = to_v2::version_to_v2(uuid::Uuid::parse_str(&id).unwrap(), 3, obj(&version)).expect("a version");
        let back = to_v1::grade_only(&v).expect("back");
        assert_eq!(back, canon(Store::Graph, &g));
        let v2 = to_v2::version_to_v2(uuid::Uuid::parse_str(&id).unwrap(), 3, obj(&json!({ "name": "v2", "node_graph": back }))).expect("again");
        assert!(v2 == v, "version round trip: {}", graph_diff(&v2, &v));
    }
}

/// Both properties for one clip, as an error rather than a panic.
#[test]
fn looks_stills_and_colour_presets_convert_and_come_back_whole() {
    for s in 0..300 {
        let r = &mut Rng(0x0051_07E5 ^ (s + 1));
        let g = grade_graph(r);
        let id = uuid(r);
        check_store("look", &g, &look(&id), &conv_look).unwrap();
        check_store("still", &g, &still, &conv_still).unwrap();
        check_store("colour preset", &g, &|g| g, &conv_preset).unwrap();
    }
    // A still with no grade has no graph.
    assert!(to_v2::still_to_v2(obj(&json!({ "name": "Still", "filename": "a.png" }))).unwrap().is_none());
    assert!(to_v2::still_to_v2(obj(&json!({ "name": "Still", "filename": "a.png", "grade": null }))).unwrap().is_none());
}

/// The sweep over every real project the #1054 scan found: both
/// properties on every clip, track, timeline grade, grade-group stage,
/// grade version and grade still. Run by the lead with
/// `KLIPP_SWEEP_SCAN=<scan report.json> cargo test -p klipp-convert --test oracle -- --ignored --nocapture`.
/// It reads only; a project today's reader refuses is counted as a stop.
#[test]
#[ignore = "reads every project on this machine; run with KLIPP_SWEEP_SCAN"]
fn every_real_project_converts_and_comes_back_whole() {
    let Some(scan) = std::env::var_os("KLIPP_SWEEP_SCAN") else {
        eprintln!("skipped: set KLIPP_SWEEP_SCAN to a klipp-convert scan report");
        return;
    };
    let report: Value = serde_json::from_str(&std::fs::read_to_string(&scan).expect("the scan report")).expect("scan JSON");
    let mut counts: std::collections::BTreeMap<&str, usize> = Default::default();
    let mut failures = Vec::new();
    for p in report["projects"].as_array().expect("projects") {
        let dir = std::path::PathBuf::from(p["path"].as_str().expect("a path"));
        let Ok(text) = std::fs::read_to_string(dir.join("project.json")) else {
            *counts.entry("unreadable files").or_default() += 1;
            continue;
        };
        if klipp_convert::Document::parse(&text, klipp_convert::StoreKind::Project).is_err() {
            *counts.entry("stops (the reader refuses)").or_default() += 1;
            continue;
        }
        *counts.entry("projects").or_default() += 1;
        let raw: Value = serde_json::from_str(&text).expect("parsed once already");
        let seqs: Vec<&Value> = raw.get("sequences").and_then(Value::as_array).map(|s| s.iter().collect()).or_else(|| raw.get("sequence").map(|s| vec![s])).unwrap_or_default();
        let mut check = |what: &'static str, at: String, r: Result<(), String>| {
            *counts.entry(what).or_default() += 1;
            if let Err(e) = r {
                failures.push(format!("{} {at}: {e}", dir.display()));
            }
        };
        for (si, seq) in seqs.iter().enumerate() {
            if let Some(g) = seq.get("timeline_graph").filter(|g| !g.is_null()) {
                check("timeline grades", format!("seq {si}"), check_grade(g));
            }
            for (gi, group) in seq.get("grade_groups").and_then(Value::as_array).into_iter().flatten().enumerate() {
                for stage in ["pre", "post"] {
                    if let Some(g) = group.get(stage).filter(|g| !g.is_null()) {
                        check("group stages", format!("seq {si} group {gi} {stage}"), check_grade(g));
                    }
                }
            }
            for (ti, track) in seq.get("tracks").and_then(Value::as_array).into_iter().flatten().enumerate() {
                check("tracks", format!("seq {si} track {ti}"), check_track(track));
                for (ci, clip) in track.get("clips").and_then(Value::as_array).into_iter().flatten().enumerate() {
                    check("clips", format!("seq {si} track {ti} clip {ci}"), check_clip(clip));
                    for (vi, version) in clip.get("grade_versions").and_then(Value::as_array).into_iter().flatten().enumerate() {
                        if let Some(g) = version.get("node_graph") {
                            check("grade versions", format!("seq {si} track {ti} clip {ci} version {vi}"), check_grade(g));
                        }
                    }
                }
            }
        }
        if let Some(stills) = std::fs::read_to_string(dir.join("media/stills.json")).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()) {
            for (i, entry) in stills.as_array().into_iter().flatten().enumerate() {
                if let Some(g) = entry.get("grade").filter(|g| !g.is_null()) {
                    let rest = entry.clone();
                    let wrap = move |g: Value| {
                        let mut e = rest.clone();
                        e["grade"] = g;
                        e
                    };
                    check("grade stills", format!("still {i}"), check_store("still", g, &wrap, &conv_still));
                }
            }
        }
        for entry in std::fs::read_dir(dir.join("looks")).into_iter().flatten().flatten() {
            let Some(l) = std::fs::read_to_string(entry.path()).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()) else { continue };
            if let (Some(g), Some(id)) = (l.get("node_graph"), l.get("id").and_then(Value::as_str)) {
                check("looks", entry.file_name().to_string_lossy().into_owned(), check_store("look", g, &look(id), &conv_look));
            }
        }
    }
    // The colour presets in the config folder, beside the projects.
    let config = std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")));
    for entry in config.map(|c| c.join("klipp/presets/color")).and_then(|d| std::fs::read_dir(d).ok()).into_iter().flatten().flatten() {
        let Some(g) = std::fs::read_to_string(entry.path()).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()) else { continue };
        if g.get("nodes").is_some() {
            *counts.entry("colour presets").or_default() += 1;
            if let Err(e) = check_store("colour preset", &g, &|g| g, &conv_preset) {
                failures.push(format!("{}: {e}", entry.path().display()));
            }
        }
    }
    eprintln!("sweep counts: {counts:?}");
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.iter().take(30).cloned().collect::<Vec<_>>().join("\n"));
}

/// Two sources into one port: the first edge is the one today's executor
/// reads (`NodeGraph::input_of`), so the conversion keeps the saved order
/// and the round trip gives back the same first source.
#[test]
fn two_sources_into_one_port_keep_their_order() {
    let (a, b, out) = ("00000000-0000-4000-8000-00000000000a", "00000000-0000-4000-8000-000000000001", "00000000-0000-4000-8000-0000000000ff");
    let node = |id: &str, kind: &str| json!({ "id": id, "kind": kind, "label": "", "enabled": true, "params": {}, "keyframes": {"tracks": []}, "output_link": null, "input_links": [] });
    // Both saved orders, so a sort by either id would fail one of them.
    for (first, second) in [(a, b), (b, a)] {
        let g = json!({
            "nodes": [node(a, "Source"), node(b, "Source"), node(out, "Output")],
            "edges": [
                { "from_node": first, "from_port": 0, "to_node": out, "to_port": 0 },
                { "from_node": second, "from_port": 0, "to_node": out, "to_port": 0 }
            ],
            "output_node_id": out
        });
        check_grade(&g).unwrap();
        let back = to_v1::grade_only(&to_v2::version_to_v2(uuid::Uuid::nil(), 0, obj(&json!({ "name": "", "node_graph": g }))).unwrap()).unwrap();
        assert_eq!(back["edges"][0]["from_node"], json!(first), "the saved first source stays first");
        let typed: klipp_nodes::NodeGraph = serde_json::from_value(back).expect("today's reader reads it");
        let picked = typed.input_of(uuid::Uuid::parse_str(out).unwrap(), 0).unwrap();
        assert_eq!(picked, uuid::Uuid::parse_str(first).unwrap(), "today's reader picks the saved first source, as before the conversion");
    }
}

/// A track's audio chain is not picture: the track's round trip compares
/// its picture only, and a track with audio effects converts whole.
#[test]
fn a_track_with_audio_effects_round_trips_its_picture() {
    let t = json!({ "id": "00000000-0000-4000-8000-000000000077", "name": "A1", "effects": { "video": [], "audio": [{"type": "gain", "db": 3.0}] } });
    check_track(&t).unwrap();
}

/// Whether the frozen v1 reader knows `t`'s type.
fn in_v1(t: &klipp_effects::registry::EffectType) -> bool {
    serde_json::from_value::<klipp_convert::v1::effects::BuiltinEffect>(serde_json::to_value((t.sample)()).unwrap()).is_ok() || t.tag == "speed_ramp"
}
