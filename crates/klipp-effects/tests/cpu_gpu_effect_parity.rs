//! Every built-in effect gives the same picture on the CPU as on the GPU.
//!
//! The GPU shaders define what an effect does: they work on the linear-
//! light frame (chroma key and the LUT, deliberately, on its sRGB
//! encoding). The CPU runs when there is no usable GPU, and for chains
//! with an effect the GPU cannot run. Each case here renders one effect
//! both ways on the same frame and compares the display bytes.

use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_effects::{apply_effects, linear_f32_rgba_to_srgb_u8, BuiltinEffect, ColorFadeMode, SpillMethod};

const W: u32 = 160;
const H: u32 = 90;

/// Gradients, hard edges, saturated patches, a near-white highlight and a
/// near-black shadow: something for every kind of effect to act on.
fn frame() -> Vec<f32> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let (fx, fy) = (x as f32 / (W - 1) as f32, y as f32 / (H - 1) as f32);
            let mut px = [fx, fy, 1.0 - fx * fy, 1.0];
            // Saturated patches.
            if (20..40).contains(&x) && (10..30).contains(&y) {
                px = [0.9, 0.05, 0.05, 1.0];
            }
            if (60..80).contains(&x) && (10..30).contains(&y) {
                px = [0.05, 0.8, 0.1, 1.0];
            }
            if (100..120).contains(&x) && (50..70).contains(&y) {
                px = [0.02, 0.03, 0.9, 1.0];
            }
            // A highlight and a shadow.
            if (130..150).contains(&x) && (10..30).contains(&y) {
                px = [0.98, 0.97, 0.95, 1.0];
            }
            if (10..30).contains(&x) && (60..80).contains(&y) {
                px = [0.004, 0.003, 0.005, 1.0];
            }
            // Fine checker for edge filters.
            if (60..90).contains(&x) && (50..80).contains(&y) && (x / 3 + y / 3) % 2 == 0 {
                px = [0.6, 0.6, 0.6, 1.0];
            }
            v.extend_from_slice(&px);
        }
    }
    v
}

fn cases() -> Vec<(&'static str, BuiltinEffect)> {
    use BuiltinEffect::*;
    vec![
        ("Opacity", Opacity { value: 0.5 }),
        ("ColorFade", ColorFade { color: [20, 40, 200, 255], duration_secs: 1.0, mode: ColorFadeMode::In, progress: 0.4 }),
        ("Crop", Crop { left: 0.1, right: 0.05, top: 0.1, bottom: 0.2, feather: 0.0 }),
        ("Crop feathered", Crop { left: 0.1, right: 0.05, top: 0.1, bottom: 0.2, feather: 0.05 }),
        ("GaussianBlur", GaussianBlur { radius: 4.0 }),
        ("Sharpen", Sharpen { amount: 1.0 }),
        ("Transform", Transform {
            position_x: 10.0, position_y: -5.0, scale_x: 0.8, scale_y: 0.8, rotation: 10.0,
            anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None,
        }),
        ("ChromaKey", ChromaKey {
            key_color: [13, 230, 26], tolerance: 0.3, soft_edge: 0.1, spill_method: SpillMethod::None, spill_strength: 0.0,
        }),
        ("Bloom", Bloom { threshold: 0.7, radius: 6.0, intensity: 0.8 }),
        ("Glow", Glow { radius: 6.0, intensity: 0.6 }),
        ("LightRays", LightRays { center_x: 0.5, center_y: 0.3, threshold: 0.6, samples: 32, decay: 0.95, intensity: 0.5 }),
        ("Vignette", Vignette { amount: 0.6, size: 0.5, softness: 0.5, roundness: 0.5 }),
        ("Diffusion", Diffusion { amount: 0.5, radius: 5.0, threshold: 0.4, desaturation: 0.2 }),
        ("LensFlare", LensFlare {
            position_x: 0.7, position_y: 0.3, brightness: 0.8, flare_size: 0.3, ghost_count: 4,
            ghost_intensity: 0.4, chromatic_aberration: 0.3,
        }),
        ("LightWrap", LightWrap { width: 6.0, intensity: 0.5, softness: 0.5 }),
        ("AnamorphicStreak", AnamorphicStreak { threshold: 0.7, length: 60.0, intensity: 0.6, tint: 0.5 }),
        ("FilmHalation", FilmHalation { threshold: 0.6, radius: 6.0, intensity: 0.5, hue: 0.05 }),
        ("Invert", Invert { mix: 0.7 }),
        ("Posterize", Posterize { levels: 5.0 }),
        ("Mosaic", Mosaic { block_size: 8.0 }),
        ("EdgeDetect", EdgeDetect { strength: 1.0, mix: 0.8 }),
        ("ChannelMixer", ChannelMixer {
            red_red: 0.8, red_green: 0.2, red_blue: 0.0, green_red: 0.1, green_green: 0.8, green_blue: 0.1,
            blue_red: 0.0, blue_green: 0.3, blue_blue: 0.7,
        }),
        ("DirectionalBlur", DirectionalBlur { angle: 30.0, distance: 8.0, samples: 12 }),
        ("Emboss", Emboss { angle: 45.0, strength: 1.0, mix: 0.7 }),
        ("Mirror", Mirror { horizontal: 1, vertical: 0 }),
        ("LensDistortion", LensDistortion { distortion: 0.3, scale: 1.0 }),
        ("Dilate", Dilate { radius: 2 }),
        ("Erode", Erode { radius: 2 }),
        ("Denoise", Denoise { spatial_sigma: 2.0, color_sigma: 0.1, iterations: 1 }),
        ("Gamma", Gamma { gamma: 1.6 }),
        ("HSVAdjust", HSVAdjust { hue_shift: 40.0, saturation: 1.3, value: 0.9 }),
        ("Vibrance", Vibrance { amount: 0.5 }),
        ("NoiseGenerator", NoiseGenerator { amount: 0.2, monochrome: 0, seed: 7 }),
        ("MedianFilter", MedianFilter { radius: 2 }),
        ("RadialBlur zoom", RadialBlur { center_x: 0.5, center_y: 0.5, amount: 15.0, samples: 16, mode: 0 }),
        ("RadialBlur spin", RadialBlur { center_x: 0.5, center_y: 0.5, amount: 10.0, samples: 16, mode: 1 }),
        ("Levels", Levels { in_black: 0.1, in_white: 0.9, gamma: 1.3, out_black: 0.05, out_white: 0.95 }),
        ("Equalize", Equalize { strength: 1.0 }),
        ("Letterbox", Letterbox { aspect_ratio: 2.39, bar_color: 0, opacity: 1.0 }),
        ("ChromaticAberration", ChromaticAberration { red_shift: 3.0, blue_shift: -3.0, center_x: 0.5, center_y: 0.5 }),
    ]
}

/// (worst, mean) channel difference in display bytes.
fn diff(a: &[f32], b: &[f32]) -> (i32, f64) {
    let (a, b) = (linear_f32_rgba_to_srgb_u8(a), linear_f32_rgba_to_srgb_u8(b));
    let mut worst = 0;
    let mut sum = 0u64;
    for (i, (x, y)) in a.iter().zip(&b).enumerate() {
        let d = (*x as i32 - *y as i32).abs();
        if i % 4 == 3 {
            continue;
        }
        worst = worst.max(d);
        sum += d as u64;
    }
    (worst, sum as f64 / (a.len() / 4 * 3) as f64)
}

#[test]
fn every_effect_matches_between_cpu_and_gpu() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        eprintln!("skipping: no GPU");
        return;
    };
    let input = frame();
    let mut failures = Vec::new();
    for (name, effect) in cases() {
        let fx = [effect];
        let cpu = apply_effects(&fx, input.clone(), W, H);
        let on_gpu = gpu.apply_effects(&fx, input.clone(), W, H);
        let (worst, mean) = diff(&cpu, &on_gpu);
        // And each effect does something: a pass-through on one side
        // would match a pass-through on the other.
        let (_, change) = diff(&input, &on_gpu);
        eprintln!("{name:20} CPU vs GPU worst {worst:3} mean {mean:6.3} | change {change:7.3}");
        if worst > 1 {
            failures.push(format!("{name}: CPU and GPU differ by {worst} levels"));
        }
        if change < 0.05 {
            failures.push(format!("{name}: leaves the frame as it was"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
