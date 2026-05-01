//! GPU parity check for the working-space refactor: the same P010
//! source pushed through `convert_p010_to_rgba` twice — once with the
//! BT.709-targeted primaries matrix, once with the ACEScg-targeted
//! one — must produce sRGB-encoded outputs that obey the working-
//! space relationship in linear light:
//!
//!     linear(acescg_output) ≈ bt709_to_acescg() · linear(bt709_output)
//!
//! within ~2 LSB at 8-bit (the sRGB encode + quantise on each side
//! eats some precision; tolerance accounts for that).
//!
//! Establishes that the matrix-uniform refactor in `nv12_gpu` /
//! `p010_gpu` actually consumes the matrix value the engine packs at
//! the call site — a regression here would mean the GPU silently
//! ignored the working-colorspace setting.
//!
//! We deliberately use BT.709 SDR source data, not BT.2020+PQ:
//!   * No HDR tone-map — the shader's tone-map clips to `[0, 1]`,
//!     which would silently break the matrix-relationship test on the
//!     side that lost information.
//!   * BT.709 source values stay in `[0, 1]` for both working spaces
//!     (AP1 strictly contains BT.709), so sRGB encode + quantise on
//!     each side stays bit-faithful.

use klipp_color::camera_log::primaries_to_working_matrix;
use klipp_color::primaries::{bt709_to_acescg, matrix3_mul_vec};
use klipp_color::transfer::srgb_to_linear;
use klipp_core::{ColorPrimaries, WorkingColorspace};
use klipp_effects::color_wgsl::pack_primaries_matrix;
use klipp_effects::p010_gpu::GpuP010Converter;

const W: u32 = 32;
const H: u32 = 32;

/// Build a P010 frame with limited-range BT.709 SDR content — Y
/// across the full limited range (~0.5 luma on average), neutral-ish
/// chroma with a sweep. Stays in-gamut for both BT.709 and ACEScg.
fn make_bt709_sdr_p010(width: u32, height: u32) -> (Vec<u8>, Vec<u8>) {
    let mut y = Vec::with_capacity((width * height * 2) as usize);
    for row in 0..height {
        for col in 0..width {
            // Luma sweep across the limited 64..940 range.
            let v: u32 = 64 + ((col * 31 + row * 13) % 877);
            let packed: u16 = ((v & 0x3FF) << 6) as u16;
            y.extend_from_slice(&packed.to_le_bytes());
        }
    }
    let mut uv = Vec::with_capacity((width * height) as usize);
    for row in 0..(height / 2) {
        for col in 0..(width / 2) {
            // Modest chroma sweep around 512 (neutral) in a way that
            // produces a mix of slight casts — enough non-grey colour
            // that the matrix actually does work.
            let u: u32 = 512 + (((col as i32 - row as i32) * 30).rem_euclid(400)) as u32 - 200;
            let v: u32 = 512 + (((col as i32 + row as i32) * 30).rem_euclid(400)) as u32 - 200;
            let pu: u16 = ((u & 0x3FF) << 6) as u16;
            let pv: u16 = ((v & 0x3FF) << 6) as u16;
            uv.extend_from_slice(&pu.to_le_bytes());
            uv.extend_from_slice(&pv.to_le_bytes());
        }
    }
    (y, uv)
}

#[test]
fn gpu_acescg_output_relates_to_gpu_bt709_output_via_matrix() {
    let Some(mut conv) = GpuP010Converter::new() else {
        eprintln!("skipping: P010 GPU converter unavailable on this system");
        return;
    };

    let (y, uv) = make_bt709_sdr_p010(W, H);
    let y_stride = W * 2;
    let uv_stride = W * 2;

    // [LIMITED, BT.709 matrix, BT.709 transfer, BT.709 primaries] = all 0u.
    let color_ids = [0u32, 0, 0, 0];

    // Path A: ws = Rec709Linear → matrix = BT.709 → BT.709 (identity).
    let m_to_709 = pack_primaries_matrix(primaries_to_working_matrix(
        ColorPrimaries::Bt709,
        WorkingColorspace::Rec709Linear,
    ));
    let bt709_bytes = conv.convert_p010_to_rgba(
        &y, &uv, W, H, y_stride, uv_stride, W, H, 0,
        color_ids, m_to_709,
    );

    // Path B: ws = AcesCG → matrix = BT.709 → AP1.
    let m_to_acescg = pack_primaries_matrix(primaries_to_working_matrix(
        ColorPrimaries::Bt709,
        WorkingColorspace::AcesCG,
    ));
    let acescg_bytes = conv.convert_p010_to_rgba(
        &y, &uv, W, H, y_stride, uv_stride, W, H, 0,
        color_ids, m_to_acescg,
    );

    assert_eq!(
        bt709_bytes.len(),
        acescg_bytes.len(),
        "byte-length mismatch — one path returned a different shape",
    );

    // Decode both sRGB u8 → linear, push the BT.709 linear values
    // through bt709_to_acescg, compare against the AcesCG path's
    // linear values.
    let to_acescg = bt709_to_acescg();
    let n_pixels = bt709_bytes.len() / 4;
    let mut max_abs = 0.0_f32;
    let mut over_tol_count = 0usize;
    // 2 LSB at 8-bit = 2/255 ≈ 0.0078 normalised, but the sRGB EOTF
    // is non-linear so the linear-domain delta varies with brightness.
    // 0.015 tolerance covers up to ~3 LSB at peak — comfortable
    // headroom for the encode/quantise on both sides.
    let tol_linear = 0.015;
    // Bytes that go through the shader's `clamp(0, 1)` on at least one
    // side break the relationship by definition. The BT.709 SDR
    // fixture is designed to stay in-gamut, but synthesised YUV with
    // arbitrary chroma can still produce a few rogue pixels — allow a
    // tiny escape hatch.
    let max_pct_over_tol = 1.0;
    for i in 0..n_pixels {
        let o = i * 4;
        let bt709_lin = [
            srgb_to_linear(bt709_bytes[o] as f32 / 255.0),
            srgb_to_linear(bt709_bytes[o + 1] as f32 / 255.0),
            srgb_to_linear(bt709_bytes[o + 2] as f32 / 255.0),
        ];
        let acescg_lin = [
            srgb_to_linear(acescg_bytes[o] as f32 / 255.0),
            srgb_to_linear(acescg_bytes[o + 1] as f32 / 255.0),
            srgb_to_linear(acescg_bytes[o + 2] as f32 / 255.0),
        ];
        let expected = matrix3_mul_vec(to_acescg, bt709_lin);
        for k in 0..3 {
            // Skip pixels where either side hit the clamp endpoint —
            // those carry no information about the matrix (clipping
            // erases the relationship by construction).
            let saturated = bt709_bytes[o + k] == 255 || acescg_bytes[o + k] == 255
                || bt709_bytes[o + k] == 0   || acescg_bytes[o + k] == 0;
            if saturated { continue; }
            let d = (acescg_lin[k] - expected[k]).abs();
            if d > max_abs { max_abs = d; }
            if d > tol_linear {
                over_tol_count += 1;
            }
        }
    }
    let total_channels = (n_pixels * 3) as f32;
    let over_pct = over_tol_count as f32 / total_channels * 100.0;
    assert!(
        over_pct < max_pct_over_tol,
        "{over_pct:.2}% of channels exceeded {tol_linear:.4} linear (max abs delta {max_abs:.4}) — \
         the GPU's primaries_matrix uniform is probably ignored or computed wrong",
    );
}
