//! Picture revision 4's rows through a real save and a strict reload
//! (M4 step F): each case is planted through the app's own model, saved by
//! `save_project`, read back by `load_project_strict` (whose stored picture
//! must equal a fresh lift of the clip), and held to what the row stores
//! and what the clip is once loaded. A test on a hand-built shape can miss
//! what the writer really writes (the OFX value's `{"t", "v"}` form did).

use klipp_effects::BuiltinEffect as E;
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::types;
use klipp_media::MediaPool;
use klipp_project::{load_project_strict, save_project};
use klipp_timeline::{Clip, GradeAccess, Sequence};
use serde_json::Value;

fn tc(s: f64) -> klipp_core::RationalTime {
    klipp_core::RationalTime::from_secs_f64(s, 24)
}

fn clip() -> Clip {
    Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0))
}

fn transform(scale_x: f32, scale_y: f32) -> E {
    E::Transform { position_x: 30.0, position_y: 0.0, scale_x, scale_y, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None }
}

/// `c` saved alone in a project and read back strictly: the clip loaded,
/// and its stored picture as the file holds it.
fn saved(name: &str, c: Clip) -> (Clip, Value) {
    let dir = std::env::temp_dir().join(format!("klipp_step_f_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let id = c.id;
    let mut seq = Sequence::new_default();
    seq.tracks[0].clips.push(c);
    save_project(&dir, &[seq], &MediaPool::new()).unwrap_or_else(|e| panic!("{name}: saving: {e}"));
    let file: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("project.json")).unwrap()).unwrap();
    let (sequences, ..) = load_project_strict(&dir).unwrap_or_else(|e| panic!("{name}: a strict reload: {e}"));
    let back = sequences[0].all_clips().find(|c| c.id == id).expect("the clip").clone();
    let stored = file["sequences"][0]["tracks"].as_array().unwrap().iter().flat_map(|t| t["clips"].as_array().unwrap().iter()).find(|c| c["id"] == id.0.to_string()).expect("the clip in the file")["graph_v2"].clone();
    let _ = std::fs::remove_dir_all(&dir);
    (back, stored)
}

/// Every node of a stored picture, its groups' bodies included.
fn nodes(picture: &Value) -> Vec<&Value> {
    fn walk<'a>(g: &'a Value, out: &mut Vec<&'a Value>) {
        for n in g["nodes"].as_array().into_iter().flatten() {
            out.push(n);
            if let Some(b) = n.get("group").and_then(|g| g.get("graph")) {
                walk(b, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(&picture["graph"], &mut out);
    out
}

/// #1406: a Scale Y apart from the width is stored as the aspect and comes
/// back on the Transform.
#[test]
fn an_uneven_scale_is_saved_as_its_aspect() {
    let mut c = clip();
    c.set_effect_list(vec![transform(1.0, 0.5)], vec![true]);
    let (back, stored) = saved("1406", c);
    assert_eq!(stored["placement"]["aspect"], serde_json::json!(0.5), "{}", stored["placement"]);
    assert!(matches!(back.builtin_effects()[0], E::Transform { scale_x: 1.0, scale_y: 0.5, .. }), "{:?}", back.builtin_effects()[0]);
    assert_eq!(back.picture_v2().unwrap().picture.placement.aspect, Some(0.5));
}

/// #1471: a height keyed apart from the width is stored as a lane of its
/// own beside the width's, and comes back as that lane.
#[test]
fn a_height_keyed_apart_is_saved_as_its_own_lane() {
    use klipp_timeline::test_clip::TestClip;
    let scaled = || TestClip::of(clip()).effect(transform(1.0, 0.5)).lane("scale", 0.0, 0.5).lane("scale", 2.0, 1.5);
    let c = scaled().lane("scale_y", 0.0, 0.25).lane("scale_y", 2.0, 0.75).build();
    let lane = |c: &Clip, key: &str| c.picture_v2().unwrap().picture.placement_anim.get(&klipp_graph_format::ParamKey::from(key)).cloned();
    let height = lane(&c, "scale_y").expect("a height lane");
    let (back, stored) = saved("1471", c);
    let values = |lane: &str| stored["placement_anim"][lane]["keys"].as_array().unwrap_or_else(|| panic!("no {lane} lane: {}", stored["placement_anim"])).iter().map(|k| k["value"].as_f64().unwrap()).collect::<Vec<_>>();
    assert_eq!(values("scale"), [0.5, 1.5]);
    assert_eq!(values("scale_y"), [0.25, 0.75]);
    assert_eq!(lane(&back, "scale_y"), Some(height), "the height's keys come back");

    // A height with no lane of its own stores none.
    let (back, stored) = saved("1471-none", scaled().build());
    assert!(stored["placement_anim"].get("scale_y").is_none(), "{}", stored["placement_anim"]);
    assert_eq!(lane(&back, "scale_y"), None);
}

/// #1386: an earlier enabled Opacity is stored on, with its own keys, and
/// the anchor's value is the clip's opacity.
#[test]
fn an_earlier_keyed_opacity_is_saved_on_with_its_keys() {
    let mut c = clip();
    c.set_effect_list(vec![E::Opacity { value: 0.5 }, E::Opacity { value: 0.8 }], vec![true, true]);
    c.edit_effect_keyframes(|k| {
        k.insert("0:value".into(), vec![klipp_core::Keyframe { time_secs: 0.0, value: 0.2, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }]);
    });
    let (back, stored) = saved("1386", c);
    // An enabled node writes no `enabled` (it's the default).
    let on: Vec<&Value> = nodes(&stored).into_iter().filter(|n| n["type_id"] == types::LEGACY_OPACITY && n.get("enabled") != Some(&Value::Bool(false))).collect();
    assert_eq!(on.len(), 1, "the earlier Opacity is on: {on:?}");
    assert!(on[0]["legacy"]["keyframes"].get("0:value").is_some(), "with its own keys: {}", on[0]);
    assert!((stored["opacity"].as_f64().unwrap() - 0.8).abs() < 1e-6, "the anchor's value: {}", stored["opacity"]);
    assert_eq!(back.builtin_effects().len(), 2);
    assert_eq!(back.effect_keyframes().get("0:value").map(Vec::len), Some(1), "its keys come back");
}

/// #1391: a grade compound is stored as a group whose body is its
/// subgraph, and comes back as the compound it was.
#[test]
fn a_grade_compound_is_saved_as_a_group() {
    let mut c = clip();
    let mut inner = klipp_nodes::NodeGraph::default();
    let cor = inner.ensure_corrector();
    if let Some(n) = inner.nodes.iter_mut().find(|n| n.id == cor) {
        n.params.exposure = Some(0.4);
    }
    let kind = klipp_nodes::NodeKind::Compound;
    let params = klipp_nodes::NodeParams { compound_subgraph: Some(inner), ..Default::default() };
    let compound = klipp_nodes::Node { id: klipp_nodes::NodeId::new_v4(), kind, label: kind.display_name().to_string(), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: Vec::new() };
    let compound_id = compound.id;
    let anchor = c.grade().nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Corrector).map(|n| n.id);
    c.insert_grade_node(anchor, compound);
    let (back, stored) = saved("1391", c);
    let group = nodes(&stored).into_iter().find(|n| n["id"] == compound_id.to_string()).expect("the compound in the picture");
    assert_eq!(group["type_id"], lt::GROUP, "{group}");
    assert!(!stored.to_string().contains("compound_subgraph"), "no subgraph stays a payload");
    let held = back.grade().nodes.iter().find(|n| n.id == compound_id).expect("the compound comes back");
    let sub = held.params.compound_subgraph.as_ref().expect("with its subgraph");
    assert!(sub.nodes.iter().any(|n| n.params.exposure == Some(0.4)), "its Corrector's exposure");
}

/// #1409: a Speed Ramp is stored with its speed and blend mode only.
#[test]
fn a_speed_ramp_is_saved_with_speed_and_blend_mode_only() {
    let mut c = clip();
    c.set_effect_list(vec![E::SpeedRamp { speed: 2.0, blend_mode: 1 }], vec![true]);
    let (back, stored) = saved("1409", c);
    let ramp = nodes(&stored).into_iter().find(|n| n["legacy"]["payload"]["type"] == "speed_ramp").expect("the ramp in the picture");
    let payload = ramp["legacy"]["payload"].as_object().unwrap();
    let mut keys: Vec<&str> = payload.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["blend_mode", "speed", "type"], "{payload:?}");
    assert!(matches!(back.builtin_effects()[0], E::SpeedRamp { speed, blend_mode: 1 } if speed == 2.0));
}

/// An OFX effect's context and transition amount come back after a real
/// save and a strict reload (#1144).
#[test]
fn an_ofx_effects_context_and_transition_amount_are_saved() {
    let mut c = clip();
    let wipe = serde_json::from_value(serde_json::json!({"plugin_name": "org.example.Wipe", "enabled": true, "context": "Transition", "transition_amount": 0.35, "param_values": {}})).unwrap();
    c.set_ofx_effects(vec![wipe]);
    let (back, _) = saved("1144", c);
    let e = &back.ofx_effects()[0];
    assert_eq!(e.context, klipp_plugin::OfxCategory::Transition, "the context comes back");
    assert_eq!(e.transition_amount, 0.35, "the transition amount comes back");
}
