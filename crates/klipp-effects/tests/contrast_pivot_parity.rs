//! The contrast pivot means the same thing on the CPU as on the GPU.
//!
//! The pivot is a number the shader and its Rust twin both multiply
//! around, and a grade encoding either of them derives for itself is a
//! way for the two to drift. Both are handed the value
//! `klipp_grade_model::encode_contrast_pivot` produced, and these tests
//! hold them to it — and to the pivot actually doing something, since
//! two implementations that both ignore it would agree perfectly.

use klipp_effects::cpu_grade::{color_grade, ColorGrade};
use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_grade_model::{encode_contrast_pivot, DEFAULT_CONTRAST_PIVOT};

const W: u32 = 64;
const H: u32 = 48;

/// A ramp through the shadows, the midtones and well past diffuse
/// white, so a pivot anywhere in its range has samples on both sides of
/// it to move in opposite directions.
fn frame() -> Vec<f32> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let t = x as f32 / (W - 1) as f32;
            let s = y as f32 / (H - 1) as f32;
            // 0.002 to ~3.0, geometric, so the shadows get as many
            // samples as the highlights.
            let base = 0.002 * (1500f32).powf(t);
            v.extend_from_slice(&[base, base * (0.6 + 0.8 * s), base * (1.4 - 0.6 * s), 1.0]);
        }
    }
    v
}

fn grade_at(pivot_linear: f32, contrast: f32) -> ColorGrade {
    ColorGrade {
        exposure: 0.0,
        contrast,
        contrast_pivot_encoded: encode_contrast_pivot(pivot_linear),
        saturation: 1.0,
        white_balance: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        highlights: 0.0,
        shadows: 0.0,
        whites: 0.0,
        blacks: 0.0,
        lift: (0.0, 0.0, 0.0),
        gamma: (0.0, 0.0, 0.0),
        gain: (0.0, 0.0, 0.0),
        offset: (0.0, 0.0, 0.0),
        log_wheels: None,
        hue: 0.0,
        global_mix: 1.0,
        luma: [0.2126, 0.7152, 0.0722],
        encoding: klipp_grade_model::Encoding::Bt1886,
    }
}

fn on_gpu(gpu: &mut GpuEffectsPipeline, input: &[f32], g: &ColorGrade) -> Vec<f32> {
    gpu.apply_color_grade(
        input,
        W,
        H,
        g.exposure,
        g.contrast,
        g.contrast_pivot_encoded,
        g.saturation,
        g.white_balance,
        g.highlights,
        g.shadows,
        g.whites,
        g.blacks,
        g.lift,
        g.gamma,
        g.gain,
        g.offset,
        g.log_wheels,
        g.hue,
        g.global_mix,
    )
}

fn on_cpu(input: &[f32], g: &ColorGrade) -> Vec<f32> {
    let mut px = input.to_vec();
    color_grade(&mut px, g);
    px
}

/// Worst and mean absolute difference in linear light.
fn diff(a: &[f32], b: &[f32]) -> (f32, f32) {
    let mut worst = 0.0f32;
    let mut sum = 0.0f64;
    let mut n = 0u64;
    for (x, y) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        for i in 0..3 {
            let d = (x[i] - y[i]).abs();
            worst = worst.max(d);
            sum += d as f64;
            n += 1;
        }
    }
    (worst, (sum / n.max(1) as f64) as f32)
}

#[test]
fn the_pivot_gives_the_same_picture_on_cpu_and_gpu() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        eprintln!("skipping: no GPU");
        return;
    };
    let input = frame();
    let mut failures = Vec::new();
    for pivot in [0.01, 0.05, DEFAULT_CONTRAST_PIVOT, 0.45, 1.0, 2.0] {
        for contrast in [-0.6, 0.4, 1.5] {
            let g = grade_at(pivot, contrast);
            let cpu = on_cpu(&input, &g);
            let gpu_out = on_gpu(&mut gpu, &input, &g);
            let (worst, mean) = diff(&cpu, &gpu_out);
            eprintln!("pivot {pivot:5.2} contrast {contrast:+5.2}  worst {worst:.6} mean {mean:.6}");
            // The two run the same arithmetic in the same order; only
            // float association should separate them.
            if worst > 2e-3 {
                failures.push(format!("pivot {pivot} contrast {contrast}: worst {worst}"));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Two implementations that both ignored the pivot would agree
/// perfectly, so prove it changes the picture — and in the direction
/// the control claims.
#[test]
fn moving_the_pivot_moves_the_picture() {
    let input = frame();
    let low = on_cpu(&input, &grade_at(0.02, 0.8));
    let high = on_cpu(&input, &grade_at(1.0, 0.8));
    let (worst, mean) = diff(&low, &high);
    assert!(worst > 0.05, "the pivot barely moved anything: worst {worst} mean {mean}");

    // Everything below a pivot is pulled down by a contrast push, and a
    // higher pivot leaves more of the picture below it — so the same
    // push spends more of itself darkening.
    let sum = |v: &[f32]| v.chunks_exact(4).map(|p| p[0] as f64).sum::<f64>();
    assert!(
        sum(&high) < sum(&low),
        "a higher pivot should darken more of the frame than a lower one",
    );
}

/// The default pivot is mid grey, so a grade that never touches the
/// control renders exactly as it did before the control existed.
#[test]
fn the_default_pivot_leaves_every_existing_grade_alone() {
    let input = frame();
    let explicit = on_cpu(&input, &grade_at(DEFAULT_CONTRAST_PIVOT, 0.7));
    let mid_grey = {
        let mut g = grade_at(DEFAULT_CONTRAST_PIVOT, 0.7);
        g.contrast_pivot_encoded = klipp_grade_model::MID_GREY_ENCODED;
        on_cpu(&input, &g)
    };
    let (worst, _) = diff(&explicit, &mid_grey);
    assert!(worst < 1e-6, "the default pivot is not mid grey: worst {worst}");
}

/// #467: a whole grade, the tone controls, the wheels and contrast,
/// gives the same picture on the CPU and the GPU in each encoding; and in
/// ACEScct offset is an exposure, which is why colourists grade there.
#[test]
fn every_grade_encoding_gives_the_same_picture_on_cpu_and_gpu() {
    use klipp_grade_model::{encode_contrast_pivot_in, Encoding};
    let input = frame();
    let full = |enc: Encoding, log: bool| ColorGrade {
        contrast: 0.3,
        contrast_pivot_encoded: encode_contrast_pivot_in(0.18, enc),
        highlights: -0.05,
        shadows: 0.04,
        whites: 0.02,
        blacks: -0.01,
        lift: (0.02, 0.0, -0.01),
        gamma: (0.05, 0.02, 0.0),
        gain: (0.1, 0.0, -0.05),
        offset: (0.01, 0.0, 0.02),
        log_wheels: log.then_some([(0.02, 0.0, -0.01), (0.0, 0.03, 0.0), (-0.02, 0.0, 0.01)]),
        encoding: enc,
        ..grade_at(0.18, 0.0)
    };
    // Offset in ACEScct is exposure: +1/17.52 there is +1 stop above the toe.
    let mut stop = grade_at(0.18, 0.0);
    stop.encoding = Encoding::AcesCct;
    stop.offset = (1.0 / 17.52, 1.0 / 17.52, 1.0 / 17.52);
    let out = on_cpu(&input, &stop);
    for (a, b) in input.chunks_exact(4).zip(out.chunks_exact(4)).filter(|(a, _)| a[0] > 0.01) {
        assert!((b[0] / a[0] - 2.0).abs() < 1e-3, "{} -> {}", a[0], b[0]);
    }
    let Some(mut gpu) = GpuEffectsPipeline::new() else {
        eprintln!("skipping the GPU half: no GPU");
        return;
    };
    for enc in [Encoding::Bt1886, Encoding::AcesCct, Encoding::AcesCc] {
        gpu.set_grade_encoding(enc.as_u8());
        for log in [false, true] {
            let g = full(enc, log);
            let cpu = on_cpu(&input, &g);
            let gpu_out = on_gpu(&mut gpu, &input, &g);
            let (worst, mean) = diff(&cpu, &gpu_out);
            eprintln!("{enc:?} log {log}: worst {worst:.6} mean {mean:.6}");
            assert!(worst < 2e-3, "{enc:?} log {log}: CPU and GPU differ by {worst}");
            // And the encoding changes the picture: it is not ignored.
            if enc != Encoding::Bt1886 {
                let (d, _) = diff(&cpu, &on_cpu(&input, &full(Encoding::Bt1886, log)));
                assert!(d > 1e-3, "{enc:?} grades like BT.1886");
            }
        }
    }
}
