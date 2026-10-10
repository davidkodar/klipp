//! A keyed corner is read at the frame's time when the clip's effects
//! are resolved, so a pin that moves in the inspector moves in the
//! render; and a still pin is dropped from the chain as the identity it
//! is.

use klipp_compose::render::clip_effects;
use klipp_core::Timecode;
use klipp_effects::{corner_pin, BuiltinEffect};
use klipp_timeline::test_clip::TestClip;
use klipp_timeline::{Clip, Sequence};

#[test]
fn a_keyed_corner_is_where_its_keys_say_at_that_frame() {
    let tc = |s: f64| Timecode::from_secs_f64(s, 24);
    let scene = Sequence::new_default();
    let clip = TestClip::of(Clip::sequence(scene.id, tc(0.0), tc(4.0)))
        .effect(BuiltinEffect::CornerPin { tl: [0.0, 0.0], tr: [0.9, 0.0], br: [1.0, 1.0], bl: [0.0, 1.0] })
        .key(0, "tr_x", 0.0, 0.9)
        .key(0, "tr_x", 2.0, 0.5)
        .key(0, "bl_y", 0.0, 1.0)
        .key(0, "bl_y", 2.0, 0.6)
        .build();

    let (fx, _) = clip_effects(&clip, tc(1.0), 24.0, None, None);
    let pin = fx.iter().find_map(corner_pin::corners).expect("the pin is in the chain");
    assert!((pin[1][0] - 0.7).abs() < 1e-6, "tr_x halfway between its keys: {}", pin[1][0]);
    assert!((pin[3][1] - 0.8).abs() < 1e-6, "bl_y halfway between its keys: {}", pin[3][1]);
    assert_eq!(pin[0], [0.0, 0.0], "an unkeyed corner keeps its value");
}

#[test]
fn a_still_pin_is_not_in_the_chain() {
    let tc = |s: f64| Timecode::from_secs_f64(s, 24);
    let scene = Sequence::new_default();
    let clip = TestClip::of(Clip::sequence(scene.id, tc(0.0), tc(4.0))).effect(corner_pin::still()).build();
    let (fx, _) = clip_effects(&clip, tc(1.0), 24.0, None, None);
    assert!(!fx.iter().any(|e| matches!(e, BuiltinEffect::CornerPin { .. })));
}
