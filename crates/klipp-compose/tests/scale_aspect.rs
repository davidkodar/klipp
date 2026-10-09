//! A clip scaled unevenly draws its height (#1406): the scale lane keys
//! the width, and the height keeps the clip's aspect under it, so a width
//! keyed to 0 gives a height of 0, never a NaN.

use klipp_compose::eval;
use klipp_core::{MediaId, RationalTime, VideoParam};
use klipp_effects::BuiltinEffect as E;
use klipp_timeline::{Clip, ClipKind};

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn clip(scale_x: f32, scale_y: f32) -> Clip {
    let mut c = Clip { kind: ClipKind::Media { media_id: MediaId::new() }, ..Clip::gap(tc(0.0), tc(2.0)) };
    c.set_effect_list(vec![E::Transform { position_x: 0.0, position_y: 0.0, scale_x, scale_y, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None }], vec![true]);
    c
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
