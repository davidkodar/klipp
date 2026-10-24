//! What today's renderer consumes for a clip, read by today's own code
//! (#1068): the reference the lift's adapter is held to.
//!
//! `render_inputs` loads a clip with today's reader and asks today's
//! functions what they would render: the grade graph after
//! `with_gates_applied`, each port's source by `NodeGraph::input_of`, and
//! at a few times across the clip, `clip_effects` (the effect chain with
//! the transform placed and the opacity), `temporal_denoise_of`,
//! `frame_blends` and the adjustment-layer opacity, each where today's
//! renderer asks it (the head only of a Media clip, the adjustment
//! opacity only of an adjustment layer). Four rules are re-derived rather
//! than called, and each is tested against the executor: which unwired
//! ports read the source frame, which read what feeds port 0, which are
//! never read at all, and that a correction reads its port 1 only through
//! a gate.
//!
//! Over it, two properties: today's renderer consumes every clip
//! identically before and after the structural round trip
//! `to_v1(to_v2(x))`, and after the lift and its adapter,
//! `adapter(lift(to_v2(x)))`, over generated clips and every real one.

use std::path::{Path, PathBuf};

use klipp_compose::render::{frame_blends, temporal_denoise_of};
// The effect chain as the renderer read it before step 11d, frozen: the
// oracle must not move with the renderer.
#[path = "support/frozen_clip_effects.rs"]
mod frozen_clip_effects;
use frozen_clip_effects::clip_effects;
use klipp_convert::canon::{AUDIO_MODIFIERS, CLIP_FIELDS};
use klipp_convert::lift::{lift_clip, ClipKind, LiftCtx};
use klipp_convert::splice::{splice, stage_body, track_parts, SpliceInputs, StageInputs, TrackInputs};
use klipp_convert::{adapter, delift, to_v1, to_v2};
use klipp_core::Timecode;
use klipp_nodes::{NodeGraph, NodeKind};
use klipp_timeline::{Clip, Sequence, Track};
use uuid::Uuid;
use serde_json::{json, Map, Value};
use klipp_timeline::GradeAccess;

/// Whether an unwired input `port` of a `kind` node reads the source
/// frame rather than nothing.
// re-derived from crates/klipp-nodes/src/executor.rs: the CPU path's
// primary falls back to `source_frame` (~2190); Layer's unwired background
// (~4231) and ApplyMatte's unwired ungraded input (~4141) fall back to the
// source too. Tested against the executor below.
fn reads_source_when_unwired(kind: NodeKind, port: u8) -> bool {
    port == 0 || (kind == NodeKind::Layer && port == 1) || (kind == NodeKind::ApplyMatte && port == 2)
}

/// Whether an unwired input `port` of a `kind` node reads what feeds its
/// port 0.
// re-derived from crates/klipp-nodes/src/executor.rs: Blend's B falls back
// to `primary_input` (~4207), RGBCombiner's G and B sources too (~4471).
// Tested against the executor below.
fn reads_primary_when_unwired(kind: NodeKind, port: u8) -> bool {
    (kind == NodeKind::Blend && port == 1) || (kind == NodeKind::RGBCombiner && matches!(port, 1 | 2))
}

/// Whether the executor reads input `port` of a `kind` node at all, wired
/// or not. Once the gates are applied a correction's port 1 is read by
/// nothing: a gated one's edge has moved to its ApplyMatte, and an ungated
/// one never reads it. ColorMatch's reference is read when the match is
/// made, not when it renders.
// re-derived from crates/klipp-nodes/src/executor.rs: `execute_node` reads
// a second input for CombineMattes, MaskCutout, ApplyMatte, Blend, Layer
// and RGBCombiner and a third for ApplyMatte and RGBCombiner (~4030-4480);
// LayerMixer reads every port it has (~2101). Tested below.
fn reads_port(kind: NodeKind, port: u8) -> bool {
    use NodeKind as K;
    port == 0
        || match kind {
            K::Layer | K::Blend | K::CombineMattes | K::MaskCutout => port == 1,
            K::ApplyMatte | K::RGBCombiner => port <= 2,
            K::LayerMixer => true,
            _ => false,
        }
}

/// The corrections a matte on port 1 gates (`graph.rs` `gateable`).
const GATEABLE: [NodeKind; 8] = [
    NodeKind::Corrector,
    NodeKind::ColorBalance,
    NodeKind::Exposure,
    NodeKind::Contrast,
    NodeKind::Saturation,
    NodeKind::ColorWheels,
    NodeKind::ToneControls,
    NodeKind::RGBCurves,
];

/// The grade the executor runs, with each port's source resolved, or
/// `"passthrough"` where it refuses to run it: today's `topo_sort` finds a
/// loop in the gated graph, or nothing that ran feeds the output
/// (`NoOutputConnection`, `MissingNodeOutput`). The renderer then shows
/// the frame ungraded (`grade`'s error path in klipp-compose).
///
/// A Source node and the Output node do nothing themselves: what reads a
/// Source is listed as reading `"source"`, and what feeds the Output is
/// the `result`. They are left out of the list, so a graph the lift gave
/// an input or an output it lacked lists as the one it came from.
fn grade(g: &NodeGraph) -> Value {
    let g = g.with_gates_applied().unwrap_or_else(|| g.clone());
    if g.topo_sort().is_err() {
        return json!("passthrough");
    }
    let result = match g.input_of(g.output_node_id, 0).and_then(|id| g.nodes.iter().find(|m| m.id == id)) {
        None => return json!("passthrough"),
        Some(m) if m.kind == NodeKind::Source => json!("source"),
        Some(m) => json!(m.id),
    };
    let nodes: Vec<Value> = g
        .nodes
        .iter()
        .filter(|n| n.kind != NodeKind::Source && !(n.kind == NodeKind::Output && n.id == g.output_node_id))
        .map(|n| {
            let ports = n.dynamic_input_port_count().max(3);
            let mut inputs: Vec<Value> = (0..ports)
                .map(|p| {
                    if !reads_port(n.kind, p) {
                        return Value::Null;
                    }
                    let from = g.input_of(n.id, p).and_then(|id| g.nodes.iter().find(|m| m.id == id));
                    match from {
                        Some(m) if m.kind != NodeKind::Source => json!(m.id),
                        _ if reads_source_when_unwired(n.kind, p) => json!("source"),
                        Some(_) => json!("source"),
                        None => Value::Null,
                    }
                })
                .collect();
            for p in 1..inputs.len() {
                if inputs[p].is_null() && reads_primary_when_unwired(n.kind, p as u8) {
                    inputs[p] = inputs[0].clone();
                }
            }
            json!({ "id": n.id, "kind": n.kind, "enabled": n.enabled, "params": n.params, "keyframes": n.keyframes, "inputs": inputs })
        })
        .collect();
    json!({ "nodes": nodes, "output": g.output_node_id, "result": result, "working_space_override": g.working_space_override })
}

/// The times a clip is read at: its first two frames, a third and a half
/// of the way in, and its last frame.
fn sample_times(c: &Clip, fps: f64) -> Vec<Timecode> {
    let (a, b) = (c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64());
    let frame = 1.0 / fps.max(1.0);
    let rate = c.timeline_in.rate;
    [a, a + frame, a + (b - a) / 3.0, a + (b - a) / 2.0, (b - frame).max(a)].iter().map(|s| Timecode::from_secs_f64(*s, rate)).collect()
}

/// What today's renderer consumes for `c` in a sequence at `fps`.
fn render_inputs(c: &Clip, fps: f64) -> Value {
    use klipp_timeline::ClipKind as K;
    // A gap draws nothing.
    if matches!(c.kind, K::Gap) {
        return json!("gap");
    }
    // The head (frame blending, temporal denoise) runs on the source of a
    // Media clip only (`render_clip`); an adjustment layer's opacity is
    // `evaluate_clip_opacity`, every other clip's comes from its effects.
    let media = matches!(c.kind, K::Media { .. });
    let adjustment = matches!(c.kind, K::AdjustmentLayer);
    let samples: Vec<Value> = sample_times(c, fps)
        .into_iter()
        .map(|t| {
            let (fx, opacity) = clip_effects(c, t, fps, None, None);
            let opacity = if adjustment { klipp_compose::eval::evaluate_clip_opacity(c, t, fps) } else { opacity };
            json!({
                "at": t.to_secs_f64(),
                "effects": fx,
                "opacity": opacity,
                "temporal_denoise": if media { temporal_denoise_of(c, t, fps) } else { None },
                "frame_blends": media && frame_blends(c, t, None, fps),
            })
        })
        .collect();
    json!({ "grade": grade(c.grade()), "ofx": c.ofx_effects().iter().map(|e| json!({"plugin": e.plugin_name, "enabled": e.enabled})).collect::<Vec<_>>(), "samples": samples })
}

/// The sequences of the project in `dir`, as today's reader loads them.
fn load(dir: &Path) -> Result<Vec<Sequence>, String> {
    klipp_project::load_project_v1(dir).map(|r| r.0)
}

/// Every clip of `seqs` by id, with its sequence's rate.
fn clips(seqs: &[Sequence]) -> Vec<(&Clip, f64)> {
    seqs.iter().flat_map(|s| s.tracks.iter().flat_map(move |t| t.clips.iter().map(move |c| (c, s.fps.as_f64())))).collect()
}

/// `clip`'s JSON with its picture fields replaced by `picture` (the
/// record's fields; audio modifiers stay).
fn with_picture(clip: &Value, picture: &Value) -> Value {
    let mut c = clip.as_object().cloned().unwrap_or_default();
    for f in CLIP_FIELDS {
        c.remove(f);
    }
    let audio: Map<String, Value> = c
        .get("param_modifiers")
        .and_then(Value::as_object)
        .map(|m| m.iter().filter(|(k, _)| AUDIO_MODIFIERS.contains(&k.as_str())).map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();
    c.remove("param_modifiers");
    let mut mods = audio;
    for (k, v) in picture.as_object().into_iter().flatten() {
        if k == "param_modifiers" {
            mods.extend(v.as_object().cloned().unwrap_or_default());
        } else {
            c.insert(k.clone(), v.clone());
        }
    }
    if !mods.is_empty() {
        c.insert("param_modifiers".into(), Value::Object(mods));
    }
    Value::Object(c)
}

/// A clip's picture through the structural round trip, `to_v1(to_v2(x))`.
fn round_trip(clip: &Value) -> Result<Value, String> {
    let record = to_v2::clip_to_v2(clip.as_object().ok_or("a clip")?).map_err(|e| format!("to_v2: {e:?}"))?;
    to_v1::clip(&serde_json::to_value(&record).map_err(|e| e.to_string())?)
}

/// The loop rule (decision 6): the lift switches a clip's own grade off
/// exactly where today's renderer refuses to order it, the gated graph as
/// the executor sorts it. The adapter gives a grade that's off back as one
/// that loops whatever it holds, so the comparison alone would not see a
/// loop the lift missed: this does.
fn loop_rule(clip: &Value, picture: &klipp_graph_format::lifted::ClipPicture) -> Result<(), String> {
    use klipp_graph_format::lifted::types as lt;
    let Some(own) = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP && n.origin.is_none()) else { return Ok(()) };
    let refused = match clip.get("node_graph").filter(|g| !g.is_null()) {
        Some(g) => {
            let g: NodeGraph = serde_json::from_value(g.clone()).map_err(|e| format!("the stored grade: {e}"))?;
            g.with_gates_applied().unwrap_or_else(|| g.clone()).topo_sort().is_err()
        }
        None => false,
    };
    match (refused, own.enabled) {
        (true, true) => Err("today's renderer can't order the clip's grade, and the lift kept it on".into()),
        (false, false) => Err("today's renderer orders the clip's grade, and the lift switched it off".into()),
        _ => Ok(()),
    }
}

/// A clip's picture through the lift and back by the adapter,
/// `adapter(lift(to_v2(x)))`.
fn lifted(clip: &Value) -> Result<Value, String> {
    let record = to_v2::clip_to_v2(clip.as_object().ok_or("a clip")?).map_err(|e| format!("to_v2: {e:?}"))?;
    let clip_id = Uuid::parse_str(clip.get("id").and_then(Value::as_str).ok_or("a clip without an id")?).map_err(|e| e.to_string())?;
    let ctx = LiftCtx { clip_id, kind: ClipKind::from_json(clip.get("kind"))? };
    let (picture, _notes) = lift_clip(&record, &ctx).map_err(|e| format!("lift: {e}"))?;
    loop_rule(clip, &picture)?;
    adapter::clip(&picture, &ctx).map_err(|e| format!("adapter: {e}"))
}

/// The project in `dir`, written again into `out` with each clip's
/// picture through `through`.
fn rewritten(dir: &Path, out: &Path, through: fn(&Value) -> Result<Value, String>) -> Result<(), String> {
    let mut project: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("project.json")).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let seqs = project.get_mut("sequences").and_then(Value::as_array_mut).ok_or("no sequences")?;
    for seq in seqs.iter_mut() {
        for track in seq.get_mut("tracks").and_then(Value::as_array_mut).into_iter().flatten() {
            for clip in track.get_mut("clips").and_then(Value::as_array_mut).into_iter().flatten() {
                let picture = through(clip).map_err(|e| format!("clip {}: {e}", clip.get("id").and_then(Value::as_str).unwrap_or("?")))?;
                *clip = with_picture(clip, &picture);
            }
        }
    }
    write_beside(dir, out, &project)
}

/// `project` written into `out` with the files of `dir` the reader may open
/// it with.
fn write_beside(dir: &Path, out: &Path, project: &Value) -> Result<(), String> {
    std::fs::create_dir_all(out.join("media")).map_err(|e| e.to_string())?;
    // The files the reader may open the pair from: the recovery copy of a
    // save that stopped, and the backups beside each file.
    let copy_dir = |from: &Path, to: &Path| -> Result<(), String> {
        std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
        for e in std::fs::read_dir(from).into_iter().flatten().flatten() {
            if e.file_type().is_ok_and(|t| t.is_file()) {
                std::fs::copy(e.path(), to.join(e.file_name())).map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    };
    if dir.join(".recovery").is_dir() {
        copy_dir(&dir.join(".recovery"), &out.join(".recovery"))?;
    }
    for (from, to) in [(dir.to_path_buf(), out.to_path_buf()), (dir.join("media"), out.join("media"))] {
        for e in std::fs::read_dir(&from).into_iter().flatten().flatten() {
            if e.file_name().to_string_lossy().contains(".bak") {
                std::fs::copy(e.path(), to.join(e.file_name())).map_err(|e| e.to_string())?;
            }
        }
    }
    std::fs::copy(dir.join("media/manifest.json"), out.join("media/manifest.json")).map_err(|e| e.to_string())?;
    std::fs::write(out.join("project.json"), project.to_string()).map_err(|e| e.to_string())?;
    Ok(())
}

/// What a project's check found: clips alike, and clips whose two plain
/// loads differ (a load-time rewrite that mints ids: the Color Warper
/// today's reader inserts into an old grade chain, the default pipeline of
/// a clip with no grade). The structural round trip carries the file as
/// it is, so those say nothing about it and are counted apart; the lift
/// makes the rewrite once with ids it derives, so for it they are
/// compared with the minted ids matched to the derived ones.
///
/// It also counts what the comparison reached, so a generator can be held
/// to reaching it: clips that blend frames somewhere, and grades today's
/// renderer refuses to run.
#[derive(Default, Debug)]
pub struct Checked {
    pub alike: usize,
    pub unstable_load: usize,
    pub blending: usize,
    pub passthrough: usize,
    /// Render copies left out of the comparison because the conversion
    /// stops their project: a `CrossGroupWireless` note (#1102), where
    /// today's merged namespace and revision 2's scoped links may differ.
    pub crossed: usize,
}

impl Checked {
    fn reached(&mut self, r: &Value) {
        if r["samples"].as_array().is_some_and(|s| s.iter().any(|x| x["frame_blends"] == true)) {
            self.blending += 1;
        }
        if r["grade"] == "passthrough" {
            self.passthrough += 1;
        }
    }
}

/// Today's renderer consumes every clip of the project in `dir` the same
/// after `through`, or the first difference.
fn check_project(dir: &Path, scratch: &Path, through: fn(&Value) -> Result<Value, String>, align: bool) -> Result<Checked, String> {
    let before = load(dir)?;
    rewritten(dir, scratch, through)?;
    let after = load(scratch)?;
    let (a, b) = (clips(&before), clips(&after));
    if a.len() != b.len() {
        return Err(format!("{} clips before, {} after", a.len(), b.len()));
    }
    let again = load(dir)?;
    let a2 = clips(&again);
    let file = std::fs::read_to_string(dir.join("project.json")).map_err(|e| e.to_string())?;
    let mut checked = Checked::default();
    for (((ca, fa), (cb, _)), (c2, _)) in a.iter().zip(&b).zip(&a2) {
        let (mut ra, rb) = (render_inputs(ca, *fa), render_inputs(cb, *fa));
        checked.reached(&ra);
        if ra != render_inputs(c2, *fa) {
            checked.unstable_load += 1;
            if !align {
                continue;
            }
            ra = align_minted(&ra, &rb, &file);
        } else {
            checked.alike += 1;
        }
        if ra != rb {
            return Err(format!("clip {}: what the renderer consumes changed: {}", ca.id.0, first_diff(&ra, &rb, "")));
        }
    }
    Ok(checked)
}

/// `ra` with each grade node id today's loader minted on this load (one
/// not in the `file`) replaced by the id `rb` has for the node of the same
/// kind in the same place.
fn align_minted(ra: &Value, rb: &Value, file: &str) -> Value {
    let (Some(na), Some(nb)) = (ra["grade"]["nodes"].as_array(), rb["grade"]["nodes"].as_array()) else { return ra.clone() };
    // The listed nodes, and the Output, which is not listed.
    let output = (&ra["grade"]["output"], &rb["grade"]["output"]);
    let swaps: Vec<(String, String)> = na
        .iter()
        .zip(nb)
        .map(|(a, b)| (&a["id"], &b["id"], a["kind"] == b["kind"]))
        .chain(std::iter::once((output.0, output.1, true)))
        .filter_map(|(a, b, same)| Some((a.as_str()?.to_owned(), b.as_str()?.to_owned(), same)))
        .filter(|(ia, ib, same)| ia != ib && *same && !file.contains(ia.as_str()))
        .map(|(ia, ib, _)| (ia, ib))
        .collect();
    // In place: a trip through text would move the floats (serde_json
    // does not parse them back exactly).
    fn swap(v: &mut Value, swaps: &[(String, String)]) {
        match v {
            Value::String(s) => {
                for (from, to) in swaps {
                    if s.contains(from.as_str()) {
                        *s = s.replace(from.as_str(), to);
                    }
                }
            }
            Value::Array(xs) => xs.iter_mut().for_each(|x| swap(x, swaps)),
            Value::Object(m) => m.values_mut().for_each(|x| swap(x, swaps)),
            _ => {}
        }
    }
    let mut out = ra.clone();
    swap(&mut out, &swaps);
    out
}

/// Where two values first differ, as a JSON pointer, with both sides.
fn first_diff(a: &Value, b: &Value, at: &str) -> String {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => x.keys().chain(y.keys()).map(|k| first_diff(x.get(k).unwrap_or(&Value::Null), y.get(k).unwrap_or(&Value::Null), &format!("{at}/{k}"))).find(|d| !d.is_empty()).unwrap_or_default(),
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => x.iter().zip(y).enumerate().map(|(i, (p, q))| first_diff(p, q, &format!("{at}/{i}"))).find(|d| !d.is_empty()).unwrap_or_default(),
        _ if a == b => String::new(),
        _ => format!("{at}: {a} vs {b}"),
    }
}

fn temp(label: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    std::env::temp_dir().join(format!("klipp_render_inputs_{label}_{}_{nanos}", std::process::id()))
}

/// The first re-derived rule, held to the executor: an unwired input that
/// `reads_source_when_unwired` says reads the source gives the same
/// picture as an edge from the Source node.
/// An uneven Transform comes back from the adapter with its height: the
/// lift keeps the aspect (#1406), and the adapter writes Scale Y as the
/// width times it, so the picture it hands the renderer is the original's.
#[test]
fn the_adapter_keeps_an_uneven_transforms_height() {
    let clip = json!({
        "id": "00000000-0000-4000-8000-0000000014a6",
        "builtin_effects": [{"type": "transform", "position_x": 3.0, "position_y": 0.0, "scale_x": 1.25, "scale_y": 1.6, "rotation": 0.0,
                             "anchor_x": 0.5, "anchor_y": 0.5, "rotation_x": 0.0, "rotation_y": 0.0, "perspective": 0.0, "focal_ref": null}]
    });
    let back = lifted(&clip).unwrap();
    let tf = back["builtin_effects"].as_array().unwrap().iter().find(|e| e["type"] == "transform").expect("a Transform");
    let (x, y) = (tf["scale_x"].as_f64().unwrap(), tf["scale_y"].as_f64().unwrap());
    assert!((x - 1.25).abs() < 1e-6 && (y - 1.6).abs() < 1e-6, "scale ({x}, {y})");
}

#[test]
fn the_ports_that_read_the_source_unwired_are_the_executors() {
    use klipp_nodes::graph::{Edge, Node, NodeId};
    use klipp_nodes::params::NodeParams;
    let node = |kind, params| Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: Vec::new() };
    let frame: Vec<u8> = (0..16u32 * 4).flat_map(|i| [(i * 13 % 256) as u8, (i * 7 % 256) as u8, 90, 255]).collect();
    for (kind, port) in [(NodeKind::Exposure, 0u8), (NodeKind::Layer, 1), (NodeKind::ApplyMatte, 2)] {
        let run = |wired: bool| {
            let src = node(NodeKind::Source, NodeParams::default());
            let grade = node(NodeKind::Exposure, NodeParams { exposure: Some(1.0), ..Default::default() });
            let n = node(kind, NodeParams::default());
            let out = node(NodeKind::Output, NodeParams::default());
            let mut edges = vec![Edge { from_node: n.id, from_port: 0, to_node: out.id, to_port: 0 }];
            // The other inputs from a graded copy, so the source read
            // shows where it is the source.
            edges.push(Edge { from_node: src.id, from_port: 0, to_node: grade.id, to_port: 0 });
            for p in 0..3u8 {
                if p != port && (p == 0 || kind != NodeKind::Exposure) {
                    edges.push(Edge { from_node: grade.id, from_port: 0, to_node: n.id, to_port: p });
                }
            }
            if wired {
                edges.push(Edge { from_node: src.id, from_port: 0, to_node: n.id, to_port: port });
            }
            let g = NodeGraph { output_node_id: out.id, nodes: vec![src, grade, n, out], edges, ..Default::default() };
            klipp_nodes::GraphExecutor::new_cpu_only().execute(&frame, &g, 16, 4, 0.0, 0, None, None).unwrap().0
        };
        assert!(reads_source_when_unwired(kind, port));
        assert_eq!(run(false), run(true), "{kind:?} port {port}: unwired reads the source, as an edge from Source does");
    }
}

/// The second re-derived rule, held to the executor: an unwired input
/// that `reads_primary_when_unwired` says reads port 0's picture gives the
/// same picture as an edge from what feeds port 0.
#[test]
fn the_ports_that_read_the_primary_unwired_are_the_executors() {
    use klipp_nodes::graph::{Edge, Node, NodeId};
    use klipp_nodes::params::NodeParams;
    let node = |kind, params| Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: Vec::new() };
    let frame: Vec<u8> = (0..16u32 * 4).flat_map(|i| [(i * 13 % 256) as u8, (i * 7 % 256) as u8, 90, 255]).collect();
    for (kind, port) in [(NodeKind::Blend, 1u8), (NodeKind::RGBCombiner, 1), (NodeKind::RGBCombiner, 2)] {
        let run = |wired: bool| {
            let src = node(NodeKind::Source, NodeParams::default());
            // Port 0 reads a graded copy, other ports the plain source, so
            // a read of port 0's picture shows.
            let grade = node(NodeKind::Exposure, NodeParams { exposure: Some(1.0), ..Default::default() });
            let n = node(kind, NodeParams { mix: Some(0.5), ..Default::default() });
            let out = node(NodeKind::Output, NodeParams::default());
            let mut edges = vec![
                Edge { from_node: src.id, from_port: 0, to_node: grade.id, to_port: 0 },
                Edge { from_node: grade.id, from_port: 0, to_node: n.id, to_port: 0 },
                Edge { from_node: n.id, from_port: 0, to_node: out.id, to_port: 0 },
            ];
            for p in 1..3u8 {
                if p != port && kind == NodeKind::RGBCombiner {
                    edges.push(Edge { from_node: src.id, from_port: 0, to_node: n.id, to_port: p });
                }
            }
            if wired {
                edges.push(Edge { from_node: grade.id, from_port: 0, to_node: n.id, to_port: port });
            }
            let g = NodeGraph { output_node_id: out.id, nodes: vec![src, grade, n, out], edges, ..Default::default() };
            klipp_nodes::GraphExecutor::new_cpu_only().execute(&frame, &g, 16, 4, 0.0, 0, None, None).unwrap().0
        };
        assert!(reads_primary_when_unwired(kind, port));
        assert_eq!(run(false), run(true), "{kind:?} port {port}: unwired reads port 0's picture, as an edge from its feed does");
    }
}

/// The third re-derived rule, held to the executor: a port `reads_port`
/// says is never read changes nothing when wired.
#[test]
fn the_ports_never_read_are_the_executors() {
    use klipp_nodes::graph::{Edge, Node, NodeId};
    use klipp_nodes::params::NodeParams;
    let node = |kind, params| Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: Vec::new() };
    let frame: Vec<u8> = (0..16u32 * 4).flat_map(|i| [(i * 13 % 256) as u8, (i * 7 % 256) as u8, 90, 255]).collect();
    let mut checked = 0;
    for name in [
        "Corrector", "Exposure", "Contrast", "Saturation", "ToneControls", "ColorWheels", "WheelSatBri", "RGBCurves", "HueSatCurves", "ColorBalance", "Qualifier",
        "LumaKey", "SoftClip", "Mask", "CombineMattes", "ExternalMatte", "DifferenceKey", "Layer", "Blend", "ApplyMatte", "Lut3D", "FilmGrain", "ColorMatch",
        "ColorWarper", "Denoise", "RGBSplitter", "RGBCombiner", "PrimariesMatrix", "MaskCutout", "Reroute",
    ] {
        let kind: NodeKind = serde_json::from_value(json!(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        for port in (1..4u8).filter(|p| !reads_port(kind, *p)) {
            let run = |wired: bool| {
                let src = node(NodeKind::Source, NodeParams::default());
                let grade = node(NodeKind::Exposure, NodeParams { exposure: Some(1.5), ..Default::default() });
                let n = node(kind, NodeParams { exposure: Some(0.5), saturation: Some(0.4), mix: Some(0.5), ..Default::default() });
                let out = node(NodeKind::Output, NodeParams::default());
                let mut edges = vec![
                    Edge { from_node: src.id, from_port: 0, to_node: grade.id, to_port: 0 },
                    Edge { from_node: src.id, from_port: 0, to_node: n.id, to_port: 0 },
                    Edge { from_node: n.id, from_port: 0, to_node: out.id, to_port: 0 },
                ];
                if wired {
                    edges.push(Edge { from_node: grade.id, from_port: 0, to_node: n.id, to_port: port });
                }
                let g = NodeGraph { output_node_id: out.id, nodes: vec![src, grade, n, out], edges, ..Default::default() };
                let g = g.with_gates_applied().unwrap_or(g);
                klipp_nodes::GraphExecutor::new_cpu_only().execute(&frame, &g, 16, 4, 0.0, 0, None, None).map(|r| r.0).map_err(|e| format!("{e:?}"))
            };
            // A gated correction is the gate's to read, not the correction's.
            if port == 1 && GATEABLE.contains(&kind) {
                continue;
            }
            assert_eq!(run(false), run(true), "{kind:?} port {port}: a port never read changed the picture");
            checked += 1;
        }
    }
    assert!(checked > 50, "only {checked} ports checked");
}

/// The fourth re-derived rule, held to the executor: a correction's port
/// 1, wired with port 0 unwired (so no gate), changes nothing.
#[test]
fn an_ungated_correction_reads_nothing_on_port_1() {
    use klipp_nodes::graph::{Edge, Node, NodeId};
    use klipp_nodes::params::NodeParams;
    let node = |kind, params| Node { id: NodeId::new_v4(), kind, label: String::new(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: Vec::new() };
    let frame: Vec<u8> = (0..16u32 * 4).flat_map(|i| [(i * 13 % 256) as u8, (i * 7 % 256) as u8, 90, 255]).collect();
    for kind in GATEABLE {
        let run = |wired: bool| {
            let src = node(NodeKind::Source, NodeParams::default());
            let matte = node(NodeKind::Mask, NodeParams::default());
            let corr = node(kind, NodeParams { exposure: Some(1.0), saturation: Some(0.3), contrast: Some(1.4), ..Default::default() });
            let out = node(NodeKind::Output, NodeParams::default());
            let mut edges = vec![
                Edge { from_node: src.id, from_port: 0, to_node: matte.id, to_port: 0 },
                Edge { from_node: corr.id, from_port: 0, to_node: out.id, to_port: 0 },
            ];
            if wired {
                edges.push(Edge { from_node: matte.id, from_port: 0, to_node: corr.id, to_port: 1 });
            }
            let g = NodeGraph { output_node_id: out.id, nodes: vec![src, matte, corr, out], edges, ..Default::default() };
            assert!(g.with_gates_applied().is_none(), "{kind:?} is gated with port 0 unwired");
            klipp_nodes::GraphExecutor::new_cpu_only().execute(&frame, &g, 16, 4, 0.0, 0, None, None).unwrap().0
        };
        assert_eq!(run(false), run(true), "{kind:?}: an ungated port 1 changed the picture");
    }
}

/// What the splice takes for `clip` on `track` of `seq`, each skip decided
/// by today's own code, as the bakes decide it: a group's stage only where
/// the group grades and its `prepend_keeping_ids` or `append_keeping_ids`
/// changes the clip's graph, the track's effects only on a picture track
/// that touches the picture, its grade only where its append changes the
/// graph inside the group's. `raw_seq` and `raw_track` are the file's;
/// with `v2`, the same sequence and track of its revision-2 conversion,
/// whose stored bodies are taken instead of lifting them here.
fn splice_inputs(seq: &Sequence, track: &Track, clip: &Clip, raw_seq: &Value, raw_track: &Value, v2: Option<(&Value, &Value)>) -> Result<SpliceInputs, String> {
    use klipp_timeline::{ClipKind as K, TrackKind};
    let mut inputs = SpliceInputs::default();
    let mut g = clip.grade().clone();
    let group = clip.grade_group.and_then(|id| seq.grade_group(id)).filter(|g| g.grades());
    if let Some(group) = group {
        let raw = raw_seq["grade_groups"].as_array().into_iter().flatten().find(|r| r["id"] == group.id.0.to_string()).ok_or("the group's entry")?;
        let (pre, post) = to_v2::stages_to_v2(raw.as_object().ok_or("a group")?).map_err(|e| format!("{e:?}"))?;
        // The stage bodies as the converted file stores them.
        let stored = |which: &str| -> Result<Option<klipp_graph_format::Graph>, String> {
            let (v2_seq, _) = v2.ok_or("no file")?;
            let g = v2_seq["grade_groups"].as_array().into_iter().flatten().find(|r| r["id"] == group.id.0.to_string()).ok_or("the group's converted entry")?;
            g["graph_v2"].get(which).map(|b| serde_json::from_value(b.clone()).map_err(|e| e.to_string())).transpose()
        };
        let mut take = |stage: Option<&klipp_nodes::NodeGraph>, structural: Option<klipp_graph_format::Graph>, prepend: bool| -> Result<Option<klipp_graph_format::Graph>, String> {
            let Some(stage) = stage.filter(|s| !s.is_identity()) else { return Ok(None) };
            let mut h = g.clone();
            let ok = if prepend { h.prepend_keeping_ids(stage) } else { h.append_keeping_ids(stage) }.is_ok();
            if !ok || h == g {
                return Ok(None);
            }
            g = h;
            if v2.is_some() {
                return stored(if prepend { "pre" } else { "post" })?.ok_or_else(|| "a stage the file doesn't store".to_owned()).map(Some);
            }
            stage_body(&structural.ok_or("a stage with no structural graph")?).map(Some)
        };
        let pre = take(group.stage(klipp_timeline::GradeStage::Pre), pre, true)?;
        let post = take(group.stage(klipp_timeline::GradeStage::Post), post, false)?;
        inputs.stages = Some(StageInputs { group: group.id.0, pre, post });
    }
    let picture = track.effects.touches_picture() && matches!(track.kind, TrackKind::Video | TrackKind::Text);
    if !track.effects.is_empty() && picture && !matches!(clip.kind, K::Gap | K::AdjustmentLayer) {
        let (effects, grade) = match v2 {
            // The track's effects and grade as the converted file stores them.
            Some((_, v2_track)) => {
                let stored = &v2_track["effects"]["graph_v2"];
                let effects: Vec<klipp_graph_format::Node> = serde_json::from_value(stored["effects"].clone()).map_err(|e| format!("the stored track effects: {e}"))?;
                let grade = stored.get("grade").map(|g| serde_json::from_value(g.clone()).map_err(|e| format!("the stored track grade: {e}"))).transpose()?;
                (effects, grade)
            }
            None => track_parts(&to_v2::track_to_v2(raw_track.as_object().ok_or("a track")?).map_err(|e| format!("{e:?}"))?)?,
        };
        let applies = track.effects.track_grade().filter(|t| !t.is_identity()).is_some_and(|t| {
            let mut h = g.clone();
            h.append_keeping_ids(t).is_ok() && h != g
        });
        inputs.track = Some(TrackInputs { track: track.id.0, effects, grade: grade.filter(|_| applies) });
    }
    Ok(inputs)
}

/// The renderer consumes every clip of the project in `dir` as its render
/// copy (`baked_for_render`) after the lift, the splice and the adapter's
/// flattening: `adapter(splice(lift(to_v2(x))))`. The flattened grade is
/// read as the reader reads a graph (serde), not loaded again, since a
/// load would migrate it as if it were a file's.
///
/// With `from_file`, every lifted piece is read from the project's
/// revision-2 conversion (`revision2::project`) as it would be written,
/// not lifted here: the converted file renders as the original does.
fn check_spliced(dir: &Path, scratch: &Path, from_file: bool) -> Result<Checked, String> {
    let file = std::fs::read_to_string(dir.join("project.json")).map_err(|e| e.to_string())?;
    let project: Value = serde_json::from_str(&file).map_err(|e| e.to_string())?;
    let converted = if from_file { Some(klipp_convert::revision2::project(&project).map_err(|e| format!("revision 2: {e}"))?.0) } else { None };
    check_spliced_against(dir, scratch, converted)
}

/// The render check of `klipp-convert run` (#1087): the revision-2 project
/// written into `copy` renders, clip by clip, as the v1 project in
/// `original` does today (its render copy, `baked_for_render`). `scratch`
/// is an empty folder the check may write into.
pub fn check_written(original: &Path, copy: &Path, scratch: &Path) -> Result<Checked, String> {
    let text = std::fs::read_to_string(copy.join("project.json")).map_err(|e| format!("the written project: {e}"))?;
    let written: Value = serde_json::from_str(&text).map_err(|e| format!("the written project: {e}"))?;
    check_spliced_against(original, scratch, Some(written))
}

/// [`check_spliced`] with the converted project given, or lifted here when
/// `None`.
fn check_spliced_against(dir: &Path, scratch: &Path, converted: Option<Value>) -> Result<Checked, String> {
    let before = load(dir)?;
    let file = std::fs::read_to_string(dir.join("project.json")).map_err(|e| e.to_string())?;
    let mut project: Value = serde_json::from_str(&file).map_err(|e| e.to_string())?;
    // The clips whose link names meet a merged graph's: the conversion
    // stops their project, so their render copies are not compared.
    let crossed: std::collections::BTreeSet<Uuid> = project["sequences"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .flat_map(klipp_convert::revision2::cross_group_wireless)
        .filter_map(|n| match n {
            klipp_graph_format::lifted::LiftNote::CrossGroupWireless { clip, .. } => Some(clip),
            _ => None,
        })
        .collect();
    let mut grades: Vec<Value> = Vec::new();
    // How many of each copy's effects are its track's, which the bake
    // marks by keeping the clip's own aside (`own_effects`).
    let mut track_fx: Vec<usize> = Vec::new();
    let raw_seqs = project["sequences"].as_array_mut().ok_or("no sequences")?;
    if raw_seqs.len() != before.len() {
        return Err("sequences and their entries differ in number".into());
    }
    for (si, (seq, raw_seq)) in before.iter().zip(raw_seqs.iter_mut()).enumerate() {
        let raw_view = raw_seq.clone();
        let v2_seq = converted.as_ref().map(|c| &c["sequences"][si]);
        let raw_tracks = raw_seq["tracks"].as_array_mut().ok_or("no tracks")?;
        for (ti, (track, raw_track)) in seq.tracks.iter().zip(raw_tracks.iter_mut()).enumerate() {
            let track_view = raw_track.clone();
            let v2_track = v2_seq.map(|s| &s["tracks"][ti]);
            let raw_clips = raw_track["clips"].as_array_mut().ok_or("no clips")?;
            for (ci, (clip, raw_clip)) in track.clips.iter().zip(raw_clips.iter_mut()).enumerate() {
                let id = clip.id.0;
                let ctx = LiftCtx { clip_id: id, kind: ClipKind::from_json(raw_clip.get("kind"))? };
                let picture: klipp_graph_format::lifted::ClipPicture = match v2_track {
                    Some(t) => serde_json::from_value(t["clips"][ci]["graph_v2"].clone()).map_err(|e| format!("clip {id}: the stored picture: {e}"))?,
                    None => {
                        let record = to_v2::clip_to_v2(raw_clip.as_object().ok_or("a clip")?).map_err(|e| format!("to_v2: {e:?}"))?;
                        lift_clip(&record, &ctx).map_err(|e| format!("lift: {e}"))?.0
                    }
                };
                loop_rule(raw_clip, &picture).map_err(|e| format!("clip {id}: {e}"))?;
                let inputs = splice_inputs(seq, track, clip, &raw_view, &track_view, v2_seq.zip(v2_track)).map_err(|e| format!("clip {id}: {e}"))?;
                let adapted = adapter::clip(&splice(&picture, ctx.kind, &inputs), &ctx).map_err(|e| format!("clip {id}: adapter: {e}"))?;
                grades.push(adapted.get("node_graph").cloned().unwrap_or(Value::Null));
                track_fx.push(inputs.track.as_ref().map_or(0, |t| t.effects.len()));
                *raw_clip = with_picture(raw_clip, &adapted);
            }
            // Baked into its clips now.
            if let Some(t) = raw_track.as_object_mut() {
                t.remove("effects");
            }
        }
        if let Some(s) = raw_seq.as_object_mut() {
            s.remove("grade_groups");
        }
    }
    write_beside(dir, scratch, &project)?;
    let after = load(scratch)?;
    let again = load(dir)?;
    let baked: Vec<Sequence> = before.iter().map(|s| s.baked_for_render().into_owned()).collect();
    let baked_again: Vec<Sequence> = again.iter().map(|s| s.baked_for_render().into_owned()).collect();
    let (a, b, a2) = (clips(&baked), clips(&after), clips(&baked_again));
    if a.len() != b.len() || a.len() != grades.len() {
        return Err(format!("{} clips baked, {} after", a.len(), b.len()));
    }
    let mut checked = Checked::default();
    for (((((ca, fa), (cb, _)), (c2, _)), grade), k) in a.iter().zip(&b).zip(&a2).zip(&grades).zip(&track_fx) {
        let mut cb = (*cb).clone();
        if *k > 0 {
            // The spliced copy holds its track's effects after its own:
            // counted as a bake counts them.
            let own = cb.builtin_effects().len().saturating_sub(*k);
            cb.baked_own = Some(klipp_timeline::BakedOwn { effects: own, audio: cb.audio_effects.len() });
        }
        if !grade.is_null() {
            let flattened = serde_json::from_value(grade.clone()).map_err(|e| format!("clip {}: the flattened grade: {e}", cb.id.0))?;
            cb.edit_grade(|g| *g = flattened);
        }
        if crossed.contains(&ca.id.0) {
            checked.crossed += 1;
            continue;
        }
        let (mut ra, rb) = (render_inputs(ca, *fa), render_inputs(&cb, *fa));
        checked.reached(&ra);
        if ra != render_inputs(c2, *fa) {
            checked.unstable_load += 1;
            ra = align_minted(&ra, &rb, &file);
        } else {
            checked.alike += 1;
        }
        if ra != rb {
            return Err(format!("clip {}: what the renderer consumes of the render copy changed: {}", ca.id.0, first_diff(&ra, &rb, "")));
        }
    }
    Ok(checked)
}

/// A small seeded generator, as the oracle's.
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
    fn unit(&mut self) -> f64 {
        self.below(1000) as f64 / 1000.0
    }
}

fn keys(r: &mut Rng, scale: f64) -> Value {
    let n = 1 + r.below(3);
    Value::Array((0..n).map(|i| json!({ "time_secs": i as f64 * 0.7, "value": r.unit() * scale, "interpolation": (["Linear", "Step"][r.below(2) as usize]) })).collect())
}

/// A modifier stack that changes a value, or the time its keys are read
/// at.
fn modifiers(r: &mut Rng) -> Value {
    let one = |r: &mut Rng| match r.below(5) {
        0 => json!({ "type": "oscillate", "amplitude": r.unit(), "frequency": 1.0 + r.unit() }),
        1 => json!({ "type": "ramp", "start_value": r.unit(), "end_value": r.unit(), "blend": (["Add", "Multiply", "Replace"][r.below(3) as usize]) }),
        2 => json!({ "type": "clamp", "min": 0.1, "max": 0.6 }),
        3 => json!({ "type": "loop", "period_secs": 0.5 + r.unit() }),
        _ => json!({ "type": "ping_pong", "period_secs": 0.5 + r.unit() }),
    };
    let n = 1 + r.below(2);
    json!({ "modifiers": (0..n).map(|_| one(r)).collect::<Vec<_>>() })
}

/// A live effect entry as v1 stored it: a Speed Ramp with the fields
/// picture revision 4 drops (#1409), which the frozen reader requires;
/// none of them drew.
fn v1_entry(mut e: Value) -> Value {
    if e["type"] == "speed_ramp" {
        for (k, x) in [("ramp_curve", json!(0)), ("ramp_start", json!(0.0)), ("ramp_end", json!(1.0)), ("reverse", json!(false)), ("freeze_at", json!(0.0)), ("freeze", json!(false))] {
            e[k] = x;
        }
    }
    e
}

/// An effect from the registry with its numbers moved, or one of the
/// effects the lift reads: opacity, transform, temporal denoise, a speed
/// ramp that blends frames.
fn effect(r: &mut Rng, samples: &[Value]) -> Value {
    let mut e = match r.below(6) {
        0 => json!({ "type": "opacity", "value": r.unit() * 1.2 }),
        1 => serde_json::to_value(klipp_effects::registry::effect_type("transform").map(|t| (t.sample)())).unwrap(),
        2 => serde_json::to_value(klipp_effects::registry::effect_type("temporal_denoise").map(|t| (t.sample)())).unwrap(),
        3 => {
            let mut s = serde_json::to_value(klipp_effects::registry::effect_type("speed_ramp").map(|t| (t.sample)())).unwrap();
            s["blend_mode"] = json!(r.below(2));
            v1_entry(s)
        }
        _ => samples[r.below(samples.len() as u64) as usize].clone(),
    };
    let numeric: Vec<String> = e.as_object().unwrap().iter().filter(|(k, v)| v.is_f64() && k.as_str() != "type").map(|(k, _)| k.clone()).collect();
    for k in numeric {
        if r.chance(50) {
            e[&k] = json!(e[&k].as_f64().unwrap() * (0.5 + r.unit()) + if r.chance(30) { r.unit() } else { 0.0 });
        }
    }
    if e["type"] == "temporal_denoise" && r.chance(30) {
        e["strength"] = json!(0.0);
    }
    e
}

/// A v1 grade graph of the shapes the lift rewrites: gates, a port 1
/// with port 0 unwired, an unwired primary, Layer's unwired background,
/// merges, no Source at all, the old chain the loader migrates, two edges
/// into one port, wireless links.
fn grade_graph(r: &mut Rng) -> Value {
    const KINDS: [&str; 13] = ["Corrector", "Exposure", "Contrast", "Saturation", "ColorWheels", "HueSatCurves", "Mask", "Qualifier", "Layer", "Blend", "LayerMixer", "ApplyMatte", "RGBCombiner"];
    let id = |r: &mut Rng| format!("00000000-0000-4000-8000-{:012x}", r.below(1 << 40));
    let mut nodes = Vec::new();
    let source = if r.chance(85) { Some(id(r)) } else { None };
    if let Some(s) = &source {
        nodes.push(json!({ "id": s, "kind": "Source", "label": "", "enabled": true }));
    }
    let n = 1 + r.below(5) as usize;
    let mut ids = Vec::new();
    for _ in 0..n {
        let i = id(r);
        let kind = KINDS[r.below(KINDS.len() as u64) as usize];
        let mut node = json!({ "id": i, "kind": kind, "label": "", "enabled": r.chance(85), "params": { "exposure": r.unit(), "saturation": r.unit() * 2.0, "mix": r.unit() } });
        if r.chance(25) {
            node["keyframes"] = json!({ "tracks": [{ "param_name": "exposure", "keyframes": keys(r, 2.0) }] });
        }
        nodes.push(node);
        ids.push(i);
    }
    let output = id(r);
    nodes.push(json!({ "id": output, "kind": "Output", "label": "", "enabled": true }));
    let mut edges = Vec::new();
    let mut prev = source.clone();
    for i in &ids {
        // The chain, with a link left out now and then: an unwired primary.
        if let (Some(p), true) = (&prev, r.chance(80)) {
            edges.push(json!({ "from_node": p, "from_port": 0, "to_node": i, "to_port": 0 }));
        }
        // A second input, from earlier in the chain or the source.
        if r.chance(35) {
            let from = if r.chance(50) { source.clone() } else { ids.first().cloned() };
            if let Some(f) = from.filter(|f| f != i) {
                edges.push(json!({ "from_node": f, "from_port": 0, "to_node": i, "to_port": 1 }));
                if r.chance(15) {
                    edges.push(json!({ "from_node": f, "from_port": 0, "to_node": i, "to_port": 1 }));
                }
            }
        }
        prev = Some(i.clone());
    }
    if let Some(p) = prev {
        edges.push(json!({ "from_node": p, "from_port": 0, "to_node": output, "to_port": 0 }));
    }
    if ids.len() > 1 && r.chance(15) {
        let last = nodes.len() - 2;
        nodes[last]["output_link"] = json!("bus");
        let at = if source.is_some() { 1 } else { 0 };
        nodes[at]["input_links"] = json!([null, "bus"]);
    }
    json!({ "nodes": nodes, "edges": edges, "output_node_id": output })
}

/// One v1 clip picture.
fn picture(r: &mut Rng, samples: &[Value]) -> Value {
    let mut c = json!({});
    if r.chance(85) {
        c["node_graph"] = grade_graph(r);
    }
    if r.chance(20) {
        c["effects"] = json!([{ "plugin_name": "org.example.Glow", "enabled": r.chance(50), "param_values": {} }]);
    }
    let n = r.below(6) as usize;
    let fx: Vec<Value> = (0..n).map(|_| effect(r, samples)).collect();
    if n > 0 {
        let flags = r.below(n as u64 + 2);
        c["builtin_effects_enabled"] = Value::Array((0..flags).map(|_| Value::Bool(r.chance(75))).collect());
    }
    let mut ekeys = Map::new();
    let mut mods = Map::new();
    for (i, e) in fx.iter().enumerate() {
        let params: Vec<&String> = e.as_object().unwrap().iter().filter(|(_, v)| v.is_number()).map(|(k, _)| k).collect();
        if params.is_empty() {
            continue;
        }
        let k = params[r.below(params.len() as u64) as usize].clone();
        // The name `chroma_key` animates `soft_edge` by.
        let k = if e["type"] == "chroma_key" && r.chance(50) { "spill".to_owned() } else { k };
        if r.chance(30) {
            ekeys.insert(format!("{i}:{k}"), keys(r, 1.0));
        }
        if r.chance(20) {
            mods.insert(format!("effect:{i}:{k}"), modifiers(r));
        }
    }
    if r.chance(10) {
        ekeys.insert(format!("{}:value", n + 2), keys(r, 1.0));
    }
    c["builtin_effects"] = Value::Array(fx);
    if !ekeys.is_empty() {
        c["effect_keyframes"] = Value::Object(ekeys);
    }
    for (field, modifier, scale) in [
        ("opacity_keyframes", Some("opacity"), 1.0),
        ("transform_pos_x_keyframes", Some("position_x"), 300.0),
        ("transform_pos_y_keyframes", Some("position_y"), 300.0),
        ("transform_scale_keyframes", Some("scale"), 2.0),
        ("transform_rotation_keyframes", Some("rotation"), 90.0),
        ("transform_tilt_x_keyframes", None, 40.0),
        ("transform_tilt_y_keyframes", None, 40.0),
    ] {
        if r.chance(15) {
            c[field] = keys(r, scale);
        }
        if let (Some(m), true) = (modifier, r.chance(12)) {
            mods.insert(m.into(), modifiers(r));
        }
    }
    if !mods.is_empty() {
        c["param_modifiers"] = Value::Object(mods);
    }
    c
}

/// A project of `n` generated clips (media, adjustment layers and gaps)
/// on one track, saved by today's code into `dir` and then given their
/// generated pictures.
fn generated_project(dir: &Path, seed: u64, n: usize) {
    use klipp_timeline::ClipKind as K;
    let mut r = Rng(0x9E37_79B9_7F4A_7C15 ^ (seed + 1));
    let samples: Vec<Value> = klipp_effects::registry::effect_types().filter(|t| in_v1(t)).map(|t| v1_entry(serde_json::to_value((t.sample)()).unwrap())).collect();
    let mut seq = Sequence::new_default();
    let tc = |s: f64| Timecode::from_secs_f64(s, 24);
    for i in 0..n {
        let kind = match r.below(10) {
            0..=6 => K::Media { media_id: klipp_core::MediaId::new() },
            7 | 8 => K::AdjustmentLayer,
            _ => K::Gap,
        };
        let id = klipp_core::ClipId(Uuid::from_u64_pair(seed + 1, i as u64 + 1));
        seq.tracks[0].clips.push(Clip { id, kind, ..Clip::gap(tc(i as f64 * 2.0), tc(i as f64 * 2.0 + 2.0)) });
    }
    klipp_project::save_project_v1(dir, &[seq], &klipp_media::MediaPool::new()).expect("save");
    let path = dir.join("project.json");
    let mut project: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    // The speeds a frame blend needs to show, from a stream of their own so
    // the pictures stay the ones generated before: slower than 1, keyed
    // across 1, and reversed (which blends nothing).
    let mut speeds = Rng(0x5EED_5BEE ^ (seed + 3));
    for clip in project["sequences"][0]["tracks"][0]["clips"].as_array_mut().unwrap() {
        *clip = with_picture(clip, &picture(&mut r, &samples));
        match speeds.below(5) {
            0 => clip["speed"] = json!(0.25 + speeds.unit() * 0.7),
            1 => clip["speed_keyframes"] = keys(&mut speeds, 1.6),
            2 => clip["speed"] = json!(-(0.25 + speeds.unit())),
            _ => {}
        }
    }
    std::fs::write(&path, project.to_string()).unwrap();
}

/// [`generated_project`] with grade groups and track effects: a group with
/// a pre and a post stage (either may be absent, an identity, or have no
/// Source) holding some of the clips, and the track's video effects and
/// grade.
/// A grade graph as generated, unless its wireless bus closes a loop by
/// today's ordering, in which case without the bus.
fn loop_free(mut g: Value) -> Value {
    let typed: klipp_nodes::NodeGraph = serde_json::from_value(g.clone()).unwrap();
    if typed.topo_sort().is_ok() {
        return g;
    }
    for n in g["nodes"].as_array_mut().into_iter().flatten() {
        if let Some(o) = n.as_object_mut() {
            o.remove("output_link");
            o.remove("input_links");
        }
    }
    g
}

fn generated_spliced_project(dir: &Path, seed: u64, n: usize) {
    generated_project(dir, seed, n);
    let mut r = Rng(0x51CE_D00D ^ (seed + 7));
    let samples: Vec<Value> = klipp_effects::registry::effect_types().filter(|t| in_v1(t)).map(|t| v1_entry(serde_json::to_value((t.sample)()).unwrap())).collect();
    let path = dir.join("project.json");
    let mut project: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let seq = &mut project["sequences"][0];
    let group = Uuid::from_u64_pair(0x0067_726f_7570, seed + 1).to_string();
    // A stage or track grade that truly loops is a conversion stop (today
    // a loop merged into a clip fails that clip's whole grade), settled by
    // hand; the generator keeps its wireless buses but leaves out the
    // graphs whose bus closes a loop.
    let stage = |r: &mut Rng| match r.below(6) {
        0 => Value::Null,
        1 => json!({ "nodes": [], "edges": [], "output_node_id": Uuid::nil() }),
        _ => loop_free(grade_graph(r)),
    };
    let (pre, post) = (stage(&mut r), stage(&mut r));
    let mut g = json!({ "id": group, "name": "Night" });
    if !pre.is_null() {
        g["pre"] = pre;
    }
    if !post.is_null() {
        g["post"] = post;
    }
    seq["grade_groups"] = json!([g]);
    for clip in seq["tracks"][0]["clips"].as_array_mut().unwrap() {
        if r.chance(60) {
            clip["grade_group"] = json!(group);
        }
    }
    if r.chance(70) {
        // Opacity among them: a track's weighs the clip's (#1078).
        let video: Vec<Value> = (0..r.below(3)).map(|_| effect(&mut r, &samples)).collect();
        let flags: Vec<Value> = (0..r.below(video.len() as u64 + 1)).map(|_| json!(r.chance(75))).collect();
        let mut fx = json!({ "video": video, "video_enabled": flags });
        if r.chance(60) {
            fx["grade"] = loop_free(grade_graph(&mut r));
        }
        seq["tracks"][0]["effects"] = fx;
    }
    std::fs::write(&path, project.to_string()).unwrap();
}

/// The lift, the splice and the adapter's flattening change nothing
/// today's renderer consumes of a clip's render copy, over generated
/// clips in grade groups on tracks with effects and grades: lifted here,
/// and read back from the revision-2 file the conversion writes.
#[test]
fn the_renderer_consumes_every_generated_render_copy_alike_after_the_splice() {
    for from_file in [false, true] {
        let (mut clips, mut migrated, mut blending, mut passthrough, mut crossed) = (0, 0, 0, 0, 0);
        let mut failures = Vec::new();
        for seed in 0..40 {
            let (dir, scratch) = (temp("spl"), temp("spl_out"));
            generated_spliced_project(&dir, seed, 20);
            match check_spliced(&dir, &scratch, from_file) {
                Ok(c) => {
                    clips += c.alike + c.unstable_load;
                    migrated += c.unstable_load;
                    blending += c.blending;
                    passthrough += c.passthrough;
                    crossed += c.crossed;
                }
                Err(e) => failures.push(format!("seed {seed}: {e}")),
            }
            let _ = std::fs::remove_dir_all(&dir);
            let _ = std::fs::remove_dir_all(&scratch);
        }
        let how = if from_file { "read from the revision-2 file" } else { "lifted" };
        eprintln!("spliced, {how}: {clips} render copies alike, {migrated} of them rewritten on load, {blending} blending frames, {passthrough} not graded, {crossed} stopped for crossed links");
        assert!(failures.is_empty(), "{how}: {} failures:\n{}", failures.len(), failures.join("\n"));
        assert!(clips >= 700 && blending > 0 && passthrough > 0, "the generator reached too little: {clips} clips, {blending} blending, {passthrough} not graded");
        // The note fires on the generated case of a stage's sender taking
        // over a clip's receiver (seed 3), and leaves out only a few.
        assert!((1..=clips / 20).contains(&crossed), "{crossed} render copies stopped for crossed links");
    }
}

/// A clip whose own grade loops by its edges (decision 6), in a grade
/// group with a pre and a post stage: today the baked graph still loops
/// and nothing grades, stages included, and the lifted splice, its grade
/// switched off, renders it the same, lifted here and read from the file.
#[test]
fn a_looping_grade_in_a_group_grades_nothing_stages_included() {
    let stage = |seed: u64| {
        let mut r = Rng(0xC0FF_EE00 ^ seed);
        loop {
            let g = loop_free(grade_graph(&mut r));
            if g["nodes"].as_array().is_some_and(|n| n.len() > 2) {
                return g;
            }
        }
    };
    let (a, b, src, out) = ("00000000-0000-4000-8000-0000000000a1", "00000000-0000-4000-8000-0000000000b2", "00000000-0000-4000-8000-0000000000c3", "00000000-0000-4000-8000-0000000000d4");
    let looping = json!({
        "nodes": [
            { "id": src, "kind": "Source", "label": "", "enabled": true },
            { "id": a, "kind": "Exposure", "label": "", "enabled": true, "params": { "exposure": 0.5 } },
            { "id": b, "kind": "Saturation", "label": "", "enabled": true, "params": { "saturation": 1.5 } },
            { "id": out, "kind": "Output", "label": "", "enabled": true },
        ],
        "edges": [
            { "from_node": b, "from_port": 0, "to_node": a, "to_port": 0 },
            { "from_node": a, "from_port": 0, "to_node": b, "to_port": 0 },
            { "from_node": b, "from_port": 0, "to_node": out, "to_port": 0 },
        ],
        "output_node_id": out,
    });
    for from_file in [false, true] {
        let (dir, scratch) = (temp("loopgrp"), temp("loopgrp_out"));
        let mut seq = Sequence::new_default();
        let id = klipp_core::ClipId(Uuid::from_u64_pair(0x100b, 1));
        let tc = |s: f64| Timecode::from_secs_f64(s, 24);
        seq.tracks[0].clips.push(Clip { id, kind: klipp_timeline::ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) });
        klipp_project::save_project_v1(&dir, &[seq], &klipp_media::MediaPool::new()).expect("save");
        let path = dir.join("project.json");
        let mut project: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let group = Uuid::from_u64_pair(0x100b, 2).to_string();
        let s = &mut project["sequences"][0];
        s["grade_groups"] = json!([{ "id": group, "name": "Night", "pre": stage(1), "post": stage(2) }]);
        let clip = &mut s["tracks"][0]["clips"][0];
        clip["node_graph"] = looping.clone();
        clip["grade_group"] = json!(group);
        std::fs::write(&path, project.to_string()).unwrap();

        // Both stages reach the baked graph, and it still loops.
        let baked = load(&dir).unwrap()[0].baked_for_render().into_owned();
        let c = &baked.tracks[0].clips[0];
        assert!(c.grade().nodes.len() > 4, "the stages are baked in");
        assert_eq!(render_inputs(c, 24.0)["grade"], "passthrough", "today grades nothing");
        let checked = check_spliced(&dir, &scratch, from_file).unwrap_or_else(|e| panic!("from_file {from_file}: {e}"));
        assert_eq!((checked.alike + checked.unstable_load, checked.passthrough), (1, 1));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&scratch);
    }
}

/// The lift and its adapter change nothing today's renderer consumes,
/// over generated clips.
#[test]
fn the_renderer_consumes_every_generated_clip_alike_after_the_lift() {
    let (mut clips, mut migrated, mut blending, mut passthrough) = (0, 0, 0, 0);
    let mut failures = Vec::new();
    for seed in 0..40 {
        let (dir, scratch) = (temp("gen"), temp("gen_out"));
        generated_project(&dir, seed, 25);
        match check_project(&dir, &scratch, lifted, true) {
            Ok(c) => {
                clips += c.alike + c.unstable_load;
                migrated += c.unstable_load;
                blending += c.blending;
                passthrough += c.passthrough;
            }
            Err(e) => failures.push(format!("seed {seed}: {e}")),
        }
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&scratch);
    }
    eprintln!("generated: {clips} clips alike after the lift, {migrated} of them rewritten on load, {blending} blending frames, {passthrough} not graded");
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
    assert!(clips >= 900 && migrated > 0 && blending > 0 && passthrough > 0, "the generator reached too little: {clips} clips, {migrated} migrated, {blending} blending, {passthrough} not graded");
}

/// A clip's picture lifted and brought back by `delift`, as v1 fields:
/// `to_v1(delift(lift(to_v2(x))))`.
fn delifted(clip: &Value) -> Result<Value, String> {
    let record = to_v2::clip_to_v2(clip.as_object().ok_or("a clip")?).map_err(|e| format!("to_v2: {e:?}"))?;
    let clip_id = Uuid::parse_str(clip.get("id").and_then(Value::as_str).ok_or("a clip without an id")?).map_err(|e| e.to_string())?;
    let ctx = LiftCtx { clip_id, kind: ClipKind::from_json(clip.get("kind"))? };
    let (picture, _notes) = lift_clip(&record, &ctx).map_err(|e| format!("lift: {e}"))?;
    let back = delift::clip(&picture).map_err(|e| format!("delift: {e}"))?;
    to_v1::clip(&serde_json::to_value(&back).map_err(|e| e.to_string())?)
}

/// The lift keeps everything (#1089): over generated clips, `delift` gives
/// back the record the lift was given (where today's loader rewrote
/// nothing), the re-lift of what it gives back is the same picture byte
/// for byte, and the v1 picture it gives back renders as today.
#[test]
fn the_lift_keeps_everything_and_delift_gives_it_back() {
    let (mut clips, mut exact, mut rewritten_on_load) = (0, 0, 0);
    let mut failures = Vec::new();
    for seed in 0..40 {
        let (dir, scratch) = (temp("delift"), temp("delift_out"));
        generated_project(&dir, seed, 25);
        if let Err(e) = check_project(&dir, &scratch, delifted, true) {
            failures.push(format!("seed {seed} render: {e}"));
        }
        let project: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("project.json")).unwrap()).unwrap();
        for clip in project["sequences"].as_array().into_iter().flatten().flat_map(|s| s["tracks"].as_array().into_iter().flatten()).flat_map(|t| t["clips"].as_array().into_iter().flatten()) {
            clips += 1;
            let r = (|| -> Result<(), String> {
                let record = to_v2::clip_to_v2(clip.as_object().ok_or("a clip")?).map_err(|e| format!("{e:?}"))?;
                let ctx = LiftCtx { clip_id: Uuid::parse_str(clip["id"].as_str().unwrap()).unwrap(), kind: ClipKind::from_json(clip.get("kind"))? };
                let (p, notes) = lift_clip(&record, &ctx)?;
                let back = delift::clip(&p)?;
                let loaded = notes.iter().any(|n| matches!(n, klipp_graph_format::lifted::LiftNote::LoadMigration { .. } | klipp_graph_format::lifted::LiftNote::DefaultPipeline { .. }));
                if loaded {
                    rewritten_on_load += 1;
                } else {
                    let d = first_diff(&serde_json::to_value(&record).unwrap(), &serde_json::to_value(&back).unwrap(), "record");
                    if !d.is_empty() {
                        return Err(format!("delift(lift(r)) != r: {d}"));
                    }
                    exact += 1;
                }
                let (again, _) = lift_clip(&back, &ctx)?;
                let (a, b) = (serde_json::to_string(&p).unwrap(), serde_json::to_string(&again).unwrap());
                if a != b {
                    return Err(format!("lift(delift(p)) != p: {}", first_diff(&serde_json::to_value(&p).unwrap(), &serde_json::to_value(&again).unwrap(), "")));
                }
                Ok(())
            })();
            if let Err(e) = r {
                failures.push(format!("seed {seed} clip {}: {e}", clip["id"]));
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&scratch);
    }
    eprintln!("delift: {clips} clips, {exact} given back exactly, {rewritten_on_load} rewritten on load and re-lifted exactly");
    if std::env::var_os("DELIFT_ALL").is_some() {
        for f in &failures {
            eprintln!("FAIL {f}");
        }
    }
    let shown: Vec<String> = failures.iter().take(12).cloned().collect();
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), shown.join("\n"));
}

/// The structural round trip, and the lift with its adapter, change
/// nothing today's renderer consumes, over every clip of the projects a
/// klipp-convert scan lists (`KLIPP_SWEEP_SCAN`).
#[test]
#[ignore = "reads every project on this machine; run with KLIPP_SWEEP_SCAN"]
fn the_renderer_consumes_every_real_clip_alike() {
    let Some(scan) = std::env::var_os("KLIPP_SWEEP_SCAN") else {
        eprintln!("skipped: set KLIPP_SWEEP_SCAN to a klipp-convert scan report");
        return;
    };
    let report: Value = serde_json::from_str(&std::fs::read_to_string(&scan).expect("the scan report")).expect("scan JSON");
    let mut failures = Vec::new();
    for (name, through, align) in [("round trip", round_trip as fn(&Value) -> Result<Value, String>, false), ("lift", lifted, true)] {
        let (mut projects, mut clips, mut unreadable, mut unstable) = (0, 0, 0, 0);
        for p in report["projects"].as_array().expect("projects") {
            let dir = PathBuf::from(p["path"].as_str().expect("a path"));
            if !dir.join("project.json").exists() || load(&dir).is_err() {
                unreadable += 1;
                continue;
            }
            let scratch = temp("sweep");
            match check_project(&dir, &scratch, through, align) {
                Ok(c) => {
                    projects += 1;
                    clips += c.alike;
                    unstable += c.unstable_load;
                }
                Err(e) => failures.push(format!("{name}: {}: {e}", dir.display())),
            }
            let _ = std::fs::remove_dir_all(&scratch);
        }
        let how = if align { "matched to the derived ids" } else { "left out" };
        eprintln!("{name}: {projects} projects, {clips} clips alike; {unstable} clips rewritten on load ({how}); {unreadable} unreadable or gone");
    }
    for (name, from_file) in [("splice", false), ("revision-2 file", true)] {
        let (mut projects, mut clips, mut unstable) = (0, 0, 0);
        for p in report["projects"].as_array().expect("projects") {
            let dir = PathBuf::from(p["path"].as_str().expect("a path"));
            if !dir.join("project.json").exists() || load(&dir).is_err() {
                continue;
            }
            let scratch = temp("sweep_spl");
            match check_spliced(&dir, &scratch, from_file) {
                Ok(c) => {
                    projects += 1;
                    clips += c.alike;
                    unstable += c.unstable_load;
                }
                Err(e) => failures.push(format!("{name}: {}: {e}", dir.display())),
            }
            let _ = std::fs::remove_dir_all(&scratch);
        }
        eprintln!("{name}: {projects} projects, {clips} render copies alike; {unstable} rewritten on load (matched)");
    }
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.iter().take(20).cloned().collect::<Vec<_>>().join("\n"));
}

/// A graph with every node id replaced by its place in the node list, in
/// its edges, output and layout: the shape, whoever minted the ids.
fn by_place(g: &Value) -> Value {
    let ids: Vec<String> = g["nodes"].as_array().into_iter().flatten().filter_map(|n| n["id"].as_str().map(str::to_owned)).collect();
    let place = |v: &Value| -> Value { v.as_str().and_then(|s| ids.iter().position(|i| i == s)).map(|i| json!(i)).unwrap_or(v.clone()) };
    let mut g = g.clone();
    for n in g["nodes"].as_array_mut().into_iter().flatten() {
        let p = place(&n["id"]);
        n["id"] = p;
    }
    for e in g["edges"].as_array_mut().into_iter().flatten() {
        let (f, t) = (place(&e["from_node"]), place(&e["to_node"]));
        e["from_node"] = f;
        e["to_node"] = t;
    }
    let out = place(&g["output_node_id"]);
    g["output_node_id"] = out;
    if let Some(pos) = g.get("node_positions").and_then(Value::as_object).cloned() {
        g["node_positions"] = Value::Object(pos.into_iter().map(|(k, v)| (place(&json!(k)).to_string(), v)).collect());
    }
    g
}

/// Where today's loader rewrites a grade (a Color Warper migration, a
/// default pipeline), `delift(lift(r))` gives the graph today's loader
/// holds, node for node and edge for edge; only the ids it mints differ,
/// which are random today and derived in the lift (#1102 review).
#[test]
fn a_rewritten_load_delifts_to_todays_loaded_graph() {
    let (mut rewritten, mut failures) = (0, Vec::new());
    for seed in 0..40 {
        let dir = temp("rewritten");
        generated_project(&dir, seed, 25);
        let raw: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("project.json")).unwrap()).unwrap();
        let seqs = load(&dir).unwrap();
        let today: std::collections::BTreeMap<String, &Clip> = clips(&seqs).into_iter().map(|(c, _)| (c.id.0.to_string(), c)).collect();
        for clip in raw["sequences"].as_array().into_iter().flatten().flat_map(|s| s["tracks"].as_array().into_iter().flatten()).flat_map(|t| t["clips"].as_array().into_iter().flatten()) {
            let record = to_v2::clip_to_v2(clip.as_object().unwrap()).unwrap();
            let ctx = LiftCtx { clip_id: Uuid::parse_str(clip["id"].as_str().unwrap()).unwrap(), kind: ClipKind::from_json(clip.get("kind")).unwrap() };
            let (p, notes) = lift_clip(&record, &ctx).unwrap();
            if !notes.iter().any(|n| matches!(n, klipp_graph_format::lifted::LiftNote::LoadMigration { .. } | klipp_graph_format::lifted::LiftNote::DefaultPipeline { .. })) {
                continue;
            }
            rewritten += 1;
            let ours = to_v1::clip(&serde_json::to_value(delift::clip(&p).unwrap()).unwrap()).unwrap();
            let theirs = serde_json::to_value(today[clip["id"].as_str().unwrap()].grade()).unwrap();
            // Today's typed graph saves defaults the stored form leaves out:
            // both go through today's own type before they're compared.
            let ours: klipp_nodes::NodeGraph = serde_json::from_value(ours["node_graph"].clone()).unwrap();
            let (a, b) = (by_place(&serde_json::to_value(&ours).unwrap()), by_place(&theirs));
            let d = first_diff(&a, &b, "node_graph");
            if !d.is_empty() {
                failures.push(format!("seed {seed} clip {}: {d}", clip["id"]));
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    eprintln!("rewritten loads: {rewritten} clips held to today's loaded graph");
    assert!(rewritten > 100, "the generator reached too few rewritten loads: {rewritten}");
    let shown: Vec<String> = failures.iter().take(8).cloned().collect();
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), shown.join("\n"));
}

/// One malformed change to a v1 project, as a damaged or hand-edited file
/// can hold: a repeated node, made-up ports and ids, a missing output, values
/// of the wrong type, odd effect lists and keys.
fn damage(r: &mut Rng, project: &mut Value) {
    let mut all: Vec<&mut Value> = project["sequences"].as_array_mut().into_iter().flatten().flat_map(|s| s["tracks"].as_array_mut().into_iter().flatten()).flat_map(|t| t["clips"].as_array_mut().into_iter().flatten()).collect();
    if all.is_empty() {
        return;
    }
    let i = r.below(all.len() as u64) as usize;
    let clip = &mut *all[i];
    let junk = || json!("00000000-0000-0000-0000-00000000beef");
    match r.below(10) {
        0 => {
            if let Some(n) = clip["node_graph"]["nodes"].as_array().and_then(|ns| ns.first().cloned()) {
                clip["node_graph"]["nodes"].as_array_mut().unwrap().push(n);
            }
        }
        1 => {
            for e in clip["node_graph"]["edges"].as_array_mut().into_iter().flatten() {
                e["to_port"] = json!(r.below(16));
                e["from_port"] = json!(r.below(4));
            }
        }
        2 => {
            if let Some(es) = clip["node_graph"]["edges"].as_array_mut() {
                es.push(json!({"from_node": junk(), "from_port": 0, "to_node": junk(), "to_port": 3}));
            }
        }
        3 => {
            if clip["node_graph"].is_object() {
                clip["node_graph"]["output_node_id"] = junk();
            }
        }
        4 => {
            for n in clip["node_graph"]["nodes"].as_array_mut().into_iter().flatten() {
                n["params"] = json!(5);
            }
        }
        5 => {
            for n in clip["node_graph"]["nodes"].as_array_mut().into_iter().flatten() {
                n["input_links"] = json!([null, 7, "x"]);
                n["output_link"] = json!("x");
            }
        }
        6 => clip["builtin_effects"] = json!([{"type": "gaussian_blur", "radius": "wide"}, 3, {"type": "opacity"}]),
        7 => clip["builtin_effects_enabled"] = json!([true, "no", null, false, true, true]),
        8 => clip["effect_keyframes"] = json!({"99:radius": [{"time_secs": 0.0, "value": 1.0}], "abc": [], "0:": 5}),
        _ => clip["param_modifiers"] = json!({"effect:x:y": {"modifiers": 3}, "opacity": []}),
    }
}

/// No file makes the conversion panic: over the generated projects, each
/// damaged one way at a time, the conversion and the lift either work or
/// return an error, which stops that project (#1102 review).
#[test]
fn a_damaged_file_never_panics_the_conversion() {
    let mut r = Rng(0x00da_4a6e);
    let (mut runs, mut refused) = (0, 0);
    for seed in 0..20 {
        let dir = temp("damage");
        generated_project(&dir, seed, 10);
        let clean: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("project.json")).unwrap()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        for _ in 0..30 {
            let mut p = clean.clone();
            for _ in 0..1 + r.below(3) {
                damage(&mut r, &mut p);
            }
            runs += 1;
            let outcome = std::panic::catch_unwind(|| {
                let converted = klipp_convert::revision2::project(&p);
                for clip in p["sequences"].as_array().into_iter().flatten().flat_map(|s| s["tracks"].as_array().into_iter().flatten()).flat_map(|t| t["clips"].as_array().into_iter().flatten()) {
                    if let Ok(record) = to_v2::clip_to_v2(clip.as_object().unwrap()) {
                        if let (Some(id), Ok(kind)) = (clip["id"].as_str().and_then(|s| Uuid::parse_str(s).ok()), ClipKind::from_json(clip.get("kind"))) {
                            if let Ok((pic, _)) = lift_clip(&record, &LiftCtx { clip_id: id, kind }) {
                                let _ = pic.graph.check_lifted(false);
                                let _ = delift::clip(&pic).map(|b| to_v1::clip(&serde_json::to_value(b).unwrap()));
                            }
                        }
                    }
                }
                converted.is_err()
            });
            match outcome {
                Ok(true) => refused += 1,
                Ok(false) => {}
                Err(e) => {
                    let why = e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| (*s).to_owned())).unwrap_or_default();
                    panic!("a damaged file panicked the conversion: {why}\n{}", serde_json::to_string(&p).unwrap());
                }
            }
        }
    }
    eprintln!("damaged files: {runs} converted or refused without a panic ({refused} refused)");
    assert!(refused > 0, "the damage reached nothing the conversion refuses");
}

/// `revision2(unlift(v)) == v` byte for byte over the generated projects,
/// the odd cases included: what a save after the switch writes back is the
/// file it read (#1089).
#[test]
fn every_generated_project_comes_back_to_the_same_revision_2_file() {
    let mut projects = 0;
    let mut failures = Vec::new();
    for seed in 0..40 {
        let dir = temp("unlift");
        generated_project(&dir, seed, 25);
        let raw: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("project.json")).unwrap()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let (v2, _) = klipp_convert::revision2::project(&raw).unwrap();
        let back = klipp_convert::unlift::project(&v2).unwrap();
        let (again, _) = klipp_convert::revision2::project(&back).unwrap();
        projects += 1;
        if serde_json::to_string(&v2).unwrap() != serde_json::to_string(&again).unwrap() {
            failures.push(format!("seed {seed}: {}", first_diff(&v2, &again, "")));
        }
    }
    eprintln!("unlift: {projects} generated projects back and forth");
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.iter().take(5).cloned().collect::<Vec<_>>().join("\n"));
}

/// The render check `klipp-convert run` applies before its swap (#1087),
/// for a converted copy at `copy`: the project it came from is beside it.
pub fn render_check(copy: &Path) -> Result<(), String> {
    let name = copy.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let original = copy.with_file_name(name.strip_suffix(".klipp-convert").ok_or("not a converted copy")?);
    let scratch = temp("render_check");
    let r = check_written(&original, copy, &scratch);
    let _ = std::fs::remove_dir_all(&scratch);
    r.map(drop)
}

/// The run's render check passes generated projects in grade groups on
/// tracks with effects and grades, as written to disk; and stops one whose
/// written copy renders otherwise, rolled back.
#[test]
fn the_runs_render_check_holds_the_written_copy_to_the_original() {
    use klipp_convert::run::{run, Hooks, Settings};
    let ok_parity = |_: &Value, _: klipp_convert::StoreKind| -> Result<(), String> { Ok(()) };
    for seed in 0..6 {
        let base = temp(&format!("runcheck{seed}"));
        let dir = base.join("Film.klipp");
        std::fs::create_dir_all(&base).unwrap();
        generated_spliced_project(&dir, seed, 12);
        let s = Settings { projects: vec![dir.clone()], excluded: Default::default(), config_dir: None, backup_root: base.join("backup"), date: "2026-10-06".into() };
        let hooks = Hooks { crash: &|_, _| false, crash_in_move: &|_, _| false, render_check: &render_check, ..Hooks::NONE };
        let report = run(&s, &|| Ok(()), &ok_parity, &hooks).unwrap();
        assert_eq!(report.converted, vec![std::fs::canonicalize(&dir).unwrap()], "seed {seed}: {:?}", report.stopped);
        let _ = std::fs::remove_dir_all(&base);
    }
    // A written copy that renders otherwise: one clip's grade loses its
    // exposure in the file.
    let base = temp("runcheck_bad");
    let dir = base.join("Film.klipp");
    std::fs::create_dir_all(&base).unwrap();
    generated_spliced_project(&dir, 3, 12);
    let sabotage = |copy: &Path| -> Result<(), String> {
        let p = copy.join("project.json");
        let text = std::fs::read_to_string(&p).unwrap();
        std::fs::write(&p, text.replacen("\"exposure\"", "\"exposure_gone\"", 1)).unwrap();
        render_check(copy)
    };
    let s = Settings { projects: vec![dir.clone()], excluded: Default::default(), config_dir: None, backup_root: base.join("backup"), date: "2026-10-06".into() };
    let hooks = Hooks { crash: &|_, _| false, crash_in_move: &|_, _| false, render_check: &sabotage, ..Hooks::NONE };
    let report = run(&s, &|| Ok(()), &ok_parity, &hooks).unwrap();
    assert!(report.converted.is_empty() && report.stopped.len() == 1, "{report:?}");
    assert!(!dir.with_file_name("Film.klipp.klipp-convert").exists(), "rolled back");
    let _ = std::fs::remove_dir_all(&base);
}

/// Whether a v1 file could hold `t`'s type: one the frozen reader knows
/// (types made since, #1746, never were in one).
fn in_v1(t: &klipp_effects::registry::EffectType) -> bool {
    serde_json::from_value::<klipp_convert::v1::effects::BuiltinEffect>(v1_entry(serde_json::to_value((t.sample)()).unwrap())).is_ok()
}
