//! Four small render faults (#1020), each pinned on the CPU and the GPU.

use klipp_effects::{apply_effects, apply_effects_with_luma, BuiltinEffect, SpillMethod};

const W: u32 = 32;
const H: u32 = 16;
/// The working space's luma weights for ACEScg (AP1) and BT.709.
const AP1: [f32; 3] = [0.272_229, 0.674_082, 0.053_689_5];
const BT709: [f32; 3] = [0.2126, 0.7152, 0.0722];

fn flat(v: [f32; 3]) -> Vec<f32> {
    [v[0], v[1], v[2], 1.0].repeat((W * H) as usize)
}

/// A colourful picture, so weights that differ between spaces matter.
fn colourful() -> Vec<f32> {
    (0..W * H)
        .flat_map(|i| {
            let (x, y) = ((i % W) as f32 / (W - 1) as f32, (i / W) as f32 / (H - 1) as f32);
            [x * 0.9, (1.0 - x) * y * 0.8, (1.0 - y) * 0.7, 1.0]
        })
        .collect()
}

fn gpu() -> Option<klipp_effects::gpu::GpuEffectsPipeline> {
    let g = klipp_effects::gpu::GpuEffectsPipeline::new();
    if g.is_none() {
        eprintln!("no GPU: checking the CPU path only");
    }
    g
}

fn worst(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

/// Spill suppression takes the spill out of the key channel only, on the
/// pixel's own values: a red above white stays where it was.
#[test]
fn spill_suppression_keeps_light_above_white() {
    let key = BuiltinEffect::ChromaKey { key_color: [13, 230, 26], tolerance: 0.05, soft_edge: 0.0, spill_method: SpillMethod::Hawk, spill_strength: 1.0 };
    let input = flat([1.5, 2.0, 0.3]);
    let cpu = apply_effects(std::slice::from_ref(&key), input.clone(), W, H);
    assert_eq!(cpu[3], 1.0, "the pixel is not keyed out");
    assert!((cpu[0] - 1.5).abs() < 1e-4, "red above white survives the spill: {}", cpu[0]);
    assert!((cpu[2] - 0.3).abs() < 1e-4, "blue is untouched: {}", cpu[2]);
    assert!(cpu[1] < 2.0 && cpu[1] > 0.3, "green drops toward the others: {}", cpu[1]);
    if let Some(mut g) = gpu() {
        let gpu = g.apply_effects(std::slice::from_ref(&key), &input, W, H);
        assert!(worst(&cpu, &gpu) < 1e-3, "CPU and GPU agree: cpu {:?}, gpu {:?}", &cpu[..4], &gpu[..4]);
    }
}

/// Equalize weighs luma by the working space, as every other effect does.
#[test]
fn equalize_uses_the_working_spaces_luma() {
    let eq = BuiltinEffect::Equalize { strength: 1.0 };
    let ap1 = apply_effects_with_luma(std::slice::from_ref(&eq), colourful(), W, H, 0.0, AP1);
    let bt709 = apply_effects_with_luma(std::slice::from_ref(&eq), colourful(), W, H, 0.0, BT709);
    assert!(worst(&ap1, &bt709) > 1e-3, "AP1 and BT.709 weights give different equalisations");
    if let Some(mut g) = gpu() {
        g.set_luma_coefs(AP1);
        let gpu = g.apply_effects(std::slice::from_ref(&eq), &colourful(), W, H);
        assert!(worst(&ap1, &gpu) < 1e-3, "CPU and GPU agree in AP1: {}", worst(&ap1, &gpu));
    }
}

/// Posterize below two levels has no band to step to; it is held at two.
#[test]
fn posterize_below_two_levels_is_finite() {
    for levels in [1.0, 0.5, 1.5] {
        let fx = BuiltinEffect::Posterize { levels };
        let cpu = apply_effects(std::slice::from_ref(&fx), colourful(), W, H);
        assert!(cpu.iter().all(|v| v.is_finite()), "levels {levels}: every value finite");
        let two = apply_effects(&[BuiltinEffect::Posterize { levels: 2.0 }], colourful(), W, H);
        assert_eq!(cpu, two, "levels {levels} is two levels");
        if let Some(mut g) = gpu() {
            let gpu = g.apply_effects(std::slice::from_ref(&fx), &colourful(), W, H);
            assert!(gpu.iter().all(|v| v.is_finite()), "levels {levels}: GPU finite");
            assert!(worst(&cpu, &gpu) < 1e-3, "levels {levels}: CPU and GPU agree: {}", worst(&cpu, &gpu));
        }
    }
}
