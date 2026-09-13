//! What HDR monitoring costs the viewer per 4K frame, on the CPU.
//! A probe, not a pass/fail test:
//! `cargo test --release -p klipp-color --test hdr_view_cost_probe -- --ignored --nocapture`
#[test]
#[ignore = "perf probe: run explicitly"]
fn cost() {
    use klipp_color::hdr_view::{hdr_to_sdr_preview, preview_frame, HdrMeasure};
    let (w, h) = (3840u32, 2160u32);
    let n = (w * h) as usize;
    let mut linear = Vec::with_capacity(n * 4);
    for i in 0..n {
        let v = (i % 4096) as f32 / 4096.0 * 6.0;
        linear.extend_from_slice(&[v, v * 0.8, v * 0.6, 1.0]);
    }
    let m = HdrMeasure::for_working_space(klipp_core::WorkingColorspace::Rec709Linear);
    let ws = klipp_core::WorkingColorspace::Rec709Linear;
    let op = klipp_core::ToneMapOp::None;

    for _ in 0..3 {
        // The route this replaced: copy the frame, roll it off in place,
        // encode the copy.
        let t = std::time::Instant::now();
        let mut copy = linear.clone();
        use rayon::prelude::*;
        copy.par_chunks_exact_mut(4).for_each(|px| {
            for c in 0..3 {
                px[c] = hdr_to_sdr_preview(px[c]);
            }
        });
        let old = klipp_color::display::display_transform_ws(&copy, op, ws);
        let old_ms = t.elapsed().as_secs_f64() * 1000.0;

        let t = std::time::Instant::now();
        let new = preview_frame(&linear, op, ws);
        let new_ms = t.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(old, new);

        let t = std::time::Instant::now();
        let map = m.nit_map(&linear, w, h, 4);
        let nit_ms = t.elapsed().as_secs_f64() * 1000.0;

        let t = std::time::Instant::now();
        let fc = m.false_colour_frame(&linear);
        let fc_ms = t.elapsed().as_secs_f64() * 1000.0;
        assert!(!fc.is_empty() && map.1 > 0);

        eprintln!(
            "4K preview: two-pass {old_ms:5.1} ms -> fused {new_ms:5.1} ms \
             | nit map {nit_ms:4.1} | false colour {fc_ms:5.1}"
        );
    }
}

/// The fused preview is byte-identical to the copy-and-map version it
/// replaced, in every working space.
///
/// The point of fusing was speed, so the thing to prove is that nothing
/// else moved. The old code copied the frame, applied the roll-off in
/// place and encoded that; this reads each pixel once. Same arithmetic,
/// same order, so it must agree exactly — not to a code value, to the
/// byte.
#[test]
fn the_fused_preview_is_the_two_pass_one() {
    use klipp_color::hdr_view::{hdr_to_sdr_preview, preview_frame};
    use klipp_core::{ToneMapOp, WorkingColorspace as W};

    // Deterministic noise across the HDR range, plus negatives and the
    // levels the bands sit on.
    let mut st = 0x853C_49E6_748F_EA9Bu64;
    let mut next = move || {
        st ^= st << 13;
        st ^= st >> 7;
        st ^= st << 17;
        (st >> 11) as f32 / (1u64 << 53) as f32
    };
    let mut linear: Vec<f32> = Vec::new();
    for _ in 0..20_000 {
        // 0 to ~10x reference white, and some below zero.
        let k = if next() < 0.05 { -0.2 } else { 10.0 };
        linear.extend_from_slice(&[next() * k, next() * k, next() * k, 1.0]);
    }
    for v in [0.0f32, 0.128, 0.75, 1.0, 1.0001, 4.926, 100.0] {
        linear.extend_from_slice(&[v, v, v, 1.0]);
    }

    for ws in [W::Rec709Linear, W::Bt2020Linear, W::AcesCG] {
        for op in [ToneMapOp::None, ToneMapOp::Hable] {
            // The old route, verbatim.
            let mut copy = linear.clone();
            use rayon::prelude::*;
            copy.par_chunks_exact_mut(4).for_each(|px| {
                for c in 0..3 {
                    px[c] = hdr_to_sdr_preview(px[c]);
                }
            });
            let want = klipp_color::display::display_transform_ws(&copy, op, ws);
            let got = preview_frame(&linear, op, ws);
            assert_eq!(got.len(), want.len(), "{ws:?}/{op:?}");
            let differing = got.iter().zip(want.iter()).filter(|(a, b)| a != b).count();
            assert_eq!(differing, 0, "{ws:?}/{op:?}: {differing} bytes differ");
        }
    }

    // And the roll-off actually did something: without it the picture
    // above reference white would be a different set of bytes.
    let plain = klipp_color::display::display_transform_ws(&linear, ToneMapOp::None, W::Rec709Linear);
    let preview = preview_frame(&linear, ToneMapOp::None, W::Rec709Linear);
    let worst = plain
        .iter()
        .zip(preview.iter())
        .map(|(a, b)| (*a as i32 - *b as i32).abs())
        .max()
        .unwrap_or(0);
    eprintln!("preview vs a plain encode: worst {worst} of 255");
    assert!(worst > 8, "only {worst}: the roll-off is not reaching the picture");
}
