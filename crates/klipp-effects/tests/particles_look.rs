//! What the Particles effect (#1141) draws, on the path that renders it
//! on the CPU (the parity test holds the GPU to it): seeded and the same
//! every time, moving with the clip's time, each kind its own, and the
//! same picture at any render size.

use klipp_effects::particles::preset;
use klipp_effects::{apply_effects, BuiltinEffect};

const W: u32 = 384;
const H: u32 = 216;

fn grey(w: u32, h: u32) -> Vec<f32> {
    [0.1, 0.1, 0.1, 1.0].repeat((w * h) as usize)
}

fn draw(e: &BuiltinEffect, w: u32, h: u32) -> Vec<f32> {
    apply_effects(std::slice::from_ref(e), grey(w, h), w, h)
}

/// How much of the frame the particles changed.
fn touched(img: &[f32]) -> usize {
    img.chunks_exact(4).filter(|p| (p[0] - 0.1).abs() > 0.01 || (p[2] - 0.1).abs() > 0.01).count()
}

#[test]
fn particles_are_seeded_and_move_with_the_clips_time() {
    for kind in 0..5 {
        let e = preset(kind).with_time(2.0);
        let (a, b) = (draw(&e, W, H), draw(&e, W, H));
        assert_eq!(a, b, "kind {kind}: the same frame every time");
        let n = touched(&a);
        assert!(n > 8 && n < (W * H) as usize / 2, "kind {kind}: some particles, not a wash ({n} pixels)");
        assert_ne!(a, draw(&preset(kind).with_time(2.1), W, H), "kind {kind}: they move");
        let BuiltinEffect::Particles { seed, .. } = e else { unreachable!() };
        let other = match e { BuiltinEffect::Particles { style: kind, amount, size, speed, direction, spread, lifetime, opacity, color, time, .. } => BuiltinEffect::Particles { style: kind, amount, size, speed, direction, spread, lifetime, opacity, color, seed: seed + 1, time }, e => e };
        assert_ne!(a, draw(&other, W, H), "kind {kind}: another seed, another pattern");
    }
    // More of them with the amount; none at no opacity.
    let more = match preset(1).with_time(2.0) { BuiltinEffect::Particles { amount, opacity, .. } => (amount, opacity), _ => unreachable!() };
    let dense = |amount: f32, opacity: f32| match preset(1).with_time(2.0) {
        BuiltinEffect::Particles { style: kind, size, speed, direction, spread, lifetime, color, seed, time, .. } => BuiltinEffect::Particles { style: kind, amount, size, speed, direction, spread, lifetime, opacity, color, seed, time },
        e => e,
    };
    assert!(touched(&draw(&dense(more.0 * 3.0, more.1), W, H)) > touched(&draw(&dense(more.0, more.1), W, H)));
    assert_eq!(touched(&draw(&dense(more.0, 0.0), W, H)), 0);
}

/// Sparks and embers add light; dust, snow and rain lay their colour
/// over: on a bright frame sparks only ever brighten it, while rain,
/// greyer than the frame, darkens where it falls.
#[test]
fn sparks_add_light_and_rain_lays_over() {
    let bright = [0.95f32, 0.95, 0.95, 1.0].repeat((W * H) as usize);
    let on = |e: BuiltinEffect| apply_effects(&[e], bright.clone(), W, H);
    for kind in [3, 4] {
        let img = on(preset(kind).with_time(0.9).denser(3.0));
        assert!(img.chunks_exact(4).all(|p| p[..3].iter().all(|c| *c >= 0.95 - 1e-6)), "kind {kind} only adds light");
        assert!(img.chunks_exact(4).any(|p| p[0] > 0.96), "kind {kind} adds some");
    }
    let rain = on(preset(2).with_time(0.6).denser(3.0));
    assert!(rain.chunks_exact(4).any(|p| p[0] < 0.94), "rain lays its grey over");
}

/// Rendered at half the size the particles are where they were, at half
/// the size: the effect's numbers are a 1080-line frame's, and the
/// lattice scales with the frame.
#[test]
fn half_size_is_the_same_picture() {
    let e = preset(1).with_time(2.5);
    let full = draw(&e, W * 2, H * 2);
    let half = draw(&e, W, H);
    // The full frame boxed down to the half.
    let mut diff = 0.0f32;
    for y in 0..H {
        for x in 0..W {
            for c in 0..3 {
                let at = |xx: u32, yy: u32| full[((yy * W * 2 + xx) * 4 + c) as usize];
                let down = (at(2 * x, 2 * y) + at(2 * x + 1, 2 * y) + at(2 * x, 2 * y + 1) + at(2 * x + 1, 2 * y + 1)) / 4.0;
                diff += (down - half[((y * W + x) * 4 + c) as usize]).abs();
            }
        }
    }
    let mean = diff / (W * H * 3) as f32;
    assert!(mean < 0.01, "the half-size frame is the full one smaller: mean difference {mean}");
}
