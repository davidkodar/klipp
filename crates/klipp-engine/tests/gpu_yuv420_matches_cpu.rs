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

fn gpu_planes(linear: &[f32], op: ToneMapOp, semi: bool) -> Option<Vec<u8>> {
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
    let planes =
        pipeline.encode_chain_to_yuv420(true, W, H, tone_map_id(op), [KR, 1.0 - KR - KB, KB], semi);
    if planes.is_none() {
        panic!("encode_chain_to_yuv420 refused a {W}x{H} frame with tone map {op:?}");
    }
    planes
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
            .encode_chain_to_yuv420(true, W, H, tone_map_id(ToneMapOp::Aces), [KR, 1.0 - KR - KB, KB], false)
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
        pipeline.encode_chain_to_yuv420(true, 60, 32, 0, [KR, 1.0 - KR - KB, KB], false).is_none(),
        "width 60 is not a multiple of 8"
    );
    assert!(
        pipeline.encode_chain_to_yuv420(true, 64, 31, 0, [KR, 1.0 - KR - KB, KB], false).is_none(),
        "height 31 is odd"
    );
}
