//! Unfinished title faces for a compositor that can keep styling on the GPU.
use super::*;

const STYLE_CACHE_BYTES: usize = 64 << 20;
const STYLE_CACHE_ENTRIES: usize = 16;

/// CPU face work performed by this call, separate from resident GPU styling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StyleFaceWork {
    Reused,
    Prepared,
    AnimatedRaster,
}
#[derive(Clone)]
pub struct StyleSource {
    pub face_work: StyleFaceWork,
    pub id: u64,
    pub pixels: std::sync::Arc<BoxRaster>,
    /// Exact padded coverage box, needed by inverted inner shadows.
    pub roi: [u32; 4],
    pub fx: LayerFx,
}
pub(super) struct SourceCache {
    title: TitleData,
    frame: (u32, u32),
    source: StyleSource,
}

impl TitleRenderer {
    /// Prepare animated, single-text styles without CPU blur/dilate/composition.
    /// Unsupported combinations keep the established renderer. Static titles
    /// retain their existing finished-sprite cache rather than restyling it.
    pub fn style_source(
        &mut self,
        title: &TitleData,
        width: u32,
        height: u32,
        time: f32,
        window: Option<f32>,
    ) -> Option<StyleSource> {
        if title.objects.len() != 1
            || title.background != TitleBackground::None
            || title.mixes_layers()
        {
            return None;
        }
        let o = &title.objects[0];
        let TitleObjectContent::TextBox(tb) = &o.content else {
            return None;
        };
        let fx = o.fx();
        if !o.visible || o.opacity != 1.0 || !o.opacity_keyframes.is_empty() || !stack_draws(fx) {
            return None;
        }
        let allowed_fx = LayerFx {
            motion: if tb.three_d.as_ref().is_some_and(|d| d.enabled) {
                fx.motion
            } else {
                Default::default()
            },
            glows: fx.glows.clone(),
            glow_stack_enabled: fx.glow_stack_enabled,
            shadows: fx.shadows.clone(),
            shadow_stack_enabled: fx.shadow_stack_enabled,
            outlines: fx.outlines.clone(),
            outline_stack_enabled: fx.outline_stack_enabled,
            inner_shadows: fx.inner_shadows.clone(),
            inner_shadow_stack_enabled: fx.inner_shadow_stack_enabled,
            ..Default::default()
        };
        if *fx != allowed_fx {
            return None;
        }
        let allowed_transform = klipp_timeline::ObjectTransform {
            center: o.transform.center,
            size: o.transform.size,
            anchor_norm: o.transform.anchor_norm,
            ..TitleData::single_text(Default::default())
                .objects
                .remove(0)
                .transform
        };
        if o.transform != allowed_transform {
            return None;
        }
        let animated = tb.has_keyframes() || fx.styles_keyed();
        if !animated {
            return None;
        }
        // A conservative text allowlist keeps post effects and future features
        // out until their order and coverage are explicitly tested.
        let allowed = klipp_timeline::TextBox {
            text: tb.text.clone(),
            font_name: tb.font_name.clone(),
            font_style: tb.font_style,
            font_size: tb.font_size,
            font_size_keyframes: tb.font_size_keyframes.clone(),
            color: tb.color,
            color_keyframes: tb.color_keyframes.clone(),
            alignment: tb.alignment,
            tracking: tb.tracking,
            tracking_keyframes: tb.tracking_keyframes.clone(),
            line_spacing: tb.line_spacing,
            line_spacing_keyframes: tb.line_spacing_keyframes.clone(),
            vertical_align: tb.vertical_align,
            baseline_shift: tb.baseline_shift,
            case_transform: tb.case_transform,
            features: tb.features.clone(),
            font_weight: tb.font_weight,
            font_axes: tb.font_axes.clone(),
            three_d: tb.three_d.clone(),
            ..Default::default()
        };
        if *tb != allowed {
            return None;
        }
        let mut evaluated = title.clone();
        evaluated.objects[0].content = TitleObjectContent::TextBox(tb.at(time as f64));
        let flat = evaluated
            .primary_text()
            .unwrap()
            .three_d
            .as_ref()
            .is_none_or(|d| !d.enabled);
        if flat && evaluated.primary_text().unwrap().color[3] != 255 {
            return None;
        }
        // Keep recently used faces together: alternating styled titles must
        // not evict one another every frame. Promote hits to the LRU tail.
        if !fx.motion.is_active() {
            if let Some(index) = self.style_source_cache.iter().position(|cached| {
                cached.frame == (width, height) && cached.title == evaluated
            }) {
                let cached = self.style_source_cache.remove(index);
                self.last_focus_blur = None;
                self.last_blur = 0.0;
                self.last_rows = (cached.source.roi[1], cached.source.roi[1] + cached.source.roi[3]);
                let source = StyleSource { face_work: StyleFaceWork::Reused, ..cached.source.clone() };
                self.style_source_cache.push(cached);
                return Some(source);
            }
        }
        let cache_key = evaluated.clone();
        self.motion_window = window;
        let (face, roi) = if flat {
            let layout = self.render_layout_at_time(&evaluated, width, height, Some(time));
            // Colour glyphs have distinct coverage/fill semantics.
            if !layout.colour_glyphs.is_empty() {
                self.motion_window = None;
                return None;
            }
            let roi = [layout.mask_x0, layout.mask_y0, layout.mask_w, layout.mask_h];
            evaluated.objects[0].fx = Some(LayerFx::default());
            (
                Self::compose(&layout, &evaluated, width, height, None, Some(time)),
                roi,
            )
        } else {
            let result = self.render_text3d_solid(&evaluated, width, height, Some(time));
            self.motion_window = None;
            let (face, drawn) = result?;
            let [x0, y0, x1, y1] = drawn?;
            let pad = effect_padding(&evaluated, frame_scale(width, height)).max(0) as usize + 2;
            let mx0 = x0.saturating_sub(pad);
            let my0 = y0.saturating_sub(pad);
            let mx1 = (x1 + pad).min(width as usize);
            let my1 = (y1 + pad).min(height as usize);
            (
                face,
                [
                    mx0 as u32,
                    my0 as u32,
                    (mx1 - mx0) as u32,
                    (my1 - my0) as u32,
                ],
            )
        };
        self.motion_window = None;
        self.last_focus_blur = None;
        self.last_blur = 0.0;
        self.last_rows = (roi[1], roi[1] + roi[3]);
        let pixels = BoxRaster::from_bytes(
            &face,
            (width, height),
            self.last_rows,
            (roi[0], roi[0] + roi[2]),
        );
        let id = self.next_cache_id;
        self.next_cache_id = self.next_cache_id.wrapping_add(1);
        let source = StyleSource {
            face_work: if tb.has_keyframes()
                || fx.motion.is_active()
                || pixels.byte_size() > STYLE_CACHE_BYTES
            {
                StyleFaceWork::AnimatedRaster
            } else {
                StyleFaceWork::Prepared
            },
            id,
            pixels: std::sync::Arc::new(pixels),
            roi,
            fx: fx.clone(),
        };
        if !fx.motion.is_active() && source.pixels.byte_size() <= STYLE_CACHE_BYTES {
            let mut bytes: usize = self.style_source_cache.iter().map(|c| c.source.pixels.byte_size()).sum();
            while self.style_source_cache.len() >= STYLE_CACHE_ENTRIES
                || bytes + source.pixels.byte_size() > STYLE_CACHE_BYTES
            {
                bytes -= self.style_source_cache.remove(0).source.pixels.byte_size();
            }
            self.style_source_cache.push(SourceCache {
                title: cache_key,
                frame: (width, height),
                source: source.clone(),
            });
        }
        Some(source)
    }
}
