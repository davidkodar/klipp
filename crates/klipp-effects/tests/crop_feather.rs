//! A crop's feather is in pixels of the sequence's frame (#1030): a
//! feather of N fades the cropped edge over N pixels at the sequence's
//! size, and over N times the render scale at any other.

use klipp_effects::{apply_effects, BuiltinEffect};

/// The alpha `d` pixels in from the crop's left edge, on a `w`-wide
/// frame cropped by a fifth on each side, with the effect scaled to
/// `scale`.
fn alpha_in_from_edge(feather: f32, w: u32, scale: f32, d: u32) -> f32 {
    let h = w / 2;
    let crop = BuiltinEffect::Crop { left: 0.2, right: 0.2, top: 0.0, bottom: 0.0, feather, aspect: 0.0, focus_x: 0.5, focus_y: 0.5 }.at_render_scale(scale);
    let out = apply_effects(&[crop], [0.5, 0.5, 0.5, 1.0].repeat((w * h) as usize), w, h);
    let edge = (0.2 * w as f32).ceil() as u32;
    out[(((h / 2) * w + edge + d) * 4 + 3) as usize]
}

#[test]
fn a_feather_of_n_fades_over_n_pixels_and_scales_with_the_render() {
    // At the sequence's size: half way in at 20 of 40 pixels, full past it.
    assert!((alpha_in_from_edge(40.0, 400, 1.0, 20) - 0.5).abs() < 0.05);
    assert_eq!(alpha_in_from_edge(40.0, 400, 1.0, 45), 1.0);
    // Rendered at half size: the same edge, so half way at 10 pixels.
    assert!((alpha_in_from_edge(40.0, 200, 0.5, 10) - 0.5).abs() < 0.05);
}
