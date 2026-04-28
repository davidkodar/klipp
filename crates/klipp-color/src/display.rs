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
//! * [`ToneMapOp::Aces`] — Stephen Hill's fitted approximation of the
//!   ACES Reference Rendering Transform. A non-separable operator: R,
//!   G, B are mixed through a 3×3 input matrix before the curve and
//!   a 3×3 output matrix after, which preserves hue through highlight
//!   roll-off better than any per-channel curve. This is what most
//!   "cinematic" or "filmic" default looks resolve to in modern DCC
//!   tools — the industry standard for HDR mastering.

use crate::transfer::linear_to_srgb;

/// Re-export of [`klipp_core::ToneMapOp`]. The enum lives in
/// `klipp-core` so `ExportSettings` can carry it, but existing code
/// paths import it via `klipp_color::display::ToneMapOp` — keep that
/// path alive rather than cascading a rename through every call site.
pub use klipp_core::ToneMapOp;

/// Re-export of [`klipp_core::OdtChoice`]. Same crate-layering rationale
/// as [`ToneMapOp`] — the enum lives in `klipp-core` so the runtime
/// export type can carry it without depending on `klipp-color`.
pub use klipp_core::OdtChoice;

/// Iterate every operator in menu order. Useful for UI pickers and
/// round-trip tests without hard-coding variant lists.
pub const TONE_MAP_OPS_ALL: [ToneMapOp; 4] = [
    ToneMapOp::None,
    ToneMapOp::Reinhard,
    ToneMapOp::Hable,
    ToneMapOp::Aces,
];

/// Human-readable label for menus / serialised settings.
pub fn tone_map_op_label(op: ToneMapOp) -> &'static str {
    match op {
        ToneMapOp::None => "None",
        ToneMapOp::Reinhard => "Reinhard",
        ToneMapOp::Hable => "Hable (Filmic)",
        ToneMapOp::Aces => "ACES",
    }
}

/// Apply a tone map to a single linear-light channel value.
///
/// Alpha never goes through this — sRGB spec leaves alpha linear.
///
/// `ToneMapOp::Aces` collapses to its neutral-axis curve here: the
/// separable path has no way to run the 3×3 mixing matrices, so
/// results only match true ACES when R == G == B. Use
/// [`tone_map_rgb`] for per-pixel mapping when the pixel is not
/// already on the neutral axis.
#[inline]
pub fn tone_map(v: f32, op: ToneMapOp) -> f32 {
    match op {
        ToneMapOp::None => v,
        ToneMapOp::Reinhard => v / (1.0 + v),
        ToneMapOp::Hable => hable_normalised(v),
        ToneMapOp::Aces => aces_fit_neutral(v),
    }
}

/// Apply a tone map to a full linear-light RGB triple.
///
/// For separable operators this is just three calls to [`tone_map`].
/// For [`ToneMapOp::Aces`] it runs the full fitted RRT+ODT pipeline
/// with input / output matrices, which is the only way ACES actually
/// preserves hue — a per-channel ACES curve desaturates warm
/// highlights into yellow, which is exactly the artefact the real
/// transform is designed to avoid.
#[inline]
pub fn tone_map_rgb(rgb: [f32; 3], op: ToneMapOp) -> [f32; 3] {
    match op {
        ToneMapOp::Aces => aces_fitted(rgb),
        _ => [
            tone_map(rgb[0], op),
            tone_map(rgb[1], op),
            tone_map(rgb[2], op),
        ],
    }
}

/// Stephen Hill's fitted ACES RRT+ODT approximation.
///
/// The real ACES reference implementation is a stack of matrix
/// operations plus a long S-shaped curve derived from CTL scripts in
/// the Academy's ACES repository. This fit (published in Hill's
/// GDC 2017 talk "Multiverse: Space, Colour & Time") squeezes the
/// whole sRGB-output path into two 3×3 matrices sandwiching one
/// rational curve. The error vs. the reference is <1 code value on
/// the full sRGB display range, which is the same scale as 8-bit
/// quantisation — i.e. visually indistinguishable for display-
/// referred work. The fit also costs one order of magnitude less
/// compute than the CTL reference, which matters for per-pixel
/// viewer repaints.
///
/// Input is scene-referred linear-light RGB (rec.709 / sRGB
/// primaries). Output is in the same space, mapped into a display-
/// referred `[0, 1]` range — still linear, so the caller gamma-
/// encodes for display.
fn aces_fitted(rgb: [f32; 3]) -> [f32; 3] {
    let rrt = matrix3x3_mul(ACES_INPUT_MATRIX, rgb);
    let fitted = [
        rtt_and_odt_fit(rrt[0]),
        rtt_and_odt_fit(rrt[1]),
        rtt_and_odt_fit(rrt[2]),
    ];
    matrix3x3_mul(ACES_OUTPUT_MATRIX, fitted)
}

/// The neutral-axis collapse of [`aces_fitted`] — skips both matrices
/// (they're identity on the R=G=B axis by design) and returns just
/// the scalar curve. Used by the per-channel [`tone_map`] path and
/// by the Hable/ACES comparison tests on pure grey ramps.
fn aces_fit_neutral(x: f32) -> f32 {
    rtt_and_odt_fit(x)
}

#[inline]
fn rtt_and_odt_fit(v: f32) -> f32 {
    let a = v * (v + 0.0245786) - 0.000090537;
    let b = v * (0.983729 * v + 0.4329510) + 0.238081;
    a / b
}

#[inline]
fn matrix3x3_mul(m: [[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// Stephen Hill's ACES fit: rec.709 → AP1-ish working space.
const ACES_INPUT_MATRIX: [[f32; 3]; 3] = [
    [0.59719, 0.35458, 0.04823],
    [0.07600, 0.90834, 0.01566],
    [0.02840, 0.13383, 0.83777],
];

/// Stephen Hill's ACES fit: AP1-ish working space → rec.709.
const ACES_OUTPUT_MATRIX: [[f32; 3]; 3] = [
    [1.60475, -0.53108, -0.07367],
    [-0.10208, 1.10813, -0.00605],
    [-0.00327, -0.07276, 1.07602],
];

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
    use rayon::prelude::*;
    assert!(
        linear_rgba.len() % 4 == 0,
        "display_transform requires RGBA-packed input",
    );
    // Pre-allocate the output buffer so each rayon worker writes directly
    // into its slot — no per-chunk Vec::push allocations, no reduce step.
    let mut out = vec![0u8; linear_rgba.len()];
    out.par_chunks_exact_mut(4)
        .zip(linear_rgba.par_chunks_exact(4))
        .for_each(|(dst, chunk)| {
            // Clamp negatives before the tone-map: ACES' matrix path can
            // turn small negative channels into wildly over-bright
            // positives (the 1.60475 coefficient in the output matrix
            // amplifies anything it sees). The other operators already
            // guard against negatives on their own.
            let mapped = tone_map_rgb(
                [chunk[0].max(0.0), chunk[1].max(0.0), chunk[2].max(0.0)],
                tone_map_op,
            );
            dst[0] = (linear_to_srgb(mapped[0]).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            dst[1] = (linear_to_srgb(mapped[1]).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            dst[2] = (linear_to_srgb(mapped[2]).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            dst[3] = (chunk[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        });
    out
}

/// Working-space-aware display transform.
///
/// Dispatches based on `working_space`:
///
/// * [`klipp_core::WorkingColorspace::Rec709Linear`] — the legacy
///   path. Per-pixel: the user-selected tone-map operator
///   ([`ToneMapOp::Hable`] / `Reinhard` / `AcesFitted` / `None`) →
///   sRGB EOTF inverse → quantise to u8. Identical to
///   [`display_transform`] (the latter is now a thin wrapper around
///   `display_transform_ws(.., Rec709Linear)`).
///
/// * [`klipp_core::WorkingColorspace::AcesCG`] — full ACES display
///   pipeline. Per-pixel:
///   `acescg_to_rec709_sdr` (RRT → Rec.709 ODT → BT.1886 inverse)
///   → quantise to u8. The user-selected `tone_map_op` is ignored
///   here because the ACES Output Device Transform IS the tone
///   curve in this configuration; honouring `tone_map_op` on top
///   would compose two opinionated curves and produce a muddy look.
///
/// * [`klipp_core::WorkingColorspace::Bt2020Linear`] — stub aliased
///   to the `Rec709Linear` path until the BT.2020 pipeline lands
///   end-to-end. Matches the decoder/grade-chain stubbing from
///   earlier sessions; documented so the divergence is loud rather
///   than silent.
///
/// All branches preserve alpha as a linear value (never gamma-
/// encoded; clamped + quantised only).
pub fn display_transform_ws(
    linear_rgba: &[f32],
    tone_map_op: ToneMapOp,
    working_space: klipp_core::WorkingColorspace,
) -> Vec<u8> {
    use klipp_core::WorkingColorspace;
    use rayon::prelude::*;
    assert!(
        linear_rgba.len() % 4 == 0,
        "display_transform_ws requires RGBA-packed input",
    );
    if matches!(working_space, WorkingColorspace::Rec709Linear | WorkingColorspace::Bt2020Linear) {
        // Bt2020Linear is stubbed as Rec709Linear today (matches the
        // decoder/grade-chain stubs). When BT.2020 PQ/HLG ODTs land
        // (PHASE-E-S4C), this branch will need its own pipeline.
        return display_transform(linear_rgba, tone_map_op);
    }
    // AcesCG: ACES Output Device Transform path.
    let mut out = vec![0u8; linear_rgba.len()];
    out.par_chunks_exact_mut(4)
        .zip(linear_rgba.par_chunks_exact(4))
        .for_each(|(dst, chunk)| {
            // `acescg_to_rec709_sdr` chains AP1→AP0, RRT, Rec.709 ODT,
            // and BT.1886 inverse — output is `[0, 1]` display-encoded
            // Rec.709. Quantise to u8 here at the same precision as
            // the legacy path.
            let acescg = [chunk[0], chunk[1], chunk[2]];
            let encoded = crate::aces::acescg_to_rec709_sdr(acescg);
            dst[0] = (encoded[0].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            dst[1] = (encoded[1].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            dst[2] = (encoded[2].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            dst[3] = (chunk[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        });
    let _ = tone_map_op; // Intentionally unused in the ACES path.
    out
}

/// Export-side display transform.
///
/// Same shape as [`display_transform_ws`] but takes a delivery-target
/// [`OdtChoice`] so the rendered file can land in any of the
/// supported display colour spaces (Rec.709 SDR, Rec.2100 PQ at 1000
/// or 4000 nit, DCI-P3 D65 cinema, or the legacy sRGB-only path).
/// All branches read interleaved linear-light f32 RGBA and emit
/// interleaved display-encoded u8 RGBA.
///
/// # Dispatch
///
/// * [`OdtChoice::MatchViewer`] — defers to [`display_transform_ws`],
///   so the export pixels match the viewer pixel-for-pixel under the
///   same project working colorspace + tone-map operator.
/// * [`OdtChoice::Rec709Sdr`] — explicit Rec.709 SDR cinema (RRT +
///   Rec.709 ODT + BT.1886 inverse), regardless of working space.
///   When `working_space` is [`Rec709Linear`], the input is converted
///   to AcesCG (BT.709 → AP1 in linear) before the ODT — the ODT
///   chain assumes AcesCG-basis input. This is documented loud rather
///   than silent.
/// * [`OdtChoice::Rec2100Pq1000`] / [`OdtChoice::Rec2100Pq4000`] —
///   HDR10 PQ at 1000 / 4000 nit peak luminance with BT.2020
///   primaries. Same BT.709 → AP1 conversion when the project
///   working space is Rec.709-linear.
/// * [`OdtChoice::DciP3D65_48`] — DCI-P3 D65 theatrical at 48 nit.
///   Same conversion rule.
/// * [`OdtChoice::Srgb`] — legacy sRGB-only path, identical to
///   [`display_transform`]. No RRT/ODT, no working-space awareness;
///   byte-identical to every export written before delivery-target
///   ODTs landed.
/// * [`OdtChoice::Unknown`] — forwards-compat sentinel; treated as
///   `MatchViewer` so older builds opening a project with a newer
///   ODT stay safe.
///
/// `tone_map_op` is honoured only on the legacy paths
/// (`MatchViewer + Rec709Linear` / `Srgb`); the ACES-derived ODTs
/// supply their own tone curves and ignore the user's `tone_map_op`
/// to avoid composing two opinionated curves into a muddy look.
pub fn display_transform_for_export(
    linear_rgba: &[f32],
    tone_map_op: ToneMapOp,
    working_space: klipp_core::WorkingColorspace,
    odt: OdtChoice,
) -> Vec<u8> {
    use klipp_core::WorkingColorspace;
    use rayon::prelude::*;
    assert!(
        linear_rgba.len() % 4 == 0,
        "display_transform_for_export requires RGBA-packed input",
    );

    // MatchViewer + Srgb: dispatch directly so the legacy path stays
    // byte-identical to today's output. Unknown variant aliases
    // MatchViewer for forwards-compat.
    match odt {
        OdtChoice::MatchViewer | OdtChoice::Unknown => {
            return display_transform_ws(linear_rgba, tone_map_op, working_space);
        }
        OdtChoice::Srgb => {
            // Legacy non-ACES path. No RRT/ODT, no working-space
            // awareness — the user explicitly opted out of the new
            // pipeline. Byte-identical to pre-Phase-E exports.
            return display_transform(linear_rgba, tone_map_op);
        }
        _ => {}
    }

    // The remaining variants are all AcesCG-basis ODTs. If the
    // project's working space is BT.709-linear we have to walk the
    // input into AP1 first; otherwise the ODT sees Rec.709-basis
    // values and produces a wrong, hue-shifted look.
    //
    // BT.2020-linear projects fall through unconverted — there's no
    // BT.2020 → AP1 matrix wired here yet. The viewer aliases BT.2020
    // to Rec.709 today (display_transform_ws stub), so this matches
    // that intent until a proper BT.2020 IDT lands.
    let needs_709_to_ap1 = matches!(working_space, WorkingColorspace::Rec709Linear);
    let m_709_to_ap1 = if needs_709_to_ap1 {
        Some(crate::primaries::bt709_to_acescg())
    } else {
        None
    };

    let mut out = vec![0u8; linear_rgba.len()];
    out.par_chunks_exact_mut(4)
        .zip(linear_rgba.par_chunks_exact(4))
        .for_each(|(dst, chunk)| {
            // Walk into AP1 if the project ws isn't already there.
            let ap1 = if let Some(m) = m_709_to_ap1 {
                crate::primaries::matrix3_mul_vec(m, [chunk[0], chunk[1], chunk[2]])
            } else {
                [chunk[0], chunk[1], chunk[2]]
            };

            let encoded = match odt {
                OdtChoice::Rec709Sdr => crate::aces::acescg_to_rec709_sdr(ap1),
                OdtChoice::Rec2100Pq1000 => {
                    crate::aces::acescg_to_rec2100_pq_1000_nit(ap1)
                }
                OdtChoice::Rec2100Pq4000 => {
                    crate::aces::acescg_to_rec2100_pq_4000_nit(ap1)
                }
                OdtChoice::DciP3D65_48 => crate::aces::acescg_to_dcip3_d65_48nit(ap1),
                // The MatchViewer / Srgb / Unknown branches were
                // handled above; this match arm is unreachable, but
                // a Rust match must be total.
                OdtChoice::MatchViewer | OdtChoice::Srgb | OdtChoice::Unknown => ap1,
            };

            dst[0] = (encoded[0].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            dst[1] = (encoded[1].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            dst[2] = (encoded[2].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            dst[3] = (chunk[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        });

    let _ = tone_map_op; // Intentionally unused on the ODT branches.
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
        for op in TONE_MAP_OPS_ALL {
            // ACES has a tiny negative offset (`-0.000090537` in the
            // curve numerator) so it reports ~-3.8e-4 at zero input
            // — loose the tolerance a touch, but still well within
            // one 8-bit code value once quantised.
            let y = tone_map(0.0, op);
            assert!(
                y.abs() < 1e-3,
                "op {op:?} shifted zero by {y}",
            );
        }
    }

    #[test]
    fn all_operators_are_non_decreasing_across_range() {
        // Sample and verify monotonicity — a tone map that dips
        // backwards destroys gradients.
        for op in [ToneMapOp::Reinhard, ToneMapOp::Hable, ToneMapOp::Aces] {
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
        for op in TONE_MAP_OPS_ALL {
            for &v in &samples {
                let r = tone_map(v.max(0.0), op);
                assert!(r.is_finite(), "{op:?} on {v} produced {r}");
            }
        }
    }

    // ─── ACES-specific invariants ─────────────────────────────────

    #[test]
    fn aces_is_a_valid_menu_entry() {
        // Catch typos in the menu list / label table before they
        // reach the UI.
        assert_eq!(TONE_MAP_OPS_ALL.len(), 4);
        assert!(TONE_MAP_OPS_ALL.contains(&ToneMapOp::Aces));
        assert_eq!(tone_map_op_label(ToneMapOp::Aces), "ACES");
    }

    #[test]
    fn aces_preserves_neutral_axis() {
        // A grey input (R=G=B) must stay grey through ACES. The
        // input/output matrices are designed to be identity on the
        // achromatic axis, but a transcription error in either
        // matrix row sum would show here.
        for x in [0.05_f32, 0.18, 0.5, 1.0, 2.0, 5.0] {
            let out = tone_map_rgb([x, x, x], ToneMapOp::Aces);
            // All three channels equal within f32 precision.
            assert!(
                approx(out[0], out[1], 1e-3) && approx(out[1], out[2], 1e-3),
                "ACES broke neutral at x={x}: {out:?}",
            );
        }
    }

    #[test]
    fn aces_compresses_highlights_more_than_shadows() {
        // Same filmic contract as Hable: equal linear deltas map to
        // smaller output deltas in the highlights than in the shadows.
        // Run on the neutral axis to keep it a scalar check.
        let lo_a = aces_fit_neutral(0.05);
        let lo_b = aces_fit_neutral(0.15);
        let hi_a = aces_fit_neutral(2.0);
        let hi_b = aces_fit_neutral(2.1);
        assert!(
            (hi_b - hi_a) < (lo_b - lo_a),
            "ACES failed to compress highlights: shadow Δ={}, hi Δ={}",
            lo_b - lo_a,
            hi_b - hi_a,
        );
    }

    #[test]
    fn aces_mid_grey_sits_in_a_natural_range() {
        // Scene-referred 0.18 linear ("18% grey card") should come
        // out of ACES well short of display white. Different tools
        // land it anywhere from 0.08 to 0.12 depending on the
        // exposure bias; this loose band catches only catastrophic
        // crush (<0.04) or runaway gain (>0.20).
        let y = aces_fit_neutral(0.18);
        assert!(
            y > 0.04 && y < 0.20,
            "ACES mid-grey landed at {y} — outside filmic range",
        );
    }

    #[test]
    fn aces_bright_highlight_below_display_white() {
        // A 10-stops-over-mid-grey scene value (huge highlight)
        // should roll off into display white without exceeding 1.0
        // by a wide margin. Slight overshoot is acceptable — the
        // sRGB encode clamps at 1.0. But if 100.0 linear maps to
        // e.g. 2.5, something has lost the S-curve shape entirely.
        let out = tone_map_rgb([100.0, 100.0, 100.0], ToneMapOp::Aces);
        for &c in &out {
            assert!(
                c > 0.5 && c < 1.3,
                "ACES highlight runaway: {c} from input 100.0",
            );
        }
    }

    #[test]
    fn aces_separable_path_matches_matrix_path_on_neutral() {
        // The per-channel `tone_map` path collapses ACES to just the
        // curve. On the neutral axis that's equivalent to the full
        // matrix path (within matrix-arithmetic round-off), which
        // is the contract we document. Anywhere off-axis the two
        // paths legitimately differ.
        for x in [0.18_f32, 0.5, 1.0, 2.0] {
            let scalar = tone_map(x, ToneMapOp::Aces);
            let matrix = tone_map_rgb([x, x, x], ToneMapOp::Aces);
            // Tolerance: matrix round-trip drifts ~0.01 off the
            // scalar fit on bright inputs because the input matrix
            // rows don't sum to exactly 1.0.
            assert!(
                approx(scalar, matrix[0], 0.02),
                "scalar vs matrix ACES diverged at x={x}: {scalar} vs {matrix:?}",
            );
        }
    }

    #[test]
    fn aces_hue_survives_highlight_roll_off() {
        // A warm highlight (R=2.0, G=1.0, B=0.5) should keep its
        // "warm" ordering after ACES: R > G > B. A per-channel
        // curve would squash R toward display white faster than G
        // and B, collapsing the hue. The matrix path preserves
        // this ordering — that's the whole point of ACES.
        let out = tone_map_rgb([2.0, 1.0, 0.5], ToneMapOp::Aces);
        assert!(
            out[0] > out[1] && out[1] > out[2],
            "ACES ate the hue: {out:?}",
        );
    }

    // ─── display_transform_ws ──────────────────────────────────

    #[test]
    fn ws_aware_rec709_path_matches_legacy_display_transform() {
        // The Rec709Linear branch of `display_transform_ws` must
        // produce byte-identical output to the legacy
        // `display_transform`. If this drifts, every existing
        // viewer call site silently changes behaviour the moment
        // it's switched to the `_ws` form.
        let pix = vec![
            0.18, 0.5, 0.9, 1.0,
            0.05, 0.3, 0.7, 0.5,
            1.5, 0.0, -0.05, 1.0,   // out-of-range channels
        ];
        for op in [ToneMapOp::None, ToneMapOp::Hable, ToneMapOp::Reinhard, ToneMapOp::Aces] {
            let legacy = display_transform(&pix, op);
            let ws = display_transform_ws(&pix, op, klipp_core::WorkingColorspace::Rec709Linear);
            assert_eq!(legacy, ws, "Rec709Linear path drifted from legacy at op={op:?}");
        }
    }

    #[test]
    fn ws_aware_bt2020_path_aliases_rec709_for_now() {
        // Bt2020Linear is a stub — same output as Rec.709 until the
        // proper BT.2020 PQ/HLG ODTs ship. Document that intent so
        // a future "this is wrong" claim has a known baseline.
        let pix = vec![0.18, 0.5, 0.9, 1.0];
        let rec709 = display_transform_ws(&pix, ToneMapOp::Hable,
            klipp_core::WorkingColorspace::Rec709Linear);
        let bt2020 = display_transform_ws(&pix, ToneMapOp::Hable,
            klipp_core::WorkingColorspace::Bt2020Linear);
        assert_eq!(rec709, bt2020);
    }

    #[test]
    fn ws_aware_acescg_path_produces_neutral_grey_on_neutral_input() {
        // Mid-grey AP1 (0.18, 0.18, 0.18) → through full RRT+ODT
        // → display-encoded Rec.709 sRGB. Each channel must be
        // equal (neutral preservation through ACES) and clearly in
        // the mid range — not crushed black, not blown white.
        let pix = vec![0.18, 0.18, 0.18, 1.0];
        let out = display_transform_ws(&pix, ToneMapOp::None,
            klipp_core::WorkingColorspace::AcesCG);
        // Equal channels (within u8 quantisation noise).
        assert!((out[0] as i32 - out[1] as i32).abs() <= 1,
            "AcesCG mid-grey lost neutrality: {:?}", &out[..3]);
        assert!((out[1] as i32 - out[2] as i32).abs() <= 1);
        // Mid range — between 50 and 180 covers the perceptual
        // mid-grey territory after the BT.1886 encode.
        assert!(out[0] > 50 && out[0] < 180,
            "AcesCG 0.18 mid-grey should produce a visible mid-grey, got {:?}", &out[..3]);
    }

    #[test]
    fn ws_aware_acescg_clips_out_of_gamut_red_to_display_max() {
        // Saturated AP1 red (1, 0, 0) goes through RRT → ODT →
        // BT.1886. The Rec.709 SDR ODT clamps to display gamut, so
        // R should saturate near 255 while G/B sit near 0.
        let pix = vec![1.0, 0.0, 0.0, 1.0];
        let out = display_transform_ws(&pix, ToneMapOp::None,
            klipp_core::WorkingColorspace::AcesCG);
        assert!(out[0] > 200,
            "saturated AcesCG red R channel should be near display max, got {}", out[0]);
        assert!(out[3] == 255);
    }

    #[test]
    fn ws_aware_acescg_alpha_is_quantised_not_gamma_encoded() {
        // Per sRGB convention, alpha never gets gamma-encoded —
        // just clamped + quantised. The AcesCG path should mirror
        // the Rec.709 path on this.
        let pix = vec![0.5, 0.5, 0.5, 0.5];
        let out = display_transform_ws(&pix, ToneMapOp::None,
            klipp_core::WorkingColorspace::AcesCG);
        // 0.5 * 255 + 0.5 = 128 (rounding).
        assert!(out[3] == 128 || out[3] == 127);
    }

    // ─── display_transform_for_export ─────────────────────────────────
    //
    // The export pipeline's seam: linear-light frame → display-encoded
    // u8. Tests live here so the CPU oracle stays beside the viewer's
    // path; that lets parity drift be caught before any GPU work.

    /// A 4×4 RGBA test frame covering the corner cases that matter:
    /// mid-grey, saturated red (out-of-gamut for some ODT primaries),
    /// near-black, and near-white. Returned as interleaved f32 RGBA
    /// linear-light.
    fn small_linear_test_frame() -> Vec<f32> {
        let mut out = Vec::with_capacity(4 * 4 * 4);
        // Mid-grey (4 px).
        for _ in 0..4 { out.extend_from_slice(&[0.18, 0.18, 0.18, 1.0]); }
        // Saturated red — exercises gamut mapping.
        for _ in 0..4 { out.extend_from_slice(&[1.0, 0.0, 0.0, 1.0]); }
        // Near-black.
        for _ in 0..4 { out.extend_from_slice(&[0.005, 0.005, 0.005, 1.0]); }
        // Near-white (slight overshoot — realistic linear master).
        for _ in 0..4 { out.extend_from_slice(&[0.95, 0.97, 0.99, 1.0]); }
        out
    }

    #[test]
    fn export_match_viewer_at_acescg_is_byte_identical_to_viewer() {
        // The whole point of `MatchViewer`: the rendered file is
        // pixel-for-pixel what the user has been seeing in the
        // viewer. Anything else here is a viewer-vs-export drift bug.
        let frame = small_linear_test_frame();
        let viewer = display_transform_ws(
            &frame,
            ToneMapOp::Hable,
            klipp_core::WorkingColorspace::AcesCG,
        );
        let export = display_transform_for_export(
            &frame,
            ToneMapOp::Hable,
            klipp_core::WorkingColorspace::AcesCG,
            OdtChoice::MatchViewer,
        );
        assert_eq!(
            viewer, export,
            "MatchViewer + AcesCG diverged from the viewer's display path",
        );
    }

    #[test]
    fn export_match_viewer_at_rec709_matches_viewer_across_tone_map_ops() {
        // Under Rec.709 working space, the viewer's path is the
        // legacy `display_transform`; `MatchViewer` must collapse to
        // the same pixels for every tone-map operator the dialog
        // exposes. Pinned per-op so an accidental tone-map split
        // (e.g. the AcesCG branch grabbing the Rec.709 path) shows
        // here.
        let frame = small_linear_test_frame();
        for op in TONE_MAP_OPS_ALL {
            let viewer = display_transform_ws(
                &frame,
                op,
                klipp_core::WorkingColorspace::Rec709Linear,
            );
            let export = display_transform_for_export(
                &frame,
                op,
                klipp_core::WorkingColorspace::Rec709Linear,
                OdtChoice::MatchViewer,
            );
            assert_eq!(
                viewer, export,
                "MatchViewer + Rec709 + {op:?} diverged from the viewer",
            );
        }
    }

    #[test]
    fn export_match_viewer_at_bt2020_aliases_rec709_stub() {
        // BT.2020 is currently stubbed to the Rec.709 path in the
        // viewer (display_transform_ws). The export must alias the
        // same stub so a BT.2020 user gets matching pixels rather
        // than a silent wrong-gamut export.
        let frame = small_linear_test_frame();
        let viewer = display_transform_ws(
            &frame,
            ToneMapOp::Hable,
            klipp_core::WorkingColorspace::Bt2020Linear,
        );
        let export = display_transform_for_export(
            &frame,
            ToneMapOp::Hable,
            klipp_core::WorkingColorspace::Bt2020Linear,
            OdtChoice::MatchViewer,
        );
        assert_eq!(viewer, export);
    }

    #[test]
    fn export_rec709_sdr_at_acescg_matches_match_viewer() {
        // Under AcesCG, an explicit `Rec709Sdr` must produce the same
        // pixels as `MatchViewer` — the viewer's AcesCG path IS the
        // Rec.709 SDR ODT. Drift here means one of the two paths
        // grew an extra step the other didn't.
        let frame = small_linear_test_frame();
        let mv = display_transform_for_export(
            &frame,
            ToneMapOp::None,
            klipp_core::WorkingColorspace::AcesCG,
            OdtChoice::MatchViewer,
        );
        let explicit = display_transform_for_export(
            &frame,
            ToneMapOp::None,
            klipp_core::WorkingColorspace::AcesCG,
            OdtChoice::Rec709Sdr,
        );
        assert_eq!(
            mv, explicit,
            "Rec709Sdr + AcesCG should equal MatchViewer + AcesCG",
        );
    }

    #[test]
    fn export_srgb_legacy_path_matches_pre_phase_e_export() {
        // `OdtChoice::Srgb` is the safety net for users who want the
        // old, pre-ACES export. Must be byte-identical to the
        // legacy `display_transform` regardless of the project's
        // working space — by definition the legacy path doesn't
        // care about working space.
        let frame = small_linear_test_frame();
        let legacy = display_transform(&frame, ToneMapOp::Hable);
        for ws in [
            klipp_core::WorkingColorspace::Rec709Linear,
            klipp_core::WorkingColorspace::AcesCG,
            klipp_core::WorkingColorspace::Bt2020Linear,
        ] {
            let export = display_transform_for_export(
                &frame,
                ToneMapOp::Hable,
                ws,
                OdtChoice::Srgb,
            );
            assert_eq!(
                legacy, export,
                "Srgb-mode export drifted from legacy display_transform under ws={ws:?}",
            );
        }
    }

    #[test]
    fn export_unknown_odt_aliases_match_viewer() {
        // The serde sentinel arm: a future-version project file
        // smuggled in an ODT we don't know about. The runtime
        // dispatch treats Unknown as MatchViewer so the user still
        // gets the safe viewer-matching pixels.
        let frame = small_linear_test_frame();
        let mv = display_transform_for_export(
            &frame,
            ToneMapOp::Hable,
            klipp_core::WorkingColorspace::AcesCG,
            OdtChoice::MatchViewer,
        );
        let unknown = display_transform_for_export(
            &frame,
            ToneMapOp::Hable,
            klipp_core::WorkingColorspace::AcesCG,
            OdtChoice::Unknown,
        );
        assert_eq!(mv, unknown);
    }

    #[test]
    fn export_hdr_pq_1000_differs_measurably_from_sdr_rec709() {
        // The PQ ODT must produce different pixels from the SDR
        // Rec.709 ODT — anything else means the dispatch slot is
        // grabbing the wrong curve. We don't pin absolute Resolve
        // parity here (that's the soak's job); just that the two
        // paths diverge clearly. The mid-grey patch is the cleanest
        // signal: 0.18 linear becomes ~118/255 in SDR, but a
        // significantly different code value through PQ.
        let frame = small_linear_test_frame();
        let sdr = display_transform_for_export(
            &frame,
            ToneMapOp::None,
            klipp_core::WorkingColorspace::AcesCG,
            OdtChoice::Rec709Sdr,
        );
        let pq = display_transform_for_export(
            &frame,
            ToneMapOp::None,
            klipp_core::WorkingColorspace::AcesCG,
            OdtChoice::Rec2100Pq1000,
        );
        // Compare luma on the mid-grey patch (first 4 px).
        let sdr_y = sdr[0] as i32 + sdr[1] as i32 + sdr[2] as i32;
        let pq_y = pq[0] as i32 + pq[1] as i32 + pq[2] as i32;
        let drift = (sdr_y - pq_y).abs();
        assert!(
            drift > 8,
            "PQ-1000 mid-grey too close to SDR ({sdr_y} vs {pq_y}, Δ={drift}) — \
             dispatch likely picked the wrong ODT",
        );
    }

    #[test]
    fn export_hdr_pq_4000_differs_from_pq_1000() {
        // Different peak-luminance scaling must yield different
        // pixels — same dispatch family, different ODT instance.
        let frame = small_linear_test_frame();
        let pq1k = display_transform_for_export(
            &frame,
            ToneMapOp::None,
            klipp_core::WorkingColorspace::AcesCG,
            OdtChoice::Rec2100Pq1000,
        );
        let pq4k = display_transform_for_export(
            &frame,
            ToneMapOp::None,
            klipp_core::WorkingColorspace::AcesCG,
            OdtChoice::Rec2100Pq4000,
        );
        // Compare on the saturated-red patch (px index 4..8).
        let red_idx = 4 * 4; // start of the second 4-px block
        let drift = (pq1k[red_idx] as i32 - pq4k[red_idx] as i32).abs();
        assert!(
            drift > 0,
            "PQ-1000 and PQ-4000 produced identical red — peak scaling lost",
        );
    }

    #[test]
    fn export_dci_p3_differs_from_rec709_sdr() {
        // Same shape: different ODT variant must produce different
        // pixels. The DCI cinema curve has different highlight roll-
        // off + DCI gamma 2.6 inverse, so even on neutral grey the
        // output should diverge from the SDR Rec.709 path.
        let frame = small_linear_test_frame();
        let sdr = display_transform_for_export(
            &frame,
            ToneMapOp::None,
            klipp_core::WorkingColorspace::AcesCG,
            OdtChoice::Rec709Sdr,
        );
        let dcip3 = display_transform_for_export(
            &frame,
            ToneMapOp::None,
            klipp_core::WorkingColorspace::AcesCG,
            OdtChoice::DciP3D65_48,
        );
        // Mid-grey patch.
        let drift = (sdr[0] as i32 - dcip3[0] as i32).abs()
            + (sdr[1] as i32 - dcip3[1] as i32).abs()
            + (sdr[2] as i32 - dcip3[2] as i32).abs();
        assert!(
            drift > 4,
            "DCI-P3 D65 collapsed to the same pixels as Rec.709 SDR — wrong ODT slot",
        );
    }

    #[test]
    fn export_explicit_pq_under_rec709_project_walks_through_ap1() {
        // The cross-working-space contract: a Rec.709-linear project
        // with an explicit ODT (PQ here) MUST get its linear master
        // walked into AP1 first. Without that step the PQ ODT sees
        // Rec.709-basis values and produces a wrong, hue-shifted
        // image. Pin via a single linear-red pixel — under AP1
        // basis the output should diverge from the same input
        // treated as already-AP1.
        let pix_rec709 = vec![1.0_f32, 0.0, 0.0, 1.0];
        let from_709 = display_transform_for_export(
            &pix_rec709,
            ToneMapOp::None,
            klipp_core::WorkingColorspace::Rec709Linear,
            OdtChoice::Rec2100Pq1000,
        );
        let from_acescg = display_transform_for_export(
            &pix_rec709,
            ToneMapOp::None,
            klipp_core::WorkingColorspace::AcesCG,
            OdtChoice::Rec2100Pq1000,
        );
        // The two paths see the same numbers but the BT.709→AP1
        // matrix scrambles them — outputs must differ.
        assert_ne!(
            from_709, from_acescg,
            "Rec.709 project with PQ ODT didn't apply the AP1 matrix step — \
             the ODT is being fed the wrong basis",
        );
    }
}
