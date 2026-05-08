//! GPU-vs-CPU parity for the standalone soft-clip transform.
//!
//! `klipp_color::soft_clip::apply_soft_clip` is the production CPU
//! oracle (seven unit tests covering hard-clip, smooth rolloff,
//! threshold-at-1, alpha preservation, batch independence).
//! Treating it as authoritative, this test feeds the same synthetic
//! gradient through `GpuEffectsPipeline::apply_soft_clip_gpu` and
//! asserts the output matches within ~2 LSB across a handful of
//! threshold/knee fixtures.
//!
//! Skipped when no wgpu adapter is available (e.g. CI without a
//! GPU). The skip is **not silent** — we print a one-line warning
//! so a "passing" CI run on the wrong machine doesn't look like a
//! real pass.

use klipp_color::soft_clip::apply_soft_clip;
use klipp_core::SoftClipSettings;
use klipp_effects::gpu::GpuEffectsPipeline;

/// 4×4 fixture — horizontal greyscale gradient from 0 to ~1.25 so
/// some values fall above the soft-clip threshold and exercise the
/// rolloff curve. Scaled by 1.25 / (width-1) so pixel 15 hits 1.25
/// (clearly past 1.0 to test the asymptotic clamp).
fn gradient_rgba_f32(width: u32, height: u32) -> Vec<f32> {
    let mut out = Vec::with_capacity((width * height * 4) as usize);
    for _y in 0..height {
        for x in 0..width {
            let v = (x as f32 / (width - 1).max(1) as f32) * 1.25;
            out.push(v);
            out.push(v);
            out.push(v);
            out.push(1.0);
        }
    }
    out
}

/// Pixel-mismatch helper. Per-component LSB-equivalent tolerance
/// converted to f32 (2 LSB ≈ 0.0078). Returns Some(reason) on
/// failure.
fn f32_mismatch(cpu: &[f32], gpu: &[f32], tolerance: f32) -> Option<String> {
    if cpu.len() != gpu.len() {
        return Some(format!("len mismatch: cpu {} vs gpu {}", cpu.len(), gpu.len()));
    }
    let mut max_diff = 0.0_f32;
    let mut bad = 0usize;
    for (c, g) in cpu.iter().zip(gpu.iter()) {
        let d = (c - g).abs();
        if d > tolerance {
            bad += 1;
            if d > max_diff {
                max_diff = d;
            }
        }
    }
    if bad > 0 {
        Some(format!("{} components above {tolerance:.5} (max diff {max_diff:.5})", bad))
    } else {
        None
    }
}

struct Fixture {
    name: &'static str,
    settings: SoftClipSettings,
}

fn fixtures() -> Vec<Fixture> {
    vec![
        Fixture {
            name: "broadcast_typical",
            settings: SoftClipSettings { enabled: true, threshold: 0.95, knee: 0.05 },
        },
        Fixture {
            name: "hard_clip_85",
            settings: SoftClipSettings { enabled: true, threshold: 0.85, knee: 0.0 },
        },
        Fixture {
            name: "soft_rolloff_wide",
            settings: SoftClipSettings { enabled: true, threshold: 0.7,  knee: 0.3 },
        },
        Fixture {
            name: "threshold_at_one",
            settings: SoftClipSettings { enabled: true, threshold: 1.0,  knee: 0.05 },
        },
    ]
}

#[test]
fn gpu_matches_cpu_oracle_across_fixtures() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        eprintln!(
            "[soft_clip_gpu_parity] SKIPPED — no wgpu adapter available. \
             This test must run on a machine with a working GPU; a skip \
             on the wrong host masks real shader regressions.",
        );
        return;
    };

    let width = 16u32;
    let height = 4u32;
    let linear = gradient_rgba_f32(width, height);

    let mut failures: Vec<String> = Vec::new();

    for fx in fixtures() {
        let cpu_out = apply_soft_clip(&linear, width, height, &fx.settings);
        let gpu_out = gpu.apply_soft_clip_gpu(
            linear.clone(),
            width, height,
            fx.settings.threshold,
            fx.settings.knee,
        );

        // 2 LSB equivalent in f32. Our pipeline operates entirely in
        // linear-f32, so the only drift comes from SIMD rounding on
        // the GPU's `exp` implementation vs. the CPU `f32::exp`.
        // 0.001 is conservative for a single-channel exponential.
        if let Some(reason) = f32_mismatch(&cpu_out, &gpu_out, 0.001) {
            failures.push(format!("{}: {}", fx.name, reason));
        }
    }

    assert!(
        failures.is_empty(),
        "soft-clip GPU parity failures:\n  - {}",
        failures.join("\n  - "),
    );
}
