//! The GPU's BT.2020 → BT.709 display mapping is the CPU's.
//!
//! The same bargain as the ACES shader: the reach tables and cusps are
//! *data*, baked from `klipp_color::gamut` by bisecting each gamut's
//! boundary 24 times per grid point, and only the arithmetic that reads
//! them is hand-written. So the risk is not a wrong table — it is a
//! wrong index, a mis-transcribed early return, or a binary search that
//! converges somewhere else.
//!
//! That shapes the test. The mapping is a cascade of early exits — a
//! chroma floor, a hue-free row-minimum test, a knee, a
//! source-holds-no-more test — and most of a picture leaves through one
//! of them *unchanged*. A fixture that mostly hits those would agree
//! with almost anything, so this one deliberately reaches past them:
//! saturated colours at many lightnesses, the yellow region where
//! BT.709 holds almost no chroma and the mapping has to trade
//! lightness, and a sweep right around the knee where the two branches
//! meet.

use klipp_color::display::ToneMapOp;
use klipp_color::gamut::bt2020_to_bt709_gpu_data;
use klipp_effects::{gpu::GpuEffectsPipeline, gpu_context::GpuContext};

/// The CPU answer for a BT.2020-linear pixel, quantised as the shader
/// quantises: the real viewer transform, not a re-derivation.
fn cpu_u8(rgb: [f32; 3]) -> [u8; 3] {
    let px = [rgb[0], rgb[1], rgb[2], 1.0];
    let out = klipp_color::display::display_transform_ws(
        &px,
        ToneMapOp::None,
        klipp_core::WorkingColorspace::Bt2020Linear,
    );
    [out[0], out[1], out[2]]
}

fn gpu(colours: &[[f32; 3]]) -> Option<Vec<[u8; 3]>> {
    let ctx = GpuContext::new()?;
    let mut pipeline = GpuEffectsPipeline::with_context(&ctx)?;
    let w = colours.len() as u32;
    let mut linear = Vec::with_capacity(colours.len() * 4);
    for c in colours {
        linear.extend_from_slice(&[c[0], c[1], c[2], 1.0]);
    }
    pipeline.ensure_chain_buffers((w as u64) * 16);
    pipeline.upload_chain_input_linear(&linear);
    let data = bt2020_to_bt709_gpu_data();
    let bytes = pipeline.encode_chain_to_bt2020_display_u8(true, w, 1, &data)?;
    Some(bytes.chunks_exact(4).map(|p| [p[0], p[1], p[2]]).collect())
}

/// Compare, and also report how many colours the mapping actually
/// *moved* — a run where it moved nothing proves nothing.
fn compare(label: &str, colours: &[[f32; 3]], tolerance: i32) {
    let Some(got) = gpu(colours) else {
        eprintln!("skipping {label}: no usable GPU");
        return;
    };
    let mut worst = 0i32;
    let mut worst_at = 0usize;
    let mut differing = 0usize;
    let mut moved = 0usize;
    for (i, (g, c)) in got.iter().zip(colours.iter()).enumerate() {
        let want = cpu_u8(*c);
        // Did the mapping change this colour at all? Compare the CPU
        // answer against the same pixel with the compression skipped.
        let plain = klipp_color::display::display_transform_ws(
            &[c[0], c[1], c[2], 1.0],
            ToneMapOp::None,
            klipp_core::WorkingColorspace::Rec709Linear,
        );
        if (0..3).any(|k| want[k] != plain[k]) {
            moved += 1;
        }
        let d = (0..3).map(|k| (g[k] as i32 - want[k] as i32).abs()).max().unwrap();
        if d > 0 {
            differing += 1;
        }
        if d > worst {
            worst = d;
            worst_at = i;
        }
    }
    let c = colours[worst_at];
    eprintln!(
        "{label}: worst {worst} of 255 over {} colours ({differing} differ, {moved} were mapped); \
         worst at [{:.4}, {:.4}, {:.4}]",
        colours.len(),
        c[0],
        c[1],
        c[2],
    );
    assert!(
        moved * 4 > colours.len(),
        "{label}: the mapping only moved {moved} of {} colours, so this fixture mostly \
         exercises the early exits and would agree with almost anything",
        colours.len(),
    );
    assert!(
        worst <= tolerance,
        "{label}: out by {worst} code values at [{:.4}, {:.4}, {:.4}] — gpu {:?} vs cpu {:?}",
        c[0],
        c[1],
        c[2],
        got[worst_at],
        cpu_u8(c),
    );
}

/// Saturated BT.2020 colours across the hue circle and the lightness
/// range — the ones that land outside BT.709 and so actually go through
/// the compression rather than an early exit.
fn saturated_sweep() -> Vec<[f32; 3]> {
    let mut out = Vec::new();
    for hi in 0..72 {
        let h = hi as f32 / 72.0 * std::f32::consts::TAU;
        for level in [0.05f32, 0.15, 0.3, 0.5, 0.7, 0.9] {
            // A saturated colour around the hue circle, in BT.2020's
            // wider basis so much of it is outside BT.709.
            let r = level * (1.0 + 0.95 * h.cos());
            let g = level * (1.0 + 0.95 * (h - 2.094).cos());
            let b = level * (1.0 + 0.95 * (h + 2.094).cos());
            out.push([r.max(0.0), g.max(0.0), b.max(0.0)]);
        }
    }
    out
}

#[test]
fn bt2020_display_matches_the_cpu_on_saturated_colour() {
    compare("saturated sweep", &saturated_sweep(), 1);
}

/// The yellow region, which is the awkward one.
///
/// A straight line out from the neutral axis at lightness 96 leaves
/// BT.709 at chroma 41 and touches it again only at chroma 97 — the
/// gamut is not star-shaped there. That is the case the mapping spends
/// lightness on, so it runs the eight-step binary search, and a search
/// that converged differently would show up here and almost nowhere
/// else.
#[test]
fn bt2020_display_matches_the_cpu_in_the_yellow_gap() {
    let mut cases = Vec::new();
    for i in 0..48 {
        let t = i as f32 / 47.0;
        // Bright yellows of increasing purity.
        cases.push([0.75 + 0.25 * t, 0.75 + 0.25 * t, 0.02 + 0.10 * (1.0 - t)]);
        // And the same around the yellow-green and yellow-orange edges.
        cases.push([0.55 + 0.45 * t, 0.95, 0.05]);
        cases.push([0.95, 0.55 + 0.45 * t, 0.05]);
    }
    compare("yellow gap", &cases, 1);
}

/// Right around the knee, where the mapping switches from leaving a
/// colour alone to compressing it. The knee is at 0.8 of what BT.709
/// holds, and it starts *inside* the gamut on purpose so a gradient
/// rolls off instead of stepping — which means both branches are live
/// within a few per cent of each other and a wrong comparison shows as
/// a visible step.
#[test]
fn bt2020_display_matches_the_cpu_across_the_knee() {
    let mut cases = Vec::new();
    for hi in 0..24 {
        let h = hi as f32 / 24.0 * std::f32::consts::TAU;
        for k in 0..24 {
            // Sweep purity finely so samples land either side of the knee.
            let p = 0.55 + 0.45 * (k as f32 / 23.0);
            let level = 0.4f32;
            cases.push([
                (level * (1.0 + p * h.cos())).max(0.0),
                (level * (1.0 + p * (h - 2.094).cos())).max(0.0),
                (level * (1.0 + p * (h + 2.094).cos())).max(0.0),
            ]);
        }
    }
    compare("knee", &cases, 1);
}

/// What the shader is *for*: the frame not crossing the bus.
///
/// A probe, not a pass/fail test:
/// `cargo test --release -p klipp-engine --test bt2020_gamut_gpu_matches_cpu -- --ignored --nocapture`
///
/// Both routes start from the same graded 4K frame sitting in the chain
/// buffer. One encodes it on the GPU and reads back 33 MB of bytes; the
/// other reads back 132 MB of floats and runs `display_transform_ws`,
/// bisecting the gamut boundary per pixel on the CPU. That second route
/// is what a BT.2020 project used to pay on every frame.
#[test]
#[ignore = "perf probe: run explicitly"]
fn bt2020_display_on_the_gpu_beats_the_round_trip() {
    let (w, h) = (3840u32, 2160u32);
    let n = (w as usize) * (h as usize);
    let Some(ctx) = GpuContext::new() else {
        eprintln!("skipping: no usable GPU");
        return;
    };
    let mut pipeline = GpuEffectsPipeline::with_context(&ctx).expect("pipeline");

    // A saturated frame, so the mapping does real work rather than
    // leaving most of the picture through an early exit.
    let mut linear = Vec::with_capacity(n * 4);
    for i in 0..n {
        let t = (i % 4096) as f32 / 4096.0;
        let h = t * std::f32::consts::TAU;
        let level = 0.05 + 0.85 * ((i / 4096) % 64) as f32 / 64.0;
        for k in 0..3 {
            let phase = h - k as f32 * 2.094;
            linear.push((level * (1.0 + 0.95 * phase.cos())).max(0.0));
        }
        linear.push(1.0);
    }
    pipeline.ensure_chain_buffers((n as u64) * 16);
    pipeline.upload_chain_input_linear(&linear);
    let data = bt2020_to_bt709_gpu_data();

    // Once to warm the pipeline and the staging buffers.
    let _ = pipeline.encode_chain_to_bt2020_display_u8(true, w, h, &data);
    let mut gpu_ms = Vec::new();
    for _ in 0..5 {
        let t = std::time::Instant::now();
        let bytes = pipeline.encode_chain_to_bt2020_display_u8(true, w, h, &data).expect("encode");
        gpu_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(bytes.len(), n * 4);
    }

    let mut cpu_ms = Vec::new();
    for _ in 0..3 {
        let t = std::time::Instant::now();
        let floats = pipeline.download_chain_output_linear(true, n);
        let read_ms = t.elapsed().as_secs_f64() * 1000.0;
        let t2 = std::time::Instant::now();
        let bytes = klipp_color::display::display_transform_ws(
            &floats,
            ToneMapOp::None,
            klipp_core::WorkingColorspace::Bt2020Linear,
        );
        let enc_ms = t2.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(bytes.len(), n * 4);
        eprintln!("  round trip: readback {read_ms:.1} ms + transform {enc_ms:.1} ms");
        cpu_ms.push(read_ms + enc_ms);
    }

    let median = |mut v: Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };
    let (g, c) = (median(gpu_ms), median(cpu_ms));
    eprintln!(
        "4K BT.2020 display transform: on the GPU {g:.1} ms/frame, \
         via the CPU round trip {c:.1} ms/frame ({:.1}x)",
        c / g
    );
}

/// Highlights above white, which the mapping scales into range and back
/// out again, and negatives, which a wide-gamut source arrives as.
#[test]
fn bt2020_display_matches_the_cpu_outside_the_unit_range() {
    let mut cases = vec![[-0.05, 0.6, 0.9], [0.9, -0.02, 0.1], [0.0, 0.0, 0.0]];
    for stops in 0..8 {
        let k = 2.0f32.powf(stops as f32);
        cases.push([k * 0.9, k * 0.2, k * 0.05]);
        cases.push([k * 0.1, k * 0.9, k * 0.3]);
        cases.push([k * 0.2, k * 0.4, k * 0.95]);
    }
    compare("outside unit range", &cases, 1);
}
