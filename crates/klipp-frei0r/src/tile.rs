//! Tiling renderer for resolutions beyond frei0r's 2048×2048 cap.
//!
//! ## Strategy
//!
//! For any frame with width > 2048 OR height > 2048, render in
//! 1024×1024 tiles with a 64-px overlap on each interior side. Each
//! tile is fed through a per-tile [`Frei0rEffect`] instance (one
//! plugin handle, multiple instances — frei0r ABI permits this since
//! `f0r_construct` returns an opaque handle).
//!
//! ## Seam handling
//!
//! Spatial filters (blur, glow, distort, edge-detect) read pixels
//! outside the tile they're rendering. The 64-px overlap guarantees
//! that for a filter with a kernel ≤ 64 px, the inner tile region is
//! correct — the overlap region absorbs the edge artifacts and is
//! discarded on composite. For wider kernels, seams will show as
//! faint discontinuities at tile boundaries; that is the documented
//! v1 limitation. Frei0r doesn't expose kernel widths, so we can't
//! grow the overlap dynamically.
//!
//! ## Temporal filters
//!
//! Plugins listed in [`crate::instance::temporal::TEMPORAL_PLUGINS`]
//! must NEVER be tiled — their internal state is keyed off the whole
//! frame. The render dispatch in `klipp-effects` checks the allowlist
//! BEFORE calling into this module and rejects with a toast. By the
//! time we're in `render_tiled`, the plugin is known safe to tile.
//!
//! ## Dimension alignment
//!
//! frei0r mandates 8-aligned dimensions. A 1080p frame (1920×1080)
//! is aligned. A 1081-line frame is NOT. For non-aligned input, the
//! renderer falls back to the same passthrough used for >2048 in
//! the no-tile path; the inspector blocks that case at apply time.

use std::sync::Arc;

use crate::ffi::{F0R_DIM_ALIGN, F0R_MAX_DIM};
use crate::instance::{Frei0rEffect, Frei0rPlugin};

/// Tile size — must be ≤ F0R_MAX_DIM and must be 8-aligned. 1024 leaves
/// margin so the tile + overlap stays well under 2048 even at the
/// frame interior.
pub const TILE_DIM: u32 = 1024;

/// Overlap on each interior side of a tile, in pixels. Discarded on
/// composite. 64 covers most common spatial-filter kernels (Gaussian
/// blur up to ~21px sigma, edge-detect, light wraps); larger filters
/// will show seams.
pub const TILE_OVERLAP: u32 = 64;

/// Render an effect over a frame at any resolution. Tiles internally
/// when the frame exceeds [`F0R_MAX_DIM`] in either dimension, runs
/// straight through the plugin otherwise.
///
/// `apply_params` is invoked once per instance after construction so
/// the caller can push the current `Frei0rValue` set onto the plugin.
///
/// Returns the input bytes unchanged on:
///   - non-8-aligned dimensions
///   - dimensions below 8 (frei0r min)
///   - any per-tile construct failure
///
/// Never panics; FFI calls already go through `frei0r_guard!`.
pub fn render<F>(
    plugin: Arc<Frei0rPlugin>,
    width: u32,
    height: u32,
    time_secs: f64,
    rgba_in: &[u8],
    apply_params: &F,
) -> Vec<u8>
where
    F: Fn(&Frei0rEffect),
{
    let expected = (width as usize) * (height as usize) * 4;
    if rgba_in.len() != expected {
        tracing::warn!(
            "frei0r render: buffer size mismatch ({} vs expected {expected})",
            rgba_in.len()
        );
        return rgba_in.to_vec();
    }

    if width % F0R_DIM_ALIGN != 0
        || height % F0R_DIM_ALIGN != 0
        || width < F0R_DIM_ALIGN
        || height < F0R_DIM_ALIGN
    {
        tracing::warn!(
            "frei0r render: {width}×{height} is not 8-aligned, passing through"
        );
        return rgba_in.to_vec();
    }

    // Hot path — frame fits in one frei0r call.
    if width <= F0R_MAX_DIM && height <= F0R_MAX_DIM {
        let effect = match Frei0rEffect::new(plugin, width, height) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("frei0r render: instance construct failed: {e}");
                return rgba_in.to_vec();
            }
        };
        apply_params(&effect);
        return effect.run(time_secs, rgba_in);
    }

    // Tile path — split into ≤ TILE_DIM blocks with TILE_OVERLAP on
    // each interior side.
    render_tiled(plugin, width, height, time_secs, rgba_in, apply_params)
}

fn render_tiled<F>(
    plugin: Arc<Frei0rPlugin>,
    width: u32,
    height: u32,
    time_secs: f64,
    rgba_in: &[u8],
    apply_params: &F,
) -> Vec<u8>
where
    F: Fn(&Frei0rEffect),
{
    let mut out = vec![0u8; rgba_in.len()];

    // Walk the frame in TILE_DIM strides. For each tile we compute:
    //   - the inner rect (final destination region — TILE_DIM × TILE_DIM
    //     except at the right/bottom edges where it's the remainder)
    //   - the padded rect (inner + overlap on every interior side)
    //
    // The padded rect is what frei0r sees; only the inner rect's
    // pixels are written back to `out`.

    let mut y = 0u32;
    while y < height {
        let inner_h = TILE_DIM.min(height - y);
        let mut x = 0u32;
        while x < width {
            let inner_w = TILE_DIM.min(width - x);

            // Padded rect bounds, clamped to frame.
            let pad_x = x.saturating_sub(TILE_OVERLAP);
            let pad_y = y.saturating_sub(TILE_OVERLAP);
            let pad_right = (x + inner_w + TILE_OVERLAP).min(width);
            let pad_bottom = (y + inner_h + TILE_OVERLAP).min(height);
            let mut pad_w = pad_right - pad_x;
            let mut pad_h = pad_bottom - pad_y;

            // Round the padded dimensions down to 8-alignment AND
            // clamp at F0R_MAX_DIM. Rounding down is safe — it makes
            // the padded rect smaller, which only reduces overlap;
            // the inner rect inside is unchanged.
            pad_w = (pad_w / F0R_DIM_ALIGN) * F0R_DIM_ALIGN;
            pad_h = (pad_h / F0R_DIM_ALIGN) * F0R_DIM_ALIGN;
            pad_w = pad_w.min(F0R_MAX_DIM);
            pad_h = pad_h.min(F0R_MAX_DIM);

            if pad_w < F0R_DIM_ALIGN || pad_h < F0R_DIM_ALIGN {
                // Tile too small to feed through frei0r — copy
                // pass-through.
                copy_inner_passthrough(
                    rgba_in,
                    &mut out,
                    width,
                    x,
                    y,
                    inner_w,
                    inner_h,
                );
                x += inner_w;
                continue;
            }

            // Construct tile-sized instance.
            let effect = match Frei0rEffect::new(plugin.clone(), pad_w, pad_h) {
                Ok(e) => e,
                Err(e) => {
                    tracing::debug!("frei0r tile construct failed: {e}");
                    copy_inner_passthrough(
                        rgba_in,
                        &mut out,
                        width,
                        x,
                        y,
                        inner_w,
                        inner_h,
                    );
                    x += inner_w;
                    continue;
                }
            };
            apply_params(&effect);

            // Extract the padded tile from the frame.
            let tile_in = extract_rect(rgba_in, width, pad_x, pad_y, pad_w, pad_h);
            let tile_out = effect.run(time_secs, &tile_in);

            // Composite the inner rect back into `out`. The inner
            // rect lives at offset (x - pad_x, y - pad_y) inside the
            // padded tile.
            let inner_off_x = x - pad_x;
            let inner_off_y = y - pad_y;
            blit_rect(
                &tile_out,
                pad_w,
                inner_off_x,
                inner_off_y,
                inner_w,
                inner_h,
                &mut out,
                width,
                x,
                y,
            );

            x += inner_w;
        }
        y += inner_h;
    }
    out
}

/// Copy a rect from the input frame straight into the output frame
/// (used when a tile can't be processed for any reason — keeps the
/// rest of the frame intact).
fn copy_inner_passthrough(
    rgba_in: &[u8],
    out: &mut [u8],
    frame_w: u32,
    rect_x: u32,
    rect_y: u32,
    rect_w: u32,
    rect_h: u32,
) {
    for row in 0..rect_h {
        let src_off = (((rect_y + row) * frame_w + rect_x) as usize) * 4;
        let dst_off = (((rect_y + row) * frame_w + rect_x) as usize) * 4;
        let len = (rect_w as usize) * 4;
        out[dst_off..dst_off + len]
            .copy_from_slice(&rgba_in[src_off..src_off + len]);
    }
}

/// Extract a rect from a flat RGBA8888 buffer. Returns a fresh
/// `Vec<u8>` containing `rect_w * rect_h * 4` bytes.
fn extract_rect(
    src: &[u8],
    src_w: u32,
    rect_x: u32,
    rect_y: u32,
    rect_w: u32,
    rect_h: u32,
) -> Vec<u8> {
    let mut out = Vec::with_capacity((rect_w * rect_h * 4) as usize);
    for row in 0..rect_h {
        let src_off = (((rect_y + row) * src_w + rect_x) as usize) * 4;
        let len = (rect_w as usize) * 4;
        out.extend_from_slice(&src[src_off..src_off + len]);
    }
    out
}

/// Copy a sub-rect of a tile into the output frame.
#[allow(clippy::too_many_arguments)]
fn blit_rect(
    src: &[u8],
    src_w: u32,
    src_x: u32,
    src_y: u32,
    rect_w: u32,
    rect_h: u32,
    dst: &mut [u8],
    dst_w: u32,
    dst_x: u32,
    dst_y: u32,
) {
    for row in 0..rect_h {
        let src_off = (((src_y + row) * src_w + src_x) as usize) * 4;
        let dst_off = (((dst_y + row) * dst_w + dst_x) as usize) * 4;
        let len = (rect_w as usize) * 4;
        dst[dst_off..dst_off + len].copy_from_slice(&src[src_off..src_off + len]);
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_then_blit_round_trips() {
        // 16×16 frame, 4 channels, fill with row-major byte index.
        let w = 16u32;
        let h = 16u32;
        let mut src = vec![0u8; (w * h * 4) as usize];
        for i in 0..src.len() {
            src[i] = (i & 0xFF) as u8;
        }

        // Extract an 8×8 sub-rect at (4, 4).
        let rect = extract_rect(&src, w, 4, 4, 8, 8);
        assert_eq!(rect.len(), 8 * 8 * 4);

        // Blit it into a fresh dest at the same position. Since src
        // and rect carry the same bytes, the dest should match src
        // for the rect region.
        let mut dst = vec![0u8; src.len()];
        blit_rect(&rect, 8, 0, 0, 8, 8, &mut dst, w, 4, 4);
        for row in 0..8u32 {
            for col in 0..8u32 {
                let off = (((4 + row) * w + (4 + col)) * 4) as usize;
                assert_eq!(dst[off..off + 4], src[off..off + 4]);
            }
        }
        // Outside the rect, dst is still zero.
        assert_eq!(dst[0], 0);
    }

    #[test]
    fn tile_dim_constants_satisfy_frei0r_constraints() {
        assert!(TILE_DIM <= F0R_MAX_DIM, "tile must fit in one frei0r call");
        assert!(TILE_DIM % F0R_DIM_ALIGN == 0);
        assert!((TILE_DIM + 2 * TILE_OVERLAP) <= F0R_MAX_DIM,
            "tile + both-sided overlap must still fit in one call");
        assert!(TILE_OVERLAP % F0R_DIM_ALIGN == 0);
    }
}
