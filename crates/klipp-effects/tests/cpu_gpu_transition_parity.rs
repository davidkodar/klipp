//! Every transition gives the same picture on the CPU as on the GPU.
//!
//! Both run `SHADER_TRANSITION` — the CPU through its Rust transcription
//! (`render_transition_linear`). Each kind is rendered at three points of
//! its progress between two different frames. A few values may land on the
//! other side of a hard edge in the effect itself — the clock wipe's seam,
//! where WGSL allows `atan2` more error than the CPU has — but no more than
//! a pixel's worth.

use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_effects::{linear_f32_rgba_to_srgb_u8, render_transition_linear};

const W: u32 = 96;
const H: u32 = 54;

fn frames() -> (Vec<f32>, Vec<f32>) {
    let (mut a, mut b) = (Vec::new(), Vec::new());
    for y in 0..H {
        for x in 0..W {
            let (fx, fy) = (x as f32 / (W - 1) as f32, y as f32 / (H - 1) as f32);
            a.extend_from_slice(&[0.8 * fx, 0.2 + 0.3 * fy, 0.1, 1.0]);
            let check = if (x / 6 + y / 6) % 2 == 0 { 0.7 } else { 0.2 };
            b.extend_from_slice(&[0.05, check * fy, check, 1.0]);
        }
    }
    (a, b)
}

/// (worst, count over 1) channel difference in display bytes.
fn diff(a: &[f32], b: &[f32]) -> (i32, usize) {
    let (a, b) = (linear_f32_rgba_to_srgb_u8(a), linear_f32_rgba_to_srgb_u8(b));
    let mut worst = 0;
    let mut over = 0;
    for (i, (x, y)) in a.iter().zip(&b).enumerate() {
        if i % 4 == 3 {
            continue;
        }
        let d = (*x as i32 - *y as i32).abs();
        worst = worst.max(d);
        if d > 1 {
            over += 1;
        }
    }
    (worst, over)
}

#[test]
fn every_transition_matches_between_cpu_and_gpu() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        eprintln!("skipping: no GPU");
        return;
    };
    let (from, to) = frames();
    let params = [4.0, 0.5, 0.5, 0.5];
    let color = [0.8, 0.3, 0.1, 1.0];
    let mut failures = Vec::new();
    for kind in 0..73u32 {
        for p in [0.25f32, 0.5, 0.75] {
            let cpu = render_transition_linear(&from, &to, kind, p, W, H, params[0], params[1], params[2], params[3], color);
            let g = gpu.render_transition_linear(&from, &to, kind, p, W, H, params[0], params[1], params[2], params[3], color);
            let (worst, over) = diff(&cpu, &g);
            if over > 4 {
                failures.push(format!("kind {kind} at {p}: worst {worst}, {over} values over 1"));
            }
        }
    }
    for f in &failures {
        eprintln!("{f}");
    }
    assert!(failures.is_empty(), "{} mismatches", failures.len());
}
