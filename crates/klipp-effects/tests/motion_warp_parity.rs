//! Stabilisation and rolling-shutter correction give the same picture on
//! the CPU as on the GPU, from the same motion data (#698).
//!
//! Both used to be GPU-only: the compositor uploaded their motion data to
//! the GPU pipeline and the CPU arm passed the frame through, so a render
//! that took the CPU path for a clip (a frei0r plugin in its chain, no
//! usable GPU) dropped the stabilisation without a word.

use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_effects::rolling_shutter::RowTransform;
use klipp_effects::{apply_effects_with_motion, linear_f32_rgba_to_srgb_u8, BuiltinEffect, MotionData};

const W: u32 = 160;
const H: u32 = 90;
const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// Gradients with hard-edged patches, so a warp shows everywhere.
fn frame() -> Vec<f32> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let (fx, fy) = (x as f32 / (W - 1) as f32, y as f32 / (H - 1) as f32);
            let mut px = [fx, fy, 1.0 - fx * fy, 1.0];
            if (x / 10 + y / 10) % 2 == 0 {
                px = [px[0] * 0.3, px[1] * 0.3, px[2] * 0.3, 1.0];
            }
            v.extend_from_slice(&px);
        }
    }
    v
}

fn diff(a: &[f32], b: &[f32]) -> (i32, f64) {
    let (a, b) = (linear_f32_rgba_to_srgb_u8(a), linear_f32_rgba_to_srgb_u8(b));
    let mut worst = 0;
    let mut sum = 0u64;
    for (i, (x, y)) in a.iter().zip(&b).enumerate() {
        if i % 4 == 3 {
            continue;
        }
        let d = (*x as i32 - *y as i32).abs();
        worst = worst.max(d);
        sum += d as u64;
    }
    (worst, sum as f64 / (a.len() / 4 * 3) as f64)
}

/// A correction as stabilisation makes them: a small turn, a shift and a
/// touch of perspective.
fn homography() -> [f32; 12] {
    let (s, c) = 2.5f32.to_radians().sin_cos();
    [c, -s, 6.3, s, c, -4.7, 0.00004, -0.00003, 1.0, 0.0, 0.0, 0.0]
}

/// Rows sheared and shifted progressively down the frame, as a wobbling
/// readout gives them.
fn rows() -> Vec<RowTransform> {
    (0..H)
        .map(|y| {
            let t = y as f32 / H as f32;
            RowTransform { a: 1.0, b: 0.02 * t, tx: 7.5 * t - 2.0, c: 0.0, d: 1.0, ty: 1.25 * t, _pad0: 0.0, _pad1: 0.0 }
        })
        .collect()
}

#[test]
fn stabilisation_and_rolling_shutter_match_between_cpu_and_gpu() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        eprintln!("skipping: no GPU");
        return;
    };
    gpu.set_luma_coefs(LUMA);
    let input = frame();
    let motion = MotionData { stabilization: Some(homography()), rsc_rows: Some(rows()) };
    gpu.upload_stabilization_homography(motion.stabilization.as_ref().unwrap());
    gpu.upload_rsc_row_transforms(motion.rsc_rows.as_ref().unwrap());
    let cases = [
        ("stabilise, zoomed", vec![BuiltinEffect::VideoStabilization { smoothing: 0.5, crop_ratio: 0.05, zoom: true }]),
        ("stabilise, no zoom", vec![BuiltinEffect::VideoStabilization { smoothing: 0.5, crop_ratio: 0.05, zoom: false }]),
        ("rolling shutter", vec![BuiltinEffect::RollingShutterCorrection { strength: 0.8, readout_ratio: 0.4 }]),
        ("both, with a blur between", vec![
            BuiltinEffect::RollingShutterCorrection { strength: 0.8, readout_ratio: 0.4 },
            BuiltinEffect::GaussianBlur { radius: 2.0 },
            BuiltinEffect::VideoStabilization { smoothing: 0.5, crop_ratio: 0.03, zoom: true },
        ]),
    ];
    let mut failures = Vec::new();
    for (name, fx) in cases {
        let cpu = apply_effects_with_motion(&fx, input.clone(), W, H, 0.0, LUMA, &motion);
        let on_gpu = gpu.apply_effects(&fx, &input, W, H);
        let (worst, mean) = diff(&cpu, &on_gpu);
        let (_, change) = diff(&input, &cpu);
        eprintln!("{name:26} CPU vs GPU worst {worst:3} mean {mean:6.3} | change {change:7.3}");
        if worst > 1 {
            failures.push(format!("{name}: CPU and GPU differ by {worst} levels"));
        }
        // A pass-through on both sides would match too.
        if change < 1.0 {
            failures.push(format!("{name}: the CPU arm leaves the frame as it was"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Without motion data, before analysis or once it is cleared, both arms
/// pass the frame through: neither warps by data it was not given.
#[test]
fn without_motion_data_both_arms_pass_the_frame_through() {
    let input = frame();
    let fx = [
        BuiltinEffect::VideoStabilization { smoothing: 0.5, crop_ratio: 0.05, zoom: true },
        BuiltinEffect::RollingShutterCorrection { strength: 0.8, readout_ratio: 0.4 },
    ];
    let cpu = apply_effects_with_motion(&fx, input.clone(), W, H, 0.0, LUMA, &MotionData::default());
    assert_eq!(cpu, input, "the CPU arm warped without motion data");
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        eprintln!("skipping the GPU half: no GPU");
        return;
    };
    // Data from an earlier clip, then cleared, as the compositor clears it
    // for a clip that has none.
    gpu.upload_stabilization_homography(&homography());
    gpu.upload_rsc_row_transforms(&rows());
    gpu.clear_stabilization_homography();
    gpu.clear_rsc_row_transforms();
    let on_gpu = gpu.apply_effects(&fx, &input, W, H);
    assert_eq!(diff(&on_gpu, &input).0, 0, "the GPU arm warped by cleared motion data");
}
