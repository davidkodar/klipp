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
        L::LeicaLLog => leica_llog_to_linear(c),
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
    match primaries {
        ColorPrimaries::Bt709 => rgb,
        ColorPrimaries::Bt2020 => mul3([
            [ 1.6605, -0.5876, -0.0728],
            [-0.1246,  1.1329, -0.0083],
            [-0.0182, -0.1006,  1.1187],
        ], rgb),
        ColorPrimaries::Bt601_525 => mul3([
            [ 0.939_54,  0.050_33,  0.010_13],
            [ 0.017_88,  0.965_67,  0.016_46],
            [-0.001_64, -0.004_92,  1.006_56],
        ], rgb),
        ColorPrimaries::Bt601_625 => mul3([
            [ 1.0440, -0.0440,  0.0000],
            [ 0.0000,  1.0000,  0.0000],
            [ 0.0000,  0.0118,  0.9882],
        ], rgb),
        ColorPrimaries::DciP3 | ColorPrimaries::DisplayP3 => mul3([
            [ 1.2249, -0.2247,  0.0000],
            [-0.0420,  1.0419,  0.0000],
            [-0.0197, -0.0786,  1.0979],
        ], rgb),
        ColorPrimaries::SGamut3Cine => mul3([
            [ 1.626_640_43, -0.540_246_03, -0.086_394_39],
            [-0.104_063_07,  1.106_674_30, -0.002_611_23],
            [-0.003_584_98, -0.079_322_07,  1.082_895_24],
        ], rgb),
        ColorPrimaries::SGamut3 => mul3([
            [ 1.552_362_22, -0.433_807_31, -0.118_554_91],
            [-0.172_786_04,  1.210_769_27, -0.037_983_23],
            [-0.016_391_45, -0.092_276_95,  1.108_668_40],
        ], rgb),
        ColorPrimaries::VGamut => mul3([
            [ 1.806_576, -0.695_697, -0.110_879],
            [-0.170_395,  1.305_812, -0.135_417],
            [-0.020_835, -0.149_682,  1.170_517],
        ], rgb),
        ColorPrimaries::CanonCinemaGamut => mul3([
            [ 1.663_001_07, -0.427_455_05, -0.235_546_03],
            [-0.101_837_58,  1.447_687_85, -0.345_850_27],
            [ 0.004_611_91, -0.076_351_77,  1.071_739_86],
        ], rgb),
        ColorPrimaries::AlexaWideGamut3 => mul3([
            [ 1.485_583, -0.401_961, -0.083_622],
            [-0.077_419,  1.176_592, -0.099_173],
            [ 0.008_382, -0.072_473,  1.064_091],
        ], rgb),
        ColorPrimaries::AlexaWideGamut4 => mul3([
            [ 1.789_066, -0.620_925, -0.168_141],
            [-0.245_664,  1.422_466, -0.176_802],
            [-0.001_624, -0.142_113,  1.143_737],
        ], rgb),
        ColorPrimaries::RedWideGamut => mul3([
            [ 1.412_345, -0.524_711,  0.112_366],
            [-0.177_228,  1.337_117, -0.159_889],
            [ 0.024_029, -0.230_596,  1.206_567],
        ], rgb),
        ColorPrimaries::BmdWideGamut => mul3([
            [ 1.484_293_32, -0.423_483_74, -0.060_809_58],
            [-0.095_891_53,  1.181_316_62, -0.085_425_09],
            [-0.001_005_55, -0.052_988_04,  1.053_993_59],
        ], rgb),
        ColorPrimaries::Bt470M | ColorPrimaries::Smpte428
        | ColorPrimaries::Ebu3213 => rgb,
    }
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
    let mut out = Vec::with_capacity(n_pixels * 4);
    // SAFETY: writing exactly `n_pixels * 4` values.
    unsafe { out.set_len(n_pixels * 4); }
    let _ = cs.matrix; // matrix is applied by swscale, not here.
    let m = primaries_to_working_matrix(cs.primaries, ws);
    for (i, chunk) in rgba.chunks_exact(4).enumerate() {
        let r = apply_inverse_eotf(chunk[0] as f32 / 255.0, cs.transfer);
        let g = apply_inverse_eotf(chunk[1] as f32 / 255.0, cs.transfer);
        let b = apply_inverse_eotf(chunk[2] as f32 / 255.0, cs.transfer);
        let working = mul3(m, [r, g, b]);
        let o = i * 4;
        out[o]     = working[0];
        out[o + 1] = working[1];
        out[o + 2] = working[2];
        out[o + 3] = chunk[3] as f32 / 255.0;
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
    let mut out = Vec::with_capacity(n_pixels * 4);
    unsafe { out.set_len(n_pixels * 4); }
    let m = primaries_to_working_matrix(cs.primaries, ws);
    for (i, chunk) in rgba64.chunks_exact(8).enumerate() {
        let r = u16::from_le_bytes([chunk[0], chunk[1]]) as f32 / 65535.0;
        let g = u16::from_le_bytes([chunk[2], chunk[3]]) as f32 / 65535.0;
        let b = u16::from_le_bytes([chunk[4], chunk[5]]) as f32 / 65535.0;
        let a = u16::from_le_bytes([chunk[6], chunk[7]]) as f32 / 65535.0;
        let r_lin = apply_inverse_eotf(r, cs.transfer);
        let g_lin = apply_inverse_eotf(g, cs.transfer);
        let b_lin = apply_inverse_eotf(b, cs.transfer);
        let working = mul3(m, [r_lin, g_lin, b_lin]);
        let o = i * 4;
        out[o]     = working[0];
        out[o + 1] = working[1];
        out[o + 2] = working[2];
        out[o + 3] = a;
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
/// [`primaries_to_bt709_matrix`] — i.e. behaviour is byte-identical
/// to the pre-upgrade decoder.
///
/// For [`WorkingColorspace::AcesCG`] the source-primaries → BT.709
/// matrix is composed with [`crate::primaries::bt709_to_acescg`] so
/// the per-pixel cost stays at one 3×3 multiply: source → AP1
/// directly, no intermediate BT.709 hop.
///
/// [`WorkingColorspace::Bt2020Linear`] currently routes through the
/// BT.709 matrix as a placeholder — Bt2020Linear has no end-to-end
/// pipeline behind it yet. Wiring lands later; until then a project
/// set to Bt2020Linear behaves the same as Rec709Linear.
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
        WorkingColorspace::Bt2020Linear => to_bt709,
    }
}

/// 3×3 matrix `M_target^-1 · M_source` baked from
/// [`primaries_to_bt709`]'s per-primaries match. Computed once per
/// frame and reused per-pixel — running the match inside a 2M-pixel
/// hot loop costs ~25 ns/pixel of branch-predict misses + condition
/// evaluation; precomputing it drops that to a fixed 9-multiply
/// FMA chain.
fn primaries_to_bt709_matrix(p: ColorPrimaries) -> [[f32; 3]; 3] {
    match p {
        ColorPrimaries::Bt709 => [
            [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0],
        ],
        ColorPrimaries::Bt2020 => [
            [ 1.6605, -0.5876, -0.0728],
            [-0.1246,  1.1329, -0.0083],
            [-0.0182, -0.1006,  1.1187],
        ],
        ColorPrimaries::Bt601_525 => [
            [ 0.939_54,  0.050_33,  0.010_13],
            [ 0.017_88,  0.965_67,  0.016_46],
            [-0.001_64, -0.004_92,  1.006_56],
        ],
        ColorPrimaries::Bt601_625 => [
            [ 1.0440, -0.0440,  0.0000],
            [ 0.0000,  1.0000,  0.0000],
            [ 0.0000,  0.0118,  0.9882],
        ],
        ColorPrimaries::DciP3 | ColorPrimaries::DisplayP3 => [
            [ 1.2249, -0.2247,  0.0000],
            [-0.0420,  1.0419,  0.0000],
            [-0.0197, -0.0786,  1.0979],
        ],
        ColorPrimaries::SGamut3Cine => [
            [ 1.626_640_43, -0.540_246_03, -0.086_394_39],
            [-0.104_063_07,  1.106_674_30, -0.002_611_23],
            [-0.003_584_98, -0.079_322_07,  1.082_895_24],
        ],
        ColorPrimaries::SGamut3 => [
            [ 1.552_362_22, -0.433_807_31, -0.118_554_91],
            [-0.172_786_04,  1.210_769_27, -0.037_983_23],
            [-0.016_391_45, -0.092_276_95,  1.108_668_40],
        ],
        ColorPrimaries::VGamut => [
            [ 1.806_576, -0.695_697, -0.110_879],
            [-0.170_395,  1.305_812, -0.135_417],
            [-0.020_835, -0.149_682,  1.170_517],
        ],
        ColorPrimaries::CanonCinemaGamut => [
            [ 1.663_001_07, -0.427_455_05, -0.235_546_03],
            [-0.101_837_58,  1.447_687_85, -0.345_850_27],
            [ 0.004_611_91, -0.076_351_77,  1.071_739_86],
        ],
        ColorPrimaries::AlexaWideGamut3 => [
            [ 1.485_583, -0.401_961, -0.083_622],
            [-0.077_419,  1.176_592, -0.099_173],
            [ 0.008_382, -0.072_473,  1.064_091],
        ],
        ColorPrimaries::AlexaWideGamut4 => [
            [ 1.789_066, -0.620_925, -0.168_141],
            [-0.245_664,  1.422_466, -0.176_802],
            [-0.001_624, -0.142_113,  1.143_737],
        ],
        ColorPrimaries::RedWideGamut => [
            [ 1.412_345, -0.524_711,  0.112_366],
            [-0.177_228,  1.337_117, -0.159_889],
            [ 0.024_029, -0.230_596,  1.206_567],
        ],
        ColorPrimaries::BmdWideGamut => [
            [ 1.484_293_32, -0.423_483_74, -0.060_809_58],
            [-0.095_891_53,  1.181_316_62, -0.085_425_09],
            [-0.001_005_55, -0.052_988_04,  1.053_993_59],
        ],
        ColorPrimaries::Bt470M | ColorPrimaries::Smpte428
        | ColorPrimaries::Ebu3213 => [
            [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0],
        ],
    }
}

/// 1025-entry LUT of `apply_inverse_eotf` sampled at uniform input
/// `s = i / 1024.0`. Linear-interpolated lookup costs ~3 ns/pixel
/// versus ~30 ns for the analytic `powf` form, dropping the
/// per-pixel EOTF cost by ~10×. Used per-channel inside the hot
/// loop. The +1 sentinel slot lets the lookup avoid a bounds check
/// when the floor index lands at 1024.
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
        let idx = pos as usize;
        let frac = pos - idx as f32;
        // `idx` is in 0..=1024 — sentinel slot covers idx=1024.
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
    let mut out: Vec<f32> = Vec::with_capacity(n_pixels * 4);
    // SAFETY: we write exactly `n_pixels * 4` slots below, indexed
    // through the parallel chunks_exact_mut iterator (one chunk per
    // output row, each chunk = `dst_w_us * 4` slots).
    unsafe { out.set_len(n_pixels * 4); }
    let row_bytes = dst_w_us * 4;
    out.par_chunks_exact_mut(row_bytes)
        .enumerate()
        .for_each(|(dy, row_out)| {
            // Nearest-neighbour resize: map output row to source row.
            let sy = (dy * src_h_us / dst_h_us).min(src_h_us - 1);
            let cy = sy / 2; // chroma is at half resolution.
            let y_row_off = sy * y_stride;
            let u_row_off = cy * u_stride;
            let v_row_off = cy * v_stride;
            for dx in 0..dst_w_us {
                let sx = (dx * src_w_us / dst_w_us).min(src_w_us - 1);
                let cx = sx / 2;
                let yo = y_row_off + sx * 2;
                let uo = u_row_off + cx * 2;
                let vo = v_row_off + cx * 2;
                let y_raw = u16::from_le_bytes([y_data[yo], y_data[yo + 1]]) & 0x3FF;
                let u_raw = u16::from_le_bytes([u_data[uo], u_data[uo + 1]]) & 0x3FF;
                let v_raw = u16::from_le_bytes([v_data[vo], v_data[vo + 1]]) & 0x3FF;
                let y = (y_raw as f32 - y_off) * y_inv_div;
                let u = (u_raw as f32 - uv_off) * uv_inv_div;
                let v = (v_raw as f32 - uv_off) * uv_inv_div;
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
}
