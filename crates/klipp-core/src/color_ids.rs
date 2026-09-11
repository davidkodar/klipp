//! Stable u32 IDs for [`klipp_core::SourceColorspace`] enums, used as
//! shader uniforms.
//!
//! WGSL has no enum types, so each axis (range / matrix / transfer /
//! primaries) is encoded as a `u32` and the shader branches on it.
//! Keeping the IDs in one place — and exposed as both Rust constants
//! and a WGSL prelude — means a future re-numbering only happens in
//! one file.
//!
//! IDs are dense, contiguous, and assigned by frequency-of-occurrence
//! (BT.709 SDR is `0` everywhere because it's the overwhelming
//! majority of consumer footage; HDR variants come next). Out-of-range
//! IDs in a uniform are treated as the default (BT.709 / sRGB / BT.709)
//! by every shader's fall-through branch — never silent corruption.

use crate::{ColorMatrix, ColorPrimaries, ColorRange, ColorTransfer, SourceColorspace};

// ─── range ──────────────────────────────────────────────────────────
pub const COLOR_RANGE_LIMITED: u32 = 0;
pub const COLOR_RANGE_FULL: u32 = 1;

pub fn range_id(r: ColorRange) -> u32 {
    match r {
        ColorRange::Limited => COLOR_RANGE_LIMITED,
        ColorRange::Full => COLOR_RANGE_FULL,
    }
}

// ─── matrix ─────────────────────────────────────────────────────────
pub const MATRIX_BT709: u32 = 0;
pub const MATRIX_BT601: u32 = 1;
pub const MATRIX_BT2020_NCL: u32 = 2;
pub const MATRIX_SMPTE240: u32 = 3;
pub const MATRIX_IDENTITY: u32 = 4;

pub fn matrix_id(m: ColorMatrix) -> u32 {
    match m {
        ColorMatrix::Bt709 => MATRIX_BT709,
        ColorMatrix::Bt601 => MATRIX_BT601,
        ColorMatrix::Bt2020Ncl | ColorMatrix::Bt2020Cl => MATRIX_BT2020_NCL,
        ColorMatrix::Smpte240 => MATRIX_SMPTE240,
        ColorMatrix::Identity => MATRIX_IDENTITY,
        // Rare / theoretical matrices (FCC, YCgCo, ChromaDerived,
        // ICtCp) fall through to BT.709. Adding correct
        // implementations is straightforward when a real source
        // appears.
        _ => MATRIX_BT709,
    }
}

// ─── transfer ───────────────────────────────────────────────────────
pub const TRANSFER_BT709: u32 = 0;
pub const TRANSFER_SRGB: u32 = 1;
pub const TRANSFER_LINEAR: u32 = 2;
pub const TRANSFER_PQ: u32 = 3;
pub const TRANSFER_HLG: u32 = 4;
pub const TRANSFER_GAMMA22: u32 = 5;
pub const TRANSFER_GAMMA28: u32 = 6;
// Camera log curves — IDs >= 100 to leave room for adding more
// container-tagged transfers in the 0..99 range without renumbering.
pub const TRANSFER_SLOG3: u32 = 100;
/// Every transfer id at or above this is a camera log curve.
pub const FIRST_CAMERA_LOG: u32 = TRANSFER_SLOG3;
pub const TRANSFER_SLOG2: u32 = 101;
pub const TRANSFER_VLOG: u32 = 102;
pub const TRANSFER_LOGC3: u32 = 103;
pub const TRANSFER_LOGC4: u32 = 104;
pub const TRANSFER_CANON_LOG: u32 = 105;
pub const TRANSFER_CANON_LOG3: u32 = 106;
pub const TRANSFER_RED_LOG3G10: u32 = 107;
pub const TRANSFER_BMD_FILM_GEN5: u32 = 108;
pub const TRANSFER_DJI_DLOG: u32 = 109;
pub const TRANSFER_FLOG: u32 = 110;
pub const TRANSFER_APPLE_LOG: u32 = 111;
pub const TRANSFER_NLOG: u32 = 112;
pub const TRANSFER_GOPRO_FLAT: u32 = 113;

pub fn transfer_id(t: ColorTransfer) -> u32 {
    match t {
        // BT.709, BT.601, BT.2020 10/12, SMPTE 170M all share the
        // BT.1886 EOTF shape — the differences are only in the spec's
        // toe / linear-segment break that's below u8 precision.
        ColorTransfer::Bt709
        | ColorTransfer::Bt601
        | ColorTransfer::Bt2020_10
        | ColorTransfer::Bt2020_12 => TRANSFER_BT709,
        ColorTransfer::Srgb => TRANSFER_SRGB,
        ColorTransfer::Linear => TRANSFER_LINEAR,
        ColorTransfer::Pq => TRANSFER_PQ,
        ColorTransfer::Hlg => TRANSFER_HLG,
        ColorTransfer::Gamma22 => TRANSFER_GAMMA22,
        ColorTransfer::Gamma28 => TRANSFER_GAMMA28,
        ColorTransfer::SLog3 => TRANSFER_SLOG3,
        ColorTransfer::SLog2 => TRANSFER_SLOG2,
        ColorTransfer::VLog => TRANSFER_VLOG,
        ColorTransfer::LogC3 => TRANSFER_LOGC3,
        ColorTransfer::LogC4 => TRANSFER_LOGC4,
        ColorTransfer::CanonLog => TRANSFER_CANON_LOG,
        ColorTransfer::CanonLog3 => TRANSFER_CANON_LOG3,
        ColorTransfer::RedLog3G10 => TRANSFER_RED_LOG3G10,
        ColorTransfer::BmdFilmGen5 => TRANSFER_BMD_FILM_GEN5,
        ColorTransfer::DJIDLog => TRANSFER_DJI_DLOG,
        ColorTransfer::FLog => TRANSFER_FLOG,
        ColorTransfer::AppleLog => TRANSFER_APPLE_LOG,
        ColorTransfer::NLog => TRANSFER_NLOG,
        ColorTransfer::GoProFlat => TRANSFER_GOPRO_FLAT,
        // SMPTE 240, xvYCC, BT.1361, SMPTE 428 — fall back to
        // BT.709-shape EOTF. Each can become its own ID when a real
        // source appears.
        _ => TRANSFER_BT709,
    }
}

// ─── primaries ──────────────────────────────────────────────────────
pub const PRIMARIES_BT709: u32 = 0;
pub const PRIMARIES_BT2020: u32 = 1;
pub const PRIMARIES_BT601_525: u32 = 2;
pub const PRIMARIES_BT601_625: u32 = 3;
pub const PRIMARIES_DCI_P3: u32 = 4;
pub const PRIMARIES_DISPLAY_P3: u32 = 5;
// Camera-native gamuts — IDs >= 100, same scheme as TRANSFER_*.
pub const PRIMARIES_S_GAMUT3_CINE: u32 = 100;
pub const PRIMARIES_S_GAMUT3: u32 = 101;
pub const PRIMARIES_V_GAMUT: u32 = 102;
pub const PRIMARIES_CANON_CINEMA_GAMUT: u32 = 103;
pub const PRIMARIES_ALEXA_WIDE_GAMUT_3: u32 = 104;
pub const PRIMARIES_ALEXA_WIDE_GAMUT_4: u32 = 105;
pub const PRIMARIES_RED_WIDE_GAMUT: u32 = 106;
pub const PRIMARIES_BMD_WIDE_GAMUT: u32 = 107;

pub fn primaries_id(p: ColorPrimaries) -> u32 {
    match p {
        ColorPrimaries::Bt709 => PRIMARIES_BT709,
        ColorPrimaries::Bt2020 => PRIMARIES_BT2020,
        ColorPrimaries::Bt601_525 => PRIMARIES_BT601_525,
        ColorPrimaries::Bt601_625 => PRIMARIES_BT601_625,
        ColorPrimaries::DciP3 => PRIMARIES_DCI_P3,
        ColorPrimaries::DisplayP3 => PRIMARIES_DISPLAY_P3,
        ColorPrimaries::SGamut3Cine => PRIMARIES_S_GAMUT3_CINE,
        ColorPrimaries::SGamut3 => PRIMARIES_S_GAMUT3,
        ColorPrimaries::VGamut => PRIMARIES_V_GAMUT,
        ColorPrimaries::CanonCinemaGamut => PRIMARIES_CANON_CINEMA_GAMUT,
        ColorPrimaries::AlexaWideGamut3 => PRIMARIES_ALEXA_WIDE_GAMUT_3,
        ColorPrimaries::AlexaWideGamut4 => PRIMARIES_ALEXA_WIDE_GAMUT_4,
        ColorPrimaries::RedWideGamut => PRIMARIES_RED_WIDE_GAMUT,
        ColorPrimaries::BmdWideGamut => PRIMARIES_BMD_WIDE_GAMUT,
        // BT.470M, SMPTE 428, EBU 3213 — fall through to BT.709.
        _ => PRIMARIES_BT709,
    }
}

/// Bundle the four IDs for a `SourceColorspace` into a `[u32; 4]`
/// suitable for direct insertion into a shader uniform struct
/// (`[range, matrix, transfer, primaries]`).
pub fn ids(cs: SourceColorspace) -> [u32; 4] {
    [
        range_id(cs.range),
        matrix_id(cs.matrix),
        transfer_id(cs.transfer),
        primaries_id(cs.primaries),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_for_a6700_full_range_bt709() {
        let cs = SourceColorspace {
            range: ColorRange::Full,
            matrix: ColorMatrix::Bt709,
            primaries: ColorPrimaries::Bt709,
            transfer: ColorTransfer::Bt709,
        };
        assert_eq!(ids(cs), [COLOR_RANGE_FULL, MATRIX_BT709, TRANSFER_BT709, PRIMARIES_BT709]);
    }

    #[test]
    fn ids_for_default_bt709_limited_sdr() {
        assert_eq!(
            ids(SourceColorspace::BT709_LIMITED_SDR),
            [COLOR_RANGE_LIMITED, MATRIX_BT709, TRANSFER_BT709, PRIMARIES_BT709],
        );
    }

    #[test]
    fn ids_for_hdr_pq_bt2020() {
        let cs = SourceColorspace {
            range: ColorRange::Limited,
            matrix: ColorMatrix::Bt2020Ncl,
            primaries: ColorPrimaries::Bt2020,
            transfer: ColorTransfer::Pq,
        };
        assert_eq!(ids(cs), [COLOR_RANGE_LIMITED, MATRIX_BT2020_NCL, TRANSFER_PQ, PRIMARIES_BT2020]);
    }

    #[test]
    fn rare_matrices_fall_back_to_bt709() {
        assert_eq!(matrix_id(ColorMatrix::Fcc), MATRIX_BT709);
        assert_eq!(matrix_id(ColorMatrix::Ycgco), MATRIX_BT709);
        assert_eq!(matrix_id(ColorMatrix::Ictcp), MATRIX_BT709);
    }

    #[test]
    fn bt2020_constant_luminance_uses_ncl_id() {
        // We approximate constant-luminance BT.2020 as NCL — the
        // difference only matters for HDR streams that legally have
        // to be decoded with the constant-luminance form, which is
        // vanishingly rare. Verify the ID is at least the right
        // matrix family.
        assert_eq!(matrix_id(ColorMatrix::Bt2020Cl), MATRIX_BT2020_NCL);
    }

    #[test]
    fn camera_log_transfers_default_to_bt709_until_phase_d() {
        // Phase D adds explicit shader paths for camera log curves
        // via the per-clip `LogFormat` override. Until then, a file
        // tagged with a log transfer (xvYCC, BT.1361, SMPTE 240,
        // BT.428, log100, log316) is decoded as BT.709 — wrong for
        // the log content but correct for the wrapper transfer.
        assert_eq!(transfer_id(ColorTransfer::Iec61966_2_4), TRANSFER_BT709);
        assert_eq!(transfer_id(ColorTransfer::Bt1361), TRANSFER_BT709);
        assert_eq!(transfer_id(ColorTransfer::Log100), TRANSFER_BT709);
    }
}
