//! Reusable glyph coverage separated from the layout and fill of a frame.
use super::*;
use std::{collections::VecDeque, sync::Arc};

pub struct GlyphDraw {
    /// Preserve a CBDT glyph's own RGBA instead of tinting its coverage.
    pub native_colour: bool,
    /// A retained premultiplied strike to resize on GPU; otherwise already sized.
    pub target_size: Option<[u32;2]>,
    pub id: u64,
    pub pixels: Arc<BoxRaster>,
    pub origin: [i32; 2],
    /// The glyph's posed alpha, as an outline draw's (#2203).
    pub alpha: f32,
    /// The frame rows the posed glyph may cover; a mask motion's resting box.
    pub clip_y: [i32; 2],
}
#[derive(Clone)]
pub struct OutlineDraw {
    pub id: u64,
    pub edges: Arc<Vec<[f32; 4]>>,
    pub origin: [i32; 2],
    pub size: [u32; 2],
    /// Scale from the cached 2048px-em outline, then translate to bitmap space.
    pub placement: [f32; 4],
    pub alpha: f32,
    pub clip_y: [i32; 2],
    pub source_size: [u32; 2],
    pub blur: f32,
    pub radians: f32,
    /// Path placement uses the CPU path rasterizer's nearest sampling.
    pub path: bool,
    pub color: Option<[u8; 4]>,
}
pub struct TextureDraw {
    pub id: u64,
    pub pixels: Arc<BoxRaster>,
    pub fill: klipp_timeline::TextureFill,
}
pub struct GlyphSource {
    pub glyphs: Vec<GlyphDraw>,
    pub outlines: Vec<OutlineDraw>,
    pub face_draws: Vec<OutlineDraw>,
    pub face_stripes: Vec<([u32; 4], [u8; 4])>,
    pub(super) face_incomplete: bool,
    pub roi: [u32; 4],
    pub color: [u8; 4],
    pub fx: LayerFx,
    pub gradient: Option<klipp_timeline::TextGradient>,
    pub pattern: Option<klipp_timeline::TextPattern>,
    /// A metal face (#2706), and the text's size in frame pixels, which
    /// its bevel is a fraction of.
    pub material: Option<klipp_core::title_material::TitleMaterial>,
    pub font_px: f32,
    pub bounds: [f32; 4],
    pub texture: Option<TextureDraw>,
    pub prepared: usize,
    pub rerasterized: bool,
    pub outline_cutoff: i32,
    pub write_cutoff: i32,
    pub stripes: Vec<[u32; 4]>,
    pub backing: Option<([u32; 4], [u8; 4])>,
}
struct CachedGlyph {
    native_colour: bool,
    key: fontdue::layout::GlyphRasterConfig,
    id: u64,
    pixels: Arc<BoxRaster>,
}
struct CachedOutline {
    font: usize,
    glyph: u16,
    id: u64,
    edges: Arc<Vec<[f32; 4]>>,
}
pub(super) struct GlyphCache {
    entries: VecDeque<CachedGlyph>,
    outlines: VecDeque<CachedOutline>,
    strikes: VecDeque<CachedGlyph>,
    recent_strikes: VecDeque<fontdue::layout::GlyphRasterConfig>,
    next_id: u64,
    recent: VecDeque<fontdue::layout::GlyphRasterConfig>,
}
impl Default for GlyphCache {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
            outlines: VecDeque::new(),
            strikes: VecDeque::new(),
            recent_strikes: VecDeque::new(),
            next_id: 1 << 63,
            recent: VecDeque::new(),
        }
    }
}
impl GlyphCache {
    pub(super) fn clear(&mut self) {
        self.entries.clear();
        self.outlines.clear();
        self.strikes.clear();
        self.recent_strikes.clear();
        self.recent.clear();
    }
    pub(super) fn layout(
        &mut self,
        fonts: &[Face],
        glyphs: &[fontdue::layout::GlyphPosition],
        offset: [f32; 2],
        roi: [u32; 4],
        color: [u8; 4],
        fx: &LayerFx,
        geometry: bool,
    ) -> Option<GlyphSource> {
        if glyphs.len() > 4096 {
            return None;
        }
        if geometry {
            return self.outline_layout(fonts, glyphs, offset, roi, color, fx);
        }
        // Admit the entire unique working set before rasterizing anything.
        // Pin it for this layout so an LRU hit cannot evict a later glyph.
        let mut keys = std::collections::HashSet::new();
        let mut bytes = 0usize;
        for g in glyphs.iter().filter(|g| g.width > 0 && g.height > 0) {
            if keys.insert(g.key) {
                bytes = bytes.checked_add(g.width.checked_mul(g.height)?.checked_mul(8)?)?;
            }
        }
        if keys.len() > 512 || bytes > 64 << 20 {
            return None;
        }
        let mut draws = Vec::new();
        let mut prepared = 0;
        let mut rerasterized = false;
        for g in glyphs.iter().filter(|g| g.width > 0 && g.height > 0) {
            let cached = if let Some(i) = self.entries.iter().position(|c| c.key == g.key) {
                self.entries.remove(i)?
            } else {
                rerasterized |= self.recent.contains(&g.key);
                if self.recent.len() >= 4096 {
                    self.recent.pop_front();
                }
                self.recent.push_back(g.key);
                let native=fonts[g.font_index].colour_glyph(g.key.glyph_index,g.key.px);
                let native_colour=native.is_some();
                let bytes=if let Some(bitmap)=native {
                    if (bitmap.width,bitmap.height)!=(g.width,g.height) {return None;}
                    bitmap.rgba.as_ref().clone()
                }else{
                    let (_,coverage)=fonts[g.font_index].coverage(g.key);
                    if coverage.len()!=g.width*g.height {return None;}
                    coverage.into_iter().flat_map(|alpha|[255,255,255,alpha]).collect()
                };
                let pixels = Arc::new(BoxRaster::from_bytes(
                    &bytes,
                    (g.width as u32, g.height as u32),
                    (0, g.height as u32),
                    (0, g.width as u32),
                ));
                let id = self.next_id;
                self.next_id = self.next_id.wrapping_add(1);
                prepared += 1;
                CachedGlyph {
                    native_colour,
                    key: g.key,
                    id,
                    pixels,
                }
            };
            draws.push(GlyphDraw {
                native_colour:cached.native_colour,
                target_size:None,
                id: cached.id,
                pixels: cached.pixels.clone(),
                origin: [(g.x + offset[0]) as i32, (g.y + offset[1]) as i32],
                alpha: 1.0,
                clip_y: [i32::MIN, i32::MAX],
            });
            let bytes = cached.pixels.byte_size();
            if bytes <= 64 << 20 {
                while self.entries.len() >= 512
                    || self
                        .entries
                        .iter()
                        .map(|c| c.pixels.byte_size())
                        .sum::<usize>()
                        + bytes
                        > 64 << 20
                {
                    let victim = self.entries.iter().position(|c| !keys.contains(&c.key))?;
                    self.entries.remove(victim);
                }
                self.entries.push_back(cached);
            }
        }
        Some(GlyphSource {
            glyphs: draws,
            outlines: Vec::new(),
            face_draws: Vec::new(),
            face_stripes: Vec::new(),
            face_incomplete: false,
            roi,
            color,
            fx: fx.clone(),
            prepared,
            rerasterized,
            outline_cutoff: i32::MAX,
            write_cutoff: i32::MAX,
            stripes: Vec::new(),
            backing: None,
            gradient: None,
            pattern: None,
            material: None,
            font_px: 0.0,
            bounds: [0.0; 4],
            texture: None,
        })
    }
}
impl GlyphCache {
    pub(super) fn bitmap_size_layout(
        &mut self,
        fonts: &[Face],
        glyphs: &[fontdue::layout::GlyphPosition],
        offset: [f32; 2],
        roi: [u32; 4],
        color: [u8; 4],
        fx: &LayerFx,
    ) -> Option<GlyphSource> {
        if glyphs.len() > 4096 {
            return None;
        }
        let mut ordinary = Vec::new();
        let mut native = Vec::new();
        let mut keys = std::collections::HashSet::new();
        let mut bytes = 0usize;
        for g in glyphs.iter().filter(|g| g.width > 0 && g.height > 0) {
            if let Some(source) = fonts[g.font_index]
                .font
                .colour_source(g.key.glyph_index, g.key.px)
            {
                // The strike cache uses the table index, not the requested size.
                let mut key = g.key;
                key.px = (source.strike.index + 1) as f32;
                if keys.insert(key) {
                    bytes = bytes.checked_add(
                        source
                            .strike
                            .width
                            .checked_mul(source.strike.height)?
                            .checked_mul(8)?,
                    )?;
                }
                native.push((g, key, source));
            } else {
                ordinary.push(*g);
            }
        }
        let ordinary_keys: std::collections::HashSet<_> = ordinary
            .iter()
            .map(|g| (g.key.font_hash, g.key.glyph_index))
            .collect();
        if keys.len() + ordinary_keys.len() > 512 || bytes > 64 << 20 {
            return None;
        }
        let mut source = self.outline_layout(fonts, &ordinary, offset, roi, color, fx)?;
        for (g, key, bitmap) in native {
            let cached = if let Some(i) = self.strikes.iter().position(|c| c.key == key) {
                self.strikes.remove(i)?
            } else {
                source.rerasterized |= self.recent_strikes.contains(&key);
                if self.recent_strikes.len() >= 4096 {
                    self.recent_strikes.pop_front();
                }
                self.recent_strikes.push_back(key);
                let strike = bitmap.strike;
                let pixels = Arc::new(BoxRaster::from_bytes(
                    &strike.rgba,
                    (strike.width as u32, strike.height as u32),
                    (0, strike.height as u32),
                    (0, strike.width as u32),
                ));
                let id = self.next_id;
                self.next_id = self.next_id.wrapping_add(1);
                source.prepared += 1;
                CachedGlyph {
                    key,
                    id,
                    pixels,
                    native_colour: true,
                }
            };
            source.glyphs.push(GlyphDraw {
                native_colour: true,
                target_size: Some([g.width as u32, g.height as u32]),
                id: cached.id,
                pixels: Arc::clone(&cached.pixels),
                origin: [(g.x + offset[0]) as i32, (g.y + offset[1]) as i32],
                alpha: 1.0,
                clip_y: [i32::MIN, i32::MAX],
            });
            while self.strikes.len() >= 512
                || self
                    .strikes
                    .iter()
                    .map(|c| c.pixels.byte_size())
                    .sum::<usize>()
                    + cached.pixels.byte_size()
                    > 64 << 20
            {
                let victim = self.strikes.iter().position(|c| !keys.contains(&c.key))?;
                self.strikes.remove(victim);
            }
            self.strikes.push_back(cached);
        }
        Some(source)
    }

    fn outline_layout(
        &mut self,
        fonts: &[Face],
        glyphs: &[fontdue::layout::GlyphPosition],
        offset: [f32; 2],
        roi: [u32; 4],
        color: [u8; 4],
        fx: &LayerFx,
    ) -> Option<GlyphSource> {
        let keys: std::collections::HashSet<_> = glyphs
            .iter()
            .filter(|g| g.width > 0 && g.height > 0)
            .map(|g| (g.key.font_hash, g.key.glyph_index))
            .collect();
        if keys.len() > 512 {
            return None;
        }
        let mut outlines = Vec::new();
        let mut prepared = 0;
        for g in glyphs.iter().filter(|g| g.width > 0 && g.height > 0) {
            if g.key.px == 0.0
                || fonts[g.font_index]
                    .colour_glyph(g.key.glyph_index, g.key.px)
                    .is_some()
            {
                return None;
            }
            let font = fonts[g.font_index].font;
            let cached = if let Some(i) = self
                .outlines
                .iter()
                .position(|c| c.font == g.key.font_hash && c.glyph == g.key.glyph_index)
            {
                self.outlines.remove(i)?
            } else {
                let edges = font.coverage_outline(g.key.glyph_index)?;
                let id = self.next_id;
                self.next_id = self.next_id.wrapping_add(1);
                prepared += 1;
                CachedOutline {
                    font: g.key.font_hash,
                    glyph: g.key.glyph_index,
                    id,
                    edges: Arc::new(edges),
                }
            };
            let metrics = font.metrics_indexed(g.key.glyph_index, g.key.px.abs());
            outlines.push(OutlineDraw {
                id: cached.id,
                edges: cached.edges.clone(),
                origin: [(g.x + offset[0]) as i32, (g.y + offset[1]) as i32],
                size: [g.width as u32, g.height as u32],
                source_size: [g.width as u32, g.height as u32],
                blur: 0.0,
                radians: 0.0,
                path: false,
                color: None,
                alpha: 1.0,
                clip_y: [i32::MIN, i32::MAX],
                placement: [
                    g.key.px.abs() / 2048.0,
                    -(metrics.xmin as f32),
                    metrics.height as f32 + metrics.ymin as f32,
                    if g.key.px < 0.0 { 1.0 } else { 0.0 },
                ],
            });
            while self.outlines.len() >= 512
                || self
                    .outlines
                    .iter()
                    .map(|c| c.edges.len() * 16)
                    .sum::<usize>()
                    + cached.edges.len() * 16
                    > 32 << 20
            {
                let victim = self
                    .outlines
                    .iter()
                    .position(|c| !keys.contains(&(c.font, c.glyph)))?;
                self.outlines.remove(victim);
            }
            self.outlines.push_back(cached);
        }
        Some(GlyphSource {
            glyphs: Vec::new(),
            outlines,
            face_draws: Vec::new(),
            face_stripes: Vec::new(),
            face_incomplete: false,
            roi,
            color,
            fx: fx.clone(),
            prepared,
            rerasterized: false,
            outline_cutoff: i32::MAX,
            write_cutoff: i32::MAX,
            stripes: Vec::new(),
            backing: None,
            gradient: None,
            pattern: None,
            material: None,
            font_px: 0.0,
            bounds: [0.0; 4],
            texture: None,
        })
    }
}

/// Whether the glyph route lays `title` out (#2335): a lone text it can
/// draw, whose layout or styles change over time, or that is still. One
/// test for the route and for the warm-up thread, which has no resting
/// raster to make for a title drawn here.
pub fn takes_glyph_route(title: &TitleData, scene: Option<&TitleData>) -> bool {
    let [o] = title.objects.as_slice() else {
        return false;
    };
    let TitleObjectContent::TextBox(tb) = &o.content else {
        return false;
    };
    let linked = matches!(tb.text_path, Some(klipp_timeline::TextPath::Layer { .. }));
    let fx = o.fx();
    if !o.visible
        || o.opacity != 1.0
        || !o.opacity_keyframes.is_empty()
        || title.mixes_layers()
        || matches!(title.background, TitleBackground::FullFrame { .. })
        || (!linked && (o.transform.rotation != 0.0 || !o.transform.rotation_keyframes.is_empty()))
        || tb.three_d.as_ref().is_some_and(|d| d.enabled)
        || tb.extrusion.is_some()
        || tb.glitch_intensity > 0.0
        || tb.chromatic_aberration_px > 0.0
        || tb.char_animation.as_ref().is_some_and(|c| c.enabled)
        || !fx.edge_fade.is_none()
        // A blurred layer whose drawing moves (an entrance, an exit, a
        // loop) is drawn at each of the shutter's instants by the source
        // route. One that moves only by its keys is drawn once, as here,
        // and the shutter blurs where it is placed (#3160).
        || (fx.motion_blur.is_some() && (fx.motion.enter.is_some() || fx.motion.exit.is_some() || !fx.loops.is_empty()))
    {
        return false;
    }
    let raster_animator = fx.animators.iter().any(|a| a.enabled && (a.props.blur != 0.0 || a.props.paints()));
    let stagger_blur = fx.motion.enter.iter().chain(fx.motion.exit.iter()).any(|m|m.kind.blurs() && m.stagger!=klipp_timeline::StaggerUnit::None);
    // Text on a still linked path that only moves is the resting sprite
    // posed glyph by glyph, as the CPU draws it; translating it in the
    // path layout would round each glyph's centre another way.
    let still_link = match &tb.text_path {
        Some(klipp_timeline::TextPath::Layer { layer, offset_keyframes, .. }) => offset_keyframes.is_empty()
            && scene.unwrap_or(title).object(*layer).is_some_and(|l| {
                let t = &l.transform;
                t.center_x_keyframes.is_empty() && t.center_y_keyframes.is_empty() && t.size_x_keyframes.is_empty()
                    && t.size_y_keyframes.is_empty() && t.rotation_keyframes.is_empty()
                    && t.anchor_x_keyframes.is_empty() && t.anchor_y_keyframes.is_empty()
                    && !matches!(&l.content, TitleObjectContent::Shape(s) if matches!(&s.kind,
                        klipp_timeline::ShapeKind::Path(p) if !p.keyframes.is_empty() || p.trim.as_ref().is_some_and(|t| !t.keyframes.is_empty())))
            }),
        _ => false,
    };
    let translates = fx.motion.enter.iter().chain(fx.motion.exit.iter())
        .any(|m| !matches!(m.kind, klipp_timeline::MotionKind::Fade | klipp_timeline::MotionKind::Flicker | klipp_timeline::MotionKind::Typewriter))
        || fx.animators.iter().any(|a| a.enabled && (a.props.dx != 0.0 || a.props.dy != 0.0));
    let changes = (linked && !(still_link && translates)) || raster_animator || stagger_blur || tb.gradient.as_ref().is_some_and(|g| g.has_keyframes())
        || !o.transform.size_x_keyframes.is_empty()
        || !o.transform.size_y_keyframes.is_empty()
        // A keyed position lays the text out afresh where it is now,
        // as the CPU raster does (#2335).
        || !o.transform.center_x_keyframes.is_empty()
        || !o.transform.center_y_keyframes.is_empty()
        // Keyed styles under a live motion: the glyphs posed and the
        // styles drawn from them at this time, as the CPU raster
        // does; still, `style_source` keeps them (#2335).
        || (fx.motion.is_active() && fx.styles_keyed())
        || !tb.font_size_keyframes.is_empty()
        || !tb.color_keyframes.is_empty()
        || !tb.line_spacing_keyframes.is_empty()
        || !tb.tracking_keyframes.is_empty()
        || fx.loops.iter().any(|l| {
            matches!(
                l,
                klipp_timeline::TitleLoop::ColorCycle { .. }
                    | klipp_timeline::TitleLoop::ColorBlend { .. }
                    | klipp_timeline::TitleLoop::Counter { .. }
            )
        });
    // A still text is laid out and drawn here too, every frame: its
    // first frame then waits on a glyph cache miss, about 12 ms at
    // 1080p, where the CPU's resting raster took 50 to 90, and the
    // viewer and the export draw it one way (#2335). Anything that
    // moves keeps the route it was proven on.
    // Keyed styles at rest stay with `style_source`, as they were proven.
    let still = !fx.motion.is_active() && fx.loops.is_empty() && !fx.animators.iter().any(|a| a.enabled) && !fx.styles_keyed();
    changes || still
}

impl TitleRenderer {
    /// Layout changes reuse each font glyph's coverage. Layout/shaping runs on
    /// CPU; the frame mask, fill and styles are assembled on GPU.
    pub fn glyph_source(
        &mut self,
        title: &TitleData,
        width: u32,
        height: u32,
        time: f32,
        window: Option<f32>,
    ) -> Option<GlyphSource> {
        self.glyph_source_in_scene(title, width, height, time, window, None)
    }

    /// Preserve a linked path's authoring scene while rendering one text layer.
    pub fn glyph_source_in_scene(
        &mut self, title: &TitleData, width: u32, height: u32,
        time: f32, window: Option<f32>, scene: Option<&TitleData>,
    ) -> Option<GlyphSource> {
        if !takes_glyph_route(title, scene) {
            return None;
        }
        let [o] = title.objects.as_slice() else {
            return None;
        };
        let TitleObjectContent::TextBox(tb) = &o.content else {
            return None;
        };
        let linked = matches!(tb.text_path, Some(klipp_timeline::TextPath::Layer { .. }));
        let fx = o.fx();
        let mut evaluated = title.clone();
        let text = evaluated.primary_text_mut()?;
        *text = tb.at(time as f64);
        let font_px = text.font_size * frame_scale(width, height);
        let (text_w, text_h) = text.loop_extent(font_px);
        let pose = klipp_core::title_motion::eval_loops(
            &fx.loops,
            time,
            window,
            klipp_timeline::LoopGeometry {
                frame_w: width as f32,
                frame_h: height as f32,
                text_w,
                text_h,
                font_px,
            },
            text.color,
        );
        if pose.focus.is_some() || pose.dx != 0.0 || pose.dy != 0.0 || pose.opacity != 1.0 {
            return None;
        }
        if let Some(c) = pose.color {
            text.color = c;
        }
        if let Some(t) = pose.text {
            text.text = t;
        }
        evaluated.objects[0].fx_mut().loops.clear();
        evaluated.objects[0].transform = o.transform.evaluated_at(time as f64);
        let bound_path = if linked { Some(self.bound_path_line(scene?, &evaluated, Some(time), width, height)?) } else { None };
        self.bound_path = bound_path;
        self.gpu_layout_only = true;
        self.gpu_bitmap_size = !tb.font_size_keyframes.is_empty()
            || !o.transform.size_x_keyframes.is_empty() || !o.transform.size_y_keyframes.is_empty();
        self.gpu_motion = (fx.motion.is_active() || fx.animators.iter().any(|a| a.enabled))
            .then_some((time, window));
        self.gpu_wave = pose.wave.map(|(amp, rate)| (time, amp, rate));
        // Moving glyphs take outlines only where their poses need them,
        // which the layout decides (#2203).
        self.gpu_outline_only = tb.text_path.is_some()
            || !tb.font_size_keyframes.is_empty()
            || !tb.line_spacing_keyframes.is_empty()
            || !tb.tracking_keyframes.is_empty()
            || !o.transform.size_x_keyframes.is_empty()
            || !o.transform.size_y_keyframes.is_empty()
            || fx
                .loops
                .iter()
                .any(|l| matches!(l, klipp_timeline::TitleLoop::Counter { .. }));
        *self.gpu_layout_capture.borrow_mut() = None;
        let layout = self.render_layout_at_time(&evaluated, width, height, tb.text_path.as_ref().map(|_| time));
        self.bound_path = None;
        self.gpu_layout_only = false;
        self.gpu_bitmap_size = false;
        self.gpu_wave = None;
        self.gpu_motion = None;
        self.gpu_outline_only = false;
        let mut source = self.gpu_layout_capture.borrow_mut().take().or_else(|| {
            (layout.glyph_count == 0 || layout.mask_w == 0 || layout.mask_h == 0).then(|| {
                GlyphSource {
                    glyphs: Vec::new(),
                    outlines: Vec::new(),
                    face_draws: Vec::new(),
                    face_stripes: Vec::new(),
                    face_incomplete: false,
                    roi: [0; 4],
                    color: evaluated.color(),
                    fx: fx.clone(),
                    prepared: 0,
                    rerasterized: false,
                    outline_cutoff: i32::MAX,
                    write_cutoff: i32::MAX,
                    stripes: Vec::new(),
                    backing: None,
                    gradient: None,
                    pattern: None,
                    material: None,
                    font_px: 0.0,
                    bounds: [0.0; 4],
                    texture: None,
                }
            })
        })?;
        if source.face_incomplete {
            return None;
        }
        if let Some(fill) = tb.texture_fill.as_ref().filter(|f| f.enabled) {
            let hit = self
                .gpu_textures
                .iter()
                .position(|(key, _, _)| key == &fill.source);
            if let Some(index) = hit {
                let entry = self.gpu_textures.remove(index)?;
                source.texture = Some(TextureDraw {
                    id: entry.1,
                    pixels: Arc::clone(&entry.2),
                    fill: fill.clone(),
                });
                self.gpu_textures.push_back(entry);
            } else {
                let data = match &fill.source {
                    klipp_timeline::TextureSource::File(path) if !path.is_empty() => self
                        .load_texture_fill(path)
                        .map(|(p, w, h)| (p.to_vec(), w, h)),
                    klipp_timeline::TextureSource::Preset(p) => {
                        Some(generate_preset_texture(*p, 256, 256))
                    }
                    _ => None,
                };
                if let Some((data, w, h)) = data {
                    let bytes = u64::from(w) * u64::from(h) * 8;
                    if bytes > 64 * 1024 * 1024 {
                        return None;
                    }
                    let pixels = Arc::new(BoxRaster::from_bytes(&data, (w, h), (0, h), (0, w)));
                    while self.gpu_textures.len() >= 16
                        || self
                            .gpu_textures
                            .iter()
                            .map(|(_, _, p)| p.byte_size())
                            .sum::<usize>()
                            + pixels.byte_size()
                            > 64 * 1024 * 1024
                    {
                        self.gpu_textures.pop_front();
                    }
                    let id = self.next_cache_id;
                    self.next_cache_id += 1;
                    self.gpu_textures
                        .push_back((fill.source.clone(), id, Arc::clone(&pixels)));
                    source.texture = Some(TextureDraw {
                        id,
                        pixels,
                        fill: fill.clone(),
                    });
                    source.prepared += 1;
                }
            }
        }
        Some(source)
    }
}
