//! Source colourspace metadata.
//!
//! Captures the four pieces of information needed to correctly interpret a
//! decoded video frame's pixels:
//!
//! * **Range** — limited (TV, 16–235) vs full (PC, 0–255). Determines the
//!   normalisation step before YUV→RGB.
//! * **Matrix** — the YUV→RGB coefficient set (BT.709, BT.601, BT.2020,
//!   etc.). Wrong matrix produces a chroma cast — this is what manifested
//!   as the "magenta/green" symptom on the A6700 clip.
//! * **Primaries** — the RGB chromaticities the source was authored
//!   against. Different primaries → a gamut conversion is required to
//!   reach the engine's working space (linear BT.709 today).
//! * **Transfer** — the encoding curve on the RGB samples (BT.1886,
//!   sRGB, PQ, HLG, linear, log…). Decoded with the matching EOTF to
//!   reach scene-referred linear light.
//!
//! Camera log curves (S-Log3, V-Log, ARRI LogC, etc.) are *not* tagged
//! in any container we've seen. They live in [`crate::types::LogFormat`]
//! and are applied as a per-clip override on top of (or instead of) the
//! container-tagged transfer. Keeping the two concerns separate means
//! [`SourceColorspace`] stays a pure read-of-the-file's-metadata while
//! [`crate::types::LogFormat`] is editorial intent.

use serde::{Deserialize, Serialize};

/// Sample range of a YUV (or RGB) source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ColorRange {
    /// Limited / TV / MPEG range. 8-bit Y in `[16, 235]`, UV in
    /// `[16, 240]`. 10-bit Y in `[64, 940]`, UV in `[64, 960]`.
    Limited,
    /// Full / PC / JPEG range. 8-bit values in `[0, 255]`, 10-bit in
    /// `[0, 1023]`. Sony cameras in S-Log3, screen-recorder tools, and
    /// most still-image sources are full-range.
    Full,
}

/// YUV→RGB conversion matrix (a.k.a. "color space" in ffmpeg parlance).
///
/// The actual coefficients are kept out of this enum on purpose — they
/// belong with the shader / converter that consumes them, so each
/// shader can pick the precision it needs (BT.601 is one set of
/// constants in linear-light video, another set if you're matching
/// legacy SMPTE-C, and so on).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ColorMatrix {
    /// BT.709. Default for HD and most modern UHD SDR.
    Bt709,
    /// BT.601 / SMPTE 170M / BT.470BG. Used for SD content. The 525-
    /// and 625-line SMPTE/EBU variants share the same coefficients;
    /// the difference between NTSC and PAL is the *primaries*, not
    /// the matrix.
    Bt601,
    /// BT.2020 non-constant luminance. UHD HDR and modern wide-gamut
    /// SDR.
    Bt2020Ncl,
    /// BT.2020 constant luminance. Theoretical; almost never seen.
    Bt2020Cl,
    /// SMPTE 240M. Early HDTV; obsolete but occasionally present.
    Smpte240,
    /// Identity / direct GBR (no YUV conversion needed). ffmpeg tags
    /// this `AVCOL_SPC_RGB` for sources that come out of the decoder
    /// already in RGB ordering.
    Identity,
    /// FCC 73.682, the original NTSC. Effectively obsolete.
    Fcc,
    /// YCgCo lossless YUV variant.
    Ycgco,
    /// Chroma-derived NCL. Rare; ICTP-related.
    ChromaDerivedNcl,
    /// Chroma-derived CL. Rare.
    ChromaDerivedCl,
    /// Dolby ICtCp. Used by some Dolby Vision streams.
    Ictcp,
}

/// RGB primaries — the chromaticities of the red, green, and blue
/// reference samples the source was authored against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ColorPrimaries {
    /// BT.709 / sRGB. Default for HD SDR and most consumer UHD.
    Bt709,
    /// BT.601 525-line (SMPTE 170M / SMPTE 240M). NTSC.
    Bt601_525,
    /// BT.601 625-line (BT.470BG). PAL / SECAM.
    Bt601_625,
    /// BT.2020 wide gamut. UHD HDR and modern wide-gamut SDR.
    Bt2020,
    /// BT.470M (NTSC 1953). Obsolete but occasionally tagged.
    Bt470M,
    /// SMPTE RP 431-2 — DCI-P3 with a theatrical white point.
    DciP3,
    /// SMPTE EG 432-1 — Display P3 (P3-D65). Used by Apple devices.
    DisplayP3,
    /// SMPTE ST 428-1 — DCDM XYZ.
    Smpte428,
    /// EBU Tech 3213-E / JEDEC P22 (same value 22 in ffmpeg). Niche
    /// SD / CRT-era phosphor primaries.
    Ebu3213,

    // ─── Camera-native gamuts (not in AVColorPrimaries) ────────
    //
    // Like the camera log curves, container metadata never tags
    // these — they're declared per-clip alongside the log format.
    //
    /// Sony S-Gamut3.Cine. Default for S-Log3 sources.
    SGamut3Cine,
    /// Sony S-Gamut3 (slightly wider than S-Gamut3.Cine).
    SGamut3,
    /// Panasonic V-Gamut.
    VGamut,
    /// Canon Cinema Gamut.
    CanonCinemaGamut,
    /// ARRI Wide Gamut 3 (Alexa Classic / Mini).
    AlexaWideGamut3,
    /// ARRI Wide Gamut 4 (Alexa 35).
    AlexaWideGamut4,
    /// RED Wide Gamut RGB.
    RedWideGamut,
    /// Blackmagic Design Wide Gamut Gen 5.
    BmdWideGamut,
}

/// Transfer characteristic / EOTF — the encoding curve applied to the
/// RGB samples. To reach scene-referred linear light, the matching EOTF
/// must be inverted at decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ColorTransfer {
    /// BT.709 / BT.1886 (~2.4 gamma with toe). Default for HD SDR.
    Bt709,
    /// BT.601 / SMPTE 170M. Same shape as BT.709 at u8 precision.
    Bt601,
    /// SMPTE 240M. Early HDTV.
    Smpte240,
    /// Linear light. No transfer curve.
    Linear,
    /// Log10, 100:1 dynamic range.
    Log100,
    /// Log10, 316.227:1 dynamic range (a.k.a. "log sqrt").
    Log316,
    /// IEC 61966-2-4 — xvYCC. BT.709 extended for negatives.
    Iec61966_2_4,
    /// BT.1361. Wide-gamut extension of BT.709. Rare.
    Bt1361,
    /// IEC 61966-2-1 — sRGB. Identical to BT.709 above ~0.018.
    Srgb,
    /// BT.2020 10-bit transfer (same shape as BT.709 / BT.1886).
    Bt2020_10,
    /// BT.2020 12-bit transfer (same shape as BT.709 / BT.1886, more
    /// codes in the toe).
    Bt2020_12,
    /// SMPTE ST 2084 — PQ. HDR EOTF (cd/m² in [0, 10000]).
    Pq,
    /// SMPTE ST 428 — DCDM gamma 2.6 + XYZ.
    Smpte428,
    /// ARIB STD-B67 — HLG. Hybrid log-gamma HDR.
    Hlg,
    /// Pure gamma 2.2 (no toe). Legacy displays.
    Gamma22,
    /// Pure gamma 2.8 (no toe). Legacy PAL displays.
    Gamma28,

    // ─── Camera log curves ─────────────────────────────────────
    //
    // None of these are tagged in any container we've seen — Sony,
    // ARRI, Canon, etc. all leave `color_trc=unknown` and rely on the
    // editor / DIT to declare the log format. We expose them on
    // `ColorTransfer` so a per-clip override can flow through the
    // same pipeline as a container-tagged transfer.
    //
    /// Sony S-Log3.
    SLog3,
    /// Sony S-Log2.
    SLog2,
    /// Panasonic V-Log.
    VLog,
    /// Canon C-Log.
    CanonLog,
    /// Canon C-Log3.
    CanonLog3,
    /// ARRI Log C3 (Alexa Classic / Mini).
    LogC3,
    /// ARRI Log C4 (Alexa 35).
    LogC4,
    /// RED Log3G10.
    RedLog3G10,
    /// Blackmagic Design Film Generation 5.
    BmdFilmGen5,
    /// DJI D-Log.
    DJIDLog,
    /// Fuji F-Log.
    FLog,
    /// Apple Log (iPhone 15 Pro+).
    AppleLog,
    /// Nikon N-Log.
    NLog,
    /// GoPro ProTune Flat.
    GoProFlat,
    /// Leica L-Log.
    LeicaLLog,
}

/// All four colourspace tags resolved to a definite value (no
/// "unspecified" variants). Built from the AVFrame's tagged fields via
/// [`SourceColorspace::resolve`], which applies dimension- and codec-
/// based heuristics for missing tags.
///
/// Once attached to a decoded frame, downstream shaders can dispatch
/// the correct math without re-deriving anything per-pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceColorspace {
    pub range: ColorRange,
    pub matrix: ColorMatrix,
    pub primaries: ColorPrimaries,
    pub transfer: ColorTransfer,
}

impl SourceColorspace {
    /// A safe default for SDR HD content with no tagged metadata —
    /// limited-range BT.709 across the board. Used as the last-resort
    /// fallback by [`Self::resolve`] and as a sensible default for
    /// callers that haven't decoded a frame yet.
    pub const BT709_LIMITED_SDR: Self = Self {
        range: ColorRange::Limited,
        matrix: ColorMatrix::Bt709,
        primaries: ColorPrimaries::Bt709,
        transfer: ColorTransfer::Bt709,
    };

    /// Resolve raw tags into a definite [`SourceColorspace`].
    ///
    /// `range_raw`, `matrix_raw`, `primaries_raw`, `transfer_raw` are
    /// the i32 enum values straight off `AVFrame` *or* off
    /// `AVCodecParameters` (we take ints rather than `ffmpeg_next`
    /// types so this crate stays free of an ffmpeg-next dependency —
    /// the engine adapter does the i32 conversion). `width × height`
    /// informs the heuristic when a tag is `Unspecified`.
    ///
    /// Heuristic ladder (only consulted when a tag is unspecified):
    ///
    /// 1. **Range** — defaults to [`ColorRange::Limited`]. Most video
    ///    is limited; full-range sources almost always tag themselves.
    /// 2. **Matrix** — UHD with BT.2020 primaries → BT.2020 NCL;
    ///    height ≥ 720 → BT.709; otherwise → BT.601.
    /// 3. **Primaries** — match the matrix where it makes sense
    ///    (BT.709 / BT.2020), otherwise resolve by frame size and
    ///    fall back to BT.709.
    /// 4. **Transfer** — BT.2020 primaries default to
    ///    [`ColorTransfer::Bt2020_10`]; otherwise BT.709.
    pub fn resolve(
        range_raw: i32,
        matrix_raw: i32,
        primaries_raw: i32,
        transfer_raw: i32,
        _width: u32,
        height: u32,
    ) -> Self {
        // `_width` is reserved for future heuristics that distinguish
        // 4:3 vs widescreen SD encodings; the current rules only need
        // height. Kept in the signature so future refinements don't
        // need a callsite churn.
        let range = resolve_range_or(range_raw, ColorRange::Limited);
        let primaries = resolve_primaries_or(primaries_raw, primaries_for_size(height));
        let matrix = resolve_matrix_or(matrix_raw, default_matrix(primaries, height));
        let transfer = resolve_transfer_or(transfer_raw, default_transfer(primaries));
        Self {
            range,
            matrix,
            primaries,
            transfer,
        }
    }

    /// Like [`Self::resolve`], but each per-frame tag that's
    /// `Unspecified` falls back to the corresponding field of
    /// `baseline` instead of the dimension-based heuristic.
    ///
    /// This is the right call when the per-frame AVFrame has lost its
    /// colour metadata (NVDEC's `av_hwframe_transfer_data` strips
    /// `color_range` and friends on download for some sources, leaving
    /// the per-frame fields zero/unspecified even though the codec
    /// parameters say otherwise). The decoder builds `baseline` once
    /// at init from the AVCodecParameters via [`Self::resolve`], then
    /// every subsequent frame reads its own AVFrame tags through this
    /// method so any per-frame override (e.g. an HDR metadata change
    /// mid-stream) wins over the baseline.
    pub fn resolve_with_baseline(
        range_raw: i32,
        matrix_raw: i32,
        primaries_raw: i32,
        transfer_raw: i32,
        baseline: SourceColorspace,
    ) -> Self {
        Self {
            range: resolve_range_or(range_raw, baseline.range),
            matrix: resolve_matrix_or(matrix_raw, baseline.matrix),
            primaries: resolve_primaries_or(primaries_raw, baseline.primaries),
            transfer: resolve_transfer_or(transfer_raw, baseline.transfer),
        }
    }

    /// Short, human-readable summary for log lines and the inspector.
    /// Matches the shape ffprobe prints, e.g.
    /// `"limited BT.709 / BT.709 primaries / BT.709 transfer"`.
    pub fn describe(&self) -> String {
        format!(
            "{} {} / {} primaries / {} transfer",
            match self.range {
                ColorRange::Limited => "limited",
                ColorRange::Full => "full",
            },
            matrix_label(self.matrix),
            primaries_label(self.primaries),
            transfer_label(self.transfer),
        )
    }
}

impl Default for SourceColorspace {
    fn default() -> Self {
        Self::BT709_LIMITED_SDR
    }
}

// ─── ffmpeg AVCOL_* numeric constants ──────────────────────────────
//
// ffmpeg's AVCOL_* enums are stable C enums with documented integer
// values (libavutil/pixfmt.h). Reproducing them as plain `i32`
// constants here keeps `klipp-core` free of an `ffmpeg-sys-next`
// dependency. The engine adapter casts the AVFrame fields straight to
// `i32` and `resolve` matches against these constants.

const AVCOL_RANGE_UNSPECIFIED: i32 = 0;
const AVCOL_RANGE_MPEG: i32 = 1;
const AVCOL_RANGE_JPEG: i32 = 2;

const AVCOL_SPC_RGB: i32 = 0;
const AVCOL_SPC_BT709: i32 = 1;
const AVCOL_SPC_UNSPECIFIED: i32 = 2;
const AVCOL_SPC_FCC: i32 = 4;
const AVCOL_SPC_BT470BG: i32 = 5;
const AVCOL_SPC_SMPTE170M: i32 = 6;
const AVCOL_SPC_SMPTE240M: i32 = 7;
const AVCOL_SPC_YCGCO: i32 = 8;
const AVCOL_SPC_BT2020_NCL: i32 = 9;
const AVCOL_SPC_BT2020_CL: i32 = 10;
const AVCOL_SPC_CHROMA_DERIVED_NCL: i32 = 12;
const AVCOL_SPC_CHROMA_DERIVED_CL: i32 = 13;
const AVCOL_SPC_ICTCP: i32 = 14;

const AVCOL_PRI_BT709: i32 = 1;
const AVCOL_PRI_UNSPECIFIED: i32 = 2;
const AVCOL_PRI_BT470M: i32 = 4;
const AVCOL_PRI_BT470BG: i32 = 5;
const AVCOL_PRI_SMPTE170M: i32 = 6;
const AVCOL_PRI_SMPTE240M: i32 = 7;
// `AVCOL_PRI_FILM` (8) is omitted on purpose: it represents generic
// film primaries with no fixed chromaticities, so the heuristic
// fallback yields a more useful answer than mapping it to a guessed
// variant.
const AVCOL_PRI_BT2020: i32 = 9;
const AVCOL_PRI_SMPTE428: i32 = 10;
const AVCOL_PRI_SMPTE431: i32 = 11;
const AVCOL_PRI_SMPTE432: i32 = 12;
// `AVCOL_PRI_EBU3213` and `AVCOL_PRI_JEDEC_P22` share the same
// numeric value 22 in ffmpeg — they're aliases. We use the EBU3213
// variant on the klipp side and the JEDEC name only appears in
// comments.
const AVCOL_PRI_EBU3213: i32 = 22;

const AVCOL_TRC_BT709: i32 = 1;
const AVCOL_TRC_UNSPECIFIED: i32 = 2;
const AVCOL_TRC_GAMMA22: i32 = 4;
const AVCOL_TRC_GAMMA28: i32 = 5;
const AVCOL_TRC_SMPTE170M: i32 = 6;
const AVCOL_TRC_SMPTE240M: i32 = 7;
const AVCOL_TRC_LINEAR: i32 = 8;
const AVCOL_TRC_LOG: i32 = 9;
const AVCOL_TRC_LOG_SQRT: i32 = 10;
const AVCOL_TRC_IEC61966_2_4: i32 = 11;
const AVCOL_TRC_BT1361_ECG: i32 = 12;
const AVCOL_TRC_IEC61966_2_1: i32 = 13;
const AVCOL_TRC_BT2020_10: i32 = 14;
const AVCOL_TRC_BT2020_12: i32 = 15;
const AVCOL_TRC_SMPTE2084: i32 = 16;
const AVCOL_TRC_SMPTE428: i32 = 17;
const AVCOL_TRC_ARIB_STD_B67: i32 = 18;

/// Read a tagged range, returning `fallback` when the tag is
/// unspecified or out-of-range.
fn resolve_range_or(raw: i32, fallback: ColorRange) -> ColorRange {
    match raw {
        AVCOL_RANGE_JPEG => ColorRange::Full,
        AVCOL_RANGE_MPEG => ColorRange::Limited,
        AVCOL_RANGE_UNSPECIFIED | _ => fallback,
    }
}

fn resolve_primaries_or(raw: i32, fallback: ColorPrimaries) -> ColorPrimaries {
    match raw {
        AVCOL_PRI_BT709 => ColorPrimaries::Bt709,
        AVCOL_PRI_BT470M => ColorPrimaries::Bt470M,
        AVCOL_PRI_BT470BG => ColorPrimaries::Bt601_625,
        AVCOL_PRI_SMPTE170M | AVCOL_PRI_SMPTE240M => ColorPrimaries::Bt601_525,
        AVCOL_PRI_BT2020 => ColorPrimaries::Bt2020,
        AVCOL_PRI_SMPTE428 => ColorPrimaries::Smpte428,
        AVCOL_PRI_SMPTE431 => ColorPrimaries::DciP3,
        AVCOL_PRI_SMPTE432 => ColorPrimaries::DisplayP3,
        AVCOL_PRI_EBU3213 => ColorPrimaries::Ebu3213,
        AVCOL_PRI_UNSPECIFIED | _ => fallback,
    }
}

fn primaries_for_size(height: u32) -> ColorPrimaries {
    if height >= 720 {
        ColorPrimaries::Bt709
    } else if height == 576 {
        // PAL SD.
        ColorPrimaries::Bt601_625
    } else {
        // NTSC SD or anything smaller.
        ColorPrimaries::Bt601_525
    }
}

fn resolve_matrix_or(raw: i32, fallback: ColorMatrix) -> ColorMatrix {
    match raw {
        AVCOL_SPC_RGB => ColorMatrix::Identity,
        AVCOL_SPC_BT709 => ColorMatrix::Bt709,
        AVCOL_SPC_FCC => ColorMatrix::Fcc,
        AVCOL_SPC_BT470BG | AVCOL_SPC_SMPTE170M => ColorMatrix::Bt601,
        AVCOL_SPC_SMPTE240M => ColorMatrix::Smpte240,
        AVCOL_SPC_YCGCO => ColorMatrix::Ycgco,
        AVCOL_SPC_BT2020_NCL => ColorMatrix::Bt2020Ncl,
        AVCOL_SPC_BT2020_CL => ColorMatrix::Bt2020Cl,
        AVCOL_SPC_CHROMA_DERIVED_NCL => ColorMatrix::ChromaDerivedNcl,
        AVCOL_SPC_CHROMA_DERIVED_CL => ColorMatrix::ChromaDerivedCl,
        AVCOL_SPC_ICTCP => ColorMatrix::Ictcp,
        AVCOL_SPC_UNSPECIFIED | _ => fallback,
    }
}

fn default_matrix(primaries: ColorPrimaries, height: u32) -> ColorMatrix {
    match primaries {
        ColorPrimaries::Bt2020 => ColorMatrix::Bt2020Ncl,
        ColorPrimaries::Bt601_525
        | ColorPrimaries::Bt601_625
        | ColorPrimaries::Bt470M
        | ColorPrimaries::Ebu3213 => ColorMatrix::Bt601,
        // Camera-native gamuts come from log-encoded sources whose
        // YUV→RGB matrix is BT.709 in every Sony / ARRI / Canon
        // workflow we've seen — even when the gamut itself is wider.
        // The matrix decode happens *before* the inverse-OETF, so it
        // operates on display-encoded video samples regardless of
        // what gamut the linear values eventually live in.
        ColorPrimaries::SGamut3Cine
        | ColorPrimaries::SGamut3
        | ColorPrimaries::VGamut
        | ColorPrimaries::CanonCinemaGamut
        | ColorPrimaries::AlexaWideGamut3
        | ColorPrimaries::AlexaWideGamut4
        | ColorPrimaries::RedWideGamut
        | ColorPrimaries::BmdWideGamut => ColorMatrix::Bt709,
        _ if height >= 720 => ColorMatrix::Bt709,
        _ => ColorMatrix::Bt601,
    }
}

fn resolve_transfer_or(raw: i32, fallback: ColorTransfer) -> ColorTransfer {
    match raw {
        AVCOL_TRC_BT709 => ColorTransfer::Bt709,
        AVCOL_TRC_GAMMA22 => ColorTransfer::Gamma22,
        AVCOL_TRC_GAMMA28 => ColorTransfer::Gamma28,
        AVCOL_TRC_SMPTE170M => ColorTransfer::Bt601,
        AVCOL_TRC_SMPTE240M => ColorTransfer::Smpte240,
        AVCOL_TRC_LINEAR => ColorTransfer::Linear,
        AVCOL_TRC_LOG => ColorTransfer::Log100,
        AVCOL_TRC_LOG_SQRT => ColorTransfer::Log316,
        AVCOL_TRC_IEC61966_2_4 => ColorTransfer::Iec61966_2_4,
        AVCOL_TRC_BT1361_ECG => ColorTransfer::Bt1361,
        AVCOL_TRC_IEC61966_2_1 => ColorTransfer::Srgb,
        AVCOL_TRC_BT2020_10 => ColorTransfer::Bt2020_10,
        AVCOL_TRC_BT2020_12 => ColorTransfer::Bt2020_12,
        AVCOL_TRC_SMPTE2084 => ColorTransfer::Pq,
        AVCOL_TRC_SMPTE428 => ColorTransfer::Smpte428,
        AVCOL_TRC_ARIB_STD_B67 => ColorTransfer::Hlg,
        AVCOL_TRC_UNSPECIFIED | _ => fallback,
    }
}

fn default_transfer(primaries: ColorPrimaries) -> ColorTransfer {
    match primaries {
        ColorPrimaries::Bt2020 => ColorTransfer::Bt2020_10,
        _ => ColorTransfer::Bt709,
    }
}

fn matrix_label(m: ColorMatrix) -> &'static str {
    match m {
        ColorMatrix::Bt709 => "BT.709",
        ColorMatrix::Bt601 => "BT.601",
        ColorMatrix::Bt2020Ncl => "BT.2020 NCL",
        ColorMatrix::Bt2020Cl => "BT.2020 CL",
        ColorMatrix::Smpte240 => "SMPTE 240M",
        ColorMatrix::Identity => "Identity (RGB)",
        ColorMatrix::Fcc => "FCC",
        ColorMatrix::Ycgco => "YCgCo",
        ColorMatrix::ChromaDerivedNcl => "Chroma-derived NCL",
        ColorMatrix::ChromaDerivedCl => "Chroma-derived CL",
        ColorMatrix::Ictcp => "ICtCp",
    }
}

fn primaries_label(p: ColorPrimaries) -> &'static str {
    match p {
        ColorPrimaries::Bt709 => "BT.709",
        ColorPrimaries::Bt601_525 => "BT.601 525",
        ColorPrimaries::Bt601_625 => "BT.601 625",
        ColorPrimaries::Bt2020 => "BT.2020",
        ColorPrimaries::Bt470M => "BT.470M",
        ColorPrimaries::DciP3 => "DCI-P3",
        ColorPrimaries::DisplayP3 => "Display P3",
        ColorPrimaries::Smpte428 => "SMPTE 428",
        ColorPrimaries::Ebu3213 => "EBU 3213 / JEDEC P22",
        ColorPrimaries::SGamut3Cine => "S-Gamut3.Cine",
        ColorPrimaries::SGamut3 => "S-Gamut3",
        ColorPrimaries::VGamut => "V-Gamut",
        ColorPrimaries::CanonCinemaGamut => "Cinema Gamut",
        ColorPrimaries::AlexaWideGamut3 => "Alexa Wide Gamut 3",
        ColorPrimaries::AlexaWideGamut4 => "Alexa Wide Gamut 4",
        ColorPrimaries::RedWideGamut => "RED Wide Gamut",
        ColorPrimaries::BmdWideGamut => "BMD Wide Gamut Gen 5",
    }
}

fn transfer_label(t: ColorTransfer) -> &'static str {
    match t {
        ColorTransfer::Bt709 => "BT.709",
        ColorTransfer::Bt601 => "BT.601",
        ColorTransfer::Smpte240 => "SMPTE 240M",
        ColorTransfer::Linear => "linear",
        ColorTransfer::Log100 => "log 100:1",
        ColorTransfer::Log316 => "log 316:1",
        ColorTransfer::Iec61966_2_4 => "xvYCC",
        ColorTransfer::Bt1361 => "BT.1361",
        ColorTransfer::Srgb => "sRGB",
        ColorTransfer::Bt2020_10 => "BT.2020 10-bit",
        ColorTransfer::Bt2020_12 => "BT.2020 12-bit",
        ColorTransfer::Pq => "PQ",
        ColorTransfer::Smpte428 => "SMPTE 428",
        ColorTransfer::Hlg => "HLG",
        ColorTransfer::Gamma22 => "gamma 2.2",
        ColorTransfer::Gamma28 => "gamma 2.8",
        ColorTransfer::SLog3 => "Sony S-Log3",
        ColorTransfer::SLog2 => "Sony S-Log2",
        ColorTransfer::VLog => "Panasonic V-Log",
        ColorTransfer::CanonLog => "Canon Log",
        ColorTransfer::CanonLog3 => "Canon Log 3",
        ColorTransfer::LogC3 => "ARRI Log C3",
        ColorTransfer::LogC4 => "ARRI Log C4",
        ColorTransfer::RedLog3G10 => "RED Log3G10",
        ColorTransfer::BmdFilmGen5 => "BMD Film Gen 5",
        ColorTransfer::DJIDLog => "DJI D-Log",
        ColorTransfer::FLog => "Fuji F-Log",
        ColorTransfer::AppleLog => "Apple Log",
        ColorTransfer::NLog => "Nikon N-Log",
        ColorTransfer::GoProFlat => "GoPro ProTune Flat",
        ColorTransfer::LeicaLLog => "Leica L-Log",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a6700_full_range_hevc_resolves_correctly() {
        // ffprobe of the user's "a6700 Log.mp4":
        //   color_range=pc, color_space=unknown,
        //   color_primaries=unknown, color_transfer=unknown,
        //   3840×2160.
        // The fixed tag is `pc` (full range); everything else is
        // unspecified and must fall through to the UHD defaults.
        let cs = SourceColorspace::resolve(
            AVCOL_RANGE_JPEG, // pc / full
            AVCOL_SPC_UNSPECIFIED,
            AVCOL_PRI_UNSPECIFIED,
            AVCOL_TRC_UNSPECIFIED,
            3840,
            2160,
        );
        assert_eq!(cs.range, ColorRange::Full, "tagged full range");
        assert_eq!(cs.matrix, ColorMatrix::Bt709, "UHD heuristic → BT.709");
        assert_eq!(
            cs.primaries,
            ColorPrimaries::Bt709,
            "UHD heuristic → BT.709 primaries"
        );
        assert_eq!(
            cs.transfer,
            ColorTransfer::Bt709,
            "fallback → BT.709 transfer"
        );
    }

    #[test]
    fn limited_range_hd_bt709_typical() {
        let cs = SourceColorspace::resolve(
            AVCOL_RANGE_MPEG,
            AVCOL_SPC_BT709,
            AVCOL_PRI_BT709,
            AVCOL_TRC_BT709,
            1920,
            1080,
        );
        assert_eq!(cs, SourceColorspace::BT709_LIMITED_SDR);
    }

    #[test]
    fn pal_sd_resolves_to_bt601_625() {
        let cs = SourceColorspace::resolve(
            AVCOL_RANGE_MPEG,
            AVCOL_SPC_BT470BG,
            AVCOL_PRI_BT470BG,
            AVCOL_TRC_GAMMA28,
            720,
            576,
        );
        assert_eq!(cs.matrix, ColorMatrix::Bt601);
        assert_eq!(cs.primaries, ColorPrimaries::Bt601_625);
        assert_eq!(cs.transfer, ColorTransfer::Gamma28);
    }

    #[test]
    fn ntsc_sd_resolves_to_bt601_525() {
        let cs = SourceColorspace::resolve(
            AVCOL_RANGE_MPEG,
            AVCOL_SPC_SMPTE170M,
            AVCOL_PRI_SMPTE170M,
            AVCOL_TRC_SMPTE170M,
            720,
            480,
        );
        assert_eq!(cs.matrix, ColorMatrix::Bt601);
        assert_eq!(cs.primaries, ColorPrimaries::Bt601_525);
        assert_eq!(cs.transfer, ColorTransfer::Bt601);
    }

    #[test]
    fn hdr_pq_bt2020_typical() {
        let cs = SourceColorspace::resolve(
            AVCOL_RANGE_MPEG,
            AVCOL_SPC_BT2020_NCL,
            AVCOL_PRI_BT2020,
            AVCOL_TRC_SMPTE2084,
            3840,
            2160,
        );
        assert_eq!(cs.matrix, ColorMatrix::Bt2020Ncl);
        assert_eq!(cs.primaries, ColorPrimaries::Bt2020);
        assert_eq!(cs.transfer, ColorTransfer::Pq);
    }

    #[test]
    fn hlg_signal_recognised() {
        let cs = SourceColorspace::resolve(
            AVCOL_RANGE_MPEG,
            AVCOL_SPC_BT2020_NCL,
            AVCOL_PRI_BT2020,
            AVCOL_TRC_ARIB_STD_B67,
            3840,
            2160,
        );
        assert_eq!(cs.transfer, ColorTransfer::Hlg);
    }

    #[test]
    fn screen_recording_full_range_srgb() {
        // Typical OBS / screen capture: full-range BT.709 with sRGB
        // transfer. Width-chosen from common monitor sizes.
        let cs = SourceColorspace::resolve(
            AVCOL_RANGE_JPEG,
            AVCOL_SPC_BT709,
            AVCOL_PRI_BT709,
            AVCOL_TRC_IEC61966_2_1,
            2560,
            1440,
        );
        assert_eq!(cs.range, ColorRange::Full);
        assert_eq!(cs.transfer, ColorTransfer::Srgb);
    }

    #[test]
    fn unspecified_uhd_with_bt2020_primaries_picks_bt2020_matrix() {
        // Mis-tagged file: only primaries declared, matrix and
        // transfer left unspecified. Heuristic should infer BT.2020
        // matrix and BT.2020-10 transfer to match.
        let cs = SourceColorspace::resolve(
            AVCOL_RANGE_UNSPECIFIED,
            AVCOL_SPC_UNSPECIFIED,
            AVCOL_PRI_BT2020,
            AVCOL_TRC_UNSPECIFIED,
            3840,
            2160,
        );
        assert_eq!(cs.matrix, ColorMatrix::Bt2020Ncl);
        assert_eq!(cs.transfer, ColorTransfer::Bt2020_10);
    }

    #[test]
    fn fully_unspecified_falls_back_to_bt709_limited() {
        let cs = SourceColorspace::resolve(
            AVCOL_RANGE_UNSPECIFIED,
            AVCOL_SPC_UNSPECIFIED,
            AVCOL_PRI_UNSPECIFIED,
            AVCOL_TRC_UNSPECIFIED,
            1920,
            1080,
        );
        assert_eq!(cs, SourceColorspace::BT709_LIMITED_SDR);
    }

    #[test]
    fn describe_is_compact() {
        assert_eq!(
            SourceColorspace::BT709_LIMITED_SDR.describe(),
            "limited BT.709 / BT.709 primaries / BT.709 transfer",
        );
    }

    #[test]
    fn baseline_overrides_unspecified_per_frame_tags() {
        // The real A6700 failure: codec params say full-range,
        // per-frame AVFrame says unspecified. The merge must yield
        // full range (from baseline), with everything else also
        // pulled from the baseline since per-frame is unspecified.
        let baseline = SourceColorspace::resolve(
            AVCOL_RANGE_JPEG, // codecpar: pc / full
            AVCOL_SPC_UNSPECIFIED,
            AVCOL_PRI_UNSPECIFIED,
            AVCOL_TRC_UNSPECIFIED,
            3840,
            2160,
        );
        // Per-frame: every tag unspecified (NVDEC stripped them).
        let merged = SourceColorspace::resolve_with_baseline(
            AVCOL_RANGE_UNSPECIFIED,
            AVCOL_SPC_UNSPECIFIED,
            AVCOL_PRI_UNSPECIFIED,
            AVCOL_TRC_UNSPECIFIED,
            baseline,
        );
        assert_eq!(
            merged.range,
            ColorRange::Full,
            "baseline's full-range tag must survive when per-frame is unspecified"
        );
        assert_eq!(
            merged, baseline,
            "all-unspecified per-frame tags → baseline"
        );
    }

    #[test]
    fn per_frame_tags_override_baseline_when_specified() {
        // Stream-level baseline is BT.709 SDR; a mid-stream HDR
        // frame tags itself BT.2020 + PQ. Per-frame must win.
        let baseline = SourceColorspace::BT709_LIMITED_SDR;
        let merged = SourceColorspace::resolve_with_baseline(
            AVCOL_RANGE_MPEG,
            AVCOL_SPC_BT2020_NCL,
            AVCOL_PRI_BT2020,
            AVCOL_TRC_SMPTE2084,
            baseline,
        );
        assert_eq!(merged.matrix, ColorMatrix::Bt2020Ncl);
        assert_eq!(merged.primaries, ColorPrimaries::Bt2020);
        assert_eq!(merged.transfer, ColorTransfer::Pq);
    }

    #[test]
    fn partial_per_frame_tags_merge_correctly() {
        // Per-frame tags only the transfer; everything else falls
        // back to the baseline.
        let baseline = SourceColorspace {
            range: ColorRange::Full,
            matrix: ColorMatrix::Bt709,
            primaries: ColorPrimaries::Bt709,
            transfer: ColorTransfer::Bt709,
        };
        let merged = SourceColorspace::resolve_with_baseline(
            AVCOL_RANGE_UNSPECIFIED,
            AVCOL_SPC_UNSPECIFIED,
            AVCOL_PRI_UNSPECIFIED,
            AVCOL_TRC_IEC61966_2_1,
            baseline,
        );
        assert_eq!(merged.range, ColorRange::Full);
        assert_eq!(merged.transfer, ColorTransfer::Srgb);
    }
}
