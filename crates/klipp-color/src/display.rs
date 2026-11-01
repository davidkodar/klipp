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

use crate::transfer::{linear_to_srgb, srgb_to_linear};

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
    display_transform_from(linear_rgba, tone_map_op, None)
}

/// Bringing a working space into BT.709 for an SDR output: the linear
/// matrix, and whether what the matrix pushes outside BT.709 is gamut-
/// compressed on the way. A BT.709 project needs neither.
#[derive(Clone, Copy)]
struct ToBt709 {
    matrix: [[f32; 3]; 3],
    compress: bool,
}

impl ToBt709 {
    /// `None` for a project already in BT.709, and for the legacy
    /// "encode the working values as they are" escape hatch.
    fn for_working_space(ws: klipp_core::WorkingColorspace) -> Option<Self> {
        let matrix = crate::primaries::working_space_conversion_matrix(
            ws,
            klipp_core::WorkingColorspace::Rec709Linear,
        )?;
        Some(Self {
            // BT.2020 is the one working space wider than BT.709 that
            // reaches an sRGB encode by matrix alone — an ACEScg project
            // goes through an ODT, which maps its own gamut.
            compress: ws == klipp_core::WorkingColorspace::Bt2020Linear,
            matrix,
        })
    }

    #[inline(always)]
    fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let rgb = crate::primaries::matrix3_mul_vec(self.matrix, rgb);
        if self.compress {
            crate::gamut::compress_bt2020_to_bt709(rgb)
        } else {
            rgb
        }
    }
}

/// [`display_transform`], with the working space's own basis converted
/// into BT.709 first. `to_709` is `None` when the picture is already
/// there. Folding the conversion into this loop keeps a wide-gamut
/// project at one pass over the frame instead of a converted copy of it.
fn display_transform_from(
    linear_rgba: &[f32],
    tone_map_op: ToneMapOp,
    to_709: Option<ToBt709>,
) -> Vec<u8> {
    display_transform_from_pre(linear_rgba, tone_map_op, to_709, |v| v)
}

/// Encode a display-linear value for the SDR viewer's transfer.
#[inline]
pub fn encode_sdr(v: f32, transfer: klipp_core::SdrTransfer) -> f32 {
    match transfer {
        klipp_core::SdrTransfer::Srgb => linear_to_srgb(v),
        klipp_core::SdrTransfer::Bt1886 => v.clamp(0.0, 1.0).powf(1.0 / 2.4),
    }
}

/// [`display_transform_from`] with a per-channel function applied to the
/// linear light first.
///
/// A hook rather than a caller-side pass, because the caller-side version
/// needs somewhere to put the result: HDR monitoring used to copy the
/// frame (`linear.to_vec()`), map it in place and encode that, and at 4K
/// the copy alone measured **30 ms** — more than the encode it fed. The
/// hook reads each pixel once and writes bytes.
fn display_transform_from_pre(
    linear_rgba: &[f32],
    tone_map_op: ToneMapOp,
    to_709: Option<ToBt709>,
    pre: impl Fn(f32) -> f32 + Sync + Send,
) -> Vec<u8> {
    display_transform_from_pre_to(linear_rgba, tone_map_op, to_709, pre, klipp_core::SdrTransfer::Srgb, false)
}

/// `dither` adds [`dither_at`] before the rounding: the viewer's encode
/// ([`display_transform_ws_dithered_to`]) does, the plain transform does
/// not.
fn display_transform_from_pre_to(
    linear_rgba: &[f32],
    tone_map_op: ToneMapOp,
    to_709: Option<ToBt709>,
    pre: impl Fn(f32) -> f32 + Sync + Send,
    transfer: klipp_core::SdrTransfer,
    dither: bool,
) -> Vec<u8> {
    use rayon::prelude::*;
    assert!(
        linear_rgba.len().is_multiple_of(4),
        "display_transform requires RGBA-packed input",
    );
    // Pre-allocate the output buffer so each rayon worker writes directly
    // into its slot — no per-chunk Vec::push allocations, no reduce step.
    let mut out = vec![0u8; linear_rgba.len()];
    out.par_chunks_exact_mut(4)
        .zip(linear_rgba.par_chunks_exact(4))
        .enumerate()
        .for_each(|(i, (dst, chunk))| {
            // Clamp negatives before the tone-map: ACES' matrix path can
            // turn small negative channels into wildly over-bright
            // positives (the 1.60475 coefficient in the output matrix
            // amplifies anything it sees). The other operators already
            // guard against negatives on their own.
            let src = [pre(chunk[0]), pre(chunk[1]), pre(chunk[2])];
            let rgb = match to_709 {
                Some(c) => c.apply(src),
                None => src,
            };
            let mapped = tone_map_rgb(
                [rgb[0].max(0.0), rgb[1].max(0.0), rgb[2].max(0.0)],
                tone_map_op,
            );
            let n = if dither { dither_at(i) } else { 0.0 };
            dst[0] = quantise(encode_sdr(mapped[0], transfer), n, 255.0) as u8;
            dst[1] = quantise(encode_sdr(mapped[1], transfer), n, 255.0) as u8;
            dst[2] = quantise(encode_sdr(mapped[2], transfer), n, 255.0) as u8;
            dst[3] = quantise(chunk[3], 0.0, 255.0) as u8;
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
/// * [`klipp_core::WorkingColorspace::Bt2020Linear`] — the same path,
///   with BT.2020 → BT.709 in linear light first, and the colour BT.709
///   cannot hold gamut-mapped into it rather than clipped against it
///   (see [`crate::gamut`]). Saturated wide-gamut colour rolls off,
///   keeping its hue and lightness and staying distinct from its
///   neighbours, so an SDR monitor of a BT.2020 project shows a
///   narrower rendering of the grade instead of a flattened one. It is
///   still a rendering: judge a delivery against the HDR view or an
///   ACES ODT.
///
/// All branches preserve alpha as a linear value (never gamma-
/// encoded; clamped + quantised only).
pub fn display_transform_ws(
    linear_rgba: &[f32],
    tone_map_op: ToneMapOp,
    working_space: klipp_core::WorkingColorspace,
) -> Vec<u8> {
    display_transform_ws_pre(linear_rgba, tone_map_op, working_space, |v| v)
}

/// [`display_transform_ws`] with a per-channel function applied to the
/// linear light first, in the same pass.
///
/// What HDR monitoring's preview needs: the roll-off
/// ([`crate::hdr_view::hdr_to_sdr_preview`]) belongs on the light before
/// the display encode, and doing it as a separate pass costs a copy of
/// the frame — 30 ms at 4K, which is more than the encode.
pub fn display_transform_ws_pre(
    linear_rgba: &[f32],
    tone_map_op: ToneMapOp,
    working_space: klipp_core::WorkingColorspace,
    pre: impl Fn(f32) -> f32 + Sync + Send + Copy,
) -> Vec<u8> {
    display_transform_ws_pre_to(linear_rgba, tone_map_op, working_space, pre, klipp_core::SdrTransfer::Srgb)
}

/// [`display_transform_ws`] encoding for `transfer` — the SDR viewer's
/// monitor. The ACES path is display-referred already (its ODT encodes
/// BT.1886) and is left as it is.
pub fn display_transform_ws_to(
    linear_rgba: &[f32],
    tone_map_op: ToneMapOp,
    working_space: klipp_core::WorkingColorspace,
    transfer: klipp_core::SdrTransfer,
) -> Vec<u8> {
    display_transform_ws_pre_to(linear_rgba, tone_map_op, working_space, |v| v, transfer)
}

pub fn display_transform_ws_pre_to(
    linear_rgba: &[f32],
    tone_map_op: ToneMapOp,
    working_space: klipp_core::WorkingColorspace,
    pre: impl Fn(f32) -> f32 + Sync + Send + Copy,
    transfer: klipp_core::SdrTransfer,
) -> Vec<u8> {
    display_transform_ws_quantised(linear_rgba, tone_map_op, working_space, pre, transfer, false)
}

/// The viewer's encode: [`display_transform_ws_to`] with the export's
/// dither added before the rounding (#2072), as the GPU's display encodes
/// add it. A smooth gradient that crosses few code values then breaks up
/// instead of showing bands, and the viewer's CPU route draws the GPU
/// route's bytes. Values already on a code value — an 8-bit picture
/// shown as it is — are not dithered ([`quantise`]).
pub fn display_transform_ws_dithered_to(
    linear_rgba: &[f32],
    tone_map_op: ToneMapOp,
    working_space: klipp_core::WorkingColorspace,
    transfer: klipp_core::SdrTransfer,
) -> Vec<u8> {
    display_transform_ws_quantised(linear_rgba, tone_map_op, working_space, |v| v, transfer, true)
}

/// [`display_transform_ws_dithered_to`] with a per-channel function on the
/// linear light first, as [`display_transform_ws_pre`] takes one: HDR
/// monitoring's preview, dithered as the GPU's preview encode is.
pub fn display_transform_ws_pre_dithered(
    linear_rgba: &[f32],
    tone_map_op: ToneMapOp,
    working_space: klipp_core::WorkingColorspace,
    pre: impl Fn(f32) -> f32 + Sync + Send + Copy,
) -> Vec<u8> {
    display_transform_ws_quantised(linear_rgba, tone_map_op, working_space, pre, klipp_core::SdrTransfer::Srgb, true)
}

fn display_transform_ws_quantised(
    linear_rgba: &[f32],
    tone_map_op: ToneMapOp,
    working_space: klipp_core::WorkingColorspace,
    pre: impl Fn(f32) -> f32 + Sync + Send + Copy,
    transfer: klipp_core::SdrTransfer,
    dither: bool,
) -> Vec<u8> {
    use klipp_core::WorkingColorspace;
    use rayon::prelude::*;
    assert!(
        linear_rgba.len().is_multiple_of(4),
        "display_transform_ws requires RGBA-packed input",
    );
    if let WorkingColorspace::Rec709Linear | WorkingColorspace::Bt2020Linear = working_space {
        return display_transform_from_pre_to(
            linear_rgba,
            tone_map_op,
            ToBt709::for_working_space(working_space),
            pre,
            transfer,
            dither,
        );
    }
    // AcesCG: ACES Output Device Transform path.
    let mut out = vec![0u8; linear_rgba.len()];
    out.par_chunks_exact_mut(4)
        .zip(linear_rgba.par_chunks_exact(4))
        .enumerate()
        .for_each(|(i, (dst, chunk))| {
            // `acescg_to_rec709_sdr` chains AP1→AP0, RRT, Rec.709 ODT,
            // and BT.1886 inverse — output is `[0, 1]` display-encoded
            // Rec.709. Quantise to u8 here at the same precision as
            // the legacy path.
            let acescg = [pre(chunk[0]), pre(chunk[1]), pre(chunk[2])];
            let encoded = crate::aces::acescg_to_rec709_sdr(acescg);
            let n = if dither { dither_at(i) } else { 0.0 };
            dst[0] = quantise(encoded[0], n, 255.0) as u8;
            dst[1] = quantise(encoded[1], n, 255.0) as u8;
            dst[2] = quantise(encoded[2], n, 255.0) as u8;
            dst[3] = quantise(chunk[3], 0.0, 255.0) as u8;
        });
    let _ = tone_map_op; // Intentionally unused in the ACES path.
    out
}

/// The display transform of an **already 8-bit** frame, as one 256-entry
/// per-channel table.
///
/// A frame that is already sRGB-encoded 8-bit — the render cache holds its
/// frames that way — has 256 possible values per channel, so the whole
/// chain of decode to linear, tone map, and encode back has 256 possible
/// answers. Composing it into a table is not an approximation of that
/// chain: it *is* that chain, evaluated ahead of time, and
/// `the_u8_table_is_the_float_path` pins the two byte for byte.
///
/// `None` when the transform is not per-channel, and so cannot be a table:
/// a working space that needs a matrix into BT.709 (BT.2020, with its gamut
/// compression, or ACEScg through its ODT), or [`ToneMapOp::Aces`], whose
/// fit sandwiches its curve between two matrices. Those callers have to go
/// through linear light.
pub fn display_transform_u8_lut(
    tone_map_op: ToneMapOp,
    working_space: klipp_core::WorkingColorspace,
) -> Option<[u8; 256]> {
    display_transform_u8_lut_to(tone_map_op, working_space, klipp_core::SdrTransfer::Srgb)
}

/// [`display_transform_u8_lut`] encoding for `transfer`.
pub fn display_transform_u8_lut_to(
    tone_map_op: ToneMapOp,
    working_space: klipp_core::WorkingColorspace,
    transfer: klipp_core::SdrTransfer,
) -> Option<[u8; 256]> {
    if working_space != klipp_core::WorkingColorspace::Rec709Linear {
        return None;
    }
    if matches!(tone_map_op, ToneMapOp::Aces) {
        return None;
    }
    let mut lut = [0u8; 256];
    for (code, slot) in lut.iter_mut().enumerate() {
        // The same three steps `display_transform_from` takes per pixel,
        // on the linear value this code decodes to.
        let linear = srgb_to_linear(code as f32 / 255.0);
        let mapped = tone_map(linear.max(0.0), tone_map_op);
        *slot = (encode_sdr(mapped, transfer).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    }
    Some(lut)
}

#[cfg(test)]
mod sdr_transfer_tests {
    use super::*;
    use klipp_core::{SdrTransfer, WorkingColorspace};

    /// BT.1886 and sRGB agree at black and white and differ between,
    /// most in the shadows — mid grey is 0.489 against sRGB's 0.461.
    #[test]
    fn bt1886_is_gamma_2_4_and_meets_srgb_only_at_the_ends() {
        assert_eq!(encode_sdr(0.0, SdrTransfer::Bt1886), 0.0);
        assert!((encode_sdr(1.0, SdrTransfer::Bt1886) - 1.0).abs() < 1e-6);
        assert!((encode_sdr(0.18, SdrTransfer::Bt1886) - 0.4894).abs() < 1e-3);
        assert!((encode_sdr(0.18, SdrTransfer::Srgb) - 0.4613).abs() < 1e-3);
        let frame = [0.18f32, 0.18, 0.18, 1.0, 0.01, 0.01, 0.01, 1.0];
        let srgb = display_transform_ws_to(&frame, ToneMapOp::None, WorkingColorspace::Rec709Linear, SdrTransfer::Srgb);
        let bt = display_transform_ws_to(&frame, ToneMapOp::None, WorkingColorspace::Rec709Linear, SdrTransfer::Bt1886);
        assert!(bt[0] > srgb[0] && bt[4] > srgb[4], "BT.1886 shows the shadows lighter than sRGB on the same monitor: {bt:?} vs {srgb:?}");
        // The table path is the float path for the chosen transfer.
        let lut = display_transform_u8_lut_to(ToneMapOp::None, WorkingColorspace::Rec709Linear, SdrTransfer::Bt1886).unwrap();
        let code = (linear_to_srgb(0.18) * 255.0 + 0.5) as usize;
        assert_eq!(lut[code], bt[0]);
    }
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
///   When `working_space` is [`klipp_core::WorkingColorspace::Rec709Linear`], the input is converted
///   to AcesCG (BT.709 → AP1 in linear) before the ODT — the ODT
///   chain assumes AcesCG-basis input. This is documented loud rather
///   than silent.
/// * [`OdtChoice::Rec2100Pq1000`] / [`OdtChoice::Rec2100Pq4000`] —
///   HDR10 PQ at 1000 / 4000 nit peak luminance with BT.2020
///   primaries. Same BT.709 → AP1 conversion when the project
///   working space is Rec.709-linear.
/// * [`OdtChoice::DciP3D65_48`] — DCI-P3 D65 theatrical at 48 nit.
///   Same conversion rule.
/// * [`OdtChoice::Rec2020Sdr`] — the Rec.709 video transform onto
///   BT.2020 primaries (RRT + Rec.2020 100-nit dim ODT + BT.1886
///   inverse). Same conversion rule.
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
    export_pixels(linear_rgba, tone_map_op, working_space, odt,
                  |v, _| quantise(v, 0.0, 255.0) as u8)
}

/// [`display_transform_for_export`] at 16 bits per channel, for 10-bit
/// and deeper deliveries. The same per-pixel maths, only quantised to
/// 65535 instead of 255 — a 10-bit export built from the 8-bit output
/// would carry 8 bits of data in 10-bit containers.
pub fn display_transform_for_export_u16(
    linear_rgba: &[f32],
    tone_map_op: ToneMapOp,
    working_space: klipp_core::WorkingColorspace,
    odt: OdtChoice,
) -> Vec<u16> {
    export_pixels(linear_rgba, tone_map_op, working_space, odt,
                  |v, _| quantise(v, 0.0, 65535.0) as u16)
}

/// The export display transform for one pixel: linear working-space RGB
/// in, display-encoded RGB in `[0, 1]` out. Built once per export from its
/// settings; [`display_transform_for_export`] and its 16-bit twin apply it
/// to whole frames, and the encoder applies it on the way into Y'CbCr.
#[derive(Clone, Copy)]
pub struct ExportDisplay {
    tone_map_op: ToneMapOp,
    odt: OdtChoice,
    path: ExportPath,
}

#[derive(Clone, Copy)]
enum ExportPath {
    /// Tone map + sRGB encode, the working space's basis brought into
    /// BT.709 first when it is not already there.
    ToneMapSrgb(Option<ToBt709>),
    AcesRec709,
    Odt(Option<[[f32; 3]; 3]>),
    /// An HDR delivery's display light (1.0 = 203 cd/m², see
    /// `crate::hdr`): to BT.2020, the peak held, PQ-encoded.
    Pq { to_2020: [[f32; 3]; 3], peak_nits: f32 },
    /// The same, HLG-encoded on the nominal 1000-nit display.
    Hlg { to_2020: [[f32; 3]; 3] },
}

impl ExportDisplay {
    /// MatchViewer follows the viewer (`display_transform_ws`): tone map +
    /// sRGB for BT.709/BT.2020-linear projects, the ACES Rec.709 output for
    /// AcesCG ones, and a BT.2020-linear project is brought into BT.709
    /// first, gamut-mapping what BT.709 cannot hold. Srgb is the legacy
    /// path and converts nothing. Unknown aliases MatchViewer for
    /// forwards-compat.
    ///
    /// The remaining variants are AcesCG-basis ODTs; a project that is not
    /// already in AP1 walks there first, or the ODT would see foreign
    /// values and shift hue.
    pub fn new(tone_map_op: ToneMapOp, working_space: klipp_core::WorkingColorspace, odt: OdtChoice) -> Self {
        use klipp_core::WorkingColorspace;
        let to_709 = || ToBt709::for_working_space(working_space);
        let path = match odt {
            OdtChoice::MatchViewer | OdtChoice::Unknown => match working_space {
                WorkingColorspace::Rec709Linear | WorkingColorspace::Bt2020Linear => {
                    ExportPath::ToneMapSrgb(to_709())
                }
                _ => ExportPath::AcesRec709,
            },
            // The legacy escape hatch: the sRGB curve over the working
            // values, with no ACES RRT. What it exists for is reproducing
            // an export written before delivery-target ODTs did — so what
            // it owes byte-identity to is the working spaces that existed
            // then, and nothing else.
            //
            // Skipping the RRT is a choice about tone; skipping the
            // primaries conversion is a choice about basis. This path used
            // to make both, everywhere, which in a wide-gamut project
            // wrote those primaries out under an sRGB label.
            OdtChoice::Srgb => match working_space {
                // BT.2020 became a working space long after the ODTs, so
                // no export predating them can have come from a BT.2020
                // project. There is no old file here to reproduce, and
                // BT.2020 primaries under an sRGB label have nothing to
                // recommend them, so this converts — the same picture
                // MatchViewer gives, which is why the picker does not
                // offer both (see `OdtChoice::suits_working_space`).
                WorkingColorspace::Bt2020Linear => ExportPath::ToneMapSrgb(to_709()),
                // Rec.709, where converting is a no-op, and AcesCG, which
                // did predate the ODTs — an AcesCG project could have been
                // exported this way in April 2026, and this is how that
                // file is reproduced. Both keep the legacy bytes.
                WorkingColorspace::Rec709Linear | WorkingColorspace::AcesCG => {
                    ExportPath::ToneMapSrgb(None)
                }
            },
            _ => ExportPath::Odt(crate::primaries::working_space_conversion_matrix(
                working_space,
                WorkingColorspace::AcesCG,
            )),
        };
        Self { tone_map_op, odt, path }
    }

    /// An HLG or PQ delivery of an HDR render (display light, 1.0 = SDR
    /// white at 203 cd/m²): BT.2020 primaries, the transfer's encode, the
    /// picture held at `peak_nits`. An SDR `delivery` gives the viewer's
    /// sRGB encode.
    pub fn hdr(working_space: klipp_core::WorkingColorspace, delivery: klipp_core::HdrMode, peak_nits: f32) -> Self {
        let to_2020 = crate::primaries::working_to_bt2020(working_space);
        let path = match delivery {
            klipp_core::HdrMode::Pq => ExportPath::Pq { to_2020, peak_nits },
            klipp_core::HdrMode::Hlg => ExportPath::Hlg { to_2020 },
            klipp_core::HdrMode::Sdr => ExportPath::ToneMapSrgb(ToBt709::for_working_space(working_space)),
        };
        Self { tone_map_op: ToneMapOp::None, odt: OdtChoice::MatchViewer, path }
    }

    /// The tone map, when this transform is *only* a tone map and an sRGB
    /// encode — no basis change, no gamut map, no ODT.
    ///
    /// `None` for everything else, and a caller with a route of its own
    /// for the simple case then knows not to take it on a transform that
    /// is more than this one.
    ///
    /// It no longer means "the GPU cannot do the rest" — it once did, and
    /// said so here. The export's shader has the ACES ODTs
    /// ([`Self::acescg_basis_odt`]), the BT.2407 gamut map
    /// ([`Self::bt2020_gamut_then_tone_map_srgb`]) and the HDR deliveries
    /// ([`Self::hdr_transfer_and_peak`]) now, each behind its own
    /// accessor, so a caller asks for the one it means rather than
    /// inferring capability from this one answering `None`.
    ///
    /// `ToneMapOp::Aces` still answers `Some`: it is separable arithmetic
    /// like the others. It is up to the caller whether its shader
    /// implements every operator.
    #[inline]
    pub fn tone_map_and_srgb_only(&self) -> Option<ToneMapOp> {
        match self.path {
            ExportPath::ToneMapSrgb(None) => Some(self.tone_map_op),
            _ => None,
        }
    }

    /// A tone map and an sRGB encode with the BT.2407 gamut compression
    /// into BT.709 in front of it, and the operator to apply between the
    /// two — what a BT.2020 project gets from `OdtChoice::MatchViewer`.
    ///
    /// `None` for every other delivery, including a BT.709 project's
    /// (nothing to compress) and the legacy Srgb escape hatch. The GPU
    /// has this transform (`SHADER_LINEAR_TO_YUV420`'s gamut branch,
    /// reading the tables `klipp_color::gamut` bakes), so a caller with a
    /// faster route can take it.
    ///
    /// `ToBt709::for_working_space` sets `compress` only for
    /// `Bt2020Linear`, so a `Some` here also fixes which matrix and which
    /// reach tables the caller must supply —
    /// `bt2020_gamut_is_the_only_compressing_export` holds that.
    #[inline]
    pub fn bt2020_gamut_then_tone_map_srgb(&self) -> Option<ToneMapOp> {
        match self.path {
            ExportPath::ToneMapSrgb(Some(t)) if t.compress => Some(self.tone_map_op),
            _ => None,
        }
    }

    /// A PQ or HLG delivery of an HDR render, and the master's peak in
    /// cd/m² — `ExportDisplay::hdr`'s two HDR paths.
    ///
    /// `Some((true, peak))` for PQ, `Some((false, 1000.0))` for HLG. The
    /// GPU has both (`SHADER_LINEAR_TO_YUV420`'s HDR branch, reading the
    /// basis matrix and the PQ table `klipp_color::hdr` bakes), so a
    /// caller with a faster route can take it — including the MaxCLL and
    /// MaxFALL a PQ delivery needs, which the shader measures while it
    /// encodes.
    ///
    /// `None` for every SDR delivery and for the ACES PQ *ODTs*: those
    /// are `ExportPath::Odt`, a different transform that reaches PQ
    /// through the RRT and an SSTS rather than from display light.
    #[inline]
    pub fn hdr_transfer_and_peak(&self) -> Option<(bool, f32)> {
        match self.path {
            ExportPath::Pq { peak_nits, .. } => Some((true, peak_nits)),
            ExportPath::Hlg { .. } => Some((false, crate::hdr::HLG_NOMINAL_PEAK_NITS)),
            _ => None,
        }
    }

    /// An AcesCG-basis output transform, when the delivery is one:
    /// `ExportPath::Odt`, which `OdtChoice` reaches for every choice but
    /// MatchViewer and the legacy Srgb.
    ///
    /// The GPU has all four (`SHADER_ACES_REC709_U8`'s `acescg_to_display`,
    /// off the blob `aces::aces_odt_gpu_data` bakes), so a caller with a
    /// faster route can take it. The project's own basis is *not* part of
    /// the answer: it goes into the blob separately, since the transform
    /// is defined on AP1.
    ///
    /// `None` for the two `ToneMapSrgb` deliveries and for the HDR ones,
    /// which reach PQ from display light rather than through the RRT.
    #[inline]
    pub fn acescg_basis_odt(&self) -> Option<crate::aces::GpuOdt> {
        use crate::aces::GpuOdt;
        if !matches!(self.path, ExportPath::Odt(_)) {
            return None;
        }
        Some(match self.odt {
            OdtChoice::Rec709Sdr => GpuOdt::Rec709Sdr,
            OdtChoice::Rec2100Pq1000 => GpuOdt::Rec2100Pq1000,
            OdtChoice::Rec2100Pq4000 => GpuOdt::Rec2100Pq4000,
            OdtChoice::DciP3D65_48 => GpuOdt::DciP3D65_48,
            OdtChoice::Rec2020Sdr => GpuOdt::Rec2020Sdr,
            // `ExportPath::Odt` is unreachable for these three — they
            // take the ToneMapSrgb or AcesRec709 paths — so there is no
            // ODT to name.
            OdtChoice::MatchViewer | OdtChoice::Srgb | OdtChoice::Unknown => return None,
        })
    }

    /// Whether this is the plain ACES Rec.709 SDR output transform —
    /// what an AcesCG project gets from `OdtChoice::MatchViewer`.
    ///
    /// The GPU has this one in full (`SHADER_ACES_REC709_U8`), matched
    /// to `acescg_to_rec709_sdr` within a code value, so a caller with
    /// a faster route can take it. The AcesCG-basis ODTs
    /// (`ExportPath::Odt`) do not have a twin yet and answer `false`.
    #[inline]
    pub fn is_aces_rec709(&self) -> bool {
        matches!(self.path, ExportPath::AcesRec709)
    }

    /// Display-encoded RGB, clamped to `[0, 1]`.
    #[inline]
    pub fn encode(&self, rgb: [f32; 3]) -> [f32; 3] {
        self.encode_with(rgb, false).0
    }

    /// [`encode`](Self::encode), and for a PQ output the light the pixel
    /// will show, cd/m², by its brightest channel — what HDR10's MaxCLL /
    /// MaxFALL are made of. 0 for other outputs.
    #[inline]
    pub fn encode_light(&self, rgb: [f32; 3]) -> ([f32; 3], f32) {
        self.encode_with(rgb, true)
    }

    #[inline(always)]
    fn encode_with(&self, rgb: [f32; 3], want_light: bool) -> ([f32; 3], f32) {
        let mut light = 0.0;
        let e = match self.path {
            ExportPath::ToneMapSrgb(to_709) => {
                let rgb = match to_709 {
                    Some(c) => c.apply(rgb),
                    None => rgb,
                };
                // Clamp negatives before the tone map: ACES' matrix path
                // amplifies small negatives into bright positives, and a
                // wide-gamut colour outside BT.709 arrives negative here.
                let m = tone_map_rgb([rgb[0].max(0.0), rgb[1].max(0.0), rgb[2].max(0.0)], self.tone_map_op);
                [linear_to_srgb(m[0]), linear_to_srgb(m[1]), linear_to_srgb(m[2])]
            }
            ExportPath::AcesRec709 => crate::aces::acescg_to_rec709_sdr(rgb),
            ExportPath::Odt(m) => {
                let ap1 = match m {
                    Some(m) => crate::primaries::matrix3_mul_vec(m, rgb),
                    None => rgb,
                };
                let e = match self.odt {
                    OdtChoice::Rec709Sdr => crate::aces::acescg_to_rec709_sdr(ap1),
                    OdtChoice::Rec2100Pq1000 => crate::aces::acescg_to_rec2100_pq_1000_nit(ap1),
                    OdtChoice::Rec2100Pq4000 => crate::aces::acescg_to_rec2100_pq_4000_nit(ap1),
                    OdtChoice::DciP3D65_48 => crate::aces::acescg_to_dcip3_d65_48nit(ap1),
                    OdtChoice::Rec2020Sdr => crate::aces::acescg_to_rec2020_sdr(ap1),
                    OdtChoice::MatchViewer | OdtChoice::Srgb | OdtChoice::Unknown => ap1,
                };
                if want_light && self.odt.is_pq() {
                    light = crate::hdr::pq_decode_nits(e[0].max(e[1]).max(e[2]).clamp(0.0, 1.0));
                }
                e
            }
            ExportPath::Pq { to_2020, peak_nits } => {
                let nits = crate::hdr::display_light_to_nits(crate::primaries::matrix3_mul_vec(to_2020, rgb), peak_nits);
                light = nits[0].max(nits[1]).max(nits[2]);
                nits.map(crate::hdr::pq_encode_nits)
            }
            ExportPath::Hlg { to_2020 } => crate::hdr::hlg_encode_nits(crate::hdr::display_light_to_nits(
                crate::primaries::matrix3_mul_vec(to_2020, rgb),
                crate::hdr::HLG_NOMINAL_PEAK_NITS,
            )),
        };
        ([e[0].clamp(0.0, 1.0), e[1].clamp(0.0, 1.0), e[2].clamp(0.0, 1.0)], light)
    }
}

/// The export display transform over a frame, quantised by `q`.
fn export_pixels<T: Copy + Default + Send + Sync>(
    linear_rgba: &[f32],
    tone_map_op: ToneMapOp,
    working_space: klipp_core::WorkingColorspace,
    odt: OdtChoice,
    q: impl Fn(f32, f32) -> T + Sync,
) -> Vec<T> {
    ExportDisplay::new(tone_map_op, working_space, odt).frame(linear_rgba, q)
}

/// The export's dither and its rounding: [`klipp_effects::dither`], where
/// the GPU's display encodes take the same pattern from, so the viewer and
/// the export round a picture the same way (#2072).
pub use klipp_effects::dither::{dither_at, quantise};

/// A channel's display value (0 to 1, before quantising) from its linear
/// light, where [`display_transform_ws_dithered_to`] works channel by
/// channel: a BT.709 working space and a tone map of each channel alone
/// (not ACES, which mixes them). `None` otherwise. With [`dither_at`] and
/// [`quantise`] it gives the same bytes, so a picture whose channels take
/// few values can be encoded through a table.
pub fn per_channel_display(tone_map_op: ToneMapOp, working_space: klipp_core::WorkingColorspace, transfer: klipp_core::SdrTransfer) -> Option<impl Fn(f32) -> f32 + Sync + Send + Copy> {
    if working_space != klipp_core::WorkingColorspace::Rec709Linear || tone_map_op == ToneMapOp::Aces || ToBt709::for_working_space(working_space).is_some() {
        return None;
    }
    Some(move |v: f32| encode_sdr(tone_map(v.max(0.0), tone_map_op), transfer))
}

impl ExportDisplay {
    /// The transform over an RGBA frame, each channel quantised by `q`.
    ///
    /// `q` is handed the encoded channel and a dither offset in output
    /// code values, and is expected to add it before rounding. Alpha
    /// passes through untransformed and undithered.
    pub fn frame<T: Copy + Default + Send + Sync>(&self, linear_rgba: &[f32], q: impl Fn(f32, f32) -> T + Sync) -> Vec<T> {
        use rayon::prelude::*;
        assert!(
            linear_rgba.len().is_multiple_of(4),
            "display_transform_for_export requires RGBA-packed input",
        );
        let d = *self;
        let mut out = vec![T::default(); linear_rgba.len()];
        out.par_chunks_exact_mut(4)
            .zip(linear_rgba.par_chunks_exact(4))
            .enumerate()
            .for_each(|(i, (dst, chunk))| {
                let e = d.encode([chunk[0], chunk[1], chunk[2]]);
                let n = dither_at(i);
                dst[0] = q(e[0], n);
                dst[1] = q(e[1], n);
                dst[2] = q(e[2], n);
                dst[3] = q(chunk[3], 0.0);
            });
        out
    }
}

#[cfg(test)]
mod tests {

    /// #3212: the per-channel display value, dithered and quantised as the
    /// encode does it, gives the encode's own bytes, for each per-channel
    /// tone map and both transfers; ACES and a wider working space have
    /// none.
    #[test]
    fn the_per_channel_display_value_gives_the_encodes_bytes() {
        use klipp_core::{SdrTransfer, WorkingColorspace};
        let frame: Vec<f32> = (0..4096).flat_map(|i| [i as f32 / 2000.0, (i % 300) as f32 / 150.0, 0.02, 1.0]).collect();
        for tone in [ToneMapOp::None, ToneMapOp::Reinhard, ToneMapOp::Hable] {
            for transfer in [SdrTransfer::Srgb, SdrTransfer::Bt1886] {
                let f = per_channel_display(tone, WorkingColorspace::Rec709Linear, transfer).expect("per channel");
                let want = display_transform_ws_dithered_to(&frame, tone, WorkingColorspace::Rec709Linear, transfer);
                let got: Vec<u8> = frame
                    .chunks_exact(4)
                    .enumerate()
                    .flat_map(|(i, p)| {
                        let n = dither_at(i);
                        [quantise(f(p[0]), n, 255.0) as u8, quantise(f(p[1]), n, 255.0) as u8, quantise(f(p[2]), n, 255.0) as u8, quantise(p[3], 0.0, 255.0) as u8]
                    })
                    .collect();
                assert_eq!(got, want, "{tone:?} {transfer:?}");
            }
        }
        assert!(per_channel_display(ToneMapOp::Aces, WorkingColorspace::Rec709Linear, SdrTransfer::Srgb).is_none());
        assert!(per_channel_display(ToneMapOp::None, WorkingColorspace::Bt2020Linear, SdrTransfer::Srgb).is_none());
        assert!(per_channel_display(ToneMapOp::None, WorkingColorspace::AcesCG, SdrTransfer::Srgb).is_none());
    }
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
                (-1e-5..1.3).contains(&y),
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
            // `v <= 255` was the assertion here, and `v` is a u8 — true of
            // every possible value, so the test could not fail. Worse, it
            // could not tell Hable apart from no tone map at all: the
            // sibling test below asserts that path gives exactly 255.
            //
            // Rolling off rather than clipping is the whole claim, so say
            // it: a 5.0 pixel lands high but strictly under the clipping
            // point. Measured at 252.
            assert!(
                v > 200,
                "a 5.0 linear pixel should still read as a bright highlight, got {v} — \
                 a low value here would be the u8 wrap-around this test is named for",
            );
            assert!(
                v < 255,
                "Hable clipped at {v} instead of rolling off; that is what \
                 ToneMapOp::None does, so the tone map is not being applied",
            );
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
    fn an_sdr_view_of_a_bt2020_project_converts_into_bt709() {
        use klipp_core::WorkingColorspace::{Bt2020Linear, Rec709Linear};
        let view = |px: &[f32], ws| display_transform_ws(px, ToneMapOp::Hable, ws);

        // Neutral is neutral in both. BT.2020 and BT.709 share D65, so
        // the matrix leaves grey alone and the two paths agree — the one
        // case where the old aliasing stub was right.
        let grey = vec![0.18, 0.18, 0.18, 1.0];
        assert_eq!(view(&grey, Rec709Linear), view(&grey, Bt2020Linear));

        // A saturated colour is where the stub showed the wrong picture:
        // BT.2020 values encoded as sRGB read as a more saturated colour
        // than they are. Converted, the same values are the colour they
        // describe — a red beyond BT.709's gamut, rendered as the most
        // saturated red BT.709 has.
        let red = vec![0.5, 0.0, 0.0, 1.0];
        let (as_709, as_2020) = (view(&red, Rec709Linear), view(&red, Bt2020Linear));
        assert_ne!(as_709, as_2020, "BT.2020 red still aliases BT.709 red");
        assert!(as_2020[0] > as_709[0], "{as_2020:?} vs {as_709:?}");

        // And the path is exactly the matrix followed by the BT.709 one,
        // for a colour far enough inside both gamuts that the gamut map
        // leaves it alone.
        let px = [0.3, 0.55, 0.12];
        let c = crate::primaries::matrix3_mul_vec(crate::primaries::bt2020_to_bt709(), px);
        let converted = vec![c[0], c[1], c[2], 1.0];
        assert_eq!(
            view(&[px[0], px[1], px[2], 1.0], Bt2020Linear),
            view(&converted, Rec709Linear),
        );
    }

    /// The point of the gamut map, at the two places an SDR picture
    /// leaves a BT.2020 project: two reds that BT.709 cannot hold, and
    /// that a clip would render as the same pixel, come out as different
    /// pixels — in the viewer and in an SDR export alike.
    #[test]
    fn wide_colour_an_sdr_clip_would_flatten_stays_distinct() {
        use klipp_core::WorkingColorspace::Bt2020Linear;
        // Both are outside BT.709: the matrix sends their green and blue
        // negative, and clamping there lands both on pure red.
        let (a, b) = ([0.75f32, 0.12, 0.04], [1.0f32, 0.0, 0.0]);
        for (which, encode) in [
            ("viewer", Box::new(|px: [f32; 3]| {
                let out = display_transform_ws(&[px[0], px[1], px[2], 1.0], ToneMapOp::None, Bt2020Linear);
                [out[0], out[1], out[2]]
            }) as Box<dyn Fn([f32; 3]) -> [u8; 3]>),
            ("sdr export", Box::new(|px: [f32; 3]| {
                let e = ExportDisplay::new(ToneMapOp::None, Bt2020Linear, OdtChoice::MatchViewer).encode(px);
                e.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
            })),
        ] {
            let (ea, eb) = (encode(a), encode(b));
            assert_ne!(ea, eb, "{which} flattened {a:?} and {b:?} onto {ea:?}");
            // Not merely different by a code: the difference is visible,
            // and it is the less saturated one that stays less saturated.
            let spread = |c: [u8; 3]| c[0] as i32 - c[1].min(c[2]) as i32;
            assert!(spread(ea) + 8 < spread(eb), "{which}: {ea:?} vs {eb:?}");
        }
    }

    /// `bt2020_gamut_then_tone_map_srgb` answers `Some` for exactly the
    /// deliveries whose gamut compression is BT.2020's.
    ///
    /// A GPU caller reads that `Some` as licence to bind the BT.2020
    /// reach tables and BT.2020's matrix, so if some future working space
    /// also set `ToBt709::compress` it would silently get BT.2020's
    /// numbers. This is the test that would stop agreeing.
    #[test]
    fn bt2020_gamut_is_the_only_compressing_export() {
        use klipp_core::WorkingColorspace as W;
        for ws in [W::Rec709Linear, W::AcesCG, W::Bt2020Linear] {
            for odt in [OdtChoice::MatchViewer, OdtChoice::Srgb, OdtChoice::Unknown] {
                for op in [ToneMapOp::None, ToneMapOp::Hable] {
                    let d = ExportDisplay::new(op, ws, odt);
                    let got = d.bt2020_gamut_then_tone_map_srgb();
                    // Every SDR choice a BT.2020 project can make, Srgb
                    // included: no export predates BT.2020 as a working
                    // space, so there are no legacy bytes to reproduce
                    // and that choice converts too.
                    let want = ws == W::Bt2020Linear;
                    assert_eq!(
                        got.is_some(), want,
                        "{ws:?} / {odt:?} / {op:?}: compressing-export accessor said {got:?}",
                    );
                    if let Some(returned) = got {
                        assert_eq!(returned, op, "the operator to apply after the compression");
                    }
                }
            }
        }
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
        // `OdtChoice::Srgb` is the safety net for users who want the old,
        // pre-ACES export, so it owes byte-identity with the legacy
        // `display_transform` to the working spaces that could have
        // produced such an export: Rec.709, and AcesCG, which predates the
        // delivery-target ODTs by four weeks. BT.2020 came long after and
        // is covered by `the_legacy_path_still_converts_a_bt2020_project`.
        let frame = small_linear_test_frame();
        let legacy = display_transform(&frame, ToneMapOp::Hable);
        for ws in [
            klipp_core::WorkingColorspace::Rec709Linear,
            klipp_core::WorkingColorspace::AcesCG,
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

    /// BT.2020 became a working space long after the delivery-target ODTs,
    /// so there is no pre-ODT export from such a project for the legacy
    /// path to reproduce — and writing BT.2020 primaries out under an sRGB
    /// label only makes an over-saturated file. It converts instead, and
    /// lands on the same picture Match Viewer gives, which is why the
    /// export picker offers one and not both.
    #[test]
    fn the_legacy_path_still_converts_a_bt2020_project() {
        let frame = small_linear_test_frame();
        let ws = klipp_core::WorkingColorspace::Bt2020Linear;
        let unconverted = display_transform(&frame, ToneMapOp::Hable);
        let export = display_transform_for_export(&frame, ToneMapOp::Hable, ws, OdtChoice::Srgb);
        assert_ne!(
            unconverted, export,
            "a BT.2020 project exported through the legacy path was left in BT.2020",
        );
        let match_viewer =
            display_transform_for_export(&frame, ToneMapOp::Hable, ws, OdtChoice::MatchViewer);
        assert_eq!(
            export, match_viewer,
            "the legacy path and Match Viewer should agree once the primaries are converted",
        );
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

    // ── the 8-bit display transform as a table ───────────────────────

    /// The table has to be the float path, not an approximation of it: the
    /// cache-hit viewer path shows its output instead of decoding to linear
    /// and transforming, so a single differing code would change the
    /// picture depending on whether a frame came from the cache.
    #[test]
    fn the_u8_table_is_the_float_path() {
        use klipp_core::WorkingColorspace::Rec709Linear;
        for op in TONE_MAP_OPS_ALL {
            let Some(lut) = display_transform_u8_lut(op, Rec709Linear) else {
                // Only ACES is expected to decline, and it is checked below.
                assert!(matches!(op, ToneMapOp::Aces), "{op:?} should have a table");
                continue;
            };
            // Every code, decoded the way the engine decodes a cached frame,
            // then through the real transform.
            let linear: Vec<f32> = (0..256)
                .flat_map(|c| {
                    let v = crate::transfer::srgb_to_linear(c as f32 / 255.0);
                    [v, v, v, c as f32 / 255.0]
                })
                .collect();
            let want = display_transform_ws(&linear, op, Rec709Linear);
            for code in 0..256usize {
                assert_eq!(
                    lut[code], want[code * 4],
                    "{op:?}: code {code} is {} through the table and {} through linear",
                    lut[code], want[code * 4],
                );
            }
        }
    }

    /// Alpha is left alone by the transform, so a cached frame's alpha needs
    /// no table — the caller copies it. Pinned so a future change to the
    /// alpha handling does not silently make the shortcut wrong.
    #[test]
    fn the_transform_leaves_alpha_where_it_found_it() {
        use klipp_core::WorkingColorspace::Rec709Linear;
        let linear: Vec<f32> = (0..256).flat_map(|c| [0.5, 0.5, 0.5, c as f32 / 255.0]).collect();
        let out = display_transform_ws(&linear, ToneMapOp::None, Rec709Linear);
        for code in 0..256usize {
            assert_eq!(out[code * 4 + 3], code as u8, "alpha {code} changed");
        }
    }

    /// The converse of the test below, and the one the shortcut actually
    /// rests on: a config that *offers* a table has to be genuinely
    /// per-channel.
    ///
    /// The sibling test enumerates the configs expected to decline. That
    /// is a list, not a property — it says nothing about whether the
    /// configs that accept are separable. And `the_u8_table_is_the_float_
    /// path` checks the accepting ones on a *neutral* frame, where
    /// `r == g == b` makes any cross-channel step invisible by
    /// construction. So this feeds colour, where a 3x3 or a gamut hop
    /// cannot hide, and requires each output channel to be the table
    /// applied to that channel's own input byte — which is the only thing
    /// a 256-entry byte LUT can express.
    #[test]
    fn a_config_with_a_table_really_is_per_channel() {
        use klipp_core::WorkingColorspace::*;
        // Saturated primaries, then mixtures: a matrix leaks between
        // channels most on the primaries and a gamut compression bites
        // hardest on the saturated ones.
        const CODES: [[u8; 3]; 7] = [
            [255, 0, 0], [0, 255, 0], [0, 0, 255],
            [200, 40, 90], [30, 180, 240], [10, 10, 250], [250, 250, 8],
        ];
        let mut checked = 0usize;
        for ws in [Rec709Linear, Bt2020Linear, AcesCG] {
            for op in TONE_MAP_OPS_ALL {
                let Some(lut) = display_transform_u8_lut(op, ws) else { continue };
                let linear: Vec<f32> = CODES
                    .iter()
                    .flat_map(|c| {
                        [
                            crate::transfer::srgb_to_linear(c[0] as f32 / 255.0),
                            crate::transfer::srgb_to_linear(c[1] as f32 / 255.0),
                            crate::transfer::srgb_to_linear(c[2] as f32 / 255.0),
                            1.0,
                        ]
                    })
                    .collect();
                let want = display_transform_ws(&linear, op, ws);
                for (i, c) in CODES.iter().enumerate() {
                    for ch in 0..3 {
                        assert_eq!(
                            want[i * 4 + ch], lut[c[ch] as usize],
                            "{ws:?}/{op:?} is not per-channel: {c:?} channel {ch} is {} \
                             through the transform and {} through the table, so the \
                             cache-hit path would show a different picture",
                            want[i * 4 + ch], lut[c[ch] as usize],
                        );
                    }
                }
                checked += 1;
            }
        }
        // If nothing offers a table there is no shortcut to protect, and
        // this test would be passing on an empty loop.
        assert!(checked > 0, "no config offered a table, so nothing was checked");
        eprintln!("{checked} table-offering configs verified separable on colour");
    }

    /// A transform that mixes channels cannot be a per-channel table, and
    /// says so rather than returning a wrong one.
    #[test]
    fn a_transform_that_mixes_channels_has_no_table() {
        use klipp_core::WorkingColorspace::*;
        // ACES' fit sandwiches its curve between two 3x3 matrices.
        assert!(display_transform_u8_lut(ToneMapOp::Aces, Rec709Linear).is_none());
        // BT.2020 needs a matrix into BT.709 and a gamut compression with
        // it; ACEScg goes through an ODT.
        for ws in [Bt2020Linear, AcesCG] {
            for op in TONE_MAP_OPS_ALL {
                assert!(
                    display_transform_u8_lut(op, ws).is_none(),
                    "{ws:?} with {op:?} is not per-channel",
                );
            }
        }
    }
}
