//! A clip's placement keeps its aspect (#1406), and its animation names
//! only the lanes this reader draws: a lane added later is refused, never
//! read with it dropped.

use klipp_graph_format::lifted::ClipPicture;
use serde_json::json;

fn picture(placement: serde_json::Value, anim: serde_json::Value) -> serde_json::Value {
    json!({ "graph": { "graph_version": 2, "nodes": [], "edges": [], "output": "00000000-0000-0000-0000-000000000000" }, "placement": placement, "placement_anim": anim, "opacity": 1.0 })
}

#[test]
fn a_placement_keeps_its_aspect() {
    let p: ClipPicture = serde_json::from_value(picture(json!({ "scale": 1.0, "aspect": 0.5 }), json!({}))).expect("an aspect reads");
    assert_eq!((p.placement.scale, p.placement.aspect), (Some(1.0), Some(0.5)));
    // Written back only where it isn't 1 (absent).
    let even: ClipPicture = serde_json::from_value(picture(json!({ "scale": 1.0 }), json!({}))).unwrap();
    assert!(!serde_json::to_string(&even).unwrap().contains("aspect"));
}

#[test]
fn a_placement_lane_this_reader_doesnt_draw_is_refused() {
    let lane = json!({ "keys": [] });
    assert!(serde_json::from_value::<ClipPicture>(picture(json!({}), json!({ "scale": lane }))).is_ok(), "a lane it draws reads");
    let err = serde_json::from_value::<ClipPicture>(picture(json!({}), json!({ "scale_y": lane }))).expect_err("a height keyed apart is refused");
    assert!(err.to_string().contains("unsupported placement animation"), "{err}");
}
