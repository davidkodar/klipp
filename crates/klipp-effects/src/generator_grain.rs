//! The fine grain the smooth generators (the two-point, four-point and
//! living gradients, the living gradient's light included) carry at
//! their source, so a dark, soft background stays smooth after an 8-bit
//! H.264 or HEVC encode (#2073).
//!
//! The display dither (`crate::dither`) is enough for the picture that
//! leaves the renderer, and not for the file a viewer watches: an 8-bit
//! encoder at a delivery bitrate smooths sub-code noise out of dark, flat
//! areas, and the steps the dither had broken up come back.
//!
//! The grain is fixed in place: the same pixel takes the same grain on
//! every frame. Grain that changes every frame is the other choice, and
//! it does not work: the encoder spends nothing on noise no later frame
//! can reuse, and smooths it away. Modelled for #2073 (the living
//! gradient below, dithered to 8 bits, x264 at CRF 23), grain new every
//! frame left the widest flat run at 304–416 px for every depth up to 2
//! code values, no better than none (333–448 px), while the same grain
//! held still brought it to 8–18 px at 2. Holding still is also what
//! keeps a still frame still.
//!
//! The constants below are the whole pattern. The WGSL
//! (`src/gpu/shaders/include/generator_grain.wgsl`) carries the same
//! literals, which a test holds equal to these, and the GPU's living
//! gradient is checked against the CPU's, so the two routes give one
//! picture.

/// The default grain, in 8-bit sRGB code values of standard deviation.
///
/// Chosen through the real export (`gradient_grain_export` in
/// klipp-render): the "A Little Space" living gradient (12,23,32 to
/// 29,60,65, light 0.18) at 1080p, 8-bit H.264, CRF 23, medium, decoded
/// and measured on the luma rows. The encoder keeps grain only above a
/// threshold near 2 code values; below it the file is as banded as with
/// none.
///
/// | grain | widest flat run | levels a row | texture left (luma std) |
/// |---|---|---|---|
/// | 0 | 512 px | 26 | 0.13 |
/// | 1.5 | 386 px | 26 | 0.26 |
/// | 2 | 64 px | 30 | 0.66 |
/// | 2.5 | 19 px | 34 | 1.31 |
/// | 3 | 13 px | 37 | 1.90 |
///
/// 2.5 clears the threshold with room for a harder encode, and what it
/// leaves in the file is a code value and a bit of fine texture.
pub const DEFAULT_GENERATOR_GRAIN: f32 = 2.5;

/// The most grain a generator takes, in code values of standard
/// deviation: past this it is a texture, which the Grain generator is for.
pub const MAX_GENERATOR_GRAIN: f32 = 8.0;

/// What each of the three uniform draws xors into the pixel index. None
/// is the display dither's (`DITHER_SECOND_DRAW`, or no salt), so the
/// grain and the dither are independent.
pub const GRAIN_SALTS: [u32; 3] = [0x85eb_ca6b, 0xc2b2_ae35, 0x27d4_eb2f];

/// How many code values from 0 or 255 the grain takes to reach its full
/// depth. Black stays black and white stays white: grain there could
/// only lift the black or dull the white, as the clamp keeps half of it.
pub const GRAIN_TAPER_CODES: f32 = 4.0;

/// The grain at a pixel, before it is scaled: near-Gaussian, mean 0,
/// standard deviation 1, within ±3.
///
/// Three uniform draws summed (Irwin–Hall), which is a bell and not a
/// box: it reads as film grain, where a uniform draw at the same depth
/// reads as a speckle with a hard edge. Monochrome, one value for all
/// three channels, as film grain is. From the pixel's index
/// (`y * width + x`) alone.
#[inline]
pub fn grain_noise(index: u32) -> f32 {
    // The hash's top 24 bits, which an f32 holds exactly: converting all
    // 32 rounds, and a GPU's rounding there is its driver's, not ours, so
    // the two routes' grain could differ.
    let draw = |salt: u32| (crate::dither::hash(index ^ salt) >> 8) as f32 / 16_777_216.0;
    (draw(GRAIN_SALTS[0]) + draw(GRAIN_SALTS[1]) + draw(GRAIN_SALTS[2]) - 1.5) * 2.0
}

/// One sRGB-encoded channel `v` (0..1) with the grain `noise` (from
/// [`grain_noise`]) at `std_codes` code values of depth, tapered off
/// toward 0 and 1 ([`GRAIN_TAPER_CODES`]).
#[inline]
pub fn grain_channel(v: f32, noise: f32, std_codes: f32) -> f32 {
    let taper = (v.min(1.0 - v) * (255.0 / GRAIN_TAPER_CODES)).clamp(0.0, 1.0);
    (v + noise * std_codes * taper / 255.0).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grain_is_a_centred_bell_of_unit_depth() {
        let n = 1u32 << 18;
        let (mut sum, mut sq, mut inner) = (0.0f64, 0.0f64, 0usize);
        for i in 0..n {
            let g = grain_noise(i);
            assert!((-3.0..=3.0).contains(&g));
            sum += g as f64;
            sq += (g as f64) * (g as f64);
            inner += usize::from(g.abs() < 1.0);
        }
        let mean = sum / n as f64;
        let std = (sq / n as f64 - mean * mean).sqrt();
        assert!(mean.abs() < 0.01, "mean {mean}");
        assert!((std - 1.0).abs() < 0.01, "std {std}");
        // A Gaussian holds 68% within one deviation, a uniform draw of
        // the same deviation 58%; Irwin–Hall of three lands near 0.67.
        let share = inner as f64 / n as f64;
        assert!((0.65..0.70).contains(&share), "share within one deviation: {share}");
    }

    #[test]
    fn the_grain_is_not_the_dither() {
        let n = 1u32 << 16;
        let (mut gd, mut gg, mut dd) = (0.0f64, 0.0f64, 0.0f64);
        for i in 0..n {
            let (g, d) = (grain_noise(i) as f64, crate::dither::dither_at(i as usize) as f64);
            gd += g * d;
            gg += g * g;
            dd += d * d;
        }
        let r = gd / (gg * dd).sqrt();
        assert!(r.abs() < 0.02, "grain and dither correlate: {r}");
    }

    #[test]
    fn black_and_white_take_no_grain() {
        for g in [-3.0f32, -1.0, 1.0, 3.0] {
            assert_eq!(grain_channel(0.0, g, MAX_GENERATOR_GRAIN), 0.0);
            assert_eq!(grain_channel(1.0, g, MAX_GENERATOR_GRAIN), 1.0);
            assert_eq!(grain_channel(0.3, g, 0.0), 0.3);
        }
        // Full depth from four codes in.
        let v = 20.0 / 255.0;
        assert!((grain_channel(v, 1.0, 2.0) - (22.0 / 255.0)).abs() < 1e-6);
    }
}
