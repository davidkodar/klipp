//! Camera-log inverse OETFs and source-primaries → BT.709 gamut
//! conversion.
//!
//! These curves are not tagged in any container we've seen — Sony,
//! ARRI, Canon, etc. all leave `color_trc=unknown` in the file and
//! rely on the editor / DIT to declare the log format. This module
//! is the CPU mirror of the WGSL helpers in
//! `klipp_effects::color_wgsl`: the math is bit-identical so the
//! GPU and CPU decode paths produce the same linear output for any
//! given source colourspace.
//!
//! Numeric breakpoints and coefficients come from each
//! manufacturer's published technical references. Where the camera
//! vendor doesn't publish a closed-form curve (Blackmagic, GoPro)
//! we use a reasonable approximation flagged in the docstring.

use crate::hdr::{chroma_taps, ChromaAxis};
use klipp_core::{
    ColorMatrix, ColorPrimaries, ColorRange, ColorTransfer, SourceColorspace,
    WorkingColorspace,
};

// ─── Per-transfer inverse OETFs ─────────────────────────────────────
//
// Every curve here takes a *normalised 10-bit code value* — CV / 1023,
// the numbering the manufacturers publish their curves in — and returns
// scene-linear reflectance, 0.18 = mid grey. Each is checked across all
// 1024 codes by tests/camera_log_parity.rs against an independent
// reference: OpenColorIO's built-in manufacturer transforms where they
// exist, colour-science otherwise (itself cross-checked against OCIO on
// the seven formats both cover, to within 2e-5 stops). BMD Film Gen5 is
// in neither and remains unverified.
//
// Several of these used to be wrong by stops — LogC4 by 20, Canon Log 3
// negative at mid grey — and the InputTransform node carried a second,
// separately wrong copy of every curve. There is now one copy, here.

/// CV / 1023 → legal-range normalised, (CV − 64) / 876. Sony S-Log/S-Log2
/// and Canon Log (v1) apply their formulas in this domain.
#[inline]
fn legal_to_full_10bit(c: f32) -> f32 {
    (c * 1023.0 - 64.0) / 876.0
}

/// Sony S-Log3 inverse OETF.
#[inline]
pub fn slog3_to_linear(c: f32) -> f32 {
    let s = c.clamp(0.0, 1.0);
    let cv = s * 1023.0;
    if cv >= 171.210_29 {
        (10.0_f32.powf((cv - 420.0) / 261.5)) * (0.18 + 0.01) - 0.01
    } else {
        (cv - 95.0) * 0.011_25 / (171.210_29 - 95.0)
    }
}

/// Sony S-Log2 inverse OETF: S-Log scaled by 219/155, per Sony's S-Log2
/// technical summary. The S-Log formula itself works on legal-range
/// normalised values.
#[inline]
pub fn slog2_to_linear(c: f32) -> f32 {
    let y = c.clamp(0.0, 1.0);
    let x = legal_to_full_10bit(y);
    // S-Log's code value for scene 0, back in CV/1023 terms: the knee
    // between the linear toe and the log segment.
    let knee = ((0.432_699 * 0.037_584_f32.log10() + 0.616_596 + 0.03) * 876.0 + 64.0) / 1023.0;
    let slog = if y >= knee {
        10.0_f32.powf((x - 0.616_596 - 0.03) / 0.432_699) - 0.037_584
    } else {
        (x - 0.030_001_223) / 5.0
    } * 0.9;
    219.0 * slog / 155.0
}

/// Panasonic V-Log inverse OETF.
#[inline]
pub fn vlog_to_linear(c: f32) -> f32 {
    let s = c.clamp(0.0, 1.0);
    let cut2 = 0.181;
    if s < cut2 {
        (s - 0.125) / 5.6
    } else {
        10.0_f32.powf((s - 0.598_206) / 0.241_514) - 0.008_73
    }
}

/// Canon Log (v1) inverse OETF, per Canon's white paper; applied in the
/// legal-range normalised domain and scaled by 0.9 to scene reflectance.
#[inline]
pub fn canon_log_to_linear(c: f32) -> f32 {
    let x = legal_to_full_10bit(c.clamp(0.0, 1.0));
    let lin = if x < 0.073_059_7 {
        -(10.0_f32.powf((0.073_059_7 - x) / 0.529_136) - 1.0) / 10.159_6
    } else {
        (10.0_f32.powf((x - 0.073_059_7) / 0.529_136) - 1.0) / 10.159_6
    };
    lin * 0.9
}

/// Canon Log 3 inverse OETF, per Canon's white paper and the ACES input
/// transform: three segments, then × 0.9 so 18% grey lands on 0.18.
#[inline]
pub fn canon_log3_to_linear(c: f32) -> f32 {
    let x = c.clamp(0.0, 1.0);
    let lin = if x < 0.097_465_473 {
        -(10.0_f32.powf((0.127_839_01 - x) / 0.367_268_45) - 1.0) / 14.983_25
    } else if x <= 0.152_778_91 {
        (x - 0.125_122_19) / 1.975_479_8
    } else {
        (10.0_f32.powf((x - 0.122_405_37) / 0.367_268_45) - 1.0) / 14.983_25
    };
    lin * 0.9
}

/// ARRI Log C3 (LogC3 EI 800) inverse OETF.
#[inline]
pub fn logc3_to_linear(c: f32) -> f32 {
    let s = c.clamp(0.0, 1.0);
    let cut = 0.149_658;
    if s > cut {
        (10.0_f32.powf((s - 0.385_537) / 0.247_190) - 0.052_272) / 5.555_556
    } else {
        (s - 0.092_809) / 5.367_655
    }
}

/// ARRI LogC4 inverse OETF, per ARRI's LogC4 specification (2022).
#[inline]
pub fn logc4_to_linear(c: f32) -> f32 {
    let e = c.clamp(0.0, 1.0) as f64;
    let a = (262_144.0 - 16.0) / 117.45;
    let b = (1023.0 - 95.0) / 1023.0;
    let cc = 95.0 / 1023.0;
    let s = (7.0 * std::f64::consts::LN_2 * 2.0_f64.powf(7.0 - 14.0 * cc / b)) / (a * b);
    let t = (2.0_f64.powf(14.0 * (-cc / b) + 6.0) - 64.0) / a;
    (if e >= 0.0 {
        (2.0_f64.powf(14.0 * (e - cc) / b + 6.0) - 64.0) / a
    } else {
        e * s + t
    }) as f32
}

/// RED Log3G10 inverse OETF, per RED's white paper (v2 curve, with its
/// 0.01 linear offset).
#[inline]
pub fn red_log3g10_to_linear(c: f32) -> f32 {
    let x = c.clamp(0.0, 1.0);
    let (a, b, off, g) = (0.224_282_f32, 155.975_33_f32, 0.01_f32, 15.192_7_f32);
    if x < 0.0 {
        x / g - off
    } else {
        (10.0_f32.powf(x / a) - 1.0) / b - off
    }
}

/// Blackmagic Film Generation 5 — approximate inverse OETF (BMD
/// doesn't publish a closed-form curve).
#[inline]
pub fn bmd_film_gen5_to_linear(c: f32) -> f32 {
    let s = c.clamp(0.0, 1.0);
    if s < 0.005 { s / 8.0 } else { 10.0_f32.powf((s - 0.385_537) / 0.245) - 0.017_3 }
}

/// DJI D-Log inverse OETF.
#[inline]
pub fn dji_dlog_to_linear(c: f32) -> f32 {
    let s = c.clamp(0.0, 1.0);
    if s <= 0.14 {
        (s - 0.092_9) / 6.025
    } else {
        (10.0_f32.powf((s - 0.584_555) / 0.256_663) - 0.010_8) / 0.989_2
    }
}

/// Fujifilm F-Log inverse OETF, per Fujifilm's F-Log data sheet.
#[inline]
pub fn flog_to_linear(c: f32) -> f32 {
    let x = c.clamp(0.0, 1.0);
    let (a, b, cc, d, e, f) = (0.555_556_f32, 0.009_468_f32, 0.344_676_f32, 0.790_453_f32, 8.735_631_f32, 0.092_864_f32);
    let cut2 = 0.100_537_775_f32;
    if x < cut2 {
        (x - f) / e
    } else {
        10.0_f32.powf((x - d) / cc) / a - b / a
    }
}

/// Apple Log inverse OETF (iPhone 15 Pro+).
///
/// Three-segment curve per the Apple Log Profile whitepaper (2023):
///   V  < 0   → L = R₀                  (negative anchor, sensor toe)
///   V  < Pₜ  → L = √(V/C) + R₀         (quadratic toe matching the
///                                       forward V = C·(L−R₀)²)
///   V ≥ Pₜ  → L = 2^((V−δ)/γ) − β      (log segment)
///
/// Constants pulled directly from Apple's whitepaper. `Pₜ` derives
/// from `C·(Rₜ−R₀)²` so the toe and log pieces meet at L = Rₜ = 0.01
/// exactly even if any constant is retuned later. Mirrors the
/// implementation already present at `transforms.rs::log_to_linear`
/// for `LogFormat::AppleLog` — keeping the two in lock-step.
///
/// History: previous body was `s/357.04167` toe + `2^((s−0.47132)/0.05)·0.01`
/// log — placeholder constants from an early scaffold pass that
/// never got the real numbers. Decoded mid-gray to ≈0.015 instead
/// of 0.18 (off by 13×). Caught in #57 by a parity test against
/// the whitepaper spec values.
#[inline]
pub fn apple_log_to_linear(c: f32) -> f32 {
    const R0:    f32 = -0.05641088;
    const RT:    f32 =  0.01;
    const C:     f32 = 47.28711236;
    const BETA:  f32 =  0.00964052;
    const GAMMA: f32 =  0.08550479;
    const DELTA: f32 =  0.69336945;
    const PT:    f32 = C * (RT - R0) * (RT - R0);
    let v = c.clamp(0.0, 1.0);
    if v < PT {
        (v / C).sqrt() + R0
    } else {
        2.0_f32.powf((v - DELTA) / GAMMA) - BETA
    }
}

/// Nikon N-Log inverse OETF, per Nikon's N-Log specification. Nikon's two
/// published segments meet with a 0.09% step at the breakpoint.
#[inline]
pub fn nlog_to_linear(c: f32) -> f32 {
    let x = c.clamp(0.0, 1.0);
    let (a, b, cc, d) = (0.635_386_12_f32, 0.0075_f32, 0.146_627_57_f32, 0.605_083_1_f32);
    let cut2 = 0.441_837_73_f32;
    if x < cut2 {
        (x / a).powi(3) - b
    } else {
        ((x - d) / cc).exp()
    }
}

/// Leica L-Log inverse OETF, per Leica's L-Log reference.
#[inline]
pub fn leica_llog_to_linear(c: f32) -> f32 {
    let x = c.clamp(0.0, 1.0);
    let (a, b, cc, d, e, f) = (8.0_f32, 0.09_f32, 0.27_f32, 1.3_f32, 0.0115_f32, 0.6_f32);
    if x <= 0.138 {
        (x - b) / a
    } else {
        (10.0_f32.powf((x - f) / cc) - e) / d
    }
}

/// Any `LogFormat` → scene-linear, from a normalised 10-bit code value.
/// The one implementation behind both the decoder and the InputTransform
/// node. Unlike `apply_inverse_eotf` it does not clamp the result at 0:
/// the sub-black toe survives for a grade to work with.
pub fn log_format_to_linear(c: f32, format: klipp_core::LogFormat) -> f32 {
    use klipp_core::LogFormat as L;
    match format {
        L::Srgb | L::Linear => c,
        L::SLog3 => slog3_to_linear(c),
        L::SLog2 => slog2_to_linear(c),
        L::LogC3 => logc3_to_linear(c),
        L::LogC4 => logc4_to_linear(c),
        L::CanonLog => canon_log_to_linear(c),
        L::CanonLog3 => canon_log3_to_linear(c),
        L::VLog => vlog_to_linear(c),
        L::NLog => nlog_to_linear(c),
        L::FLog => flog_to_linear(c),
        L::DJIDLog => dji_dlog_to_linear(c),
        L::GoProFlat => gopro_flat_to_linear(c),
        L::AppleLog => apple_log_to_linear(c),
        L::LeicaLLog | L::LeicaLLogBt709 => leica_llog_to_linear(c),
        L::RedLog3G10 => red_log3g10_to_linear(c),
        L::BmdFilmGen5 => bmd_film_gen5_to_linear(c),
        L::HLG => hlg_to_linear(c),
        L::PQ => pq_to_linear(c),
    }
}

/// GoPro Protune (Flat) inverse OETF.
#[inline]
pub fn gopro_flat_to_linear(c: f32) -> f32 {
    (113.0_f32.powf(c.clamp(0.0, 1.0)) - 1.0) / 112.0
}

/// Apply the inverse OETF for a given [`ColorTransfer`]. Output is
/// linear-light in the source's native primaries.
///
/// **Camera-log curves are clamped to non-negative output.** Sony's
/// S-Log3, ARRI's LogC3/4, RED's Log3G10 and friends all define a
/// "foot" — a small linear segment below which the spec extends to
/// negative linear values. Those negatives represent the camera's
/// noise floor (signal weaker than what the sensor could meaningfully
/// reproduce), not real scene light. Letting them propagate into the
/// gamut-conversion matrix produces a strong colour cast on dark
/// scenes (the matrix's off-diagonal coefficients amplify per-channel
/// disagreement, so e.g. a black pixel with channels [0, -0.014,
/// -0.014] becomes red after every channel passes through the matrix
/// and gets clamped at 0). Mapping below-foot signals to 0 instead is
/// what every NLE's log decoder does in practice.
pub fn apply_inverse_eotf(c: f32, transfer: ColorTransfer) -> f32 {
    match transfer {
        ColorTransfer::Linear => c,
        ColorTransfer::Pq => pq_to_linear(c).max(0.0),
        ColorTransfer::Hlg => hlg_to_linear(c).max(0.0),
        ColorTransfer::Gamma22 => c.max(0.0).powf(2.2),
        ColorTransfer::Gamma28 => c.max(0.0).powf(2.8),
        ColorTransfer::SLog3 => slog3_to_linear(c).max(0.0),
        ColorTransfer::SLog2 => slog2_to_linear(c).max(0.0),
        ColorTransfer::VLog => vlog_to_linear(c).max(0.0),
        ColorTransfer::CanonLog => canon_log_to_linear(c).max(0.0),
        ColorTransfer::CanonLog3 => canon_log3_to_linear(c).max(0.0),
        ColorTransfer::LogC3 => logc3_to_linear(c).max(0.0),
        ColorTransfer::LogC4 => logc4_to_linear(c).max(0.0),
        ColorTransfer::RedLog3G10 => red_log3g10_to_linear(c).max(0.0),
        ColorTransfer::BmdFilmGen5 => bmd_film_gen5_to_linear(c).max(0.0),
        ColorTransfer::DJIDLog => dji_dlog_to_linear(c).max(0.0),
        ColorTransfer::FLog => flog_to_linear(c).max(0.0),
        ColorTransfer::AppleLog => apple_log_to_linear(c).max(0.0),
        ColorTransfer::LeicaLLog => leica_llog_to_linear(c).max(0.0),
        ColorTransfer::NLog => nlog_to_linear(c).max(0.0),
        ColorTransfer::GoProFlat => gopro_flat_to_linear(c).max(0.0),
        // BT.709 / BT.601 / BT.2020 / sRGB / SMPTE 240 / xvYCC /
        // BT.1361 / SMPTE 428 — all close enough to sRGB at u8
        // precision.
        _ => crate::transfer::srgb_to_linear(c),
    }
}

/// PQ (SMPTE ST 2084) inverse OETF.
#[inline]
pub fn pq_to_linear(c: f32) -> f32 {
    let m1 = 0.159_301_76;
    let m2 = 78.84375;
    let c1 = 0.835_937_5;
    let c2 = 18.851_562_5;
    let c3 = 18.6875;
    let p = c.max(0.0).powf(1.0 / m2);
    let num = (p - c1).max(0.0);
    let den = (c2 - c3 * p).max(1e-6);
    (num / den).powf(1.0 / m1)
}

/// PQ (SMPTE ST 2084) forward OETF — linear light to PQ codepoint.
///
/// Input `l_norm` is luminance in `[0, 1]` where `1.0` corresponds
/// to the PQ peak of 10000 cd/m² (so 1000 cd/m² → input 0.1).
/// Output is the PQ-encoded codepoint in `[0, 1]`.
///
/// This is the inverse of [`pq_to_linear`]; round-tripping through
/// both should be near-identity to f32 precision. ACES Output Device
/// Transforms targeting Rec.2100 PQ deliveries use this as the final
/// encoding step.
#[inline]
pub fn linear_to_pq(l_norm: f32) -> f32 {
    let m1 = 0.159_301_76;
    let m2 = 78.84375;
    let c1 = 0.835_937_5;
    let c2 = 18.851_562_5;
    let c3 = 18.6875;
    let yp = l_norm.max(0.0).powf(m1);
    ((c1 + c2 * yp) / (1.0 + c3 * yp)).powf(m2)
}

/// HLG (ARIB STD-B67) inverse OETF.
#[inline]
pub fn hlg_to_linear(c: f32) -> f32 {
    let s = c.clamp(0.0, 1.0);
    let a = 0.178_832_77;
    let b = 0.284_668_92;
    let cc = 0.559_910_73;
    if s <= 0.5 { (s * s) / 3.0 } else { (((s - cc) / a).exp() + b) / 12.0 }
}

// ─── Source primaries → BT.709 (D65) gamut conversion ───────────────

/// Apply `source_primaries → BT.709` gamut conversion in linear-light.
/// Identity when the source is already BT.709.
///
/// Each matrix is `M_target^-1 · M_source` derived from the published
/// chromaticity coordinates (D65 throughout for everything except
/// SMPTE 428 XYZ which we don't currently support).
pub fn primaries_to_bt709(rgb: [f32; 3], primaries: ColorPrimaries) -> [f32; 3] {
    // One table: the per-pixel form is the matrix form applied.
    let m = primaries_to_bt709_matrix(primaries);
    [
        m[0][0] * rgb[0] + m[0][1] * rgb[1] + m[0][2] * rgb[2],
        m[1][0] * rgb[0] + m[1][1] * rgb[1] + m[1][2] * rgb[2],
        m[2][0] * rgb[0] + m[2][1] * rgb[1] + m[2][2] * rgb[2],
    ]
}

#[inline]
fn mul3(m: [[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

// ─── Full decode pipeline (CPU mirror of the WGSL shaders) ──────────

/// Decode an RGBA u8 byte slice produced by ffmpeg's swscale into a
/// linear-light f32 buffer in the requested working colorspace.
///
/// Pipeline:
///   1. u8 → normalised f32 in `[0, 1]`.
///   2. Inverse EOTF for `cs.transfer` → linear in source primaries.
///   3. `cs.primaries → working-space primaries` matrix.
///
/// `cs.range` and `cs.matrix` are *not* applied here — those are
/// already handled by swscale's `sws_setColorspaceDetails` call when
/// it converts YUV → display-encoded RGB. Calling this on an output
/// from un-configured swscale will give the wrong answer.
///
/// Alpha stays linear (per sRGB convention; not gamma-encoded).
pub fn rgba_u8_to_linear(
    rgba: &[u8],
    cs: SourceColorspace,
    ws: WorkingColorspace,
) -> Vec<f32> {
    let n_pixels = rgba.len() / 4;
    // Capacity reserved once and filled by appending, rather than
    // `set_len` over uninitialised memory. Same single allocation and no
    // zeroing pass, but the buffer is never briefly holding uninitialised
    // f32 — which `clippy::uninit_vec` denies, and rightly: the old
    // SAFETY note was only true while every slot really was written, and
    // nothing enforced that.
    let mut out = Vec::with_capacity(n_pixels * 4);
    let _ = cs.matrix; // matrix is applied by swscale, not here.
    let m = primaries_to_working_matrix(cs.primaries, ws);
    for chunk in rgba.chunks_exact(4) {
        let r = apply_inverse_eotf(chunk[0] as f32 / 255.0, cs.transfer);
        let g = apply_inverse_eotf(chunk[1] as f32 / 255.0, cs.transfer);
        let b = apply_inverse_eotf(chunk[2] as f32 / 255.0, cs.transfer);
        let working = mul3(m, [r, g, b]);
        out.extend_from_slice(&[
            working[0],
            working[1],
            working[2],
            chunk[3] as f32 / 255.0,
        ]);
    }
    let _ = ColorMatrix::Bt709; // silence the unused-import warning
                                // when no other code path needs the
                                // matrix variants in this fn.
    out
}

/// Back-compat wrapper for [`rgba_u8_to_linear`] pinned to
/// [`WorkingColorspace::Rec709Linear`]. Matches the long-standing
/// pre-upgrade behaviour byte-for-byte.
#[inline]
pub fn rgba_u8_to_linear_bt709(rgba: &[u8], cs: SourceColorspace) -> Vec<f32> {
    rgba_u8_to_linear(rgba, cs, WorkingColorspace::Rec709Linear)
}

/// ACEScg sibling of [`rgba_u8_to_linear_bt709`]. Output is in linear
/// AP1 primaries (D60 white), ready for an ACES-grade chain.
#[inline]
pub fn rgba_u8_to_linear_acescg(rgba: &[u8], cs: SourceColorspace) -> Vec<f32> {
    rgba_u8_to_linear(rgba, cs, WorkingColorspace::AcesCG)
}

/// Same as [`rgba_u8_to_linear`] but reads `RGBA64LE` (u16
/// little-endian, 16 bits per channel) — the swscale output format
/// used for >=10-bit sources to preserve precision through the
/// linear-light step.
pub fn rgba64_le_to_linear(
    rgba64: &[u8],
    cs: SourceColorspace,
    ws: WorkingColorspace,
) -> Vec<f32> {
    let n_pixels = rgba64.len() / 8; // 8 bytes per pixel (u16 × 4).
    // Appended rather than `set_len` over uninitialised memory; see
    // `rgba_u8_to_linear` above.
    let mut out = Vec::with_capacity(n_pixels * 4);
    let m = primaries_to_working_matrix(cs.primaries, ws);
    for chunk in rgba64.chunks_exact(8) {
        let r = u16::from_le_bytes([chunk[0], chunk[1]]) as f32 / 65535.0;
        let g = u16::from_le_bytes([chunk[2], chunk[3]]) as f32 / 65535.0;
        let b = u16::from_le_bytes([chunk[4], chunk[5]]) as f32 / 65535.0;
        let a = u16::from_le_bytes([chunk[6], chunk[7]]) as f32 / 65535.0;
        let r_lin = apply_inverse_eotf(r, cs.transfer);
        let g_lin = apply_inverse_eotf(g, cs.transfer);
        let b_lin = apply_inverse_eotf(b, cs.transfer);
        let working = mul3(m, [r_lin, g_lin, b_lin]);
        out.extend_from_slice(&[working[0], working[1], working[2], a]);
    }
    out
}

/// Back-compat wrapper for [`rgba64_le_to_linear`] pinned to
/// [`WorkingColorspace::Rec709Linear`].
#[inline]
pub fn rgba64_le_to_linear_bt709(rgba64: &[u8], cs: SourceColorspace) -> Vec<f32> {
    rgba64_le_to_linear(rgba64, cs, WorkingColorspace::Rec709Linear)
}

/// ACEScg sibling of [`rgba64_le_to_linear_bt709`].
#[inline]
pub fn rgba64_le_to_linear_acescg(rgba64: &[u8], cs: SourceColorspace) -> Vec<f32> {
    rgba64_le_to_linear(rgba64, cs, WorkingColorspace::AcesCG)
}

// ─── Direct YUV→linear-BT.709 converter for YUV420P10LE ─────────────

/// YUV→RGB matrix coefficients (`Cv_R`, `Cu_B`, `Cu_G`, `Cv_G`) for
/// the major matrices we recognise. All other matrices fall back to
/// BT.709 — same convention as `klipp_core::color_ids::matrix_id`.
fn matrix_coefficients(matrix: ColorMatrix) -> (f32, f32, f32, f32) {
    match matrix {
        ColorMatrix::Bt709 => ( 1.5748,  1.8556, -0.1873, -0.4681),
        ColorMatrix::Bt601 => ( 1.402,   1.772,  -0.344136, -0.714136),
        ColorMatrix::Bt2020Ncl
        | ColorMatrix::Bt2020Cl => ( 1.4746, 1.8814, -0.16455, -0.57135),
        ColorMatrix::Smpte240 => ( 1.576,  1.826,  -0.227,    -0.477),
        _ => ( 1.5748, 1.8556, -0.1873, -0.4681),
    }
}

/// 3×3 source-primaries → target-working-space matrix, baked once
/// per frame and reused per-pixel.
///
/// For [`WorkingColorspace::Rec709Linear`] (the long-standing
/// pipeline default) this returns exactly the same matrix as
/// `primaries_to_bt709_matrix` — i.e. behaviour is byte-identical
/// to the pre-upgrade decoder.
///
/// For [`WorkingColorspace::AcesCG`] the source-primaries → BT.709
/// matrix is composed with [`crate::primaries::bt709_to_acescg`] so
/// the per-pixel cost stays at one 3×3 multiply: source → AP1
/// directly, no intermediate BT.709 hop.
///
/// [`WorkingColorspace::Bt2020Linear`] composes the same way, with
/// [`crate::primaries::bt709_to_bt2020`]. A BT.2020 source lands
/// unchanged: the source→709 and 709→2020 tables are mutual inverses,
/// so the product is identity to float precision.
pub fn primaries_to_working_matrix(
    p: ColorPrimaries,
    ws: WorkingColorspace,
) -> [[f32; 3]; 3] {
    let to_bt709 = primaries_to_bt709_matrix(p);
    match ws {
        WorkingColorspace::Rec709Linear => to_bt709,
        WorkingColorspace::AcesCG => {
            // Compose: AP1_RGB = M_709→AP1 · M_src→709 · src_RGB
            // Doing the composition once per frame keeps the per-pixel
            // hot loop at one 3×3 multiply instead of two.
            crate::primaries::matrix3_mul(
                crate::primaries::bt709_to_acescg(),
                to_bt709,
            )
        }
        WorkingColorspace::Bt2020Linear => crate::primaries::matrix3_mul(
            crate::primaries::bt709_to_bt2020(),
            to_bt709,
        ),
    }
}

/// 3×3 matrix `M_target^-1 · M_source` baked from
/// [`primaries_to_bt709`]'s per-primaries match. Computed once per
/// frame and reused per-pixel — running the match inside a 2M-pixel
/// hot loop costs ~25 ns/pixel of branch-predict misses + condition
/// evaluation; precomputing it drops that to a fixed 9-multiply
/// FMA chain.
fn primaries_to_bt709_matrix(p: ColorPrimaries) -> [[f32; 3]; 3] {
    // Generated by tools/gen_colour_primaries_reference.py from
    // colour-science (published chromaticities, Bradford where white points
    // differ) and pinned by tests/camera_log_parity.rs. Do not hand-edit:
    // the hand-typed table this replaces was off by up to 0.57 per element
    // for camera gamuts, treated EBU 3213 and XYZ as identity, and gave
    // DCI-P3 Display P3's matrix despite the different white point.
    match p {
        ColorPrimaries::Bt709 => [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        // ITU-R BT.2020
        ColorPrimaries::Bt2020 => [
            [1.660491002_f32, -0.5876411388_f32, -0.07284986332_f32],
            [-0.1245504745_f32, 1.132899897_f32, -0.008349422604_f32],
            [-0.01815076335_f32, -0.100578898_f32, 1.118729661_f32],
        ],
        // SMPTE C
        ColorPrimaries::Bt601_525 => [
            [0.9395420638_f32, 0.05018135686_f32, 0.01027657937_f32],
            [0.01777222314_f32, 0.9657928625_f32, 0.01643491436_f32],
            [-0.001621599943_f32, -0.00436974966_f32, 1.00599135_f32],
        ],
        // ITU-R BT.470 - 625
        ColorPrimaries::Bt601_625 => [
            [1.044043209_f32, -0.04404320876_f32, -1.550711515e-17_f32],
            [-2.29049845e-17_f32, 1.0_f32, 2.617687953e-17_f32],
            [1.063933945e-18_f32, 0.01179337828_f32, 0.9882066217_f32],
        ],
        // ITU-R BT.470 - 525
        ColorPrimaries::Bt470M => [
            [1.485961521_f32, -0.4034344529_f32, -0.08252706811_f32],
            [-0.02511359214_f32, 0.9541655743_f32, 0.07094801787_f32],
            [-0.02721593179_f32, -0.04406279732_f32, 1.071278729_f32],
        ],
        // DCI-P3
        ColorPrimaries::DciP3 => [
            [1.157516406_f32, -0.1549623781_f32, -0.002554028126_f32],
            [-0.04150007153_f32, 1.045567923_f32, -0.004067851549_f32],
            [-0.01805003896_f32, -0.07857827265_f32, 1.096628312_f32],
        ],
        // Display P3
        ColorPrimaries::DisplayP3 => [
            [1.224940176_f32, -0.2249401763_f32, 4.639392352e-17_f32],
            [-0.04205695471_f32, 1.042056955_f32, 2.831251805e-18_f32],
            [-0.01963755459_f32, -0.07863604555_f32, 1.0982736_f32],
        ],
        // EBU Tech. 3213-E
        ColorPrimaries::Ebu3213 => [
            [1.046210904_f32, -0.04608802482_f32, -0.0001228787794_f32],
            [0.0001106123684_f32, 0.9998856494_f32, 3.73821494e-06_f32],
            [3.525321322e-05_f32, 0.01183887209_f32, 0.9881258747_f32],
        ],
        // CIE XYZ
        ColorPrimaries::Smpte428 => [
            [3.240969942_f32, -1.537383178_f32, -0.4986107603_f32],
            [-0.9692436363_f32, 1.875967502_f32, 0.04155505741_f32],
            [0.0556300797_f32, -0.2039769589_f32, 1.056971514_f32],
        ],
        // S-Gamut3
        ColorPrimaries::SGamut3 => [
            [1.877915128_f32, -0.7941687613_f32, -0.08374636707_f32],
            [-0.1768069813_f32, 1.350999621_f32, -0.1741926395_f32],
            [-0.02620112644_f32, -0.1484222623_f32, 1.174623389_f32],
        ],
        // S-Gamut3.Cine
        ColorPrimaries::SGamut3Cine => [
            [1.62694741_f32, -0.5401385388_f32, -0.08680887073_f32],
            [-0.1785155272_f32, 1.417940927_f32, -0.2394254004_f32],
            [-0.04443611496_f32, -0.1959199662_f32, 1.240356081_f32],
        ],
        // V-Gamut
        ColorPrimaries::VGamut => [
            [1.80657357_f32, -0.6956981313_f32, -0.1108785678_f32],
            [-0.1700890354_f32, 1.305954766_f32, -0.1358648217_f32],
            [-0.02520569242_f32, -0.1544677641_f32, 1.179673668_f32],
        ],
        // Cinema Gamut
        ColorPrimaries::CanonCinemaGamut => [
            [1.923861296_f32, -0.7987606632_f32, -0.1251006327_f32],
            [-0.2043108482_f32, 1.49589851_f32, -0.2915876616_f32],
            [-0.02368502097_f32, -0.420127011_f32, 1.443812032_f32],
        ],
        // ARRI Wide Gamut 3
        ColorPrimaries::AlexaWideGamut3 => [
            [1.61752447_f32, -0.5372862103_f32, -0.08023814713_f32],
            [-0.07057270691_f32, 1.334612855_f32, -0.2640402084_f32],
            [-0.02110204687_f32, -0.2269536096_f32, 1.248055924_f32],
        ],
        // ARRI Wide Gamut 4
        ColorPrimaries::AlexaWideGamut4 => [
            [1.893123443_f32, -0.7808815036_f32, -0.112241939_f32],
            [-0.2057003583_f32, 1.340257489_f32, -0.1345571311_f32],
            [-0.01270574293_f32, -0.1521848758_f32, 1.164890619_f32],
        ],
        // REDWideGamutRGB
        ColorPrimaries::RedWideGamut => [
            [1.981975445_f32, -0.9004330696_f32, -0.08154550455_f32],
            [-0.1781431363_f32, 1.500468514_f32, -0.3223244683_f32],
            [-0.1017961106_f32, -0.5352632254_f32, 1.637059548_f32],
        ],
        // Blackmagic Wide Gamut
        ColorPrimaries::BmdWideGamut => [
            [1.568414969_f32, -0.5226802154_f32, -0.04573475373_f32],
            [-0.08635912338_f32, 1.344922353_f32, -0.2585632297_f32],
            [-0.05204778145_f32, -0.2491731294_f32, 1.301220911_f32],
        ],
    }
}

/// 1025-entry LUT of `apply_inverse_eotf` sampled at uniform input
/// `s = i / 1024.0`. Linear-interpolated lookup costs ~3 ns/pixel
/// versus ~30 ns for the analytic `powf` form, dropping the
/// per-pixel EOTF cost by ~10×. Used per-channel inside the hot
/// loop.
struct EotfLut {
    table: [f32; 1025],
}

impl EotfLut {
    fn new(transfer: ColorTransfer) -> Self {
        let mut table = [0.0f32; 1025];
        for (i, slot) in table.iter_mut().enumerate() {
            let s = (i.min(1024) as f32) / 1024.0;
            *slot = apply_inverse_eotf(s, transfer);
        }
        Self { table }
    }

    #[inline(always)]
    fn lookup(&self, s: f32) -> f32 {
        let s = s.clamp(0.0, 1.0);
        let pos = s * 1024.0;
        // The last segment ends at 1.0, so full scale interpolates to
        // the last entry rather than reading one past it. (It used to
        // index `table[1025]`: every clipped white in 10-bit software
        // decode panicked, which aborts a release build.)
        let idx = (pos as usize).min(1023);
        let frac = pos - idx as f32;
        let lo = self.table[idx];
        let hi = self.table[idx + 1];
        lo + (hi - lo) * frac
    }
}

/// Decode YUV420P10LE planes directly into a linear-light f32 RGBA
/// buffer in the requested working colorspace.
///
/// Skips libswscale entirely — we observed swscale's
/// `YUV420P10LE → RGBA64LE` path producing channel-clamped output
/// (R or G hitting 0 / 1.0 across most pixels) regardless of the
/// `sws_setColorspaceDetails` configuration. This direct path uses
/// the AVFrame's raw 10-bit planes, applies the source's range +
/// matrix → display-encoded RGB, then the same EOTF + gamut chain
/// as [`rgba_u8_to_linear`] / [`rgba64_le_to_linear`].
///
/// Resize is nearest-neighbour to match the existing playback
/// downscale behaviour. Output dimensions come from the caller
/// (typically 1920×1080 for 4K → 1080p playback).
///
/// `y_data` / `u_data` / `v_data` are the AVFrame plane slices
/// (u16 little-endian, lower 10 bits valid). `*_stride` is the
/// per-row byte stride. Caller must provide sources sized
/// `src_h * y_stride` / `src_h/2 * u_stride` / `src_h/2 * v_stride`.
#[allow(clippy::too_many_arguments)]
pub fn yuv420p10le_to_linear(
    y_data: &[u8],
    u_data: &[u8],
    v_data: &[u8],
    y_stride: usize,
    u_stride: usize,
    v_stride: usize,
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
    cs: SourceColorspace,
    ws: WorkingColorspace,
) -> Vec<f32> {
    let (cv_r, cu_b, cu_g, cv_g) = matrix_coefficients(cs.matrix);
    // 10-bit range constants. Limited: Y 64..940 / UV 64..960.
    // Full: Y 0..1023 / UV 0..1023 (centred at 512). Reciprocals
    // pre-computed so the per-pixel hot loop multiplies instead of
    // divides — a divide is ~10× the cost of a multiply on x86_64.
    let (y_off, y_inv_div, uv_off, uv_inv_div) = match cs.range {
        ColorRange::Limited => (64.0_f32, 1.0 / 876.0, 512.0_f32, 1.0 / 896.0),
        ColorRange::Full =>    ( 0.0_f32, 1.0 / 1023.0, 512.0_f32, 1.0 / 1023.0),
    };
    // Build a 1025-entry LUT of the source's inverse-EOTF and a 3×3
    // primaries-conversion matrix for the requested working space.
    // Both are constant across the whole frame; computing them inside
    // the per-pixel loop was the dominant cost in the first naïve
    // implementation.
    let eotf = EotfLut::new(cs.transfer);
    let pm = primaries_to_working_matrix(cs.primaries, ws);
    let (m00, m01, m02) = (pm[0][0], pm[0][1], pm[0][2]);
    let (m10, m11, m12) = (pm[1][0], pm[1][1], pm[1][2]);
    let (m20, m21, m22) = (pm[2][0], pm[2][1], pm[2][2]);
    // Identity-matrix shortcut: skip the per-pixel matmul when the
    // source primaries already match the working space's basis. Only
    // valid for `Rec709Linear` working space — under `AcesCG` even a
    // BT.709 source needs the BT.709 → AP1 hop, so we always run the
    // matmul.
    let primaries_is_identity = matches!(ws, WorkingColorspace::Rec709Linear)
        && matches!(cs.primaries,
            ColorPrimaries::Bt709 | ColorPrimaries::Bt470M
            | ColorPrimaries::Smpte428 | ColorPrimaries::Ebu3213);

    use rayon::prelude::*;
    let dst_w_us = dst_w as usize;
    let dst_h_us = dst_h as usize;
    let src_w_us = src_w as usize;
    let src_h_us = src_h as usize;
    let n_pixels = dst_w_us * dst_h_us;
    // Zero-filled rather than `set_len` over uninitialised memory. The
    // rows are written in parallel below, so this cannot be filled by
    // appending the way the serial converters above are, and a buffer
    // holding uninitialised f32 is what `clippy::uninit_vec` denies. The
    // old SAFETY note claimed every slot gets written, which was true —
    // but only as long as it stayed true, and nothing checked.
    //
    // And it is *faster*: 10.2 ms on a 4K frame against 10.9-11.1 ms for
    // the version this replaced. `vec![0.0; n]` can take pre-zeroed pages
    // from the OS, where `with_capacity` + `set_len` hands back dirty
    // pages that fault in on first write. The unsafe block was buying
    // nothing.
    let mut out: Vec<f32> = vec![0.0; n_pixels * 4];
    let row_bytes = dst_w_us * 4;
    out.par_chunks_exact_mut(row_bytes)
        .enumerate()
        .for_each(|(dy, row_out)| {
            // Nearest-neighbour resize: map output row to source row.
            let sy = (dy * src_h_us / dst_h_us).min(src_h_us - 1);
            let y_row_off = sy * y_stride;
            // Chroma is reconstructed at H.264 4:2:0 siting, through the
            // same `chroma_taps` the scene-referred converter and the
            // decode shaders use — one definition, so this cannot drift
            // from them. Vertical taps are per-row, so they are resolved
            // once out here rather than per pixel.
            let (cy0, cy1, ty) =
                chroma_taps(sy, 1, src_h_us.div_ceil(2), ChromaAxis::Between);
            let u_row0 = cy0 * u_stride;
            let u_row1 = cy1 * u_stride;
            let v_row0 = cy0 * v_stride;
            let v_row1 = cy1 * v_stride;
            for dx in 0..dst_w_us {
                let sx = (dx * src_w_us / dst_w_us).min(src_w_us - 1);
                let (cx0, cx1, tx) =
                    chroma_taps(sx, 1, src_w_us.div_ceil(2), ChromaAxis::CoSited);
                let yo = y_row_off + sx * 2;
                let y_raw = u16::from_le_bytes([y_data[yo], y_data[yo + 1]]) & 0x3FF;
                // Bilinear over the four taps, per plane.
                let tap = |plane: &[u8], row: usize, cx: usize| -> f32 {
                    let o = row + cx * 2;
                    (u16::from_le_bytes([plane[o], plane[o + 1]]) & 0x3FF) as f32
                };
                let lerp2 = |plane: &[u8], r0: usize, r1: usize| -> f32 {
                    let top = tap(plane, r0, cx0) + (tap(plane, r0, cx1) - tap(plane, r0, cx0)) * tx;
                    let bot = tap(plane, r1, cx0) + (tap(plane, r1, cx1) - tap(plane, r1, cx0)) * tx;
                    top + (bot - top) * ty
                };
                let u_raw = lerp2(u_data, u_row0, u_row1);
                let v_raw = lerp2(v_data, v_row0, v_row1);
                let y = (y_raw as f32 - y_off) * y_inv_div;
                let u = (u_raw - uv_off) * uv_inv_div;
                let v = (v_raw - uv_off) * uv_inv_div;
                let r_disp = y + cv_r * v;
                let g_disp = y + cu_g * u + cv_g * v;
                let b_disp = y + cu_b * u;
                let r_lin = eotf.lookup(r_disp);
                let g_lin = eotf.lookup(g_disp);
                let b_lin = eotf.lookup(b_disp);
                let (r_out, g_out, b_out) = if primaries_is_identity {
                    (r_lin, g_lin, b_lin)
                } else {
                    (
                        m00 * r_lin + m01 * g_lin + m02 * b_lin,
                        m10 * r_lin + m11 * g_lin + m12 * b_lin,
                        m20 * r_lin + m21 * g_lin + m22 * b_lin,
                    )
                };
                let o = dx * 4;
                row_out[o]     = r_out;
                row_out[o + 1] = g_out;
                row_out[o + 2] = b_out;
                row_out[o + 3] = 1.0;
            }
        });
    out
}

/// Display-referred Y'CbCr → linear working space, for **any** software
/// YUV format: any subsampling, any bit depth, planar or semi-planar,
/// resized and rotated in the one pass.
///
/// This replaces swscale on the software-decode path, and the reason is
/// chroma siting rather than speed. swscale reconstructs subsampled
/// chroma at a different phase from Klipp's decode shaders, and it cannot
/// be told otherwise: `src_h_chr_pos` / `src_v_chr_pos` only reach its
/// *scaling* filters, so at 1:1 — what `playback_dimensions` returns for
/// any source inside the playback budget — they are ignored outright
/// (measured: xpos=0/ypos=128 and xpos=255/ypos=0 give byte-identical
/// output). A clip therefore changed appearance depending on whether
/// NVDEC took it, which is not a thing a colourist can be asked to
/// reason about.
///
/// Going through [`crate::hdr::YuvSamples`] is what makes this general: the samples
/// have already been read at native depth by
/// `klipp_media::yuv_samples::read_yuv_samples`, which uses the pixel
/// format's own descriptor, so 4:2:2 ProRes, 4:4:4, 12-bit and the rest
/// need no per-format code here. Chroma goes through
/// [`chroma_taps`], the same siting the shaders and
/// [`crate::hdr::YuvSamples::chroma_at`] use.
///
/// Scene-referred sources (PQ, HLG, camera log) do **not** come here —
/// they need code values for the grade, and take
/// [`crate::hdr::yuv_to_hdr_codes`] instead.
pub fn yuv_to_linear_display(
    s: &crate::hdr::YuvSamples,
    dst_w: u32,
    dst_h: u32,
    rotation: u32,
    cs: SourceColorspace,
    ws: WorkingColorspace,
) -> Vec<f32> {
    use rayon::prelude::*;

    let (src_w, src_h) = (s.width.max(1), s.height.max(1));
    let (dst_w, dst_h) = (dst_w.max(1), dst_h.max(1));
    let signal = DisplaySignal::new(s, cs);
    let eotf = EotfLut::new(cs.transfer);
    let pm = primaries_to_working_matrix(cs.primaries, ws);
    let identity = matches!(ws, WorkingColorspace::Rec709Linear)
        && matches!(
            cs.primaries,
            ColorPrimaries::Bt709
                | ColorPrimaries::Bt470M
                | ColorPrimaries::Smpte428
                | ColorPrimaries::Ebu3213
        );

    let n = (dst_w as usize) * (dst_h as usize);
    let mut out = vec![0.0f32; n * 4];
    out.par_chunks_exact_mut(dst_w as usize * 4)
        .enumerate()
        .for_each(|(dy, row)| {
            let dy = dy as u32;
            for dx in 0..dst_w {
                // Rotation folded into the source lookup, the same
                // expressions `yuv_to_hdr_codes` uses so the two agree on
                // which source pixel a destination pixel is.
                let (sx, sy) = match rotation {
                    90 => (dy * src_w / dst_h, (dst_w - 1 - dx) * src_h / dst_w),
                    180 => (
                        (dst_w - 1 - dx) * src_w / dst_w,
                        (dst_h - 1 - dy) * src_h / dst_h,
                    ),
                    270 => ((dst_h - 1 - dy) * src_w / dst_h, dx * src_h / dst_w),
                    _ => (dx * src_w / dst_w, dy * src_h / dst_h),
                };
                let (sx, sy) = (
                    (sx.min(src_w - 1)) as usize,
                    (sy.min(src_h - 1)) as usize,
                );
                let [r, g, b] = signal.at(s, sx, sy);
                let (r, g, b) = (eotf.lookup(r), eotf.lookup(g), eotf.lookup(b));
                let (r, g, b) = if identity {
                    (r, g, b)
                } else {
                    (
                        pm[0][0] * r + pm[0][1] * g + pm[0][2] * b,
                        pm[1][0] * r + pm[1][1] * g + pm[1][2] * b,
                        pm[2][0] * r + pm[2][1] * g + pm[2][2] * b,
                    )
                };
                let o = dx as usize * 4;
                row[o] = r;
                row[o + 1] = g;
                row[o + 2] = b;
                row[o + 3] = 1.0;
            }
        });
    out
}

/// A Y'CbCr source pixel's display signal, R'G'B' 0 to 1 before any
/// transfer: chroma reconstructed by [`chroma_taps`], the range and the
/// matrix applied. The one arithmetic [`yuv_to_linear_display`] and
/// [`yuv_to_display_rgba64`] share, so the viewer's software decode and
/// the export's read a file's colour the same way (#3266).
struct DisplaySignal {
    cw: usize,
    ch: usize,
    y_off: f32,
    y_inv: f32,
    uv_off: f32,
    uv_inv: f32,
    coefficients: (f32, f32, f32, f32),
}

impl DisplaySignal {
    fn new(s: &crate::hdr::YuvSamples, cs: SourceColorspace) -> Self {
        // Range constants, general over bit depth. Limited range is 16/219
        // luma and 128/224 chroma *scaled* to the depth, which is what the
        // 10-bit converter's 64/876/512/896 are at four times 8-bit.
        let max = ((1u32 << s.bit_depth) - 1) as f32;
        let (y_off, y_inv, uv_off, uv_inv) = match cs.range {
            ColorRange::Limited => {
                let k = (max + 1.0) / 256.0;
                (16.0 * k, 1.0 / (219.0 * k), 128.0 * k, 1.0 / (224.0 * k))
            }
            ColorRange::Full => (0.0, 1.0 / max, max * 0.5 + 0.5, 1.0 / max),
        };
        Self {
            cw: s.chroma_width() as usize,
            ch: s.chroma_height() as usize,
            y_off,
            y_inv,
            uv_off,
            uv_inv,
            coefficients: matrix_coefficients(cs.matrix),
        }
    }

    #[inline]
    fn at(&self, s: &crate::hdr::YuvSamples, sx: usize, sy: usize) -> [f32; 3] {
        let cw = self.cw;
        let (x0, x1, tx) = chroma_taps(sx, s.chroma_shift_w, cw, ChromaAxis::CoSited);
        let (y0, y1, ty) = chroma_taps(sy, s.chroma_shift_h, self.ch, ChromaAxis::Between);
        let lerp2 = |p: &[u16]| {
            let at = |x: usize, yy: usize| p[yy * cw + x] as f32;
            let top = at(x0, y0) + (at(x1, y0) - at(x0, y0)) * tx;
            let bot = at(x0, y1) + (at(x1, y1) - at(x0, y1)) * tx;
            top + (bot - top) * ty
        };
        let (cv_r, cu_b, cu_g, cv_g) = self.coefficients;
        let yv = (s.y[sy * s.width.max(1) as usize + sx] as f32 - self.y_off) * self.y_inv;
        let u = (lerp2(s.u) - self.uv_off) * self.uv_inv;
        let v = (lerp2(s.v) - self.uv_off) * self.uv_inv;
        [yv + cv_r * v, yv + cu_g * u + cv_g * v, yv + cu_b * u]
    }
}

/// Y'CbCr → display-encoded RGBA64 at the source's own size, as swscale's
/// RGBA64LE output lays it out (opaque; the caller keeps an alpha plane):
/// what the export's software decode hands on to its resize, rotation and
/// `klipp_render`'s `rgba64_to_linear`. Chroma is reconstructed as
/// the viewer's decoders reconstruct it, which swscale cannot be told to
/// do (#3266). Rows in parallel, as [`yuv_to_linear_display`].
pub fn yuv_to_display_rgba64(s: &crate::hdr::YuvSamples, cs: SourceColorspace) -> Vec<u16> {
    use rayon::prelude::*;

    let (w, h) = (s.width.max(1) as usize, s.height.max(1) as usize);
    let signal = DisplaySignal::new(s, cs);
    let mut out = vec![0u16; w * h * 4];
    out.par_chunks_exact_mut(w * 4).enumerate().for_each(|(sy, row)| {
        let sy = sy.min(h - 1);
        for sx in 0..w {
            let rgb = signal.at(s, sx, sy);
            let o = sx * 4;
            for (c, v) in rgb.iter().enumerate() {
                row[o + c] = (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16;
            }
            row[o + 3] = u16::MAX;
        }
    });
    out
}

/// The source pixels a downscale averages for output pixel `d` of `dst`
/// on an axis of `src`: the CPU twin of `resize_body.wgsl`'s
/// `resize_footprint`, `resize_samples` and `resize_tap`, in the same f32
/// arithmetic, so the two take the same pixels, at most 8 a side.
#[inline]
fn footprint_taps(d: u32, dst: u32, src: u32) -> impl Iterator<Item = usize> {
    let span = src as f32 / dst as f32;
    let x0 = d as f32 * span;
    let n = ((span - 0.001).ceil() as u32).clamp(1, 8);
    (0..n).map(move |i| ((x0 + (i as f32 + 0.5) / n as f32 * span) as u32).min(src - 1) as usize)
}

/// Whether a resize from `src` to `dst` averages on either side, as the
/// decode shaders decide it: more than one sample across a footprint.
pub fn resize_averages(src: (u32, u32), dst: (u32, u32)) -> bool {
    let n = |s: u32, d: u32| ((s as f32 / d.max(1) as f32 - 0.001).ceil() as u32).clamp(1, 8);
    n(src.0, dst.0) > 1 || n(src.1, dst.1) > 1
}

/// Y'CbCr → display-encoded RGBA64 at a smaller `dst_w`×`dst_h`: the CPU
/// twin of the decode shaders' downscale (#744, `resize_body.wgsl`), for
/// the export's software decode (#3266). Each output pixel is the mean,
/// in linear light, of the grid of samples the shader spreads over its
/// footprint, then encoded back for the RGBA64 hand-off. A sample is
/// quantised to 16 bits and decoded with `srgb_to_linear`, as that
/// hand-off decodes the export's frames, so a pixel averaged over one
/// sample is the same-size path's pixel. `alpha` is the file's alpha
/// plane at its own size (16-bit), averaged the same way; without one the
/// frame is opaque. Rows in parallel.
pub fn yuv_to_display_rgba64_downscaled(s: &crate::hdr::YuvSamples, cs: SourceColorspace, alpha: Option<&[u16]>, dst_w: u32, dst_h: u32) -> Vec<u16> {
    use rayon::prelude::*;

    let (src_w, src_h) = (s.width.max(1), s.height.max(1));
    let (dst_w, dst_h) = (dst_w.max(1), dst_h.max(1));
    let signal = DisplaySignal::new(s, cs);
    let code = |v: f32| (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16;
    // Each 16-bit code's linear value, `srgb_to_linear` of it: the same
    // numbers, looked up rather than worked out 25 million times a 4K
    // frame.
    static DECODE: std::sync::OnceLock<Vec<f32>> = std::sync::OnceLock::new();
    let table = DECODE.get_or_init(|| (0..=u16::MAX).map(|c| crate::transfer::srgb_to_linear(c as f32 / 65535.0)).collect());
    let linear = |v: f32| table[code(v) as usize];
    let cols: Vec<Vec<usize>> = (0..dst_w).map(|dx| footprint_taps(dx, dst_w, src_w).collect()).collect();
    let mut out = vec![0u16; dst_w as usize * dst_h as usize * 4];
    out.par_chunks_exact_mut(dst_w as usize * 4).enumerate().for_each(|(dy, row)| {
        let rows: Vec<usize> = footprint_taps(dy as u32, dst_h, src_h).collect();
        for (dx, xs) in cols.iter().enumerate() {
            let (mut acc, mut a) = ([0.0f32; 3], 0.0f32);
            for &sy in &rows {
                for &sx in xs {
                    let rgb = signal.at(s, sx, sy);
                    for c in 0..3 {
                        acc[c] += linear(rgb[c]);
                    }
                    a += alpha.map_or(1.0, |p| p[sy * src_w as usize + sx] as f32 / 65535.0);
                }
            }
            let n = (rows.len() * xs.len()) as f32;
            let o = dx * 4;
            for c in 0..3 {
                row[o + c] = code(crate::transfer::linear_to_srgb(acc[c] / n));
            }
            row[o + 3] = code(a / n);
        }
    });
    out
}

/// Lanczos-3 at distance `x`, as `resize_body.wgsl`'s `lanczos3`.
#[inline]
fn lanczos3(x: f32) -> f32 {
    let ax = x.abs();
    if ax < 0.00001 {
        return 1.0;
    }
    if ax >= 3.0 {
        return 0.0;
    }
    let px = std::f32::consts::PI * ax;
    3.0 * px.sin() * (px / 3.0).sin() / (px * px)
}

/// The six source pixels round output pixel `d`'s centre on an upscaled
/// axis and their normalised Lanczos-3 weights, as the shader's
/// `resize_lanczos` takes them.
fn lanczos_taps(d: u32, dst: u32, src: u32) -> [(usize, f32); 6] {
    let span = src as f32 / dst as f32;
    let c = d as f32 * span + span * 0.5 - 0.5;
    let i0 = c.floor() as i32;
    let mut taps = [(0usize, 0.0f32); 6];
    let mut total = 0.0;
    for (k, t) in taps.iter_mut().enumerate() {
        let i = i0 + k as i32 - 2;
        let w = lanczos3(c - i as f32);
        *t = (i.clamp(0, src as i32 - 1) as usize, w);
        total += w;
    }
    for t in &mut taps {
        t.1 /= total;
    }
    taps
}

/// Display-encoded RGBA64 grown to `dst`: the CPU twin of the decode
/// shaders' upscale (#2557, `resize_body.wgsl`'s `resize_lanczos`),
/// Lanczos-3 over the 6×6 source pixels round each output pixel's centre,
/// normalised, on the encoded values, the ringing held to the encoded
/// range (#3266). Done in two passes, which is the shader's product of
/// weights to rounding; rows in parallel. Alpha is filtered alike.
pub fn lanczos_upscale_rgba64(rgba: &[u16], src: (u32, u32), dst: (u32, u32)) -> Vec<u16> {
    use rayon::prelude::*;

    let (sw, sh) = (src.0.max(1), src.1.max(1));
    let (dw, dh) = (dst.0.max(1), dst.1.max(1));
    let cols: Vec<[(usize, f32); 6]> = (0..dw).map(|dx| lanczos_taps(dx, dw, sw)).collect();
    // Across first: each source row at the output width, unclamped.
    let mut across = vec![0.0f32; sh as usize * dw as usize * 4];
    across.par_chunks_exact_mut(dw as usize * 4).enumerate().for_each(|(sy, row)| {
        let src_row = &rgba[sy * sw as usize * 4..(sy + 1) * sw as usize * 4];
        for (dx, taps) in cols.iter().enumerate() {
            for c in 0..4 {
                row[dx * 4 + c] = taps.iter().map(|&(x, w)| src_row[x * 4 + c] as f32 * w).sum();
            }
        }
    });
    let mut out = vec![0u16; dw as usize * dh as usize * 4];
    out.par_chunks_exact_mut(dw as usize * 4).enumerate().for_each(|(dy, row)| {
        let taps = lanczos_taps(dy as u32, dh, sh);
        for (i, v) in row.iter_mut().enumerate() {
            let sum: f32 = taps.iter().map(|&(y, w)| across[y * dw as usize * 4 + i] * w).sum();
            *v = (sum / 65535.0).clamp(0.0, 1.0).mul_add(65535.0, 0.5) as u16;
        }
    });
    out
}

/// Back-compat wrapper for [`yuv420p10le_to_linear`] pinned to
/// [`WorkingColorspace::Rec709Linear`]. Matches the long-standing
/// pre-upgrade behaviour byte-for-byte.
#[allow(clippy::too_many_arguments)]
#[inline]
pub fn yuv420p10le_to_linear_bt709(
    y_data: &[u8],
    u_data: &[u8],
    v_data: &[u8],
    y_stride: usize,
    u_stride: usize,
    v_stride: usize,
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
    cs: SourceColorspace,
) -> Vec<f32> {
    yuv420p10le_to_linear(
        y_data, u_data, v_data,
        y_stride, u_stride, v_stride,
        src_w, src_h, dst_w, dst_h,
        cs, WorkingColorspace::Rec709Linear,
    )
}

/// ACEScg sibling of [`yuv420p10le_to_linear_bt709`]. Output is in
/// linear AP1 primaries — input for an ACES grade chain.
#[allow(clippy::too_many_arguments)]
#[inline]
pub fn yuv420p10le_to_linear_acescg(
    y_data: &[u8],
    u_data: &[u8],
    v_data: &[u8],
    y_stride: usize,
    u_stride: usize,
    v_stride: usize,
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
    cs: SourceColorspace,
) -> Vec<f32> {
    yuv420p10le_to_linear(
        y_data, u_data, v_data,
        y_stride, u_stride, v_stride,
        src_w, src_h, dst_w, dst_h,
        cs, WorkingColorspace::AcesCG,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Full scale and beyond read the table's last entry — they used to
    /// read one past it and panic, aborting a release build on any
    /// clipped white.
    #[test]
    fn eotf_lut_takes_full_scale() {
        for t in [ColorTransfer::Bt709, ColorTransfer::SLog3, ColorTransfer::Pq] {
            let lut = EotfLut::new(t);
            for s in [1.0_f32, 1.5, 0.999_999, 1023.0 / 1024.0] {
                let got = lut.lookup(s);
                let want = apply_inverse_eotf(s.min(1.0), t);
                assert!((got - want).abs() <= want.abs() * 1e-3 + 1e-6, "{t:?} at {s}: {got} vs {want}");
            }
        }
    }

    /// A frame of limited-range super-white — Y above 940, as clipped
    /// highlights in camera footage are — decodes instead of panicking.
    #[test]
    fn super_white_10bit_frame_decodes() {
        let (w, h) = (8usize, 4usize);
        let plane = |v: u16, n: usize| v.to_le_bytes().repeat(n);
        let (y, u, v) = (plane(1019, w * h), plane(512, w * h / 4), plane(512, w * h / 4));
        let cs = SourceColorspace {
            range: ColorRange::Limited,
            matrix: ColorMatrix::Bt709,
            transfer: ColorTransfer::Bt709,
            primaries: ColorPrimaries::Bt709,
        };
        let out = yuv420p10le_to_linear(
            &y, &u, &v, w * 2, w, w, w as u32, h as u32, w as u32, h as u32, cs, WorkingColorspace::Rec709Linear,
        );
        assert_eq!(out.len(), w * h * 4);
        assert!((out[0] - 1.0).abs() < 1e-3, "clipped white decodes as white, got {}", out[0]);
    }

    #[test]
    fn slog3_mid_grey_anchor() {
        // Sony S-Log3 mid-grey reference: 0.18 linear → ~0.41 SLog3.
        // Inverse should recover ~0.18.
        let recovered = slog3_to_linear(0.41);
        assert!((recovered - 0.18).abs() < 0.01,
                "S-Log3 mid-grey anchor off: 0.41 → {recovered}, expected ~0.18");
    }

    #[test]
    fn slog3_zero_is_near_zero() {
        // S-Log3 has a small foot offset — code 95/1023 ≈ 0.09 maps
        // to 0 linear.
        let zero_signal = 95.0 / 1023.0;
        let recovered = slog3_to_linear(zero_signal);
        assert!(recovered.abs() < 1e-4, "S-Log3 foot ≠ 0: got {recovered}");
    }

    #[test]
    fn srgb_default_for_unknown_transfer() {
        // ColorTransfer variants we don't have an explicit branch for
        // (Smpte240, Bt1361, Iec61966_2_4, Smpte428) fall back to
        // sRGB EOTF.
        let out = apply_inverse_eotf(0.5, ColorTransfer::Smpte240);
        let expected = crate::transfer::srgb_to_linear(0.5);
        assert!((out - expected).abs() < 1e-6);
    }

    #[test]
    fn primaries_bt709_is_identity() {
        let rgb = [0.3, 0.6, 0.9];
        let out = primaries_to_bt709(rgb, ColorPrimaries::Bt709);
        for (a, b) in rgb.iter().zip(out.iter()) {
            assert_eq!(a, b);
        }
    }

    #[test]
    fn bt2020_to_bt709_red_increases() {
        // Pure BT.2020 red is "more saturated" than pure BT.709 red.
        // Converting saturated BT.2020 red to BT.709 lifts the matrix
        // diagonal above 1.0 (as expected — BT.709 gamut can't quite
        // hold pure BT.2020 primaries without going negative or > 1).
        let out = primaries_to_bt709([1.0, 0.0, 0.0], ColorPrimaries::Bt2020);
        assert!(out[0] > 1.0, "BT.2020 red → BT.709 should overshoot, got {:?}", out);
    }

    #[test]
    fn nlog_matches_nikon_2018_spec() {
        // Nikon's N-Log specification (2018) gives the forward curve in
        // 10-bit integer form:
        //   x <  0.328:  V = 650 * (x + 0.0075)^(1/3) / 1023
        //   x >= 0.328:  V = (150 * ln(x) + 619) / 1023
        // Decoding Nikon's own encode of a scene value must give it back.
        //
        // This test used to hard-code a mis-derived version — 619 and 150
        // divided by 1000 rather than 1023, plus a spurious 0.0928 toe
        // offset — which put 18% grey at V = 0.4369. The published
        // formula puts it at 0.3637, as colour-science does (see
        // tests/camera_log_parity.rs).
        let encode = |x: f64| -> f64 {
            if x < 0.328 {
                650.0 * (x + 0.0075).cbrt() / 1023.0
            } else {
                (150.0 * x.ln() + 619.0) / 1023.0
            }
        };
        // Up to 14: N-Log's top code (V = 1.0) decodes to
        // exp((1023 - 619) / 150) = 14.78, its brightest representable value.
        for x in [0.0_f64, 0.01, 0.18, 0.3, 0.5, 1.0, 5.0, 14.0] {
            let v = encode(x) as f32;
            let back = nlog_to_linear(v) as f64;
            // Nikon's segments meet with a 0.09% step at the breakpoint;
            // 0.2% relative (or 1e-4 absolute near 0) clears it.
            let tol = (x.abs() * 2e-3).max(1e-4);
            assert!(
                (back - x).abs() <= tol,
                "N-Log round trip at x={x}: encoded V={v}, decoded {back}",
            );
        }
        assert!(
            (encode(0.18) - 0.3637).abs() < 1e-3,
            "18% grey should encode to V≈0.3637 per Nikon, got {}", encode(0.18),
        );
    }

    #[test]
    fn apple_log_matches_2023_profile_spec() {
        // Reference values for Apple Log Profile (2023 whitepaper):
        //   V  < 0   → L = R₀
        //   V  < Pₜ  → L = √(V/C) + R₀     (toe segment)
        //   V ≥ Pₜ  → L = 2^((V−δ)/γ) − β  (log segment)
        // with R₀=-0.05641088, Rₜ=0.01, C=47.28711236,
        //      β=0.00964052, γ=0.08550479, δ=0.69336945.
        // Pₜ = C·(Rₜ−R₀)² ≈ 0.20859.
        //
        // The reference values below were computed from those
        // constants by hand and double-checked against the
        // `transforms.rs::log_to_linear(LogFormat::AppleLog, …)`
        // path that already had the correct decoder.
        //
        // The previous `apple_log_to_linear` was a placeholder that
        // produced ~0.0149 at V=0.5 (where the spec says ≈0.20).
        // That bug silently corrupted every iPhone 15 Pro AppleLog
        // clip flowing through the `ColorTransfer::AppleLog` decoder
        // path — visible as a low-contrast / muddy-mid look that
        // colourists would wrongly try to grade out instead of
        // identifying as a bad IDT.
        let cases: &[(f32, f32, &str)] = &[
            // V    expected L                description
            (0.0,    -0.05641088,             "code 0 → R₀ (toe anchor)"),
            (0.1,    -0.01043,                "toe segment ≈ √(V/C)+R₀"),
            (0.488,   0.18,                   "≈18% mid-gray reference"),
            (0.5,     0.19883,                "log segment, just past Pₜ"),
            (0.7,     1.04443,                "log segment, > 1.0 highlight"),
            (1.0,    12.00427,                "max code, full HDR highlight"),
        ];
        for &(v, expected, desc) in cases {
            let got = apple_log_to_linear(v);
            // 0.005 absolute tolerance — well below mid-grey
            // perceptual threshold; large enough to absorb any
            // tiny f32 drift between the reference implementation
            // and ours.
            let diff = (got - expected).abs();
            assert!(
                diff < 0.005,
                "Apple Log V={v}: expected L≈{expected} ({desc}), got {got}, diff={diff}",
            );
        }
    }

    #[test]
    fn cpu_decode_matches_srgb_path_for_bt709_source() {
        // For a plain BT.709 SDR source, rgba_u8_to_linear_bt709
        // must produce the same result as the legacy sRGB-only path
        // (within float epsilon). Regression guard against the new
        // helper subtly diverging from the old one for the 95% of
        // sources that don't need camera-log handling.
        let cs = SourceColorspace::BT709_LIMITED_SDR;
        let bytes = [42u8, 128, 200, 255];
        let new = rgba_u8_to_linear_bt709(&bytes, cs);
        let old_r = crate::transfer::srgb_to_linear(42.0 / 255.0);
        let old_g = crate::transfer::srgb_to_linear(128.0 / 255.0);
        let old_b = crate::transfer::srgb_to_linear(200.0 / 255.0);
        assert!((new[0] - old_r).abs() < 1e-6);
        assert!((new[1] - old_g).abs() < 1e-6);
        assert!((new[2] - old_b).abs() < 1e-6);
        assert_eq!(new[3], 1.0);
    }

    #[test]
    fn rgba64_le_decode_matches_legacy_for_bt709() {
        // For a BT.709 SDR source the camera-log helper must match
        // the legacy `rgba64_le_to_linear` (sRGB EOTF only) within
        // float epsilon.
        let cs = SourceColorspace::BT709_LIMITED_SDR;
        // Construct 1 pixel of RGBA64LE: r = 0x4000, g = 0x8000,
        // b = 0xC000, a = 0xFFFF.
        let bytes: [u8; 8] = [0x00, 0x40, 0x00, 0x80, 0x00, 0xC0, 0xFF, 0xFF];
        let new = rgba64_le_to_linear_bt709(&bytes, cs);
        // Expected linear values from sRGB EOTF on those 16-bit
        // codepoints normalised to [0, 1].
        let r_n = 0x4000_u16 as f32 / 65535.0;
        let g_n = 0x8000_u16 as f32 / 65535.0;
        let b_n = 0xC000_u16 as f32 / 65535.0;
        let exp_r = crate::transfer::srgb_to_linear(r_n);
        let exp_g = crate::transfer::srgb_to_linear(g_n);
        let exp_b = crate::transfer::srgb_to_linear(b_n);
        assert!((new[0] - exp_r).abs() < 1e-6);
        assert!((new[1] - exp_g).abs() < 1e-6);
        assert!((new[2] - exp_b).abs() < 1e-6);
        assert_eq!(new[3], 1.0);
    }

    // ─── ACEScg working-space dispatch ──────────────────────────

    /// Direct invariant: the working-space matrix selector should
    /// satisfy `M_to_acescg = bt709_to_acescg · M_to_bt709` for every
    /// source primaries variant. If the composition logic in
    /// `primaries_to_working_matrix` ever drifts, this fires before
    /// any pixel-level test does.
    #[test]
    fn working_matrix_acescg_equals_composed_bt709() {
        use crate::primaries::{bt709_to_acescg, matrix3_mul};
        let primaries_set = [
            ColorPrimaries::Bt709,
            ColorPrimaries::Bt2020,
            ColorPrimaries::Bt601_525,
            ColorPrimaries::Bt601_625,
            ColorPrimaries::DciP3,
            ColorPrimaries::DisplayP3,
            ColorPrimaries::SGamut3Cine,
            ColorPrimaries::SGamut3,
            ColorPrimaries::VGamut,
            ColorPrimaries::CanonCinemaGamut,
            ColorPrimaries::AlexaWideGamut3,
            ColorPrimaries::AlexaWideGamut4,
            ColorPrimaries::RedWideGamut,
            ColorPrimaries::BmdWideGamut,
        ];
        let to_acescg = bt709_to_acescg();
        for p in primaries_set {
            let direct = primaries_to_working_matrix(p, WorkingColorspace::AcesCG);
            let composed = matrix3_mul(
                to_acescg,
                primaries_to_working_matrix(p, WorkingColorspace::Rec709Linear),
            );
            for i in 0..3 {
                for j in 0..3 {
                    let d = direct[i][j];
                    let c = composed[i][j];
                    assert!(
                        (d - c).abs() < 1e-6,
                        "primaries {p:?} M_acescg[{i}][{j}]={d}, composed={c}",
                    );
                }
            }
        }
    }

    /// Pixel-level oracle: rgba_u8 ACEScg output must equal the
    /// BT.709 output run through `bt709_to_acescg` per-pixel.
    /// Tolerance is 1e-4 absolute — comfortably above f32 quantisation
    /// noise but tight enough to catch a transposed or scaled matrix.
    /// Exercises a non-trivial primaries variant (S-Gamut3.Cine /
    /// S-Log3) so the composed matrix actually does work, not just
    /// the BT.709-identity path.
    #[test]
    fn rgba_u8_acescg_matches_bt709_via_acescg_matrix() {
        use crate::primaries::{bt709_to_acescg, matrix3_mul_vec};
        let cs = SourceColorspace {
            range: ColorRange::Full,
            matrix: ColorMatrix::Bt709,
            primaries: ColorPrimaries::SGamut3Cine,
            transfer: ColorTransfer::SLog3,
        };
        // Synthesised RGBA pixels covering low / mid / high signal —
        // S-Log3 mid-grey is ~108/255 (0.41 in normalised input).
        let rgba: Vec<u8> = vec![
             40,  60,  80, 255,    // shadows
            108, 128, 148, 255,    // mid-grey neighbourhood
            200, 180, 160, 255,    // highlights
            255,   0, 128, 255,    // saturated red / mid green / mid blue
              0, 255,   0,  64,    // pure green, semi-transparent
        ];
        let bt709 = rgba_u8_to_linear_bt709(&rgba, cs);
        let acescg = rgba_u8_to_linear_acescg(&rgba, cs);
        assert_eq!(bt709.len(), acescg.len());
        let m = bt709_to_acescg();
        for i in 0..(rgba.len() / 4) {
            let o = i * 4;
            let expected = matrix3_mul_vec(m, [bt709[o], bt709[o+1], bt709[o+2]]);
            for k in 0..3 {
                let diff = (acescg[o+k] - expected[k]).abs();
                assert!(
                    diff < 1e-4,
                    "pixel {i} ch {k}: bt709={} → expected_acescg={}, got={}, diff={diff}",
                    bt709[o+k], expected[k], acescg[o+k],
                );
            }
            // Alpha is colourspace-invariant.
            assert_eq!(bt709[o+3], acescg[o+3]);
        }
    }

    /// rgba64_le sibling of the rgba_u8 oracle — same invariant on
    /// the 16-bit input path. Uses BT.2020 / PQ to exercise both a
    /// wide-gamut primaries variant and the PQ EOTF.
    #[test]
    fn rgba64_le_acescg_matches_bt709_via_acescg_matrix() {
        use crate::primaries::{bt709_to_acescg, matrix3_mul_vec};
        let cs = SourceColorspace {
            range: ColorRange::Limited,
            matrix: ColorMatrix::Bt2020Ncl,
            primaries: ColorPrimaries::Bt2020,
            transfer: ColorTransfer::Pq,
        };
        // Three pixels, RGBA u16 little-endian (8 bytes each).
        // Mid-grey, saturated red, and a near-clipped green.
        let mut rgba64 = Vec::new();
        for px in [
            [0x4000u16, 0x4000, 0x4000, 0xFFFF],
            [0xC000,    0x2000, 0x1000, 0xFFFF],
            [0x0800,    0xE000, 0x0800, 0xFFFF],
        ] {
            for v in px {
                rgba64.extend_from_slice(&v.to_le_bytes());
            }
        }
        let bt709 = rgba64_le_to_linear_bt709(&rgba64, cs);
        let acescg = rgba64_le_to_linear_acescg(&rgba64, cs);
        let m = bt709_to_acescg();
        for i in 0..(rgba64.len() / 8) {
            let o = i * 4;
            let expected = matrix3_mul_vec(m, [bt709[o], bt709[o+1], bt709[o+2]]);
            for k in 0..3 {
                let diff = (acescg[o+k] - expected[k]).abs();
                assert!(
                    diff < 1e-4,
                    "rgba64 pixel {i} ch {k}: diff={diff}",
                );
            }
            assert_eq!(bt709[o+3], acescg[o+3]);
        }
    }

    /// Sanity: the back-compat `_bt709` wrapper produces byte-
    /// identical output to the underlying `_to_linear` call with
    /// `WorkingColorspace::Rec709Linear`. If this drifts, every
    /// existing caller of the `_bt709` API is silently changing
    /// behaviour.
    #[test]
    fn bt709_wrapper_is_byte_identical_to_rec709_linear_dispatch() {
        let cs = SourceColorspace::BT709_LIMITED_SDR;
        let rgba: Vec<u8> = (0..256).map(|v| v as u8).collect();
        let via_wrapper = rgba_u8_to_linear_bt709(&rgba, cs);
        let via_dispatch = rgba_u8_to_linear(&rgba, cs, WorkingColorspace::Rec709Linear);
        assert_eq!(via_wrapper, via_dispatch);
    }
    #[test]
    fn a_bt2020_source_lands_unchanged_in_a_bt2020_project() {
        // The whole point of a BT.2020 working space: the most common HDR
        // source arrives in the basis the project already works in, so
        // the decoder's matrix is identity and nothing is lost on the way
        // in. Before this, the source was converted to BT.709 and then
        // treated as BT.2020 — a silent double error.
        let m = primaries_to_working_matrix(ColorPrimaries::Bt2020, WorkingColorspace::Bt2020Linear);
        for (i, row) in m.iter().enumerate() {
            for (j, v) in row.iter().enumerate() {
                let want = if i == j { 1.0 } else { 0.0 };
                assert!((v - want).abs() < 1e-5, "m[{i}][{j}] = {v}, want {want}");
            }
        }
    }

    #[test]
    fn a_bt709_source_is_widened_into_a_bt2020_project() {
        // A BT.709 source in a BT.2020 project is the other everyday
        // case: it must be converted in, landing inside the wider gamut
        // with every channel positive, and come back where it started.
        let m = primaries_to_working_matrix(ColorPrimaries::Bt709, WorkingColorspace::Bt2020Linear);
        for rgb in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.3, 0.6, 0.1]] {
            let in_2020 = crate::primaries::matrix3_mul_vec(m, rgb);
            assert!(in_2020.iter().all(|c| *c >= -1e-6), "{rgb:?} → {in_2020:?}");
            let back = crate::primaries::matrix3_mul_vec(crate::primaries::bt2020_to_bt709(), in_2020);
            for c in 0..3 {
                assert!((back[c] - rgb[c]).abs() < 1e-5, "{rgb:?} → {in_2020:?} → {back:?}");
            }
        }
        // Neutral is neutral: shared D65, no adaptation.
        let white = crate::primaries::matrix3_mul_vec(m, [1.0, 1.0, 1.0]);
        assert!(white.iter().all(|c| (c - 1.0).abs() < 1e-5), "white moved to {white:?}");
    }

    /// Leica's own published numbers, against our curve.
    ///
    /// Table 1 of the L-Log Reference Manual v1.6 lists the 10-bit code for
    /// each of eight scene reflectances, from black level to the curve's
    /// clipping point. Those are the camera maker's figures, arrived at
    /// independently of the colour-science fixture in
    /// `tests/camera_log_parity.rs`, so agreeing with both says the curve
    /// is right rather than merely self-consistent.
    ///
    /// Codes are normalised by 1023: L-Log is recorded full range, which is
    /// what the table's black level of 92 and clipping point of 1023 mean —
    /// video levels would put them at 64 and 940.
    #[test]
    fn leica_llog_matches_the_reference_manuals_table() {
        // (10-bit digital value, linear scene reflectance, what it is)
        let table = [
            (92u32, 0.00_f32, "black level"),
            (220, 0.02, "2% black"),
            (445, 0.18, "middle grey"),
            (634, 0.90, "90% white"),
            (814, 4.07, "Q3 and SL3 white limit"),
            (852, 5.59, "SL (Typ 601) white limit"),
            (897, 8.15, "SL2, SL2-S and SL3-S white limit"),
            (1023, 23.30, "L-Log white limit"),
        ];
        for (code, want, meaning) in table {
            let got = leica_llog_to_linear(code as f32 / 1023.0);
            // The table is printed to two decimals, so its own rounding is
            // the tolerance: half a unit in the last place it states, and
            // a relative allowance where the values are large.
            let tol = 0.005_f32.max(want * 0.002);
            assert!(
                (got - want).abs() <= tol,
                "{meaning}: code {code} decodes to {got:.4}, the manual says {want}",
            );
        }
    }

    /// Middle grey is the one point a colourist feels immediately, and the
    /// manual fixes it at 0 EV — so the curve has to put code 445 there.
    #[test]
    fn leica_llog_puts_middle_grey_at_zero_ev() {
        let grey = leica_llog_to_linear(445.0 / 1023.0);
        let ev = (grey / 0.18).log2();
        assert!(ev.abs() < 0.01, "middle grey lands at {ev:+.3} EV, not 0");
    }
}
