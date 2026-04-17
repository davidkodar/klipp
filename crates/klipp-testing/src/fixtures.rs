//! Synthetic reference frames for the linear-light migration.
//!
//! Everything here is generated in code — no binary blobs in git.
//! Two benefits: fixtures are reproducible across every dev machine,
//! and the reader can see the exact source values without opening a
//! hex editor.
//!
//! Frame layout matches the engine: flat `Vec<f32>` of RGBA pixels in
//! row-major order, four floats per pixel, alpha in channel 3.

use crate::transfer::{linear_to_srgb_u8, srgb_u8_to_linear};

/// 18 % perceptual mid-grey as a linear f32 RGBA pixel.
/// This is the "18 % grey card" reference photographers use.
pub const MID_GREY_LINEAR: [f32; 4] = [0.18, 0.18, 0.18, 1.0];

/// 18 % mid-grey encoded to sRGB u8. Round-trips to [0.18, 0.18, 0.18]
/// through `srgb_to_linear` within quantisation noise.
pub const MID_GREY_SRGB_U8: [u8; 4] = [118, 118, 118, 255];

/// Fill a `width × height` RGBA frame with one solid linear colour.
pub fn solid_linear(width: u32, height: u32, colour: [f32; 4]) -> Vec<f32> {
    let n = (width * height) as usize;
    let mut out = Vec::with_capacity(n * 4);
    for _ in 0..n {
        out.extend_from_slice(&colour);
    }
    out
}

/// Fill a `width × height` RGBA frame with one solid sRGB colour
/// (u8 input, linear f32 output).
pub fn solid_srgb_u8(width: u32, height: u32, colour: [u8; 4]) -> Vec<f32> {
    solid_linear(width, height, srgb_u8_to_linear(colour))
}

/// Horizontal black-to-white ramp in linear light. R = G = B = x / (w-1).
/// Useful for testing monotonicity of point operations: any function
/// that preserves order must produce a monotonically non-decreasing
/// output.
pub fn linear_ramp(width: u32, height: u32) -> Vec<f32> {
    let mut out = Vec::with_capacity((width * height * 4) as usize);
    let w = width.max(1) as f32;
    for _ in 0..height {
        for x in 0..width {
            let v = x as f32 / (w - 1.0).max(1.0);
            out.extend_from_slice(&[v, v, v, 1.0]);
        }
    }
    out
}

/// 24-patch X-Rite ColorChecker classic values, in sRGB u8 as
/// published by X-Rite. Returned as linear f32 RGBA via the sRGB
/// decode. Order is left-to-right, top-to-bottom: skin rows first,
/// primaries / secondaries, then the grey ramp.
pub fn color_checker_24_linear() -> [[f32; 4]; 24] {
    const SRGB_U8: [[u8; 3]; 24] = [
        // Row 1: natural colours
        [115, 82, 68],   // dark skin
        [194, 150, 130], // light skin
        [98, 122, 157],  // blue sky
        [87, 108, 67],   // foliage
        [133, 128, 177], // blue flower
        [103, 189, 170], // bluish green
        // Row 2: miscellaneous
        [214, 126, 44],  // orange
        [80, 91, 166],   // purplish blue
        [193, 90, 99],   // moderate red
        [94, 60, 108],   // purple
        [157, 188, 64],  // yellow green
        [224, 163, 46],  // orange yellow
        // Row 3: primaries / secondaries
        [56, 61, 150],   // blue
        [70, 148, 73],   // green
        [175, 54, 60],   // red
        [231, 199, 31],  // yellow
        [187, 86, 149],  // magenta
        [8, 133, 161],   // cyan
        // Row 4: neutrals (greyscale)
        [243, 243, 242], // white (0.94)
        [200, 200, 200], // neutral 8
        [160, 160, 160], // neutral 6.5
        [122, 122, 121], // neutral 5
        [85, 85, 85],    // neutral 3.5
        [52, 52, 52],    // black 2
    ];
    let mut out = [[0.0f32; 4]; 24];
    for (i, srgb) in SRGB_U8.iter().enumerate() {
        out[i] = srgb_u8_to_linear([srgb[0], srgb[1], srgb[2], 255]);
    }
    out
}

/// Encode a linear RGBA frame to sRGB u8 (for comparison against the
/// current u8 pipeline's outputs).
pub fn encode_linear_to_srgb_u8(linear: &[f32]) -> Vec<u8> {
    assert!(linear.len() % 4 == 0, "linear buffer must be RGBA-packed");
    let mut out = Vec::with_capacity(linear.len());
    for chunk in linear.chunks_exact(4) {
        let px = linear_to_srgb_u8([chunk[0], chunk[1], chunk[2], chunk[3]]);
        out.extend_from_slice(&px);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solid_fixture_has_correct_pixel_count() {
        let buf = solid_linear(4, 3, MID_GREY_LINEAR);
        assert_eq!(buf.len(), 4 * 3 * 4);
        for chunk in buf.chunks_exact(4) {
            assert_eq!(chunk, &MID_GREY_LINEAR[..]);
        }
    }

    #[test]
    fn ramp_is_monotonic_and_bounded() {
        let buf = linear_ramp(256, 1);
        // Every red channel must be >= the previous one.
        let reds: Vec<f32> = buf.chunks_exact(4).map(|c| c[0]).collect();
        for pair in reds.windows(2) {
            assert!(pair[1] >= pair[0]);
        }
        assert_eq!(reds.first().copied(), Some(0.0));
        assert_eq!(reds.last().copied(), Some(1.0));
    }

    #[test]
    fn color_checker_neutral_ramp_is_near_grey() {
        // The last six patches (indices 18..24) are the neutral
        // ramp. R, G and B should be approximately equal in each.
        for (i, patch) in color_checker_24_linear()[18..].iter().enumerate() {
            let max = patch[0].max(patch[1]).max(patch[2]);
            let min = patch[0].min(patch[1]).min(patch[2]);
            assert!(
                (max - min) < 0.01,
                "neutral patch {i} has RGB drift > 0.01: {patch:?}",
            );
        }
    }

    #[test]
    fn mid_grey_srgb_decodes_to_18_percent_linear() {
        // A published ColorChecker "neutral 5" patch is ~0.18 linear;
        // our MID_GREY_SRGB_U8 constant is the canonical sRGB for it.
        let px = srgb_u8_to_linear(MID_GREY_SRGB_U8);
        for i in 0..3 {
            assert!(
                (px[i] - 0.18).abs() < 0.005,
                "channel {i}: expected ~0.18, got {}",
                px[i],
            );
        }
    }

    #[test]
    fn color_checker_exposes_all_24_patches() {
        let patches = color_checker_24_linear();
        assert_eq!(patches.len(), 24);
        // Spot check three patches with published sRGB values:
        //   #0  "dark skin"   sRGB (115, 82, 68)    → ~0.179, 0.090, 0.063 linear
        //   #14 "red"         sRGB (175, 54, 60)    → ~0.421, 0.037, 0.046 linear
        //   #18 "white 9.5"   sRGB (243, 243, 242)  → ~0.88 linear
        assert!((patches[0][0] - 0.179).abs() < 0.01);
        assert!((patches[14][0] - 0.421).abs() < 0.01);
        assert!(patches[18][0] > 0.87 && patches[18][0] < 0.90);
        // Alpha must always be 1.0.
        for (i, p) in patches.iter().enumerate() {
            assert_eq!(p[3], 1.0, "patch {i} alpha = {}", p[3]);
        }
    }

    #[test]
    fn encode_then_decode_round_trips_within_one_code_value() {
        // Round-trip quantisation check.
        let linear = color_checker_24_linear();
        for patch in linear.iter() {
            let u8_px = linear_to_srgb_u8(*patch);
            let back = srgb_u8_to_linear(u8_px);
            for i in 0..3 {
                assert!(
                    (patch[i] - back[i]).abs() < 1.0 / 255.0 + 1e-3,
                    "channel {i}: orig={} round={}",
                    patch[i],
                    back[i],
                );
            }
        }
    }
}
