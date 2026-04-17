//! Tiny smoke test — boots the GPU pipeline and runs a few effects to
//! exercise every shader we touched in the linear-f32 migration.
//!
//! Run with `cargo run --example gpu_smoke -p klipp-effects`.

use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_effects::BuiltinEffect;

fn main() {
    let _ = tracing_subscriber::fmt().try_init();

    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        eprintln!("GPU pipeline initialization returned None — check vulkan ICD / driver.");
        std::process::exit(1);
    };

    // 2×2 test frame — grey, red, green, blue.
    let w: u32 = 2;
    let h: u32 = 2;
    let frame: Vec<u8> = vec![
        128, 128, 128, 255, // grey
        255,   0,   0, 255, // red
          0, 255,   0, 255, // green
          0,   0, 255, 255, // blue
    ];

    let check = |label: &str, out: &[u8]| {
        if out.len() != (w * h * 4) as usize {
            panic!(
                "{label}: wrong output size {} (expected {})",
                out.len(),
                w * h * 4
            );
        }
        println!("{label}: {:?}", out);
    };

    // Opacity 1.0 — should round-trip near-identical (some drift from
    // sRGB→linear→sRGB round-trip is expected but small).
    let out = gpu.apply_effects(
        &[BuiltinEffect::Opacity { value: 1.0 }],
        frame.clone(),
        w,
        h,
    );
    check("Opacity 1.0", &out);

    // Gaussian blur.
    let out = gpu.apply_effects(
        &[BuiltinEffect::GaussianBlur { radius: 1.0 }],
        frame.clone(),
        w,
        h,
    );
    check("Blur 1px", &out);

    // Invert.
    let out = gpu.apply_effects(
        &[BuiltinEffect::Invert { mix: 1.0 }],
        frame.clone(),
        w,
        h,
    );
    check("Invert", &out);

    // Render transition (cross-dissolve).
    let from = frame.clone();
    let to: Vec<u8> = vec![
        255, 255, 255, 255,
        0, 0, 0, 255,
        128, 128, 128, 255,
        255, 128, 0, 255,
    ];
    let out = gpu.render_transition(&from, &to, 0, 0.5, w, h, 0.0, 0.0, 0.0, 0.0, [0.0; 4]);
    check("Transition 0.5", &out);

    // Scope accumulation (just make sure it doesn't crash).
    let scope = gpu.render_scope_histogram(&frame, w, h, 64, 64);
    if scope.len() != 64 * 64 * 4 {
        panic!("Scope histogram wrong size");
    }
    println!("Scope histogram: ran OK ({} bytes)", scope.len());

    println!("\nGPU smoke test passed — all shaders compile and dispatch.");
}
