//! A frei0r effect kept across frames at one frame size: the instances
//! a frame needs, made once and run in order, so a plug-in that keeps
//! history (#1468) sees each frame after the one before.
//!
//! ## Sizes frei0r can't take
//!
//! frei0r 1.x takes widths and heights that are multiples of 8, from 8
//! to 2048. Any other size is padded to the next multiple of 8 (at
//! least 8) by repeating the edge pixels, rendered, and cropped back:
//! a declared [`Fallback::Padded`], never the picture returned
//! unchanged (#1469). Pixel-local plug-ins come out exact; one that
//! reads the frame's geometry (a vignette, a position parameter) sees
//! a frame up to 7 pixels larger.
//!
//! ## Tiling
//!
//! Above 2048 in either direction the padded frame is split into
//! 1024×1024 tiles with a 64-px overlap on each interior side, one
//! instance per tile, kept like any other. The overlap absorbs the
//! edges of filters whose kernel is at most 64 px and is dropped on
//! composite; wider kernels show seams, and frei0r doesn't expose
//! kernel widths. A plug-in that keeps frame history is refused above
//! 2048 ([`Refusal::TemporalAboveMax`]): each tile would keep a
//! history of its own part of the frame.
//!
//! ## Memory
//!
//! frei0r has no way to ask a plug-in what it keeps. [`Frei0rInstanceSet::bytes`]
//! is an estimate: each instance's frame, plus, for the plug-ins listed
//! in [`crate::instance::temporal`], the frames of history measured for
//! each. A plug-in that keeps history and isn't listed is unbounded by
//! that estimate; the instance owner's slot count is the bound then.

use std::sync::Arc;

use crate::ffi::{F0R_DIM_ALIGN, F0R_MAX_DIM};
use crate::instance::{temporal, Frei0rEffect, Frei0rPlugin, Frei0rValue};

/// Tile size — must be ≤ F0R_MAX_DIM and must be 8-aligned. 1024 leaves
/// margin so the tile + overlap stays well under 2048 even at the
/// frame interior.
pub const TILE_DIM: u32 = 1024;

/// Overlap on each interior side of a tile, in pixels. Discarded on
/// composite. 64 covers most common spatial-filter kernels (Gaussian
/// blur up to ~21px sigma, edge-detect, light wraps); larger filters
/// will show seams.
pub const TILE_OVERLAP: u32 = 64;

/// Why a frei0r effect can't render a frame. The caller reports it;
/// nothing here passes the picture through in its place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// A width or height of 0.
    EmptyFrame,
    /// The frame isn't `width × height × 4` bytes.
    BufferSize { got: usize, expected: usize },
    /// A plug-in that keeps frame history, above 2048 (tiles would
    /// split its history).
    TemporalAboveMax { plugin: String, width: u32, height: u32 },
    /// The plug-in wouldn't make an instance.
    Construct(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyFrame => write!(f, "frei0r can't render an empty frame"),
            Self::BufferSize { got, expected } => write!(f, "frei0r frame is {got} bytes, expected {expected}"),
            Self::TemporalAboveMax { plugin, width, height } => {
                write!(f, "{plugin} keeps frame history and can't be tiled: {width}×{height} is above {F0R_MAX_DIM}")
            }
            Self::Construct(e) => write!(f, "frei0r instance: {e}"),
        }
    }
}

/// How a frame was rendered other than as asked, declared so the caller
/// can count and show it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fallback {
    /// `from` isn't a size frei0r takes: rendered at `to` (the next
    /// multiple of 8, the edges repeated) and cropped back.
    Padded { from: (u32, u32), to: (u32, u32) },
}

/// One rendered frame, `width × height` RGBA8.
#[derive(Debug, Clone, PartialEq)]
pub struct Rendered {
    pub rgba: Vec<u8>,
    pub fallback: Option<Fallback>,
}

/// One instance and the part of the (padded) frame it renders.
struct Tile {
    effect: Frei0rEffect,
    /// What the instance sees: its rect in the padded frame.
    pad_x: u32,
    pad_y: u32,
    pad_w: u32,
    pad_h: u32,
    /// What it writes back: the inner rect, in the padded frame.
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

/// A frei0r effect at one frame size, kept across frames: one instance,
/// or one per tile above 2048. Render frames through it in order and a
/// plug-in's history carries; make a new one where history must start
/// again (a seek, a new size, another source).
pub struct Frei0rInstanceSet {
    width: u32,
    height: u32,
    padded: (u32, u32),
    tiles: Vec<Tile>,
    bytes: usize,
}

impl Frei0rInstanceSet {
    /// The instances `plugin` needs to render `width × height` frames.
    pub fn new(plugin: Arc<Frei0rPlugin>, width: u32, height: u32) -> Result<Self, Refusal> {
        if width == 0 || height == 0 {
            return Err(Refusal::EmptyFrame);
        }
        let padded = (padded_dim(width), padded_dim(height));
        let tiled = padded.0 > F0R_MAX_DIM || padded.1 > F0R_MAX_DIM;
        if tiled && temporal::is_temporal(&plugin.info) {
            return Err(Refusal::TemporalAboveMax { plugin: plugin.info.name.clone(), width, height });
        }
        let history = temporal::history_frames(&plugin.info) as usize;
        let mut tiles = Vec::new();
        for (pad_x, pad_y, pad_w, pad_h, x, y, w, h) in layout(padded.0, padded.1) {
            let effect = Frei0rEffect::new(plugin.clone(), pad_w, pad_h).map_err(Refusal::Construct)?;
            tiles.push(Tile { effect, pad_x, pad_y, pad_w, pad_h, x, y, w, h });
        }
        let bytes = tiles.iter().map(|t| t.pad_w as usize * t.pad_h as usize * 4 * (1 + history)).sum();
        Ok(Self { width, height, padded, tiles, bytes })
    }

    /// Set the plug-in's parameter `index` on every instance.
    pub fn set_param(&self, index: usize, value: &Frei0rValue) {
        for tile in &self.tiles {
            tile.effect.set_param(index, value);
        }
    }

    /// Set every parameter, in the plug-in's order.
    pub fn set_params(&self, params: &[Frei0rValue]) {
        for (index, value) in params.iter().enumerate() {
            self.set_param(index, value);
        }
    }

    /// Render the next frame, `width × height` RGBA8 in Klipp's byte
    /// order, at `time_secs`.
    pub fn render(&mut self, time_secs: f64, rgba_in: &[u8]) -> Result<Rendered, Refusal> {
        let expected = self.width as usize * self.height as usize * 4;
        if rgba_in.len() != expected {
            return Err(Refusal::BufferSize { got: rgba_in.len(), expected });
        }
        let (pw, ph) = self.padded;
        let fallback = ((pw, ph) != (self.width, self.height)).then_some(Fallback::Padded { from: (self.width, self.height), to: (pw, ph) });
        let padded_in;
        let frame: &[u8] = if fallback.is_some() {
            padded_in = pad_edges(rgba_in, self.width, self.height, pw, ph);
            &padded_in
        } else {
            rgba_in
        };
        let out = if let [tile] = self.tiles.as_slice() {
            tile.effect.run(time_secs, frame)
        } else {
            let mut out = vec![0u8; frame.len()];
            for t in &self.tiles {
                let tile_in = extract_rect(frame, pw, t.pad_x, t.pad_y, t.pad_w, t.pad_h);
                let tile_out = t.effect.run(time_secs, &tile_in);
                blit_rect(&tile_out, t.pad_w, t.x - t.pad_x, t.y - t.pad_y, t.w, t.h, &mut out, pw, t.x, t.y);
            }
            out
        };
        let rgba = if fallback.is_some() { extract_rect(&out, pw, 0, 0, self.width, self.height) } else { out };
        Ok(Rendered { rgba, fallback })
    }

    /// The frame size it renders.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// How many instances it keeps: 1, or one per tile.
    pub fn instances(&self) -> usize {
        self.tiles.len()
    }

    /// An estimate of the memory its instances keep (see the module's
    /// "Memory").
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

/// `v` rounded up to a multiple of 8, at least 8.
fn padded_dim(v: u32) -> u32 {
    v.div_ceil(F0R_DIM_ALIGN).max(1) * F0R_DIM_ALIGN
}

/// The instances for a `w × h` frame (both multiples of 8): one for the
/// whole frame up to 2048, else ≤ TILE_DIM tiles, each with TILE_OVERLAP
/// on its interior sides. Each entry: the padded rect, then the inner.
#[allow(clippy::type_complexity)]
fn layout(w: u32, h: u32) -> Vec<(u32, u32, u32, u32, u32, u32, u32, u32)> {
    if w <= F0R_MAX_DIM && h <= F0R_MAX_DIM {
        return vec![(0, 0, w, h, 0, 0, w, h)];
    }
    let mut rects = Vec::new();
    let mut y = 0;
    while y < h {
        let inner_h = TILE_DIM.min(h - y);
        let mut x = 0;
        while x < w {
            let inner_w = TILE_DIM.min(w - x);
            let pad_x = x.saturating_sub(TILE_OVERLAP);
            let pad_y = y.saturating_sub(TILE_OVERLAP);
            let pad_right = (x + inner_w + TILE_OVERLAP).min(w);
            let pad_bottom = (y + inner_h + TILE_OVERLAP).min(h);
            rects.push((pad_x, pad_y, pad_right - pad_x, pad_bottom - pad_y, x, y, inner_w, inner_h));
            x += inner_w;
        }
        y += inner_h;
    }
    rects
}

/// `src` (`w × h`) in a `pw × ph` frame, its last column and row
/// repeated into the rest.
fn pad_edges(src: &[u8], w: u32, h: u32, pw: u32, ph: u32) -> Vec<u8> {
    let (w, h, pw, ph) = (w as usize, h as usize, pw as usize, ph as usize);
    let mut out = Vec::with_capacity(pw * ph * 4);
    for y in 0..ph {
        let row = &src[y.min(h - 1) * w * 4..][..w * 4];
        out.extend_from_slice(row);
        let last = &row[(w - 1) * 4..];
        for _ in w..pw {
            out.extend_from_slice(last);
        }
    }
    out
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
    fn padded_sizes_are_the_next_multiple_of_8() {
        assert_eq!(padded_dim(1), 8);
        assert_eq!(padded_dim(8), 8);
        assert_eq!(padded_dim(37), 40);
        assert_eq!(padded_dim(540), 544);
        assert_eq!(padded_dim(1080), 1080);
    }

    #[test]
    fn padding_repeats_the_last_column_and_row() {
        // 2×2: pixels 1..4, one byte value per pixel.
        let src: Vec<u8> = [1u8, 2, 3, 4].iter().flat_map(|&v| [v; 4]).collect();
        let out = pad_edges(&src, 2, 2, 3, 3);
        let px: Vec<u8> = out.chunks_exact(4).map(|p| p[0]).collect();
        assert_eq!(px, [1, 2, 2, 3, 4, 4, 3, 4, 4]);
    }

    #[test]
    fn tiles_cover_the_frame_once_with_aligned_rects() {
        for (w, h) in [(2064, 16), (16, 2064), (4096, 2160), (2048, 2048), (40, 24)] {
            let mut covered = vec![0u8; (w * h) as usize];
            for (px, py, pw, ph, x, y, iw, ih) in layout(w, h) {
                for v in [px, py, pw, ph, x, y, iw, ih] {
                    assert!(v.is_multiple_of(F0R_DIM_ALIGN), "{w}×{h}: {v} unaligned");
                }
                assert!(pw <= F0R_MAX_DIM && ph <= F0R_MAX_DIM && pw >= F0R_DIM_ALIGN && ph >= F0R_DIM_ALIGN);
                assert!(px <= x && py <= y && x + iw <= px + pw && y + ih <= py + ph, "inner inside padded");
                for yy in y..y + ih {
                    for xx in x..x + iw {
                        covered[(yy * w + xx) as usize] += 1;
                    }
                }
            }
            assert!(covered.iter().all(|&c| c == 1), "{w}×{h}: every pixel written once");
        }
    }

    #[test]
    fn tile_dim_constants_satisfy_frei0r_constraints() {
        assert!(TILE_DIM <= F0R_MAX_DIM, "tile must fit in one frei0r call");
        assert!(TILE_DIM.is_multiple_of(F0R_DIM_ALIGN));
        assert!((TILE_DIM + 2 * TILE_OVERLAP) <= F0R_MAX_DIM,
            "tile + both-sided overlap must still fit in one call");
        assert!(TILE_OVERLAP.is_multiple_of(F0R_DIM_ALIGN));
    }
}
