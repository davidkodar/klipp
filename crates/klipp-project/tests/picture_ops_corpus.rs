//! Graph ops written through a clip's fields against the op engine, over
//! every clip of real projects (node-graph M2 step 15a, amendment 11):
//! each effect entry toggled, removed, moved, keyed and unkeyed, an
//! effect added, and a grade parameter set, each one alone on its own
//! copy of the clip, must give the picture the engine makes of the
//! lifted copy.
//!
//! Ignored: it needs `KLIPP_PROJECT_LIST`, a file of project folders, one
//! per line. Each project's own files are read from a copy; nothing is
//! written to the projects.

#[path = "../../klipp-timeline/tests/support/picture_ops_check.rs"]
mod picture_ops_check;

use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_format::{types, Legacy, Node, ParamValue};
use klipp_graph_lift::ops::GraphOp;
use picture_ops_check::{check, effects, picture, Outcome};
use std::path::Path;

/// The ops tried on one clip, each alone.
fn ops_for(c: &klipp_timeline::Clip) -> Vec<GraphOp> {
    let p = picture(c);
    let at = |ids: Vec<uuid::Uuid>| NodePath::new(Root::Clip { clip: c.id.0 }, ids);
    let fx = effects(&p);
    let mut ops = Vec::new();
    for id in &fx {
        let n = p.graph.nodes.iter().find(|n| n.id == *id).unwrap();
        ops.push(GraphOp::SetEnabled { path: at(vec![*id]), enabled: !n.enabled });
        ops.push(GraphOp::RemoveNode { path: at(vec![*id]) });
        let effect: Option<klipp_effects::BuiltinEffect> = n.legacy.as_ref().and_then(|l| serde_json::from_value(l.payload.clone()).ok());
        if let Some(name) = effect.and_then(|e| e.animatable_params().first().copied()) {
            let key = serde_json::json!({"time_secs": 0.5, "value": 0.25, "interpolation": "Linear", "tangent_out": [0.0, 0.0], "tangent_in": [0.0, 0.0]});
            ops.push(GraphOp::AddKeyAt { path: at(vec![*id]), key: name.into(), keyframe: key });
        }
        for key in n.anim.keys() {
            ops.push(GraphOp::SetAnim { path: at(vec![*id]), key: key.clone(), anim: None });
        }
    }
    if let (Some(first), Some(last)) = (fx.first(), fx.last()) {
        if first != last {
            ops.push(GraphOp::MoveAfter { path: at(vec![*first]), after: *last });
            if let Some(display) = p.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::SCENE_TO_DISPLAY) {
                ops.push(GraphOp::MoveAfter { path: at(vec![*last]), after: display.id });
            }
        }
        let blur = Node {
            id: uuid::Uuid::from_u128(0xf00d),
            type_id: types::LEGACY_EFFECT.into(),
            type_version: 1,
            label: String::new(),
            enabled: true,
            params: Default::default(),
            anim: Default::default(),
            mix: None,
            wireless: Default::default(),
            group: None,
            legacy: Some(Legacy { slot: None, payload: serde_json::json!({"type": "gaussian_blur", "radius": 2.0}), keyframes: None, modifiers: Default::default() }),
            legacy_flags: None,
            origin: None,
        };
        ops.push(GraphOp::AddNode { after: at(vec![*last]), node: Box::new(blur) });
    }
    // A grade parameter: the first number on the first grade node with one.
    if let Some(g) = p.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP) {
        let body = &g.group.as_ref().unwrap().graph;
        let set = body.nodes.iter().find_map(|n| {
            let params = n.legacy.as_ref()?.payload.get("params")?.as_object()?;
            let (k, v) = params.iter().find(|(_, v)| v.is_f64())?;
            Some((n.id, k.clone(), v.as_f64()? as f32))
        });
        if let Some((node, key, v)) = set {
            ops.push(GraphOp::SetParam { path: at(vec![g.id, node]), key: key.as_str().into(), value: ParamValue::Float(v + 0.125) });
        }
    }
    ops
}

#[test]
#[ignore = "needs KLIPP_PROJECT_LIST naming project folders"]
fn every_op_on_every_real_clip_is_the_engines() {
    let list = std::env::var_os("KLIPP_PROJECT_LIST").expect("set KLIPP_PROJECT_LIST");
    let scratch = std::env::temp_dir().join(format!("picture-ops-corpus-{}", std::process::id()));
    let (mut clips, mut same, mut refused_by_engine, mut refused_by_clip) = (0, 0, 0, 0);
    let mut differ = Vec::new();
    for (i, dir) in std::fs::read_to_string(list).unwrap().lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let copy = scratch.join(format!("{i}.klipp"));
        std::fs::create_dir_all(&copy).unwrap();
        // The project's own files (project, side stores) and its media
        // manifest, not the media or caches.
        for entry in std::fs::read_dir(dir).unwrap().flatten().filter(|e| e.path().is_file()) {
            std::fs::copy(entry.path(), copy.join(entry.file_name())).unwrap();
        }
        let manifest = Path::new(dir).join("media").join("manifest.json");
        if manifest.is_file() {
            std::fs::create_dir_all(copy.join("media")).unwrap();
            std::fs::copy(&manifest, copy.join("media").join("manifest.json")).unwrap();
        }
        let (sequences, ..) = klipp_project::load_project(&copy).unwrap_or_else(|e| panic!("project {i}: {e}"));
        for c in sequences.iter().flat_map(|s| &s.tracks).flat_map(|t| &t.clips) {
            if c.picture_v2().is_err() {
                continue;
            }
            clips += 1;
            for op in ops_for(c) {
                match check(c, &op) {
                    Outcome::Same => same += 1,
                    Outcome::EngineRefused(_) => refused_by_engine += 1,
                    Outcome::ClipRefused(why) => {
                        refused_by_clip += 1;
                        eprintln!("project {i}: the clip refused what the engine took: {why}: {op:?}");
                    }
                    Outcome::Differ { written, engine } => {
                        if let (true, Ok(dir)) = (differ.is_empty(), std::env::var("PICTURE_OPS_DUMP")) {
                            std::fs::write(format!("{dir}/written.json"), serde_json::to_string_pretty(&written).unwrap()).unwrap();
                            std::fs::write(format!("{dir}/engine.json"), serde_json::to_string_pretty(&engine).unwrap()).unwrap();
                        }
                        differ.push(format!("project {i}, clip {}: {op:?}", c.id.0));
                    }
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    println!("corpus: {clips} clips, {same} ops the same both ways, {refused_by_engine} refused by the engine, {refused_by_clip} refused by the clip only, {} differ", differ.len());
    assert!(differ.is_empty(), "{}", differ.join("\n"));
    assert!(clips > 0 && same > 0);
}
