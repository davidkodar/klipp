//! Image-comparison helpers for the linear-light migration.
//!
//! Two modes:
//! * **Exact** — per-pixel equality within a tolerance. Use for point
//!   operations (exposure, contrast, LUTs) where the output value is
//!   determined exactly by the input value. A one-pixel mismatch is
//!   a real bug.
//! * **PSNR** — peak signal-to-noise ratio. Use for spatial operations
//!   (blur, bloom, denoise, sharpen) where minor numerical drift is
//!   unavoidable. 50 dB is the conventional "visually identical"
//!   threshold; 60 dB is "identical to integration noise."

/// First per-channel mismatch between two linear-f32 RGBA buffers.
/// `None` ⇒ buffers match within `tol` on every channel.
/// Returns `(pixel_index, channel_index, |a - b|)` for the first
/// channel whose absolute difference exceeds `tol`.
pub fn first_exact_mismatch(
    a: &[f32],
    b: &[f32],
    tol: f32,
) -> Option<(usize, usize, f32)> {
    assert_eq!(a.len(), b.len(), "image buffers must be the same size");
    assert!(a.len() % 4 == 0, "buffers must be RGBA-packed");
    for (px_idx, (pa, pb)) in a.chunks_exact(4).zip(b.chunks_exact(4)).enumerate() {
        for ch in 0..4 {
            let diff = (pa[ch] - pb[ch]).abs();
            if diff > tol {
                return Some((px_idx, ch, diff));
            }
        }
    }
    None
}

/// Peak signal-to-noise ratio of `candidate` against `reference`, in
/// decibels. Higher is better; the standard "perceptually identical"
/// threshold is ~50 dB for 8-bit content. Returns `f32::INFINITY`
/// when the buffers are bit-identical.
///
/// The reference peak is assumed to be 1.0 (we're working in linear
/// `[0, 1]`-ish space). Pixels above 1.0 are tolerated — they add to
/// MSE but don't push the peak, which is the physically right
/// behaviour for HDR content.
pub fn psnr(reference: &[f32], candidate: &[f32]) -> f32 {
    assert_eq!(reference.len(), candidate.len());
    let mut sum_sq = 0.0f64;
    for (r, c) in reference.iter().zip(candidate.iter()) {
        let d = (*r - *c) as f64;
        sum_sq += d * d;
    }
    if sum_sq == 0.0 {
        return f32::INFINITY;
    }
    let mse = sum_sq / reference.len() as f64;
    (10.0 * (1.0f64 / mse).log10()) as f32
}

/// Convenience assertion: fail with a readable diagnostic when two
/// buffers deviate by more than `tol` on any channel.
#[track_caller]
pub fn assert_exact(reference: &[f32], candidate: &[f32], tol: f32) {
    if let Some((px, ch, diff)) = first_exact_mismatch(reference, candidate, tol) {
        let r = &reference[px * 4..px * 4 + 4];
        let c = &candidate[px * 4..px * 4 + 4];
        panic!(
            "pixel {px} channel {ch} differs by {diff} (tol {tol}): \
             reference={r:?}, candidate={c:?}",
        );
    }
}

/// Convenience assertion: fail when `candidate`'s PSNR against
/// `reference` falls below `min_db`.
#[track_caller]
pub fn assert_psnr_at_least(
    reference: &[f32],
    candidate: &[f32],
    min_db: f32,
) {
    let got = psnr(reference, candidate);
    assert!(
        got >= min_db,
        "PSNR {got:.2} dB is below the {min_db:.2} dB threshold",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_buffers_have_infinite_psnr() {
        let a = vec![0.1_f32, 0.2, 0.3, 1.0, 0.4, 0.5, 0.6, 1.0];
        assert_eq!(psnr(&a, &a), f32::INFINITY);
    }

    #[test]
    fn exact_match_finds_no_mismatch() {
        let a = vec![0.5_f32; 16];
        let b = vec![0.5_f32; 16];
        assert_eq!(first_exact_mismatch(&a, &b, 0.0), None);
    }

    #[test]
    fn exact_match_pinpoints_first_bad_channel() {
        let a = vec![0.5_f32; 16];
        let mut b = a.clone();
        b[6] = 0.7; // pixel 1, channel 2
        let got = first_exact_mismatch(&a, &b, 0.01);
        let (px, ch, diff) = got.expect("mismatch should be detected");
        assert_eq!(px, 1);
        assert_eq!(ch, 2);
        assert!((diff - 0.2).abs() < 1e-5, "diff = {diff}");
    }

    #[test]
    fn psnr_of_1_lsb_noise_is_around_48_db() {
        // Flip one u8 bit across a buffer of 1024 f32 channels. In
        // linear space a single u8 step is 1/255 ≈ 0.00392. Across
        // N samples the MSE contribution is 1/255² / N, so PSNR is
        // ~58 dB for a single perturbed channel in 1024 samples. We
        // use a loose lower bound to keep the test stable on any
        // floating-point implementation.
        let n = 1024;
        let a = vec![0.5_f32; n];
        let mut b = a.clone();
        b[0] += 1.0 / 255.0;
        let got = psnr(&a, &b);
        assert!(got > 50.0, "expected PSNR > 50 dB, got {got}");
    }

    #[test]
    fn psnr_drops_as_noise_scales() {
        // Two buffers where one has uniform 0.01 noise should be
        // noticeably worse than one with 0.001 noise.
        let a = vec![0.5_f32; 64];
        let mut b_small = a.clone();
        for v in b_small.iter_mut() {
            *v += 0.001;
        }
        let mut b_big = a.clone();
        for v in b_big.iter_mut() {
            *v += 0.01;
        }
        let p_small = psnr(&a, &b_small);
        let p_big = psnr(&a, &b_big);
        assert!(
            p_small > p_big,
            "small noise should score higher than big: {p_small} vs {p_big}",
        );
    }

    #[test]
    fn assert_exact_is_silent_on_match() {
        let a = vec![0.5_f32; 16];
        assert_exact(&a, &a, 0.0);
    }

    #[test]
    #[should_panic(expected = "differs by")]
    fn assert_exact_panics_on_mismatch() {
        let a = vec![0.5_f32; 4];
        let b = vec![0.5_f32, 0.5, 0.7, 1.0];
        assert_exact(&a, &b, 0.01);
    }

    #[test]
    fn psnr_handles_hdr_range_without_nan() {
        // Values above 1.0 are legal in the linear pipeline. Identical
        // HDR buffers still return infinity; mismatched ones return
        // a finite, positive number (not NaN).
        let hdr = vec![1.5_f32, 0.5, 2.0, 1.0, 3.0, 0.1, 0.8, 1.0];
        assert_eq!(psnr(&hdr, &hdr), f32::INFINITY);
        let mut noisy = hdr.clone();
        noisy[0] = 1.6; // 0.1 off on an HDR pixel
        let got = psnr(&hdr, &noisy);
        assert!(got.is_finite(), "PSNR went non-finite on HDR input: {got}");
        assert!(got > 0.0);
    }

    #[test]
    fn first_exact_mismatch_on_empty_buffers_is_none() {
        let empty: Vec<f32> = vec![];
        assert_eq!(first_exact_mismatch(&empty, &empty, 0.0), None);
    }

    #[test]
    #[should_panic(expected = "below the")]
    fn assert_psnr_at_least_panics_below_threshold() {
        // Two buffers with visibly different values — PSNR well
        // below the 80 dB demand.
        let a = vec![0.5_f32; 16];
        let mut b = a.clone();
        for v in b.iter_mut() {
            *v += 0.1;
        }
        assert_psnr_at_least(&a, &b, 80.0);
    }

    #[test]
    #[should_panic]
    fn first_exact_mismatch_panics_on_length_mismatch() {
        // Guard rail: comparing different-sized buffers is always a
        // bug, never silent.
        let a = vec![0.5_f32; 4];
        let b = vec![0.5_f32; 8];
        let _ = first_exact_mismatch(&a, &b, 0.0);
    }
}
