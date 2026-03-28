use fontdue::layout::{
    CoordinateSystem, HorizontalAlign, Layout, LayoutSettings, TextStyle, VerticalAlign,
    WrapStyle,
};
use fontdue::Font;
use klipp_timeline::{FontStyle, StyleRun, TextAlignment, TextPath, TextPattern, TitleBackground, TitleData};
use std::collections::{BTreeMap, HashMap};

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

/// Renders title clips to RGBA pixel buffers.
pub struct TitleRenderer {
    fallback_font: Font,
    font_cache: HashMap<String, Font>,
    font_registry: BTreeMap<String, Vec<font_discovery::FontInfo>>,
    /// Cached texture fill image: (path, rgba_data, width, height)
    texture_fill_cache: Option<(String, Vec<u8>, u32, u32)>,
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
            texture_fill_cache: None,
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

    /// Phase 1: Font loading, glyph rasterization into alpha mask, returns layout info.
    pub fn render_layout(&mut self, title: &TitleData, width: u32, height: u32) -> TitleLayout {
        let w = width as usize;
        let h = height as usize;

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

        // Append text with per-run font sizes when style runs exist.
        // Track (glyph_count_before, segment_byte_start) for byte_offset fixup.
        let mut segment_info: Vec<(usize, usize)> = Vec::new();

        if title.style_runs.is_empty() {
            segment_info.push((0, 0));
            layout.append(&[font], &TextStyle::new(&title.text, font_px, 0));
        } else {
            let mut pos = 0usize;
            for run in &title.style_runs {
                if run.start > pos && run.start <= title.text.len() {
                    let segment = &title.text[pos..run.start];
                    if !segment.is_empty() {
                        segment_info.push((layout.glyphs().len(), pos));
                        layout.append(&[font], &TextStyle::new(segment, font_px, 0));
                    }
                    pos = run.start;
                }
                let run_end = run.end.min(title.text.len());
                if run_end > pos {
                    let segment = &title.text[pos..run_end];
                    let run_size = run.font_size.unwrap_or(title.font_size) * scale;
                    if !segment.is_empty() {
                        segment_info.push((layout.glyphs().len(), pos));
                        layout.append(&[font], &TextStyle::new(segment, run_size, 0));
                    }
                    pos = run_end;
                }
            }
            if pos < title.text.len() {
                let segment = &title.text[pos..];
                if !segment.is_empty() {
                    segment_info.push((layout.glyphs().len(), pos));
                    layout.append(&[font], &TextStyle::new(segment, font_px, 0));
                }
            }
        }

        let glyphs = layout.glyphs();
        if glyphs.is_empty() {
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
            };
        }

        // ── Post-process glyph positions for tracking and line spacing ──
        let mut glyphs_owned: Vec<fontdue::layout::GlyphPosition> = glyphs.to_vec();

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
        if title.line_spacing != 1.0 {
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
                    let y_shift = base_line_height * (title.line_spacing - 1.0) * line_idx as f32;
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
        if title.tracking != 0.0 {
            let tracking_px = title.tracking * scale;
            for (i, g) in glyphs_owned.iter_mut().enumerate() {
                g.x += tracking_px * i as f32;
            }
        }

        let glyphs = &glyphs_owned;

        // ── Text on path: rasterize per-glyph with rotation ──
        if let Some(text_path) = &title.text_path {
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

        // Position offset — alignment affects horizontal anchor point.
        // The layout's max_width is width*0.9, leaving 5% margin each side.
        let margin = width as f32 * 0.05;
        let offset_x = match title.alignment {
            TextAlignment::Left => margin - text_min_x,
            TextAlignment::Right => width as f32 - margin - text_w - text_min_x,
            TextAlignment::Center => title.position.x * width as f32 - text_w * 0.5 - text_min_x,
        };
        let offset_y = title.position.y * height as f32 - text_h * 0.5 - text_min_y;

        // Compute the mask bounding box (padded for effects)
        let max_pad = effect_padding(title, scale);
        let mask_x0 = ((text_min_x + offset_x).floor() as i32 - max_pad).max(0) as usize;
        let mask_y0 = ((text_min_y + offset_y).floor() as i32 - max_pad).max(0) as usize;
        let mask_x1 = ((text_max_x + offset_x).ceil() as i32 + max_pad + 1).min(w as i32) as usize;
        let mask_y1 = ((text_max_y + offset_y).ceil() as i32 + max_pad + 1).min(h as i32) as usize;
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
            };
        }

        // Rasterize glyphs into alpha mask
        let mut alpha_mask = vec![0u8; mask_w * mask_h];
        rasterize_to_mask(font, glyphs, offset_x, offset_y, &mut alpha_mask, mask_w, mask_h, mask_x0, mask_y0);

        // ── Rich text: resolve per-glyph styles ──
        let has_rich_text = !title.style_runs.is_empty();
        let resolved_colors: Vec<[u8; 4]> = if has_rich_text {
            resolve_glyph_colors(title, glyphs, &title.style_runs)
        } else {
            Vec::new()
        };

        // Apply write-on reveal
        if title.write_on < 1.0 {
            let cutoff_x = ((text_min_x + offset_x) + text_w * title.write_on.clamp(0.0, 1.0)) as i32;
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
        let center_x = title.position.x * width as f32;
        let center_y = title.position.y * height as f32;

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
        if title.write_on < 1.0 {
            let cutoff_x = (text_min_x + (text_max_x - text_min_x) * title.write_on.clamp(0.0, 1.0)) as i32;
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
    ) -> Vec<u8> {
        let w = width as usize;
        let h = height as usize;
        let mut buf = vec![0u8; w * h * 4];

        // Fill background if requested
        if let TitleBackground::FullFrame { color } = &title.background {
            for pixel in buf.chunks_exact_mut(4) {
                pixel.copy_from_slice(color);
            }
        }

        if layout.glyph_count == 0 || layout.mask_w == 0 || layout.mask_h == 0 {
            return buf;
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

        // 1. Outer glow
        if let Some(glow) = &title.glow {
            let blurred = do_blur(alpha_mask, mask_w, mask_h, glow.radius * scale, &mut gpu);
            draw_mask_to_buf(&mut buf, &blurred, mask_w, mask_h, mask_x0, mask_y0, w, h, &glow.color, glow.intensity);
        }

        // 2. Shadow (with blur support)
        if let Some(shadow) = &title.shadow {
            let sox = (shadow.offset_x * scale).round() as i32;
            let soy = (shadow.offset_y * scale).round() as i32;

            if shadow.blur > 0.0 {
                let offset_mask = offset_alpha(alpha_mask, mask_w, mask_h, sox, soy);
                let blurred = do_blur(&offset_mask, mask_w, mask_h, shadow.blur * scale, &mut gpu);
                draw_mask_to_buf(&mut buf, &blurred, mask_w, mask_h, mask_x0, mask_y0, w, h, &shadow.color, 1.0);
            } else {
                let offset_mask = offset_alpha(alpha_mask, mask_w, mask_h, sox, soy);
                draw_mask_to_buf(&mut buf, &offset_mask, mask_w, mask_h, mask_x0, mask_y0, w, h, &shadow.color, 1.0);
            }
        }

        // 3. Outline/stroke — multiple layers (widest first) then legacy single
        {
            let mut all_outlines: Vec<&klipp_timeline::TextOutline> = Vec::new();
            for ol in &title.outlines {
                all_outlines.push(ol);
            }
            if title.outlines.is_empty() {
                if let Some(ref ol) = title.outline {
                    all_outlines.push(ol);
                }
            }
            all_outlines.sort_by(|a, b| b.width.partial_cmp(&a.width).unwrap_or(std::cmp::Ordering::Equal));

            for outline in &all_outlines {
                let dilated = do_dilate(alpha_mask, mask_w, mask_h, outline.width * scale, &mut gpu);
                let mut outline_mask = vec![0u8; mask_w * mask_h];
                for i in 0..outline_mask.len() {
                    outline_mask[i] = dilated[i].saturating_sub(alpha_mask[i]);
                }
                draw_mask_to_buf(&mut buf, &outline_mask, mask_w, mask_h, mask_x0, mask_y0, w, h, &outline.color, 1.0);
            }
        }

        // 4. 3D Text Extrusion (rendered before face text, after outlines)
        if let Some(ref ext) = title.extrusion {
            render_extrusion(&mut buf, alpha_mask, mask_w, mask_h, mask_x0, mask_y0, w, h, ext, title);
        }

        // 5. Text fill (pattern > gradient > solid) — this is the face text
        {
            // If extrusion is active and has a non-transparent face_color, use that
            let face_color = if let Some(ref ext) = title.extrusion {
                if ext.face_color[3] > 0 {
                    ext.face_color
                } else {
                    title.color
                }
            } else {
                title.color
            };

            if let Some(pattern) = &title.pattern_fill {
                draw_pattern_fill(&mut buf, alpha_mask, mask_w, mask_h, mask_x0, mask_y0, w, h, pattern, scale);
            } else if let Some(gradient) = &title.gradient {
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

        // 6. Inner shadow (rendered inside the text shape)
        if let Some(inner) = &title.inner_shadow {
            let sox = (inner.offset_x * scale).round() as i32;
            let soy = (inner.offset_y * scale).round() as i32;
            let mut inverted = vec![0u8; mask_w * mask_h];
            for i in 0..inverted.len() {
                inverted[i] = 255u8.saturating_sub(alpha_mask[i]);
            }
            let offset_inv = offset_alpha(&inverted, mask_w, mask_h, sox, soy);
            let shadow_mask = if inner.blur > 0.0 {
                do_blur(&offset_inv, mask_w, mask_h, inner.blur * scale, &mut gpu)
            } else {
                offset_inv
            };
            let mut inner_mask = vec![0u8; mask_w * mask_h];
            for i in 0..inner_mask.len() {
                inner_mask[i] = ((shadow_mask[i] as u16 * alpha_mask[i] as u16) / 255) as u8;
            }
            draw_mask_to_buf(&mut buf, &inner_mask, mask_w, mask_h, mask_x0, mask_y0, w, h, &inner.color, 1.0);
        }

        buf
    }

    /// Render a `TitleData` to an RGBA buffer of `width x height` pixels (backward compat).
    pub fn render(&mut self, title: &TitleData, width: u32, height: u32) -> Vec<u8> {
        let layout = self.render_layout(title, width, height);
        let mut buf = Self::compose(&layout, title, width, height, None);
        self.apply_texture_fill(&mut buf, &layout, title, width, height);
        buf
    }

    /// Render with optional GPU-accelerated alpha mask processing.
    pub fn render_with_gpu(
        &mut self,
        title: &TitleData,
        width: u32,
        height: u32,
        gpu: Option<&mut dyn AlphaMaskProcessor>,
    ) -> Vec<u8> {
        let layout = self.render_layout(title, width, height);
        let mut buf = Self::compose(&layout, title, width, height, gpu);
        self.apply_texture_fill(&mut buf, &layout, title, width, height);
        buf
    }

    /// Compute character positions for the given title text, using the same layout
    /// algorithm as `render_layout`. Returns positions in frame pixel coordinates.
    /// Used by the viewer for cursor placement and click hit-testing.
    pub fn compute_char_positions(&mut self, title: &TitleData, width: u32, height: u32) -> Vec<CharPosition> {
        if title.text.is_empty() {
            return Vec::new();
        }

        // Same font + layout as render_layout
        let cache_key = self.ensure_font_loaded(&title.font_name, title.font_style);
        let font = self.font_cache.get(&cache_key).unwrap_or(&self.fallback_font);

        let scale = height as f32 / 1080.0;
        let font_px = title.font_size * scale;

        let h_align = match title.alignment {
            TextAlignment::Left => HorizontalAlign::Left,
            TextAlignment::Center => HorizontalAlign::Center,
            TextAlignment::Right => HorizontalAlign::Right,
        };

        let mut layout = Layout::new(CoordinateSystem::PositiveYDown);
        layout.reset(&LayoutSettings {
            x: 0.0,
            y: 0.0,
            max_width: Some(width as f32 * 0.9),
            max_height: Some(height as f32 * 0.9),
            horizontal_align: h_align,
            vertical_align: VerticalAlign::Top,
            wrap_style: WrapStyle::Word,
            ..LayoutSettings::default()
        });

        // Track (glyph_count_before_append, segment_byte_start_in_full_text)
        // so we can fix up fontdue's per-segment byte_offset → absolute byte_offset.
        let mut segment_info: Vec<(usize, usize)> = Vec::new();

        if title.style_runs.is_empty() {
            segment_info.push((0, 0));
            layout.append(&[font], &TextStyle::new(&title.text, font_px, 0));
        } else {
            let mut pos = 0usize;
            for run in &title.style_runs {
                if run.start > pos && run.start <= title.text.len() {
                    let segment = &title.text[pos..run.start];
                    if !segment.is_empty() {
                        segment_info.push((layout.glyphs().len(), pos));
                        layout.append(&[font], &TextStyle::new(segment, font_px, 0));
                    }
                    pos = run.start;
                }
                let run_end = run.end.min(title.text.len());
                if run_end > pos {
                    let segment = &title.text[pos..run_end];
                    let run_size = run.font_size.unwrap_or(title.font_size) * scale;
                    if !segment.is_empty() {
                        segment_info.push((layout.glyphs().len(), pos));
                        layout.append(&[font], &TextStyle::new(segment, run_size, 0));
                    }
                    pos = run_end;
                }
            }
            if pos < title.text.len() {
                let segment = &title.text[pos..];
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
                x: title.position.x * width as f32,
                y: title.position.y * height as f32,
                advance: font_px * 0.5,
                height: font_px,
            }];
        }

        let mut glyphs_owned: Vec<fontdue::layout::GlyphPosition> = glyphs.to_vec();

        // Fix up byte_offset: fontdue's byte_offset is relative to each append() call's string,
        // but we need it relative to the full title.text.
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
        if title.line_spacing != 1.0 {
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
                    let y_shift = base_line_height * (title.line_spacing - 1.0) * line_idx as f32;
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
        if title.tracking != 0.0 {
            let tracking_px = title.tracking * scale;
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

        let margin = width as f32 * 0.05;
        let offset_x = match title.alignment {
            TextAlignment::Left => margin - text_min_x,
            TextAlignment::Right => width as f32 - margin - text_w - text_min_x,
            TextAlignment::Center => title.position.x * width as f32 - text_w * 0.5 - text_min_x,
        };
        let offset_y = title.position.y * height as f32 - text_h * 0.5 - text_min_y;

        // Build character positions from glyph data.
        // fontdue gives us one glyph per visible character (spaces may have width=0).
        // We need to map byte_offset → char_index for correct cursor positioning.
        let mut positions: Vec<CharPosition> = Vec::with_capacity(glyphs_owned.len() + 1);

        // Build a byte_offset → char_index lookup
        let byte_to_char: Vec<(usize, usize)> = title.text
            .char_indices()
            .enumerate()
            .map(|(ci, (bi, _))| (bi, ci))
            .collect();

        let char_count = title.text.chars().count();

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
                        byte_offset: title.text.char_indices().nth(ci).map(|(bi, _)| bi).unwrap_or(0),
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
                byte_offset: title.text.len(),
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
        let tex_path = match &title.texture_fill_path {
            Some(p) if !p.is_empty() => p.clone(),
            _ => return,
        };

        if layout.glyph_count == 0 || layout.mask_w == 0 || layout.mask_h == 0 {
            return;
        }

        // Text bounding box in frame coordinates
        let text_left = layout.text_left;
        let text_top = layout.text_top;
        let text_w = (layout.text_right - layout.text_left).max(1.0);
        let text_h = (layout.text_bottom - layout.text_top).max(1.0);

        let (tex_data, tex_w, tex_h) = match self.load_texture_fill(&tex_path) {
            Some(t) => t,
            None => return,
        };

        let mask_w = layout.mask_w as usize;
        let mask_h = layout.mask_h as usize;
        let mask_x0 = layout.mask_x0 as usize;
        let mask_y0 = layout.mask_y0 as usize;
        let w = width as usize;

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

                // Map pixel position within the text bounding box to texture coordinates
                let u = ((global_x as f32 - text_left) / text_w).clamp(0.0, 1.0);
                let v = ((global_y as f32 - text_top) / text_h).clamp(0.0, 1.0);

                let tx = ((u * (tex_w as f32 - 1.0)).round() as u32).min(tex_w - 1);
                let ty = ((v * (tex_h as f32 - 1.0)).round() as u32).min(tex_h - 1);
                let tex_idx = ((ty * tex_w + tx) * 4) as usize;
                if tex_idx + 3 >= tex_data.len() {
                    continue;
                }

                let buf_idx = (global_y * w + global_x) * 4;
                let alpha = mask_alpha as f32 / 255.0;

                // Replace the fill color with the texture color, modulated by mask alpha
                buf[buf_idx] = (tex_data[tex_idx] as f32 * alpha) as u8;
                buf[buf_idx + 1] = (tex_data[tex_idx + 1] as f32 * alpha) as u8;
                buf[buf_idx + 2] = (tex_data[tex_idx + 2] as f32 * alpha) as u8;
                buf[buf_idx + 3] = mask_alpha;
            }
        }
    }
}

// ─── 3D Text Extrusion ──────────────────────────────────────────────────────

/// Render pseudo-3D text extrusion by drawing offset layers of the alpha mask.
#[allow(clippy::too_many_arguments)]
fn render_extrusion(
    buf: &mut [u8],
    alpha_mask: &[u8],
    mask_w: usize,
    mask_h: usize,
    mask_x0: usize,
    mask_y0: usize,
    buf_w: usize,
    buf_h: usize,
    ext: &klipp_timeline::TextExtrusion,
    title: &TitleData,
) {
    let depth = ext.depth.clamp(1.0, 50.0) as i32;
    let angle_rad = ext.angle.to_radians();
    let cos_a = angle_rad.cos();
    let sin_a = angle_rad.sin();

    // Determine face color (for exclusion — we don't draw the face here)
    let _face_color = if ext.face_color[3] > 0 { ext.face_color } else { title.color };

    // Draw layers from back (farthest) to front (closest to face)
    for i in (1..=depth).rev() {
        let t = i as f32 / depth as f32; // 1.0 = farthest, approaching 0.0 = nearest
        let dx = (cos_a * i as f32).round() as i32;
        let dy = (sin_a * i as f32).round() as i32;

        // Lighting: darken back layers, lighten front layers
        let light_factor = 1.0 - ext.lighting * 0.5 * t; // back is darker
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
    let mut pad = 0i32;
    if let Some(outline) = &title.outline {
        pad = pad.max((outline.width * scale).ceil() as i32 + 1);
    }
    for outline in &title.outlines {
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
    if let Some(inner) = &title.inner_shadow {
        let sx = (inner.offset_x.abs() * scale).ceil() as i32;
        let sy = (inner.offset_y.abs() * scale).ceil() as i32;
        let sb = (inner.blur * scale * 2.0).ceil() as i32;
        pad = pad.max(sx + sb + 1);
        pad = pad.max(sy + sb + 1);
    }
    // Extra padding for tracking (text extends further right)
    if title.tracking > 0.0 {
        pad = pad.max((title.tracking * scale * 200.0).ceil() as i32); // generous for long text
    }
    // Extra padding for extrusion depth
    if let Some(ext) = &title.extrusion {
        let depth = ext.depth.clamp(1.0, 50.0);
        let ex = (depth * ext.angle.to_radians().cos().abs()).ceil() as i32 + 1;
        let ey = (depth * ext.angle.to_radians().sin().abs()).ceil() as i32 + 1;
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
    let base_color = title.color;
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

/// Draw rich text fill: each glyph is drawn with its own color.
#[allow(clippy::too_many_arguments)]
fn draw_rich_text_fill(
    buf: &mut [u8],
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
    buf: &mut [u8],
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
        let no_ext = TitleData {
            text: "3D".to_string(),
            font_size: 100.0,
            ..TitleData::default()
        };
        let with_ext = TitleData {
            extrusion: Some(klipp_timeline::TextExtrusion {
                depth: 10.0,
                angle: 135.0,
                face_color: [0, 0, 0, 0],
                side_color: [100, 50, 50, 255],
                lighting: 0.5,
            }),
            ..no_ext.clone()
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
        let td = TitleData {
            text: "Hello".to_string(),
            font_size: 72.0,
            ..TitleData::default()
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
        let td = TitleData {
            text: "Line1\nLine2".to_string(),
            font_size: 72.0,
            ..TitleData::default()
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

    #[test]
    fn test_char_positions_with_style_runs() {
        let mut renderer = TitleRenderer::new();
        let td = TitleData {
            text: "Hello World".to_string(),
            font_size: 72.0,
            style_runs: vec![
                StyleRun { start: 0, end: 5, font_size: Some(100.0), color: None, font_name: None, font_style: None, tracking: None, baseline_offset: None },
                StyleRun { start: 6, end: 11, font_size: Some(50.0), color: None, font_name: None, font_style: None, tracking: None, baseline_offset: None },
            ],
            ..TitleData::default()
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
        let td = TitleData {
            text: text.to_string(),
            font_size: 72.0,
            ..TitleData::default()
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
        let td = TitleData {
            text: text.to_string(),
            font_size: 72.0,
            style_runs: vec![
                StyleRun { start: 0, end: 3, font_size: Some(100.0), color: None, font_name: None, font_style: None, tracking: None, baseline_offset: None }, // ABC
                StyleRun { start: 3, end: 7, font_size: Some(50.0), color: None, font_name: None, font_style: None, tracking: None, baseline_offset: None },  // DEFG
                StyleRun { start: 7, end: 10, font_size: Some(72.0), color: None, font_name: None, font_style: None, tracking: None, baseline_offset: None }, // HIJ
            ],
            ..TitleData::default()
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
            modified.insert_str(byte_pos, "X");

            // Verify insertion at position ci puts X after the ci-th character
            let chars: Vec<char> = modified.chars().collect();
            assert_eq!(chars[ci], 'X', "Insert at char {} should place X there, got '{}' in '{}'", ci, chars[ci], modified);
        }
    }
}
