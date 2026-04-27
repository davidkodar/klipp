//! GPU-vs-CPU parity for the mask matte rasteriser.
//!
//! `klipp_color::masks::generate_mask_matte` is the production CPU
//! rasteriser, with 70+ unit tests in its own module pinning down the
//! correct output across circle / rectangle / linear-gradient / bezier
//! shapes plus feathering, inversion, opacity, and multi-mask union.
//! Treating that as the **oracle**, this test runs the same fixture
//! masks through the GPU path (`GpuEffectsPipeline::generate_mask_gpu`)
//! and asserts the output is within ~2/255 per-channel everywhere with
//! at most a 0.1 % bad-pixel halo — the band that catches real shader
//! bugs while tolerating the tiny edge-pixel drift that's inherent to
//! antialiasing rounding diff between scalar CPU and SIMD GPU paths.
//!
//! Skipped when no wgpu adapter is available (CI without a GPU). The
//! skip is **not silent** — we print a one-line warning so a "passing"
//! CI run on the wrong machine doesn't look identical to a real pass.

use klipp_color::masks::generate_mask_matte;
use klipp_core::{Mask, MaskShape, MaskStack};
use klipp_effects::gpu::{GpuEffectsPipeline, GpuMaskDef};
use klipp_testing::snapshot::{assert_matches_golden_png_in, Tolerance};
use std::path::PathBuf;

/// Resolve the goldens directory relative to this crate's manifest.
/// Lives at `crates/klipp-effects/goldens/mask_gpu_parity/<name>.png`.
fn goldens_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("goldens")
        .join("mask_gpu_parity")
}

/// Convert a single CPU `Mask` to a `GpuMaskDef`. Returns `None` for
/// shapes the GPU pipeline doesn't support yet — the parity test
/// uses that to detect "GPU is missing this case, expected to fail."
fn mask_to_gpu_def(mask: &Mask) -> Option<GpuMaskDef> {
    if !mask.enabled {
        return None;
    }
    match &mask.shape {
        MaskShape::Circle { center_x, center_y, radius } => Some(GpuMaskDef::circle(
            *center_x, *center_y, *radius,
            mask.feather, mask.invert, mask.opacity,
        )),
        MaskShape::Rectangle { center_x, center_y, width, height, rotation } => {
            Some(GpuMaskDef::rectangle(
                *center_x, *center_y, *width, *height, *rotation,
                mask.feather, mask.invert, mask.opacity,
            ))
        }
        MaskShape::LinearGradient { start_x, start_y, end_x, end_y } => {
            Some(GpuMaskDef::linear_gradient(
                *start_x, *start_y, *end_x, *end_y,
                mask.feather, mask.invert, mask.opacity,
            ))
        }
        MaskShape::Bezier { points } => Some(GpuMaskDef::bezier(
            points.clone(),
            mask.feather, mask.invert, mask.opacity,
        )),
    }
}

/// Convert an f32 single-channel matte (0..1) to RGBA8 (R=G=B=matte,
/// A=255). Mirrors what the GPU shader writes so the comparison is
/// apples-to-apples.
fn matte_f32_to_rgba8(matte: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(matte.len() * 4);
    for v in matte {
        let m = (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        out.extend_from_slice(&[m, m, m, 255]);
    }
    out
}

struct ParityCase {
    name: &'static str,
    width: u32,
    height: u32,
    masks: MaskStack,
    /// `true` when at least one mask in the stack is currently
    /// expected to be missing from the GPU path (bezier today).
    /// The test treats these as "expected failure" — the assertion
    /// flips to a strict parity check the moment the shader gains
    /// the missing case.
    expects_gpu_gap: bool,
}

fn fixtures() -> Vec<ParityCase> {
    vec![
        // ── Single-shape sanity ──
        ParityCase {
            name: "circle_centered_no_feather",
            width: 96, height: 64,
            masks: MaskStack { masks: vec![Mask {
                shape: MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.25 },
                feather: 0.0, invert: false, opacity: 1.0, enabled: true,
                keyframes: Default::default(),
            }] },
            expects_gpu_gap: false,
        },
        ParityCase {
            name: "circle_with_feather",
            width: 96, height: 64,
            masks: MaskStack { masks: vec![Mask {
                shape: MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.2 },
                feather: 0.1, invert: false, opacity: 1.0, enabled: true,
                keyframes: Default::default(),
            }] },
            expects_gpu_gap: false,
        },
        ParityCase {
            name: "rectangle_rotated",
            width: 96, height: 64,
            masks: MaskStack { masks: vec![Mask {
                shape: MaskShape::Rectangle {
                    center_x: 0.5, center_y: 0.5,
                    width: 0.4, height: 0.3, rotation: 30.0,
                },
                feather: 0.05, invert: false, opacity: 0.9, enabled: true,
                keyframes: Default::default(),
            }] },
            expects_gpu_gap: false,
        },
        ParityCase {
            name: "linear_gradient",
            width: 96, height: 64,
            masks: MaskStack { masks: vec![Mask {
                shape: MaskShape::LinearGradient {
                    start_x: 0.2, start_y: 0.5, end_x: 0.8, end_y: 0.5,
                },
                feather: 0.0, invert: false, opacity: 1.0, enabled: true,
                keyframes: Default::default(),
            }] },
            expects_gpu_gap: false,
        },
        ParityCase {
            name: "circle_inverted",
            width: 96, height: 64,
            masks: MaskStack { masks: vec![Mask {
                shape: MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.3 },
                feather: 0.05, invert: true, opacity: 1.0, enabled: true,
                keyframes: Default::default(),
            }] },
            expects_gpu_gap: false,
        },
        // ── Bezier (#40 ships these) ──
        ParityCase {
            name: "bezier_triangle",
            width: 96, height: 64,
            masks: MaskStack { masks: vec![Mask {
                shape: MaskShape::Bezier { points: vec![
                    (0.5, 0.2),
                    (0.2, 0.8),
                    (0.8, 0.8),
                ] },
                feather: 0.0, invert: false, opacity: 1.0, enabled: true,
                keyframes: Default::default(),
            }] },
            expects_gpu_gap: false,
        },
        ParityCase {
            name: "bezier_square_with_feather",
            width: 96, height: 64,
            masks: MaskStack { masks: vec![Mask {
                shape: MaskShape::Bezier { points: vec![
                    (0.3, 0.3),
                    (0.7, 0.3),
                    (0.7, 0.7),
                    (0.3, 0.7),
                ] },
                feather: 0.08, invert: false, opacity: 1.0, enabled: true,
                keyframes: Default::default(),
            }] },
            expects_gpu_gap: false,
        },
        // Multi-mask union mixing supported shapes + bezier — proves
        // the storage-buffer architecture handles a heterogeneous
        // stack in one dispatch (no per-shape pipeline switching).
        ParityCase {
            name: "circle_union_bezier",
            width: 96, height: 64,
            masks: MaskStack { masks: vec![
                Mask {
                    shape: MaskShape::Circle { center_x: 0.3, center_y: 0.5, radius: 0.15 },
                    feather: 0.05, invert: false, opacity: 1.0, enabled: true,
                    keyframes: Default::default(),
                },
                Mask {
                    shape: MaskShape::Bezier { points: vec![
                        (0.6, 0.3), (0.85, 0.5), (0.6, 0.7),
                    ] },
                    feather: 0.05, invert: false, opacity: 1.0, enabled: true,
                    keyframes: Default::default(),
                },
            ] },
            expects_gpu_gap: false,
        },
        // Stress test: 8 masks in one stack — proves the
        // 4-mask cap is gone. Mixed shapes so it also catches
        // any per-shape regression.
        ParityCase {
            name: "eight_mask_stack_no_cap",
            width: 96, height: 64,
            masks: MaskStack { masks: vec![
                Mask {
                    shape: MaskShape::Circle { center_x: 0.15, center_y: 0.3, radius: 0.06 },
                    feather: 0.0, invert: false, opacity: 1.0, enabled: true,
                    keyframes: Default::default(),
                },
                Mask {
                    shape: MaskShape::Circle { center_x: 0.35, center_y: 0.3, radius: 0.06 },
                    feather: 0.0, invert: false, opacity: 1.0, enabled: true,
                    keyframes: Default::default(),
                },
                Mask {
                    shape: MaskShape::Circle { center_x: 0.55, center_y: 0.3, radius: 0.06 },
                    feather: 0.0, invert: false, opacity: 1.0, enabled: true,
                    keyframes: Default::default(),
                },
                Mask {
                    shape: MaskShape::Circle { center_x: 0.75, center_y: 0.3, radius: 0.06 },
                    feather: 0.0, invert: false, opacity: 1.0, enabled: true,
                    keyframes: Default::default(),
                },
                Mask {
                    shape: MaskShape::Rectangle { center_x: 0.2, center_y: 0.7, width: 0.1, height: 0.1, rotation: 0.0 },
                    feather: 0.0, invert: false, opacity: 1.0, enabled: true,
                    keyframes: Default::default(),
                },
                Mask {
                    shape: MaskShape::Rectangle { center_x: 0.5, center_y: 0.7, width: 0.1, height: 0.1, rotation: 0.0 },
                    feather: 0.0, invert: false, opacity: 1.0, enabled: true,
                    keyframes: Default::default(),
                },
                Mask {
                    shape: MaskShape::Rectangle { center_x: 0.8, center_y: 0.7, width: 0.1, height: 0.1, rotation: 0.0 },
                    feather: 0.0, invert: false, opacity: 1.0, enabled: true,
                    keyframes: Default::default(),
                },
                Mask {
                    shape: MaskShape::Bezier { points: vec![
                        (0.05, 0.05), (0.95, 0.05), (0.95, 0.15), (0.05, 0.15),
                    ] },
                    feather: 0.0, invert: false, opacity: 1.0, enabled: true,
                    keyframes: Default::default(),
                },
            ] },
            expects_gpu_gap: false,
        },
    ]
}

#[test]
fn gpu_matches_cpu_oracle_across_fixtures() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        eprintln!(
            "[mask_gpu_parity] SKIPPED — no wgpu adapter available. \
             This test must run on a machine with a working GPU; a \
             skip on the wrong host masks real shader regressions.",
        );
        return;
    };

    let goldens = goldens_root();
    let mut hard_failures: Vec<String> = Vec::new();
    let mut expected_gaps: Vec<String> = Vec::new();

    for case in fixtures() {
        // CPU oracle.
        let cpu_matte = generate_mask_matte(case.width, case.height, &case.masks);
        let cpu_rgba = matte_f32_to_rgba8(&cpu_matte);

        // GPU dispatch — drop unsupported shapes (today: bezier).
        let gpu_defs: Vec<GpuMaskDef> = case.masks.masks.iter()
            .filter_map(mask_to_gpu_def)
            .collect();
        let stack_was_truncated = gpu_defs.len()
            != case.masks.masks.iter().filter(|m| m.enabled).count();
        let gpu_rgba = gpu.generate_mask_gpu(case.width, case.height, &gpu_defs);

        // Snapshot the CPU oracle so a regression in the CPU
        // rasteriser also gets caught — and so the GPU output once
        // we ship #40 has a stable baseline to compare to.
        assert_matches_golden_png_in(
            &cpu_rgba, case.width, case.height,
            &goldens,
            &format!("cpu_oracle_{}", case.name),
            Tolerance::strict(),
        );

        // Compare GPU to CPU within cross-implementation tolerance.
        // Hard failure = unexpected drift on a shape the GPU should
        // already render correctly.
        let mismatch = pixel_mismatch(&cpu_rgba, &gpu_rgba, 2, 0.001);
        match (case.expects_gpu_gap, stack_was_truncated, mismatch) {
            (true, true, _) => {
                expected_gaps.push(format!(
                    "{}: skipped on GPU (unsupported shape) — expected gap",
                    case.name,
                ));
            }
            (false, _, Some(reason)) => {
                hard_failures.push(format!("{}: {}", case.name, reason));
            }
            (true, false, Some(reason)) => {
                // Bezier landed on GPU but parity failed — that's
                // a real bug in the new shader.
                hard_failures.push(format!(
                    "{} (formerly expected gap, now reachable): {}",
                    case.name, reason,
                ));
            }
            (true, false, None) => {
                expected_gaps.push(format!(
                    "{}: NOW PASSES on GPU — flip expects_gpu_gap=false",
                    case.name,
                ));
            }
            (false, _, None) => { /* clean parity */ }
        }
    }

    if !expected_gaps.is_empty() {
        eprintln!("[mask_gpu_parity] expected gaps:");
        for g in &expected_gaps {
            eprintln!("  - {g}");
        }
    }
    if !hard_failures.is_empty() {
        panic!(
            "mask GPU parity hard failures:\n{}",
            hard_failures.join("\n"),
        );
    }
}

/// Return `Some(reason)` when the GPU buffer drifts from the CPU
/// buffer by more than the cross-impl tolerance; `None` on parity.
fn pixel_mismatch(cpu: &[u8], gpu: &[u8], per_channel_max: u8, max_bad_fraction: f32) -> Option<String> {
    if cpu.len() != gpu.len() {
        return Some(format!("buffer length mismatch: cpu={} gpu={}", cpu.len(), gpu.len()));
    }
    let mut bad_pixels = 0usize;
    let mut worst = 0u8;
    for (c, g) in cpu.chunks_exact(4).zip(gpu.chunks_exact(4)) {
        let mut pixel_bad = false;
        for (cv, gv) in c.iter().zip(g.iter()) {
            let d = cv.abs_diff(*gv);
            if d > worst { worst = d; }
            if d > per_channel_max { pixel_bad = true; }
        }
        if pixel_bad { bad_pixels += 1; }
    }
    let total = (cpu.len() / 4).max(1);
    let frac = bad_pixels as f32 / total as f32;
    if frac > max_bad_fraction {
        Some(format!(
            "{} / {} pixels ({:.3}%) over ±{}/255 (worst {}); bad-pixel limit {:.3}%",
            bad_pixels, total, frac * 100.0,
            per_channel_max, worst, max_bad_fraction * 100.0,
        ))
    } else {
        None
    }
}
