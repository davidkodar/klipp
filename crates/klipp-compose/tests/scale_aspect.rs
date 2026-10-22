//! A clip scaled unevenly draws its height (#1406): the scale lane keys
//! the width, and the height keeps the clip's aspect under it, so a width
//! keyed to 0 gives a height of 0, never a NaN.

use klipp_compose::eval;
use klipp_core::{MediaId, RationalTime, VideoParam};
use klipp_effects::BuiltinEffect as E;
use klipp_timeline::{Clip, ClipKind};
use klipp_timeline::test_clip::TestClip;

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn clip(scale_x: f32, scale_y: f32) -> Clip {
    let c = Clip { kind: ClipKind::Media { media_id: MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    TestClip::of(c).effect(E::Transform { position_x: 0.0, position_y: 0.0, scale_x, scale_y, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None }).build()
}

/// The width and height the clip is drawn at, at `secs`.
fn scales(c: &Clip, secs: f64) -> (f32, f32) {
    let t = eval::evaluate_clip_own_transform(c, secs, &eval::eval_context(c, tc(secs), 24.0));
    (t.2, t.3)
}

#[test]
fn an_uneven_scale_draws_its_height() {
    assert_eq!(scales(&clip(1.0, 0.5), 1.0), (1.0, 0.5));
    assert_eq!(scales(&clip(2.0, 0.5), 1.0), (2.0, 0.5));
    // Even, as before.
    assert_eq!(scales(&clip(0.8, 0.8), 1.0), (0.8, 0.8));
}

#[test]
fn keys_on_the_scale_key_both_axes_at_the_clip_aspect() {
    // Aspect 0.5: a key of 2 on the width draws a height of 1.
    let mut c = clip(1.0, 0.5);
    c.edit_video_param_keyframes(VideoParam::Scale, |k| {
        klipp_core::keyframe::upsert_keyframe(k, 0.0, 2.0);
        klipp_core::keyframe::upsert_keyframe(k, 2.0, 2.0);
    });
    assert_eq!(scales(&c, 1.0), (2.0, 1.0));
}

#[test]
fn a_width_keyed_to_0_gives_a_height_of_0() {
    let mut c = clip(1.0, 0.5);
    c.edit_video_param_keyframes(VideoParam::Scale, |k| {
        klipp_core::keyframe::upsert_keyframe(k, 0.0, 0.0);
        klipp_core::keyframe::upsert_keyframe(k, 2.0, 0.0);
    });
    let (w, h) = scales(&c, 1.0);
    assert_eq!((w, h), (0.0, 0.0), "a height of {h}");
    // A static width of 0 has no aspect to keep: it draws nothing, at a
    // height of 0, never a NaN.
    assert_eq!(scales(&clip(0.0, 0.5), 1.0), (0.0, 0.0));
}

/// `c` with its width keyed 0.5 to 1.5 and its height keyed apart from
/// it, 0.25 to 0.75, over its two seconds (#1471).
fn keyed_apart(c: Clip) -> Clip {
    TestClip::of(c).lane("scale", 0.0, 0.5).lane("scale", 2.0, 1.5).lane("scale_y", 0.0, 0.25).lane("scale_y", 2.0, 0.75).build()
}

#[test]
fn a_height_keyed_apart_follows_its_own_keys() {
    let c = keyed_apart(clip(1.0, 2.0));
    // Between the keys each axis follows its own lane, whatever the
    // aspect (2 here) would have made of the width.
    assert_eq!(scales(&c, 1.0), (1.0, 0.5));
    assert_eq!(scales(&c, 0.5), (0.75, 0.375));
    // The picture carries the lane, so the renderer reads it there.
    let lifted = c.picture_v2().unwrap();
    assert_eq!(lifted.picture.placement_anim[&klipp_graph_format::ParamKey::from("scale_y")].keys.len(), 2);
}

#[test]
fn without_its_lane_the_height_follows_the_width_at_the_aspect_again() {
    use klipp_graph_format::op::GraphOp;
    let mut c = keyed_apart(clip(1.0, 2.0));
    let path = c.placement_path().unwrap();
    c.apply_picture_ops(&[GraphOp::SetAnim { path, key: "scale_y".into(), anim: None }]).unwrap();
    assert!(!c.picture_v2().unwrap().picture.placement_anim.contains_key(&klipp_graph_format::ParamKey::from("scale_y")), "the lane is gone from the picture");
    assert_eq!(scales(&c, 1.0), (1.0, 2.0));
}

/// A picture-truth clip (one holding what its fields can't) keys its
/// height through its PLACEMENT node, as the inspector does, and draws it
/// from its picture's lane.
#[test]
fn a_picture_truth_clip_keys_its_height_through_its_placement() {
    use klipp_graph_format::op::GraphOp;
    let mut c = TestClip::of(clip(1.0, 1.0)).effect(E::GaussianBlur { radius: 3.0 }).build();
    let blur = c.effect_node_at(false, 1).unwrap();
    let blur = klipp_graph_format::path::NodePath::new(klipp_graph_format::path::Root::Clip { clip: c.id.0 }, [blur]);
    c.apply_picture_ops(&[GraphOp::SetMix { path: blur, mix: Some(0.5) }]).unwrap();
    assert!(c.is_picture_truth());
    let path = c.placement_path().unwrap();
    let key = |t: f64, v: f32| serde_json::to_value(klipp_core::Keyframe { time_secs: t, value: v, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }).unwrap();
    c.apply_picture_ops(&[
        GraphOp::AddKeyAt { path: path.clone(), key: "scale_y".into(), keyframe: key(0.0, 0.25) },
        GraphOp::AddKeyAt { path: path.clone(), key: "scale_y".into(), keyframe: key(2.0, 0.75) },
    ])
    .unwrap();
    assert!(c.is_picture_truth());
    assert_eq!(scales(&c, 1.0), (1.0, 0.5));
    for t in [0.0, 2.0] {
        c.apply_picture_ops(&[GraphOp::RemoveKeyAt { path: path.clone(), key: "scale_y".into(), time_secs: t }]).unwrap();
    }
    assert_eq!(scales(&c, 1.0), (1.0, 1.0), "the width's again");
}
