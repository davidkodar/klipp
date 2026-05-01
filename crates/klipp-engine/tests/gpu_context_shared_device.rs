//! `GpuContext` sanity: NV12 and P010 converters built on the same
//! shared `GpuContext` produce byte-identical output to converters
//! built with their own private device. Also verifies that a buffer
//! produced by one converter and wrapped in `Frame::Gpu` is readable
//! via the shared device (the whole point of the unification).
//!
//! This is the foundation test for phase 4 — if converters can share
//! a device, the grade chain can consume their output buffers without
//! a CPU round-trip.

use std::sync::Arc;

use klipp_effects::{
    color_wgsl::IDENTITY_PRIMARIES_MATRIX_PACKED,
    gpu_context::GpuContext,
    nv12_gpu::GpuNv12Converter,
    p010_gpu::GpuP010Converter,
};
use klipp_engine::{Frame, GpuFrame};

const W: u32 = 128;
const H: u32 = 64;

/// `[range, matrix_id, transfer_id, primaries_id]` for a BT.709
/// limited-range SDR source — the colourspace the synthesised test
/// fixtures represent. Identity primaries matrix follows.
const COLOR_IDS_BT709_SDR: [u32; 4] = [0, 0, 0, 0];

fn make_nv12(width: u32, height: u32) -> (Vec<u8>, Vec<u8>) {
    let mut y = Vec::with_capacity((width * height) as usize);
    for row in 0..height {
        for col in 0..width {
            y.push(((col + row * 3) & 0xFF) as u8);
        }
    }
    let mut uv = Vec::with_capacity((width * height / 2) as usize);
    for row in 0..(height / 2) {
        for col in 0..(width / 2) {
            uv.push(((col as u32 * 2 + row as u32 + 80) & 0xFF) as u8); // U
            uv.push(((col as u32 + row as u32 * 5 + 160) & 0xFF) as u8); // V
        }
    }
    (y, uv)
}

fn make_p010(width: u32, height: u32) -> (Vec<u8>, Vec<u8>) {
    let mut y = Vec::with_capacity((width * height * 2) as usize);
    for row in 0..height {
        for col in 0..width {
            let v: u32 = 64 + (col + row * 5) % 896;
            let packed: u16 = ((v & 0x3FF) << 6) as u16;
            y.extend_from_slice(&packed.to_le_bytes());
        }
    }
    let mut uv = Vec::with_capacity((width * height) as usize);
    for row in 0..(height / 2) {
        for col in 0..(width / 2) {
            let u: u32 = 128 + (col as u32 + row as u32 * 3) % 768;
            let v: u32 = 128 + (col as u32 * 2 + row as u32) % 768;
            let pu: u16 = ((u & 0x3FF) << 6) as u16;
            let pv: u16 = ((v & 0x3FF) << 6) as u16;
            uv.extend_from_slice(&pu.to_le_bytes());
            uv.extend_from_slice(&pv.to_le_bytes());
        }
    }
    (y, uv)
}

#[test]
fn nv12_with_context_matches_private_device() {
    let Some(ctx) = GpuContext::new() else {
        eprintln!("skipping: no usable GpuContext");
        return;
    };

    let Some(mut private) = GpuNv12Converter::new() else { return; };
    let Some(mut shared) = GpuNv12Converter::with_context(&ctx) else { return; };

    let (y, uv) = make_nv12(W, H);
    let priv_bytes = private.convert_nv12_to_rgba(
        &y, &uv, W, H, W, W, W, H, 0,
        COLOR_IDS_BT709_SDR, IDENTITY_PRIMARIES_MATRIX_PACKED,
    );
    let shared_bytes = shared.convert_nv12_to_rgba(
        &y, &uv, W, H, W, W, W, H, 0,
        COLOR_IDS_BT709_SDR, IDENTITY_PRIMARIES_MATRIX_PACKED,
    );
    assert_eq!(priv_bytes, shared_bytes,
        "shared-context NV12 converter diverges from private-device converter");
}

#[test]
fn p010_with_context_matches_private_device() {
    let Some(ctx) = GpuContext::new() else {
        eprintln!("skipping: no usable GpuContext");
        return;
    };

    let Some(mut private) = GpuP010Converter::new() else { return; };
    let Some(mut shared) = GpuP010Converter::with_context(&ctx) else { return; };

    let (y, uv) = make_p010(W, H);
    let y_stride = W * 2;
    let uv_stride = W * 2;
    let priv_bytes = private.convert_p010_to_rgba(
        &y, &uv, W, H, y_stride, uv_stride, W, H, 0,
        COLOR_IDS_BT709_SDR, IDENTITY_PRIMARIES_MATRIX_PACKED,
    );
    let shared_bytes = shared.convert_p010_to_rgba(
        &y, &uv, W, H, y_stride, uv_stride, W, H, 0,
        COLOR_IDS_BT709_SDR, IDENTITY_PRIMARIES_MATRIX_PACKED,
    );
    assert_eq!(priv_bytes, shared_bytes,
        "shared-context P010 converter diverges from private-device converter");
}

/// The foundation-stone test: a buffer produced by the NV12 converter
/// on a shared context is readable via that same context's device
/// when wrapped in `Frame::Gpu`. Phase 4's grade chain will consume
/// the buffer directly (no readback); this test proves the device
/// bindings are correct by verifying a readback.
#[test]
fn shared_context_buffer_is_readable_via_frame() {
    let Some(ctx) = GpuContext::new() else {
        eprintln!("skipping: no usable GpuContext");
        return;
    };
    let Some(mut conv) = GpuNv12Converter::with_context(&ctx) else {
        eprintln!("skipping: NV12 converter unavailable");
        return;
    };

    let (y, uv) = make_nv12(W, H);
    let expected = conv.convert_nv12_to_rgba(
        &y, &uv, W, H, W, W, W, H, 0,
        COLOR_IDS_BT709_SDR, IDENTITY_PRIMARIES_MATRIX_PACKED,
    );
    let buf = conv.convert_nv12_to_rgba_on_gpu(
        &y, &uv, W, H, W, W, W, H, 0,
        COLOR_IDS_BT709_SDR, IDENTITY_PRIMARIES_MATRIX_PACKED,
    );

    // Build the Frame using the CONTEXT's device + queue, not the
    // converter's accessors. Proves the buffer lives on `ctx.device`
    // and any future consumer (grade pipeline) built on `ctx` can
    // bind it.
    let frame = Frame::Gpu(GpuFrame::new(ctx.device(), ctx.queue(), Arc::clone(&buf), W, H));
    let readback = frame.into_cpu().data;
    assert_eq!(expected, readback,
        "GpuFrame readback via shared context device must match CPU-download path");
}
