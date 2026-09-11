//! Camera log recorded 8-bit (e.g. 8-bit XAVC S-Log3) through the NV12
//! decoder. Same contract as the P010 route in `hdr_p010_split.rs`: the
//! shader emits 10-bit code values for the grade and a tone-mapped preview
//! for everything else, and the two must agree.

use klipp_color::camera_log::primaries_to_working_matrix;
use klipp_color::hdr::{
    apply_hdr_display_tonemap, hdr10_codes_to_scene_linear_rgba, unpack_rgb10, HdrTransfer,
};
use klipp_color::transfer::linear_to_srgb;
use klipp_core::color_ids;
use klipp_core::{ColorPrimaries, ColorTransfer, WorkingColorspace};
use klipp_effects::color_wgsl::pack_primaries_matrix;
use klipp_effects::nv12_gpu::GpuNv12Converter;

const W: u32 = 64;
const H: u32 = 32;

fn slog3_ids() -> [u32; 4] {
    [
        color_ids::COLOR_RANGE_LIMITED,
        color_ids::MATRIX_BT709,
        color_ids::TRANSFER_SLOG3,
        color_ids::primaries_id(ColorPrimaries::SGamut3Cine),
    ]
}

/// Limited-range 8-bit luma across its legal range with a chroma sweep.
fn make_nv12(width: u32, height: u32) -> (Vec<u8>, Vec<u8>) {
    let y: Vec<u8> = (0..height)
        .flat_map(|row| (0..width).map(move |col| (16 + (col * 7 + row * 3) % 220) as u8))
        .collect();
    let uv: Vec<u8> = (0..height / 2)
        .flat_map(|row| {
            (0..width / 2).flat_map(move |col| {
                let u = 128 + ((col as i32 - row as i32) * 5).rem_euclid(80) - 40;
                let v = 128 + ((col as i32 + row as i32) * 7).rem_euclid(80) - 40;
                [u as u8, v as u8]
            })
        })
        .collect();
    (y, uv)
}

fn flat_nv12(code: u8) -> (Vec<u8>, Vec<u8>) {
    (vec![code; (W * H) as usize], vec![128; (W * H / 2) as usize])
}

#[test]
fn slog3_8bit_codes_through_the_cpu_input_stage_reproduce_the_shader_preview() {
    let Some(mut conv) = GpuNv12Converter::new() else {
        eprintln!("skipping: NV12 GPU converter unavailable");
        return;
    };
    let (y, uv) = make_nv12(W, H);
    let m = primaries_to_working_matrix(ColorPrimaries::SGamut3Cine, WorkingColorspace::Rec709Linear);
    let pm = pack_primaries_matrix(m);
    let ids = slog3_ids();
    let preview = conv.convert_nv12_to_rgba(&y, &uv, W, H, W, W, W, H, 0, ids, pm);
    let codes = conv
        .convert_nv12_hdr_codes(&y, &uv, W, H, W, W, W, H, 0, ids, pm)
        .expect("8-bit camera log must take the codes route");
    assert_eq!(codes.len(), (W * H) as usize);

    let transfer = HdrTransfer::Log(ColorTransfer::SLog3);
    let mut linear = hdr10_codes_to_scene_linear_rgba(&codes, transfer, m);
    apply_hdr_display_tonemap(&mut linear, transfer);
    let mut worst = 0i32;
    for (i, px) in linear.chunks_exact(4).enumerate() {
        for c in 0..3 {
            let cpu = (linear_to_srgb(px[c]).clamp(0.0, 1.0) * 255.0 + 0.5) as i32;
            worst = worst.max((cpu - preview[i * 4 + c] as i32).abs());
        }
    }
    eprintln!("8-bit S-Log3: worst preview-vs-graded-path difference {worst} LSB");
    assert!(worst <= 1, "8-bit S-Log3 preview and CPU input stage disagree by {worst} LSB");
}

/// The levels mapping, exactly: a limited-range 8-bit code c is the 10-bit
/// code 4c (BT.709 scales by 2^(n-8)), so the curve sees the same value an
/// equivalent 10-bit file would give it. 18% grey (10-bit 420) is 8-bit 105.
#[test]
fn slog3_8bit_codes_land_on_their_10bit_equivalents_and_grey_is_mid_grey() {
    let Some(mut conv) = GpuNv12Converter::new() else {
        eprintln!("skipping: NV12 GPU converter unavailable");
        return;
    };
    let m = primaries_to_working_matrix(ColorPrimaries::SGamut3Cine, WorkingColorspace::Rec709Linear);
    let pm = pack_primaries_matrix(m);
    for code in [16u8, 64, 105, 150, 200, 235] {
        let (y, uv) = flat_nv12(code);
        let codes = conv
            .convert_nv12_hdr_codes(&y, &uv, W, H, W, W, W, H, 0, slog3_ids(), pm)
            .unwrap();
        let c10 = code as u32 * 4;
        assert_eq!(
            unpack_rgb10(codes[0]),
            [c10, c10, c10],
            "8-bit code {code} must map to 10-bit code {c10}",
        );
    }
    let (y, uv) = flat_nv12(105);
    let codes = conv.convert_nv12_hdr_codes(&y, &uv, W, H, W, W, W, H, 0, slog3_ids(), pm).unwrap();
    let scene = hdr10_codes_to_scene_linear_rgba(&codes, HdrTransfer::Log(ColorTransfer::SLog3), m);
    eprintln!("8-bit S-Log3 grey: code 105 -> {:.4}", scene[1]);
    assert!((scene[1] / 0.18).log2().abs() < 0.02, "8-bit S-Log3 grey decoded to {}", scene[1]);
}

/// SDR NV12 stays on its old path: no codes, no extra buffer.
#[test]
fn sdr_nv12_produces_no_codes() {
    let Some(mut conv) = GpuNv12Converter::new() else {
        eprintln!("skipping: NV12 GPU converter unavailable");
        return;
    };
    let (y, uv) = make_nv12(W, H);
    let m = primaries_to_working_matrix(ColorPrimaries::Bt709, WorkingColorspace::Rec709Linear);
    let sdr = [color_ids::COLOR_RANGE_LIMITED, color_ids::MATRIX_BT709, color_ids::TRANSFER_BT709, 0];
    assert!(conv
        .convert_nv12_hdr_codes(&y, &uv, W, H, W, W, W, H, 0, sdr, pack_primaries_matrix(m))
        .is_none());
    let out = conv.convert_nv12_on_gpu(&y, &uv, W, H, W, W, W, H, 0, sdr, pack_primaries_matrix(m));
    assert!(out.hdr_codes.is_none(), "SDR frames must not allocate a codes buffer");
}
