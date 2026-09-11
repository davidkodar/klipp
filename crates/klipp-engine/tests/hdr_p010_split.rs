//! The HDR split in the P010 decoder.
//!
//! HDR frames leave the decoder as packed 10-bit PQ/HLG code values, and
//! the grade reads those; the decoder's u8 output is only a display
//! preview for frames nothing grades. The two must agree — otherwise the
//! picture changes the moment a grade node goes from default to anything
//! else, which is precisely the class of bug this replaces.
//!
//! So: take the shader's codes, run them through the CPU input transform
//! and display tone map in `klipp_color::hdr`, and require the result to
//! match the shader's own preview.

use klipp_color::camera_log::primaries_to_working_matrix;
use klipp_color::hdr::{
    apply_hdr_display_tonemap, hdr10_codes_to_scene_linear_rgba, HdrTransfer,
};
use klipp_color::transfer::linear_to_srgb;
use klipp_core::color_ids;
use klipp_core::{ColorPrimaries, WorkingColorspace};
use klipp_effects::color_wgsl::pack_primaries_matrix;
use klipp_effects::p010_gpu::GpuP010Converter;

const W: u32 = 64;
const H: u32 = 32;

/// Limited-range BT.2020 content spanning the whole luma range — deep
/// shadows through PQ highlights well past 1000 nits — with a chroma
/// sweep so the gamut matrix does real work.
fn make_hdr_p010(width: u32, height: u32) -> (Vec<u8>, Vec<u8>) {
    let mut y = Vec::with_capacity((width * height * 2) as usize);
    for row in 0..height {
        for col in 0..width {
            let v: u32 = 64 + ((col * 37 + row * 11) % 877);
            y.extend_from_slice(&(((v & 0x3FF) << 6) as u16).to_le_bytes());
        }
    }
    let mut uv = Vec::with_capacity((width * height) as usize);
    for row in 0..(height / 2) {
        for col in 0..(width / 2) {
            let u = 512 + (((col as i32 - row as i32) * 23).rem_euclid(300)) as u32 - 150;
            let v = 512 + (((col as i32 + row as i32) * 29).rem_euclid(300)) as u32 - 150;
            uv.extend_from_slice(&(((u & 0x3FF) << 6) as u16).to_le_bytes());
            uv.extend_from_slice(&(((v & 0x3FF) << 6) as u16).to_le_bytes());
        }
    }
    (y, uv)
}

fn hdr_ids(transfer: u32) -> [u32; 4] {
    [
        color_ids::COLOR_RANGE_LIMITED,
        color_ids::MATRIX_BT2020_NCL,
        transfer,
        color_ids::PRIMARIES_BT2020,
    ]
}

#[test]
fn hdr_codes_through_the_cpu_input_transform_reproduce_the_shader_preview() {
    let Some(mut conv) = GpuP010Converter::new() else {
        eprintln!("skipping: P010 GPU converter unavailable");
        return;
    };
    let (y, uv) = make_hdr_p010(W, H);
    let m = primaries_to_working_matrix(ColorPrimaries::Bt2020, WorkingColorspace::Rec709Linear);

    for (transfer_id, transfer) in [
        (color_ids::TRANSFER_PQ, HdrTransfer::Pq),
        (color_ids::TRANSFER_HLG, HdrTransfer::Hlg),
    ] {
        let ids = hdr_ids(transfer_id);
        let preview = conv.convert_p010_to_rgba(
            &y, &uv, W, H, W * 2, W * 2, W, H, 0, ids, pack_primaries_matrix(m),
        );
        let codes = conv
            .convert_p010_hdr_codes(&y, &uv, W, H, W * 2, W * 2, W, H, 0, ids, pack_primaries_matrix(m))
            .expect("HDR transfer must yield codes");
        assert_eq!(codes.len(), (W * H) as usize);

        // The graded-at-identity path, on the CPU.
        let mut linear = hdr10_codes_to_scene_linear_rgba(&codes, transfer, m);
        apply_hdr_display_tonemap(&mut linear, transfer);

        let mut worst = 0i32;
        for (i, px) in linear.chunks_exact(4).enumerate() {
            for c in 0..3 {
                let cpu = (linear_to_srgb(px[c]).clamp(0.0, 1.0) * 255.0 + 0.5) as i32;
                let gpu = preview[i * 4 + c] as i32;
                worst = worst.max((cpu - gpu).abs());
            }
        }
        eprintln!("{transfer:?}: worst preview-vs-graded-path difference {worst} LSB");
        assert!(
            worst <= 1,
            "{transfer:?}: shader preview and CPU input transform disagree by {worst} LSB — \
             an ungraded frame and the same frame at an identity grade would look different",
        );
    }
}

/// The grade has to see highlights, or moving the tone map achieved
/// nothing. Content reaching past SDR white must come out of the input
/// transform above 1.0.
#[test]
fn hdr_input_transform_preserves_highlights_above_sdr_white() {
    let Some(mut conv) = GpuP010Converter::new() else {
        eprintln!("skipping: P010 GPU converter unavailable");
        return;
    };
    let (y, uv) = make_hdr_p010(W, H);
    let m = primaries_to_working_matrix(ColorPrimaries::Bt2020, WorkingColorspace::Rec709Linear);
    let codes = conv
        .convert_p010_hdr_codes(
            &y, &uv, W, H, W * 2, W * 2, W, H, 0,
            hdr_ids(color_ids::TRANSFER_PQ), pack_primaries_matrix(m),
        )
        .unwrap();
    let linear = hdr10_codes_to_scene_linear_rgba(&codes, HdrTransfer::Pq, m);
    let peak = linear.chunks_exact(4).map(|p| p[0].max(p[1]).max(p[2])).fold(0.0_f32, f32::max);
    eprintln!("scene-linear peak: {peak:.2} (1.0 = 100 nits)");
    assert!(peak > 5.0, "HDR highlights were clipped before the grade: peak {peak}");
}

/// SDR sources take none of the new path: no codes, and the transfer ids
/// the effects crate mirrors must still match klipp-core's.
#[test]
fn sdr_sources_produce_no_hdr_codes_and_ids_stay_in_step() {
    assert_eq!(color_ids::TRANSFER_PQ, 3, "p010_gpu mirrors this value");
    assert_eq!(color_ids::TRANSFER_HLG, 4, "p010_gpu mirrors this value");
    assert_eq!(HdrTransfer::from_transfer_id(color_ids::TRANSFER_PQ), Some(HdrTransfer::Pq));
    assert_eq!(HdrTransfer::from_transfer_id(color_ids::TRANSFER_BT709), None);

    let Some(mut conv) = GpuP010Converter::new() else {
        eprintln!("skipping: P010 GPU converter unavailable");
        return;
    };
    let (y, uv) = make_hdr_p010(W, H);
    let m = primaries_to_working_matrix(ColorPrimaries::Bt709, WorkingColorspace::Rec709Linear);
    let sdr = [color_ids::COLOR_RANGE_LIMITED, color_ids::MATRIX_BT709, color_ids::TRANSFER_BT709, 0];
    assert!(conv
        .convert_p010_hdr_codes(&y, &uv, W, H, W * 2, W * 2, W, H, 0, sdr, pack_primaries_matrix(m))
        .is_none());
    let out = conv.convert_p010_on_gpu(&y, &uv, W, H, W * 2, W * 2, W, H, 0, sdr, pack_primaries_matrix(m));
    assert!(out.hdr_codes.is_none(), "SDR frames must not allocate an HDR buffer");
}

/// Camera log takes the same route: the shader's S-Log3 codes, through the
/// CPU input stage and log display tone map, must reproduce its preview.
#[test]
fn slog3_codes_through_the_cpu_input_stage_reproduce_the_shader_preview() {
    let Some(mut conv) = GpuP010Converter::new() else {
        eprintln!("skipping: P010 GPU converter unavailable");
        return;
    };
    let (y, uv) = make_hdr_p010(W, H);
    let m = primaries_to_working_matrix(ColorPrimaries::SGamut3Cine, WorkingColorspace::Rec709Linear);
    // Log footage is BT.709-matrix, limited range; S-Log3 + S-Gamut3.Cine.
    let ids = [
        color_ids::COLOR_RANGE_LIMITED,
        color_ids::MATRIX_BT709,
        color_ids::TRANSFER_SLOG3,
        color_ids::primaries_id(ColorPrimaries::SGamut3Cine),
    ];
    let preview = conv.convert_p010_to_rgba(&y, &uv, W, H, W * 2, W * 2, W, H, 0, ids, pack_primaries_matrix(m));
    let codes = conv
        .convert_p010_hdr_codes(&y, &uv, W, H, W * 2, W * 2, W, H, 0, ids, pack_primaries_matrix(m))
        .expect("camera log must take the codes route");
    let transfer = HdrTransfer::Log(klipp_core::ColorTransfer::SLog3);
    let mut linear = hdr10_codes_to_scene_linear_rgba(&codes, transfer, m);
    apply_hdr_display_tonemap(&mut linear, transfer);
    let mut worst = 0i32;
    for (i, px) in linear.chunks_exact(4).enumerate() {
        for c in 0..3 {
            let cpu = (linear_to_srgb(px[c]).clamp(0.0, 1.0) * 255.0 + 0.5) as i32;
            worst = worst.max((cpu - preview[i * 4 + c] as i32).abs());
        }
    }
    eprintln!("S-Log3: worst preview-vs-graded-path difference {worst} LSB");
    assert!(worst <= 1, "S-Log3 preview and CPU input stage disagree by {worst} LSB");
}

/// The levels fix. S-Log3 is published over CV / 1023: 18% grey is code
/// 420. A limited-range file stores that code as-is, and the decoder's
/// range normalisation maps 64..940 onto 0..1 — feeding that to the curve
/// read grey ~0.05 stops and white ~0.3 stops hot. A flat Y = 420 frame
/// must decode to 0.18.
#[test]
fn slog3_published_grey_code_decodes_to_mid_grey_through_the_decoder() {
    let Some(mut conv) = GpuP010Converter::new() else {
        eprintln!("skipping: P010 GPU converter unavailable");
        return;
    };
    let flat = |code: u32| -> (Vec<u8>, Vec<u8>) {
        let y: Vec<u8> = (0..W * H).flat_map(|_| (((code & 0x3FF) << 6) as u16).to_le_bytes()).collect();
        let uv: Vec<u8> = (0..(W / 2) * (H / 2) * 2).flat_map(|_| ((512u32 << 6) as u16).to_le_bytes()).collect();
        (y, uv)
    };
    let m = primaries_to_working_matrix(ColorPrimaries::SGamut3Cine, WorkingColorspace::Rec709Linear);
    let ids = [
        color_ids::COLOR_RANGE_LIMITED,
        color_ids::MATRIX_BT709,
        color_ids::TRANSFER_SLOG3,
        color_ids::primaries_id(ColorPrimaries::SGamut3Cine),
    ];
    let transfer = HdrTransfer::Log(klipp_core::ColorTransfer::SLog3);
    for (code, expected, label) in [(420u32, 0.18_f32, "18% grey"), (598, 0.90, "90% white")] {
        let (y, uv) = flat(code);
        let codes = conv
            .convert_p010_hdr_codes(&y, &uv, W, H, W * 2, W * 2, W, H, 0, ids, pack_primaries_matrix(m))
            .unwrap();
        let scene = hdr10_codes_to_scene_linear_rgba(&codes, transfer, m);
        let g = scene[1];
        eprintln!("S-Log3 {label}: code {code} -> {g:.4} (expected {expected})");
        assert!(
            (g / expected).log2().abs() < 0.02,
            "S-Log3 {label} (code {code}) decoded to {g}, expected {expected} — levels wrong",
        );
    }
}
