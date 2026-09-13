//! HDR monitoring on the GPU is HDR monitoring on the CPU.
//!
//! Two pieces, both needed before monitoring can keep the fast path: the
//! preview encode (the roll-off above reference white, then the display
//! transform) and the light map the pointer readout reads. Each has a CPU
//! twin in `klipp_color::hdr_view`, and the twin is the authority — it is
//! what the viewer has shipped and what `hdr_monitoring.rs` checks
//! end to end.
//!
//! What makes this worth testing rather than eyeballing: a monitoring
//! picture *looks* plausible whatever curve produced it. There is no
//! reference image a colourist can check it against, which is exactly why
//! it has to be checked against the arithmetic.

use klipp_color::hdr_view::{preview_frame, HdrMeasure};
use klipp_core::{ToneMapOp, WorkingColorspace};
use klipp_effects::{gpu::GpuEffectsPipeline, gpu_context::GpuContext};

const W: u32 = 160;
const H: u32 = 90;

/// A frame of HDR *display light* — 1.0 is reference white at 203 cd/m²,
/// so this spans black to about ten times that. Deliberately reaches well
/// past 1.0, because everything interesting about the preview happens
/// above the knee at 0.75, and past the top band at 900 cd/m².
fn display_light() -> Vec<f32> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let fx = x as f32 / (W - 1) as f32;
            let fy = y as f32 / (H - 1) as f32;
            // 0 .. 10x reference white down the frame, hue rotating across.
            let l = fy * 10.0;
            let mut px = [l * (0.2 + 0.8 * fx), l * (0.9 - 0.5 * fx), l * (0.1 + 0.9 * (1.0 - fx)), 1.0];
            // Right at the knee and right at the top band, where a
            // boundary off by a hair would show.
            if y == 0 {
                px = [0.75, 0.75, 0.75, 1.0];
            }
            if y == 1 {
                // 900 cd/m² is the last false-colour band's edge.
                let at_900 = 900.0 / 203.0;
                px = [at_900, at_900, at_900, 1.0];
            }
            // A negative, which a wide-gamut colour arrives as.
            if y == 2 {
                px = [-0.2, 0.4, 0.9, 1.0];
            }
            v.extend_from_slice(&px);
        }
    }
    v
}

fn pipeline() -> Option<(GpuContext, GpuEffectsPipeline)> {
    let ctx = GpuContext::new()?;
    let p = GpuEffectsPipeline::with_context(&ctx)?;
    Some((ctx, p))
}

/// The preview encode: the GPU's bytes are the CPU's.
#[test]
fn the_preview_encode_matches_the_cpu() {
    let Some((_ctx, mut gpu)) = pipeline() else {
        eprintln!("skipping: no usable GPU");
        return;
    };
    let linear = display_light();

    for ws in [WorkingColorspace::Rec709Linear, WorkingColorspace::Bt2020Linear] {
        gpu.ensure_chain_buffers((W as u64) * (H as u64) * 16);
        gpu.upload_chain_input_linear(&linear);
        let got = gpu
            .encode_chain_to_display_u8_view(true, W, H, None, true)
            .expect("the preview encode was refused");
        let want = preview_frame(&linear, ToneMapOp::None, ws);

        // The GPU's display encode is the sRGB one; it has no basis
        // change, so only a BT.709-basis project can be compared to it
        // byte for byte. For BT.2020 the comparison is the *preview
        // curve*, applied before whichever basis change follows, so the
        // twin is asked for BT.709 there too.
        let want = if ws == WorkingColorspace::Rec709Linear {
            want
        } else {
            preview_frame(&linear, ToneMapOp::None, WorkingColorspace::Rec709Linear)
        };

        let mut worst = 0i32;
        let mut worst_at = 0usize;
        let mut differing = 0usize;
        for (i, (g, w)) in got.iter().zip(want.iter()).enumerate() {
            let d = (*g as i32 - *w as i32).abs();
            if d > 0 {
                differing += 1;
            }
            if d > worst {
                worst = d;
                worst_at = i;
            }
        }
        eprintln!(
            "{ws:?}: preview worst {worst} of 255 ({differing} of {} bytes differ), \
             worst at byte {worst_at}",
            got.len(),
        );
        assert!(worst <= 1, "{ws:?}: the preview differs by {worst} code values");
    }

    // And the roll-off is actually running: the same frame encoded without
    // it is a different picture. Otherwise this test would pass for a
    // shader that ignored `view_op` entirely.
    gpu.upload_chain_input_linear(&linear);
    let plain = gpu.encode_chain_to_display_u8(true, W, H, None).expect("plain encode");
    gpu.upload_chain_input_linear(&linear);
    let preview = gpu
        .encode_chain_to_display_u8_view(true, W, H, None, true)
        .expect("preview encode");
    let d = plain
        .iter()
        .zip(preview.iter())
        .map(|(a, b)| (*a as i32 - *b as i32).abs())
        .max()
        .unwrap_or(0);
    eprintln!("preview vs the plain encode on the GPU: worst {d} of 255");
    assert!(d > 8, "only {d}: the shader is not applying the roll-off");
}

/// The light map: the GPU measures what the CPU measures.
///
/// Exactly, not approximately — both take the same sample (the top-left
/// pixel of each cell, not an average), both use the brightest channel in
/// BT.2020, and both scale by the same reference white. Any real
/// difference here is a wrong index or a wrong matrix.
#[test]
fn the_nit_map_matches_the_cpu() {
    let Some((_ctx, mut gpu)) = pipeline() else {
        eprintln!("skipping: no usable GPU");
        return;
    };
    let linear = display_light();

    for ws in [
        WorkingColorspace::Rec709Linear,
        WorkingColorspace::Bt2020Linear,
        WorkingColorspace::AcesCG,
    ] {
        let measure = HdrMeasure::for_working_space(ws);
        let to_2020 = klipp_effects::color_wgsl::pack_primaries_matrix(
            klipp_color::primaries::working_to_bt2020(ws),
        );
        for step in [1u32, 4, 7] {
            gpu.ensure_chain_buffers((W as u64) * (H as u64) * 16);
            gpu.upload_chain_input_linear(&linear);
            let (got, gw, gh) =
                gpu.nit_map_of_chain(true, W, H, step, to_2020).expect("the nit map was refused");
            let (want, ww, wh) = measure.nit_map(&linear, W, H, step);
            assert_eq!((gw, gh), (ww, wh), "{ws:?} step {step}: map dimensions");
            assert_eq!(got.len(), want.len());

            let mut worst = 0.0f32;
            let mut worst_at = 0usize;
            for (i, (g, w)) in got.iter().zip(want.iter()).enumerate() {
                let d = (g - w).abs();
                if d > worst {
                    worst = d;
                    worst_at = i;
                }
            }
            // Absolute cd/m², against a frame that reaches ~2000. A
            // thousandth of a nit is float noise in the matrix multiply;
            // a wrong index or matrix would be tens or hundreds.
            eprintln!(
                "{ws:?} step {step}: {gw}x{gh} map, worst {worst:.5} cd/m² \
                 (cell {worst_at}, cpu {:.3})",
                want[worst_at],
            );
            assert!(
                worst < 0.01,
                "{ws:?} step {step}: the GPU map is out by {worst} cd/m² at cell {worst_at}",
            );
            // The frame has to reach real HDR levels, or agreeing about
            // it says nothing.
            let peak = want.iter().cloned().fold(0.0f32, f32::max);
            assert!(peak > 900.0, "{ws:?}: the fixture only reaches {peak} cd/m²");
        }
    }

    // The basis matters, and the map is measured in BT.2020 for that
    // reason: the same working values read differently once the project's
    // basis is declared. Without this the matrix could be an identity and
    // every comparison above would still hold.
    let bt709 = HdrMeasure::for_working_space(WorkingColorspace::Rec709Linear)
        .nit_map(&linear, W, H, 4)
        .0;
    let bt2020 = HdrMeasure::for_working_space(WorkingColorspace::Bt2020Linear)
        .nit_map(&linear, W, H, 4)
        .0;
    let spread = bt709
        .iter()
        .zip(bt2020.iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    eprintln!("the same frame measured in BT.709 vs BT.2020: {spread:.1} cd/m² apart");
    assert!(spread > 1.0, "the working basis makes no difference, so the matrix is untested");
}

/// What monitoring costs with the frame on the GPU against off it.
///
/// A probe, not a pass/fail test:
/// `cargo test --release -p klipp-engine --test hdr_monitor_gpu_matches_cpu -- --ignored --nocapture`
///
/// Both routes start from the same graded 4K frame in the chain buffer.
/// One encodes the preview and measures the light where the frame is; the
/// other brings 132 MB of floats down and does both on the CPU — which is
/// what turning monitoring on used to cost, on top of losing the fast path
/// for the rest of the pipeline.
#[test]
#[ignore = "perf probe: run explicitly"]
fn monitoring_on_the_gpu_beats_the_round_trip() {
    let (w, h) = (3840u32, 2160u32);
    let n = (w as usize) * (h as usize);
    let Some((_ctx, mut gpu)) = pipeline() else {
        eprintln!("skipping: no usable GPU");
        return;
    };
    let ws = WorkingColorspace::Rec709Linear;
    let measure = HdrMeasure::for_working_space(ws);
    let to_2020 = klipp_effects::color_wgsl::pack_primaries_matrix(
        klipp_color::primaries::working_to_bt2020(ws),
    );

    let mut linear = Vec::with_capacity(n * 4);
    for i in 0..n {
        let v = ((i / 4096) % 1024) as f32 / 1024.0 * 10.0;
        let t = (i % 4096) as f32 / 4096.0;
        linear.extend_from_slice(&[v * (0.2 + 0.8 * t), v * (0.9 - 0.4 * t), v * (0.1 + 0.9 * t), 1.0]);
    }
    gpu.ensure_chain_buffers((n as u64) * 16);
    gpu.upload_chain_input_linear(&linear);

    // Warm.
    let _ = gpu.encode_chain_to_display_u8_view(true, w, h, None, true);
    let _ = gpu.nit_map_of_chain(true, w, h, 4, to_2020);

    let median = |mut v: Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };
    let mut on_gpu = Vec::new();
    for _ in 0..5 {
        let t = std::time::Instant::now();
        let bytes = gpu.encode_chain_to_display_u8_view(true, w, h, None, true).expect("preview");
        let map = gpu.nit_map_of_chain(true, w, h, 4, to_2020).expect("map");
        on_gpu.push(t.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(bytes.len(), n * 4);
        assert!(map.0.iter().cloned().fold(0.0f32, f32::max) > 900.0);
    }

    let mut round_trip = Vec::new();
    for _ in 0..3 {
        let t = std::time::Instant::now();
        let floats = gpu.download_chain_output_linear(true, n);
        let read_ms = t.elapsed().as_secs_f64() * 1000.0;
        let t2 = std::time::Instant::now();
        let bytes = preview_frame(&floats, ToneMapOp::None, ws);
        let map = measure.nit_map(&floats, w, h, 4);
        let cpu_ms = t2.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(bytes.len(), n * 4);
        assert!(map.1 > 0);
        eprintln!("  round trip: readback {read_ms:.1} ms + preview and map {cpu_ms:.1} ms");
        round_trip.push(read_ms + cpu_ms);
    }

    let (g, c) = (median(on_gpu), median(round_trip));
    eprintln!(
        "4K HDR monitoring: preview and light map on the GPU {g:.1} ms/frame, \
         via the CPU round trip {c:.1} ms/frame ({:.1}x)",
        c / g
    );
}
