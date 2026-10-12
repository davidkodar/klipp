//! Colour glyphs for titles (#1503): emoji and the other glyphs a font
//! draws as colour bitmaps in its CBDT table, as Noto Color Emoji does.
//! fontdue draws outlines only, and a bitmap font has none; its glyphs
//! are read here with read-fonts, the PNG decoded, and scaled from the
//! strike nearest the size to the size.
//!
//! A colour glyph is laid out as an outline glyph is, with the scaled
//! bitmap's box as its bounds. Its alpha is its coverage, so outlines,
//! shadows and glows go round it as round a letter; the face draws its
//! colours where the text fill would draw.
//!
//! COLR (v0 and v1) and sbix colour glyphs are not drawn yet: a font
//! with them draws its outlines, where it has any, in the text colour.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use read_fonts::tables::bitmap::{BitmapContent, BitmapDataFormat, BitmapMetrics};
use read_fonts::types::GlyphId;
use read_fonts::{FontRef, TableProvider};

/// Colour bitmaps cached per font, before the cache is emptied: a long
/// title's emoji at a few sizes.
const CACHE_CAP: usize = 512;

/// A colour glyph drawn at a size: straight (not premultiplied) RGBA,
/// `width` x `height`, and where its box sits from the pen on the
/// baseline, `xmin` across and its bottom `ymin` up, as fontdue's
/// outline bounds give them.
pub(crate) struct ColourBitmap {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) xmin: f32,
    pub(crate) ymin: f32,
    pub(crate) rgba: Arc<Vec<u8>>,
}

/// A font's colour bitmaps (CBDT with CBLC), drawn as they are asked
/// for and kept by glyph and size.
pub(crate) struct ColourBitmaps {
    cache: Mutex<HashMap<(u16, u32), Option<Arc<ColourBitmap>>>>,
}

impl ColourBitmaps {
    /// The colour bitmaps of the font in `data`; `None` for a font
    /// without CBDT and CBLC tables.
    pub(crate) fn of(data: &[u8]) -> Option<Self> {
        let font = FontRef::from_index(data, 0).ok()?;
        font.cblc().ok()?;
        font.cbdt().ok()?;
        Some(Self { cache: Mutex::new(HashMap::new()) })
    }

    /// Glyph `gid` of the font in `data` drawn at `px` pixels per em;
    /// `None` for a glyph the font has no colour bitmap for.
    pub(crate) fn glyph(&self, data: &[u8], gid: u16, px: f32) -> Option<Arc<ColourBitmap>> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(hit) = cache.get(&(gid, px.to_bits())) {
            return hit.clone();
        }
        if cache.len() >= CACHE_CAP {
            cache.clear();
        }
        let drawn = draw(data, gid, px).map(Arc::new);
        cache.insert((gid, px.to_bits()), drawn.clone());
        drawn
    }
}

/// Glyph `gid` from the strike with the smallest size at or above
/// `px` (the largest when none is that large), scaled to `px`.
fn draw(data: &[u8], gid: u16, px: f32) -> Option<ColourBitmap> {
    let font = FontRef::from_index(data, 0).ok()?;
    let (cblc, cbdt) = (font.cblc().ok()?, font.cbdt().ok()?);
    let sizes = cblc.bitmap_sizes();
    let size = sizes
        .iter()
        .filter(|s| s.ppem_y() as f32 >= px)
        .min_by_key(|s| s.ppem_y())
        .or_else(|| sizes.iter().max_by_key(|s| s.ppem_y()))?;
    let location = size.location(cblc.offset_data(), GlyphId::new(gid as u32)).ok()?;
    let bitmap = cbdt.data(&location).ok()?;
    let BitmapContent::Data(BitmapDataFormat::Png, png) = bitmap.content else { return None };
    let (bearing_x, bearing_y) = match bitmap.metrics {
        BitmapMetrics::Small(m) => (m.bearing_x.get(), m.bearing_y.get()),
        BitmapMetrics::Big(m) => (m.hori_bearing_x.get(), m.hori_bearing_y.get()),
    };
    let image = image::load_from_memory_with_format(png, image::ImageFormat::Png).ok()?.to_rgba8();
    let scale = px / size.ppem_y().max(1) as f32;
    let width = ((image.width() as f32 * scale).round() as u32).max(1);
    let height = ((image.height() as f32 * scale).round() as u32).max(1);
    // Scaled premultiplied, so a transparent pixel's colour does not
    // bleed into the edge, then made straight again.
    let mut pre = image;
    for p in pre.pixels_mut() {
        let a = p[3] as u32;
        for c in &mut p.0[..3] {
            *c = ((*c as u32 * a + 127) / 255) as u8;
        }
    }
    let mut scaled = image::imageops::resize(&pre, width, height, image::imageops::FilterType::Triangle);
    for p in scaled.pixels_mut() {
        let a = p[3] as u32;
        for c in &mut p.0[..3] {
            if let Some(v) = (*c as u32 * 255 + a / 2).checked_div(a) {
                *c = v.min(255) as u8;
            }
        }
    }
    let top = bearing_y as f32 * scale;
    Some(ColourBitmap {
        width: width as usize,
        height: height as usize,
        xmin: (bearing_x as f32 * scale).round(),
        ymin: top.round() - height as f32,
        rgba: Arc::new(scaled.into_raw()),
    })
}
