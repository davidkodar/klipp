//! What the corner pin does to a picture: the still pin is a no-op to
//! the byte, a pin into a quarter of the frame lands the picture there
//! at half size, and outside the quadrilateral — and a quadrilateral
//! that is none — there is nothing.

use klipp_effects::{apply_effects, corner_pin, BuiltinEffect};

const W: u32 = 64;
const H: u32 = 32;

/// A picture with a distinct colour at its centre and its corners, on
/// an opaque grey ground.
fn picture() -> Vec<f32> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let px = if x == W / 2 && y == H / 2 {
                [1.0, 0.0, 0.0, 1.0]
            } else if x == 0 && y == 0 {
                [0.0, 1.0, 0.0, 1.0]
            } else {
                [0.3, 0.3, 0.3, 1.0]
            };
            v.extend_from_slice(&px);
        }
    }
    v
}

fn at(frame: &[f32], x: u32, y: u32) -> [f32; 4] {
    let i = ((y * W + x) * 4) as usize;
    [frame[i], frame[i + 1], frame[i + 2], frame[i + 3]]
}

#[test]
fn the_still_pin_leaves_every_byte_alone() {
    let src = picture();
    let out = apply_effects(&[corner_pin::still()], src.clone(), W, H);
    assert_eq!(out, src);
}

#[test]
fn a_quarter_pin_lands_the_picture_at_half_size_and_clears_the_rest() {
    let pin = BuiltinEffect::CornerPin { tl: [0.0, 0.0], tr: [0.5, 0.0], br: [0.5, 0.5], bl: [0.0, 0.5] };
    let out = apply_effects(&[pin], picture(), W, H);
    // The picture's centre (32,16) now sits at the frame's quarter point.
    assert_eq!(at(&out, W / 4, H / 4), [1.0, 0.0, 0.0, 1.0]);
    // Its top-left corner is still the frame's top-left.
    assert_eq!(at(&out, 0, 0), [0.0, 1.0, 0.0, 1.0]);
    // Outside the quadrilateral: transparent.
    assert_eq!(at(&out, 3 * W / 4, 3 * H / 4), [0.0; 4]);
    assert_eq!(at(&out, W - 1, 0), [0.0; 4]);
}

#[test]
fn corners_that_make_no_quadrilateral_draw_nothing() {
    let pin = BuiltinEffect::CornerPin { tl: [0.2, 0.2], tr: [0.2, 0.2], br: [0.2, 0.2], bl: [0.2, 0.2] };
    let out = apply_effects(&[pin], picture(), W, H);
    assert!(out.iter().all(|v| *v == 0.0));
}

/// A trapezoid narrowing to the bottom is a floor in perspective; rows
/// below its horizon are behind the eye and stay empty rather than
/// wrapping the picture back in.
#[test]
fn beyond_the_pins_horizon_the_frame_is_empty() {
    let pin = BuiltinEffect::CornerPin { tl: [0.0, 0.0], tr: [1.0, 0.0], br: [0.6, 0.4], bl: [0.4, 0.4] };
    let out = apply_effects(&[pin], picture(), W, H);
    assert_eq!(at(&out, W / 2, 2), [0.3, 0.3, 0.3, 1.0], "inside the trapezoid the picture is there");
    for y in (3 * H / 4)..H {
        assert_eq!(at(&out, W / 2, y), [0.0; 4], "row {y} is beyond the horizon");
    }
}
