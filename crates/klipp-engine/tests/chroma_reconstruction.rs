//! The decode shaders reconstruct subsampled chroma instead of repeating
//! it, and reconstruct it in the right place.
//!
//! `sw_decode_parity` proves the GPU and the CPU twin agree. It cannot
//! prove they agree on the *right* answer: both were nearest-neighbour
//! and matched each other perfectly, repeating one chroma sample across
//! each 2x2 luma block and stepping hard wherever chroma changed. So this
//! drives the real shader on a hard chroma edge and checks the shape.
//!
//! `klipp-color`'s `chroma_siting_tests` pin the arithmetic on the CPU
//! side, where it is cheap to state exactly. This pins the shader, which
//! is the copy that can drift from it.
//!
//! Siting is H.264/HEVC `chroma_sample_loc_type = 0` (ffmpeg's
//! `AVCHROMA_LOC_LEFT`), the default for everything Klipp decodes: a
//! chroma sample shares a column with the even luma sample and sits
//! vertically between the two luma rows.

use klipp_color::camera_log::primaries_to_working_matrix;
use klipp_core::color_ids;
use klipp_core::{ColorPrimaries, WorkingColorspace};
use klipp_effects::color_wgsl::pack_primaries_matrix;
use klipp_effects::nv12_gpu::GpuNv12Converter;

const W: u32 = 16;
const H: u32 = 4;

/// Flat luma with a hard chroma edge down the middle: saturated red on
/// the left, saturated blue on the right. Luma is constant, so chroma
/// reconstruction is the only thing that can vary across the frame.
///
/// The edge falls between chroma columns 3 and 4, which is between luma
/// columns 7 and 8.
fn edge_nv12() -> (Vec<u8>, Vec<u8>) {
    let y = vec![128u8; (W * H) as usize];
    let mut uv = Vec::new();
    for _ in 0..H / 2 {
        for cx in 0..W / 2 {
            if cx < W / 4 {
                uv.extend_from_slice(&[90, 240]); // Cb low, Cr high: red
            } else {
                uv.extend_from_slice(&[240, 90]); // blue
            }
        }
    }
    (y, uv)
}

#[test]
fn the_nv12_shader_reconstructs_chroma_where_h264_sites_it() {
    let Some(mut conv) = GpuNv12Converter::new() else {
        eprintln!("skipping: no GPU NV12 converter");
        return;
    };
    let (y, uv) = edge_nv12();
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
    let out = conv.convert_nv12_to_rgba(&y, &uv, W, H, W, W, W, H, 0, ids, pm);
    let px = |x: usize| [out[x * 4] as i32, out[x * 4 + 1] as i32, out[x * 4 + 2] as i32];
    let apart = |a: [i32; 3], b: [i32; 3]| (0..3).map(|c| (a[c] - b[c]).abs()).max().unwrap();

    for x in 0..W as usize {
        let p = px(x);
        eprintln!("  x={x:>2}: {:>3},{:>3},{:>3}", p[0], p[1], p[2]);
    }

    // The plateaus, read well away from the edge.
    let (left, right) = (px(2), px(13));
    assert!(apart(left, right) > 100, "the fixture needs a real chroma edge");

    // Nearest sampling leaves *nothing* between the plateaus: every pixel
    // is exactly one of the two. This is the assertion that fails if the
    // shader goes back to repeating a sample.
    let blended = (0..W as usize)
        .filter(|x| apart(px(*x), left) > 12 && apart(px(*x), right) > 12)
        .collect::<Vec<_>>();
    assert!(
        !blended.is_empty(),
        "no pixel sits between the two chroma values — the shader is still nearest-neighbour",
    );

    // And it is reconstructed in the right place. Co-sited horizontally
    // means the even luma columns land on a chroma sample exactly and the
    // blend falls on the odd column between them — here x=7, the odd
    // column left of the boundary. A centre-sited implementation would
    // blend at x=8 instead, which is the same amount of smoothing applied
    // half a pixel to the right: every colour edge in the programme
    // shifted sideways, and nothing that looks like a bug.
    assert_eq!(blended, vec![7], "the blend is not on the co-sited column");
    assert_eq!(apart(px(6), left), 0, "x=6 must read chroma column 3 exactly");
    assert_eq!(apart(px(8), right), 0, "x=8 must read chroma column 4 exactly");
}

/// Bars with hard colour edges, NV12 at `w`×`h`: luma and chroma both
/// step, so a downscale that takes other pixels, or averages them in
/// another light, shows.
fn bars_nv12(w: u32, h: u32) -> (Vec<u8>, Vec<u8>) {
    let bar = |x: u32| (x * 7 / w) as usize;
    const Y: [u8; 7] = [180, 162, 131, 112, 84, 65, 35];
    const UV: [[u8; 2]; 7] = [[44, 142], [156, 44], [72, 58], [184, 198], [100, 212], [212, 114], [128, 128]];
    let y = (0..h).flat_map(|r| (0..w).map(move |x| Y[bar(x)].saturating_add(if r % 9 == 0 { 40 } else { 0 }))).collect();
    let uv = (0..h / 2).flat_map(|_| (0..w / 2).flat_map(|cx| UV[bar(cx * 2)])).collect();
    (y, uv)
}

/// #3266: the export's software decode resizes a frame with the CPU
/// twins of the shaders' resize, `yuv_to_display_rgba64_downscaled` (the
/// footprint average in linear light, #744) and `lanczos_upscale_rgba64`
/// (Lanczos-3 on the encoded values, #2557), so a software-decoded file
/// resizes as a hardware-decoded one does and as the viewer shows it.
/// Shrunk at 2:1, 3:2 and 1.6:1, grown at 1.5 and 2 times, every pixel is
/// within a level of the shader's.
#[test]
fn the_cpu_resize_is_the_decode_shaders() {
    let Some(mut conv) = GpuNv12Converter::new() else {
        if std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some() {
            panic!("KLIPP_TEST_REQUIRE_GPU is set and there is no GPU NV12 converter");
        }
        eprintln!("skipping: no GPU NV12 converter");
        return;
    };
    let (sw, sh) = (96u32, 54u32);
    let (y, uv) = bars_nv12(sw, sh);
    let pm = pack_primaries_matrix(primaries_to_working_matrix(ColorPrimaries::Bt709, WorkingColorspace::Rec709Linear));
    let ids = [color_ids::COLOR_RANGE_LIMITED, color_ids::MATRIX_BT709, color_ids::TRANSFER_BT709, 0];
    let cs = klipp_core::SourceColorspace {
        range: klipp_core::ColorRange::Limited,
        matrix: klipp_core::ColorMatrix::Bt709,
        transfer: klipp_core::ColorTransfer::Bt709,
        primaries: ColorPrimaries::Bt709,
    };
    let y16: Vec<u16> = y.iter().map(|&v| v as u16).collect();
    let u16s: Vec<u16> = uv.iter().step_by(2).map(|&v| v as u16).collect();
    let v16s: Vec<u16> = uv.iter().skip(1).step_by(2).map(|&v| v as u16).collect();
    let samples = klipp_color::hdr::YuvSamples { y: &y16, u: &u16s, v: &v16s, width: sw, height: sh, chroma_shift_w: 1, chroma_shift_h: 1, bit_depth: 8 , siting: Default::default()};
    for (dw, dh) in [(48u32, 27u32), (64, 36), (60, 34), (144, 81), (192, 108)] {
        let shrinks = klipp_color::camera_log::resize_averages((sw, sh), (dw, dh));
        assert_eq!(shrinks, dw < sw, "{dw}x{dh}");
        let gpu = conv.convert_nv12_to_rgba(&y, &uv, sw, sh, sw, sw, dw, dh, 0, ids, pm);
        let cpu = if shrinks {
            klipp_color::camera_log::yuv_to_display_rgba64_downscaled(&samples, cs, None, dw, dh)
        } else {
            klipp_color::camera_log::lanczos_upscale_rgba64(&klipp_color::camera_log::yuv_to_display_rgba64(&samples, cs), (sw, sh), (dw, dh))
        };
        let mut worst = 0;
        for i in 0..(dw * dh) as usize {
            for c in 0..3 {
                let cpu8 = (cpu[i * 4 + c] as f32 / 65535.0 * 255.0).round() as i32;
                worst = worst.max((cpu8 - gpu[i * 4 + c] as i32).abs());
            }
        }
        eprintln!("{sw}x{sh} → {dw}x{dh}: worst {worst} levels");
        assert!(worst <= 1, "{dw}x{dh}: the CPU resize is {worst} levels from the shader's");
    }
}

/// #3266: the decode shaders reconstruct chroma where the file says it
/// sits. At each siting (left, centre, top-left, top), the NV12 and P010
/// shaders' frame of bars with hard edges across and down is within a
/// level of `yuv_to_display_rgba64` at that siting, the CPU converter the
/// software decode and the export use; and the shader told centre does
/// not draw what it draws at left.
#[test]
fn the_shaders_take_a_files_chroma_siting() {
    use klipp_color::hdr::ChromaSiting;
    use klipp_effects::p010_gpu::GpuP010Converter;
    let (Some(mut nv12), Some(mut p010)) = (GpuNv12Converter::new(), GpuP010Converter::new()) else {
        if std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some() {
            panic!("KLIPP_TEST_REQUIRE_GPU is set and there is no GPU converter");
        }
        eprintln!("skipping: no GPU converter");
        return;
    };
    let (w, h) = (96u32, 54u32);
    // Bars across, and a hard band change down every 9 rows.
    let (mut y, uv) = bars_nv12(w, h);
    for (i, v) in y.iter_mut().enumerate() {
        if (i as u32 / w / 9) % 2 == 1 {
            *v = v.saturating_sub(30);
        }
    }
    let uv: Vec<u8> = uv.chunks_exact(2).enumerate().flat_map(|(i, p)| if (i as u32 / (w / 2) * 2 / 9) % 2 == 1 { [p[1], p[0]] } else { [p[0], p[1]] }).collect();
    let pm = pack_primaries_matrix(primaries_to_working_matrix(ColorPrimaries::Bt709, WorkingColorspace::Rec709Linear));
    let cs = klipp_core::SourceColorspace {
        range: klipp_core::ColorRange::Limited,
        matrix: klipp_core::ColorMatrix::Bt709,
        transfer: klipp_core::ColorTransfer::Bt709,
        primaries: ColorPrimaries::Bt709,
    };
    let y16: Vec<u16> = y.iter().map(|&v| v as u16).collect();
    let u16s: Vec<u16> = uv.iter().step_by(2).map(|&v| v as u16).collect();
    let v16s: Vec<u16> = uv.iter().skip(1).step_by(2).map(|&v| v as u16).collect();
    // The same picture at 10 bits, for P010: each code four times over.
    let y10: Vec<u16> = y16.iter().map(|&v| v << 2).collect();
    let (u10, v10): (Vec<u16>, Vec<u16>) = (u16s.iter().map(|&v| v << 2).collect(), v16s.iter().map(|&v| v << 2).collect());
    let yb: Vec<u8> = y10.iter().flat_map(|s| (s << 6).to_le_bytes()).collect();
    let uvb: Vec<u8> = u10.iter().zip(&v10).flat_map(|(a, b)| [(a << 6).to_le_bytes(), (b << 6).to_le_bytes()]).flatten().collect();
    let to8 = |cpu: &[u16]| cpu.iter().map(|&c| (c as f32 / 65535.0 * 255.0).round() as i32).collect::<Vec<_>>();
    let apart = |a: &[i32], b: &[u8]| a.chunks_exact(4).zip(b.chunks_exact(4)).map(|(a, b)| (0..3).map(|c| (a[c] - b[c] as i32).abs()).max().unwrap()).max().unwrap();
    let mut at_left = None;
    for (name, location) in [("left", 1), ("centre", 2), ("top-left", 3), ("top", 4)] {
        let siting = ChromaSiting::from_av_location(location);
        let s8 = klipp_color::hdr::YuvSamples { y: &y16, u: &u16s, v: &v16s, width: w, height: h, chroma_shift_w: 1, chroma_shift_h: 1, bit_depth: 8, siting };
        let s10 = klipp_color::hdr::YuvSamples { y: &y10, u: &u10, v: &v10, width: w, height: h, chroma_shift_w: 1, chroma_shift_h: 1, bit_depth: 10, siting };
        nv12.set_chroma_offsets(siting.shader_offsets());
        p010.set_chroma_offsets(siting.shader_offsets());
        let ids = [color_ids::COLOR_RANGE_LIMITED, color_ids::MATRIX_BT709, color_ids::TRANSFER_BT709, 0];
        let g8 = nv12.convert_nv12_to_rgba(&y, &uv, w, h, w, w, w, h, 0, ids, pm);
        let g10 = p010.convert_p010_to_rgba(&yb, &uvb, w, h, w * 2, w * 2, w, h, 0, ids, pm);
        let (c8, c10) = (to8(&klipp_color::camera_log::yuv_to_display_rgba64(&s8, cs)), to8(&klipp_color::camera_log::yuv_to_display_rgba64(&s10, cs)));
        let (d8, d10) = (apart(&c8, &g8), apart(&c10, &g10));
        eprintln!("{name}: NV12 {d8} levels, P010 {d10} levels from the CPU");
        assert!(d8 <= 1 && d10 <= 1, "{name}: the shaders are {d8} (NV12) and {d10} (P010) levels from the CPU converter");
        match &at_left {
            None => at_left = Some(g8),
            Some(left) if name == "centre" => {
                let moved = left.chunks_exact(4).zip(g8.chunks_exact(4)).map(|(a, b)| (0..3).map(|c| (a[c] as i32 - b[c] as i32).abs()).max().unwrap()).max().unwrap();
                assert!(moved > 20, "centre siting drew what left does ({moved} levels apart)");
            }
            _ => {}
        }
    }
}
