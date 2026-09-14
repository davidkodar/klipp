//! Verifies `GpuP010Converter::convert_p010_to_rgba_on_gpu` round-
//! tripped through `Frame::into_cpu()` yields byte-identical output
//! to `convert_p010_to_rgba` (downloads directly). Same contract as
//! the NV12 companion test, exercised for both SDR (BT.709) and HDR
//! (BT.2020+PQ tone-map) shader modes.

use std::sync::Arc;

use klipp_effects::color_wgsl::IDENTITY_PRIMARIES_MATRIX_PACKED;
use klipp_effects::p010_gpu::GpuP010Converter;
use klipp_engine::{Frame, GpuFrame};

const W: u32 = 128;
const H: u32 = 64;

/// `[range, matrix_id, transfer_id, primaries_id]` for SDR (BT.709
/// limited) and HDR (BT.2020 PQ). The PQ variant is what triggers the
/// shader's HDR tone-map branch — the differentiator this test
/// preserves from its pre-`color_ids` `is_hdr: bool` form. Both call
/// sites use the same identity primaries matrix; this test verifies
/// CPU-readback ↔ GPU-resident path consistency, not colorimetric
/// correctness.
const COLOR_IDS_BT709_SDR: [u32; 4] = [0, 0, 0, 0];
const COLOR_IDS_BT2020_PQ: [u32; 4] = [0, 2, 3, 1];

/// Build a synthetic P010 frame. Each 10-bit code value goes into the
/// upper 10 bits of a 16-bit LE word (P010 layout). The pattern is a
/// ramp so a miscopy is obvious.
fn make_p010(width: u32, height: u32) -> (Vec<u8>, Vec<u8>, u32, u32) {
    let y_stride = width * 2; // 2 bytes per Y sample
    let uv_stride = width * 2; // 2 bytes per chroma sample, interleaved U/V
    let mut y = Vec::with_capacity((y_stride * height) as usize);
    for row in 0..height {
        for col in 0..width {
            let v10: u32 = 64 + (col + row) % 896; // stay in [64, 960] limited range
            let packed: u16 = ((v10 & 0x3FF) << 6) as u16;
            y.extend_from_slice(&packed.to_le_bytes());
        }
    }
    let mut uv = Vec::with_capacity((uv_stride * (height / 2)) as usize);
    for row in 0..(height / 2) {
        for col in 0..(width / 2) {
            let u10: u32 = 128 + (col + row) % 768;
            let v10: u32 = 128 + (col * 2 + row) % 768;
            let pu: u16 = ((u10 & 0x3FF) << 6) as u16;
            let pv: u16 = ((v10 & 0x3FF) << 6) as u16;
            uv.extend_from_slice(&pu.to_le_bytes());
            uv.extend_from_slice(&pv.to_le_bytes());
        }
    }
    (y, uv, y_stride, uv_stride)
}

fn round_trip_matches(is_hdr: bool) {
    let Some(mut conv) = GpuP010Converter::new() else {
        eprintln!("skipping: P010 GPU converter unavailable on this system");
        return;
    };

    let (y, uv, y_stride, uv_stride) = make_p010(W, H);
    let color_ids = if is_hdr { COLOR_IDS_BT2020_PQ } else { COLOR_IDS_BT709_SDR };

    let cpu_bytes = conv.convert_p010_to_rgba(
        &y, &uv, W, H, y_stride, uv_stride, W, H, 0,
        color_ids, IDENTITY_PRIMARIES_MATRIX_PACKED,
    );
    let gpu_buffer = conv.convert_p010_to_rgba_on_gpu(
        &y, &uv, W, H, y_stride, uv_stride, W, H, 0,
        color_ids, IDENTITY_PRIMARIES_MATRIX_PACKED,
    );
    let frame = Frame::Gpu(GpuFrame::new(conv.device(), conv.queue(), gpu_buffer, W, H));
    let gpu_bytes = frame.into_cpu().data;

    assert_eq!(cpu_bytes.len(), gpu_bytes.len(), "length mismatch (is_hdr={is_hdr})");
    assert_eq!(
        cpu_bytes, gpu_bytes,
        "P010 GPU-resident readback diverges from CPU-download path (is_hdr={is_hdr})"
    );
}

#[test]
fn p010_sdr_gpu_buffer_matches_cpu_download() {
    round_trip_matches(false);
}

#[test]
fn p010_hdr_gpu_buffer_matches_cpu_download() {
    round_trip_matches(true);
}

#[test]
fn p010_successive_gpu_buffers_are_independent() {
    let Some(mut conv) = GpuP010Converter::new() else {
        eprintln!("skipping: P010 GPU converter unavailable on this system");
        return;
    };
    let (y1, uv1, ys, us) = make_p010(W, H);
    let buf1 = conv.convert_p010_to_rgba_on_gpu(
        &y1, &uv1, W, H, ys, us, W, H, 0,
        COLOR_IDS_BT709_SDR, IDENTITY_PRIMARIES_MATRIX_PACKED,
    );

    // Mutate Y samples (keep them in range). Two distinct inputs must
    // produce two distinct outputs; a reused internal buffer would
    // leak frame 2 back over frame 1.
    let mut y2 = y1.clone();
    for chunk in y2.chunks_exact_mut(2) {
        let v = u16::from_le_bytes([chunk[0], chunk[1]]);
        let mutated = v.wrapping_add(0x0200); // shift ~8 codes up
        chunk.copy_from_slice(&mutated.to_le_bytes());
    }
    let buf2 = conv.convert_p010_to_rgba_on_gpu(
        &y2, &uv1, W, H, ys, us, W, H, 0,
        COLOR_IDS_BT709_SDR, IDENTITY_PRIMARIES_MATRIX_PACKED,
    );

    let frame1 = Frame::Gpu(GpuFrame::new(conv.device(), conv.queue(), Arc::clone(&buf1), W, H));
    let frame2 = Frame::Gpu(GpuFrame::new(conv.device(), conv.queue(), Arc::clone(&buf2), W, H));
    let b1 = frame1.into_cpu().data;
    let b2 = frame2.into_cpu().data;

    assert_ne!(b1, b2, "different P010 inputs must produce different outputs");
}
