//! Title / text rasteriser: layout, glyph compositing, and style runs.
//!
//! Given a [`klipp_timeline::TitleData`] — text content, font choice,
//! size, colour, alignment, shadow / outline / glow / gradient /
//! pattern / motion-blur parameters, plus per-character style runs for
//! mixed formatting — this crate produces an RGBA buffer ready for
//! compositing over a video frame.
//!
//! Layout runs through `fontdue` (pure-Rust TTF/OTF rasteriser), each
//! line shaped with OpenType by `harfrust` (`shaping`, #1501), so the
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
//!
//! A character the face lacks falls back on its own (#1503): it is
//! drawn from the first system font that has it (`fallback`), else
//! from the embedded face or a bundled family. Colour emoji in CBDT
//! bitmap fonts draw in their colours (`colour`).

mod colour;
pub mod raster;
pub use raster::BoxRaster;
mod fallback;
mod shaping;
mod text_on_path;
pub mod text3d_mesh;
pub mod text3d_triangles;
pub mod prepared_3d;
pub mod text3d_render;
pub use text_on_path::PathUnits;
mod ruby;
mod vertical;
mod gpu_styles;
mod gpu_glyphs;
mod outline_2d;
pub use gpu_glyphs::{GlyphDraw, GlyphSource};
pub use colour::colour_bitmap_rasterizations;
pub use gpu_styles::{StyleFaceWork, StyleSource};
mod tracking;
pub mod variable;
mod vector_path;
pub use vector_path::{keys_path_geometry, path_coverage, PathCoverage};
use fontdue::layout::{HorizontalAlign, LayoutSettings, VerticalAlign, WrapStyle};
use fontdue::Font;
use shaping::{Face, LoadedFont, ShapedLayout};
pub use shaping::SHAPING_VERSION;
pub use fallback::FALLBACK_VERSION;
use klipp_timeline::{LayerFx, TitleObject, TitleObjectId};
use klipp_timeline::{
    FontStyle, ImageFit, ImageLayer, LastLineAlign, ObjectTransform, Shape, ShapeKind, StyleRun,
    TextAlignment, TextDirection, TextPath, TextPattern, TextVerticalAlign, TitleBackground, TitleData,
    TitleObjectContent,
};
use klipp_core::title_motion::{FocusBand, RampAxis, ScreenFade};
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

/// How much bigger than on a 1080p frame everything a title measures in
/// pixels is drawn: sizes, strokes, shadows, padding. By the frame's short
/// side, so a size reads the same on a 9:16 frame as on a 16:9 one. By the
/// height, a 2160x3840 vertical frame drew its text 3.56 times as large on
/// a frame 2160 wide, where 2 was meant: a `size: 120` headline filled the
/// width and broke inside its words (#460, #508).
pub fn frame_scale(width: u32, height: u32) -> f32 {
    klipp_core::title_motion::frame_scale(width as f32, height as f32)
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
    /// Scale factor, [`frame_scale`].
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
    /// The colour glyphs (emoji) drawn, in their own colours over the
    /// text fill (#1503); their coverage is in the mask too.
    pub colour_glyphs: Vec<ColourGlyph>,
    /// For a text on a drawn path, each glyph's place along the text and
    /// on the path at rest (#1697): what poses it as the path moves.
    pub path_units: Option<PathUnits>,
    /// Untracked X and frame-space Y before pixel rounding, for spacing animation.
    pub tracking_origins: Vec<[f32; 2]>,
}

impl TitleLayout {
    fn bounds_only(bounds: [f32; 4], glyph_count: usize, scale: f32) -> Self {
        Self {alpha_mask:Vec::new(),mask_w:0,mask_h:0,mask_x0:0,mask_y0:0,
            text_left:bounds[0],text_top:bounds[1],text_right:bounds[2],text_bottom:bounds[3],glyph_count,scale,
            glyph_colors:Vec::new(),glyph_coverages:Vec::new(),glyph_index:Vec::new(),glyph_boxes:Vec::new(),glyph_parents:Vec::new(),pad:0,
            colour_glyphs:Vec::new(),path_units:None,tracking_origins:Vec::new()}
    }
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
    /// The text's places along a drawn path, when it is on one (#1697).
    pub path: Option<PathUnits>,
    /// Untracked X and frame-space Y before pixel rounding.
    pub tracking_origins: Vec<[f32; 2]>,
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
        Self { x0, y0, w, h, index, boxes: layout.glyph_boxes.clone(), parents: layout.glyph_parents.clone(), pad: layout.pad, path: layout.path_units.clone(), tracking_origins: layout.tracking_origins.clone() }
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
    /// Turned clockwise by this many radians about `anchor`, scaled
    /// first (an animator's, #1510); 0 for none.
    pub rotation: f32,
}

impl GlyphPose {
    /// The sixteen numbers the GPU pass reads per glyph.
    pub fn packed(&self) -> [f32; 16] {
        let (c0, c1) = self.clip.map_or((0.0, -1.0), |(a, b)| (a as f32, b as f32));
        [
            self.moved[0] as f32, self.moved[1] as f32, self.moved[2] as f32, self.moved[3] as f32,
            self.dx, self.dy, self.scale, self.alpha,
            self.anchor.0, self.anchor.1, c0, c1,
            self.rotation, 0.0, 0.0, 0.0,
        ]
    }
}

/// A resting render and, for a title whose motion staggers, the map of
/// which glyph each pixel is: what `render_resting` makes and `adopt`
/// takes.
pub struct RestingRender {
    pub pixels: BoxRaster,
    pub units: Option<std::sync::Arc<UnitMap>>,
}

/// A colour glyph of a [`TitleLayout`] (#1503): its box's top-left in
/// frame pixels, its size, and its straight (not premultiplied) RGBA.
#[derive(Debug, Clone)]
pub struct ColourGlyph {
    pub x: i32,
    pub y: i32,
    pub width: usize,
    pub height: usize,
    pub rgba: std::sync::Arc<Vec<u8>>,
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
    /// The character reads right to left (#1693): the caret before it
    /// is at its right edge, after it at its left.
    pub rtl: bool,
    /// The character is set in a column (#1704): `(x, y)` is its cell's
    /// top left, `advance` runs down the column and `height` across it;
    /// the caret before it lies across its top, after it across its foot.
    pub vertical: bool,
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
    buffer: std::sync::Arc<BoxRaster>,
    /// The rows with any coverage, `y0..y1`.
    rows: (u32, u32),
    /// The columns with any coverage, `x0..x1`.
    cols: (u32, u32),
    units: Option<std::sync::Arc<UnitMap>>,
    /// Names this render to whoever keeps a copy of it elsewhere (a
    /// GPU sprite): never reused within a renderer.
    id: u64,
}

/// A title's resting render, as cached: the box it covers as
/// straight-alpha sRGB half floats (#190), the rows and columns with any
/// coverage, and an id that names it.
#[derive(Clone)]
pub struct RestingSprite {
    pub pixels: std::sync::Arc<BoxRaster>,
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
    /// bytes are the resting render shifted and faded; a compositor that
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
        /// The layer's fade by where it is on the frame: its edge fade
        /// and a roll's focus band (#592).
        shape: Option<ScreenFade>,
        rows: (u32, u32),
    },
    /// The resting render's glyphs each placed on its own: a motion
    /// that staggers by character, word or line. The frame's bytes are
    /// those `pose_sprite_glyphs` makes from the sprite's `units`.
    Glyphs { sprite: RestingSprite, poses: Vec<GlyphPose>, rows: (u32, u32) },
    /// Rendered for this time alone.
    Rendered { pixels: BoxRaster, rows: (u32, u32) },
    /// Text layers whose only change is keyed opacity or a whole-layer fade, a
    /// lower third's lines fading in one after another: each layer's
    /// resting render with its alpha, laid in order, each over the ones
    /// before it (#2069). The frame's bytes are those [`compose_layers`]
    /// makes; the GPU stack places the sprites where it composites.
    Layers { layers: Vec<(RestingSprite, f32)>, rows: (u32, u32) },
}

/// The title whose resting render is the sprite: a full-frame card's
/// words alone (its fill goes under them when the sprite is placed);
/// any other title as it is.
pub fn resting_title(title: &TitleData) -> std::borrow::Cow<'_, TitleData> {
    if card_goes_under(title) {
        let mut words = title.clone();
        words.background = TitleBackground::None;
        std::borrow::Cow::Owned(words)
    } else {
        std::borrow::Cow::Borrowed(title)
    }
}

/// Whether a full-frame card's fill is laid under the sprite when it is
/// placed rather than drawn into it. Not for a title whose layers mix
/// (#1513): a layer that multiplies or screens mixes with the fill
/// below it, so the fill is part of the render.
fn card_goes_under(title: &TitleData) -> bool {
    matches!(title.background, TitleBackground::FullFrame { .. }) && !title.mixes_layers()
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

/// [`alpha_rows`] of a layer stack in byte units: a pixel carries alpha
/// when it would round to at least one level.
fn stack_rows(stack: &[f32], width: u32, height: u32) -> (u32, u32) {
    use rayon::prelude::*;
    let row = (width as usize) * 4;
    if row == 0 || stack.len() < row * height as usize {
        return (0, 0);
    }
    let (lo, hi) = stack
        .par_chunks(row)
        .enumerate()
        .filter(|(_, r)| r.chunks_exact(4).any(|p| p[3] >= 0.5))
        .map(|(y, _)| (y as u32, y as u32 + 1))
        .reduce(|| (u32::MAX, 0), |a, b| (a.0.min(b.0), a.1.max(b.1)));
    if lo == u32::MAX { (0, 0) } else { (lo, hi) }
}

/// [`alpha_cols`] of a layer stack in byte units.
fn stack_cols(stack: &[f32], width: u32, height: u32, rows: (u32, u32)) -> (u32, u32) {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || stack.len() < w * h * 4 || rows.1 <= rows.0 {
        return (0, 0);
    }
    let (mut lo, mut hi) = (w, 0usize);
    for y in rows.0 as usize..(rows.1 as usize).min(h) {
        let row = &stack[y * w * 4..(y + 1) * w * 4];
        if let Some(first) = row.chunks_exact(4).position(|p| p[3] >= 0.5) {
            lo = lo.min(first);
            let last = row.chunks_exact(4).rposition(|p| p[3] >= 0.5).unwrap_or(first);
            hi = hi.max(last + 1);
        }
    }
    if hi <= lo { (0, 0) } else { (lo as u32, hi as u32) }
}

/// The rows a raster's box covers, `y0..y1`.
fn raster_rows(r: &BoxRaster) -> (u32, u32) {
    if r.is_empty() { (0, 0) } else { (r.y0, r.y0 + r.h) }
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
    fallback_font: LoadedFont,
    font_cache: HashMap<String, LoadedFont>,
    /// Shared registry of available system fonts. Cheap to clone — internally
    /// an `Arc` populated by `font_discovery::discover_system_fonts()`, which
    /// dedupes across the whole process and caches the result on disk.
    font_registry: font_discovery::FontRegistry,
    /// Cached texture fill image: (path, rgba_data, width, height)
    texture_fill_cache: Option<(String, Vec<u8>, u32, u32)>,
    gpu_textures:std::collections::VecDeque<(klipp_timeline::TextureSource,u64,std::sync::Arc<BoxRaster>)>,
    /// Small LRU of recently-rasterized titles. Capped at
    /// `RENDER_CACHE_CAP` so memory stays bounded even when many
    /// title clips are visited (e.g. scrubbing across a long credits
    /// sequence).
    render_cache: Vec<CachedRender>,
    /// The text layers of several-layer titles, as rendered at rest
    /// (#2095): a title whose other layer animates (a line drawing itself
    /// on, its trim keyed) is rendered afresh each frame, and its text,
    /// unchanged, was rasterised with it every time, 50 ms at 4K. Kept as
    /// the box the layer covers, by the one-text title it was rendered
    /// from and the frame size.
    text_layer_cache: Vec<CachedTextLayer>,
    /// How many text layers of several-layer titles were rendered rather
    /// than taken from `text_layer_cache`: what a test reads.
    text_layers_rendered: u64,
    /// Resting renders made rather than taken from `render_cache`.
    resting_renders: u64,
    /// Recently used unfinished faces; bounded separately and invalidated with raster caches.
    style_source_cache: Vec<gpu_styles::SourceCache>,
    gpu_layout_only: bool,
    layout_bounds_only: bool,
    gpu_outline_only: bool,
    gpu_bitmap_size: bool,
    gpu_wave: Option<(f32,f32,f32)>,
    gpu_motion:Option<(f32,Option<f32>)>,
    gpu_layout_capture: std::cell::RefCell<Option<GlyphSource>>,
    gpu_glyph_cache: std::cell::RefCell<gpu_glyphs::GlyphCache>,
    /// The rows with coverage in the last render, `y0..y1`.
    last_rows: (u32, u32),
    /// The blur the last `Placed` frame asked for, so a caller that
    /// took bytes can apply it.
    last_blur: f32,
    /// A placed frame's focus band gone soft: (blur radius in frame
    /// pixels, axis, band), for the caller to blur the placed layer by.
    last_focus_blur: Option<(f32, RampAxis, f32)>,
    /// The solid point of the last resting sprite a motion scaled about
    /// (#787): the sprite is fixed while it moves, so the box scan and
    /// distance pass run once per sprite, not once a frame.
    solid_point: Option<(u64, Option<(f32, f32)>)>,
    /// The unit map the last `render_stack` made, for `cache_store`.
    last_units: Option<std::sync::Arc<UnitMap>>,
    /// The glyphs the last layout of a 3D text placed (#1512): what
    /// `render_text_layer` builds the solids from. Filled while the
    /// layout's faces borrow the renderer, so behind a lock.
    last_glyphs3d: std::sync::Mutex<crate::text3d_render::Placed3d>,
    /// The next `CachedRender::id`.
    next_cache_id: u64,
    geometry3d_cache: std::collections::VecDeque<prepared_3d::GeometryCache>,
    /// The clip's length while rendering with `render_with_window`,
    /// which exit motion counts back from. `None` outside a clip.
    motion_window: Option<f32>,
    /// Per-glyph wave of the layer being rendered, from its `Wave`
    /// loop: (amplitude px, rate Hz). Set by `render_stack` per object.
    loop_wave: Option<(f32, f32)>,
    /// The waves of every layer of the title being rendered, by
    /// object id; filled by `render_with_time`.
    loop_waves: HashMap<u32, (f32, f32)>,
    /// The drawn path the text layer being rendered follows, resolved
    /// from its title by `render_stack` (#1508).
    bound_path: Option<text_on_path::PathLine>,
    /// A roll's focus band per layer for the frame being rastered,
    /// applied with the layer's edge fade where it has been placed (#592).
    loop_focus: HashMap<u32, (RampAxis, FocusBand)>,
    /// The resting render of a one-layer title leaves its fade to the
    /// placement, which knows where the layer has moved to.
    defer_layer_fades: bool,
    /// The one layer `render_stack` draws while it makes a matte: the
    /// mask source alone, shown or not, at the masked layer's time
    /// (#1513).
    matte_only: Option<TitleObjectId>,
    /// Decoded image layers by path; `None` for a file that would
    /// not open, so it is not retried every frame.
    image_cache: HashMap<String, Option<std::sync::Arc<DecodedImage>>>,
    /// Whether a character a face lacks is looked for in the system's
    /// fonts before the ones that ship with the app (#1503); off in
    /// tests that must draw the same on every machine.
    system_fallback: bool,
    /// The font that draws a character for a font that lacks it, by
    /// (font key, character, as emoji), from [`Self::fallback_key`].
    fallback_keys: HashMap<(String, char, bool), Option<String>>,
}

/// Resting renders kept: one for each title of a busy stretch, as the
/// text layers are, the bytes bounded by `RENDER_CACHE_BYTES`. Four,
/// with eight titles on screen, put each one out of the cache before it
/// came round again, and every title was rendered afresh on the CPU
/// every frame: 60 ms at 4K (#2069).
const RENDER_CACHE_CAP: usize = 16;

/// How many text layers [`TitleRenderer`] keeps at rest (#2095): one for
/// each title of a busy stretch, a dozen at most.
const TEXT_LAYER_CACHE_CAP: usize = 12;

/// A text layer rendered at rest, cropped to the box it covers (#2095).
struct CachedTextLayer {
    /// The one-text title it was rendered from.
    title: TitleData,
    width: u32,
    height: u32,
    /// The box: `x0, y0, w, h`, and its RGBA rows.
    kept: std::sync::Arc<KeptLayer>,
    /// The glyph map the render left (`last_units`).
    units: Option<std::sync::Arc<UnitMap>>,
}

/// A text layer's box (`x0, y0, w, h`) and its RGBA rows, at rest.
struct KeptLayer {
    x0: usize,
    y0: usize,
    w: usize,
    h: usize,
    crop: Vec<u8>,
}

impl KeptLayer {
    /// The frame-sized layer it was cut from.
    fn layer(&self, width: u32, height: u32) -> Vec<u8> {
        let mut layer = vec![0u8; (width as usize) * (height as usize) * 4];
        let row = self.w * 4;
        for y in 0..self.h {
            let at = ((self.y0 + y) * width as usize + self.x0) * 4;
            layer[at..at + row].copy_from_slice(&self.crop[y * row..(y + 1) * row]);
        }
        layer
    }

    /// Written into `stack`, a frame of zeros in the stack's units, as the
    /// first layer is: its box alone, the rest of the frame left as it is,
    /// its alpha scaled by `alpha` (a fade) as `scale_layer_alpha` scales
    /// a layer's.
    fn write_into_stack(&self, stack: &mut [f32], width: u32, alpha: f32) {
        let f = alpha.clamp(0.0, 1.0);
        let row = self.w * 4;
        for y in 0..self.h {
            let at = ((self.y0 + y) * width as usize + self.x0) * 4;
            for (d, s) in stack[at..at + row].chunks_exact_mut(4).zip(self.crop[y * row..(y + 1) * row].chunks_exact(4)) {
                d[0] = s[0] as f32;
                d[1] = s[1] as f32;
                d[2] = s[2] as f32;
                d[3] = if f >= 0.9999 { s[3] as f32 } else { ((s[3] as f32) * f).round().clamp(0.0, 255.0) };
            }
        }
    }
}
/// The most the cache's rasters take together (#190): a guard, since a
/// title's box is usually far smaller than its frame. Three full-frame
/// 4K cards in half floats fit; the newest render is always kept.
const RENDER_CACHE_BYTES: usize = 200 << 20;

impl Default for TitleRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl TitleRenderer {
    pub fn new() -> Self {
        let registry = font_discovery::discover_system_fonts();
        tracing::info!("Font discovery: found {} font families", registry.len());
        Self::with_font_registry(registry)
    }

    /// Render with an explicit font registry, sharing the same embedded fallback
    /// and caches as the system-discovery constructor.
    pub fn with_font_registry(registry: font_discovery::FontRegistry) -> Self {
        let fallback_font = LoadedFont::new(std::borrow::Cow::Borrowed(EMBEDDED_FONT))
            .expect("Failed to load embedded Noto Sans font");
        Self {
            fallback_font,
            font_cache: HashMap::new(),
            font_registry: registry,
            texture_fill_cache: None,
            gpu_textures:Default::default(),
            render_cache: Vec::with_capacity(RENDER_CACHE_CAP),
            text_layer_cache: Vec::new(),
            text_layers_rendered: 0,
            resting_renders: 0,
            style_source_cache: Vec::new(),
            gpu_layout_only: false,
            layout_bounds_only: false,
            gpu_outline_only: false,
            gpu_bitmap_size: false,
            gpu_wave: None,
            gpu_motion:None,
            gpu_layout_capture: Default::default(),
            gpu_glyph_cache: Default::default(),
            last_rows: (0, 0),
            last_blur: 0.0,
            last_focus_blur: None,
            solid_point: None,
            last_units: None,
            last_glyphs3d: std::sync::Mutex::new(Default::default()),
            next_cache_id: 1,
            geometry3d_cache: Default::default(),
            motion_window: None,
            loop_wave: None,
            loop_waves: HashMap::new(),
            bound_path: None,
            loop_focus: HashMap::new(),
            defer_layer_fades: false,
            matte_only: None,
            image_cache: HashMap::new(),
            system_fallback: true,
            fallback_keys: HashMap::new(),
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
            if let Some(font) = LoadedFont::new(std::borrow::Cow::Borrowed(bytes)) {
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
                    if let Some(font) = LoadedFont::new(std::borrow::Cow::Owned(data)) {
                        self.font_cache.insert(key.clone(), font);
                    }
                }
            }
        }

        key
    }

    /// Ensure the face a text box or a style run names is loaded
    /// (#1504). Naming no weight and no axes, its family in `style`, as
    /// [`Self::ensure_font_loaded`] loads it. Else the family's face
    /// nearest the weight (`style`'s, 700 or 400, when none is named)
    /// in `style`'s slope, picked as CSS picks one, at the axes named,
    /// with a variable face's weight axis at the weight named. The
    /// faces that ship with the app are a bundled family's only faces.
    /// Returns the cache key.
    fn ensure_face_loaded(&mut self, family: &str, style: FontStyle, weight: Option<u16>, axes: &[klipp_timeline::FontAxis]) -> String {
        if weight.is_none() && axes.is_empty() {
            return self.ensure_font_loaded(family, style);
        }
        let key = format!("{}::{weight:?}::{axes:?}", Self::cache_key(family, style));
        if family == "sans-serif" || family.is_empty() || self.font_cache.contains_key(&key) {
            return key;
        }
        let bold = matches!(style, FontStyle::Bold | FontStyle::BoldItalic);
        let italic = matches!(style, FontStyle::Italic | FontStyle::BoldItalic);
        let wanted = weight.unwrap_or(if bold { 700 } else { 400 });
        let unread = || variable::FaceTraits { weight: 400, italic: false, axes: Vec::new() };
        let bundled: Vec<&'static [u8]> = bundled::BUNDLED.iter().filter(|f| f.family == family).map(|f| f.bytes).collect();
        let picked = if !bundled.is_empty() {
            let traits: Vec<_> = bundled.iter().map(|b| variable::face_traits(b).unwrap_or_else(unread)).collect();
            variable::nearest_face(&traits, wanted, italic).map(|i| (std::borrow::Cow::Borrowed(bundled[i]), traits[i].clone()))
        } else if let Some(infos) = self.font_registry.get(family) {
            let traits: Vec<_> = infos.iter().map(|i| i.traits()).collect();
            variable::nearest_face(&traits, wanted, italic)
                .and_then(|i| std::fs::read(&infos[i].path).ok().map(|data| (std::borrow::Cow::Owned(data), traits[i].clone())))
        } else {
            None
        };
        if let Some((data, traits)) = picked {
            // The axes named win over the weight's.
            let mut variations: Vec<klipp_timeline::FontAxis> = Vec::new();
            if let Some(w) = weight.filter(|_| traits.axes.iter().any(|a| a.tag == "wght")) {
                variations.push(("wght".into(), w as f32));
            }
            variations.extend(axes.iter().cloned());
            if let Some(font) = LoadedFont::at(data, &variations) {
                self.font_cache.insert(key.clone(), font);
            }
        }
        key
    }

    /// The faces a text box is set in (#1505): its own font first, then
    /// each other family and style its style runs name, every one
    /// loaded and resolved as the box's own font is. A run with its own
    /// OpenType features is a face of its own, shaped with the box's
    /// features and then its (#1501). A run's weight is its own, else
    /// the box's unless the run's style is bold where the box's is not
    /// or the other way round; its axes are set on top of the box's
    /// (#1504).
    /// `tb` with its superscript and subscript runs set as the layout
    /// sets any run (#1721): in the run's face's `sups` or `subs` where it
    /// has the feature; else at 62% of the run's size, raised a third of
    /// that size or lowered a seventh.
    fn with_scripts(&self, mut tb: klipp_timeline::TextBox) -> klipp_timeline::TextBox {
        if tb.style_runs.iter().all(|r| r.script.is_none()) {
            return tb;
        }
        let (family, base_style, base_size) = (tb.font_name.clone(), tb.font_style, tb.font_size);
        for run in tb.style_runs.iter_mut() {
            let Some(script) = run.script else { continue };
            let style = match run.font_style.unwrap_or(base_style) {
                FontStyle::Regular => "Regular",
                FontStyle::Bold => "Bold",
                FontStyle::Italic => "Italic",
                FontStyle::BoldItalic => "Bold Italic",
            };
            let (feature, rise) = match script {
                klipp_timeline::Script::Superscript => ("sups", 0.33),
                klipp_timeline::Script::Subscript => ("subs", -0.14),
            };
            let tags = font_discovery::family_feature_tags(&self.font_registry, run.font_name.as_deref().unwrap_or(&family), style);
            if tags.iter().any(|t| t == feature) {
                if !run.features.iter().any(|f| f == feature) {
                    run.features.push(feature.into());
                }
            } else {
                let size = run.font_size.unwrap_or(base_size);
                run.font_size = Some(size * 0.62);
                run.baseline_offset = Some(run.baseline_offset.unwrap_or(0.0) + size * rise);
            }
        }
        tb
    }

    fn run_faces(&mut self, tb: &klipp_timeline::TextBox) -> RunFaces {
        let mut keys = vec![(self.ensure_face_loaded(&tb.font_name, tb.font_style, tb.font_weight, &tb.font_axes), tb.features.clone())];
        let bold = |s: FontStyle| matches!(s, FontStyle::Bold | FontStyle::BoldItalic);
        let of_run = tb.style_runs.iter().map(|run| {
            if run.font_name.is_none() && run.font_style.is_none() && run.font_weight.is_none() && run.font_axes.is_empty() && run.features.is_empty() {
                return 0;
            }
            let family = run.font_name.as_deref().unwrap_or(&tb.font_name);
            let style = run.font_style.unwrap_or(tb.font_style);
            let weight = run.font_weight.or(tb.font_weight.filter(|_| bold(style) == bold(tb.font_style)));
            let axes: Vec<klipp_timeline::FontAxis> = tb.font_axes.iter().chain(&run.font_axes).cloned().collect();
            let font = self.ensure_face_loaded(family, style, weight, &axes);
            let key = (font, tb.features.iter().chain(&run.features).cloned().collect());
            keys.iter().position(|k| *k == key).unwrap_or_else(|| {
                keys.push(key);
                keys.len() - 1
            })
        }).collect();
        RunFaces { keys, of_run, fallback: HashMap::new() }
    }

    /// Give each character of `text` whose face lacks it the face that
    /// draws it instead (#1503): the first in the chain the face's
    /// [`Self::fallback_key`] walks. The fallback face is shaped with
    /// the features of the face it stands in for. A control character,
    /// and one that joins the character before it, takes no face of its
    /// own. Text its faces cover changes nothing.
    fn fall_back(&mut self, faces: &mut RunFaces, runs: &[StyleRun], text: &str) {
        let mut chars = text.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            if c.is_control() || fallback::joins_previous(c) {
                continue;
            }
            let face = faces.at(runs, i);
            if faces.fallback.contains_key(&(face, c)) || self.face_has(&faces.keys[face].0, c) {
                continue;
            }
            let emoji = fallback::wants_emoji(c, chars.peek().map(|&(_, n)| n));
            let Some(key) = self.fallback_key(&faces.keys[face].0, c, emoji) else { continue };
            let entry = (key, faces.keys[face].1.clone());
            let index = faces.keys.iter().position(|k| *k == entry).unwrap_or_else(|| {
                faces.keys.push(entry);
                faces.keys.len() - 1
            });
            faces.fallback.insert((face, c), index);
        }
    }

    /// Whether the font loaded as `key` (the embedded fallback for a
    /// key that did not load) has a glyph for `c`.
    fn face_has(&self, key: &str, c: char) -> bool {
        self.font_cache.get(key).unwrap_or(&self.fallback_font).font.lookup_glyph_index(c) != 0
    }

    /// The cache key of the font that draws `c` for the font `key`,
    /// which lacks it (#1503): the first system font that has it (an
    /// emoji font first when `emoji`), else the embedded Noto Sans,
    /// else the first bundled family's Regular that has it; `None` when
    /// no face does, and the face draws its missing glyph. Kept per
    /// font and character.
    fn fallback_key(&mut self, key: &str, c: char, emoji: bool) -> Option<String> {
        let cache_key = (key.to_string(), c, emoji);
        if let Some(found) = self.fallback_keys.get(&cache_key) {
            return found.clone();
        }
        let mut found = None;
        if self.system_fallback {
            if let Some(path) = fallback::system_font_for(c, emoji) {
                let k = format!("fallback::{}", path.display());
                if !self.font_cache.contains_key(&k) {
                    if let Some(font) = std::fs::read(&path).ok().and_then(|data| LoadedFont::new(std::borrow::Cow::Owned(data))) {
                        self.font_cache.insert(k.clone(), font);
                    }
                }
                if self.font_cache.contains_key(&k) && self.face_has(&k, c) {
                    found = Some(k);
                }
            }
        }
        if found.is_none() {
            // The embedded face: a key never loaded into the cache.
            let embedded = Self::cache_key("sans-serif", FontStyle::Regular);
            if self.face_has(&embedded, c) {
                found = Some(embedded);
            }
        }
        if found.is_none() {
            for f in bundled::BUNDLED.iter().filter(|f| f.style == "Regular") {
                let k = self.ensure_font_loaded(f.family, FontStyle::Regular);
                if self.font_cache.contains_key(&k) && self.face_has(&k, c) {
                    found = Some(k);
                    break;
                }
            }
        }
        let found = found.filter(|k| k != key);
        self.fallback_keys.insert(cache_key, found.clone());
        found
    }

    /// The faces for `keys`, each its loaded font (the fallback font
    /// for a key that did not load) and its features.
    fn fonts_of(&self, keys: &[(String, Vec<String>)]) -> Vec<Face<'_>> {
        keys.iter().map(|(k, features)| Face::new(self.font_cache.get(k).unwrap_or(&self.fallback_font), features)).collect()
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
        let tb = self.with_scripts(title.primary_text_or_default());
        let w = width as usize;
        let h = height as usize;
        // A text on a drawn path follows the path its layer resolved, or,
        // laid from a whole title, the one it names there (#1508).
        let path_line = match &tb.text_path {
            Some(TextPath::Layer { .. }) => self.bound_path.clone().or_else(|| self.bound_path_line(title, title, time_secs, width, height)),
            _ => None,
        };
        // One whose path is gone is laid as text that follows none.
        let on_path = match &tb.text_path {
            Some(TextPath::Layer { .. }) => path_line.is_some(),
            other => other.is_some(),
        };

        // Resolve the fonts: the box's own, then every other face its
        // style runs name (#1505), each loaded into the cache if needed.
        let mut faces = self.run_faces(&tb);

        // Scale font size to the frame's short side (72pt at 1080p).
        // SmallCaps renders at 80 % of the configured size so the
        // upper-cased glyphs read like smallcaps even on fonts
        // without dedicated smallcaps glyph variants.
        let scale = crate::frame_scale(width, height);
        let case_scale = match tb.case_transform {
            klipp_timeline::TextCase::SmallCaps => 0.80,
            _ => 1.0,
        };
        let font_px = tb.font_size * scale * case_scale;

        // Lay out text
        let h_align = h_align_of(tb.alignment);

        let mut layout = ShapedLayout::new();
        layout.set_rtl(tb.text_direction == TextDirection::Rtl);
        layout.set_paragraphs(paragraphs_px(&tb, scale));
        layout.reset(&LayoutSettings {
            x: 0.0,
            y: 0.0,
            // Text on a path is one line, as long as the text (#1508).
            max_width: (!on_path).then(|| text_box_extent(title, width, height).0),
            max_height: Some(text_box_extent(title, width, height).1),
            horizontal_align: h_align,
            vertical_align: VerticalAlign::Top,
            wrap_style: WrapStyle::Word,
            // The layout spaces its own lines, by each line's tallest
            // type: the box's spacing is a multiple of that (#1676).
            line_height: tb.line_spacing,
            ..LayoutSettings::default()
        });

        // Resolve the laid-out text, honouring the pre-layout case
        // transform. The `text` field on TextBox is preserved verbatim
        // so the user can flip the treatment without re-typing.
        let resolved_text: String = match tb.case_transform {
            klipp_timeline::TextCase::None => tb.text.clone(),
            klipp_timeline::TextCase::Uppercase
            | klipp_timeline::TextCase::SmallCaps => tb.text.to_uppercase(),
            klipp_timeline::TextCase::Lowercase => tb.text.to_lowercase(),
        };
        // Right to left is the paragraph's base direction, which the
        // layout's bidi algorithm orders and shapes the text by (#1502);
        // the text stays in its logical order.
        // A character its face lacks is drawn from a face that has it,
        // measured and laid in that face (#1503).
        self.fall_back(&mut faces, &tb.style_runs, &resolved_text);
        let fonts = self.fonts_of(&faces.keys);
        let (box_w, box_h) = text_box_extent(title, width, height);
        let face_at = |i: usize| -> &Font { &fonts[faces.drawn(&tb.style_runs, &resolved_text, i)] };
        let shaped_widest = |ratio: f32| {
            let run_px = |run: &StyleRun| run.font_size.unwrap_or(tb.font_size) * scale * case_scale * ratio;
            let pieces = faces.split(&resolved_text, set_pieces(&tb.style_runs, &faces.of_run, 0, resolved_text.len(), font_px * ratio, run_px));
            shaping::widest_typed_line(&fonts, &resolved_text, &pieces)
        };
        let (fit_w, fit_h) = paragraph_room(&tb, &resolved_text, scale, box_w, box_h);
        let fit = box_fit_ratio(&tb, face_at, &resolved_text, |i| run_px_at(&tb.style_runs, i, font_px, scale * case_scale), shaped_widest, fit_w, fit_h);
        let font_px = font_px * fit;
        // The text from one byte to another, cut where its style runs
        // begin and end, each piece at its run's size and face (#1505).
        let run_px = |run: &StyleRun| run.font_size.unwrap_or(tb.font_size) * scale * case_scale * fit;
        let pieces_of = |from: usize, to: usize| faces.split(&resolved_text, set_pieces(&tb.style_runs, &faces.of_run, from, to, font_px, run_px));

        // Drop cap: when `drop_cap_scale >= 1.0` we enlarge the first
        // character. The straightforward "drop initial" path appends
        // the cap as a head-segment of the main layout — body text
        // continues on the same baseline. The "wrap-under" path
        // (alignment = Left) builds the layout from
        // three sub-layouts so the body wraps to the right of the
        // cap for the cap's vertical extent and re-flows at full
        // width below it.
        let drop_cap_active = tb.drop_cap_scale >= 1.0 && !resolved_text.is_empty();
        let drop_cap_byte_end = if drop_cap_active {
            resolved_text.chars().next().unwrap().len_utf8()
        } else { 0 };
        let body_start = drop_cap_byte_end;
        // A vertical drop cap is set with the columns (#1704).
        let drop_cap_wrap_eligible = drop_cap_active
            && tb.alignment == TextAlignment::Left && !tb.vertical_layout;
        // The cap is set in the face of the run over it, if any (#1505),
        // or the face it falls back to (#1503).
        let cap_face = faces.drawn(&tb.style_runs, &resolved_text, 0);

        // Every glyph's byte offset indexes `resolved_text`: each piece
        // is laid as its byte range of it.

        // Pre-built glyph list for the drop-cap wrap path. Empty when
        // we use fontdue's normal Layout flow.
        let mut drop_cap_wrap_glyphs: Vec<fontdue::layout::GlyphPosition> = Vec::new();

        // Vertical text (#1507): set top to bottom in columns, from the
        // right unless the box says from the left, in vertical forms with
        // sideways runs turned; a drop cap at the top of the first
        // column, the body beside and below it (#1704).
        let vertical_active = tb.vertical_layout;
        let mut vertical_glyphs: Vec<fontdue::layout::GlyphPosition> = Vec::new();
        let mut vertical_starts: Vec<usize> = Vec::new();
        if vertical_active {
            let cap = drop_cap_active.then_some((drop_cap_byte_end, font_px * tb.drop_cap_scale, cap_face));
            (vertical_glyphs, vertical_starts, _) = lay_vertical(&fonts, &resolved_text, &tb, font_px, scale, box_h, pieces_of, cap);
        }

        // Multi-column body layout. When `columns >= 2` we split the
        // body text by character count on word boundaries and lay
        // each column out separately, then translate columns into
        // place horizontally. Drop caps disable this path (single-
        // column fall-through preserves their semantics); style runs
        // set their pieces of each column (#1505).
        let columns_n = tb.columns.max(1) as usize;
        let columns_active = columns_n >= 2
            && !drop_cap_active && !vertical_active;
        let mut columns_glyphs: Vec<fontdue::layout::GlyphPosition> = Vec::new();
        // Where each line of the glyphs starts, for the drop-cap wrap
        // and column paths (#1505).
        let mut path_line_starts: Vec<usize> = Vec::new();
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
                let mut col_layout = ShapedLayout::new();
                col_layout.set_rtl(tb.text_direction == TextDirection::Rtl);
                col_layout.reset(&LayoutSettings {
                    x: column_x,
                    y: 0.0,
                    max_width: Some(col_w),
                    horizontal_align: h_align,
                    vertical_align: VerticalAlign::Top,
                    wrap_style: WrapStyle::Word,
                    line_height: tb.line_spacing,
                    ..LayoutSettings::default()
                });
                let col_glyphs = lay_pieces(&mut col_layout, &fonts, &resolved_text, &pieces_of(byte_offset, byte_offset + chunk.len()));
                if let Some(lines) = col_layout.lines() {
                    path_line_starts.extend(lines.iter().map(|l| columns_glyphs.len() + l.glyph_start));
                }
                columns_glyphs.extend(col_glyphs);
                byte_offset += chunk.len();
            }
        }

        if drop_cap_wrap_eligible {
            // Build the layout in three passes so body text wraps
            // properly around the drop cap. See `layout_drop_cap_wrap`.
            let cap_size = font_px * tb.drop_cap_scale;
            (drop_cap_wrap_glyphs, path_line_starts) = layout_drop_cap_wrap(
                &fonts,
                &resolved_text,
                body_start,
                (cap_size, cap_face),
                font_px,
                |from| pieces_of(from, resolved_text.len()),
                text_box_extent(title, width, height).0,
                tb.text_direction == TextDirection::Rtl,
            );
        } else if drop_cap_active {
            // "Drop initial" path: cap as the head segment.
            let cap_size = font_px * tb.drop_cap_scale;
            layout.append(&fonts, &resolved_text, (0, drop_cap_byte_end, cap_size, cap_face));
        }

        // The body, from `body_start` onwards (the drop cap, if
        // active, was laid out above), in pieces at each style run's
        // size and face. Run boundaries are clamped against
        // `body_start` so a run that overlaps the drop cap collapses
        // to its post-cap remainder. The drop-cap wrap built its
        // glyphs already.
        if !drop_cap_wrap_eligible {
            for piece in pieces_of(body_start, resolved_text.len()) {
                layout.append(&fonts, &resolved_text, piece);
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
                colour_glyphs: Vec::new(),
                path_units: None,
                tracking_origins: Vec::new(),
            };
        }

        // ── Post-process glyph positions for tracking and line spacing ──

        let line_starts = if drop_cap_wrap_eligible || columns_active {
            std::mem::take(&mut path_line_starts)
        } else if vertical_active {
            std::mem::take(&mut vertical_starts)
        } else {
            layout.lines().map_or(Vec::new(), |lines| lines.iter().map(|l| l.glyph_start).collect())
        };
        // The drop-cap wrap and the vertical stack set their own lines:
        // each line after the first moves down by its place in the
        // lines, a body line's height (a vertical character's) times
        // the extra spacing. The other paths' layouts spaced theirs.
        // Vertical text spaced its columns and its characters as it set
        // them (#1507).
        if tb.line_spacing != 1.0 && drop_cap_wrap_eligible {
            let pitch = {
                let face = pieces_of(body_start, resolved_text.len()).first().map_or(0, |p| p.3);
                fonts[face].horizontal_line_metrics(font_px).map_or(font_px * 1.2, |m| m.new_line_size)
            };
            for (line_idx, &start) in line_starts.iter().enumerate().skip(1) {
                let end = line_starts.get(line_idx + 1).copied().unwrap_or(glyphs_owned.len());
                for g in glyphs_owned.get_mut(start..end).unwrap_or_default() {
                    g.y += pitch * (tb.line_spacing - 1.0) * line_idx as f32;
                }
            }
        }

        let tracking_origins = if tracking::eligible(title) { glyphs_owned.iter().map(|g| [g.x, g.y]).collect::<Vec<_>>() } else { Vec::new() };
        // Apply tracking (extra pixel spacing per character)
        if tb.tracking != 0.0 && !vertical_active {
            let tracking_px = tb.tracking * scale;
            for (i, g) in glyphs_owned.iter_mut().enumerate() {
                g.x += tracking_px * i as f32;
            }
        }
        // A style run's own tracking and baseline offset (#1505).
        if !vertical_active {
            apply_run_spacing(&mut glyphs_owned, &tb.style_runs, &line_starts, tb.tracking, scale);
        }

        // Justified lines fill the box (#1506). Only fontdue's own
        // layout carries its lines; the drop-cap wrap, vertical and
        // column paths set their lines from the left.
        let alignment = if vertical_active && tb.alignment == TextAlignment::Justify {
            TextAlignment::Center
        } else {
            tb.alignment
        };
        if alignment == TextAlignment::Justify && !drop_cap_wrap_eligible && !columns_active {
            if let Some(lines) = layout.lines() {
                justify_lines(&mut glyphs_owned, lines, &resolved_text, box_w, tb.last_line);
            }
        }

        let text_glyphs = glyphs_owned.len();
        // A list's markers (#1721), on the lines fontdue's layout laid.
        let plain_lines = !drop_cap_wrap_eligible && !columns_active && !vertical_active && !on_path;
        let markers = match layout.lines().filter(|_| plain_lines) {
            Some(lines) => {
                let piece_at = |b: usize| pieces_of(b, b + resolved_text[b..].chars().next().map_or(1, char::len_utf8)).first().map(|p| (p.2, p.3));
                list_markers(&tb, &fonts, &resolved_text, lines, &glyphs_owned, piece_at, scale, box_w)
            }
            None => Vec::new(),
        };
        let marker_pad = (!markers.is_empty()).then(|| (tb.paragraph.indent_start.max(0.0) * scale, tb.text_direction == TextDirection::Rtl));
        // Ruby (#1704): each run's gloss beside its text, laid after the
        // text's glyphs and drawn with them; the text's lines and
        // underlines are its own glyphs', the first `text_glyphs`.
        if !on_path && tb.style_runs.iter().any(|r| r.ruby.is_some()) {
            let gloss = ruby::lay(&fonts, &glyphs_owned, &tb.style_runs, run_px, vertical_active);
            glyphs_owned.extend(gloss);
        }
        glyphs_owned.extend(markers);
        let glyphs = &glyphs_owned;

        // ── Text on path: rasterize per-glyph with rotation ──
        if let Some(text_path) = tb.text_path.as_ref().filter(|_| on_path) {
            let line = path_line;
            let lines = layout.lines().cloned().unwrap_or_default();
            let keys: Vec<&str> = faces.keys.iter().map(|k| k.0.as_str()).collect();
            // A vertical box follows the path with its columns (#1704).
            let columns = vertical_active.then_some(line_starts.as_slice());
            return self.render_layout_on_path(
                &fonts, &keys, glyphs, &lines, columns, title, text_path, line.as_ref(), time_secs, width, height, w, h, scale,
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

        // Position offset: which edge of the text rests on the anchor,
        // and where it sits up and down its box (`text_offset_x`,
        // `text_offset_y`).
        let anchor_x = title.position().x * width as f32;
        let offset_x = text_offset_x(alignment, anchor_x, text_w, text_min_x, box_w)
            + indent_shift(alignment, layout.lines().filter(|_| !drop_cap_wrap_eligible && !columns_active && !vertical_active), marker_pad);
        // Baseline shift is authored in 1080p reference pixels and
        // scales with the frame so a +20 px offset on a 1080p edit
        // looks the same on a 4K master. Positive shifts text DOWN.
        let baseline_shift_px = tb.baseline_shift * scale;
        let offset_y = text_offset_y(tb.vertical_align, title.position().y * height as f32, box_h, text_h, text_min_y)
            + baseline_shift_px;
        if self.layout_bounds_only {
            return TitleLayout::bounds_only([text_min_x+offset_x,text_min_y+offset_y,text_max_x+offset_x,text_max_y+offset_y],glyphs.len(),scale);
        }
        // A 3D text's glyphs where they stand, for their solids (#1512):
        // each one's font, glyph, size and baseline origin on the frame.
        if tb.three_d.as_ref().is_some_and(|t| t.enabled) {
            let placed = glyphs
                .iter()
                .enumerate()
                .filter(|(_, g)| g.width > 0 && g.height > 0)
                .map(|(unit, g)| {
                    let face = &fonts[g.font_index];
                    let b = face.font.metrics_indexed(g.key.glyph_index, g.key.px).bounds;
                    let sprite = face.font.colour_glyph(g.key.glyph_index, g.key.px).map(|c| crate::text3d_render::Sprite {
                        x: ((g.x + offset_x) as i32) as f32,
                        y: ((g.y + offset_y) as i32) as f32,
                        width: c.width,
                        height: c.height,
                        rgba: c.rgba.clone(),
                    });
                    crate::text3d_render::Glyph3d {
                        font: faces.keys[g.font_index].0.clone(),
                        glyph: g.key.glyph_index,
                        px: g.key.px,
                        origin: [g.x + offset_x - b.xmin, g.y + offset_y + b.height + b.ymin],
                        sprite,
                        unit,
                        turn: None,
                    }
                })
                .collect();
            // A write-on reveals the solids up to where it reveals the
            // flat text (#1735); the stripes are added where they are cut.
            let reveal = (tb.write_on < 1.0).then(|| (((text_min_x + offset_x) + text_w * tb.write_on.clamp(0.0, 1.0)) as i32) as f32);
            *self.last_glyphs3d.lock().unwrap_or_else(|e| e.into_inner()) = crate::text3d_render::Placed3d { glyphs: placed, stripes: Vec::new(), reveal };
        }

        // Compute the mask bounding box (padded for effects). When
        // an underline is enabled the stripe sits ~5 % below the
        // visible text bottom — the mask must extend that far down
        // or the underline gets clipped on the descender side.
        let max_pad = effect_padding(title, scale);
        // A style run's own underline (#1668) is drawn per line under
        // the run's glyphs, in place of the box's single stripe; so is
        // the box's own in a box with runs, in each run's colour (#1680).
        let run_underlines = tb.style_runs.iter().any(|r| r.underline.is_some()) || (tb.underline && !tb.style_runs.is_empty());
        let underline_pad = if tb.underline || run_underlines {
            (text_h * 0.10).ceil() as i32 + (font_px * 0.06).ceil() as i32
        } else {
            0
        };
        let mask_x0 = ((text_min_x + offset_x).floor() as i32 - max_pad).max(0) as usize;
        let mask_y0 = ((text_min_y + offset_y).floor() as i32 - max_pad).max(0) as usize;
        let mask_x1 = ((text_max_x + offset_x).ceil() as i32 + max_pad + 1).clamp(0, w as i32) as usize;
        let mask_y1 = ((text_max_y + offset_y).ceil() as i32 + max_pad + underline_pad + 1)
            .clamp(0, h as i32) as usize;
        // Glyphs a motion or an animator moves, scales, turns or blurs at a
        // time reach out of the resting box: rastered here, they are kept to
        // the frame, as the placed sprite and the GPU's posed glyphs are, not
        // cut at it. A fade, a flicker or a typewriter stays inside, and so
        // does a motion at rest: its frame is the resting one, to the byte.
        let posed_fx = title.primary_fx();
        let (posed_t, posed_window) = match self.gpu_motion {
            Some((t, window)) => (Some(t), window.or(self.motion_window)),
            None => (time_secs, self.motion_window),
        };
        let moves = |m: &Option<klipp_timeline::Motion>| m.is_some_and(|m| !matches!(m.kind, klipp_timeline::MotionKind::Fade | klipp_timeline::MotionKind::Flicker | klipp_timeline::MotionKind::Typewriter));
        let posed = posed_t.is_some_and(|t| {
            // The first unit and the last bound a staggered motion's span,
            // counted as `frame_at` counts a motion's units.
            let last = |m: &Option<klipp_timeline::Motion>| match m.map(|m| m.stagger).unwrap_or_default() {
                klipp_timeline::StaggerUnit::None => 0,
                klipp_timeline::StaggerUnit::Line => tb.text.lines().count().max(1) - 1,
                klipp_timeline::StaggerUnit::Word => tb.text.split_whitespace().count().max(1) - 1,
                klipp_timeline::StaggerUnit::Char => tb.text.chars().filter(|c| !c.is_whitespace()).count().max(1) - 1,
            };
            let in_motion = [0, last(&posed_fx.motion.enter), last(&posed_fx.motion.exit)].into_iter().any(|unit| {
                let (enter, exit) = posed_fx.motion.progress(t, posed_window, unit);
                (moves(&posed_fx.motion.enter) && (enter - 1.0).abs() > 1e-6) || (moves(&posed_fx.motion.exit) && (exit - 1.0).abs() > 1e-6)
            });
            in_motion || posed_fx.animators.iter().any(|a| a.enabled && (a.props.dx != 0.0 || a.props.dy != 0.0 || a.props.scale != 1.0 || a.props.rotation != 0.0 || a.props.blur != 0.0))
        });
        let (mask_x0, mask_y0, mask_x1, mask_y1) = if posed { (0, 0, w, h) } else { (mask_x0, mask_y0, mask_x1, mask_y1) };
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
                colour_glyphs: Vec::new(),
                path_units: None,
                tracking_origins: Vec::new(),
            };
        }
        if self.gpu_layout_only {
            let bitmap=glyphs.iter().any(|g|fonts[g.font_index].font.colour_source(g.key.glyph_index,g.key.px).is_some());
            // Cached RGBA and retained-strike resizing handle unposed text, its
            // outlines, decorations and colour loops; the rest needs
            // native-colour sampling or mask semantics.
            let bitmap_supported=tb.style_runs.is_empty() && tb.write_on>=1.0
                && tb.gradient.as_ref().is_none_or(|g|!g.enabled)
                && (tb.pattern_fill.is_none() || !tb.pattern_fill_enabled)
                && tb.texture_fill.as_ref().is_none_or(|f|!f.enabled)
                && glyphs.iter().all(|g|g.key.px>=0.0);
            let rest_glyphs=glyphs;
            let resolved_colors=resolve_glyph_colors(title,glyphs,&tb.style_runs);
            let mut posed=glyphs.to_vec();
            let mut draw_poses=Vec::new();
            let coloured=title.primary_fx().animators.iter().any(|a|a.enabled && a.props.colour.is_some());
            let fx=title.primary_fx();
            let enter_units=motion_units(glyphs,fx.motion.enter.map(|m|m.stagger).unwrap_or_default());
            let exit_units=motion_units(glyphs,fx.motion.exit.map(|m|m.stagger).unwrap_or_default());
            let parents:Vec<_>=glyphs.iter().map(|g|g.parent).collect();
            let animated=self.gpu_motion.map(|(time,_)|klipp_core::title_motion::animator_poses(&fx.animators,&parents,font_px,time as f64)).unwrap_or_default();
            for (i,(g,rest_g)) in posed.iter_mut().zip(glyphs).enumerate() {
                // Spaces still count for selector indices but have no coverage
                // to expand, blur or rotate.
                if rest_g.width==0 || rest_g.height==0 {continue;}
                let mut dx=0.0;let mut dy=0.0;let mut alpha=1.0;let mut clip=[i32::MIN,i32::MAX];let mut sigma=0.0;let mut radians=0.0;let mut colour=None;
                if let Some((time,window))=self.gpu_motion {
                    let (pe,_)=fx.motion.progress(time,window,enter_units[i]);
                    let (_,px)=fx.motion.progress(time,window,exit_units[i]);
                    let rest=(1.0,0.0,0.0,1.0,0.0,false);
                    let (a1,x1,y1,s1,b1,c1)=fx.motion.enter.map(|m|m.pose(pe,font_px)).unwrap_or(rest);
                    let (a2,x2,y2,s2,b2,c2)=fx.motion.exit.map(|m|m.pose(px,font_px)).unwrap_or(rest);
                    let pose=animated.get(i).copied().unwrap_or_default();
                    if coloured {colour=Some(pose.colour_of(resolved_colors.get(i).copied().unwrap_or(tb.color)));}
                    sigma=b1.max(b2).max(pose.blur);radians=pose.rotation.to_radians();
                    alpha=a1*a2*pose.alpha;
                    if alpha<=0.001 {alpha=0.0;}
                    dx=x1+x2+pose.dx;dy=y1+y2+pose.dy;
                    let scale=s1*s2*pose.scale;
                    if (scale-1.0).abs()>0.001 {
                        g.key.px=(font_px*scale).max(1.0).copysign(g.key.px);
                        let m=fonts[g.font_index].font.metrics_indexed(g.key.glyph_index,g.key.px.abs());
                        (g.width,g.height)=if g.key.px<0.0 {(m.height,m.width)}else{(m.width,m.height)};
                    }
                    if c1||c2 {let top=(rest_g.y+offset_y) as i32;clip=[top,top+rest_g.height as i32];}
                }
                if let Some((time,amp,rate))=self.gpu_wave {dy+=(time*rate*std::f32::consts::TAU+i as f32*0.5).sin()*amp;}
                let source_size=[g.width as u32,g.height as u32];
                if sigma>=0.5 {let pad=(sigma*2.5).ceil().max(1.0) as usize;g.width+=2*pad;g.height+=2*pad;}
                if radians!=0.0 {
                    let(c,sn)=(radians.cos(),radians.sin());let(hx,hy)=(g.width as f32*0.5,g.height as f32*0.5);
                    let rw=((hx*c).abs()+(hy*sn).abs()).ceil() as usize*2+2;
                    let rh=((hx*sn).abs()+(hy*c).abs()).ceil() as usize*2+2;
                    g.width=rw+(rw+g.width)%2;g.height=rh+(rh+g.height)%2;
                }
                let origin=[(rest_g.x+offset_x+dx) as i32-(g.width as i32-rest_g.width as i32)/2,
                    (rest_g.y+offset_y+dy) as i32-(g.height as i32-rest_g.height as i32)/2];
                g.x=origin[0] as f32-offset_x;g.y=origin[1] as f32-offset_y;
                if g.width>0 && g.height>0 {draw_poses.push((origin,alpha,clip,source_size,sigma,radians,colour));}
            }
            // Glyphs posed only by a move, a fade or a mask's clip are their
            // bitmaps placed, as the CPU rasteriser draws them; a blurred,
            // turned or coloured pose needs the outlines, and a scaled one the
            // outlines or a resized strike (#2203).
            let scaled=posed.iter().zip(rest_glyphs).any(|(p,r)|p.key.px!=r.key.px);
            let reshaped=draw_poses.iter().any(|(_,_,_,_,blur,radians,colour)|*blur>=0.05 || *radians!=0.0 || colour.is_some());
            let glyphs=posed.as_slice();
            // The CPU draws a colour glyph as far as the whole mask reaches,
            // where an overlapping letter's coverage can outweigh its own
            // posed alpha; the GPU has only its own, so such a frame keeps
            // the CPU (#2203).
            let boxes:Vec<([i32;4],bool,f32)>=glyphs.iter().filter(|g|g.width>0 && g.height>0).zip(&draw_poses).map(|(g,(origin,alpha,..))| {
                ([origin[0],origin[1],origin[0]+g.width as i32,origin[1]+g.height as i32],fonts[g.font_index].font.colour_source(g.key.glyph_index,g.key.px).is_some(),*alpha)
            }).collect();
            let meets=|a:&[i32;4],b:&[i32;4]|a[0]<b[2] && b[0]<a[2] && a[1]<b[3] && b[1]<a[3];
            let overlapped=boxes.iter().enumerate().any(|(i,(r,colour,alpha))|*colour && *alpha>0.0 && *alpha<1.0
                && boxes.iter().enumerate().any(|(j,(o,_,a))|i!=j && *a>0.0 && meets(r,o)));
            let sized_bitmap=bitmap && (self.gpu_bitmap_size || fit!=1.0 || scaled);
            *self.gpu_layout_capture.borrow_mut() = if bitmap && (!bitmap_supported || reshaped || overlapped) {None}
            else if sized_bitmap {self.gpu_glyph_cache.borrow_mut().bitmap_size_layout(&fonts,glyphs,[offset_x,offset_y],[mask_x0 as u32,mask_y0 as u32,mask_w as u32,mask_h as u32],tb.color,title.primary_fx())}
            else{self.gpu_glyph_cache.borrow_mut().layout(
                &fonts,glyphs,[offset_x,offset_y],[mask_x0 as u32,mask_y0 as u32,mask_w as u32,mask_h as u32],tb.color,title.primary_fx(),
                (self.gpu_outline_only || (self.gpu_motion.is_some() && (scaled || reshaped))) && !bitmap,
            )};
            if let Some(source) = self.gpu_layout_capture.borrow_mut().as_mut() {
                let outline_pose=|draw:&mut gpu_glyphs::OutlineDraw,(origin,alpha,clip,size,blur,radians,colour):&([i32;2],f32,[i32;2],[u32;2],f32,f32,Option<[u8;4]>)| {
                    draw.origin = *origin;draw.alpha = *alpha;draw.clip_y = *clip;draw.source_size = *size;draw.blur = *blur;draw.radians = *radians;draw.color = *colour;
                };
                let glyph_pose=|draw:&mut gpu_glyphs::GlyphDraw,(origin,alpha,clip,..):&([i32;2],f32,[i32;2],[u32;2],f32,f32,Option<[u8;4]>)| {
                    draw.origin = *origin;draw.alpha = *alpha;draw.clip_y = *clip;
                };
                if !sized_bitmap {
                    for (draw,pose) in source.outlines.iter_mut().zip(&draw_poses) {outline_pose(draw,pose);}
                    for (draw,pose) in source.glyphs.iter_mut().zip(&draw_poses) {glyph_pose(draw,pose);}
                } else {
                    // Ordinary glyphs are outlines here and colour ones resized
                    // strikes, each list in glyph order.
                    let (mut outlines,mut strikes)=(source.outlines.iter_mut(),source.glyphs.iter_mut());
                    for (g,pose) in glyphs.iter().filter(|g|g.width>0 && g.height>0).zip(&draw_poses) {
                        if fonts[g.font_index].font.colour_source(g.key.glyph_index,g.key.px).is_some() {
                            if let Some(draw)=strikes.next() {glyph_pose(draw,pose);}
                        } else if let Some(draw)=outlines.next() {outline_pose(draw,pose);}
                    }
                }
                if coloured && draw_poses.iter().any(|pose| pose.1 > 0.0) {
                    source.face_draws=source.outlines.clone();
                } else if !tb.style_runs.is_empty() {
                    if let Some(rest)=self.gpu_glyph_cache.borrow_mut().layout(&fonts,rest_glyphs,[offset_x,offset_y],source.roi,tb.color,title.primary_fx(),true) {
                        source.prepared+=rest.prepared;
                        source.face_draws=rest.outlines;
                        let colors=rest_glyphs.iter().enumerate().filter(|(_,g)|g.width>0 && g.height>0).map(|(i,_)|resolved_colors.get(i).copied().unwrap_or(tb.color));
                        for (draw,color) in source.face_draws.iter_mut().zip(colors) {draw.color=Some(color);}
                    } else {
                        source.face_draws.clear();
                        // A bitmap colour glyph still needs the colour-source path.
                        source.face_incomplete=true;
                    }
                }
                if run_underlines {
                    for (from,to) in underline_stretches(&rest_glyphs[..text_glyphs],&tb.style_runs,&line_starts,tb.underline) {
                        let line=line_of(&line_starts,from);
                        let lo=line_starts.get(line).copied().unwrap_or(0);let hi=line_starts.get(line+1).copied().unwrap_or(text_glyphs);
                        let inked=|range:&[fontdue::layout::GlyphPosition]|range.iter().filter(|g|g.width>0).copied().collect::<Vec<_>>();
                        let line_glyphs=inked(&rest_glyphs[lo..hi]);let stretch=inked(&rest_glyphs[from..to]);
                        let top=line_glyphs.iter().map(|g|g.y).fold(f32::MAX,f32::min);
                        let bottom=line_glyphs.iter().map(|g|g.y+g.height as f32).fold(f32::MIN,f32::max);
                        let left=stretch.iter().map(|g|g.x).fold(f32::MAX,f32::min);
                        let right=stretch.iter().map(|g|g.x+g.width as f32).fold(f32::MIN,f32::max);
                        let run=tb.style_runs.iter().find(|r|r.start<=rest_glyphs[from].byte_offset&&rest_glyphs[from].byte_offset<r.end);
                        let thick=(run.map_or(font_px,run_px)*0.06).max(1.0).round() as i32;
                        let x0=((left+offset_x).floor() as i32).max(mask_x0 as i32);
                        let x1=((right+offset_x).ceil() as i32).min((mask_x0+mask_w) as i32);
                        let y0=((top+offset_y+(bottom-top)*1.05).round() as i32-thick/2).max(mask_y0 as i32);
                        let y1=(y0+thick).min(height as i32);
                        if x1>x0 && y1>y0 {
                            let rect=[x0 as u32,y0 as u32,(x1-x0) as u32,(y1-y0) as u32];
                            source.stripes.push(rect);
                            source.face_stripes.push((rect,resolved_colors.get(from).copied().unwrap_or(tb.color)));
                        }
                    }
                }
                source.bounds=[text_min_x+offset_x,text_min_y+offset_y,text_max_x+offset_x,text_max_y+offset_y];
                source.gradient=tb.gradient.clone().filter(|g|g.enabled);
                source.pattern=tb.pattern_fill.filter(|_|tb.pattern_fill_enabled);
                if let TitleBackground::Color{color,padding}=title.background {
                    let pad=padding*scale;
                    let x0=((text_min_x+offset_x-pad) as i32).max(0) as u32;
                    let y0=((text_min_y+offset_y-pad) as i32).max(0) as u32;
                    let x1=((text_max_x+offset_x+pad) as i32).min(width as i32).max(0) as u32;
                    let y1=((text_max_y+offset_y+pad) as i32).min(height as i32).max(0) as u32;
                    if x1>x0 && y1>y0 {source.backing=Some(([x0,y0,x1-x0,y1-y0],color));}
                }
                source.write_cutoff=if tb.write_on<1.0 {((text_min_x+offset_x)+text_w*tb.write_on.clamp(0.0,1.0)) as i32} else {i32::MAX};
                if tb.underline || tb.strikethrough {
                    let thickness=(font_px*0.06).max(1.0).round() as i32;
                    let x0=((text_min_x+offset_x).floor() as i32).max(mask_x0 as i32);
                    let x1=((text_max_x+offset_x).ceil() as i32).min((mask_x0+mask_w) as i32);
                    for (on,at) in [(tb.strikethrough,0.40),(tb.underline && !run_underlines,1.05)] {
                        if on && x1>x0 {
                            let y0=((text_min_y+offset_y+text_h*at).round() as i32-thickness/2).max(mask_y0 as i32);
                            let y1=(y0+thickness).min((mask_y0+mask_h) as i32);
                            if y1>y0 {source.stripes.push([x0 as u32,y0 as u32,(x1-x0) as u32,(y1-y0) as u32]);}
                        }
                    }
                }
                let progress = tb.outline_write_on.clamp(0.0,1.0);
                source.outline_cutoff = if progress >= 1.0 { i32::MAX } else {
                    (text_min_x + offset_x + (text_max_x-text_min_x)*progress).floor() as i32
                };
            }
            return TitleLayout {
                alpha_mask: Vec::new(), mask_w: mask_w as u32, mask_h: mask_h as u32, mask_x0: mask_x0 as u32, mask_y0: mask_y0 as u32,
                text_top: text_min_y+offset_y, text_bottom:text_max_y+offset_y,
                text_left:text_min_x+offset_x, text_right:text_max_x+offset_x,
                glyph_count:glyphs.len(), scale, glyph_colors:Vec::new(),glyph_coverages:Vec::new(),
                glyph_index:Vec::new(),glyph_boxes:Vec::new(),glyph_parents:Vec::new(),pad:max_pad,
                colour_glyphs:Vec::new(),path_units:None,tracking_origins:Vec::new(),
            };
        }
        // A staggered motion poses each glyph on its own from the
        // resting render, so it needs to know which glyph a pixel is.
        let staggers = |m: Option<klipp_timeline::Motion>| m.is_some_and(|m| m.stagger != klipp_timeline::StaggerUnit::None);
        let fx = title.primary_fx();
        let animates = fx.animators.iter().any(|a| a.enabled);
        // A wave bobs each glyph on its own too (#2069).
        let waves = fx.loops.iter().any(|l| matches!(l, klipp_timeline::TitleLoop::Wave { .. }));
        let tag_glyphs = time_secs.is_none() && (staggers(fx.motion.enter) || staggers(fx.motion.exit) || animates || waves || tracking::eligible(title));
        let mut glyph_index = if tag_glyphs { vec![GLYPH_NONE; mask_w * mask_h] } else { Vec::new() };

        // Rasterize glyphs into alpha mask. When the textbox has a
        // `char_animation` AND the renderer was called with a clip
        // time, dispatch to the per-glyph animated rasterizer; the
        // rest of the pipeline (compose, outline, shadow, glow,
        // texture fill) consumes the resulting mask unmodified.
        let mut alpha_mask = vec![0u8; mask_w * mask_h];
        let mut colour_glyphs = Vec::new();
        // Each posed glyph's coverage where an animator colours it.
        let mut animated_fills: Vec<(GlyphColorInfo, Vec<u8>)> = Vec::new();
        match time_secs {
            Some(t) if fx.motion.is_active() || self.loop_wave.is_some() || animates => {
                rasterize_to_mask_motion(
                    &fonts, glyphs,
                    offset_x, offset_y,
                    &mut alpha_mask, mask_w, mask_h, mask_x0, mask_y0,
                    &fx.motion, &fx.animators, t, font_px, self.motion_window, self.loop_wave,
                    &mut colour_glyphs,
                    &mut animated_fills,
                    &if animates { resolve_glyph_colors(title, glyphs, &tb.style_runs) } else { Vec::new() },
                );
            }
            _ => {
                rasterize_to_mask(
                    &fonts, glyphs,
                    offset_x, offset_y,
                    &mut alpha_mask, mask_w, mask_h, mask_x0, mask_y0,
                    if tag_glyphs { Some(&mut glyph_index) } else { None },
                    &mut colour_glyphs,
                );
            }
        }

        // A 3D text's stripes are solids of their own, kept here in frame
        // pixels (#1735).
        let three_d = tb.three_d.as_ref().is_some_and(|t| t.enabled);
        let stripe3d = |x0: i32, x1: i32, y0: i32, y1: i32| {
            if three_d && x1 > x0 && y1 > y0 {
                let (ox, oy) = (mask_x0 as f32, mask_y0 as f32);
                self.last_glyphs3d.lock().unwrap_or_else(|e| e.into_inner()).stripes.push([x0 as f32 + ox, y0 as f32 + oy, x1 as f32 + ox, y1 as f32 + oy]);
            }
        };
        // Underline / strikethrough: paint a horizontal stripe across
        // the text bounds inside the alpha mask. Thickness scales
        // with font_px so it stays proportional at any resolution;
        // capped at a 1-pixel minimum so it doesn't disappear at
        // small sizes. Strikethrough sits at ~40 % from the bottom
        // (covering the x-height midpoint for most fonts);
        // underline sits 10 % below the visible bottom (descender
        // territory) so it doesn't crash into descenders.
        if (tb.underline && !run_underlines) || tb.strikethrough {
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
                stripe3d(line_x0, line_x1, stripe_top, stripe_top + stripe_thickness);
            }
            if tb.underline && !run_underlines {
                let line_y_centre = (text_min_y + offset_y) + text_h * 1.05;
                let stripe_top = (line_y_centre.round() as i32 - mask_y0 as i32
                    - stripe_thickness / 2)
                    .max(0);
                paint_alpha_mask_stripe(
                    &mut alpha_mask, mask_w, mask_h,
                    line_x0, line_x1, stripe_top, stripe_top + stripe_thickness,
                );
                stripe3d(line_x0, line_x1, stripe_top, stripe_top + stripe_thickness);
            }
        }

        // ── Rich text: resolve per-glyph styles ──
        let has_rich_text = !tb.style_runs.is_empty();
        let resolved_colors: Vec<[u8; 4]> = if has_rich_text {
            resolve_glyph_colors(title, glyphs, &tb.style_runs)
        } else {
            Vec::new()
        };

        // Run underlines (#1668): a stripe in the mask under each
        // line's stretch of glyphs underlined, a run's own or else the
        // box's, at the box underline's thickness for the run's size
        // and its place below the line, filled in the run's colour.
        let mut underline_fills: Vec<GlyphColorInfo> = Vec::new();
        if run_underlines {
            for (from, to) in underline_stretches(&glyphs[..text_glyphs], &tb.style_runs, &line_starts, tb.underline) {
                let line = line_of(&line_starts, from);
                let (line_from, line_to) = (line_starts.get(line).copied().unwrap_or(0), line_starts.get(line + 1).copied().unwrap_or(text_glyphs));
                // Ink only: a space has none.
                let inked = |range: &[fontdue::layout::GlyphPosition]| range.iter().filter(|g| g.width > 0).copied().collect::<Vec<_>>();
                let line_glyphs = inked(&glyphs[line_from..line_to]);
                let line_top = line_glyphs.iter().map(|g| g.y).fold(f32::MAX, f32::min);
                let line_bottom = line_glyphs.iter().map(|g| g.y + g.height as f32).fold(f32::MIN, f32::max);
                let stretch = inked(&glyphs[from..to]);
                let x0 = stretch.iter().map(|g| g.x).fold(f32::MAX, f32::min);
                let x1 = stretch.iter().map(|g| g.x + g.width as f32).fold(f32::MIN, f32::max);
                let run = tb.style_runs.iter().find(|r| r.start <= glyphs[from].byte_offset && glyphs[from].byte_offset < r.end);
                let thickness = (run.map_or(font_px, run_px) * 0.06).max(1.0).round() as i32;
                let stripe_x0 = ((x0 + offset_x).floor() as i32 - mask_x0 as i32).max(0);
                let stripe_x1 = ((x1 + offset_x).ceil() as i32 - mask_x0 as i32).min(mask_w as i32);
                let line_y_centre = (line_top + offset_y) + (line_bottom - line_top) * 1.05;
                let stripe_top = (line_y_centre.round() as i32 - mask_y0 as i32 - thickness / 2).max(0);
                paint_alpha_mask_stripe(&mut alpha_mask, mask_w, mask_h, stripe_x0, stripe_x1, stripe_top, stripe_top + thickness);
                stripe3d(stripe_x0, stripe_x1, stripe_top, stripe_top + thickness);
                underline_fills.push(GlyphColorInfo {
                    mask_x: stripe_x0,
                    mask_y: stripe_top,
                    width: (stripe_x1 - stripe_x0).max(0) as usize,
                    height: thickness as usize,
                    color: resolved_colors.get(from).copied().unwrap_or(title.color()),
                });
            }
        }

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
        let (mut glyph_colors, mut glyph_coverages) = if has_rich_text {
            build_glyph_color_info(&fonts, glyphs, offset_x, offset_y, mask_x0, mask_y0, &resolved_colors)
        } else {
            (Vec::new(), Vec::new())
        };
        // Glyphs an animator colours fill in their posed colours, each
        // over its posed coverage (#1510).
        if !animated_fills.is_empty() {
            (glyph_colors, glyph_coverages) = animated_fills.into_iter().unzip();
        }
        // The rich fill draws each underline as a solid glyph of its own.
        for fill in underline_fills {
            glyph_coverages.push(vec![255; fill.width * fill.height]);
            glyph_colors.push(fill);
        }

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
            colour_glyphs,
            path_units: None,
            tracking_origins: tracking_origins.into_iter().map(|p| [p[0], p[1] + offset_y]).collect(),
        }
    }

    /// Render glyphs placed along a path (#1508): a drawn path's `line`,
    /// or a fixed arc, circle or wave about the title's position. Each
    /// glyph keeps its place along the shaped line (`lines` are its
    /// lines, set one after another) and stands on the path by its
    /// baseline, rasterized on its own and turned to the tangent.
    #[allow(clippy::too_many_arguments)]
    fn render_layout_on_path(
        &self,
        fonts: &[Face],
        keys: &[&str],
        glyphs: &[fontdue::layout::GlyphPosition],
        lines: &[shaping::Line],
        columns: Option<&[usize]>,
        title: &TitleData,
        text_path: &TextPath,
        line: Option<&text_on_path::PathLine>,
        time_secs: Option<f32>,
        width: u32,
        height: u32,
        w: usize,
        h: usize,
        scale: f32,
    ) -> TitleLayout {
        let tb = title.primary_text_or_default();
        // Each glyph's centre along the text, its lines run end to end
        // a space apart, and its height off its line's baseline.
        let gap = tb.font_size * scale * 0.3;
        let mut laid = vec![text_on_path::Laid { along: 0.0, below_baseline: 0.0 }; glyphs.len()];
        let mut carried = 0.0f32;
        match columns {
            // Vertical text (#1704): each column runs down the path, one
            // after another, its middle on the path; a glyph across from
            // the middle stands off the path to the column's right, which
            // is the path's left once the column's foot points along it.
            Some(starts) => {
                for (k, &from) in starts.iter().enumerate() {
                    let to = starts.get(k + 1).copied().unwrap_or(glyphs.len()).min(glyphs.len());
                    let column = &glyphs[from.min(to)..to];
                    let inked = || column.iter().filter(|g| g.width > 0 && g.height > 0);
                    let top = inked().map(|g| g.y).fold(f32::MAX, f32::min);
                    let foot = inked().map(|g| g.y + g.height as f32).fold(f32::MIN, f32::max);
                    let left = inked().map(|g| g.x).fold(f32::MAX, f32::min);
                    let right = inked().map(|g| g.x + g.width as f32).fold(f32::MIN, f32::max);
                    if top > foot {
                        continue;
                    }
                    let middle = (left + right) * 0.5;
                    for (i, g) in column.iter().enumerate() {
                        laid[from + i] = text_on_path::Laid { along: carried + g.y + g.height as f32 * 0.5 - top, below_baseline: middle - (g.x + g.width as f32 * 0.5) };
                    }
                    carried += foot - top + gap;
                }
            }
            None => {
                for l in lines {
                    let range = l.glyph_start..=l.glyph_end.min(glyphs.len().saturating_sub(1));
                    let left = range.clone().map(|i| glyphs[i].x).fold(f32::MAX, f32::min);
                    let right = range.clone().map(|i| glyphs[i].x + glyphs[i].width as f32).fold(f32::MIN, f32::max);
                    if left > right {
                        continue;
                    }
                    for i in range {
                        let g = &glyphs[i];
                        laid[i] = text_on_path::Laid { along: carried + g.x + g.width as f32 * 0.5 - left, below_baseline: g.y + g.height as f32 * 0.5 - l.baseline };
                    }
                    carried += right - left + gap;
                }
            }
        }
        let length = (carried - gap).max(1.0);
        let centre = [title.position().x * width as f32, title.position().y * height as f32];
        let offset = text_path.offset_at(time_secs.unwrap_or(0.0) as f64);
        let mut placed = text_on_path::place(text_path, line, offset, &laid, length, centre, tb.baseline_shift * scale);
        // A vertical glyph's foot points along the path: turned a
        // quarter back from a horizontal one's (#1704).
        if columns.is_some() {
            for p in placed.iter_mut().flatten() {
                p.angle -= std::f32::consts::FRAC_PI_2;
            }
        }

        let fx=title.primary_fx();
        // 3D solids evaluate scale and rotation later, in render_text3d_solid.
        let three_d=tb.three_d.as_ref().is_some_and(|t|t.enabled);
        let mut path_alpha=vec![1.0f32;glyphs.len()];
        let mut path_blur=vec![0.0f32;glyphs.len()];
        let mut path_clip=vec![[i32::MIN,i32::MAX];glyphs.len()];
        let mut posed_glyphs=glyphs.to_vec();
        let coloured=!three_d && fx.animators.iter().any(|a|a.enabled && a.props.colour.is_some());
        let base_colours=resolve_glyph_colors(title,glyphs,&tb.style_runs);
        let mut path_colours=base_colours.clone();
        {
            if let Some(time)=time_secs {
                let window=self.gpu_motion.and_then(|(_,window)|window).or(self.motion_window);
                let enter_units=motion_units(glyphs,fx.motion.enter.map(|m|m.stagger).unwrap_or_default());
                let exit_units=motion_units(glyphs,fx.motion.exit.map(|m|m.stagger).unwrap_or_default());
                let parents:Vec<_>=glyphs.iter().map(|g|g.parent).collect();
                let poses=klipp_core::title_motion::animator_poses(&fx.animators,&parents,tb.font_size*scale,time as f64);
                for i in 0..glyphs.len() {
                    let (enter,_)=fx.motion.progress(time,window,enter_units[i]);
                    let (_,exit)=fx.motion.progress(time,window,exit_units[i]);
                    let rest=(1.0,0.0,0.0,1.0,0.0,false);
                    let a=fx.motion.enter.map_or(rest,|m|m.pose(enter,tb.font_size*scale));
                    let b=fx.motion.exit.map_or(rest,|m|m.pose(exit,tb.font_size*scale));
                    let pose=poses.get(i).copied().unwrap_or_default();
                    let alpha=a.0*b.0*pose.alpha;
                    path_alpha[i]=if alpha<=0.001 {0.0}else{alpha.clamp(0.0,1.0)};
                    let factor=if three_d {1.0}else{a.3*b.3*pose.scale};
                    let g=&mut posed_glyphs[i];
                    // A collapsed glyph has no coverage, but retain its metrics so
                    // the outline list keeps the same association with text units.
                    if factor<=0.0 {path_alpha[i]=0.0;}
                    else if (factor-1.0).abs()>0.001 && g.width>0 && g.height>0 {
                        // Preserve style-run sizes and shaping fit/case factors.
                        g.key.px=(g.key.px.abs()*factor).max(1.0).copysign(g.key.px);
                        let m=fonts[g.font_index].font.metrics_indexed(g.key.glyph_index,g.key.px.abs());
                        (g.width,g.height)=if g.key.px<0.0 {(m.height,m.width)}else{(m.width,m.height)};
                    }
                    if !three_d {path_blur[i]=a.4.max(b.4).max(pose.blur);}
                    if coloured {path_colours[i]=pose.colour_of(base_colours.get(i).copied().unwrap_or(tb.color));}
                    // Motion is in screen axes after path placement. Reveal clips
                    // use the unposed, rotated glyph's original screen-space rows.
                    if let Some(at)=placed[i].as_mut() {
                        if !three_d && (a.5||b.5) {
                            let(c,sn)=(at.angle.cos().abs(),at.angle.sin().abs());
                            let extent=glyphs[i].width as f32*0.5*sn+glyphs[i].height as f32*0.5*c;
                            path_clip[i]=[(at.centre[1]-extent).floor() as i32,(at.centre[1]+extent).ceil() as i32+1];
                        }
                        at.centre[0]+=a.1+b.1+pose.dx;
                        at.centre[1]+=a.2+b.2+pose.dy;
                        if !three_d {at.angle+=pose.rotation.to_radians();}
                    }
                }
            }
        }
        let glyphs=posed_glyphs.as_slice();
        let mut path_fills:Vec<(GlyphColorInfo,Vec<u8>)>=Vec::new();
        let max_pad = effect_padding(title, scale);
        // Use the full frame as the mask (path text can go anywhere)
        let mask_w = w;
        let mask_h = h;
        let mut alpha_mask = if self.gpu_layout_only || self.layout_bounds_only {Vec::new()}else{vec![0u8; mask_w * mask_h]};
        // Which glyph each pixel is, and each glyph's turned box: the
        // map a moving path poses the glyphs by (#1697).
        let mut owner = if self.gpu_layout_only || self.layout_bounds_only {Vec::new()}else{vec![GLYPH_NONE; mask_w * mask_h]};
        let mut glyph_boxes = vec![[0i32; 4]; glyphs.len()];

        let mut text_min_x = f32::MAX;
        let mut text_min_y = f32::MAX;
        let mut text_max_x = f32::MIN;
        let mut text_max_y = f32::MIN;
        // A write-on reveals the text in its order along the path.
        let reveal = tb.write_on.clamp(0.0, 1.0) * length;
        // A 3D text's glyphs, each stood where its flat self is (#1735).
        let mut placed3d = Vec::new();

        for (i, glyph) in glyphs.iter().enumerate() {
            if glyph.width == 0 || glyph.height == 0 {
                continue;
            }
            let Some(at) = placed[i] else { continue };
            if tb.write_on < 1.0 && laid[i].along > reveal {
                continue;
            }
            let (px, py) = (at.centre[0], at.centre[1]);
            let gw = glyph.width;
            let gh = glyph.height;
            if three_d && !self.layout_bounds_only {
                let face = &fonts[glyph.font_index];
                let b = face.font.metrics_indexed(glyph.key.glyph_index, glyph.key.px).bounds;
                let from = [glyph.x + gw as f32 * 0.5, glyph.y + gh as f32 * 0.5];
                // A colour glyph stays upright, centred where it stands.
                let sprite = face.font.colour_glyph(glyph.key.glyph_index, glyph.key.px).map(|c| crate::text3d_render::Sprite {
                    x: (px - c.width as f32 * 0.5).round(),
                    y: (py - c.height as f32 * 0.5).round(),
                    width: c.width,
                    height: c.height,
                    rgba: c.rgba.clone(),
                });
                placed3d.push(crate::text3d_render::Glyph3d {
                    font: keys.get(glyph.font_index).copied().unwrap_or_default().to_string(),
                    glyph: glyph.key.glyph_index,
                    px: glyph.key.px,
                    origin: [glyph.x - b.xmin, glyph.y + b.height + b.ymin],
                    sprite,
                    unit: i,
                    turn: Some(crate::text3d_render::Turn { from, to: [px, py], angle: at.angle }),
                });
            }
            let pad=if path_blur[i]>=0.5 {(path_blur[i]*2.5).ceil().max(1.0) as usize}else{0};
            let (paint_w,paint_h)=(gw+2*pad,gh+2*pad);
            if !self.gpu_layout_only && !self.layout_bounds_only {
                let (_,coverage)=fonts[glyph.font_index].coverage(glyph.key);
                let (coverage,bw,bh)=gaussian_blur_expand(&coverage,gw,gh,path_blur[i]);
                let faded:Vec<u8>=coverage.iter().map(|a|(*a as f32*path_alpha[i]).round() as u8).collect();
                blit_rotated(&mut alpha_mask,mask_w,mask_h,&faded,bw,bh,px,py,at.angle,
                    Some((&mut owner,i as u16)),path_clip[i]);
                if coloured {
                    let radius=((bw as f32*0.5).powi(2)+(bh as f32*0.5).powi(2)).sqrt().ceil() as i32;
                    let side=(2*radius+1) as usize;
                    let mut coverage=vec![0u8;side*side];
                    let top=py as i32-radius;
                    blit_rotated(&mut coverage,side,side,&faded,bw,bh,radius as f32,radius as f32,at.angle,None,
                        [path_clip[i][0].saturating_sub(top),path_clip[i][1].saturating_sub(top)]);
                    path_fills.push((GlyphColorInfo{mask_x:px as i32-radius,mask_y:top,width:side,height:side,color:path_colours[i]},coverage));
                }
            }
            let (c, sn) = (at.angle.cos().abs(), at.angle.sin().abs());
            let (ex, ey) = (paint_w as f32 * 0.5 * c + paint_h as f32 * 0.5 * sn, paint_w as f32 * 0.5 * sn + paint_h as f32 * 0.5 * c);
            glyph_boxes[i] = [(px - ex).floor() as i32, (py - ey).floor() as i32, (px + ex).ceil() as i32 + 1, (py + ey).ceil() as i32 + 1];
            let radius = ((paint_w as f32 / 2.0).powi(2) + (paint_h as f32 / 2.0).powi(2)).sqrt();
            text_min_x = text_min_x.min(px - radius);
            text_min_y = text_min_y.min(py - radius);
            text_max_x = text_max_x.max(px + radius);
            text_max_y = text_max_y.max(py + radius);
        }
        if three_d {
            *self.last_glyphs3d.lock().unwrap_or_else(|e| e.into_inner()) = crate::text3d_render::Placed3d { glyphs: placed3d, ..Default::default() };
        }
        let (center_x, center_y) = (centre[0], centre[1]);

        // Clamp bounding box
        if text_min_x == f32::MAX {
            text_min_x = center_x;
            text_min_y = center_y;
            text_max_x = center_x;
            text_max_y = center_y;
        }

        if self.layout_bounds_only {
            return TitleLayout::bounds_only([text_min_x,text_min_y,text_max_x,text_max_y],glyphs.len(),scale);
        }
        // Crop the mask to a tight bounding box with padding
        let crop_x0 = ((text_min_x.floor() as i32) - max_pad).clamp(0,w as i32) as usize;
        let crop_y0 = ((text_min_y.floor() as i32) - max_pad).clamp(0,h as i32) as usize;
        let crop_x1 = ((text_max_x.ceil() as i32) + max_pad + 1).clamp(0,w as i32) as usize;
        let crop_y1 = ((text_max_y.ceil() as i32) + max_pad + 1).clamp(0,h as i32) as usize;
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
                colour_glyphs: Vec::new(),
                path_units: None,
                tracking_origins: Vec::new(),
            };
        }

        if self.gpu_layout_only {
            let roi=[crop_x0 as u32,crop_y0 as u32,crop_w as u32,crop_h as u32];
            let mut source=self.gpu_glyph_cache.borrow_mut().layout(fonts,glyphs,[0.0;2],roi,tb.color,title.primary_fx(),true);
            if let Some(source)=source.as_mut() {
                let mut visible=glyphs.iter().enumerate().filter(|(_,g)|g.width>0&&g.height>0);
                source.outlines.retain_mut(|draw| {
                    let Some((i,g))=visible.next() else {return false;};
                    let Some(at)=placed[i] else {return false;};
                    if tb.write_on<1.0 && laid[i].along>reveal {return false;}
                    let pad=if path_blur[i]>=0.5 {(path_blur[i]*2.5).ceil().max(1.0)}else{0.0};
                    let radius=((g.width as f32*0.5+pad).powi(2)+(g.height as f32*0.5+pad).powi(2)).sqrt().ceil() as i32;
                    draw.origin=[at.centre[0] as i32-radius,at.centre[1] as i32-radius];
                    draw.size=[(2*radius+1) as u32;2];draw.radians=at.angle;draw.path=true;draw.alpha=path_alpha[i];
                    draw.blur=path_blur[i];draw.clip_y=path_clip[i];
                    if coloured {draw.color=Some(path_colours[i]);}
                    true
                });
                if coloured {source.face_draws=source.outlines.clone();}
                source.bounds=[text_min_x,text_min_y,text_max_x,text_max_y];
                source.gradient=tb.gradient.clone().filter(|g|g.enabled);
                source.pattern=tb.pattern_fill.filter(|_|tb.pattern_fill_enabled);
                if tb.outline_write_on<1.0 {source.outline_cutoff=(text_min_x+(text_max_x-text_min_x)*tb.outline_write_on.clamp(0.0,1.0)).floor() as i32;}
                if let TitleBackground::Color{color,padding}=title.background {
                    let pad=padding*scale;
                    let x0=((text_min_x-pad) as i32).max(0) as u32;let y0=((text_min_y-pad) as i32).max(0) as u32;
                    let x1=((text_max_x+pad) as i32).clamp(0,width as i32) as u32;let y1=((text_max_y+pad) as i32).clamp(0,height as i32) as u32;
                    if x1>x0&&y1>y0 {source.backing=Some(([x0,y0,x1-x0,y1-y0],color));}
                }
            }
            *self.gpu_layout_capture.borrow_mut()=source;
            return TitleLayout {alpha_mask:Vec::new(),mask_w:roi[2],mask_h:roi[3],mask_x0:roi[0],mask_y0:roi[1],
                text_top:text_min_y,text_bottom:text_max_y,text_left:text_min_x,text_right:text_max_x,glyph_count:glyphs.len(),scale,
                glyph_colors:Vec::new(),glyph_coverages:Vec::new(),glyph_index:Vec::new(),glyph_boxes:Vec::new(),glyph_parents:Vec::new(),pad:max_pad,
                colour_glyphs:Vec::new(),path_units:None,tracking_origins:Vec::new()};
        }
        for (info,_) in &mut path_fills {
            info.mask_x-=crop_x0 as i32;info.mask_y-=crop_y0 as i32;
        }
        let (glyph_colors,glyph_coverages)=path_fills.into_iter().unzip();
        let mut cropped = vec![0u8; crop_w * crop_h];
        let mut glyph_index = vec![GLYPH_NONE; crop_w * crop_h];
        for cy in 0..crop_h {
            let src_y = crop_y0 + cy;
            if src_y >= mask_h { continue; }
            for cx in 0..crop_w {
                let src_x = crop_x0 + cx;
                if src_x >= mask_w { continue; }
                cropped[cy * crop_w + cx] = alpha_mask[src_y * mask_w + src_x];
                glyph_index[cy * crop_w + cx] = owner[src_y * mask_w + src_x];
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
            glyph_colors,
            glyph_coverages,
            glyph_index,
            glyph_boxes,
            glyph_parents: glyphs.iter().map(|g| g.parent).collect(),
            pad: max_pad,
            colour_glyphs: Vec::new(),
            path_units: Some(text_on_path::PathUnits { laid, length, resting: placed }),
            tracking_origins: Vec::new(),
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
        let w = width as usize;
        let h = height as usize;
        // Only the rows the layer draws in are worked and made bytes
        // (#1725): a full-frame card fills all, else the text's mask, its
        // effects' room included, and its backing. The working buffer is
        // this thread's, kept zero between layers: a 4K frame of floats
        // made afresh, and cleared, each layer cost more than the layer.
        let full = matches!(title.background, TitleBackground::FullFrame { .. });
        if layout.glyph_count == 0 || layout.mask_w == 0 || layout.mask_h == 0 {
            if !full {
                return vec![0u8; w * h * 4];
            }
            let mut buf = vec![0f32; w * h * 4];
            fill_full_frame(&mut buf, title);
            return to_bytes(&buf);
        }
        let (y0, y1) = if full {
            (0, h)
        } else {
            let (mut y0, mut y1) = ((layout.mask_y0 as usize).min(h), (layout.mask_y0 as usize + layout.mask_h as usize).min(h));
            if let TitleBackground::Color { padding, .. } = &title.background {
                let pad = padding * layout.scale;
                y0 = y0.min(((layout.text_top - pad) as i32).max(0) as usize);
                y1 = y1.max((((layout.text_bottom + pad) as i32).max(0) as usize).min(h));
            }
            (y0, y1)
        };
        COMPOSE_SCRATCH.with_borrow_mut(|scratch| {
            scratch.resize(w * h * 4, 0.0);
            let out = Self::compose_into(scratch, layout, title, width, height, gpu, time_secs, (y0, y1));
            #[cfg(debug_assertions)]
            debug_assert!(
                scratch[..y0 * w * 4].iter().chain(&scratch[y1 * w * 4..]).all(|v| *v == 0.0),
                "a title layer drew outside its rows {y0}..{y1}"
            );
            // Kept zero for the next layer.
            if full {
                scratch.fill(0.0);
            } else {
                scratch[y0 * w * 4..y1 * w * 4].fill(0.0);
            }
            out
        })
    }

    /// [`Self::compose`] into `buf`, a zero frame, which only rows
    /// `rows` of are drawn in and made bytes.
    #[allow(clippy::too_many_arguments)]
    fn compose_into(
        buf: &mut [f32],
        layout: &TitleLayout,
        title: &TitleData,
        width: u32,
        height: u32,
        gpu: Option<&mut dyn AlphaMaskProcessor>,
        time_secs: Option<f32>,
        rows: (usize, usize),
    ) -> Vec<u8> {
        let tb = title.primary_text_or_default();
        let (w, h) = (width as usize, height as usize);
        let full = matches!(title.background, TitleBackground::FullFrame { .. });
        // The working buffer is float, on the same 0..255 scale the
        // layers already compute in. A title is a stack — a backing
        // panel, a shadow, a glow, an outline, the glyphs — and each
        // one used to be rounded into eight bits before the next one
        // read it back, so five layers meant five roundings, each
        // throwing away what the next needed. It is rounded once now,
        // where the buffer leaves this function.
        fill_full_frame(buf, title);

        let scale = layout.scale;
        let mask_w = layout.mask_w as usize;
        let mask_h = layout.mask_h as usize;
        let mask_x0 = layout.mask_x0 as usize;
        let mask_y0 = layout.mask_y0 as usize;
        let alpha_mask = &layout.alpha_mask;

        paint_backing(buf, title, [layout.text_left, layout.text_top, layout.text_right, layout.text_bottom], width, height, scale);

        // Determine if GPU acceleration should be used (> 250K pixels in mask)
        let use_gpu = gpu.is_some() && (mask_w * mask_h > 250_000);
        let mut gpu = gpu;

        // ── Render effects back-to-front ──

        // 1–3. What the stack puts behind the face: glows, shadows,
        // outlines. Stroke draw-on clips the outlines horizontally to a
        // left-anchored fraction of the text bounds; at 1.0 every pixel
        // passes.
        {
            let t = time_secs.map(|t| t as f64).unwrap_or(0.0);
            let outline_progress = tb.outline_write_on.clamp(0.0, 1.0);
            let outline_cutoff_x = if outline_progress >= 1.0 {
                i32::MAX
            } else {
                (layout.text_left + (layout.text_right - layout.text_left) * outline_progress).floor() as i32
            };
            stack_behind(buf, alpha_mask, mask_w, mask_h, mask_x0, mask_y0, w, h, title.primary_fx(), scale, t, outline_cutoff_x, use_gpu, &mut gpu);
        }

        // 4. 3D Text Extrusion (rendered before face text, after outlines)
        // — depth / angle / lighting may animate via keyframes.
        if let Some(ext) = tb.extrusion.as_ref().filter(|e| e.enabled) {
            render_extrusion(buf, alpha_mask, mask_w, mask_h, mask_x0, mask_y0, w, h, ext, title, time_secs);
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

            // A colour glyph's pixels are its own colours, not the
            // fill's: the fill draws round them (#1503).
            let face_mask: std::borrow::Cow<[u8]> = if layout.colour_glyphs.is_empty() {
                std::borrow::Cow::Borrowed(alpha_mask)
            } else {
                let mut m = alpha_mask.clone();
                colour_pixels(layout, |i, _, _, c| m[i] = m[i].saturating_sub(c[3]));
                std::borrow::Cow::Owned(m)
            };
            let alpha_mask = &*face_mask;
            if let Some(pattern) = tb.pattern_fill.as_ref().filter(|_| tb.pattern_fill_enabled) {
                draw_pattern_fill(buf, alpha_mask, mask_w, mask_h, mask_x0, mask_y0, w, h, pattern, scale);
            } else if let Some(gradient) = tb.gradient.as_ref().filter(|g| g.enabled && !g.is_pair()) {
                // Stops at an angle or out from a centre, across the
                // text block, mixed in linear light (#1509).
                let ramp = klipp_core::fill_gradient::GradientRamp::new(&gradient.stops());
                let (left, top) = (layout.text_left, layout.text_top);
                let bw = (layout.text_right - left).max(1.0);
                let bh = (layout.text_bottom - top).max(1.0);
                for my in 0..mask_h {
                    let py = mask_y0 + my;
                    if py >= h { continue; }
                    let v = (py as f32 + 0.5 - top) / bh;
                    for mx in 0..mask_w {
                        let cov = alpha_mask[my * mask_w + mx];
                        if cov == 0 { continue; }
                        let px = mask_x0 + mx;
                        if px >= w { continue; }
                        let u = (px as f32 + 0.5 - left) / bw;
                        let c = ramp.at(gradient.kind.position_at(u, v, bw, bh));
                        let alpha = (cov as u16 * c[3] as u16 / 255) as u8;
                        blend_pixel(buf, (py * w + px) * 4, &[c[0], c[1], c[2], alpha]);
                    }
                }
            } else if let Some(gradient) = tb.gradient.as_ref().filter(|g| g.enabled) {
                // The two colours top to bottom, mixed in sRGB row by
                // row as they always were.
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
                        blend_pixel(buf, idx, &tinted);
                    }
                }
            } else if !layout.glyph_colors.is_empty() {
                // Rich text: draw each glyph individually with its own color
                draw_rich_text_fill(
                    buf, w, h, mask_x0, mask_y0, mask_w, mask_h,
                    &layout.glyph_colors, &layout.glyph_coverages,
                );
            } else {
                draw_mask_to_buf(buf, alpha_mask, mask_w, mask_h, mask_x0, mask_y0, w, h, &face_color, 1.0);
            }
            // The colour glyphs in their colours (#1503).
            colour_pixels(layout, |_, px, py, c| {
                if px < w && py < h {
                    blend_pixel(buf, (py * w + px) * 4, &c);
                }
            });
        }

        // 6. What the stack puts inside the face: inner shadows.
        {
            let t = time_secs.map(|t| t as f64).unwrap_or(0.0);
            stack_inside(buf, alpha_mask, mask_w, mask_h, mask_x0, mask_y0, w, h, title.primary_fx(), scale, t, use_gpu, &mut gpu);
        }

        if full {
            return to_bytes(buf);
        }
        to_bytes_rows(buf, w, rows.0, rows.1)
    }

    /// A shape's or a picture's layer with its stack and its motion on
    /// it: what `compose` does for the glyphs, done for any coverage.
    /// The stack is drawn around and inside the layer's own pixels;
    /// then the entrance, the exit and the loops pose the dressed layer
    /// as one — faded, moved, scaled about its centre, softened, or
    /// shown within its resting rows — with the layer's own height as
    /// the unit a motion's `distance` is in, as a text's font size is.
    /// With no time, the layer is at rest and only the stack applies.
    #[allow(clippy::too_many_arguments)]
    fn dress_layer(
        &mut self,
        layer: Vec<u8>,
        fx: &LayerFx,
        width: u32,
        height: u32,
        time_secs: Option<f32>,
        window: Option<f32>,
        gpu: &mut Option<&mut dyn AlphaMaskProcessor>,
    ) -> Vec<u8> {
        use klipp_core::title_motion::{eval_loops, LoopGeometry};
        let (w, h) = (width as usize, height as usize);
        let Some((x0, y0, x1, y1)) = layer_bounds(&layer, width, height) else { return layer };
        let scale = crate::frame_scale(width, height);

        // The pose, on the numbers: entrance and exit combined, then
        // the loops on top. (alpha, dx, dy, scale, blur sigma, clip)
        let mut pose = (1.0f32, 0.0f32, 0.0f32, 1.0f32, 0.0f32, false);
        let mut anchor_at = None;
        if let Some(t) = time_secs {
            let unit_px = (y1 - y0) as f32;
            let motion = whole_layer_motion(&fx.motion);
            if motion.is_active() {
                let (pe, px) = motion.progress(t, window, 0);
                anchor_at = motion.anchor_at(pe, px);
                let rest = (1.0, 0.0, 0.0, 1.0, 0.0, false);
                let a = motion.enter.map(|m| m.pose(pe, unit_px)).unwrap_or(rest);
                let b = motion.exit.map(|m| m.pose(px, unit_px)).unwrap_or(rest);
                pose = (a.0 * b.0, a.1 + b.1, a.2 + b.2, a.3 * b.3, a.4.max(b.4), a.5 || b.5);
            }
            if !fx.loops.is_empty() {
                let geo = LoopGeometry { frame_w: w as f32, frame_h: h as f32, text_w: (x1 - x0) as f32, text_h: unit_px, font_px: unit_px };
                let lp = eval_loops(&fx.loops, t, window, geo, [255, 255, 255, 255]);
                pose.0 *= lp.opacity;
                pose.1 += lp.dx;
                pose.2 += lp.dy;
            }
        }

        // The stack, around and inside the layer's own coverage.
        let mut layer = layer;
        if stack_draws(fx) {
            let pad = stack_reach(fx, scale);
            let (mx0, my0) = (x0.saturating_sub(pad), y0.saturating_sub(pad));
            let (mx1, my1) = ((x1 + pad).min(w), (y1 + pad).min(h));
            let (mw, mh) = (mx1 - mx0, my1 - my0);
            let mut mask = vec![0u8; mw * mh];
            for my in 0..mh {
                for mx in 0..mw {
                    mask[my * mw + mx] = layer[((my0 + my) * w + mx0 + mx) * 4 + 3];
                }
            }
            let use_gpu = gpu.is_some() && mw * mh > 250_000;
            let t = time_secs.map(|t| t as f64).unwrap_or(0.0);
            let mut buf = vec![0f32; w * h * 4];
            stack_behind(&mut buf, &mask, mw, mh, mx0, my0, w, h, fx, scale, t, i32::MAX, use_gpu, gpu);
            for (i, px) in layer.chunks_exact(4).enumerate() {
                if px[3] > 0 {
                    blend_pixel(&mut buf, i * 4, &[px[0], px[1], px[2], px[3]]);
                }
            }
            stack_inside(&mut buf, &mask, mw, mh, mx0, my0, w, h, fx, scale, t, use_gpu, gpu);
            layer = to_bytes(&buf);
        }

        // Then the pose, on the dressed layer.
        if pose.4 > 0.5 {
            blur_rgba(&mut layer, width, height, pose.4);
        }
        let at_rest = (pose.0 - 1.0).abs() < 1e-6 && pose.1.abs() < 0.5 && pose.2.abs() < 0.5 && (pose.3 - 1.0).abs() < 1e-6 && !pose.5;
        if !at_rest {
            // The dressed layer as the box it covers, posed, rounded once.
            let covered = alpha_rows(&layer, width, height);
            let rest = BoxRaster::from_bytes(&layer, (width, height), covered, alpha_cols(&layer, width, height, covered));
            let anchor = resolve_anchor(anchor_at, &rest, (x0 as u32, x1 as u32), (y0 as u32, y1 as u32));
            let rows = if pose.5 { (y0 as u32, y1 as u32) } else { (0, height) };
            layer = to_bytes(&pose_sprite(&rest, width, height, pose.1.round() as i32, pose.2.round() as i32, pose.0, pose.3, anchor, None, None, rows));
        }
        layer
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
            return hit.pixels.to_bytes();
        }
        let raster = self.raster_at(title, width, height, gpu, None);
        self.last_rows = raster_rows(&raster);
        let bytes = raster.to_bytes();
        self.cache_store(title.clone(), width, height, raster);
        bytes
    }

    /// [`Self::render`] kept as the box it covers in half floats (#190),
    /// through the same cache: for a caller that keeps more than eight
    /// bits.
    pub fn render_raster(&mut self, title: &TitleData, width: u32, height: u32) -> std::sync::Arc<BoxRaster> {
        if let Some(hit) = self.cache_lookup(title, width, height) {
            self.last_rows = hit.rows;
            return hit.pixels;
        }
        let raster = self.raster_at(title, width, height, None, None);
        self.last_rows = raster_rows(&raster);
        self.cache_store(title.clone(), width, height, raster).pixels
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
        to_bytes(&self.render_with_window_stack(title, width, height, gpu, time_secs, window_secs))
    }

    /// [`Self::render_with_window`] unrounded (#190): straight-alpha sRGB
    /// in byte units 0..255, as the layers were stacked and the frame
    /// posed, for a compositor that keeps more than eight bits.
    pub fn render_with_window_stack(
        &mut self,
        title: &TitleData,
        width: u32,
        height: u32,
        gpu: Option<&mut dyn AlphaMaskProcessor>,
        time_secs: f32,
        window_secs: Option<f32>,
    ) -> Vec<f32> {
        self.motion_window = window_secs;
        let out = self.stack_with_time(title, width, height, gpu, time_secs);
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

    /// The frame at `time_secs` of `title`, whose `text` travels along
    /// its drawn path, as the resting render's glyphs posed (#1697): each
    /// moved and turned from where it stood on the path at rest to where
    /// it stands now. `None` when the resting render holds no glyph map,
    /// has too many glyphs to pose, or lacks a glyph the path now shows.
    fn path_glyph_frame(&mut self, title: &TitleData, text: &TitleObject, width: u32, height: u32, gpu: Option<&mut dyn AlphaMaskProcessor>, time_secs: f32) -> Option<TitleFrame> {
        let TitleObjectContent::TextBox(tb) = &text.content else { return None };
        let path = tb.text_path.as_ref()?;
        let sprite = self.resting_sprite(&resting_title(title), width, height, gpu);
        let units = sprite.units.clone().filter(|u| u.boxes.len() <= MAX_POSED_GLYPHS)?;
        let at = units.path.as_ref()?;
        let line = self.bound_path_line(title, title, Some(time_secs), width, height);
        let shift = tb.baseline_shift * crate::frame_scale(width, height);
        let now = text_on_path::place(path, line.as_ref(), path.offset_at(time_secs as f64), &at.laid, at.length, [0.0; 2], shift);
        let pad = units.pad as f32;
        let mut poses = Vec::with_capacity(units.boxes.len());
        for (g, b) in units.boxes.iter().enumerate() {
            let rest = at.resting.get(g).copied().flatten();
            let here = now.get(g).copied().flatten();
            let hidden = GlyphPose { dx: 0.0, dy: 0.0, scale: 1.0, alpha: 0.0, anchor: (0.0, 0.0), clip: None, moved: [0; 4], rotation: 0.0 };
            let pose = match (rest, here) {
                // A glyph off the path at rest has no pixels to pose.
                (None, Some(_)) => return None,
                (_, None) => hidden,
                (Some(rest), Some(here)) if b[2] > b[0] && b[3] > b[1] => {
                    let (dx, dy) = (here.centre[0] - rest.centre[0], here.centre[1] - rest.centre[1]);
                    let turn = (here.angle - rest.angle + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
                    // The box round the resting box turned again, moved.
                    let (c, sn) = (turn.cos().abs(), turn.sin().abs());
                    let (hx, hy) = ((b[2] - b[0]) as f32 * 0.5 + pad, (b[3] - b[1]) as f32 * 0.5 + pad);
                    let (ex, ey) = (hx * c + hy * sn, hx * sn + hy * c);
                    let anchor = (rest.centre[0], rest.centre[1]);
                    let moved = [
                        (anchor.0 + dx - ex).floor() as i32 - 1,
                        (anchor.1 + dy - ey).floor() as i32 - 1,
                        (anchor.0 + dx + ex).ceil() as i32 + 1,
                        (anchor.1 + dy + ey).ceil() as i32 + 1,
                    ];
                    GlyphPose { dx, dy, scale: 1.0, alpha: 1.0, anchor, clip: None, moved, rotation: turn }
                }
                _ => hidden,
            };
            poses.push(pose);
        }
        let (mut y0, mut y1) = (i32::MAX, i32::MIN);
        for p in &poses {
            if p.alpha > 0.001 && p.moved[3] > p.moved[1] {
                y0 = y0.min(p.moved[1]);
                y1 = y1.max(p.moved[3]);
            }
        }
        let rows = if y1 <= y0 { (0, 0) } else { (y0.clamp(0, height as i32) as u32, y1.clamp(0, height as i32) as u32) };
        self.last_rows = rows;
        self.last_blur = 0.0;
        Some(TitleFrame::Glyphs { sprite, poses, rows })
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
        to_bytes(&self.stack_with_time(title, width, height, gpu, time_secs))
    }

    /// [`Self::render_with_time`] unrounded, in byte units.
    fn stack_with_time(&mut self, title: &TitleData, width: u32, height: u32, gpu: Option<&mut dyn AlphaMaskProcessor>, time_secs: f32) -> Vec<f32> {
        match self.frame_at(title, width, height, gpu, time_secs) {
            TitleFrame::Placed { sprite, dx, dy, opacity, scale, anchor, under, shape, rows, .. } => {
                pose_sprite(&sprite.pixels, width, height, dx, dy, opacity, scale, anchor, under, shape, rows)
            }
            TitleFrame::Glyphs { sprite, poses, rows } => match &sprite.units {
                Some(units) => pose_sprite_glyphs(&sprite.pixels, width, height, units, &poses, rows),
                None => vec![0.0; (width as usize) * (height as usize) * 4],
            },
            TitleFrame::Rendered { pixels, .. } => pixels.to_stack(),
            TitleFrame::Layers { layers, rows } => compose_layers(&layers, width, height, rows),
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
        // A focus blur is the placed frame's alone: every other way out
        // of here asks for none (#598 review).
        self.last_focus_blur = None;
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
        let motion_live = |o: &TitleObject| -> bool {
            // A shape or a picture moves as one: no stagger, one unit.
            let text = match &o.content { TitleObjectContent::TextBox(tb) => tb.text.as_str(), _ => "" };
            let fx = o.fx();
            // How long a motion runs: its own time, plus the stagger of
            // its last unit — and only when it staggers. The stagger time
            // has a default of its own, and counting it for a motion with
            // no stagger made a 0.4 s fade on a 22-letter name "live" for
            // 1.55 s at each end: a four-second lower third was rastered
            // afresh on three frames in four, 110 ms each.
            let units = |m: &klipp_timeline::Motion| -> f32 {
                match m.stagger {
                    klipp_timeline::StaggerUnit::None => 0.0,
                    klipp_timeline::StaggerUnit::Line => text.lines().count().max(1) as f32,
                    klipp_timeline::StaggerUnit::Word => text.split_whitespace().count().max(1) as f32,
                    klipp_timeline::StaggerUnit::Char => text.chars().filter(|c| !c.is_whitespace()).count().max(1) as f32,
                }
            };
            let span = |m: &klipp_timeline::Motion| m.delay_secs + m.duration_secs + units(m) * m.stagger_secs + 0.05;
            let enter = fx.motion.enter.as_ref().is_some_and(|m| time_secs < span(m));
            let exit = match (fx.motion.exit.as_ref(), window) {
                (Some(m), Some(w)) => (w - time_secs) < span(m),
                _ => false,
            };
            enter || exit
        };
        // The loops, evaluated up front: a loop that only moves or
        // fades the layer can be applied to the cached resting render.
        let scale = crate::frame_scale(width, height);
        let poses: Vec<(usize, klipp_timeline::LoopPose)> = title
            .objects
            .iter()
            .enumerate()
            .filter_map(|(i, o)| {
                let TitleObjectContent::TextBox(tb) = &o.content else { return None };
                let fx = o.fx();
                if fx.loops.is_empty() || !o.visible {
                    return None;
                }
                // A roll travels the text as it is now (#1511).
                let keyed;
                let tb = if tb.has_keyframes() {
                    keyed = tb.at(time_secs as f64);
                    &keyed
                } else {
                    tb
                };
                let font_px = tb.font_size * scale;
                let (text_w, text_h) = tb.loop_extent(font_px);
                let geo = klipp_timeline::LoopGeometry { frame_w: width as f32, frame_h: height as f32, text_w, text_h, font_px };
                let mut pose = klipp_core::title_motion::eval_loops(&fx.loops, time_secs, window, geo, tb.color);
                // A colour loop at the layer's own colour (a blend of it
                // with itself) changes nothing to raster for (#2069).
                if pose.color == Some(tb.color) {
                    pose.color = None;
                }
                Some((i, pose))
            })
            .collect();
        // A wave alone bobs each glyph: posed one by one on the GPU, as a
        // staggered motion is (#2069), on one visible layer that is not
        // scaling as a whole, which would scale it round the layer and not
        // round each glyph.
        let wave = match poses.as_slice() {
            [(i, p)] if p.color.is_none() && p.text.is_none() => p.wave.map(|w| (*i, w)),
            _ => None,
        };
        let loops_need_raster = poses.iter().any(|(_, p)| p.color.is_some() || p.text.is_some()) || (poses.iter().any(|(_, p)| p.wave.is_some()) && wave.is_none());
        let any_motion_live = title.objects.iter().any(|o| o.visible && motion_live(o));
        // A layer's animators pose its glyphs one by one, from the
        // resting render where they only move, turn, scale or fade it;
        // a blur or a colour is rastered, for that layer alone (#1510).
        let animates = |o: &TitleObject| o.visible && matches!(o.content, TitleObjectContent::TextBox(_)) && o.fx().animators.iter().any(|a| a.enabled);
        let animators_raster = |o: &TitleObject| o.fx().animators.iter().any(|a| a.enabled && (a.props.blur != 0.0 || a.props.colour.is_some()));
        let any_animating = title.objects.iter().any(animates);
        // One visible layer carrying the only pose: the cached resting
        // render, shifted and faded, is the frame.
        let visible = title.objects.iter().filter(|o| o.visible).count();
        // A title whose layers blend or mask is posed only at rest: a
        // pose on its render would move the mixed picture, matte and
        // all, and not the one layer (#1513). It is rastered instead.
        let single = visible == 1 && !title.mixes_layers();
        let whole_scale = |o: &TitleObject| {
            let TitleObjectContent::TextBox(tb) = &o.content else { return 1.0 };
            let fx = o.fx();
            let font_px = tb.font_size * scale;
            let (pe, px) = fx.motion.progress(time_secs, window, 0);
            let rest = (1.0, 0.0, 0.0, 1.0, 0.0, false);
            fx.motion.enter.map(|m| m.pose(pe, font_px)).unwrap_or(rest).3 * fx.motion.exit.map(|m| m.pose(px, font_px)).unwrap_or(rest).3
        };
        let wave = wave.filter(|(i, _)| single && title.objects.get(*i).is_some_and(|o| o.visible && (whole_scale(o) - 1.0).abs() < 1e-6));
        let loops_need_raster = loops_need_raster || (poses.iter().any(|(_, p)| p.wave.is_some()) && wave.is_none());
        let translate_only = match poses.as_slice() {
            [(_, p)] => (!loops_need_raster && single).then(|| p.clone()),
            _ => None,
        };
        // An entrance or exit that moves, fades or scales the whole
        // layer — no stagger, no blur — is a pose on the resting render,
        // as a loop is: the frame is the sprite placed, on the GPU where
        // the compositor keeps it. Rastered afresh, it cost 30 ms a
        // frame at 4K for the whole of a half-second rise.
        // (opacity, dx, dy, scale, blur sigma, clip to the resting box)
        let mut motion_anchor = None;
        let motion_pose: Option<(f32, f32, f32, f32, f32, bool)> = if any_motion_live && !any_animating && single && !loops_need_raster && wave.is_none() {
            title.objects.iter().find(|o| o.visible).and_then(|o| {
                let TitleObjectContent::TextBox(tb) = &o.content else { return None };
                let fx = o.fx();
                let whole = |m: Option<klipp_timeline::Motion>| m.is_none_or(|m| m.stagger == klipp_timeline::StaggerUnit::None);
                if !(whole(fx.motion.enter) && whole(fx.motion.exit)) {
                    return None;
                }
                let font_px = tb.font_size * scale;
                let (pe, px) = fx.motion.progress(time_secs, window, 0);
                motion_anchor = fx.motion.anchor_at(pe, px);
                let rest = (1.0, 0.0, 0.0, 1.0, 0.0, false);
                let (a1, dx1, dy1, s1, b1, c1) = fx.motion.enter.map(|m| m.pose(pe, font_px)).unwrap_or(rest);
                let (a2, dx2, dy2, s2, b2, c2) = fx.motion.exit.map(|m| m.pose(px, font_px)).unwrap_or(rest);
                Some((a1 * a2, dx1 + dx2, dy1 + dy2, s1 * s2, b1.max(b2), c1 || c2))
            })
        } else {
            None
        };
        // A motion that staggers by character, word or line poses each
        // glyph on its own from the resting render's glyph map — the
        // frame is the glyphs placed, on the GPU. A blur cannot be
        // posed per glyph and is rastered; nor can a 3D text's glyphs,
        // whose solids turn and scale in depth and are posed as they are
        // drawn (#1735).
        let staggered: Option<&TitleObject> = if (any_motion_live || any_animating || wave.is_some()) && motion_pose.is_none() && single && !loops_need_raster {
            title.objects.iter().find(|o| o.visible).and_then(|o| match &o.content {
                TitleObjectContent::TextBox(tb)
                    if tb.three_d.as_ref().is_none_or(|t| !t.enabled)
                        && o.fx().motion.enter.is_none_or(|m| !m.kind.blurs())
                        && o.fx().motion.exit.is_none_or(|m| !m.kind.blurs())
                        && !animators_raster(o) =>
                {
                    Some(o)
                }
                _ => None,
            })
        } else {
            None
        };
        let any_path_motion = title.objects.iter().any(|o| o.visible && matches!(&o.content, TitleObjectContent::Shape(Shape {kind:ShapeKind::Path(p),..}) if !p.keyframes.is_empty() || p.trim.as_ref().is_some_and(|t|!t.keyframes.is_empty())));
        // A lone drawn path whose geometry stays put, moved or faded whole
        // by its motion or loops: the resting render posed as `dress_layer`
        // poses the painted layer — the same `pose_sprite`, the same rounded
        // offset — so it is placed, on the GPU, not rastered each frame.
        // Nothing here scales, blurs or clips it, and nothing rounds between
        // the pose and the frame: no stack, no edge fade, no opacity.
        let lone_shape = if single && title.background == TitleBackground::None {
            title.objects.iter().find(|o| o.visible).filter(|o| {
                use klipp_timeline::MotionKind as K;
                let TitleObjectContent::Shape(Shape { kind: ShapeKind::Path(path), fit: None, .. }) = &o.content else { return false };
                let fx = o.fx();
                let whole = |m: &Option<klipp_timeline::Motion>| m.is_none_or(|m| matches!(m.kind,
                    K::Fade | K::Rise | K::Drop | K::SlideLeft | K::SlideRight | K::FlyLeft | K::FlyRight | K::FlyTop | K::FlyBottom | K::Bounce | K::Flicker | K::Typewriter));
                path.keyframes.is_empty() && path.trim.as_ref().is_none_or(|t| t.keyframes.is_empty())
                    && o.opacity == 1.0 && o.opacity_keyframes.is_empty() && fx.edge_fade.is_none() && !stack_draws(fx)
                    && fx.motion_blur.is_none() && whole(&fx.motion.enter) && whole(&fx.motion.exit)
                    && fx.loops.iter().all(|l| matches!(l, klipp_timeline::TitleLoop::Blink { .. } | klipp_timeline::TitleLoop::Pulse { .. } | klipp_timeline::TitleLoop::Shake { .. }))
            })
        } else {
            None
        };
        let any_char_anim = (any_motion_live && motion_pose.is_none() && staggered.is_none() && lone_shape.is_none()) || (!poses.is_empty() && translate_only.is_none())
            || (any_animating && staggered.is_none());

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
        let any_transform_keyframes = title.objects.iter().any(|o| {
            !o.transform.size_x_keyframes.is_empty()
                || !o.transform.size_y_keyframes.is_empty()
                || !o.transform.rotation_keyframes.is_empty()
                || !o.transform.anchor_x_keyframes.is_empty()
                || !o.transform.anchor_y_keyframes.is_empty()
        });

        // Per-effect keyframes: TextGlow's radius/intensity animate
        // when their respective vecs are non-empty. Same cache-bypass
        // requirement as position keyframes.
        let any_glow_keyframes = title.objects.iter().any(|o| {
            o.fx().glows.iter().any(|g| !g.radius_keyframes.is_empty() || !g.intensity_keyframes.is_empty())
        });
        let any_shadow_keyframes = title.objects.iter().any(|o| {
            o.fx().shadows.iter().any(|s| {
                !s.offset_x_keyframes.is_empty() || !s.offset_y_keyframes.is_empty() || !s.blur_keyframes.is_empty()
            })
        });
        let any_inner_shadow_keyframes = title.objects.iter().any(|o| {
            o.fx().inner_shadows.iter().any(|s| {
                !s.offset_x_keyframes.is_empty() || !s.offset_y_keyframes.is_empty() || !s.blur_keyframes.is_empty()
            })
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
        let any_outline_keyframes = title.objects.iter().any(|o| o.fx().outlines.iter().any(|ol| !ol.width_keyframes.is_empty()));
        // A keyed size, tracking, spacing or colour sets the text afresh
        // at each time (#1511).
        let any_type_keyframes = title.objects.iter().any(|o| matches!(&o.content, TitleObjectContent::TextBox(tb) if tb.has_keyframes()));
        let any_opacity_keyframes = title.objects.iter().any(|o| !o.opacity_keyframes.is_empty());

        // A shape or a picture with a live entrance, exit or loop is
        // posed on its own layer at raster time, so its frame is its
        // time's and the resting render cannot stand in for it.
        let any_dressed_motion = title.objects.iter().any(|o| {
            o.visible && !matches!(o.content, TitleObjectContent::TextBox(_)) && (motion_live(o) || !o.fx().loops.is_empty())
                && !lone_shape.is_some_and(|l| std::ptr::eq(l, o))
        });
        // A text travelling along a drawn path (#1508); the path's own
        // morph and its layer's keys are counted above.
        let any_text_path_motion = title.objects.iter().any(|o| matches!(&o.content, TitleObjectContent::TextBox(tb) if matches!(&tb.text_path, Some(TextPath::Layer { offset_keyframes, .. }) if !offset_keyframes.is_empty())));
        let any_render_time_override = any_path_motion || any_text_path_motion || any_char_anim || any_dressed_motion
            || any_glitch_active
            || any_position_keyframes
            || any_transform_keyframes
            || any_glow_keyframes
            || any_shadow_keyframes
            || any_inner_shadow_keyframes
            || any_extrusion_keyframes
            || any_outline_keyframes
            || any_type_keyframes;

        let any_render_time_override = any_render_time_override || any_opacity_keyframes;
        // A staggered entrance on a layer that fades by where it is: the
        // glyphs are posed one by one on the GPU, where the fade is not,
        // so it is rastered.
        let any_render_time_override = any_render_time_override
            || (staggered.is_some_and(|o| !o.fx().edge_fade.is_none()) || staggered.is_some() && poses.iter().any(|(_, p)| p.focus.is_some()));

        let mut gpu = gpu;
        // Keep the same layer sprites through the fades and their resting
        // interval, rather than rastering a combined title between fades.
        if let Some(frame) = self.fading_layers(title, width, height, &mut gpu, time_secs) {
            self.last_blur = 0.0;
            return frame;
        }
        if tracking::eligible(title) {
            if let Some(frame) = self.tracking_frame(title, width, height, time_secs) {
                return frame;
            }
        }
        // A text whose only motion is its drawn path's (it travels, the
        // path morphs or moves) poses its resting glyphs along the path
        // as the sprite path does a stagger (#1697).
        let others_move = any_char_anim || any_dressed_motion || any_glitch_active || any_glow_keyframes || any_shadow_keyframes
            || any_inner_shadow_keyframes || any_extrusion_keyframes || any_outline_keyframes || any_type_keyframes || any_opacity_keyframes
            || title.background != TitleBackground::None;
        if !others_move && (any_path_motion || any_text_path_motion || any_position_keyframes || any_transform_keyframes) {
            if let Some(frame) = path_traveller(title).and_then(|text| self.path_glyph_frame(title, text, width, height, gpu.take(), time_secs)) {
                return frame;
            }
        }
        // Key the existing bounded sprite cache by the evaluated geometry,
        // not by the presence of animation keys. Other time-dependent title
        // features still go through their normal evaluation below.
        if let Some(evaluated) = vector_path::evaluated_title(title, time_secs as f64) {
            return self.frame_at(&evaluated, width, height, gpu, time_secs);
        }
        if !any_render_time_override {
            // A full-frame card: the words are the sprite and the fill
            // goes under them at placement, so the fill stays put while
            // the words move, and one resting render serves the card
            // moving and at rest.
            let under = match &title.background {
                TitleBackground::FullFrame { color } if card_goes_under(title) => Some(*color),
                _ => None,
            };
            let sprite = self.resting_sprite(&resting_title(title), width, height, gpu.take());
            let rows = sprite.rows;
            if let Some(o) = staggered {
                if let Some(units) = sprite.units.clone().filter(|u| u.boxes.len() <= MAX_POSED_GLYPHS) {
                    let font_px = match &o.content { TitleObjectContent::TextBox(tb) => tb.font_size, _ => 72.0 } * scale;
                    let loop_pose = translate_only.as_ref().map(|p| (p.dx, p.dy, p.opacity)).unwrap_or((0.0, 0.0, 1.0));
                    let mut poses = glyph_poses(&units, &o.fx().motion, &o.fx().animators, time_secs, window, font_px, loop_pose, under.is_some());
                    // The wave's bob of each glyph, as the rasteriser
                    // bobs it (`rasterize_to_mask_motion`).
                    if let Some((_, (amp, rate))) = wave {
                        for (g, p) in poses.iter_mut().enumerate() {
                            let bob = (time_secs * rate * std::f32::consts::TAU + g as f32 * 0.5).sin() * amp;
                            p.dy += bob;
                            if p.moved[3] > p.moved[1] {
                                p.moved[1] += bob.floor() as i32;
                                p.moved[3] += bob.ceil() as i32;
                                if let Some((y0, y1)) = p.clip {
                                    p.moved[1] = p.moved[1].max(y0);
                                    p.moved[3] = p.moved[3].min(y1);
                                }
                            }
                        }
                    }
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
            if let Some(o) = lone_shape {
                // `dress_layer`'s pose: the layer's own height the unit its
                // motion travels in, its loops on top.
                let fx = o.fx();
                let unit_px = sprite.rows.1.saturating_sub(sprite.rows.0) as f32;
                let motion = whole_layer_motion(&fx.motion);
                if motion.is_active() {
                    let (pe, px) = motion.progress(time_secs, window, 0);
                    let rest = (1.0, 0.0, 0.0, 1.0, 0.0, false);
                    let a = motion.enter.map(|m| m.pose(pe, unit_px)).unwrap_or(rest);
                    let b = motion.exit.map(|m| m.pose(px, unit_px)).unwrap_or(rest);
                    opacity *= a.0 * b.0;
                    dx += a.1 + b.1;
                    dy += a.2 + b.2;
                }
                if !fx.loops.is_empty() {
                    let geo = klipp_timeline::LoopGeometry { frame_w: width as f32, frame_h: height as f32, text_w: sprite.cols.1.saturating_sub(sprite.cols.0) as f32, text_h: unit_px, font_px: unit_px };
                    let lp = klipp_core::title_motion::eval_loops(&fx.loops, time_secs, window, geo, [255, 255, 255, 255]);
                    opacity *= lp.opacity;
                    dx += lp.dx;
                    dy += lp.dy;
                }
            }
            let (dx, dy, opacity) = (dx.round() as i32, dy.round() as i32, opacity.clamp(0.0, 1.0));
            let anchor = match motion_anchor.filter(|_| motion_pose.is_some()) {
                Some(klipp_core::title_motion::MotionAnchor::Solid) => {
                    let solid = match self.solid_point {
                        Some((id, p)) if id == sprite.id => p,
                        _ => {
                            let p = solid_point(&sprite.pixels, sprite.cols, sprite.rows);
                            self.solid_point = Some((sprite.id, p));
                            p
                        }
                    };
                    solid.unwrap_or_else(|| sprite.anchor())
                }
                Some(a) => resolve_anchor(Some(a), &sprite.pixels, sprite.cols, sprite.rows),
                None => sprite.anchor(),
            };
            // One layer: its fade goes on where it is placed; the resting
            // render was made without it. More layers: each was faded in
            // the resting render where it stands, since none moves here —
            // both poses above come only for one visible layer.
            let scale_px = crate::frame_scale(width, height);
            debug_assert!(single || (dx == 0 && dy == 0 && (scale - 1.0).abs() < 1e-6), "a title of several layers placed with a move would carry its baked fades with it");
            let shape = if single {
                title.objects.iter().find(|o| o.visible).and_then(|o| ScreenFade::new(o.fx().edge_fade, translate_only.as_ref().and_then(|p| p.focus)))
            } else {
                None
            };
            // A focus band that goes soft: the caller blurs the placed
            // layer by it (`last_focus_blur`), in frame pixels.
            self.last_focus_blur = shape.and_then(|s| s.focus).filter(|(_, f)| f.blur > 0.0).map(|(axis, f)| (f.blur * scale_px, axis, f.band)).filter(|(r, _, _)| *r > 0.5);
            let rows = if under.is_some() {
                (0, height)
            } else if opacity <= 0.001 || rows.1 <= rows.0 {
                (0, 0)
            } else if clip {
                // A mask motion: the words show inside their resting box only.
                rows
            } else {
                let grown = |y: u32| anchor.1 + (y as f32 - anchor.1) * scale + dy as f32;
                let reach = if blur > 0.5 { blur * 3.0 } else { 0.0 } + self.last_focus_blur.map_or(0.0, |(r, _, _)| r * 3.0);
                let y0 = grown(rows.0).floor() - 1.0 - reach;
                let y1 = grown(rows.1).ceil() + 1.0 + reach;
                (y0.clamp(0.0, height as f32) as u32, y1.clamp(0.0, height as f32) as u32)
            };
            self.last_rows = rows;
            self.last_blur = if blur > 0.5 { blur } else { 0.0 };
            return TitleFrame::Placed { sprite, dx, dy, opacity, scale, anchor, under, blur: self.last_blur, shape, rows };
            }
        }
        self.last_blur = 0.0;
        self.last_focus_blur = None;

        // Build a one-frame derived TitleData that applies every
        // active render-time override. The clone is bounded to
        // exactly one allocation per render call.
        // The loops of every layer, folded into a one-frame copy of
        // the title: opacity, offset, colour and counter text on the
        // object, the wave kept aside for the glyph rasteriser.
        self.loop_waves.clear();
        self.loop_focus.clear();
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
                if let Some(f) = pose.focus {
                    self.loop_focus.insert(obj.id.0, f);
                }
            }
            td
        };

        // Pass time through whenever ANY render-time effect needs
        // it — char_animation drives per-glyph staggering, glitch
        // seeds its stripe pattern from the clip clock, per-layer
        // position keyframes evaluate against this time inside
        // `render_stack`, and per-effect keyframes (e.g. TextGlow
        // radius/intensity) sample inside `compose`.
        // The one decision, made once above: a frame that depends on
        // its time is rastered at its time.
        // A staggered or waving layer with too many glyphs to pose falls
        // through to here, and is rastered at its time too.
        let time_arg = (any_render_time_override || staggered.is_some()).then_some(time_secs);
        let out = self.raster_at(&derived, width, height, gpu, time_arg);
        self.loop_waves.clear();
        self.loop_focus.clear();
        self.last_rows = raster_rows(&out);
        TitleFrame::Rendered { rows: self.last_rows, pixels: out }
    }


    /// [`TitleFrame::Layers`] for `title` at `time_secs`, when it is two or
    /// more visible text layers, or a single opacity-keyed text layer,
    /// each changing only by keyed opacity or a whole-layer fade
    /// ([`whole_layer_fade`]), with no backing, no mixing, no edge fade
    /// and no text on a path: each layer's resting render, as the one-text
    /// title it renders as in [`Self::render_stack`], kept as any resting
    /// render is. `None` for anything else.
    fn fading_layers(&mut self, title: &TitleData, width: u32, height: u32, gpu: &mut Option<&mut dyn AlphaMaskProcessor>, time_secs: f32) -> Option<TitleFrame> {
        if title.background != TitleBackground::None || title.mixes_layers() {
            return None;
        }
        let visible: Vec<&TitleObject> = title.objects.iter().filter(|o| o.visible).collect();
        if visible.is_empty() || !visible.iter().any(|o| !o.opacity_keyframes.is_empty() || (visible.len() > 1 && o.fx().motion.is_active())) {
            return None;
        }
        let window = self.motion_window;
        let mut plan = Vec::with_capacity(visible.len());
        for o in &visible {
            let TitleObjectContent::TextBox(tb) = &o.content else { return None };
            if tb.text_path.is_some() || ScreenFade::new(o.fx().edge_fade, None).is_some() {
                return None;
            }
            let alpha = whole_layer_fade(o, tb, time_secs, window)? * o.evaluated_opacity(time_secs as f64).clamp(0.0, 1.0);
            let mut single = TitleData::single_text_fx(tb.clone(), o.fx().clone());
            if let Some(first) = single.objects.first_mut() {
                first.transform = o.transform.clone();
            }
            plan.push((single, alpha));
        }
        let (mut y0, mut y1) = (height, 0);
        let mut layers = Vec::with_capacity(plan.len());
        for (single, alpha) in plan {
            let sprite = match gpu {
                Some(gpu) => self.resting_sprite(&single, width, height, Some(&mut **gpu)),
                None => self.resting_sprite(&single, width, height, None),
            };
            if alpha > 0.001 && sprite.rows.1 > sprite.rows.0 {
                y0 = y0.min(sprite.rows.0);
                y1 = y1.max(sprite.rows.1);
            }
            layers.push((sprite, alpha));
        }
        let rows = if y1 > y0 { (y0, y1) } else { (0, 0) };
        self.last_rows = rows;
        Some(TitleFrame::Layers { layers, rows })
    }

    /// [`Self::render_stack`] kept as the box it covers (#190).
    fn raster_at(&mut self, title: &TitleData, width: u32, height: u32, gpu: Option<&mut dyn AlphaMaskProcessor>, time_secs: Option<f32>) -> BoxRaster {
        let stack = self.render_stack(title, width, height, gpu, time_secs);
        let rows = stack_rows(&stack, width, height);
        let cols = stack_cols(&stack, width, height, rows);
        BoxRaster::from_stack(&stack, (width, height), rows, cols)
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
    ///
    /// The layers of `title` stacked in byte units but not rounded, so
    /// that each layer is rounded once, where it was drawn, and the
    /// stack once, where it leaves (#190). Empty when nothing is drawn.
    fn render_stack(
        &mut self,
        title: &TitleData,
        width: u32,
        height: u32,
        gpu: Option<&mut dyn AlphaMaskProcessor>,
        time_secs: Option<f32>,
    ) -> Vec<f32> {
        // The clip's length, for a layer's exit and loops.
        let window = self.motion_window;
        // The GPU alpha-mask processor is consumable: we use it for
        // the first TextBox layer (the most common case is one text
        // object per title — credits, lower-third title, single-line
        // banner). Additional TextBox layers in the same title fall
        // back to the CPU mask path. When per-object animation lands,
        // we'll likely refactor this to per-layer GPU dispatch via
        // the `wgpu` queue.
        let mut gpu_for_first_text = gpu;
        let pixel_count = (width as usize) * (height as usize);
        let mut buf = vec![0.0f32; pixel_count * 4];
        // A matte is its source layer alone: no fill, no backing (#1513).
        let matte_only = self.matte_only;
        // Title-level background fill (legacy field — will move to a
        // Shape object once on-canvas editing lands).
        if matte_only.is_none() {
            fill_full_frame(&mut buf, title);
        }

        // Until something is painted, `buf` is empty (unless a full
        // frame background filled it): the first text layer is the
        // buffer, no blend needed.
        let mut buf_untouched = matte_only.is_some() || !matches!(title.background, TitleBackground::FullFrame { .. });
        // A colour backing is a box round the words, so it is the text
        // layer that knows where it goes: the first text box carries it.
        // Dropped from every layer, as it was, it was never painted.
        let mut backing = (matches!(title.background, TitleBackground::Color { .. }) && matte_only.is_none()).then(|| title.background.clone());
        for obj in &title.objects {
            // A layer that blends or is masked is drawn on its own and
            // then mixed with what is below it (#1513); a matte is drawn
            // only into the mask it makes.
            let mixed = match matte_only {
                Some(id) => {
                    if obj.id != id {
                        continue;
                    }
                    false
                }
                None => {
                    if !obj.visible || title.is_matte(obj.id) {
                        continue;
                    }
                    obj.blend != klipp_core::BlendMode::Normal || title.mask_of(obj).is_some()
                }
            };
            // A mixed layer is drawn on its own frame, made only when
            // something is drawn into it: a text at full opacity is its
            // own frame already (#1725).
            let below = mixed.then(|| std::mem::take(&mut buf));
            let first_layer = buf_untouched || mixed;
            buf_untouched = false;
            let opacity = time_secs
                .map(|t| obj.evaluated_opacity(t as f64))
                .unwrap_or(obj.opacity)
                .clamp(0.0, 1.0);
            // Per-layer position keyframes: when present, override the
            // static `center` for this frame. Empty keyframe vecs
            // short-circuit to the static value via `evaluated_center`,
            // so titles without animation pay nothing extra here.
            let mut effective_transform = time_secs
                .map(|t| obj.transform.evaluated_at(t as f64))
                .unwrap_or_else(|| obj.transform.clone());
            match &obj.content {
                TitleObjectContent::TextBox(tb) => {
                    // A layer whose only change over time is a whole-layer
                    // fade (its entrance or exit): its frame is the layer at
                    // rest with its alpha scaled, as a layer's opacity is
                    // (#2095). Its shadow, glow or outline fades with it, as
                    // one: the layer faded whole, as the placed resting
                    // render of a one-layer title fades (#2069). The fade's
                    // alpha, when so.
                    let fade_alpha = time_secs.and_then(|t| whole_layer_fade(obj, tb, t, window));
                    // Synthesise a one-object TitleData carrying just
                    // this TextBox, its stack and its transform centre,
                    // then route through the existing single-text raster
                    // path. A full-frame `background` is painted once,
                    // above, and not per text layer; a colour backing
                    // goes with the first text box.
                    // Its keyed size, tracking and spacing as they are
                    // now (#1511).
                    let tb = time_secs.map_or_else(|| tb.clone(), |t| tb.at(t as f64));
                    let mut single = TitleData::single_text_fx(tb, obj.fx().clone());
                    single.background = backing.take().unwrap_or(TitleBackground::None);
                    if let Some(o) = single.objects.first_mut() {
                        o.transform = effective_transform.clone();
                    }
                    self.loop_wave = self.loop_waves.get(&obj.id.0).copied();
                    // A text on a drawn path follows that layer as it is
                    // drawn now, in frame pixels; the path places it, not
                    // its own box (#1508).
                    let on_layer = matches!(single.text_path(), Some(TextPath::Layer { .. }));
                    self.bound_path = if on_layer { self.bound_path_line(title, &single, time_secs, width, height) } else { None };
                    // At rest, the layer is the one-text title's alone: kept,
                    // and drawn again only when it changes (#2095). The GPU
                    // goes with the first text layer either way.
                    let gpu = gpu_for_first_text.take();
                    let still = time_secs.is_none() && self.loop_wave.is_none() && !on_layer;
                    let faded = fade_alpha.filter(|_| self.loop_wave.is_none() && !on_layer);
                    let mut gpu = gpu;
                    let mut kept = if still || faded.is_some() { self.text_layer_lookup(&single, width, height) } else { None };
                    // Fading in with no resting layer kept (played into, not
                    // parked on): the resting layer drawn once and kept, for
                    // the fade's frames after.
                    if kept.is_none() && faded.is_some() {
                        self.text_layers_rendered += 1;
                        let resting = self.render_text_layer(&single, width, height, gpu.take(), None);
                        self.text_layer_store(&single, width, height, &resting);
                        kept = self.text_layer_lookup(&single, width, height);
                    }
                    // Kept, and going in as it is (the first layer, whole,
                    // upright, with no fade of its own): its box straight
                    // into the stack, the rest of the frame untouched. A
                    // whole frame converted for a dot was most of the
                    // 35 ms a frame such a title still cost.
                    let fades = !self.defer_layer_fades && (self.loop_focus.contains_key(&obj.id.0) || ScreenFade::new(obj.fx().edge_fade, None).is_some());
                    // Mixed layers must reach the mask/blend step below, even when cached.
                    let as_is = !mixed && first_layer && (opacity - 1.0).abs() < 0.001 && effective_transform.rotation.abs() <= 0.001 && !fades;
                    if let Some(kept) = kept.as_ref().filter(|_| as_is) {
                        self.bound_path = None;
                        self.loop_wave = None;
                        buf = vec![0.0f32; pixel_count * 4];
                        kept.write_into_stack(&mut buf, width, faded.unwrap_or(1.0));
                        continue;
                    }
                    // A fade below the first layer: the kept layer, its fade
                    // on its opacity.
                    let opacity = opacity * faded.filter(|_| kept.is_some()).unwrap_or(1.0);
                    let mut layer = match kept {
                        Some(kept) => kept.layer(width, height),
                        None => {
                            self.text_layers_rendered += 1;
                            let layer = self.render_text_layer(&single, width, height, gpu, time_secs);
                            if still {
                                self.text_layer_store(&single, width, height, &layer);
                            }
                            layer
                        }
                    };
                    // Text layers use the same object transform as their
                    // editor.  Layout still happens in the authored box,
                    // then the rendered glyph layer is rotated around the
                    // authored anchor so the canvas and Inspector stay
                    // WYSIWYG.
                    self.bound_path = None;
                    if effective_transform.rotation.abs() > 0.001 && !on_layer {
                        rotate_rgba_layer(
                            &mut layer,
                            width as usize,
                            height as usize,
                            effective_transform.center.x * width as f32
                                + (effective_transform.anchor_norm.x - 0.5)
                                    * effective_transform.size.x
                                    * width as f32,
                            effective_transform.center.y * height as f32
                                + (effective_transform.anchor_norm.y - 0.5)
                                    * effective_transform.size.y
                                    * height as f32,
                            effective_transform.rotation.to_radians(),
                        );
                    }
                    self.loop_wave = None;
                    self.fade_layer(obj, &mut layer, width, height);
                    if first_layer && (opacity - 1.0).abs() < 0.001 {
                        buf = layer.iter().map(|&v| v as f32).collect();
                    } else if (opacity - 1.0).abs() < 0.001 {
                        frame(&mut buf, pixel_count);
                        over_blend(&mut buf, &layer);
                    } else {
                        // Per-object opacity: scale the layer's alpha
                        // channel before composition. Branching on the
                        // ~1.0 case keeps the common path identical to
                        // the pre-opacity rendering hot loop.
                        let mut faded = layer;
                        scale_layer_alpha(&mut faded, opacity);
                        frame(&mut buf, pixel_count);
                        over_blend(&mut buf, &faded);
                    }
                }
                TitleObjectContent::Shape(shape) => {
                    let Some(transform) = self.shape_transform(title, obj, shape, time_secs, width, height) else {
                        // Nothing to draw: what was below stays.
                        if let Some(below) = below {
                            buf = below;
                        }
                        continue;
                    };
                    effective_transform = transform;
                    let fx = obj.fx();
                    if !fx.is_default() {
                        // The layer, then its stack and its pose on it.
                        let mut layer = vec![0u8; (width as usize) * (height as usize) * 4];
                        paint_shape_layer(&mut layer, shape, &effective_transform, width, height, time_secs.unwrap_or(0.0), None, mixed);
                        let mut layer = self.dress_layer(layer, fx, width, height, time_secs, window, &mut None);
                        self.fade_layer(obj, &mut layer, width, height);
                        if (opacity - 1.0).abs() >= 0.001 {
                            scale_layer_alpha(&mut layer, opacity);
                        }
                        frame(&mut buf, pixel_count);
                        over_blend(&mut buf, &layer);
                    } else if (opacity - 1.0).abs() < 0.001 {
                        frame(&mut buf, pixel_count);
                        paint_shape_layer(&mut buf, shape, &effective_transform, width, height, time_secs.unwrap_or(0.0), gpu_for_first_text.as_mut().map(|g| &mut **g as &mut dyn AlphaMaskProcessor), mixed);
                    } else {
                        // Render the shape into a fresh transparent
                        // layer, scale its alpha, then over-blend —
                        // matches the per-object opacity behaviour
                        // for TextBox layers.
                        let mut layer = vec![0u8; (width as usize) * (height as usize) * 4];
                        paint_shape_layer(&mut layer, shape, &effective_transform, width, height, time_secs.unwrap_or(0.0), gpu_for_first_text.as_mut().map(|g| &mut **g as &mut dyn AlphaMaskProcessor), mixed);
                        scale_layer_alpha(&mut layer, opacity);
                        frame(&mut buf, pixel_count);
                        over_blend(&mut buf, &layer);
                    }
                }
                TitleObjectContent::Image(img) => {
                    if let Some(decoded) = self.decoded_image(img) {
                        let mut layer = vec![0u8; (width as usize) * (height as usize) * 4];
                        paint_image_layer(&mut layer, &decoded, img.fit, &effective_transform, width, height);
                        if !obj.fx().is_default() {
                            layer = self.dress_layer(layer, obj.fx(), width, height, time_secs, window, &mut None);
                            self.fade_layer(obj, &mut layer, width, height);
                        }
                        if (opacity - 1.0).abs() >= 0.001 {
                            scale_layer_alpha(&mut layer, opacity);
                        }
                        frame(&mut buf, pixel_count);
                        over_blend(&mut buf, &layer);
                    }
                }
            }
            if let Some(mut below) = below {
                let mut layer = std::mem::take(&mut buf);
                // Nothing drawn: what was below stays.
                if layer.is_empty() {
                    buf = below;
                    continue;
                }
                if let Some((src, kind)) = title.mask_of(obj) {
                    let matte = self.matte_layer(title, src.id, width, height, time_secs);
                    apply_layer_mask(&mut layer, &matte, kind);
                }
                blend_layer_mode(&mut below, &layer, obj.blend);
                buf = below;
            }
        }
        buf
    }

    /// Where a shape layer is drawn at `time_secs`: its transform as
    /// keyed then, or the box it fits round its target text. One place,
    /// so a text on the layer's path follows exactly what is drawn
    /// (#1508). `None` when it fits round nothing.
    fn shape_transform(&mut self, title: &TitleData, obj: &TitleObject, shape: &klipp_timeline::Shape, time_secs: Option<f32>, width: u32, height: u32) -> Option<ObjectTransform> {
        match shape.fit {
            Some(fit) => self.resolved_shape_fit(title, fit, width, height, time_secs),
            None => Some(time_secs.map_or_else(|| obj.transform.clone(), |t| obj.transform.evaluated_at(t as f64))),
        }
    }

    /// The drawn path `text`'s primary text box follows (#1508): its
    /// layer in `title` as that layer is drawn at `time_secs`, morph
    /// keys and transform and all, its contour flattened in frame
    /// pixels. `None` when the box is not on a drawn path, or the layer
    /// is missing or not a path.
    fn bound_path_line(&mut self, title: &TitleData, text: &TitleData, time_secs: Option<f32>, width: u32, height: u32) -> Option<text_on_path::PathLine> {
        let Some(TextPath::Layer { layer, contour, .. }) = text.text_path() else { return None };
        let obj = title.object(layer)?;
        let TitleObjectContent::Shape(shape) = &obj.content else { return None };
        let ShapeKind::Path(path) = &shape.kind else { return None };
        let transform = self.shape_transform(title, obj, shape, time_secs, width, height)?;
        let pose = path.at(time_secs.unwrap_or(0.0) as f64);
        let (closed, points) = vector_path::contours(&pose, &transform, width, height).into_iter().nth(contour)?;
        text_on_path::PathLine::new(points, closed)
    }

    /// The layer `id` of `title` drawn alone, at `time_secs`, as the
    /// mask of another layer (#1513): shown or hidden, its own blend and
    /// mask left out.
    fn matte_layer(&mut self, title: &TitleData, id: TitleObjectId, width: u32, height: u32, time_secs: Option<f32>) -> Vec<f32> {
        let units = self.last_units.take();
        let outer = self.matte_only.replace(id);
        let mut matte = self.render_stack(title, width, height, None, time_secs);
        frame(&mut matte, (width as usize) * (height as usize));
        self.matte_only = outer;
        self.last_units = units;
        matte
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
        let tb = time_secs.map_or_else(|| tb.clone(), |t| tb.at(t as f64));
        let mut single = TitleData::single_text_fx(tb, target.fx().clone());
        single.objects[0].transform = target.transform.clone();
        if let Some(t) = time_secs {
            single.objects[0].transform.center = target.transform.evaluated_center(t as f64);
        }
        let previous_bounds_only = std::mem::replace(&mut self.layout_bounds_only, true);
        let layout = self.render_layout_at_time(&single, width, height, time_secs);
        self.layout_bounds_only = previous_bounds_only;
        if layout.glyph_count == 0 { return None; }
        let px = fit.padding.x.max(0.0) * crate::frame_scale(width, height);
        let py = fit.padding.y.max(0.0) * crate::frame_scale(width, height);
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
    /// `render_stack` always has exactly one `TextBox` object, so
    /// the existing single-text helpers stay unchanged.
    fn render_text_layer(
        &mut self,
        title: &TitleData,
        width: u32,
        height: u32,
        gpu: Option<&mut dyn AlphaMaskProcessor>,
        time_secs: Option<f32>,
    ) -> Vec<u8> {
        // Real 3D text (#1512): the glyphs as solids, drawn in place of
        // the flat face (and of any older extrusion).
        // The GPU goes to the 3D path when it draws, as it does to the
        // flat face's stack.
        let mut gpu = gpu;
        if let Some(buf) = self.render_text3d(title, width, height, time_secs, &mut gpu) {
            self.last_units = None;
            return buf;
        }
        let layout = self.render_layout_at_time(title, width, height, time_secs);
        let mut buf = Self::compose(&layout, title, width, height, gpu, time_secs);
        self.apply_texture_fill(&mut buf, &layout, title, width, height);

        post_effects(&mut buf, title, width, height, time_secs);

        self.last_units = (!layout.glyph_index.is_empty()).then(|| std::sync::Arc::new(UnitMap::from_layout(&layout, &buf, width, height)));
        buf
    }

    /// `title`'s text in 3D at `width`×`height`, when it has 3D on
    /// (#1512): laid out as the flat text is, each glyph's outline
    /// extruded and bevelled, the whole turned about the text's centre
    /// and lit. `None` for a text without 3D.
    /// It keeps the rest of the text's look (#1735): the face's fill and
    /// its runs' colours on the front faces, underline and strikethrough
    /// as thin solids, a write-on revealing the solids, each glyph's own
    /// motion posing its solid, a path standing each solid on it, and
    /// the post effects on the drawn layer.
    fn render_text3d(&mut self, title: &TitleData, width: u32, height: u32, time_secs: Option<f32>, gpu: &mut Option<&mut dyn AlphaMaskProcessor>) -> Option<Vec<u8>> {
        let (solid, drawn) = self.render_text3d_solid(title, width, height, time_secs)?;
        let mut buf = Self::dress_3d(solid, drawn, title, width, height, time_secs, gpu);
        post_effects(&mut buf, title, width, height, time_secs);
        Some(buf)
    }

    fn render_text3d_solid(&mut self, title: &TitleData, width: u32, height: u32, time_secs: Option<f32>) -> Option<(Vec<u8>, Option<[usize; 4]>)> {
        let tb = title.primary_text()?;
        let settings = tb.three_d.as_ref().filter(|t| t.enabled)?.clone();
        let layout = self.render_layout_at_time(title, width, height, time_secs);
        let placed = std::mem::take(&mut *self.last_glyphs3d.lock().unwrap_or_else(|e| e.into_inner()));
        // Depths in ems of the text's size, as its size is drawn.
        let scale = crate::frame_scale(width, height);
        let em = tb.font_size * scale;
        let bevel = |b: &Option<klipp_timeline::Bevel>| b.map(|b| text3d_mesh::BevelPx { size: b.size * em, segments: b.segments, round: b.profile == klipp_timeline::BevelProfile::Round });
        let (front, back) = (bevel(&settings.front_bevel), bevel(&settings.back_bevel));
        let depth = settings.depth * em;
        // An outline as the solid takes it: stood on its path, and cut
        // where a write-on has reached.
        let shaped = |raw: Vec<Vec<[f32; 2]>>, turn: Option<text3d_render::Turn>| -> Vec<text3d_mesh::Contour> {
            let raw = raw.into_iter().map(|c| match turn {
                Some(t) => c.into_iter().map(|p| t.apply(p)).collect(),
                None => c,
            });
            match placed.reveal {
                Some(x) => text3d_mesh::contours_of(raw.map(|c| text3d_mesh::clip_left_of(&c, x)).collect()),
                None => text3d_mesh::contours_of(raw.collect()),
            }
        };
        // A colour glyph is a flat sprite on the front plane.
        let sprites: Vec<text3d_render::Sprite> = placed.glyphs.iter().filter_map(|g| g.sprite.clone()).filter(|sp| placed.reveal.is_none_or(|x| sp.x < x)).collect();
        // Each solid and the glyph whose motion it follows.
        let mut solids: Vec<(text3d_mesh::GlyphMesh, Option<usize>)> = placed
            .glyphs
            .iter()
            .filter(|g| g.sprite.is_none())
            .filter_map(|g| {
                let font = self.font_cache.get(&g.font).unwrap_or(&self.fallback_font);
                let contours = font.outline(g.glyph, g.px, g.origin, 0.2)?;
                let contours = shaped(contours.into_iter().map(|c| c.points).collect(), g.turn);
                (!contours.is_empty()).then(|| (text3d_mesh::extrude(&contours, depth, front, back, 25.0), Some(g.unit)))
            })
            .collect();
        // The stripes, each cut into the stretches under its glyphs so
        // each stretch moves with its glyph, as the flat stripe's pixels
        // go with the glyph nearest them.
        let boxes = &layout.glyph_boxes;
        let inked: Vec<usize> = placed.glyphs.iter().map(|g| g.unit).filter(|&u| boxes.get(u).is_some_and(|b| b[2] > b[0])).collect();
        for st in &placed.stripes {
            let [x0, y0, x1, y1] = *st;
            let mut under: Vec<(f32, usize)> = inked
                .iter()
                .filter(|&&u| {
                    let b = boxes[u];
                    (b[0] as f32) < x1 && (b[2] as f32) > x0 && ((b[1] + b[3]) as f32 * 0.5 - (y0 + y1) * 0.5).abs() < em * 1.5
                })
                .map(|&u| ((boxes[u][0] + boxes[u][2]) as f32 * 0.5, u))
                .collect();
            under.sort_by(|a, b| a.0.total_cmp(&b.0));
            let pieces: Vec<(f32, f32, Option<usize>)> = if under.is_empty() {
                vec![(x0, x1, None)]
            } else {
                (0..under.len())
                    .map(|k| {
                        let a = if k == 0 { x0 } else { (under[k - 1].0 + under[k].0) * 0.5 };
                        let b = if k + 1 == under.len() { x1 } else { (under[k].0 + under[k + 1].0) * 0.5 };
                        (a.max(x0), b.min(x1), Some(under[k].1))
                    })
                    .collect()
            };
            for (a, b, unit) in pieces {
                let contours = shaped(vec![vec![[a, y0], [b, y0], [b, y1], [a, y1]]], None);
                if !contours.is_empty() {
                    solids.push((text3d_mesh::extrude(&contours, depth, front, back, 25.0), unit));
                }
            }
        }
        let meshes: Vec<text3d_mesh::GlyphMesh> = solids.iter().map(|s| s.0.clone()).collect();
        // Each glyph's own motion at this time, as the flat glyphs are
        // posed: a staggered entrance or exit and the animators, each
        // solid about its glyph's centre, half way into its depth.
        let fx = title.primary_fx();
        let poses: Vec<text3d_render::Pose> = match time_secs {
            Some(t) if fx.motion.is_active() || fx.animators.iter().any(|a| a.enabled) => {
                let units = UnitMap { x0: 0, y0: 0, w: 0, h: 0, index: Vec::new(), boxes: layout.glyph_boxes.clone(), parents: layout.glyph_parents.clone(), pad: 0, path: None, tracking_origins: Vec::new() };
                let glyph = glyph_poses(&units, &fx.motion, &fx.animators, t, self.motion_window, em, (0.0, 0.0, 1.0), false);
                solids
                    .iter()
                    .map(|(_, unit)| match unit.and_then(|u| glyph.get(u)) {
                        Some(p) => text3d_render::Pose { anchor: [p.anchor.0, p.anchor.1, depth * 0.5], scale: p.scale, rotation: p.rotation, offset: [p.dx, p.dy], alpha: p.alpha.clamp(0.0, 1.0) },
                        None => text3d_render::Pose::default(),
                    })
                    .collect()
            }
            _ => Vec::new(),
        };
        let pivot = [(layout.text_left + layout.text_right) * 0.5, (layout.text_top + layout.text_bottom) * 0.5, depth * 0.5];
        let face = settings.face_color.unwrap_or(tb.color);
        // A face that is not one colour: the flat face drawn alone, its
        // colours laid on the front faces.
        let texture = self.face_texture(title, face, width, height, time_secs);
        let scene = text3d_render::Scene {
            meshes: &meshes,
            sprites: &sprites,
            settings: &settings,
            pivot,
            face,
            side: settings.side_color,
            bevel: settings.bevel_color.unwrap_or(face),
            poses: &poses,
            texture: texture.as_ref().map(|rgba| text3d_render::FaceTexture { rgba, width: width as usize, height: height as usize }),
        };
        Some(text3d_render::render_boxed(&scene, width, height, 2))
    }

    /// The face of a 3D text whose face is not one colour, as its flat
    /// self draws it (#1735): its gradient, pattern or texture and its
    /// runs' colours, without what goes round or behind it, unrevealed
    /// and at rest. `None` for a face of one colour.
    fn face_texture(&mut self, title: &TitleData, face: [u8; 4], width: u32, height: u32, time_secs: Option<f32>) -> Option<Vec<u8>> {
        let tb = title.primary_text()?;
        let filled = tb.gradient.as_ref().is_some_and(|g| g.enabled)
            || (tb.pattern_fill_enabled && tb.pattern_fill.is_some())
            || tb.texture_fill.as_ref().is_some_and(|t| t.enabled)
            || tb.style_runs.iter().any(|r| r.color.is_some());
        if !filled {
            return None;
        }
        let mut flat = title.clone();
        flat.background = TitleBackground::None;
        for o in &mut flat.objects {
            if let TitleObjectContent::TextBox(t) = &mut o.content {
                o.fx = None;
                t.three_d = None;
                t.extrusion = None;
                t.write_on = 1.0;
                t.color = face;
            }
        }
        let layout = self.render_layout_at_time(&flat, width, height, time_secs);
        let mut buf = Self::compose(&layout, &flat, width, height, None, time_secs);
        self.apply_texture_fill(&mut buf, &layout, &flat, width, height);
        Some(buf)
    }

    /// A 3D text's solid dressed as a flat text's face is: its backing
    /// or full-frame card behind, its layer's glows, drop shadows and
    /// outlines round it and its inner shadows inside, all from the
    /// solid's own coverage, as they come from the glyphs' for a flat
    /// text (#1512).
    /// `drawn` is the box the solid is drawn in; only round it, out as
    /// far as the stack reaches, is the work done.
    fn dress_3d(solid: Vec<u8>, drawn: Option<[usize; 4]>, title: &TitleData, width: u32, height: u32, time_secs: Option<f32>, gpu: &mut Option<&mut dyn AlphaMaskProcessor>) -> Vec<u8> {
        let fx = title.primary_fx();
        let stacked = fx.glows.iter().any(|g| g.enabled) || fx.shadows.iter().any(|s| s.enabled) || fx.outlines.iter().any(|o| o.enabled) || fx.inner_shadows.iter().any(|s| s.enabled);
        // Nothing round it or behind it: the solid is the layer.
        if !stacked && matches!(title.background, TitleBackground::None) {
            return solid;
        }
        let (w, h) = (width as usize, height as usize);
        let scale = crate::frame_scale(width, height);
        let full_frame = matches!(title.background, TitleBackground::FullFrame { .. });
        let Some([x0, y0, x1, y1]) = drawn else {
            if !full_frame {
                return solid;
            }
            let mut buf = vec![0f32; w * h * 4];
            fill_full_frame(&mut buf, title);
            return to_bytes(&buf);
        };
        // The solid's coverage, with room round it for what the stack
        // and a backing panel draw there.
        let backing_pad = match title.background {
            TitleBackground::Color { padding, .. } => (padding * scale).ceil().max(0.0) as usize,
            _ => 0,
        };
        let pad = (effect_padding(title, scale).max(0) as usize).max(backing_pad) + 2;
        let (mx0, my0, mx1, my1) = (x0.saturating_sub(pad), y0.saturating_sub(pad), (x1 + pad).min(w), (y1 + pad).min(h));
        // The region worked on: the whole frame under a full-frame card,
        // else just round the solid. Its own buffer, its own coordinates.
        let (rx0, ry0, rx1, ry1) = if full_frame { (0, 0, w, h) } else { (mx0, my0, mx1, my1) };
        let (rw, rh) = (rx1 - rx0, ry1 - ry0);
        let mut buf = vec![0f32; rw * rh * 4];
        fill_full_frame(&mut buf, title);
        let (mw, mh) = (mx1 - mx0, my1 - my0);
        let mut mask = vec![0u8; mw * mh];
        let (mut bx0, mut by0, mut bx1, mut by1) = (usize::MAX, usize::MAX, 0usize, 0usize);
        for y in y0..y1 {
            for x in x0..x1 {
                let a = solid[(y * w + x) * 4 + 3];
                if a > 0 {
                    mask[(y - my0) * mw + (x - mx0)] = a;
                    (bx0, by0, bx1, by1) = (bx0.min(x), by0.min(y), bx1.max(x + 1), by1.max(y + 1));
                }
            }
        }
        if bx1 > bx0 {
            let local = |v: usize, o: usize| (v - o) as f32;
            paint_backing(&mut buf, title, [local(bx0, rx0), local(by0, ry0), local(bx1, rx0), local(by1, ry0)], rw as u32, rh as u32, scale);
        }
        let t = time_secs.map(|t| t as f64).unwrap_or(0.0);
        // The GPU for a mask large enough to be worth it, as `compose`.
        let use_gpu = gpu.is_some() && mw * mh > 250_000;
        stack_behind(&mut buf, &mask, mw, mh, mx0 - rx0, my0 - ry0, rw, rh, fx, scale, t, i32::MAX, use_gpu, gpu);
        for y in y0..y1 {
            for x in x0..x1 {
                let p = &solid[(y * w + x) * 4..][..4];
                if p[3] > 0 {
                    blend_pixel(&mut buf, ((y - ry0) * rw + (x - rx0)) * 4, &[p[0], p[1], p[2], p[3]]);
                }
            }
        }
        stack_inside(&mut buf, &mask, mw, mh, mx0 - rx0, my0 - ry0, rw, rh, fx, scale, t, use_gpu, gpu);
        // The region back into the frame.
        let mut out = solid;
        let bytes = to_bytes(&buf);
        for y in 0..rh {
            out[((y + ry0) * w + rx0) * 4..((y + ry0) * w + rx1) * 4].copy_from_slice(&bytes[y * rw * 4..(y + 1) * rw * 4]);
        }
        out
    }

    /// Look up a cached rasterised buffer for the given title +
    /// dimensions. On hit, moves the entry to the front of the LRU
    /// (most-recently-used end) and returns a memcpy of the buffer.
    /// The resting render of `title` (as `resting_title` names it) at
    /// `width`×`height`, made here on the CPU: for a warm-up on another
    /// thread, handed to the renderer that will place it by `adopt`.
    pub fn render_resting(&mut self, title: &TitleData, width: u32, height: u32) -> RestingRender {
        let pixels = self.raster_at(&resting_title(title), width, height, None, None);
        RestingRender { pixels, units: self.last_units.take() }
    }

    /// Keep a resting render of `title` made by `render_resting`
    /// elsewhere, so the first frame of the title costs this thread
    /// nothing. Ignored when one is cached already or the bytes are not
    /// a frame.
    pub fn adopt(&mut self, title: &TitleData, width: u32, height: u32, render: RestingRender) {
        let key = resting_title(title);
        let RestingRender { pixels, units } = render;
        if pixels.frame != (width, height) || self.cache_lookup(&key, width, height).is_some() {
            return;
        }
        self.last_units = units;
        self.cache_store(key.into_owned(), width, height, pixels);
    }

    /// The focus blur the last placed frame asked for: (radius in frame
    /// pixels, axis, band). A caller working from the bytes applies it as
    /// klipp-effects' `FocusBlur`, as the GPU stack does to the sprite.
    pub fn last_focus_blur(&self) -> Option<(f32, RampAxis, f32)> {
        self.last_focus_blur
    }

    /// The blur the last placed frame asked for (sigma in pixels): a
    /// caller working from the bytes applies it as a Gaussian blur.
    pub fn last_blur(&self) -> f32 {
        self.last_blur
    }

    /// Fade a rastered layer by where it is on the frame: its edge fade,
    /// and its roll's focus band on the frame being rastered (#592).
    fn fade_layer(&self, obj: &TitleObject, layer: &mut [u8], width: u32, height: u32) {
        if self.defer_layer_fades {
            return;
        }
        let focus = self.loop_focus.get(&obj.id.0).copied();
        if let Some(fade) = ScreenFade::new(obj.fx().edge_fade, focus) {
            fade.apply_rgba8(layer, width, height);
        }
        // A focus band gone soft: the layer mixed with itself blurred,
        // by how far each pixel is from the band, as the placed path's
        // `FocusBlur` does.
        if let Some((axis, f)) = focus.filter(|(_, f)| f.blur > 0.0) {
            let radius = f.blur * crate::frame_scale(width, height);
            if radius > 0.5 {
                let mut soft = layer.to_vec();
                blur_rgba(&mut soft, width, height, radius);
                let (w, h) = (width as usize, height as usize);
                for (i, (px, bl)) in layer.chunks_exact_mut(4).zip(soft.chunks_exact(4)).enumerate() {
                    let u = match axis {
                        RampAxis::Vertical => ((i / w) as f32 + 0.5) / h as f32,
                        RampAxis::Horizontal => ((i % w) as f32 + 0.5) / w as f32,
                    };
                    let m = f.blur_mix(u);
                    if m > 0.0 {
                        // Mixed premultiplied, as `blur_rgba` blurs, then
                        // back to straight.
                        let (pa, ba) = (px[3] as f32 / 255.0, bl[3] as f32 / 255.0);
                        let a = pa + (ba - pa) * m;
                        for c in 0..3 {
                            let v = px[c] as f32 * pa + (bl[c] as f32 * ba - px[c] as f32 * pa) * m;
                            px[c] = if a > 1e-6 { (v / a).round().clamp(0.0, 255.0) as u8 } else { 0 };
                        }
                        px[3] = (a * 255.0).round().clamp(0.0, 255.0) as u8;
                    }
                }
            }
        }
    }

    /// The resting render of `title`: from the cache, or made and kept.
    fn resting_sprite(&mut self, title: &TitleData, width: u32, height: u32, gpu: Option<&mut dyn AlphaMaskProcessor>) -> RestingSprite {
        match self.cache_lookup(title, width, height) {
            Some(hit) => hit,
            None => {
                self.resting_renders += 1;
                self.defer_layer_fades = title.objects.iter().filter(|o| o.visible).count() == 1 && !title.mixes_layers();
                let raster = self.raster_at(title, width, height, gpu, None);
                self.defer_layer_fades = false;
                self.cache_store(title.clone(), width, height, raster)
            }
        }
    }

    /// How many text layers of several-layer titles this renderer drew
    /// rather than took from its kept ones (#2095). For tests.
    #[doc(hidden)]
    pub fn text_layers_rendered(&self) -> u64 {
        self.text_layers_rendered
    }

    /// How many resting renders this renderer made rather than took from
    /// its cache (#2069). The compositor reads it to report keyed path
    /// geometry rasterised again (#2201).
    pub fn resting_renders(&self) -> u64 {
        self.resting_renders
    }

    /// The text layer `single` renders to at rest, when kept
    /// ([`CachedTextLayer`]): its box, and its glyph map as the render
    /// leaves it.
    fn text_layer_lookup(&mut self, single: &TitleData, width: u32, height: u32) -> Option<std::sync::Arc<KeptLayer>> {
        let pos = self.text_layer_cache.iter().position(|c| c.width == width && c.height == height && &c.title == single)?;
        let entry = self.text_layer_cache.remove(pos);
        let kept = std::sync::Arc::clone(&entry.kept);
        self.last_units = entry.units.clone();
        self.text_layer_cache.insert(0, entry);
        Some(kept)
    }

    /// Keep `layer`, the text layer `single` rendered to at rest, as the
    /// box it covers.
    fn text_layer_store(&mut self, single: &TitleData, width: u32, height: u32, layer: &[u8]) {
        let (w, h) = (width as usize, height as usize);
        let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
        for y in 0..h {
            for x in 0..w {
                if layer[(y * w + x) * 4 + 3] != 0 {
                    x0 = x0.min(x);
                    x1 = x1.max(x + 1);
                    y0 = y0.min(y);
                    y1 = y1.max(y + 1);
                }
            }
        }
        let (bw, bh) = if x1 > x0 { (x1 - x0, y1 - y0) } else { (0, 0) };
        let mut crop = Vec::with_capacity(bw * bh * 4);
        for y in y0..y0 + bh {
            crop.extend_from_slice(&layer[(y * w + x0) * 4..(y * w + x0 + bw) * 4]);
        }
        if self.text_layer_cache.len() >= TEXT_LAYER_CACHE_CAP {
            self.text_layer_cache.pop();
        }
        let kept = std::sync::Arc::new(KeptLayer { x0, y0, w: bw, h: bh, crop });
        self.text_layer_cache.insert(0, CachedTextLayer { title: single.clone(), width, height, kept, units: self.last_units.clone() });
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
    fn cache_store(&mut self, title: TitleData, width: u32, height: u32, raster: BoxRaster) -> RestingSprite {
        // A guard on memory as well as the count: the box a title covers
        // is usually far smaller than its frame, but a full-frame card is
        // not (#190).
        let size = raster.byte_size();
        while !self.render_cache.is_empty() && (self.render_cache.len() >= RENDER_CACHE_CAP || self.render_cache.iter().map(|c| c.buffer.byte_size()).sum::<usize>() + size > RENDER_CACHE_BYTES) {
            self.render_cache.pop();
        }
        let id = self.next_cache_id;
        self.next_cache_id += 1;
        let rows = raster_rows(&raster);
        let cols = if raster.is_empty() { (0, 0) } else { (raster.x0, raster.x0 + raster.w) };
        let units = self.last_units.take();
        let pixels = std::sync::Arc::new(raster);
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
        self.geometry3d_cache.clear();
        self.render_cache.clear();
        self.text_layer_cache.clear();
        self.style_source_cache.clear();
        self.gpu_glyph_cache.borrow_mut().clear();
        self.gpu_textures.clear();
        self.texture_fill_cache=None;
    }

    /// Compute character positions for the given title text, using the same layout
    /// algorithm as `render_layout`. Returns positions in frame pixel coordinates.
    /// Used by the viewer for cursor placement and click hit-testing.
    pub fn compute_char_positions(&mut self, title: &TitleData, width: u32, height: u32) -> Vec<CharPosition> {
        let tb = self.with_scripts(title.primary_text_or_default());
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

        // Same fonts + layout as render_layout, falling back as it
        // does (#1503).
        let mut faces = self.run_faces(&tb);
        self.fall_back(&mut faces, &tb.style_runs, &resolved_text);
        let fonts = self.fonts_of(&faces.keys);

        let scale = crate::frame_scale(width, height);
        let case_scale = match tb.case_transform {
            klipp_timeline::TextCase::SmallCaps => 0.80,
            _ => 1.0,
        };
        let font_px = tb.font_size * scale * case_scale;
        let (box_w, box_h) = text_box_extent(title, width, height);
        let face_at = |i: usize| -> &Font { &fonts[faces.drawn(&tb.style_runs, &resolved_text, i)] };
        let shaped_widest = |ratio: f32| {
            let run_px = |run: &StyleRun| run.font_size.unwrap_or(tb.font_size) * scale * case_scale * ratio;
            let pieces = faces.split(&resolved_text, set_pieces(&tb.style_runs, &faces.of_run, 0, resolved_text.len(), font_px * ratio, run_px));
            shaping::widest_typed_line(&fonts, &resolved_text, &pieces)
        };
        let (fit_w, fit_h) = paragraph_room(&tb, &resolved_text, scale, box_w, box_h);
        let fit = box_fit_ratio(&tb, face_at, &resolved_text, |i| run_px_at(&tb.style_runs, i, font_px, scale * case_scale), shaped_widest, fit_w, fit_h);
        let font_px = font_px * fit;
        let run_px = |run: &StyleRun| run.font_size.unwrap_or(tb.font_size) * scale * case_scale * fit;

        // Vertical text (#1704): each character in its cell of the
        // columns the renderer sets.
        if tb.vertical_layout {
            return self.vertical_char_positions(title, &tb, &fonts, &faces, &resolved_text, font_px, scale, run_px, width, height);
        }

        let h_align = h_align_of(tb.alignment);

        let mut layout = ShapedLayout::new();
        layout.set_rtl(tb.text_direction == TextDirection::Rtl);
        layout.set_paragraphs(paragraphs_px(&tb, scale));
        layout.reset(&LayoutSettings {
            x: 0.0,
            y: 0.0,
            // Text on a path is one line, as long as the text (#1508).
            max_width: tb.text_path.is_none().then(|| text_box_extent(title, width, height).0),
            max_height: Some(text_box_extent(title, width, height).1),
            horizontal_align: h_align,
            vertical_align: VerticalAlign::Top,
            wrap_style: WrapStyle::Word,
            // The layout spaces its own lines, by each line's tallest
            // type: the box's spacing is a multiple of that (#1676).
            line_height: tb.line_spacing,
            ..LayoutSettings::default()
        });

        // Each glyph's byte offset indexes `resolved_text`.
        for piece in faces.split(&resolved_text, set_pieces(&tb.style_runs, &faces.of_run, 0, resolved_text.len(), font_px, run_px)) {
            layout.append(&fonts, &resolved_text, piece);
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
                rtl: false,
                vertical: false,
            }];
        }

        let mut glyphs_owned: Vec<fontdue::layout::GlyphPosition> = glyphs.to_vec();

        // Apply tracking (same as render_layout)
        if tb.tracking != 0.0 {
            let tracking_px = tb.tracking * scale;
            for (i, g) in glyphs_owned.iter_mut().enumerate() {
                g.x += tracking_px * i as f32;
            }
        }
        // A style run's own tracking and baseline offset (same as
        // render_layout, #1505)
        let line_starts: Vec<usize> = layout.lines().map_or(Vec::new(), |lines| lines.iter().map(|l| l.glyph_start).collect());
        apply_run_spacing(&mut glyphs_owned, &tb.style_runs, &line_starts, tb.tracking, scale);

        // Justify (same as render_layout, #1506)
        if tb.alignment == TextAlignment::Justify {
            if let Some(lines) = layout.lines() {
                justify_lines(&mut glyphs_owned, lines, &resolved_text, box_w, tb.last_line);
            }
        }

        // Compute text bounding box and offset (same as render_layout),
        // the runs' glosses in it (#1704).
        let gloss = if tb.text_path.is_none() && tb.style_runs.iter().any(|r| r.ruby.is_some()) {
            ruby::lay(&fonts, &glyphs_owned, &tb.style_runs, run_px, false)
        } else {
            Vec::new()
        };
        // A list's markers (same as render_layout, #1721): not the
        // text's, but where its box sits.
        let markers = match layout.lines().filter(|_| tb.text_path.is_none()) {
            Some(lines) => {
                let piece_at = |b: usize| {
                    let to = b + resolved_text[b..].chars().next().map_or(1, char::len_utf8);
                    faces.split(&resolved_text, set_pieces(&tb.style_runs, &faces.of_run, b, to, font_px, run_px)).first().map(|p| (p.2, p.3))
                };
                list_markers(&tb, &fonts, &resolved_text, lines, &glyphs_owned, piece_at, scale, box_w)
            }
            None => Vec::new(),
        };
        let marker_pad = (!markers.is_empty()).then(|| (tb.paragraph.indent_start.max(0.0) * scale, tb.text_direction == TextDirection::Rtl));

        let mut text_min_x = f32::MAX;
        let mut text_min_y = f32::MAX;
        let mut text_max_x = f32::MIN;
        let mut text_max_y = f32::MIN;
        for g in glyphs_owned.iter().chain(&gloss).chain(&markers) {
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
        let offset_x = text_offset_x(tb.alignment, anchor_x, text_w, text_min_x, box_w) + indent_shift(tb.alignment, layout.lines(), marker_pad);
        // Baseline shift is authored in 1080p reference pixels and
        // scales with the frame so a +20 px offset on a 1080p edit
        // looks the same on a 4K master. Positive shifts text DOWN.
        let baseline_shift_px = tb.baseline_shift * scale;
        let offset_y = text_offset_y(tb.vertical_align, title.position().y * height as f32, box_h, text_h, text_min_y)
            + baseline_shift_px;

        // Build character positions from glyph data.
        // There is one glyph per character (spaces may have width=0),
        // but for a ligature (#1501): its glyph stands for every
        // character from its byte offset to the next glyph's, and they
        // share its box evenly.
        // We need to map byte_offset → char_index for correct cursor positioning.
        let mut positions: Vec<CharPosition> = Vec::with_capacity(glyphs_owned.len() + 1);
        let mut starts: Vec<usize> = glyphs_owned.iter().map(|g| g.byte_offset).collect();
        starts.sort_unstable();
        starts.dedup();

        // Build a byte_offset → char_index lookup
        let byte_to_char: Vec<(usize, usize)> = tb.text
            .char_indices()
            .enumerate()
            .map(|(ci, (bi, _))| (bi, ci))
            .collect();

        let char_count = tb.text.chars().count();

        let glyph_rtl = layout.glyph_rtl().to_vec();
        for (gi, g) in glyphs_owned.iter().enumerate() {
            let rtl = glyph_rtl.get(gi).copied().unwrap_or(false);
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

            let next = starts.iter().find(|&&b| b > g.byte_offset).copied().unwrap_or(resolved_text.len());
            let shared = resolved_text.get(g.byte_offset..next).map_or(1, |s| s.chars().count().max(1));
            let share = advance / shared as f32;
            for k in 0..shared {
                positions.push(CharPosition {
                    char_index: char_index + k,
                    byte_offset: if k == 0 { g.byte_offset } else { tb.text.char_indices().nth(char_index + k).map_or(g.byte_offset, |(bi, _)| bi) },
                    // A cluster's characters share its box in their
                    // reading order: from its right, right to left.
                    x: g.x + offset_x + if rtl { advance - share * (k + 1) as f32 } else { share * k as f32 },
                    y: g.y + offset_y,
                    advance: if shared > 1 { share } else { advance },
                    height: glyph_height,
                    rtl,
                    vertical: false,
                });
            }
        }

        // Sort by char_index for consistent ordering; glyphs of one
        // cluster (a letter and its marks) give its characters once.
        positions.sort_by_key(|p| p.char_index);
        positions.dedup_by_key(|p| p.char_index);

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
                        x: if prev.rtl { prev.x } else { prev.x + prev.advance },
                        y: prev.y,
                        advance: 0.0,
                        height: prev.height,
                        rtl: prev.rtl,
                        vertical: false,
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
                        rtl: next.rtl,
                        vertical: false,
                    });
                }
            }
        }

        // Add end-of-text position for cursor-at-end
        if let Some(last) = filled.last() {
            // After the last character: past its reading end.
            filled.push(CharPosition {
                char_index: char_count,
                byte_offset: tb.text.len(),
                x: if last.rtl { last.x } else { last.x + last.advance },
                y: last.y,
                advance: 0.0,
                height: last.height,
                rtl: last.rtl,
                vertical: false,
            });
        }

        filled
    }

    /// [`Self::compute_char_positions`] for a vertical box (#1704): the
    /// glyphs laid in columns as `render_layout` lays them, glosses and
    /// drop cap included, placed as it places them, each character its
    /// cell of its column.
    #[allow(clippy::too_many_arguments)]
    fn vertical_char_positions(
        &self,
        title: &TitleData,
        tb: &klipp_timeline::TextBox,
        fonts: &[Face],
        faces: &RunFaces,
        resolved_text: &str,
        font_px: f32,
        scale: f32,
        run_px: impl Fn(&StyleRun) -> f32 + Copy,
        width: u32,
        height: u32,
    ) -> Vec<CharPosition> {
        let (box_w, box_h) = text_box_extent(title, width, height);
        let pieces_of = |from: usize, to: usize| faces.split(resolved_text, set_pieces(&tb.style_runs, &faces.of_run, from, to, font_px, run_px));
        let cap = (tb.drop_cap_scale >= 1.0).then(|| (resolved_text.chars().next().map_or(0, char::len_utf8), font_px * tb.drop_cap_scale, faces.drawn(&tb.style_runs, resolved_text, 0)));
        let (glyphs, _, cells) = lay_vertical(fonts, resolved_text, tb, font_px, scale, box_h, pieces_of, cap);
        let gloss = if tb.text_path.is_none() && tb.style_runs.iter().any(|r| r.ruby.is_some()) {
            ruby::lay(fonts, &glyphs, &tb.style_runs, run_px, true)
        } else {
            Vec::new()
        };
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for g in glyphs.iter().chain(&gloss) {
            (x0, y0) = (x0.min(g.x), y0.min(g.y));
            (x1, y1) = (x1.max(g.x + g.width as f32), y1.max(g.y + g.height as f32));
        }
        if glyphs.is_empty() {
            return Vec::new();
        }
        let alignment = if tb.alignment == TextAlignment::Justify { TextAlignment::Center } else { tb.alignment };
        let offset_x = text_offset_x(alignment, title.position().x * width as f32, x1 - x0, x0, box_w);
        let offset_y = text_offset_y(tb.vertical_align, title.position().y * height as f32, box_h, y1 - y0, y0) + tb.baseline_shift * scale;
        let char_of = |byte: usize| tb.text[..byte.min(tb.text.len())].chars().count();
        let mut starts: Vec<usize> = glyphs.iter().map(|g| g.byte_offset).collect();
        starts.sort_unstable();
        starts.dedup();
        let mut positions: Vec<CharPosition> = Vec::new();
        for (g, cell) in glyphs.iter().zip(&cells) {
            // A cluster's characters share its cell down the column.
            let next = starts.iter().find(|&&b| b > g.byte_offset).copied().unwrap_or(resolved_text.len());
            // A line break after it is no part of it: it has no glyph.
            let shared = resolved_text.get(g.byte_offset..next).map_or(1, |s| s.split('\n').next().unwrap_or("").chars().count().max(1));
            let share = cell[2] / shared as f32;
            let first = char_of(g.byte_offset);
            for k in 0..shared {
                positions.push(CharPosition {
                    char_index: first + k,
                    byte_offset: tb.text.char_indices().nth(first + k).map_or(g.byte_offset, |(b, _)| b),
                    x: cell[0] + offset_x,
                    y: cell[1] + offset_y + share * k as f32,
                    advance: share,
                    height: cell[3],
                    rtl: false,
                    vertical: true,
                });
            }
        }
        positions.sort_by_key(|p| p.char_index);
        positions.dedup_by_key(|p| p.char_index);
        // A character with no glyph (a line break) at the foot of the one
        // before; the end of the text after the last.
        let count = tb.text.chars().count();
        let mut filled: Vec<CharPosition> = Vec::with_capacity(count + 1);
        let mut at = positions.into_iter().peekable();
        for ci in 0..=count {
            if let Some(p) = at.next_if(|p| p.char_index == ci) {
                filled.push(p);
                continue;
            }
            let Some(prev) = filled.last() else { continue };
            filled.push(CharPosition {
                char_index: ci,
                byte_offset: tb.text.char_indices().nth(ci).map_or(tb.text.len(), |(b, _)| b),
                y: prev.y + prev.advance,
                advance: 0.0,
                ..prev.clone()
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
/// Post-pass effects: applied to the fully-composed layer buffer so
/// they pick up outline, shadow, glow, gradient, texture-fill, and any
/// other decorative layer the user configured, a 3D text's solid among
/// them (#1735). Cheap; only run when the corresponding field is
/// non-zero.
fn post_effects(buf: &mut [u8], title: &TitleData, width: u32, height: u32, time_secs: Option<f32>) {
    let Some(tb) = title.primary_text() else { return };
    let scale = crate::frame_scale(width, height);
    if tb.chromatic_aberration_px > 0.0 {
        apply_chromatic_aberration(buf, width, height, tb.chromatic_aberration_px * scale);
    }
    if tb.glitch_intensity > 0.0 {
        let seed = time_secs.map_or(0u32, |t| {
            // Quantise to ~12 steps/sec so the glitch pattern doesn't
            // change every single frame (looks like static noise) while
            // still feeling alive. 12 steps/s reads as TV-glitchy.
            (t * 12.0) as u32
        });
        apply_glitch(buf, width, height, tb.glitch_intensity, seed);
    }
}

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

    // A wide blur, a glyph an animator or an entrance blurs by a good
    // part of its size, as three box blurs each way (#1725): as near a
    // Gaussian as the eye tells, at a cost the radius does not grow. A
    // 2 em blur at 4K made the exact kernel nearly a thousand taps and
    // a frame take seconds.
    if sigma > BOX_BLUR_SIGMA {
        let mut a: Vec<f32> = padded.iter().map(|&v| v as f32).collect();
        let mut b = vec![0.0f32; a.len()];
        for r in box_radii(sigma) {
            box_pass(&a, &mut b, new_w, new_h, r, true);
            std::mem::swap(&mut a, &mut b);
        }
        for r in box_radii(sigma) {
            box_pass(&a, &mut b, new_w, new_h, r, false);
            std::mem::swap(&mut a, &mut b);
        }
        return (a.iter().map(|v| v.round().clamp(0.0, 255.0) as u8).collect(), new_w, new_h);
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

/// Above this sigma, in pixels, [`gaussian_blur_expand`] blurs with
/// three boxes each way instead of the exact kernel (#1725).
const BOX_BLUR_SIGMA: f32 = 8.0;

/// The radii of three box blurs that together blur as a Gaussian of
/// `sigma` does (Kovesi's widths for a given sigma).
fn box_radii(sigma: f32) -> [usize; 3] {
    let n = 3.0f32;
    let ideal = (12.0 * sigma * sigma / n + 1.0).sqrt();
    let mut wl = ideal.floor() as i32;
    if wl % 2 == 0 {
        wl -= 1;
    }
    let wl_f = wl as f32;
    let m = ((12.0 * sigma * sigma - n * wl_f * wl_f - 4.0 * n * wl_f - 3.0 * n) / (-4.0 * wl_f - 4.0)).round() as i32;
    let width = |i: i32| if i < m { wl } else { wl + 2 };
    [0, 1, 2].map(|i| (width(i).max(1) as usize - 1) / 2)
}

/// One box blur of radius `r` along rows (`across`) or columns of a
/// `w`×`h` frame, zero beyond it: a running sum, so the radius costs
/// nothing.
fn box_pass(src: &[f32], dst: &mut [f32], w: usize, h: usize, r: usize, across: bool) {
    let (lines, len, step, stride) = if across { (h, w, 1, w) } else { (w, h, w, 1) };
    let norm = 1.0 / (2 * r + 1) as f32;
    for line in 0..lines {
        let base = line * stride;
        let at = |i: usize| src[base + i * step];
        let mut acc: f32 = (0..=r.min(len - 1)).map(at).sum();
        for i in 0..len {
            dst[base + i * step] = acc * norm;
            if i + r + 1 < len {
                acc += at(i + r + 1);
            }
            if i >= r {
                acc -= at(i - r);
            }
        }
    }
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
/// into the full `text` (the cap is `text[..cap_end]`), so downstream
/// hit-testing and rich-text resolution work as if the layout had
/// been a single fontdue pass, and where each of its lines starts.
/// The body is set in `body_from(byte)`'s pieces from that byte on,
/// each at its style run's size and face (#1505); the cap is set at
/// `cap.0` in face `cap.1`. In a right-to-left box (`rtl`, #1693) the
/// cap stands at the box's right and the body, set right to left from
/// the right, wraps on its left.
#[allow(clippy::too_many_arguments)]
fn layout_drop_cap_wrap(
    fonts: &[Face],
    text: &str,
    cap_end: usize,
    cap: (f32, usize),
    body_size: f32,
    body_from: impl Fn(usize) -> Vec<Piece>,
    full_max_w: f32,
    rtl: bool,
) -> (Vec<fontdue::layout::GlyphPosition>, Vec<usize>) {
    let (cap_size, cap_face) = cap;
    // The body's lines start at the box's right in a right-to-left box.
    let body_align = if rtl { HorizontalAlign::Right } else { HorizontalAlign::Left };
    let new_layout = || {
        let mut l = ShapedLayout::new();
        l.set_rtl(rtl);
        l
    };

    // ── Stage 1: drop cap ──
    let mut cap_layout = new_layout();
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
    cap_layout.append(fonts, text, (0, cap_end, cap_size, cap_face));
    let cap_glyphs: Vec<fontdue::layout::GlyphPosition> = cap_layout.glyphs().to_vec();
    if cap_glyphs.is_empty() {
        // Defensive: no glyph for the first character (shouldn't
        // happen for a non-empty string + valid font, but if it
        // does, fall through to plain body layout).
        let mut body_layout = new_layout();
        body_layout.reset(&LayoutSettings {
            max_width: Some(full_max_w),
            horizontal_align: body_align,
            vertical_align: VerticalAlign::Top,
            wrap_style: WrapStyle::Word,
            ..LayoutSettings::default()
        });
        let glyphs = lay_pieces(&mut body_layout, fonts, text, &body_from(cap_end));
        let starts = body_layout.lines().map_or(Vec::new(), |lines| lines.iter().map(|l| l.glyph_start).collect());
        return (glyphs, starts);
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
    let mut narrow_layout = new_layout();
    narrow_layout.reset(&LayoutSettings {
        x: 0.0,
        y: 0.0,
        max_width: Some(narrow_w),
        max_height: None,
        horizontal_align: body_align,
        vertical_align: VerticalAlign::Top,
        wrap_style: WrapStyle::Word,
        ..LayoutSettings::default()
    });
    let narrow_glyphs = lay_pieces(&mut narrow_layout, fonts, text, &body_from(cap_end));

    // Find the first glyph whose top exceeds cap_h — that's the
    // first body character that no longer fits beside the cap.
    let first_below = narrow_glyphs.iter().position(|g| g.y >= cap_h);
    let (in_column, byte_consumed) = match first_below {
        None => (narrow_glyphs.clone(), text.len()),
        Some(0) => (Vec::new(), cap_end),
        Some(idx) => {
            // The glyphs' byte offsets index the full `text`; the body
            // below starts at the first glyph that did not fit, so a
            // ligature kept beside the cap is not set again (#1501).
            (narrow_glyphs[..idx].to_vec(), narrow_glyphs[idx].byte_offset)
        }
    };

    // Each line starts where fontdue started it: the cap's, the
    // narrow lines kept beside it, then the wide lines below.
    let mut starts = vec![0];
    if let Some(lines) = narrow_layout.lines() {
        starts.extend(lines.iter().filter(|l| l.glyph_start < in_column.len()).map(|l| cap_glyphs.len() + l.glyph_start));
    }

    // ── Stage 3: full-width body below the cap ──
    let wide_glyphs: Vec<fontdue::layout::GlyphPosition> = if byte_consumed >= text.len() {
        Vec::new()
    } else {
        let mut wide_layout = new_layout();
        wide_layout.reset(&LayoutSettings {
            x: 0.0,
            y: 0.0,
            max_width: Some(full_max_w),
            max_height: None,
            horizontal_align: body_align,
            vertical_align: VerticalAlign::Top,
            wrap_style: WrapStyle::Word,
            ..LayoutSettings::default()
        });
        let glyphs = lay_pieces(&mut wide_layout, fonts, text, &body_from(byte_consumed));
        if let Some(lines) = wide_layout.lines() {
            starts.extend(lines.iter().map(|l| cap_glyphs.len() + in_column.len() + l.glyph_start));
        }
        glyphs
    };

    // ── Combine ──
    // Cap glyphs keep their byte_offsets (bytes 0..cap_end of the
    // full source); the body's already index it.
    let mut out: Vec<fontdue::layout::GlyphPosition> = cap_glyphs;
    // Right to left, the cap at the box's right and the narrow body,
    // already set from the right of its column, left of it; else the
    // narrow body shifted right of the cap.
    if rtl {
        for g in &mut out {
            g.x += full_max_w - cap_w;
        }
    }
    for mut g in in_column {
        if !rtl {
            g.x += body_x_offset;
        }
        out.push(g);
    }
    // Wide body glyphs: shift y by cap_h.
    for mut g in wide_glyphs {
        g.y += cap_h;
        out.push(g);
    }
    (out, starts)
}

/// A piece of a text box's text (#1505): its byte range in the text,
/// and the size and face (an index into the box's fonts) it is set in.
type Piece = (usize, usize, f32, usize);

/// A vertical box's glyphs, in frame pixels from the block's top left,
/// where each column's glyphs start (#1507), and each glyph's cell as
/// [`vertical::lay_cells`] gives it. A drop cap `cap`
/// (`(end byte, size, face)`) stands at the top of the first column,
/// as many columns wide as it needs, the body's first columns starting
/// below it (#1704).
#[allow(clippy::too_many_arguments)]
fn lay_vertical(
    fonts: &[Face],
    text: &str,
    tb: &klipp_timeline::TextBox,
    font_px: f32,
    scale: f32,
    box_h: f32,
    pieces_of: impl Fn(usize, usize) -> Vec<Piece>,
    cap: Option<(usize, f32, usize)>,
) -> (Vec<fontdue::layout::GlyphPosition>, Vec<usize>, Vec<[f32; 4]>) {
    // A gloss beside a column takes room of its own between columns.
    let glossed = tb.style_runs.iter().any(|r| r.ruby.is_some());
    let pitch = font_px * (1.2 + if glossed { ruby::RUBY_SCALE } else { 0.0 }) * tb.line_spacing;
    let how = vertical::Columns {
        length: box_h,
        pitch,
        tracking: tb.tracking * scale,
        left_to_right: tb.vertical_columns_ltr,
        rtl: tb.text_direction == TextDirection::Rtl,
        indent: (0, 0.0),
    };
    let Some((cap_end, cap_px, cap_face)) = cap else {
        return vertical::lay_cells(fonts, text, &pieces_of(0, text.len()), &how);
    };
    // The cap's columns, and the body below it in those.
    let spans = ((cap_px / pitch).ceil() as usize).max(1);
    let below = cap_px + font_px * 0.1;
    let (mut body, body_starts, mut body_cells) = vertical::lay_cells(fonts, text, &pieces_of(cap_end, text.len()), &vertical::Columns { indent: (spans, below), ..how });
    let (mut capped, _, mut cells) = vertical::lay_cells(fonts, text, &[(0, cap_end, cap_px, cap_face)], &vertical::Columns { length: f32::MAX, pitch: pitch * spans as f32, indent: (0, 0.0), ..how });
    // Columns run from the right: the first ones, the cap's, rightmost.
    let total = body_starts.len().max(spans);
    let (cap_x, body_x) = if how.left_to_right { (0.0, 0.0) } else { ((total - spans) as f32 * pitch, (total - body_starts.len()) as f32 * pitch) };
    capped.iter_mut().for_each(|g| g.x += cap_x);
    cells.iter_mut().for_each(|c| c[0] += cap_x);
    body.iter_mut().for_each(|g| g.x += body_x);
    body_cells.iter_mut().for_each(|c| c[0] += body_x);
    let n = capped.len();
    let mut starts: Vec<usize> = body_starts.iter().map(|s| s + n).collect();
    if let Some(first) = starts.first_mut() {
        *first = 0;
    }
    capped.extend(body);
    cells.extend(body_cells);
    (capped, starts, cells)
}

/// Lay `pieces` of `text` into `layout` one after another, each at its
/// own size and face, and the glyphs with byte offsets into `text`.
fn lay_pieces(layout: &mut ShapedLayout, fonts: &[Face], text: &str, pieces: &[Piece]) -> Vec<fontdue::layout::GlyphPosition> {
    for &piece in pieces {
        layout.append(fonts, text, piece);
    }
    layout.glyphs().to_vec()
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

/// A text layer's alpha at `t` when its only change over time is a
/// whole-layer fade (its entrance or exit, unstaggered), with nothing
/// keyed except object opacity and nothing that moves on its own: its frame is the layer at
/// rest with that alpha, its shadow, glow and outline fading with it
/// (#2095, #2069). `None` when anything else changes.
fn whole_layer_fade(obj: &TitleObject, tb: &klipp_timeline::TextBox, t: f32, window: Option<f32>) -> Option<f32> {
    let fx = obj.fx();
    let plain_fade = |m: &Option<klipp_timeline::Motion>| m.as_ref().is_none_or(|m| m.kind == klipp_core::title_motion::MotionKind::Fade && m.stagger == klipp_timeline::StaggerUnit::None);
    let unkeyed = obj.transform.center_x_keyframes.is_empty()
        && obj.transform.center_y_keyframes.is_empty()
        && obj.transform.size_x_keyframes.is_empty()
        && obj.transform.size_y_keyframes.is_empty()
        && obj.transform.rotation_keyframes.is_empty()
        && obj.transform.anchor_x_keyframes.is_empty()
        && obj.transform.anchor_y_keyframes.is_empty()
        && !tb.has_keyframes();
    let bare = fx.glows.iter().all(|g| g.radius_keyframes.is_empty() && g.intensity_keyframes.is_empty())
        && fx.shadows.iter().all(|s| s.offset_x_keyframes.is_empty() && s.offset_y_keyframes.is_empty() && s.blur_keyframes.is_empty())
        && fx.inner_shadows.iter().all(|s| s.offset_x_keyframes.is_empty() && s.offset_y_keyframes.is_empty() && s.blur_keyframes.is_empty())
        && fx.outlines.iter().all(|o| o.width_keyframes.is_empty())
        && fx.loops.is_empty()
        && fx.animators.iter().all(|a| !a.enabled)
        && fx.motion_blur.is_none()
        && tb.char_animation.as_ref().is_none_or(|c| !c.enabled)
        && tb.glitch_intensity == 0.0
        && tb.extrusion.is_none()
        && tb.three_d.as_ref().is_none_or(|d| !d.enabled)
        && tb.texture_fill.is_none();
    (unkeyed && bare && plain_fade(&fx.motion.enter) && plain_fade(&fx.motion.exit)).then(|| {
        let (pe, px) = fx.motion.progress(t, window, 0);
        fx.motion.enter.map_or(1.0, |m| m.pose(pe, 0.0).0) * fx.motion.exit.map_or(1.0, |m| m.pose(px, 0.0).0)
    })
}

/// [`TitleFrame::Layers`] composed on the CPU, as the title renderer
/// lays its layers: each resting render at its alpha, each over the
/// ones before it (`over_blend`), in byte units. What the GPU stack's
/// sprite passes make of it (`stack_layer_place_sprite_posed`, then
/// `stack_layer_place_sprite_over`), to a level.
pub fn compose_layers(layers: &[(RestingSprite, f32)], width: u32, height: u32, rows: (u32, u32)) -> Vec<f32> {
    let mut out = vec![0.0f32; (width as usize) * (height as usize) * 4];
    for (k, (sprite, alpha)) in layers.iter().enumerate() {
        let layer = pose_sprite(&sprite.pixels, width, height, 0, 0, *alpha, 1.0, sprite.anchor(), None, None, rows);
        if k == 0 {
            out = layer;
            continue;
        }
        for (d, s) in out.chunks_exact_mut(4).zip(layer.chunks_exact(4)) {
            let sa = s[3] / 255.0;
            if sa <= 0.0 {
                continue;
            }
            let da = d[3] / 255.0;
            let out_a = sa + da * (1.0 - sa);
            for i in 0..3 {
                d[i] = (s[i] * sa + d[i] * da * (1.0 - sa)) / out_a;
            }
            d[3] = out_a * 255.0;
        }
    }
    out
}

/// Glyphs a staggered motion poses one by one at most; beyond it the
/// frame is rastered. Every output pixel considers every glyph.
pub const MAX_POSED_GLYPHS: usize = 256;

/// Which unit each glyph belongs to for `unit`: the line, word or
/// non-space character it is in, counted as `motion_units` counts them.
pub fn units_of(parents: &[char], unit: klipp_timeline::StaggerUnit) -> Vec<usize> {
    klipp_core::title_motion::units_of(parents, unit)
}

/// Every glyph's pose at `time_secs` for a staggered motion: the
/// entrance and exit progress of the glyph's unit, the poses they
/// give, and the loop's offset and fade on top — as the per-glyph
/// rasteriser combines them.
#[allow(clippy::too_many_arguments)]
fn glyph_poses(
    units: &UnitMap,
    motion: &klipp_timeline::TitleMotion,
    animators: &[klipp_timeline::Animator],
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
    // The animators' pose of each glyph, on top of the motion's (#1510).
    let animated = klipp_core::title_motion::animator_poses(animators, &units.parents, font_px, time_secs as f64);
    units
        .boxes
        .iter()
        .enumerate()
        .map(|(g, b)| {
            let (pe, _) = motion.progress(time_secs, window, enter_units.get(g).copied().unwrap_or(0));
            let (_, px) = motion.progress(time_secs, window, exit_units.get(g).copied().unwrap_or(0));
            let (a1, dx1, dy1, s1, _, c1) = motion.enter.map(|m| m.pose(pe, font_px)).unwrap_or(rest);
            let (a2, dx2, dy2, s2, _, c2) = motion.exit.map(|m| m.pose(px, font_px)).unwrap_or(rest);
            let mut alpha = a1 * a2 * loop_pose.2;
            let (mut dx, mut dy) = (dx1 + dx2 + loop_pose.0, dy1 + dy2 + loop_pose.1);
            let mut scale = s1 * s2;
            let mut rotation = 0.0f32;
            if let Some(a) = animated.get(g).filter(|a| !a.is_rest()) {
                alpha *= a.alpha;
                dx += a.dx;
                dy += a.dy;
                scale *= a.scale;
                rotation = a.rotation.to_radians();
            }
            let anchor = ((b[0] + b[2]) as f32 * 0.5, (b[1] + b[3]) as f32 * 0.5);
            let clip = (c1 || c2).then_some((b[1], b[3]));
            // The frame box the posed glyph, its effects round it
            // included, can touch.
            let grow = |v: i32, c: f32, sign: f32| c + (v as f32 + sign * pad - c) * scale;
            let mut moved = if rotation == 0.0 {
                [
                    (grow(b[0], anchor.0, -1.0) + dx).floor() as i32 - 1,
                    (grow(b[1], anchor.1, -1.0) + dy).floor() as i32 - 1,
                    (grow(b[2], anchor.0, 1.0) + dx).ceil() as i32 + 1,
                    (grow(b[3], anchor.1, 1.0) + dy).ceil() as i32 + 1,
                ]
            } else {
                // Turned: the box round the turned corners.
                let (c, sn) = (rotation.cos(), rotation.sin());
                let (hx, hy) = ((b[2] - b[0]) as f32 * 0.5 + pad, (b[3] - b[1]) as f32 * 0.5 + pad);
                let (ex, ey) = ((hx * c).abs() + (hy * sn).abs(), (hx * sn).abs() + (hy * c).abs());
                [
                    (anchor.0 - ex * scale + dx).floor() as i32 - 1,
                    (anchor.1 - ey * scale + dy).floor() as i32 - 1,
                    (anchor.0 + ex * scale + dx).ceil() as i32 + 1,
                    (anchor.1 + ey * scale + dy).ceil() as i32 + 1,
                ]
            };
            if let Some((y0, y1)) = clip {
                moved[1] = moved[1].max(y0);
                moved[3] = moved[3].min(y1);
            }
            if b[2] <= b[0] || b[3] <= b[1] || (alpha <= 0.001 && !full_frame) {
                moved = [0, 0, 0, 0];
            }
            GlyphPose { dx, dy, scale, alpha, anchor, clip, moved, rotation }
        })
        .collect()
}

/// The text of `title` that can be posed along its drawn path on the
/// sprite path (#1697): the one visible text, on a drawn path, with no
/// motion, animator or loop of its own, whose path layer paints nothing
/// (a guide) and is the only other visible layer. The sprite path draws
/// only the glyphs, so a painted path is rastered with the text.
fn path_traveller(title: &TitleData) -> Option<&TitleObject> {
    let visible: Vec<&TitleObject> = title.objects.iter().filter(|o| o.visible).collect();
    let text = visible.iter().copied().find(|o| matches!(o.content, TitleObjectContent::TextBox(_)))?;
    let TitleObjectContent::TextBox(tb) = &text.content else { return None };
    let Some(TextPath::Layer { layer, .. }) = &tb.text_path else { return None };
    let fx = text.fx();
    if fx.motion.enter.is_some() || fx.motion.exit.is_some() || !fx.animators.is_empty() || !fx.loops.is_empty() {
        return None;
    }
    let path_obj = title.object(*layer)?;
    let TitleObjectContent::Shape(shape) = &path_obj.content else { return None };
    let ShapeKind::Path(path) = &shape.kind else { return None };
    let unpainted = |p: &klipp_core::vector_path::PathPose| p.fill[3] <= 0.0 && (p.stroke[3] <= 0.0 || p.stroke_width <= 0.0) && p.stroke_glow.is_none();
    let guide = shape.fill.is_none() && shape.stroke.is_none() && shape.gradient.as_ref().is_none_or(|g| !g.enabled)
        && unpainted(&path.pose) && path.keyframes.iter().all(|k| unpainted(&k.pose)) && path_obj.fx().is_default();
    let others = visible.iter().all(|o| o.id == text.id || o.id == *layer);
    (guide && others).then_some(text)
}

/// The frame a [`TitleFrame::Glyphs`] names, made on the CPU: each
/// glyph of the resting render, by the sprite's glyph map, moved,
/// scaled about its own box and faded by its pose, laid over the
/// glyphs before it; only within `rows`. The GPU glyph pass is its
/// twin.
pub fn pose_sprite_glyphs(rest: &BoxRaster, width: u32, height: u32, units: &UnitMap, poses: &[GlyphPose], rows: (u32, u32)) -> Vec<f32> {
    use rayon::prelude::*;
    let (w, h) = (width as usize, height as usize);
    let mut out = vec![0.0f32; w * h * 4];
    if w == 0 || rest.frame != (width, height) {
        return out;
    }
    let (y0, y1) = ((rows.0 as usize).min(h), (rows.1 as usize).min(h));
    // A tap of glyph `g`: the sprite's pixel where the map says `g`,
    // premultiplied, in sRGB-encoded units; nothing elsewhere.
    let tap = |x: i64, y: i64, g: u16| -> [f32; 4] {
        if x < 0 || y < 0 || x >= w as i64 || y >= h as i64 || units.at(x, y) != g {
            return [0.0; 4];
        }
        let p = rest.at(x, y);
        let a = p[3] / 255.0;
        [p[0] * a, p[1] * a, p[2] * a, a]
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
                let (mut u, mut v) = (x as f32 + 0.5 - p.dx - p.anchor.0, y as f32 + 0.5 - p.dy - p.anchor.1);
                if p.rotation != 0.0 {
                    // Back through the turn, about the glyph's anchor.
                    let (c, sn) = (p.rotation.cos(), p.rotation.sin());
                    (u, v) = (u * c + v * sn, v * c - u * sn);
                }
                let fx = u / p.scale + p.anchor.0 - 0.5;
                let fy = v / p.scale + p.anchor.1 - 0.5;
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
                // Unrounded (#190), the pose's alpha applied.
                let a = (s[3] * p.alpha).clamp(0.0, 1.0);
                if a * 255.0 < 0.5 {
                    continue;
                }
                let rgb = [s[0] / s[3], s[1] / s[3], s[2] / s[3]];
                // Over the glyphs before it, premultiplied.
                for k in 0..3 {
                    acc[k] = rgb[k] * a + acc[k] * (1.0 - a);
                }
                acc[3] = a + acc[3] * (1.0 - a);
            }
            if acc[3] > 0.0 {
                px[0] = acc[0] / acc[3];
                px[1] = acc[1] / acc[3];
                px[2] = acc[2] / acc[3];
                px[3] = acc[3] * 255.0;
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
/// and every row, it is the resting render shifted and faded.
#[allow(clippy::too_many_arguments)]
pub fn pose_sprite(
    rest: &BoxRaster,
    width: u32,
    height: u32,
    dx: i32,
    dy: i32,
    opacity: f32,
    scale: f32,
    anchor: (f32, f32),
    under: Option<[u8; 4]>,
    shape: Option<ScreenFade>,
    rows: (u32, u32),
) -> Vec<f32> {
    use rayon::prelude::*;
    let plain = (scale - 1.0).abs() < 1e-6;
    let (w, h) = (width as usize, height as usize);
    let mut out = vec![0.0f32; w * h * 4];
    if w == 0 || rest.frame != (width, height) {
        return out;
    }
    let (y0, y1) = ((rows.0 as usize).min(h), (rows.1 as usize).min(h));
    let f = opacity.clamp(0.0, 1.0);
    // The layer's fade by where it is: across the frame per column, down
    // it per row, at each pixel's centre, as the GPU twin's
    // `shape_alpha` has it.
    let columns: Option<Vec<f32>> = shape.map(|r| (0..w).map(|x| r.along_x((x as f32 + 0.5) / w as f32)).collect());
    let under_lin = under.map(|c| [srgb_u8_to_linear(c[0]), srgb_u8_to_linear(c[1]), srgb_u8_to_linear(c[2]), c[3] as f32 / 255.0]);
    // A tap: the sprite's pixel, premultiplied, in sRGB-encoded units.
    let tap = |x: i64, y: i64| -> [f32; 4] {
        let p = rest.at(x, y);
        let a = p[3] / 255.0;
        [p[0] * a, p[1] * a, p[2] * a, a]
    };
    out.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        if y < y0 || y >= y1 {
            return;
        }
        let row_k = match shape {
            Some(r) => f * r.along_y((y as f32 + 0.5) / h as f32),
            None => f,
        };
        for (x, px) in row.chunks_exact_mut(4).enumerate() {
            let f = columns.as_ref().map_or(row_k, |c| row_k * c[x]);
            // Unscaled, the pixel as it is: its colour kept even under no
            // alpha, as the frame it was shifted from had it.
            let straight = plain.then(|| rest.at(x as i64 - dx as i64, y as i64 - dy as i64));
            // A collapsed sprite contributes no pixels. Avoid dividing by zero
            // and overflowing bilinear tap coordinates; still composite `under`.
            let s = if scale == 0.0 {
                [0.0; 4]
            } else if let Some(t) = straight {
                let a = t[3] / 255.0;
                [t[0] * a, t[1] * a, t[2] * a, a]
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
            // Unrounded (#190): the alpha and colour as the sprite holds
            // them, the opacity and the fade applied.
            let a = s[3];
            let sa = (a * f).clamp(0.0, 1.0);
            // Scaled, no colour under an alpha that rounds to nothing, as
            // the byte path had it.
            let rgb = match straight {
                Some(t) => [t[0], t[1], t[2]],
                None if a > 0.0 && sa * 255.0 >= 0.5 => [s[0] / a, s[1] / a, s[2] / a],
                None => [0.0; 3],
            };
            match under_lin {
                None => {
                    px[..3].copy_from_slice(&rgb);
                    px[3] = sa * 255.0;
                }
                Some(u) => {
                    let sl = [srgb_to_linear_f(rgb[0] / 255.0), srgb_to_linear_f(rgb[1] / 255.0), srgb_to_linear_f(rgb[2] / 255.0)];
                    let oa = sa + u[3] * (1.0 - sa);
                    for k in 0..3 {
                        let c = if oa > 0.0 { (sl[k] * sa + u[k] * u[3] * (1.0 - sa)) / oa } else { 0.0 };
                        px[k] = linear_to_srgb_f(c) * 255.0;
                    }
                    px[3] = oa * 255.0;
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


/// One channel of a frame being drawn, in byte units: a byte, rounded
/// at every write, as a single layer is drawn, or an `f32`, as the
/// layers of a title are stacked, so that one layer laid over another
/// does not read back what the last one rounded (#190).
pub(crate) trait Channel: Copy + Send + Sync + 'static {
    fn value(self) -> f32;
    fn from_value(v: f32) -> Self;
    /// The channel decoded from sRGB to linear light.
    fn linear(self) -> f32;
}

impl Channel for u8 {
    fn value(self) -> f32 {
        self as f32
    }
    fn from_value(v: f32) -> Self {
        v.round().clamp(0.0, 255.0) as u8
    }
    fn linear(self) -> f32 {
        srgb_decode_lut()[self as usize]
    }
}

impl Channel for f32 {
    fn value(self) -> f32 {
        self
    }
    fn from_value(v: f32) -> Self {
        v.clamp(0.0, 255.0)
    }
    fn linear(self) -> f32 {
        srgb_to_linear_f(self / 255.0)
    }
}

fn over_blend<C: Channel>(dst: &mut [C], src: &[u8]) {
    use rayon::prelude::*;
    debug_assert_eq!(dst.len(), src.len());
    debug_assert_eq!(dst.len() % 4, 0);
    dst.par_chunks_mut(4 * 4096).zip(src.par_chunks(4 * 4096)).for_each(|(dc, sc)| over_blend_chunk(dc, sc));
}

fn over_blend_chunk<C: Channel>(dst: &mut [C], src: &[u8]) {
    for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
        let sa = s[3] as f32 / 255.0;
        if sa <= 0.0 {
            continue;
        }
        let inv_sa = 1.0 - sa;
        let da = d[3].value() / 255.0;
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
            let dc = d[i].value();
            d[i] = C::from_value((sc * sa + dc * da * inv_sa) / out_a);
        }
        d[3] = C::from_value(out_a * 255.0);
    }
}

/// sRGB code to linear light, for the 256 codes.
fn srgb_decode_lut() -> &'static [f32; 256] {
    static LUT: std::sync::OnceLock<[f32; 256]> = std::sync::OnceLock::new();
    LUT.get_or_init(|| std::array::from_fn(|i| srgb_u8_to_linear(i as u8)))
}

/// Lay `src` on `dst` with `mode` (#1513), both straight-alpha sRGB. Normal is `over_blend`, as every layer has always been laid.
/// The other modes mix in linear light, as the clip compositor's do:
/// each channel is the mode's mix where there is something below and
/// the layer's own colour where there is not, then laid over what is
/// below, premultiplied, and encoded back.
fn blend_layer_mode<C: Channel, S: Channel>(dst: &mut [C], src: &[S], mode: klipp_core::BlendMode) {
    use rayon::prelude::*;
    debug_assert_eq!(dst.len(), src.len());
    dst.par_chunks_mut(4 * 4096).zip(src.par_chunks(4 * 4096)).for_each(|(dc, sc)| {
        for (d, s) in dc.chunks_exact_mut(4).zip(sc.chunks_exact(4)) {
            let sa = s[3].value() / 255.0;
            if sa <= 0.0 {
                continue;
            }
            let da = d[3].value() / 255.0;
            let out_a = sa + da * (1.0 - sa);
            if mode == klipp_core::BlendMode::Normal {
                // `over_blend`'s straight-alpha over, for a source that
                // may itself be unrounded.
                for i in 0..3 {
                    d[i] = C::from_value((s[i].value() * sa + d[i].value() * da * (1.0 - sa)) / out_a);
                }
            } else {
                for i in 0..3 {
                    let (cb, cs) = (d[i].linear(), s[i].linear());
                    let mixed = (1.0 - da) * cs + da * mode.blend_channel(cb, cs).clamp(0.0, 1.0);
                    let co = mixed * sa + cb * da * (1.0 - sa);
                    d[i] = C::from_value(linear_to_srgb_f(co / out_a) * 255.0);
                }
            }
            d[3] = C::from_value(out_a * 255.0);
        }
    });
}

/// Scale `layer`'s alpha by the mask `kind` reads from `matte` (#1513),
/// both straight-alpha sRGB of the frame. A luma mask weighs the
/// matte's colour in linear light by Rec.709's row, the primaries a
/// title is authored in.
fn apply_layer_mask<C: Channel, M: Channel>(layer: &mut [C], matte: &[M], kind: klipp_timeline::LayerMaskKind) {
    use rayon::prelude::*;
    debug_assert_eq!(layer.len(), matte.len());
    layer.par_chunks_mut(4 * 4096).zip(matte.par_chunks(4 * 4096)).for_each(|(lc, mc)| {
        for (l, m) in lc.chunks_exact_mut(4).zip(mc.chunks_exact(4)) {
            if l[3].value() <= 0.0 {
                continue;
            }
            let luma = 0.2126 * m[0].linear() + 0.7152 * m[1].linear() + 0.0722 * m[2].linear();
            let v = kind.value(luma, m[3].value() / 255.0);
            l[3] = C::from_value(l[3].value() * v);
        }
    });
}

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

/// How much to shrink a text box's type so its widest word fits
/// `max_width`: 1 when it fits already. The layout wraps between words,
/// and a single word wider than the box it can only break between
/// letters: "Wherever" came out as "Whereve / r" (#508). Each character
/// is measured at its own size, `px_at(byte offset)`, and in its own
/// face, `font_at(byte offset)` (#1505), so a style run set larger or in
/// a wider face counts; the ratio then scales the base size and every
/// run's alike, and the title keeps its proportions.
fn fit_ratio<'f>(font_at: impl Fn(usize) -> &'f Font, text: &str, px_at: impl Fn(usize) -> f32, max_width: f32) -> f32 {
    let mut widest = 0.0f32;
    let mut word = 0.0f32;
    for (i, c) in text.char_indices() {
        if c.is_whitespace() {
            word = 0.0;
        } else {
            word += font_at(i).metrics(c, px_at(i)).advance_width;
            widest = widest.max(word);
        }
    }
    if widest > max_width && max_width > 0.0 {
        // A little under the box, so rounding in the layout cannot put
        // the last letter over the edge.
        max_width / widest * 0.98
    } else {
        1.0
    }
}

/// [`fit_ratio`], and then smaller again until the text, wrapped at
/// `max_width` between words as the layout wraps it, is no taller than
/// `max_height` (#722). A big two-word title ("LA MATA" at 760 px for a
/// word matte) wrapped at the space and ran off the top and bottom of
/// the frame; now it shrinks until it fits — on one line when one line
/// fits, as that is the larger size. Steps of four percent from the
/// width's fit down, so the first size that fits is the largest.
fn fit_block_ratio<'f>(font_at: impl Fn(usize) -> &'f Font, text: &str, px_at: impl Fn(usize) -> f32, max_width: f32, max_height: f32, line_spacing: f32) -> f32 {
    let mut ratio = fit_ratio(&font_at, text, &px_at, max_width);
    if !(max_height > 0.0 && max_width > 0.0) {
        return ratio;
    }
    for _ in 0..60 {
        if wrapped_height(&font_at, text, |i| px_at(i) * ratio, max_width, line_spacing) <= max_height {
            break;
        }
        ratio *= 0.96;
    }
    // A big title set on the lines its author gave reads better than one
    // the box broke: when keeping each line whole costs no more than a
    // 15% of the size, keep them whole.
    let widest_line = widest_line(&font_at, text, &px_at);
    if widest_line * ratio > max_width {
        let whole = max_width / widest_line * 0.98;
        if whole >= ratio * 0.85 && wrapped_height(&font_at, text, |i| px_at(i) * whole, max_width, line_spacing) <= max_height {
            return whole;
        }
    }
    ratio
}

/// The widest of the lines `text` was typed on, at `px_at`, each
/// character in its own face.
fn widest_line<'f>(font_at: impl Fn(usize) -> &'f Font, text: &str, px_at: impl Fn(usize) -> f32) -> f32 {
    let mut widest = 0.0f32;
    let mut offset = 0usize;
    for line in text.split('\n') {
        let w: f32 = line.char_indices().map(|(i, c)| font_at(offset + i).metrics(c, px_at(offset + i)).advance_width).sum();
        widest = widest.max(w);
        offset += line.len() + 1;
    }
    widest
}

/// How much to scale a text box's type for its box, by the box's fit
/// (#1722): [`fit_block_ratio`] for `Wrap`; for `Grow` only what its
/// widest word needs, the lines then taking the height they take; for
/// `Shrink` its typed lines whole, smaller until the widest fits the
/// width and the lines the height, held at the box's minimum size.
///
/// `shaped_widest(ratio)` is the widest typed line shaped at `ratio`, as
/// the layout measures it: each advance is rounded up, so it is measured
/// at each size tried rather than scaled.
fn box_fit_ratio<'f>(
    tb: &klipp_timeline::TextBox,
    font_at: impl Fn(usize) -> &'f Font,
    text: &str,
    px_at: impl Fn(usize) -> f32,
    shaped_widest: impl Fn(f32) -> f32,
    max_width: f32,
    max_height: f32,
) -> f32 {
    match tb.fit {
        klipp_timeline::TextFit::Wrap => fit_block_ratio(font_at, text, px_at, max_width, max_height, tb.line_spacing),
        klipp_timeline::TextFit::Grow => fit_ratio(font_at, text, px_at, max_width),
        klipp_timeline::TextFit::Shrink { min_size } => {
            let floor = if tb.font_size > 0.0 && min_size.is_finite() { (min_size / tb.font_size).clamp(0.0, 1.0) } else { 1.0 };
            let mut ratio = 1.0f32;
            if max_width > 0.0 {
                for _ in 0..30 {
                    let widest = shaped_widest(ratio);
                    if widest <= max_width || ratio <= floor {
                        break;
                    }
                    // A little under the box, as `fit_ratio` leaves it.
                    ratio *= (max_width / widest * 0.99).min(0.99);
                }
            }
            if max_height > 0.0 && max_width > 0.0 {
                for _ in 0..60 {
                    if ratio <= floor || wrapped_height(&font_at, text, |i| px_at(i) * ratio, max_width, tb.line_spacing) <= max_height {
                        break;
                    }
                    ratio *= 0.96;
                }
            }
            ratio.max(floor)
        }
    }
}

/// How tall `text` is laid out at `px_at`, wrapped between words at
/// `max_width` and at its own line breaks: each line as tall as its
/// largest type's line spacing, in the face of the line's first
/// character. Each character is measured in its own face, `font_at`.
fn wrapped_height<'f>(font_at: impl Fn(usize) -> &'f Font, text: &str, px_at: impl Fn(usize) -> f32, max_width: f32, line_spacing: f32) -> f32 {
    // Every line after the first is moved down by the box's line spacing.
    let spacing = |font: &Font, px: f32| font.horizontal_line_metrics(px).map_or(px * 1.2, |m| m.new_line_size) * line_spacing.max(1.0);
    let mut total = 0.0f32;
    let mut offset = 0usize;
    for line in text.split('\n') {
        let mut x = 0.0f32;
        let mut line_px = 0.0f32;
        let mut line_font = font_at(offset);
        let mut first = true;
        let mut word_start = offset;
        for word in line.split(' ') {
            let w: f32 = word.char_indices().map(|(i, c)| font_at(word_start + i).metrics(c, px_at(word_start + i)).advance_width).sum();
            let space = font_at(word_start).metrics(' ', px_at(word_start)).advance_width;
            let px = word.char_indices().map(|(i, _)| px_at(word_start + i)).fold(px_at(word_start), f32::max);
            if !first && x + space + w > max_width {
                total += spacing(line_font, line_px.max(px));
                x = w;
                line_px = px;
                line_font = font_at(word_start);
            } else {
                x += if first { w } else { space + w };
                line_px = line_px.max(px);
            }
            first = false;
            word_start += word.len() + 1;
        }
        total += spacing(line_font, line_px.max(px_at(offset)));
        offset += line.len() + 1;
    }
    total
}

/// The faces a text box is set in, from [`TitleRenderer::run_faces`]:
/// cache keys and OpenType features (#1501), the box's own first, and
/// the index into them of each style run's face (#1505). `fallback`
/// holds, for a face and a character it lacks, the face that draws
/// that character instead (#1503), from [`TitleRenderer::fall_back`].
struct RunFaces {
    keys: Vec<(String, Vec<String>)>,
    of_run: Vec<usize>,
    fallback: HashMap<(usize, char), usize>,
}

impl RunFaces {
    /// The face of the character at byte `i`: its style run's, else
    /// the box's own.
    fn at(&self, runs: &[StyleRun], i: usize) -> usize {
        run_index_at(runs, i).map_or(0, |ri| self.of_run[ri])
    }

    /// The face that draws the character at byte `i` of `text`: the
    /// face it is set in ([`Self::at`]), or the face it falls back to
    /// (#1503). A character that joins the one before it (a variation
    /// selector, a joiner) is drawn in that one's face when it fell back.
    fn drawn(&self, runs: &[StyleRun], text: &str, i: usize) -> usize {
        let face = self.at(runs, i);
        if self.fallback.is_empty() {
            return face;
        }
        let Some(c) = text.get(i..).and_then(|t| t.chars().next()) else { return face };
        if fallback::joins_previous(c) {
            if let Some(prev) = text[..i].chars().next_back() {
                let p = i - prev.len_utf8();
                let drawn = self.drawn(runs, text, p);
                if drawn != self.at(runs, p) {
                    return drawn;
                }
            }
        }
        self.fallback.get(&(face, c)).copied().unwrap_or(face)
    }

    /// `pieces` of `text`, each cut where the face that draws its
    /// characters changes (#1503), so a character its face lacks is
    /// laid and shaped in the face it falls back to. Text its faces
    /// cover keeps its pieces as they were.
    fn split(&self, text: &str, pieces: Vec<Piece>) -> Vec<Piece> {
        if self.fallback.is_empty() {
            return pieces;
        }
        let mut out = Vec::with_capacity(pieces.len());
        for (start, end, px, face) in pieces {
            let mut from = start;
            let mut current = face;
            let mut prev: Option<usize> = None;
            for (j, c) in text[start..end].char_indices() {
                let drawn = match prev {
                    Some(p) if fallback::joins_previous(c) => p,
                    _ => self.fallback.get(&(face, c)).copied().unwrap_or(face),
                };
                prev = Some(drawn);
                if drawn != current {
                    if start + j > from {
                        out.push((from, start + j, px, current));
                    }
                    from = start + j;
                    current = drawn;
                }
            }
            if end > from {
                out.push((from, end, px, current));
            }
        }
        out
    }
}

/// The style run over byte `i`: the first that covers it, as the size
/// and colour of a character are looked up.
fn run_index_at(runs: &[StyleRun], i: usize) -> Option<usize> {
    runs.iter().position(|r| r.start <= i && i < r.end)
}

/// The text from byte `from` to `to`, cut where its style runs begin
/// and end: each piece's byte range and the run that styles it, if one
/// does. Runs are taken in their order, a run's bounds clamped to the
/// range, so a run that starts before `from` styles from `from`.
fn run_pieces(runs: &[StyleRun], from: usize, to: usize) -> Vec<(usize, usize, Option<usize>)> {
    let mut pieces = Vec::new();
    let mut pos = from;
    for (ri, run) in runs.iter().enumerate() {
        let run_start = run.start.clamp(from, to);
        if run_start > pos {
            pieces.push((pos, run_start, None));
            pos = run_start;
        }
        let run_end = run.end.clamp(from, to);
        if run_end > pos {
            pieces.push((pos, run_end, Some(ri)));
            pos = run_end;
        }
    }
    if pos < to {
        pieces.push((pos, to, None));
    }
    pieces
}

/// [`run_pieces`], each set at its run's size (`run_px`) and face
/// (`of_run`), or the box's `base_px` and own face (#1505).
fn set_pieces(runs: &[StyleRun], of_run: &[usize], from: usize, to: usize, base_px: f32, run_px: impl Fn(&StyleRun) -> f32) -> Vec<Piece> {
    run_pieces(runs, from, to)
        .into_iter()
        .map(|(start, end, ri)| match ri {
            Some(ri) => (start, end, run_px(&runs[ri]), of_run[ri]),
            None => (start, end, base_px, 0),
        })
        .collect()
}

/// A style run's tracking and baseline offset, on its laid-out glyphs
/// (#1505). A run's tracking stands in for the box's `base_tracking`
/// on its characters: the difference widens the advance after each of
/// them, which moves the rest of its line along; each line, starting
/// at the glyph indices `line_starts` (ascending), starts from none.
/// A baseline offset raises the run's glyphs (positive is up). Both
/// are 1080p reference pixels, turned to the frame's by `scale`.
fn apply_run_spacing(glyphs: &mut [fontdue::layout::GlyphPosition], runs: &[StyleRun], line_starts: &[usize], base_tracking: f32, scale: f32) {
    if !runs.iter().any(|r| r.tracking.is_some() || r.baseline_offset.is_some()) {
        return;
    }
    let mut extra = 0.0f32;
    for (i, g) in glyphs.iter_mut().enumerate() {
        if line_starts.binary_search(&i).is_ok() {
            extra = 0.0;
        }
        g.x += extra;
        if let Some(run) = run_index_at(runs, g.byte_offset).map(|ri| &runs[ri]) {
            if let Some(tracking) = run.tracking {
                extra += (tracking - base_tracking) * scale;
            }
            if let Some(offset) = run.baseline_offset {
                g.y -= offset * scale;
            }
        }
    }
}

/// The size a character at byte offset `i` is laid out at: its style
/// run's own size when it has one, else `base_px`. `scale` turns a run's
/// points into pixels as the base size was.
fn run_px_at(runs: &[klipp_timeline::StyleRun], i: usize, base_px: f32, scale: f32) -> f32 {
    runs.iter()
        .find(|r| r.start <= i && i < r.end)
        .and_then(|r| r.font_size)
        .map_or(base_px, |pt| pt * scale)
}

/// The stretches of `glyphs` drawn underlined (#1668), as glyph index
/// ranges: each glyph takes its style run's underline, or `boxed`, the
/// box's, outside a run or where the run has none; a stretch ends at a
/// line's end and where the run changes.
fn underline_stretches(glyphs: &[fontdue::layout::GlyphPosition], runs: &[StyleRun], line_starts: &[usize], boxed: bool) -> Vec<(usize, usize)> {
    let run_of = |g: &fontdue::layout::GlyphPosition| runs.iter().position(|r| r.start <= g.byte_offset && g.byte_offset < r.end);
    let underlined = |run: Option<usize>| run.and_then(|r| runs[r].underline).unwrap_or(boxed);
    let mut stretches: Vec<(usize, usize)> = Vec::new();
    let mut open: Option<(usize, usize, Option<usize>)> = None;
    for (i, g) in glyphs.iter().enumerate() {
        let (line, run) = (line_of(line_starts, i), run_of(g));
        if let Some((from, open_line, open_run)) = open {
            if open_line == line && open_run == run {
                continue;
            }
            stretches.push((from, i));
            open = None;
        }
        if underlined(run) {
            open = Some((i, line, run));
        }
    }
    if let Some((from, _, _)) = open {
        stretches.push((from, glyphs.len()));
    }
    // A stretch of spaces alone draws nothing.
    stretches.retain(|&(from, to)| glyphs[from..to].iter().any(|g| g.width > 0));
    stretches
}

/// The line glyph `i` is on, by where each line starts.
fn line_of(line_starts: &[usize], i: usize) -> usize {
    line_starts.partition_point(|&s| s <= i).saturating_sub(1)
}

/// A text box's paragraph settings in frame pixels (#1721).
fn paragraphs_px(tb: &klipp_timeline::TextBox, scale: f32) -> shaping::Paragraphs {
    let p = &tb.paragraph;
    // A list's items are set past their markers.
    let list = p.list.as_ref().map_or(0.0, |l| l.indent.max(0.0));
    shaping::Paragraphs {
        start: (p.indent_start.max(0.0) + list) * scale,
        end: p.indent_end.max(0.0) * scale,
        first: p.indent_first.max(-p.indent_start.max(0.0)) * scale,
        before: p.space_before.max(0.0) * scale,
        after: p.space_after.max(0.0) * scale,
        tabs: p.tabs.iter().filter(|t| t.at.is_finite()).map(|t| (t.at.max(0.0) * scale, t.align)).collect(),
        // Every two ems of the box's type.
        tab_step: tb.font_size.max(1.0) * 2.0 * scale,
        min_last_words: p.min_last_words,
    }
}

/// The room a text box's fit has for its lines (#1721): the box's width
/// less its indents, and its height less the space its paragraphs put
/// between them.
fn paragraph_room(tb: &klipp_timeline::TextBox, text: &str, scale: f32, box_w: f32, box_h: f32) -> (f32, f32) {
    let p = paragraphs_px(tb, scale);
    let gaps = text.matches('\n').count() as f32 * (p.before + p.after);
    ((box_w - p.start - p.end).max(1.0), (box_h - gaps).max(1.0))
}

/// fontdue's alignment for a text box's: a justified box is laid out
/// from the left and [`justify_lines`] then spreads its lines (#1506).
fn h_align_of(alignment: TextAlignment) -> HorizontalAlign {
    match alignment {
        TextAlignment::Left | TextAlignment::Justify => HorizontalAlign::Left,
        TextAlignment::Center => HorizontalAlign::Center,
        TextAlignment::Right => HorizontalAlign::Right,
    }
}

/// How far laid-out text moves across the frame. `transform.center`
/// is the textbox's anchor on the frame; alignment determines which
/// edge of the rendered text rests at that anchor — Left = text's
/// left edge at anchor, Center = text's centre at anchor, Right =
/// text's right edge at anchor. This is the standard NLE model and is
/// the *only* way dragging a Left/Right-aligned textbox actually moves
/// the text. Justified text was laid out across the box's width from
/// x = 0, so the box itself is centred on the anchor (#1506).
fn text_offset_x(alignment: TextAlignment, anchor_x: f32, text_w: f32, text_min_x: f32, box_w: f32) -> f32 {
    match alignment {
        TextAlignment::Left => anchor_x - text_min_x,
        TextAlignment::Center => anchor_x - text_w * 0.5 - text_min_x,
        TextAlignment::Right => anchor_x - text_w - text_min_x,
        TextAlignment::Justify => anchor_x - box_w * 0.5,
    }
}

/// How far a box's indents move its text from where [`text_offset_x`]
/// rests it (#1721): that rests the text's ink, which an indent every
/// line has moves too, so the least indent is put back — from the left
/// for left-aligned text, from the right for right-aligned, half their
/// difference for centred. Justified text is set in the box already.
///
/// A list's markers, at the start indent, count as a line there.
fn indent_shift(alignment: TextAlignment, lines: Option<&Vec<shaping::Line>>, marker: Option<(f32, bool)>) -> f32 {
    let Some(lines) = lines.filter(|l| !l.is_empty()) else { return 0.0 };
    let mut left = lines.iter().map(|l| l.left).fold(f32::MAX, f32::min);
    let mut right = lines.iter().map(|l| l.right).fold(f32::MAX, f32::min);
    match marker {
        Some((pad, false)) => left = left.min(pad),
        Some((pad, true)) => right = right.min(pad),
        None => {}
    }
    match alignment {
        TextAlignment::Left => left,
        TextAlignment::Right => -right,
        TextAlignment::Center => (left - right) * 0.5,
        TextAlignment::Justify => 0.0,
    }
}

/// A list's markers (#1721): before each paragraph that has text, its
/// label laid in the face and at the size of the paragraph's first
/// character (`piece_at(byte)`), at the start indent on the paragraph's
/// first line's baseline — on the right in right-to-left text. Each
/// glyph takes the byte offset of that first character, so the marker
/// is drawn in its colour. A face without the bullet draws "-".
#[allow(clippy::too_many_arguments)]
fn list_markers(
    tb: &klipp_timeline::TextBox,
    fonts: &[shaping::Face],
    text: &str,
    lines: &[shaping::Line],
    glyphs: &[fontdue::layout::GlyphPosition],
    piece_at: impl Fn(usize) -> Option<(f32, usize)>,
    scale: f32,
    box_w: f32,
) -> Vec<fontdue::layout::GlyphPosition> {
    let Some(list) = tb.paragraph.list.as_ref() else { return Vec::new() };
    let rtl = tb.text_direction == TextDirection::Rtl;
    let start = tb.paragraph.indent_start.max(0.0) * scale;
    let mut out = Vec::new();
    let mut item = 0usize;
    for line in lines.iter().filter(|l| l.starts_paragraph) {
        let end = (line.glyph_end + 1).min(glyphs.len());
        let Some(first) = glyphs.get(line.glyph_start..end).and_then(|g| g.iter().filter(|g| !g.parent.is_whitespace() && !g.parent.is_control()).map(|g| g.byte_offset).min()) else { continue };
        let Some((px, face)) = text.get(first..).and_then(|_| piece_at(first)) else { continue };
        let Some(f) = fonts.get(face) else { continue };
        let mut label = list.marker.label(item);
        item += 1;
        if label.chars().any(|c| f.lookup_glyph_index(c) == 0) {
            label = "-".into();
        }
        let mut lay = ShapedLayout::new();
        lay.reset(&LayoutSettings { x: 0.0, y: 0.0, max_width: None, ..LayoutSettings::default() });
        lay.append(fonts, &label, (0, label.len(), px, face));
        let Some(base) = lay.lines().and_then(|l| l.first()).map(|l| l.baseline) else { continue };
        let laid = lay.glyphs();
        let width = laid.iter().map(|g| g.x + g.width as f32).fold(0.0f32, f32::max);
        let dx = if rtl { box_w - start - width } else { start };
        for g in laid {
            let mut g = *g;
            g.x += dx;
            g.y += line.baseline - base;
            g.byte_offset = first;
            out.push(g);
        }
    }
    out
}

/// How far laid-out text moves down the frame: its block's top to the
/// top of the box, its centre to the box's centre `anchor_y`, or its
/// bottom to the box's bottom (#1506).
fn text_offset_y(align: TextVerticalAlign, anchor_y: f32, box_h: f32, text_h: f32, text_min_y: f32) -> f32 {
    match align {
        TextVerticalAlign::Top => anchor_y - box_h * 0.5 - text_min_y,
        TextVerticalAlign::Middle => anchor_y - text_h * 0.5 - text_min_y,
        TextVerticalAlign::Bottom => anchor_y + box_h * 0.5 - text_h - text_min_y,
    }
}

/// Spread each line of a justified text box across `box_w` (#1506):
/// the gaps between its words widen until its last letter's right edge
/// meets the box's right edge. A paragraph's last line — the one before
/// a line break in `text`, or the last of all — follows `last` instead,
/// and a line of one word has no gap to widen and stays where it is.
/// `glyphs` were laid out from the left at x = 0 and `lines` are
/// fontdue's for them; their byte offsets index `text`.
fn justify_lines(
    glyphs: &mut [fontdue::layout::GlyphPosition],
    lines: &[shaping::Line],
    text: &str,
    box_w: f32,
    last: LastLineAlign,
) {
    let seen = |g: &fontdue::layout::GlyphPosition| !g.parent.is_whitespace();
    for line in lines {
        let start = line.glyph_start;
        let end = line.glyph_end.min(glyphs.len().saturating_sub(1));
        if start > end {
            continue;
        }
        let Some(first) = (start..=end).find(|&i| seen(&glyphs[i])) else { continue };
        let Some(final_) = (start..=end).rev().find(|&i| seen(&glyphs[i])) else { continue };
        let right = glyphs[final_].x + glyphs[final_].width as f32;
        // The line's own box, inside its paragraph's indents (#1721).
        let limit = box_w - line.right;
        // The paragraph ends here when a line break lies between this
        // line's last letter and the next line's first.
        // In the text's order, not the line's: a right-to-left line's
        // last letter is its leftmost (#1502).
        let line_end = (start..=end).filter(|&i| seen(&glyphs[i])).map(|i| glyphs[i].byte_offset).max().unwrap_or(glyphs[final_].byte_offset);
        let ends_paragraph = glyphs[end + 1..].iter().filter(|g| seen(g)).map(|g| g.byte_offset).min().is_none_or(|next| {
            text.get(line_end..next).is_some_and(|between| between.contains('\n'))
        });
        if ends_paragraph && last != LastLineAlign::Justify {
            if last == LastLineAlign::Center {
                let shift = (limit - right - (glyphs[first].x - line.left)) * 0.5;
                for g in &mut glyphs[start..=end] {
                    g.x += shift;
                }
            }
            continue;
        }
        let mut gaps = 0usize;
        let mut in_gap = false;
        for g in &glyphs[first..=final_] {
            if !seen(g) {
                in_gap = true;
            } else if in_gap {
                gaps += 1;
                in_gap = false;
            }
        }
        let extra = limit - right;
        if gaps == 0 || extra <= 0.0 {
            continue;
        }
        let step = extra / gaps as f32;
        let (mut k, mut in_gap) = (0usize, false);
        for g in &mut glyphs[first..=end] {
            if !seen(g) {
                in_gap = true;
            } else if in_gap {
                k += 1;
                in_gap = false;
            }
            g.x += step * k as f32;
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

/// Paint a `Shape` layer onto `buf` using its `ObjectTransform`.
/// `transform.center` and `transform.size` are normalised 0..1
/// frame-relative coords, mapped to a pixel rect on the frame and
/// turned by `rotation` about the anchor, then filled and stroked.
/// `straight` lays each pixel over what is there in straight alpha,
/// for a layer drawn alone to be blended or masked (#1513); without
/// it a rectangle or an ellipse mixes its colour with the pixel's as
/// it always has, which darkens a translucent fill over nothing.
fn paint_shape_layer<C: Channel>(
    buf: &mut [C],
    shape: &Shape,
    transform: &ObjectTransform,
    width: u32,
    height: u32,
    time_secs: f32,
    gpu: Option<&mut dyn AlphaMaskProcessor>,
    straight: bool,
) {
    // A gradient fills the shape's box in place of its colour (#1509).
    let gradient = shape.gradient.as_ref().filter(|g| g.enabled).map(|g| (klipp_core::fill_gradient::GradientRamp::new(&g.stops()), g.kind));
    if let ShapeKind::Path(path) = &shape.kind {
        vector_path::paint_filled(buf, &path.at(time_secs as f64), transform, width, height, path.trim.as_ref().map(|t|t.at(time_secs as f64)), gradient.as_ref(), gpu);
        return;
    }
    let w_f = width as f32;
    let h_f = height as f32;
    let half_w = (transform.size.x * w_f * 0.5).max(0.0);
    let half_h = (transform.size.y * h_f * 0.5).max(0.0);
    let cx = transform.center.x * w_f;
    let cy = transform.center.y * h_f;
    // Rotation turns the box about its anchor, as text and paths do.
    // Each pixel centre is turned back into the box's own frame and
    // tested against the unrotated rect, so rounded corners and the
    // stroke rotate with it.
    let pivot_x = cx + (transform.anchor_norm.x - 0.5) * 2.0 * half_w;
    let pivot_y = cy + (transform.anchor_norm.y - 0.5) * 2.0 * half_h;
    let (sin, cos) = transform.rotation.to_radians().sin_cos();
    let unrotate = |px: f32, py: f32| {
        let (dx, dy) = (px - pivot_x, py - pivot_y);
        (pivot_x + cos * dx + sin * dy, pivot_y - sin * dx + cos * dy)
    };
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for (ux, uy) in [(cx - half_w, cy - half_h), (cx + half_w, cy - half_h), (cx - half_w, cy + half_h), (cx + half_w, cy + half_h)] {
        let (dx, dy) = (ux - pivot_x, uy - pivot_y);
        let (rx, ry) = (pivot_x + cos * dx - sin * dy, pivot_y + sin * dx + cos * dy);
        min_x = min_x.min(rx);
        min_y = min_y.min(ry);
        max_x = max_x.max(rx);
        max_y = max_y.max(ry);
    }
    let x0 = (min_x.floor() as i32).max(0);
    let y0 = (min_y.floor() as i32).max(0);
    let x1 = (max_x.ceil() as i32).min(width as i32);
    let y1 = (max_y.ceil() as i32).min(height as i32);
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
            let scaled_r = (r * (crate::frame_scale(width, height))).max(0.0);
            let r2 = scaled_r * scaled_r;

            let fill = shape.fill;
            let stroke = shape.stroke;
            let stroke_w = stroke.map(|s| s.width).unwrap_or(0.0).max(0.0);
            let scaled_sw = stroke_w * (crate::frame_scale(width, height));
            for y in y0..y1 {
                for x in x0..x1 {
                    // Distance from the rectangle's edge — used both
                    // for the rounded-corner mask and for stroke
                    // detection.
                    let (px, py) = unrotate(x as f32 + 0.5, y as f32 + 0.5);
                    let in_rect = is_in_rounded_rect(
                        px, py,
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
                            px, py,
                            cx - half_w + pad, cy - half_h + pad,
                            cx + half_w - pad, cy + half_h - pad,
                            (scaled_r - pad).max(0.0),
                            (scaled_r - pad).max(0.0).powi(2),
                        )
                    };
                    let pixel_color = if in_stroke {
                        stroke.map(|s| s.color)
                    } else if let Some((ramp, kind)) = &gradient {
                        let (bw, bh) = ((2.0 * half_w).max(1.0), (2.0 * half_h).max(1.0));
                        let (u, v) = ((px - (cx - half_w)) / bw, (py - (cy - half_h)) / bh);
                        Some(ramp.at(kind.position_at(u, v, bw, bh)))
                    } else {
                        fill
                    };
                    if let Some(c) = pixel_color {
                        // Composite this single pixel using "over".
                        let idx = ((y as usize) * (width as usize) + (x as usize)) * 4;
                        let one = [c[0], c[1], c[2], c[3]];
                        let tmp = [buf[idx].value(), buf[idx + 1].value(), buf[idx + 2].value(), buf[idx + 3].value()];
                        let sa = one[3] as f32 / 255.0;
                        let inv_sa = 1.0 - sa;
                        let da = tmp[3] / 255.0;
                        let out_a = sa + da * inv_sa;
                        for i in 0..3 {
                            let sc = one[i] as f32;
                            let dc = tmp[i];
                            buf[idx + i] = C::from_value(if straight {
                                // A layer drawn alone to be mixed keeps
                                // its fill's colour where it is
                                // translucent (#1513).
                                if out_a > 0.0 { (sc * sa + dc * da * inv_sa) / out_a } else { 0.0 }
                            } else {
                                sc * sa + dc * inv_sa
                            });
                        }
                        buf[idx + 3] = C::from_value(out_a * 255.0);
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
    let fx = title.primary_fx();
    let mut pad = 0i32;
    // Room for what the animators do to a glyph at most (#1510): move
    // it, grow it, turn it (a glyph's corner reaches about its size
    // away) and blur it.
    let font_px = tb.font_size * scale;
    for a in fx.animators.iter().filter(|a| a.enabled) {
        let p = &a.props;
        let reach = (p.dx.abs() + p.dy.abs()) * font_px
            + (p.scale - 1.0).max(0.0) * font_px
            + if p.rotation != 0.0 { font_px * p.scale.max(1.0) } else { 0.0 }
            + p.blur * font_px * 3.0;
        pad = pad.max(reach.ceil() as i32 + 1);
    }
    for (i, outline) in fx.outlines.iter().enumerate().filter(|(_, o)| o.enabled) {
        // Mirror the renderer's stack gate so we don't reserve padding
        // for layers that won't draw.
        if i >= 1 && !fx.outline_stack_enabled {
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
    for (i, glow) in fx.glows.iter().enumerate().filter(|(_, g)| g.enabled) {
        if i >= 1 && !fx.glow_stack_enabled {
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
    for (i, shadow) in fx.shadows.iter().enumerate().filter(|(_, s)| s.enabled) {
        if i >= 1 && !fx.shadow_stack_enabled {
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
    for (i, inner) in fx.inner_shadows.iter().enumerate().filter(|(_, s)| s.enabled) {
        if i >= 1 && !fx.inner_shadow_stack_enabled {
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

/// Each pixel of `layout`'s colour glyphs inside its mask (#1503): the
/// mask index, the frame pixel's x and y, and the glyph's colour there,
/// its alpha no more than the mask's coverage (a write-on, a fade, a
/// posed glyph's own alpha).
fn colour_pixels(layout: &TitleLayout, mut f: impl FnMut(usize, usize, usize, [u8; 4])) {
    let (mask_w, mask_h) = (layout.mask_w as i32, layout.mask_h as i32);
    let (mask_x0, mask_y0) = (layout.mask_x0 as i32, layout.mask_y0 as i32);
    for g in &layout.colour_glyphs {
        for row in 0..g.height {
            let my = g.y + row as i32 - mask_y0;
            if my < 0 || my >= mask_h {
                continue;
            }
            for col in 0..g.width {
                let mx = g.x + col as i32 - mask_x0;
                if mx < 0 || mx >= mask_w {
                    continue;
                }
                let i = my as usize * mask_w as usize + mx as usize;
                let p = &g.rgba[(row * g.width + col) * 4..][..4];
                let a = p[3].min(layout.alpha_mask[i]);
                if a > 0 {
                    f(i, (mask_x0 + mx) as usize, (mask_y0 + my) as usize, [p[0], p[1], p[2], a]);
                }
            }
        }
    }
}

/// Build per-glyph color info and coverage bitmaps for rich text fill rendering.
/// This is used in the compose phase to draw each glyph with its own color.
fn build_glyph_color_info(
    fonts: &[Face],
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
        // A colour glyph draws in its own colours (#1503).
        if glyph.width == 0 || glyph.height == 0 || fonts[glyph.font_index].colour_glyph(glyph.key.glyph_index, glyph.key.px).is_some() {
            continue;
        }
        let (_, coverage) = fonts[glyph.font_index].coverage(glyph.key);
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
    fonts: &[Face],
    glyphs: &[fontdue::layout::GlyphPosition],
    offset_x: f32,
    offset_y: f32,
    mask: &mut [u8],
    mask_w: usize,
    mask_h: usize,
    mask_x0: usize,
    mask_y0: usize,
    mut index: Option<&mut [u16]>,
    colour: &mut Vec<ColourGlyph>,
) {
    for (gi, glyph) in glyphs.iter().enumerate() {
        if glyph.width == 0 || glyph.height == 0 {
            continue;
        }
        let (_, coverage) = fonts[glyph.font_index].coverage(glyph.key);

        let gx = (glyph.x + offset_x) as i32;
        let gy = (glyph.y + offset_y) as i32;
        // A colour glyph's colours, for `compose` to draw (#1503).
        if let Some(c) = fonts[glyph.font_index].colour_glyph(glyph.key.glyph_index, glyph.key.px) {
            colour.push(ColourGlyph { x: gx, y: gy, width: c.width, height: c.height, rgba: c.rgba.clone() });
        }

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
    fonts: &[Face],
    glyphs: &[fontdue::layout::GlyphPosition],
    offset_x: f32,
    offset_y: f32,
    mask: &mut [u8],
    mask_w: usize,
    mask_h: usize,
    mask_x0: usize,
    mask_y0: usize,
    motion: &klipp_timeline::TitleMotion,
    animators: &[klipp_timeline::Animator],
    time_secs: f32,
    base_font_px: f32,
    window_secs: Option<f32>,
    wave: Option<(f32, f32)>,
    colour: &mut Vec<ColourGlyph>,
    fills: &mut Vec<(GlyphColorInfo, Vec<u8>)>,
    base_colours: &[[u8; 4]],
) {
    let enter_units = motion_units(glyphs, motion.enter.map(|m| m.stagger).unwrap_or_default());
    let exit_units = motion_units(glyphs, motion.exit.map(|m| m.stagger).unwrap_or_default());
    // The animators' pose of each glyph (#1510); a colour one fills
    // every glyph in its own posed colour.
    let parents: Vec<char> = glyphs.iter().map(|g| g.parent).collect();
    let animated = klipp_core::title_motion::animator_poses(animators, &parents, base_font_px, time_secs as f64);
    let colours = animators.iter().any(|a| a.enabled && a.props.colour.is_some());

    for (i, glyph) in glyphs.iter().enumerate() {
        if glyph.width == 0 || glyph.height == 0 {
            continue;
        }
        let (pe, _) = motion.progress(time_secs, window_secs, enter_units[i]);
        let (_, px) = motion.progress(time_secs, window_secs, exit_units[i]);
        let rest = (1.0, 0.0, 0.0, 1.0, 0.0, false);
        let (a1, dx1, dy1, s1, b1, c1) = motion.enter.map(|m| m.pose(pe, base_font_px)).unwrap_or(rest);
        let (a2, dx2, dy2, s2, b2, c2) = motion.exit.map(|m| m.pose(px, base_font_px)).unwrap_or(rest);
        let pose = animated.get(i).copied().unwrap_or_default();
        let alpha = a1 * a2 * pose.alpha;
        if alpha <= 0.001 {
            continue;
        }
        let wave_dy = match wave {
            Some((amp, rate)) => (time_secs * rate * std::f32::consts::TAU + i as f32 * 0.5).sin() * amp,
            None => 0.0,
        };
        let dx = dx1 + dx2 + pose.dx;
        let dy = dy1 + dy2 + wave_dy + pose.dy;
        let scale = s1 * s2 * pose.scale;
        let sigma = b1.max(b2).max(pose.blur);
        let clip_to_rest = c1 || c2;

        let font = &fonts[glyph.font_index];
        let (coverage_base, base_w, base_h) = if (scale - 1.0).abs() > 0.001 {
            let scaled_px = (base_font_px * scale).max(1.0);
            // The shaped glyph (a ligature, a feature's form), not its
            // first character's (#1501).
            // By its key, so a sideways glyph is turned (#1507).
            let (m, cov) = font.coverage(fontdue::layout::GlyphRasterConfig { px: scaled_px.copysign(glyph.key.px), ..glyph.key });
            (cov, m.width, m.height)
        } else {
            let (_, cov) = font.coverage(glyph.key);
            (cov.to_vec(), glyph.width, glyph.height)
        };
        let (coverage, raw_w, raw_h) = if sigma > 0.05 {
            gaussian_blur_expand(&coverage_base, base_w, base_h, sigma)
        } else {
            (coverage_base, base_w, base_h)
        };
        let (coverage, raw_w, raw_h) = if pose.rotation != 0.0 {
            rotate_coverage(&coverage, raw_w, raw_h, pose.rotation.to_radians())
        } else {
            (coverage, raw_w, raw_h)
        };
        let mut fill = colours.then(|| vec![0u8; raw_w * raw_h]);

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
        // A colour glyph's colours at its pose, unblurred: `compose`
        // draws them only as far as the posed coverage reaches (#1503).
        let colour_px = if (scale - 1.0).abs() > 0.001 { (base_font_px * scale).max(1.0) } else { glyph.key.px };
        if let Some(c) = font.colour_glyph(glyph.key.glyph_index, colour_px) {
            let (cx, cy) = (base_x - (base_w as i32 - glyph.width as i32) / 2, base_y - (base_h as i32 - glyph.height as i32) / 2);
            colour.push(ColourGlyph { x: cx, y: cy, width: c.width, height: c.height, rgba: c.rgba.clone() });
        }

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
                if let Some(f) = fill.as_mut() {
                    f[row * raw_w + col] = scaled_cov;
                }
            }
        }
        if let Some(f) = fill {
            let base = base_colours.get(i).copied().unwrap_or([255, 255, 255, 255]);
            let info = GlyphColorInfo { mask_x: gx - mask_x0 as i32, mask_y: gy - mask_y0 as i32, width: raw_w, height: raw_h, color: pose.colour_of(base) };
            fills.push((info, f));
        }
    }
}

/// A glyph's coverage turned clockwise by `radians` about its centre,
/// in a bitmap as large as the turned box, sampled bilinearly; the
/// turned bitmap shares the centre of the one it came from (#1510).
fn rotate_coverage(cov: &[u8], w: usize, h: usize, radians: f32) -> (Vec<u8>, usize, usize) {
    let (c, sn) = (radians.cos(), radians.sin());
    let (hx, hy) = (w as f32 * 0.5, h as f32 * 0.5);
    let ow = ((hx * c).abs() + (hy * sn).abs()).ceil() as usize * 2 + 2;
    let oh = ((hx * sn).abs() + (hy * c).abs()).ceil() as usize * 2 + 2;
    // Keep the centre on the same parity, so the turned glyph sits where
    // the unturned one did.
    let (ow, oh) = (ow + (ow + w) % 2, oh + (oh + h) % 2);
    let at = |x: i64, y: i64| if x < 0 || y < 0 || x >= w as i64 || y >= h as i64 { 0.0 } else { cov[y as usize * w + x as usize] as f32 };
    let mut out = vec![0u8; ow * oh];
    let (ocx, ocy) = (ow as f32 * 0.5, oh as f32 * 0.5);
    for y in 0..oh {
        for x in 0..ow {
            let (u, v) = (x as f32 + 0.5 - ocx, y as f32 + 0.5 - ocy);
            // Back through the turn.
            let (sx, sy) = (u * c + v * sn + hx - 0.5, v * c - u * sn + hy - 0.5);
            let (x0, y0) = (sx.floor(), sy.floor());
            let (tx, ty) = (sx - x0, sy - y0);
            let (x0, y0) = (x0 as i64, y0 as i64);
            let s = at(x0, y0) * (1.0 - tx) * (1.0 - ty) + at(x0 + 1, y0) * tx * (1.0 - ty) + at(x0, y0 + 1) * (1.0 - tx) * ty + at(x0 + 1, y0 + 1) * tx * ty;
            out[y * ow + x] = s.round().clamp(0.0, 255.0) as u8;
        }
    }
    (out, ow, oh)
}

/// Draw an alpha mask onto an RGBA buffer using a given color and intensity.
#[allow(clippy::too_many_arguments)]
/// A mask blurred by `radius`: on the GPU when a processor is in hand
/// and the mask is large enough to be worth the trip, else on the CPU.
fn mask_blur(mask: &[u8], mw: usize, mh: usize, radius: f32, use_gpu: bool, gpu: &mut Option<&mut dyn AlphaMaskProcessor>) -> Vec<u8> {
    if use_gpu {
        if let Some(g) = gpu.as_deref_mut() {
            return g.blur(mask, mw as u32, mh as u32, radius);
        }
    }
    blur_alpha(mask, mw, mh, radius)
}

/// A mask grown by `radius`, on the GPU or the CPU as [`mask_blur`].
fn mask_dilate(mask: &[u8], mw: usize, mh: usize, radius: f32, use_gpu: bool, gpu: &mut Option<&mut dyn AlphaMaskProcessor>) -> Vec<u8> {
    if use_gpu {
        if let Some(g) = gpu.as_deref_mut() {
            return g.dilate(mask, mw as u32, mh as u32, radius);
        }
    }
    dilate_alpha(mask, mw, mh, radius)
}

/// What a layer's stack puts behind its face, drawn from the layer's
/// coverage `mask` into `buf`, back to front: the glows, largest radius
/// first; the drop shadows, highest index first; the outlines, widest
/// first. Secondary layers of each stack are gated by the stack's
/// enabled flag; the primary by its own. The mask is any layer's — the
/// glyphs of a text, the fill of a shape, the pixels of a picture — so
/// the stacks are the same stacks whatever carries them.
/// `outline_cutoff_x` clips the outlines at a frame x (a text's stroke
/// draw-on); `i32::MAX` lets every pixel pass.
#[allow(clippy::too_many_arguments)]
fn stack_behind(
    buf: &mut [f32],
    mask: &[u8],
    mask_w: usize,
    mask_h: usize,
    mask_x0: usize,
    mask_y0: usize,
    w: usize,
    h: usize,
    fx: &LayerFx,
    scale: f32,
    t: f64,
    outline_cutoff_x: i32,
    use_gpu: bool,
    gpu: &mut Option<&mut dyn AlphaMaskProcessor>,
) {
    // 1. Outer glow stack — largest radius first so the tighter glows
    // read on top. radius/intensity may animate via keyframes.
    let mut active_glows: Vec<(f32, f32, &klipp_timeline::TextGlow)> = fx
        .glows
        .iter()
        .enumerate()
        .filter(|(_, g)| g.enabled)
        .filter(|(i, _)| *i == 0 || fx.glow_stack_enabled)
        .map(|(_, g)| (g.evaluated_radius(t), g.evaluated_intensity(t), g))
        .collect();
    active_glows.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    for (radius, intensity, glow) in &active_glows {
        let blurred = mask_blur(mask, mask_w, mask_h, *radius * scale, use_gpu, gpu);
        draw_mask_to_buf(buf, &blurred, mask_w, mask_h, mask_x0, mask_y0, w, h, &glow.color, *intensity);
    }

    // 2. Drop shadow stack — back to front (highest index first) so the
    // lowest index reads as nearest the face.
    for (i, shadow) in fx.shadows.iter().enumerate().rev() {
        if !shadow.enabled || (i >= 1 && !fx.shadow_stack_enabled) {
            continue;
        }
        let sox = (shadow.evaluated_offset_x(t) * scale).round() as i32;
        let soy = (shadow.evaluated_offset_y(t) * scale).round() as i32;
        let blur = shadow.evaluated_blur(t);
        let offset_mask = offset_alpha(mask, mask_w, mask_h, sox, soy);
        if blur > 0.0 {
            let blurred = mask_blur(&offset_mask, mask_w, mask_h, blur * scale, use_gpu, gpu);
            draw_mask_to_buf(buf, &blurred, mask_w, mask_h, mask_x0, mask_y0, w, h, &shadow.color, 1.0);
        } else {
            draw_mask_to_buf(buf, &offset_mask, mask_w, mask_h, mask_x0, mask_y0, w, h, &shadow.color, 1.0);
        }
    }

    // 3. Outlines — widest first. Width may animate, so the evaluated
    // widths are collected up front and sorted on.
    let mut all_outlines: Vec<(f32, &klipp_timeline::TextOutline)> = fx
        .outlines
        .iter()
        .enumerate()
        .filter(|(i, o)| o.enabled && (*i == 0 || fx.outline_stack_enabled))
        .map(|(_, o)| (o.evaluated_width(t), o))
        .collect();
    all_outlines.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    for (eff_width, outline) in &all_outlines {
        let dilated = mask_dilate(mask, mask_w, mask_h, *eff_width * scale, use_gpu, gpu);
        let mut outline_mask: Vec<u8> = dilated.iter().zip(mask).map(|(d, m)| d.saturating_sub(*m)).collect();
        if outline_cutoff_x != i32::MAX {
            for my in 0..mask_h {
                for mx in 0..mask_w {
                    if (mask_x0 + mx) as i32 >= outline_cutoff_x {
                        outline_mask[my * mask_w + mx] = 0;
                    }
                }
            }
        }
        draw_mask_to_buf(buf, &outline_mask, mask_w, mask_h, mask_x0, mask_y0, w, h, &outline.color, 1.0);
    }
}

/// What a layer's stack puts inside its face: the inner shadows, front
/// to back so the lowest index reads as nearest the surface. Drawn over
/// the face, within its coverage only.
#[allow(clippy::too_many_arguments)]
fn stack_inside(
    buf: &mut [f32],
    mask: &[u8],
    mask_w: usize,
    mask_h: usize,
    mask_x0: usize,
    mask_y0: usize,
    w: usize,
    h: usize,
    fx: &LayerFx,
    scale: f32,
    t: f64,
    use_gpu: bool,
    gpu: &mut Option<&mut dyn AlphaMaskProcessor>,
) {
    for (i, inner) in fx.inner_shadows.iter().enumerate().rev() {
        if !inner.enabled || (i >= 1 && !fx.inner_shadow_stack_enabled) {
            continue;
        }
        let sox = (inner.evaluated_offset_x(t) * scale).round() as i32;
        let soy = (inner.evaluated_offset_y(t) * scale).round() as i32;
        let blur = inner.evaluated_blur(t);
        let inverted: Vec<u8> = mask.iter().map(|m| 255u8.saturating_sub(*m)).collect();
        let offset_inv = offset_alpha(&inverted, mask_w, mask_h, sox, soy);
        let shadow_mask = if blur > 0.0 { mask_blur(&offset_inv, mask_w, mask_h, blur * scale, use_gpu, gpu) } else { offset_inv };
        let inner_mask: Vec<u8> = shadow_mask.iter().zip(mask).map(|(s, m)| ((*s as u16 * *m as u16) / 255) as u8).collect();
        draw_mask_to_buf(buf, &inner_mask, mask_w, mask_h, mask_x0, mask_y0, w, h, &inner.color, 1.0);
    }
}

/// Whether a stack draws anything around or inside its layer.
fn stack_draws(fx: &LayerFx) -> bool {
    fx.glows.iter().enumerate().any(|(i, g)| g.enabled && (i == 0 || fx.glow_stack_enabled))
        || fx.shadows.iter().enumerate().any(|(i, s)| s.enabled && (i == 0 || fx.shadow_stack_enabled))
        || fx.outlines.iter().enumerate().any(|(i, o)| o.enabled && (i == 0 || fx.outline_stack_enabled))
        || fx.inner_shadows.iter().enumerate().any(|(i, s)| s.enabled && (i == 0 || fx.inner_shadow_stack_enabled))
}

/// How far, in frame pixels, a stack reaches beyond its layer's
/// coverage at worst: the widest glow, the farthest shadow with its
/// blur, the widest outline — the padding a working mask needs.
fn stack_reach(fx: &LayerFx, scale: f32) -> usize {
    let worst = |v: f32, keys: &[klipp_core::Keyframe]| keys.iter().map(|k| k.value).fold(v, f32::max);
    let mut reach = 0.0f32;
    for g in fx.glows.iter().filter(|g| g.enabled) {
        reach = reach.max(worst(g.radius, &g.radius_keyframes) * 3.0);
    }
    for s in fx.shadows.iter().filter(|s| s.enabled) {
        let ox = worst(s.offset_x.abs(), &s.offset_x_keyframes);
        let oy = worst(s.offset_y.abs(), &s.offset_y_keyframes);
        reach = reach.max(ox.max(oy) + worst(s.blur, &s.blur_keyframes) * 3.0);
    }
    for o in fx.outlines.iter().filter(|o| o.enabled) {
        reach = reach.max(worst(o.width, &o.width_keyframes));
    }
    (reach * scale).ceil().max(1.0) as usize
}

/// The rows and columns of a frame-sized RGBA8 layer that carry any
/// alpha, as `(x0, y0, x1, y1)` with exclusive ends; `None` for an empty
/// layer.
fn layer_bounds(layer: &[u8], width: u32, height: u32) -> Option<(usize, usize, usize, usize)> {
    let (y0, y1) = alpha_rows(layer, width, height);
    if y1 <= y0 {
        return None;
    }
    let (x0, x1) = alpha_cols(layer, width, height, (y0, y1));
    (x1 > x0).then_some((x0 as usize, y0 as usize, x1 as usize, y1 as usize))
}

/// A frame-sized RGBA8 layer softened by a Gaussian of `sigma` pixels:
/// three box passes each way over premultiplied colour, so a blurred
/// edge does not darken toward the colour of nothing. The soft-in a
/// `Blur` entrance asks of a shape or a picture.
fn blur_rgba(layer: &mut [u8], width: u32, height: u32, sigma: f32) {
    let (w, h) = (width as usize, height as usize);
    let r = ((sigma / 1.7).round() as usize).max(1);
    if w == 0 || h == 0 || layer.len() < w * h * 4 {
        return;
    }
    let mut pre: Vec<f32> = layer
        .chunks_exact(4)
        .flat_map(|p| {
            let a = p[3] as f32 / 255.0;
            [p[0] as f32 * a, p[1] as f32 * a, p[2] as f32 * a, p[3] as f32]
        })
        .collect();
    let mut tmp = vec![0f32; pre.len()];
    for _ in 0..3 {
        // Rows: a running window of 2r+1, clamped at the edges.
        for y in 0..h {
            let row = &pre[y * w * 4..(y + 1) * w * 4];
            let out = &mut tmp[y * w * 4..(y + 1) * w * 4];
            for c in 0..4 {
                let mut acc = 0.0f32;
                for x in 0..=r.min(w - 1) {
                    acc += row[x * 4 + c];
                }
                let mut n = (r.min(w - 1) + 1) as f32;
                for x in 0..w {
                    out[x * 4 + c] = acc / n;
                    if x + r + 1 < w {
                        acc += row[(x + r + 1) * 4 + c];
                        n += 1.0;
                    }
                    if x >= r {
                        acc -= row[(x - r) * 4 + c];
                        n -= 1.0;
                    }
                }
            }
        }
        // Columns.
        for x in 0..w {
            for c in 0..4 {
                let mut acc = 0.0f32;
                for y in 0..=r.min(h - 1) {
                    acc += tmp[(y * w + x) * 4 + c];
                }
                let mut n = (r.min(h - 1) + 1) as f32;
                for y in 0..h {
                    pre[(y * w + x) * 4 + c] = acc / n;
                    if y + r + 1 < h {
                        acc += tmp[((y + r + 1) * w + x) * 4 + c];
                        n += 1.0;
                    }
                    if y >= r {
                        acc -= tmp[((y - r) * w + x) * 4 + c];
                        n -= 1.0;
                    }
                }
            }
        }
    }
    for (p, q) in layer.chunks_exact_mut(4).zip(pre.chunks_exact(4)) {
        let a = q[3] / 255.0;
        if a > 0.0 {
            p[0] = (q[0] / a).round().clamp(0.0, 255.0) as u8;
            p[1] = (q[1] / a).round().clamp(0.0, 255.0) as u8;
            p[2] = (q[2] / a).round().clamp(0.0, 255.0) as u8;
        } else {
            p[0] = 0;
            p[1] = 0;
            p[2] = 0;
        }
        p[3] = q[3].round().clamp(0.0, 255.0) as u8;
    }
}

/// The point, in frame pixels, a scaling motion scales about: the centre
/// of the covered box `cols` x `rows` unless the motion names one (#787).
/// `Solid` finds the point deepest inside the letters: the covered box is
/// read in cells, each solid when most of it is covered, and a two-pass
/// chamfer distance gives the cell furthest from any gap. Grown past the
/// frame about it, the word fills the frame with a stroke, never with the
/// gap between its lines or words.
fn resolve_anchor(anchor: Option<klipp_core::title_motion::MotionAnchor>, rgba: &BoxRaster, cols: (u32, u32), rows: (u32, u32)) -> (f32, f32) {
    use klipp_core::title_motion::MotionAnchor;
    let (x0, x1, y0, y1) = (cols.0 as f32, cols.1 as f32, rows.0 as f32, rows.1 as f32);
    let centre = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
    match anchor {
        None => centre,
        Some(MotionAnchor::Point { x, y }) => (x0 + x.clamp(0.0, 1.0) * (x1 - x0), y0 + y.clamp(0.0, 1.0) * (y1 - y0)),
        Some(MotionAnchor::Solid) => solid_point(rgba, cols, rows).unwrap_or(centre),
    }
}

/// The centre of the most covered cell furthest from any uncovered one,
/// in frame pixels; `None` for a box with nothing in it.
fn solid_point(rgba: &BoxRaster, cols: (u32, u32), rows: (u32, u32)) -> Option<(f32, f32)> {
    let (bw, bh) = (cols.1.saturating_sub(cols.0) as usize, rows.1.saturating_sub(rows.0) as usize);
    if bw == 0 || bh == 0 {
        return None;
    }
    // About 160 cells across the longer side: a stroke is several cells.
    let cell = (bw.max(bh) / 160).max(1);
    let (gw, gh) = (bw.div_ceil(cell), bh.div_ceil(cell));
    let alpha = |x: usize, y: usize| rgba.alpha8((cols.0 as usize + x) as i64, (rows.0 as usize + y) as i64);
    // Distance to the nearest uncovered cell, 3 a step and 4 a diagonal;
    // outside the box counts as uncovered.
    const FAR: u32 = u32::MAX / 4;
    let mut d: Vec<u32> = (0..gw * gh)
        .map(|i| {
            let (gx, gy) = (i % gw, i / gw);
            let mut covered = 0usize;
            let mut n = 0usize;
            for y in (gy * cell)..((gy + 1) * cell).min(bh) {
                for x in (gx * cell)..((gx + 1) * cell).min(bw) {
                    n += 1;
                    covered += usize::from(alpha(x, y) >= 128);
                }
            }
            if covered * 2 > n { FAR } else { 0 }
        })
        .collect();
    let at = |d: &Vec<u32>, x: isize, y: isize| if x < 0 || y < 0 || x >= gw as isize || y >= gh as isize { 0 } else { d[y as usize * gw + x as usize] };
    for y in 0..gh as isize {
        for x in 0..gw as isize {
            let i = y as usize * gw + x as usize;
            if d[i] == 0 {
                continue;
            }
            let v = [at(&d, x - 1, y) + 3, at(&d, x, y - 1) + 3, at(&d, x - 1, y - 1) + 4, at(&d, x + 1, y - 1) + 4];
            d[i] = d[i].min(*v.iter().min().unwrap());
        }
    }
    for y in (0..gh as isize).rev() {
        for x in (0..gw as isize).rev() {
            let i = y as usize * gw + x as usize;
            if d[i] == 0 {
                continue;
            }
            let v = [at(&d, x + 1, y) + 3, at(&d, x, y + 1) + 3, at(&d, x + 1, y + 1) + 4, at(&d, x - 1, y + 1) + 4];
            d[i] = d[i].min(*v.iter().min().unwrap());
        }
    }
    let (best, &depth) = d.iter().enumerate().max_by_key(|(_, v)| **v)?;
    if depth == 0 {
        return None;
    }
    let (gx, gy) = (best % gw, best / gw);
    Some((cols.0 as f32 + (gx as f32 + 0.5) * cell as f32, rows.0 as f32 + (gy as f32 + 0.5) * cell as f32))
}

/// A layer's motion as a shape or a picture takes it: as one, with no
/// stagger — a stagger is per glyph, and a layer that is not glyphs has
/// nothing to stagger. Authoring refuses one; data that arrives with one
/// anyway is played whole, and the log says so once.
fn whole_layer_motion(motion: &klipp_timeline::TitleMotion) -> klipp_timeline::TitleMotion {
    let whole = |m: Option<klipp_timeline::Motion>| {
        m.map(|mut m| {
            if m.stagger != klipp_timeline::StaggerUnit::None {
                static SAID: std::sync::Once = std::sync::Once::new();
                SAID.call_once(|| tracing::warn!("a shape or picture layer has a staggered motion; a layer that is not glyphs moves as one"));
                m.stagger = klipp_timeline::StaggerUnit::None;
            }
            m
        })
    };
    klipp_timeline::TitleMotion { enter: whole(motion.enter), exit: whole(motion.exit) }
}

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
    if r == 0 || w == 0 || h == 0 {
        return mask.to_vec();
    }
    use rayon::prelude::*;
    let mut src = mask.to_vec();
    let mut temp = vec![0u8; w * h];
    // Sliding windows preserve the three box passes and truncation exactly.
    // Traverse vertical windows by row bands, with independent halo reads:
    // no strided column walks, and no division for empty windows.
    let horizontal = |input: &[u8], output: &mut [u8]| {
        let mut sum: u64 = input[..r.saturating_add(1).min(w)].iter().map(|&v| v as u64).sum();
        for (x, out) in output.iter_mut().enumerate() {
            let lo = x.saturating_sub(r);
            let hi = x.saturating_add(r).saturating_add(1).min(w);
            *out = if sum == 0 { 0 } else { (sum / (hi - lo) as u64) as u8 };
            if x >= r { sum -= input[x - r] as u64; }
            if let Some(&v) = input.get(x.saturating_add(r).saturating_add(1)) { sum += v as u64; }
        }
    };
    let vertical = |input: &[u8], output: &mut [u8], y0: usize| {
        let mut sums = vec![0u64; w];
        let lo = y0.saturating_sub(r);
        let hi = y0.saturating_add(r).saturating_add(1).min(h);
        for row in input[lo*w..hi*w].chunks_exact(w) {
            for (sum, &v) in sums.iter_mut().zip(row) { *sum += v as u64; }
        }
        for (row, output) in output.chunks_exact_mut(w).enumerate() {
            let y = y0 + row;
            let lo = y.saturating_sub(r);
            let hi = y.saturating_add(r).saturating_add(1).min(h);
            let count = (hi - lo) as u64;
            for (out, &sum) in output.iter_mut().zip(&sums) {
                *out = if sum == 0 { 0 } else { (sum / count) as u8 };
            }
            if y >= r {
                for (sum, &v) in sums.iter_mut().zip(&input[(y - r) * w..][..w]) { *sum -= v as u64; }
            }
            if let Some(next) = y.checked_add(r).and_then(|v| v.checked_add(1)).filter(|&v| v < h) {
                for (sum, &v) in sums.iter_mut().zip(&input[next * w..][..w]) { *sum += v as u64; }
            }
        }
    };
    for _ in 0..3 {
        if w * h >= 65536 {
            src.par_chunks_exact(w).zip(temp.par_chunks_exact_mut(w)).for_each(|(input, output)| horizontal(input, output));
            // At least 64 rows of work per task; the full input is read-only.
            src.par_chunks_mut(w * 64).enumerate().for_each(|(band, output)| vertical(&temp, output, band * 64));
        } else {
            for (input, output) in src.chunks_exact(w).zip(temp.chunks_exact_mut(w)) { horizontal(input, output); }
            vertical(&temp, &mut src, 0);
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

/// Blit a small source alpha buffer into a destination mask with rotation.
/// The glyph is centered at (cx, cy) in the destination and rotated by angle_rad.
#[allow(clippy::too_many_arguments)]
fn blit_rotated(
    dst: &mut [u8], dst_w: usize, dst_h: usize,
    src: &[u8], src_w: usize, src_h: usize,
    cx: f32, cy: f32,
    angle_rad: f32,
    mut owner: Option<(&mut [u16], u16)>,
    clip_y: [i32;2],
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
            if dst_x < 0 || dst_x >= dst_w as i32 || dst_y < 0 || dst_y >= dst_h as i32 || dst_y<clip_y[0] || dst_y>=clip_y[1] {
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
                    // The pixel is the glyph's that paints it most.
                    if let Some((owner, g)) = owner.as_mut() {
                        if src_alpha > dst[dst_idx] {
                            owner[dst_idx] = *g;
                        }
                    }
                    dst[dst_idx] = dst[dst_idx].max(src_alpha);
                }
            }
        }
    }
}

/// Rotate a frame-sized RGBA layer around an object anchor. The title
/// renderer already produces a transparent frame-sized layer, so this
/// touches only the non-transparent bounds and keeps the common zero-
/// rotation path allocation-free.
fn rotate_rgba_layer(
    layer: &mut [u8],
    width: usize,
    height: usize,
    pivot_x: f32,
    pivot_y: f32,
    angle_rad: f32,
) {
    if width == 0 || height == 0 || layer.len() < width.saturating_mul(height).saturating_mul(4) {
        return;
    }
    let mut min_x = width;
    let mut min_y = height;
    let mut max_x = 0usize;
    let mut max_y = 0usize;
    for y in 0..height {
        for x in 0..width {
            if layer[(y * width + x) * 4 + 3] != 0 {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }
    if min_x > max_x || min_y > max_y {
        return;
    }
    let source = layer.to_vec();
    layer.fill(0);
    let (sin, cos) = angle_rad.sin_cos();
    let radius = ((max_x - min_x + 1).pow(2) as f32
        + (max_y - min_y + 1).pow(2) as f32)
        .sqrt()
        .ceil() as i32;
    let x0 = (pivot_x.floor() as i32 - radius).max(0);
    let x1 = (pivot_x.ceil() as i32 + radius).min(width as i32 - 1);
    let y0 = (pivot_y.floor() as i32 - radius).max(0);
    let y1 = (pivot_y.ceil() as i32 + radius).min(height as i32 - 1);
    for y in y0..=y1 {
        for x in x0..=x1 {
            let dx = x as f32 - pivot_x;
            let dy = y as f32 - pivot_y;
            let sx = pivot_x + dx * cos + dy * sin;
            let sy = pivot_y - dx * sin + dy * cos;
            let sx_i = sx.round() as i32;
            let sy_i = sy.round() as i32;
            if sx_i < min_x as i32 || sx_i > max_x as i32 || sy_i < min_y as i32 || sy_i > max_y as i32 {
                continue;
            }
            let src_idx = (sy_i as usize * width + sx_i as usize) * 4;
            if source[src_idx + 3] == 0 {
                continue;
            }
            let dst_idx = (y as usize * width + x as usize) * 4;
            layer[dst_idx..dst_idx + 4].copy_from_slice(&source[src_idx..src_idx + 4]);
        }
    }
}

#[cfg(test)]
mod object_transform_tests {
    use super::rotate_rgba_layer;

    #[test]
    fn rotates_rgba_layer_around_anchor() {
        let mut layer = vec![0u8; 5 * 5 * 4];
        let idx = (1usize * 5 + 2) * 4;
        layer[idx..idx + 4].copy_from_slice(&[255, 10, 20, 255]);
        rotate_rgba_layer(&mut layer, 5, 5, 2.0, 2.0, std::f32::consts::FRAC_PI_2);
        let rotated = (2usize * 5 + 3) * 4;
        assert_eq!(&layer[rotated..rotated + 4], &[255, 10, 20, 255]);
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
/// A full-frame card's colour over the whole of `buf`, when the title
/// has one.
fn fill_full_frame(buf: &mut [f32], title: &TitleData) {
    if let TitleBackground::FullFrame { color } = &title.background {
        let fill = [color[0] as f32, color[1] as f32, color[2] as f32, color[3] as f32];
        for pixel in buf.chunks_exact_mut(4) {
            pixel.copy_from_slice(&fill);
        }
    }
}

/// A backing panel round `bounds` (left, top, right, bottom in frame
/// pixels), padded by its padding at `scale`, when the title has one.
fn paint_backing(buf: &mut [f32], title: &TitleData, bounds: [f32; 4], width: u32, height: u32, scale: f32) {
    if let TitleBackground::Color { color, padding } = &title.background {
        let w = width as usize;
        let pad = padding * scale;
        let bx0 = ((bounds[0] - pad) as i32).max(0) as usize;
        let by0 = ((bounds[1] - pad) as i32).max(0) as usize;
        let bx1 = ((bounds[2] + pad) as i32).min(width as i32) as usize;
        let by1 = ((bounds[3] + pad) as i32).min(height as i32) as usize;
        for row in by0..by1 {
            for col in bx0..bx1 {
                let idx = (row * w + col) * 4;
                blend_pixel(buf, idx, color);
            }
        }
    }
}

/// `buf` made a frame of `pixels` transparent pixels when it is none yet:
/// a mixed layer's frame, made when the layer draws into it (#1725).
fn frame(buf: &mut Vec<f32>, pixels: usize) {
    if buf.is_empty() {
        *buf = vec![0.0; pixels * 4];
    }
}

thread_local! {
    /// [`TitleRenderer::compose`]'s working frame, this thread's, zero
    /// between layers (#1725).
    static COMPOSE_SCRATCH: std::cell::RefCell<Vec<f32>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn to_bytes(buf: &[f32]) -> Vec<u8> {
    buf.iter().map(|v| v.round().clamp(0.0, 255.0) as u8).collect()
}

/// [`to_bytes`] of rows `y0..y1` of a frame `w` wide, every other row
/// zero (#1725): what a layer drawn only in those rows rounds to.
fn to_bytes_rows(buf: &[f32], w: usize, y0: usize, y1: usize) -> Vec<u8> {
    use rayon::prelude::*;
    let mut out = vec![0u8; buf.len()];
    let (a, b) = (y0 * w * 4, (y1 * w * 4).min(buf.len()));
    if a < b {
        // A row at a time across the threads: at 4K a word's rows are
        // millions of values.
        let row = (w * 4).max(1);
        out[a..b].par_chunks_mut(row).zip(buf[a..b].par_chunks(row)).for_each(|(o, v)| {
            for (o, v) in o.iter_mut().zip(v) {
                *o = v.round().clamp(0.0, 255.0) as u8;
            }
        });
    }
    out
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
/// * `case_transform`, `alignment`, `last_line`, `vertical_align`
/// * `tracking`, `line_spacing`, `baseline_shift`
/// * `style_runs[*]` — `start`, `end`, `font_name`, `font_size`,
///   `font_style`, `tracking` and `baseline_offset` (each moves the
///   run's glyphs, #1505), and `features` (#1501); not its colour
/// * `font_weight` and `font_axes`, the box's and each run's (#1504)
/// * `features`, the box's OpenType features, and [`SHAPING_VERSION`],
///   so positions laid out before shaping changed do not match (#1501)
/// * [`FALLBACK_VERSION`], so positions laid out before the fallback
///   chain changed which face draws a character do not match (#1503)
/// * `vertical_layout`, `vertical_columns_ltr` and, in a vertical box,
///   `drop_cap_scale`: the columns the caret follows (#1704)
/// * `position()` — the title's frame-relative offset
/// * `frame_w`, `frame_h` — render resolution
///
/// **Deliberately excluded** (visual fill, doesn't move glyphs):
/// color, gradient, glow, outline, shadow, extrusion, motion blur,
/// character animation, texture fill, underline (the box's and a
/// style run's, #1668). A colour-only edit must NOT
/// invalidate the cache or the picker recomputes wastefully on
/// every colour-drag frame.
pub fn glyph_layout_hash(td: &TitleData, frame_w: u32, frame_h: u32) -> u64 {
    layout_hash_with(td, frame_w, frame_h, FALLBACK_VERSION)
}

/// [`glyph_layout_hash`] with the fallback chain at `fallback_version`.
fn layout_hash_with(td: &TitleData, frame_w: u32, frame_h: u32, fallback_version: u32) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();

    let tb = td.primary_text_or_default();
    tb.text.hash(&mut h);
    tb.font_name.hash(&mut h);
    std::mem::discriminant(&tb.font_style).hash(&mut h);
    tb.font_size.to_bits().hash(&mut h);
    std::mem::discriminant(&tb.case_transform).hash(&mut h);
    std::mem::discriminant(&tb.alignment).hash(&mut h);
    std::mem::discriminant(&tb.last_line).hash(&mut h);
    std::mem::discriminant(&tb.vertical_align).hash(&mut h);
    tb.tracking.to_bits().hash(&mut h);
    tb.line_spacing.to_bits().hash(&mut h);
    tb.baseline_shift.to_bits().hash(&mut h);
    // Vertical text sets the characters in columns (#1704).
    tb.vertical_layout.hash(&mut h);
    if tb.vertical_layout {
        tb.vertical_columns_ltr.hash(&mut h);
        tb.drop_cap_scale.to_bits().hash(&mut h);
    }
    // The path the text follows, when it follows one (#1508).
    if let Some(path) = &tb.text_path {
        serde_json::to_string(path).unwrap_or_default().hash(&mut h);
    }
    tb.features.hash(&mut h);
    tb.font_weight.hash(&mut h);
    tb.font_axes.iter().for_each(|(tag, v)| (tag, v.to_bits()).hash(&mut h));
    SHAPING_VERSION.hash(&mut h);
    fallback_version.hash(&mut h);

    for run in &tb.style_runs {
        run.start.hash(&mut h);
        run.end.hash(&mut h);
        run.font_size.map(f32::to_bits).hash(&mut h);
        run.font_name.hash(&mut h);
        run.font_style.map(|s| std::mem::discriminant(&s)).hash(&mut h);
        run.tracking.map(f32::to_bits).hash(&mut h);
        run.baseline_offset.map(f32::to_bits).hash(&mut h);
        run.features.hash(&mut h);
        run.font_weight.hash(&mut h);
        run.font_axes.iter().for_each(|(tag, v)| (tag, v.to_bits()).hash(&mut h));
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
    #[allow(unused_mut)]
    let mut fx = klipp_timeline::LayerFx::default();
    let mut td = klipp_timeline::TitleData::single_text_fx(tb, fx);
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
    #[allow(unused_mut)]
    let mut fx = klipp_timeline::LayerFx::default();
    let mut td = klipp_timeline::TitleData::single_text_fx(tb, fx);
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
            style: None,
            published: Vec::new(),
            template: None,
        };
        let _shape_id = td.add_object(
            TitleObjectContent::Shape(klipp_timeline::Shape {
                kind: klipp_timeline::ShapeKind::Rectangle { corner_radius: 0.0 },
                fit: None,
                fill: Some([255, 0, 0, 255]),
                gradient: None,
                stroke: None,
            }),
            klipp_timeline::ObjectTransform {
                center: TitlePosition { x: 0.5, y: 0.5 },
                center_x_keyframes: Vec::new(),
                center_y_keyframes: Vec::new(),
                size_x_keyframes: Vec::new(),
                size_y_keyframes: Vec::new(),
                size: TitlePosition { x: 0.5, y: 0.5 },
                rotation: 0.0,
                rotation_keyframes: Vec::new(),
                anchor_norm: TitlePosition { x: 0.5, y: 0.5 },
                anchor_x_keyframes: Vec::new(),
                anchor_y_keyframes: Vec::new(),
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
    let tb = klipp_timeline::TextBox {
        text: "S".to_string(),
        font_size: 100.0,
        ..Default::default()
    };
    #[allow(unused_mut)]
    let mut fx = klipp_timeline::LayerFx::default();
    fx.shadows.clear();
    
    klipp_timeline::TitleData::single_text_fx(tb, fx)
};
        let with_shadow = {
    let mut td = no_shadow.clone();
    if let Some(fx) = td.primary_object_mut().map(|o| o.fx_mut()) {
        fx.shadows.push(klipp_timeline::TitleShadow {
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
    #[allow(unused_mut)]
    let mut fx = klipp_timeline::LayerFx::default();
    
    klipp_timeline::TitleData::single_text_fx(tb, fx)
};
        let with_outline = {
    let mut td = no_outline.clone();
    if let Some(fx) = td.primary_object_mut().map(|o| o.fx_mut()) {
        fx.outlines.insert(0, klipp_timeline::TextOutline {
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
    #[allow(unused_mut)]
    let mut fx = klipp_timeline::LayerFx::default();
    
    klipp_timeline::TitleData::single_text_fx(tb, fx)
};
        let with_glow = {
    let mut td = no_glow.clone();
    if let Some(fx) = td.primary_object_mut().map(|o| o.fx_mut()) {
        fx.glows.push(klipp_timeline::TextGlow {
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
        gradient: Some(klipp_timeline::TextGradient::pair([255, 0, 0, 255], [0, 0, 255, 255], true)),
        ..Default::default()
    };
    #[allow(unused_mut)]
    let mut fx = klipp_timeline::LayerFx::default();
    
    klipp_timeline::TitleData::single_text_fx(tb, fx)
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
    #[allow(unused_mut)]
    let mut fx = klipp_timeline::LayerFx::default();
    
    klipp_timeline::TitleData::single_text_fx(tb, fx)
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
    #[allow(unused_mut)]
    let mut fx = klipp_timeline::LayerFx::default();
    
    klipp_timeline::TitleData::single_text_fx(tb, fx)
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

    fn text_title(text: &str, size: f32) -> klipp_timeline::TitleData {
        let tb = klipp_timeline::TextBox { text: text.to_string(), font_size: size, ..Default::default() };
        klipp_timeline::TitleData::single_text_fx(tb, klipp_timeline::LayerFx::default())
    }

    /// #460: a size is pixels of the frame's short side at 1080, so a
    /// title reads the same on a vertical frame as on a landscape one of
    /// the same resolution. By the height, 9:16 drew it 3.56x as large on
    /// a 2160-wide frame.
    #[test]
    fn a_size_reads_the_same_on_a_vertical_frame() {
        let mut renderer = TitleRenderer::new();
        let td = text_title("La Mata", 72.0);
        let line = |r: &mut TitleRenderer, w, h| r.compute_char_positions(&td, w, h)[0].height;
        for (w, h) in [(1920, 1080), (3840, 2160)] {
            let (landscape, portrait) = (line(&mut renderer, w, h), line(&mut renderer, h, w));
            assert!((landscape - portrait).abs() < 0.5, "{w}x{h}: {landscape} px landscape, {portrait} px portrait");
        }
        assert_eq!(frame_scale(2160, 3840), 2.0);
        assert_eq!(frame_scale(1920, 1080), 1.0);
    }

    /// #508: a word wider than its box shrinks the text rather than
    /// breaking between its letters ("Whereve / r"); text that fits keeps
    /// its size.
    #[test]
    fn a_word_wider_than_the_box_stays_whole() {
        let mut renderer = TitleRenderer::new();
        let (w, h) = (1920, 1080);
        let big = renderer.compute_char_positions(&text_title("Wherever We Walk", 420.0), w, h);
        // On one line x only grows; a break goes back to the line's start.
        assert!(big[..8].windows(2).all(|p| p[1].x > p[0].x), "\"Wherever\" broke across lines: {:?}", big[..8].iter().map(|c| c.x).collect::<Vec<_>>());
        let box_w = 0.9 * w as f32;
        let right = big[..8].iter().map(|c| c.x + c.advance).fold(0.0f32, f32::max) - big[0].x;
        assert!(right <= box_w + 1.0, "the word is {right} px, wider than the {box_w} px box");

        let fits = renderer.compute_char_positions(&text_title("Wherever We Walk", 72.0), w, h);
        let unfitted = TitleRenderer::new().compute_char_positions(&text_title("Hi", 72.0), w, h);
        assert!((fits[0].height - unfitted[0].height).abs() < 0.5, "text that fits keeps its size");
    }

    /// #722: a big two-word title for a word matte, too wide for one line
    /// at its size, wrapped at the space and ran off the top and bottom
    /// of the frame. It now shrinks until it fits: on one line, which is
    /// the larger size, and inside the frame.
    #[test]
    fn a_big_two_word_title_fits_the_frame() {
        let mut renderer = TitleRenderer::new();
        let (w, h) = (1920, 1080);
        let pos = renderer.compute_char_positions(&text_title("LA MATA", 760.0), w, h);
        assert!(pos.windows(2).all(|p| p[1].x > p[0].x), "it broke across lines: {:?}", pos.iter().map(|c| (c.x, c.y)).collect::<Vec<_>>());
        let top = pos.iter().map(|c| c.y).fold(f32::MAX, f32::min);
        let bottom = pos.iter().map(|c| c.y + c.height).fold(0.0f32, f32::max);
        assert!(top >= 0.0 && bottom <= h as f32, "the title runs off the frame: {top}..{bottom}");
        let right = pos.iter().map(|c| c.x + c.advance).fold(0.0f32, f32::max) - pos[0].x;
        assert!(right <= 0.9 * w as f32 + 1.0, "wider than the box: {right}");
        // Lines given by the author stay lines, and the block fits.
        let text = "SALT\nAND SUN";
        let two = renderer.compute_char_positions(&text_title(text, 900.0), w, h);
        // The glyphs, not the boxes given to the spaces and the break.
        let chars: Vec<char> = text.chars().collect();
        let glyphs: Vec<&CharPosition> = two.iter().filter(|c| chars.get(c.char_index).is_some_and(|ch| !ch.is_whitespace())).collect();
        let top = glyphs.iter().map(|c| c.y).fold(f32::MAX, f32::min);
        let bottom = glyphs.iter().map(|c| c.y + c.height).fold(0.0f32, f32::max);
        assert!(top >= 0.0 && bottom <= h as f32, "two lines run off the frame: {top}..{bottom}");
        assert!(glyphs.iter().filter(|c| c.char_index < 4).all(|c| c.y < glyphs[4].y), "the author's break is kept");
    }

    /// #508 review: the fit shrinks the whole title alike, a style run
    /// with it. A colour-only highlight over the oversized word stays the
    /// same size as the words round it, and a run set larger is measured,
    /// so it fits too.
    #[test]
    fn a_fitted_title_keeps_its_style_runs_in_proportion() {
        let mut renderer = TitleRenderer::new();
        let (w, h) = (1920, 1080);
        let titled = |runs: Vec<klipp_timeline::StyleRun>| {
            let tb = klipp_timeline::TextBox { text: "Wherever We Walk".into(), font_size: 420.0, style_runs: runs, ..Default::default() };
            klipp_timeline::TitleData::single_text_fx(tb, klipp_timeline::LayerFx::default())
        };
        let highlight = klipp_timeline::StyleRun { color: Some([255, 200, 0, 255]), ..klipp_timeline::StyleRun::default_for_range(0, 8) };
        let pos = renderer.compute_char_positions(&titled(vec![highlight]), w, h);
        let (inside, outside) = (pos[0].height, pos[9].height);
        assert!((inside - outside).abs() < 0.5, "the highlighted word is {inside} px high, the rest {outside}");

        let bigger = klipp_timeline::StyleRun { font_size: Some(560.0), ..klipp_timeline::StyleRun::default_for_range(0, 8) };
        let pos = renderer.compute_char_positions(&titled(vec![bigger]), w, h);
        assert!(pos[..8].windows(2).all(|p| p[1].x > p[0].x), "the larger word stays whole");
        let right = pos[..8].iter().map(|c| c.x + c.advance).fold(0.0f32, f32::max) - pos[0].x;
        assert!(right <= 0.9 * w as f32 + 1.0, "and inside the box: {right} px");
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
    #[allow(unused_mut)]
    let mut fx = klipp_timeline::LayerFx::default();
    
    klipp_timeline::TitleData::single_text_fx(tb, fx)
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
            #[allow(unused_mut)]
            let mut fx = klipp_timeline::LayerFx::default();
            klipp_timeline::TitleData::single_text_fx(tb, fx)
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
            #[allow(unused_mut)]
            let mut fx = klipp_timeline::LayerFx::default();
            klipp_timeline::TitleData::single_text_fx(tb, fx)
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

    /// #1502: a right-to-left box is a right-to-left paragraph, ordered
    /// by the bidi algorithm: Latin letters keep their order, a closing
    /// mark goes to the paragraph's start, on its left, and Hebrew reads
    /// right to left; the cursor's positions follow what is drawn.
    #[test]
    fn a_right_to_left_box_orders_its_text_by_the_bidi_algorithm() {
        let mut r = TitleRenderer::new();
        r.system_fallback = false;
        let path = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/NotoSansHebrew-subset.ttf"));
        let traits = variable::face_traits(&std::fs::read(&path).unwrap()).unwrap();
        let info = font_discovery::FontInfo { family: "Hebrew Test".into(), style: "Regular".into(), path, weight: traits.weight, italic: traits.italic, axes: traits.axes };
        r.font_registry = std::sync::Arc::new([("Hebrew Test".to_string(), vec![info])].into_iter().collect());
        let mk = |text: &str, dir: klipp_timeline::TextDirection| {
            let tb = klipp_timeline::TextBox { text: text.to_string(), font_size: 80.0, font_name: "Hebrew Test".into(), text_direction: dir, ..Default::default() };
            klipp_timeline::TitleData::single_text_fx(tb, klipp_timeline::LayerFx::default())
        };
        let x_of = |at: &[CharPosition], text: &str, c: char| at.iter().find(|p| text[p.byte_offset..].starts_with(c)).unwrap_or_else(|| panic!("no {c}")).x;
        let at = r.compute_char_positions(&mk("ABC!", klipp_timeline::TextDirection::Rtl), 1280, 720);
        let x = |c| x_of(&at, "ABC!", c);
        assert!(x('!') < x('A') && x('A') < x('B') && x('B') < x('C'), "Latin in its order, the mark at the start");
        let at = r.compute_char_positions(&mk("ABC!", klipp_timeline::TextDirection::Ltr), 1280, 720);
        let x = |c| x_of(&at, "ABC!", c);
        assert!(x('A') < x('B') && x('C') < x('!'), "left to right, as ever");
        let at = r.compute_char_positions(&mk("שלום", klipp_timeline::TextDirection::Rtl), 1280, 720);
        let x = |c| x_of(&at, "שלום", c);
        assert!(x('ש') > x('ל') && x('ל') > x('ו') && x('ו') > x('ם'), "Hebrew right to left");
        let rtl = r.render(&mk("ABC!", klipp_timeline::TextDirection::Rtl), 1280, 720);
        let ltr = r.render(&mk("ABC!", klipp_timeline::TextDirection::Ltr), 1280, 720);
        assert_ne!(rtl, ltr, "the mark draws where it is laid");
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
            #[allow(unused_mut)]
            let mut fx = klipp_timeline::LayerFx::default();
            klipp_timeline::TitleData::single_text_fx(tb, fx)
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
        #[allow(unused_mut)]
        let mut fx = klipp_timeline::LayerFx::default();
        let td = klipp_timeline::TitleData::single_text_fx(tb, fx);

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
        let tb = klipp_timeline::TextBox {
            text: "ABCDE".to_string(),
            font_size: 80.0,
            ..Default::default()
        };
        #[allow(unused_mut)]
        let mut fx = klipp_timeline::LayerFx::default();
        fx.outlines.push(klipp_timeline::TextOutline {
            color: [255, 0, 0, 255],
            width: 4.0,
            enabled: true,
            width_keyframes: Vec::new(),
        });
        let td = klipp_timeline::TitleData::single_text_fx(tb, fx);

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
        #[allow(unused_mut)]
        let mut fx = klipp_timeline::LayerFx::default();
        let mut td = klipp_timeline::TitleData::single_text_fx(tb, fx);
        td.primary_object_mut().unwrap().fx_mut().loops.push(klipp_timeline::TitleLoop::ColorBlend {
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
            #[allow(unused_mut)]
            let mut fx = klipp_timeline::LayerFx::default();
            klipp_timeline::TitleData::single_text_fx(tb, fx)
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
            #[allow(unused_mut)]
            let mut fx = klipp_timeline::LayerFx::default();
            klipp_timeline::TitleData::single_text_fx(tb, fx)
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

    /// #1674: every line of a drop-cap box sits inside the box. The
    /// box is the width its alignment sets from the anchor: from it
    /// rightwards for Left, about it for Center, leftwards for Right.
    /// Left wraps beside the cap: the lines beside it start past the
    /// cap and keep to the width left beside it, the lines under it
    /// start back under the cap at the box's full width.
    #[test]
    fn drop_cap_lines_stay_inside_the_box() {
        use klipp_timeline::{TextAlignment, TitlePosition};
        const TEXT: &str = "Lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor \
                            incididunt ut labore et dolore magna aliqua ut enim ad minim veniam quis nostrud \
                            exercitation WWWWWWWWWW ullamco laboris nisi ut aliquip ex ea commodo consequat";
        let mut renderer = TitleRenderer::new();
        renderer.system_fallback = false;
        for (alignment, anchor) in [(TextAlignment::Left, 0.2), (TextAlignment::Center, 0.5), (TextAlignment::Right, 0.8)] {
            for family in ["Inter", "Bebas Neue"] {
                let tb = klipp_timeline::TextBox { text: TEXT.to_string(), font_name: family.into(), font_size: 48.0, alignment, drop_cap_scale: 3.0, ..Default::default() };
                let mut td = klipp_timeline::TitleData::single_text_fx(tb, klipp_timeline::LayerFx::default());
                let o = td.objects.first_mut().unwrap();
                o.transform.center = TitlePosition { x: anchor, y: 0.5 };
                o.transform.size = TitlePosition { x: 0.55, y: 0.9 };
                let layout = renderer.render_layout(&td, 1920, 1080);
                let (box_w, anchor_x) = (0.55 * 1920.0, anchor * 1920.0);
                let (left, right) = match alignment {
                    TextAlignment::Left => (anchor_x, anchor_x + box_w),
                    TextAlignment::Center => (anchor_x - box_w * 0.5, anchor_x + box_w * 0.5),
                    _ => (anchor_x - box_w, anchor_x),
                };
                let ink: Vec<_> = layout.glyph_boxes.iter().zip(&layout.glyph_parents).filter(|(_, c)| !c.is_whitespace()).map(|(b, _)| *b).collect();
                assert!(ink.len() > 100, "{alignment:?} {family}: {} glyphs", ink.len());
                for b in &ink {
                    // A glyph's ink may reach a pixel or two past its advance.
                    assert!(b[0] as f32 >= left - 3.0 && b[2] as f32 <= right + 3.0, "{alignment:?} {family}: a glyph at {b:?} is outside {left}..{right}");
                }
                if alignment != TextAlignment::Left {
                    continue;
                }
                let cap = ink[0];
                let beside: Vec<_> = ink[1..].iter().filter(|b| b[1] < cap[3]).collect();
                let under: Vec<_> = ink[1..].iter().filter(|b| b[1] > cap[3]).collect();
                assert!(!beside.is_empty() && !under.is_empty(), "{family}: {} beside, {} under", beside.len(), under.len());
                assert!(beside.iter().all(|b| b[0] > cap[2]), "{family}: a line beside the cap starts on it");
                let under_left = under.iter().map(|b| b[0]).min().unwrap();
                let under_right = under.iter().map(|b| b[2]).max().unwrap();
                assert!(under_left < cap[2], "{family}: the lines under the cap start at {under_left}, not under it");
                assert!(under_right as f32 > right - (cap[2] - cap[0]) as f32, "{family}: the lines under the cap end at {under_right}, short of the full width");
            }
        }
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
        #[allow(unused_mut)]
        let mut fx = klipp_timeline::LayerFx::default();
        let mut td = klipp_timeline::TitleData::single_text_fx(tb, fx);
        td.primary_object_mut().unwrap().fx_mut().loops.push(klipp_timeline::TitleLoop::ColorCycle { rate_hz: 1.0 });

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
            ..Default::default()
        };
        #[allow(unused_mut)]
        let mut fx = klipp_timeline::LayerFx { motion: klipp_timeline::TitleMotion::enter(klipp_timeline::Motion::new(klipp_timeline::MotionKind::Blur).duration(0.4)), ..Default::default() };
        let td = klipp_timeline::TitleData::single_text_fx(tb, fx);

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
        #[allow(unused_mut)]
        let mut fx = klipp_timeline::LayerFx::default();
        let td_full = {
            
            klipp_timeline::TitleData::single_text_fx(tb.clone(), fx.clone())
        };
        let td_half = {
            let mut td = klipp_timeline::TitleData::single_text_fx(tb.clone(), fx.clone());
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
            #[allow(unused_mut)]
            let mut fx = klipp_timeline::LayerFx::default();
            klipp_timeline::TitleData::single_text_fx(tb, fx)
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
            #[allow(unused_mut)]
            let mut fx = klipp_timeline::LayerFx::default();
            klipp_timeline::TitleData::single_text_fx(tb, fx)
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
            #[allow(unused_mut)]
            let mut fx = klipp_timeline::LayerFx::default();
            klipp_timeline::TitleData::single_text_fx(tb, fx)
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
            ..Default::default()
        };
        #[allow(unused_mut)]
        let mut fx = klipp_timeline::LayerFx {
            motion: klipp_timeline::TitleMotion::enter(
                klipp_timeline::Motion::new(klipp_timeline::MotionKind::Fade).duration(0.4).stagger(klipp_timeline::StaggerUnit::Char, 0.05),
            ),
            ..Default::default()
        };
        let td = klipp_timeline::TitleData::single_text_fx(tb, fx);

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
            ..Default::default()
        };
        #[allow(unused_mut)]
        let mut fx = klipp_timeline::LayerFx {
            motion: klipp_timeline::TitleMotion::enter(
                klipp_timeline::Motion::new(klipp_timeline::MotionKind::Rise).duration(0.4).stagger(klipp_timeline::StaggerUnit::Char, 0.1),
            ),
            ..Default::default()
        };
        let td = klipp_timeline::TitleData::single_text_fx(tb, fx);

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
            #[allow(unused_mut)]
            let mut fx = klipp_timeline::LayerFx::default();
            let mut td = klipp_timeline::TitleData::single_text_fx(tb, fx);
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
                    StyleRun { start: 0, end: 5, font_size: Some(100.0), color: None, font_name: None, font_style: None, tracking: None, baseline_offset: None, features: Vec::new(), underline: None, font_weight: None, font_axes: Vec::new(), ruby: None, script: None },
                    StyleRun { start: 6, end: 11, font_size: Some(50.0), color: None, font_name: None, font_style: None, tracking: None, baseline_offset: None, features: Vec::new(), underline: None, font_weight: None, font_axes: Vec::new(), ruby: None, script: None },
                ],
        ..Default::default()
    };
    #[allow(unused_mut)]
    let mut fx = klipp_timeline::LayerFx::default();
    
    klipp_timeline::TitleData::single_text_fx(tb, fx)
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
    #[allow(unused_mut)]
    let mut fx = klipp_timeline::LayerFx::default();
    
    klipp_timeline::TitleData::single_text_fx(tb, fx)
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
                    StyleRun { start: 0, end: 3, font_size: Some(100.0), color: None, font_name: None, font_style: None, tracking: None, baseline_offset: None, features: Vec::new(), underline: None, font_weight: None, font_axes: Vec::new(), ruby: None, script: None }, // ABC
                    StyleRun { start: 3, end: 7, font_size: Some(50.0), color: None, font_name: None, font_style: None, tracking: None, baseline_offset: None, features: Vec::new(), underline: None, font_weight: None, font_axes: Vec::new(), ruby: None, script: None },  // DEFG
                    StyleRun { start: 7, end: 10, font_size: Some(72.0), color: None, font_name: None, font_style: None, tracking: None, baseline_offset: None, features: Vec::new(), underline: None, font_weight: None, font_axes: Vec::new(), ruby: None, script: None }, // HIJ
                ],
        ..Default::default()
    };
    #[allow(unused_mut)]
    let mut fx = klipp_timeline::LayerFx::default();
    
    klipp_timeline::TitleData::single_text_fx(tb, fx)
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
        #[allow(unused_mut)]
        let mut fx = klipp_timeline::LayerFx::default();
        let mut td = klipp_timeline::TitleData::single_text_fx(tb, fx);
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
                    handles: Default::default(),
                },
                Keyframe {
                    time_secs: 2.0,
                    value: 0.9,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0),
                    tangent_out: (0.0, 0.0),
                    handles: Default::default(),
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
        #[allow(unused_mut)]
        let mut fx = klipp_timeline::LayerFx::default();
        let td = klipp_timeline::TitleData::single_text_fx(tb, fx);

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

        let tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 80.0,
            ..Default::default()
        };
        #[allow(unused_mut)]
        let mut fx = klipp_timeline::LayerFx::default();
        fx.glows.push(klipp_timeline::TextGlow {
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
                    handles: Default::default(),
                },
                Keyframe {
                    time_secs: 2.0,
                    value: 24.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0),
                    tangent_out: (0.0, 0.0),
                    handles: Default::default(),
                },
            ],
            intensity: 1.0,
            intensity_keyframes: Vec::new(),
        });
        let td = klipp_timeline::TitleData::single_text_fx(tb, fx);

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

        let tb = klipp_timeline::TextBox {
            text: "O".to_string(),
            font_size: 80.0,
            ..Default::default()
        };
        #[allow(unused_mut)]
        let mut fx = klipp_timeline::LayerFx::default();
        fx.glows.push(klipp_timeline::TextGlow {
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
                    handles: Default::default(),
                },
                Keyframe {
                    time_secs: 1.0,
                    value: 1.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0),
                    tangent_out: (0.0, 0.0),
                    handles: Default::default(),
                },
            ],
        });
        let td = klipp_timeline::TitleData::single_text_fx(tb, fx);

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
            ..Default::default()
        };
        #[allow(unused_mut)]
        let mut fx = klipp_timeline::LayerFx { motion: klipp_timeline::TitleMotion::enter(klipp_timeline::Motion::new(klipp_timeline::MotionKind::Fade).duration(1.0)), loops: vec![klipp_timeline::TitleLoop::Wave { amplitude: 0.3, rate_hz: 2.0 }], ..Default::default() };
        let td = klipp_timeline::TitleData::single_text_fx(tb, fx);
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
            ..Default::default()
        };
        #[allow(unused_mut)]
        let mut fx = klipp_timeline::LayerFx { loops: vec![klipp_timeline::TitleLoop::Blink { rate_hz: 1.0, duty: 0.5 }], ..Default::default() };
        let td = klipp_timeline::TitleData::single_text_fx(tb, fx);
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
            ..Default::default()
        };
        #[allow(unused_mut)]
        let mut fx = klipp_timeline::LayerFx { loops: vec![klipp_timeline::TitleLoop::Scroll { direction: klipp_timeline::ScrollDirection::Up, speed_px_per_sec: 0.0, shape: Default::default() }], ..Default::default() };
        let td = klipp_timeline::TitleData::single_text_fx(tb, fx);
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

        let tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 80.0,
            ..Default::default()
        };
        #[allow(unused_mut)]
        let mut fx = klipp_timeline::LayerFx::default();
        fx.glows.push(klipp_timeline::TextGlow {
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
                    handles: Default::default(),
                },
                Keyframe {
                    time_secs: 2.0,
                    value: 24.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0),
                    tangent_out: (0.0, 0.0),
                    handles: Default::default(),
                },
            ],
            intensity: 1.0,
            intensity_keyframes: Vec::new(),
        });
        let td = klipp_timeline::TitleData::single_text_fx(tb, fx);

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

        let tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 80.0,
            ..Default::default()
        };
        #[allow(unused_mut)]
        let mut fx = klipp_timeline::LayerFx::default();
        fx.glows.push(klipp_timeline::TextGlow {
            enabled: true,
            color: [255, 200, 0, 255],
            radius: 12.0,
            radius_keyframes: Vec::new(),
            intensity: 0.8,
            intensity_keyframes: Vec::new(),
        });
        let td = klipp_timeline::TitleData::single_text_fx(tb, fx);

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

        let tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 80.0,
            ..Default::default()
        };
        #[allow(unused_mut)]
        let mut fx = klipp_timeline::LayerFx::default();
        fx.shadows.push(klipp_timeline::TitleShadow {
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
                    handles: Default::default(),
                },
                Keyframe {
                    time_secs: 2.0, value: 20.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                    handles: Default::default(),
                },
            ],
        });
        let td = klipp_timeline::TitleData::single_text_fx(tb, fx);

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

        let tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 100.0,
            color: [255, 255, 255, 255],
            ..Default::default()
        };
        #[allow(unused_mut)]
        let mut fx = klipp_timeline::LayerFx::default();
        fx.shadows.push(klipp_timeline::TitleShadow {
            enabled: true,
            color: [0, 0, 0, 255],
            offset_x: 0.0,
            offset_x_keyframes: vec![
                Keyframe {
                    time_secs: 0.0, value: -40.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                    handles: Default::default(),
                },
                Keyframe {
                    time_secs: 2.0, value: 40.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                    handles: Default::default(),
                },
            ],
            offset_y: 0.0,
            offset_y_keyframes: Vec::new(),
            blur: 0.0,
            blur_keyframes: Vec::new(),
        });
        let td = klipp_timeline::TitleData::single_text_fx(tb, fx);

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

        let tb = klipp_timeline::TextBox {
            text: "O".to_string(),
            font_size: 100.0,
            color: [255, 255, 255, 255],
            ..Default::default()
        };
        #[allow(unused_mut)]
        let mut fx = klipp_timeline::LayerFx::default();
        fx.inner_shadows.push(klipp_timeline::InnerShadow {
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
                    handles: Default::default(),
                },
                Keyframe {
                    time_secs: 2.0, value: 18.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                    handles: Default::default(),
                },
            ],
        });
        let td = klipp_timeline::TitleData::single_text_fx(tb, fx);

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

        let tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 80.0,
            ..Default::default()
        };
        #[allow(unused_mut)]
        let mut fx = klipp_timeline::LayerFx::default();
        fx.shadows.push(klipp_timeline::TitleShadow {
            enabled: true,
            color: [0, 0, 0, 200],
            offset_x: 4.0,
            offset_x_keyframes: Vec::new(),
            offset_y: 4.0,
            offset_y_keyframes: Vec::new(),
            blur: 6.0,
            blur_keyframes: Vec::new(),
        });
        let td = klipp_timeline::TitleData::single_text_fx(tb, fx);

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

        let tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 80.0,
            color: [255, 255, 255, 255],
            ..Default::default()
        };
        #[allow(unused_mut)]
        let mut fx = klipp_timeline::LayerFx::default();
        fx.outlines.push(klipp_timeline::TextOutline {
            enabled: true,
            color: [255, 0, 0, 255],
            width: 1.0,
            width_keyframes: vec![
                Keyframe {
                    time_secs: 0.0, value: 1.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                    handles: Default::default(),
                },
                Keyframe {
                    time_secs: 2.0, value: 8.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                    handles: Default::default(),
                },
            ],
        });
        let td = klipp_timeline::TitleData::single_text_fx(tb, fx);

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

        let tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 80.0,
            color: [255, 255, 255, 255],
            ..Default::default()
        };
        #[allow(unused_mut)]
        let mut fx = klipp_timeline::LayerFx::default();
        fx.outlines.push(klipp_timeline::TextOutline {
            enabled: true,
            color: [0, 100, 255, 255],
            width: 4.0,
            width_keyframes: Vec::new(),
        });
        // outlines[1]: green, keyframed 1→10 px (rendered behind
        // outlines[0] because the renderer draws widest-first).
        fx.outlines.push(klipp_timeline::TextOutline {
            enabled: true,
            color: [0, 255, 0, 255],
            width: 1.0,
            width_keyframes: vec![
                Keyframe {
                    time_secs: 0.0, value: 1.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                    handles: Default::default(),
                },
                Keyframe {
                    time_secs: 2.0, value: 10.0,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                    handles: Default::default(),
                },
            ],
        });
        let td = klipp_timeline::TitleData::single_text_fx(tb, fx);

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

        let tb = klipp_timeline::TextBox {
            text: "G".to_string(),
            font_size: 100.0,
            color: [255, 255, 255, 255],
            ..Default::default()
        };
        #[allow(unused_mut)]
        let mut fx = klipp_timeline::LayerFx::default();
        fx.glows.push(klipp_timeline::TextGlow {
            enabled: true,
            color: [255, 0, 0, 255],
            radius: 4.0,
            radius_keyframes: Vec::new(),
            intensity: 0.8,
            intensity_keyframes: Vec::new(),
        });
        // Secondary glow (green, much wider so it spreads further).
        fx.glows.push(klipp_timeline::TextGlow {
            enabled: true,
            color: [0, 255, 0, 255],
            radius: 30.0,
            radius_keyframes: Vec::new(),
            intensity: 1.0,
            intensity_keyframes: Vec::new(),
        });
        let td_on = klipp_timeline::TitleData::single_text_fx(tb.clone(), fx.clone());
        fx.glow_stack_enabled = false;
        let td_off = klipp_timeline::TitleData::single_text_fx(tb, fx);

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

        let tb = klipp_timeline::TextBox {
            text: "O".to_string(),
            font_size: 100.0,
            color: [255, 255, 255, 255],
            ..Default::default()
        };
        #[allow(unused_mut)]
        let mut fx = klipp_timeline::LayerFx::default();
        fx.inner_shadows.push(klipp_timeline::InnerShadow {
            enabled: true,
            color: [255, 0, 0, 220],
            offset_x: 2.0, offset_x_keyframes: Vec::new(),
            offset_y: 2.0, offset_y_keyframes: Vec::new(),
            blur: 0.0, blur_keyframes: Vec::new(),
        });
        // Secondary inner shadow (green, much bigger).
        fx.inner_shadows.push(klipp_timeline::InnerShadow {
            enabled: true,
            color: [0, 255, 0, 220],
            offset_x: 10.0, offset_x_keyframes: Vec::new(),
            offset_y: 10.0, offset_y_keyframes: Vec::new(),
            blur: 0.0, blur_keyframes: Vec::new(),
        });
        let td_on = klipp_timeline::TitleData::single_text_fx(tb.clone(), fx.clone());
        fx.inner_shadow_stack_enabled = false;
        let td_off = klipp_timeline::TitleData::single_text_fx(tb, fx);

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

        let tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 80.0,
            color: [255, 255, 255, 255],
            ..Default::default()
        };
        #[allow(unused_mut)]
        let mut fx = klipp_timeline::LayerFx::default();
        fx.shadows.push(klipp_timeline::TitleShadow {
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
        fx.shadows.push(klipp_timeline::TitleShadow {
            enabled: true,
            color: [0, 255, 0, 220],
            offset_x: 30.0,
            offset_x_keyframes: Vec::new(),
            offset_y: 30.0,
            offset_y_keyframes: Vec::new(),
            blur: 0.0,
            blur_keyframes: Vec::new(),
        });
        let td_on = klipp_timeline::TitleData::single_text_fx(tb.clone(), fx.clone());
        fx.shadow_stack_enabled = false;
        let td_off = klipp_timeline::TitleData::single_text_fx(tb, fx);

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

        let tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 80.0,
            color: [255, 255, 255, 255],
            ..Default::default()
        };
        #[allow(unused_mut)]
        let mut fx = klipp_timeline::LayerFx::default();
        fx.outlines.push(klipp_timeline::TextOutline {
            enabled: true,
            color: [0, 100, 255, 255],
            width: 4.0,
            width_keyframes: Vec::new(),
        });
        fx.outlines.push(klipp_timeline::TextOutline {
            enabled: true,
            color: [0, 255, 0, 255],
            width: 8.0,
            width_keyframes: Vec::new(),
        });
        let td_on = klipp_timeline::TitleData::single_text_fx(tb.clone(), fx.clone());
        fx.outline_stack_enabled = false;
        let td_off = klipp_timeline::TitleData::single_text_fx(tb, fx);

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

        let tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 80.0,
            ..Default::default()
        };
        #[allow(unused_mut)]
        let mut fx = klipp_timeline::LayerFx::default();
        fx.outlines.push(klipp_timeline::TextOutline {
            enabled: true,
            color: [255, 0, 0, 255],
            width: 3.0,
            width_keyframes: Vec::new(),
        });
        let td = klipp_timeline::TitleData::single_text_fx(tb, fx);

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
                        handles: Default::default(),
                    },
                    Keyframe {
                        time_secs: 2.0, value: 30.0,
                        interpolation: Interpolation::Linear,
                        tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                        handles: Default::default(),
                    },
                ],
                angle: 135.0, angle_keyframes: Vec::new(),
                lighting: 0.5, lighting_keyframes: Vec::new(),
            }),
            ..Default::default()
        };
        #[allow(unused_mut)]
        let mut fx = klipp_timeline::LayerFx::default();
        let td = klipp_timeline::TitleData::single_text_fx(tb, fx);

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
        #[allow(unused_mut)]
        let mut fx = klipp_timeline::LayerFx::default();
        let td = klipp_timeline::TitleData::single_text_fx(tb, fx);

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

        let tb = klipp_timeline::TextBox {
            text: "X".to_string(),
            font_size: 80.0,
            ..Default::default()
        };
        #[allow(unused_mut)]
        let mut fx = klipp_timeline::LayerFx::default();
        fx.inner_shadows.push(klipp_timeline::InnerShadow {
            enabled: true,
            color: [0, 0, 0, 200],
            offset_x: 2.0, offset_x_keyframes: Vec::new(),
            offset_y: 2.0, offset_y_keyframes: Vec::new(),
            blur: 4.0, blur_keyframes: Vec::new(),
        });
        let td = klipp_timeline::TitleData::single_text_fx(tb, fx);

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
        #[allow(unused_mut)]
        let mut fx = klipp_timeline::LayerFx::default();
        klipp_timeline::TitleData::single_text_fx(tb, fx)
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

    /// #1506: the last line and the vertical alignment move glyphs too.
    #[test]
    fn glyph_hash_changes_on_last_line_and_vertical_align() {
        let baseline = baseline_glyph_hash();
        let last = modify_baseline_and_glyph_hash(|td| {
            glyph_hash_modify_text_box(td, |tb| tb.last_line = klipp_timeline::LastLineAlign::Justify)
        });
        let vertical = modify_baseline_and_glyph_hash(|td| {
            glyph_hash_modify_text_box(td, |tb| tb.vertical_align = klipp_timeline::TextVerticalAlign::Top)
        });
        assert_ne!(baseline, last, "the last line must invalidate cache");
        assert_ne!(baseline, vertical, "vertical alignment must invalidate cache");
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
                    features: Vec::new(),
                    underline: None,
                    font_weight: None,
                    font_axes: Vec::new(),
                    ruby: None,
                    script: None,
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

#[cfg(test)]
mod solid_point_tests {
    /// #787: two bars with a gap between them, as two lines of a word: the
    /// solid point is inside the thicker bar, not in the gap at the box's
    /// centre; an empty box has none.
    #[test]
    fn the_solid_point_is_in_a_stroke_not_the_gap() {
        let (w, h) = (200usize, 100usize);
        let mut rgba = vec![0u8; w * h * 4];
        let mut fill = |y0: usize, y1: usize| {
            for y in y0..y1 {
                for x in 10..190 {
                    rgba[(y * w + x) * 4 + 3] = 255;
                }
            }
        };
        fill(10, 30);
        fill(55, 95);
        let frame = (w as u32, h as u32);
        let raster = super::BoxRaster::from_bytes(&rgba, frame, (10, 95), (10, 190));
        let (x, y) = super::solid_point(&raster, (10, 190), (10, 95)).unwrap();
        assert!((55.0..95.0).contains(&y) && (10.0..190.0).contains(&x), "({x}, {y}) is not in the thick bar");
        let centre = super::resolve_anchor(None, &raster, (10, 190), (10, 95));
        assert!((30.0..55.0).contains(&centre.1), "the box's centre is the gap: {centre:?}");
        let empty = super::BoxRaster::from_bytes(&vec![0u8; w * h * 4], frame, (10, 95), (10, 190));
        assert_eq!(super::solid_point(&empty, (10, 190), (10, 95)), None);
    }
}

#[cfg(test)]
mod box_alignment_tests {
    //! #1506: justified text and text set up and down its box.
    use super::*;
    use klipp_timeline::{LastLineAlign, TextBox, TextVerticalAlign, TitlePosition};

    const W: u32 = 1920;
    const H: u32 = 1080;
    /// Two paragraphs, each long enough to wrap in a half-frame box; the
    /// first one's last word is the only capital Z.
    const TWO_PARAGRAPHS: &str = "the quick brown fox jumps over the lazy dog and runs far into the woods by the river Zed\nsome more words follow here so this second paragraph wraps onto a further line of its own end";

    fn boxed(text: &str, alignment: TextAlignment, size: (f32, f32), adjust: impl FnOnce(&mut TextBox)) -> TitleData {
        let mut tb = TextBox { text: text.to_string(), font_name: "sans-serif".into(), font_size: 48.0, alignment, ..Default::default() };
        adjust(&mut tb);
        let mut td = TitleData::single_text_fx(tb, LayerFx::default());
        let o = td.objects.first_mut().unwrap();
        o.transform.center = TitlePosition { x: 0.5, y: 0.5 };
        o.transform.size = TitlePosition { x: size.0, y: size.1 };
        td
    }

    /// The rendered lines, each as its letters' frame boxes and
    /// characters, in reading order: a new line starts where a letter
    /// sits left of the one before it.
    fn lines(layout: &TitleLayout) -> Vec<Vec<([i32; 4], char)>> {
        let mut out: Vec<Vec<([i32; 4], char)>> = Vec::new();
        let mut last_x = i32::MIN;
        for (b, c) in layout.glyph_boxes.iter().zip(&layout.glyph_parents) {
            if c.is_whitespace() || b[2] == b[0] {
                continue;
            }
            if b[0] < last_x || out.is_empty() {
                out.push(Vec::new());
            }
            last_x = b[0];
            out.last_mut().unwrap().push((*b, *c));
        }
        out
    }

    fn right(line: &[([i32; 4], char)]) -> i32 {
        line.iter().map(|(b, _)| b[2]).max().unwrap()
    }

    fn left(line: &[([i32; 4], char)]) -> i32 {
        line.iter().map(|(b, _)| b[0]).min().unwrap()
    }

    /// The space between each pair of words on a line, in pixels.
    fn word_gaps(layout: &TitleLayout, line_no: usize) -> Vec<i32> {
        // Words are the runs between whitespace glyphs, on the line.
        let mut gaps = Vec::new();
        let (mut seen_line, mut prev_right, mut after_space, mut last_x) = (0usize, None::<i32>, false, i32::MIN);
        let mut line = 0usize;
        for (b, c) in layout.glyph_boxes.iter().zip(&layout.glyph_parents) {
            if c.is_whitespace() || b[2] == b[0] {
                after_space = true;
                continue;
            }
            if b[0] < last_x {
                line += 1;
                prev_right = None;
            }
            last_x = b[0];
            if line == line_no {
                seen_line += 1;
                if let (true, Some(r)) = (after_space, prev_right) {
                    gaps.push(b[0] - r);
                }
                prev_right = Some(b[2]);
            }
            after_space = false;
        }
        assert!(seen_line > 0, "no line {line_no}");
        gaps
    }

    /// Justified lines but each paragraph's last fill the box's width,
    /// by gaps wider than the same line set from the left; the
    /// paragraphs' last lines stay set from the left.
    #[test]
    fn justified_lines_fill_the_box() {
        let mut r = TitleRenderer::new();
        let justified = r.render_layout(&boxed(TWO_PARAGRAPHS, TextAlignment::Justify, (0.5, 0.9), |_| {}), W, H);
        let flush = r.render_layout(&boxed(TWO_PARAGRAPHS, TextAlignment::Left, (0.5, 0.9), |_| {}), W, H);
        let (box_left, box_right) = (480, 1440);
        let ls = lines(&justified);
        assert!(ls.len() >= 4, "the two paragraphs should wrap to four lines or more, got {}", ls.len());
        assert_eq!(ls.len(), lines(&flush).len(), "justifying moves no word to another line");
        let ends: Vec<bool> = ls.iter().enumerate().map(|(i, l)| i + 1 == ls.len() || l.iter().any(|(_, c)| *c == 'Z')).collect();
        assert_eq!(ends.iter().filter(|e| **e).count(), 2, "one line ends each paragraph");
        for (i, line) in ls.iter().enumerate() {
            assert!((left(line) - box_left).abs() <= 4, "line {i} starts at {} not the box's left {box_left}", left(line));
            if ends[i] {
                assert!(right(line) < box_right - 40, "paragraph's last line {i} was spread to {}", right(line));
                continue;
            }
            assert!((right(line) - box_right).abs() <= 1, "line {i} ends at {} not the box's right {box_right}", right(line));
            let (wide, narrow) = (word_gaps(&justified, i), word_gaps(&flush, i));
            assert_eq!(wide.len(), narrow.len(), "line {i} keeps its words");
            assert!(wide.iter().zip(&narrow).all(|(w, n)| w > n), "line {i}'s gaps did not widen: {wide:?} vs {narrow:?}");
        }
    }

    /// The last line follows its choice: centred in the box, or spread
    /// across it like the lines above.
    #[test]
    fn the_last_line_follows_its_choice() {
        let mut r = TitleRenderer::new();
        let mut last = |choice| {
            let layout = r.render_layout(&boxed(TWO_PARAGRAPHS, TextAlignment::Justify, (0.5, 0.9), |tb| tb.last_line = choice), W, H);
            let ls = lines(&layout);
            let line = ls.last().unwrap().clone();
            (left(&line), right(&line))
        };
        let (l, rt) = last(LastLineAlign::Center);
        assert!(((l + rt) / 2 - 960).abs() <= 2, "a centred last line spans {l}..{rt}, not about the box's centre 960");
        let (l, rt) = last(LastLineAlign::Justify);
        assert!((l - 480).abs() <= 4 && (rt - 1440).abs() <= 1, "a justified last line spans {l}..{rt}, not the box 480..1440");
    }

    /// The cursor follows the justified letters, as it follows every
    /// other alignment: each character sits where its glyph was drawn.
    #[test]
    fn the_cursor_follows_justified_letters() {
        let mut r = TitleRenderer::new();
        let td = boxed(TWO_PARAGRAPHS, TextAlignment::Justify, (0.5, 0.9), |_| {});
        let layout = r.render_layout(&td, W, H);
        let positions = r.compute_char_positions(&td, W, H);
        let drawn: Vec<i32> = layout.glyph_boxes.iter().zip(&layout.glyph_parents).filter(|(b, c)| !c.is_whitespace() && b[2] > b[0]).map(|(b, _)| b[0]).collect();
        let cursor: Vec<f32> = positions.iter().filter(|p| TWO_PARAGRAPHS[p.byte_offset..].chars().next().is_some_and(|c| !c.is_whitespace())).map(|p| p.x).collect();
        assert_eq!(drawn.len(), cursor.len());
        for (d, c) in drawn.iter().zip(&cursor) {
            assert!((*d as f32 - c).abs() <= 6.0, "a letter drawn at {d} has its cursor at {c}");
        }
    }

    /// Top, middle and bottom put the text's block at the top of its
    /// box, about its centre and at its bottom, in the drawn pixels and
    /// in the layout the cursor reads.
    #[test]
    fn text_sits_top_middle_or_bottom_of_its_box() {
        let mut r = TitleRenderer::new();
        // A 0.6-high box about the frame's centre: 216..864 of 1080.
        let (top, bottom) = (216.0f32, 864.0f32);
        let mut at = |v| {
            let td = boxed("Hello\nworld", TextAlignment::Center, (0.5, 0.6), |tb| tb.vertical_align = v);
            let layout = r.render_layout(&td, W, H);
            let buf = r.render(&td, W, H);
            let rows: Vec<usize> = (0..H as usize).filter(|y| (0..W as usize).any(|x| buf[(y * W as usize + x) * 4 + 3] > 128)).collect();
            let cursor_y = r.compute_char_positions(&td, W, H)[0].y;
            (layout.text_top, layout.text_bottom, *rows.first().unwrap() as f32, *rows.last().unwrap() as f32, cursor_y)
        };
        let (t, b, first, _, top_cursor) = at(TextVerticalAlign::Top);
        assert!((t - top).abs() <= 1.0 && (first - top).abs() <= 2.0, "top: text {t}..{b}, pixels from {first}");
        let (t, b, first, last, middle_cursor) = at(TextVerticalAlign::Middle);
        assert!(((t + b) / 2.0 - 540.0).abs() <= 1.0, "middle: text {t}..{b}");
        assert!(((first + last) / 2.0 - 540.0).abs() <= 2.0, "middle: pixels {first}..{last}");
        let text_h = b - t;
        let (t, b, _, last, bottom_cursor) = at(TextVerticalAlign::Bottom);
        assert!((b - bottom).abs() <= 1.0 && (last - bottom).abs() <= 2.0, "bottom: text {t}..{b}, pixels to {last}");
        // The cursor moves with the text.
        let half = (bottom - top - text_h) / 2.0;
        assert!((middle_cursor - top_cursor - half).abs() <= 1.0, "cursor {top_cursor} -> {middle_cursor}, text moved {half}");
        assert!((bottom_cursor - middle_cursor - half).abs() <= 1.0, "cursor {middle_cursor} -> {bottom_cursor}, text moved {half}");
    }

    fn fnv(bytes: impl IntoIterator<Item = u8>) -> u64 {
        bytes.into_iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
    }

    /// A text box saved before #1506 has neither field, loads at Middle
    /// and a left last line, saves as it did, and draws and lays out
    /// byte for byte as it did before them: the hashes were taken with
    /// the renderer as it stood without them.
    #[test]
    fn a_box_saved_before_reads_and_draws_as_before() {
        let mut r = TitleRenderer::new();
        let mut hashes = Vec::new();
        for (alignment, size, spacing) in [
            (TextAlignment::Left, (0.5, 0.9), 1.0),
            (TextAlignment::Center, (1.0, 1.0), 1.0),
            (TextAlignment::Right, (0.4, 0.8), 1.3),
        ] {
            let td = boxed(TWO_PARAGRAPHS, alignment, size, |tb| tb.line_spacing = spacing);
            let json = serde_json::to_string(&td).unwrap();
            assert!(!json.contains("last_line") && !json.contains("vertical_align"), "a default box writes neither field");
            let read: TitleData = serde_json::from_str(&json).unwrap();
            assert_eq!(read, td);
            let tb = read.primary_text().unwrap();
            assert_eq!((tb.vertical_align, tb.last_line), (TextVerticalAlign::Middle, LastLineAlign::Left));
            let pixels = fnv(r.render(&read, 640, 360));
            let cursor = fnv(r.compute_char_positions(&read, 640, 360).iter().flat_map(|p| [p.x.to_bits(), p.y.to_bits()]).flat_map(u32::to_le_bytes));
            hashes.push((pixels, cursor));
        }
        // Taken on main at f26cf5792, before #1506; the third, at a
        // line spacing of 1.3, again when the layout took over line
        // spacing (#1676).
        let before = [
            (2089990751451958423, 15491691981405734563),
            (16810429097172737439, 12119957885803439428),
            (14579633377248752159, 17912027904950030411),
        ];
        assert_eq!(hashes, before, "a box saved before #1506 draws or lays out differently");
    }
}

#[cfg(test)]
mod style_run_tests {
    //! #1505: a style run's font, style, tracking and baseline offset.
    use super::*;
    use klipp_timeline::{TextBox, TitlePosition};

    const W: u32 = 1920;
    const H: u32 = 1080;
    const TWO_PARAGRAPHS: &str = "the quick brown fox jumps over the lazy dog and runs far into the woods by the river Zed\nsome more words follow here so this second paragraph wraps onto a further line of its own end";

    fn boxed(text: &str, alignment: TextAlignment, adjust: impl FnOnce(&mut TextBox)) -> TitleData {
        let mut tb = TextBox { text: text.to_string(), font_name: "sans-serif".into(), font_size: 48.0, alignment, ..Default::default() };
        adjust(&mut tb);
        let mut td = TitleData::single_text_fx(tb, LayerFx::default());
        let o = td.objects.first_mut().unwrap();
        o.transform.center = TitlePosition { x: 0.5, y: 0.5 };
        o.transform.size = TitlePosition { x: 0.6, y: 0.9 };
        td
    }

    fn fnv(bytes: impl IntoIterator<Item = u8>) -> u64 {
        bytes.into_iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
    }

    /// "the quick brown fox jumps" on one line of an Inter box, from the
    /// left: "brown" is bytes 10..15, "fox" 16..19, "jumps" 20..25.
    const SENTENCE: &str = "the quick brown fox jumps";

    fn sentence(runs: Vec<StyleRun>, adjust: impl FnOnce(&mut TextBox)) -> TitleData {
        boxed(SENTENCE, TextAlignment::Left, |tb| {
            tb.font_name = "Inter".into();
            tb.style_runs = runs;
            adjust(tb);
        })
    }

    fn run(start: usize, end: usize, adjust: impl FnOnce(&mut StyleRun)) -> StyleRun {
        let mut run = StyleRun::default_for_range(start, end);
        adjust(&mut run);
        run
    }

    /// A bundled face, as the renderer loads it.
    fn face(family: &str, style: &str) -> fontdue::Font {
        fontdue::Font::from_bytes(bundled::lookup(family, style).unwrap(), fontdue::FontSettings::default()).unwrap()
    }

    /// The mask's pixels inside glyph `i`'s box.
    fn ink(layout: &TitleLayout, i: usize) -> Vec<u8> {
        let b = layout.glyph_boxes[i];
        let (x0, y0) = (layout.mask_x0 as i32, layout.mask_y0 as i32);
        let mut out = Vec::new();
        for y in b[1]..b[3] {
            for x in b[0]..b[2] {
                out.push(layout.alpha_mask[((y - y0) * layout.mask_w as i32 + (x - x0)) as usize]);
            }
        }
        out
    }

    /// Glyph `i` was drawn as `font` draws its character at `px`: its
    /// box is that bitmap's size and its pixels hold that bitmap (a
    /// kerned neighbour's ink may reach into the box, #1501).
    fn drawn_in(layout: &TitleLayout, i: usize, font: &fontdue::Font, px: f32) -> bool {
        let (m, bitmap) = font.rasterize(layout.glyph_parents[i], px);
        let b = layout.glyph_boxes[i];
        (b[2] - b[0], b[3] - b[1]) == (m.width as i32, m.height as i32) && ink(layout, i).iter().zip(&bitmap).all(|(i, c)| i >= c)
    }

    /// A word set in another family and style, and one in the box's
    /// family but bold, draw in those faces; the words around them
    /// draw as they did, the words before them where they were.
    #[test]
    fn a_word_in_another_face_draws_in_it() {
        let mut r = TitleRenderer::new();
        let runs = vec![
            run(10, 15, |r| { r.font_name = Some("Libre Baskerville".into()); r.font_style = Some(FontStyle::Italic) }),
            run(20, 25, |r| r.font_style = Some(FontStyle::Bold)),
        ];
        let styled = r.render_layout(&sentence(runs, |_| {}), W, H);
        let plain = r.render_layout(&sentence(Vec::new(), |_| {}), W, H);
        assert_eq!(styled.glyph_count, SENTENCE.len());
        let (italic, bold, regular) = (face("Libre Baskerville", "Italic"), face("Inter", "Bold"), face("Inter", "Regular"));
        for i in 10..15 {
            assert!(drawn_in(&styled, i, &italic, 48.0), "{:?} is not in Libre Baskerville Italic", styled.glyph_parents[i]);
            assert!(!drawn_in(&styled, i, &regular, 48.0));
        }
        for i in 20..25 {
            assert!(drawn_in(&styled, i, &bold, 48.0), "{:?} is not in Inter Bold", styled.glyph_parents[i]);
            assert!(!drawn_in(&styled, i, &regular, 48.0));
        }
        let dy = styled.glyph_boxes[0][1] - plain.glyph_boxes[0][1];
        let fox_dx = styled.glyph_boxes[16][0] - plain.glyph_boxes[16][0];
        for i in (0..10).chain(16..19).filter(|&i| SENTENCE.as_bytes()[i] != b' ') {
            assert!(drawn_in(&styled, i, &regular, 48.0), "{:?} is not in the box's Inter", styled.glyph_parents[i]);
            assert_eq!(ink(&styled, i), ink(&plain, i));
            let (s, p) = (styled.glyph_boxes[i], plain.glyph_boxes[i]);
            let dx = if i < 10 { 0 } else { fox_dx };
            assert_eq!(s, [p[0] + dx, p[1] + dy, p[2] + dx, p[3] + dy], "{:?} moved within its line", styled.glyph_parents[i]);
        }
    }

    /// A run's tracking widens the advance after each of its letters by
    /// that much, in place of the box's own, and the rest of the line
    /// moves along by the run's widening; the cursor moves with them.
    #[test]
    fn tracking_widens_its_run() {
        let mut r = TitleRenderer::new();
        for (box_tracking, run_tracking) in [(0.0f32, 10.0f32), (4.0, 10.0), (6.0, -2.0)] {
            let tracked = sentence(vec![run(10, 15, |r| r.tracking = Some(run_tracking))], |tb| tb.tracking = box_tracking);
            let plain = sentence(Vec::new(), |tb| tb.tracking = box_tracking);
            let (t, p) = (r.render_layout(&tracked, W, H), r.render_layout(&plain, W, H));
            let (tc, pc) = (r.compute_char_positions(&tracked, W, H), r.compute_char_positions(&plain, W, H));
            let step = run_tracking - box_tracking;
            for i in 0..SENTENCE.len() {
                // Letters of the run move by `step` per letter before
                // them in it; everything after, by all five.
                let expect = step * i.saturating_sub(10).min(5) as f32;
                let moved = (t.glyph_boxes[i][0] - p.glyph_boxes[i][0]) as f32;
                assert!((moved - expect).abs() <= 1.0, "{box_tracking}/{run_tracking}: glyph {i} moved {moved}, not {expect}");
                assert_eq!(t.glyph_boxes[i][1], p.glyph_boxes[i][1]);
                let cursor = tc[i].x - pc[i].x;
                assert!((cursor - expect).abs() <= 0.01, "{box_tracking}/{run_tracking}: cursor {i} moved {cursor}, not {expect}");
            }
        }
    }

    /// A run's baseline offset raises its letters' ink by that many
    /// pixels against the rest of the line (a negative one lowers it),
    /// and the cursor rises with them.
    #[test]
    fn baseline_offset_raises_its_run() {
        let mut r = TitleRenderer::new();
        let regular = face("Inter", "Regular");
        for offset in [12.0f32, -9.0] {
            let raised = sentence(vec![run(10, 15, |r| r.baseline_offset = Some(offset))], |_| {});
            let plain = sentence(Vec::new(), |_| {});
            let (t, p) = (r.render_layout(&raised, W, H), r.render_layout(&plain, W, H));
            let (tc, pc) = (r.compute_char_positions(&raised, W, H), r.compute_char_positions(&plain, W, H));
            for i in 0..SENTENCE.len() {
                let expect = if (10..15).contains(&i) { -offset } else { 0.0 };
                let rel = |l: &TitleLayout| (l.glyph_boxes[i][1] - l.glyph_boxes[0][1]) as f32;
                assert!((rel(&t) - rel(&p) - expect).abs() <= 1.0, "{offset}: glyph {i} moved {} against the line, not {expect}", rel(&t) - rel(&p));
                if SENTENCE.as_bytes()[i] != b' ' {
                    assert!(drawn_in(&t, i, &regular, 48.0), "{offset}: glyph {i}'s ink is not in its box");
                }
                let cursor = (tc[i].y - tc[0].y) - (pc[i].y - pc[0].y);
                assert!((cursor - expect).abs() <= 0.01, "{offset}: cursor {i} moved {cursor}, not {expect}");
            }
        }
    }

    /// The cursor sits on each letter where it is drawn when runs set
    /// faces, tracking and baseline offsets, as it does without them.
    #[test]
    fn the_cursor_follows_styled_runs() {
        let mut r = TitleRenderer::new();
        let td = sentence(vec![
            run(4, 9, |r| { r.font_name = Some("JetBrains Mono".into()); r.tracking = Some(8.0) }),
            run(10, 15, |r| { r.font_name = Some("Libre Baskerville".into()); r.font_style = Some(FontStyle::BoldItalic); r.baseline_offset = Some(10.0) }),
            run(20, 25, |r| { r.font_size = Some(72.0); r.font_style = Some(FontStyle::Bold); r.tracking = Some(-3.0) }),
        ], |_| {});
        let layout = r.render_layout(&td, W, H);
        let positions = r.compute_char_positions(&td, W, H);
        for (i, b) in layout.glyph_boxes.iter().enumerate() {
            let p = &positions[i];
            assert_eq!(p.byte_offset, i);
            assert!((p.x - b[0] as f32).abs() <= 1.0 && (p.y - b[1] as f32).abs() <= 1.0, "{:?} drawn at {b:?}, cursor at {}, {}", layout.glyph_parents[i], p.x, p.y);
        }
    }

    /// Columns keep their columns with a run in them, and the run draws
    /// in its face there; the first column's letters sit where they sat.
    #[test]
    fn columns_set_a_run_in_its_face() {
        const TEXT: &str = "one two three four five six seven eight nine ten eleven twelve";
        let run_at = TEXT.find("eleven").unwrap();
        let mut r = TitleRenderer::new();
        // "fi" is not joined (#1501), so glyph i is letter i.
        let columns = |runs: Vec<StyleRun>| boxed(TEXT, TextAlignment::Center, |tb| { tb.columns = 2; tb.style_runs = runs; tb.features = vec!["-liga".into()] });
        let styled = r.render_layout(&columns(vec![run(run_at, run_at + 6, |r| { r.font_name = Some("Libre Baskerville".into()); r.font_style = Some(FontStyle::Italic) })]), W, H);
        let plain = r.render_layout(&columns(Vec::new()), W, H);
        assert_eq!(styled.glyph_count, TEXT.len());
        // The second column starts half the box's width (1152) or more
        // to the right of the first.
        let mid = styled.glyph_boxes[0][0] + 1152 / 2;
        assert!((run_at..run_at + 6).all(|i| styled.glyph_boxes[i][0] > mid), "the run is not in the second column");
        let italic = face("Libre Baskerville", "Italic");
        for i in run_at..run_at + 6 {
            assert!(drawn_in(&styled, i, &italic, 48.0), "{:?} is not in Libre Baskerville Italic", styled.glyph_parents[i]);
        }
        // The first column's letters sit as they sat (each line of the
        // second is centred in it, the run's line differently).
        let (dx, dy) = (styled.glyph_boxes[0][0] - plain.glyph_boxes[0][0], styled.glyph_boxes[0][1] - plain.glyph_boxes[0][1]);
        let first_column: Vec<usize> = (0..run_at).filter(|&i| plain.glyph_boxes[i][0] < plain.glyph_boxes[0][0] + 1152 / 2).collect();
        assert!(first_column.len() > 20);
        for i in first_column {
            let (s, p) = (styled.glyph_boxes[i], plain.glyph_boxes[i]);
            assert_eq!(s, [p[0] + dx, p[1] + dy, p[2] + dx, p[3] + dy], "{:?} moved", styled.glyph_parents[i]);
        }
    }

    /// A drop cap still wraps its paragraph with runs in it: the cap
    /// draws in the face of a run over it, the letters before a styled
    /// word sit where they sat, and the word draws in its face.
    #[test]
    fn a_drop_cap_wraps_with_runs() {
        const TEXT: &str = "the quick brown fox jumps over the lazy dog and runs far into the woods by the river Zed and on past the mill to the sea where the boats come in at night";
        let run_at = TEXT.find("river").unwrap();
        let mut r = TitleRenderer::new();
        // Set from the left at a fifth of the frame, so the box's width
        // stays in it.
        let capped = |runs: Vec<StyleRun>| {
            let mut td = boxed(TEXT, TextAlignment::Left, |tb| {
                tb.drop_cap_scale = 3.0;
                tb.style_runs = runs;
            });
            td.objects[0].transform.center.x = 0.2;
            td
        };
        let italic_word = run(run_at, run_at + 5, |r| { r.font_name = Some("Libre Baskerville".into()); r.font_style = Some(FontStyle::Italic) });
        let styled = r.render_layout(&capped(vec![italic_word.clone()]), W, H);
        let plain = r.render_layout(&capped(Vec::new()), W, H);
        assert_eq!(styled.glyph_count, TEXT.len());
        // The body wraps beside the cap and then under it.
        let cap = plain.glyph_boxes[0];
        let beside: Vec<i32> = plain.glyph_boxes[1..].iter().filter(|b| b[1] < cap[3] && b[2] > b[0]).map(|b| b[0]).collect();
        assert!(beside.iter().all(|&x| x > cap[2]), "body letters beside the cap overlap it");
        assert!(plain.glyph_boxes[1..].iter().any(|b| b[1] >= cap[3] && b[0] < cap[2]), "no line wraps under the cap");
        let italic = face("Libre Baskerville", "Italic");
        for i in run_at..run_at + 5 {
            assert!(drawn_in(&styled, i, &italic, 48.0), "{:?} is not in Libre Baskerville Italic", styled.glyph_parents[i]);
        }
        let dy = styled.glyph_boxes[0][1] - plain.glyph_boxes[0][1];
        for i in 0..run_at {
            let (s, p) = (styled.glyph_boxes[i], plain.glyph_boxes[i]);
            assert_eq!(s, [p[0], p[1] + dy, p[2], p[3] + dy], "{:?} moved", styled.glyph_parents[i]);
        }
        // A run over the cap sets the cap in its face.
        let cap_run = run(0, 1, |r| r.font_name = Some("Libre Baskerville".into()));
        let styled_cap = r.render_layout(&capped(vec![cap_run, italic_word]), W, H);
        assert!(drawn_in(&styled_cap, 0, &face("Libre Baskerville", "Regular"), 144.0), "the cap is not in Libre Baskerville");
    }

    /// Each field a run can set moves its glyphs, so each one re-lays
    /// the cursor's glyph cache when it changes.
    #[test]
    fn the_glyph_cache_hash_reads_every_run_field() {
        let base = sentence(vec![run(10, 15, |_| {})], |_| {});
        let hash = |adjust: fn(&mut StyleRun)| {
            let mut td = base.clone();
            adjust(&mut td.primary_text_mut().unwrap().style_runs[0]);
            glyph_layout_hash(&td, W, H)
        };
        let plain = glyph_layout_hash(&base, W, H);
        assert_eq!(hash(|r| r.color = Some([1, 2, 3, 255])), plain, "a run's colour does not move its glyphs");
        for adjust in [
            (|r: &mut StyleRun| r.font_name = Some("Libre Baskerville".into())) as fn(&mut StyleRun),
            |r| r.font_style = Some(FontStyle::Italic),
            |r| r.font_size = Some(30.0),
            |r| r.tracking = Some(5.0),
            |r| r.baseline_offset = Some(5.0),
        ] {
            assert_ne!(hash(adjust), plain);
        }
    }

    /// Boxes whose runs set none of the four fields: drawn and laid out
    /// for the cursor at 640x360.
    fn unchanged_boxes() -> Vec<TitleData> {
        let sized = |tb: &mut TextBox| {
            tb.style_runs = vec![
                StyleRun { font_size: Some(64.0), ..StyleRun::default_for_range(4, 9) },
                StyleRun { color: Some([255, 0, 0, 255]), ..StyleRun::default_for_range(20, 30) },
            ];
        };
        vec![
            boxed(TWO_PARAGRAPHS, TextAlignment::Left, |_| {}),
            boxed(TWO_PARAGRAPHS, TextAlignment::Left, |tb| tb.tracking = 3.0),
            boxed(TWO_PARAGRAPHS, TextAlignment::Center, |tb| tb.drop_cap_scale = 3.0),
            boxed(TWO_PARAGRAPHS, TextAlignment::Left, |tb| tb.drop_cap_scale = 3.0),
            boxed(TWO_PARAGRAPHS, TextAlignment::Left, |tb| tb.columns = 2),
            boxed("Hi there", TextAlignment::Center, |tb| tb.vertical_layout = true),
            boxed(TWO_PARAGRAPHS, TextAlignment::Center, sized),
            boxed(TWO_PARAGRAPHS, TextAlignment::Justify, |tb| { sized(tb); tb.tracking = 2.0 }),
            boxed("Hello world", TextAlignment::Center, |tb| { tb.font_name = "Inter".into(); tb.font_style = FontStyle::Bold; sized(tb) }),
        ]
    }

    fn hashes_of(r: &mut TitleRenderer, boxes: &[TitleData]) -> Vec<(u64, u64)> {
        boxes.iter().map(|td| {
            let pixels = fnv(r.render(td, 640, 360));
            let cursor = fnv(r.compute_char_positions(td, 640, 360).iter().flat_map(|p| [p.x.to_bits(), p.y.to_bits()]).flat_map(u32::to_le_bytes));
            (pixels, cursor)
        }).collect()
    }

    #[test]
    fn boxes_without_the_fields_draw_as_before() {
        let mut r = TitleRenderer::new();
        let hashes = hashes_of(&mut r, &unchanged_boxes());
        // Taken on main at e4618c0f1, before #1505; the last, "Hello
        // world" in Inter, retaken with #1501's shaping, which kerns it;
        // the vertical "Hi there" retaken with #1507, which turns Latin
        // sideways in a column, and its cursor with #1704, which puts the
        // characters where the column sets them.
        let before = [
            (537262310058089473, 12990052878724863373),
            (9375910112802164878, 7007222290492971662),
            (17185529782468128921, 6239473186763768033),
            (1323807162608565093, 12990052878724863373),
            (12328210842903889326, 12990052878724863373),
            (13922581151920256384, 382594927444531904),
            (5448820052067944582, 6194283091192519508),
            (7193490694850778374, 224027851749628502),
            (7400897415938749996, 9238244902904153988),
        ];
        assert_eq!(hashes, before, "a box whose runs set no font, style, tracking or baseline offset draws or lays out differently");
    }
}

#[cfg(test)]
mod shaping_tests {
    //! #1501: title text is shaped with OpenType — kerning, ligatures
    //! and the features a box or a run turns on or off.
    use super::*;
    use klipp_timeline::{TextBox, TitlePosition};

    const W: u32 = 1920;
    const H: u32 = 1080;
    /// Libre Baskerville Regular's "f_f_i" ligature.
    const FFI: u16 = 623;

    fn boxed(text: &str, family: &str, alignment: TextAlignment, adjust: impl FnOnce(&mut TextBox)) -> TitleData {
        let mut tb = TextBox { text: text.to_string(), font_name: family.into(), font_size: 48.0, alignment, ..Default::default() };
        adjust(&mut tb);
        let mut td = TitleData::single_text_fx(tb, LayerFx::default());
        let o = td.objects.first_mut().unwrap();
        o.transform.center = TitlePosition { x: 0.5, y: 0.5 };
        o.transform.size = TitlePosition { x: 0.6, y: 0.9 };
        td
    }

    fn loaded(family: &str, style: &str) -> shaping::LoadedFont {
        shaping::LoadedFont::new(std::borrow::Cow::Borrowed(bundled::lookup(family, style).unwrap())).unwrap()
    }

    /// `text` laid on one line in a bundled face with `features`, at
    /// 48 px: each glyph's character, glyph id and left edge.
    fn shaped(family: &str, text: &str, features: &[&str]) -> Vec<(char, u16, f32)> {
        let font = loaded(family, "Regular");
        let features: Vec<String> = features.iter().map(|f| f.to_string()).collect();
        let faces = [Face::new(&font, &features)];
        let mut layout = ShapedLayout::new();
        layout.reset(&LayoutSettings::default());
        layout.append(&faces, text, (0, text.len(), 48.0, 0));
        layout.glyphs().iter().map(|g| (g.parent, g.key.glyph_index, g.x)).collect()
    }

    /// "AV" kerns: the V sits closer to the A than the A's advance puts
    /// it, as fontdue alone laid it, and "-kern" puts it back there.
    #[test]
    fn a_kerning_pair_sits_closer() {
        for family in ["Libre Baskerville", "Inter"] {
            let font = loaded(family, "Regular").font;
            let mut plain = fontdue::layout::Layout::new(fontdue::layout::CoordinateSystem::PositiveYDown);
            plain.append(&[&font], &fontdue::layout::TextStyle::new("AV", 48.0, 0));
            let unkerned = plain.glyphs()[1].x - plain.glyphs()[0].x;
            let kerned = shaped(family, "AV", &[]);
            assert!(kerned[1].0 == 'V' && kerned[1].2 - kerned[0].2 < unkerned - 1.0, "{family}: V is {} past A, unkerned {unkerned}", kerned[1].2 - kerned[0].2);
            let off = shaped(family, "AV", &["-kern"]);
            assert_eq!(off[1].2 - off[0].2, unkerned, "{family}: -kern still kerns");
            // The renderer draws the pair kerned.
            let mut r = TitleRenderer::new();
            let mut gap = |features: Vec<String>| {
                let layout = r.render_layout(&boxed("AV", family, TextAlignment::Left, |tb| tb.features = features), W, H);
                layout.glyph_boxes[1][0] - layout.glyph_boxes[0][0]
            };
            assert!(gap(Vec::new()) < gap(vec!["-kern".into()]), "{family}: the drawn V is not kerned");
        }
    }

    /// "office" in Libre Baskerville sets "ffi" as its one ligature
    /// glyph, which stands for the first f; "-liga" sets three letters.
    #[test]
    fn ffi_forms_a_ligature() {
        let glyphs = shaped("Libre Baskerville", "office", &[]);
        assert_eq!(glyphs.iter().map(|g| (g.0, g.1)).collect::<Vec<_>>()[..2], [('o', 481), ('f', FFI)]);
        assert_eq!(glyphs.len(), 4, "o, ffi, c, e");
        assert_eq!(glyphs.iter().filter(|g| g.1 == FFI).count(), 1);
        let off = shaped("Libre Baskerville", "office", &["-liga"]);
        assert_eq!(off.len(), 6);
        assert!(off.iter().all(|g| g.1 != FFI));
        // The renderer draws the ligature, and the box's "-liga" or a
        // run's turns it off for its text only.
        let mut r = TitleRenderer::new();
        let mut count = |adjust: fn(&mut TextBox)| r.render_layout(&boxed("office office", "Libre Baskerville", TextAlignment::Left, adjust), W, H).glyph_count;
        assert_eq!(count(|_| {}), 9);
        assert_eq!(count(|tb| tb.features = vec!["-liga".into()]), 13);
        assert_eq!(count(|tb| tb.style_runs = vec![StyleRun { features: vec!["-liga".into()], ..StyleRun::default_for_range(7, 13) }]), 11);
        let layout = r.render_layout(&boxed("office", "Libre Baskerville", TextAlignment::Left, |_| {}), W, H);
        let (m, _) = loaded("Libre Baskerville", "Regular").font.rasterize_indexed(FFI, 48.0);
        let b = layout.glyph_boxes[1];
        assert_eq!((layout.glyph_parents[1], b[2] - b[0], b[3] - b[1]), ('f', m.width as i32, m.height as i32), "the ffi glyph is not drawn");
    }

    /// A feature the shaper's defaults leave off turns on: Inter's
    /// slashed zero.
    #[test]
    fn a_feature_turns_on() {
        let plain = shaped("Inter", "0", &[]);
        let slashed = shaped("Inter", "0", &["zero"]);
        assert_ne!(plain[0].1, slashed[0].1, "zero did not change the glyph");
    }

    /// The cursor stays within a pixel of each letter of a kerned line,
    /// and the letters of a ligature take their places inside its box,
    /// left to right.
    #[test]
    fn the_cursor_follows_kerned_and_joined_letters() {
        const TEXT: &str = "AVATAR To office";
        let mut r = TitleRenderer::new();
        let td = boxed(TEXT, "Libre Baskerville", TextAlignment::Center, |_| {});
        let layout = r.render_layout(&td, W, H);
        let positions = r.compute_char_positions(&td, W, H);
        assert_eq!(positions.len(), TEXT.chars().count() + 1);
        let ffi = TEXT.find("ffi").unwrap();
        // One glyph per character but for the ligature's two after it.
        assert_eq!(layout.glyph_count, TEXT.len() - 2);
        for (g, b) in layout.glyph_boxes.iter().enumerate() {
            let at = if g > ffi { g + 2 } else { g };
            let p = &positions[at];
            assert_eq!(p.byte_offset, at);
            if layout.glyph_parents[g] != ' ' {
                assert!((p.x - b[0] as f32).abs() <= 1.0 && (p.y - b[1] as f32).abs() <= 1.0, "{:?} drawn at {b:?}, cursor at {}, {}", layout.glyph_parents[g], p.x, p.y);
            }
        }
        let b = layout.glyph_boxes[ffi];
        let xs: Vec<f32> = positions[ffi..ffi + 3].iter().map(|p| p.x).collect();
        assert!(xs.windows(2).all(|w| w[0] < w[1]), "the ligature's letters are not in order: {xs:?}");
        for p in &positions[ffi..ffi + 3] {
            assert!(p.x >= b[0] as f32 && p.x + p.advance <= b[2] as f32 + 0.01, "a letter of ffi at {}..{} is outside its box {b:?}", p.x, p.x + p.advance);
        }
    }

    /// Justified lines of kerned text still fill the box: each line but
    /// the paragraph's last ends at the box's right edge.
    #[test]
    fn justify_fills_the_box_with_kerned_text() {
        const TEXT: &str = "AVOWAL To Tea Ty Wa fly office we Yo VA LT AV To Wo yo Te Ta AV WAVY Toyota affix Tea To Ty AVA LTA";
        let mut r = TitleRenderer::new();
        let layout = r.render_layout(&boxed(TEXT, "Libre Baskerville", TextAlignment::Justify, |tb| tb.font_size = 40.0), W, H);
        // Lines, by where a letter sits left of the one before.
        let mut rights: Vec<i32> = Vec::new();
        let mut last_x = i32::MIN;
        for (b, c) in layout.glyph_boxes.iter().zip(&layout.glyph_parents) {
            if c.is_whitespace() || b[2] == b[0] {
                continue;
            }
            if b[0] < last_x || rights.is_empty() {
                rights.push(b[2]);
            }
            last_x = b[0];
            *rights.last_mut().unwrap() = (*rights.last().unwrap()).max(b[2]);
        }
        assert!(rights.len() >= 2, "the text should wrap, got {} lines", rights.len());
        let box_right = 960 + 1152 / 2;
        for (i, right) in rights[..rights.len() - 1].iter().enumerate() {
            assert!((right - box_right).abs() <= 1, "line {i} ends at {right}, not the box's right {box_right}");
        }
    }

    fn fnv(bytes: impl IntoIterator<Item = u8>) -> u64 {
        bytes.into_iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
    }

    /// Text whose face kerns and joins none of its letters draws and
    /// lays out for the cursor byte for byte as it did before shaping:
    /// JetBrains Mono, plain, in columns, with a drop cap, justified
    /// and with a sized run, at 640x360.
    #[test]
    fn text_shaping_leaves_alone_draws_as_before() {
        const TEXT: &str = "the quick brown fox jumps over the lazy dog and runs far into the woods\nby the river and home";
        let mono = |alignment, adjust: fn(&mut TextBox)| boxed(TEXT, "JetBrains Mono", alignment, adjust);
        let boxes = [
            mono(TextAlignment::Left, |_| {}),
            mono(TextAlignment::Center, |tb| tb.columns = 2),
            mono(TextAlignment::Left, |tb| tb.drop_cap_scale = 3.0),
            mono(TextAlignment::Justify, |tb| tb.tracking = 2.0),
            mono(TextAlignment::Right, |tb| tb.style_runs = vec![StyleRun { font_size: Some(64.0), color: Some([255, 0, 0, 255]), ..StyleRun::default_for_range(4, 9) }]),
        ];
        let mut r = TitleRenderer::new();
        let hashes: Vec<(u64, u64)> = boxes.iter().map(|td| {
            let pixels = fnv(r.render(td, 640, 360));
            let cursor = fnv(r.compute_char_positions(td, 640, 360).iter().flat_map(|p| [p.x.to_bits(), p.y.to_bits()]).flat_map(u32::to_le_bytes));
            (pixels, cursor)
        }).collect();
        // Taken on main at 926cfab3d, before #1501.
        let before = [
            (6118582486406094929, 12454316361513787245),
            (945665265085242760, 15164285115390767754),
            (1478896466625051445, 12454316361513787245),
            (10567450272971704784, 60548708505280472),
            (17028306380591570627, 11204599021776345478),
        ];
        assert_eq!(hashes, before, "text with nothing to shape draws or lays out differently");
    }

    /// A box's and a run's features each re-lay the cursor's glyph
    /// cache when they change.
    #[test]
    fn the_glyph_cache_hash_reads_features() {
        let base = boxed("office", "Libre Baskerville", TextAlignment::Left, |tb| tb.style_runs = vec![StyleRun::default_for_range(0, 3)]);
        let plain = glyph_layout_hash(&base, W, H);
        let mut on_box = base.clone();
        on_box.primary_text_mut().unwrap().features = vec!["-liga".into()];
        let mut on_run = base.clone();
        on_run.primary_text_mut().unwrap().style_runs[0].features = vec!["-liga".into()];
        assert_ne!(glyph_layout_hash(&on_box, W, H), plain);
        assert_ne!(glyph_layout_hash(&on_run, W, H), plain);
    }
}

#[cfg(test)]
mod fallback_tests {
    //! #1503: a character the face lacks is drawn from a face in the
    //! fallback chain that has it; colour emoji draw in colour.
    use super::*;
    use klipp_timeline::{TextBox, TitlePosition};

    fn boxed(text: &str, family: &str, alignment: TextAlignment, adjust: impl FnOnce(&mut TextBox)) -> TitleData {
        let mut tb = TextBox { text: text.to_string(), font_name: family.into(), font_size: 48.0, alignment, ..Default::default() };
        adjust(&mut tb);
        let mut td = TitleData::single_text_fx(tb, LayerFx::default());
        let o = td.objects.first_mut().unwrap();
        o.transform.center = TitlePosition { x: 0.5, y: 0.5 };
        o.transform.size = TitlePosition { x: 0.6, y: 0.9 };
        td
    }

    fn fnv(bytes: impl IntoIterator<Item = u8>) -> u64 {
        bytes.into_iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
    }

    const W: u32 = 1920;
    const H: u32 = 1080;

    /// A renderer whose chain is the faces that ship with the app only,
    /// so a test draws the same on every machine.
    fn bundled_only() -> TitleRenderer {
        let mut r = TitleRenderer::new();
        r.system_fallback = false;
        r
    }

    fn font(bytes: &'static [u8]) -> fontdue::Font {
        fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()).unwrap()
    }

    /// The first glyph drawing `c`.
    fn glyph_of(layout: &TitleLayout, c: char) -> usize {
        layout.glyph_parents.iter().position(|&p| p == c).unwrap_or_else(|| panic!("no glyph draws {c:?}"))
    }

    /// The glyph drawing `c` was drawn as `font` draws it at `px`: its
    /// box is that bitmap's size and its pixels hold that bitmap.
    fn drawn_in(layout: &TitleLayout, c: char, font: &fontdue::Font, px: f32) -> bool {
        let b = layout.glyph_boxes[glyph_of(layout, c)];
        let (m, bitmap) = font.rasterize(c, px);
        if (b[2] - b[0], b[3] - b[1]) != (m.width as i32, m.height as i32) {
            return false;
        }
        let (x0, y0) = (layout.mask_x0 as i32, layout.mask_y0 as i32);
        let mut i = 0;
        for y in b[1]..b[3] {
            for x in b[0]..b[2] {
                if layout.alpha_mask[((y - y0) * layout.mask_w as i32 + (x - x0)) as usize] < bitmap[i] {
                    return false;
                }
                i += 1;
            }
        }
        true
    }

    /// Bebas Neue has no Cyrillic "Ж", no arrow and no box-drawing
    /// cross. The "Ж" draws from the embedded Noto Sans; the arrow,
    /// which Noto Sans lacks too, from the first bundled family with
    /// it, Inter; the cross, which Inter lacks as well, from JetBrains
    /// Mono. "Ж" does not draw as Bebas Neue's missing glyph. The letters round them still
    /// draw in Bebas Neue, in the plain box, a drop cap, columns and a
    /// justified box, and from a style run set in Bebas Neue.
    #[test]
    fn a_character_the_face_lacks_draws_from_a_fallback() {
        let bebas = font(bundled::lookup("Bebas Neue", "Regular").unwrap());
        let noto = font(EMBEDDED_FONT);
        let mono = font(bundled::lookup("JetBrains Mono", "Regular").unwrap());
        let inter = font(bundled::lookup("Inter", "Regular").unwrap());
        assert!(bebas.lookup_glyph_index('Ж') == 0 && bebas.lookup_glyph_index('┼') == 0 && noto.lookup_glyph_index('┼') == 0);
        assert!(inter.lookup_glyph_index('┼') == 0 && bebas.lookup_glyph_index('→') == 0 && noto.lookup_glyph_index('→') == 0);
        let (missing, _) = bebas.rasterize_indexed(0, 48.0);
        let mut r = bundled_only();
        let plain = r.render_layout(&boxed("SALE Ж AT ┼ END", "Bebas Neue", TextAlignment::Center, |_| {}), W, H);
        assert!(drawn_in(&plain, 'Ж', &noto, 48.0), "Ж is not Noto Sans's");
        assert!(drawn_in(&plain, '┼', &mono, 48.0), "the cross is not JetBrains Mono's");
        let b = plain.glyph_boxes[glyph_of(&plain, 'Ж')];
        assert_ne!((b[2] - b[0], b[3] - b[1]), (missing.width as i32, missing.height as i32), "Ж drew as the missing glyph");
        assert!(drawn_in(&plain, 'S', &bebas, 48.0) && drawn_in(&plain, 'N', &bebas, 48.0));

        const TEXT: &str = "ЖAR → ┼ SALE NOW ON EVERY Ж LINE AND MORE WORDS TO WRAP ONTO ANOTHER LINE AND THEN A THIRD LINE AND A FOURTH ONE TOO";
        let boxes = [
            (boxed(TEXT, "Bebas Neue", TextAlignment::Left, |tb| tb.drop_cap_scale = 3.0), 144.0),
            (boxed(TEXT, "Bebas Neue", TextAlignment::Center, |tb| tb.drop_cap_scale = 3.0), 144.0),
            (boxed(TEXT, "Bebas Neue", TextAlignment::Left, |tb| tb.columns = 2), 48.0),
            (boxed(TEXT, "Bebas Neue", TextAlignment::Justify, |_| {}), 48.0),
            (boxed(TEXT, "Inter", TextAlignment::Left, |tb| tb.style_runs = vec![StyleRun { font_name: Some("Bebas Neue".into()), ..StyleRun::default_for_range(0, 4) }]), 48.0),
        ];
        for (i, (td, cap_px)) in boxes.iter().enumerate() {
            let layout = r.render_layout(td, W, H);
            assert!(drawn_in(&layout, 'Ж', &noto, *cap_px), "box {i}: the first Ж is not Noto Sans's");
            assert!(drawn_in(&layout, '→', &inter, 48.0), "box {i}: the arrow is not Inter's");
            assert!(drawn_in(&layout, '┼', &mono, 48.0), "box {i}: the cross is not JetBrains Mono's");
            assert!(drawn_in(&layout, 'W', if i == 4 { &inter } else { &bebas }, 48.0), "box {i}: W is not in its face");
        }
    }

    /// The cursor stays within a pixel of each letter across a
    /// character drawn from a fallback face, and on the lines after it.
    #[test]
    fn the_cursor_follows_fallback_glyphs() {
        const TEXT: &str = "SALE Ж AT → END OF THE ЖЖ SEASON AND SOME MORE WORDS SO THE TEXT WRAPS";
        let mut r = bundled_only();
        for alignment in [TextAlignment::Left, TextAlignment::Center, TextAlignment::Justify] {
            let td = boxed(TEXT, "Bebas Neue", alignment, |_| {});
            let layout = r.render_layout(&td, W, H);
            let positions = r.compute_char_positions(&td, W, H);
            assert_eq!(layout.glyph_count, TEXT.chars().count());
            for (k, (b, c)) in layout.glyph_boxes.iter().zip(&layout.glyph_parents).enumerate() {
                let p = &positions[k];
                assert_eq!(p.char_index, k);
                if *c != ' ' {
                    assert!((p.x - b[0] as f32).abs() <= 1.0 && (p.y - b[1] as f32).abs() <= 1.0, "{c:?} drawn at {b:?}, cursor at {}, {}", p.x, p.y);
                }
            }
        }
    }

    /// A Latin-only face with Chinese characters and an emoji: the
    /// Chinese draw from a system font that has them, in its glyphs.
    /// Skips where the system has no font with them.
    #[test]
    fn cjk_draws_from_a_system_font() {
        let Some(path) = fallback::system_font_for('漢', false) else {
            eprintln!("skipped: no system font has 漢");
            return;
        };
        let cjk = LoadedFont::new(std::borrow::Cow::Owned(std::fs::read(&path).unwrap())).unwrap();
        let mut r = TitleRenderer::new();
        let layout = r.render_layout(&boxed("TOKYO 漢字 🌈 NIGHTS", "Bebas Neue", TextAlignment::Center, |_| {}), W, H);
        assert!(drawn_in(&layout, '漢', &cjk.font, 48.0), "漢 is not {}'s", path.display());
        assert!(drawn_in(&layout, '字', &cjk.font, 48.0));
    }

    /// A colour emoji draws in its colours: a rainbow in a Latin-only
    /// face, from the system's colour emoji font, has pixels of at
    /// least three distinct hues. Skips where the system has no emoji
    /// font drawn as colour bitmaps (CBDT).
    #[test]
    fn a_colour_emoji_draws_in_colour() {
        let colour = fallback::system_font_for('🌈', true).and_then(|path| {
            let font = LoadedFont::new(std::borrow::Cow::Owned(std::fs::read(&path).ok()?))?;
            let gid = font.font.lookup_glyph_index('🌈');
            font.colour_glyph(gid, 48.0).is_some().then_some(path)
        });
        let Some(path) = colour else {
            eprintln!("skipped: no system emoji font with colour bitmaps (CBDT) has 🌈");
            return;
        };
        let mut r = TitleRenderer::new();
        let td = boxed("RAIN 🌈 BOW", "Bebas Neue", TextAlignment::Center, |_| {});
        let layout = r.render_layout(&td, W, H);
        assert_eq!(layout.colour_glyphs.len(), 1, "🌈 from {} is not a colour glyph", path.display());
        let b = layout.glyph_boxes[glyph_of(&layout, '🌈')];
        assert!(b[2] - b[0] > 30 && b[3] - b[1] > 30, "🌈 is {b:?}");
        // Pixels in the box with a clear colour, by 30-degree hue.
        let hues = |frame: &[u8]| {
            let mut hues = [0usize; 12];
            for y in b[1]..b[3] {
                for x in b[0]..b[2] {
                    let p = &frame[((y as u32 * W + x as u32) * 4) as usize..][..4];
                    let (max, min) = (p[0].max(p[1]).max(p[2]) as f32, p[0].min(p[1]).min(p[2]) as f32);
                    if p[3] < 200 || max - min < 0.3 * max {
                        continue;
                    }
                    let (r_, g, b_) = (p[0] as f32, p[1] as f32, p[2] as f32);
                    let hue = if max == r_ {
                        60.0 * ((g - b_) / (max - min)).rem_euclid(6.0)
                    } else if max == g {
                        60.0 * ((b_ - r_) / (max - min) + 2.0)
                    } else {
                        60.0 * ((r_ - g) / (max - min) + 4.0)
                    };
                    hues[(hue / 30.0) as usize % 12] += 1;
                }
            }
            hues
        };
        let frame = r.render(&td, W, H);
        let at_rest = hues(&frame);
        assert!(at_rest.iter().filter(|&&n| n >= 20).count() >= 3, "🌈 drew in too few hues: {at_rest:?}");
        // The letters round it keep the text colour.
        let s = layout.glyph_boxes[0];
        let p = &frame[((((s[1] + s[3]) / 2) as u32 * W + ((s[0] + s[2]) / 2) as u32) * 4) as usize..][..4];
        assert!(p[0] == p[1] && p[1] == p[2], "R is not the text colour: {p:?}");
        // A staggered soft rise is rastered glyph by glyph; once the
        // rainbow has arrived, while the letters after it still rise,
        // it draws in its colours as at rest.
        let mut rising = td.clone();
        rising.objects[0].fx_mut().motion.enter = Some(klipp_timeline::Motion { stagger: klipp_timeline::StaggerUnit::Char, ..klipp_timeline::Motion::new(klipp_core::title_motion::MotionKind::SoftRise) });
        let frame = r.render_with_time(&rising, W, H, None, 0.85);
        assert_eq!(hues(&frame), at_rest, "🌈 drew differently while the letters after it rose");
    }

    /// The glyph cache's hash reads the fallback chain's version, so
    /// positions laid out with another chain do not match.
    #[test]
    fn the_glyph_cache_hash_reads_the_fallback_chain() {
        let td = boxed("SALE Ж", "Bebas Neue", TextAlignment::Left, |_| {});
        assert_eq!(glyph_layout_hash(&td, W, H), layout_hash_with(&td, W, H, FALLBACK_VERSION));
        assert_ne!(layout_hash_with(&td, W, H, FALLBACK_VERSION), layout_hash_with(&td, W, H, FALLBACK_VERSION + 1));
    }

    /// Text each face has every character of, drawn and laid out for the
    /// cursor byte for byte as before fallback: each bundled family and
    /// the embedded face, a run in another face, a drop cap, columns,
    /// vertical and justified, at 640x360.
    #[test]
    fn text_the_face_covers_draws_as_before() {
        const LATIN: &str = "Café déjà vu — naïve “quotes” 1234 €£½ and ÅÖßñ\nsome more words wrap onto a further line here";
        let boxes = [
            boxed("Café déjà vu — naïve “quotes” 1234 €£½ ΩΣЖж→★✓", "Inter", TextAlignment::Center, |_| {}),
            boxed("CAFÉ DÉJÀ VU — NAÏVE “QUOTES” 1234 €£½ ÅÖÑ Ω", "Bebas Neue", TextAlignment::Left, |_| {}),
            boxed(LATIN, "Libre Baskerville", TextAlignment::Justify, |tb| tb.tracking = 2.0),
            boxed("Café déjà vu → ΣЖж ┼░ ✓", "JetBrains Mono", TextAlignment::Right, |_| {}),
            boxed("Café déjà vu — naïve ΩΣЖж ₹ ₿", "sans-serif", TextAlignment::Center, |_| {}),
            boxed(LATIN, "Inter", TextAlignment::Left, |tb| tb.style_runs = vec![StyleRun { font_name: Some("JetBrains Mono".into()), font_size: Some(64.0), ..StyleRun::default_for_range(6, 13) }]),
            boxed(LATIN, "Inter", TextAlignment::Left, |tb| tb.drop_cap_scale = 3.0),
            boxed(LATIN, "Libre Baskerville", TextAlignment::Center, |tb| tb.columns = 2),
            boxed("Ωé", "Inter", TextAlignment::Center, |tb| tb.vertical_layout = true),
        ];
        let mut r = TitleRenderer::new();
        let hashes: Vec<(u64, u64)> = boxes.iter().map(|td| {
            let pixels = fnv(r.render(td, 640, 360));
            let cursor = fnv(r.compute_char_positions(td, 640, 360).iter().flat_map(|p| [p.x.to_bits(), p.y.to_bits()]).flat_map(u32::to_le_bytes));
            (pixels, cursor)
        }).collect();
        // Taken on main at ee722dcb6, before #1503; the vertical "Ωé"
        // retaken with #1507, which turns them sideways in a column, and
        // its cursor with #1704, which follows the column.
        let before = [
            (12362235235435922744, 11758040230992881638),
            (11429411338127416158, 14383662946162571450),
            (3569128012378324932, 2346819359444522047),
            (11565043810062183343, 1917150523641949634),
            (9098010272575383407, 179561844797826116),
            (3193317923789182864, 14796392279787328656),
            (17543495563288601848, 15058820355971908112),
            (1948305321527838988, 11067866960264763338),
            (14617716708989983263, 7285711496372817013),
        ];
        assert_eq!(hashes, before, "text its face covers draws or lays out differently");
    }
}

#[cfg(test)]
mod run_underline_tests {
    //! #1668: a style run's own underline is drawn under that run's
    //! glyphs only; a box with none draws as before.
    use super::*;
    use klipp_timeline::{TextBox, TitlePosition};

    const W: u32 = 1280;
    const H: u32 = 720;

    fn boxed(text: &str, adjust: impl FnOnce(&mut TextBox)) -> TitleData {
        let mut tb = TextBox { text: text.to_string(), font_name: "Inter".into(), font_size: 48.0, alignment: TextAlignment::Center, ..Default::default() };
        adjust(&mut tb);
        let mut td = TitleData::single_text_fx(tb, LayerFx::default());
        let o = td.objects.first_mut().unwrap();
        o.transform.center = TitlePosition { x: 0.5, y: 0.5 };
        o.transform.size = TitlePosition { x: 0.8, y: 0.9 };
        td
    }

    fn fnv(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
    }

    const LONG: &str = "An underlined title long enough to wrap onto a second line and then a third line of words";

    /// Boxes with no run underline: the box's underline and
    /// strikethrough without style runs, and runs without an underline.
    fn without_run_underlines() -> Vec<TitleData> {
        let runs = || vec![
            StyleRun { font_size: Some(72.0), ..StyleRun::default_for_range(0, 5) },
            StyleRun { color: Some([255, 40, 40, 255]), ..StyleRun::default_for_range(6, 11) },
        ];
        vec![
            boxed("Hello world", |tb| tb.underline = true),
            boxed(LONG, |tb| {
                tb.underline = true;
                tb.strikethrough = true;
            }),
            boxed("Hello world", |tb| tb.style_runs = runs()),
            boxed(LONG, |tb| {
                tb.underline = true;
                tb.alignment = TextAlignment::Left;
                tb.drop_cap_scale = 3.0;
            }),
        ]
    }

    /// Taken on main before #1668: a box without run underlines draws
    /// byte for byte as it did.
    #[test]
    fn boxes_without_run_underlines_draw_as_before() {
        const BEFORE: [u64; 4] = [0xae7f_e880_8be2_206a, 0x3641_3dbc_c81a_b919, 0x429f_31d9_51d9_f8a9, 0x1360_216f_dbf6_1906];
        let mut r = TitleRenderer::new();
        r.system_fallback = false;
        for (i, td) in without_run_underlines().iter().enumerate() {
            assert_eq!(fnv(&r.render(td, W, H)), BEFORE[i], "box {i} draws differently");
        }
    }

    /// The pixels of `frame` drawn at all, and those drawn red, in the
    /// columns `x0..x1` and the rows `y0..y1`.
    fn drawn(frame: &[u8], (x0, x1): (i32, i32), (y0, y1): (i32, i32)) -> (usize, usize) {
        let (mut any, mut red) = (0, 0);
        for y in y0.max(0)..y1.min(H as i32) {
            for x in x0.max(0)..x1.min(W as i32) {
                let p = &frame[(y as usize * W as usize + x as usize) * 4..][..4];
                if p[3] > 0 {
                    any += 1;
                    if p[0] > 200 && p[1] < 80 && p[2] < 80 {
                        red += 1;
                    }
                }
            }
        }
        (any, red)
    }

    /// The ink columns of the glyphs drawing `text[from..to]`, and the
    /// bottom of their line's ink.
    fn ink(layout: &TitleLayout, text: &str, from: usize, to: usize) -> ((i32, i32), i32) {
        let chars: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
        let boxes: Vec<[i32; 4]> = layout.glyph_boxes.iter().enumerate().filter(|&(k, b)| (from..to).contains(&chars[k]) && b[2] > b[0]).map(|(_, b)| *b).collect();
        let x0 = boxes.iter().map(|b| b[0]).min().unwrap();
        let x1 = boxes.iter().map(|b| b[2]).max().unwrap();
        let line_bottom = boxes.iter().map(|b| b[3]).max().unwrap();
        ((x0, x1), line_bottom)
    }

    /// One red word in a box, underlined on its own: a red stripe
    /// lies under the word, across it, and nothing is drawn under the
    /// word before it. Turned off on that word in an underlined box,
    /// the stripe lies under the other word and not under it.
    #[test]
    fn a_run_underline_lies_under_its_glyphs_only() {
        let mut r = TitleRenderer::new();
        r.system_fallback = false;
        let run = |underline| vec![StyleRun { color: Some([255, 0, 0, 255]), underline: Some(underline), ..StyleRun::default_for_range(6, 11) }];
        let td = boxed("Hello world", |tb| tb.style_runs = run(true));
        let layout = r.render_layout(&td, W, H);
        let frame = r.render(&td, W, H);
        let (hello, bottom) = ink(&layout, "Hello world", 0, 5);
        let (world, _) = ink(&layout, "Hello world", 6, 11);
        let below = (bottom + 1, bottom + 12);
        let (any, red) = drawn(&frame, world, below);
        assert!(red > 0 && red == any, "under the word: {any} drawn, {red} red");
        // The stripe spans the word: drawn in its first and last columns.
        assert!(drawn(&frame, (world.0, world.0 + 2), below).1 > 0 && drawn(&frame, (world.1 - 2, world.1), below).1 > 0);
        assert_eq!(drawn(&frame, (hello.0, hello.1), below).0, 0, "drawn under the word without an underline");
        // Without the run's underline, nothing is drawn below the line.
        let plain = r.render(&boxed("Hello world", |tb| tb.style_runs = vec![StyleRun { color: Some([255, 0, 0, 255]), ..StyleRun::default_for_range(6, 11) }]), W, H);
        assert_eq!(drawn(&plain, (0, W as i32), below).0, 0);

        let td = boxed("Hello world", |tb| {
            tb.underline = true;
            tb.style_runs = run(false);
        });
        let frame = r.render(&td, W, H);
        assert!(drawn(&frame, hello, below).0 > 0, "the box's underline is not under the other word");
        assert_eq!(drawn(&frame, world, below).0, 0, "drawn under the word with its underline off");
    }

    /// An underlined box with style runs draws its underline (#1680):
    /// under a red run in red, and under the text outside any run in
    /// the box's colour, not red.
    #[test]
    fn a_box_underline_is_drawn_with_style_runs() {
        let mut r = TitleRenderer::new();
        r.system_fallback = false;
        let td = boxed("Hello world", |tb| {
            tb.underline = true;
            tb.style_runs = vec![StyleRun { color: Some([255, 0, 0, 255]), ..StyleRun::default_for_range(6, 11) }];
        });
        let layout = r.render_layout(&td, W, H);
        let frame = r.render(&td, W, H);
        let (hello, bottom) = ink(&layout, "Hello world", 0, 5);
        let (world, _) = ink(&layout, "Hello world", 6, 11);
        let below = (bottom + 1, bottom + 12);
        let (any, red) = drawn(&frame, world, below);
        assert!(red > 0 && red == any, "under the red word: {any} drawn, {red} red");
        let (any, red) = drawn(&frame, hello, below);
        assert!(any > 0 && red == 0, "under the other word: {any} drawn, {red} red");
        // Not underlined, the same box draws nothing below the line.
        let plain = r.render(&boxed("Hello world", |tb| tb.style_runs = vec![StyleRun { color: Some([255, 0, 0, 255]), ..StyleRun::default_for_range(6, 11) }]), W, H);
        assert_eq!(drawn(&plain, (0, W as i32), below).0, 0);
    }

    /// In a box of several lines, a word's underline lies under that
    /// word on its own line, not at the foot of the box.
    #[test]
    fn a_run_underline_follows_its_line() {
        let mut r = TitleRenderer::new();
        r.system_fallback = false;
        let td = boxed(LONG, |tb| {
            tb.font_size = 96.0;
            tb.style_runs = vec![StyleRun { underline: Some(true), ..StyleRun::default_for_range(3, 13) }];
        });
        let layout = r.render_layout(&td, W, H);
        let frame = r.render(&td, W, H);
        let (word, bottom) = ink(&layout, LONG, 3, 13);
        assert!(layout.text_bottom as i32 > bottom + 40, "the text is one line");
        // Under the word's line, whose descenders reach below the word.
        assert!(drawn(&frame, word, (bottom + 1, bottom + 24)).0 > 0, "no underline under the word");
        let (_, last) = ink(&layout, LONG, LONG.len() - 5, LONG.len());
        assert_eq!(drawn(&frame, (0, W as i32), (last + 1, last + 24)).0, 0, "an underline at the foot of the box");
    }
}

#[cfg(test)]
mod font_weight_tests {
    //! #1504: a text box or a style run names a weight, 100 to 900, and
    //! a variable font's axes; boxes that name neither draw as before.
    use super::*;
    use klipp_timeline::{TextBox, TitlePosition};

    const W: u32 = 1280;
    const H: u32 = 720;
    const SENTENCE: &str = "the quick brown fox jumps";

    fn boxed(family: &str, style: FontStyle, adjust: impl FnOnce(&mut TextBox)) -> TitleData {
        let mut tb = TextBox { text: SENTENCE.to_string(), font_name: family.into(), font_style: style, font_size: 48.0, alignment: TextAlignment::Left, ..Default::default() };
        adjust(&mut tb);
        let mut td = TitleData::single_text_fx(tb, LayerFx::default());
        let o = td.objects.first_mut().unwrap();
        o.transform.center = TitlePosition { x: 0.5, y: 0.5 };
        o.transform.size = TitlePosition { x: 0.8, y: 0.9 };
        td
    }

    fn fnv(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
    }

    fn run(start: usize, end: usize, adjust: impl FnOnce(&mut StyleRun)) -> StyleRun {
        let mut run = StyleRun::default_for_range(start, end);
        adjust(&mut run);
        run
    }

    /// Boxes that name only a family and a `FontStyle`, in the bundled
    /// families, with and without style runs.
    fn style_only() -> Vec<TitleData> {
        vec![
            boxed("Inter", FontStyle::Regular, |_| {}),
            boxed("Inter", FontStyle::Bold, |_| {}),
            boxed("Inter", FontStyle::Italic, |_| {}),
            boxed("Inter", FontStyle::BoldItalic, |_| {}),
            boxed("Bebas Neue", FontStyle::Bold, |_| {}),
            boxed("sans-serif", FontStyle::Regular, |_| {}),
            boxed("Libre Baskerville", FontStyle::Bold, |tb| {
                tb.style_runs = vec![
                    run(4, 9, |r| r.font_style = Some(FontStyle::Italic)),
                    run(10, 15, |r| { r.font_name = Some("JetBrains Mono".into()); r.font_style = Some(FontStyle::BoldItalic) }),
                    run(20, 25, |r| { r.font_name = Some("Inter".into()); r.color = Some([255, 0, 0, 255]) }),
                ];
            }),
        ]
    }

    /// Taken before #1504: a box that names no weight and no axes
    /// draws byte for byte as it did.
    #[test]
    fn boxes_naming_only_a_style_draw_as_before() {
        const BEFORE: [u64; 7] = [0x3d54_7a04_d592_8137, 0x91d0_ae45_91a3_267f, 0xf90e_c3ac_7866_a010, 0xd7f2_9a99_8b42_ba13, 0x027f_d4c1_efec_eef3, 0x6dbb_0ff9_f7ed_f289, 0x8773_dd0f_4e73_31d6];
        let mut r = TitleRenderer::new();
        r.system_fallback = false;
        for (i, td) in style_only().iter().enumerate() {
            assert_eq!(fnv(&r.render(td, W, H)), BEFORE[i], "box {i} draws differently");
        }
    }

    /// The mask's pixels inside glyph `i`'s box.
    fn ink(layout: &TitleLayout, i: usize) -> Vec<u8> {
        let b = layout.glyph_boxes[i];
        let (x0, y0) = (layout.mask_x0 as i32, layout.mask_y0 as i32);
        let mut out = Vec::new();
        for y in b[1]..b[3] {
            for x in b[0]..b[2] {
                out.push(layout.alpha_mask[((y - y0) * layout.mask_w as i32 + (x - x0)) as usize]);
            }
        }
        out
    }

    /// Glyph `i` was drawn as the bundled `family` `style` face draws
    /// its character: its box that bitmap's size, its pixels at least
    /// the bitmap's (a kerned neighbour's ink may reach in).
    fn drawn_in(layout: &TitleLayout, i: usize, family: &str, style: &str) -> bool {
        let font = fontdue::Font::from_bytes(bundled::lookup(family, style).unwrap(), fontdue::FontSettings::default()).unwrap();
        let (m, bitmap) = font.rasterize(layout.glyph_parents[i], 48.0 * layout.scale);
        let b = layout.glyph_boxes[i];
        (b[2] - b[0], b[3] - b[1]) == (m.width as i32, m.height as i32) && ink(layout, i).iter().zip(&bitmap).all(|(i, c)| i >= c)
    }

    /// A box naming a weight draws in the family's face nearest it, as
    /// CSS picks one: Inter ships 400 and 700, so 650 and 900 draw in
    /// Bold and 100 and 450 in Regular, italic in the italics; each
    /// draws exactly as the box set in that face by its style does.
    #[test]
    fn a_weight_draws_the_familys_nearest_face() {
        let mut r = TitleRenderer::new();
        r.system_fallback = false;
        let by_style = |r: &mut TitleRenderer, style| r.render(&boxed("Inter", style, |_| {}), W, H);
        let by_weight = |r: &mut TitleRenderer, style, w| r.render(&boxed("Inter", style, |tb| tb.font_weight = Some(w)), W, H);
        let (regular, bold, bold_italic) = (by_style(&mut r, FontStyle::Regular), by_style(&mut r, FontStyle::Bold), by_style(&mut r, FontStyle::BoldItalic));
        assert_ne!(regular, bold);
        for w in [650, 700, 900] {
            assert!(by_weight(&mut r, FontStyle::Regular, w) == bold, "{w} does not draw in Inter Bold");
        }
        for w in [100, 400, 450] {
            assert!(by_weight(&mut r, FontStyle::Bold, w) == regular, "{w} does not draw in Inter Regular");
        }
        assert!(by_weight(&mut r, FontStyle::Italic, 800) == bold_italic, "an italic 800 does not draw in Inter Bold Italic");
        let layout = r.render_layout(&boxed("Inter", FontStyle::Regular, |tb| tb.font_weight = Some(700)), W, H);
        for i in (0..SENTENCE.len()).filter(|&i| SENTENCE.as_bytes()[i] != b' ') {
            assert!(drawn_in(&layout, i, "Inter", "Bold"), "{:?} is not in Inter Bold", layout.glyph_parents[i]);
        }
    }

    /// A run's weight draws its glyphs in that face; a run that sets
    /// italic alone keeps the box's weight, one that sets bold does not.
    #[test]
    fn a_runs_weight_draws_its_glyphs_in_that_face() {
        let mut r = TitleRenderer::new();
        r.system_fallback = false;
        let layout = r.render_layout(&boxed("Inter", FontStyle::Regular, |tb| tb.style_runs = vec![run(10, 15, |r| r.font_weight = Some(800))]), W, H);
        for i in 10..15 {
            assert!(drawn_in(&layout, i, "Inter", "Bold"), "{:?} is not in Inter Bold", layout.glyph_parents[i]);
        }
        assert!(drawn_in(&layout, 4, "Inter", "Regular"));
        let layout = r.render_layout(&boxed("Inter", FontStyle::Regular, |tb| {
            tb.font_weight = Some(700);
            tb.style_runs = vec![run(10, 15, |r| r.font_style = Some(FontStyle::Italic)), run(20, 25, |r| r.font_style = Some(FontStyle::Bold))];
        }), W, H);
        for i in 10..15 {
            assert!(drawn_in(&layout, i, "Inter", "Bold Italic"), "{:?} is not in Inter Bold Italic", layout.glyph_parents[i]);
        }
        for i in 20..25 {
            assert!(drawn_in(&layout, i, "Inter", "Bold"), "{:?} is not in Inter Bold", layout.glyph_parents[i]);
        }
        assert!(drawn_in(&layout, 4, "Inter", "Bold"));
    }

    const VARIABLE: &str = "Inter Variable Test";

    /// A renderer whose only system family is a subset of Inter's
    /// variable font (weight 100 to 900, optical size 14 to 32).
    fn with_variable_family() -> TitleRenderer {
        let mut r = TitleRenderer::new();
        r.system_fallback = false;
        let path = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/InterVariable-subset.ttf"));
        let traits = variable::face_traits(&std::fs::read(&path).unwrap()).unwrap();
        assert!(traits.axes.iter().any(|a| a.tag == "wght" && a.min == 100.0 && a.max == 900.0));
        let info = font_discovery::FontInfo { family: VARIABLE.into(), style: "Regular".into(), path, weight: traits.weight, italic: traits.italic, axes: traits.axes };
        r.font_registry = std::sync::Arc::new([(VARIABLE.to_string(), vec![info])].into_iter().collect());
        r
    }

    fn at_axes(axes: &[(&str, f32)]) -> TitleData {
        boxed(VARIABLE, FontStyle::Regular, |tb| tb.font_axes = axes.iter().map(|&(t, v)| (t.to_string(), v)).collect())
    }

    /// The ink drawn, summed.
    fn ink_sum(layout: &TitleLayout) -> u64 {
        layout.alpha_mask.iter().map(|&a| a as u64).sum()
    }

    /// A variable face's weight axis shapes and draws its instance: the
    /// heavier the wider the line and the more ink, 650 between 600 and
    /// 700; a weight named on the box sets the axis, and an axis left at
    /// its default draws as the face does unset.
    #[test]
    fn a_variable_faces_weight_axis_draws_its_instance() {
        let mut r = with_variable_family();
        let unset = r.render_layout(&at_axes(&[]), W, H);
        let at = |r: &mut TitleRenderer, w: f32| r.render_layout(&at_axes(&[("wght", w)]), W, H);
        let (regular, semibold, between, bold) = (at(&mut r, 400.0), at(&mut r, 600.0), at(&mut r, 650.0), at(&mut r, 700.0));
        assert_eq!((regular.alpha_mask.clone(), regular.glyph_boxes.clone()), (unset.alpha_mask.clone(), unset.glyph_boxes.clone()), "the default instance draws differently");
        let last = SENTENCE.len() - 1;
        let right = |l: &TitleLayout| l.glyph_boxes[last][2];
        assert!(right(&regular) < right(&semibold) && right(&semibold) < right(&between) && right(&between) < right(&bold), "the advances do not widen with the weight");
        assert!(ink_sum(&regular) < ink_sum(&semibold) && ink_sum(&semibold) < ink_sum(&between) && ink_sum(&between) < ink_sum(&bold), "the ink does not grow with the weight");
        let by_weight = r.render(&boxed(VARIABLE, FontStyle::Regular, |tb| tb.font_weight = Some(650)), W, H);
        assert!(by_weight == r.render(&at_axes(&[("wght", 650.0)]), W, H), "a weight of 650 is not the weight axis at 650");
        // Another axis than the weight's draws differently too.
        assert_ne!(r.render(&at_axes(&[("opsz", 32.0)]), W, H), r.render(&at_axes(&[]), W, H));
    }

    /// A run's axes are set on top of the box's: a word in a box at
    /// weight 400 drawn at 800 is drawn as the box at 800 draws it.
    #[test]
    fn a_runs_axes_draw_its_glyphs_at_them() {
        let mut r = with_variable_family();
        let td = boxed(VARIABLE, FontStyle::Regular, |tb| tb.style_runs = vec![run(10, 15, |r| r.font_axes = vec![("wght".into(), 800.0)])]);
        let mixed = r.render_layout(&td, W, H);
        let heavy = r.render_layout(&at_axes(&[("wght", 800.0)]), W, H);
        let light = r.render_layout(&at_axes(&[]), W, H);
        for i in 10..15 {
            assert_eq!(ink(&mixed, i), ink(&heavy, i), "{:?} is not at weight 800", mixed.glyph_parents[i]);
        }
        assert_eq!(ink(&mixed, 4), ink(&light, 4));
    }

    #[test]
    fn glyph_hash_changes_on_weight_and_axes() {
        let plain = boxed("Inter", FontStyle::Regular, |_| {});
        let hash = |td: &TitleData| glyph_layout_hash(td, W, H);
        let weighted = boxed("Inter", FontStyle::Regular, |tb| tb.font_weight = Some(700));
        let axes = boxed("Inter", FontStyle::Regular, |tb| tb.font_axes = vec![("wght".into(), 700.0)]);
        let run_weight = boxed("Inter", FontStyle::Regular, |tb| tb.style_runs = vec![run(0, 3, |r| r.font_weight = Some(300))]);
        let run_axes = boxed("Inter", FontStyle::Regular, |tb| tb.style_runs = vec![run(0, 3, |r| r.font_axes = vec![("wdth".into(), 80.0)])]);
        let no_runs_style = boxed("Inter", FontStyle::Regular, |tb| tb.style_runs = vec![run(0, 3, |_| {})]);
        for (name, td) in [("weight", weighted), ("axes", axes)] {
            assert_ne!(hash(&plain), hash(&td), "{name}");
        }
        for (name, td) in [("run weight", run_weight), ("run axes", run_axes)] {
            assert_ne!(hash(&no_runs_style), hash(&td), "{name}");
        }
    }
}

#[cfg(test)]
mod gradient_fill_tests {
    //! #1509: text and shapes fill with gradients of any number of stops,
    //! linear at an angle or radial, mixed in linear light; the old
    //! two-colour pair draws as it did.
    use super::*;
    use klipp_core::fill_gradient::{mix_linear, GradientRamp};
    use klipp_timeline::{GradientKind, GradientStop, ShapePreset, TextBox, TextGradient, TitlePosition, VectorPath};

    const W: u32 = 640;
    const H: u32 = 360;
    const RED: [u8; 4] = [255, 0, 0, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];

    fn fnv(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
    }

    fn stops(list: &[(f32, [u8; 4])]) -> Vec<GradientStop> {
        list.iter().map(|&(position, color)| GradientStop { position, color }).collect()
    }

    fn text(gradient: Option<TextGradient>, body: &str) -> TitleData {
        let tb = TextBox { text: body.into(), font_size: 120.0, gradient, ..Default::default() };
        TitleData::single_text_fx(tb, LayerFx::default())
    }

    /// A shape filling the box from a quarter to three quarters across
    /// and down, unturned.
    fn shape(kind: ShapeKind, gradient: Option<TextGradient>) -> TitleData {
        let mut td = TitleData::default();
        td.objects.clear();
        td.add_object(
            TitleObjectContent::Shape(Shape { kind, fill: Some([9, 9, 9, 255]), gradient, stroke: None, fit: None }),
            klipp_timeline::ObjectTransform {
                center: TitlePosition { x: 0.5, y: 0.5 },
                size: TitlePosition { x: 0.5, y: 0.5 },
                ..klipp_timeline::default_text_transform()
            },
        );
        td
    }

    fn rect(gradient: TextGradient) -> TitleData {
        shape(ShapeKind::Rectangle { corner_radius: 0.0 }, Some(gradient))
    }

    /// How far a sampled pixel may be from a stop's colour: its centre
    /// is half a pixel from the stop, and in linear light the first
    /// step away from a zero channel is a big one in sRGB.
    const EDGE: u8 = 32;

    fn px(buf: &[u8], x: u32, y: u32) -> [u8; 4] {
        let i = ((y * W + x) * 4) as usize;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    }

    /// The pixel whose centre is at `(u, v)` of the shape's box.
    fn at(buf: &[u8], u: f32, v: f32) -> [u8; 4] {
        let (bw, bh) = (W as f32 * 0.5, H as f32 * 0.5);
        let x = (W as f32 * 0.25 + u * bw).min(W as f32 * 0.75 - 0.5);
        let y = (H as f32 * 0.25 + v * bh).min(H as f32 * 0.75 - 0.5);
        px(buf, x as u32, y as u32)
    }

    fn near(a: [u8; 4], b: [u8; 4], tol: u8, what: &str) {
        assert!(a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= tol), "{what}: {a:?} is not {b:?}");
    }

    /// Taken on main before #1509: the two-colour pair, translucent,
    /// switched off, and solid shape fills beside a text, draw byte for
    /// byte as they did. Built from the saved form, as an old file has it.
    #[test]
    fn old_two_colour_gradients_draw_as_before() {
        const BEFORE: [u64; 8] = [
            0x4d8c_db0e_2a65_99ae, 0x560c_05b5_cf8b_48a0, 0xf17e_ab77_ad95_545a, 0x7500_53c7_7e64_92af,
            0x410f_8ab4_0d83_ad58, 0x0a04_66db_ed53_3706, 0x434b_00d9_3b42_4aec, 0x0687_5e9a_360f_fd0e,
        ];
        let saved = |json: &str, body: &str| {
            let g: Option<TextGradient> = serde_json::from_str(json).unwrap();
            text(g, body)
        };
        let mut shapes = saved(r#"{"top_color":[255,200,50,255],"bottom_color":[20,50,255,200],"enabled":true}"#, "Over");
        shapes.add_object(
            TitleObjectContent::Shape(serde_json::from_str::<Shape>(r#"{"kind":{"Rectangle":{"corner_radius":12.0}},"fill":[200,40,40,220],"stroke":{"color":[255,255,255,255],"width":3.0}}"#).unwrap()),
            klipp_timeline::ObjectTransform { center: TitlePosition { x: 0.3, y: 0.7 }, size: TitlePosition { x: 0.3, y: 0.2 }, rotation: 12.0, ..klipp_timeline::default_text_transform() },
        );
        let mut p = VectorPath::default();
        p.pose.fill = [0.1, 0.8, 0.3, 0.9];
        shapes.add_object(
            TitleObjectContent::Shape(Shape { kind: ShapeKind::Path(p), fill: None, gradient: None, stroke: None, fit: None }),
            klipp_timeline::ObjectTransform { center: TitlePosition { x: 0.7, y: 0.4 }, size: TitlePosition { x: 0.25, y: 0.3 }, ..klipp_timeline::default_text_transform() },
        );
        let cases = [
            saved(r#"{"top_color":[255,0,0,255],"bottom_color":[0,0,255,255],"enabled":true}"#, "GRADIENT"),
            saved(r#"{"top_color":[255,255,255,128],"bottom_color":[10,200,30,255]}"#, "Two lines\nof text"),
            saved(r#"{"top_color":[255,0,0,255],"bottom_color":[0,0,255,255],"enabled":false}"#, "Off"),
            shapes,
        ];
        let mut got = Vec::new();
        for td in &cases {
            for (w, h) in [(640, 360), (1920, 1080)] {
                got.push(fnv(&TitleRenderer::new().render(td, w, h)));
            }
        }
        assert_eq!(got, BEFORE);
    }

    #[test]
    fn a_pair_saves_as_it_always_has() {
        let json = r#"{"top_color":[255,0,0,255],"bottom_color":[0,0,255,255],"enabled":true}"#;
        let g: TextGradient = serde_json::from_str(json).unwrap();
        assert!(g.is_pair());
        assert_eq!(serde_json::to_string(&g).unwrap(), json);
        // Two stops at the ends, top to bottom, are the pair.
        assert_eq!(TextGradient::with_stops(&stops(&[(1.0, BLUE), (0.0, RED)]), GradientKind::default(), true), g);
    }

    #[test]
    fn three_stops_land_where_they_are_set_on_a_shape() {
        let three = stops(&[(0.0, RED), (0.5, GREEN), (1.0, BLUE)]);
        let fill = |angle_deg: f32| TitleRenderer::new().render(&rect(TextGradient::with_stops(&three, GradientKind::Linear { angle_deg }, true)), W, H);
        // Top to bottom.
        let down = fill(0.0);
        near(at(&down, 0.3, 0.0), RED, EDGE, "0 deg, top");
        near(at(&down, 0.7, 0.5), GREEN, EDGE, "0 deg, middle");
        near(at(&down, 0.3, 1.0), BLUE, EDGE, "0 deg, bottom");
        // Left to right.
        let across = fill(90.0);
        near(at(&across, 0.0, 0.8), RED, EDGE, "90 deg, left");
        near(at(&across, 0.5, 0.2), GREEN, EDGE, "90 deg, middle");
        near(at(&across, 1.0, 0.5), BLUE, EDGE, "90 deg, right");
        // Corner to corner.
        let diag = fill(45.0);
        near(at(&diag, 0.0, 0.0), RED, EDGE, "45 deg, top left");
        near(at(&diag, 0.5, 0.5), GREEN, EDGE, "45 deg, middle");
        near(at(&diag, 1.0, 1.0), BLUE, EDGE, "45 deg, bottom right");
        // The other corners are both half way.
        assert_eq!(at(&diag, 1.0, 0.0)[1], at(&diag, 0.0, 1.0)[1]);
    }

    #[test]
    fn a_radial_gradient_runs_from_the_centre_to_the_radius_on_a_shape() {
        let g = TextGradient::with_stops(&stops(&[(0.0, RED), (1.0, BLUE)]), GradientKind::Radial { centre: [0.5, 0.5], radius: 0.3 }, true);
        let boxed = TitleRenderer::new().render(&rect(g.clone()), W, H);
        near(at(&boxed, 0.5, 0.5), RED, EDGE, "the centre");
        near(at(&boxed, 0.8, 0.5), BLUE, EDGE, "the radius across");
        near(at(&boxed, 0.5, 0.2), BLUE, EDGE, "the radius up");
        near(at(&boxed, 0.95, 0.95), BLUE, 0, "past the radius");
        // A path takes it over its own box, as the rectangle does.
        let ellipse = VectorPath::preset(ShapePreset::Ellipse, [0.0, 1.0, 0.0, 1.0], [0.0; 4], 0.0);
        let path = TitleRenderer::new().render(&shape(ShapeKind::Path(ellipse), Some(g)), W, H);
        near(at(&path, 0.5, 0.5), RED, EDGE, "the ellipse's centre");
        near(at(&path, 0.8, 0.5), BLUE, EDGE, "the ellipse at the radius");
        assert_eq!(at(&path, 0.02, 0.02)[3], 0, "the ellipse leaves its box's corners");
    }

    #[test]
    fn stops_meet_in_linear_light() {
        let g = TextGradient::with_stops(&stops(&[(0.0, RED), (1.0, BLUE)]), GradientKind::Linear { angle_deg: 90.0 }, true);
        let mid = at(&TitleRenderer::new().render(&rect(g), W, H), 0.5, 0.5);
        // Half of each in linear light encodes as 188; an sRGB mix would be 128.
        near(mid, [188, 0, 188, 255], 2, "red to blue, half way");
        near(mid, mix_linear(RED, BLUE, 0.5), 2, "the linear-light mix");
    }

    /// Every opaque pixel of a text's face against its colour at the
    /// point of the text block it is at, worked out here.
    fn text_follows(g: &TextGradient, t_at: impl Fn(f32, f32, f32, f32) -> f32) -> usize {
        let td = text(Some(g.clone()), "HHHHH\nHHHHH");
        let mut r = TitleRenderer::new();
        let layout = r.render_layout(&td, W, H);
        let buf = r.render(&td, W, H);
        let (bw, bh) = (layout.text_right - layout.text_left, layout.text_bottom - layout.text_top);
        let ramp = GradientRamp::new(&g.stops());
        let mut seen = 0;
        for y in 0..H {
            for x in 0..W {
                let p = px(&buf, x, y);
                if p[3] < 255 {
                    continue;
                }
                let (u, v) = ((x as f32 + 0.5 - layout.text_left) / bw, (y as f32 + 0.5 - layout.text_top) / bh);
                near(p, ramp.at(t_at(u, v, bw, bh).clamp(0.0, 1.0)), 1, &format!("({x}, {y})"));
                seen += 1;
            }
        }
        seen
    }

    #[test]
    fn text_takes_stops_at_an_angle_and_radial() {
        let three = stops(&[(0.0, RED), (0.5, GREEN), (1.0, BLUE)]);
        let linear = |angle_deg| TextGradient::with_stops(&three, GradientKind::Linear { angle_deg }, true);
        assert!(text_follows(&linear(0.0), |_, v, _, _| v) > 1000);
        assert!(text_follows(&linear(90.0), |u, _, _, _| u) > 1000);
        assert!(text_follows(&linear(45.0), |u, v, w, h| (u * w + v * h) / (w + h)) > 1000);
        let radial = TextGradient::with_stops(&three, GradientKind::Radial { centre: [0.5, 0.5], radius: 0.5 }, true);
        assert!(text_follows(&radial, |u, v, _, _| ((u - 0.5).powi(2) + (v - 0.5).powi(2)).sqrt() / 0.5) > 1000);
        // Across the text, the stops' colours are all there, set in
        // from the block's sides, where the letters' ink is.
        let inset = TextGradient::with_stops(&stops(&[(0.15, RED), (0.5, GREEN), (0.85, BLUE)]), GradientKind::Linear { angle_deg: 90.0 }, true);
        let buf = TitleRenderer::new().render(&text(Some(inset), "HHHHH\nHHHHH"), W, H);
        for c in [RED, GREEN, BLUE] {
            assert!(buf.chunks_exact(4).any(|p| p[3] == 255 && p.iter().zip(c).all(|(a, b)| a.abs_diff(b) <= EDGE)), "{c:?} is drawn");
        }
    }

    #[test]
    fn a_changed_stop_is_not_served_from_the_cache() {
        let mut r = TitleRenderer::new();
        let set = |mid: [u8; 4]| TextGradient::with_stops(&stops(&[(0.0, RED), (0.5, mid), (1.0, BLUE)]), GradientKind::Linear { angle_deg: 90.0 }, true);
        let (a, b) = (set(GREEN), set([255, 255, 255, 255]));
        assert_ne!(r.render(&text(Some(a.clone()), "HHHH"), W, H), r.render(&text(Some(b.clone()), "HHHH"), W, H));
        assert_ne!(r.render(&rect(a), W, H), r.render(&rect(b), W, H));
    }
}

#[cfg(test)]
mod wide_blur_tests {
    /// #1725: a wide blur is three boxes each way, as near the exact
    /// Gaussian as the eye tells, its ink where the Gaussian's is.
    #[test]
    fn a_wide_blur_is_near_the_gaussian_and_fast() {
        let (w, h) = (40usize, 30usize);
        let mut glyph = vec![0u8; w * h];
        for y in 8..22 {
            for x in 10..30 {
                glyph[y * w + x] = 255;
            }
        }
        let sigma = 20.0;
        let (boxed, bw, bh) = super::gaussian_blur_expand(&glyph, w, h, sigma);
        // The exact kernel, as below the threshold, scaled up.
        let exact = {
            let radius = (sigma * 2.5).ceil() as usize;
            assert_eq!((bw, bh), (w + 2 * radius, h + 2 * radius), "the same room");
            let k: Vec<f32> = (0..=2 * radius).map(|i| { let x = i as f32 - radius as f32; (-x * x / (2.0 * sigma * sigma)).exp() }).collect();
            let sum: f32 = k.iter().sum();
            let at = |x: i64, y: i64| if x < radius as i64 || y < radius as i64 || x >= (w + radius) as i64 || y >= (h + radius) as i64 { 0.0 } else { glyph[(y as usize - radius) * w + (x as usize - radius)] as f32 };
            let mut out = vec![0.0f32; bw * bh];
            for y in 0..bh {
                for x in 0..bw {
                    let mut acc = 0.0;
                    for (j, ky) in k.iter().enumerate() {
                        for (i, kx) in k.iter().enumerate() {
                            acc += at(x as i64 + i as i64 - radius as i64, y as i64 + j as i64 - radius as i64) * kx * ky;
                        }
                    }
                    out[y * bw + x] = acc / (sum * sum);
                }
            }
            out
        };
        let worst = boxed.iter().zip(&exact).map(|(a, b)| (*a as f32 - b).abs()).fold(0.0, f32::max);
        assert!(worst < 12.0, "within a few levels of the Gaussian: {worst}");
        let ink = |v: &mut dyn Iterator<Item = f32>| v.sum::<f32>();
        let (a, b) = (ink(&mut boxed.iter().map(|&v| v as f32)), ink(&mut exact.iter().copied()));
        assert!((a / b - 1.0).abs() < 0.02, "the same ink: {a} vs {b}");
    }
}

#[cfg(test)]
mod vector_cache_tests;

#[cfg(test)]
mod fading_layers_tests {
    use super::*;

    #[test]
    fn keyed_opacity_matches_raster_without_rebuilding_layers() {
        use klipp_timeline::{LayerFx, Motion, MotionKind, TextBox, TitleMotion};
        let mut title = TitleData::single_text_fx(
            TextBox { text: "Opacity".into(), font_size: 120.0, ..Default::default() },
            LayerFx { motion: TitleMotion::enter(Motion::new(MotionKind::Fade).duration(0.5)), ..Default::default() },
        );
        title.objects[0].opacity = 0.15; // keys replace, rather than multiply, this value
        title.objects[0].upsert_opacity_keyframe(0.0, 0.0);
        title.objects[0].upsert_opacity_keyframe(1.0, 1.0);
        title.objects[0].upsert_opacity_keyframe(2.0, 0.0);
        for count in [1, 2] {
            if count == 2 {
                let mut other = title.objects[0].clone();
                other.id = klipp_timeline::TitleObjectId(1);
                other.transform.center.x += 0.02;
                other.upsert_opacity_keyframe(1.0, 0.4);
                title.objects.push(other);
            }
            let mut cached = TitleRenderer::new();
            for t in [0.0, 0.25, 1.0, 2.0, 0.5] {
                let mut evaluated = title.clone();
                for obj in &mut evaluated.objects {
                    obj.opacity = obj.evaluated_opacity(t as f64);
                    obj.opacity_keyframes.clear();
                }
                let expected = TitleRenderer::new().render_stack(&evaluated, 640, 360, None, Some(t));
                let actual = cached.render_with_window_stack(&title, 640, 360, None, t, None);
                let worst = expected.chunks_exact(4).zip(actual.chunks_exact(4)).map(|(a,b)| {
                    (0..3).map(|i| (a[i]*a[3]-b[i]*b[3]).abs()/255.0)
                        .fold((a[3]-b[3]).abs(), f32::max)
                }).fold(0.0, f32::max);
                assert!(worst <= 1.5, "{count} layers at {t}: {worst}");
                assert_eq!(cached.resting_renders(), count);
            }
        }
        title.objects[0].transform.center_x_keyframes = title.objects[0].opacity_keyframes.clone();
        assert!(matches!(TitleRenderer::new().frame_with_window(&title, 640, 360, None, 0.5, None), TitleFrame::Rendered { .. }));
    }

    #[test]
    fn fading_layers_preserve_the_original_raster_composition() {
        use klipp_timeline::{LayerFx, Motion, MotionKind, TextBox, TitleMotion};
        let mut title = TitleData::single_text_fx(
            TextBox { text: "Over".into(), font_size: 120.0, color: [240, 40, 80, 255], ..Default::default() },
            LayerFx { motion: TitleMotion::both(Motion::new(MotionKind::Fade).duration(1.0), Motion::exit(MotionKind::Fade).duration(1.0)), ..Default::default() },
        );
        let mut second = title.objects[0].clone();
        second.id = klipp_timeline::TitleObjectId(1);
        second.opacity = 0.6;
        second.transform.center.x += 0.015;
        if let TitleObjectContent::TextBox(tb) = &mut second.content { tb.color = [30, 180, 240, 255]; }
        title.objects.push(second);
        let mut cached = TitleRenderer::new();
        for t in [0.1, 0.5, 2.0, 3.6, 4.0, 0.3] {
            let mut reference = TitleRenderer::new();
            reference.motion_window = Some(4.0);
            // Call the established raster compositor directly, avoiding
            // frame_at so the reference cannot use the new Layers route.
            let expected = reference.render_stack(&title, 640, 360, None, Some(t));
            let actual = cached.render_with_window_stack(&title, 640, 360, None, t, Some(4.0));
            let worst = expected.chunks_exact(4).zip(actual.chunks_exact(4)).map(|(a, b)| {
                let colour = (0..3).map(|i| (a[i] * a[3] - b[i] * b[3]).abs() / 255.0).fold(0.0, f32::max);
                colour.max((a[3] - b[3]).abs())
            }).fold(0.0, f32::max);
            assert!(worst <= 1.5, "at {t}: cached layers differ from raster by {worst} levels");
        }
    }
}
