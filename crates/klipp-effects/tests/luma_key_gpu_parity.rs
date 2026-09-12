//! GPU-vs-CPU parity for the standalone luminance-threshold key.
//!
//! `klipp_color::luma_key::generate_luma_key_matte` is the production
//! CPU oracle (six unit tests covering hard-cut, soft-edge, invert,
//! disabled). Treating it as authoritative, this test feeds the same
//! synthetic gradient through `GpuEffectsPipeline::apply_luma_key_gpu`
//! and asserts the matte output matches the CPU path within
//! ~2 LSB / 0.001 rate — the cross-implementation tolerance Klipp
//! uses for every CPU↔GPU shader pair.
//!
//! Skipped when no wgpu adapter is available (e.g. CI without a
//! GPU). The skip is **not silent** — we print a one-line warning so
//! a "passing" CI run on the wrong machine doesn't look identical to
//! a real pass.

use klipp_color::luma_key::generate_luma_key_matte;
use klipp_core::LumaKeySettings;
use klipp_effects::gpu::GpuEffectsPipeline;

/// 4×4 fixture — a horizontal luma gradient from 0 to ~1. Small
/// enough to debug pixel-by-pixel if anything drifts; large enough
/// to exercise the workgroup-dispatch path (one workgroup of 256
/// is the default; 16 pixels still hits the skip-tail branch).
fn gradient_rgba_u8(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((width * height * 4) as usize);
    for _y in 0..height {
        for x in 0..width {
            let v = ((x as f32 / (width - 1).max(1) as f32) * 255.0).round() as u8;
            out.push(v);
            out.push(v);
            out.push(v);
            out.push(255);
        }
    }
    out
}

/// The u8 fixture as the linear-light frame the shader is handed in
/// production: sRGB-decoded, so that the shader's own encode brings it
/// back to the bytes the CPU oracle reads.
///
/// This used to hand the shader `byte / 255.0` unconverted, and said so:
/// the shader keyed linear luma while the oracle keyed the bytes, and
/// feeding both the same numbers sidestepped the difference. It also
/// hid it. The same threshold picked a different brightness depending
/// on which path ran — a quarter of a frame apart on a real graph,
/// which `klipp-nodes/tests/secondary_gpu_parity.rs` eventually caught.
/// The shader now keys the display-encoded picture, as the Qualifier's
/// lum window does, so the two paths agree on a *picture* and this test
/// can drive them with one.
fn u8_rgba_to_linear(rgba: &[u8]) -> Vec<f32> {
    rgba.iter()
        .map(|&c| {
            let v = c as f32 / 255.0;
            if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        })
        .collect()
}

/// Inverse of the above — clamp + scale, no sRGB encode.
fn passthrough_f32_to_u8(linear: &[f32]) -> Vec<u8> {
    linear.iter().map(|&v| (v.clamp(0.0, 1.0) * 255.0).round() as u8).collect()
}

/// Pixel-mismatch helper. Each pixel allowed `tolerance` LSB drift
/// per channel; `bad_rate_max` is the max ratio of mismatching
/// pixels (zero by default — we allow no drift since the matte is
/// produced from the same source by the same algorithm). Returns
/// `Some(reason)` on failure.
fn pixel_mismatch(cpu: &[u8], gpu: &[u8], tolerance: u8, bad_rate_max: f64) -> Option<String> {
    if cpu.len() != gpu.len() {
        return Some(format!("len mismatch: cpu {} vs gpu {}", cpu.len(), gpu.len()));
    }
    let mut bad = 0usize;
    let mut max_drift = 0u8;
    for (c, g) in cpu.iter().zip(gpu.iter()) {
        let drift = c.abs_diff(*g);
        if drift > tolerance {
            bad += 1;
            if drift > max_drift {
                max_drift = drift;
            }
        }
    }
    let rate = bad as f64 / (cpu.len() as f64 / 4.0);
    if rate > bad_rate_max {
        Some(format!(
            "{} bad pixels ({:.3}%, max drift {})",
            bad / 4,
            rate * 100.0,
            max_drift,
        ))
    } else {
        None
    }
}

struct Fixture {
    name: &'static str,
    settings: LumaKeySettings,
}

fn fixtures() -> Vec<Fixture> {
    vec![
        Fixture {
            name: "midband_select",
            settings: LumaKeySettings { enabled: true, low: 0.3, high: 0.7, soft: 0.0, invert: false },
        },
        Fixture {
            name: "midband_select_soft",
            settings: LumaKeySettings { enabled: true, low: 0.3, high: 0.7, soft: 0.1, invert: false },
        },
        Fixture {
            name: "highlights_only",
            settings: LumaKeySettings { enabled: true, low: 0.7, high: 1.0, soft: 0.05, invert: false },
        },
        Fixture {
            name: "shadows_only",
            settings: LumaKeySettings { enabled: true, low: 0.0, high: 0.3, soft: 0.05, invert: false },
        },
        Fixture {
            name: "midband_inverted",
            settings: LumaKeySettings { enabled: true, low: 0.3, high: 0.7, soft: 0.0, invert: true },
        },
    ]
}

#[test]
fn gpu_matches_cpu_oracle_across_fixtures() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        eprintln!(
            "[luma_key_gpu_parity] SKIPPED — no wgpu adapter available. \
             This test must run on a machine with a working GPU; a skip \
             on the wrong host masks real shader regressions.",
        );
        return;
    };

    let width = 16u32;
    let height = 4u32;
    let rgba_u8 = gradient_rgba_u8(width, height);
    let linear = u8_rgba_to_linear(&rgba_u8);

    let mut failures: Vec<String> = Vec::new();

    for fx in fixtures() {
        // CPU oracle. Returns one f32 per pixel; expand to u8 RGBA
        // matching the GPU's output convention (R=G=B=matte, A=255).
        let cpu_matte = generate_luma_key_matte(&rgba_u8, width, height, &fx.settings);
        let cpu_rgba: Vec<u8> = cpu_matte.iter()
            .flat_map(|&v| {
                let b = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
                [b, b, b, 255]
            })
            .collect();

        // GPU path. Returns linear-f32 RGBA; convert back to u8 to
        // compare on the same scale as the CPU oracle.
        let gpu_linear = gpu.apply_luma_key_gpu(
            &linear,
            width,
            height,
            fx.settings.low,
            fx.settings.high,
            fx.settings.soft,
            fx.settings.invert,
        );
        let gpu_rgba = passthrough_f32_to_u8(&gpu_linear);

        // 2 LSB tolerance is the cross-implementation drift Klipp
        // uses everywhere. The matte path doesn't need a bad-rate
        // budget since it's a pure threshold function — every pixel
        // should agree.
        if let Some(reason) = pixel_mismatch(&cpu_rgba, &gpu_rgba, 2, 0.0) {
            failures.push(format!("{}: {}", fx.name, reason));
        }
    }

    assert!(
        failures.is_empty(),
        "luma-key GPU parity failures:\n  - {}",
        failures.join("\n  - "),
    );
}
