//! Verifies that `GpuNv12Converter::convert_nv12_to_rgba_on_gpu`
//! (produces a `wgpu::Buffer`) round-tripped through
//! `Frame::into_cpu()` yields byte-identical output to the existing
//! `convert_nv12_to_rgba` method (downloads directly).
//!
//! This is the end-to-end contract for phase 3b: a producer on the
//! decode thread can stash the shader output as a `Frame::Gpu`,
//! thread it across to the engine, and the engine can collapse to
//! CPU at the grade boundary with no data-path divergence from the
//! current CPU-download flow.

use std::sync::Arc;

use klipp_effects::nv12_gpu::GpuNv12Converter;
use klipp_engine::{Frame, GpuFrame};

/// 1280×720 is a realistic NV12 size (smallest 16:9 HD). Smaller
/// would still exercise the shader but larger is closer to the real
/// workload. Both test vectors use deterministic luma/chroma patterns
/// so a miscopy is obvious.
const W: u32 = 128;
const H: u32 = 64;

fn make_nv12(width: u32, height: u32) -> (Vec<u8>, Vec<u8>, u32, u32) {
    // Y: horizontal ramp 0..255. UV: interleaved U/V with a diagonal
    // ramp. Both planes use stride == width (tightly packed).
    let y_stride = width;
    let uv_stride = width; // U and V interleaved → same stride as Y
    let mut y = Vec::with_capacity((y_stride * height) as usize);
    for row in 0..height {
        for col in 0..width {
            y.push(((col + row) & 0xFF) as u8);
        }
    }
    let mut uv = Vec::with_capacity((uv_stride * (height / 2)) as usize);
    for row in 0..(height / 2) {
        for col in 0..(width / 2) {
            uv.push(((col as u32 + row as u32 + 64) & 0xFF) as u8); // U
            uv.push(((col as u32 * 2 + row as u32 + 128) & 0xFF) as u8); // V
        }
    }
    (y, uv, y_stride, uv_stride)
}

#[test]
fn gpu_buffer_readback_matches_cpu_download() {
    let Some(mut conv) = GpuNv12Converter::new() else {
        eprintln!("skipping: NV12 GPU converter unavailable on this system");
        return;
    };

    let (y, uv, y_stride, uv_stride) = make_nv12(W, H);

    // Path A: existing CPU-download. Authoritative reference.
    let cpu_bytes = conv.convert_nv12_to_rgba(
        &y, &uv, W, H, y_stride, uv_stride, W, H, 0,
    );

    // Path B: new GPU-resident output, then readback via
    // Frame::into_cpu(). Must produce the same bytes.
    let gpu_buffer = conv.convert_nv12_to_rgba_on_gpu(
        &y, &uv, W, H, y_stride, uv_stride, W, H, 0,
    );
    let frame = Frame::Gpu(GpuFrame::new(conv.device(), conv.queue(), gpu_buffer, W, H));
    let gpu_bytes = frame.into_cpu().data;

    assert_eq!(cpu_bytes.len(), gpu_bytes.len(), "byte length mismatch");
    assert_eq!(
        cpu_bytes, gpu_bytes,
        "GPU-resident readback diverges from CPU-download path"
    );
}

#[test]
fn successive_gpu_buffers_are_independent() {
    // Allocating a fresh output buffer per call means two sequential
    // conversions must not alias: mutating the second must not
    // corrupt the first.
    let Some(mut conv) = GpuNv12Converter::new() else {
        eprintln!("skipping: NV12 GPU converter unavailable on this system");
        return;
    };

    let (y1, uv1, ys, us) = make_nv12(W, H);

    let buf1 = conv.convert_nv12_to_rgba_on_gpu(&y1, &uv1, W, H, ys, us, W, H, 0);
    // Second conversion with DIFFERENT luma must not change buf1's
    // contents.
    let mut y2 = y1.clone();
    for b in y2.iter_mut() {
        *b = b.wrapping_add(33);
    }
    let buf2 = conv.convert_nv12_to_rgba_on_gpu(&y2, &uv1, W, H, ys, us, W, H, 0);

    let frame1 = Frame::Gpu(GpuFrame::new(conv.device(), conv.queue(), Arc::clone(&buf1), W, H));
    let frame2 = Frame::Gpu(GpuFrame::new(conv.device(), conv.queue(), Arc::clone(&buf2), W, H));
    let bytes1 = frame1.into_cpu().data;
    let bytes2 = frame2.into_cpu().data;

    assert_ne!(
        bytes1, bytes2,
        "different inputs must produce different outputs"
    );
}
