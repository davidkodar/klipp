//! The software decode path's scene-referred conversion
//! (`klipp_color::hdr::yuv_to_hdr_codes`) against the GPU decoder shaders.
//!
//! A clip that decodes in software — ProRes, anything NVDEC cannot take,
//! the no-GPU fallback, thumbnails and skim frames — must produce the same
//! code values as the same clip on the GPU, or its picture shifts depending
//! on which decoder happened to run. Everything downstream of the codes is
//! already one implementation, so code parity is picture parity.

use klipp_color::camera_log::primaries_to_working_matrix;
use klipp_color::hdr::{unpack_rgb10, yuv_to_hdr_codes, YuvSamples};
use klipp_core::color_ids;
use klipp_core::{ColorPrimaries, WorkingColorspace};
use klipp_effects::color_wgsl::pack_primaries_matrix;
use klipp_effects::nv12_gpu::GpuNv12Converter;
use klipp_effects::p010_gpu::GpuP010Converter;

const W: u32 = 64;
const H: u32 = 32;

/// 10-bit samples spanning below-black to super-white, chroma swept.
fn pattern_10bit() -> (Vec<u16>, Vec<u16>, Vec<u16>) {
    let y = (0..H).flat_map(|r| (0..W).map(move |c| (40 + (c * 37 + r * 11) % 980) as u16)).collect();
    let u = (0..H / 2)
        .flat_map(|r| (0..W / 2).map(move |c| (362 + ((c as i32 - r as i32) * 23).rem_euclid(300)) as u16))
        .collect();
    let v = (0..H / 2)
        .flat_map(|r| (0..W / 2).map(move |c| (362 + ((c as i32 + r as i32) * 29).rem_euclid(300)) as u16))
        .collect();
    (y, u, v)
}

fn p010_bytes(y: &[u16], u: &[u16], v: &[u16]) -> (Vec<u8>, Vec<u8>) {
    let yb = y.iter().flat_map(|s| (s << 6).to_le_bytes()).collect();
    let uvb = u.iter().zip(v).flat_map(|(a, b)| [(a << 6).to_le_bytes(), (b << 6).to_le_bytes()]).flatten().collect();
    (yb, uvb)
}

fn compare(label: &str, gpu: &[u32], cpu: &[u32]) {
    assert_eq!(gpu.len(), cpu.len(), "{label}: sizes differ");
    let mut worst = 0i32;
    let mut differing = 0usize;
    for (g, c) in gpu.iter().zip(cpu) {
        let d = unpack_rgb10(*g).iter().zip(unpack_rgb10(*c)).map(|(a, b)| (*a as i32 - b as i32).abs()).max().unwrap();
        worst = worst.max(d);
        differing += (d > 0) as usize;
    }
    eprintln!("{label}: {differing}/{} pixels differ, worst {worst} code", gpu.len());
    // Float evaluation order can tip a value sitting on a rounding boundary.
    assert!(worst <= 1, "{label}: CPU and GPU codes differ by {worst}");
    assert!(differing * 100 <= gpu.len(), "{label}: {differing} pixels differ — not just rounding");
}

#[test]
fn cpu_codes_match_the_p010_shader() {
    let Some(mut conv) = GpuP010Converter::new() else {
        eprintln!("skipping: P010 GPU converter unavailable");
        return;
    };
    let (y, u, v) = pattern_10bit();
    let (yb, uvb) = p010_bytes(&y, &u, &v);
    let samples = YuvSamples { y: &y, u: &u, v: &v, width: W, height: H, chroma_shift_w: 1, chroma_shift_h: 1, bit_depth: 10 , siting: Default::default()};
    let pm = pack_primaries_matrix(primaries_to_working_matrix(ColorPrimaries::Bt2020, WorkingColorspace::Rec709Linear));
    let cases = [
        ("PQ limited", [color_ids::COLOR_RANGE_LIMITED, color_ids::MATRIX_BT2020_NCL, color_ids::TRANSFER_PQ, 0]),
        ("HLG limited", [color_ids::COLOR_RANGE_LIMITED, color_ids::MATRIX_BT2020_NCL, color_ids::TRANSFER_HLG, 0]),
        ("S-Log3 limited", [color_ids::COLOR_RANGE_LIMITED, color_ids::MATRIX_BT709, color_ids::TRANSFER_SLOG3, 0]),
        ("S-Log3 full", [color_ids::COLOR_RANGE_FULL, color_ids::MATRIX_BT709, color_ids::TRANSFER_SLOG3, 0]),
    ];
    for (label, ids) in cases {
        for (dw, dh, rot) in [(W, H, 0u32), (W / 2, H / 2, 0), (W, H, 180), (H, W, 90), (H / 2, W / 2, 270)] {
            let gpu = conv
                .convert_p010_hdr_codes(&yb, &uvb, W, H, W * 2, W * 2, dw, dh, rot, ids, pm)
                .expect("scene-referred transfer yields codes");
            let cpu = yuv_to_hdr_codes(&samples, dw, dh, rot, ids);
            compare(&format!("{label} {dw}x{dh} rot {rot}"), &gpu, cpu.packed10().expect("10-bit codes"));
        }
    }
}

#[test]
fn cpu_codes_match_the_nv12_shader() {
    let Some(mut conv) = GpuNv12Converter::new() else {
        eprintln!("skipping: NV12 GPU converter unavailable");
        return;
    };
    let (y10, u10, v10) = pattern_10bit();
    let (y, u, v): (Vec<u16>, Vec<u16>, Vec<u16>) = (
        y10.iter().map(|s| s / 4).collect(),
        u10.iter().map(|s| s / 4).collect(),
        v10.iter().map(|s| s / 4).collect(),
    );
    let yb: Vec<u8> = y.iter().map(|s| *s as u8).collect();
    let uvb: Vec<u8> = u.iter().zip(&v).flat_map(|(a, b)| [*a as u8, *b as u8]).collect();
    let samples = YuvSamples { y: &y, u: &u, v: &v, width: W, height: H, chroma_shift_w: 1, chroma_shift_h: 1, bit_depth: 8 , siting: Default::default()};
    let pm = pack_primaries_matrix(primaries_to_working_matrix(ColorPrimaries::SGamut3Cine, WorkingColorspace::Rec709Linear));
    let ids = [color_ids::COLOR_RANGE_LIMITED, color_ids::MATRIX_BT709, color_ids::TRANSFER_SLOG3, 0];
    for (dw, dh, rot) in [(W, H, 0u32), (W / 2, H / 2, 90)] {
        let gpu = conv.convert_nv12_hdr_codes(&yb, &uvb, W, H, W, W, dw, dh, rot, ids, pm).unwrap();
        let cpu = yuv_to_hdr_codes(&samples, dw, dh, rot, ids);
        compare(&format!("8-bit S-Log3 {dw}x{dh} rot {rot}"), &gpu, cpu.packed10().expect("8-bit codes"));
    }
}

/// 4:2:2 has no GPU twin to compare with, so pin the chroma addressing
/// directly: a 4:2:2 frame whose chroma rows differ must use each row's own
/// chroma, where 4:2:0 addressing would take every other row's.
#[test]
fn chroma_422_uses_full_height_chroma() {
    let (w, h) = (4u32, 2u32);
    let y = vec![420u16; 8];
    // Row 0 neutral, row 1 strongly red (V high).
    let u = vec![512u16; 4];
    let v = vec![512, 512, 800, 800];
    let s = YuvSamples { y: &y, u: &u, v: &v, width: w, height: h, chroma_shift_w: 1, chroma_shift_h: 0, bit_depth: 10 , siting: Default::default()};
    let ids = [color_ids::COLOR_RANGE_LIMITED, color_ids::MATRIX_BT709, color_ids::TRANSFER_SLOG3, 0];
    let codes = yuv_to_hdr_codes(&s, w, h, 0, ids);
    let codes = codes.packed10().expect("10-bit codes");
    let [r0, g0, b0] = unpack_rgb10(codes[0]);
    let [r1, g1, b1] = unpack_rgb10(codes[4]);
    assert_eq!((r0, g0, b0), (420, 420, 420), "neutral row stays neutral");
    // Cr moves R up and G down; B depends on Cb only.
    assert!(r1 > 500 && g1 < 420 && b1 == 420, "second row must take its own (red) chroma: {r1}/{g1}/{b1}");
}
