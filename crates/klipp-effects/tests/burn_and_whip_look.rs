//! What the film burn and the whip pan look like, on the path that
//! renders them (`render_transition_linear`, the shader's body; the parity
//! test holds the GPU to it).

use klipp_core::spec_for_kind;
use klipp_core::TransitionKind;

const W: u32 = 96;
const H: u32 = 54;

fn render(kind: TransitionKind, from: &[f32], to: &[f32], p: f32) -> Vec<f32> {
    let spec = spec_for_kind(kind);
    let u = spec.default_params;
    klipp_effects::render_transition_linear(from, to, spec.kind_id as u32, p, W, H, u.param0, u.param1, u.param2, u.param3, u.color)
}

fn solid(v: f32) -> Vec<f32> {
    [v, v, v, 1.0].repeat((W * H) as usize)
}

/// Mean brightness of the columns `x0..x1` over every row.
fn mean_cols(img: &[f32], x0: u32, x1: u32) -> f32 {
    let mut sum = 0.0;
    for y in 0..H {
        for x in x0..x1 {
            let i = ((y * W + x) * 4) as usize;
            sum += (img[i] + img[i + 1] + img[i + 2]) / 3.0;
        }
    }
    sum / ((x1 - x0) * H) as f32
}

/// #511: the burn is a hot spot with a shape, from the right edge, not a
/// flat grain field over the whole frame; it is only the pictures at the
/// ends.
#[test]
fn a_film_burn_is_a_shaped_hot_spot() {
    let (a, b) = (solid(0.1), solid(0.2));
    let start = render(TransitionKind::FilmBurn, &a, &b, 0.0);
    let end = render(TransitionKind::FilmBurn, &a, &b, 1.0);
    assert!(start.iter().zip(&a).all(|(x, y)| (x - y).abs() < 1e-3), "the outgoing picture at the start");
    assert!(end.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-3), "the incoming picture at the end");

    let mid = render(TransitionKind::FilmBurn, &a, &b, 0.5);
    let (left, right) = (mean_cols(&mid, 0, W / 4), mean_cols(&mid, W * 3 / 4, W));
    assert!(right > left + 0.3, "the hot spot is on the right: {left:.2} left, {right:.2} right");
    // Linear light: 0.4 is about two thirds of the way up on screen, from
    // pictures at 0.1 and 0.2.
    assert!(right > 0.4, "and burns bright: {right:.2}");
    // Warm: the burn's red is above its blue where it is lit.
    let i = ((H / 3 * W + W - 4) * 4) as usize;
    assert!(mid[i] > mid[i + 2], "warm, not white-blue: {:?}", &mid[i..i + 3]);
}

/// #512: a whip pan is a push smeared along the move at the cut, not a
/// dissolve; it is the pictures at the ends.
#[test]
fn a_whip_pan_smears_along_the_move_at_the_cut() {
    // Fine vertical stripes on the outgoing picture: a push without blur
    // would keep them sharp.
    let mut stripes = Vec::with_capacity((W * H * 4) as usize);
    for _y in 0..H {
        for x in 0..W {
            let v = if (x / 2) % 2 == 0 { 0.0 } else { 1.0 };
            stripes.extend_from_slice(&[v, v, v, 1.0]);
        }
    }
    let flat = solid(0.5);
    let start = render(TransitionKind::WhipPan, &stripes, &flat, 0.0);
    let end = render(TransitionKind::WhipPan, &stripes, &flat, 1.0);
    assert!(start.iter().zip(&stripes).all(|(x, y)| (x - y).abs() < 1e-3), "the outgoing picture at the start");
    assert!(end.iter().zip(&flat).all(|(x, y)| (x - y).abs() < 1e-3), "the incoming picture at the end");

    // Contrast along a row of the outgoing part of the frame.
    let contrast = |img: &[f32]| {
        let row = H / 2;
        let vals: Vec<f32> = (0..W / 4).map(|x| img[((row * W + x) * 4) as usize]).collect();
        vals.iter().cloned().fold(0.0f32, f32::max) - vals.iter().cloned().fold(1.0f32, f32::min)
    };
    let early = render(TransitionKind::WhipPan, &stripes, &flat, 0.02);
    let mid = render(TransitionKind::WhipPan, &stripes, &flat, 0.5);
    assert!(contrast(&early) > 0.8, "just after the start the stripes are still there: {:.2}", contrast(&early));
    assert!(contrast(&mid) < 0.25, "at the cut they are smeared out: {:.2}", contrast(&mid));
}
