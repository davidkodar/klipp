//! Tell swscale which Y'CbCr matrix and range it is converting.
//!
//! Left alone, swscale converts every Y'CbCr ↔ RGB with its default matrix,
//! BT.601 (`SWS_CS_DEFAULT` = SMPTE 170M), whatever the file says. Export
//! decoded BT.709 sources with BT.601 and encoded with BT.601 while tagging
//! the output BT.709, so every export shifted colour: SMPTE bars came out up
//! to 39 levels off, a generated red [200, 40, 40] played back as
//! [212, 54, 36]. The viewer has always set the file's matrix
//! (`video_decoder::convert_sw_frame`); this is the export's equivalent.

use ffmpeg_next::ffi::SwsContext;
use klipp_core::{ColorMatrix, ColorRange};

/// swscale's coefficient table for a matrix (`SWS_CS_*` ids, swscale.h).
unsafe fn coefficients(matrix: ColorMatrix) -> *const i32 {
    const SWS_CS_ITU709: i32 = 1;
    const SWS_CS_FCC: i32 = 4;
    const SWS_CS_SMPTE170M: i32 = 5;
    const SWS_CS_SMPTE240M: i32 = 7;
    const SWS_CS_BT2020: i32 = 9;
    let id = match matrix {
        ColorMatrix::Bt601 => SWS_CS_SMPTE170M,
        ColorMatrix::Bt2020Ncl | ColorMatrix::Bt2020Cl => SWS_CS_BT2020,
        ColorMatrix::Smpte240 => SWS_CS_SMPTE240M,
        ColorMatrix::Fcc => SWS_CS_FCC,
        _ => SWS_CS_ITU709,
    };
    ffmpeg_next::ffi::sws_getCoefficients(id)
}

fn range_flag(range: ColorRange) -> i32 {
    match range {
        ColorRange::Full => 1,
        ColorRange::Limited => 0,
    }
}

/// Configure a Y'CbCr → RGB scaler for a source with this matrix and range.
///
/// SAFETY: `ctx` must be a valid swscale context whose source is Y'CbCr.
pub(crate) unsafe fn set_yuv_to_rgb(ctx: *mut SwsContext, matrix: ColorMatrix, range: ColorRange) -> bool {
    let table = coefficients(matrix);
    ffmpeg_next::ffi::sws_setColorspaceDetails(ctx, table, range_flag(range), table, 1, 0, 1 << 16, 1 << 16) >= 0
}

/// Configure an RGB → Y'CbCr scaler to write this matrix and range.
///
/// SAFETY: `ctx` must be a valid swscale context whose destination is Y'CbCr.
pub(crate) unsafe fn set_rgb_to_yuv(ctx: *mut SwsContext, matrix: ColorMatrix, range: ColorRange) -> bool {
    let table = coefficients(matrix);
    ffmpeg_next::ffi::sws_setColorspaceDetails(ctx, table, 1, table, range_flag(range), 0, 1 << 16, 1 << 16) >= 0
}
