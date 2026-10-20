//! Reusable glyph coverage separated from the layout and fill of a frame.
use super::*;
use std::{collections::VecDeque, sync::Arc};

pub struct GlyphDraw {
    pub id: u64,
    pub pixels: Arc<BoxRaster>,
    pub origin: [i32; 2],
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
    next_id: u64,
    recent: VecDeque<fontdue::layout::GlyphRasterConfig>,
}
impl Default for GlyphCache {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
            outlines: VecDeque::new(),
            next_id: 1 << 63,
            recent: VecDeque::new(),
        }
    }
}
impl GlyphCache {
    pub(super) fn clear(&mut self) {
        self.entries.clear();
        self.outlines.clear();
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
            if fonts[g.font_index]
                .colour_glyph(g.key.glyph_index, g.key.px)
                .is_some()
            {
                return None;
            }
            let cached = if let Some(i) = self.entries.iter().position(|c| c.key == g.key) {
                self.entries.remove(i)?
            } else {
                rerasterized |= self.recent.contains(&g.key);
                if self.recent.len() >= 4096 {
                    self.recent.pop_front();
                }
                self.recent.push_back(g.key);
                let (_, coverage) = fonts[g.font_index].coverage(g.key);
                if coverage.len() != g.width * g.height {
                    return None;
                }
                let mut bytes = Vec::with_capacity(coverage.len() * 4);
                for alpha in coverage {
                    bytes.extend_from_slice(&[255, 255, 255, alpha]);
                }
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
                    key: g.key,
                    id,
                    pixels,
                }
            };
            draws.push(GlyphDraw {
                id: cached.id,
                pixels: cached.pixels.clone(),
                origin: [(g.x + offset[0]) as i32, (g.y + offset[1]) as i32],
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
            bounds: [0.0; 4],
            texture: None,
        })
    }
}
impl GlyphCache {
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
            bounds: [0.0; 4],
            texture: None,
        })
    }
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
        let [o] = title.objects.as_slice() else {
            return None;
        };
        let TitleObjectContent::TextBox(tb) = &o.content else {
            return None;
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
            // Path motion poses the already turned coverage, unlike flat glyph poses.
            || (tb.text_path.is_some() && !path_opacity_only(fx))
            || tb.extrusion.is_some()
            || tb.glitch_intensity > 0.0
            || tb.chromatic_aberration_px > 0.0
            || tb.char_animation.as_ref().is_some_and(|c| c.enabled)
            || !fx.edge_fade.is_none()
            || fx.motion_blur.is_some()
        {
            return None;
        }
        let raster_animator = fx.animators.iter().any(|a| a.enabled && (a.props.blur != 0.0 || a.props.colour.is_some()));
        let stagger_blur = fx.motion.enter.iter().chain(fx.motion.exit.iter()).any(|m|m.kind.blurs() && m.stagger!=klipp_timeline::StaggerUnit::None);
        let changes = linked || raster_animator || stagger_blur || tb.gradient.as_ref().is_some_and(|g| g.has_keyframes())
            || !o.transform.size_x_keyframes.is_empty()
            || !o.transform.size_y_keyframes.is_empty()
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
        if !changes {
            return None;
        }
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
        self.gpu_motion = (fx.motion.is_active() || fx.animators.iter().any(|a| a.enabled))
            .then_some((time, window));
        self.gpu_wave = pose.wave.map(|(amp, rate)| (time, amp, rate));
        self.gpu_outline_only = tb.text_path.is_some() || self.gpu_motion.is_some()
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

/// Opacity does not change the sampling of already rotated path coverage.
/// Spatial poses need a separate path-aware sampler and remain fallback.
pub(crate) fn path_opacity_only(fx: &klipp_timeline::LayerFx) -> bool {
    use klipp_timeline::MotionKind;
    fx.motion.enter.iter().chain(fx.motion.exit.iter()).all(|m|matches!(m.kind,MotionKind::Fade|MotionKind::Flicker|MotionKind::Typewriter))
        && fx.animators.iter().filter(|a|a.enabled).all(|a| {
            let p=&a.props;
            p.dx==0.0 && p.dy==0.0 && p.scale==1.0 && p.rotation==0.0 && p.blur==0.0 && p.colour.is_none()
        })
}
