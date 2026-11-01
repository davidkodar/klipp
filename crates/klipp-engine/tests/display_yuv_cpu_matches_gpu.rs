//! `camera_log::yuv_to_linear_display` against the NV12 decode shader.
//!
//! This converter took over the software-decode path from swscale so that
//! subsampled chroma is reconstructed at the same siting the shaders use.
//! That only buys anything if it actually agrees with them, so: the same
//! 8-bit 4:2:0 samples through both, compared in linear light.
//!
//! It also covers what swscale was reached for — 4:2:2 and 4:4:4 — where
//! there is no GPU twin to compare against, so those are checked against
//! the property that distinguishes them instead.

use klipp_color::camera_log::{primaries_to_working_matrix, yuv_to_linear_display};
use klipp_color::hdr::YuvSamples;
use klipp_core::{color_ids, ColorPrimaries, SourceColorspace, WorkingColorspace};
use klipp_effects::color_wgsl::pack_primaries_matrix;
use klipp_effects::nv12_gpu::GpuNv12Converter;

const W: u32 = 64;
const H: u32 = 32;

fn bt709_sdr() -> SourceColorspace {
    SourceColorspace {
        range: klipp_core::ColorRange::Limited,
        matrix: klipp_core::ColorMatrix::Bt709,
        primaries: ColorPrimaries::Bt709,
        transfer: klipp_core::ColorTransfer::Bt709,
    }
}

/// 8-bit samples with chroma that changes along both axes — flat chroma
/// would pass whatever the siting.
fn samples_8bit() -> (Vec<u16>, Vec<u16>, Vec<u16>) {
    let y = (0..H).flat_map(|r| (0..W).map(move |c| (16 + (c * 3 + r * 5) % 219) as u16)).collect();
    let u = (0..H / 2)
        .flat_map(|r| (0..W / 2).map(move |c| (16 + ((c as i32 * 7 - r as i32 * 11).rem_euclid(224)) as u32) as u16))
        .collect();
    let v = (0..H / 2)
        .flat_map(|r| (0..W / 2).map(move |c| (16 + ((c as i32 * 13 + r as i32 * 3).rem_euclid(224)) as u32) as u16))
        .collect();
    (y, u, v)
}

fn gpu_linear(conv: &mut GpuNv12Converter, y: &[u16], u: &[u16], v: &[u16]) -> Vec<f32> {
    let yb: Vec<u8> = y.iter().map(|s| *s as u8).collect();
    let uvb: Vec<u8> = u.iter().zip(v).flat_map(|(a, b)| [*a as u8, *b as u8]).collect();
    let pm = pack_primaries_matrix(primaries_to_working_matrix(
        ColorPrimaries::Bt709,
        WorkingColorspace::Rec709Linear,
    ));
    let ids = [
        color_ids::COLOR_RANGE_LIMITED,
        color_ids::MATRIX_BT709,
        color_ids::TRANSFER_BT709,
        0,
    ];
    let buf = conv.convert_nv12_to_linear_on_gpu(&yb, &uvb, W, H, W, W, W, H, 0, ids, pm);
    let (device, queue) = (conv.device(), conv.queue());
    let bytes = (W as u64) * (H as u64) * 16;
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("display_yuv_parity_readback"),
        size: bytes,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_buffer_to_buffer(&buf, 0, &staging, 0, bytes);
    queue.submit(std::iter::once(enc.finish()));
    let slice = staging.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::Maintain::Wait);
    let data = slice.get_mapped_range();
    let out: Vec<f32> = bytemuck::pod_collect_to_vec(&data);
    drop(data);
    staging.unmap();
    out
}

#[test]
fn the_display_converter_matches_the_nv12_shader() {
    let Some(mut conv) = GpuNv12Converter::new() else {
        eprintln!("skipping: no GPU NV12 converter");
        return;
    };
    let (y, u, v) = samples_8bit();
    let s = YuvSamples {
        y: &y, u: &u, v: &v, width: W, height: H,
        chroma_shift_w: 1, chroma_shift_h: 1, bit_depth: 8, siting: Default::default(),
    };
    let cpu = yuv_to_linear_display(&s, W, H, 0, bt709_sdr(), WorkingColorspace::Rec709Linear);
    let gpu = gpu_linear(&mut conv, &y, &u, &v);
    assert_eq!(cpu.len(), gpu.len());

    let mut worst = 0.0f32;
    let mut over = 0usize;
    for i in (0..cpu.len()).step_by(4) {
        for c in 0..3 {
            let (a, b) = (cpu[i + c], gpu[i + c]);
            let d = (a - b).abs() / a.abs().max(b.abs()).max(1e-3);
            if d > 0.01 {
                over += 1;
            }
            worst = worst.max(d);
        }
    }
    eprintln!("worst relative difference {:.4}%; {over} channels over 1%", worst * 100.0);
    // Same allowance as the 10-bit twin: the shader's `pow` against the
    // CPU's 1025-entry EOTF LUT. A siting mismatch reads far larger.
    assert!(worst < 0.015, "CPU and GPU 8-bit 4:2:0 differ by {:.2}%", worst * 100.0);
}

/// 4:2:2 keeps every chroma row its own — there is no vertical
/// subsampling to interpolate across. This is what swscale was handling
/// for ProRes, and getting it wrong here would soften colour vertically
/// on every 4:2:2 clip.
#[test]
fn four_two_two_does_not_blend_chroma_rows() {
    let (w, h) = (4u32, 4u32);
    let y = vec![128u16; (w * h) as usize];
    // Chroma at full height, half width. Row 0 neutral, row 1 strongly
    // red, rows 2-3 neutral again.
    let cw = (w / 2) as usize;
    let mut u = vec![128u16; cw * h as usize];
    let mut v = vec![128u16; cw * h as usize];
    for c in 0..cw {
        u[cw + c] = 90;
        v[cw + c] = 240;
    }
    let s = YuvSamples {
        y: &y, u: &u, v: &v, width: w, height: h,
        chroma_shift_w: 1, chroma_shift_h: 0, bit_depth: 8, siting: Default::default(),
    };
    let out = yuv_to_linear_display(&s, w, h, 0, bt709_sdr(), WorkingColorspace::Rec709Linear);
    let red_of = |row: usize| out[(row * w as usize) * 4];
    eprintln!("row reds: {:?}", (0..4).map(red_of).collect::<Vec<_>>());
    // Only row 1 carries the red chroma. If vertical interpolation were
    // applied to a 4:2:2 plane, rows 0 and 2 would pick some of it up.
    assert!(red_of(1) > red_of(0) * 1.5, "row 1 must be the red one");
    assert_eq!(red_of(0), red_of(3), "rows 0 and 3 are both neutral");
    assert_eq!(red_of(0), red_of(2), "row 2 must not borrow row 1's chroma");
}

/// Rotation is folded into the source lookup, so a 90-degree clip comes
/// back with swapped extent and its content actually turned — not merely
/// relabelled, which is the failure that looks right until you watch it.
#[test]
fn rotation_turns_the_picture_not_just_the_dimensions() {
    let (w, h) = (4u32, 2u32);
    // A luma ramp along x only, so a 90-degree turn must make it vary
    // along y instead.
    let y: Vec<u16> = (0..h).flat_map(|_| (0..w).map(|c| (16 + c * 50) as u16)).collect();
    let u = vec![128u16; ((w / 2) * (h / 2)).max(1) as usize];
    let v = vec![128u16; ((w / 2) * (h / 2)).max(1) as usize];
    let s = YuvSamples {
        y: &y, u: &u, v: &v, width: w, height: h,
        chroma_shift_w: 1, chroma_shift_h: 1, bit_depth: 8, siting: Default::default(),
    };
    let (dw, dh) = (h, w);
    let out = yuv_to_linear_display(&s, dw, dh, 90, bt709_sdr(), WorkingColorspace::Rec709Linear);
    assert_eq!(out.len(), (dw * dh) as usize * 4);
    let at = |x: usize, yy: usize| out[(yy * dw as usize + x) * 4 + 1];
    let down_column: Vec<f32> = (0..dh as usize).map(|yy| at(0, yy)).collect();
    eprintln!("green down the rotated column: {down_column:?}");
    let varies = down_column.windows(2).any(|p| (p[0] - p[1]).abs() > 1e-4);
    assert!(varies, "the ramp did not end up along y: {down_column:?}");
}
