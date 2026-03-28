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

        // ── Rasterize glyphs into a temporary alpha mask ──
        // Compute the mask bounding box (padded for effects)
        let max_pad = effect_padding(title, scale);
        let mask_x0 = ((text_min_x + offset_x).floor() as i32 - max_pad).max(0) as usize;
        let mask_y0 = ((text_min_y + offset_y).floor() as i32 - max_pad).max(0) as usize;
        let mask_x1 = ((text_max_x + offset_x).ceil() as i32 + max_pad + 1).min(w as i32) as usize;
        let mask_y1 = ((text_max_y + offset_y).ceil() as i32 + max_pad + 1).min(h as i32) as usize;
        let mask_w = mask_x1.saturating_sub(mask_x0);
        let mask_h = mask_y1.saturating_sub(mask_y0);

        if mask_w == 0 || mask_h == 0 {
            return buf;
        }

        // Rasterize glyphs into alpha mask
        let mut alpha_mask = vec![0u8; mask_w * mask_h];
        rasterize_to_mask(font, glyphs, offset_x, offset_y, &mut alpha_mask, mask_w, mask_h, mask_x0, mask_y0);

        // ── Render effects back-to-front ──

        // 1. Outer glow
        if let Some(glow) = &title.glow {
            let blurred = blur_alpha(&alpha_mask, mask_w, mask_h, glow.radius * scale);
            draw_mask_to_buf(&mut buf, &blurred, mask_w, mask_h, mask_x0, mask_y0, w, h, &glow.color, glow.intensity);
        }

        // 2. Shadow (with blur support)
        if let Some(shadow) = &title.shadow {
            let sox = (shadow.offset_x * scale).round() as i32;
            let soy = (shadow.offset_y * scale).round() as i32;

            if shadow.blur > 0.0 {
                // Create offset mask, then blur it
                let offset_mask = offset_alpha(&alpha_mask, mask_w, mask_h, sox, soy);
                let blurred = blur_alpha(&offset_mask, mask_w, mask_h, shadow.blur * scale);
                draw_mask_to_buf(&mut buf, &blurred, mask_w, mask_h, mask_x0, mask_y0, w, h, &shadow.color, 1.0);
            } else {
                // No blur — draw shadow at offset directly
                let offset_mask = offset_alpha(&alpha_mask, mask_w, mask_h, sox, soy);
                draw_mask_to_buf(&mut buf, &offset_mask, mask_w, mask_h, mask_x0, mask_y0, w, h, &shadow.color, 1.0);
            }
        }

        // 3. Outline/stroke
        if let Some(outline) = &title.outline {
            let dilated = dilate_alpha(&alpha_mask, mask_w, mask_h, outline.width * scale);
            // Draw dilated minus original = outline ring
            let mut outline_mask = vec![0u8; mask_w * mask_h];
            for i in 0..outline_mask.len() {
                outline_mask[i] = dilated[i].saturating_sub(alpha_mask[i]);
            }
            draw_mask_to_buf(&mut buf, &outline_mask, mask_w, mask_h, mask_x0, mask_y0, w, h, &outline.color, 1.0);
        }

        // 4. Text fill (solid color or gradient)
        if let Some(gradient) = &title.gradient {
            // Gradient fill based on Y position within text bounding box
            let text_top = (text_min_y + offset_y) as i32;
            let text_bot = (text_max_y + offset_y) as i32;
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
        } else {
            // Solid color fill
            draw_mask_to_buf(&mut buf, &alpha_mask, mask_w, mask_h, mask_x0, mask_y0, w, h, &title.color, 1.0);
        }

        buf
    }
}

/// Compute the maximum padding needed around the text bounding box for effects.
fn effect_padding(title: &TitleData, scale: f32) -> i32 {
    let mut pad = 0i32;
    if let Some(outline) = &title.outline {
        pad = pad.max((outline.width * scale).ceil() as i32 + 1);
    }
    if let Some(glow) = &title.glow {
        pad = pad.max((glow.radius * scale * 2.0).ceil() as i32 + 1);
    }
    if let Some(shadow) = &title.shadow {
        let sx = (shadow.offset_x.abs() * scale).ceil() as i32;
        let sy = (shadow.offset_y.abs() * scale).ceil() as i32;
        let sb = (shadow.blur * scale * 2.0).ceil() as i32;
        pad = pad.max(sx + sb + 1);
        pad = pad.max(sy + sb + 1);
    }
    pad
}

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
            let my = py - mask_y0 as i32;
            if my < 0 || my >= mask_h as i32 { continue; }
            for col in 0..glyph.width {
                let px = gx + col as i32;
                let mx = px - mask_x0 as i32;
                if mx < 0 || mx >= mask_w as i32 { continue; }
                let cov = coverage[row * glyph.width + col];
                if cov == 0 { continue; }
                let idx = my as usize * mask_w + mx as usize;
                // Max blend so overlapping glyphs don't exceed 255
                mask[idx] = mask[idx].max(cov);
            }
        }
    }
}

/// Draw an alpha mask onto an RGBA buffer using a given color and intensity.
#[allow(clippy::too_many_arguments)]
fn draw_mask_to_buf(
    buf: &mut [u8],
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

#[inline]
fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t).round().clamp(0.0, 255.0) as u8
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

    #[test]
    fn outline_adds_extra_pixels() {
        let mut renderer = TitleRenderer::new();
        let no_outline = TitleData {
            text: "O".to_string(),
            font_size: 100.0,
            ..TitleData::default()
        };
        let with_outline = TitleData {
            outline: Some(klipp_timeline::TextOutline {
                color: [255, 0, 0, 255],
                width: 4.0,
            }),
            ..no_outline.clone()
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
        let no_glow = TitleData {
            text: "G".to_string(),
            font_size: 100.0,
            ..TitleData::default()
        };
        let with_glow = TitleData {
            glow: Some(klipp_timeline::TextGlow {
                color: [0, 255, 0, 255],
                radius: 10.0,
                intensity: 0.8,
            }),
            ..no_glow.clone()
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
        let title = TitleData {
            text: "GRADIENT".to_string(),
            font_size: 100.0,
            gradient: Some(klipp_timeline::TextGradient {
                top_color: [255, 0, 0, 255],
                bottom_color: [0, 0, 255, 255],
            }),
            ..TitleData::default()
        };
        let buf = renderer.render(&title, 640, 480);
        // Should have both reddish and bluish pixels
        let has_red = buf.chunks_exact(4).any(|p| p[3] > 0 && p[0] > 100 && p[2] < 50);
        let has_blue = buf.chunks_exact(4).any(|p| p[3] > 0 && p[2] > 100 && p[0] < 50);
        assert!(has_red, "Gradient should produce red-ish pixels at top");
        assert!(has_blue, "Gradient should produce blue-ish pixels at bottom");
    }
}
