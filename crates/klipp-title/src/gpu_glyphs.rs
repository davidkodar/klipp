//! Reusable glyph coverage separated from the layout and fill of a frame.
use super::*;
use std::{collections::VecDeque, sync::Arc};

pub struct GlyphDraw {
    pub id: u64,
    pub pixels: Arc<BoxRaster>,
    pub origin: [i32; 2],
}
pub struct GlyphSource {
    pub glyphs: Vec<GlyphDraw>,
    pub roi: [u32; 4],
    pub color: [u8; 4],
    pub fx: LayerFx,
    pub prepared: usize,
    pub rerasterized: bool,
    pub outline_cutoff: i32,
}
struct CachedGlyph {
    key: fontdue::layout::GlyphRasterConfig,
    id: u64,
    pixels: Arc<BoxRaster>,
}
pub(super) struct GlyphCache {
    entries: VecDeque<CachedGlyph>,
    next_id: u64,
    recent: VecDeque<fontdue::layout::GlyphRasterConfig>,
}
impl Default for GlyphCache {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
            next_id: 1 << 63,
            recent: VecDeque::new(),
        }
    }
}
impl GlyphCache {
    pub(super) fn clear(&mut self) {
        self.entries.clear();
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
    ) -> Option<GlyphSource> {
        if glyphs.len() > 4096 {
            return None;
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
            roi,
            color,
            fx: fx.clone(),
            prepared,
            rerasterized,
            outline_cutoff: i32::MAX,
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
        let [o] = title.objects.as_slice() else {
            return None;
        };
        let TitleObjectContent::TextBox(tb) = &o.content else {
            return None;
        };
        let fx = o.fx();
        if !o.visible
            || o.opacity != 1.0
            || !o.opacity_keyframes.is_empty()
            || title.mixes_layers()
            || title.background != TitleBackground::None
            || o.transform.rotation != 0.0
            || !o.transform.rotation_keyframes.is_empty()
            || tb.three_d.as_ref().is_some_and(|d| d.enabled)
            || tb.text_path.is_some()
            || tb.underline
            || tb.strikethrough
            || !tb.style_runs.is_empty()
            || tb.write_on != 1.0
            || tb.gradient.is_some()
            || tb.pattern_fill.is_some()
            || tb.texture_fill.is_some()
            || tb.extrusion.is_some()
            || tb.glitch_intensity > 0.0
            || tb.chromatic_aberration_px > 0.0
            || tb.char_animation.as_ref().is_some_and(|c| c.enabled)
            || fx.motion.is_active()
            || fx.animators.iter().any(|a| a.enabled)
            || !fx.edge_fade.is_none()
            || fx.motion_blur.is_some()
            || !tb.font_size_keyframes.is_empty()
        // animated size will use outline geometry
        {
            return None;
        }
        let changes = !tb.color_keyframes.is_empty()
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
        if pose.wave.is_some()
            || pose.focus.is_some()
            || pose.dx != 0.0
            || pose.dy != 0.0
            || pose.opacity != 1.0
        {
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
        self.gpu_layout_only = true;
        *self.gpu_layout_capture.borrow_mut() = None;
        let layout = self.render_layout_at_time(&evaluated, width, height, None);
        self.gpu_layout_only = false;
        self.gpu_layout_capture.borrow_mut().take().or_else(|| {
            (layout.glyph_count == 0 || layout.mask_w == 0 || layout.mask_h == 0).then(|| {
                GlyphSource {
                    glyphs: Vec::new(),
                    roi: [0; 4],
                    color: evaluated.color(),
                    fx: fx.clone(),
                    prepared: 0,
                    rerasterized: false,
                    outline_cutoff: i32::MAX,
                }
            })
        })
    }
}
