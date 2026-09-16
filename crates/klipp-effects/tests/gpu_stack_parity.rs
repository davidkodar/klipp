//! The GPU layer stack is the CPU stack: every blend mode at several
//! opacities lands within a level of `blend_layer_linear`, a sprite
//! decodes as `srgb_u8_to_linear_f32_rgba` does, and the first Normal
//! layer moves in untouched.

use klipp_effects::BlendMode;
use klipp_effects::composite::blend_layer_linear;
use klipp_effects::gpu::GpuEffectsPipeline;

const W: u32 = 64;
const H: u32 = 48;

fn pipeline() -> Option<GpuEffectsPipeline> {
    let p = GpuEffectsPipeline::new();
    if p.is_none() {
        eprintln!("skipping: no usable GPU");
    }
    p
}

/// A frame with gradients in every channel, alpha included, and some
/// values above one so the HDR-safe modes are exercised.
fn frame(seed: f32) -> Vec<f32> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let fx = x as f32 / (W - 1) as f32;
            let fy = y as f32 / (H - 1) as f32;
            v.extend_from_slice(&[
                (fx * 1.3 + seed * 0.1).fract() * 1.2,
                (fy + seed * 0.3).fract(),
                ((fx + fy) * 0.5 + seed * 0.2).fract(),
                if seed > 0.5 { (fx * 3.0).fract() } else { 1.0 },
            ]);
        }
    }
    v
}

fn all_modes() -> [BlendMode; 13] {
    [
        BlendMode::Normal, BlendMode::Add, BlendMode::Multiply, BlendMode::Screen, BlendMode::Overlay,
        BlendMode::SoftLight, BlendMode::HardLight, BlendMode::Difference, BlendMode::Exclusion,
        BlendMode::ColorDodge, BlendMode::ColorBurn, BlendMode::Darken, BlendMode::Lighten,
    ]
}

fn worst_level(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max) * 255.0
}

#[test]
fn every_blend_mode_matches_the_cpu_stack() {
    let Some(mut gpu) = pipeline() else { return };
    let base = frame(0.2);
    let top = frame(0.8);
    for mode in all_modes() {
        for opacity in [1.0f32, 0.5, 0.25] {
            let mut cpu = base.clone();
            blend_layer_linear(&mut cpu, &top, opacity, mode);

            assert!(gpu.stack_begin(W, H, 2));
            assert!(gpu.stack_layer_from_linear(0, &base));
            assert!(gpu.stack_layer_from_linear(1, &top));
            assert!(gpu.stack_blend(0, 1.0, 0));
            assert!(gpu.stack_blend(1, opacity, mode as u32));
            assert!(gpu.stack_finish_into_chain(true));
            let got = gpu.stack_download_linear();
            let worst = worst_level(&cpu, &got);
            assert!(worst <= 1.0, "{mode:?} at {opacity}: GPU differs from CPU by {worst:.2} levels");
        }
    }
}

#[test]
fn a_sprite_decodes_as_the_cpu_bridge_does() {
    let Some(mut gpu) = pipeline() else { return };
    let mut rgba = Vec::with_capacity((W * H * 4) as usize);
    for i in 0..(W * H) {
        rgba.extend_from_slice(&[(i % 251) as u8, (i * 7 % 256) as u8, (i * 13 % 256) as u8, (i * 3 % 256) as u8]);
    }
    let cpu = klipp_effects::srgb_u8_to_linear_f32_rgba(&rgba);
    assert!(gpu.stack_begin(W, H, 1));
    assert!(gpu.stack_layer_from_srgb_u8(0, &rgba));
    assert!(gpu.stack_blend(0, 1.0, 0));
    assert!(gpu.stack_finish_into_chain(true));
    let got = gpu.stack_download_linear();
    let worst = worst_level(&cpu, &got);
    assert!(worst <= 1.0, "sprite decode differs by {worst:.2} levels");
    // Straight alpha survives the bridge.
    assert!((got[3] - rgba[3] as f32 / 255.0).abs() < 1e-3);
}

#[test]
fn the_finish_premultiplies_like_the_cpu_stack() {
    let Some(mut gpu) = pipeline() else { return };
    let layer = frame(0.9);
    let mut cpu = layer.clone();
    for px in cpu.chunks_exact_mut(4) {
        let a = px[3].clamp(0.0, 1.0);
        px[0] *= a;
        px[1] *= a;
        px[2] *= a;
        px[3] = 1.0;
    }
    assert!(gpu.stack_begin(W, H, 1));
    assert!(gpu.stack_layer_from_linear(0, &layer));
    assert!(gpu.stack_blend(0, 1.0, 0));
    assert!(gpu.stack_finish_into_chain(false));
    let got = gpu.stack_download_linear();
    let worst = worst_level(&cpu, &got);
    assert!(worst <= 1.0, "premultiply differs by {worst:.2} levels");
}

/// A resident sprite placed by the GPU is the sprite shifted and faded
/// on the CPU and decoded as rows: the same layer, and no second upload
/// for the second placement.
#[test]
fn a_placed_sprite_is_the_shifted_and_faded_upload() {
    let Some(mut gpu) = pipeline() else { return };
    let mut rgba = Vec::with_capacity((W * H * 4) as usize);
    for i in 0..(W * H) {
        rgba.extend_from_slice(&[(i % 251) as u8, (i * 7 % 256) as u8, (i * 13 % 256) as u8, (i * 3 % 256) as u8]);
    }
    let (w, h) = (W as i32, H as i32);
    for (dx, dy, opacity) in [(0, 0, 1.0f32), (5, -3, 1.0), (-7, 4, 0.5), (0, 9, 0.37)] {
        // The CPU twin: `klipp_title`'s `shift_and_fade`.
        let mut shifted = vec![0u8; rgba.len()];
        for y in 0..h {
            for x in 0..w {
                let (sx, sy) = (x - dx, y - dy);
                if sx < 0 || sy < 0 || sx >= w || sy >= h {
                    continue;
                }
                let d = ((y * w + x) * 4) as usize;
                let so = ((sy * w + sx) * 4) as usize;
                shifted[d..d + 4].copy_from_slice(&rgba[so..so + 4]);
                if opacity < 0.9999 {
                    shifted[d + 3] = ((shifted[d + 3] as f32) * opacity).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        let y0 = dy.max(0) as u32;
        let y1 = (h + dy.min(0)).max(0) as u32;
        assert!(gpu.stack_begin(W, H, 1));
        assert!(gpu.stack_layer_from_srgb_u8_rows(0, &shifted, y0, y1));
        assert!(gpu.stack_blend(0, 1.0, 0));
        assert!(gpu.stack_finish_into_chain(true));
        let want = gpu.stack_download_linear();

        assert!(gpu.stack_begin(W, H, 1));
        assert!(gpu.stack_layer_place_sprite(0, 77, &rgba, dx, dy, opacity, y0, y1));
        assert!(gpu.stack_sprite_resident(77));
        assert!(gpu.stack_blend(0, 1.0, 0));
        assert!(gpu.stack_finish_into_chain(true));
        let got = gpu.stack_download_linear();
        let worst = want.iter().zip(&got).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(worst < 1e-5, "placed sprite ({dx}, {dy}, {opacity}) differs from the shifted upload by {worst}");
    }
}
