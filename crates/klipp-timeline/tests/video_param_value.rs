//! A new key on a clip's placement or opacity lane starts from the value
//! the renderer uses where nothing animates it (M3 step 4): the clip's
//! picture's placement and opacity, not the first matching entry of its
//! effect list. They differ where the list holds a disabled Opacity before
//! an enabled one, or no Transform at all.

use klipp_core::{RationalTime, VideoParam as P};
use klipp_effects::BuiltinEffect as E;
use klipp_timeline::{Clip, ClipKind};

fn clip(effects: Vec<E>, enabled: Vec<bool>) -> Clip {
    let mut c = Clip { kind: ClipKind::Media { media_id: klipp_core::MediaId::new() }, ..Clip::gap(RationalTime::from_secs_f64(0.0, 24), RationalTime::from_secs_f64(4.0, 24)) };
    c.set_effect_list(effects, enabled);
    c
}

fn transform(x: f32, scale: f32, rotation: f32, tilt: f32) -> E {
    E::Transform { position_x: x, position_y: -5.0, scale_x: scale, scale_y: scale, rotation, anchor_x: 0.5, anchor_y: 0.5, rotation_x: tilt, rotation_y: 0.0, perspective: 0.0, focal_ref: None }
}

#[test]
fn a_lane_starts_from_the_value_the_renderer_uses() {
    // Nothing on the list: identity placement, full opacity.
    let bare = clip(vec![], vec![]);
    for (param, want) in [(P::Opacity, 1.0), (P::PositionX, 0.0), (P::PositionY, 0.0), (P::Scale, 1.0), (P::Rotation, 0.0), (P::TiltX, 0.0), (P::TiltY, 0.0), (P::MatteFeather, 0.0)] {
        assert_eq!(bare.video_param_value(param), want, "{param:?}");
    }
    // The first Transform places the clip; the last enabled Opacity sets
    // its opacity (a disabled one before it doesn't).
    let c = clip(vec![E::Opacity { value: 0.2 }, transform(30.0, 0.8, 12.0, 7.0), E::Opacity { value: 0.6 }, transform(99.0, 2.0, 0.0, 0.0)], vec![false, true, true, true]);
    assert_eq!(c.video_param_value(P::Opacity), 0.6);
    assert_eq!(c.video_param_value(P::PositionX), 30.0);
    assert_eq!(c.video_param_value(P::PositionY), -5.0);
    assert_eq!(c.video_param_value(P::Scale), 0.8);
    assert_eq!(c.video_param_value(P::Rotation), 12.0);
    assert_eq!(c.video_param_value(P::TiltX), 7.0);
}
