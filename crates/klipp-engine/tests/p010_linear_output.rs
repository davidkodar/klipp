//! `GpuP010Converter::convert_p010_to_linear_on_gpu` is the same picture
//! as the u8 hand-off, without the quantisation.
//!
//! The same bargain as `nv12_linear_output`: the u8 path is the authority
//! — it is what the viewer ships and what `p010_gpu_frame_identity` holds
//! against the CPU — so the new linear output is checked by re-encoding
//! it and requiring the same bytes. That is a strong check precisely
//! because the shader computes *one* linear value and the two outputs
//! differ only in what they do with it.
//!
//! Why the render needs it: without this path a 10-bit frame is scaled to
//! RGBA64 by swscale instead, which measures **42 ms** on a 4K frame — the
//! single largest per-frame cost left on the export's input side, and it
//! lands on exactly the footage an HDR or 10-bit SDR delivery is cut
//! from. The last test here pins the precision argument the same way its
//! NV12 sibling does.
//!
//! **Scope: two outputs of one shader.** Both values compared here come
//! from the same converter, so the colour maths — the YUV→RGB matrix,
//! range handling, transfer — is shared and a wrong one is reproduced on
//! both sides and passes. That maths is checked against *independent* CPU
//! implementations elsewhere, and that is where to look when a picture is
//! wrong rather than merely imprecise:
//!
//! * 10-bit 4:2:0 — `yuv420p10_cpu_matches_gpu`, the P010 shader against
//!   `camera_log::yuv420p10le_to_linear`;
//! * 8-bit 4:2:0 — `display_yuv_cpu_matches_gpu`, the NV12 shader against
//!   `camera_log::yuv_to_linear_display`;
//! * scene-referred — `sw_decode_parity`, both shaders against
//!   `hdr::yuv_to_hdr_codes`.
//!
//! What these two tests *do* establish is the precision argument the
//! linear path is paid for, and the assertion below has more teeth than
//! it looks: it reads like arithmetic, but its job is to detect the case
//! where the two routes **stop** differing. A shader that quantised to
//! eight bits and expanded again would make them agree exactly, and the
//! threshold drops to zero and fires. Verified by doing precisely that —
//! it went to "0 of 1023 on 0 of 24576 samples". Do not soften it into a
//! distinct-value count or an off-grid ratio; both are weaker, because
//! both depend on which transfer curve the shader happened to use.

use klipp_effects::color_wgsl::IDENTITY_PRIMARIES_MATRIX_PACKED;
use klipp_effects::p010_gpu::GpuP010Converter;

/// Limited range, BT.709 matrix, BT.709 transfer, BT.709 primaries — the
/// display-referred 10-bit case, which is the one that took the slow
/// path. (A scene-referred 10-bit source has its own route through
/// packed codes; `p010_gpu_acescg_parity` covers that.)
const COLOR_IDS_BT709_SDR: [u32; 4] = [0, 0, 0, 0];

const W: u32 = 128;
const H: u32 = 64;

/// P010 planes: Y a diagonal ramp over the full 10-bit range, U and V on
/// different periods so no two channels agree and a swapped plane or a
/// mis-strided row shows up. Samples sit in the *upper* ten bits of each
/// u16, which is what P010 means and the easiest thing to get wrong.
fn make_p010(width: u32, height: u32) -> (Vec<u8>, Vec<u8>) {
    let mut y = Vec::with_capacity((width * height * 2) as usize);
    for row in 0..height {
        for col in 0..width {
            let code = ((col * 8 + row * 3) % 1024) as u16;
            y.extend_from_slice(&(code << 6).to_le_bytes());
        }
    }
    let mut uv = Vec::with_capacity((width * height) as usize);
    for row in 0..(height / 2) {
        for col in 0..(width / 2) {
            let u = ((col * 11 + row * 5 + 200) % 1024) as u16;
            let v = ((col * 7 + row * 13 + 600) % 1024) as u16;
            uv.extend_from_slice(&(u << 6).to_le_bytes());
            uv.extend_from_slice(&(v << 6).to_le_bytes());
        }
    }
    (y, uv)
}

/// Strides in bytes: two per sample for Y, four per chroma pair.
const Y_STRIDE: u32 = W * 2;
const UV_STRIDE: u32 = W * 2;

fn linear_frame(conv: &mut GpuP010Converter) -> Vec<f32> {
    let (y, uv) = make_p010(W, H);
    let buf = conv.convert_p010_to_linear_on_gpu(
        &y,
        &uv,
        W,
        H,
        Y_STRIDE,
        UV_STRIDE,
        W,
        H,
        0,
        COLOR_IDS_BT709_SDR,
        IDENTITY_PRIMARIES_MATRIX_PACKED,
    );
    let (device, queue) = (conv.device(), conv.queue());
    let bytes = (W as u64) * (H as u64) * 16;
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("p010_linear_readback"),
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

fn u8_frame(conv: &mut GpuP010Converter) -> Vec<u8> {
    let (y, uv) = make_p010(W, H);
    conv.convert_p010_to_rgba(
        &y,
        &uv,
        W,
        H,
        Y_STRIDE,
        UV_STRIDE,
        W,
        H,
        0,
        COLOR_IDS_BT709_SDR,
        IDENTITY_PRIMARIES_MATRIX_PACKED,
    )
}

/// Re-encoding the linear output gives the u8 output back. One shader,
/// one linear value, two ways of writing it down.
#[test]
fn the_linear_output_re_encodes_to_the_u8_output() {
    let Some(mut conv) = GpuP010Converter::new() else {
        eprintln!("skipping: P010 GPU converter unavailable on this system");
        return;
    };
    let linear = linear_frame(&mut conv);
    let u8s = u8_frame(&mut conv);
    assert_eq!(linear.len(), u8s.len(), "pixel count mismatch");

    let mut worst = 0i32;
    let mut worst_at = 0usize;
    for (i, (l, &want)) in linear.iter().zip(u8s.iter()).enumerate() {
        let got = if i % 4 == 3 {
            (l * 255.0 + 0.5).clamp(0.0, 255.0) as i32
        } else {
            let enc = klipp_color::transfer::linear_to_srgb(*l);
            (enc * 255.0 + 0.5).clamp(0.0, 255.0) as i32
        };
        let delta = (got - want as i32).abs();
        if delta > worst {
            worst = delta;
            worst_at = i;
        }
    }
    eprintln!("worst re-encode difference: {worst} level(s)");
    assert!(
        worst <= 1,
        "re-encoded linear differs from the u8 path by {worst} levels at sample {worst_at}"
    );

    // And the fixture is not black or flat, or the agreement above would
    // be agreement about nothing.
    let spread = linear
        .chunks_exact(4)
        .map(|p| p[0])
        .fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
    assert!(
        spread.1 - spread.0 > 0.5,
        "the fixture's red channel spans only {:.4}..{:.4}",
        spread.0,
        spread.1,
    );
}

/// Ten bits do not fit in eight, and the render is the caller that
/// notices.
///
/// This is the same argument its NV12 sibling makes, but stronger here:
/// there the loss was subtle because an 8-bit Y step and an 8-bit RGB
/// step are comparable resolutions. A *10-bit* source carries four times
/// that, so narrowing to u8 throws away real precision even before a
/// grade amplifies it — and a 10-bit deliverable made from a u8
/// intermediate cannot be better than eight bits however it is encoded.
#[test]
fn the_u8_output_cannot_reproduce_the_linear_one() {
    let Some(mut conv) = GpuP010Converter::new() else {
        eprintln!("skipping: P010 GPU converter unavailable on this system");
        return;
    };
    let linear = linear_frame(&mut conv);
    let u8s = u8_frame(&mut conv);

    // A plain contrast push, applied to the linear values and to the u8
    // route's reconstruction of them, quantised to ten bits at the end —
    // which is what a 10-bit delivery does.
    let gain = 3.0f32;
    let mut worst = 0i32;
    let mut differing = 0usize;
    let mut total = 0usize;
    for (i, (l, &byte)) in linear.iter().zip(u8s.iter()).enumerate() {
        if i % 4 == 3 {
            continue;
        }
        total += 1;
        let from_u8 = klipp_color::transfer::srgb_to_linear(byte as f32 / 255.0);
        let q = |v: f32| {
            (klipp_color::transfer::linear_to_srgb(v * gain) * 1023.0 + 0.5).clamp(0.0, 1023.0)
                as i32
        };
        let d = (q(*l) - q(from_u8)).abs();
        if d > 0 {
            differing += 1;
        }
        worst = worst.max(d);
    }
    eprintln!(
        "after a {gain}x push, the u8 route differs by up to {worst} of 1023 \
         on {differing} of {total} samples"
    );
    assert!(
        worst >= 4,
        "the u8 route reproduced the linear one to {worst} of 1023, so this path buys nothing"
    );
}
