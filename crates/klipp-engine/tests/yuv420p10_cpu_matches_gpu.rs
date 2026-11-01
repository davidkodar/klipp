//! `camera_log::yuv420p10le_to_linear` against the P010 decode shader.
//!
//! These two convert the same thing — 10-bit 4:2:0, display-referred or
//! camera-log, to linear working space — from two layouts of identical
//! samples: planar yuv420p10le (software decode) and semi-planar P010
//! (hardware decode). Which one runs is decided by whether NVDEC took the
//! clip, so they have to agree or a clip changes appearance for reasons
//! the colourist cannot see.
//!
//! Nothing held them together before. `sw_decode_parity` covers the
//! *scene-referred* CPU twin (`yuv_to_hdr_codes`) and
//! `gpu_yuv420_matches_cpu` covers the export *encoder*; this converter
//! sat between them with no parity test, which is how it kept
//! nearest-neighbour chroma while the other three paths moved to sited
//! reconstruction.

use klipp_color::camera_log::{primaries_to_working_matrix, yuv420p10le_to_linear};
use klipp_color::hdr::YuvSamples;
use klipp_core::{color_ids, ColorPrimaries, SourceColorspace, WorkingColorspace};
use klipp_effects::color_wgsl::pack_primaries_matrix;
use klipp_effects::p010_gpu::GpuP010Converter;

const W: u32 = 64;
const H: u32 = 32;

/// 10-bit samples with luma detail and a chroma sweep that changes in
/// both axes — chroma reconstruction is invisible on flat chroma, so a
/// fixture without gradients would pass either way.
fn samples() -> (Vec<u16>, Vec<u16>, Vec<u16>) {
    let y = (0..H)
        .flat_map(|r| (0..W).map(move |c| (64 + (c * 37 + r * 11) % 876) as u16))
        .collect();
    let u = (0..H / 2)
        .flat_map(|r| (0..W / 2).map(move |c| (64 + ((c as i32 * 53 - r as i32 * 29).rem_euclid(896)) as u32) as u16))
        .collect();
    let v = (0..H / 2)
        .flat_map(|r| (0..W / 2).map(move |c| (64 + ((c as i32 * 31 + r as i32 * 67).rem_euclid(896)) as u32) as u16))
        .collect();
    (y, u, v)
}

/// Planar yuv420p10le: three planes of u16 LE, value in the low 10 bits.
fn planar(y: &[u16], u: &[u16], v: &[u16]) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let pack = |p: &[u16]| p.iter().flat_map(|s| s.to_le_bytes()).collect();
    (pack(y), pack(u), pack(v))
}

/// Semi-planar P010: u16 LE with the value in the *upper* 10 bits, and
/// the chroma planes interleaved.
fn semi_planar(y: &[u16], u: &[u16], v: &[u16]) -> (Vec<u8>, Vec<u8>) {
    let yb = y.iter().flat_map(|s| (s << 6).to_le_bytes()).collect();
    let uvb = u
        .iter()
        .zip(v)
        .flat_map(|(a, b)| [(a << 6).to_le_bytes(), (b << 6).to_le_bytes()])
        .flatten()
        .collect();
    (yb, uvb)
}

fn gpu_linear(conv: &mut GpuP010Converter, yb: &[u8], uvb: &[u8], ids: [u32; 4], pm: [[f32; 4]; 3]) -> Vec<f32> {
    let buf = conv.convert_p010_to_linear_on_gpu(yb, uvb, W, H, W * 2, W * 2, W, H, 0, ids, pm);
    let (device, queue) = (conv.device(), conv.queue());
    let bytes = (W as u64) * (H as u64) * 16;
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("yuv420p10_parity_readback"),
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
fn the_planar_10bit_converter_matches_the_p010_shader() {
    let Some(mut conv) = GpuP010Converter::new() else {
        eprintln!("skipping: no GPU P010 converter");
        return;
    };
    let (y, u, v) = samples();
    let (yp, up, vp) = planar(&y, &u, &v);
    let (yb, uvb) = semi_planar(&y, &u, &v);

    let cs = SourceColorspace {
        range: klipp_core::ColorRange::Limited,
        matrix: klipp_core::ColorMatrix::Bt709,
        primaries: ColorPrimaries::Bt709,
        transfer: klipp_core::ColorTransfer::Bt709,
    };
    let ws = WorkingColorspace::Rec709Linear;
    let ids = [
        color_ids::COLOR_RANGE_LIMITED,
        color_ids::MATRIX_BT709,
        color_ids::TRANSFER_BT709,
        0,
    ];
    let pm = pack_primaries_matrix(primaries_to_working_matrix(ColorPrimaries::Bt709, ws));

    let cpu = yuv420p10le_to_linear(
        &yp, &up, &vp,
        (W * 2) as usize, (W / 2 * 2) as usize, (W / 2 * 2) as usize,
        W, H, W, H, cs, ws, Default::default(),
    );
    let gpu = gpu_linear(&mut conv, &yb, &uvb, ids, pm);
    assert_eq!(cpu.len(), gpu.len(), "sizes differ");

    // Compared in linear light, where a chroma phase error shows as a
    // large relative difference on the saturated samples rather than a
    // rounding wobble.
    let mut worst = 0.0f32;
    let mut worst_at = 0;
    let mut differing = 0usize;
    for i in (0..cpu.len()).step_by(4) {
        for c in 0..3 {
            let (a, b) = (cpu[i + c], gpu[i + c]);
            let d = (a - b).abs() / a.abs().max(b.abs()).max(1e-3);
            if d > 0.01 {
                differing += 1;
            }
            if d > worst {
                worst = d;
                worst_at = i / 4;
            }
        }
    }
    eprintln!(
        "worst relative difference {:.4}% at pixel {worst_at}; {differing} channels over 1%",
        worst * 100.0
    );
    // 1.5% absorbs the shader's `pow` against the CPU's 1025-entry EOTF
    // LUT, which is the known and intended difference between these two.
    // A chroma siting mismatch is not subtle at this fixture's gradients:
    // forcing the CPU side back to nearest-neighbour reads 100.0% worst
    // with 4104 channels over 1%, against 0.011% worst and none over 1%
    // when both reconstruct.
    assert!(
        worst < 0.015,
        "CPU and GPU 10-bit 4:2:0 differ by {:.2}% — check chroma siting first",
        worst * 100.0,
    );
}

/// This fixture can actually see a chroma-siting regression.
///
/// The parity test above compares two implementations of the same
/// conversion. If the fixture's chroma were flat, both would agree
/// whatever siting they used, and the comparison would prove nothing —
/// which is exactly how nearest-neighbour chroma survived in four places
/// until it was found. The comment up there records that forcing the CPU
/// side back to nearest read 100% worst; that was a manual experiment,
/// and a manual experiment is not a test. This is.
///
/// Stated as a fixture property rather than by mutating the converter:
/// over this frame, reconstructing chroma and taking the nearest sample
/// have to give materially different answers. If they stop doing so, the
/// fixture has gone flat and the parity assertion above is no longer
/// load-bearing, whatever it still says.
///
/// Both chroma planes are checked separately rather than together. A
/// combined `max` would let one varying plane cover for a flattened
/// other one — which it did on the first attempt at this, when only U
/// was flattened and V alone kept the assertion satisfied.
#[test]
fn the_fixture_can_see_a_siting_regression() {
    let (y, u, v) = samples();
    let s = YuvSamples {
        y: &y, u: &u, v: &v, width: W, height: H,
        chroma_shift_w: 1, chroma_shift_h: 1, bit_depth: 10, siting: Default::default(),
    };
    let cw = s.chroma_width() as usize;

    for (plane, name) in [(0usize, "Cb"), (1, "Cr")] {
        let mut differing = 0usize;
        let mut total = 0usize;
        let mut worst = 0.0f32;
        for sy in 0..H as usize {
            for sx in 0..W as usize {
                // Reconstructed, as the converter does it now.
                let recon = {
                    let (cu, cv) = s.chroma_at(sx, sy);
                    if plane == 0 { cu } else { cv }
                };
                // Nearest, as it did before — the regression this guards.
                let ci = (sy >> 1) * cw + (sx >> 1);
                let near = if plane == 0 { u[ci] as f32 } else { v[ci] as f32 };
                // A whole 10-bit code, so float noise cannot pass for a
                // discrimination.
                let d = (recon - near).abs();
                worst = worst.max(d);
                total += 1;
                if d > 1.0 {
                    differing += 1;
                }
            }
        }
        eprintln!(
            "{name}: {differing} of {total} positions read differently under nearest \
             chroma; worst {worst:.1} of 1023"
        );
        assert!(
            differing * 2 > total,
            "{name}: only {differing} of {total} positions distinguish reconstructed \
             chroma from nearest — this fixture is too flat for the parity test above \
             to be detecting a siting regression",
        );
        assert!(
            worst > 50.0,
            "{name}: the largest difference between the two sitings is {worst:.1} of \
             1023, which a parity tolerance could swallow",
        );
    }
}
