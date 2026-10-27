//! Every built-in effect stays sane on a picture brighter than white.
//!
//! An HDR delivery renders display light — 1.0 is SDR white at 203 cd/m²,
//! a specular highlight is several times that — and the effects run on
//! that picture. An effect written for `[0, 1]` shows it in three ways:
//! a negative channel (the screen blends inverted, so a glow became a
//! black halo), a highlight crushed to white (anything that clamped its
//! input), or a NaN. This pins all three, on the CPU and the GPU, for
//! every effect.
//!
//! Flat frames, deliberately: a blur or a sharpen legitimately over- and
//! undershoots at an edge, and that would drown the signal here.
//! `cpu_gpu_effect_parity` covers the spatial behaviour.

use klipp_effects::{apply_effects, BuiltinEffect, ColorFadeMode, SpillMethod};

const W: u32 = 48;
const H: u32 = 32;

fn flat(v: [f32; 3]) -> Vec<f32> {
    [v[0], v[1], v[2], 1.0].repeat((W * H) as usize)
}

fn centre(px: &[f32]) -> [f32; 3] {
    let i = ((H / 2 * W + W / 2) * 4) as usize;
    [px[i], px[i + 1], px[i + 2]]
}

/// Whether the effect is meant to keep a highlight above white — most
/// are. The exceptions map the picture onto the display range by
/// definition: inverting a highlight gives black, a fade goes to its
/// colour, a key removes the pixel.
#[derive(PartialEq)]
enum Highlights {
    Keeps,
    Maps,
}

fn cases() -> Vec<(&'static str, BuiltinEffect, Highlights)> {
    use BuiltinEffect::*;
    use Highlights::*;
    vec![
        ("Opacity", Opacity { value: 0.5 }, Keeps),
        ("ColorFade", ColorFade { color: [20, 40, 200, 255], duration_secs: 1.0, mode: ColorFadeMode::In, progress: 0.4 }, Maps),
        ("Crop feathered", Crop { left: 0.1, right: 0.05, top: 0.1, bottom: 0.2, feather: 0.05 }, Keeps),
        ("GaussianBlur", GaussianBlur { radius: 4.0 }, Keeps),
        ("Sharpen", Sharpen { amount: 1.0 }, Keeps),
        ("Transform", Transform {
            position_x: 4.0, position_y: -2.0, scale_x: 0.8, scale_y: 0.8, rotation: 5.0,
            anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None,
        }, Keeps),
        ("ChromaKey", ChromaKey {
            key_color: [13, 230, 26], tolerance: 0.3, soft_edge: 0.1, spill_method: SpillMethod::None, spill_strength: 0.0,
        }, Keeps),
        ("Bloom", Bloom { threshold: 0.7, radius: 6.0, intensity: 0.8 }, Keeps),
        ("Glow", Glow { radius: 6.0, intensity: 0.6, threshold: 0.0 }, Keeps),
        ("LightRays", LightRays { center_x: 0.5, center_y: 0.3, threshold: 0.6, samples: 32, decay: 0.95, intensity: 0.5 }, Keeps),
        ("Vignette", Vignette { amount: 0.6, size: 0.5, softness: 0.5, roundness: 0.5 }, Keeps),
        ("Diffusion", Diffusion { amount: 0.5, radius: 5.0, threshold: 0.4, desaturation: 0.2 }, Keeps),
        ("LensFlare", LensFlare {
            position_x: 0.7, position_y: 0.3, brightness: 0.8, flare_size: 0.3, ghost_count: 4,
            ghost_intensity: 0.4, chromatic_aberration: 0.3,
        }, Keeps),
        ("LightWrap", LightWrap { width: 6.0, intensity: 0.5, softness: 0.5 }, Keeps),
        ("AnamorphicStreak", AnamorphicStreak { threshold: 0.7, length: 60.0, intensity: 0.6, tint: 0.5 }, Keeps),
        ("FilmHalation", FilmHalation { threshold: 0.6, radius: 6.0, intensity: 0.5, hue: 0.05 }, Keeps),
        ("Invert", Invert { mix: 1.0 }, Maps),
        ("Posterize", Posterize { levels: 5.0 }, Keeps),
        ("Mosaic", Mosaic { block_size: 8.0 }, Keeps),
        ("EdgeDetect", EdgeDetect { strength: 1.0, mix: 0.8 }, Keeps),
        ("ChannelMixer", ChannelMixer {
            red_red: 0.8, red_green: 0.2, red_blue: 0.0, green_red: 0.1, green_green: 0.8, green_blue: 0.1,
            blue_red: 0.0, blue_green: 0.3, blue_blue: 0.7,
        }, Keeps),
        ("DirectionalBlur", DirectionalBlur { angle: 30.0, distance: 8.0, samples: 12 }, Keeps),
        ("Emboss", Emboss { angle: 45.0, strength: 1.0, mix: 0.7 }, Keeps),
        ("Mirror", Mirror { horizontal: 1, vertical: 0 }, Keeps),
        ("LensDistortion", LensDistortion { distortion: 0.3, scale: 1.0 }, Keeps),
        ("Displace", Displace { amount: 6.0, size: 24.0, speed: 1.0, direction: 0, seed: 3, time: 0.4 }, Keeps),
        ("Reflection", Reflection { waterline: 0.55, gap: 0.0, ripple_amount: 3.0, ripple_size: 12.0, ripple_speed: 1.0, breakup: 0.5, smear: 0.3, fade: 0.4, opacity: 0.8, darken: 0.0, contact: 0.0, blend: 0, tint: [20, 40, 60], tint_amount: 0.2, tint_auto: false, darken_auto: false, water: [0, 0, 0], water_key: 0, submerge: 1.0, ripple_from: 0, seed: 3, time: 0.4 }, Keeps),
        ("Dilate", Dilate { radius: 2 }, Keeps),
        ("Erode", Erode { radius: 2 }, Keeps),
        ("Denoise", Denoise { spatial_sigma: 2.0, color_sigma: 0.1, iterations: 1 }, Keeps),
        ("Gamma", Gamma { gamma: 1.6 }, Keeps),
        ("HSVAdjust", HSVAdjust { hue_shift: 40.0, saturation: 1.3, value: 0.9 }, Keeps),
        ("Vibrance", Vibrance { amount: 0.5 }, Keeps),
        ("NoiseGenerator", NoiseGenerator { amount: 0.2, monochrome: 0, seed: 7 }, Keeps),
        ("MedianFilter", MedianFilter { radius: 2 }, Keeps),
        ("RadialBlur zoom", RadialBlur { center_x: 0.5, center_y: 0.5, amount: 15.0, samples: 16, mode: 0 }, Keeps),
        ("Levels", Levels { in_black: 0.1, in_white: 0.9, gamma: 1.3, out_black: 0.05, out_white: 0.95 }, Keeps),
        ("Equalize", Equalize { strength: 1.0 }, Keeps),
        ("Letterbox", Letterbox { aspect_ratio: 2.39, bar_color: 0, opacity: 1.0 }, Keeps),
        ("ChromaticAberration", ChromaticAberration { red_shift: 3.0, blue_shift: -3.0, center_x: 0.5, center_y: 0.5 }, Keeps),
    ]
}

/// SDR white, and the same colour at four and twelve times that.
const INPUTS: [[f32; 3]; 3] = [[0.9, 0.35, 0.2], [3.6, 1.4, 0.8], [10.8, 4.2, 2.4]];

#[test]
fn every_effect_survives_a_picture_brighter_than_white() {
    let mut gpu = klipp_effects::gpu::GpuEffectsPipeline::new();
    if gpu.is_none() {
        eprintln!("no GPU: checking the CPU paths only");
    }
    let mut failures = Vec::new();
    for (name, effect, highlights) in cases() {
        let mut out_cpu = Vec::new();
        for input in INPUTS {
            let cpu = apply_effects(std::slice::from_ref(&effect), flat(input), W, H);
            let c = centre(&cpu);
            out_cpu.push(c);
            let bad = |v: f32| !v.is_finite() || v < -0.001;
            if c.iter().copied().any(bad) || cpu.iter().copied().any(bad) {
                failures.push(format!("{name} on {input:?}: CPU gave {c:?}"));
            }
            if let Some(gpu) = gpu.as_mut() {
                let g = centre(&gpu.apply_effects(std::slice::from_ref(&effect), &flat(input), W, H));
                if g.iter().copied().any(bad) {
                    failures.push(format!("{name} on {input:?}: GPU gave {g:?}"));
                }
                let worst = (0..3).map(|i| (c[i] - g[i]).abs()).fold(0.0f32, f32::max);
                // A tenth of a step of SDR white, scaled to the input.
                if worst > 0.1 * input[0].max(1.0) / 255.0 * 10.0 {
                    failures.push(format!("{name} on {input:?}: CPU {c:?} vs GPU {g:?}"));
                }
            }
        }
        let (at_white, at_four, at_twelve) = (out_cpu[0][0], out_cpu[1][0], out_cpu[2][0]);
        eprintln!("{name:22} red at white {at_white:8.3} → 4x {at_four:8.3} → 12x {at_twelve:8.3}");
        if highlights == Highlights::Keeps {
            // The brighter input stays the brighter output, and is not
            // parked on a ceiling: a clamp to white showed up here as
            // 4x and 12x landing on the same value.
            if !(at_four > at_white * 1.5 && at_twelve > at_four * 1.5) {
                failures.push(format!(
                    "{name}: highlights crushed — white {at_white:.3}, 4x {at_four:.3}, 12x {at_twelve:.3}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
