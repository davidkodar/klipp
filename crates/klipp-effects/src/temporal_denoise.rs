//! Temporal noise reduction: averaging a pixel with itself, over time.
//!
//! Grain is uncorrelated between frames and the picture is not. Average
//! the same pixel across neighbouring frames and the grain cancels
//! while the detail stays — which is why temporal denoising can clean
//! low-light footage without the waxy smearing a spatial filter leaves.
//! A spatial filter cannot tell grain from detail, because within one
//! frame they look the same; it can only decide a pixel is unlike its
//! neighbours and pull it towards them.
//!
//! The catch is motion. Averaging frame N with N±1 at the same pixel is
//! only valid where nothing moved; everywhere else it ghosts, which is
//! worse than the grain. So every neighbour is weighted by how much
//! that pixel changed: unchanged pixels average fully, moved ones keep
//! their own value. That is the whole of it.
//!
//! This is the un-motion-compensated form — stage two of three in the
//! issue. It does nothing for a pixel that moved, so on a handheld
//! shot it cleans the still parts and leaves the moving ones. On a
//! tripod, a locked-off interview or a static product shot, which is
//! much of the footage this matters for, it cleans everything.
//! Estimating motion and following a pixel to where it went is the
//! stage after this one.
//!
//! Runs on the **source**, before the grade, because that is where the
//! noise is and because the neighbouring frames a denoiser reads are
//! source frames. Grading first would average a graded picture against
//! ungraded ones. Resolve and Premiere place theirs the same way.

use rayon::prelude::*;

/// Below this the guard is off and every neighbour averages fully,
/// which is a frame blend rather than a denoise.
pub const MIN_THRESHOLD: f32 = 1e-4;

/// The gaussian's denominator for a `threshold`: `2 · t²`, the threshold
/// floored at [`MIN_THRESHOLD`] first. Shared with the GPU kernel.
pub fn denominator(threshold: f32) -> f32 {
    let t = threshold.max(MIN_THRESHOLD);
    2.0 * t * t
}

/// The luma of a linear RGB pixel, with the working space's weights
/// (`klipp_color::linear::luma_coefs`), as the GPU kernel takes them
/// (#2019): BT.709's in every space weighed an ACEScg or Rec.2020
/// picture's brightness by the wrong primaries.
///
/// The guard compares brightness rather than each channel: chroma noise
/// moves the channels apart without moving the brightness much, and
/// gating on that would refuse to clean exactly the noise this is best
/// at removing.
#[inline]
fn luma(p: &[f32], w: [f32; 3]) -> f32 {
    w[0] * p[0] + w[1] * p[1] + w[2] * p[2]
}

/// One pixel's temporal average, given its neighbours' values.
///
/// Separated out because it is the whole algorithm, and a shader and a
/// parallel loop should not each carry their own copy of it.
#[inline]
pub fn blend_pixel(cur: [f32; 3], neighbours: &[[f32; 3]], threshold: f32, strength: f32, luma_weights: [f32; 3]) -> [f32; 3] {
    let cur_luma = luma(&cur, luma_weights);
    let denom = 2.0 * threshold * threshold;
    let mut sum = cur;
    let mut weight = 1.0f32;
    for n in neighbours {
        let n_luma = luma(n, luma_weights);
        let d = cur_luma - n_luma;
        // A gaussian on the brightness difference: identical pixels
        // weigh 1, and the weight falls away as the pixel changes, so
        // there is no threshold to sit exactly on and flicker across.
        let w = (-(d * d) / denom).exp();
        for c in 0..3 {
            sum[c] += n[c] * w;
        }
        weight += w;
    }
    let inv = 1.0 / weight;
    let mut out = [0.0f32; 3];
    for c in 0..3 {
        let averaged = sum[c] * inv;
        out[c] = cur[c] + (averaged - cur[c]) * strength;
    }
    out
}

/// The source frames the denoiser reads for one frame: the one either
/// side of it.
pub const NEIGHBOURS: usize = 2;

/// Denoise `current` against its neighbours, all linear RGBA f32 of the
/// same size.
///
/// Neighbours of the wrong length are ignored rather than refused: a
/// clip's first and last frames have one side and no other, and that is
/// normal rather than an error.
///
/// `threshold` is how much a pixel may change and still be treated as
/// the same pixel — in linear light, so it is a fraction of the light
/// rather than of a code. `strength` mixes the result back. `luma_weights`
/// are the working space's, `klipp_color::linear::luma_coefs`.
pub fn denoise_frame(
    current: &[f32],
    neighbours: &[&[f32]],
    threshold: f32,
    strength: f32,
    luma_weights: [f32; 3],
) -> Vec<f32> {
    let strength = strength.clamp(0.0, 1.0);
    if strength <= 0.0 || neighbours.is_empty() {
        return current.to_vec();
    }
    let usable: Vec<&&[f32]> = neighbours.iter().filter(|n| n.len() == current.len()).collect();
    if usable.is_empty() {
        return current.to_vec();
    }
    let denom = denominator(threshold);

    let mut out = current.to_vec();
    out.par_chunks_exact_mut(4).enumerate().for_each(|(i, px)| {
        let base = i * 4;
        let cur_luma = luma(px, luma_weights);
        let mut sum = [px[0], px[1], px[2]];
        let mut weight = 1.0f32;
        for n in &usable {
            let np = &n[base..base + 3];
            let n_luma = luma(np, luma_weights);
            let d = cur_luma - n_luma;
            let w = (-(d * d) / denom).exp();
            for c in 0..3 {
                sum[c] += np[c] * w;
            }
            weight += w;
        }
        let inv = 1.0 / weight;
        for c in 0..3 {
            let averaged = sum[c] * inv;
            px[c] += (averaged - px[c]) * strength;
        }
        // Alpha is untouched: the pixel was noisy, not transparent.
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// BT.709's weights, the Rec.709 working space's.
    const R709: [f32; 3] = [0.2126, 0.7152, 0.0722];

    const W: usize = 64;
    const H: usize = 48;
    const N: usize = W * H;

    /// A deterministic pseudo-random noise field, so a test failure is
    /// reproducible rather than occasionally unlucky.
    fn noise(seed: u32, amount: f32) -> Vec<f32> {
        let mut x = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
        let mut v = Vec::with_capacity(N);
        for _ in 0..N {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            v.push(((x >> 8) as f32 / 8_388_608.0 - 1.0) * amount);
        }
        v
    }

    /// A flat grey frame plus a given noise field.
    fn frame_with(base: f32, n: &[f32]) -> Vec<f32> {
        let mut v = Vec::with_capacity(N * 4);
        for i in 0..N {
            let s = (base + n[i]).max(0.0);
            v.extend_from_slice(&[s, s, s, 1.0]);
        }
        v
    }

    /// Standard deviation of the luma, which is what "how noisy" means.
    fn sigma(frame: &[f32]) -> f32 {
        let l: Vec<f32> = frame.chunks_exact(4).map(|p| luma(p, R709)).collect();
        let mean = l.iter().sum::<f32>() / l.len() as f32;
        (l.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / l.len() as f32).sqrt()
    }

    /// The thing it is for: three noisy frames of the same still
    /// picture come back measurably cleaner.
    ///
    /// Averaging three independent samples cuts the deviation by about
    /// √3, so a bit over 0.55× is the theoretical floor and anything
    /// near it is the filter working as intended.
    #[test]
    fn noise_on_a_still_picture_falls() {
        let base = 0.18;
        let cur = frame_with(base, &noise(1, 0.02));
        let prev = frame_with(base, &noise(2, 0.02));
        let next = frame_with(base, &noise(3, 0.02));

        let before = sigma(&cur);
        // A threshold well above the noise, so the guard lets it work.
        let out = denoise_frame(&cur, &[&prev, &next], 0.1, 1.0, R709);
        let after = sigma(&out);
        eprintln!("noise sigma: {before:.5} -> {after:.5} ({:.2}x)", after / before);
        assert!(after < before * 0.7, "noise barely fell: {before} -> {after}");
    }

    /// And the thing it must not do. A pixel that changed is motion,
    /// and averaging it with where it used to be is a ghost.
    #[test]
    fn a_moved_pixel_keeps_its_own_value() {
        // Current: a bright patch on the left. Neighbours: the same
        // patch on the right, as if it moved across.
        let mut cur = vec![0.05f32; N * 4];
        let mut other = vec![0.05f32; N * 4];
        for y in 10..30 {
            for x in 5..20 {
                let i = (y * W + x) * 4;
                cur[i] = 0.9;
                cur[i + 1] = 0.9;
                cur[i + 2] = 0.9;
            }
            for x in 40..55 {
                let i = (y * W + x) * 4;
                other[i] = 0.9;
                other[i + 1] = 0.9;
                other[i + 2] = 0.9;
            }
        }
        for px in cur.chunks_exact_mut(4) {
            px[3] = 1.0;
        }
        for px in other.chunks_exact_mut(4) {
            px[3] = 1.0;
        }

        // A threshold sized for grain, not for a 0.85 jump.
        let out = denoise_frame(&cur, &[&other, &other], 0.02, 1.0, R709);

        // The patch where it actually is stays bright.
        let on = (20 * W + 10) * 4;
        assert!(out[on] > 0.85, "the subject was dimmed by its own past: {}", out[on]);
        // And no ghost appears where it used to be.
        let ghost = (20 * W + 47) * 4;
        assert!(
            out[ghost] < 0.10,
            "a ghost of the moved patch appeared: {}",
            out[ghost],
        );
    }

    /// With the guard wide open it is a frame blend, which is what
    /// makes the guard the point rather than a refinement.
    #[test]
    fn a_wide_guard_is_just_an_average() {
        let cur = [0.0f32, 0.0, 0.0, 1.0].repeat(N);
        let other = [1.0f32, 1.0, 1.0, 1.0].repeat(N);
        let out = denoise_frame(&cur, &[&other], 1e6, 1.0, R709);
        assert!((out[0] - 0.5).abs() < 1e-3, "expected the midpoint, got {}", out[0]);
        // Alpha is not part of the average.
        assert_eq!(out[3], 1.0);
    }

    #[test]
    fn strength_and_absent_neighbours_are_no_ops() {
        let cur = frame_with(0.18, &noise(7, 0.02));
        let other = frame_with(0.18, &noise(8, 0.02));
        assert_eq!(denoise_frame(&cur, &[&other], 0.1, 0.0, R709), cur, "strength 0");
        assert_eq!(denoise_frame(&cur, &[], 0.1, 1.0, R709), cur, "no neighbours");
        // A clip's first frame has no `prev`; a wrong-sized neighbour
        // is ignored rather than refused.
        let short = [0.0f32; 8];
        assert_eq!(denoise_frame(&cur, &[&short[..]], 0.1, 1.0, R709), cur, "wrong size");
    }

    /// Half strength is half the way to the averaged value.
    #[test]
    fn strength_mixes_the_result_back() {
        let cur = [0.0f32, 0.0, 0.0, 1.0].repeat(N);
        let other = [1.0f32, 1.0, 1.0, 1.0].repeat(N);
        let full = denoise_frame(&cur, &[&other], 1e6, 1.0, R709);
        let half = denoise_frame(&cur, &[&other], 1e6, 0.5, R709);
        assert!((half[0] - full[0] * 0.5).abs() < 1e-4, "got {}", half[0]);
    }

    /// The per-pixel helper the shader mirrors has to agree with the
    /// loop, or the twin is a different filter.
    #[test]
    fn the_pixel_helper_matches_the_frame_loop() {
        let cur = frame_with(0.18, &noise(11, 0.03));
        let a = frame_with(0.18, &noise(12, 0.03));
        let b = frame_with(0.18, &noise(13, 0.03));
        let out = denoise_frame(&cur, &[&a, &b], 0.05, 0.8, R709);
        for i in (0..N).step_by(37) {
            let p = i * 4;
            let want = blend_pixel(
                [cur[p], cur[p + 1], cur[p + 2]],
                &[[a[p], a[p + 1], a[p + 2]], [b[p], b[p + 1], b[p + 2]]],
                0.05,
                0.8,
                R709,
            );
            for c in 0..3 {
                assert!(
                    (out[p + c] - want[c]).abs() < 1e-6,
                    "pixel {i} channel {c}: {} vs {}",
                    out[p + c],
                    want[c],
                );
            }
        }
    }

    /// #2019: the guard weighs brightness by the working space's
    /// primaries. Two pixels BT.709 calls the same brightness differ in
    /// ACEScg's, so a neighbour that averages fully in Rec.709 is held
    /// back in ACEScg.
    #[test]
    fn a_wide_space_weighs_brightness_by_its_own_primaries() {
        let aces = klipp_color::linear::luma_coefs(klipp_core::WorkingColorspace::AcesCG);
        // Equal luma in BT.709: more red, less green and blue.
        let cur = [0.5f32, 0.5, 0.5];
        let dr = 0.2;
        let n = [0.5 + dr, 0.5 - dr * R709[0] / (R709[1] + R709[2]), 0.5 - dr * R709[0] / (R709[1] + R709[2])];
        assert!((luma(&cur, R709) - luma(&n, R709)).abs() < 1e-6, "the same brightness in BT.709");
        let rec709 = blend_pixel(cur, &[n], 0.01, 1.0, R709);
        let acescg = blend_pixel(cur, &[n], 0.01, 1.0, aces);
        // BT.709: the neighbour averages fully, half of it.
        assert!((rec709[0] - (cur[0] + n[0]) / 2.0).abs() < 1e-4, "{rec709:?}");
        // ACEScg sees it brighter, and keeps the pixel nearer its own.
        assert!((acescg[0] - cur[0]).abs() < (rec709[0] - cur[0]).abs() - 0.01, "ACEScg {acescg:?} vs Rec.709 {rec709:?}");
    }
}
