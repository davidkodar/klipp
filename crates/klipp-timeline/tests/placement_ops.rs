//! A clip's placement and opacity set as clip properties, through its
//! PLACEMENT node (M3 step 13): each lands in the field the lift reads it
//! from, so the picture the clip renders is the one the edit meant, and
//! the node's own parameters never hold them.

use klipp_core::{Interpolation, Keyframe, MediaId, RationalTime, VideoParam};
use klipp_effects::BuiltinEffect as E;
use klipp_graph_format::{ParamKey, ParamValue};
use klipp_graph_lift::ops::GraphOp;
use klipp_timeline::Clip;

fn clip(effects: Vec<E>, enabled: Vec<bool>) -> Clip {
    let t = |s| RationalTime::from_secs_f64(s, 24);
    let mut c = Clip::media(MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
    c.set_effect_list(effects, enabled);
    c
}

fn transform(x: f32) -> E {
    E::Transform { position_x: x, position_y: 0.0, scale_x: 1.0, scale_y: 1.0, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None }
}

fn set(c: &mut Clip, key: &str, v: f32) {
    let path = c.placement_path().expect("a placement node");
    c.apply_picture_ops(&[GraphOp::SetParam { path, key: ParamKey::from(key), value: ParamValue::Float(v) }]).unwrap_or_else(|e| panic!("{key}: {e}"));
}

#[test]
fn a_placement_value_lands_on_the_first_enabled_transform() {
    let mut c = clip(vec![E::Gamma { gamma: 1.1 }, transform(5.0), transform(9.0)], vec![true; 3]);
    set(&mut c, "pos_x", 40.0);
    set(&mut c, "scale", 0.5);
    assert!(matches!(c.builtin_effects()[1], E::Transform { position_x: 40.0, scale_x: 0.5, scale_y: 0.5, .. }), "{:?}", c.builtin_effects()[1]);
    assert!(matches!(c.builtin_effects()[2], E::Transform { position_x: 9.0, .. }), "the second is a card, untouched");
    let lifted = c.picture_v2().unwrap();
    assert_eq!((lifted.picture.placement.pos_x, lifted.picture.placement.scale), (Some(40.0), Some(0.5)), "the picture reads it back");
    let placement = lifted.picture.graph.nodes.iter().find(|n| n.type_id.as_str() == klipp_graph_format::lifted::types::PLACEMENT).unwrap();
    assert!(!placement.params.contains_key(&ParamKey::from("pos_x")), "never the node's own parameter");
}

#[test]
fn a_clip_without_a_transform_gets_one() {
    let mut c = clip(vec![E::Gamma { gamma: 1.1 }], vec![true]);
    let address = c.placement_path().unwrap();
    set(&mut c, "rotation", 12.0);
    assert!(matches!(c.builtin_effects()[1], E::Transform { rotation: 12.0, position_x: 0.0, .. }), "an identity Transform at the end, rotated: {:?}", c.builtin_effects());
    assert_eq!(c.picture_v2().unwrap().picture.placement.rotation, Some(12.0));
    // Fixed now, its node under the Transform's id; the address holds.
    assert_eq!(c.placement_path().unwrap(), address, "one address, automatic or fixed");
    c.apply_picture_ops(&[GraphOp::SetParam { path: address, key: "pos_x".into(), value: ParamValue::Float(7.0) }]).unwrap();
    assert!(matches!(c.builtin_effects()[1], E::Transform { rotation: 12.0, position_x: 7.0, .. }), "{:?}", c.builtin_effects());
}

#[test]
fn the_opacity_lands_on_the_entry_the_clip_reads() {
    let mut c = clip(vec![E::Opacity { value: 0.9 }, E::Opacity { value: 0.5 }, E::Opacity { value: 0.2 }], vec![true, true, false]);
    set(&mut c, "opacity", 0.3);
    let values: Vec<f32> = c.builtin_effects().iter().map(|e| match e { E::Opacity { value } => *value, _ => -1.0 }).collect();
    assert_eq!(values, [0.9, 0.3, 0.2], "the last enabled one");
    assert_eq!(c.video_param_value(VideoParam::Opacity), 0.3);
    let mut bare = clip(vec![], vec![]);
    set(&mut bare, "opacity", 0.4);
    assert_eq!(bare.video_param_value(VideoParam::Opacity), 0.4, "one added when there's none");
}

#[test]
fn keys_land_on_the_clips_lanes() {
    let mut c = clip(vec![transform(0.0)], vec![true]);
    let path = c.placement_path().unwrap();
    let key = |t: f64, v: f32| serde_json::to_value(Keyframe { time_secs: t, value: v, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }).unwrap();
    c.apply_picture_ops(&[
        GraphOp::AddKeyAt { path: path.clone(), key: "pos_x".into(), keyframe: key(1.0, 10.0) },
        GraphOp::AddKeyAt { path: path.clone(), key: "pos_x".into(), keyframe: key(0.0, 0.0) },
        GraphOp::AddKeyAt { path: path.clone(), key: "opacity".into(), keyframe: key(2.0, 0.5) },
    ])
    .unwrap();
    assert_eq!(c.video_param_keyframes(VideoParam::PositionX).iter().map(|k| (k.time_secs, k.value)).collect::<Vec<_>>(), [(0.0, 0.0), (1.0, 10.0)]);
    assert_eq!(c.video_param_keyframes(VideoParam::Opacity).len(), 1);
    c.apply_picture_ops(&[GraphOp::RemoveKeyAt { path: path.clone(), key: "pos_x".into(), time_secs: 1.0 }]).unwrap();
    assert_eq!(c.video_param_keyframes(VideoParam::PositionX).len(), 1);
    assert!(c.apply_picture_ops(&[GraphOp::AddKeyAt { path, key: "anchor_x".into(), keyframe: key(0.0, 0.5) }]).is_err(), "an anchor takes no keys");
}

/// A picture-truth clip's placement is its picture's: set there, never in
/// the node's parameters.
#[test]
fn a_picture_truth_clips_placement_is_its_pictures() {
    let mut c = clip(vec![E::Gamma { gamma: 1.1 }, E::GaussianBlur { radius: 3.0 }], vec![true; 2]);
    // A mix on an effect is something only the picture holds.
    let node = c.effect_node_at(false, 1).unwrap();
    let path = klipp_graph_format::path::NodePath::new(klipp_graph_format::path::Root::Clip { clip: c.id.0 }, [node]);
    c.apply_picture_ops(&[GraphOp::SetMix { path, mix: Some(0.5) }]).unwrap();
    assert!(c.is_picture_truth());
    set(&mut c, "pos_x", 25.0);
    set(&mut c, "opacity", 0.6);
    let lifted = c.picture_v2().unwrap();
    assert_eq!(lifted.picture.placement.pos_x, Some(25.0));
    assert_eq!(lifted.picture.opacity, 0.6);
    let placement = lifted.picture.graph.nodes.iter().find(|n| n.type_id.as_str() == klipp_graph_format::lifted::types::PLACEMENT).unwrap();
    assert!(placement.params.keys().all(|k| k.as_str() == "mode"), "{:?}", placement.params);
}

/// A static width of exactly 0 resets the clip's aspect to 1 (a zero width
/// draws nothing, so nothing visible goes); the height is always the width
/// times the aspect. Scale keys through 0 keep it: the aspect is the
/// static Transform's, which a key never writes (#1406).
#[test]
fn a_width_of_zero_resets_the_aspect_and_keys_through_zero_keep_it() {
    let half = || E::Transform { position_x: 0.0, position_y: 0.0, scale_x: 1.0, scale_y: 0.5, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None };
    let mut c = clip(vec![half()], vec![true]);
    set(&mut c, "scale", 0.0);
    assert!(matches!(c.builtin_effects()[0], E::Transform { scale_x: 0.0, scale_y: 0.0, .. }), "{:?}", c.builtin_effects()[0]);
    set(&mut c, "scale", 1.0);
    assert!(matches!(c.builtin_effects()[0], E::Transform { scale_x: 1.0, scale_y: 1.0, .. }), "{:?}", c.builtin_effects()[0]);
    assert_eq!(c.picture_v2().unwrap().picture.placement.aspect, None, "aspect 1");

    let mut keyed = clip(vec![half()], vec![true]);
    let path = keyed.placement_path().unwrap();
    for (t, v) in [(0.0, 1.0), (1.0, 0.0), (2.0, 1.0)] {
        let key = Keyframe { time_secs: t, value: v, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
        keyed.apply_picture_ops(&[GraphOp::AddKeyAt { path: path.clone(), key: ParamKey::from("scale"), keyframe: serde_json::to_value(key).unwrap() }]).unwrap();
    }
    let p = keyed.picture_v2().unwrap().picture.clone();
    assert_eq!(p.placement.aspect, Some(0.5), "keyed through 0, the aspect holds");
    assert_eq!(p.placement_anim[&ParamKey::from("scale")].keys.len(), 3);
}

/// The height takes keys of its own (#1471): they land on the clip's
/// height lane, the picture holds them beside the width's, and the last
/// one taken off leaves the height following the width again.
#[test]
fn the_height_takes_keys_of_its_own() {
    let half = E::Transform { position_x: 0.0, position_y: 0.0, scale_x: 1.0, scale_y: 0.5, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None };
    let mut c = clip(vec![half], vec![true]);
    let path = c.placement_path().unwrap();
    let key = |t: f64, v: f32| serde_json::to_value(Keyframe { time_secs: t, value: v, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }).unwrap();
    c.apply_picture_ops(&[
        GraphOp::AddKeyAt { path: path.clone(), key: "scale_y".into(), keyframe: key(0.0, 0.25) },
        GraphOp::AddKeyAt { path: path.clone(), key: "scale_y".into(), keyframe: key(2.0, 0.75) },
    ])
    .unwrap();
    let p = c.picture_v2().unwrap().picture.clone();
    let values: Vec<f64> = p.placement_anim[&ParamKey::from("scale_y")].keys.iter().map(|k| k["value"].as_f64().unwrap()).collect();
    assert_eq!(values, [0.25, 0.75]);
    assert!(!p.placement_anim.contains_key(&ParamKey::from("scale")), "the width's lane is its own");
    assert_eq!(p.placement.aspect, Some(0.5), "the aspect stays for when the lane goes");
    for t in [0.0, 2.0] {
        c.apply_picture_ops(&[GraphOp::RemoveKeyAt { path: path.clone(), key: "scale_y".into(), time_secs: t }]).unwrap();
    }
    assert!(!c.picture_v2().unwrap().picture.placement_anim.contains_key(&ParamKey::from("scale_y")));
}
