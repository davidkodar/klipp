//! Soft-clip / hard-clip transform — Reinhard-style asymptotic
//! highlight rolloff applied per RGB channel. Intended for delivery
//! prep where broadcast / streaming targets want highlights pulled
//! below 1.0 so downstream limiters don't trip and cap-related
//! banding doesn't appear.
//!
//! See [`klipp_core::SoftClipSettings`] for the full curve doc; the
//! shape is:
//!
//! ```text
//! y = x                                              if x <= threshold
//! y = threshold                                      if knee == 0     (hard clip)
//! y = threshold + (1 - threshold) * (1 - e^(-excess / knee))   otherwise
//! ```
//!
//! Per-channel, in linear-light space. Value transform — does not
//! produce a matte; output buffer has the same RGBA shape as input.

use klipp_core::SoftClipSettings;

/// Apply soft-clip per-channel to a linear-f32 RGBA buffer.
/// Returns a new `Vec<f32>` so the caller can swap buffers without
/// the alias risk of an in-place mutation. Alpha is passed through
/// untouched.
pub fn apply_soft_clip(
    linear_rgba: &[f32],
    _width: u32,
    _height: u32,
    s: &SoftClipSettings,
) -> Vec<f32> {
    if !s.enabled {
        return linear_rgba.to_vec();
    }
    let mut out = Vec::with_capacity(linear_rgba.len());
    for chunk in linear_rgba.chunks_exact(4) {
        out.push(soft_clip_value(chunk[0], s.threshold, s.knee));
        out.push(soft_clip_value(chunk[1], s.threshold, s.knee));
        out.push(soft_clip_value(chunk[2], s.threshold, s.knee));
        out.push(chunk[3]); // alpha untouched
    }
    out
}

/// Single-value soft-clip curve. Pulled out so the GPU shader
/// (WGSL) and the unit tests can verify the same scalar formula.
fn soft_clip_value(x: f32, threshold: f32, knee: f32) -> f32 {
    if x <= threshold {
        return x;
    }
    if knee <= 1e-6 {
        return threshold;
    }
    let excess = x - threshold;
    let max_remaining = 1.0 - threshold;
    if max_remaining <= 0.0 {
        // Threshold at or above 1 — nothing to roll into; behave
        // as a passthrough cap.
        return x.min(threshold);
    }
    let rolloff = max_remaining * (1.0 - (-excess / knee).exp());
    (threshold + rolloff).min(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(threshold: f32, knee: f32) -> SoftClipSettings {
        SoftClipSettings { enabled: true, threshold, knee }
    }

    #[test]
    fn disabled_is_identity() {
        // Default settings have enabled=false. Output must equal
        // input bit-for-bit; users dragging the threshold slider
        // before the enable toggle shouldn't see any change.
        let s = SoftClipSettings::default();
        let input = vec![0.5, 0.95, 1.5, 1.0];
        let out = apply_soft_clip(&input, 1, 1, &s);
        assert_eq!(out, input);
    }

    #[test]
    fn passthrough_below_threshold() {
        let s = settings(0.8, 0.1);
        let rgba = vec![0.5, 0.5, 0.5, 1.0];
        let out = apply_soft_clip(&rgba, 1, 1, &s);
        assert!((out[0] - 0.5).abs() < 1e-6);
        assert!((out[1] - 0.5).abs() < 1e-6);
        assert!((out[2] - 0.5).abs() < 1e-6);
        assert!((out[3] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn knee_zero_is_hard_clip() {
        // knee=0 should clamp every above-threshold value to
        // threshold exactly (no rolloff).
        let s = settings(0.8, 0.0);
        let rgba = vec![0.85, 0.95, 2.0, 1.0];
        let out = apply_soft_clip(&rgba, 1, 1, &s);
        assert!((out[0] - 0.8).abs() < 1e-6);
        assert!((out[1] - 0.8).abs() < 1e-6);
        assert!((out[2] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn smooth_rolloff_above_threshold() {
        // With knee>0, values above threshold should be pulled in
        // toward but not equal to threshold, monotonically rising
        // toward 1.0 as x grows.
        let y_at_thresh    = soft_clip_value(0.80, 0.8, 0.1);
        let y_just_above   = soft_clip_value(0.85, 0.8, 0.1);
        let y_far_above    = soft_clip_value(2.00, 0.8, 0.1);
        // At threshold: still threshold (curve is tangent to passthrough there).
        assert!((y_at_thresh - 0.8).abs() < 1e-4);
        // Just above: between threshold and 1.0
        assert!(y_just_above > 0.8 && y_just_above < 1.0);
        // Far above: clamped to 1.0
        assert!((y_far_above - 1.0).abs() < 1e-4);
        // Monotonic
        assert!(y_just_above < y_far_above);
    }

    #[test]
    fn threshold_at_one_is_no_op() {
        // threshold = 1.0 means "never roll off" — every input <= 1
        // passes through unchanged (the curve never engages).
        let s = settings(1.0, 0.1);
        let out = apply_soft_clip(&[0.0, 0.5, 1.0, 1.0], 1, 1, &s);
        assert!((out[0] - 0.0).abs() < 1e-6);
        assert!((out[1] - 0.5).abs() < 1e-6);
        assert!((out[2] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn alpha_is_untouched() {
        let s = settings(0.5, 0.1);
        let rgba = vec![0.9, 0.9, 0.9, 0.42];
        let out = apply_soft_clip(&rgba, 1, 1, &s);
        // RGB rolled, alpha exactly preserved.
        assert!((out[3] - 0.42).abs() < 1e-6);
    }

    #[test]
    fn batch_pixels_independent() {
        // Three pixels: [under threshold, hits curve, well above].
        // Verify per-pixel iteration writes the correct slot.
        let s = settings(0.8, 0.1);
        let input = vec![
            0.5, 0.5, 0.5, 1.0,    // pixel 0 — under threshold
            0.85, 0.85, 0.85, 1.0, // pixel 1 — in curve region
            2.0, 2.0, 2.0, 1.0,    // pixel 2 — far above
        ];
        let out = apply_soft_clip(&input, 3, 1, &s);
        // pixel 0: passthrough
        assert!((out[0] - 0.5).abs() < 1e-6);
        // pixel 1: rolled but not 1.0
        assert!(out[4] > 0.8 && out[4] < 1.0);
        // pixel 2: clamped at 1.0
        assert!((out[8] - 1.0).abs() < 1e-4);
    }
}
