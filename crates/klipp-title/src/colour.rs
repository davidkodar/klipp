//! Colour glyphs for titles (#1503): emoji and the other glyphs a font
//! draws as colour bitmaps, in its CBDT table as Noto Color Emoji does or
//! in an sbix table as Apple's fonts do (#3238). fontdue draws outlines
//! only, and a bitmap font has none; its glyphs are read here with
//! read-fonts, the PNG decoded, and scaled from the strike nearest the
//! size to the size.
//!
//! A font with more than one kind of colour glyph draws the first of
//! [`PRECEDENCE`] it has that Klipp draws.
//!
//! A colour glyph is laid out as an outline glyph is, with the scaled
//! bitmap's box as its bounds. Its alpha is its coverage, so outlines,
//! shadows and glows go round it as round a letter; the face draws its
//! colours where the text fill would draw.
//!
//! COLR v0 and v1 glyphs are drawn too (`colr.rs`): each at a size from
//! a ladder of sizes at or above the one asked for, made a strike, and
//! scaled to the size as a bitmap strike is. The text's own colour in a
//! COLR glyph (palette index 0xFFFF) is black for now, as FreeType and
//! Skia draw it by default.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use read_fonts::tables::bitmap::{BitmapContent, BitmapDataFormat, BitmapMetrics};
use read_fonts::types::{GlyphId, Tag};
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
    /// The bitmap's left edge and top from the glyph's origin, in the
    /// strike's pixels, y up.
    bearing: [i32; 2],
    pub(crate) rgba: Arc<Vec<u8>>,
}
pub(crate) struct ColourSource {
    pub(crate) strike: Arc<ColourStrike>,
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) xmin: f32,
    pub(crate) ymin: f32,
}
thread_local! { static RASTERIZATIONS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; }
/// Diagnostic count of CPU colour-bitmap size preparations, including cache
/// misses, on the calling thread: they are made on the thread that draws the
/// title, so tests running side by side count only their own.
pub fn colour_bitmap_rasterizations() -> u64 {
    RASTERIZATIONS.with(std::cell::Cell::get)
}

/// The kinds of colour glyph a font can carry, in the order a font with
/// several draws them (#3238): its vector glyphs before its bitmaps, as
/// browsers do, so a font with COLR and CBDT both is drawn sharp at any
/// size.
pub(crate) const PRECEDENCE: [ColourFormat; 4] = [ColourFormat::ColrV1, ColourFormat::ColrV0, ColourFormat::Sbix, ColourFormat::Cbdt];

/// A kind of colour glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColourFormat {
    ColrV1,
    ColrV0,
    Sbix,
    Cbdt,
}

impl ColourFormat {
    /// Whether the font in `font` carries this kind.
    fn in_font(self, font: &FontRef) -> bool {
        match self {
            Self::ColrV1 => font.colr().is_ok_and(|c| c.version() >= 1),
            Self::ColrV0 => font.colr().is_ok_and(|c| c.num_base_glyph_records() > 0),
            Self::Sbix => font.sbix().is_ok(),
            Self::Cbdt => font.cblc().is_ok() && font.cbdt().is_ok(),
        }
    }

    /// Whether Klipp draws this kind yet.
    fn drawn(self) -> bool {
        matches!(self, Self::ColrV1 | Self::ColrV0 | Self::Sbix | Self::Cbdt)
    }
}

/// The colour glyphs the font in `data` draws, by [`PRECEDENCE`].
pub(crate) fn format_of(data: &[u8]) -> Option<ColourFormat> {
    let font = FontRef::from_index(data, 0).ok()?;
    PRECEDENCE.into_iter().find(|f| f.drawn() && f.in_font(&font))
}

/// A font's colour bitmaps (CBDT with CBLC, or sbix), drawn as they are
/// asked for and kept by glyph and size.
pub(crate) struct ColourBitmaps {
    format: ColourFormat,
    cache: Mutex<HashMap<(u16, u32), Option<Arc<ColourBitmap>>>>,
    strikes: Mutex<HashMap<(u16, usize), Arc<ColourStrike>>>,
}

impl ColourBitmaps {
    /// The colour bitmaps of the font in `data`; `None` for a font with
    /// no colour glyphs Klipp draws.
    pub(crate) fn of(data: &[u8]) -> Option<Self> {
        Some(Self {
            format: format_of(data)?,
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
        let (index, ppem) = match self.format {
            ColourFormat::Cbdt => nearest_strike(&font.cblc().ok()?.bitmap_sizes().iter().map(|s| s.ppem_y() as u16).collect::<Vec<_>>(), px)?,
            ColourFormat::Sbix => nearest_strike(&font.sbix().ok()?.strikes().iter().map(|s| s.map_or(0, |s| s.ppem())).collect::<Vec<_>>(), px)?,
            ColourFormat::ColrV0 | ColourFormat::ColrV1 => colr_size(px),
        };
        let mut strikes = self.strikes.lock().unwrap_or_else(|e| e.into_inner());
        let strike = if let Some(s) = strikes.get(&(gid, index)) {
            Arc::clone(s)
        } else {
            if let Some(colr) = match self.format {
                ColourFormat::ColrV1 => Some(skrifa::color::ColorGlyphFormat::ColrV1),
                ColourFormat::ColrV0 => Some(skrifa::color::ColorGlyphFormat::ColrV0),
                ColourFormat::Sbix | ColourFormat::Cbdt => None,
            } {
                let drawn = crate::colr::draw(data, gid, f32::from(ppem), colr, COLR_FOREGROUND)?;
                let strike = Arc::new(ColourStrike { index, width: drawn.width, height: drawn.height, bearing: [drawn.left, drawn.top], rgba: Arc::new(drawn.rgba) });
                keep_strike(&mut strikes, (gid, index), &strike);
                return Some(scaled(strike, px, ppem));
            }
            let (png, bearing) = match self.format {
                ColourFormat::Cbdt => {
                    let cblc = font.cblc().ok()?;
                    let size = cblc.bitmap_sizes().get(index)?;
                    let location = size.location(cblc.offset_data(), GlyphId::new(gid as u32)).ok()?;
                    let bitmap = font.cbdt().ok()?.data(&location).ok()?;
                    let BitmapContent::Data(BitmapDataFormat::Png, png) = bitmap.content else {
                        return None;
                    };
                    let bearing = match bitmap.metrics {
                        BitmapMetrics::Small(m) => [m.bearing_x.get(), m.bearing_y.get()],
                        BitmapMetrics::Big(m) => [m.hori_bearing_x.get(), m.hori_bearing_y.get()],
                    };
                    (png, Bearing::Top(bearing.map(i32::from)))
                }
                ColourFormat::Sbix => {
                    let strike = font.sbix().ok()?.strikes().get(index).ok()?;
                    let glyph = strike.glyph_data(GlyphId::new(gid as u32)).ok()??;
                    // PNG only: JPEG and TIFF graphics are rare, and a
                    // `dupe` stands for another glyph's (not drawn).
                    if glyph.graphic_type() != Tag::new(b"png ") {
                        return None;
                    }
                    (glyph.data(), Bearing::Bottom([glyph.origin_offset_x().into(), glyph.origin_offset_y().into()]))
                }
                ColourFormat::ColrV1 | ColourFormat::ColrV0 => return None,
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
            let height = image.height() as usize;
            let bearing = match bearing {
                Bearing::Top(b) => b,
                // An sbix origin offset is the bitmap's bottom left.
                Bearing::Bottom([x, y]) => [x, y + height as i32],
            };
            let strike = Arc::new(ColourStrike {
                index,
                width: image.width() as usize,
                height,
                bearing,
                rgba: Arc::new(image.into_raw()),
            });
            keep_strike(&mut strikes, (gid, index), &strike);
            strike
        };
        Some(scaled(strike, px, ppem))
    }
}

/// The text's own colour in a COLR glyph, for now: opaque black.
const COLR_FOREGROUND: [u8; 4] = [0, 0, 0, 255];

/// The size a COLR glyph is drawn at for `px`, and its step on the ladder:
/// the first of 16 px times the fourth roots of two at or above `px`, at
/// most 1536. A title growing on screen draws each step once, each frame
/// scaling the nearest down, as a bitmap font's strikes are.
fn colr_size(px: f32) -> (usize, u16) {
    let step = (4.0 * (px.max(16.0) / 16.0).log2()).ceil().clamp(0.0, 26.0) as usize;
    (step, (16.0 * 2f32.powf(step as f32 / 4.0)).round().min(1536.0) as u16)
}

/// `strike` kept, under `key`, the cache emptied first when it is full.
fn keep_strike(strikes: &mut HashMap<(u16, usize), Arc<ColourStrike>>, key: (u16, usize), strike: &Arc<ColourStrike>) {
    if strikes.len() >= CACHE_CAP || strikes.values().map(|s| s.rgba.len()).sum::<usize>() + strike.rgba.len() > 64 << 20 {
        strikes.clear();
    }
    if strike.rgba.len() <= 64 << 20 {
        strikes.insert(key, Arc::clone(strike));
    }
}

/// `strike`, drawn at `ppem`, laid out at `px`.
fn scaled(strike: Arc<ColourStrike>, px: f32, ppem: u16) -> ColourSource {
    let scale = px / f32::from(ppem.max(1));
    let width = ((strike.width as f32 * scale).round() as usize).max(1);
    let height = ((strike.height as f32 * scale).round() as usize).max(1);
    ColourSource {
        xmin: (strike.bearing[0] as f32 * scale).round(),
        ymin: (strike.bearing[1] as f32 * scale).round() - height as f32,
        width,
        height,
        strike,
    }
}

/// Where a strike's bitmap sits: its top left (CBDT's bearings) or its
/// bottom left (sbix's origin offset), from the glyph's origin.
enum Bearing {
    Top([i32; 2]),
    Bottom([i32; 2]),
}

/// The strike to draw `px` from: the smallest at or above it, else the
/// largest, by its index among `ppems` and its size.
fn nearest_strike(ppems: &[u16], px: f32) -> Option<(usize, u16)> {
    let indexed = || ppems.iter().copied().enumerate().filter(|(_, p)| *p > 0);
    indexed().filter(|(_, p)| f32::from(*p) >= px).min_by_key(|(_, p)| *p).or_else(|| indexed().max_by_key(|(_, p)| *p))
}

fn draw(source: ColourSource) -> ColourBitmap {
    RASTERIZATIONS.with(|n| n.set(n.get() + 1));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn font(name: &str) -> Vec<u8> {
        std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fonts").join(name)).unwrap()
    }

    fn rainbow(data: &[u8]) -> u16 {
        let font = FontRef::from_index(data, 0).unwrap();
        font.cmap().unwrap().map_codepoint('🌈').unwrap().to_u32() as u16
    }

    /// Each font draws the kind of colour glyph it carries, and a font
    /// with sbix and CBDT both draws its sbix glyphs (#3238); a font
    /// with none draws none.
    #[test]
    fn a_font_draws_the_first_kind_of_colour_glyph_it_has() {
        assert_eq!(format_of(&font("NotoColorEmoji-cbdt-subset.ttf")), Some(ColourFormat::Cbdt));
        assert_eq!(format_of(&font("NotoColorEmoji-sbix-subset.ttf")), Some(ColourFormat::Sbix));
        assert_eq!(format_of(&font("NotoColorEmoji-sbix-cbdt-subset.ttf")), Some(ColourFormat::Sbix));
        assert_eq!(format_of(&font("NotoSansHebrew-subset.ttf")), None);
        let both = font("NotoColorEmoji-sbix-cbdt-subset.ttf");
        assert_eq!(ColourBitmaps::of(&both).unwrap().format, ColourFormat::Sbix);
    }

    /// An sbix glyph is drawn as the same bitmap's CBDT glyph is: the
    /// same size, the same place from its origin and the same pixels, at
    /// sizes below, at and above the strike's.
    #[test]
    fn an_sbix_glyph_draws_as_its_cbdt_twin() {
        let (cbdt, sbix) = (font("NotoColorEmoji-cbdt-subset.ttf"), font("NotoColorEmoji-sbix-subset.ttf"));
        let (a, b) = (ColourBitmaps::of(&cbdt).unwrap(), ColourBitmaps::of(&sbix).unwrap());
        let (ga, gb) = (rainbow(&cbdt), rainbow(&sbix));
        for px in [24.0, 64.0, 109.0, 300.0] {
            let (sa, sb) = (a.source(&cbdt, ga, px).unwrap(), b.source(&sbix, gb, px).unwrap());
            assert_eq!((sa.width, sa.height, sa.xmin, sa.ymin), (sb.width, sb.height, sb.xmin, sb.ymin), "at {px} px");
            let (da, db) = (a.glyph(&cbdt, ga, px).unwrap(), b.glyph(&sbix, gb, px).unwrap());
            assert_eq!(da.rgba, db.rgba, "the pixels at {px} px");
            let hues = db.rgba.chunks_exact(4).filter(|p| p[3] > 200 && p[0].max(p[1]).max(p[2]) - p[0].min(p[1]).min(p[2]) > 80).count();
            assert!(hues > 20, "the rainbow drawn in colour at {px} px");
        }
    }

    /// A font arrives with a shared project, so its sbix table is read as
    /// untrusted: offsets past its end, strikes that are not there and
    /// glyph data that is not a picture draw nothing, and never panic.
    #[test]
    fn a_damaged_sbix_table_draws_nothing() {
        let good = font("NotoColorEmoji-sbix-subset.ttf");
        let gid = rainbow(&good);
        let record = FontRef::from_index(&good, 0).unwrap().table_directory.table_records().iter().find(|r| r.tag() == Tag::new(b"sbix")).unwrap();
        let (start, len) = (record.offset() as usize, record.length() as usize);
        // Past the version and flags: the strike count, its offsets, and
        // the strikes' own offsets and data, overwritten one way and then
        // another.
        for fill in [0xFFu8, 0x00, 0x7F] {
            for from in [4, 8, 12, len / 2] {
                let mut bad = good.clone();
                bad[start + from..start + len].fill(fill);
                if let Some(b) = ColourBitmaps::of(&bad) {
                    for px in [16.0, 109.0, 4000.0] {
                        let _ = b.glyph(&bad, gid, px);
                    }
                }
            }
        }
    }

    /// A COLR v0 font draws its colour glyphs: drawn at the ladder's step
    /// at or above the size and scaled to it, close to FreeType's drawing
    /// at the size itself, and placed as FreeType places it (#3238). Its
    /// glyph with no colour layers falls back to its outline.
    #[test]
    fn a_colr_v0_glyph_is_a_colour_glyph() {
        let data = font("KlippTestColrV0.ttf");
        assert_eq!(format_of(&data), Some(ColourFormat::ColrV0));
        let colr = ColourBitmaps::of(&data).unwrap();
        let cmap = |c: char| FontRef::from_index(&data, 0).unwrap().cmap().unwrap().map_codepoint(c).unwrap().to_u32() as u16;
        for px in [24.0f32, 64.0, 150.0] {
            let source = colr.source(&data, cmap('A'), px).unwrap();
            let drawn = colr.glyph(&data, cmap('A'), px).unwrap();
            let golden = std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/goldens/KlippTestColrV0-A-{px}.rgba"))).unwrap();
            let top = (source.ymin + source.height as f32) as i32;
            let (worst, mean, _) = crate::colr::tests::against_golden(drawn.width, drawn.height, source.xmin as i32, top, &drawn.rgba, &golden);
            eprintln!("COLR v0 through the strike path at {px} px: worst {worst}, mean {mean:.3}");
            assert!(mean < 6.0, "at {px} px: mean {mean:.3} levels from FreeType");
        }
        assert!(colr.source(&data, cmap('B'), 48.0).is_none(), "no colour layers, an outline");
    }

    /// A COLR v1 font draws its paint graphs through the strike path: a
    /// gradient glyph comes out in its gradient's colours.
    #[test]
    fn a_colr_v1_glyph_is_a_colour_glyph() {
        let data = font("test_glyphs-glyf_colr_1.ttf");
        assert_eq!(format_of(&data), Some(ColourFormat::ColrV1));
        let colr = ColourBitmaps::of(&data).unwrap();
        let index: serde_json::Value = serde_json::from_slice(&std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/colrv1/origins.json")).unwrap()).unwrap();
        let code = index["origins"]["sweep_0_360_pad_narrow"][2].as_u64().unwrap() as u32;
        let gid = FontRef::from_index(&data, 0).unwrap().cmap().unwrap().map_codepoint(code).unwrap().to_u32() as u16;
        let drawn = colr.glyph(&data, gid, 64.0).unwrap();
        let hues: std::collections::BTreeSet<u8> = drawn.rgba.chunks_exact(4).filter(|p| p[3] > 200).map(|p| (p[0] / 64) * 16 + (p[1] / 64) * 4 + p[2] / 64).collect();
        assert!(hues.len() >= 4, "a sweep of several colours, not one: {hues:?}");
    }

    /// A glyph the strike has no bitmap for, the font's `.notdef`, is no
    /// colour glyph: it falls back to its outline.
    #[test]
    fn a_glyph_without_a_bitmap_is_no_colour_glyph() {
        let sbix = font("NotoColorEmoji-sbix-subset.ttf");
        assert!(ColourBitmaps::of(&sbix).unwrap().source(&sbix, 0, 48.0).is_none());
    }
}
