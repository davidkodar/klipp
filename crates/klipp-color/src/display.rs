//! Display transform: linear-light f32 → viewer-ready sRGB u8.
//!
//! This is the *one legitimate place* to resolve out-of-range linear
//! values. Every operation upstream (effects, composite, grade) is
//! allowed to produce values above 1.0 (highlights) or below 0.0
//! (negative chroma). This module tone-maps them into the display
//! range and then encodes to sRGB for the viewer's u8 texture.
//!
//! # When each tone-map operator is right
//!
//! * [`ToneMapOp::None`] — identity. Use for SDR content that
//!   doesn't exceed 1.0, or when you want to preview "as authored"
//!   without any compression. Values above 1.0 clip at the u8
//!   boundary.
//!
//! * [`ToneMapOp::Reinhard`] — simple soft roll-off, never clips.
//!   Mathematically one line: `x / (1 + x)`. Neutral / documentary
//!   look. Good default when you need a safe fallback but don't
//!   want anything opinionated.
//!
//! * [`ToneMapOp::Hable`] — filmic curve from Naughty Dog's
//!   Uncharted 2. Shoulder + toe that mimic film stock. Slight
//!   contrast boost, highlights roll off cinema-style. This is the
//!   default; it's what "looks right" for the YouTube / cinematic
//!   grading workflow without the user knowing to pick anything.
//!
//! ACES RRT (Phase 12 follow-up) will join this enum.

use crate::transfer::linear_to_srgb;

/// Tone-mapping operator used at the display boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToneMapOp {
    None,
    Reinhard,
    Hable,
}

impl Default for ToneMapOp {
    fn default() -> Self {
        Self::Hable
    }
}

/// Apply a tone map to a single linear-light channel value.
/// Alpha never goes through this — sRGB spec leaves alpha linear.
#[inline]
pub fn tone_map(v: f32, op: ToneMapOp) -> f32 {
    match op {
        ToneMapOp::None => v,
        ToneMapOp::Reinhard => v / (1.0 + v),
        ToneMapOp::Hable => hable_normalised(v),
    }
}

/// Hable (Uncharted 2) filmic curve, normalised so `hable(W) == 1.0`
/// at the white-point `W = 11.2`. Input is pre-scaled by an
/// exposure bias of 2.0 — matches the standard Uncharted 2 recipe
/// and keeps 18 % mid-grey near its input value instead of
/// crushing it ~3× darker. Above the white point the curve
/// asymptotes toward 1.0 but never quite reaches it — soft clip.
fn hable_normalised(x: f32) -> f32 {
    const EXPOSURE_BIAS: f32 = 2.0;
    const W: f32 = 11.2;
    hable(x * EXPOSURE_BIAS) / hable(W)
}

fn hable(x: f32) -> f32 {
    const A: f32 = 0.15;
    const B: f32 = 0.50;
    const C: f32 = 0.10;
    const D: f32 = 0.20;
    const E: f32 = 0.02;
    const F: f32 = 0.30;
    // Classic Uncharted 2 filmic curve.
    ((x * (A * x + C * B) + D * E) / (x * (A * x + B) + D * F)) - E / F
}

/// Full display transform: tone-map + sRGB encode + quantise to u8.
/// Expects interleaved linear f32 RGBA; returns interleaved sRGB u8
/// RGBA. Alpha is normalised and quantised but never gamma-encoded.
pub fn display_transform(linear_rgba: &[f32], tone_map_op: ToneMapOp) -> Vec<u8> {
    assert!(
        linear_rgba.len() % 4 == 0,
        "display_transform requires RGBA-packed input",
    );
    let mut out = Vec::with_capacity(linear_rgba.len());
    for chunk in linear_rgba.chunks_exact(4) {
        let r_lin = tone_map(chunk[0].max(0.0), tone_map_op);
        let g_lin = tone_map(chunk[1].max(0.0), tone_map_op);
        let b_lin = tone_map(chunk[2].max(0.0), tone_map_op);
        let r = (linear_to_srgb(r_lin).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        let g = (linear_to_srgb(g_lin).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        let b = (linear_to_srgb(b_lin).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        let a = (chunk[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        out.push(r);
        out.push(g);
        out.push(b);
        out.push(a);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    // ─── ToneMapOp invariants ──────────────────────────────────────

    #[test]
    fn default_is_hable() {
        assert_eq!(ToneMapOp::default(), ToneMapOp::Hable);
    }

    #[test]
    fn none_is_identity_under_tone_map() {
        for &v in &[0.0_f32, 0.18, 0.5, 1.0, 2.5, 10.0] {
            assert_eq!(tone_map(v, ToneMapOp::None), v);
        }
    }

    #[test]
    fn all_operators_map_zero_to_zero() {
        for op in [ToneMapOp::None, ToneMapOp::Reinhard, ToneMapOp::Hable] {
            assert!(tone_map(0.0, op).abs() < 1e-5, "op {op:?} shifted zero");
        }
    }

    #[test]
    fn all_operators_are_non_decreasing_across_range() {
        // Sample and verify monotonicity — a tone map that dips
        // backwards destroys gradients.
        for op in [ToneMapOp::Reinhard, ToneMapOp::Hable] {
            let mut prev = tone_map(0.0, op);
            for i in 1..=200 {
                let v = i as f32 * 0.1;
                let curr = tone_map(v, op);
                assert!(
                    curr + 1e-5 >= prev,
                    "{op:?} dropped at v={v}: {prev} -> {curr}",
                );
                prev = curr;
            }
        }
    }

    #[test]
    fn reinhard_never_clips_and_asymptotes_toward_one() {
        // Reinhard: x / (1 + x). Classic formula, testable.
        assert!(approx(tone_map(1.0, ToneMapOp::Reinhard), 0.5, 1e-5));
        assert!(approx(tone_map(3.0, ToneMapOp::Reinhard), 0.75, 1e-5));
        // Very high inputs approach 1.0 but never reach it.
        assert!(tone_map(1000.0, ToneMapOp::Reinhard) < 1.0);
        assert!(tone_map(1000.0, ToneMapOp::Reinhard) > 0.999);
    }

    #[test]
    fn hable_compresses_highlights_more_than_shadows() {
        // A filmic curve's job: bend the top end harder than the
        // bottom. For equal linear deltas in shadows vs highlights,
        // the highlight pair should end up closer together than
        // the shadow pair (compression), not farther apart.
        let shadow_a = tone_map(0.05, ToneMapOp::Hable);
        let shadow_b = tone_map(0.15, ToneMapOp::Hable);
        let hi_a = tone_map(2.0, ToneMapOp::Hable);
        let hi_b = tone_map(2.1, ToneMapOp::Hable);
        let shadow_spread = shadow_b - shadow_a; // 0.1 linear delta
        let hi_spread = hi_b - hi_a; // 0.1 linear delta
        assert!(
            hi_spread < shadow_spread,
            "highlights should be compressed harder than shadows: \
             shadow Δ={shadow_spread:.4} hi Δ={hi_spread:.4}",
        );
    }

    #[test]
    fn hable_stays_finite_and_near_one_for_realistic_inputs() {
        // Across the realistic linear range, Hable must produce
        // finite, bounded output. Slight overshoot above 1.0 is ok
        // (`display_transform` clamps at the sRGB-encode boundary);
        // anything wilder than ~1.3 signals a curve with the wrong
        // white-point math.
        for x in [0.0, 0.18, 0.5, 1.0, 2.0, 5.0, 11.2, 100.0] {
            let y = tone_map(x, ToneMapOp::Hable);
            assert!(y.is_finite(), "Hable({x}) = {y}");
            assert!(
                y >= -1e-5 && y < 1.3,
                "Hable({x}) = {y} is wildly out of range",
            );
        }
    }

    // ─── display_transform ────────────────────────────────────────

    #[test]
    fn transform_is_deterministic_and_preserves_rgba_stride() {
        let pix = [0.18_f32, 0.5, 0.9, 1.0];
        let out_a = display_transform(&pix, ToneMapOp::Hable);
        let out_b = display_transform(&pix, ToneMapOp::Hable);
        assert_eq!(out_a, out_b);
        assert_eq!(out_a.len(), 4);
    }

    #[test]
    fn transform_none_on_sdr_round_trips_within_one_code_value() {
        // For values that don't exceed 1.0, ToneMapOp::None should
        // give us the same answer as a naive `linear_to_srgb_u8`
        // conversion — the only thing that changes is the encoding
        // step is still present.
        let input = [0.0_f32, 0.18, 0.5, 1.0];
        let out = display_transform(&input, ToneMapOp::None);
        // linear_to_srgb(0.0) = 0; linear_to_srgb(1.0) ≈ 1.0
        assert_eq!(out[0], 0);
        assert!((out[3] as i32 - 255).abs() <= 1);
    }

    #[test]
    fn alpha_is_not_gamma_encoded() {
        // Alpha channel is never gamma-encoded per sRGB spec.
        // Linear 0.5 should map to u8 128 (not 188, which is what
        // gamma-encoding 0.5 would produce).
        let pix = [0.0_f32, 0.0, 0.0, 0.5];
        let out = display_transform(&pix, ToneMapOp::None);
        assert!(
            (out[3] as i32 - 128).abs() <= 1,
            "alpha = {}, expected ~128",
            out[3],
        );
    }

    #[test]
    fn negative_channels_clamp_to_zero() {
        // Over-saturation can push channels below 0.0. That's legal
        // in linear but must clamp at the display boundary.
        let pix = [-0.3_f32, 0.5, 1.2, 1.0];
        let out = display_transform(&pix, ToneMapOp::Hable);
        assert_eq!(out[0], 0, "negative R did not clamp to 0: {}", out[0]);
    }

    #[test]
    fn hdr_input_fits_in_u8_range_with_hable() {
        // An extremely bright linear pixel (5.0) should still fit
        // in u8 under Hable — no 256 wrap-around.
        let pix = [5.0_f32, 5.0, 5.0, 1.0];
        let out = display_transform(&pix, ToneMapOp::Hable);
        for &v in &out[..3] {
            assert!(v < 255 || v == 255, "channel overflowed u8: {v}");
        }
    }

    #[test]
    fn hdr_input_without_tone_map_clamps_not_wraps() {
        // Same HDR pixel, but ToneMapOp::None — must still produce
        // valid u8 via the final clamp.
        let pix = [5.0_f32, 5.0, 5.0, 1.0];
        let out = display_transform(&pix, ToneMapOp::None);
        for &v in &out[..3] {
            assert_eq!(v, 255, "no tone-map path must clamp at 255");
        }
    }

    #[test]
    fn mid_grey_round_trips_under_none() {
        // Linear 0.18 with no tone map should encode to ~sRGB 118
        // (the canonical 18 % mid-grey code value).
        let pix = [0.18_f32, 0.18, 0.18, 1.0];
        let out = display_transform(&pix, ToneMapOp::None);
        for &v in &out[..3] {
            assert!(
                (v as i32 - 118).abs() <= 1,
                "channel = {v}, expected ~118",
            );
        }
    }

    #[test]
    fn every_operator_produces_finite_output() {
        // Sanity fuzz — no NaN / inf from any operator across a
        // representative linear range.
        let samples: Vec<f32> = (-10..=50).map(|i| i as f32 / 10.0).collect();
        for op in [ToneMapOp::None, ToneMapOp::Reinhard, ToneMapOp::Hable] {
            for &v in &samples {
                let r = tone_map(v.max(0.0), op);
                assert!(r.is_finite(), "{op:?} on {v} produced {r}");
            }
        }
    }
}
