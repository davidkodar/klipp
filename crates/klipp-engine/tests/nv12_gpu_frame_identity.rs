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
//!
//! **Scope: plumbing, not colour.** Both sides run the same shader, so
//! the byte-exact `assert_eq!` below is only possible *because* the
//! colour maths is shared — and a wrong YUV→RGB matrix, wrong range
//! handling or wrong transfer would be reproduced identically on both
//! sides and pass. The name "frame identity" invites reading this as a
//! colour test; it is not one. What it does check is real and worth
//! having: that the GPU-resident hand-off does not corrupt, truncate or
//! alias the frame.
//!
//! The colour maths is covered against *independent* CPU
//! implementations elsewhere, and that is where to look if a picture is
//! wrong rather than merely mis-delivered:
//!
//! * display-referred — `display_yuv_cpu_matches_gpu`, the NV12 shader
//!   against `camera_log::yuv_to_linear_display`;
//! * scene-referred — `sw_decode_parity::cpu_codes_match_the_nv12_shader`,
//!   the same shader against `hdr::yuv_to_hdr_codes`.
//!
//! (The P010 sibling carried this disclaimer and this one did not, which
//! is the only reason it is spelled out at this length.)

use std::sync::Arc;

use klipp_effects::color_wgsl::IDENTITY_PRIMARIES_MATRIX_PACKED;
use klipp_effects::nv12_gpu::GpuNv12Converter;
use klipp_engine::{Frame, GpuFrame};

/// `color_ids` for a BT.709 limited-range SDR source — the default
/// the test fixture's synthesised YUV frames represent. `[0, 0, 0, 0]`
/// in numeric form: limited range, BT.709 matrix, BT.709 transfer,
/// BT.709 primaries.
const COLOR_IDS_BT709_SDR: [u32; 4] = [0, 0, 0, 0];

/// A scene-referred source: BT.709 limited range, S-Log3. The shader
/// takes a different branch for these — it writes packed code values to a
/// second buffer and the RGBA output becomes a tone-mapped preview — so
/// the GPU-resident hand-off has a second shape to get right.
///
/// The P010 sibling exercises both its SDR and PQ branches; this one only
/// ever ran SDR, leaving the branch that writes two buffers untested
/// through the plumbing under test here.
const COLOR_IDS_SLOG3: [u32; 4] = [
    klipp_core::color_ids::COLOR_RANGE_LIMITED,
    klipp_core::color_ids::MATRIX_BT709,
    klipp_core::color_ids::TRANSFER_SLOG3,
    0,
];

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
            uv.push(((col + row + 64) & 0xFF) as u8); // U
            uv.push(((col * 2 + row + 128) & 0xFF) as u8); // V
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

    let mut per_branch: Vec<(&str, Vec<u8>)> = Vec::new();
    for (name, ids) in [
        ("BT.709 SDR", COLOR_IDS_BT709_SDR),
        ("S-Log3 scene-referred", COLOR_IDS_SLOG3),
    ] {
        // Path A: existing CPU-download. Authoritative reference.
        let cpu_bytes = conv.convert_nv12_to_rgba(
            &y, &uv, W, H, y_stride, uv_stride, W, H, 0,
            ids, IDENTITY_PRIMARIES_MATRIX_PACKED,
        );

        // Path B: new GPU-resident output, then readback via
        // Frame::into_cpu(). Must produce the same bytes.
        let gpu_buffer = conv.convert_nv12_to_rgba_on_gpu(
            &y, &uv, W, H, y_stride, uv_stride, W, H, 0,
            ids, IDENTITY_PRIMARIES_MATRIX_PACKED,
        );
        let frame = Frame::Gpu(GpuFrame::new(conv.device(), conv.queue(), gpu_buffer, W, H));
        let gpu_bytes = frame.into_cpu().data;

        assert_eq!(cpu_bytes.len(), gpu_bytes.len(), "{name}: byte length mismatch");
        assert_eq!(
            cpu_bytes, gpu_bytes,
            "{name}: GPU-resident readback diverges from CPU-download path",
        );
        // The fixture has to produce a picture, or two empty buffers would
        // compare equal and say nothing.
        assert!(
            cpu_bytes.chunks_exact(4).any(|p| p[..3] != cpu_bytes[..3]),
            "{name}: the converted frame is flat, so identity is unfalsifiable",
        );
        per_branch.push((name, cpu_bytes));
    }

    // The two cases have to actually take different paths through the
    // shader, or the second one is a copy of the first and tests nothing
    // the first did not.
    let (a, b) = (&per_branch[0], &per_branch[1]);
    let differing = a.1.iter().zip(&b.1).filter(|(x, y)| x != y).count();
    eprintln!("{} vs {}: {differing} of {} bytes differ", a.0, b.0, a.1.len());
    assert!(
        differing * 4 > a.1.len(),
        "{} and {} produced nearly the same bytes, so the scene-referred branch \
         is not being exercised",
        a.0, b.0,
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

    let buf1 = conv.convert_nv12_to_rgba_on_gpu(
        &y1, &uv1, W, H, ys, us, W, H, 0,
        COLOR_IDS_BT709_SDR, IDENTITY_PRIMARIES_MATRIX_PACKED,
    );
    // Second conversion with DIFFERENT luma must not change buf1's
    // contents.
    let mut y2 = y1.clone();
    for b in y2.iter_mut() {
        *b = b.wrapping_add(33);
    }
    let buf2 = conv.convert_nv12_to_rgba_on_gpu(
        &y2, &uv1, W, H, ys, us, W, H, 0,
        COLOR_IDS_BT709_SDR, IDENTITY_PRIMARIES_MATRIX_PACKED,
    );

    let frame1 = Frame::Gpu(GpuFrame::new(conv.device(), conv.queue(), Arc::clone(&buf1), W, H));
    let frame2 = Frame::Gpu(GpuFrame::new(conv.device(), conv.queue(), Arc::clone(&buf2), W, H));
    let bytes1 = frame1.into_cpu().data;
    let bytes2 = frame2.into_cpu().data;

    assert_ne!(
        bytes1, bytes2,
        "different inputs must produce different outputs"
    );
}
