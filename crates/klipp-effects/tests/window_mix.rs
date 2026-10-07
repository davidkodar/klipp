//! A window drawn inside an effect's mix (#1252) is the window drawn into
//! a matte and mixed through it, up to float rounding: the same shapes,
//! the same signed distance and feather (one copy of the WGSL), the same
//! coverage arithmetic. Two shaders compiled apart may still round a value
//! differently (a fused multiply-add here, not there): a window in
//! perspective differed in one value of 55,296 by 1.5e-8. Every shape kind, inverted, part opaque and in perspective,
//! through each mask channel and at partial and whole mixes. A missing GPU
//! leaves it unverified; with `KLIPP_TEST_REQUIRE_GPU=1` it fails.

use klipp_effects::gpu::{EffectMatte, GpuEffectsPipeline, GpuMaskDef};
use klipp_effects::mask_mix::{MaskChannel, MaskMix};
use klipp_effects::BuiltinEffect;

const W: u32 = 64;
/// Float rounding, far below a level (1/255 = 3.9e-3).
const ROUNDING: f32 = 1.0e-6;
const H: u32 = 36;

fn frame() -> Vec<f32> {
    (0..W * H)
        .flat_map(|i| {
            let (x, y) = ((i % W) as f32 / (W - 1) as f32, (i / W) as f32 / (H - 1) as f32);
            [x, 0.2 + 0.6 * y, 1.0 - x * y, 0.25 + 0.75 * y]
        })
        .collect()
}

/// Every shape kind, and the flags that change the coverage.
fn windows() -> Vec<(&'static str, Vec<GpuMaskDef>)> {
    // A shear in perspective, as a tracked window carries.
    let warp = [1.0, 0.08, -0.02, 0.0, 0.95, 0.03, 0.05, 0.02, 1.0];
    vec![
        ("circle", vec![GpuMaskDef::circle(0.5, 0.5, 0.25, 0.1, false, 1.0)]),
        ("rectangle, rotated and inverted", vec![GpuMaskDef::rectangle(0.4, 0.6, 0.5, 0.3, 30.0, 0.05, true, 1.0)]),
        ("gradient, part opaque", vec![GpuMaskDef::linear_gradient(0.1, 0.2, 0.9, 0.8, 0.0, false, 0.6)]),
        ("bezier", vec![GpuMaskDef::bezier(vec![(0.2, 0.2), (0.8, 0.25), (0.7, 0.8), (0.25, 0.7)], 0.08, false, 1.0)]),
        ("circle in perspective", vec![GpuMaskDef::circle(0.45, 0.5, 0.3, 0.1, false, 1.0).with_warp(Some(warp))]),
        ("two shapes", vec![GpuMaskDef::circle(0.3, 0.4, 0.2, 0.05, false, 1.0), GpuMaskDef::rectangle(0.7, 0.6, 0.3, 0.3, 0.0, 0.1, false, 0.8)]),
    ]
}

/// The effect over the frame, mixed by `matte`.
fn run(gpu: &mut GpuEffectsPipeline, mix: MaskMix, matte: EffectMatte<'_>) -> Vec<f32> {
    let n = (W * H) as usize;
    gpu.ensure_chain_buffers(n as u64 * 16);
    gpu.upload_chain_input_linear(&frame());
    gpu.begin_chain();
    let run = gpu.chain_run_effects_mixed(&[BuiltinEffect::GaussianBlur { radius: 2.0 }], true, W, H, mix, Some(matte)).expect("the mix runs");
    assert!(run.complete, "the mix was recorded");
    let out = gpu.flush_chain_and_download_linear(run.in_a, n);
    gpu.end_slot_run();
    out
}

#[test]
fn a_window_inside_the_mix_is_the_window_drawn_into_a_matte() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        assert_ne!(std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref(), Ok("1"), "required GPU unavailable");
        eprintln!("unverified: no GPU pipeline for the window mix");
        return;
    };
    let luma = [0.2126, 0.7152, 0.0722];
    let n = (W * H) as usize;
    for (what, shapes) in windows() {
        // The matte SHADER_MASK_V2 draws, as the route before #1252 had it.
        gpu.ensure_chain_buffers(n as u64 * 16);
        gpu.begin_chain();
        gpu.generate_mask_gpu_to_matte_buf(W, H, &shapes);
        gpu.submit_chain();
        let matte = gpu.read_matte_buf_linear(n);
        assert_eq!(matte.len(), n * 4, "{what}: the matte reads back");
        assert!(matte.iter().any(|v| *v > 0.0) && matte.iter().any(|v| *v < 1.0), "{what}: the window covers part of the frame");
        for channel in [MaskChannel::Red, MaskChannel::Luminance, MaskChannel::Alpha] {
            for amount in [0.4, 1.0] {
                let mix = MaskMix::new(amount, channel, luma).unwrap();
                let through_matte = run(&mut gpu, mix, EffectMatte::Pixels(&matte));
                let inside = run(&mut gpu, mix, EffectMatte::Shapes(&shapes));
                let worst = inside.iter().zip(&through_matte).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
                assert!(worst <= ROUNDING, "{what}, {channel:?} at {amount}: worst {worst:e}");
            }
        }
    }
}
