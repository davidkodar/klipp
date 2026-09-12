//! Tiny smoke test — boots the GPU pipeline and runs a few effects to
//! exercise every shader we touched in the linear-f32 migration.
//!
//! Run with `cargo run --example gpu_smoke -p klipp-effects`.

use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_effects::{srgb_u8_to_linear_f32_rgba, linear_f32_rgba_to_srgb_u8, BuiltinEffect};

fn main() {
    let _ = tracing_subscriber::fmt().try_init();

    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        eprintln!("GPU pipeline initialization returned None — check vulkan ICD / driver.");
        std::process::exit(1);
    };

    // 2×2 test frame — grey, red, green, blue.
    let w: u32 = 2;
    let h: u32 = 2;
    let frame_u8: Vec<u8> = vec![
        128, 128, 128, 255, // grey
        255,   0,   0, 255, // red
          0, 255,   0, 255, // green
          0,   0, 255, 255, // blue
    ];
    // apply_effects speaks linear-f32 after the edge-to-edge refactor;
    // bridge at the smoke-test boundary so we still print u8 values.
    let frame: Vec<f32> = srgb_u8_to_linear_f32_rgba(&frame_u8);

    let check = |label: &str, out_linear: &[f32]| {
        let out_u8 = linear_f32_rgba_to_srgb_u8(out_linear);
        if out_u8.len() != (w * h * 4) as usize {
            panic!(
                "{label}: wrong output size {} (expected {})",
                out_u8.len(),
                w * h * 4
            );
        }
        println!("{label}: {:?}", out_u8);
    };

    // Opacity 1.0 — should round-trip near-identical (some drift from
    // sRGB→linear→sRGB round-trip is expected but small).
    let out = gpu.apply_effects(
        &[BuiltinEffect::Opacity { value: 1.0 }],
        &frame,
        w,
        h,
    );
    check("Opacity 1.0", &out);

    // Gaussian blur.
    let out = gpu.apply_effects(
        &[BuiltinEffect::GaussianBlur { radius: 1.0 }],
        &frame,
        w,
        h,
    );
    check("Blur 1px", &out);

    // Invert.
    let out = gpu.apply_effects(
        &[BuiltinEffect::Invert { mix: 1.0 }],
        &frame,
        w,
        h,
    );
    check("Invert", &out);

    // Render transition (cross-dissolve). Still takes u8 at this u8-
    // boundary API.
    let from = frame_u8.clone();
    let to: Vec<u8> = vec![
        255, 255, 255, 255,
        0, 0, 0, 255,
        128, 128, 128, 255,
        255, 128, 0, 255,
    ];
    let out_u8 = gpu.render_transition(&from, &to, 0, 0.5, w, h, 0.0, 0.0, 0.0, 0.0, [0.0; 4]);
    if out_u8.len() != (w * h * 4) as usize {
        panic!("Transition 0.5: wrong output size");
    }
    println!("Transition 0.5: {:?}", out_u8);

    // Scope rendering moved to libavfilter (klipp-engine) — the GPU
    // scope kernels no longer exist here, so nothing to smoke-test.

    println!("\nGPU smoke test passed — all shaders compile and dispatch.");
}
