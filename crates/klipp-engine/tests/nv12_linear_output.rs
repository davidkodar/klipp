//! `GpuNv12Converter::convert_nv12_to_linear_on_gpu` is the same picture
//! as the u8 hand-off, without the quantisation.
//!
//! The u8 path is the authority here: it is what the viewer has shipped
//! and what `sw_decode_parity` holds against swscale. So the new linear
//! output is checked by re-encoding it and requiring the same bytes —
//! which is a strong check, because the shader computes one linear value
//! and the two outputs differ only in what they do with it.
//!
//! And then the point of the exercise, in the other direction: the u8
//! output *cannot* reproduce the linear one. That is the whole reason
//! the render needs this path, so it is pinned rather than asserted in
//! a commit message.
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
use klipp_effects::nv12_gpu::GpuNv12Converter;

/// Limited range, BT.709 matrix, BT.709 transfer, BT.709 primaries.
const COLOR_IDS_BT709_SDR: [u32; 4] = [0, 0, 0, 0];

const W: u32 = 128;
const H: u32 = 64;

/// Y a diagonal ramp, U and V ramps on different periods, so no two
/// channels agree and a swapped row or plane shows up.
fn make_nv12(width: u32, height: u32) -> (Vec<u8>, Vec<u8>) {
    let mut y = Vec::with_capacity((width * height) as usize);
    for row in 0..height {
        for col in 0..width {
            y.push(((col + row) & 0xFF) as u8);
        }
    }
    let mut uv = Vec::with_capacity((width * height / 2) as usize);
    for row in 0..(height / 2) {
        for col in 0..(width / 2) {
            uv.push(((col + row + 64) & 0xFF) as u8);
            uv.push(((col * 2 + row + 128) & 0xFF) as u8);
        }
    }
    (y, uv)
}

fn linear_frame(conv: &mut GpuNv12Converter) -> Vec<f32> {
    let (y, uv) = make_nv12(W, H);
    let buf = conv.convert_nv12_to_linear_on_gpu(
        &y, &uv, W, H, W, W, W, H, 0,
        COLOR_IDS_BT709_SDR, IDENTITY_PRIMARIES_MATRIX_PACKED,
    );
    // Read it back through a staging buffer; the converter's own device.
    let (device, queue) = (conv.device(), conv.queue());
    let bytes = (W as u64) * (H as u64) * 16;
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("nv12_linear_readback"),
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

fn u8_frame(conv: &mut GpuNv12Converter) -> Vec<u8> {
    let (y, uv) = make_nv12(W, H);
    conv.convert_nv12_to_rgba(
        &y, &uv, W, H, W, W, W, H, 0,
        COLOR_IDS_BT709_SDR, IDENTITY_PRIMARIES_MATRIX_PACKED,
    )
}

/// Re-encoding the linear output gives the u8 output back. One shader,
/// one linear value, two ways of writing it down.
#[test]
fn the_linear_output_re_encodes_to_the_u8_output() {
    let Some(mut conv) = GpuNv12Converter::new() else {
        eprintln!("skipping: NV12 GPU converter unavailable on this system");
        return;
    };
    let linear = linear_frame(&mut conv);
    let u8s = u8_frame(&mut conv);
    assert_eq!(linear.len(), u8s.len(), "pixel count mismatch");

    let mut worst = 0i32;
    let mut worst_at = 0usize;
    for (i, (l, &want)) in linear.iter().zip(u8s.iter()).enumerate() {
        // Alpha is 1.0 / 255 in the two representations; the transfer
        // applies to colour only.
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
    // One level, for the rounding that happens on each side of the
    // encode rather than once.
    assert!(
        worst <= 1,
        "re-encoded linear differs from the u8 path by {worst} levels at sample {worst_at}"
    );
    eprintln!("worst re-encode difference: {worst} level(s)");
}

/// The reason this path exists — measured where it actually shows.
///
/// In the raw conversion the u8 hand-off looks almost free: an 8-bit Y
/// step and an 8-bit RGB step are comparable resolutions, so only a
/// handful of neighbouring pixels collapse onto the same code. The loss
/// appears once a grade *amplifies* it, which is what a render does and
/// a decode does not. So this applies a plain contrast gain to both and
/// compares the results: the u8 route's rounding error is multiplied,
/// and it lands on a visibly different level.
///
/// This is the same effect that cost 7 levels on the user's footage when
/// the export briefly narrowed to 8 bits, and it is why the render pays
/// for 16 bytes a pixel here instead of 4.
#[test]
fn a_grade_amplifies_what_the_u8_output_threw_away() {
    let Some(mut conv) = GpuNv12Converter::new() else {
        eprintln!("skipping: NV12 GPU converter unavailable on this system");
        return;
    };
    let linear = linear_frame(&mut conv);
    let u8s = u8_frame(&mut conv);

    // A contrast push about mid grey, the most ordinary grade there is.
    let graded = |v: f32| ((v - 0.18) * 3.0 + 0.18).clamp(0.0, 1.0);

    let mut worst = 0i32;
    let mut differing = 0usize;
    for i in 0..linear.len() {
        if i % 4 == 3 {
            continue;
        }
        // Full precision: grade the linear value the shader computed.
        let from_linear = graded(linear[i]);
        // Eight bits: the same value after a round trip through the u8
        // hand-off, which is all the u8 route has to grade from.
        let from_u8 = graded(klipp_color::transfer::srgb_to_linear(u8s[i] as f32 / 255.0));

        let a = (klipp_color::transfer::linear_to_srgb(from_linear) * 255.0 + 0.5) as i32;
        let b = (klipp_color::transfer::linear_to_srgb(from_u8) * 255.0 + 0.5) as i32;
        if a != b {
            differing += 1;
        }
        worst = worst.max((a - b).abs());
    }

    assert!(
        worst >= 1,
        "a 3x contrast push found no difference between the 16-byte and the \
         4-byte route, so this fixture does not demonstrate the precision \
         the render is paying for"
    );
    eprintln!(
        "after a 3x contrast push: {differing} samples differ, worst {worst} level(s)"
    );
}
