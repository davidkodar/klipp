//! Title / text rasteriser: layout, glyph compositing, and style runs.
//!
//! Given a [`klipp_timeline::TitleData`] — text content, font choice,
//! size, colour, alignment, shadow / outline / glow / gradient /
//! pattern / motion-blur parameters, plus per-character style runs for
//! mixed formatting — this crate produces an RGBA buffer ready for
//! compositing over a video frame.
//!
//! Layout runs through `fontdue` (pure-Rust TTF/OTF rasteriser) so the
//! crate has no C dependency. System font discovery is in
//! [`font_discovery`] and is cached at startup via
//! `~/.cache/klipp/font_registry.json`.
//!
//! # Alpha mask processor
//!
//! [`AlphaMaskProcessor`] is the trait the title renderer calls for
//! GPU-accelerated alpha-mask work — `blur` and `dilate`, which is
//! what glows and shadows need. The implementation lives in
//! `klipp-compose` (`GpuAlphaMaskAdapter`) to avoid a circular dep —
//! pass one in via [`TitleRenderer::render_with_gpu`]. Without one the
//! renderer falls back to its own CPU blur and dilate.
//!
//! # Fallback font
//!
//! An embedded Noto Sans Regular (OFL) is linked in so titles always
//! render, even when the user's system has no matching font. It's
//! picked automatically when the requested font can't be resolved.

mod vector_path;
use fontdue::layout::{
    CoordinateSystem, HorizontalAlign, Layout, LayoutSettings, TextStyle, VerticalAlign,
    WrapStyle,
};
use fontdue::Font;
use klipp_timeline::{
    FontStyle, ImageFit, ImageLayer, ObjectTransform, Shape, ShapeKind, StyleRun,
    TextAlignment, TextDirection, TextPath, TextPattern, TitleBackground, TitleData,
    TitleObjectContent,
};
use std::collections::HashMap;

pub mod bundled;
pub mod font_discovery;

/// Embedded fallback font — Noto Sans Regular (OFL license).
static EMBEDDED_FONT: &[u8] = include_bytes!("../assets/NotoSans-Regular.ttf");

/// Trait for GPU-accelerated alpha mask operations.
/// Implemented in klipp-engine to avoid circular dependencies.
pub trait AlphaMaskProcessor {
    fn blur(&mut self, mask: &[u8], w: u32, h: u32, radius: f32) -> Vec<u8>;
    fn dilate(&mut self, mask: &[u8], w: u32, h: u32, radius: f32) -> Vec<u8>;
}

/// Result of the layout phase — alpha mask + bounding box info.
pub struct TitleLayout {
    /// Single-channel alpha mask of rasterized text.
    pub alpha_mask: Vec<u8>,
    /// Mask width (may be smaller than output width — covers text + effect padding).
    pub mask_w: u32,
    /// Mask height.
    pub mask_h: u32,
    /// Mask origin X in the output frame.
    pub mask_x0: u32,
    /// Mask origin Y in the output frame.
    pub mask_y0: u32,
    /// Text bounding box top (global frame coords, for gradient/pattern).
    pub text_top: f32,
    /// Text bounding box bottom.
    pub text_bottom: f32,
    /// Text bounding box left.
    pub text_left: f32,
    /// Text bounding box right.
    pub text_right: f32,
    /// Number of glyphs rendered.
    pub glyph_count: usize,
    /// Scale factor (height / 1080.0).
    pub scale: f32,
    /// Per-glyph color data for rich text (style runs). Empty = uniform fill.
    /// Each entry: (mask_local_x, mask_local_y, glyph_w, glyph_h, color_rgba).
    /// The coverage data for each glyph is stored in `glyph_coverages`.
    pub glyph_colors: Vec<GlyphColorInfo>,
    /// Per-glyph coverage bitmaps for rich text fill. Empty = use alpha_mask with uniform color.
    pub glyph_coverages: Vec<Vec<u8>>,
    /// Which glyph each mask pixel belongs to (`GLYPH_NONE` for none),
    /// mask-local; filled only for a title with a staggered motion.
    pub glyph_index: Vec<u16>,
    /// Each glyph's bitmap box in frame pixels, `[x0, y0, x1, y1]`.
    pub glyph_boxes: Vec<[i32; 4]>,
    /// Each glyph's character, for the unit it staggers with.
    pub glyph_parents: Vec<char>,
    /// The padding the mask carries round the text for its effects.
    pub pad: i32,
}

/// No glyph: the value of `glyph_index` where nothing was painted.
pub const GLYPH_NONE: u16 = u16::MAX;

/// Which glyph each pixel of a resting render belongs to, over the
/// region round the text, with the glyph boxes and characters a
/// staggered motion poses them by. Pixels of glow, shadow and outline
/// go to the nearest glyph box.
#[derive(Debug, Clone)]
pub struct UnitMap {
    pub x0: u32,
    pub y0: u32,
    pub w: u32,
    pub h: u32,
    pub index: Vec<u16>,
    pub boxes: Vec<[i32; 4]>,
    pub parents: Vec<char>,
    pub pad: i32,
}

impl UnitMap {
    fn from_layout(layout: &TitleLayout, buf: &[u8], width: u32, height: u32) -> Self {
        let (x0, y0, w, h) = (layout.mask_x0, layout.mask_y0, layout.mask_w, layout.mask_h);
        let mut index = layout.glyph_index.clone();
        index.resize((w * h) as usize, GLYPH_NONE);
        let boxes = &layout.glyph_boxes;
        let fw = width as usize;
        for my in 0..h as usize {
            let fy = y0 as usize + my;
            if fy >= height as usize {
                break;
            }
            for mx in 0..w as usize {
                let fx = x0 as usize + mx;
                if fx >= fw {
                    break;
                }
                let i = my * w as usize + mx;
                if index[i] != GLYPH_NONE || buf[(fy * fw + fx) * 4 + 3] == 0 {
                    continue;
                }
                // The nearest glyph box, by the distance to its edges.
                let (px, py) = (fx as f32 + 0.5, fy as f32 + 0.5);
                let mut best = (f32::MAX, GLYPH_NONE);
                for (g, b) in boxes.iter().enumerate() {
                    if b[2] <= b[0] || b[3] <= b[1] {
                        continue;
                    }
                    let ddx = (b[0] as f32 - px).max(px - b[2] as f32).max(0.0);
                    let ddy = (b[1] as f32 - py).max(py - b[3] as f32).max(0.0);
                    let d = ddx * ddx + ddy * ddy;
                    if d < best.0 {
                        best = (d, g as u16);
                    }
                }
                index[i] = best.1;
            }
        }
        Self { x0, y0, w, h, index, boxes: layout.glyph_boxes.clone(), parents: layout.glyph_parents.clone(), pad: layout.pad }
    }

    /// The glyph at frame pixel `(x, y)`, if any.
    #[inline]
    pub fn at(&self, x: i64, y: i64) -> u16 {
        let (mx, my) = (x - self.x0 as i64, y - self.y0 as i64);
        if mx < 0 || my < 0 || mx >= self.w as i64 || my >= self.h as i64 {
            return GLYPH_NONE;
        }
        self.index[my as usize * self.w as usize + mx as usize]
    }
}

/// One glyph's pose in a staggered motion: where the resting glyph
/// goes, scaled about `anchor`, and the rows it may show in (a mask
/// motion clips it to its resting box). `moved` is the frame box the
/// posed glyph can touch.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlyphPose {
    pub dx: f32,
    pub dy: f32,
    pub scale: f32,
    pub alpha: f32,
    pub anchor: (f32, f32),
    /// Rows `y0..y1` the glyph may paint; `None` for all.
    pub clip: Option<(i32, i32)>,
    pub moved: [i32; 4],
}

impl GlyphPose {
    /// The sixteen numbers the GPU pass reads per glyph.
    pub fn packed(&self) -> [f32; 16] {
        let (c0, c1) = self.clip.map_or((0.0, -1.0), |(a, b)| (a as f32, b as f32));
        [
            self.moved[0] as f32, self.moved[1] as f32, self.moved[2] as f32, self.moved[3] as f32,
            self.dx, self.dy, self.scale, self.alpha,
            self.anchor.0, self.anchor.1, c0, c1,
            0.0, 0.0, 0.0, 0.0,
        ]
    }
}

/// A resting render and, for a title whose motion staggers, the map of
/// which glyph each pixel is: what `render_resting` makes and `adopt`
/// takes.
pub struct RestingRender {
    pub pixels: Vec<u8>,
    pub units: Option<std::sync::Arc<UnitMap>>,
}

/// Color and position info for a single glyph in rich text mode.
#[derive(Debug, Clone)]
pub struct GlyphColorInfo {
    /// X position in mask-local coordinates.
    pub mask_x: i32,
    /// Y position in mask-local coordinates.
    pub mask_y: i32,
    /// Glyph bitmap width.
    pub width: usize,
    /// Glyph bitmap height.
    pub height: usize,
    /// RGBA fill color for this glyph.
    pub color: [u8; 4],
}

/// Position info for a single character, used for cursor placement and click hit-testing.
#[derive(Debug, Clone)]
pub struct CharPosition {
    /// Character index in the string (0-based, counting chars not bytes).
    pub char_index: usize,
    /// Byte offset in the source string.
    pub byte_offset: usize,
    /// Left edge x position in frame pixels.
    pub x: f32,
    /// Top edge y position in frame pixels.
    pub y: f32,
    /// Advance width of the character cell in frame pixels.
    pub advance: f32,
    /// Line height in frame pixels.
    pub height: f32,
}

/// One entry in `TitleRenderer`'s rasterized-frame cache. Keyed by
/// the full `TitleData` value + output dimensions; on a hit we
/// memcpy the cached `Arc<Vec<u8>>` into the caller's buffer instead
/// of re-running the cosmic-text shaping + glyph rasterisation, the
/// outline / glow / extrusion compositing, and the texture-fill pass.
///
/// For a 1920×1080 RGBA buffer that's ~8 MB of memcpy vs ~10–30 ms
/// of CPU work — the difference shows up directly as smooth vs
/// stuttering playback for any title whose CONTENT doesn't change
/// frame-to-frame (everything except `Typewriter`, where `write_on`
/// advances per frame and naturally invalidates the cache).
struct CachedRender {
    title: TitleData,
    width: u32,
    height: u32,
    buffer: std::sync::Arc<Vec<u8>>,
    /// The rows with any coverage, `y0..y1`.
    rows: (u32, u32),
    /// The columns with any coverage, `x0..x1`.
    cols: (u32, u32),
    units: Option<std::sync::Arc<UnitMap>>,
    /// Names this render to whoever keeps a copy of it elsewhere (a
    /// GPU sprite): never reused within a renderer.
    id: u64,
}

/// A title's resting render, as cached: frame-sized straight-alpha
/// sRGB RGBA8, the rows with any coverage, and an id that names it.
#[derive(Clone)]
pub struct RestingSprite {
    pub pixels: std::sync::Arc<Vec<u8>>,
    pub rows: (u32, u32),
    /// The columns with any coverage, `x0..x1`; with `rows`, the box a
    /// pose scales about.
    pub cols: (u32, u32),
    pub id: u64,
    /// Which glyph each pixel is, for a staggered motion; made only
    /// for a title that has one.
    pub units: Option<std::sync::Arc<UnitMap>>,
}

impl RestingSprite {
    /// The centre of the covered box, in frame pixels: what a scale
    /// keeps in place.
    pub fn anchor(&self) -> (f32, f32) {
        ((self.cols.0 + self.cols.1) as f32 * 0.5, (self.rows.0 + self.rows.1) as f32 * 0.5)
    }
}

/// A title's frame at a time, in the cheapest form that is exact.
pub enum TitleFrame {
    /// The resting render moved by `(dx, dy)` pixels with its alpha
    /// scaled by `opacity`: a static title, or one whose only motion at
    /// this time is a scroll, crawl, shake, blink, pulse or fade. `rows`
    /// are the destination rows the moved sprite covers. The frame's
    /// bytes are those `shift_and_fade` would make; a compositor that
    /// keeps the sprite can make them where it composites.
    /// `scale` grows or shrinks it about `anchor` (an entrance that
    /// scales); `under` is a full-frame background laid beneath the
    /// posed text, so a card's fill stays put while its words move.
    /// The frame's bytes are those `pose_sprite` makes.
    Placed {
        sprite: RestingSprite,
        dx: i32,
        dy: i32,
        opacity: f32,
        scale: f32,
        anchor: (f32, f32),
        under: Option<[u8; 4]>,
        /// A blur (sigma, pixels) over the placed sprite: the blur
        /// entrance coming into focus. 0 for none.
        blur: f32,
        rows: (u32, u32),
    },
    /// The resting render's glyphs each placed on its own: a motion
    /// that staggers by character, word or line. The frame's bytes are
    /// those `pose_sprite_glyphs` makes from the sprite's `units`.
    Glyphs { sprite: RestingSprite, poses: Vec<GlyphPose>, rows: (u32, u32) },
    /// Rendered for this time alone.
    Rendered { pixels: Vec<u8>, rows: (u32, u32) },
}

/// The title whose resting render is the sprite: a full-frame card's
/// words alone (its fill goes under them when the sprite is placed);
/// any other title as it is.
pub fn resting_title(title: &TitleData) -> std::borrow::Cow<'_, TitleData> {
    if matches!(title.background, TitleBackground::FullFrame { .. }) {
        let mut words = title.clone();
        words.background = TitleBackground::None;
        std::borrow::Cow::Owned(words)
    } else {
        std::borrow::Cow::Borrowed(title)
    }
}

/// The columns of `rows` of a frame-sized RGBA8 buffer that carry any
/// alpha, as `x0..x1` (empty when nothing does).
fn alpha_cols(buf: &[u8], width: u32, height: u32, rows: (u32, u32)) -> (u32, u32) {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || buf.len() < w * h * 4 || rows.1 <= rows.0 {
        return (0, 0);
    }
    let (mut lo, mut hi) = (w, 0usize);
    for y in rows.0 as usize..(rows.1 as usize).min(h) {
        let row = &buf[y * w * 4..(y + 1) * w * 4];
        if let Some(first) = row.chunks_exact(4).position(|p| p[3] != 0) {
            lo = lo.min(first);
            let last = row.chunks_exact(4).rposition(|p| p[3] != 0).unwrap_or(first);
            hi = hi.max(last + 1);
        }
    }
    if hi <= lo { (0, 0) } else { (lo as u32, hi as u32) }
}

/// The rows of a frame-sized RGBA8 buffer that carry any alpha, as
/// `y0..y1` (empty when nothing does). Scanned across threads.
fn alpha_rows(buf: &[u8], width: u32, height: u32) -> (u32, u32) {
    use rayon::prelude::*;
    let row = (width as usize) * 4;
    if row == 0 || buf.len() < row * height as usize {
        return (0, height);
    }
    let (lo, hi) = buf
        .par_chunks(row)
        .enumerate()
        .filter(|(_, r)| r.chunks_exact(4).any(|p| p[3] != 0))
        .map(|(y, _)| (y as u32, y as u32 + 1))
        .reduce(|| (u32::MAX, 0), |a, b| (a.0.min(b.0), a.1.max(b.1)));
    if lo == u32::MAX {
        (0, 0)
    } else {
        (lo, hi)
    }
}

/// Renders title clips to RGBA pixel buffers.
pub struct TitleRenderer {
    fallback_font: Font,
    font_cache: HashMap<String, Font>,
    /// Shared registry of available system fonts. Cheap to clone — internally
    /// an `Arc` populated by `font_discovery::discover_system_fonts()`, which
    /// dedupes across the whole process and caches the result on disk.
    font_registry: font_discovery::FontRegistry,
    /// Cached texture fill image: (path, rgba_data, width, height)
    texture_fill_cache: Option<(String, Vec<u8>, u32, u32)>,
    /// Small LRU of recently-rasterized titles. Capped at
    /// `RENDER_CACHE_CAP` so memory stays bounded even when many
    /// title clips are visited (e.g. scrubbing across a long credits
    /// sequence).
    render_cache: Vec<CachedRender>,
    /// The rows with coverage in the last render, `y0..y1`.
    last_rows: (u32, u32),
    /// The blur the last `Placed` frame asked for, so a caller that
    /// took bytes can apply it.
    last_blur: f32,
    /// The unit map the last `render_inner` made, for `cache_store`.
    last_units: Option<std::sync::Arc<UnitMap>>,
    /// The next `CachedRender::id`.
    next_cache_id: u64,
    /// The clip's length while rendering with `render_with_window`,
    /// which exit motion counts back from. `None` outside a clip.
    motion_window: Option<f32>,
    /// Per-glyph wave of the layer being rendered, from its `Wave`
    /// loop: (amplitude px, rate Hz). Set by `render_inner` per object.
    loop_wave: Option<(f32, f32)>,
    /// The waves of every layer of the title being rendered, by
    /// object id; filled by `render_with_time`.
    loop_waves: HashMap<u32, (f32, f32)>,
    /// Decoded image layers by path; `None` for a file that would
    /// not open, so it is not retried every frame.
    image_cache: HashMap<String, Option<std::sync::Arc<DecodedImage>>>,
}

const RENDER_CACHE_CAP: usize = 4;

impl Default for TitleRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl TitleRenderer {
    pub fn new() -> Self {
        let fallback_font = Font::from_bytes(
            EMBEDDED_FONT,
            fontdue::FontSettings::default(),
        )
        .expect("Failed to load embedded Noto Sans font");
        let registry = font_discovery::discover_system_fonts();
        tracing::info!("Font discovery: found {} font families", registry.len());
        Self {
            fallback_font,
            font_cache: HashMap::new(),
            font_registry: registry,
            texture_fill_cache: None,
            render_cache: Vec::with_capacity(RENDER_CACHE_CAP),
            last_rows: (0, 0),
            last_blur: 0.0,
            last_units: None,
            next_cache_id: 1,
            motion_window: None,
            loop_wave: None,
            loop_waves: HashMap::new(),
            image_cache: HashMap::new(),
        }
    }

    /// Get a list of available font family names (sorted alphabetically).
    pub fn available_families(&self) -> Vec<&str> {
        self.font_registry.keys().map(|s| s.as_str()).collect()
    }

    /// Get available styles for a given font family.
    pub fn available_styles(&self, family: &str) -> Vec<&str> {
        self.font_registry
            .get(family)
            .map(|infos| infos.iter().map(|i| i.style.as_str()).collect())
            .unwrap_or_default()
    }

    /// Build a cache key from family name and style.
    fn cache_key(family: &str, style: FontStyle) -> String {
        let style_str = match style {
            FontStyle::Regular => "Regular",
            FontStyle::Bold => "Bold",
            FontStyle::Italic => "Italic",
            FontStyle::BoldItalic => "Bold Italic",
        };
        format!("{family}::{style_str}")
    }

    /// Ensure a font is loaded into the cache. Returns the cache key.
    fn ensure_font_loaded(&mut self, family: &str, style: FontStyle) -> String {
        let key = Self::cache_key(family, style);

        if family == "sans-serif" || family.is_empty() || self.font_cache.contains_key(&key) {
            return key;
        }

        let style_str = match style {
            FontStyle::Regular => "Regular",
            FontStyle::Bold => "Bold",
            FontStyle::Italic => "Italic",
            FontStyle::BoldItalic => "Bold Italic",
        };

        // Fonts that ship with the app come first: the same face on
        // every machine, no disk read.
        if let Some(bytes) = bundled::lookup(family, style_str) {
            if let Ok(font) = Font::from_bytes(bytes, fontdue::FontSettings::default()) {
                self.font_cache.insert(key.clone(), font);
            }
            return key;
        }

        if let Some(infos) = self.font_registry.get(family) {

            // Try exact style match first, then fall back to Regular, then first available
            let info = infos
                .iter()
                .find(|i| i.style == style_str)
                .or_else(|| infos.iter().find(|i| i.style == "Regular"))
                .or_else(|| infos.first());

            if let Some(info) = info {
                if let Ok(data) = std::fs::read(&info.path) {
                    if let Ok(font) =
                        Font::from_bytes(data.as_slice(), fontdue::FontSettings::default())
                    {
                        self.font_cache.insert(key.clone(), font);
                    }
                }
            }
        }

        key
    }

    /// Phase 1: Font loading, glyph rasterization into alpha mask, returns layout info.
    /// Static (time = 0 / no animation) layout — thin wrapper over
    /// `render_layout_at_time` so existing callers (tests, cursor
    /// hit-testing) keep their signature.
    pub fn render_layout(&mut self, title: &TitleData, width: u32, height: u32) -> TitleLayout {
        self.render_layout_at_time(title, width, height, None)
    }

    /// Time-aware variant of `render_layout`. When `time_secs` is
    /// `Some` AND the primary `TextBox` has `char_animation` set, the
    /// alpha mask is built per-glyph with the entrance pose applied
    /// (FadeIn alpha, Slide{Up,Down,Left,Right} offsets, Scale{Up,Down}
    /// re-rasterized at the scaled size, plus the wave bobble). Every
    /// other field of `TitleLayout` is computed from the static glyph
    /// positions so cursor hit-testing keeps tracking the resting
    /// glyph layout — only the visible bitmap shifts during entrance.
    pub fn render_layout_at_time(
        &mut self,
        title: &TitleData,
        width: u32,
        height: u32,
        time_secs: Option<f32>,
    ) -> TitleLayout {
        // Object-model bridge: this function still operates on the
        // legacy single-text shape; commit-1 keeps the renderer's
        // internal flow intact by extracting the primary TextBox
        // up-front. A follow-up commit teaches the renderer to walk
        // every object in `title.objects` for proper multi-layer
        // titles.
        let tb = title.primary_text_or_default();
        let w = width as usize;
        let h = height as usize;

        // Resolve the font: load into cache if needed, then get reference
        let cache_key = self.ensure_font_loaded(&tb.font_name, tb.font_style);
        let font = self
            .font_cache
            .get(&cache_key)
            .unwrap_or(&self.fallback_font);

        // Scale font size relative to frame height (72pt at 1080p).
        // SmallCaps renders at 80 % of the configured size so the
        // upper-cased glyphs read like smallcaps even on fonts
        // without dedicated smallcaps glyph variants.
        let scale = height as f32 / 1080.0;
        let case_scale = match tb.case_transform {
            klipp_timeline::TextCase::SmallCaps => 0.80,
            _ => 1.0,
        };
        let font_px = tb.font_size * scale * case_scale;

        // Lay out text
        let h_align = match tb.alignment {
            TextAlignment::Left => HorizontalAlign::Left,
            TextAlignment::Center => HorizontalAlign::Center,
            TextAlignment::Right => HorizontalAlign::Right,
        };

        let mut layout = Layout::new(CoordinateSystem::PositiveYDown);
        layout.reset(&LayoutSettings {
            x: 0.0,
            y: 0.0,
            max_width: Some(text_box_extent(title, width, height).0),
            max_height: Some(text_box_extent(title, width, height).1),
            horizontal_align: h_align,
            vertical_align: VerticalAlign::Top,
            wrap_style: WrapStyle::Word,
            ..LayoutSettings::default()
        });

        // Resolve the laid-out text, honouring the pre-layout case
        // transform. The `text` field on TextBox is preserved verbatim
        // so the user can flip the treatment without re-typing.
        let mut resolved_text: String = match tb.case_transform {
            klipp_timeline::TextCase::None => tb.text.clone(),
            klipp_timeline::TextCase::Uppercase
            | klipp_timeline::TextCase::SmallCaps => tb.text.to_uppercase(),
            klipp_timeline::TextCase::Lowercase => tb.text.to_lowercase(),
        };
        // RTL: reverse the visual character order. This works
        // correctly for Hebrew + ASCII; Arabic shaping (joining
        // forms, lam-alif ligatures) needs harfbuzz and is tracked
        // for a follow-up.
        if tb.text_direction == TextDirection::Rtl {
            resolved_text = resolved_text.chars().rev().collect();
        }

        // Drop cap: when `drop_cap_scale >= 1.0` we enlarge the first
        // character. The straightforward "drop initial" path appends
        // the cap as a head-segment of the main layout — body text
        // continues on the same baseline. The "wrap-under" path
        // (alignment = Left + no style runs) builds the layout from
        // three sub-layouts so the body wraps to the right of the
        // cap for the cap's vertical extent and re-flows at full
        // width below it.
        let drop_cap_active = tb.drop_cap_scale >= 1.0 && !resolved_text.is_empty();
        let drop_cap_byte_end = if drop_cap_active {
            resolved_text.chars().next().unwrap().len_utf8()
        } else { 0 };
        let body_start = drop_cap_byte_end;
        let drop_cap_wrap_eligible = drop_cap_active
            && tb.alignment == TextAlignment::Left
            && tb.style_runs.is_empty();

        // Append text with per-run font sizes when style runs exist.
        // Track (glyph_count_before, segment_byte_start) for byte_offset fixup.
        let mut segment_info: Vec<(usize, usize)> = Vec::new();

        // Pre-built glyph list for the drop-cap wrap path. Empty when
        // we use fontdue's normal Layout flow.
        let mut drop_cap_wrap_glyphs: Vec<fontdue::layout::GlyphPosition> = Vec::new();

        // Vertical layout (CJK-style top-to-bottom stacking). Each
        // character becomes its own line so glyphs stack vertically
        // with their normal horizontal orientation. Drop caps and
        // wrap-under disable this path (single-line cap doesn't
        // make sense as vertical column header).
        let vertical_active = tb.vertical_layout && !drop_cap_active;
        let mut vertical_glyphs: Vec<fontdue::layout::GlyphPosition> = Vec::new();
        if vertical_active {
            let line_height = font_px * 1.2;
            let mut byte_offset = 0usize;
            let column_x = width as f32 * 0.45;  // single-column centred
            for (char_idx, ch) in resolved_text.chars().enumerate() {
                let ch_str = ch.to_string();
                let mut sub_layout = Layout::new(CoordinateSystem::PositiveYDown);
                sub_layout.reset(&LayoutSettings {
                    x: column_x,
                    y: char_idx as f32 * line_height,
                    max_width: Some(font_px * 2.5),
                    horizontal_align: HorizontalAlign::Left,
                    vertical_align: VerticalAlign::Top,
                    wrap_style: WrapStyle::Word,
                    ..LayoutSettings::default()
                });
                sub_layout.append(&[font], &TextStyle::new(&ch_str, font_px, 0));
                for g in sub_layout.glyphs() {
                    let mut g_owned = *g;
                    g_owned.byte_offset += byte_offset;
                    vertical_glyphs.push(g_owned);
                }
                byte_offset += ch.len_utf8();
            }
            segment_info.push((0, 0));
        }

        // Multi-column body layout. When `columns >= 2` we split the
        // body text by character count on word boundaries and lay
        // each column out separately, then translate columns into
        // place horizontally. Style runs and drop caps disable this
        // path (single-column fall-through preserves their
        // semantics).
        let columns_n = tb.columns.max(1) as usize;
        let columns_active = columns_n >= 2 && tb.style_runs.is_empty()
            && !drop_cap_active && !vertical_active;
        let mut columns_glyphs: Vec<fontdue::layout::GlyphPosition> = Vec::new();
        if columns_active {
            let column_gap = font_px * 1.5;
            let total_w = text_box_extent(title, width, height).0;
            let col_w = ((total_w - column_gap * (columns_n - 1) as f32)
                / columns_n as f32).max(1.0);
            // Split resolved_text into N chunks on word boundaries.
            // We aim for equal character counts; the splitter walks
            // forward to the next whitespace after each chunk's
            // ideal endpoint so words aren't cut.
            let total_chars = resolved_text.chars().count();
            let chunk_chars = total_chars.div_ceil(columns_n);
            let mut chunks: Vec<String> = Vec::new();
            let mut buffer = String::new();
            let mut chunk_count = 0usize;
            for ch in resolved_text.chars() {
                buffer.push(ch);
                chunk_count += 1;
                if chunk_count >= chunk_chars && ch.is_whitespace()
                    && chunks.len() + 1 < columns_n
                {
                    chunks.push(std::mem::take(&mut buffer));
                    chunk_count = 0;
                }
            }
            if !buffer.is_empty() {
                chunks.push(buffer);
            }
            // Pad with empty strings if fewer chunks than columns.
            while chunks.len() < columns_n {
                chunks.push(String::new());
            }
            let mut byte_offset = 0usize;
            for (col_idx, chunk) in chunks.iter().enumerate() {
                let column_x = col_idx as f32 * (col_w + column_gap);
                let mut col_layout = Layout::new(CoordinateSystem::PositiveYDown);
                col_layout.reset(&LayoutSettings {
                    x: column_x,
                    y: 0.0,
                    max_width: Some(col_w),
                    horizontal_align: h_align,
                    vertical_align: VerticalAlign::Top,
                    wrap_style: WrapStyle::Word,
                    ..LayoutSettings::default()
                });
                col_layout.append(&[font], &TextStyle::new(chunk, font_px, 0));
                for g in col_layout.glyphs() {
                    let mut g_owned = *g;
                    g_owned.byte_offset += byte_offset;
                    columns_glyphs.push(g_owned);
                }
                byte_offset += chunk.len();
            }
            segment_info.push((0, 0));
        }

        if drop_cap_wrap_eligible {
            // Build the layout in three passes so body text wraps
            // properly around the drop cap. See `layout_drop_cap_wrap`.
            let cap_size = font_px * tb.drop_cap_scale;
            let body_text = &resolved_text[body_start..];
            drop_cap_wrap_glyphs = layout_drop_cap_wrap(
                font,
                &resolved_text[..body_start],
                body_text,
                cap_size,
                font_px,
                text_box_extent(title, width, height).0,
            );
            segment_info.push((0, 0));
            // We synthesise byte-offsets later from drop_cap_wrap_glyphs.
        } else if drop_cap_active {
            // "Drop initial" path: cap as the head segment.
            let cap_text = &resolved_text[..drop_cap_byte_end];
            let cap_size = font_px * tb.drop_cap_scale;
            segment_info.push((0, 0));
            layout.append(&[font], &TextStyle::new(cap_text, cap_size, 0));
        }

        if tb.style_runs.is_empty() {
            if drop_cap_wrap_eligible {
                // Glyphs already built; nothing further to append.
            } else if !drop_cap_active {
                segment_info.push((0, 0));
                layout.append(&[font], &TextStyle::new(&resolved_text, font_px, 0));
            } else if body_start < resolved_text.len() {
                segment_info.push((layout.glyphs().len(), body_start));
                layout.append(&[font], &TextStyle::new(&resolved_text[body_start..], font_px, 0));
            }
        } else {
            // Style runs cover the body text from `body_start`
            // onwards (the drop cap, if active, was laid out above).
            // Run boundaries are clamped against `body_start` so a
            // run that overlaps the drop cap collapses to its
            // post-cap remainder.
            let mut pos = body_start;
            for run in &tb.style_runs {
                let run_start = run.start.max(body_start);
                let run_end = run.end.max(body_start).min(resolved_text.len());
                if run_start > pos && run_start <= resolved_text.len() {
                    let segment = &resolved_text[pos..run_start];
                    if !segment.is_empty() {
                        segment_info.push((layout.glyphs().len(), pos));
                        layout.append(&[font], &TextStyle::new(segment, font_px, 0));
                    }
                    pos = run_start;
                }
                if run_end > pos {
                    let segment = &resolved_text[pos..run_end];
                    let run_size = run.font_size.unwrap_or(tb.font_size) * scale * case_scale;
                    if !segment.is_empty() {
                        segment_info.push((layout.glyphs().len(), pos));
                        layout.append(&[font], &TextStyle::new(segment, run_size, 0));
                    }
                    pos = run_end;
                }
            }
            if pos < resolved_text.len() {
                let segment = &resolved_text[pos..];
                if !segment.is_empty() {
                    segment_info.push((layout.glyphs().len(), pos));
                    layout.append(&[font], &TextStyle::new(segment, font_px, 0));
                }
            }
        }

        // Pick the glyph source for this render — drop-cap wrap,
        // vertical layout, multi-column, or fontdue's accumulated
        // single-column layout. Active paths are mutually exclusive
        // (set up by the gating booleans above).
        let mut glyphs_owned: Vec<fontdue::layout::GlyphPosition> = if drop_cap_wrap_eligible {
            std::mem::take(&mut drop_cap_wrap_glyphs)
        } else if vertical_active {
            std::mem::take(&mut vertical_glyphs)
        } else if columns_active {
            std::mem::take(&mut columns_glyphs)
        } else {
            layout.glyphs().to_vec()
        };
        if glyphs_owned.is_empty() {
            return TitleLayout {
                alpha_mask: Vec::new(),
                mask_w: 0, mask_h: 0,
                mask_x0: 0, mask_y0: 0,
                text_top: 0.0, text_bottom: 0.0,
                text_left: 0.0, text_right: 0.0,
                glyph_count: 0,
                scale,
                glyph_colors: Vec::new(),
                glyph_coverages: Vec::new(),
                glyph_index: Vec::new(),
                glyph_boxes: Vec::new(),
                glyph_parents: Vec::new(),
                pad: 0,
            };
        }

        // ── Post-process glyph positions for tracking and line spacing ──

        // Fix up byte_offset: fontdue's byte_offset is per-segment, but style runs
        // use absolute byte positions. Add each segment's byte start.
        for (seg_idx, &(glyph_start, byte_start)) in segment_info.iter().enumerate() {
            let glyph_end = if seg_idx + 1 < segment_info.len() {
                segment_info[seg_idx + 1].0
            } else {
                glyphs_owned.len()
            };
            for g in &mut glyphs_owned[glyph_start..glyph_end] {
                g.byte_offset += byte_start;
            }
        }

        // Apply line spacing multiplier (shift lines after the first)
        if tb.line_spacing != 1.0 {
            let line_height_scale = height as f32 / 1080.0;
            let mut line_starts: Vec<(usize, f32)> = Vec::new();
            let mut last_y: Option<f32> = None;
            for (i, g) in glyphs_owned.iter().enumerate() {
                let gy = g.y;
                if last_y.is_none() || (gy - last_y.unwrap()).abs() > 1.0 {
                    line_starts.push((i, gy));
                    last_y = Some(gy);
                }
            }
            if line_starts.len() > 1 {
                let base_line_height = if line_starts.len() >= 2 {
                    line_starts[1].1 - line_starts[0].1
                } else {
                    font_px
                };
                let _ = line_height_scale;
                for line_idx in 1..line_starts.len() {
                    let y_shift = base_line_height * (tb.line_spacing - 1.0) * line_idx as f32;
                    let start = line_starts[line_idx].0;
                    let end = if line_idx + 1 < line_starts.len() {
                        line_starts[line_idx + 1].0
                    } else {
                        glyphs_owned.len()
                    };
                    for g in &mut glyphs_owned[start..end] {
                        g.y += y_shift;
                    }
                }
            }
        }

        // Apply tracking (extra pixel spacing per character)
        if tb.tracking != 0.0 {
            let tracking_px = tb.tracking * scale;
            for (i, g) in glyphs_owned.iter_mut().enumerate() {
                g.x += tracking_px * i as f32;
            }
        }

        let glyphs = &glyphs_owned;

        // ── Text on path: rasterize per-glyph with rotation ──
        if let Some(text_path) = &tb.text_path {
            return self.render_layout_on_path(
                font, glyphs, title, text_path, width, height, w, h, scale,
            );
        }

        // Compute text bounding box
        let mut text_min_x = f32::MAX;
        let mut text_min_y = f32::MAX;
        let mut text_max_x = f32::MIN;
        let mut text_max_y = f32::MIN;
        for g in glyphs {
            text_min_x = text_min_x.min(g.x);
            text_min_y = text_min_y.min(g.y);
            text_max_x = text_max_x.max(g.x + g.width as f32);
            text_max_y = text_max_y.max(g.y + g.height as f32);
        }
        let text_w = text_max_x - text_min_x;
        let text_h = text_max_y - text_min_y;

        // Position offset. `transform.center` is the textbox's anchor
        // on the frame; alignment determines which edge of the
        // rendered text rests at that anchor — Left = text's left
        // edge at anchor, Center = text's centre at anchor, Right =
        // text's right edge at anchor. This is the standard NLE model
        // (matches DaVinci / FCP / Premiere) and is the *only* way
        // dragging a Left/Right-aligned textbox actually moves the
        // text — the previous implementation pinned Left/Right to a
        // fixed frame-edge margin, silently dropping the user's drag.
        let anchor_x = title.position().x * width as f32;
        let offset_x = match tb.alignment {
            TextAlignment::Left => anchor_x - text_min_x,
            TextAlignment::Center => anchor_x - text_w * 0.5 - text_min_x,
            TextAlignment::Right => anchor_x - text_w - text_min_x,
        };
        // Baseline shift is authored in 1080p reference pixels and
        // scales with the frame so a +20 px offset on a 1080p edit
        // looks the same on a 4K master. Positive shifts text DOWN.
        let baseline_shift_px = tb.baseline_shift * scale;
        let offset_y = title.position().y * height as f32 - text_h * 0.5 - text_min_y
            + baseline_shift_px;

        // Compute the mask bounding box (padded for effects). When
        // an underline is enabled the stripe sits ~5 % below the
        // visible text bottom — the mask must extend that far down
        // or the underline gets clipped on the descender side.
        let max_pad = effect_padding(title, scale);
        let underline_pad = if tb.underline {
            (text_h * 0.10).ceil() as i32 + (font_px * 0.06).ceil() as i32
        } else {
            0
        };
        let mask_x0 = ((text_min_x + offset_x).floor() as i32 - max_pad).max(0) as usize;
        let mask_y0 = ((text_min_y + offset_y).floor() as i32 - max_pad).max(0) as usize;
        let mask_x1 = ((text_max_x + offset_x).ceil() as i32 + max_pad + 1).clamp(0, w as i32) as usize;
        let mask_y1 = ((text_max_y + offset_y).ceil() as i32 + max_pad + underline_pad + 1)
            .clamp(0, h as i32) as usize;
        let mask_w = mask_x1.saturating_sub(mask_x0);
        let mask_h = mask_y1.saturating_sub(mask_y0);

        if mask_w == 0 || mask_h == 0 {
            return TitleLayout {
                alpha_mask: Vec::new(),
                mask_w: 0, mask_h: 0,
                mask_x0: 0, mask_y0: 0,
                text_top: text_min_y + offset_y,
                text_bottom: text_max_y + offset_y,
                text_left: text_min_x + offset_x,
                text_right: text_max_x + offset_x,
                glyph_count: glyphs.len(),
                scale,
                glyph_colors: Vec::new(),
                glyph_coverages: Vec::new(),
                glyph_index: Vec::new(),
                glyph_boxes: Vec::new(),
                glyph_parents: Vec::new(),
                pad: max_pad,
            };
        }
        // A staggered motion poses each glyph on its own from the
        // resting render, so it needs to know which glyph a pixel is.
        let staggers = |m: Option<klipp_timeline::Motion>| m.is_some_and(|m| m.stagger != klipp_timeline::StaggerUnit::None);
        let tag_glyphs = time_secs.is_none() && (staggers(tb.motion.enter) || staggers(tb.motion.exit));
        let mut glyph_index = if tag_glyphs { vec![GLYPH_NONE; mask_w * mask_h] } else { Vec::new() };

        // Rasterize glyphs into alpha mask. When the textbox has a
        // `char_animation` AND the renderer was called with a clip
        // time, dispatch to the per-glyph animated rasterizer; the
        // rest of the pipeline (compose, outline, shadow, glow,
        // texture fill) consumes the resulting mask unmodified.
        let mut alpha_mask = vec![0u8; mask_w * mask_h];
        match time_secs {
            Some(t) if tb.motion.is_active() || self.loop_wave.is_some() => {
                rasterize_to_mask_motion(
                    font, glyphs,
                    offset_x, offset_y,
                    &mut alpha_mask, mask_w, mask_h, mask_x0, mask_y0,
                    &tb.motion, t, font_px, self.motion_window, self.loop_wave,
                );
            }
            _ => {
                rasterize_to_mask(
                    font, glyphs,
                    offset_x, offset_y,
                    &mut alpha_mask, mask_w, mask_h, mask_x0, mask_y0,
                    if tag_glyphs { Some(&mut glyph_index) } else { None },
                );
            }
        }

        // Underline / strikethrough: paint a horizontal stripe across
        // the text bounds inside the alpha mask. Thickness scales
        // with font_px so it stays proportional at any resolution;
        // capped at a 1-pixel minimum so it doesn't disappear at
        // small sizes. Strikethrough sits at ~40 % from the bottom
        // (covering the x-height midpoint for most fonts);
        // underline sits 10 % below the visible bottom (descender
        // territory) so it doesn't crash into descenders.
        if tb.underline || tb.strikethrough {
            let stripe_thickness = (font_px * 0.06).max(1.0).round() as i32;
            let line_x0 = ((text_min_x + offset_x).floor() as i32 - mask_x0 as i32).max(0);
            let line_x1 = ((text_max_x + offset_x).ceil() as i32 - mask_x0 as i32)
                .min(mask_w as i32);
            // Strikethrough: 40 % from text_min_y → text_max_y
            // (visually covers the x-height midline).
            if tb.strikethrough {
                let line_y_centre = (text_min_y + offset_y) + text_h * 0.40;
                let stripe_top = (line_y_centre.round() as i32 - mask_y0 as i32
                    - stripe_thickness / 2)
                    .max(0);
                paint_alpha_mask_stripe(
                    &mut alpha_mask, mask_w, mask_h,
                    line_x0, line_x1, stripe_top, stripe_top + stripe_thickness,
                );
            }
            if tb.underline {
                let line_y_centre = (text_min_y + offset_y) + text_h * 1.05;
                let stripe_top = (line_y_centre.round() as i32 - mask_y0 as i32
                    - stripe_thickness / 2)
                    .max(0);
                paint_alpha_mask_stripe(
                    &mut alpha_mask, mask_w, mask_h,
                    line_x0, line_x1, stripe_top, stripe_top + stripe_thickness,
                );
            }
        }

        // ── Rich text: resolve per-glyph styles ──
        let has_rich_text = !tb.style_runs.is_empty();
        let resolved_colors: Vec<[u8; 4]> = if has_rich_text {
            resolve_glyph_colors(title, glyphs, &tb.style_runs)
        } else {
            Vec::new()
        };

        // Apply write-on reveal
        if tb.write_on < 1.0 {
            let cutoff_x = ((text_min_x + offset_x) + text_w * tb.write_on.clamp(0.0, 1.0)) as i32;
            for my in 0..mask_h {
                for mx in 0..mask_w {
                    let global_x = (mask_x0 + mx) as i32;
                    if global_x >= cutoff_x {
                        alpha_mask[my * mask_w + mx] = 0;
                    }
                }
            }
        }

        // Build per-glyph color info for rich text fill
        let (glyph_colors, glyph_coverages) = if has_rich_text {
            build_glyph_color_info(font, glyphs, offset_x, offset_y, mask_x0, mask_y0, &resolved_colors)
        } else {
            (Vec::new(), Vec::new())
        };

        let glyph_boxes = glyphs
            .iter()
            .map(|g| {
                let (gx, gy) = ((g.x + offset_x) as i32, (g.y + offset_y) as i32);
                [gx, gy, gx + g.width as i32, gy + g.height as i32]
            })
            .collect();
        TitleLayout {
            alpha_mask,
            mask_w: mask_w as u32,
            mask_h: mask_h as u32,
            mask_x0: mask_x0 as u32,
            mask_y0: mask_y0 as u32,
            text_top: text_min_y + offset_y,
            text_bottom: text_max_y + offset_y,
            text_left: text_min_x + offset_x,
            text_right: text_max_x + offset_x,
            glyph_count: glyphs.len(),
            scale,
            glyph_colors,
            glyph_coverages,
            glyph_index,
            glyph_boxes,
            glyph_parents: glyphs.iter().map(|g| g.parent).collect(),
            pad: max_pad,
        }
    }

    /// Render glyphs placed along a geometric path (arc, circle, wave).
    /// Each glyph is rasterized individually and rotated to follow the path tangent.
    #[allow(clippy::too_many_arguments)]
    fn render_layout_on_path(
        &self,
        font: &Font,
        glyphs: &[fontdue::layout::GlyphPosition],
        title: &TitleData,
        text_path: &TextPath,
        width: u32,
        height: u32,
        w: usize,
        h: usize,
        scale: f32,
    ) -> TitleLayout {
        let tb = title.primary_text_or_default();
        // Compute cumulative advance widths to map each glyph to an arc-length parameter
        let mut advances: Vec<f32> = Vec::with_capacity(glyphs.len());
        let mut cumulative = 0.0f32;
        for g in glyphs {
            advances.push(cumulative);
            // Use glyph width + x_advance as the advance for this character
            cumulative += g.width as f32;
            // Add small gap between characters
            if g.width > 0 {
                cumulative += 2.0;
            }
        }
        let total_advance = cumulative.max(1.0);

        // Center of the path is at the title position
        let center_x = title.position().x * width as f32;
        let center_y = title.position().y * height as f32;

        // For each glyph, compute its position and rotation on the path
        // Then rasterize into a small buffer and blit rotated onto the full-frame mask
        let max_pad = effect_padding(title, scale);
        // Use the full frame as the mask (path text can go anywhere)
        let mask_x0 = 0usize;
        let _mask_y0 = 0usize;
        let mask_w = w;
        let mask_h = h;
        let mut alpha_mask = vec![0u8; mask_w * mask_h];

        let mut text_min_x = f32::MAX;
        let mut text_min_y = f32::MAX;
        let mut text_max_x = f32::MIN;
        let mut text_max_y = f32::MIN;

        for (i, glyph) in glyphs.iter().enumerate() {
            if glyph.width == 0 || glyph.height == 0 {
                continue;
            }

            // Normalized position along path (0..1)
            let t = (advances[i] + glyph.width as f32 * 0.5) / total_advance;
            let (px, py, angle) = path_point(text_path, t, total_advance, center_x, center_y);

            // Rasterize this glyph into a small buffer
            let (_, coverage) = font.rasterize_config(glyph.key);
            let gw = glyph.width;
            let gh = glyph.height;

            // Blit rotated glyph onto the main mask
            blit_rotated(
                &mut alpha_mask, mask_w, mask_h,
                &coverage, gw, gh,
                px, py, angle,
            );

            // Update bounding box
            let radius = ((gw as f32 / 2.0).powi(2) + (gh as f32 / 2.0).powi(2)).sqrt();
            text_min_x = text_min_x.min(px - radius);
            text_min_y = text_min_y.min(py - radius);
            text_max_x = text_max_x.max(px + radius);
            text_max_y = text_max_y.max(py + radius);
        }

        // Clamp bounding box
        if text_min_x == f32::MAX {
            text_min_x = center_x;
            text_min_y = center_y;
            text_max_x = center_x;
            text_max_y = center_y;
        }

        // Apply write-on reveal
        if tb.write_on < 1.0 {
            let cutoff_x = (text_min_x + (text_max_x - text_min_x) * tb.write_on.clamp(0.0, 1.0)) as i32;
            for my in 0..mask_h {
                for mx in 0..mask_w {
                    let global_x = (mask_x0 + mx) as i32;
                    if global_x >= cutoff_x {
                        alpha_mask[my * mask_w + mx] = 0;
                    }
                }
            }
        }

        // Crop the mask to a tight bounding box with padding
        let crop_x0 = ((text_min_x.floor() as i32) - max_pad).max(0) as usize;
        let crop_y0 = ((text_min_y.floor() as i32) - max_pad).max(0) as usize;
        let crop_x1 = ((text_max_x.ceil() as i32) + max_pad + 1).min(w as i32) as usize;
        let crop_y1 = ((text_max_y.ceil() as i32) + max_pad + 1).min(h as i32) as usize;
        let crop_w = crop_x1.saturating_sub(crop_x0);
        let crop_h = crop_y1.saturating_sub(crop_y0);

        if crop_w == 0 || crop_h == 0 {
            return TitleLayout {
                alpha_mask: Vec::new(),
                mask_w: 0, mask_h: 0,
                mask_x0: 0, mask_y0: 0,
                text_top: text_min_y, text_bottom: text_max_y,
                text_left: text_min_x, text_right: text_max_x,
                glyph_count: glyphs.len(),
                scale,
                glyph_colors: Vec::new(),
                glyph_coverages: Vec::new(),
                glyph_index: Vec::new(),
                glyph_boxes: Vec::new(),
                glyph_parents: Vec::new(),
                pad: 0,
            };
        }

        let mut cropped = vec![0u8; crop_w * crop_h];
        for cy in 0..crop_h {
            let src_y = crop_y0 + cy;
            if src_y >= mask_h { continue; }
            for cx in 0..crop_w {
                let src_x = crop_x0 + cx;
                if src_x >= mask_w { continue; }
                cropped[cy * crop_w + cx] = alpha_mask[src_y * mask_w + src_x];
            }
        }

        TitleLayout {
            alpha_mask: cropped,
            mask_w: crop_w as u32,
            mask_h: crop_h as u32,
            mask_x0: crop_x0 as u32,
            mask_y0: crop_y0 as u32,
            text_top: text_min_y,
            text_bottom: text_max_y,
            text_left: text_min_x,
            text_right: text_max_x,
            glyph_count: glyphs.len(),
            scale,
            glyph_colors: Vec::new(),
            glyph_coverages: Vec::new(),
            glyph_index: Vec::new(),
            glyph_boxes: Vec::new(),
            glyph_parents: Vec::new(),
            pad: 0,
        }
    }

    /// Phase 2: Compose layout + effects into final RGBA buffer.
    /// If `gpu` is Some and the mask is large enough, GPU-accelerated blur/dilate will be used.
    pub fn compose(
        layout: &TitleLayout,
        title: &TitleData,
        width: u32,
        height: u32,
        gpu: Option<&mut dyn AlphaMaskProcessor>,
        time_secs: Option<f32>,
    ) -> Vec<u8> {
        let tb = title.primary_text_or_default();
        let w = width as usize;
        let h = height as usize;
        // The working buffer is float, on the same 0..255 scale the
        // layers already compute in. A title is a stack — a backing
        // panel, a shadow, a glow, an outline, the glyphs — and each
        // one used to be rounded into eight bits before the next one
        // read it back, so five layers meant five roundings, each
        // throwing away what the next needed. It is rounded once now,
        // where the buffer leaves this function.
        let mut buf = vec![0f32; w * h * 4];

        // Fill background if requested
        if let TitleBackground::FullFrame { color } = &title.background {
            let fill = [color[0] as f32, color[1] as f32, color[2] as f32, color[3] as f32];
            for pixel in buf.chunks_exact_mut(4) {
                pixel.copy_from_slice(&fill);
            }
        }

        if layout.glyph_count == 0 || layout.mask_w == 0 || layout.mask_h == 0 {
            return to_bytes(&buf);
        }

        let scale = layout.scale;
        let mask_w = layout.mask_w as usize;
        let mask_h = layout.mask_h as usize;
        let mask_x0 = layout.mask_x0 as usize;
        let mask_y0 = layout.mask_y0 as usize;
        let alpha_mask = &layout.alpha_mask;

        // Draw background box if Color variant
        if let TitleBackground::Color { color, padding } = &title.background {
            let pad = padding * scale;
            let bx0 = ((layout.text_left - pad) as i32).max(0) as usize;
            let by0 = ((layout.text_top - pad) as i32).max(0) as usize;
            let bx1 = ((layout.text_right + pad) as i32).min(width as i32) as usize;
            let by1 = ((layout.text_bottom + pad) as i32).min(height as i32) as usize;
            for row in by0..by1 {
                for col in bx0..bx1 {
                    let idx = (row * w + col) * 4;
                    blend_pixel(&mut buf, idx, color);
                }
            }
        }

        // Determine if GPU acceleration should be used (> 250K pixels in mask)
        let use_gpu = gpu.is_some() && (mask_w * mask_h > 250_000);
        // We need to use a mutable ref conditionally — wrap in Option
        let mut gpu = gpu;

        // Helper closures that dispatch to GPU or CPU
        let do_blur = |mask: &[u8], mw: usize, mh: usize, radius: f32, gpu: &mut Option<&mut dyn AlphaMaskProcessor>| -> Vec<u8> {
            if use_gpu {
                if let Some(ref mut g) = gpu {
                    return g.blur(mask, mw as u32, mh as u32, radius);
                }
            }
            blur_alpha(mask, mw, mh, radius)
        };

        let do_dilate = |mask: &[u8], mw: usize, mh: usize, radius: f32, gpu: &mut Option<&mut dyn AlphaMaskProcessor>| -> Vec<u8> {
            if use_gpu {
                if let Some(ref mut g) = gpu {
                    return g.dilate(mask, mw as u32, mh as u32, radius);
                }
            }
            dilate_alpha(mask, mw, mh, radius)
        };

        // ── Render effects back-to-front ──

        // 1. Outer glow stack — render largest-radius first so the
        // tighter glows read on top. Secondary layers (index >= 1) are
        // gated by `glow_stack_enabled`. radius/intensity may animate
        // via per-element keyframes.
        {
            let t = time_secs.map(|t| t as f64).unwrap_or(0.0);
            let mut active_glows: Vec<(f32, f32, &klipp_timeline::TextGlow)> = tb
                .glows
                .iter()
                .enumerate()
                .filter(|(_, g)| g.enabled)
                .filter(|(i, _)| *i == 0 || tb.glow_stack_enabled)
                .map(|(_, g)| (g.evaluated_radius(t), g.evaluated_intensity(t), g))
                .collect();
            // Largest radius first (rendered behind smaller ones).
            active_glows.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            for (radius, intensity, glow) in &active_glows {
                let blurred = do_blur(alpha_mask, mask_w, mask_h, *radius * scale, &mut gpu);
                draw_mask_to_buf(&mut buf, &blurred, mask_w, mask_h, mask_x0, mask_y0, w, h, &glow.color, *intensity);
            }
        }

        // 2. Drop shadow stack — render back-to-front (highest index
        // first) so the lowest index reads as nearest the text.
        // shadows[0] is the primary; shadows[1..] are gated by
        // `shadow_stack_enabled` so the user can hide the secondary
        // layers without dropping data. Per-element keyframes evaluate
        // through the same evaluator the legacy single-shadow used.
        {
            let t = time_secs.map(|t| t as f64).unwrap_or(0.0);
            for (i, shadow) in tb.shadows.iter().enumerate().rev() {
                if !shadow.enabled {
                    continue;
                }
                if i >= 1 && !tb.shadow_stack_enabled {
                    continue;
                }
                let off_x = shadow.evaluated_offset_x(t);
                let off_y = shadow.evaluated_offset_y(t);
                let blur = shadow.evaluated_blur(t);
                let sox = (off_x * scale).round() as i32;
                let soy = (off_y * scale).round() as i32;

                if blur > 0.0 {
                    let offset_mask = offset_alpha(alpha_mask, mask_w, mask_h, sox, soy);
                    let blurred = do_blur(&offset_mask, mask_w, mask_h, blur * scale, &mut gpu);
                    draw_mask_to_buf(&mut buf, &blurred, mask_w, mask_h, mask_x0, mask_y0, w, h, &shadow.color, 1.0);
                } else {
                    let offset_mask = offset_alpha(alpha_mask, mask_w, mask_h, sox, soy);
                    draw_mask_to_buf(&mut buf, &offset_mask, mask_w, mask_h, mask_x0, mask_y0, w, h, &shadow.color, 1.0);
                }
            }
        }

        // 3. Outline/stroke — multiple layers (widest first) then legacy single.
        // width may animate via per-stack-element keyframes, so we
        // collect evaluated widths up front and sort on those.
        {
            let t_secs = time_secs.map(|t| t as f64).unwrap_or(0.0);
            let mut all_outlines: Vec<(f32, &klipp_timeline::TextOutline)> = Vec::new();
            // Index 0 is the primary outline (always drawn when its
            // own `enabled` is true). Index >= 1 is the secondary
            // stack, gated by `outline_stack_enabled` so the user
            // can hide the whole stack without dropping the data.
            for (i, ol) in tb.outlines.iter().enumerate().filter(|(_, o)| o.enabled) {
                if i >= 1 && !tb.outline_stack_enabled {
                    continue;
                }
                all_outlines.push((ol.evaluated_width(t_secs), ol));
            }
            // Legacy single-outline support is gone — outline data
            // lives entirely in `tb.outlines` now.
            all_outlines.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

            // Stroke draw-on: clip the rendered outline horizontally
            // to a left-anchored fraction of the text bounds. When
            // `outline_write_on >= 1.0` the cutoff exceeds the text
            // right edge and every pixel passes — branch-free fast
            // path. Below 1.0, pixels with global x past the cutoff
            // are zeroed before draw_mask_to_buf.
            let outline_progress = tb.outline_write_on.clamp(0.0, 1.0);
            let outline_cutoff_x = if outline_progress >= 1.0 {
                i32::MAX
            } else {
                (layout.text_left + (layout.text_right - layout.text_left) * outline_progress).floor() as i32
            };

            for (eff_width, outline) in &all_outlines {
                let dilated = do_dilate(alpha_mask, mask_w, mask_h, *eff_width * scale, &mut gpu);
                let mut outline_mask = vec![0u8; mask_w * mask_h];
                for i in 0..outline_mask.len() {
                    outline_mask[i] = dilated[i].saturating_sub(alpha_mask[i]);
                }
                if outline_progress < 1.0 {
                    for my in 0..mask_h {
                        for mx in 0..mask_w {
                            let global_x = (mask_x0 + mx) as i32;
                            if global_x >= outline_cutoff_x {
                                outline_mask[my * mask_w + mx] = 0;
                            }
                        }
                    }
                }
                draw_mask_to_buf(&mut buf, &outline_mask, mask_w, mask_h, mask_x0, mask_y0, w, h, &outline.color, 1.0);
            }
        }

        // 4. 3D Text Extrusion (rendered before face text, after outlines)
        // — depth / angle / lighting may animate via keyframes.
        if let Some(ext) = tb.extrusion.as_ref().filter(|e| e.enabled) {
            render_extrusion(&mut buf, alpha_mask, mask_w, mask_h, mask_x0, mask_y0, w, h, ext, title, time_secs);
        }

        // 5. Text fill (pattern > gradient > solid) — this is the face text
        {
            // If extrusion is active and has a non-transparent face_color, use that
            let face_color = if let Some(ext) = tb.extrusion.as_ref().filter(|e| e.enabled) {
                if ext.face_color[3] > 0 {
                    ext.face_color
                } else {
                    tb.color
                }
            } else {
                tb.color
            };

            if let Some(pattern) = tb.pattern_fill.as_ref().filter(|_| tb.pattern_fill_enabled) {
                draw_pattern_fill(&mut buf, alpha_mask, mask_w, mask_h, mask_x0, mask_y0, w, h, pattern, scale);
            } else if let Some(gradient) = tb.gradient.as_ref().filter(|g| g.enabled) {
                let text_top = layout.text_top as i32;
                let text_bot = layout.text_bottom as i32;
                let text_height = (text_bot - text_top).max(1) as f32;
                for my in 0..mask_h {
                    let global_y = (mask_y0 + my) as i32;
                    let t = ((global_y - text_top) as f32 / text_height).clamp(0.0, 1.0);
                    let r = lerp_u8(gradient.top_color[0], gradient.bottom_color[0], t);
                    let g = lerp_u8(gradient.top_color[1], gradient.bottom_color[1], t);
                    let b = lerp_u8(gradient.top_color[2], gradient.bottom_color[2], t);
                    let a = lerp_u8(gradient.top_color[3], gradient.bottom_color[3], t);
                    for mx in 0..mask_w {
                        let cov = alpha_mask[my * mask_w + mx];
                        if cov == 0 { continue; }
                        let px = mask_x0 + mx;
                        let py = mask_y0 + my;
                        if px >= w || py >= h { continue; }
                        let alpha = (cov as u16 * a as u16 / 255) as u8;
                        let tinted = [r, g, b, alpha];
                        let idx = (py * w + px) * 4;
                        blend_pixel(&mut buf, idx, &tinted);
                    }
                }
            } else if !layout.glyph_colors.is_empty() {
                // Rich text: draw each glyph individually with its own color
                draw_rich_text_fill(
                    &mut buf, w, h, mask_x0, mask_y0, mask_w, mask_h,
                    &layout.glyph_colors, &layout.glyph_coverages,
                );
            } else {
                draw_mask_to_buf(&mut buf, alpha_mask, mask_w, mask_h, mask_x0, mask_y0, w, h, &face_color, 1.0);
            }
        }

        // 6. Inner shadow stack — rendered front-to-back so the
        // lowest index reads as nearest the surface. Secondary layers
        // (index >= 1) are gated by `inner_shadow_stack_enabled`.
        {
            let t = time_secs.map(|t| t as f64).unwrap_or(0.0);
            for (i, inner) in tb.inner_shadows.iter().enumerate().rev() {
                if !inner.enabled {
                    continue;
                }
                if i >= 1 && !tb.inner_shadow_stack_enabled {
                    continue;
                }
                let off_x = inner.evaluated_offset_x(t);
                let off_y = inner.evaluated_offset_y(t);
                let blur = inner.evaluated_blur(t);
                let sox = (off_x * scale).round() as i32;
                let soy = (off_y * scale).round() as i32;
                let mut inverted = vec![0u8; mask_w * mask_h];
                for j in 0..inverted.len() {
                    inverted[j] = 255u8.saturating_sub(alpha_mask[j]);
                }
                let offset_inv = offset_alpha(&inverted, mask_w, mask_h, sox, soy);
                let shadow_mask = if blur > 0.0 {
                    do_blur(&offset_inv, mask_w, mask_h, blur * scale, &mut gpu)
                } else {
                    offset_inv
                };
                let mut inner_mask = vec![0u8; mask_w * mask_h];
                for j in 0..inner_mask.len() {
                    inner_mask[j] = ((shadow_mask[j] as u16 * alpha_mask[j] as u16) / 255) as u8;
                }
                draw_mask_to_buf(&mut buf, &inner_mask, mask_w, mask_h, mask_x0, mask_y0, w, h, &inner.color, 1.0);
            }
        }

        to_bytes(&buf)
    }

    /// Render a `TitleData` to an RGBA buffer of `width x height` pixels (backward compat).
    ///
    /// Hits the per-renderer LRU when the same title content lands at
    /// the same size — the common case during playback (scrolling
    /// credits, crawling tickers, slide / fade animations all keep
    /// `TitleData` constant frame-to-frame and only animate via
    /// downstream `transform_pos_*` keyframes). Cache miss cost is
    /// the full shaping + rasterisation; cache hit cost is one
    /// memcpy of the rasterised buffer.
    pub fn render(&mut self, title: &TitleData, width: u32, height: u32) -> Vec<u8> {
        self.render_with_gpu(title, width, height, None)
    }

    /// Render with optional GPU-accelerated alpha mask processing.
    ///
    /// Cache is keyed only by `(title, width, height)` — the GPU
    /// argument is a runtime processor, not part of the visual
    /// output, so a cached buffer from a CPU render is safe to
    /// return on the GPU path too. Hitting the cache means the GPU
    /// processor runs zero work for that frame.
    pub fn render_with_gpu(
        &mut self,
        title: &TitleData,
        width: u32,
        height: u32,
        gpu: Option<&mut dyn AlphaMaskProcessor>,
    ) -> Vec<u8> {
        if let Some(hit) = self.cache_lookup(title, width, height) {
            self.last_rows = hit.rows;
            return (*hit.pixels).clone();
        }
        let buf = self.render_inner(title, width, height, gpu, None);
        let rows = alpha_rows(&buf, width, height);
        self.last_rows = rows;
        self.cache_store(title.clone(), width, height, buf.clone(), rows);
        buf
    }

    /// Render at a specific clip-relative time for time-aware
    /// animations such as per-character entrance staggering. The
    /// cache is bypassed entirely when ANY object's `char_animation`
    /// is set, since the visual output varies per frame for the
    /// duration of the entrance ramp; once every glyph has settled
    /// (every `local_t = 1`) the per-glyph path produces an output
    /// identical to the static cached render — the engine could
    /// elide the call there as a future optimisation.
    /// Render at `time_secs` into a clip `window_secs` long, so exit
    /// motion can count back from the clip's end.
    pub fn render_with_window(
        &mut self,
        title: &TitleData,
        width: u32,
        height: u32,
        gpu: Option<&mut dyn AlphaMaskProcessor>,
        time_secs: f32,
        window_secs: Option<f32>,
    ) -> Vec<u8> {
        self.motion_window = window_secs;
        let out = self.render_with_time(title, width, height, gpu, time_secs);
        self.motion_window = None;
        out
    }

    /// `render_with_window`, with the rows that carry any pixel as
    /// `y0..y1`, so a compositor can upload and blend only those.
    #[allow(clippy::too_many_arguments)]
    pub fn render_with_window_rows(
        &mut self,
        title: &TitleData,
        width: u32,
        height: u32,
        gpu: Option<&mut dyn AlphaMaskProcessor>,
        time_secs: f32,
        window_secs: Option<f32>,
    ) -> (Vec<u8>, (u32, u32)) {
        let out = self.render_with_window(title, width, height, gpu, time_secs, window_secs);
        (out, self.last_rows)
    }

    /// [`Self::render_with_window`]'s frame in the cheapest exact form:
    /// the resting sprite and where it goes when that is all the frame
    /// is, else the rendered pixels. `render_with_window` is this with
    /// the placement applied on the CPU.
    #[allow(clippy::too_many_arguments)]
    pub fn frame_with_window(
        &mut self,
        title: &TitleData,
        width: u32,
        height: u32,
        gpu: Option<&mut dyn AlphaMaskProcessor>,
        time_secs: f32,
        window_secs: Option<f32>,
    ) -> TitleFrame {
        self.motion_window = window_secs;
        let out = self.frame_at(title, width, height, gpu, time_secs);
        self.motion_window = None;
        out
    }

    pub fn render_with_time(
        &mut self,
        title: &TitleData,
        width: u32,
        height: u32,
        gpu: Option<&mut dyn AlphaMaskProcessor>,
        time_secs: f32,
    ) -> Vec<u8> {
        match self.frame_at(title, width, height, gpu, time_secs) {
            TitleFrame::Placed { sprite, dx, dy, opacity, scale, anchor, under, rows, .. } => {
                pose_sprite(&sprite.pixels, width, height, dx, dy, opacity, scale, anchor, under, rows)
            }
            TitleFrame::Glyphs { sprite, poses, rows } => match &sprite.units {
                Some(units) => pose_sprite_glyphs(&sprite.pixels, width, height, units, &poses, rows),
                None => vec![0u8; (width as usize) * (height as usize) * 4],
            },
            TitleFrame::Rendered { pixels, .. } => pixels,
        }
    }

    /// The frame at `time_secs` as a [`TitleFrame`]; `last_rows` is set
    /// either way.
    fn frame_at(
        &mut self,
        title: &TitleData,
        width: u32,
        height: u32,
        gpu: Option<&mut dyn AlphaMaskProcessor>,
        time_secs: f32,
    ) -> TitleFrame {
        // Time-varying inputs:
        //   - char_animation on any object (per-glyph entrance)
        //   - title-level ColorCycle (continuous hue rotation)
        // Both bypass the static cache. Other animation_type variants
        // are clip-level keyframes evaluated by the engine, not the
        // renderer, so they don't change `title` and stay cacheable.
        // Soft-disabled char_animation (`enabled: false`) keeps its
        // struct + keyframes around but the renderer skips it (see
        // `match (time_secs, tb.char_animation.as_ref().filter(|c| c.enabled))`
        // in the rasterize path). Cache-bypass must respect that or
        // muted char_anim with stale keyframes pointlessly busts the
        // cache every frame.
        // An entrance or exit only changes the frame inside its own
        // window; at rest the render is the resting one and cacheable.
        let window = self.motion_window;
        let motion_live = |tb: &klipp_timeline::TextBox| -> bool {
            // How long a motion runs: its own time, plus the stagger of
            // its last unit — and only when it staggers. The stagger time
            // has a default of its own, and counting it for a motion with
            // no stagger made a 0.4 s fade on a 22-letter name "live" for
            // 1.55 s at each end: a four-second lower third was rastered
            // afresh on three frames in four, 110 ms each.
            let units = |m: &klipp_timeline::Motion| -> f32 {
                match m.stagger {
                    klipp_timeline::StaggerUnit::None => 0.0,
                    klipp_timeline::StaggerUnit::Line => tb.text.lines().count().max(1) as f32,
                    klipp_timeline::StaggerUnit::Word => tb.text.split_whitespace().count().max(1) as f32,
                    klipp_timeline::StaggerUnit::Char => tb.text.chars().filter(|c| !c.is_whitespace()).count().max(1) as f32,
                }
            };
            let span = |m: &klipp_timeline::Motion| m.delay_secs + m.duration_secs + units(m) * m.stagger_secs + 0.05;
            let enter = tb.motion.enter.as_ref().is_some_and(|m| time_secs < span(m));
            let exit = match (tb.motion.exit.as_ref(), window) {
                (Some(m), Some(w)) => (w - time_secs) < span(m),
                _ => false,
            };
            enter || exit
        };
        // The loops, evaluated up front: a loop that only moves or
        // fades the layer can be applied to the cached resting render.
        let scale = height as f32 / 1080.0;
        let poses: Vec<(usize, klipp_timeline::LoopPose)> = title
            .objects
            .iter()
            .enumerate()
            .filter_map(|(i, o)| {
                let TitleObjectContent::TextBox(tb) = &o.content else { return None };
                if tb.loops.is_empty() || !o.visible {
                    return None;
                }
                let font_px = tb.font_size * scale;
                let lines = tb.text.lines().count().max(1) as f32;
                let max_chars = tb.text.lines().map(|l| l.chars().count()).max().unwrap_or(0).max(1) as f32;
                let geo = klipp_timeline::LoopGeometry {
                    frame_w: width as f32,
                    frame_h: height as f32,
                    text_w: max_chars * font_px * 0.55,
                    text_h: lines * font_px * tb.line_spacing.max(0.5) * 1.4,
                    font_px,
                };
                Some((i, klipp_core::title_motion::eval_loops(&tb.loops, time_secs, window, geo, tb.color)))
            })
            .collect();
        let loops_need_raster = poses.iter().any(|(_, p)| p.wave.is_some() || p.color.is_some() || p.text.is_some());
        let any_motion_live = title.objects.iter().any(|o| match &o.content {
            TitleObjectContent::TextBox(tb) => o.visible && motion_live(tb),
            _ => false,
        });
        // One visible layer carrying the only pose: the cached resting
        // render, shifted and faded, is the frame.
        let visible = title.objects.iter().filter(|o| o.visible).count();
        let translate_only = match poses.as_slice() {
            [(_, p)] => (!loops_need_raster && visible == 1).then(|| p.clone()),
            _ => None,
        };
        // An entrance or exit that moves, fades or scales the whole
        // layer — no stagger, no blur — is a pose on the resting render,
        // as a loop is: the frame is the sprite placed, on the GPU where
        // the compositor keeps it. Rastered afresh, it cost 30 ms a
        // frame at 4K for the whole of a half-second rise.
        // (opacity, dx, dy, scale, blur sigma, clip to the resting box)
        let motion_pose: Option<(f32, f32, f32, f32, f32, bool)> = if any_motion_live && visible == 1 && !loops_need_raster {
            title.objects.iter().find(|o| o.visible).and_then(|o| {
                let TitleObjectContent::TextBox(tb) = &o.content else { return None };
                let whole = |m: Option<klipp_timeline::Motion>| m.is_none_or(|m| m.stagger == klipp_timeline::StaggerUnit::None);
                if !(whole(tb.motion.enter) && whole(tb.motion.exit)) {
                    return None;
                }
                let font_px = tb.font_size * scale;
                let (pe, px) = tb.motion.progress(time_secs, window, 0);
                let rest = (1.0, 0.0, 0.0, 1.0, 0.0, false);
                let (a1, dx1, dy1, s1, b1, c1) = tb.motion.enter.map(|m| m.pose(pe, font_px)).unwrap_or(rest);
                let (a2, dx2, dy2, s2, b2, c2) = tb.motion.exit.map(|m| m.pose(px, font_px)).unwrap_or(rest);
                Some((a1 * a2, dx1 + dx2, dy1 + dy2, s1 * s2, b1.max(b2), c1 || c2))
            })
        } else {
            None
        };
        // A motion that staggers by character, word or line poses each
        // glyph on its own from the resting render's glyph map — the
        // frame is the glyphs placed, on the GPU. A blur cannot be
        // posed per glyph and is rastered.
        let staggered: Option<&klipp_timeline::TextBox> = if any_motion_live && motion_pose.is_none() && visible == 1 && !loops_need_raster {
            title.objects.iter().find(|o| o.visible).and_then(|o| match &o.content {
                TitleObjectContent::TextBox(tb)
                    if tb.motion.enter.is_none_or(|m| m.kind != klipp_timeline::MotionKind::Blur)
                        && tb.motion.exit.is_none_or(|m| m.kind != klipp_timeline::MotionKind::Blur) =>
                {
                    Some(tb)
                }
                _ => None,
            })
        } else {
            None
        };
        let any_path_motion = title.objects.iter().any(|o| o.visible && matches!(&o.content, TitleObjectContent::Shape(Shape {kind:ShapeKind::Path(p),..}) if !p.keyframes.is_empty()));
        let any_char_anim = (any_motion_live && motion_pose.is_none() && staggered.is_none()) || (!poses.is_empty() && translate_only.is_none());

        // Glitch is per-frame (deterministic seed bound to clip
        // time, but pattern shifts ~12 times/sec) so it bypasses
        // cache too.
        let any_glitch_active = title.objects.iter().any(|o| {
            if let TitleObjectContent::TextBox(tb) = &o.content {
                tb.glitch_intensity > 0.0
            } else {
                false
            }
        });

        // Per-layer position keyframes: any layer with non-empty
        // center_*_keyframes makes the frame time-dependent and must
        // bypass the static cache.
        let any_position_keyframes = title.objects.iter().any(|o| {
            !o.transform.center_x_keyframes.is_empty()
                || !o.transform.center_y_keyframes.is_empty()
        });

        // Per-effect keyframes: TextGlow's radius/intensity animate
        // when their respective vecs are non-empty. Same cache-bypass
        // requirement as position keyframes.
        let any_glow_keyframes = title.objects.iter().any(|o| {
            if let TitleObjectContent::TextBox(tb) = &o.content {
                tb.glows.iter().any(|g| {
                    !g.radius_keyframes.is_empty() || !g.intensity_keyframes.is_empty()
                })
            } else {
                false
            }
        });
        let any_shadow_keyframes = title.objects.iter().any(|o| {
            if let TitleObjectContent::TextBox(tb) = &o.content {
                tb.shadows.iter().any(|s| {
                    !s.offset_x_keyframes.is_empty()
                        || !s.offset_y_keyframes.is_empty()
                        || !s.blur_keyframes.is_empty()
                })
            } else {
                false
            }
        });
        let any_inner_shadow_keyframes = title.objects.iter().any(|o| {
            if let TitleObjectContent::TextBox(tb) = &o.content {
                tb.inner_shadows.iter().any(|s| {
                    !s.offset_x_keyframes.is_empty()
                        || !s.offset_y_keyframes.is_empty()
                        || !s.blur_keyframes.is_empty()
                })
            } else {
                false
            }
        });
        let any_extrusion_keyframes = title.objects.iter().any(|o| {
            if let TitleObjectContent::TextBox(tb) = &o.content {
                tb.extrusion
                    .as_ref()
                    .map(|e| {
                        !e.depth_keyframes.is_empty()
                            || !e.angle_keyframes.is_empty()
                            || !e.lighting_keyframes.is_empty()
                    })
                    .unwrap_or(false)
            } else {
                false
            }
        });
        let any_outline_keyframes = title.objects.iter().any(|o| {
            if let TitleObjectContent::TextBox(tb) = &o.content {
                tb.outlines.iter().any(|ol| !ol.width_keyframes.is_empty())
            } else {
                false
            }
        });

        let any_render_time_override = any_path_motion || any_char_anim
            || any_glitch_active
            || any_position_keyframes
            || any_glow_keyframes
            || any_shadow_keyframes
            || any_inner_shadow_keyframes
            || any_extrusion_keyframes
            || any_outline_keyframes;

        let mut gpu = gpu;
        if !any_render_time_override {
            // A full-frame card: the words are the sprite and the fill
            // goes under them at placement, so the fill stays put while
            // the words move, and one resting render serves the card
            // moving and at rest.
            let under = match &title.background {
                TitleBackground::FullFrame { color } => Some(*color),
                _ => None,
            };
            let sprite = self.resting_sprite(&resting_title(title), width, height, gpu.take());
            let rows = sprite.rows;
            if let Some(tb) = staggered {
                if let Some(units) = sprite.units.clone().filter(|u| u.boxes.len() <= MAX_POSED_GLYPHS) {
                    let font_px = tb.font_size * scale;
                    let loop_pose = translate_only.as_ref().map(|p| (p.dx, p.dy, p.opacity)).unwrap_or((0.0, 0.0, 1.0));
                    let poses = glyph_poses(&units, &tb.motion, time_secs, window, font_px, loop_pose, under.is_some());
                    let rows = if under.is_some() {
                        (0, height)
                    } else {
                        let (mut y0, mut y1) = (i32::MAX, i32::MIN);
                        for p in &poses {
                            if p.alpha > 0.001 && p.moved[3] > p.moved[1] {
                                y0 = y0.min(p.moved[1]);
                                y1 = y1.max(p.moved[3]);
                            }
                        }
                        if y1 <= y0 { (0, 0) } else { (y0.clamp(0, height as i32) as u32, y1.clamp(0, height as i32) as u32) }
                    };
                    self.last_rows = rows;
                    self.last_blur = 0.0;
                    return TitleFrame::Glyphs { sprite, poses, rows };
                }
                // Too many glyphs to pose: rastered, below.
            } else {
            let (mut dx, mut dy, mut opacity, mut scale, mut blur, mut clip) = (0.0f32, 0.0f32, 1.0f32, 1.0f32, 0.0f32, false);
            if let Some(pose) = &translate_only {
                dx += pose.dx;
                dy += pose.dy;
                opacity *= pose.opacity;
            }
            if let Some((a, mx, my, ms, b, c)) = motion_pose {
                dx += mx;
                dy += my;
                opacity *= a;
                scale *= ms;
                blur = b;
                clip = c;
            }
            let (dx, dy, opacity) = (dx.round() as i32, dy.round() as i32, opacity.clamp(0.0, 1.0));
            let anchor = sprite.anchor();
            let rows = if under.is_some() {
                (0, height)
            } else if opacity <= 0.001 || rows.1 <= rows.0 {
                (0, 0)
            } else if clip {
                // A mask motion: the words show inside their resting box only.
                rows
            } else {
                let grown = |y: u32| anchor.1 + (y as f32 - anchor.1) * scale + dy as f32;
                let reach = if blur > 0.5 { blur * 3.0 } else { 0.0 };
                let y0 = grown(rows.0).floor() - 1.0 - reach;
                let y1 = grown(rows.1).ceil() + 1.0 + reach;
                (y0.clamp(0.0, height as f32) as u32, y1.clamp(0.0, height as f32) as u32)
            };
            self.last_rows = rows;
            self.last_blur = if blur > 0.5 { blur } else { 0.0 };
            return TitleFrame::Placed { sprite, dx, dy, opacity, scale, anchor, under, blur: self.last_blur, rows };
            }
        }
        self.last_blur = 0.0;

        // Build a one-frame derived TitleData that applies every
        // active render-time override. The clone is bounded to
        // exactly one allocation per render call.
        // The loops of every layer, folded into a one-frame copy of
        // the title: opacity, offset, colour and counter text on the
        // object, the wave kept aside for the glyph rasteriser.
        self.loop_waves.clear();
        let derived: TitleData = {
            let mut td = title.clone();
            for (i, pose) in poses {
                let obj = &mut td.objects[i];
                let TitleObjectContent::TextBox(tb) = &mut obj.content else { continue };
                obj.opacity *= pose.opacity;
                if obj.opacity <= 0.001 {
                    obj.visible = false;
                    continue;
                }
                obj.transform.center.x += pose.dx / width as f32;
                obj.transform.center.y += pose.dy / height as f32;
                if let Some(c) = pose.color {
                    tb.color = c;
                }
                if let Some(text) = pose.text {
                    tb.text = text;
                }
                if let Some(w) = pose.wave {
                    self.loop_waves.insert(obj.id.0, w);
                }
            }
            td
        };

        // Pass time through whenever ANY render-time effect needs
        // it — char_animation drives per-glyph staggering, glitch
        // seeds its stripe pattern from the clip clock, per-layer
        // position keyframes evaluate against this time inside
        // `render_inner`, and per-effect keyframes (e.g. TextGlow
        // radius/intensity) sample inside `compose`.
        let time_arg = if any_path_motion || any_char_anim
            || any_glitch_active
            || any_position_keyframes
            || any_glow_keyframes
            || any_shadow_keyframes
            || any_inner_shadow_keyframes
            || any_extrusion_keyframes
            || any_outline_keyframes
        {
            Some(time_secs)
        } else {
            None
        };
        let out = self.render_inner(&derived, width, height, gpu, time_arg);
        self.loop_waves.clear();
        self.last_rows = alpha_rows(&out, width, height);
        TitleFrame::Rendered { rows: self.last_rows, pixels: out }
    }

    /// Object-model composition: walk every layer in the title (back
    /// → front), rasterise each onto a transparent frame-sized RGBA
    /// buffer, then alpha-composite the stack onto the title-level
    /// background. Each `TextBox` object goes through the existing
    /// single-text raster path via a synthesised single-object
    /// `TitleData` wrapper so all the rich styling effects (shadow,
    /// outline, glow, gradient, extrusion, text-on-path, character
    /// animation, motion blur) work per-object without duplicating
    /// the whole pipeline. `Shape` objects are painted directly.
    /// Hidden objects (`visible == false`) are skipped.
    fn render_inner(
        &mut self,
        title: &TitleData,
        width: u32,
        height: u32,
        gpu: Option<&mut dyn AlphaMaskProcessor>,
        time_secs: Option<f32>,
    ) -> Vec<u8> {
        // The GPU alpha-mask processor is consumable: we use it for
        // the first TextBox layer (the most common case is one text
        // object per title — credits, lower-third title, single-line
        // banner). Additional TextBox layers in the same title fall
        // back to the CPU mask path. When per-object animation lands,
        // we'll likely refactor this to per-layer GPU dispatch via
        // the `wgpu` queue.
        let mut gpu_for_first_text = gpu;
        let pixel_count = (width as usize) * (height as usize);
        let mut buf = vec![0u8; pixel_count * 4];
        // Title-level background fill (legacy field — will move to a
        // Shape object once on-canvas editing lands).
        if let TitleBackground::FullFrame { color } = &title.background {
            for pixel in buf.chunks_exact_mut(4) {
                pixel.copy_from_slice(color);
            }
        }

        // Until something is painted, `buf` is empty (unless a full
        // frame background filled it): the first text layer is the
        // buffer, no blend needed.
        let mut buf_untouched = !matches!(title.background, TitleBackground::FullFrame { .. });
        // A colour backing is a box round the words, so it is the text
        // layer that knows where it goes: the first text box carries it.
        // Dropped from every layer, as it was, it was never painted.
        let mut backing = matches!(title.background, TitleBackground::Color { .. }).then(|| title.background.clone());
        for obj in &title.objects {
            if !obj.visible {
                continue;
            }
            let first_layer = buf_untouched;
            buf_untouched = false;
            // Per-layer position keyframes: when present, override the
            // static `center` for this frame. Empty keyframe vecs
            // short-circuit to the static value via `evaluated_center`,
            // so titles without animation pay nothing extra here.
            let mut effective_transform = match time_secs {
                Some(t) => {
                    let mut t_xform = obj.transform.clone();
                    t_xform.center = obj.transform.evaluated_center(t as f64);
                    t_xform
                }
                None => obj.transform.clone(),
            };
            match &obj.content {
                TitleObjectContent::TextBox(tb) => {
                    // Synthesise a one-object TitleData carrying just
                    // this TextBox + its transform centre, then route
                    // through the existing single-text raster path.
                    // A full-frame `background` is painted once, above,
                    // and not per text layer; a colour backing goes with
                    // the first text box.
                    let mut single = TitleData::single_text(tb.clone());
                    single.background = backing.take().unwrap_or(TitleBackground::None);
                    if let Some(o) = single.objects.first_mut() {
                        o.transform = effective_transform;
                    }
                    self.loop_wave = self.loop_waves.get(&obj.id.0).copied();
                    let layer = self.render_text_layer(
                        &single,
                        width,
                        height,
                        gpu_for_first_text.take(),
                        time_secs,
                    );
                    self.loop_wave = None;
                    if first_layer && (obj.opacity - 1.0).abs() < 0.001 {
                        buf = layer;
                    } else if (obj.opacity - 1.0).abs() < 0.001 {
                        over_blend(&mut buf, &layer);
                    } else {
                        // Per-object opacity: scale the layer's alpha
                        // channel before composition. Branching on the
                        // ~1.0 case keeps the common path identical to
                        // the pre-opacity rendering hot loop.
                        let mut faded = layer;
                        scale_layer_alpha(&mut faded, obj.opacity);
                        over_blend(&mut buf, &faded);
                    }
                }
                TitleObjectContent::Shape(shape) => {
                    if let Some(fit) = shape.fit {
                        let Some(fitted) = self.resolved_shape_fit(title, fit, width, height, time_secs) else { continue };
                        effective_transform = fitted;
                    }
                    if (obj.opacity - 1.0).abs() < 0.001 {
                        paint_shape_layer(&mut buf, shape, &effective_transform, width, height, time_secs.unwrap_or(0.0));
                    } else {
                        // Render the shape into a fresh transparent
                        // layer, scale its alpha, then over-blend —
                        // matches the per-object opacity behaviour
                        // for TextBox layers.
                        let mut layer = vec![0u8; (width as usize) * (height as usize) * 4];
                        paint_shape_layer(&mut layer, shape, &effective_transform, width, height, time_secs.unwrap_or(0.0));
                        scale_layer_alpha(&mut layer, obj.opacity);
                        over_blend(&mut buf, &layer);
                    }
                }
                TitleObjectContent::Image(img) => {
                    if let Some(decoded) = self.decoded_image(img) {
                        let mut layer = vec![0u8; (width as usize) * (height as usize) * 4];
                        paint_image_layer(&mut layer, &decoded, img.fit, &effective_transform, width, height);
                        if (obj.opacity - 1.0).abs() >= 0.001 {
                            scale_layer_alpha(&mut layer, obj.opacity);
                        }
                        over_blend(&mut buf, &layer);
                    }
                }
            }
        }
        buf
    }

    /// Resolve a backing's normalized rectangle using the same text bounds
    /// as rendering. Missing, hidden or empty text has no selectable backing.
    /// Callers may cache resting geometry and translate it with the target's
    /// evaluated center when only its position keyframes change.
    pub fn resolved_shape_fit(
        &mut self,
        title: &TitleData,
        fit: klipp_timeline::ShapeFit,
        width: u32,
        height: u32,
        time_secs: Option<f32>,
    ) -> Option<ObjectTransform> {
        if width == 0 || height == 0 { return None; }
        let target = title.object(fit.target).filter(|o| o.visible)?;
        let TitleObjectContent::TextBox(tb) = &target.content else { return None };
        if tb.text.trim().is_empty() { return None; }
        let mut single = TitleData::single_text(tb.clone());
        single.objects[0].transform = target.transform.clone();
        if let Some(t) = time_secs {
            single.objects[0].transform.center = target.transform.evaluated_center(t as f64);
        }
        let layout = self.render_layout_at_time(&single, width, height, time_secs);
        if layout.glyph_count == 0 { return None; }
        let px = fit.padding.x.max(0.0) * height as f32 / 1080.0;
        let py = fit.padding.y.max(0.0) * height as f32 / 1080.0;
        let mut transform = klipp_timeline::default_text_transform();
        transform.center.x = (layout.text_left + layout.text_right) * 0.5 / width as f32;
        transform.center.y = (layout.text_top + layout.text_bottom) * 0.5 / height as f32;
        transform.size.x = (layout.text_right - layout.text_left + 2.0 * px) / width as f32;
        transform.size.y = (layout.text_bottom - layout.text_top + 2.0 * py) / height as f32;
        Some(transform)
    }

    /// The decoded pixels of an image layer, read once per path.
    fn decoded_image(&mut self, img: &ImageLayer) -> Option<std::sync::Arc<DecodedImage>> {
        if let Some(cached) = self.image_cache.get(&img.path) {
            return cached.clone();
        }
        let decoded = image::open(&img.path).ok().map(|i| {
            let rgba = i.to_rgba8();
            std::sync::Arc::new(DecodedImage { w: rgba.width() as usize, h: rgba.height() as usize, rgba: rgba.into_raw() })
        });
        if decoded.is_none() {
            tracing::warn!("title image layer could not be read: {}", img.path);
        }
        self.image_cache.insert(img.path.clone(), decoded.clone());
        decoded
    }

    /// Forget a decoded image so a changed file is read again.
    pub fn forget_image(&mut self, path: &str) {
        self.image_cache.remove(path);
    }

    /// Single-text rasterisation: layout → compose → texture-fill,
    /// returns a frame-sized RGBA buffer with the text composited
    /// onto a transparent canvas. The synthesised `TitleData` from
    /// `render_inner` always has exactly one `TextBox` object, so
    /// the existing single-text helpers stay unchanged.
    fn render_text_layer(
        &mut self,
        title: &TitleData,
        width: u32,
        height: u32,
        gpu: Option<&mut dyn AlphaMaskProcessor>,
        time_secs: Option<f32>,
    ) -> Vec<u8> {
        let layout = self.render_layout_at_time(title, width, height, time_secs);
        let mut buf = Self::compose(&layout, title, width, height, gpu, time_secs);
        self.apply_texture_fill(&mut buf, &layout, title, width, height);

        // Post-pass effects: applied to the fully-composed layer
        // buffer so they pick up outline, shadow, glow, gradient,
        // texture-fill, and any other decorative layer the user
        // configured. Cheap; only run when the corresponding field
        // is non-zero.
        if let Some(tb) = title.primary_text() {
            let scale = height as f32 / 1080.0;
            if tb.chromatic_aberration_px > 0.0 {
                apply_chromatic_aberration(
                    &mut buf, width, height,
                    tb.chromatic_aberration_px * scale,
                );
            }
            if tb.glitch_intensity > 0.0 {
                let seed = time_secs.map_or(0u32, |t| {
                    // Quantise to ~12 steps/sec so the glitch
                    // pattern doesn't change every single frame
                    // (looks like static noise) while still feeling
                    // alive. 12 steps/s reads as TV-glitchy.
                    (t * 12.0) as u32
                });
                apply_glitch(&mut buf, width, height, tb.glitch_intensity, seed);
            }
        }

        self.last_units = (!layout.glyph_index.is_empty()).then(|| std::sync::Arc::new(UnitMap::from_layout(&layout, &buf, width, height)));
        buf
    }

    /// Look up a cached rasterised buffer for the given title +
    /// dimensions. On hit, moves the entry to the front of the LRU
    /// (most-recently-used end) and returns a memcpy of the buffer.
    /// The resting render of `title` (as `resting_title` names it) at
    /// `width`×`height`, made here on the CPU: for a warm-up on another
    /// thread, handed to the renderer that will place it by `adopt`.
    pub fn render_resting(&mut self, title: &TitleData, width: u32, height: u32) -> RestingRender {
        let pixels = self.render_inner(&resting_title(title), width, height, None, None);
        RestingRender { pixels, units: self.last_units.take() }
    }

    /// Keep a resting render of `title` made by `render_resting`
    /// elsewhere, so the first frame of the title costs this thread
    /// nothing. Ignored when one is cached already or the bytes are not
    /// a frame.
    pub fn adopt(&mut self, title: &TitleData, width: u32, height: u32, render: RestingRender) {
        let key = resting_title(title);
        let RestingRender { pixels, units } = render;
        if pixels.len() != (width as usize) * (height as usize) * 4 || self.cache_lookup(&key, width, height).is_some() {
            return;
        }
        let rows = alpha_rows(&pixels, width, height);
        self.last_units = units;
        self.cache_store(key.into_owned(), width, height, pixels, rows);
    }

    /// The blur the last placed frame asked for (sigma in pixels): a
    /// caller working from the bytes applies it as a Gaussian blur.
    pub fn last_blur(&self) -> f32 {
        self.last_blur
    }

    /// The resting render of `title`: from the cache, or made and kept.
    fn resting_sprite(&mut self, title: &TitleData, width: u32, height: u32, gpu: Option<&mut dyn AlphaMaskProcessor>) -> RestingSprite {
        match self.cache_lookup(title, width, height) {
            Some(hit) => hit,
            None => {
                let buf = self.render_inner(title, width, height, gpu, None);
                let rows = alpha_rows(&buf, width, height);
                self.cache_store(title.clone(), width, height, buf, rows)
            }
        }
    }

    fn cache_lookup(&mut self, title: &TitleData, width: u32, height: u32) -> Option<RestingSprite> {
        let pos = self.render_cache.iter().position(|c| {
            c.width == width && c.height == height && &c.title == title
        })?;
        // Move to front (LRU).
        let entry = self.render_cache.remove(pos);
        let sprite = RestingSprite { pixels: std::sync::Arc::clone(&entry.buffer), rows: entry.rows, cols: entry.cols, id: entry.id, units: entry.units.clone() };
        self.render_cache.insert(0, entry);
        Some(sprite)
    }

    /// Insert a freshly-rendered buffer into the cache, evicting the
    /// least-recently-used entry if at capacity.
    fn cache_store(&mut self, title: TitleData, width: u32, height: u32, buffer: Vec<u8>, rows: (u32, u32)) -> RestingSprite {
        if self.render_cache.len() >= RENDER_CACHE_CAP {
            self.render_cache.pop();
        }
        let id = self.next_cache_id;
        self.next_cache_id += 1;
        let cols = alpha_cols(&buffer, width, height, rows);
        let units = self.last_units.take();
        let pixels = std::sync::Arc::new(buffer);
        self.render_cache.insert(
            0,
            CachedRender {
                title,
                width,
                height,
                buffer: std::sync::Arc::clone(&pixels),
                rows,
                cols,
                units: units.clone(),
                id,
            },
        );
        RestingSprite { pixels, rows, cols, id, units }
    }

    /// Drop every cached rasterised buffer. Useful when the host
    /// changes a font on disk or wants to free the render-cache
    /// memory (e.g. exporting a different project). Not currently
    /// invoked anywhere — the LRU bound (4 entries × 1920×1080×4 ≈
    /// 32 MB) is small enough to live indefinitely.
    pub fn invalidate_cache(&mut self) {
        self.render_cache.clear();
    }

    /// Compute character positions for the given title text, using the same layout
    /// algorithm as `render_layout`. Returns positions in frame pixel coordinates.
    /// Used by the viewer for cursor placement and click hit-testing.
    pub fn compute_char_positions(&mut self, title: &TitleData, width: u32, height: u32) -> Vec<CharPosition> {
        let tb = title.primary_text_or_default();
        if tb.text.is_empty() {
            return Vec::new();
        }

        // Mirror render_layout's case-transform handling so cursor
        // positions land on the rendered (possibly upper/lower-cased)
        // text rather than the original characters.
        let resolved_text: String = match tb.case_transform {
            klipp_timeline::TextCase::None => tb.text.clone(),
            klipp_timeline::TextCase::Uppercase
            | klipp_timeline::TextCase::SmallCaps => tb.text.to_uppercase(),
            klipp_timeline::TextCase::Lowercase => tb.text.to_lowercase(),
        };

        // Same font + layout as render_layout
        let cache_key = self.ensure_font_loaded(&tb.font_name, tb.font_style);
        let font = self.font_cache.get(&cache_key).unwrap_or(&self.fallback_font);

        let scale = height as f32 / 1080.0;
        let case_scale = match tb.case_transform {
            klipp_timeline::TextCase::SmallCaps => 0.80,
            _ => 1.0,
        };
        let font_px = tb.font_size * scale * case_scale;

        let h_align = match tb.alignment {
            TextAlignment::Left => HorizontalAlign::Left,
            TextAlignment::Center => HorizontalAlign::Center,
            TextAlignment::Right => HorizontalAlign::Right,
        };

        let mut layout = Layout::new(CoordinateSystem::PositiveYDown);
        layout.reset(&LayoutSettings {
            x: 0.0,
            y: 0.0,
            max_width: Some(text_box_extent(title, width, height).0),
            max_height: Some(text_box_extent(title, width, height).1),
            horizontal_align: h_align,
            vertical_align: VerticalAlign::Top,
            wrap_style: WrapStyle::Word,
            ..LayoutSettings::default()
        });

        // Track (glyph_count_before_append, segment_byte_start_in_full_text)
        // so we can fix up fontdue's per-segment byte_offset → absolute byte_offset.
        let mut segment_info: Vec<(usize, usize)> = Vec::new();

        if tb.style_runs.is_empty() {
            segment_info.push((0, 0));
            layout.append(&[font], &TextStyle::new(&resolved_text, font_px, 0));
        } else {
            let mut pos = 0usize;
            for run in &tb.style_runs {
                if run.start > pos && run.start <= resolved_text.len() {
                    let segment = &resolved_text[pos..run.start];
                    if !segment.is_empty() {
                        segment_info.push((layout.glyphs().len(), pos));
                        layout.append(&[font], &TextStyle::new(segment, font_px, 0));
                    }
                    pos = run.start;
                }
                let run_end = run.end.min(resolved_text.len());
                if run_end > pos {
                    let segment = &resolved_text[pos..run_end];
                    let run_size = run.font_size.unwrap_or(tb.font_size) * scale * case_scale;
                    if !segment.is_empty() {
                        segment_info.push((layout.glyphs().len(), pos));
                        layout.append(&[font], &TextStyle::new(segment, run_size, 0));
                    }
                    pos = run_end;
                }
            }
            if pos < resolved_text.len() {
                let segment = &resolved_text[pos..];
                if !segment.is_empty() {
                    segment_info.push((layout.glyphs().len(), pos));
                    layout.append(&[font], &TextStyle::new(segment, font_px, 0));
                }
            }
        }

        let glyphs = layout.glyphs();
        if glyphs.is_empty() {
            // No glyphs but text exists (e.g. whitespace only) — return a single position
            return vec![CharPosition {
                char_index: 0,
                byte_offset: 0,
                x: title.position().x * width as f32,
                y: title.position().y * height as f32,
                advance: font_px * 0.5,
                height: font_px,
            }];
        }

        let mut glyphs_owned: Vec<fontdue::layout::GlyphPosition> = glyphs.to_vec();

        // Fix up byte_offset: fontdue's byte_offset is relative to each append() call's string,
        // but we need it relative to the full tb.text.
        for (seg_idx, &(glyph_start, byte_start)) in segment_info.iter().enumerate() {
            let glyph_end = if seg_idx + 1 < segment_info.len() {
                segment_info[seg_idx + 1].0
            } else {
                glyphs_owned.len()
            };
            for g in &mut glyphs_owned[glyph_start..glyph_end] {
                g.byte_offset += byte_start;
            }
        }

        // Apply line spacing (same as render_layout)
        if tb.line_spacing != 1.0 {
            let mut line_starts: Vec<(usize, f32)> = Vec::new();
            let mut last_y: Option<f32> = None;
            for (i, g) in glyphs_owned.iter().enumerate() {
                let gy = g.y;
                if last_y.is_none() || (gy - last_y.unwrap()).abs() > 1.0 {
                    line_starts.push((i, gy));
                    last_y = Some(gy);
                }
            }
            if line_starts.len() > 1 {
                let base_line_height = line_starts[1].1 - line_starts[0].1;
                for line_idx in 1..line_starts.len() {
                    let y_shift = base_line_height * (tb.line_spacing - 1.0) * line_idx as f32;
                    let start = line_starts[line_idx].0;
                    let end = if line_idx + 1 < line_starts.len() {
                        line_starts[line_idx + 1].0
                    } else {
                        glyphs_owned.len()
                    };
                    for g in &mut glyphs_owned[start..end] {
                        g.y += y_shift;
                    }
                }
            }
        }

        // Apply tracking (same as render_layout)
        if tb.tracking != 0.0 {
            let tracking_px = tb.tracking * scale;
            for (i, g) in glyphs_owned.iter_mut().enumerate() {
                g.x += tracking_px * i as f32;
            }
        }

        // Compute text bounding box and offset (same as render_layout)
        let mut text_min_x = f32::MAX;
        let mut text_min_y = f32::MAX;
        let mut text_max_x = f32::MIN;
        let mut text_max_y = f32::MIN;
        for g in &glyphs_owned {
            text_min_x = text_min_x.min(g.x);
            text_min_y = text_min_y.min(g.y);
            text_max_x = text_max_x.max(g.x + g.width as f32);
            text_max_y = text_max_y.max(g.y + g.height as f32);
        }
        let text_w = text_max_x - text_min_x;
        let text_h = text_max_y - text_min_y;

        // Mirrors render_layout's offset calc — kept in lockstep so
        // cursor hit-testing matches the rendered glyph positions.
        let anchor_x = title.position().x * width as f32;
        let offset_x = match tb.alignment {
            TextAlignment::Left => anchor_x - text_min_x,
            TextAlignment::Center => anchor_x - text_w * 0.5 - text_min_x,
            TextAlignment::Right => anchor_x - text_w - text_min_x,
        };
        // Baseline shift is authored in 1080p reference pixels and
        // scales with the frame so a +20 px offset on a 1080p edit
        // looks the same on a 4K master. Positive shifts text DOWN.
        let baseline_shift_px = tb.baseline_shift * scale;
        let offset_y = title.position().y * height as f32 - text_h * 0.5 - text_min_y
            + baseline_shift_px;

        // Build character positions from glyph data.
        // fontdue gives us one glyph per visible character (spaces may have width=0).
        // We need to map byte_offset → char_index for correct cursor positioning.
        let mut positions: Vec<CharPosition> = Vec::with_capacity(glyphs_owned.len() + 1);

        // Build a byte_offset → char_index lookup
        let byte_to_char: Vec<(usize, usize)> = tb.text
            .char_indices()
            .enumerate()
            .map(|(ci, (bi, _))| (bi, ci))
            .collect();

        let char_count = tb.text.chars().count();

        for g in &glyphs_owned {
            let char_index = byte_to_char.iter()
                .find(|(bi, _)| *bi == g.byte_offset)
                .map(|(_, ci)| *ci)
                .unwrap_or(0);

            // For zero-width glyphs (spaces), use the font_px as a reasonable advance
            let advance = if g.width > 0 {
                g.width as f32
            } else {
                font_px * 0.3 // approximate space width
            };

            let glyph_height = if g.height > 0 {
                g.height as f32
            } else {
                font_px
            };

            positions.push(CharPosition {
                char_index,
                byte_offset: g.byte_offset,
                x: g.x + offset_x,
                y: g.y + offset_y,
                advance,
                height: glyph_height,
            });
        }

        // Sort by char_index for consistent ordering
        positions.sort_by_key(|p| p.char_index);

        // Fill in positions for characters that didn't produce glyphs (e.g. newlines).
        // Use interpolated positions from neighbors.
        let mut filled = Vec::with_capacity(char_count + 1);
        let mut pos_idx = 0;
        for ci in 0..char_count {
            if pos_idx < positions.len() && positions[pos_idx].char_index == ci {
                filled.push(positions[pos_idx].clone());
                pos_idx += 1;
            } else {
                // Invisible char (newline, etc.) — place at the end of previous line or start of next
                if let Some(prev) = filled.last() {
                    filled.push(CharPosition {
                        char_index: ci,
                        byte_offset: tb.text.char_indices().nth(ci).map(|(bi, _)| bi).unwrap_or(0),
                        x: prev.x + prev.advance,
                        y: prev.y,
                        advance: 0.0,
                        height: prev.height,
                    });
                } else if !positions.is_empty() {
                    let next = &positions[0];
                    filled.push(CharPosition {
                        char_index: ci,
                        byte_offset: 0,
                        x: next.x,
                        y: next.y,
                        advance: 0.0,
                        height: next.height,
                    });
                }
            }
        }

        // Add end-of-text position for cursor-at-end
        if let Some(last) = filled.last() {
            filled.push(CharPosition {
                char_index: char_count,
                byte_offset: tb.text.len(),
                x: last.x + last.advance,
                y: last.y,
                advance: 0.0,
                height: last.height,
            });
        }

        filled
    }

    /// Load a texture fill image, caching it across frames.
    fn load_texture_fill(&mut self, path: &str) -> Option<(&[u8], u32, u32)> {
        // Check if we already have this path cached
        let needs_load = match &self.texture_fill_cache {
            Some((cached_path, _, _, _)) => cached_path != path,
            None => true,
        };

        if needs_load {
            match image::open(path) {
                Ok(img) => {
                    let rgba = img.to_rgba8();
                    let (w, h) = (rgba.width(), rgba.height());
                    self.texture_fill_cache = Some((path.to_string(), rgba.into_raw(), w, h));
                }
                Err(e) => {
                    tracing::warn!("Failed to load texture fill image '{}': {}", path, e);
                    self.texture_fill_cache = None;
                    return None;
                }
            }
        }

        self.texture_fill_cache
            .as_ref()
            .map(|(_, data, w, h)| (data.as_slice(), *w, *h))
    }

    /// Apply texture fill over the composed buffer, replacing text fill pixels
    /// with sampled image pixels where the alpha mask is non-zero.
    fn apply_texture_fill(
        &mut self,
        buf: &mut [u8],
        layout: &TitleLayout,
        title: &TitleData,
        width: u32,
        _height: u32,
    ) {
        let tb = title.primary_text_or_default();
        let tf = match tb.texture_fill.as_ref() {
            Some(t) if t.enabled => t.clone(),
            _ => return,
        };

        if layout.glyph_count == 0 || layout.mask_w == 0 || layout.mask_h == 0 {
            return;
        }

        // Resolve the texture source into an owned RGBA buffer plus
        // dimensions. File paths go through the existing load cache;
        // procedural presets generate at the standard 256×256 native
        // size — every preset is cheap enough to regenerate per frame
        // (~65 k pixels with constant-time math each).
        let (tex_data, tex_w, tex_h): (Vec<u8>, u32, u32) = match &tf.source {
            klipp_timeline::TextureSource::File(p) if !p.is_empty() => {
                match self.load_texture_fill(p) {
                    Some((d, w, h)) => (d.to_vec(), w, h),
                    None => return,
                }
            }
            klipp_timeline::TextureSource::Preset(id) => {
                let (d, w, h) = generate_preset_texture(*id, 256, 256);
                (d, w, h)
            }
            _ => return,
        };
        if tex_data.is_empty() || tex_w == 0 || tex_h == 0 {
            return;
        }

        // Text bounding box in frame coordinates
        let text_left = layout.text_left;
        let text_top = layout.text_top;
        let text_w = (layout.text_right - layout.text_left).max(1.0);
        let text_h = (layout.text_bottom - layout.text_top).max(1.0);

        let mask_w = layout.mask_w as usize;
        let mask_h = layout.mask_h as usize;
        let mask_x0 = layout.mask_x0 as usize;
        let mask_y0 = layout.mask_y0 as usize;
        let w = width as usize;

        // Pre-compute the per-pixel UV transform constants so the
        // hot loop only touches scalar math.
        let scale_inv = if tf.scale.abs() < 1e-6 { 1.0 } else { 1.0 / tf.scale };
        let rot_rad = tf.rotation.to_radians();
        let cos_r = rot_rad.cos();
        let sin_r = rot_rad.sin();
        let off_u = tf.offset_x / tex_w as f32;
        let off_v = tf.offset_y / tex_h as f32;
        let opacity = tf.opacity.clamp(0.0, 1.0);
        let tint_r = tf.tint[0] as f32 / 255.0;
        let tint_g = tf.tint[1] as f32 / 255.0;
        let tint_b = tf.tint[2] as f32 / 255.0;
        // Aspect ratios used by Fit / Cover tile modes.
        let aspect_text = text_w / text_h;
        let aspect_tex = tex_w as f32 / tex_h as f32;
        let tile_mode = tf.tile_mode;

        for my in 0..mask_h {
            for mx in 0..mask_w {
                let mask_alpha = layout.alpha_mask[my * mask_w + mx];
                if mask_alpha == 0 {
                    continue;
                }

                let global_x = mask_x0 + mx;
                let global_y = mask_y0 + my;
                if global_x >= w {
                    continue;
                }

                // Base UV — pixel position normalised inside the
                // text bbox.
                let mut u = (global_x as f32 - text_left) / text_w;
                let mut v = (global_y as f32 - text_top) / text_h;

                // Tile-mode adjustment to UV (Fit / Cover apply
                // before rotation/offset/scale so the texture stays
                // in its native aspect when the user dials those
                // modes).
                let mut skip = false;
                match tile_mode {
                    klipp_timeline::TextureTileMode::Stretch => {
                        // No adjustment — UV maps directly to bbox.
                    }
                    klipp_timeline::TextureTileMode::Tile => {
                        // Adjustment happens in the rem_euclid step
                        // below after offset/scale/rotation.
                    }
                    klipp_timeline::TextureTileMode::Fit => {
                        // Preserve aspect, fit inside bbox — extra
                        // space outside the texture footprint is
                        // skipped (renders the underlying fill).
                        if aspect_text > aspect_tex {
                            let f = aspect_tex / aspect_text;
                            u = (u - 0.5) / f + 0.5;
                        } else if aspect_text < aspect_tex {
                            let f = aspect_text / aspect_tex;
                            v = (v - 0.5) / f + 0.5;
                        }
                        if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
                            skip = true;
                        }
                    }
                    klipp_timeline::TextureTileMode::Cover => {
                        // Preserve aspect, cover bbox — overflow
                        // is cropped via clamp at sample time.
                        if aspect_text > aspect_tex {
                            let f = aspect_text / aspect_tex;
                            v = (v - 0.5) * f + 0.5;
                        } else if aspect_text < aspect_tex {
                            let f = aspect_tex / aspect_text;
                            u = (u - 0.5) * f + 0.5;
                        }
                    }
                }
                if skip {
                    continue;
                }

                // Rotation around (0.5, 0.5)
                if tf.rotation != 0.0 {
                    let cu = u - 0.5;
                    let cv = v - 0.5;
                    u = 0.5 + cu * cos_r - cv * sin_r;
                    v = 0.5 + cu * sin_r + cv * cos_r;
                }

                // Offset (in texture-space pixels normalised by tex
                // size) and scale.
                u = (u - off_u) * scale_inv;
                v = (v - off_v) * scale_inv;

                // Sampling: tile wraps with rem_euclid so it works
                // for any UV; other modes clamp.
                let (tu, tv) = match tile_mode {
                    klipp_timeline::TextureTileMode::Tile => (u.rem_euclid(1.0), v.rem_euclid(1.0)),
                    _ => {
                        if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
                            // Outside texture for non-tile modes
                            // (Stretch/Fit/Cover after transform):
                            // skip pixel so the underlying fill
                            // remains visible.
                            continue;
                        }
                        (u.clamp(0.0, 1.0), v.clamp(0.0, 1.0))
                    }
                };

                let tx = ((tu * (tex_w as f32 - 1.0)).round() as u32).min(tex_w - 1);
                let ty = ((tv * (tex_h as f32 - 1.0)).round() as u32).min(tex_h - 1);
                let tex_idx = ((ty * tex_w + tx) * 4) as usize;
                if tex_idx + 3 >= tex_data.len() {
                    continue;
                }

                let buf_idx = (global_y * w + global_x) * 4;
                let alpha_norm = mask_alpha as f32 / 255.0;

                // Sample + tint multiply.
                let tex_r = tex_data[tex_idx] as f32 * tint_r;
                let tex_g = tex_data[tex_idx + 1] as f32 * tint_g;
                let tex_b = tex_data[tex_idx + 2] as f32 * tint_b;

                // The buffer at this point holds the previously-
                // rendered fill (solid colour or gradient) in
                // premultiplied alpha. Blend the texture in
                // premultiplied space using opacity as the mix
                // factor — at opacity=1 the texture fully replaces,
                // at opacity=0 the underlying fill is preserved.
                let prev_r = buf[buf_idx] as f32;
                let prev_g = buf[buf_idx + 1] as f32;
                let prev_b = buf[buf_idx + 2] as f32;
                let tex_r_pm = tex_r * alpha_norm;
                let tex_g_pm = tex_g * alpha_norm;
                let tex_b_pm = tex_b * alpha_norm;
                let inv_op = 1.0 - opacity;
                buf[buf_idx] = (prev_r * inv_op + tex_r_pm * opacity).clamp(0.0, 255.0) as u8;
                buf[buf_idx + 1] = (prev_g * inv_op + tex_g_pm * opacity).clamp(0.0, 255.0) as u8;
                buf[buf_idx + 2] = (prev_b * inv_op + tex_b_pm * opacity).clamp(0.0, 255.0) as u8;
                buf[buf_idx + 3] = mask_alpha;
            }
        }
    }
}

// ─── Procedural texture preset generators ───────────────────────
//
// Every preset is a pure function of (width, height) → RGBA Vec<u8>.
// The patterns are simple algorithmic shapes (hash-noise, sin
// modulation, modular stripes, linear gradient) so adding more
// presets is just another function and a match arm — no image
// assets to ship.

/// Cheap deterministic 2-D hash returning a u8 in [0, 255]. Used by
/// the paper-grain preset to produce a value-noise pattern without
/// pulling in a noise crate.
fn hash_2d(x: u32, y: u32) -> u8 {
    let mut h = x.wrapping_mul(0x9E3779B9).wrapping_add(y.wrapping_mul(0x85EBCA6B));
    h ^= h >> 13;
    h = h.wrapping_mul(0xC2B2AE35);
    h ^= h >> 16;
    (h & 0xFF) as u8
}

/// Soft fibrous noise — three octaves of hash-noise stacked, biased
/// toward warm off-white (paper-stock look).
fn gen_paper_grain(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((w as usize) * (h as usize) * 4);
    for y in 0..h {
        for x in 0..w {
            let n1 = hash_2d(x, y) as f32 / 255.0;
            let n2 = hash_2d(x / 4, y / 4) as f32 / 255.0;
            let n3 = hash_2d(x / 16, y / 16) as f32 / 255.0;
            let n = n1 * 0.4 + n2 * 0.4 + n3 * 0.2;
            // 220 base + up to 35 of variation = warm cream.
            let l = (220.0 + n * 35.0).clamp(0.0, 255.0) as u8;
            // Slight per-channel offset for warmth.
            out.push(l);
            out.push(l.saturating_sub(4));
            out.push(l.saturating_sub(10));
            out.push(255);
        }
    }
    out
}

/// Horizontal CRT-style scanlines. Smooth sin modulation across y so
/// the pattern doesn't alias when scaled or rotated.
fn gen_scanlines(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((w as usize) * (h as usize) * 4);
    for y in 0..h {
        // 4-pixel period — ~6 scanlines visible at 256 height.
        let phase = (y as f32 / 4.0 * std::f32::consts::PI * 2.0).sin();
        let l = (170.0 + phase * 80.0).clamp(0.0, 255.0) as u8;
        for _ in 0..w {
            out.push(l);
            out.push(l);
            out.push(l);
            out.push(255);
        }
    }
    out
}

/// Diagonal hatch — every 8 pixels the (x+y) slot drops to a darker
/// tone for ~2 pixels, producing thin diagonal lines.
fn gen_diagonal_hatch(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((w as usize) * (h as usize) * 4);
    for y in 0..h {
        for x in 0..w {
            let d = (x as i32 + y as i32).rem_euclid(8);
            let l: u8 = if d < 2 { 80 } else { 220 };
            out.push(l);
            out.push(l);
            out.push(l);
            out.push(255);
        }
    }
    out
}

/// Smooth vertical light → dark gradient. Top row fully bright,
/// bottom fully dark — gives text a metallic sheen feel when
/// combined with a slight tint.
fn gen_gradient_sweep(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((w as usize) * (h as usize) * 4);
    for y in 0..h {
        let t = if h <= 1 { 0.0 } else { y as f32 / (h - 1) as f32 };
        let l = (255.0 * (1.0 - t)).clamp(0.0, 255.0) as u8;
        for _ in 0..w {
            out.push(l);
            out.push(l);
            out.push(l);
            out.push(255);
        }
    }
    out
}

/// Dispatch by `TexturePresetId` → owned RGBA buffer + dimensions.
fn generate_preset_texture(
    id: klipp_timeline::TexturePresetId,
    w: u32,
    h: u32,
) -> (Vec<u8>, u32, u32) {
    use klipp_timeline::TexturePresetId as P;
    let buf = match id {
        P::PaperGrain => gen_paper_grain(w, h),
        P::Scanlines => gen_scanlines(w, h),
        P::DiagonalHatch => gen_diagonal_hatch(w, h),
        P::GradientSweep => gen_gradient_sweep(w, h),
    };
    (buf, w, h)
}

// ─── 3D Text Extrusion ──────────────────────────────────────────────────────

/// Render pseudo-3D text extrusion by drawing offset layers of the alpha mask.
#[allow(clippy::too_many_arguments)]
/// Standard "source over destination" alpha compositing on
/// straight-alpha 8-bit RGBA buffers, in-place into `dst`. Both
/// buffers must be the same size. The blend formula:
///
///   out_rgb   = src_rgb * src_a + dst_rgb * (1 - src_a)
///   out_alpha = src_a + dst_a * (1 - src_a)
///
/// Skips writes for fully-transparent source pixels — the common
/// case for the empty regions around each rasterised TextBox.
/// Apply a chromatic-aberration colour-channel split to a fully
/// composited RGBA buffer. The R channel is sourced from `(x - off, y)`,
/// G from `(x, y)`, B from `(x + off, y)`; the new alpha is the max
/// of the three sample alphas so the visible silhouette grows
/// naturally instead of clipping. Out-of-bounds samples clamp.
fn apply_chromatic_aberration(buf: &mut [u8], width: u32, height: u32, offset_px: f32) {
    let off = offset_px.round() as i32;
    if off == 0 || width == 0 || height == 0 {
        return;
    }
    let original = buf.to_vec();
    let w = width as i32;
    let h = height as i32;
    let stride = (w * 4) as usize;
    for y in 0..h {
        let row = y as usize * stride;
        for x in 0..w {
            let dst = row + (x * 4) as usize;
            let rx = (x - off).clamp(0, w - 1);
            let bx = (x + off).clamp(0, w - 1);
            let r_idx = (y * w + rx) as usize * 4;
            let g_idx = (y * w + x) as usize * 4;
            let b_idx = (y * w + bx) as usize * 4;
            buf[dst] = original[r_idx];
            buf[dst + 1] = original[g_idx + 1];
            buf[dst + 2] = original[b_idx + 2];
            buf[dst + 3] = original[r_idx + 3]
                .max(original[g_idx + 3])
                .max(original[b_idx + 3]);
        }
    }
}

/// Apply a horizontal-stripe glitch displacement to a fully composited
/// RGBA buffer. The buffer is sliced into 8-pixel stripes; each stripe
/// gets a deterministic ±N pixel x-offset (max N = ⌈intensity × 30⌉).
/// The seed is mixed with `time_seed` so each frame produces a fresh
/// pattern but the same `(intensity, time_seed)` pair always renders
/// identical pixels — no surprises on re-render or export.
fn apply_glitch(buf: &mut [u8], width: u32, height: u32, intensity: f32, time_seed: u32) {
    if intensity <= 0.001 || width == 0 || height == 0 {
        return;
    }
    const STRIPE_H: u32 = 8;
    let max_offset = ((intensity * 30.0).round() as i32).max(1);
    let original = buf.to_vec();
    let w = width as i32;
    let stride = (width * 4) as usize;
    let mut seed: u32 = 0x9E37_79B9u32
        .wrapping_mul(1664525)
        .wrapping_add(time_seed.wrapping_mul(2_654_435_761));
    let mut stripe_y = 0u32;
    while stripe_y < height {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let stripe_offset = ((seed >> 8) as i32).rem_euclid(2 * max_offset + 1) - max_offset;
        let stripe_end = (stripe_y + STRIPE_H).min(height);
        for y in stripe_y..stripe_end {
            let dst_row = y as usize * stride;
            for x in 0..w {
                let src_x = (x - stripe_offset).clamp(0, w - 1);
                let src_idx = (y as i32 * w + src_x) as usize * 4;
                let dst_idx = dst_row + (x * 4) as usize;
                buf[dst_idx] = original[src_idx];
                buf[dst_idx + 1] = original[src_idx + 1];
                buf[dst_idx + 2] = original[src_idx + 2];
                buf[dst_idx + 3] = original[src_idx + 3];
            }
        }
        stripe_y = stripe_end;
    }
}

/// Two-pass separable Gaussian blur on a single-channel coverage
/// buffer, expanding the canvas by `ceil(sigma * 2.5)` pixels on
/// every side so the diffused tail isn't clipped at the original
/// glyph bounds. Returns `(blurred, new_w, new_h)`.
///
/// `sigma` is in pixels; `< 0.5` short-circuits to a no-op. The
/// kernel is precomputed once per call (small allocation, cheap
/// even at 60 fps × 20-glyph titles).
fn gaussian_blur_expand(
    input: &[u8],
    in_w: usize,
    in_h: usize,
    sigma: f32,
) -> (Vec<u8>, usize, usize) {
    if sigma < 0.5 || in_w == 0 || in_h == 0 {
        return (input.to_vec(), in_w, in_h);
    }
    let radius = (sigma * 2.5).ceil().max(1.0) as usize;
    let kernel_size = 2 * radius + 1;
    let mut kernel = vec![0.0f32; kernel_size];
    let inv_2sigma2 = 1.0 / (2.0 * sigma * sigma);
    let mut sum = 0.0;
    for i in 0..kernel_size {
        let x = i as f32 - radius as f32;
        let v = (-x * x * inv_2sigma2).exp();
        kernel[i] = v;
        sum += v;
    }
    for k in &mut kernel { *k /= sum; }

    let new_w = in_w + 2 * radius;
    let new_h = in_h + 2 * radius;

    // Zero-pad the input into the expanded buffer's centre.
    let mut padded = vec![0u8; new_w * new_h];
    for y in 0..in_h {
        let dst_row = (y + radius) * new_w + radius;
        let src_row = y * in_w;
        padded[dst_row..dst_row + in_w].copy_from_slice(&input[src_row..src_row + in_w]);
    }

    // Horizontal pass into a float buffer, then vertical pass back to u8.
    let mut tmp = vec![0.0f32; new_w * new_h];
    for y in 0..new_h {
        for x in 0..new_w {
            let mut acc = 0.0;
            for (k, w) in kernel.iter().enumerate() {
                let dx = k as i32 - radius as i32;
                let sx = (x as i32 + dx).clamp(0, new_w as i32 - 1) as usize;
                acc += padded[y * new_w + sx] as f32 * w;
            }
            tmp[y * new_w + x] = acc;
        }
    }
    let mut out = vec![0u8; new_w * new_h];
    for y in 0..new_h {
        for x in 0..new_w {
            let mut acc = 0.0;
            for (k, w) in kernel.iter().enumerate() {
                let dy = k as i32 - radius as i32;
                let sy = (y as i32 + dy).clamp(0, new_h as i32 - 1) as usize;
                acc += tmp[sy * new_w + x] * w;
            }
            out[y * new_w + x] = acc.round().clamp(0.0, 255.0) as u8;
        }
    }
    (out, new_w, new_h)
}

/// True wrap-under drop cap layout. The body text is laid out in
/// two stages so it wraps to the right of the drop cap for the
/// cap's vertical extent, then re-flows at full width below it.
///
/// Algorithm:
///   1. Lay out the cap glyph at its enlarged size; measure its
///      width and height.
///   2. Lay out the body text in a narrow column to the right of
///      the cap (`max_width = full_max - cap_w`). Identify the
///      first glyph whose top exceeds `cap_h`.
///   3. Re-lay out the body text from that byte boundary at full
///      width. Translate those glyphs down by `cap_h` so they sit
///      below the drop cap.
///   4. Combine: cap glyphs + narrow body glyphs (shifted right by
///      cap_w) + wide body glyphs (shifted down by cap_h).
///
/// Returns a single Vec<GlyphPosition> with byte_offsets that point
/// into the FULL `cap_text + body_text` source string, so downstream
/// hit-testing and rich-text resolution work as if the layout had
/// been a single fontdue pass.
fn layout_drop_cap_wrap(
    font: &Font,
    cap_text: &str,
    body_text: &str,
    cap_size: f32,
    body_size: f32,
    full_max_w: f32,
) -> Vec<fontdue::layout::GlyphPosition> {
    let cap_byte_len = cap_text.len();

    // ── Stage 1: drop cap ──
    let mut cap_layout = Layout::new(CoordinateSystem::PositiveYDown);
    cap_layout.reset(&LayoutSettings {
        x: 0.0,
        y: 0.0,
        max_width: Some(full_max_w),
        max_height: None,
        horizontal_align: HorizontalAlign::Left,
        vertical_align: VerticalAlign::Top,
        wrap_style: WrapStyle::Word,
        ..LayoutSettings::default()
    });
    cap_layout.append(&[font], &TextStyle::new(cap_text, cap_size, 0));
    let cap_glyphs: Vec<fontdue::layout::GlyphPosition> = cap_layout.glyphs().to_vec();
    if cap_glyphs.is_empty() {
        // Defensive: no glyph for the first character (shouldn't
        // happen for a non-empty string + valid font, but if it
        // does, fall through to plain body layout).
        let mut body_layout = Layout::new(CoordinateSystem::PositiveYDown);
        body_layout.reset(&LayoutSettings {
            max_width: Some(full_max_w),
            horizontal_align: HorizontalAlign::Left,
            vertical_align: VerticalAlign::Top,
            wrap_style: WrapStyle::Word,
            ..LayoutSettings::default()
        });
        body_layout.append(&[font], &TextStyle::new(body_text, body_size, 0));
        return body_layout.glyphs().to_vec();
    }
    let cap_w = cap_glyphs.iter()
        .map(|g| g.x + g.width as f32)
        .fold(0.0f32, f32::max);
    let cap_h = cap_glyphs.iter()
        .map(|g| g.y + g.height as f32)
        .fold(0.0f32, f32::max);

    // Add a small horizontal gutter so the body doesn't kiss the
    // cap's right edge — half a body-em is the conventional spacing.
    let gutter = body_size * 0.25;
    let body_x_offset = cap_w + gutter;

    // ── Stage 2: narrow body to the right of the cap ──
    let narrow_w = (full_max_w - body_x_offset).max(body_size);
    let mut narrow_layout = Layout::new(CoordinateSystem::PositiveYDown);
    narrow_layout.reset(&LayoutSettings {
        x: 0.0,
        y: 0.0,
        max_width: Some(narrow_w),
        max_height: None,
        horizontal_align: HorizontalAlign::Left,
        vertical_align: VerticalAlign::Top,
        wrap_style: WrapStyle::Word,
        ..LayoutSettings::default()
    });
    narrow_layout.append(&[font], &TextStyle::new(body_text, body_size, 0));
    let narrow_glyphs: Vec<fontdue::layout::GlyphPosition> = narrow_layout.glyphs().to_vec();

    // Find the first glyph whose top exceeds cap_h — that's the
    // first body character that no longer fits beside the cap.
    let first_below = narrow_glyphs.iter().position(|g| g.y >= cap_h);
    let (in_column, byte_consumed) = match first_below {
        None => (narrow_glyphs.clone(), body_text.len()),
        Some(0) => (Vec::new(), 0),
        Some(idx) => {
            let last_kept = &narrow_glyphs[idx - 1];
            // byte_offset on GlyphPosition is the byte offset into
            // the appended segment (not into the full source). Since
            // we appended `body_text` as one segment, this lines up
            // 1:1 with body_text's byte indices.
            let consumed = last_kept.byte_offset + last_kept.parent.len_utf8();
            (narrow_glyphs[..idx].to_vec(), consumed)
        }
    };

    // ── Stage 3: full-width body below the cap ──
    let remaining_body = if byte_consumed < body_text.len() {
        &body_text[byte_consumed..]
    } else { "" };
    let wide_glyphs: Vec<fontdue::layout::GlyphPosition> = if remaining_body.is_empty() {
        Vec::new()
    } else {
        let mut wide_layout = Layout::new(CoordinateSystem::PositiveYDown);
        wide_layout.reset(&LayoutSettings {
            x: 0.0,
            y: 0.0,
            max_width: Some(full_max_w),
            max_height: None,
            horizontal_align: HorizontalAlign::Left,
            vertical_align: VerticalAlign::Top,
            wrap_style: WrapStyle::Word,
            ..LayoutSettings::default()
        });
        wide_layout.append(&[font], &TextStyle::new(remaining_body, body_size, 0));
        wide_layout.glyphs().to_vec()
    };

    // ── Combine ──
    // Cap glyphs keep their byte_offsets (point into cap_text, i.e.
    // bytes 0..cap_byte_len of the full source).
    let mut out: Vec<fontdue::layout::GlyphPosition> = cap_glyphs;
    // Narrow body glyphs: shift x by body_x_offset; rewrite
    // byte_offset to point into the full source by adding cap_byte_len.
    for mut g in in_column {
        g.x += body_x_offset;
        g.byte_offset += cap_byte_len;
        out.push(g);
    }
    // Wide body glyphs: shift y by cap_h; byte_offset points into
    // remaining_body, which started at `byte_consumed` in body_text,
    // so add cap_byte_len + byte_consumed for the absolute index.
    for mut g in wide_glyphs {
        g.y += cap_h;
        g.byte_offset += cap_byte_len + byte_consumed;
        out.push(g);
    }
    out
}




/// Multiply the alpha channel of every pixel in a layer by `factor`
/// (clamped to 0..1). Used to apply per-object opacity to a fully-
/// rendered layer before it gets `over_blend`-composited onto the
/// running buffer.
fn scale_layer_alpha(layer: &mut [u8], factor: f32) {
    use rayon::prelude::*;
    let f = factor.clamp(0.0, 1.0);
    if f >= 0.9999 {
        return;
    }
    layer.par_chunks_mut(4 * 4096).for_each(|chunk| {
        for px in chunk.chunks_exact_mut(4) {
            px[3] = ((px[3] as f32) * f).round().clamp(0.0, 255.0) as u8;
        }
    });
}

/// Glyphs a staggered motion poses one by one at most; beyond it the
/// frame is rastered. Every output pixel considers every glyph.
pub const MAX_POSED_GLYPHS: usize = 256;

/// Which unit each glyph belongs to for `unit`: the line, word or
/// non-space character it is in, counted as `motion_units` counts them.
pub fn units_of(parents: &[char], unit: klipp_timeline::StaggerUnit) -> Vec<usize> {
    use klipp_timeline::StaggerUnit as U;
    let mut out = Vec::with_capacity(parents.len());
    let (mut line, mut word, mut ch) = (0usize, 0usize, 0usize);
    let mut in_word = false;
    for &c in parents {
        if c == '\n' {
            line += 1;
            in_word = false;
            out.push(match unit { U::None => 0, U::Line => line, U::Word => word.saturating_sub(1), U::Char => ch });
            continue;
        }
        if c.is_whitespace() {
            in_word = false;
        } else if !in_word {
            in_word = true;
            word += 1;
        }
        out.push(match unit { U::None => 0, U::Line => line, U::Word => word.saturating_sub(1), U::Char => ch });
        if !c.is_whitespace() {
            ch += 1;
        }
    }
    out
}

/// Every glyph's pose at `time_secs` for a staggered motion: the
/// entrance and exit progress of the glyph's unit, the poses they
/// give, and the loop's offset and fade on top — as the per-glyph
/// rasteriser combines them.
fn glyph_poses(
    units: &UnitMap,
    motion: &klipp_timeline::TitleMotion,
    time_secs: f32,
    window: Option<f32>,
    font_px: f32,
    loop_pose: (f32, f32, f32),
    full_frame: bool,
) -> Vec<GlyphPose> {
    let enter_units = units_of(&units.parents, motion.enter.map(|m| m.stagger).unwrap_or_default());
    let exit_units = units_of(&units.parents, motion.exit.map(|m| m.stagger).unwrap_or_default());
    let rest = (1.0, 0.0, 0.0, 1.0, 0.0, false);
    let pad = units.pad as f32;
    units
        .boxes
        .iter()
        .enumerate()
        .map(|(g, b)| {
            let (pe, _) = motion.progress(time_secs, window, enter_units.get(g).copied().unwrap_or(0));
            let (_, px) = motion.progress(time_secs, window, exit_units.get(g).copied().unwrap_or(0));
            let (a1, dx1, dy1, s1, _, c1) = motion.enter.map(|m| m.pose(pe, font_px)).unwrap_or(rest);
            let (a2, dx2, dy2, s2, _, c2) = motion.exit.map(|m| m.pose(px, font_px)).unwrap_or(rest);
            let alpha = a1 * a2 * loop_pose.2;
            let (dx, dy) = (dx1 + dx2 + loop_pose.0, dy1 + dy2 + loop_pose.1);
            let scale = s1 * s2;
            let anchor = ((b[0] + b[2]) as f32 * 0.5, (b[1] + b[3]) as f32 * 0.5);
            let clip = (c1 || c2).then_some((b[1], b[3]));
            // The frame box the posed glyph, its effects round it
            // included, can touch.
            let grow = |v: i32, c: f32, sign: f32| c + (v as f32 + sign * pad - c) * scale;
            let mut moved = [
                (grow(b[0], anchor.0, -1.0) + dx).floor() as i32 - 1,
                (grow(b[1], anchor.1, -1.0) + dy).floor() as i32 - 1,
                (grow(b[2], anchor.0, 1.0) + dx).ceil() as i32 + 1,
                (grow(b[3], anchor.1, 1.0) + dy).ceil() as i32 + 1,
            ];
            if let Some((y0, y1)) = clip {
                moved[1] = moved[1].max(y0);
                moved[3] = moved[3].min(y1);
            }
            if b[2] <= b[0] || b[3] <= b[1] || (alpha <= 0.001 && !full_frame) {
                moved = [0, 0, 0, 0];
            }
            GlyphPose { dx, dy, scale, alpha, anchor, clip, moved }
        })
        .collect()
}

/// The frame a [`TitleFrame::Glyphs`] names, made on the CPU: each
/// glyph of the resting render, by the sprite's glyph map, moved,
/// scaled about its own box and faded by its pose, laid over the
/// glyphs before it; only within `rows`. The GPU glyph pass is its
/// twin.
pub fn pose_sprite_glyphs(rest: &[u8], width: u32, height: u32, units: &UnitMap, poses: &[GlyphPose], rows: (u32, u32)) -> Vec<u8> {
    use rayon::prelude::*;
    let (w, h) = (width as usize, height as usize);
    let mut out = vec![0u8; w * h * 4];
    if rest.len() < w * h * 4 || w == 0 {
        return out;
    }
    let (y0, y1) = ((rows.0 as usize).min(h), (rows.1 as usize).min(h));
    // A tap of glyph `g`: the sprite's pixel where the map says `g`,
    // premultiplied, in sRGB-encoded units; nothing elsewhere.
    let tap = |x: i64, y: i64, g: u16| -> [f32; 4] {
        if x < 0 || y < 0 || x >= w as i64 || y >= h as i64 || units.at(x, y) != g {
            return [0.0; 4];
        }
        let p = &rest[(y as usize * w + x as usize) * 4..][..4];
        let a = p[3] as f32 / 255.0;
        [p[0] as f32 * a, p[1] as f32 * a, p[2] as f32 * a, a]
    };
    out.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        if y < y0 || y >= y1 {
            return;
        }
        let yi = y as i32;
        for (x, px) in row.chunks_exact_mut(4).enumerate() {
            let xi = x as i32;
            let mut acc = [0.0f32; 4];
            for (g, p) in poses.iter().enumerate() {
                if xi < p.moved[0] || xi >= p.moved[2] || yi < p.moved[1] || yi >= p.moved[3] {
                    continue;
                }
                let fx = (x as f32 + 0.5 - p.dx - p.anchor.0) / p.scale + p.anchor.0 - 0.5;
                let fy = (y as f32 + 0.5 - p.dy - p.anchor.1) / p.scale + p.anchor.1 - 0.5;
                let (x0, yy0) = (fx.floor(), fy.floor());
                let (tx, ty) = (fx - x0, fy - yy0);
                let (x0, yy0) = (x0 as i64, yy0 as i64);
                let mut s = [0.0f32; 4];
                for (sx, sy, wgt) in [
                    (x0, yy0, (1.0 - tx) * (1.0 - ty)),
                    (x0 + 1, yy0, tx * (1.0 - ty)),
                    (x0, yy0 + 1, (1.0 - tx) * ty),
                    (x0 + 1, yy0 + 1, tx * ty),
                ] {
                    let t = tap(sx, sy, g as u16);
                    for k in 0..4 {
                        s[k] += t[k] * wgt;
                    }
                }
                if s[3] <= 0.0 {
                    continue;
                }
                // The glyph's alpha to whole levels, as a placed sprite's.
                let a8 = (s[3] * 255.0 * p.alpha + 0.5).floor().clamp(0.0, 255.0);
                if a8 <= 0.0 {
                    continue;
                }
                let a = a8 / 255.0;
                let rgb = [(s[0] / s[3] + 0.5).floor(), (s[1] / s[3] + 0.5).floor(), (s[2] / s[3] + 0.5).floor()];
                // Over the glyphs before it, premultiplied.
                for k in 0..3 {
                    acc[k] = rgb[k] * a + acc[k] * (1.0 - a);
                }
                acc[3] = a + acc[3] * (1.0 - a);
            }
            if acc[3] > 0.0 {
                px[0] = (acc[0] / acc[3]).round().clamp(0.0, 255.0) as u8;
                px[1] = (acc[1] / acc[3]).round().clamp(0.0, 255.0) as u8;
                px[2] = (acc[2] / acc[3]).round().clamp(0.0, 255.0) as u8;
                px[3] = (acc[3] * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
    });
    out
}

/// The frame a [`TitleFrame::Placed`] names, made on the CPU: the
/// resting render moved by `(dx, dy)`, scaled by `scale` about `anchor`
/// (bilinear, premultiplied), its alpha scaled by `opacity`, laid over
/// a full-frame `under` colour when there is one, and only within
/// `rows` (a mask motion shows the words inside their resting box).
/// The GPU sprite pass (`stack_layer_place_sprite_posed`) is its twin;
/// the parity tests hold the two to a level. With no scale, no `under`
/// and every row, it is `shift_and_fade`.
#[allow(clippy::too_many_arguments)]
pub fn pose_sprite(
    rest: &[u8],
    width: u32,
    height: u32,
    dx: i32,
    dy: i32,
    opacity: f32,
    scale: f32,
    anchor: (f32, f32),
    under: Option<[u8; 4]>,
    rows: (u32, u32),
) -> Vec<u8> {
    use rayon::prelude::*;
    let plain = (scale - 1.0).abs() < 1e-6;
    let (w, h) = (width as usize, height as usize);
    if plain && under.is_none() {
        let mut out = shift_and_fade(rest.to_vec(), width, height, dx, dy, opacity);
        let (y0, y1) = ((rows.0 as usize).min(h), (rows.1 as usize).min(h));
        out[..y0 * w * 4].fill(0);
        out[y1.max(y0) * w * 4..].fill(0);
        return out;
    }
    let mut out = vec![0u8; w * h * 4];
    if rest.len() < w * h * 4 || w == 0 {
        return out;
    }
    let (y0, y1) = ((rows.0 as usize).min(h), (rows.1 as usize).min(h));
    let f = opacity.clamp(0.0, 1.0);
    let under_lin = under.map(|c| [srgb_u8_to_linear(c[0]), srgb_u8_to_linear(c[1]), srgb_u8_to_linear(c[2]), c[3] as f32 / 255.0]);
    // A tap: the sprite's pixel, premultiplied, in sRGB-encoded units.
    let tap = |x: i64, y: i64| -> [f32; 4] {
        if x < 0 || y < 0 || x >= w as i64 || y >= h as i64 {
            return [0.0; 4];
        }
        let p = &rest[(y as usize * w + x as usize) * 4..][..4];
        let a = p[3] as f32 / 255.0;
        [p[0] as f32 * a, p[1] as f32 * a, p[2] as f32 * a, a]
    };
    out.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        if y < y0 || y >= y1 {
            return;
        }
        for (x, px) in row.chunks_exact_mut(4).enumerate() {
            let s = if plain {
                tap(x as i64 - dx as i64, y as i64 - dy as i64)
            } else {
                let fx = (x as f32 + 0.5 - dx as f32 - anchor.0) / scale + anchor.0 - 0.5;
                let fy = (y as f32 + 0.5 - dy as f32 - anchor.1) / scale + anchor.1 - 0.5;
                let (x0, y0) = (fx.floor(), fy.floor());
                let (tx, ty) = (fx - x0, fy - y0);
                let (x0, y0) = (x0 as i64, y0 as i64);
                let mut acc = [0.0f32; 4];
                for (sx, sy, wgt) in [
                    (x0, y0, (1.0 - tx) * (1.0 - ty)),
                    (x0 + 1, y0, tx * (1.0 - ty)),
                    (x0, y0 + 1, (1.0 - tx) * ty),
                    (x0 + 1, y0 + 1, tx * ty),
                ] {
                    let t = tap(sx, sy);
                    for k in 0..4 {
                        acc[k] += t[k] * wgt;
                    }
                }
                acc
            };
            let a = s[3];
            let a8 = (a * 255.0 * f + 0.5).floor().clamp(0.0, 255.0);
            // No colour under no alpha: what a tie in the sum rounds to
            // must not show as a colour difference.
            let rgb = if a > 0.0 && (plain || a8 > 0.0) { [s[0] / a, s[1] / a, s[2] / a] } else { [0.0; 3] };
            match under_lin {
                None => {
                    px[0] = rgb[0].round().clamp(0.0, 255.0) as u8;
                    px[1] = rgb[1].round().clamp(0.0, 255.0) as u8;
                    px[2] = rgb[2].round().clamp(0.0, 255.0) as u8;
                    px[3] = a8 as u8;
                }
                Some(u) => {
                    let sa = a8 / 255.0;
                    let sl = [srgb_to_linear_f(rgb[0] / 255.0), srgb_to_linear_f(rgb[1] / 255.0), srgb_to_linear_f(rgb[2] / 255.0)];
                    let oa = sa + u[3] * (1.0 - sa);
                    for k in 0..3 {
                        let c = if oa > 0.0 { (sl[k] * sa + u[k] * u[3] * (1.0 - sa)) / oa } else { 0.0 };
                        px[k] = (linear_to_srgb_f(c) * 255.0).round().clamp(0.0, 255.0) as u8;
                    }
                    px[3] = (oa * 255.0).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    });
    out
}

fn srgb_to_linear_f(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn linear_to_srgb_f(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

fn srgb_u8_to_linear(v: u8) -> f32 {
    srgb_to_linear_f(v as f32 / 255.0)
}

/// The resting render moved by `(dx, dy)` pixels and faded to
/// `opacity`: what a scroll, crawl, shake, blink or pulse frame is,
/// without rasterising again. Rows are copied in parallel; what moves
/// out of the frame is dropped, what moves in is empty.
fn shift_and_fade(rest: Vec<u8>, width: u32, height: u32, dx: i32, dy: i32, opacity: f32) -> Vec<u8> {
    use rayon::prelude::*;
    if opacity <= 0.001 {
        return vec![0u8; rest.len()];
    }
    if dx == 0 && dy == 0 {
        let mut out = rest;
        scale_layer_alpha(&mut out, opacity);
        return out;
    }
    let (w, h) = (width as usize, height as usize);
    let row_bytes = w * 4;
    let mut out = vec![0u8; rest.len()];
    let f = opacity.clamp(0.0, 1.0);
    out.par_chunks_mut(row_bytes).enumerate().for_each(|(y, row)| {
        let sy = y as i64 - dy as i64;
        if sy < 0 || sy >= h as i64 {
            return;
        }
        let src = &rest[sy as usize * row_bytes..(sy as usize + 1) * row_bytes];
        // Destination columns [dx0, dx1) come from source [dx0 - dx, dx1 - dx).
        let dx0 = dx.max(0) as usize;
        let dx1 = (w as i64 + (dx as i64).min(0)).max(0) as usize;
        if dx1 <= dx0 {
            return;
        }
        let sx0 = (dx0 as i64 - dx as i64) as usize;
        let n = dx1 - dx0;
        row[dx0 * 4..dx1 * 4].copy_from_slice(&src[sx0 * 4..(sx0 + n) * 4]);
        if f < 0.9999 {
            for px in row[dx0 * 4..dx1 * 4].chunks_exact_mut(4) {
                px[3] = ((px[3] as f32) * f).round().clamp(0.0, 255.0) as u8;
            }
        }
    });
    out
}

fn over_blend(dst: &mut [u8], src: &[u8]) {
    use rayon::prelude::*;
    debug_assert_eq!(dst.len(), src.len());
    debug_assert_eq!(dst.len() % 4, 0);
    dst.par_chunks_mut(4 * 4096).zip(src.par_chunks(4 * 4096)).for_each(|(dc, sc)| over_blend_chunk(dc, sc));
}

fn over_blend_chunk(dst: &mut [u8], src: &[u8]) {
    for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
        let sa = s[3] as f32 / 255.0;
        if sa <= 0.0 {
            continue;
        }
        let inv_sa = 1.0 - sa;
        let da = d[3] as f32 / 255.0;
        let out_a = sa + da * inv_sa;
        if out_a <= 0.0 {
            continue;
        }
        // Straight-alpha "over": the colour is the alpha-weighted mix,
        // divided back out of the result's alpha, so a layer over
        // nothing keeps its own colour at every edge pixel instead of
        // darkening where it is translucent.
        for i in 0..3 {
            let sc = s[i] as f32;
            let dc = d[i] as f32;
            d[i] = ((sc * sa + dc * da * inv_sa) / out_a).round().clamp(0.0, 255.0) as u8;
        }
        d[3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
    }
}

/// Paint a `Shape` layer onto `buf` using its `ObjectTransform`.
/// `transform.center` and `transform.size` are normalised
/// 0..1 frame-relative coords; this maps to absolute pixel rect on
/// the frame, then fills + strokes per the shape's parameters.
/// Rotation is currently ignored (the on-canvas editor in commit 3
/// will wire it through). Phase 1 supports only the rectangle
/// primitive; future shape kinds extend the inner match.
/// An image layer's pixels, straight-alpha RGBA.
pub struct DecodedImage {
    pub w: usize,
    pub h: usize,
    pub rgba: Vec<u8>,
}

impl DecodedImage {
    /// Bilinear sample at source pixel coordinates, edges clamped.
    fn sample(&self, u: f32, v: f32) -> [f32; 4] {
        let u = u.clamp(0.0, (self.w - 1) as f32);
        let v = v.clamp(0.0, (self.h - 1) as f32);
        let (x0, y0) = (u.floor() as usize, v.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(self.w - 1), (y0 + 1).min(self.h - 1));
        let (fx, fy) = (u - x0 as f32, v - y0 as f32);
        let px = |x: usize, y: usize| {
            let i = (y * self.w + x) * 4;
            [self.rgba[i] as f32, self.rgba[i + 1] as f32, self.rgba[i + 2] as f32, self.rgba[i + 3] as f32]
        };
        let (a, b, c, d) = (px(x0, y0), px(x1, y0), px(x0, y1), px(x1, y1));
        let mut out = [0.0; 4];
        for i in 0..4 {
            let top = a[i] + (b[i] - a[i]) * fx;
            let bottom = c[i] + (d[i] - c[i]) * fx;
            out[i] = top + (bottom - top) * fy;
        }
        out
    }
}

/// Paint an image layer into its box: the picture fitted, filled or
/// stretched, sampled bilinearly, composited over what is there.
fn paint_image_layer(
    buf: &mut [u8],
    img: &DecodedImage,
    fit: ImageFit,
    transform: &ObjectTransform,
    width: u32,
    height: u32,
) {
    if img.w == 0 || img.h == 0 {
        return;
    }
    let (w_f, h_f) = (width as f32, height as f32);
    let bw = (transform.size.x * w_f).max(1.0);
    let bh = (transform.size.y * h_f).max(1.0);
    let cx = transform.center.x * w_f;
    let cy = transform.center.y * h_f;
    let (iw, ih) = (img.w as f32, img.h as f32);
    let (dw, dh) = match fit {
        ImageFit::Stretch => (bw, bh),
        ImageFit::Contain => {
            let s = (bw / iw).min(bh / ih);
            (iw * s, ih * s)
        }
        ImageFit::Cover => {
            let s = (bw / iw).max(bh / ih);
            (iw * s, ih * s)
        }
    };
    let (left, top) = (cx - dw * 0.5, cy - dh * 0.5);
    let x0 = left.max(cx - bw * 0.5).floor().max(0.0) as i32;
    let y0 = top.max(cy - bh * 0.5).floor().max(0.0) as i32;
    let x1 = (cx + dw * 0.5).min(cx + bw * 0.5).ceil().min(w_f) as i32;
    let y1 = (cy + dh * 0.5).min(cy + bh * 0.5).ceil().min(h_f) as i32;
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    for y in y0..y1 {
        let v = ((y as f32 + 0.5) - top) / dh * ih - 0.5;
        for x in x0..x1 {
            let u = ((x as f32 + 0.5) - left) / dw * iw - 0.5;
            let src = img.sample(u, v);
            let sa = src[3] / 255.0;
            if sa <= 0.0 {
                continue;
            }
            let idx = ((y as usize) * (width as usize) + (x as usize)) * 4;
            let inv_sa = 1.0 - sa;
            let da = buf[idx + 3] as f32 / 255.0;
            let out_a = sa + da * inv_sa;
            for i in 0..3 {
                buf[idx + i] = (src[i] * sa + buf[idx + i] as f32 * inv_sa).round().clamp(0.0, 255.0) as u8;
            }
            buf[idx + 3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
}

/// Full-frame text keeps the historical five-percent margin. Explicit
/// boxes use their authored dimensions for wrapping and cursor layout.
fn text_box_extent(title: &TitleData, width: u32, height: u32) -> (f32, f32) {
    let size = title.objects.iter().find_map(|o| {
        matches!(o.content, TitleObjectContent::TextBox(_)).then_some(o.transform.size)
    }).unwrap_or(klipp_timeline::TitlePosition { x: 1.0, y: 1.0 });
    let extent = |v: f32, pixels: u32| {
        let fraction = if !v.is_finite() || v == 1.0 { 0.9 } else { v.max(0.0) };
        (fraction * pixels as f32).max(1.0)
    };
    (extent(size.x, width), extent(size.y, height))
}

fn paint_shape_layer(
    buf: &mut [u8],
    shape: &Shape,
    transform: &ObjectTransform,
    width: u32,
    height: u32,
    time_secs: f32,
) {
    if let ShapeKind::Path(path) = &shape.kind {
        vector_path::paint(buf, &path.at(time_secs as f64), transform, width, height);
        return;
    }
    let w_f = width as f32;
    let h_f = height as f32;
    let half_w = (transform.size.x * w_f * 0.5).max(0.0);
    let half_h = (transform.size.y * h_f * 0.5).max(0.0);
    let cx = transform.center.x * w_f;
    let cy = transform.center.y * h_f;
    let x0 = ((cx - half_w).floor() as i32).max(0);
    let y0 = ((cy - half_h).floor() as i32).max(0);
    let x1 = ((cx + half_w).ceil() as i32).min(width as i32);
    let y1 = ((cy + half_h).ceil() as i32).min(height as i32);
    if x1 <= x0 || y1 <= y0 {
        return;
    }

    match &shape.kind {
        ShapeKind::Path(_) => unreachable!("path handled above"),
        ShapeKind::Rectangle { corner_radius } => {
            let r = corner_radius.max(0.0);
            // Render-scale the corner radius the same way the
            // text renderer scales font_size, so a value tuned at
            // 1080p reads the same at 720p / 4K.
            let scaled_r = (r * (height as f32 / 1080.0)).max(0.0);
            let r2 = scaled_r * scaled_r;

            let fill = shape.fill;
            let stroke = shape.stroke;
            let stroke_w = stroke.map(|s| s.width).unwrap_or(0.0).max(0.0);
            let scaled_sw = stroke_w * (height as f32 / 1080.0);
            for y in y0..y1 {
                for x in x0..x1 {
                    // Distance from the rectangle's edge — used both
                    // for the rounded-corner mask and for stroke
                    // detection.
                    let in_rect = is_in_rounded_rect(
                        x as f32 + 0.5, y as f32 + 0.5,
                        cx - half_w, cy - half_h, cx + half_w, cy + half_h,
                        scaled_r, r2,
                    );
                    if !in_rect {
                        continue;
                    }
                    // Stroke band (inside the rect, near the edge).
                    let in_stroke = scaled_sw > 0.0 && {
                        let pad = scaled_sw;
                        !is_in_rounded_rect(
                            x as f32 + 0.5, y as f32 + 0.5,
                            cx - half_w + pad, cy - half_h + pad,
                            cx + half_w - pad, cy + half_h - pad,
                            (scaled_r - pad).max(0.0),
                            (scaled_r - pad).max(0.0).powi(2),
                        )
                    };
                    let pixel_color = if in_stroke {
                        stroke.map(|s| s.color)
                    } else {
                        fill
                    };
                    if let Some(c) = pixel_color {
                        // Composite this single pixel using "over".
                        let idx = ((y as usize) * (width as usize) + (x as usize)) * 4;
                        let one = [c[0], c[1], c[2], c[3]];
                        let mut tmp = [0u8; 4];
                        tmp.copy_from_slice(&buf[idx..idx + 4]);
                        let sa = one[3] as f32 / 255.0;
                        let inv_sa = 1.0 - sa;
                        let da = tmp[3] as f32 / 255.0;
                        let out_a = sa + da * inv_sa;
                        for i in 0..3 {
                            let sc = one[i] as f32;
                            let dc = tmp[i] as f32;
                            buf[idx + i] = (sc * sa + dc * inv_sa)
                                .round().clamp(0.0, 255.0) as u8;
                        }
                        buf[idx + 3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
                    }
                }
            }
        }
    }
}

/// Test whether a point lies inside a rounded rectangle defined by
/// its bounds and corner radius. The four corners use a circular
/// quadrant; the body is a plain rectangle inset by `r` on each
/// side. `r2` is `r * r` precomputed.
fn is_in_rounded_rect(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32, r2: f32) -> bool {
    if px < x0 || px > x1 || py < y0 || py > y1 {
        return false;
    }
    if r <= 0.0 {
        return true;
    }
    // Inside the rounded-rect except for the four corners.
    let near_left = px < x0 + r;
    let near_right = px > x1 - r;
    let near_top = py < y0 + r;
    let near_bot = py > y1 - r;
    if !(near_left || near_right) || !(near_top || near_bot) {
        return true;
    }
    // Corner: pick the matching quadrant centre and check distance.
    let cx = if near_left { x0 + r } else { x1 - r };
    let cy = if near_top { y0 + r } else { y1 - r };
    let dx = px - cx;
    let dy = py - cy;
    dx * dx + dy * dy <= r2
}

fn render_extrusion(
    buf: &mut [f32],
    alpha_mask: &[u8],
    mask_w: usize,
    mask_h: usize,
    mask_x0: usize,
    mask_y0: usize,
    buf_w: usize,
    buf_h: usize,
    ext: &klipp_timeline::TextExtrusion,
    title: &TitleData,
    time_secs: Option<f32>,
) {
    let tb = title.primary_text_or_default();
    let t_secs = time_secs.map(|t| t as f64).unwrap_or(0.0);
    let depth_eval = ext.evaluated_depth(t_secs);
    let angle_eval = ext.evaluated_angle(t_secs);
    let lighting_eval = ext.evaluated_lighting(t_secs);
    let depth = depth_eval.clamp(1.0, 50.0) as i32;
    let angle_rad = angle_eval.to_radians();
    let cos_a = angle_rad.cos();
    let sin_a = angle_rad.sin();

    // Determine face color (for exclusion — we don't draw the face here)
    let _face_color = if ext.face_color[3] > 0 { ext.face_color } else { tb.color };

    // Draw layers from back (farthest) to front (closest to face)
    for i in (1..=depth).rev() {
        let t = i as f32 / depth as f32; // 1.0 = farthest, approaching 0.0 = nearest
        let dx = (cos_a * i as f32).round() as i32;
        let dy = (sin_a * i as f32).round() as i32;

        // Lighting: darken back layers, lighten front layers
        let light_factor = 1.0 - lighting_eval * 0.5 * t; // back is darker
        let side_r = (ext.side_color[0] as f32 * light_factor).clamp(0.0, 255.0) as u8;
        let side_g = (ext.side_color[1] as f32 * light_factor).clamp(0.0, 255.0) as u8;
        let side_b = (ext.side_color[2] as f32 * light_factor).clamp(0.0, 255.0) as u8;
        let side_a = ext.side_color[3];
        let layer_color = [side_r, side_g, side_b, side_a];

        // Draw the offset mask
        for my in 0..mask_h {
            let dst_y = mask_y0 as i32 + my as i32 + dy;
            if dst_y < 0 || dst_y >= buf_h as i32 { continue; }
            for mx in 0..mask_w {
                let cov = alpha_mask[my * mask_w + mx];
                if cov == 0 { continue; }
                let dst_x = mask_x0 as i32 + mx as i32 + dx;
                if dst_x < 0 || dst_x >= buf_w as i32 { continue; }
                let alpha = (cov as u16 * side_a as u16 / 255) as u8;
                if alpha == 0 { continue; }
                let tinted = [layer_color[0], layer_color[1], layer_color[2], alpha];
                let idx = (dst_y as usize * buf_w + dst_x as usize) * 4;
                blend_pixel(buf, idx, &tinted);
            }
        }
    }
}

// ─── Effect padding ─────────────────────────────────────────────────────────

/// Compute the maximum padding needed around the text bounding box for effects.
fn effect_padding(title: &TitleData, scale: f32) -> i32 {
    let tb = title.primary_text_or_default();
    let mut pad = 0i32;
    for (i, outline) in tb.outlines.iter().enumerate().filter(|(_, o)| o.enabled) {
        // Mirror the renderer's stack gate so we don't reserve padding
        // for layers that won't draw.
        if i >= 1 && !tb.outline_stack_enabled {
            continue;
        }
        // Pad for the worst-case width across the clip — static value
        // OR any keyframe value, whichever is largest. A keyframed
        // outline that grows from 1px to 8px must reserve space for 8.
        let max_width = outline
            .width_keyframes
            .iter()
            .map(|k| k.value)
            .fold(outline.width, f32::max);
        pad = pad.max((max_width * scale).ceil() as i32 + 1);
    }
    // Glow stack — pad for the worst-case radius across every layer
    // that will draw. Each layer's worst case is the larger of its
    // static radius or any keyframe value.
    for (i, glow) in tb.glows.iter().enumerate().filter(|(_, g)| g.enabled) {
        if i >= 1 && !tb.glow_stack_enabled {
            continue;
        }
        let max_radius = glow
            .radius_keyframes
            .iter()
            .map(|k| k.value)
            .fold(glow.radius, f32::max);
        pad = pad.max((max_radius * scale * 2.0).ceil() as i32 + 1);
    }
    // Pad for the worst-case shadow extent across the entire stack —
    // each layer that will draw needs its own offset+blur reservation.
    for (i, shadow) in tb.shadows.iter().enumerate().filter(|(_, s)| s.enabled) {
        if i >= 1 && !tb.shadow_stack_enabled {
            continue;
        }
        let max_ox = shadow
            .offset_x_keyframes
            .iter()
            .map(|k| k.value.abs())
            .fold(shadow.offset_x.abs(), f32::max);
        let max_oy = shadow
            .offset_y_keyframes
            .iter()
            .map(|k| k.value.abs())
            .fold(shadow.offset_y.abs(), f32::max);
        let max_blur = shadow
            .blur_keyframes
            .iter()
            .map(|k| k.value)
            .fold(shadow.blur, f32::max);
        let sx = (max_ox * scale).ceil() as i32;
        let sy = (max_oy * scale).ceil() as i32;
        let sb = (max_blur * scale * 2.0).ceil() as i32;
        pad = pad.max(sx + sb + 1);
        pad = pad.max(sy + sb + 1);
    }
    // Inner shadow stack — pad worst-case extent for every layer
    // that will render. Inner shadow stays within the glyph mask, so
    // this pad mostly matters when the user keyframes blur way up.
    for (i, inner) in tb.inner_shadows.iter().enumerate().filter(|(_, s)| s.enabled) {
        if i >= 1 && !tb.inner_shadow_stack_enabled {
            continue;
        }
        let max_ox = inner
            .offset_x_keyframes
            .iter()
            .map(|k| k.value.abs())
            .fold(inner.offset_x.abs(), f32::max);
        let max_oy = inner
            .offset_y_keyframes
            .iter()
            .map(|k| k.value.abs())
            .fold(inner.offset_y.abs(), f32::max);
        let max_blur = inner
            .blur_keyframes
            .iter()
            .map(|k| k.value)
            .fold(inner.blur, f32::max);
        let sx = (max_ox * scale).ceil() as i32;
        let sy = (max_oy * scale).ceil() as i32;
        let sb = (max_blur * scale * 2.0).ceil() as i32;
        pad = pad.max(sx + sb + 1);
        pad = pad.max(sy + sb + 1);
    }
    // Extra padding for tracking: each glyph is pushed right by the
    // tracking times its index, so the text grows by the tracking times
    // the glyphs there are. This was the tracking times 200 whatever the
    // text: 600 px on every side of an 18-letter role line, and every
    // pass over the layer — its shadow's blur above all — ran over
    // sixty times the pixels, 72 ms a frame against 8.
    if tb.tracking > 0.0 {
        let glyphs = tb.text.chars().count().max(1) as f32;
        pad = pad.max((tb.tracking * scale * glyphs).ceil() as i32 + 2);
    }
    // Extra padding for extrusion depth — worst-case across the
    // clip. The horizontal/vertical extent depends on both depth and
    // angle, so we evaluate the diagonal length per (depth, angle)
    // pair across the static value AND every keyframe pair, taking
    // the largest. A purely-depth-keyframed effect uses static angle,
    // a purely-angle-keyframed effect uses static depth, and the
    // mixed case checks both vec products. Conservative — the actual
    // clip never exceeds this pad.
    if let Some(ext) = tb.extrusion.as_ref().filter(|e| e.enabled) {
        let max_depth = ext
            .depth_keyframes
            .iter()
            .map(|k| k.value)
            .fold(ext.depth, f32::max)
            .clamp(1.0, 50.0);
        // For angle, both extremes (min and max) can produce the
        // largest cos.abs() / sin.abs(), so collect candidate angles
        // from the static value and every keyframe.
        let mut angles: Vec<f32> = ext.angle_keyframes.iter().map(|k| k.value).collect();
        angles.push(ext.angle);
        let max_cos_abs = angles
            .iter()
            .map(|a| a.to_radians().cos().abs())
            .fold(0.0_f32, f32::max);
        let max_sin_abs = angles
            .iter()
            .map(|a| a.to_radians().sin().abs())
            .fold(0.0_f32, f32::max);
        let ex = (max_depth * max_cos_abs).ceil() as i32 + 1;
        let ey = (max_depth * max_sin_abs).ceil() as i32 + 1;
        pad = pad.max(ex);
        pad = pad.max(ey);
    }
    pad
}

// ─── Rich text (style runs) helpers ─────────────────────────────────────────

/// Resolve per-glyph fill colors from style runs.
/// Returns a Vec<[u8; 4]> with one color per glyph.
/// Uses a simple approach: each glyph's byte offset in the source text
/// determines which style run (if any) applies.
fn resolve_glyph_colors(
    title: &TitleData,
    glyphs: &[fontdue::layout::GlyphPosition],
    style_runs: &[StyleRun],
) -> Vec<[u8; 4]> {
    let base_color = title.color();
    glyphs
        .iter()
        .map(|g| {
            // GlyphPosition.byte_offset gives the byte offset into the source text
            let byte_off = g.byte_offset;
            // Find the style run containing this byte offset
            for run in style_runs {
                if byte_off >= run.start && byte_off < run.end {
                    return run.color.unwrap_or(base_color);
                }
            }
            base_color
        })
        .collect()
}

/// Build per-glyph color info and coverage bitmaps for rich text fill rendering.
/// This is used in the compose phase to draw each glyph with its own color.
fn build_glyph_color_info(
    font: &Font,
    glyphs: &[fontdue::layout::GlyphPosition],
    offset_x: f32,
    offset_y: f32,
    mask_x0: usize,
    mask_y0: usize,
    resolved_colors: &[[u8; 4]],
) -> (Vec<GlyphColorInfo>, Vec<Vec<u8>>) {
    let mut colors = Vec::with_capacity(glyphs.len());
    let mut coverages = Vec::with_capacity(glyphs.len());

    for (i, glyph) in glyphs.iter().enumerate() {
        if glyph.width == 0 || glyph.height == 0 {
            continue;
        }
        let (_, coverage) = font.rasterize_config(glyph.key);
        let gx = (glyph.x + offset_x) as i32 - mask_x0 as i32;
        let gy = (glyph.y + offset_y) as i32 - mask_y0 as i32;
        let color = if i < resolved_colors.len() {
            resolved_colors[i]
        } else {
            [255, 255, 255, 255]
        };
        colors.push(GlyphColorInfo {
            mask_x: gx,
            mask_y: gy,
            width: glyph.width,
            height: glyph.height,
            color,
        });
        coverages.push(coverage);
    }

    (colors, coverages)
}

// ─── Rasterization helpers ──────────────────────────────────────────────────

/// Rasterize fontdue glyphs into a single-channel alpha mask.
#[allow(clippy::too_many_arguments)]
fn rasterize_to_mask(
    font: &Font,
    glyphs: &[fontdue::layout::GlyphPosition],
    offset_x: f32,
    offset_y: f32,
    mask: &mut [u8],
    mask_w: usize,
    mask_h: usize,
    mask_x0: usize,
    mask_y0: usize,
    mut index: Option<&mut [u16]>,
) {
    for (gi, glyph) in glyphs.iter().enumerate() {
        if glyph.width == 0 || glyph.height == 0 {
            continue;
        }
        let (_, coverage) = font.rasterize_config(glyph.key);

        let gx = (glyph.x + offset_x) as i32;
        let gy = (glyph.y + offset_y) as i32;

        for row in 0..glyph.height {
            let py = gy + row as i32;
            let my = py - mask_y0 as i32;
            if my < 0 || my >= mask_h as i32 { continue; }
            for col in 0..glyph.width {
                let px = gx + col as i32;
                let mx = px - mask_x0 as i32;
                if mx < 0 || mx >= mask_w as i32 { continue; }
                let cov = coverage[row * glyph.width + col];
                if cov == 0 { continue; }
                let idx = my as usize * mask_w + mx as usize;
                // Max blend so overlapping glyphs don't exceed 255;
                // the pixel is the glyph that painted it darkest.
                if cov > mask[idx] {
                    if let Some(index) = index.as_deref_mut() {
                        index[idx] = gi.min(GLYPH_NONE as usize - 1) as u16;
                    }
                }
                mask[idx] = mask[idx].max(cov);
            }
        }
    }
}

/// Paint a solid alpha-255 horizontal stripe into the mask between
/// `(x0, y0)` and `(x1, y1)`, with bounds-checking. Used for
/// underline / strikethrough decoration so they share the rest of
/// the text rendering pipeline (outline / shadow / glow / texture
/// fill all naturally cover the stripe pixels).
fn paint_alpha_mask_stripe(
    mask: &mut [u8],
    mask_w: usize,
    mask_h: usize,
    x0: i32,
    x1: i32,
    y0: i32,
    y1: i32,
) {
    let x_lo = x0.max(0) as usize;
    let x_hi = (x1.min(mask_w as i32).max(0)) as usize;
    let y_lo = y0.max(0) as usize;
    let y_hi = (y1.min(mask_h as i32).max(0)) as usize;
    for y in y_lo..y_hi {
        let row_start = y * mask_w;
        for x in x_lo..x_hi {
            let idx = row_start + x;
            mask[idx] = 255;
        }
    }
}

/// Which stagger unit each glyph belongs to, in glyph order: the line,
/// word or character index, or 0 for everything at once.
fn motion_units(glyphs: &[fontdue::layout::GlyphPosition], unit: klipp_timeline::StaggerUnit) -> Vec<usize> {
    let parents: Vec<char> = glyphs.iter().map(|g| g.parent).collect();
    units_of(&parents, unit)
}

/// Rasterize the alpha mask with `TitleMotion` applied per glyph: the
/// entrance and exit poses at this time, eased and staggered by unit,
/// combined into one alpha, offset, scale and blur per glyph. Mask
/// kinds clip each glyph to its resting box so it appears to rise out
/// of an invisible edge.
#[allow(clippy::too_many_arguments)]
fn rasterize_to_mask_motion(
    font: &Font,
    glyphs: &[fontdue::layout::GlyphPosition],
    offset_x: f32,
    offset_y: f32,
    mask: &mut [u8],
    mask_w: usize,
    mask_h: usize,
    mask_x0: usize,
    mask_y0: usize,
    motion: &klipp_timeline::TitleMotion,
    time_secs: f32,
    base_font_px: f32,
    window_secs: Option<f32>,
    wave: Option<(f32, f32)>,
) {
    let enter_units = motion_units(glyphs, motion.enter.map(|m| m.stagger).unwrap_or_default());
    let exit_units = motion_units(glyphs, motion.exit.map(|m| m.stagger).unwrap_or_default());

    for (i, glyph) in glyphs.iter().enumerate() {
        if glyph.width == 0 || glyph.height == 0 {
            continue;
        }
        let (pe, _) = motion.progress(time_secs, window_secs, enter_units[i]);
        let (_, px) = motion.progress(time_secs, window_secs, exit_units[i]);
        let rest = (1.0, 0.0, 0.0, 1.0, 0.0, false);
        let (a1, dx1, dy1, s1, b1, c1) = motion.enter.map(|m| m.pose(pe, base_font_px)).unwrap_or(rest);
        let (a2, dx2, dy2, s2, b2, c2) = motion.exit.map(|m| m.pose(px, base_font_px)).unwrap_or(rest);
        let alpha = a1 * a2;
        if alpha <= 0.001 {
            continue;
        }
        let wave_dy = match wave {
            Some((amp, rate)) => (time_secs * rate * std::f32::consts::TAU + i as f32 * 0.5).sin() * amp,
            None => 0.0,
        };
        let dx = dx1 + dx2;
        let dy = dy1 + dy2 + wave_dy;
        let scale = s1 * s2;
        let sigma = b1.max(b2);
        let clip_to_rest = c1 || c2;

        let (coverage_base, base_w, base_h) = if (scale - 1.0).abs() > 0.001 {
            let scaled_px = (base_font_px * scale).max(1.0);
            let (m, cov) = font.rasterize(glyph.parent, scaled_px);
            (cov, m.width, m.height)
        } else {
            let (_, cov) = font.rasterize_config(glyph.key);
            (cov.to_vec(), glyph.width, glyph.height)
        };
        let (coverage, raw_w, raw_h) = if sigma > 0.05 {
            gaussian_blur_expand(&coverage_base, base_w, base_h, sigma)
        } else {
            (coverage_base, base_w, base_h)
        };

        // The resting box, for the mask kinds.
        // The resting box, exactly the rows the resting rasteriser
        // paints for this glyph.
        let rest_top = (glyph.y + offset_y) as i32;
        let rest_bottom = rest_top + glyph.height as i32;

        // Placed the way the resting rasteriser places a glyph (the
        // position truncated), with a scaled or blurred bitmap centred
        // on that box, so the frame after an entrance ends matches the
        // cached resting render to the pixel.
        let base_x = (glyph.x + offset_x + dx) as i32;
        let base_y = (glyph.y + offset_y + dy) as i32;
        let gx = base_x - (raw_w as i32 - glyph.width as i32) / 2;
        let gy = base_y - (raw_h as i32 - glyph.height as i32) / 2;

        for row in 0..raw_h {
            let py = gy + row as i32;
            if clip_to_rest && (py < rest_top || py >= rest_bottom) {
                continue;
            }
            let my = py - mask_y0 as i32;
            if my < 0 || my >= mask_h as i32 {
                continue;
            }
            for col in 0..raw_w {
                let px_ = gx + col as i32;
                let mx = px_ - mask_x0 as i32;
                if mx < 0 || mx >= mask_w as i32 {
                    continue;
                }
                let cov = coverage[row * raw_w + col];
                if cov == 0 {
                    continue;
                }
                let scaled_cov = (cov as f32 * alpha).round() as u8;
                if scaled_cov == 0 {
                    continue;
                }
                let idx = my as usize * mask_w + mx as usize;
                mask[idx] = mask[idx].max(scaled_cov);
            }
        }
    }
}

/// Draw an alpha mask onto an RGBA buffer using a given color and intensity.
#[allow(clippy::too_many_arguments)]
fn draw_mask_to_buf(
    buf: &mut [f32],
    mask: &[u8],
    mask_w: usize,
    mask_h: usize,
    mask_x0: usize,
    mask_y0: usize,
    buf_w: usize,
    buf_h: usize,
    color: &[u8; 4],
    intensity: f32,
) {
    for my in 0..mask_h {
        let py = mask_y0 + my;
        if py >= buf_h { continue; }
        for mx in 0..mask_w {
            let px = mask_x0 + mx;
            if px >= buf_w { continue; }
            let cov = mask[my * mask_w + mx];
            if cov == 0 { continue; }
            let alpha = (cov as f32 * color[3] as f32 * intensity / (255.0 * 1.0)).min(255.0) as u8;
            if alpha == 0 { continue; }
            let tinted = [color[0], color[1], color[2], alpha];
            let idx = (py * buf_w + px) * 4;
            blend_pixel(buf, idx, &tinted);
        }
    }
}

/// Draw rich text fill: each glyph is drawn with its own color.
#[allow(clippy::too_many_arguments)]
fn draw_rich_text_fill(
    buf: &mut [f32],
    buf_w: usize,
    buf_h: usize,
    mask_x0: usize,
    mask_y0: usize,
    _mask_w: usize,
    _mask_h: usize,
    glyph_colors: &[GlyphColorInfo],
    glyph_coverages: &[Vec<u8>],
) {
    for (gi, info) in glyph_colors.iter().enumerate() {
        let coverage = match glyph_coverages.get(gi) {
            Some(c) => c,
            None => continue,
        };
        let color = info.color;
        for row in 0..info.height {
            let py = mask_y0 as i32 + info.mask_y + row as i32;
            if py < 0 || py >= buf_h as i32 { continue; }
            for col in 0..info.width {
                let px = mask_x0 as i32 + info.mask_x + col as i32;
                if px < 0 || px >= buf_w as i32 { continue; }
                let cov = coverage[row * info.width + col];
                if cov == 0 { continue; }
                let alpha = (cov as u16 * color[3] as u16 / 255) as u8;
                if alpha == 0 { continue; }
                let tinted = [color[0], color[1], color[2], alpha];
                let idx = (py as usize * buf_w + px as usize) * 4;
                blend_pixel(buf, idx, &tinted);
            }
        }
    }
}

/// Offset an alpha mask by (dx, dy) pixels.
fn offset_alpha(mask: &[u8], w: usize, h: usize, dx: i32, dy: i32) -> Vec<u8> {
    let mut out = vec![0u8; w * h];
    for y in 0..h as i32 {
        let sy = y - dy;
        if sy < 0 || sy >= h as i32 { continue; }
        for x in 0..w as i32 {
            let sx = x - dx;
            if sx < 0 || sx >= w as i32 { continue; }
            out[y as usize * w + x as usize] = mask[sy as usize * w + sx as usize];
        }
    }
    out
}

/// Dilate an alpha mask by `radius` pixels using circular distance.
fn dilate_alpha(mask: &[u8], w: usize, h: usize, radius: f32) -> Vec<u8> {
    let r = radius.ceil() as i32;
    if r <= 0 { return mask.to_vec(); }
    let mut dilated = vec![0u8; w * h];
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let mut max_alpha = 0u8;
            for dy in -r..=r {
                let ny = y + dy;
                if ny < 0 || ny >= h as i32 { continue; }
                for dx in -r..=r {
                    let nx = x + dx;
                    if nx < 0 || nx >= w as i32 { continue; }
                    let dist_sq = dx * dx + dy * dy;
                    let r_sq = (radius * radius) as i32;
                    if dist_sq <= r_sq {
                        let a = mask[ny as usize * w + nx as usize];
                        if a > max_alpha { max_alpha = a; }
                    }
                }
            }
            dilated[y as usize * w + x as usize] = max_alpha;
        }
    }
    dilated
}

/// Blur an alpha mask using a 2-pass box blur approximation of gaussian (3 iterations).
fn blur_alpha(mask: &[u8], w: usize, h: usize, radius: f32) -> Vec<u8> {
    let r = radius.round() as usize;
    if r == 0 { return mask.to_vec(); }
    let mut src = mask.to_vec();
    // 3 passes of box blur for gaussian approximation
    for _ in 0..3 {
        let mut temp = vec![0u8; w * h];
        // Horizontal pass
        for y in 0..h {
            for x in 0..w {
                let mut sum = 0u32;
                let mut count = 0u32;
                let x0 = x.saturating_sub(r);
                let x1 = (x + r).min(w - 1);
                for dx in x0..=x1 {
                    sum += src[y * w + dx] as u32;
                    count += 1;
                }
                temp[y * w + x] = (sum / count) as u8;
            }
        }
        // Vertical pass
        for y in 0..h {
            for x in 0..w {
                let mut sum = 0u32;
                let mut count = 0u32;
                let y0 = y.saturating_sub(r);
                let y1 = (y + r).min(h - 1);
                for dy in y0..=y1 {
                    sum += temp[dy * w + x] as u32;
                    count += 1;
                }
                src[y * w + x] = (sum / count) as u8;
            }
        }
    }
    src
}

/// Draw a pattern fill through an alpha mask.
#[allow(clippy::too_many_arguments)]
fn draw_pattern_fill(
    buf: &mut [f32],
    mask: &[u8],
    mask_w: usize,
    mask_h: usize,
    mask_x0: usize,
    mask_y0: usize,
    buf_w: usize,
    buf_h: usize,
    pattern: &TextPattern,
    scale: f32,
) {
    for my in 0..mask_h {
        let py = mask_y0 + my;
        if py >= buf_h { continue; }
        for mx in 0..mask_w {
            let cov = mask[my * mask_w + mx];
            if cov == 0 { continue; }
            let px = mask_x0 + mx;
            if px >= buf_w { continue; }

            let color = pattern_color_at(pattern, px as f32, py as f32, scale);
            let alpha = (cov as u16 * color[3] as u16 / 255) as u8;
            if alpha == 0 { continue; }
            let tinted = [color[0], color[1], color[2], alpha];
            let idx = (py * buf_w + px) * 4;
            blend_pixel(buf, idx, &tinted);
        }
    }
}

// ─── Text-on-path helpers ───────────────────────────────────────────────────

/// Evaluate a point on the path at parameter t (0.0-1.0 normalized arc-length).
/// Returns (x, y, angle_radians) where angle is the tangent direction.
fn path_point(path: &TextPath, t: f32, total_advance: f32, center_x: f32, center_y: f32) -> (f32, f32, f32) {
    match path {
        TextPath::Arc { radius, start_angle, sweep } => {
            let angle = start_angle.to_radians() + sweep.to_radians() * t;
            let x = center_x + radius * angle.cos();
            let y = center_y + radius * angle.sin();
            let tangent_angle = angle + std::f32::consts::FRAC_PI_2;
            (x, y, tangent_angle)
        }
        TextPath::Circle { radius } => {
            let angle = 2.0 * std::f32::consts::PI * t;
            let x = center_x + radius * angle.cos();
            let y = center_y + radius * angle.sin();
            (x, y, angle + std::f32::consts::FRAC_PI_2)
        }
        TextPath::Wave { amplitude, frequency, phase } => {
            let linear_x = center_x - total_advance * 0.5 + total_advance * t;
            let wave_arg = frequency * 2.0 * std::f32::consts::PI * t + phase.to_radians();
            let y = center_y + amplitude * wave_arg.sin();
            let dy_dx = amplitude * frequency * 2.0 * std::f32::consts::PI * wave_arg.cos() / total_advance;
            let angle = dy_dx.atan();
            (linear_x, y, angle)
        }
    }
}

/// Blit a small source alpha buffer into a destination mask with rotation.
/// The glyph is centered at (cx, cy) in the destination and rotated by angle_rad.
#[allow(clippy::too_many_arguments)]
fn blit_rotated(
    dst: &mut [u8], dst_w: usize, dst_h: usize,
    src: &[u8], src_w: usize, src_h: usize,
    cx: f32, cy: f32,
    angle_rad: f32,
) {
    let cos_a = angle_rad.cos();
    let sin_a = angle_rad.sin();
    let half_w = src_w as f32 / 2.0;
    let half_h = src_h as f32 / 2.0;

    // Bounding radius of the rotated glyph
    let radius = (half_w * half_w + half_h * half_h).sqrt().ceil() as i32;
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let dst_x = cx as i32 + dx;
            let dst_y = cy as i32 + dy;
            if dst_x < 0 || dst_x >= dst_w as i32 || dst_y < 0 || dst_y >= dst_h as i32 {
                continue;
            }

            // Inverse-rotate to find source pixel
            let rx = dx as f32 * cos_a + dy as f32 * sin_a + half_w;
            let ry = -dx as f32 * sin_a + dy as f32 * cos_a + half_h;

            let sx = rx.round() as i32;
            let sy = ry.round() as i32;
            if sx >= 0 && sx < src_w as i32 && sy >= 0 && sy < src_h as i32 {
                let src_alpha = src[sy as usize * src_w + sx as usize];
                if src_alpha > 0 {
                    let dst_idx = dst_y as usize * dst_w + dst_x as usize;
                    dst[dst_idx] = dst[dst_idx].max(src_alpha);
                }
            }
        }
    }
}

/// Compute the fill color at a given pixel position for a pattern.
fn pattern_color_at(pattern: &TextPattern, x: f32, y: f32, scale: f32) -> [u8; 4] {
    match pattern {
        TextPattern::HorizontalStripes { color_a, color_b, stripe_width } => {
            let sw = (stripe_width * scale).max(1.0);
            let band = (y / sw) as i32;
            if band % 2 == 0 { *color_a } else { *color_b }
        }
        TextPattern::VerticalStripes { color_a, color_b, stripe_width } => {
            let sw = (stripe_width * scale).max(1.0);
            let band = (x / sw) as i32;
            if band % 2 == 0 { *color_a } else { *color_b }
        }
        TextPattern::Checkerboard { color_a, color_b, cell_size } => {
            let cs = (cell_size * scale).max(1.0);
            let cx = (x / cs) as i32;
            let cy = (y / cs) as i32;
            if (cx + cy) % 2 == 0 { *color_a } else { *color_b }
        }
        TextPattern::DiagonalStripes { color_a, color_b, stripe_width } => {
            let sw = (stripe_width * scale).max(1.0);
            let diag = (x + y) / sw;
            let band = diag.floor() as i32;
            if band % 2 == 0 { *color_a } else { *color_b }
        }
    }
}

#[inline]
fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t).round().clamp(0.0, 255.0) as u8
}

/// Alpha-blend `src` (RGBA) over the pixel at `buf[idx..idx+4]`.
#[inline]
/// One source pixel over the working buffer.
///
/// The source is still eight-bit — a title's colours come from the
/// document as bytes and nothing is gained by widening them — but what
/// it lands on is not. The old version did this in 1/255 integer units
/// and rounded the result back into a byte, which is correct for one
/// layer and lossy for five: a backing panel, a shadow, a glow, an
/// outline and the glyphs each read back what the last one had rounded.
/// The working buffer, rounded once, where it leaves the compositor.
fn to_bytes(buf: &[f32]) -> Vec<u8> {
    buf.iter().map(|v| v.round().clamp(0.0, 255.0) as u8).collect()
}

fn blend_pixel(buf: &mut [f32], idx: usize, src: &[u8; 4]) {
    let sa = src[3] as f32 / 255.0;
    if sa <= 0.0 {
        return;
    }
    if src[3] == 255 {
        buf[idx] = src[0] as f32;
        buf[idx + 1] = src[1] as f32;
        buf[idx + 2] = src[2] as f32;
        buf[idx + 3] = 255.0;
        return;
    }
    let da = buf[idx + 3] / 255.0;
    let inv_sa = 1.0 - sa;
    // Straight-alpha "over": the result's alpha, and the colour as the
    // alpha-weighted mix divided back out of it, so a translucent edge
    // keeps its colour instead of going dark.
    let out_a = sa + da * inv_sa;
    if out_a <= 0.0 {
        return;
    }
    for i in 0..3 {
        let sc = src[i] as f32;
        let dc = buf[idx + i];
        buf[idx + i] = ((sc * sa + dc * da * inv_sa) / out_a).clamp(0.0, 255.0);
    }
    buf[idx + 3] = (out_a * 255.0).clamp(0.0, 255.0);
}

/// Per-clip cache entry for [`ensure_glyph_positions`]. Holds the
/// fontdue layout pass output for one `(TitleData, frame_w,
/// frame_h)` combination. Recomputed when [`glyph_layout_hash`] no
/// longer matches the current input.
///
/// Used by both the inspector's visual letter picker (UI-side
/// cache) and the engine's glyph-anchored focal-point resolver
/// (render-side cache). The two consumers each own their own
/// `HashMap<ClipId, GlyphCacheEntry>` — different lifetimes, same
/// shape — and share the cache logic via this module.
#[derive(Debug, Clone)]
pub struct GlyphCacheEntry {
    pub layout_hash: u64,
    pub frame_w: u32,
    pub frame_h: u32,
    pub positions: Vec<CharPosition>,
}

/// Hash every [`TitleData`] field that affects glyph layout. Used as
/// the cache key for [`GlyphCacheEntry`] so position recomputation
/// fires when (and only when) one of these fields changes. Frame
/// dimensions are folded into the hash too — a 1920×1080 layout
/// differs from 1280×720 for the same `TitleData`.
///
/// **Layout-affecting fields included in the hash:**
///
/// * `text` (the source string)
/// * `font_name`, `font_style`, `font_size`
/// * `case_transform`, `alignment`
/// * `tracking`, `line_spacing`, `baseline_shift`
/// * `style_runs[*]` — only `start`, `end`, `font_size` (per-run
///   `font_name` and `font_style` are silently dropped by
///   [`TitleRenderer::compute_char_positions`] today; including
///   them in the hash would over-invalidate)
/// * `position()` — the title's frame-relative offset
/// * `frame_w`, `frame_h` — render resolution
///
/// **Deliberately excluded** (visual fill, doesn't move glyphs):
/// color, gradient, glow, outline, shadow, extrusion, motion blur,
/// character animation, texture fill. A colour-only edit must NOT
/// invalidate the cache or the picker recomputes wastefully on
/// every colour-drag frame.
pub fn glyph_layout_hash(td: &TitleData, frame_w: u32, frame_h: u32) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();

    let tb = td.primary_text_or_default();
    tb.text.hash(&mut h);
    tb.font_name.hash(&mut h);
    std::mem::discriminant(&tb.font_style).hash(&mut h);
    tb.font_size.to_bits().hash(&mut h);
    std::mem::discriminant(&tb.case_transform).hash(&mut h);
    std::mem::discriminant(&tb.alignment).hash(&mut h);
    tb.tracking.to_bits().hash(&mut h);
    tb.line_spacing.to_bits().hash(&mut h);
    tb.baseline_shift.to_bits().hash(&mut h);

    for run in &tb.style_runs {
        run.start.hash(&mut h);
        run.end.hash(&mut h);
        run.font_size.map(f32::to_bits).hash(&mut h);
    }

    let pos = td.position();
    pos.x.to_bits().hash(&mut h);
    pos.y.to_bits().hash(&mut h);

    let (box_w, box_h) = text_box_extent(td, frame_w, frame_h);
    box_w.to_bits().hash(&mut h);
    box_h.to_bits().hash(&mut h);

    frame_w.hash(&mut h);
    frame_h.hash(&mut h);

    h.finish()
}

/// Look up or refresh the glyph positions for a `TitleData`,
/// memoising results in `cache` keyed by `clip_id`. The first call
/// for a given `(clip_id, td, frame_w, frame_h)` runs the fontdue
/// layout pass (~3–8 ms typical); subsequent calls within the same
/// hash window return the cached slice (sub-µs).
///
/// `clip_id` is the cache key, not a constraint — caller decides
/// what counts as "the same title" (the inspector keys by the
/// title clip's id; the engine likewise). Two different consumers
/// pointing at the same TitleData under the same clip_id will hit
/// each other's cache entries; that's intentional.
///
/// Returns an empty slice if `td` is invalid in a way that
/// `compute_char_positions` returns nothing for (no visible glyphs
/// + no fallback). Callers handle that as "no glyphs to pick from."
pub fn ensure_glyph_positions<'a>(
    cache: &'a mut HashMap<klipp_core::ClipId, GlyphCacheEntry>,
    clip_id: klipp_core::ClipId,
    renderer: &mut TitleRenderer,
    td: &TitleData,
    frame_w: u32,
    frame_h: u32,
) -> &'a [CharPosition] {
    let target_hash = glyph_layout_hash(td, frame_w, frame_h);
    let needs_refresh = cache
        .get(&clip_id)
        .map(|e| e.layout_hash != target_hash || e.frame_w != frame_w || e.frame_h != frame_h)
        .unwrap_or(true);
    if needs_refresh {
        let positions = renderer.compute_char_positions(td, frame_w, frame_h);
        cache.insert(
            clip_id,
            GlyphCacheEntry {
                layout_hash: target_hash,
                frame_w,
                frame_h,
                positions,
            },
        );
    }
    &cache.get(&clip_id).expect("just inserted").positions
}

#[cfg(test)]
mod tests {
    /// A faint layer used to stall against an eight-bit buffer.
    ///
    /// Alpha 1/255 of white over a mid grey moves the result by about
    /// four tenths of a code value. Rounded back into a byte that is no
    /// move at all, so the second layer read the same grey the first
    /// one did, and a hundred of them left the pixel exactly where they
    /// found it. A title stacks its backing, shadow, glow, outline and
    /// glyphs this way, and the faint ends of each — the soft edge of a
    /// glow, the falloff of a shadow — are made of exactly these
    /// blends.
    #[test]
    fn faint_layers_accumulate_instead_of_stalling() {
        let mut buf = vec![100.0f32, 100.0, 100.0, 255.0];
        for _ in 0..30 {
            blend_pixel(&mut buf, 0, &[200, 200, 200, 1]);
        }
        let out = to_bytes(&buf);
        assert!(
            out[0] >= 108,
            "thirty faint layers should have carried the grey up from 100, \
             but it is {} — which is where eight bits used to leave it",
            out[0],
        );
        assert_eq!(out[3], 255, "opaque stays opaque");
    }

    /// The other end: one fully opaque layer still lands exactly on its
    /// own colour, with no drift from the float round trip.
    #[test]
    fn an_opaque_layer_is_exact() {
        let mut buf = vec![0.0f32; 4];
        blend_pixel(&mut buf, 0, &[17, 34, 51, 255]);
        assert_eq!(to_bytes(&buf), vec![17, 34, 51, 255]);
    }

    use super::*;
    use klipp_timeline::TitlePosition;

    #[test]
    fn renders_non_empty_frame() {
        let mut renderer = TitleRenderer::new();
        let title = TitleData::default();
        let buf = renderer.render(&title, 1920, 1080);
        assert_eq!(buf.len(), 1920 * 1080 * 4);
        // At least some pixels should be non-zero (the text)
        let nonzero = buf.iter().any(|&b| b != 0);
        assert!(nonzero, "Title render produced an all-zero frame");
    }

    #[test]
    fn white_text_has_correct_color() {
        let mut renderer = TitleRenderer::new();
        let title = {
    let tb = klipp_timeline::TextBox {
        text: "X".to_string(),
        font_size: 200.0,
        color: [255, 255, 255, 255],
        ..Default::default()
    };
    let mut td = klipp_timeline::TitleData::single_text(tb);
    if let Some(o) = td.objects.first_mut() { o.transform.center = TitlePosition { x: 0.5, y: 0.5 }; }
    td
};
        let buf = renderer.render(&title, 640, 480);
        // Find a fully opaque text pixel
        let has_white = buf.chunks_exact(4).any(|p| p[0] == 255 && p[3] == 255);
        assert!(has_white, "Expected fully white opaque pixels");
    }

    #[test]
    fn full_frame_background() {
        let mut renderer = TitleRenderer::new();
        let title = {
    let tb = klipp_timeline::TextBox {
        text: "BG".to_string(),
        ..Default::default()
    };
    let mut td = klipp_timeline::TitleData::single_text(tb);
    td.background = TitleBackground::FullFrame {
                color: [0, 0, 128, 255],
            };
    td
};
        let buf = renderer.render(&title, 320, 240);
        // Corner pixel should be the background color (no text there)
        assert_eq!(&buf[0..4], &[0, 0, 128, 255]);
    }

    #[test]
    fn a_colour_backing_is_painted_round_the_words() {
        let mut renderer = TitleRenderer::new();
        let mut td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "BG".to_string(), ..Default::default() });
        td.background = TitleBackground::Color { color: [0, 0, 0, 200], padding: 20.0 };
        let (w, h) = (320usize, 240usize);
        let buf = renderer.render(&td, w as u32, h as u32);
        // Dark, mostly opaque pixels that are no part of a white glyph:
        // the box. And it is a box round the text, not the whole frame.
        let boxed = buf.chunks_exact(4).filter(|p| p[3] >= 190 && p[0] < 20).count();
        assert!(boxed > 200, "the backing was not painted ({boxed} px)");
        assert_eq!(buf[3], 0, "the corner of the frame is outside it");
    }

    #[test]
    fn multi_object_composes_in_stack_order() {
        // Two-layer title: a bottom shape (red rectangle filling a
        // 100×100 region centred at frame centre) + a top text layer.
        // Verify the shape's red pixels actually land in the buffer
        // (proving the Shape path runs) and that the alpha-composite
        // doesn't blank them.
        let mut td = TitleData {
            objects: Vec::new(),
            next_object_id: 0,
            animation_type: klipp_timeline::TitleAnimationType::default(),
            background: TitleBackground::None,
        };
        let _shape_id = td.add_object(
            TitleObjectContent::Shape(klipp_timeline::Shape {
                kind: klipp_timeline::ShapeKind::Rectangle { corner_radius: 0.0 },
                fit: None,
                fill: Some([255, 0, 0, 255]),
                stroke: None,
            }),
            klipp_timeline::ObjectTransform {
                center: TitlePosition { x: 0.5, y: 0.5 },
                center_x_keyframes: Vec::new(),
                center_y_keyframes: Vec::new(),
                size: TitlePosition { x: 0.5, y: 0.5 },
                rotation: 0.0,
                anchor_norm: TitlePosition { x: 0.5, y: 0.5 },
            },
        );
        // Plain text on top. Use a short string so it doesn't
        // blanket the whole rectangle.
        let tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 60.0,
            color: [0, 255, 0, 255],
            ..Default::default()
        };
        let _text_id = td.add_object(
            TitleObjectContent::TextBox(tb),
            klipp_timeline::default_text_transform(),
        );

        let mut renderer = TitleRenderer::new();
        let buf = renderer.render(&td, 320, 200);
        // Must find at least one fully-opaque red pixel (shape) and
        // one fully-opaque green pixel (text). If multi-object
        // compositing fails, only one of the two would survive.
        let has_red = buf.chunks_exact(4)
            .any(|p| p[0] >= 200 && p[1] < 30 && p[2] < 30 && p[3] >= 200);
        let has_green = buf.chunks_exact(4)
            .any(|p| p[0] < 30 && p[1] >= 200 && p[2] < 30 && p[3] >= 200);
        assert!(has_red, "expected red pixels from the bottom shape layer");
        assert!(has_green, "expected green pixels from the top text layer");
    }

    #[test]
    fn hidden_object_is_skipped() {
        let mut td = TitleData::default();
        // Add a second TextBox with a unique colour, then hide it.
        // The visible-flag path must keep it out of the composite.
        let tb2 = klipp_timeline::TextBox {
            text: "MARK".to_string(),
            font_size: 80.0,
            color: [0, 0, 255, 255],
            ..Default::default()
        };
        let id2 = td.add_object(
            TitleObjectContent::TextBox(tb2),
            klipp_timeline::default_text_transform(),
        );
        if let Some(o) = td.object_mut(id2) {
            o.visible = false;
        }

        let mut renderer = TitleRenderer::new();
        let buf = renderer.render(&td, 320, 200);
        // The hidden TextBox uses pure blue. If it were rendered we'd
        // see fully-opaque blue pixels; with it hidden we shouldn't.
        let has_blue = buf.chunks_exact(4)
            .any(|p| p[0] < 30 && p[1] < 30 && p[2] >= 200 && p[3] >= 200);
        assert!(!has_blue, "hidden TextBox layer leaked into the render");
    }

    #[test]
    fn shadow_adds_extra_pixels() {
        let mut renderer = TitleRenderer::new();
        let no_shadow = {
    let mut tb = klipp_timeline::TextBox {
        text: "S".to_string(),
        font_size: 100.0,
        ..Default::default()
    };
    tb.shadows.clear();
    
    klipp_timeline::TitleData::single_text(tb)
};
        let with_shadow = {
    let mut td = no_shadow.clone();
    if let Some(tb) = td.primary_text_mut() {
        tb.shadows.push(klipp_timeline::TitleShadow {
            color: [0, 0, 0, 200],
            offset_x: 4.0,
            offset_y: 4.0,
            blur: 0.0,
            enabled: true,
            offset_x_keyframes: Vec::new(),
            offset_y_keyframes: Vec::new(),
            blur_keyframes: Vec::new(),
        });
    }
    td
};
        let buf1 = renderer.render(&no_shadow, 640, 480);
        let buf2 = renderer.render(&with_shadow, 640, 480);
        let count1 = buf1.chunks_exact(4).filter(|p| p[3] > 0).count();
        let count2 = buf2.chunks_exact(4).filter(|p| p[3] > 0).count();
        assert!(count2 > count1, "Shadow should add more non-transparent pixels");
    }

    #[test]
    fn outline_adds_extra_pixels() {
        let mut renderer = TitleRenderer::new();
        let no_outline = {
    let tb = klipp_timeline::TextBox {
        text: "O".to_string(),
        font_size: 100.0,
        ..Default::default()
    };
    
    klipp_timeline::TitleData::single_text(tb)
};
        let with_outline = {
    let mut td = no_outline.clone();
    if let Some(tb) = td.primary_text_mut() {
        tb.outlines.insert(0, klipp_timeline::TextOutline {
            color: [255, 0, 0, 255],
            width: 4.0,
            enabled: true,
            width_keyframes: Vec::new(),
        });
    }
    td
};
        let buf1 = renderer.render(&no_outline, 640, 480);
        let buf2 = renderer.render(&with_outline, 640, 480);
        let count1 = buf1.chunks_exact(4).filter(|p| p[3] > 0).count();
        let count2 = buf2.chunks_exact(4).filter(|p| p[3] > 0).count();
        assert!(count2 > count1, "Outline should add more non-transparent pixels");
    }

    #[test]
    fn glow_adds_extra_pixels() {
        let mut renderer = TitleRenderer::new();
        let no_glow = {
    let tb = klipp_timeline::TextBox {
        text: "G".to_string(),
        font_size: 100.0,
        ..Default::default()
    };
    
    klipp_timeline::TitleData::single_text(tb)
};
        let with_glow = {
    let mut td = no_glow.clone();
    if let Some(tb) = td.primary_text_mut() {
        tb.glows.push(klipp_timeline::TextGlow {
            color: [0, 255, 0, 255],
            radius: 10.0,
            intensity: 0.8,
            enabled: true,
            radius_keyframes: Vec::new(),
            intensity_keyframes: Vec::new(),
        });
    }
    td
};
        let buf1 = renderer.render(&no_glow, 640, 480);
        let buf2 = renderer.render(&with_glow, 640, 480);
        let count1 = buf1.chunks_exact(4).filter(|p| p[3] > 0).count();
        let count2 = buf2.chunks_exact(4).filter(|p| p[3] > 0).count();
        assert!(count2 > count1, "Glow should add more non-transparent pixels");
    }

    #[test]
    fn gradient_produces_color_variation() {
        let mut renderer = TitleRenderer::new();
        let title = {
    let tb = klipp_timeline::TextBox {
        text: "GRADIENT".to_string(),
        font_size: 100.0,
        gradient: Some(klipp_timeline::TextGradient {
            top_color: [255, 0, 0, 255],
            bottom_color: [0, 0, 255, 255],
            enabled: true,
        }),
        ..Default::default()
    };
    
    klipp_timeline::TitleData::single_text(tb)
};
        let buf = renderer.render(&title, 640, 480);
        // Should have both reddish and bluish pixels
        let has_red = buf.chunks_exact(4).any(|p| p[3] > 0 && p[0] > 100 && p[2] < 50);
        let has_blue = buf.chunks_exact(4).any(|p| p[3] > 0 && p[2] > 100 && p[0] < 50);
        assert!(has_red, "Gradient should produce red-ish pixels at top");
        assert!(has_blue, "Gradient should produce blue-ish pixels at bottom");
    }

    #[test]
    fn layout_phase_produces_alpha_mask() {
        let mut renderer = TitleRenderer::new();
        let title = TitleData::default();
        let layout = renderer.render_layout(&title, 1920, 1080);
        assert!(layout.glyph_count > 0, "Should have glyphs");
        assert!(!layout.alpha_mask.is_empty(), "Should have alpha mask");
        assert!(layout.mask_w > 0 && layout.mask_h > 0, "Mask dimensions should be nonzero");
        let has_coverage = layout.alpha_mask.iter().any(|&a| a > 0);
        assert!(has_coverage, "Alpha mask should have nonzero coverage");
    }

    #[test]
    fn extrusion_adds_extra_pixels() {
        let mut renderer = TitleRenderer::new();
        let no_ext = {
    let tb = klipp_timeline::TextBox {
        text: "3D".to_string(),
        font_size: 100.0,
        ..Default::default()
    };
    
    klipp_timeline::TitleData::single_text(tb)
};
        let with_ext = {
    let mut td = no_ext.clone();
    if let Some(tb) = td.primary_text_mut() {
        tb.extrusion = Some(klipp_timeline::TextExtrusion {
            depth: 10.0,
            angle: 135.0,
            face_color: [0, 0, 0, 0],
            side_color: [100, 50, 50, 255],
            lighting: 0.5,
            enabled: true,
            depth_keyframes: Vec::new(),
            angle_keyframes: Vec::new(),
            lighting_keyframes: Vec::new(),
        });
    }
    td
};
        let buf1 = renderer.render(&no_ext, 640, 480);
        let buf2 = renderer.render(&with_ext, 640, 480);
        let count1 = buf1.chunks_exact(4).filter(|p| p[3] > 0).count();
        let count2 = buf2.chunks_exact(4).filter(|p| p[3] > 0).count();
        assert!(count2 > count1, "Extrusion should add more non-transparent pixels");
    }

    #[test]
    fn test_char_positions_simple() {
        let mut renderer = TitleRenderer::new();
        let td = {
    let tb = klipp_timeline::TextBox {
        text: "Hello".to_string(),
        font_size: 72.0,
        ..Default::default()
    };
    
    klipp_timeline::TitleData::single_text(tb)
};
        let positions = renderer.compute_char_positions(&td, 1920, 1080);
        // Should have 6 entries: 5 chars + 1 end-of-text cursor position
        assert_eq!(positions.len(), 6, "Expected 6 positions (5 chars + end), got {}", positions.len());
        // char_indices should be sequential 0..=5
        for (i, cp) in positions.iter().enumerate() {
            assert_eq!(cp.char_index, i, "Position {} has wrong char_index: {}", i, cp.char_index);
        }
        // x positions should be monotonically increasing
        for i in 1..positions.len() {
            assert!(positions[i].x >= positions[i-1].x,
                "x not increasing: pos[{}].x={} < pos[{}].x={}", i, positions[i].x, i-1, positions[i-1].x);
        }
    }

    #[test]
    fn test_char_positions_multiline() {
        let mut renderer = TitleRenderer::new();
        let td = {
    let tb = klipp_timeline::TextBox {
        text: "Line1\nLine2".to_string(),
        font_size: 72.0,
        ..Default::default()
    };
    
    klipp_timeline::TitleData::single_text(tb)
};
        let positions = renderer.compute_char_positions(&td, 1920, 1080);
        let char_count = "Line1\nLine2".chars().count(); // 11
        assert_eq!(positions.len(), char_count + 1, "Expected {} positions, got {}", char_count + 1, positions.len());
        // Sequential char_indices
        for (i, cp) in positions.iter().enumerate() {
            assert_eq!(cp.char_index, i, "Position {} has wrong char_index: {} (byte_offset={})", i, cp.char_index, cp.byte_offset);
        }
        // Line2 chars should have higher y than Line1 chars
        let line1_y = positions[0].y;
        let line2_first = positions.iter().find(|p| p.char_index == 6).unwrap(); // 'L' of Line2
        assert!(line2_first.y > line1_y + 5.0,
            "Line2 y ({}) should be below Line1 y ({})", line2_first.y, line1_y);
    }

    /// Multi-column layout produces glyphs at distinct horizontal
    /// bands across the frame — proves the body actually flows into
    /// multiple side-by-side columns rather than into a single column.
    /// We assert the rendered bitmap has visible pixels at TWO
    /// non-overlapping horizontal regions (left half AND right half).
    #[test]
    fn columns_layout_distributes_glyphs_horizontally() {
        let mut renderer = TitleRenderer::new();
        let body = "Lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod \
                    tempor incididunt ut labore et dolore magna aliqua";
        let mk = |cols: u8| {
            let tb = klipp_timeline::TextBox {
                text: body.to_string(),
                font_size: 40.0,
                // Center alignment so the column block centres in the
                // frame and both columns fit inside the visible area
                // regardless of how wide the block ends up.
                alignment: klipp_timeline::TextAlignment::Center,
                columns: cols,
                ..Default::default()
            };
            klipp_timeline::TitleData::single_text(tb)
        };
        let buf = renderer.render(&mk(2), 1280, 720);
        // Two-column body centred in the frame should populate both
        // a "left of centre" band (200..560) and "right of centre"
        // band (720..1080) — the gap between is the column gutter.
        let mut left_count = 0usize;
        let mut right_count = 0usize;
        for (i, p) in buf.chunks_exact(4).enumerate() {
            if p[3] > 50 {
                let x = (i as u32) % 1280;
                if (200..560).contains(&x) { left_count += 1; }
                else if (720..1080).contains(&x) { right_count += 1; }
            }
        }
        assert!(left_count > 50, "Two-column body should populate left band. left={left_count}");
        assert!(right_count > 50, "Two-column body should populate right band. right={right_count}");
    }

    /// Vertical layout stacks each character on its own line, giving
    /// a much taller and narrower footprint than horizontal flow.
    #[test]
    fn vertical_layout_stacks_characters() {
        let mut renderer = TitleRenderer::new();
        let mk = |vertical: bool| {
            let tb = klipp_timeline::TextBox {
                text: "VERTICAL".to_string(),
                font_size: 60.0,
                vertical_layout: vertical,
                ..Default::default()
            };
            klipp_timeline::TitleData::single_text(tb)
        };
        let horizontal = renderer.render_layout(&mk(false), 1280, 720);
        let vertical = renderer.render_layout(&mk(true), 1280, 720);
        // Vertical: tall + narrow; horizontal: short + wide.
        assert!(
            vertical.mask_h > horizontal.mask_h,
            "Vertical layout should be taller. horizontal.mask_h={} vertical.mask_h={}",
            horizontal.mask_h, vertical.mask_h
        );
        assert!(
            vertical.mask_w < horizontal.mask_w,
            "Vertical layout should be narrower. horizontal.mask_w={} vertical.mask_w={}",
            horizontal.mask_w, vertical.mask_w
        );
    }

    /// RTL reverses character order — "ABC" rendered LTR vs RTL
    /// should produce a different bitmap (the A and C glyphs swap
    /// horizontal positions).
    #[test]
    fn rtl_reverses_visible_character_order() {
        let mut renderer = TitleRenderer::new();
        let mk = |dir: klipp_timeline::TextDirection| {
            let tb = klipp_timeline::TextBox {
                text: "ABC".to_string(),
                font_size: 80.0,
                text_direction: dir,
                ..Default::default()
            };
            klipp_timeline::TitleData::single_text(tb)
        };
        let ltr = renderer.render(&mk(klipp_timeline::TextDirection::Ltr), 1280, 720);
        let rtl = renderer.render(&mk(klipp_timeline::TextDirection::Rtl), 1280, 720);
        assert_ne!(&ltr[..], &rtl[..],
            "RTL must produce different pixels than LTR for asymmetric text");
    }

    /// Chromatic aberration must change the rendered pixels —
    /// applying a non-zero offset_px must alter the output relative
    /// to no-CA. The CA-rendered text has red fringing where it
    /// previously had the resting colour.
    #[test]
    fn chromatic_aberration_alters_output() {
        let mut renderer = TitleRenderer::new();
        let mk = |ca_px: f32| {
            let tb = klipp_timeline::TextBox {
                text: "ABC".to_string(),
                font_size: 80.0,
                color: [255, 255, 255, 255],
                chromatic_aberration_px: ca_px,
                ..Default::default()
            };
            klipp_timeline::TitleData::single_text(tb)
        };
        let plain = renderer.render(&mk(0.0), 1280, 720);
        let with_ca = renderer.render(&mk(8.0), 1280, 720);
        assert_ne!(&plain[..], &with_ca[..],
            "Chromatic aberration must modify the rendered pixels");
        // CA must produce some pixels with mostly-red content (R > 200,
        // G < 50, B < 50) — the red-shifted left fringe of the white text.
        let red_pixels = with_ca.chunks_exact(4)
            .filter(|p| p[0] > 200 && p[1] < 100 && p[2] < 100 && p[3] > 50)
            .count();
        assert!(red_pixels > 50,
            "CA on white text should produce ≥50 red-fringe pixels, got {red_pixels}");
    }

    /// Glitch must produce different pixel layouts at different time
    /// seeds — the same clip with intensity > 0 must shift its
    /// stripes per frame.
    #[test]
    fn glitch_produces_different_frames_over_time() {
        let mut renderer = TitleRenderer::new();
        let tb = klipp_timeline::TextBox {
            text: "GLITCH".to_string(),
            font_size: 80.0,
            glitch_intensity: 0.5,
            ..Default::default()
        };
        let td = klipp_timeline::TitleData::single_text(tb);

        let frame_a = renderer.render_with_time(&td, 1280, 720, None, 0.0);
        let frame_b = renderer.render_with_time(&td, 1280, 720, None, 0.5);
        assert_ne!(&frame_a[..], &frame_b[..],
            "Glitch should produce different stripe patterns at different times");
    }

    /// OutlineDrawOn must clip the rendered outline horizontally
    /// during the draw-on window, so partial-progress renders show
    /// fewer outline pixels than fully-drawn renders.
    #[test]
    fn outline_draw_on_clips_outline_horizontally() {
        let mut renderer = TitleRenderer::new();
        let mut tb = klipp_timeline::TextBox {
            text: "ABCDE".to_string(),
            font_size: 80.0,
            ..Default::default()
        };
        tb.outlines.push(klipp_timeline::TextOutline {
            color: [255, 0, 0, 255],
            width: 4.0,
            enabled: true,
            width_keyframes: Vec::new(),
        });
        let td = klipp_timeline::TitleData::single_text(tb);

        let count_red = |buf: &[u8]| -> usize {
            buf.chunks_exact(4)
                .filter(|p| p[0] > 200 && p[1] < 80 && p[2] < 80 && p[3] > 50)
                .count()
        };

        // Animation: half-drawn vs fully-drawn.
        let mut td_half = td.clone();
        if let Some(tb) = td_half.primary_text_mut() {
            tb.outline_write_on = 0.5;
        }
        let buf_half = renderer.render_with_time(&td_half, 1280, 720, None, 0.5);
        let buf_full = renderer.render_with_time(&td, 1280, 720, None, 2.0);

        let n_half = count_red(&buf_half);
        let n_full = count_red(&buf_full);
        assert!(
            n_half < n_full,
            "Mid-progress outline should have FEWER red pixels than fully-drawn. half={n_half} full={n_full}"
        );
        assert!(n_half > 0, "Mid-progress should still show SOME outline pixels, got {n_half}");
    }

    /// ColorBlend must produce different pixel colours at different
    /// times — the t=0 pose (mix=0.5 → midway between A and B) must
    /// differ from the t=π/2-rate pose (mix=1.0 → pure B).
    #[test]
    fn color_blend_produces_distinct_colours_over_time() {
        let mut renderer = TitleRenderer::new();
        let tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 80.0,
            ..Default::default()
        };
        let mut td = klipp_timeline::TitleData::single_text(tb);
        td.primary_text_mut().unwrap().loops.push(klipp_timeline::TitleLoop::ColorBlend {
            color_a: [255, 0, 0, 255],
            color_b: [0, 0, 255, 255],
            rate_hz: 1.0,
        });
        let frame_t0 = renderer.render_with_time(&td, 1280, 720, None, 0.0);
        let frame_t_quarter = renderer.render_with_time(&td, 1280, 720, None, 0.25); // peak at sin = 1, mix = 1.0 → pure B
        let first_visible_rgb = |buf: &[u8]| -> Option<[u8; 3]> {
            buf.chunks_exact(4)
                .find(|p| p[3] > 200)
                .map(|p| [p[0], p[1], p[2]])
        };
        let c0 = first_visible_rgb(&frame_t0).expect("t=0 has glyph pixels");
        let c1 = first_visible_rgb(&frame_t_quarter).expect("t=0.25 has glyph pixels");
        assert_ne!(c0, c1, "ColorBlend should produce different colours at different times");
    }

    /// Wrap-under drop cap: with alignment=Left, a body long enough
    /// to overflow the narrow column past cap height MUST re-flow
    /// onto a full-width band below the cap. Pinned by checking
    /// that the rendered mask is taller than the cap alone — the
    /// failing pre-fix behaviour was a `drop_cap_active` path that
    /// only enlarged the first character without ever flowing the
    /// rest of the body below it.
    #[test]
    fn drop_cap_wrap_lays_body_below_at_full_width() {
        let mut renderer = TitleRenderer::new();
        let body_text = "Lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod \
                         tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim \
                         veniam quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea \
                         commodo consequat duis aute irure dolor in reprehenderit in voluptate \
                         velit esse cillum dolore eu fugiat nulla pariatur excepteur sint occaecat";
        let mk = |scale: f32| {
            let tb = klipp_timeline::TextBox {
                text: body_text.to_string(),
                font_size: 50.0,
                alignment: klipp_timeline::TextAlignment::Left,
                drop_cap_scale: scale,
                ..Default::default()
            };
            klipp_timeline::TitleData::single_text(tb)
        };
        let no_cap = renderer.render_layout(&mk(0.0), 1280, 720);
        let with_wrap = renderer.render_layout(&mk(3.0), 1280, 720);
        assert!(no_cap.mask_h > 0);
        assert!(with_wrap.mask_h > 0);
        // The wrap-under variant must add the cap's vertical extent
        // on top of the body — the resulting mask is materially
        // taller than the no-cap layout.
        assert!(
            with_wrap.mask_h > no_cap.mask_h,
            "Wrap-under drop cap should grow the mask vs. no-cap. with_wrap.mask_h={} no_cap.mask_h={}",
            with_wrap.mask_h, no_cap.mask_h
        );
    }

    /// Drop cap must enlarge the first character in the rendered
    /// output — the alpha mask should grow taller (the drop cap
    /// extends above the body's cap height).
    #[test]
    fn drop_cap_enlarges_first_character() {
        let mut renderer = TitleRenderer::new();
        let mk = |scale: f32| {
            let tb = klipp_timeline::TextBox {
                text: "Lorem ipsum".to_string(),
                font_size: 50.0,
                drop_cap_scale: scale,
                ..Default::default()
            };
            klipp_timeline::TitleData::single_text(tb)
        };
        let plain = renderer.render_layout(&mk(0.0), 1920, 1080);
        let with_cap = renderer.render_layout(&mk(3.0), 1920, 1080);
        // The drop-capped layout should have a taller mask (taller
        // first char extends below the body baseline) and more
        // visible coverage than the plain version.
        assert!(
            with_cap.mask_h > plain.mask_h,
            "Drop cap should enlarge the layout mask. plain.mask_h={} with_cap.mask_h={}",
            plain.mask_h, with_cap.mask_h
        );
        // Below 1.0 disables — equivalent to no drop cap.
        let off = renderer.render_layout(&mk(0.5), 1920, 1080);
        assert_eq!(off.mask_h, plain.mask_h, "drop_cap_scale<1.0 should disable");
    }

    /// ColorCycle must change the rendered colour over time even
    /// without char_animation present — pins the contract that
    /// title-level hue rotation actually flows through render_with_time.
    #[test]
    fn color_cycle_changes_rendered_pixels_over_time() {
        let mut renderer = TitleRenderer::new();
        let tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 80.0,
            color: [255, 0, 0, 255], // pure red resting hue
            ..Default::default()
        };
        let mut td = klipp_timeline::TitleData::single_text(tb);
        td.primary_text_mut().unwrap().loops.push(klipp_timeline::TitleLoop::ColorCycle { rate_hz: 1.0 });

        let frame_t0 = renderer.render_with_time(&td, 1280, 720, None, 0.0);
        let frame_t_quarter = renderer.render_with_time(&td, 1280, 720, None, 0.25); // 90° hue shift

        // Find first non-transparent pixel in each frame and compare
        // its RGB. After 90° hue rotation from red the dominant
        // channel must shift away from R toward G.
        let first_visible_rgb = |buf: &[u8]| -> Option<[u8; 3]> {
            buf.chunks_exact(4)
                .find(|p| p[3] > 200)
                .map(|p| [p[0], p[1], p[2]])
        };
        let c0 = first_visible_rgb(&frame_t0).expect("t=0 has glyph pixels");
        let c1 = first_visible_rgb(&frame_t_quarter).expect("t=0.25 has glyph pixels");
        assert_ne!(c0, c1, "ColorCycle must produce different fill colours at different times. c0={c0:?} c1={c1:?}");
    }

    /// True Gaussian BlurIn must diffuse the glyph's footprint
    /// outward during entrance — the bounding box of any-non-zero
    /// pixels at mid-entrance must be wider than at settled t=1.0
    /// (proves the blur extends beyond the natural glyph extent).
    #[test]
    fn gaussian_blur_in_diffuses_glyph_during_entrance() {
        let mut renderer = TitleRenderer::new();
        let tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 100.0,
            motion: klipp_timeline::TitleMotion::enter(klipp_timeline::Motion::new(klipp_timeline::MotionKind::Blur).duration(0.4)),
            ..Default::default()
        };
        let td = klipp_timeline::TitleData::single_text(tb);

        // local_t at t=0.2 = 0.5 → mid-entrance, decent alpha. The
        // frame is the resting sprite placed, and the blur it asks for
        // is the compositor's to apply (a Gaussian on the layer, on the
        // GPU or the CPU alike), so the renderer reports the sigma.
        let mid = renderer.frame_with_window(&td, 1280, 720, None, 0.20, Some(4.0));
        let TitleFrame::Placed { blur, opacity, .. } = mid else { panic!("a blur entrance is the sprite placed") };
        assert!(blur > 0.5, "mid-entrance the frame asks for a blur: sigma {blur}");
        assert!(opacity > 0.3 && opacity < 1.0, "mid-entrance the sprite is faded: {opacity}");
        assert!(renderer.last_blur() > 0.5);
        let rest = renderer.frame_with_window(&td, 1280, 720, None, 10.0, Some(20.0));
        let TitleFrame::Placed { blur, .. } = rest else { panic!("at rest: the sprite placed") };
        assert_eq!(blur, 0.0, "at rest nothing is blurred");
        assert_eq!(renderer.last_blur(), 0.0);
    }

    /// Per-object opacity multiplies into the rendered alpha.
    #[test]
    fn per_object_opacity_reduces_alpha_proportionally() {
        let mut renderer = TitleRenderer::new();
        let tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 80.0,
            ..Default::default()
        };
        let td_full = {
            
            klipp_timeline::TitleData::single_text(tb.clone())
        };
        let td_half = {
            let mut td = klipp_timeline::TitleData::single_text(tb.clone());
            if let Some(o) = td.objects.first_mut() { o.opacity = 0.5; }
            td
        };
        let buf_full = renderer.render(&td_full, 1280, 720);
        let buf_half = renderer.render(&td_half, 1280, 720);

        // Sum the alpha channel — half-opacity object should produce
        // approximately half the total alpha of the full-opacity one.
        let alpha_sum = |buf: &[u8]| -> u64 {
            buf.chunks_exact(4).map(|p| p[3] as u64).sum()
        };
        let a_full = alpha_sum(&buf_full);
        let a_half = alpha_sum(&buf_half);
        let ratio = (a_half as f32) / (a_full as f32);
        assert!((ratio - 0.5).abs() < 0.05,
            "Alpha sum ratio at 0.5 opacity should be ~0.5, got {ratio}. full={a_full} half={a_half}");
    }

    /// Underline / strikethrough must add visible pixels to the
    /// rendered output beyond what plain text alone produces.
    #[test]
    fn underline_and_strikethrough_add_visible_pixels() {
        let mut renderer = TitleRenderer::new();
        let mk = |underline: bool, strike: bool| {
            let tb = klipp_timeline::TextBox {
                text: "abc".to_string(),
                font_size: 80.0,
                underline,
                strikethrough: strike,
                ..Default::default()
            };
            klipp_timeline::TitleData::single_text(tb)
        };
        let count_nonzero = |buf: &[u8]| buf.chunks_exact(4).filter(|p| p[3] > 0).count();
        let plain = renderer.render(&mk(false, false), 1280, 720);
        let with_underline = renderer.render(&mk(true, false), 1280, 720);
        let with_strike = renderer.render(&mk(false, true), 1280, 720);
        let n_plain = count_nonzero(&plain);
        let n_under = count_nonzero(&with_underline);
        let n_strike = count_nonzero(&with_strike);
        assert!(
            n_under > n_plain + 50,
            "Underline must add ≥50 visible pixels. plain={n_plain} under={n_under}"
        );
        assert!(
            n_strike > n_plain + 50,
            "Strikethrough must add ≥50 visible pixels. plain={n_plain} strike={n_strike}"
        );
    }

    /// Case transform changes the rendered glyphs — uppercase `abc`
    /// uses different glyphs than lowercase `abc`, so the alpha mask
    /// pattern must differ.
    #[test]
    fn case_transform_changes_rendered_text() {
        let mut renderer = TitleRenderer::new();
        let mk = |case: klipp_timeline::TextCase| {
            let tb = klipp_timeline::TextBox {
                text: "abc".to_string(),
                font_size: 80.0,
                case_transform: case,
                ..Default::default()
            };
            klipp_timeline::TitleData::single_text(tb)
        };
        let lower = renderer.render(&mk(klipp_timeline::TextCase::None), 1280, 720);
        let upper = renderer.render(&mk(klipp_timeline::TextCase::Uppercase), 1280, 720);
        assert_ne!(
            &lower[..], &upper[..],
            "Uppercase should produce different bitmap than the original lowercase text"
        );
        // SmallCaps: same glyphs as Uppercase but at 80% size — must
        // differ from the full-size uppercase render.
        let smallcaps = renderer.render(&mk(klipp_timeline::TextCase::SmallCaps), 1280, 720);
        assert_ne!(
            &upper[..], &smallcaps[..],
            "SmallCaps (80%) should differ from full-size Uppercase render"
        );
    }

    /// Baseline shift moves the rendered text vertically without
    /// changing the glyph shapes, so a +20 shift should produce the
    /// same total pixel count as a 0 shift but at different y rows.
    #[test]
    fn baseline_shift_moves_text_vertically() {
        let mut renderer = TitleRenderer::new();
        let mk = |shift: f32| {
            let tb = klipp_timeline::TextBox {
                text: "X".to_string(),
                font_size: 80.0,
                baseline_shift: shift,
                ..Default::default()
            };
            klipp_timeline::TitleData::single_text(tb)
        };
        // Find first row with a non-transparent pixel for each render.
        let first_visible_row = |buf: &[u8], w: u32, h: u32| -> Option<u32> {
            for y in 0..h {
                let row_start = (y * w * 4) as usize;
                if buf[row_start..row_start + (w * 4) as usize]
                    .chunks_exact(4)
                    .any(|p| p[3] > 0)
                {
                    return Some(y);
                }
            }
            None
        };
        let center = renderer.render(&mk(0.0), 1280, 720);
        let lower = renderer.render(&mk(50.0), 1280, 720);
        let row_center = first_visible_row(&center, 1280, 720).expect("center has visible pixels");
        let row_lower = first_visible_row(&lower, 1280, 720).expect("shifted has visible pixels");
        assert!(
            row_lower > row_center,
            "+50 baseline_shift should move the text DOWN; row_center={row_center} row_lower={row_lower}"
        );
    }

    /// Pin the contract that `char_animation` actually changes the
    /// rendered output during the entrance window. Without this test
    /// we would silently regress to the pre-fix behaviour where
    /// per-character animation was honoured by the browser hover
    /// preview but completely dropped by the playback / export
    /// renderer.
    ///
    /// Test strategy: render the same title at t=0 (fully un-entered,
    /// every glyph at alpha = 0) and t=10s (fully settled, every
    /// glyph at alpha = 1). The total non-zero pixel count at t=0
    /// must be strictly less than at t=10s for FadeIn — proving the
    /// alpha mask responds to time.
    #[test]
    fn char_animation_alters_output_during_entrance() {
        let mut renderer = TitleRenderer::new();
        let tb = klipp_timeline::TextBox {
            text: "ABCDEFG".to_string(),
            font_size: 80.0,
            motion: klipp_timeline::TitleMotion::enter(
                klipp_timeline::Motion::new(klipp_timeline::MotionKind::Fade).duration(0.4).stagger(klipp_timeline::StaggerUnit::Char, 0.05),
            ),
            ..Default::default()
        };
        let td = klipp_timeline::TitleData::single_text(tb);

        let buf_t0 = renderer.render_with_time(&td, 1920, 1080, None, 0.0);
        let buf_t_settled = renderer.render_with_time(&td, 1920, 1080, None, 10.0);

        let count_nonzero = |buf: &[u8]| buf.chunks_exact(4).filter(|p| p[3] > 0).count();
        let n0 = count_nonzero(&buf_t0);
        let n1 = count_nonzero(&buf_t_settled);
        assert!(
            n0 < n1,
            "FadeIn at t=0 should have FEWER visible pixels than at settled t. n0={n0} n1={n1}"
        );
        // Sanity: the settled frame must have non-trivial content.
        assert!(n1 > 1000, "Settled frame should have >1000 visible pixels, got {n1}");
    }

    /// Cache MUST be bypassed when char_animation is present; without
    /// this guard, the first-frame bitmap (often zero-alpha) would
    /// stick around forever.
    #[test]
    fn char_animation_bypasses_cache_at_different_times() {
        let mut renderer = TitleRenderer::new();
        let tb = klipp_timeline::TextBox {
            text: "TEST".to_string(),
            font_size: 80.0,
            motion: klipp_timeline::TitleMotion::enter(
                klipp_timeline::Motion::new(klipp_timeline::MotionKind::Rise).duration(0.4).stagger(klipp_timeline::StaggerUnit::Char, 0.1),
            ),
            ..Default::default()
        };
        let td = klipp_timeline::TitleData::single_text(tb);

        let frame_a = renderer.render_with_time(&td, 1280, 720, None, 0.05);
        let frame_b = renderer.render_with_time(&td, 1280, 720, None, 0.20);
        // Different times during the entrance ramp must produce
        // different frames (slide offset / alpha both shift).
        assert_ne!(
            &frame_a[..],
            &frame_b[..],
            "render_with_time should produce different frames at different mid-entrance times"
        );
    }

    /// Pin the contract that `transform.center.x` is honoured for
    /// every alignment. Dragging a Left/Right-aligned textbox on the
    /// canvas must move the rendered text — the previous renderer
    /// pinned Left to a fixed left margin and Right to a fixed right
    /// margin, silently dropping the user's drag for those two
    /// alignments. Asserts via the public layout's `text_left` /
    /// `text_right` so the test fails the second the renderer
    /// regresses.
    #[test]
    fn alignment_honours_transform_center_x() {
        let mut renderer = TitleRenderer::new();
        let frame_w: u32 = 1920;
        let frame_h: u32 = 1080;
        let mut mk = |alignment: klipp_timeline::TextAlignment, cx: f32| -> TitleLayout {
            let tb = klipp_timeline::TextBox {
                text: "X".to_string(),
                font_size: 100.0,
                alignment,
                ..Default::default()
            };
            let mut td = klipp_timeline::TitleData::single_text(tb);
            if let Some(o) = td.objects.first_mut() {
                o.transform.center = TitlePosition { x: cx, y: 0.5 };
            }
            renderer.render_layout(&td, frame_w, frame_h)
        };

        // Center alignment: text centred on the anchor.
        let centred_at_quarter = mk(klipp_timeline::TextAlignment::Center, 0.25);
        let centred_at_three_quarter = mk(klipp_timeline::TextAlignment::Center, 0.75);
        let q_mid = (centred_at_quarter.text_left + centred_at_quarter.text_right) * 0.5;
        let tq_mid = (centred_at_three_quarter.text_left + centred_at_three_quarter.text_right) * 0.5;
        assert!((q_mid - 480.0).abs() < 2.0,
            "Center @ 0.25 should mid at 480px, got {q_mid}");
        assert!((tq_mid - 1440.0).abs() < 2.0,
            "Center @ 0.75 should mid at 1440px, got {tq_mid}");

        // Left alignment: text's left edge at the anchor.
        let left_at_left = mk(klipp_timeline::TextAlignment::Left, 0.1);
        let left_at_right = mk(klipp_timeline::TextAlignment::Left, 0.6);
        assert!((left_at_left.text_left - 192.0).abs() < 2.0,
            "Left @ 0.1 should start at 192px, got {}", left_at_left.text_left);
        assert!((left_at_right.text_left - 1152.0).abs() < 2.0,
            "Left @ 0.6 should start at 1152px, got {}", left_at_right.text_left);
        assert!(left_at_right.text_left > left_at_left.text_left + 100.0,
            "Dragging a Left-aligned textbox right must move the rendered text right");

        // Right alignment: text's right edge at the anchor.
        let right_at_left = mk(klipp_timeline::TextAlignment::Right, 0.4);
        let right_at_right = mk(klipp_timeline::TextAlignment::Right, 0.9);
        assert!((right_at_left.text_right - 768.0).abs() < 2.0,
            "Right @ 0.4 should end at 768px, got {}", right_at_left.text_right);
        assert!((right_at_right.text_right - 1728.0).abs() < 2.0,
            "Right @ 0.9 should end at 1728px, got {}", right_at_right.text_right);
        assert!(right_at_right.text_right > right_at_left.text_right + 100.0,
            "Dragging a Right-aligned textbox right must move the rendered text right");
    }

    #[test]
    fn test_char_positions_with_style_runs() {
        let mut renderer = TitleRenderer::new();
        let td = {
    let tb = klipp_timeline::TextBox {
        text: "Hello World".to_string(),
        font_size: 72.0,
        style_runs: vec![
                    StyleRun { start: 0, end: 5, font_size: Some(100.0), color: None, font_name: None, font_style: None, tracking: None, baseline_offset: None },
                    StyleRun { start: 6, end: 11, font_size: Some(50.0), color: None, font_name: None, font_style: None, tracking: None, baseline_offset: None },
                ],
        ..Default::default()
    };
    
    klipp_timeline::TitleData::single_text(tb)
};
        let positions = renderer.compute_char_positions(&td, 1920, 1080);
        let char_count = "Hello World".chars().count(); // 11
        assert_eq!(positions.len(), char_count + 1, "Expected {} positions, got {}", char_count + 1, positions.len());
        // Sequential char_indices — this is the critical test
        for (i, cp) in positions.iter().enumerate() {
            assert_eq!(cp.char_index, i, "Position {} has wrong char_index: {} (byte_offset={})", i, cp.char_index, cp.byte_offset);
        }
    }

    #[test]
    fn test_char_positions_byte_offsets_match_text() {
        let mut renderer = TitleRenderer::new();
        let text = "Hi there!";
        let td = {
    let tb = klipp_timeline::TextBox {
        text: text.to_string(),
        font_size: 72.0,
        ..Default::default()
    };
    
    klipp_timeline::TitleData::single_text(tb)
};
        let positions = renderer.compute_char_positions(&td, 1920, 1080);
        // Every non-end position should have a byte_offset that points to the right character
        for cp in &positions {
            if cp.char_index < text.chars().count() {
                let expected_byte = text.char_indices().nth(cp.char_index).map(|(b, _)| b).unwrap();
                assert_eq!(cp.byte_offset, expected_byte,
                    "char_index {} should have byte_offset {} but has {}", cp.char_index, expected_byte, cp.byte_offset);
            }
        }
    }

    #[test]
    fn test_char_positions_style_runs_byte_offsets() {
        let mut renderer = TitleRenderer::new();
        let text = "ABCDEFGHIJ";
        let td = {
    let tb = klipp_timeline::TextBox {
        text: text.to_string(),
        font_size: 72.0,
        style_runs: vec![
                    StyleRun { start: 0, end: 3, font_size: Some(100.0), color: None, font_name: None, font_style: None, tracking: None, baseline_offset: None }, // ABC
                    StyleRun { start: 3, end: 7, font_size: Some(50.0), color: None, font_name: None, font_style: None, tracking: None, baseline_offset: None },  // DEFG
                    StyleRun { start: 7, end: 10, font_size: Some(72.0), color: None, font_name: None, font_style: None, tracking: None, baseline_offset: None }, // HIJ
                ],
        ..Default::default()
    };
    
    klipp_timeline::TitleData::single_text(tb)
};
        let positions = renderer.compute_char_positions(&td, 1920, 1080);
        // All ASCII so byte_offset == char_index
        for cp in &positions {
            if cp.char_index < 10 {
                assert_eq!(cp.byte_offset, cp.char_index,
                    "ASCII text: char_index {} should equal byte_offset, but byte_offset={}", cp.char_index, cp.byte_offset);
            }
        }
        // Sequential char_indices
        for (i, cp) in positions.iter().enumerate() {
            assert_eq!(cp.char_index, i, "Position {} has wrong char_index: {}", i, cp.char_index);
        }
    }

    #[test]
    fn test_char_to_byte_and_insert() {
        // Simulate what the editor does: navigate to a position, insert text
        let text = "Hello World";
        let char_count = text.chars().count(); // 11

        // char_to_byte for each position
        for ci in 0..=char_count {
            let byte_pos = text.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(text.len());
            let mut modified = text.to_string();
            modified.insert(byte_pos, 'X');

            // Verify insertion at position ci puts X after the ci-th character
            let chars: Vec<char> = modified.chars().collect();
            assert_eq!(chars[ci], 'X', "Insert at char {} should place X there, got '{}' in '{}'", ci, chars[ci], modified);
        }
    }

    /// Per-layer position keyframes drive the rendered glyph location:
    /// a textbox with center_x_keyframes [(0s, 0.1) → (2s, 0.9)] should
    /// render its text on the LEFT half of the frame at t=0 and on the
    /// RIGHT half at t=2. Asserts via the alpha-pixel centroid so the
    /// test fails the moment the renderer ignores the keyframe vec.
    #[test]
    fn position_keyframes_move_rendered_text_horizontally() {
        use klipp_core::{Interpolation, Keyframe};

        let mut renderer = TitleRenderer::new();
        let frame_w: u32 = 1280;
        let frame_h: u32 = 720;

        let tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 100.0,
            ..Default::default()
        };
        let mut td = klipp_timeline::TitleData::single_text(tb);
        if let Some(o) = td.objects.first_mut() {
            // Static fallback in the centre — must be ignored once the
            // keyframes evaluate.
            o.transform.center = TitlePosition { x: 0.5, y: 0.5 };
            o.transform.center_x_keyframes = vec![
                Keyframe {
                    time_secs: 0.0,
                    value: 0.1,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0),
                    tangent_out: (0.0, 0.0),
                },
                Keyframe {
                    time_secs: 2.0,
                    value: 0.9,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0),
                    tangent_out: (0.0, 0.0),
                },
            ];
        }

        let buf_t0 = renderer.render_with_time(&td, frame_w, frame_h, None, 0.0);
        let buf_t2 = renderer.render_with_time(&td, frame_w, frame_h, None, 2.0);

        let centroid_x = |buf: &[u8]| -> f64 {
            let mut sum = 0u64;
            let mut n = 0u64;
            for (i, p) in buf.chunks_exact(4).enumerate() {
                if p[3] > 0 {
                    let x = (i as u32) % frame_w;
                    sum += x as u64;
                    n += 1;
                }
            }
            assert!(n > 0, "frame must contain visible glyph pixels");
            sum as f64 / n as f64
        };
        let cx_t0 = centroid_x(&buf_t0);
        let cx_t2 = centroid_x(&buf_t2);
        // At t=0 keyframe x=0.1 → centroid near 128px; at t=2 keyframe
        // x=0.9 → centroid near 1152px. We assert a generous separation
        // so antialiasing / glyph-box width don't make the test brittle.
        assert!(
            cx_t0 < (frame_w as f64) * 0.30,
            "t=0 centroid should be on the left third, got {cx_t0}"
        );
        assert!(
            cx_t2 > (frame_w as f64) * 0.70,
            "t=2 centroid should be on the right third, got {cx_t2}"
        );
        assert!(
            cx_t2 - cx_t0 > (frame_w as f64) * 0.4,
            "t=0 and t=2 centroids must separate by >40% of frame width, got {} → {}",
            cx_t0, cx_t2
        );
    }

    /// Empty position-keyframe vecs must NOT change the rendered output
    /// — the static `center.x/y` fallback path stays in effect, and a
    /// title with no animation renders identically to before #2.
    #[test]
    fn empty_position_keyframes_render_identical_to_pre_keyframe_path() {
        let mut renderer = TitleRenderer::new();
        let frame_w: u32 = 640;
        let frame_h: u32 = 480;

        let tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 80.0,
            ..Default::default()
        };
        let td = klipp_timeline::TitleData::single_text(tb);

        let buf_no_time = renderer.render(&td, frame_w, frame_h);
        let buf_with_time = renderer.render_with_time(&td, frame_w, frame_h, None, 1.5);

        assert_eq!(
            &buf_no_time[..],
            &buf_with_time[..],
            "title with no position keyframes must render identically with or without time_secs"
        );
    }

    /// Per-frame TextGlow.radius keyframes drive the rendered halo
    /// width: a glow ramping 4 → 24 px should cover measurably more
    /// non-zero-alpha pixels at t=2 than at t=0. Asserts via the
    /// halo's pixel count outside the text body so the test catches
    /// the renderer ignoring radius_keyframes.
    #[test]
    fn glow_radius_keyframes_change_rendered_halo_size() {
        use klipp_core::{Interpolation, Keyframe};

        let mut renderer = TitleRenderer::new();
        let frame_w: u32 = 640;
        let frame_h: u32 = 480;

        let mut tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 80.0,
            ..Default::default()
        };
        tb.glows.push(klipp_timeline::TextGlow {
            enabled: true,
            color: [255, 200, 0, 255],
            radius: 4.0,
            radius_keyframes: vec![
                Keyframe {
                    time_secs: 0.0,
                    value: 4.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0),
                    tangent_out: (0.0, 0.0),
                },
                Keyframe {
                    time_secs: 2.0,
                    value: 24.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0),
                    tangent_out: (0.0, 0.0),
                },
            ],
            intensity: 1.0,
            intensity_keyframes: Vec::new(),
        });
        let td = klipp_timeline::TitleData::single_text(tb);

        let buf_t0 = renderer.render_with_time(&td, frame_w, frame_h, None, 0.0);
        let buf_t2 = renderer.render_with_time(&td, frame_w, frame_h, None, 2.0);

        let nonzero_alpha_count = |buf: &[u8]| -> usize {
            buf.chunks_exact(4).filter(|p| p[3] > 0).count()
        };
        let n0 = nonzero_alpha_count(&buf_t0);
        let n2 = nonzero_alpha_count(&buf_t2);
        assert!(
            n2 > n0,
            "t=2 with radius=24 must cover more pixels than t=0 with radius=4 (n0={n0}, n2={n2})"
        );
        // The 6× radius growth should produce a meaningfully larger
        // halo footprint, not a marginal antialiasing difference.
        assert!(
            n2 as f32 > n0 as f32 * 1.5,
            "t=2 halo should be at least 1.5× larger than t=0 (n0={n0}, n2={n2})"
        );
    }

    /// Per-frame TextGlow.intensity keyframes drive the alpha sum of
    /// the rendered halo: a glow ramping intensity 0.1 → 1.0 at fixed
    /// radius should produce a visibly brighter halo (higher alpha
    /// integral) at t=1 than at t=0.
    #[test]
    fn glow_intensity_keyframes_change_rendered_halo_alpha() {
        use klipp_core::{Interpolation, Keyframe};

        let mut renderer = TitleRenderer::new();
        let frame_w: u32 = 640;
        let frame_h: u32 = 480;

        let mut tb = klipp_timeline::TextBox {
            text: "O".to_string(),
            font_size: 80.0,
            ..Default::default()
        };
        tb.glows.push(klipp_timeline::TextGlow {
            enabled: true,
            color: [255, 100, 100, 255],
            radius: 12.0,
            radius_keyframes: Vec::new(),
            intensity: 1.0,
            intensity_keyframes: vec![
                Keyframe {
                    time_secs: 0.0,
                    value: 0.1,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0),
                    tangent_out: (0.0, 0.0),
                },
                Keyframe {
                    time_secs: 1.0,
                    value: 1.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0),
                    tangent_out: (0.0, 0.0),
                },
            ],
        });
        let td = klipp_timeline::TitleData::single_text(tb);

        let buf_t0 = renderer.render_with_time(&td, frame_w, frame_h, None, 0.0);
        let buf_t1 = renderer.render_with_time(&td, frame_w, frame_h, None, 1.0);

        // Sum of alpha values across the frame measures total
        // light-emission energy. Higher intensity → higher sum.
        let alpha_sum = |buf: &[u8]| -> u64 {
            buf.chunks_exact(4).map(|p| p[3] as u64).sum()
        };
        let s0 = alpha_sum(&buf_t0);
        let s1 = alpha_sum(&buf_t1);
        assert!(
            s1 > s0,
            "t=1 alpha sum must exceed t=0 alpha sum (intensity 1.0 > 0.1) — s0={s0} s1={s1}"
        );
    }

    /// A `Wave` loop moves the glyphs, and keeps moving them while
    /// the layer is also fading in.
    #[test]
    fn a_wave_loop_moves_the_glyphs_over_time() {
        let mut renderer = TitleRenderer::new();
        let tb = klipp_timeline::TextBox {
            text: "ABCDE".to_string(),
            font_size: 80.0,
            color: [255, 255, 255, 255],
            motion: klipp_timeline::TitleMotion::enter(klipp_timeline::Motion::new(klipp_timeline::MotionKind::Fade).duration(1.0)),
            loops: vec![klipp_timeline::TitleLoop::Wave { amplitude: 0.3, rate_hz: 2.0 }],
            ..Default::default()
        };
        let td = klipp_timeline::TitleData::single_text(tb);
        let buf_a = renderer.render_with_time(&td, 1280, 720, None, 2.0);
        let buf_b = renderer.render_with_time(&td, 1280, 720, None, 2.1);
        assert_ne!(&buf_a[..], &buf_b[..], "the wave must move the glyphs between two settled frames");
    }

    /// A `Blink` loop switches the layer off for part of its period.
    #[test]
    fn a_blink_loop_hides_the_layer_off_phase() {
        let mut renderer = TitleRenderer::new();
        let tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 80.0,
            loops: vec![klipp_timeline::TitleLoop::Blink { rate_hz: 1.0, duty: 0.5 }],
            ..Default::default()
        };
        let td = klipp_timeline::TitleData::single_text(tb);
        let on = renderer.render_with_time(&td, 640, 360, None, 0.25);
        let off = renderer.render_with_time(&td, 640, 360, None, 0.75);
        assert!(on.chunks_exact(4).any(|p| p[3] > 0));
        assert!(off.chunks_exact(4).all(|p| p[3] == 0));
    }

    /// A `Scroll` loop with no speed carries the layer from below the
    /// frame to above it over the clip.
    #[test]
    fn a_scroll_loop_travels_through_the_frame() {
        let mut renderer = TitleRenderer::new();
        let tb = klipp_timeline::TextBox {
            text: "Roll".to_string(),
            font_size: 80.0,
            loops: vec![klipp_timeline::TitleLoop::Scroll { direction: klipp_timeline::ScrollDirection::Up, speed_px_per_sec: 0.0 }],
            ..Default::default()
        };
        let td = klipp_timeline::TitleData::single_text(tb);
        let row_of = |buf: &[u8]| -> Option<usize> {
            buf.chunks_exact(4).position(|p| p[3] > 0).map(|i| i / 640)
        };
        let start = renderer.render_with_window(&td, 640, 360, None, 0.0, Some(4.0));
        let mid = renderer.render_with_window(&td, 640, 360, None, 2.0, Some(4.0));
        let end = renderer.render_with_window(&td, 640, 360, None, 4.0, Some(4.0));
        assert!(row_of(&start).is_none(), "off screen below at the start");
        assert!(row_of(&mid).is_some(), "in frame at the midpoint");
        assert!(row_of(&end).is_none(), "off screen above at the end");
    }

    #[test]
    fn glow_keyframes_unaffected_by_clip_animation_type() {
        use klipp_core::{Interpolation, Keyframe};

        let mut renderer = TitleRenderer::new();
        let frame_w: u32 = 640;
        let frame_h: u32 = 480;

        let mut tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 80.0,
            ..Default::default()
        };
        tb.glows.push(klipp_timeline::TextGlow {
            enabled: true,
            color: [255, 200, 0, 255],
            radius: 4.0,
            radius_keyframes: vec![
                Keyframe {
                    time_secs: 0.0,
                    value: 4.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0),
                    tangent_out: (0.0, 0.0),
                },
                Keyframe {
                    time_secs: 2.0,
                    value: 24.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0),
                    tangent_out: (0.0, 0.0),
                },
            ],
            intensity: 1.0,
            intensity_keyframes: Vec::new(),
        });
        let td = klipp_timeline::TitleData::single_text(tb);

        let buf_t0 = renderer.render_with_time(&td, frame_w, frame_h, None, 0.0);
        let buf_t2 = renderer.render_with_time(&td, frame_w, frame_h, None, 2.0);

        let nonzero_alpha_count = |buf: &[u8]| -> usize {
            buf.chunks_exact(4).filter(|p| p[3] > 0).count()
        };
        let n0 = nonzero_alpha_count(&buf_t0);
        let n2 = nonzero_alpha_count(&buf_t2);
        assert!(
            n2 as f32 > n0 as f32 * 1.5,
            "glow keyframes must drive halo growth even when animation_type=FadeIn (n0={n0}, n2={n2})"
        );
    }

    /// Empty glow keyframe vecs must NOT change the rendered output —
    /// the static `radius` / `intensity` fallback path stays in
    /// effect. Pins the no-overhead path for unanimated glow.
    #[test]
    fn empty_glow_keyframes_render_identical_to_pre_keyframe_path() {
        let mut renderer = TitleRenderer::new();
        let frame_w: u32 = 640;
        let frame_h: u32 = 480;

        let mut tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 80.0,
            ..Default::default()
        };
        tb.glows.push(klipp_timeline::TextGlow {
            enabled: true,
            color: [255, 200, 0, 255],
            radius: 12.0,
            radius_keyframes: Vec::new(),
            intensity: 0.8,
            intensity_keyframes: Vec::new(),
        });
        let td = klipp_timeline::TitleData::single_text(tb);

        let buf_no_time = renderer.render(&td, frame_w, frame_h);
        let buf_with_time = renderer.render_with_time(&td, frame_w, frame_h, None, 1.5);

        assert_eq!(
            &buf_no_time[..],
            &buf_with_time[..],
            "title with no glow keyframes must render identically with or without time_secs"
        );
    }

    /// Per-frame TitleShadow.blur keyframes drive the rendered shadow
    /// softness: a shadow blur ramping 0 → 20 px should cover more
    /// non-zero-alpha pixels (wider falloff) at t=2 than at t=0.
    #[test]
    fn shadow_blur_keyframes_change_rendered_shadow_softness() {
        use klipp_core::{Interpolation, Keyframe};

        let mut renderer = TitleRenderer::new();
        let frame_w: u32 = 640;
        let frame_h: u32 = 480;

        let mut tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 80.0,
            ..Default::default()
        };
        tb.shadows.push(klipp_timeline::TitleShadow {
            enabled: true,
            color: [0, 0, 0, 220],
            offset_x: 6.0,
            offset_x_keyframes: Vec::new(),
            offset_y: 6.0,
            offset_y_keyframes: Vec::new(),
            blur: 0.0,
            blur_keyframes: vec![
                Keyframe {
                    time_secs: 0.0, value: 0.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                },
                Keyframe {
                    time_secs: 2.0, value: 20.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                },
            ],
        });
        let td = klipp_timeline::TitleData::single_text(tb);

        let buf_t0 = renderer.render_with_time(&td, frame_w, frame_h, None, 0.0);
        let buf_t2 = renderer.render_with_time(&td, frame_w, frame_h, None, 2.0);

        let nonzero_alpha_count = |buf: &[u8]| -> usize {
            buf.chunks_exact(4).filter(|p| p[3] > 0).count()
        };
        let n0 = nonzero_alpha_count(&buf_t0);
        let n2 = nonzero_alpha_count(&buf_t2);
        assert!(
            n2 > n0,
            "blurred shadow at t=2 must cover more pixels than crisp shadow at t=0 (n0={n0}, n2={n2})"
        );
    }

    /// Per-frame TitleShadow.offset_x keyframes shift the rendered
    /// shadow horizontally: a shadow whose offset ramps -20 → +20
    /// should put its dark mass on the LEFT of the glyph at t=0 and on
    /// the RIGHT at t=2. Asserts via the dark-pixel centroid.
    #[test]
    fn shadow_offset_x_keyframes_shift_rendered_shadow_position() {
        use klipp_core::{Interpolation, Keyframe};

        let mut renderer = TitleRenderer::new();
        let frame_w: u32 = 1280;
        let frame_h: u32 = 720;

        let mut tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 100.0,
            color: [255, 255, 255, 255],
            ..Default::default()
        };
        tb.shadows.push(klipp_timeline::TitleShadow {
            enabled: true,
            color: [0, 0, 0, 255],
            offset_x: 0.0,
            offset_x_keyframes: vec![
                Keyframe {
                    time_secs: 0.0, value: -40.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                },
                Keyframe {
                    time_secs: 2.0, value: 40.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                },
            ],
            offset_y: 0.0,
            offset_y_keyframes: Vec::new(),
            blur: 0.0,
            blur_keyframes: Vec::new(),
        });
        let td = klipp_timeline::TitleData::single_text(tb);

        let buf_t0 = renderer.render_with_time(&td, frame_w, frame_h, None, 0.0);
        let buf_t2 = renderer.render_with_time(&td, frame_w, frame_h, None, 2.0);

        // Centroid of pixels where the shadow's dark colour dominates
        // (R+G+B < ~150). The shadow mass shifts left at t=0 (-40px)
        // and right at t=2 (+40px) relative to the white glyph.
        let dark_centroid_x = |buf: &[u8]| -> f64 {
            let mut sum = 0u64;
            let mut n = 0u64;
            for (i, p) in buf.chunks_exact(4).enumerate() {
                if p[3] > 0 && (p[0] as u16 + p[1] as u16 + p[2] as u16) < 150 {
                    let x = (i as u32) % frame_w;
                    sum += x as u64;
                    n += 1;
                }
            }
            assert!(n > 0, "frame must contain visible shadow pixels");
            sum as f64 / n as f64
        };
        let cx0 = dark_centroid_x(&buf_t0);
        let cx2 = dark_centroid_x(&buf_t2);
        assert!(
            cx2 - cx0 > (frame_w as f64) * 0.02,
            "shadow centroid must shift right between t=0 and t=2 (cx0={cx0}, cx2={cx2})"
        );
    }

    /// Per-frame InnerShadow.blur keyframes drive the rendered
    /// inner-shadow softness — wider falloff at higher blur.
    #[test]
    fn inner_shadow_blur_keyframes_change_rendered_softness() {
        use klipp_core::{Interpolation, Keyframe};

        let mut renderer = TitleRenderer::new();
        let frame_w: u32 = 640;
        let frame_h: u32 = 480;

        let mut tb = klipp_timeline::TextBox {
            text: "O".to_string(),
            font_size: 100.0,
            color: [255, 255, 255, 255],
            ..Default::default()
        };
        tb.inner_shadows.push(klipp_timeline::InnerShadow {
            enabled: true,
            color: [0, 0, 0, 255],
            offset_x: 4.0, offset_x_keyframes: Vec::new(),
            offset_y: 4.0, offset_y_keyframes: Vec::new(),
            blur: 0.0,
            blur_keyframes: vec![
                Keyframe {
                    time_secs: 0.0, value: 0.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                },
                Keyframe {
                    time_secs: 2.0, value: 18.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                },
            ],
        });
        let td = klipp_timeline::TitleData::single_text(tb);

        let buf_t0 = renderer.render_with_time(&td, frame_w, frame_h, None, 0.0);
        let buf_t2 = renderer.render_with_time(&td, frame_w, frame_h, None, 2.0);

        // Higher blur softens the dark inner-shadow falloff so more
        // pixels in the glyph interior end up dark vs. white. Sum the
        // dark-pixel count to detect the change.
        let dark_pixel_count = |buf: &[u8]| -> usize {
            buf.chunks_exact(4)
                .filter(|p| {
                    p[3] > 0 && (p[0] as u16 + p[1] as u16 + p[2] as u16) < 250
                })
                .count()
        };
        let n0 = dark_pixel_count(&buf_t0);
        let n2 = dark_pixel_count(&buf_t2);
        assert!(
            n0 != n2,
            "inner-shadow blur keyframes must change the rendered output (n0={n0} n2={n2})"
        );
    }

    /// Empty shadow keyframe vecs must NOT change the rendered output.
    /// Pins the no-overhead path for unanimated shadows — same
    /// invariant as the glow case.
    #[test]
    fn empty_shadow_keyframes_render_identical_to_pre_keyframe_path() {
        let mut renderer = TitleRenderer::new();
        let frame_w: u32 = 640;
        let frame_h: u32 = 480;

        let mut tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 80.0,
            ..Default::default()
        };
        tb.shadows.push(klipp_timeline::TitleShadow {
            enabled: true,
            color: [0, 0, 0, 200],
            offset_x: 4.0,
            offset_x_keyframes: Vec::new(),
            offset_y: 4.0,
            offset_y_keyframes: Vec::new(),
            blur: 6.0,
            blur_keyframes: Vec::new(),
        });
        let td = klipp_timeline::TitleData::single_text(tb);

        let buf_no_time = renderer.render(&td, frame_w, frame_h);
        let buf_with_time = renderer.render_with_time(&td, frame_w, frame_h, None, 1.5);

        assert_eq!(
            &buf_no_time[..],
            &buf_with_time[..],
            "title with no shadow keyframes must render identically with or without time_secs"
        );
    }

    /// Per-frame TextOutline.width keyframes on outlines[0] drive the
    /// rendered stroke thickness — a width ramp 1 → 8 px adds visible
    /// outline pixels at t=2 vs t=0.
    #[test]
    fn outline_width_keyframes_change_rendered_stroke_thickness() {
        use klipp_core::{Interpolation, Keyframe};

        let mut renderer = TitleRenderer::new();
        let frame_w: u32 = 640;
        let frame_h: u32 = 480;

        let mut tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 80.0,
            color: [255, 255, 255, 255],
            ..Default::default()
        };
        tb.outlines.push(klipp_timeline::TextOutline {
            enabled: true,
            color: [255, 0, 0, 255],
            width: 1.0,
            width_keyframes: vec![
                Keyframe {
                    time_secs: 0.0, value: 1.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                },
                Keyframe {
                    time_secs: 2.0, value: 8.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                },
            ],
        });
        let td = klipp_timeline::TitleData::single_text(tb);

        let buf_t0 = renderer.render_with_time(&td, frame_w, frame_h, None, 0.0);
        let buf_t2 = renderer.render_with_time(&td, frame_w, frame_h, None, 2.0);

        // Count red-tinted pixels (outline color) — wider stroke at
        // t=2 produces more red coverage.
        let red_pixel_count = |buf: &[u8]| -> usize {
            buf.chunks_exact(4)
                .filter(|p| p[3] > 0 && p[0] > 200 && p[1] < 80 && p[2] < 80)
                .count()
        };
        let n0 = red_pixel_count(&buf_t0);
        let n2 = red_pixel_count(&buf_t2);
        assert!(
            n2 > n0,
            "outline width ramp 1→8 must thicken the stroke (n0={n0}, n2={n2})"
        );
    }

    /// Per-stack-element keyframes are isolated: keyframing
    /// outlines[1].width must NOT change outlines[0]'s rendered
    /// thickness. Builds a two-layer stack with outline 0 static and
    /// outline 1 keyframed, then asserts outline 0's contribution is
    /// time-independent.
    #[test]
    fn outline_keyframes_per_stack_index_are_isolated() {
        use klipp_core::{Interpolation, Keyframe};

        let mut renderer = TitleRenderer::new();
        let frame_w: u32 = 640;
        let frame_h: u32 = 480;

        let mut tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 80.0,
            color: [255, 255, 255, 255],
            ..Default::default()
        };
        tb.outlines.push(klipp_timeline::TextOutline {
            enabled: true,
            color: [0, 100, 255, 255],
            width: 4.0,
            width_keyframes: Vec::new(),
        });
        // outlines[1]: green, keyframed 1→10 px (rendered behind
        // outlines[0] because the renderer draws widest-first).
        tb.outlines.push(klipp_timeline::TextOutline {
            enabled: true,
            color: [0, 255, 0, 255],
            width: 1.0,
            width_keyframes: vec![
                Keyframe {
                    time_secs: 0.0, value: 1.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                },
                Keyframe {
                    time_secs: 2.0, value: 10.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                },
            ],
        });
        let td = klipp_timeline::TitleData::single_text(tb);

        let buf_t0 = renderer.render_with_time(&td, frame_w, frame_h, None, 0.0);
        let buf_t2 = renderer.render_with_time(&td, frame_w, frame_h, None, 2.0);

        // Blue (outline 0) coverage should be constant — index 0 is
        // not keyframed. Green (outline 1) coverage grows.
        let blue_count = |buf: &[u8]| -> usize {
            buf.chunks_exact(4)
                .filter(|p| p[3] > 200 && p[2] > 200 && p[1] < 150 && p[0] < 80)
                .count()
        };
        let green_count = |buf: &[u8]| -> usize {
            buf.chunks_exact(4)
                .filter(|p| p[3] > 200 && p[1] > 200 && p[0] < 80 && p[2] < 80)
                .count()
        };
        let blue_t0 = blue_count(&buf_t0);
        let blue_t2 = blue_count(&buf_t2);
        let green_t0 = green_count(&buf_t0);
        let green_t2 = green_count(&buf_t2);

        // Index-0 (blue) is static — its coverage should be roughly
        // identical at both times. Allow tiny tolerance for rounding.
        let blue_diff = (blue_t0 as i64 - blue_t2 as i64).abs();
        assert!(
            blue_diff < (blue_t0.max(blue_t2) as i64 / 10).max(50),
            "outlines[0] (static) blue coverage must be stable across time (t0={blue_t0}, t2={blue_t2})"
        );
        // Index-1 (green) grows — keyframed thickness rendered
        // behind blue but extending outside it.
        assert!(
            green_t2 > green_t0,
            "outlines[1] (keyframed) green coverage must grow over time (t0={green_t0}, t2={green_t2})"
        );
    }

    /// `glow_stack_enabled = false` hides every secondary glow
    /// (glows[1..]) without affecting the primary (glows[0]).
    #[test]
    fn glow_stack_soft_disable_hides_secondary_layers() {
        let mut renderer = TitleRenderer::new();
        let frame_w: u32 = 640;
        let frame_h: u32 = 480;

        let mut tb = klipp_timeline::TextBox {
            text: "G".to_string(),
            font_size: 100.0,
            color: [255, 255, 255, 255],
            ..Default::default()
        };
        tb.glows.push(klipp_timeline::TextGlow {
            enabled: true,
            color: [255, 0, 0, 255],
            radius: 4.0,
            radius_keyframes: Vec::new(),
            intensity: 0.8,
            intensity_keyframes: Vec::new(),
        });
        // Secondary glow (green, much wider so it spreads further).
        tb.glows.push(klipp_timeline::TextGlow {
            enabled: true,
            color: [0, 255, 0, 255],
            radius: 30.0,
            radius_keyframes: Vec::new(),
            intensity: 1.0,
            intensity_keyframes: Vec::new(),
        });
        let td_on = klipp_timeline::TitleData::single_text(tb.clone());
        tb.glow_stack_enabled = false;
        let td_off = klipp_timeline::TitleData::single_text(tb);

        let buf_on = renderer.render(&td_on, frame_w, frame_h);
        let buf_off = renderer.render(&td_off, frame_w, frame_h);

        // Glow falls off with the blur radius, so the green channel
        // tapers — count any visible pixel where green clearly
        // dominates over red and blue. White-text pixels are excluded
        // by `p[0] < 200` so the centre of the text doesn't false-positive.
        let green_dominant_count = |buf: &[u8]| -> usize {
            buf.chunks_exact(4)
                .filter(|p| {
                    let r = p[0] as i16;
                    let g = p[1] as i16;
                    let b = p[2] as i16;
                    let a = p[3];
                    a > 8 && g > r + 20 && g > b + 20 && r < 200
                })
                .count()
        };
        assert!(green_dominant_count(&buf_on) > 0, "stack enabled must render green secondary glow");
        assert_eq!(green_dominant_count(&buf_off), 0, "stack disabled must hide green secondary glow");
    }

    /// `inner_shadow_stack_enabled = false` hides every secondary
    /// inner shadow (inner_shadows[1..]) without affecting the
    /// primary (inner_shadows[0]).
    #[test]
    fn inner_shadow_stack_soft_disable_hides_secondary_layers() {
        let mut renderer = TitleRenderer::new();
        let frame_w: u32 = 640;
        let frame_h: u32 = 480;

        let mut tb = klipp_timeline::TextBox {
            text: "O".to_string(),
            font_size: 100.0,
            color: [255, 255, 255, 255],
            ..Default::default()
        };
        tb.inner_shadows.push(klipp_timeline::InnerShadow {
            enabled: true,
            color: [255, 0, 0, 220],
            offset_x: 2.0, offset_x_keyframes: Vec::new(),
            offset_y: 2.0, offset_y_keyframes: Vec::new(),
            blur: 0.0, blur_keyframes: Vec::new(),
        });
        // Secondary inner shadow (green, much bigger).
        tb.inner_shadows.push(klipp_timeline::InnerShadow {
            enabled: true,
            color: [0, 255, 0, 220],
            offset_x: 10.0, offset_x_keyframes: Vec::new(),
            offset_y: 10.0, offset_y_keyframes: Vec::new(),
            blur: 0.0, blur_keyframes: Vec::new(),
        });
        let td_on = klipp_timeline::TitleData::single_text(tb.clone());
        tb.inner_shadow_stack_enabled = false;
        let td_off = klipp_timeline::TitleData::single_text(tb);

        let buf_on = renderer.render(&td_on, frame_w, frame_h);
        let buf_off = renderer.render(&td_off, frame_w, frame_h);

        let green_count = |buf: &[u8]| -> usize {
            buf.chunks_exact(4)
                .filter(|p| p[3] > 0 && p[1] > 200 && p[0] < 80 && p[2] < 80)
                .count()
        };
        assert!(green_count(&buf_on) > 0, "stack enabled must render green secondary inner shadow");
        assert_eq!(green_count(&buf_off), 0, "stack disabled must hide green secondary inner shadow");
    }

    /// `shadow_stack_enabled = false` hides every secondary shadow
    /// (shadows[1..]) without affecting the primary (shadows[0]).
    /// Builds a two-shadow stack where layer 1 has a far-away, large
    /// offset that wouldn't overlap shadows[0]; the soft-disable
    /// flag must drop layer 1's contribution entirely.
    #[test]
    fn shadow_stack_soft_disable_hides_secondary_layers() {
        let mut renderer = TitleRenderer::new();
        let frame_w: u32 = 640;
        let frame_h: u32 = 480;

        let mut tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 80.0,
            color: [255, 255, 255, 255],
            ..Default::default()
        };
        tb.shadows.push(klipp_timeline::TitleShadow {
            enabled: true,
            color: [255, 0, 0, 220],
            offset_x: 4.0,
            offset_x_keyframes: Vec::new(),
            offset_y: 4.0,
            offset_y_keyframes: Vec::new(),
            blur: 0.0,
            blur_keyframes: Vec::new(),
        });
        // Secondary (green) at larger offset so it draws in a
        // different region than primary.
        tb.shadows.push(klipp_timeline::TitleShadow {
            enabled: true,
            color: [0, 255, 0, 220],
            offset_x: 30.0,
            offset_x_keyframes: Vec::new(),
            offset_y: 30.0,
            offset_y_keyframes: Vec::new(),
            blur: 0.0,
            blur_keyframes: Vec::new(),
        });
        let td_on = klipp_timeline::TitleData::single_text(tb.clone());
        tb.shadow_stack_enabled = false;
        let td_off = klipp_timeline::TitleData::single_text(tb);

        let buf_on = renderer.render(&td_on, frame_w, frame_h);
        let buf_off = renderer.render(&td_off, frame_w, frame_h);

        let red_count = |buf: &[u8]| -> usize {
            buf.chunks_exact(4)
                .filter(|p| p[3] > 0 && p[0] > 200 && p[1] < 80 && p[2] < 80)
                .count()
        };
        let green_count = |buf: &[u8]| -> usize {
            buf.chunks_exact(4)
                .filter(|p| p[3] > 0 && p[1] > 200 && p[0] < 80 && p[2] < 80)
                .count()
        };
        assert!(green_count(&buf_on) > 0, "stack enabled must render green secondary");
        assert_eq!(green_count(&buf_off), 0, "stack disabled must hide green secondary");
        let red_on = red_count(&buf_on);
        let red_off = red_count(&buf_off);
        let diff = (red_on as i64 - red_off as i64).abs();
        assert!(
            diff < (red_on.max(red_off) as i64 / 10).max(50),
            "primary shadow coverage must be stable across stack toggle (on={red_on}, off={red_off})"
        );
    }

    /// `outline_stack_enabled = false` hides every secondary layer
    /// (outlines[1..]) without affecting the primary (outlines[0]).
    /// Builds a two-layer stack and asserts the green contribution
    /// disappears once the stack is disabled while blue is unchanged.
    #[test]
    fn outline_stack_soft_disable_hides_secondary_layers() {
        let mut renderer = TitleRenderer::new();
        let frame_w: u32 = 640;
        let frame_h: u32 = 480;

        let mut tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 80.0,
            color: [255, 255, 255, 255],
            ..Default::default()
        };
        tb.outlines.push(klipp_timeline::TextOutline {
            enabled: true,
            color: [0, 100, 255, 255],
            width: 4.0,
            width_keyframes: Vec::new(),
        });
        tb.outlines.push(klipp_timeline::TextOutline {
            enabled: true,
            color: [0, 255, 0, 255],
            width: 8.0,
            width_keyframes: Vec::new(),
        });
        let td_on = klipp_timeline::TitleData::single_text(tb.clone());
        tb.outline_stack_enabled = false;
        let td_off = klipp_timeline::TitleData::single_text(tb);

        let buf_on = renderer.render_with_time(&td_on, frame_w, frame_h, None, 0.0);
        let buf_off = renderer.render_with_time(&td_off, frame_w, frame_h, None, 0.0);

        let blue_count = |buf: &[u8]| -> usize {
            buf.chunks_exact(4)
                .filter(|p| p[3] > 200 && p[2] > 200 && p[1] < 150 && p[0] < 80)
                .count()
        };
        let green_count = |buf: &[u8]| -> usize {
            buf.chunks_exact(4)
                .filter(|p| p[3] > 200 && p[1] > 200 && p[0] < 80 && p[2] < 80)
                .count()
        };
        // Stack disabled drops the green secondary outline entirely.
        assert!(green_count(&buf_on) > 0, "stack enabled must render green secondary");
        assert_eq!(green_count(&buf_off), 0, "stack disabled must hide green secondary");
        // Primary blue outline is unaffected — index 0 isn't gated
        // by outline_stack_enabled.
        let blue_on = blue_count(&buf_on);
        let blue_off = blue_count(&buf_off);
        let diff = (blue_on as i64 - blue_off as i64).abs();
        assert!(
            diff < (blue_on.max(blue_off) as i64 / 10).max(50),
            "primary outline coverage must be stable across stack toggle (on={blue_on}, off={blue_off})"
        );
    }

    /// Empty outline keyframe vecs must NOT change the rendered
    /// output — pin the no-overhead path for unanimated outlines.
    #[test]
    fn empty_outline_keyframes_render_identical_to_pre_keyframe_path() {
        let mut renderer = TitleRenderer::new();
        let frame_w: u32 = 640;
        let frame_h: u32 = 480;

        let mut tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 80.0,
            ..Default::default()
        };
        tb.outlines.push(klipp_timeline::TextOutline {
            enabled: true,
            color: [255, 0, 0, 255],
            width: 3.0,
            width_keyframes: Vec::new(),
        });
        let td = klipp_timeline::TitleData::single_text(tb);

        let buf_no_time = renderer.render(&td, frame_w, frame_h);
        let buf_with_time = renderer.render_with_time(&td, frame_w, frame_h, None, 1.5);
        assert_eq!(
            &buf_no_time[..],
            &buf_with_time[..],
            "title with no outline keyframes must render identically with or without time_secs"
        );
    }

    /// Per-frame TextExtrusion.depth keyframes drive the rendered
    /// pseudo-3D depth: a depth ramp 4 → 30 px adds more visible side
    /// pixels at t=2 than at t=0.
    #[test]
    fn extrusion_depth_keyframes_change_rendered_depth_extent() {
        use klipp_core::{Interpolation, Keyframe};

        let mut renderer = TitleRenderer::new();
        let frame_w: u32 = 1280;
        let frame_h: u32 = 720;

        let tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 100.0,
            color: [255, 255, 255, 255],
            extrusion: Some(klipp_timeline::TextExtrusion {
                enabled: true,
                face_color: [255, 255, 255, 255],
                side_color: [80, 80, 120, 255],
                depth: 4.0,
                depth_keyframes: vec![
                    Keyframe {
                        time_secs: 0.0, value: 4.0,
                        interpolation: Interpolation::Linear,
                        tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                    },
                    Keyframe {
                        time_secs: 2.0, value: 30.0,
                        interpolation: Interpolation::Linear,
                        tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                    },
                ],
                angle: 135.0, angle_keyframes: Vec::new(),
                lighting: 0.5, lighting_keyframes: Vec::new(),
            }),
            ..Default::default()
        };
        let td = klipp_timeline::TitleData::single_text(tb);

        let buf_t0 = renderer.render_with_time(&td, frame_w, frame_h, None, 0.0);
        let buf_t2 = renderer.render_with_time(&td, frame_w, frame_h, None, 2.0);

        let nonzero_alpha_count = |buf: &[u8]| -> usize {
            buf.chunks_exact(4).filter(|p| p[3] > 0).count()
        };
        let n0 = nonzero_alpha_count(&buf_t0);
        let n2 = nonzero_alpha_count(&buf_t2);
        assert!(
            n2 > n0,
            "depth ramp 4→30 must produce a larger 3D footprint at t=2 (n0={n0}, n2={n2})"
        );
    }

    /// Empty extrusion keyframe vecs must NOT change the rendered
    /// output — pin the no-overhead path for unanimated extrusions.
    #[test]
    fn empty_extrusion_keyframes_render_identical_to_pre_keyframe_path() {
        let mut renderer = TitleRenderer::new();
        let frame_w: u32 = 640;
        let frame_h: u32 = 480;

        let tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 80.0,
            extrusion: Some(klipp_timeline::TextExtrusion {
                enabled: true,
                face_color: [255, 255, 255, 255],
                side_color: [80, 80, 80, 255],
                depth: 12.0, depth_keyframes: Vec::new(),
                angle: 135.0, angle_keyframes: Vec::new(),
                lighting: 0.5, lighting_keyframes: Vec::new(),
            }),
            ..Default::default()
        };
        let td = klipp_timeline::TitleData::single_text(tb);

        let buf_no_time = renderer.render(&td, frame_w, frame_h);
        let buf_with_time = renderer.render_with_time(&td, frame_w, frame_h, None, 1.5);
        assert_eq!(
            &buf_no_time[..],
            &buf_with_time[..],
            "title with no extrusion keyframes must render identically with or without time_secs"
        );
    }

    /// Empty inner-shadow keyframe vecs must NOT change the rendered
    /// output — pin the no-overhead path for unanimated inner shadows.
    #[test]
    fn empty_inner_shadow_keyframes_render_identical_to_pre_keyframe_path() {
        let mut renderer = TitleRenderer::new();
        let frame_w: u32 = 640;
        let frame_h: u32 = 480;

        let mut tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 80.0,
            ..Default::default()
        };
        tb.inner_shadows.push(klipp_timeline::InnerShadow {
            enabled: true,
            color: [0, 0, 0, 200],
            offset_x: 2.0, offset_x_keyframes: Vec::new(),
            offset_y: 2.0, offset_y_keyframes: Vec::new(),
            blur: 4.0, blur_keyframes: Vec::new(),
        });
        let td = klipp_timeline::TitleData::single_text(tb);

        let buf_no_time = renderer.render(&td, frame_w, frame_h);
        let buf_with_time = renderer.render_with_time(&td, frame_w, frame_h, None, 1.5);
        assert_eq!(
            &buf_no_time[..],
            &buf_with_time[..],
            "title with no inner-shadow keyframes must render identically with or without time_secs"
        );
    }

    // ─── Procedural texture preset generators ───────────────────

    /// Each procedural preset produces a non-uniform 256×256 RGBA
    /// buffer with the expected structural variation: paper grain
    /// has per-pixel noise, scanlines vary by row, hatching by
    /// (x+y) diagonal, gradient by row but constant per column.
    #[test]
    fn procedural_presets_produce_expected_variation() {
        use klipp_timeline::TexturePresetId;

        // Paper grain: variance across the buffer (noise).
        let (paper, _, _) = generate_preset_texture(TexturePresetId::PaperGrain, 64, 64);
        let paper_unique: std::collections::HashSet<u8> =
            paper.chunks_exact(4).map(|p| p[0]).collect();
        assert!(
            paper_unique.len() > 8,
            "paper grain should produce many distinct luminance values, got {}",
            paper_unique.len()
        );

        // Scanlines: rows vary, columns within a row are uniform.
        let (scan, _, _) = generate_preset_texture(TexturePresetId::Scanlines, 64, 64);
        let row0_first = scan[0];
        let row0_last = scan[63 * 4];
        assert_eq!(row0_first, row0_last, "scanlines row should be uniform across x");
        let row3_first = scan[3 * 64 * 4];
        assert_ne!(
            row0_first, row3_first,
            "scanline rows {} and {} should differ",
            0, 3
        );

        // Diagonal hatch: (0,0) is a hatch slot (x+y < 2), (4,0) is
        // not (4 mod 8 = 4, > 2). Different luminance.
        let (hatch, _, _) = generate_preset_texture(TexturePresetId::DiagonalHatch, 64, 64);
        let v00 = hatch[0];
        let v40 = hatch[4 * 4];
        assert_ne!(v00, v40, "diagonal hatch should differ between hatch slot and gap");

        // Gradient sweep: top is brighter than bottom, columns
        // identical within a row.
        let (grad, _, _) = generate_preset_texture(TexturePresetId::GradientSweep, 64, 64);
        let top = grad[0];
        let bottom = grad[63 * 64 * 4];
        assert!(top > bottom, "gradient sweep top should be brighter than bottom: {top} vs {bottom}");
    }

    /// `texture_fill.opacity = 0.0` preserves the underlying solid
    /// colour fill — the texture has no visible effect. This pins the
    /// "blends with underlying colour" semantic from the locked spec.
    #[test]
    fn texture_fill_opacity_zero_preserves_underlying_fill() {
        let mut renderer = TitleRenderer::new();
        let frame_w: u32 = 640;
        let frame_h: u32 = 480;

        let tb_no_tex = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 100.0,
            color: [255, 0, 0, 255],
            ..Default::default()
        };
        let td_no_tex = klipp_timeline::TitleData::single_text(tb_no_tex);

        let tb_zero_op = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 100.0,
            color: [255, 0, 0, 255],
            texture_fill: Some(klipp_timeline::TextureFill {
                enabled: true,
                source: klipp_timeline::TextureSource::Preset(
                    klipp_timeline::TexturePresetId::Scanlines,
                ),
                opacity: 0.0,
                ..klipp_timeline::TextureFill::default()
            }),
            ..Default::default()
        };
        let td_zero_op = klipp_timeline::TitleData::single_text(tb_zero_op);

        let buf_no = renderer.render(&td_no_tex, frame_w, frame_h);
        let buf_zero = renderer.render(&td_zero_op, frame_w, frame_h);
        assert_eq!(
            &buf_no[..],
            &buf_zero[..],
            "texture_fill opacity=0 must render identical to no texture (underlying fill preserved)"
        );
    }

    /// `texture_fill.opacity = 1.0` with a full-replacement source
    /// fully overrides the underlying fill — at opacity=1 we should
    /// see noticeable difference vs no-texture.
    #[test]
    fn texture_fill_opacity_one_overrides_underlying_fill() {
        let mut renderer = TitleRenderer::new();
        let frame_w: u32 = 640;
        let frame_h: u32 = 480;

        let tb_no = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 100.0,
            color: [255, 0, 0, 255],
            ..Default::default()
        };
        let td_no = klipp_timeline::TitleData::single_text(tb_no);

        let tb_full = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 100.0,
            color: [255, 0, 0, 255],
            texture_fill: Some(klipp_timeline::TextureFill {
                enabled: true,
                source: klipp_timeline::TextureSource::Preset(
                    klipp_timeline::TexturePresetId::GradientSweep,
                ),
                opacity: 1.0,
                ..klipp_timeline::TextureFill::default()
            }),
            ..Default::default()
        };
        let td_full = klipp_timeline::TitleData::single_text(tb_full);

        let buf_no = renderer.render(&td_no, frame_w, frame_h);
        let buf_full = renderer.render(&td_full, frame_w, frame_h);
        assert_ne!(
            &buf_no[..],
            &buf_full[..],
            "texture_fill opacity=1 with a non-trivial preset must change the rendered output"
        );
    }

    /// Tint multiply — a green tint on a grayscale gradient should
    /// produce visible green output where the underlying glyph is
    /// covered, vs. no tint (white) which produces gray output.
    #[test]
    fn texture_fill_tint_multiplies_sample_color() {
        let mut renderer = TitleRenderer::new();
        let frame_w: u32 = 640;
        let frame_h: u32 = 480;

        let preset = klipp_timeline::TexturePresetId::GradientSweep;

        let tb_white = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 100.0,
            color: [255, 255, 255, 255],
            texture_fill: Some(klipp_timeline::TextureFill {
                enabled: true,
                source: klipp_timeline::TextureSource::Preset(preset),
                opacity: 1.0,
                tint: [255, 255, 255, 255],
                ..klipp_timeline::TextureFill::default()
            }),
            ..Default::default()
        };
        let td_white = klipp_timeline::TitleData::single_text(tb_white);

        let tb_green = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 100.0,
            color: [255, 255, 255, 255],
            texture_fill: Some(klipp_timeline::TextureFill {
                enabled: true,
                source: klipp_timeline::TextureSource::Preset(preset),
                opacity: 1.0,
                tint: [0, 255, 0, 255],
                ..klipp_timeline::TextureFill::default()
            }),
            ..Default::default()
        };
        let td_green = klipp_timeline::TitleData::single_text(tb_green);

        let buf_white = renderer.render(&td_white, frame_w, frame_h);
        let buf_green = renderer.render(&td_green, frame_w, frame_h);
        // Green-tinted output must zero red + blue channels in
        // glyph pixels (since gradient is grayscale × tint = green).
        let count_pure_red = |buf: &[u8]| -> usize {
            buf.chunks_exact(4)
                .filter(|p| p[3] > 0 && p[0] > 100 && p[1] < 50 && p[2] < 50)
                .count()
        };
        let count_pure_green = |buf: &[u8]| -> usize {
            buf.chunks_exact(4)
                .filter(|p| p[3] > 0 && p[1] > 100 && p[0] < 50 && p[2] < 50)
                .count()
        };
        assert_eq!(
            count_pure_red(&buf_white), 0,
            "white-tinted gradient should not have pure-red pixels"
        );
        assert!(
            count_pure_green(&buf_green) > 0,
            "green-tinted gradient should have pure-green pixels"
        );
    }

    /// Disabled texture fill is a no-op — toggling enabled=false on
    /// an otherwise-active texture must produce identical output to
    /// no texture at all.
    #[test]
    fn texture_fill_disabled_is_noop() {
        let mut renderer = TitleRenderer::new();
        let frame_w: u32 = 640;
        let frame_h: u32 = 480;

        let tb_off = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 100.0,
            color: [200, 50, 50, 255],
            texture_fill: Some(klipp_timeline::TextureFill {
                enabled: false,
                source: klipp_timeline::TextureSource::Preset(
                    klipp_timeline::TexturePresetId::PaperGrain,
                ),
                ..klipp_timeline::TextureFill::default()
            }),
            ..Default::default()
        };
        let td_off = klipp_timeline::TitleData::single_text(tb_off);

        let tb_none = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 100.0,
            color: [200, 50, 50, 255],
            ..Default::default()
        };
        let td_none = klipp_timeline::TitleData::single_text(tb_none);

        let buf_off = renderer.render(&td_off, frame_w, frame_h);
        let buf_none = renderer.render(&td_none, frame_w, frame_h);
        assert_eq!(
            &buf_off[..],
            &buf_none[..],
            "texture_fill enabled=false must render identical to no texture"
        );
    }

    // ────────────────────────────────────────────────────────────
    // Glyph layout hash + cache (used by both inspector picker and
    // engine glyph-anchored focal-point resolver — see
    // HZ-NEXT-LETTER-PICKER + HZ-NEXT-GLYPH-LOCK).
    //
    // Per-field invalidation pin: change ONE layout-affecting
    // field → hash changes. Change a non-layout field (color,
    // glow, etc.) → hash UNCHANGED. Tests document which fields
    // are layout-relevant vs visual-fill — preventing both
    // under-invalidation (cache returns stale glyph rects after
    // a font change) and over-invalidation (cache rebuilds on
    // every colour-drag frame).
    // ────────────────────────────────────────────────────────────

    use klipp_timeline::{FontStyle, StyleRun, TextAlignment, TextBox, TextCase};

    fn glyph_hash_baseline_title() -> klipp_timeline::TitleData {
        let tb = TextBox {
            text: "Hello".to_string(),
            font_name: "Sans".to_string(),
            font_size: 72.0,
            font_style: FontStyle::Regular,
            alignment: TextAlignment::Center,
            ..Default::default()
        };
        klipp_timeline::TitleData::single_text(tb)
    }

    fn glyph_hash_modify_text_box(
        td: &mut klipp_timeline::TitleData,
        modify: impl FnOnce(&mut TextBox),
    ) {
        let tb = td.primary_text_mut().expect("baseline title has a TextBox");
        modify(tb);
    }

    fn baseline_glyph_hash() -> u64 {
        glyph_layout_hash(&glyph_hash_baseline_title(), 1920, 1080)
    }

    fn modify_baseline_and_glyph_hash(modify: impl FnOnce(&mut klipp_timeline::TitleData)) -> u64 {
        let mut td = glyph_hash_baseline_title();
        modify(&mut td);
        glyph_layout_hash(&td, 1920, 1080)
    }

    #[test]
    fn glyph_hash_changes_on_text_edit() {
        let baseline = baseline_glyph_hash();
        let modified = modify_baseline_and_glyph_hash(|td| {
            glyph_hash_modify_text_box(td, |tb| tb.text = "World".into())
        });
        assert_ne!(baseline, modified, "text edit must invalidate cache");
    }

    #[test]
    fn glyph_hash_changes_on_font_name() {
        let baseline = baseline_glyph_hash();
        let modified = modify_baseline_and_glyph_hash(|td| {
            glyph_hash_modify_text_box(td, |tb| tb.font_name = "Mono".into())
        });
        assert_ne!(baseline, modified, "font_name must invalidate cache");
    }

    #[test]
    fn glyph_hash_changes_on_font_style() {
        let baseline = baseline_glyph_hash();
        let modified = modify_baseline_and_glyph_hash(|td| {
            glyph_hash_modify_text_box(td, |tb| tb.font_style = FontStyle::Bold)
        });
        assert_ne!(baseline, modified, "font_style must invalidate cache");
    }

    #[test]
    fn glyph_hash_changes_on_font_size() {
        let baseline = baseline_glyph_hash();
        let modified = modify_baseline_and_glyph_hash(|td| {
            glyph_hash_modify_text_box(td, |tb| tb.font_size = 144.0)
        });
        assert_ne!(baseline, modified, "font_size must invalidate cache");
    }

    #[test]
    fn glyph_hash_changes_on_case_transform() {
        let baseline = baseline_glyph_hash();
        let modified = modify_baseline_and_glyph_hash(|td| {
            glyph_hash_modify_text_box(td, |tb| tb.case_transform = TextCase::Uppercase)
        });
        assert_ne!(baseline, modified, "case_transform must invalidate cache");
    }

    #[test]
    fn glyph_hash_changes_on_alignment() {
        let baseline = baseline_glyph_hash();
        let modified = modify_baseline_and_glyph_hash(|td| {
            glyph_hash_modify_text_box(td, |tb| tb.alignment = TextAlignment::Left)
        });
        assert_ne!(baseline, modified, "alignment must invalidate cache");
    }

    #[test]
    fn glyph_hash_changes_on_tracking() {
        let baseline = baseline_glyph_hash();
        let modified = modify_baseline_and_glyph_hash(|td| {
            glyph_hash_modify_text_box(td, |tb| tb.tracking = 5.0)
        });
        assert_ne!(baseline, modified, "tracking must invalidate cache");
    }

    #[test]
    fn glyph_hash_changes_on_line_spacing() {
        let baseline = baseline_glyph_hash();
        let modified = modify_baseline_and_glyph_hash(|td| {
            glyph_hash_modify_text_box(td, |tb| tb.line_spacing = 1.5)
        });
        assert_ne!(baseline, modified, "line_spacing must invalidate cache");
    }

    #[test]
    fn glyph_hash_changes_on_baseline_shift() {
        let baseline = baseline_glyph_hash();
        let modified = modify_baseline_and_glyph_hash(|td| {
            glyph_hash_modify_text_box(td, |tb| tb.baseline_shift = 20.0)
        });
        assert_ne!(baseline, modified, "baseline_shift must invalidate cache");
    }

    #[test]
    fn glyph_hash_changes_on_style_run_addition() {
        let baseline = baseline_glyph_hash();
        let modified = modify_baseline_and_glyph_hash(|td| {
            glyph_hash_modify_text_box(td, |tb| {
                tb.style_runs.push(StyleRun {
                    start: 0,
                    end: 1,
                    font_name: None,
                    font_size: Some(120.0),
                    color: None,
                    font_style: None,
                    tracking: None,
                    baseline_offset: None,
                });
            })
        });
        assert_ne!(baseline, modified, "style_run addition must invalidate cache");
    }

    #[test]
    fn glyph_hash_changes_on_position() {
        let mut td = glyph_hash_baseline_title();
        let h0 = glyph_layout_hash(&td, 1920, 1080);
        td.objects
            .first_mut()
            .expect("default object exists")
            .transform
            .center = TitlePosition { x: 0.25, y: 0.75 };
        let h1 = glyph_layout_hash(&td, 1920, 1080);
        assert_ne!(h0, h1, "position change must invalidate cache");
    }

    #[test]
    fn glyph_hash_changes_on_frame_dimensions() {
        let td = glyph_hash_baseline_title();
        let h_1080 = glyph_layout_hash(&td, 1920, 1080);
        let h_720 = glyph_layout_hash(&td, 1280, 720);
        assert_ne!(h_1080, h_720, "frame dimensions must invalidate cache");
    }

    #[test]
    fn glyph_hash_does_not_change_on_color_only_edit() {
        // Pin the must-NOT-invalidate path. Colour is fill-only;
        // doesn't move glyphs. If this ever fires, the picker
        // would recompute on every colour-drag frame and the
        // engine cache would thrash too.
        let baseline = baseline_glyph_hash();
        let modified = modify_baseline_and_glyph_hash(|td| {
            glyph_hash_modify_text_box(td, |tb| tb.color = [255, 0, 0, 255])
        });
        assert_eq!(baseline, modified, "color is fill-only; must NOT invalidate");
    }

    #[test]
    fn ensure_glyph_positions_returns_cached_on_second_call() {
        // Cache hit: second call with the same TitleData + dims
        // must NOT re-run the layout pass. We can't observe that
        // directly without instrumenting `compute_char_positions`,
        // but we can pin that the entry is present after the
        // first call and that the layout_hash matches the
        // expected.
        let td = glyph_hash_baseline_title();
        let mut renderer = TitleRenderer::new();
        let mut cache = HashMap::new();
        let cid = klipp_core::ClipId::new();
        let _first = ensure_glyph_positions(&mut cache, cid, &mut renderer, &td, 1920, 1080);
        assert!(cache.contains_key(&cid), "cache populated after first call");
        let entry = cache.get(&cid).unwrap();
        assert_eq!(entry.layout_hash, glyph_layout_hash(&td, 1920, 1080));
        assert_eq!(entry.frame_w, 1920);
        assert_eq!(entry.frame_h, 1080);
    }

    #[test]
    fn ensure_glyph_positions_recomputes_on_layout_hash_mismatch() {
        let mut td = glyph_hash_baseline_title();
        let mut renderer = TitleRenderer::new();
        let mut cache = HashMap::new();
        let cid = klipp_core::ClipId::new();
        let _ = ensure_glyph_positions(&mut cache, cid, &mut renderer, &td, 1920, 1080);
        let hash_before = cache.get(&cid).unwrap().layout_hash;

        // Change a layout-affecting field; recompute.
        glyph_hash_modify_text_box(&mut td, |tb| tb.font_size = 100.0);
        let _ = ensure_glyph_positions(&mut cache, cid, &mut renderer, &td, 1920, 1080);
        let hash_after = cache.get(&cid).unwrap().layout_hash;

        assert_ne!(hash_before, hash_after, "layout hash refreshed after font_size change");
    }

    #[test]
    fn ensure_glyph_positions_recomputes_on_frame_dim_change() {
        let td = glyph_hash_baseline_title();
        let mut renderer = TitleRenderer::new();
        let mut cache = HashMap::new();
        let cid = klipp_core::ClipId::new();
        let _ = ensure_glyph_positions(&mut cache, cid, &mut renderer, &td, 1920, 1080);
        let _ = ensure_glyph_positions(&mut cache, cid, &mut renderer, &td, 1280, 720);
        let entry = cache.get(&cid).unwrap();
        assert_eq!(entry.frame_w, 1280, "cache picked up frame_w change");
        assert_eq!(entry.frame_h, 720, "cache picked up frame_h change");
    }
}

#[cfg(test)]
mod default_title_probe {
    #[test]
    fn a_default_title_is_visible_on_a_portrait_frame() {
        let mut r = super::TitleRenderer::new();
        let td = klipp_timeline::TitleData::default();
        let buf = r.render_with_window(&td, 2160, 3840, None, 1.3, Some(5.0));
        let visible = buf.chunks_exact(4).filter(|p| p[3] > 0).count();
        assert!(visible > 1000, "visible pixels: {visible}");
    }
}
