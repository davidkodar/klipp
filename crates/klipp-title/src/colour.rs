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
/// `width` x `height`. Layout metrics are read from ColourSource without
/// preparing a resized bitmap.
pub(crate) struct ColourBitmap {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) rgba: Arc<Vec<u8>>,
}

/// A decoded font strike in byte-premultiplied RGBA. Independent of layout size.
pub(crate) struct ColourStrike {
    pub(crate) index: usize,
    pub(crate) width: usize,
    pub(crate) height: usize,
    bearing: [i8; 2],
    pub(crate) rgba: Arc<Vec<u8>>,
}
pub(crate) struct ColourSource {
    pub(crate) strike: Arc<ColourStrike>,
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) xmin: f32,
    pub(crate) ymin: f32,
}
static RASTERIZATIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Diagnostic count of CPU colour-bitmap size preparations, including cache misses.
pub fn colour_bitmap_rasterizations() -> u64 {
    RASTERIZATIONS.load(std::sync::atomic::Ordering::Relaxed)
}

/// A font's colour bitmaps (CBDT with CBLC), drawn as they are asked
/// for and kept by glyph and size.
pub(crate) struct ColourBitmaps {
    cache: Mutex<HashMap<(u16, u32), Option<Arc<ColourBitmap>>>>,
    strikes: Mutex<HashMap<(u16, usize), Arc<ColourStrike>>>,
}

impl ColourBitmaps {
    /// The colour bitmaps of the font in `data`; `None` for a font
    /// without CBDT and CBLC tables.
    pub(crate) fn of(data: &[u8]) -> Option<Self> {
        let font = FontRef::from_index(data, 0).ok()?;
        font.cblc().ok()?;
        font.cbdt().ok()?;
        Some(Self {
            cache: Mutex::new(HashMap::new()),
            strikes: Mutex::new(HashMap::new()),
        })
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
        let drawn = self
            .source(data, gid, px)
            .map(|source| Arc::new(draw(source)));
        cache.insert((gid, px.to_bits()), drawn.clone());
        drawn
    }
}

impl ColourBitmaps {
    /// Select and decode a source strike once; layout needs only these metrics.
    pub(crate) fn source(&self, data: &[u8], gid: u16, px: f32) -> Option<ColourSource> {
        if !px.is_finite() {
            return None;
        }
        let font = FontRef::from_index(data, 0).ok()?;
        let cblc = font.cblc().ok()?;
        let sizes = cblc.bitmap_sizes();
        let (index, size) = sizes
            .iter()
            .enumerate()
            .filter(|(_, s)| s.ppem_y() as f32 >= px)
            .min_by_key(|(_, s)| s.ppem_y())
            .or_else(|| sizes.iter().enumerate().max_by_key(|(_, s)| s.ppem_y()))?;
        let ppem = size.ppem_y();
        let mut strikes = self.strikes.lock().unwrap_or_else(|e| e.into_inner());
        let strike = if let Some(s) = strikes.get(&(gid, index)) {
            Arc::clone(s)
        } else {
            let cbdt = font.cbdt().ok()?;
            let location = size
                .location(cblc.offset_data(), GlyphId::new(gid as u32))
                .ok()?;
            let bitmap = cbdt.data(&location).ok()?;
            let BitmapContent::Data(BitmapDataFormat::Png, png) = bitmap.content else {
                return None;
            };
            let bearing = match bitmap.metrics {
                BitmapMetrics::Small(m) => [m.bearing_x.get(), m.bearing_y.get()],
                BitmapMetrics::Big(m) => [m.hori_bearing_x.get(), m.hori_bearing_y.get()],
            };
            let mut image = image::load_from_memory_with_format(png, image::ImageFormat::Png)
                .ok()?
                .to_rgba8();
            for p in image.pixels_mut() {
                let a = p[3] as u32;
                for c in &mut p.0[..3] {
                    *c = ((*c as u32 * a + 127) / 255) as u8;
                }
            }
            let strike = Arc::new(ColourStrike {
                index,
                width: image.width() as usize,
                height: image.height() as usize,
                bearing,
                rgba: Arc::new(image.into_raw()),
            });
            if strikes.len() >= CACHE_CAP
                || strikes.values().map(|s| s.rgba.len()).sum::<usize>() + strike.rgba.len()
                    > 64 << 20
            {
                strikes.clear();
            }
            if strike.rgba.len() <= 64 << 20 {
                strikes.insert((gid, index), Arc::clone(&strike));
            }
            strike
        };
        let scale = px / ppem.max(1) as f32;
        let width = ((strike.width as f32 * scale).round() as usize).max(1);
        let height = ((strike.height as f32 * scale).round() as usize).max(1);
        Some(ColourSource {
            xmin: (strike.bearing[0] as f32 * scale).round(),
            ymin: (strike.bearing[1] as f32 * scale).round() - height as f32,
            width,
            height,
            strike,
        })
    }
}

fn draw(source: ColourSource) -> ColourBitmap {
    RASTERIZATIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let pre = image::RgbaImage::from_raw(
        source.strike.width as u32,
        source.strike.height as u32,
        source.strike.rgba.as_ref().clone(),
    )
    .unwrap();
    let mut scaled = image::imageops::resize(
        &pre,
        source.width as u32,
        source.height as u32,
        image::imageops::FilterType::Triangle,
    );
    for p in scaled.pixels_mut() {
        let a = p[3] as u32;
        for c in &mut p.0[..3] {
            if let Some(v) = (*c as u32 * 255 + a / 2).checked_div(a) {
                *c = v.min(255) as u8;
            }
        }
    }
    ColourBitmap {
        width: source.width,
        height: source.height,
        rgba: Arc::new(scaled.into_raw()),
    }
}
