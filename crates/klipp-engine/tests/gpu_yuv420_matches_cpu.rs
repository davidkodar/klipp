//! `GpuEffectsPipeline::encode_chain_to_yuv420` is `encode_linear_direct`.
//!
//! This shader is the export's last step and the reason a graded frame
//! no longer crosses the bus: display transform, colour matrix and
//! chroma subsampling happen on the GPU, and only 12.4 MB of Y'CbCr
//! planes come down instead of a 132 MB linear frame. Which means it has
//! to produce exactly what the CPU pass it replaces produced.
//!
//! The display half of the reference is the real thing —
//! `ExportDisplay::encode`, the same call `encode_linear_direct` makes.
//! The Y'CbCr half is transcribed from that function, deliberately, so
//! the test states the contract rather than sharing an implementation
//! with the code under test: quantise luma from the display-encoded RGB
//! at 16/219, and chroma from the *mean of the display-encoded RGB* over
//! each 2x2 quad at 128/224 — the mean of the RGB, not the mean of the
//! chroma, which is a different number and the easy thing to get wrong.

use klipp_color::display::ExportDisplay;
use klipp_core::{OdtChoice, ToneMapOp, WorkingColorspace};
use klipp_effects::{gpu::GpuEffectsPipeline, gpu_context::GpuContext};

/// Width a multiple of 8 and height even, which is what the shader's
/// block layout requires and the host checks.
/// Big enough to find rounding boundaries. At 2048 pixels the two paths
/// agreed exactly, which was misleading: `powf` on the CPU and `pow` in
/// the shader land either side of a half-level often enough to matter at
/// a real frame's 8.3 million, and a test that cannot see that reports
/// an agreement the export does not have.
const W: u32 = 512;
const H: u32 = 256;

const KR: f32 = 0.2126;
const KB: f32 = 0.0722;

fn tone_map_id(op: ToneMapOp) -> u32 {
    match op {
        ToneMapOp::None => 0,
        ToneMapOp::Reinhard => 1,
        ToneMapOp::Hable => 2,
        ToneMapOp::Aces => 3, // the shader refuses this
    }
}

/// A frame that exercises what the transform has to cope with: a wide
/// luminance ramp reaching well past white, saturated primaries, and
/// negatives, which a wide-gamut colour arrives as and which the twin
/// clamps *before* the tone map.
fn linear_frame() -> Vec<f32> {
    let mut px = vec![0.0f32; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            let t = x as f32 / W as f32;
            let v = y as f32 / H as f32;
            // 0 .. 8x white down the frame, hue rotating across it.
            px[i] = (v * 8.0) * (0.2 + 0.8 * t);
            px[i + 1] = (v * 8.0) * (0.9 - 0.5 * t);
            px[i + 2] = (v * 8.0) * (0.1 + 0.9 * (1.0 - t));
            px[i + 3] = 1.0;
        }
    }
    // A row of negatives, and a row of pure primaries at the clipping
    // point, both of which take different branches to the ramp.
    for x in 0..W as usize {
        let i = x * 4;
        px[i] = -0.3;
        px[i + 1] = -0.05;
        px[i + 2] = 0.4;
        let j = (W as usize + x) * 4;
        px[j] = if x % 3 == 0 { 1.0 } else { 0.0 };
        px[j + 1] = if x % 3 == 1 { 1.0 } else { 0.0 };
        px[j + 2] = if x % 3 == 2 { 1.0 } else { 0.0 };
    }
    px
}

/// The CPU twin's `quant`: offset, scale, round half up, clamp to eight
/// bits.
fn quant(v: f32, off: f32, range: f32) -> u8 {
    (off + v * range + 0.5).clamp(0.0, 255.0) as u8
}

/// `encode_linear_direct` for 8-bit 4:2:0, as Y, Cb and Cr planes.
fn cpu_planes(linear: &[f32], display: &ExportDisplay) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let kg = 1.0 - KR - KB;
    let luma = |e: [f32; 3]| KR * e[0] + kg * e[1] + KB * e[2];
    let (wu, hu) = (W as usize, H as usize);

    let e: Vec<[f32; 3]> = (0..wu * hu)
        .map(|i| display.encode([linear[i * 4], linear[i * 4 + 1], linear[i * 4 + 2]]))
        .collect();

    let y: Vec<u8> = e.iter().map(|p| quant(luma(*p), 16.0, 219.0)).collect();

    let mut cb = Vec::with_capacity(wu * hu / 4);
    let mut cr = Vec::with_capacity(wu * hu / 4);
    for by in 0..hu / 2 {
        for bx in 0..wu / 2 {
            let mut acc = [0.0f32; 3];
            for r in 0..2 {
                for c in 0..2 {
                    let p = e[(by * 2 + r) * wu + bx * 2 + c];
                    acc[0] += p[0];
                    acc[1] += p[1];
                    acc[2] += p[2];
                }
            }
            let mean = [acc[0] / 4.0, acc[1] / 4.0, acc[2] / 4.0];
            let l = luma(mean);
            cb.push(quant((mean[2] - l) / (2.0 * (1.0 - KB)), 128.0, 224.0));
            cr.push(quant((mean[0] - l) / (2.0 * (1.0 - KR)), 128.0, 224.0));
        }
    }
    (y, cb, cr)
}

fn target(op: ToneMapOp, semi: bool, bits: u32, shift: u32) -> klipp_effects::gpu::Yuv420Target {
    klipp_effects::gpu::Yuv420Target {
        tone_map_op: tone_map_id(op),
        gamut_op: 0,
        hdr_op: 0,
        peak_nits: 0.0,
        luma_coefs: [KR, 1.0 - KR - KB, KB],
        semi_planar: semi,
        bits,
        sample_shift: shift,
    }
}

fn gpu_planes_at(
    linear: &[f32],
    op: ToneMapOp,
    semi: bool,
    bits: u32,
    shift: u32,
) -> Option<Vec<u8>> {
    let Some(ctx) = GpuContext::new() else {
        eprintln!("skipping: no GpuContext");
        return None;
    };
    let Some(mut pipeline) = GpuEffectsPipeline::with_context(&ctx) else {
        eprintln!("skipping: no GpuEffectsPipeline");
        return None;
    };
    pipeline.ensure_chain_buffers((W as u64) * (H as u64) * 16);
    pipeline.upload_chain_input_linear(linear);
    let planes = pipeline.encode_chain_to_yuv420(true, W, H, target(op, semi, bits, shift), &[]);
    if planes.is_none() {
        panic!("encode_chain_to_yuv420 refused {W}x{H}, {op:?}, {bits} bits");
    }
    planes
}

fn gpu_planes(linear: &[f32], op: ToneMapOp, semi: bool) -> Option<Vec<u8>> {
    gpu_planes_at(linear, op, semi, 8, 0)
}

/// Both chroma layouts, against the same reference. NV12's interleaved
/// plane is what NVENC takes and therefore what the export actually
/// uses, so testing only the planar one would test the path nothing
/// runs — and a Cb/Cr swap in the interleave is invisible on a grey
/// fixture, which is why this one has hues in it.
fn check(op: ToneMapOp) {
    check_layout(op, false);
    check_layout(op, true);
}

fn check_layout(op: ToneMapOp, semi: bool) {
    let linear = linear_frame();
    let Some(planes) = gpu_planes(&linear, op, semi) else {
        eprintln!("skipping: no usable GPU");
        return;
    };
    let display = ExportDisplay::new(op, WorkingColorspace::Rec709Linear, OdtChoice::MatchViewer);
    assert!(
        display.tone_map_and_srgb_only().is_some(),
        "the fixture must be on the plain tone-map-and-sRGB path"
    );
    let (y, cb, cr) = cpu_planes(&linear, &display);

    assert_eq!(planes.len(), y.len() + cb.len() + cr.len(), "{op:?}: plane sizes");
    let (g_y, chroma) = planes.split_at(y.len());
    // De-interleave NV12 so both layouts meet the same reference.
    let (g_cb, g_cr): (Vec<u8>, Vec<u8>) = if semi {
        (
            chroma.iter().step_by(2).copied().collect(),
            chroma.iter().skip(1).step_by(2).copied().collect(),
        )
    } else {
        let (a, b) = chroma.split_at(cb.len());
        (a.to_vec(), b.to_vec())
    };
    let layout = if semi { "NV12" } else { "YUV420P" };

    // One level: the CPU twin computes in f32 with `powf`, the shader in
    // f32 with `pow`, and the two can land either side of a rounding
    // boundary. Anything larger is a different picture, not a different
    // rounding.
    for (name, got, want) in
        [("Y", g_y, &y), ("Cb", g_cb.as_slice(), &cb), ("Cr", g_cr.as_slice(), &cr)]
    {
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
        assert!(
            worst <= 1,
            "{op:?} {layout} {name}: differs by {worst} at sample {worst_at} \
             (gpu {} vs cpu {}), {differing} samples differ",
            got[worst_at],
            want[worst_at],
        );
        eprintln!("{op:?} {layout} {name}: worst {worst}, {differing}/{} differ", got.len());
    }
}

#[test]
fn gpu_yuv_matches_cpu_with_no_tone_map() {
    check(ToneMapOp::None);
}

#[test]
fn gpu_yuv_matches_cpu_with_reinhard() {
    check(ToneMapOp::Reinhard);
}

#[test]
fn gpu_yuv_matches_cpu_with_hable() {
    check(ToneMapOp::Hable);
}

/// A tone map the shader does not implement must be refused, not
/// approximated. ACES is two matrices around a curve and is not
/// separable, so a shader that quietly ran the per-channel branch would
/// desaturate warm highlights — the exact artefact the real transform
/// exists to avoid.
#[test]
fn an_unimplemented_tone_map_is_refused() {
    let linear = linear_frame();
    let Some(ctx) = GpuContext::new() else { return };
    let Some(mut pipeline) = GpuEffectsPipeline::with_context(&ctx) else { return };
    pipeline.ensure_chain_buffers((W as u64) * (H as u64) * 16);
    pipeline.upload_chain_input_linear(&linear);
    assert!(
        pipeline
            .encode_chain_to_yuv420(true, W, H, target(ToneMapOp::Aces, false, 8, 0), &[])
            .is_none(),
        "ACES must fall back to the CPU pass, not be approximated"
    );
}

/// Dimensions the block layout cannot tile are refused too. Eight-pixel
/// blocks are what make every plane write land on a whole word, so a
/// width of 60 would have the last block write past the row.
#[test]
fn dimensions_the_block_layout_cannot_tile_are_refused() {
    let Some(ctx) = GpuContext::new() else { return };
    let Some(mut pipeline) = GpuEffectsPipeline::with_context(&ctx) else { return };
    pipeline.ensure_chain_buffers(64 * 64 * 16);
    pipeline.upload_chain_input_linear(&vec![0.5f32; 64 * 64 * 4]);
    assert!(
        pipeline.encode_chain_to_yuv420(true, 60, 32, target(ToneMapOp::None, false, 8, 0), &[]).is_none(),
        "width 60 is not a multiple of 8"
    );
    assert!(
        pipeline.encode_chain_to_yuv420(true, 64, 31, target(ToneMapOp::None, false, 8, 0), &[]).is_none(),
        "height 31 is odd"
    );
}

/// Ten-bit output, against the same reference scaled to ten bits.
///
/// `encode_linear_direct`'s `quant` takes its offset and range in 8-bit
/// units and multiplies by `1 << (bits - 8)`, so the luma range is
/// 64..940 at ten bits rather than 16..235. Doing that arithmetic here
/// rather than reusing the 8-bit numbers is the point: a shader that
/// quantised to eight bits and shifted would pass a test that compared
/// against itself, and produce a picture with 8-bit banding in a 10-bit
/// file.
fn cpu_planes_10(display: &ExportDisplay, linear: &[f32]) -> (Vec<u16>, Vec<u16>, Vec<u16>) {
    cpu_planes_10_with(display, linear, KR, KB)
}

/// [`cpu_planes_10`] with the output's own luma coefficients, since an
/// HDR delivery carries BT.2020's rather than BT.709's.
fn cpu_planes_10_with(
    display: &ExportDisplay,
    linear: &[f32],
    kr: f32,
    kb: f32,
) -> (Vec<u16>, Vec<u16>, Vec<u16>) {
    let kg = 1.0 - kr - kb;
    let luma = |e: [f32; 3]| kr * e[0] + kg * e[1] + kb * e[2];
    let q = |v: f32, off: f32, range: f32| -> u16 {
        ((off + v * range) * 4.0 + 0.5).clamp(0.0, 1023.0) as u16
    };
    let (wu, hu) = (W as usize, H as usize);
    let e: Vec<[f32; 3]> = (0..wu * hu)
        .map(|i| display.encode([linear[i * 4], linear[i * 4 + 1], linear[i * 4 + 2]]))
        .collect();
    let y: Vec<u16> = e.iter().map(|p| q(luma(*p), 16.0, 219.0)).collect();
    let mut cb = Vec::new();
    let mut cr = Vec::new();
    for by in 0..hu / 2 {
        for bx in 0..wu / 2 {
            let mut acc = [0.0f32; 3];
            for r in 0..2 {
                for c in 0..2 {
                    let p = e[(by * 2 + r) * wu + bx * 2 + c];
                    for k in 0..3 {
                        acc[k] += p[k];
                    }
                }
            }
            let mean = [acc[0] / 4.0, acc[1] / 4.0, acc[2] / 4.0];
            let l = luma(mean);
            cb.push(q((mean[2] - l) / (2.0 * (1.0 - kb)), 128.0, 224.0));
            cr.push(q((mean[0] - l) / (2.0 * (1.0 - kr)), 128.0, 224.0));
        }
    }
    (y, cb, cr)
}

fn check_10bit(op: ToneMapOp, semi: bool, shift: u32) {
    let linear = linear_frame();
    let Some(planes) = gpu_planes_at(&linear, op, semi, 10, shift) else {
        eprintln!("skipping: no usable GPU");
        return;
    };
    let display = ExportDisplay::new(op, WorkingColorspace::Rec709Linear, OdtChoice::MatchViewer);
    let (y, cb, cr) = cpu_planes_10(&display, &linear);

    assert_eq!(planes.len(), (y.len() + cb.len() + cr.len()) * 2, "10-bit plane size");
    let words: Vec<u16> = planes
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]) >> shift)
        .collect();
    let (g_y, chroma) = words.split_at(y.len());
    let (g_cb, g_cr): (Vec<u16>, Vec<u16>) = if semi {
        (
            chroma.iter().step_by(2).copied().collect(),
            chroma.iter().skip(1).step_by(2).copied().collect(),
        )
    } else {
        let (a, b) = chroma.split_at(cb.len());
        (a.to_vec(), b.to_vec())
    };

    let layout = if semi { "P010" } else { "YUV420P10" };
    for (name, got, want) in
        [("Y", g_y, &y), ("Cb", g_cb.as_slice(), &cb), ("Cr", g_cr.as_slice(), &cr)]
    {
        let mut worst = 0i32;
        let mut worst_at = 0usize;
        for (i, (g, w)) in got.iter().zip(want.iter()).enumerate() {
            let d = (*g as i32 - *w as i32).abs();
            if d > worst {
                worst = d;
                worst_at = i;
            }
        }
        assert!(
            worst <= 1,
            "{op:?} {layout} {name}: differs by {worst} at {worst_at} (gpu {} vs cpu {})",
            got[worst_at],
            want[worst_at],
        );
        // Ten bits must actually be used. Quantising to eight and
        // scaling would still land within a level of nothing here, so
        // the values themselves have to reach past 255.
        let top = got.iter().copied().max().unwrap_or(0);
        assert!(top > 255, "{layout} {name}: nothing above 255, so this is 8-bit data");
        eprintln!("{op:?} {layout} {name}: worst {worst}, peak {top}");
    }
}

#[test]
fn gpu_yuv_matches_cpu_at_ten_bits_planar() {
    check_10bit(ToneMapOp::Hable, false, 0);
}

/// P010, which is what NVENC actually takes for a 10-bit delivery, and
/// which carries its ten bits at the *top* of each u16.
#[test]
fn gpu_yuv_matches_cpu_at_ten_bits_p010() {
    check_10bit(ToneMapOp::Hable, true, 6);
}

/// The P010 shift is real: without it the samples sit in the low bits
/// and the file reads about six stops dark.
#[test]
fn the_p010_shift_puts_the_bits_at_the_top() {
    let linear = linear_frame();
    let Some(shifted) = gpu_planes_at(&linear, ToneMapOp::None, true, 10, 6) else {
        eprintln!("skipping: no usable GPU");
        return;
    };
    let plain = gpu_planes_at(&linear, ToneMapOp::None, true, 10, 0).expect("second run");
    let word = |v: &[u8], i: usize| u16::from_le_bytes([v[i * 2], v[i * 2 + 1]]);
    // Find a sample that is not zero in the unshifted run, and check the
    // shifted run has it four-and-sixty times larger.
    let i = (0..1000).find(|&i| word(&plain, i) > 0).expect("a non-zero sample");
    assert_eq!(
        word(&shifted, i),
        word(&plain, i) << 6,
        "sample {i} was not shifted into the high bits",
    );
}

/// A depth the shader does not implement is refused rather than
/// approximated — 12-bit would otherwise silently pack as though it
/// were ten.
#[test]
fn an_unsupported_depth_is_refused() {
    let Some(ctx) = GpuContext::new() else { return };
    let Some(mut pipeline) = GpuEffectsPipeline::with_context(&ctx) else { return };
    pipeline.ensure_chain_buffers((W as u64) * (H as u64) * 16);
    pipeline.upload_chain_input_linear(&linear_frame());
    for bits in [0, 9, 12, 16] {
        assert!(
            pipeline
                .encode_chain_to_yuv420(true, W, H, target(ToneMapOp::None, true, bits, 0), &[])
                .is_none(),
            "{bits} bits was accepted",
        );
    }
}

/// The same agreement on noise rather than ramps, at both depths.
///
/// Smooth fixtures are kind to a quantiser: neighbouring samples land on
/// the same side of a rounding boundary, so a systematic error can hide.
/// This one is pseudo-random over a wide range, which is the closest
/// cheap thing to real footage, and it exists because comparing two
/// *encoded* files of the user's timeline showed differences up to 63
/// code values and I needed to know whether that was the planes or the
/// codec. It was the codec: HEVC makes different mode decisions from a
/// one-level difference and spreads them across a block.
#[test]
fn gpu_yuv_matches_cpu_on_noise() {
    // A cheap deterministic hash, so a failure is reproducible.
    let mut state = 0x243f_6a88_85a3_08d3_u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f32 / (1u64 << 53) as f32
    };
    let mut linear = vec![0.0f32; (W * H * 4) as usize];
    for px in linear.chunks_exact_mut(4) {
        // 0 .. 4x white, plus occasional negatives, as a wide-gamut
        // colour arrives.
        for c in 0..3 {
            let v = next() * 4.0;
            px[c] = if next() < 0.02 { -v * 0.25 } else { v };
        }
        px[3] = 1.0;
    }

    for (bits, shift, semi) in [(8u32, 0u32, true), (10, 6, true), (10, 0, false)] {
        let Some(planes) = gpu_planes_at(&linear, ToneMapOp::Hable, semi, bits, shift) else {
            eprintln!("skipping: no usable GPU");
            return;
        };
        let display = ExportDisplay::new(
            ToneMapOp::Hable,
            WorkingColorspace::Rec709Linear,
            OdtChoice::MatchViewer,
        );
        let (worst, n) = if bits == 8 {
            let (y, cb, cr) = cpu_planes(&linear, &display);
            let mut want: Vec<u16> = y.iter().map(|v| *v as u16).collect();
            if semi {
                for (a, b) in cb.iter().zip(cr.iter()) {
                    want.push(*a as u16);
                    want.push(*b as u16);
                }
            }
            let got: Vec<u16> = planes.iter().map(|v| *v as u16).collect();
            (
                got.iter().zip(want.iter()).map(|(g, w)| (*g as i32 - *w as i32).abs()).max().unwrap(),
                want.len(),
            )
        } else {
            let (y, cb, cr) = cpu_planes_10(&display, &linear);
            let mut want = y.clone();
            if semi {
                for (a, b) in cb.iter().zip(cr.iter()) {
                    want.push(*a);
                    want.push(*b);
                }
            } else {
                want.extend_from_slice(&cb);
                want.extend_from_slice(&cr);
            }
            let got: Vec<u16> = planes
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]) >> shift)
                .collect();
            (
                got.iter().zip(want.iter()).map(|(g, w)| (*g as i32 - *w as i32).abs()).max().unwrap(),
                want.len(),
            )
        };
        eprintln!("noise, {bits} bits, semi={semi}: worst {worst} over {n} samples");
        assert!(worst <= 1, "{bits} bits, semi={semi}: worst {worst} on noise");
    }
}

/// The combined path the export actually runs for an AcesCG project:
/// the ACES output transform *and* the Y'CbCr conversion in one shader.
///
/// `aces_gpu_matches_cpu` checks the transform alone against
/// `acescg_to_rec709_sdr`. This checks it composed with the colour
/// matrix and the chroma average, against `ExportDisplay` on the
/// AcesCG path — because the shader picks its display transform by a
/// parameter, and a wrong branch there would leave the transform
/// correct and the picture wrong.
#[test]
fn gpu_yuv_matches_cpu_on_the_aces_path() {
    const ACES_OP: u32 = 3;
    let linear = linear_frame();
    let aces = klipp_color::aces::aces_rec709_gpu_data(1024);

    let Some(ctx) = GpuContext::new() else {
        eprintln!("skipping: no GpuContext");
        return;
    };
    let Some(mut pipeline) = GpuEffectsPipeline::with_context(&ctx) else {
        eprintln!("skipping: no GpuEffectsPipeline");
        return;
    };
    pipeline.ensure_chain_buffers((W as u64) * (H as u64) * 16);
    pipeline.upload_chain_input_linear(&linear);
    let t = klipp_effects::gpu::Yuv420Target {
        tone_map_op: ACES_OP,
        gamut_op: 0,
        hdr_op: 0,
        peak_nits: 0.0,
        luma_coefs: [KR, 1.0 - KR - KB, KB],
        semi_planar: true,
        bits: 8,
        sample_shift: 0,
    };
    let planes = pipeline
        .encode_chain_to_yuv420(true, W, H, t, &aces)
        .expect("the ACES path was refused");

    // The reference: the real ExportDisplay on an AcesCG project.
    let display = ExportDisplay::new(
        ToneMapOp::None,
        WorkingColorspace::AcesCG,
        OdtChoice::MatchViewer,
    );
    assert!(display.is_aces_rec709(), "the fixture is not on the ACES path");
    let (y, cb, cr) = cpu_planes(&linear, &display);

    let (g_y, chroma) = planes.split_at(y.len());
    let g_cb: Vec<u8> = chroma.iter().step_by(2).copied().collect();
    let g_cr: Vec<u8> = chroma.iter().skip(1).step_by(2).copied().collect();
    for (name, got, want) in
        [("Y", g_y, &y), ("Cb", g_cb.as_slice(), &cb), ("Cr", g_cr.as_slice(), &cr)]
    {
        let worst = got
            .iter()
            .zip(want.iter())
            .map(|(g, w)| (*g as i32 - *w as i32).abs())
            .max()
            .unwrap_or(0);
        eprintln!("ACES {name}: worst {worst}");
        assert!(worst <= 1, "ACES {name}: differs by {worst}");
    }
    // And it is not accidentally the non-ACES path, which would also
    // agree with itself.
    let srgb = ExportDisplay::new(
        ToneMapOp::None,
        WorkingColorspace::Rec709Linear,
        OdtChoice::MatchViewer,
    );
    let (y_srgb, _, _) = cpu_planes(&linear, &srgb);
    let differing = y.iter().zip(y_srgb.iter()).filter(|(a, b)| a != b).count();
    assert!(
        differing > y.len() / 10,
        "the ACES reference barely differs from the sRGB one ({differing} samples), \
         so this test cannot tell the branches apart"
    );
}

/// The combined path the export runs for a BT.2020 project: the basis
/// change and the BT.2407 gamut compression, *then* the tone map, the
/// sRGB encode and the Y'CbCr conversion, all in one shader.
///
/// `bt2020_gamut_gpu_matches_cpu` checks the gamut work alone. This
/// checks it in its place in the chain, and that place is the point.
/// `ToBt709::apply` runs before the negatives are clamped and before the
/// tone map, so an operator other than None is the only thing that can
/// catch the two being swapped — with None the order is unobservable,
/// which is why this loops over all three rather than testing the
/// default and calling it covered.
#[test]
fn gpu_yuv_matches_cpu_on_the_bt2020_path() {
    let linear = linear_frame();
    let tables = klipp_color::gamut::bt2020_to_bt709_gpu_data();

    let Some(ctx) = GpuContext::new() else {
        eprintln!("skipping: no GpuContext");
        return;
    };
    let Some(mut pipeline) = GpuEffectsPipeline::with_context(&ctx) else {
        eprintln!("skipping: no GpuEffectsPipeline");
        return;
    };
    pipeline.ensure_chain_buffers((W as u64) * (H as u64) * 16);

    for op in [ToneMapOp::None, ToneMapOp::Reinhard, ToneMapOp::Hable] {
        let display = ExportDisplay::new(op, WorkingColorspace::Bt2020Linear, OdtChoice::MatchViewer);
        assert_eq!(
            display.bt2020_gamut_then_tone_map_srgb(),
            Some(op),
            "the fixture must be on the compressing path with {op:?}"
        );
        for (semi, bits, shift) in [(false, 8, 0), (true, 8, 0), (true, 10, 6)] {
            pipeline.upload_chain_input_linear(&linear);
            let t = klipp_effects::gpu::Yuv420Target {
                tone_map_op: tone_map_id(op),
                gamut_op: 1,
                hdr_op: 0,
                peak_nits: 0.0,
                luma_coefs: [KR, 1.0 - KR - KB, KB],
                semi_planar: semi,
                bits,
                sample_shift: shift,
            };
            let planes = pipeline
                .encode_chain_to_yuv420(true, W, H, t, &tables)
                .expect("the BT.2020 path was refused");

            // Compared at the output's own depth, since ten bits is where
            // a rounding difference has room to show.
            let label = format!("BT.2020 {op:?} {bits}-bit semi={semi}");
            if bits == 8 {
                let (y, cb, cr) = cpu_planes(&linear, &display);
                let (g_y, chroma) = planes.split_at(y.len());
                let (g_cb, g_cr): (Vec<u8>, Vec<u8>) = if semi {
                    (
                        chroma.iter().step_by(2).copied().collect(),
                        chroma.iter().skip(1).step_by(2).copied().collect(),
                    )
                } else {
                    let (a, b) = chroma.split_at(cb.len());
                    (a.to_vec(), b.to_vec())
                };
                for (name, got, want) in [
                    ("Y", g_y, &y),
                    ("Cb", g_cb.as_slice(), &cb),
                    ("Cr", g_cr.as_slice(), &cr),
                ] {
                    let worst = got
                        .iter()
                        .zip(want.iter())
                        .map(|(g, w)| (*g as i32 - *w as i32).abs())
                        .max()
                        .unwrap_or(0);
                    eprintln!("{label} {name}: worst {worst}");
                    assert!(worst <= 1, "{label} {name}: differs by {worst}");
                }
            } else {
                let (y, cb, cr) = cpu_planes_10(&display, &linear);
                let words: Vec<u16> = planes
                    .chunks_exact(2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]]) >> shift)
                    .collect();
                let (g_y, chroma) = words.split_at(y.len());
                let g_cb: Vec<u16> = chroma.iter().step_by(2).copied().collect();
                let g_cr: Vec<u16> = chroma.iter().skip(1).step_by(2).copied().collect();
                for (name, got, want) in [
                    ("Y", g_y, &y),
                    ("Cb", g_cb.as_slice(), &cb),
                    ("Cr", g_cr.as_slice(), &cr),
                ] {
                    let worst = got
                        .iter()
                        .zip(want.iter())
                        .map(|(g, w)| (*g as i32 - *w as i32).abs())
                        .max()
                        .unwrap_or(0);
                    eprintln!("{label} {name}: worst {worst} of 1023");
                    assert!(worst <= 1, "{label} {name}: differs by {worst}");
                }
            }
        }

        // And the gamut branch is doing something: the same operator
        // without it is a different picture, so the agreement above is
        // not the shader having ignored `gamut_op` and the reference
        // having ignored the basis.
        let plain = ExportDisplay::new(op, WorkingColorspace::Rec709Linear, OdtChoice::MatchViewer);
        let (y, ..) = cpu_planes(&linear, &display);
        let (y_plain, ..) = cpu_planes(&linear, &plain);
        let differing = y.iter().zip(y_plain.iter()).filter(|(a, b)| a != b).count();
        assert!(
            differing > y.len() / 10,
            "{op:?}: the BT.2020 reference barely differs from the BT.709 one \
             ({differing} of {} samples), so this cannot tell the branches apart",
            y.len(),
        );
    }
}

/// The HDR deliveries: PQ and HLG, encoded and converted in one shader.
///
/// These were the last two on the CPU float route, and PQ was there for a
/// second reason beyond the encode — HDR10's MaxCLL and MaxFALL are
/// measured per pixel in cd/m², which the twin does while it encodes, off
/// the linear frame. So this checks both halves: the planes against
/// `ExportDisplay`, and the light against the same `encode_light` the
/// twin accumulates.
///
/// Ten bits and P010 rather than eight, because that is what an HDR
/// delivery actually is; `the_p010_shift_puts_the_bits_at_the_top` covers
/// the packing separately.
#[test]
fn gpu_yuv_matches_cpu_on_the_hdr_paths() {
    let linear = linear_frame();
    let tables =
        klipp_color::hdr::hdr_encode_gpu_data(klipp_core::WorkingColorspace::Rec709Linear);

    let Some(ctx) = GpuContext::new() else {
        eprintln!("skipping: no GpuContext");
        return;
    };
    let Some(mut pipeline) = GpuEffectsPipeline::with_context(&ctx) else {
        eprintln!("skipping: no GpuEffectsPipeline");
        return;
    };
    pipeline.ensure_chain_buffers((W as u64) * (H as u64) * 16);

    for (mode, peak, hdr_op) in [
        (klipp_core::HdrMode::Pq, 1000.0f32, 1u32),
        (klipp_core::HdrMode::Pq, 4000.0, 1),
        (klipp_core::HdrMode::Hlg, 1000.0, 2),
    ] {
        let display = ExportDisplay::hdr(WorkingColorspace::Rec709Linear, mode, peak);
        assert_eq!(
            display.hdr_transfer_and_peak(),
            Some((hdr_op == 1, if hdr_op == 1 { peak } else { 1000.0 })),
            "the fixture must be on the {mode:?} path"
        );

        pipeline.upload_chain_input_linear(&linear);
        let t = klipp_effects::gpu::Yuv420Target {
            tone_map_op: 0,
            gamut_op: 0,
            hdr_op,
            peak_nits: peak,
            // An HDR delivery carries BT.2020's luma coefficients.
            luma_coefs: [0.2627, 0.6780, 0.0593],
            semi_planar: true,
            bits: 10,
            sample_shift: 6,
        };
        // PQ goes through the measuring form, which is the only one that
        // answers for it; HLG through the plain one.
        let (planes, light) = if hdr_op == 1 {
            let (p, l) = pipeline
                .encode_chain_to_yuv420_measured(true, W, H, t, &tables)
                .expect("the PQ path was refused");
            (p, Some(l))
        } else {
            assert!(
                pipeline.encode_chain_to_yuv420_measured(true, W, H, t, &tables).is_none(),
                "HLG measures no light, so the measuring form must refuse it"
            );
            pipeline.upload_chain_input_linear(&linear);
            (
                pipeline
                    .encode_chain_to_yuv420(true, W, H, t, &tables)
                    .expect("the HLG path was refused"),
                None,
            )
        };

        // The planes, at the output's own depth.
        let label = format!("{mode:?} {peak:.0} nit");
        let (kr, kb) = (0.2627f32, 0.0593f32);
        let (y, cb, cr) = cpu_planes_10_with(&display, &linear, kr, kb);
        let words: Vec<u16> =
            planes.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]]) >> 6).collect();
        let (g_y, chroma) = words.split_at(y.len());
        let g_cb: Vec<u16> = chroma.iter().step_by(2).copied().collect();
        let g_cr: Vec<u16> = chroma.iter().skip(1).step_by(2).copied().collect();
        for (name, got, want) in
            [("Y", g_y, &y), ("Cb", g_cb.as_slice(), &cb), ("Cr", g_cr.as_slice(), &cr)]
        {
            let worst = got
                .iter()
                .zip(want.iter())
                .map(|(g, w)| (*g as i32 - *w as i32).abs())
                .max()
                .unwrap_or(0);
            eprintln!("{label} {name}: worst {worst} of 1023");
            assert!(worst <= 1, "{label} {name}: differs by {worst}");
        }

        // And the light, against the twin's own accumulation.
        if let Some(l) = light {
            let mut max = 0.0f32;
            let mut sum = 0.0f64;
            for i in 0..(W as usize) * (H as usize) {
                let (_, nits) =
                    display.encode_light([linear[i * 4], linear[i * 4 + 1], linear[i * 4 + 2]]);
                max = max.max(nits);
                sum += nits as f64;
            }
            let pixels = (W as u64) * (H as u64);
            assert_eq!(l.pixels, pixels, "{label}: the shader counted the wrong pixels");
            let (gm, cm) = (l.sum_nits / pixels as f64, sum / pixels as f64);
            eprintln!(
                "{label} light: max gpu {:.4} cpu {max:.4} nits, mean gpu {gm:.4} cpu {cm:.4}",
                l.max_nits,
            );

            // Compared as the *metadata*, which is what the contract
            // actually is: both numbers go through `HdrLightStats` and
            // reach the file as whole nits. Comparing the floats directly
            // is too strict — the matrix multiply is a different
            // instruction order on each side, so the brightest channel
            // lands a ULP apart and the peak clamp carries that through.
            let stats = |max: f32, mean: f32| {
                let mut s = klipp_color::hdr::LightLevelStats::new();
                s.add_frame(max, mean);
                (s.max_cll_nits(), s.max_fall_nits())
            };
            let got = stats(l.max_nits, gm as f32);
            let want = stats(max, cm as f32);
            assert_eq!(got, want, "{label}: (MaxCLL, MaxFALL) differ");
            // And they are close as floats too, so a boundary case is the
            // only thing the quantisation above could be hiding.
            assert!(
                (l.max_nits - max).abs() <= max * 1e-5,
                "{label}: MaxCLL is {} vs {max}", l.max_nits,
            );
            assert!(
                (gm - cm).abs() < 0.01 + cm.abs() * 1e-5,
                "{label}: MaxFALL differs, gpu {gm} vs cpu {cm}"
            );
            assert!(max > 100.0, "{label}: the fixture is too dim to measure light with");
        }
    }
}

/// What the HDR branch is worth, at 4K.
///
/// A probe, not a pass/fail test:
/// `cargo test --release -p klipp-engine --test gpu_yuv420_matches_cpu -- --ignored --nocapture`
///
/// Both routes start from the same graded frame in the chain buffer. One
/// encodes and converts it on the GPU, measuring the light on the way,
/// and reads back 12.4 MB of P010. The other reads back 132 MB of floats
/// and runs `encode_light` per pixel — which is what a PQ delivery paid
/// before, on top of the Y'CbCr conversion this does not even charge for.
#[test]
#[ignore = "perf probe: run explicitly"]
fn the_hdr_path_on_the_gpu_beats_the_round_trip() {
    let (w, h) = (3840u32, 2160u32);
    let n = (w as usize) * (h as usize);
    let Some(ctx) = GpuContext::new() else {
        eprintln!("skipping: no usable GPU");
        return;
    };
    let mut pipeline = GpuEffectsPipeline::with_context(&ctx).expect("pipeline");
    let tables =
        klipp_color::hdr::hdr_encode_gpu_data(klipp_core::WorkingColorspace::Rec709Linear);

    // A frame with real highlight range, so the peak clamp and the table
    // lookup both do work.
    let mut linear = Vec::with_capacity(n * 4);
    for i in 0..n {
        let t = (i % 4096) as f32 / 4096.0;
        let v = ((i / 4096) % 1024) as f32 / 1024.0 * 12.0;
        linear.extend_from_slice(&[v * (0.2 + 0.8 * t), v * (0.9 - 0.4 * t), v * (0.1 + 0.9 * t), 1.0]);
    }
    pipeline.ensure_chain_buffers((n as u64) * 16);
    pipeline.upload_chain_input_linear(&linear);

    let t = klipp_effects::gpu::Yuv420Target {
        tone_map_op: 0,
        gamut_op: 0,
        hdr_op: 1,
        peak_nits: 1000.0,
        luma_coefs: [0.2627, 0.6780, 0.0593],
        semi_planar: true,
        bits: 10,
        sample_shift: 6,
    };
    let _ = pipeline.encode_chain_to_yuv420_measured(true, w, h, t, &tables);
    let mut gpu_ms = Vec::new();
    for _ in 0..5 {
        let start = std::time::Instant::now();
        let (planes, light) =
            pipeline.encode_chain_to_yuv420_measured(true, w, h, t, &tables).expect("encode");
        gpu_ms.push(start.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(planes.len(), t.byte_len(w, h));
        assert!(light.max_nits > 0.0);
    }

    let display = ExportDisplay::hdr(
        WorkingColorspace::Rec709Linear,
        klipp_core::HdrMode::Pq,
        1000.0,
    );
    let mut cpu_ms = Vec::new();
    for _ in 0..3 {
        let start = std::time::Instant::now();
        let floats = pipeline.download_chain_output_linear(true, n);
        let read_ms = start.elapsed().as_secs_f64() * 1000.0;
        let t2 = std::time::Instant::now();
        use rayon::prelude::*;
        let light: (f32, f64) = floats
            .par_chunks_exact(4)
            .map(|p| {
                let (_, nits) = display.encode_light([p[0], p[1], p[2]]);
                (nits, nits as f64)
            })
            .reduce(|| (0.0, 0.0), |a, b| (a.0.max(b.0), a.1 + b.1));
        let enc_ms = t2.elapsed().as_secs_f64() * 1000.0;
        assert!(light.0 > 0.0);
        eprintln!("  round trip: readback {read_ms:.1} ms + encode_light {enc_ms:.1} ms");
        cpu_ms.push(read_ms + enc_ms);
    }

    let median = |mut v: Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };
    let (g, c) = (median(gpu_ms), median(cpu_ms));
    eprintln!(
        "4K PQ delivery: transform, Y'CbCr and MaxCLL on the GPU {g:.1} ms/frame, \
         against {c:.1} ms/frame for the round trip and the transform alone ({:.1}x)",
        c / g
    );
}
