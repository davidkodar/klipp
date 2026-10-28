//! The dither every 8-bit picture output adds before it rounds: the
//! export's CPU display transform (`klipp_color::display`) and the GPU's
//! display encodes (`src/gpu/shaders/include/display_dither.wgsl`), one
//! definition for both (#2072).
//!
//! The constants below are the whole pattern. The WGSL carries the same
//! literals, which a test holds equal to these, and the GPU's bytes are
//! checked against the CPU export's to the pixel, so the two routes cannot
//! drift into different noise.

/// The first multiplier of the per-pixel hash.
pub const DITHER_HASH_MUL_A: u32 = 0x7feb_352d;
/// The second multiplier of the per-pixel hash.
pub const DITHER_HASH_MUL_B: u32 = 0x846c_a68b;
/// What the second of the two uniform draws xors into the pixel index.
pub const DITHER_SECOND_DRAW: u32 = 0x9e37_79b9;

/// The pixel hash both draws use.
#[inline]
pub(crate) fn hash(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(DITHER_HASH_MUL_A);
    x ^= x >> 15;
    x = x.wrapping_mul(DITHER_HASH_MUL_B);
    x ^= x >> 16;
    x
}

/// A triangular dither for one pixel, in output code values, in
/// `[-1, 1]`.
///
/// This is the seam where a frame of real numbers becomes integers, and
/// rounding alone is not enough. A gradient that crosses few code values
/// across the frame — a dark backdrop, a soft vignette, a slow fade —
/// rounds to flat plateaus with hard steps between them, and the eye
/// reads the steps as bands. Nudging each pixel by a fraction of a code
/// value before the rounding turns the step into a scatter, which the
/// eye averages back to the value that was asked for.
///
/// Triangular rather than uniform because uniform noise leaves the
/// quantisation error correlated with the signal — the point of dither
/// is to decorrelate it. One offset for all three channels rather than
/// three: monochrome noise is far less visible than coloured noise at
/// the same amplitude, and alpha is not dithered at all, since nothing
/// looks at it and compositing wants it exact.
///
/// Deterministic in the pixel's index (`y * width + x`) and nothing else,
/// so an export is reproducible, a still picture does not crawl from
/// frame to frame, and tests can predict it.
#[inline]
pub fn dither_at(index: usize) -> f32 {
    let a = hash(index as u32) as f32 / u32::MAX as f32;
    let b = hash((index as u32) ^ DITHER_SECOND_DRAW) as f32 / u32::MAX as f32;
    a + b - 1.0
}

/// How near a code value, in code values, a channel counts as sitting on
/// it, and so is not dithered.
///
/// It has to absorb the float error of a picture that is already 8-bit
/// coming back through linear light: a decoded frame's code expanded to
/// linear and encoded again lands within 1.5e-5 of a code on the CPU and
/// within 1e-4 on the GPU (measured for #2072: the GPU's encode of every
/// code stays exact with the tolerance at 1e-4 and not at 2e-5; the
/// `an_8bit_picture_comes_back_exact_*` tests in `display_dither` hold
/// both routes to it), so a thousandth of a code is ten times the larger. It is also small enough to cost a gradient nothing:
/// one value in five hundred falls inside it and is rounded undithered,
/// which no eye could tell from the dither around it.
pub const ON_CODE_TOLERANCE: f32 = 1e-3;

/// One channel, dithered and quantised to a scale of `scale` code
/// values, as a float ready to be cast.
///
/// Values that already sit on a code value are left alone (within
/// [`ON_CODE_TOLERANCE`]). Pure black, pure white and a saturated primary
/// are exact, and dither can only make them wrong — white nudged down
/// comes back 254, which is a dulled highlight and, on a run of them, a
/// visible lift in the blacks at the other end. The same holds for every
/// code of a picture that was 8-bit to begin with: an ungraded clip, a
/// cached frame, a still. Dithering one would add noise to bytes that
/// were exact, and the routes that show such a picture without rounding
/// it again (the decoder's frame presented directly, the render cache's
/// table) would then disagree with the routes that do (#2072).
#[inline]
pub fn quantise(v: f32, dither: f32, scale: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    let x = v * scale;
    let on_code = (x - x.round()).abs() < ON_CODE_TOLERANCE;
    let d = if v <= 0.0 || v >= 1.0 || on_code { 0.0 } else { dither };
    (x + 0.5 + d).clamp(0.0, scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dither_is_triangular_and_centred() {
        let n = 1 << 18;
        let mut sum = 0.0f64;
        let mut inner = 0usize;
        for i in 0..n {
            let d = dither_at(i);
            assert!((-1.0..=1.0).contains(&d));
            sum += d as f64;
            inner += usize::from(d.abs() < 0.5);
        }
        assert!((sum / n as f64).abs() < 0.01, "mean {}", sum / n as f64);
        // A triangle on [-1, 1] holds 3/4 of its mass within ±0.5; a
        // uniform draw would hold half.
        let share = inner as f64 / n as f64;
        assert!((share - 0.75).abs() < 0.01, "share within ±0.5: {share}");
    }

    #[test]
    fn ends_of_the_range_are_not_dithered() {
        assert_eq!(quantise(0.0, 0.9, 255.0) as u8, 0);
        assert_eq!(quantise(1.0, -0.9, 255.0) as u8, 255);
        assert_eq!(quantise(0.5, 0.0, 255.0) as u8, 128);
    }

    #[test]
    fn a_value_on_a_code_is_not_dithered() {
        for code in 0..=255u32 {
            for err in [0.0f32, 4e-4, -4e-4] {
                let v = (code as f32 + err) / 255.0;
                for d in [-0.99f32, -0.6, 0.6, 0.99] {
                    assert_eq!(quantise(v, d, 255.0) as u32, code, "code {code}{err:+} took dither {d}");
                }
            }
        }
        // Between codes it is dithered.
        let v = 100.3 / 255.0;
        assert_eq!(quantise(v, 0.9, 255.0) as u32, 101);
        assert_eq!(quantise(v, -0.9, 255.0) as u32, 99);
    }
}
