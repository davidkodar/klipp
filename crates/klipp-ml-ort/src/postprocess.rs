//! BiRefNet post-processing.
//!
//! BiRefNet's output is a single-channel logits tensor at the model
//! input resolution (1024×1024). To turn it into a `MaskBuffer` the
//! UI can paint, we:
//!
//! 1. Apply the sigmoid activation — `1 / (1 + exp(-x))` — so logits
//!    map to probabilities in [0, 1].
//! 2. Bilinear resample back to the source frame resolution. The
//!    user's click + the painted mask must land on the same pixels;
//!    matting the model output to source size is what makes that
//!    work.
//! 3. Convert the [0, 1] probability to u8 alpha (0..255).
//!
//! Pure CPU, no model dependency — testable end-to-end without ort.

use klipp_ml::MaskBuffer;

/// Sigmoid activation, applied per-element. Stable for large
/// magnitudes — uses the standard `1 / (1 + exp(-x))` form which
/// over/underflows gracefully for the float range BiRefNet emits.
pub fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// Run the full BiRefNet post-process pipeline.
///
/// `logits` must have length `model_w * model_h`. `model_w / model_h`
/// are the model's input dimensions (1024² for BiRefNet_lite). The
/// returned `MaskBuffer` is at `(target_w, target_h)`.
pub fn postprocess(
    logits: &[f32],
    model_w: u32,
    model_h: u32,
    target_w: u32,
    target_h: u32,
) -> MaskBuffer {
    let model_pixels = (model_w as usize) * (model_h as usize);
    if logits.len() != model_pixels
        || model_w == 0
        || model_h == 0
        || target_w == 0
        || target_h == 0
    {
        return MaskBuffer {
            width: target_w,
            height: target_h,
            alpha: vec![0u8; (target_w as usize) * (target_h as usize)],
        };
    }

    // 1) Sigmoid in-place into a scratch f32 buffer.
    let mut probs = Vec::with_capacity(model_pixels);
    for &x in logits {
        probs.push(sigmoid(x));
    }

    // 2) Bilinear resample probability map to target size.
    let resized = bilinear_resize_single_channel(&probs, model_w, model_h, target_w, target_h);

    // 3) Quantise to u8 (0..255). Clamp first to defend against
    //    floating-point overshoot from the resampler at edge cases.
    let mut alpha = Vec::with_capacity(resized.len());
    for p in resized {
        alpha.push((p.clamp(0.0, 1.0) * 255.0).round() as u8);
    }

    MaskBuffer {
        width: target_w,
        height: target_h,
        alpha,
    }
}

/// Single-channel bilinear resampler — same conventions as
/// `preprocess::bilinear_resize_rgba` (centre-of-pixel sampling,
/// CLAMP_TO_EDGE). Lives here rather than in `preprocess` because
/// the post-process operates on a probability map (not RGBA).
pub fn bilinear_resize_single_channel(
    src: &[f32],
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> Vec<f32> {
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return Vec::new();
    }
    if src.len() < (src_w as usize) * (src_h as usize) {
        return Vec::new();
    }

    let mut out = vec![0.0_f32; (dst_w as usize) * (dst_h as usize)];
    let scale_x = (src_w as f32) / (dst_w as f32);
    let scale_y = (src_h as f32) / (dst_h as f32);

    for dy in 0..dst_h {
        let sy = ((dy as f32) + 0.5) * scale_y - 0.5;
        let sy0 = sy.floor().max(0.0).min((src_h - 1) as f32) as u32;
        let sy1 = (sy0 + 1).min(src_h - 1);
        let fy = (sy - sy0 as f32).clamp(0.0, 1.0);

        for dx in 0..dst_w {
            let sx = ((dx as f32) + 0.5) * scale_x - 0.5;
            let sx0 = sx.floor().max(0.0).min((src_w - 1) as f32) as u32;
            let sx1 = (sx0 + 1).min(src_w - 1);
            let fx = (sx - sx0 as f32).clamp(0.0, 1.0);

            let p00 = src[(sy0 * src_w + sx0) as usize];
            let p01 = src[(sy0 * src_w + sx1) as usize];
            let p10 = src[(sy1 * src_w + sx0) as usize];
            let p11 = src[(sy1 * src_w + sx1) as usize];
            let top = p00 * (1.0 - fx) + p01 * fx;
            let bot = p10 * (1.0 - fx) + p11 * fx;
            out[(dy * dst_w + dx) as usize] = top * (1.0 - fy) + bot * fy;
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sigmoid_zero_is_half() {
        assert!((sigmoid(0.0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn sigmoid_extremes_clamp_smoothly() {
        // Large positive logit → ~1; large negative → ~0. Pin so a
        // future "stable sigmoid" rewrite doesn't drift the constants.
        assert!(sigmoid(20.0) > 0.9999);
        assert!(sigmoid(-20.0) < 0.0001);
    }

    #[test]
    fn postprocess_high_logits_become_opaque() {
        // 4×4 model output, all logits = +20. Post-process should
        // produce a 4×4 fully-opaque (255) mask.
        let logits = vec![20.0_f32; 16];
        let mask = postprocess(&logits, 4, 4, 4, 4);
        assert_eq!(mask.width, 4);
        assert_eq!(mask.height, 4);
        for &a in &mask.alpha {
            assert_eq!(a, 255, "expected fully opaque");
        }
    }

    #[test]
    fn postprocess_low_logits_become_transparent() {
        let logits = vec![-20.0_f32; 16];
        let mask = postprocess(&logits, 4, 4, 4, 4);
        for &a in &mask.alpha {
            assert_eq!(a, 0, "expected fully transparent");
        }
    }

    #[test]
    fn postprocess_resamples_to_target_size() {
        // 2×2 model output, all logits = 0 (sigmoid → 0.5 → 128).
        // Resample to 4×4 — every output pixel should be ~128.
        let logits = vec![0.0_f32; 4];
        let mask = postprocess(&logits, 2, 2, 4, 4);
        assert_eq!(mask.alpha.len(), 16);
        for &a in &mask.alpha {
            // Allow ±1 for round-to-nearest.
            assert!((127..=129).contains(&a), "expected ~128, got {a}");
        }
    }

    #[test]
    fn postprocess_partial_logits_round_to_nearest() {
        // Logit ≈ 1.0986 → sigmoid → 0.75 → 191.25 → 191.
        let logits = vec![1.0986123_f32; 4];
        let mask = postprocess(&logits, 2, 2, 2, 2);
        for &a in &mask.alpha {
            assert!((190..=192).contains(&a), "expected ~191, got {a}");
        }
    }

    #[test]
    fn postprocess_wrong_shape_returns_zero_mask() {
        // Defensive: caller passed mismatched logits length. Don't
        // panic, just return an empty (zero) mask of target size.
        let logits = vec![0.0_f32; 7];
        let mask = postprocess(&logits, 4, 4, 4, 4);
        assert_eq!(mask.alpha.len(), 16);
        for &a in &mask.alpha {
            assert_eq!(a, 0);
        }
    }

    #[test]
    fn postprocess_zero_target_returns_empty_mask() {
        let logits = vec![0.0_f32; 4];
        let mask = postprocess(&logits, 2, 2, 0, 0);
        assert_eq!(mask.alpha.len(), 0);
    }

    #[test]
    fn single_channel_resize_identity() {
        let src = vec![0.0, 0.5, 1.0, 0.25, 0.75, 0.1, 0.9, 0.4, 0.5];
        let out = bilinear_resize_single_channel(&src, 3, 3, 3, 3);
        assert_eq!(out.len(), src.len());
        for (a, b) in src.iter().zip(out.iter()) {
            assert!((a - b).abs() < 1e-4);
        }
    }

    #[test]
    fn single_channel_resize_handles_empty_input() {
        let out = bilinear_resize_single_channel(&[], 0, 0, 16, 16);
        assert!(out.is_empty());
    }

    #[test]
    fn postprocess_localised_high_logit_stays_localised_after_resize() {
        // Single bright pixel in the middle of a 4×4 logits buffer
        // should still be the brightest pixel after upscaling — checks
        // that the resampler doesn't average the signal away.
        let mut logits = vec![-20.0_f32; 16];
        logits[5] = 20.0; // (1, 1) — top-left of the central 2x2 block
        let mask = postprocess(&logits, 4, 4, 16, 16);
        // The peak should land near the centre.
        let mut max_a = 0u8;
        let mut max_idx = 0usize;
        for (i, &a) in mask.alpha.iter().enumerate() {
            if a > max_a {
                max_a = a;
                max_idx = i;
            }
        }
        let mx = (max_idx % 16) as i32;
        let my = (max_idx / 16) as i32;
        // Source bright pixel was at (1,1) in 4x4 → centre roughly (4..6) in 16x16.
        assert!(
            (3..=8).contains(&mx) && (3..=8).contains(&my),
            "peak landed at ({mx}, {my}); expected near (4..6)"
        );
        // Bilinear smoothing softens the peak; we just need it to
        // still dominate the surrounding zeros.
        assert!(max_a > 150, "peak alpha was {max_a}");
    }
}
