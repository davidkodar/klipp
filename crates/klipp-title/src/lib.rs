use fontdue::layout::{
    CoordinateSystem, HorizontalAlign, Layout, LayoutSettings, TextStyle, VerticalAlign,
    WrapStyle,
};
use fontdue::Font;
use klipp_timeline::{FontStyle, TextAlignment, TitleBackground, TitleData};
use std::collections::{BTreeMap, HashMap};

pub mod font_discovery;

/// Embedded fallback font — Noto Sans Regular (OFL license).
static EMBEDDED_FONT: &[u8] = include_bytes!("../assets/NotoSans-Regular.ttf");

/// Renders title clips to RGBA pixel buffers.
pub struct TitleRenderer {
    fallback_font: Font,
    font_cache: HashMap<String, Font>,
    font_registry: BTreeMap<String, Vec<font_discovery::FontInfo>>,
}

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

        if let Some(infos) = self.font_registry.get(family) {
            let style_str = match style {
                FontStyle::Regular => "Regular",
                FontStyle::Bold => "Bold",
                FontStyle::Italic => "Italic",
                FontStyle::BoldItalic => "Bold Italic",
            };

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

    /// Render a `TitleData` to an RGBA buffer of `width × height` pixels.
    pub fn render(&mut self, title: &TitleData, width: u32, height: u32) -> Vec<u8> {
        let w = width as usize;
        let h = height as usize;
        let mut buf = vec![0u8; w * h * 4];

        // Fill background if requested
        if let TitleBackground::FullFrame { color } = &title.background {
            for pixel in buf.chunks_exact_mut(4) {
                pixel.copy_from_slice(color);
            }
        }

        // Resolve the font: load into cache if needed, then get reference
        let cache_key = self.ensure_font_loaded(&title.font_name, title.font_style);
        let font = self
            .font_cache
            .get(&cache_key)
            .unwrap_or(&self.fallback_font);

        // Scale font size relative to frame height (72pt at 1080p)
        let scale = height as f32 / 1080.0;
        let font_px = title.font_size * scale;

        // Lay out text
        let h_align = match title.alignment {
            TextAlignment::Left => HorizontalAlign::Left,
            TextAlignment::Center => HorizontalAlign::Center,
            TextAlignment::Right => HorizontalAlign::Right,
        };

        let mut layout = Layout::new(CoordinateSystem::PositiveYDown);
        layout.reset(&LayoutSettings {
            x: 0.0,
            y: 0.0,
            max_width: Some(width as f32 * 0.9), // 5% margin each side
            max_height: Some(height as f32 * 0.9),
            horizontal_align: h_align,
            vertical_align: VerticalAlign::Top,
            wrap_style: WrapStyle::Word,
            ..LayoutSettings::default()
        });

        layout.append(&[font], &TextStyle::new(&title.text, font_px, 0));

        let glyphs = layout.glyphs();
        if glyphs.is_empty() {
            return buf;
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

        // Position offset: title.position is normalized (0.0-1.0)
        // 0.5, 0.5 = center of frame, text centered on that point
        let offset_x = title.position.x * width as f32 - text_w * 0.5 - text_min_x;
        let offset_y = title.position.y * height as f32 - text_h * 0.5 - text_min_y;

        // Draw background box if Color variant
        if let TitleBackground::Color { color, padding } = &title.background {
            let pad = padding * scale;
            let bx0 = ((text_min_x + offset_x - pad) as i32).max(0) as usize;
            let by0 = ((text_min_y + offset_y - pad) as i32).max(0) as usize;
            let bx1 = ((text_max_x + offset_x + pad) as i32).min(width as i32) as usize;
            let by1 = ((text_max_y + offset_y + pad) as i32).min(height as i32) as usize;
            for row in by0..by1 {
                for col in bx0..bx1 {
                    let idx = (row * w + col) * 4;
                    blend_pixel(&mut buf, idx, color);
                }
            }
        }

        // Draw shadow if present
        if let Some(shadow) = &title.shadow {
            let shadow_ox = shadow.offset_x * scale + offset_x;
            let shadow_oy = shadow.offset_y * scale + offset_y;
            Self::rasterize_glyphs(
                font,
                &mut buf,
                glyphs,
                shadow_ox,
                shadow_oy,
                w,
                h,
                &shadow.color,
            );
        }

        // Draw text glyphs
        Self::rasterize_glyphs(
            font,
            &mut buf,
            glyphs,
            offset_x,
            offset_y,
            w,
            h,
            &title.color,
        );

        buf
    }

    #[allow(clippy::too_many_arguments)]
    fn rasterize_glyphs(
        font: &Font,
        buf: &mut [u8],
        glyphs: &[fontdue::layout::GlyphPosition],
        offset_x: f32,
        offset_y: f32,
        buf_w: usize,
        buf_h: usize,
        color: &[u8; 4],
    ) {
        for glyph in glyphs {
            if glyph.width == 0 || glyph.height == 0 {
                continue;
            }
            let (_, coverage) = font.rasterize_config(glyph.key);

            let gx = (glyph.x + offset_x) as i32;
            let gy = (glyph.y + offset_y) as i32;

            for row in 0..glyph.height {
                let py = gy + row as i32;
                if py < 0 || py >= buf_h as i32 {
                    continue;
                }
                for col in 0..glyph.width {
                    let px = gx + col as i32;
                    if px < 0 || px >= buf_w as i32 {
                        continue;
                    }
                    let cov = coverage[row * glyph.width + col];
                    if cov == 0 {
                        continue;
                    }
                    let idx = (py as usize * buf_w + px as usize) * 4;
                    // Premultiply coverage with color alpha
                    let alpha = (cov as u16 * color[3] as u16 / 255) as u8;
                    let tinted = [color[0], color[1], color[2], alpha];
                    blend_pixel(buf, idx, &tinted);
                }
            }
        }
    }
}

/// Alpha-blend `src` (RGBA) over the pixel at `buf[idx..idx+4]`.
#[inline]
fn blend_pixel(buf: &mut [u8], idx: usize, src: &[u8; 4]) {
    let sa = src[3] as u32;
    if sa == 0 {
        return;
    }
    if sa == 255 {
        buf[idx] = src[0];
        buf[idx + 1] = src[1];
        buf[idx + 2] = src[2];
        buf[idx + 3] = 255;
        return;
    }
    let da = buf[idx + 3] as u32;
    let inv_sa = 255 - sa;
    let out_a = sa + da * inv_sa / 255;
    if out_a == 0 {
        return;
    }
    for c in 0..3 {
        let sc = src[c] as u32;
        let dc = buf[idx + c] as u32;
        buf[idx + c] = ((sc * sa + dc * da * inv_sa / 255) / out_a).min(255) as u8;
    }
    buf[idx + 3] = out_a.min(255) as u8;
}

#[cfg(test)]
mod tests {
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
        let title = TitleData {
            text: "X".to_string(),
            font_size: 200.0,
            color: [255, 255, 255, 255],
            position: TitlePosition { x: 0.5, y: 0.5 },
            ..TitleData::default()
        };
        let buf = renderer.render(&title, 640, 480);
        // Find a fully opaque text pixel
        let has_white = buf.chunks_exact(4).any(|p| p[0] == 255 && p[3] == 255);
        assert!(has_white, "Expected fully white opaque pixels");
    }

    #[test]
    fn full_frame_background() {
        let mut renderer = TitleRenderer::new();
        let title = TitleData {
            text: "BG".to_string(),
            background: TitleBackground::FullFrame {
                color: [0, 0, 128, 255],
            },
            ..TitleData::default()
        };
        let buf = renderer.render(&title, 320, 240);
        // Corner pixel should be the background color (no text there)
        assert_eq!(&buf[0..4], &[0, 0, 128, 255]);
    }

    #[test]
    fn shadow_adds_extra_pixels() {
        let mut renderer = TitleRenderer::new();
        let no_shadow = TitleData {
            text: "S".to_string(),
            font_size: 100.0,
            shadow: None,
            ..TitleData::default()
        };
        let with_shadow = TitleData {
            shadow: Some(klipp_timeline::TitleShadow {
                color: [0, 0, 0, 200],
                offset_x: 4.0,
                offset_y: 4.0,
                blur: 0.0,
            }),
            ..no_shadow.clone()
        };
        let buf1 = renderer.render(&no_shadow, 640, 480);
        let buf2 = renderer.render(&with_shadow, 640, 480);
        let count1 = buf1.chunks_exact(4).filter(|p| p[3] > 0).count();
        let count2 = buf2.chunks_exact(4).filter(|p| p[3] > 0).count();
        assert!(count2 > count1, "Shadow should add more non-transparent pixels");
    }
}
