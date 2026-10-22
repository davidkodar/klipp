//! Equalize's histogram and CDF built on the GPU, in the chain (#1215).
//!
//! The chain records Equalize as three passes, the frame's luma counted
//! into a histogram, its CDF, and the map through it, where it used to
//! read the frame back for the CPU to count. These hold the GPU's
//! histogram, CDF and picture to the CPU's (`equalize_histogram`,
//! `equalize_cdf_of`, `apply_effects_with_luma`).
//!
//! The one way they may differ: a pixel whose luma lies within float
//! rounding of a bin's edge can land in the bin beside, the GPU's sRGB
//! encode not being bit for bit the CPU's `powf`. So the histograms may
//! differ by two counts per such pixel, the CDFs by those pixels' share,
//! and the picture at those pixels; everywhere else by a level at most.

use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_effects::{apply_effects_with_luma, equalize_cdf_of, equalize_histogram, BuiltinEffect, EQUALIZE_CDF_LEN};

const YW: [f32; 3] = [0.2126, 0.7152, 0.0722];

fn pipeline() -> Option<GpuEffectsPipeline> {
    let mut g = GpuEffectsPipeline::new()?;
    g.set_luma_coefs(YW);
    Some(g)
}

fn encode(c: f32) -> f32 {
    if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

/// A pixel's luma as the histogram counts it, and as the map reads it
/// (each channel to white first).
fn lumas(c: &[f32]) -> (f32, f32) {
    let counted = (YW[0] * encode(c[0]) + YW[1] * encode(c[1]) + YW[2] * encode(c[2])).clamp(0.0, 1.0);
    let d = [0, 1, 2].map(|i| encode(c[i]).min(1.0));
    (counted, YW[0] * d[0] + YW[1] * d[1] + YW[2] * d[2])
}

/// Whether `l × 255` is within rounding of a bin's edge.
fn at_edge(l: f32) -> bool {
    let x = l * 255.0;
    (x - x.round()).abs() < 1e-4 && x > 0.5 && x < 254.5
}

/// A seeded generator, so a frame is the same each run.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
}

fn noise(w: u32, h: u32, seed: u64, scale: f32) -> Vec<f32> {
    let mut r = Lcg(seed);
    (0..w * h).flat_map(|_| [r.next() * scale, r.next() * scale, r.next() * scale, 1.0]).collect()
}

fn gradient(w: u32, h: u32) -> Vec<f32> {
    (0..w * h)
        .flat_map(|i| {
            let t = (i % w) as f32 / (w - 1).max(1) as f32;
            [t * t, t, t.sqrt(), 1.0]
        })
        .collect()
}

fn flat(w: u32, h: u32, v: f32) -> Vec<f32> {
    (0..w * h).flat_map(|_| [v, v, v, 1.0]).collect()
}

/// The reference photo, decoded and made linear, alpha varying so it is
/// seen to be left alone.
fn photo() -> (Vec<f32>, u32, u32) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/effect_preview_reference.jpg");
    let img = image::open(&path).expect("the reference photo").to_rgba8();
    let (w, h) = img.dimensions();
    let decode = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    let px = img.pixels().enumerate().flat_map(|(i, p)| [decode(p[0]), decode(p[1]), decode(p[2]), 0.5 + (i % 7) as f32 / 14.0]).collect();
    (px, w, h)
}

/// The GPU's histogram and CDF of `frame` against the CPU's.
fn histogram_and_cdf_match(g: &mut GpuEffectsPipeline, name: &str, frame: &[f32], w: u32, h: u32) {
    let (gpu_hist, gpu_cdf) = g.equalize_histogram_and_cdf(frame, w, h).expect("a GPU histogram");
    let n = (w * h) as usize;
    let cpu_hist = equalize_histogram(frame, YW);
    let cpu_cdf = equalize_cdf_of(&cpu_hist, n);
    let edges = frame.chunks_exact(4).filter(|c| at_edge(lumas(c).0)).count();
    assert_eq!(gpu_hist.iter().map(|&c| c as usize).sum::<usize>(), n, "{name}: every pixel counted once");
    let off: u64 = gpu_hist.iter().zip(&cpu_hist).map(|(a, b)| u64::from(a.abs_diff(*b))).sum();
    assert!(off as usize <= 2 * edges, "{name}: histograms differ by {off} counts, {edges} pixels at an edge");
    let first = cpu_hist.iter().position(|&c| c > 0).unwrap_or(0);
    let min_cum: usize = cpu_hist[..=first].iter().map(|&c| c as usize).sum();
    let tolerance = if n > min_cum { edges as f32 / (n - min_cum) as f32 } else { 0.0 } + 1e-6;
    let worst = gpu_cdf.iter().zip(&cpu_cdf).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(worst <= tolerance, "{name}: CDFs differ by {worst}, over {tolerance}");
    assert_eq!(gpu_cdf.len(), EQUALIZE_CDF_LEN);
}

/// The GPU's picture of `fx` on `frame` against the CPU's: alpha as it
/// was, and each colour within a level, but at pixels whose luma is at a
/// bin's edge.
fn picture_matches(g: &mut GpuEffectsPipeline, name: &str, fx: &[BuiltinEffect], frame: &[f32], w: u32, h: u32) {
    let gpu = g.apply_effects(fx, frame, w, h);
    let cpu = apply_effects_with_luma(fx, frame.to_vec(), w, h, YW);
    let edges = frame.chunks_exact(4).filter(|c| at_edge(lumas(c).0) || at_edge(lumas(c).1)).count();
    let mut off = 0;
    for (i, ((g, c), f)) in gpu.chunks_exact(4).zip(cpu.chunks_exact(4)).zip(frame.chunks_exact(4)).enumerate() {
        assert_eq!(g[3].to_bits(), f[3].to_bits(), "{name}: pixel {i}'s alpha changed");
        if (0..3).any(|k| (encode(g[k]) - encode(c[k])).abs() > 1.0 / 255.0 + 1e-4) {
            off += 1;
        }
    }
    // Two Equalizes compound an edge pixel's flip: twice the allowance.
    let allowed = edges * fx.len();
    assert!(off <= allowed, "{name}: {off} pixels off by more than a level, {allowed} allowed");
}

#[test]
fn the_gpu_histogram_and_cdf_are_the_cpus() {
    let Some(mut g) = pipeline() else { return };
    let (photo, pw, ph) = photo();
    let frames: Vec<(&str, Vec<f32>, u32, u32)> = vec![
        ("gradient", gradient(256, 64), 256, 64),
        ("noise", noise(200, 150, 7, 1.0), 200, 150),
        ("hdr", noise(160, 90, 11, 4.0), 160, 90),
        ("flat", flat(64, 64, 0.5), 64, 64),
        ("black", flat(48, 32, 0.0), 48, 32),
        ("one pixel", noise(1, 1, 3, 1.0), 1, 1),
        ("odd size", noise(333, 217, 5, 1.0), 333, 217),
        ("photo", photo, pw, ph),
    ];
    for (name, frame, w, h) in &frames {
        histogram_and_cdf_match(&mut g, name, frame, *w, *h);
    }
}

/// A frame in one bin has the identity ramp, as the CPU gives it (to the
/// GPU's division, a few ulps).
#[test]
fn a_frame_in_one_bin_maps_by_the_identity_ramp() {
    let Some(mut g) = pipeline() else { return };
    for v in [0.0, 0.5, 1.0] {
        let (hist, cdf) = g.equalize_histogram_and_cdf(&flat(20, 10, v), 20, 10).expect("a GPU histogram");
        assert_eq!(hist.iter().filter(|&&c| c > 0).count(), 1);
        assert!(cdf.iter().enumerate().all(|(i, &c)| (c - i as f32 / 255.0).abs() <= 1e-6), "{v}: {cdf:?}");
    }
}

#[test]
fn the_gpu_picture_is_the_cpus() {
    let Some(mut g) = pipeline() else { return };
    let (photo, pw, ph) = photo();
    let frames: Vec<(&str, Vec<f32>, u32, u32)> = vec![
        ("gradient", gradient(256, 64), 256, 64),
        ("noise", noise(200, 150, 7, 1.0), 200, 150),
        ("hdr", noise(160, 90, 11, 4.0), 160, 90),
        ("flat", flat(64, 64, 0.5), 64, 64),
        ("odd size", noise(333, 217, 5, 1.0), 333, 217),
        ("photo", photo, pw, ph),
    ];
    for (name, frame, w, h) in &frames {
        for strength in [0.0, 0.5, 1.0] {
            picture_matches(&mut g, &format!("{name} at {strength}"), &[BuiltinEffect::Equalize { strength }], frame, *w, *h);
        }
    }
}

/// Two Equalizes in one chain: the second counts its own input's
/// histogram, the buffer cleared for it, not added to the first's.
#[test]
fn two_equalizes_in_one_chain_each_count_their_own_frame() {
    let Some(mut g) = pipeline() else { return };
    let (w, h) = (180, 120);
    let frame = noise(w, h, 13, 0.6);
    let fx = [BuiltinEffect::Equalize { strength: 1.0 }, BuiltinEffect::Gamma { gamma: 1.6 }, BuiltinEffect::Equalize { strength: 0.5 }];
    picture_matches(&mut g, "two in a chain", &fx, &frame, w, h);
    g.ensure_recording_buffers(w, h);
    g.upload_chain_input_linear(&frame);
    g.begin_chain();
    let run = g.chain_run_effects(&fx, true, w, h);
    g.submit_chain();
    assert_eq!(run.breaks, 0, "recorded in the chain, no readback: {run:?}");
    assert!(run.complete);
}

/// Integer counts: the same frame twice is the same histogram, CDF and
/// picture, bit for bit.
#[test]
fn the_gpu_equalize_is_deterministic() {
    let Some(mut g) = pipeline() else { return };
    let (w, h) = (333, 217);
    let frame = noise(w, h, 17, 1.0);
    let a = g.equalize_histogram_and_cdf(&frame, w, h).expect("a GPU histogram");
    let b = g.equalize_histogram_and_cdf(&frame, w, h).expect("a GPU histogram");
    assert_eq!(a.0, b.0);
    assert_eq!(a.1.map(f32::to_bits), b.1.map(f32::to_bits));
    let fx = [BuiltinEffect::Equalize { strength: 0.8 }];
    let first: Vec<u32> = g.apply_effects(&fx, &frame, w, h).iter().map(|v| v.to_bits()).collect();
    let second: Vec<u32> = g.apply_effects(&fx, &frame, w, h).iter().map(|v| v.to_bits()).collect();
    assert_eq!(first, second);
}
