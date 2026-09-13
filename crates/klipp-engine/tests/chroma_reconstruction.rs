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
