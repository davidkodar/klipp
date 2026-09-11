//! The direct-present fast path publishes the decoder's sRGB-u8 bytes
//! to the viewer without grading or compositing them.
//!
//! That is only sound if the pipeline it bypasses would have handed the
//! viewer those same bytes. The bypassed pipeline is
//!
//!   readback u8 -> expand to linear f32 -> (identity grade, identity
//!   composite) -> display_transform_ws -> u8
//!
//! so what has to hold is that `display_transform_ws` inverts the
//! expansion exactly. It does, but only for the two conditions
//! `direct_present_blocker` checks: `ToneMapOp::None` and a Rec.709
//! working space. These tests pin both — the positive case for every one
//! of the 256 input levels, and the negative cases so neither gate can
//! be dropped as redundant. A wide-gamut working space is a negative
//! case for the same reason in both directions: ACES routes through its
//! ODT, BT.2020 through a matrix into BT.709, and neither lands back on
//! the decoder's own bytes.
//!
//! Deliberately exercised through `realise_srgb_u8_from_gpu` and
//! `realise_linear_from_gpu` rather than a local re-implementation: those
//! are the functions the engine actually calls, LUTs and all.

use klipp_color::display::{display_transform_ws, ToneMapOp};
use klipp_core::WorkingColorspace;
use klipp_effects::gpu_context::GpuContext;
use klipp_engine::{realise_linear_from_gpu, realise_srgb_u8_from_gpu, GpuFrame};

// 256 wide so the byte pitch (1024) is a multiple of
// COPY_BYTES_PER_ROW_ALIGNMENT, and so one row can hold every level.
const W: u32 = 256;
const H: u32 = 8;

/// Every u8 level in every colour channel, plus a fully-opaque alpha —
/// which is what the decoder's conversion shaders emit.
fn all_levels() -> Vec<u8> {
    let mut v = Vec::with_capacity((W * H * 4) as usize);
    for row in 0..H {
        for col in 0..W {
            let level = col as u8;
            // Rotate which channel carries the ramp so channel handling
            // cannot differ unnoticed, and cover a grey ramp too.
            let (r, g, b) = match row % 4 {
                0 => (level, 0, 0),
                1 => (0, level, 0),
                2 => (0, 0, level),
                _ => (level, level, level),
            };
            v.extend_from_slice(&[r, g, b, 255]);
        }
    }
    v
}

fn gpu_frame_from(bytes: &[u8]) -> Option<std::sync::Arc<GpuFrame>> {
    let ctx = GpuContext::shared()?;
    let buf = std::sync::Arc::new(ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("direct_present_identity_input"),
        size: bytes.len() as u64,
        usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    }));
    ctx.queue.write_buffer(&buf, 0, bytes);
    ctx.device.poll(wgpu::Maintain::Wait);
    Some(std::sync::Arc::new(GpuFrame::new(
        ctx.device(),
        ctx.queue(),
        buf,
        W,
        H,
    )))
}

/// What the full pipeline would have published for an identity graph.
fn through_the_full_pipeline(
    gpu: &GpuFrame,
    tone_map: ToneMapOp,
    ws: WorkingColorspace,
) -> Vec<u8> {
    let linear = realise_linear_from_gpu(gpu);
    display_transform_ws(&linear, tone_map, ws)
}

#[test]
fn direct_present_is_byte_identical() {
    let bytes = all_levels();
    let Some(gpu) = gpu_frame_from(&bytes) else {
        eprintln!("skipping: no usable GpuContext");
        return;
    };

    // The fast path.
    let direct = realise_srgb_u8_from_gpu(&gpu);
    assert_eq!(direct.len(), bytes.len(), "readback size mismatch");
    assert_eq!(
        direct, bytes,
        "the readback itself altered the decoder's bytes",
    );

    // The one working space the blocker admits must agree with it, for
    // every level, exactly.
    let ws = WorkingColorspace::Rec709Linear;
    let full = through_the_full_pipeline(&gpu, ToneMapOp::None, ws);
    assert_eq!(full.len(), direct.len(), "{ws:?}: output size mismatch");
    let mismatches: Vec<(usize, u8, u8)> = full
        .iter()
        .zip(direct.iter())
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(i, (a, b))| (i, *a, *b))
        .take(8)
        .collect();
    assert!(
        mismatches.is_empty(),
        "{ws:?}: direct present differs from the full pipeline at \
         {} of {} bytes; first few (index, full, direct): {mismatches:?}",
        full.iter().zip(direct.iter()).filter(|(a, b)| a != b).count(),
        full.len(),
    );
    eprintln!("{ws:?}: direct present is byte-identical over all 256 levels");
}

/// The `ToneMapOp::None` gate is load-bearing, not defensive clutter:
/// with an operator active the full pipeline moves the pixels, so
/// publishing the decoder's bytes would silently drop the tone map.
#[test]
fn a_tone_map_operator_makes_the_fast_path_wrong() {
    let bytes = all_levels();
    let Some(gpu) = gpu_frame_from(&bytes) else {
        eprintln!("skipping: no usable GpuContext");
        return;
    };
    let direct = realise_srgb_u8_from_gpu(&gpu);

    for op in [ToneMapOp::Hable, ToneMapOp::Reinhard, ToneMapOp::Aces] {
        let full = through_the_full_pipeline(&gpu, op, WorkingColorspace::Rec709Linear);
        assert_ne!(
            full, direct,
            "{op:?} left the pixels unchanged — if this ever becomes true, \
             the tone-map check in direct_present_blocker is no longer \
             protecting anything and this test is the wrong shape",
        );
    }

    // Same for the ACES working space, which routes through the ODT.
    let aces = through_the_full_pipeline(&gpu, ToneMapOp::None, WorkingColorspace::AcesCG);
    assert_ne!(
        aces, direct,
        "the ACES ODT left the pixels unchanged — the working-space check \
         in direct_present_blocker is no longer protecting anything",
    );

    // And for BT.2020, which needs a matrix into BT.709 before the sRGB
    // encode. While it was a stub aliased to Rec.709 this was identical
    // and the fast path admitted it; now that it is a real working space
    // the picture would come out over-saturated on the fast path and
    // correct on the slow one — the worst way for this to be wrong.
    let bt2020 = through_the_full_pipeline(&gpu, ToneMapOp::None, WorkingColorspace::Bt2020Linear);
    assert_ne!(
        bt2020, direct,
        "a BT.2020 working space left the pixels unchanged — it is either \
         still stubbed as Rec.709, or the gamut conversion is missing from \
         display_transform_ws",
    );
}
