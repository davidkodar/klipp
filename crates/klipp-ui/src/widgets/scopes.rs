use crate::state::{ScopeMode, ScopeScale};
use klipp_engine::SharedFrameBuffer;
use std::time::Instant;

/// Scope render result — either from GPU engine or CPU direct rendering.
enum ScopeImage {
    Ready(egui::ColorImage),
}

/// Persistent state for scope rendering (texture cache + throttling).
pub struct ScopeState {
    /// One cached texture per scope mode — switching is instant.
    textures: [Option<egui::TextureHandle>; 4], // Waveform, Parade, Vectorscope, Histogram
    /// Frame generation counter used for each cached texture (from FrameBuffer.generation).
    frame_generations: [u64; 4],
    /// Render sizes used for each cached texture.
    render_sizes: [(usize, usize); 4],
    /// Current scale mode (synced from AppState each frame).
    pub scale: ScopeScale,
    /// Enable horizontal low pass filter on scope output.
    pub low_pass: bool,
    /// Enable bloom on dense regions.
    pub bloom: bool,
    /// R/G/B channel pill states (synced from AppState each frame).
    pub show_r: bool,
    pub show_g: bool,
    pub show_b: bool,
    /// Quality setting (synced from AppState each frame).
    pub quality: klipp_core::ScopeQuality,
    /// Incremented when any scope setting changes (triggers re-render).
    pub settings_generation: u64,
    last_settings_generation: u64,
    /// Throttle: last time a scope render completed.
    last_render_time: Option<Instant>,
}

fn mode_index(mode: ScopeMode) -> usize {
    match mode {
        ScopeMode::Waveform => 0,
        ScopeMode::Parade => 1,
        ScopeMode::Vectorscope => 2,
        ScopeMode::Histogram => 3,
    }
}

impl ScopeState {
    /// Bump the settings generation counter to trigger a re-render.
    pub fn increment_settings_generation(&mut self) {
        self.settings_generation = self.settings_generation.wrapping_add(1);
    }
}

impl Default for ScopeState {
    fn default() -> Self {
        Self {
            textures: [None, None, None, None],
            frame_generations: [u64::MAX; 4], // MAX ensures first frame always triggers render
            render_sizes: [(0, 0); 4],
            scale: ScopeScale::TenBit,
            low_pass: false,
            bloom: true,
            show_r: true,
            show_g: true,
            show_b: true,
            quality: klipp_core::ScopeQuality::Medium,
            settings_generation: 0,
            last_settings_generation: u64::MAX,
            last_render_time: None,
        }
    }
}

// ── Scale system constants ─────────────────────────────────────────────
pub(crate) const AXIS_W: f32 = 40.0;
pub(crate) const PAD_TOP: f32 = 14.0;
pub(crate) const PAD_BOTTOM: f32 = 16.0;
pub(crate) const PAD_RIGHT: f32 = 8.0;

fn val_to_y(val: f32, rect: &egui::Rect, scale: ScopeScale) -> f32 {
    let max = scale.max_val();
    let draw_h = rect.height() - PAD_TOP - PAD_BOTTOM;
    rect.min.y + PAD_TOP + (1.0 - val / max) * draw_h
}

/// Return major tick values for the given scale mode.
fn major_ticks(scale: ScopeScale) -> Vec<u32> {
    match scale {
        ScopeScale::TenBit => vec![0, 128, 256, 384, 512, 640, 768, 896, 1023],
        _ => (0..=10).map(|i| i * 10).collect(),
    }
}

pub(crate) fn draw_scale(ui: &egui::Ui, rect: egui::Rect, scale: ScopeScale, is_parade: bool, section_count: usize) {
    let painter = ui.painter_at(rect);
    let amber = |a: u8| egui::Color32::from_rgba_unmultiplied(0xf5, 0xb4, 0x3c, a);
    let font = egui::FontId::monospace(9.0);

    let major_vals: Vec<f32> = major_ticks(scale).into_iter().map(|v| v as f32).collect();
    let minor_step: f32 = match scale {
        ScopeScale::TenBit => 32.0,
        _ => 5.0,
    };

    // Minor grid lines — rgba(245,180,60,0.04) full width, 3px tick rgba(245,180,60,0.25)
    let max = scale.max_val();
    let mut v = 0.0;
    while v <= max {
        let y = val_to_y(v, &rect, scale);
        if !major_vals.iter().any(|&mv| (mv - v).abs() < 0.1) {
            painter.line_segment(
                [egui::pos2(rect.min.x + AXIS_W, y), egui::pos2(rect.max.x, y)],
                egui::Stroke::new(0.5, amber(10)),
            );
            // 3px tick on axis
            painter.line_segment(
                [egui::pos2(rect.min.x + AXIS_W - 3.0, y), egui::pos2(rect.min.x + AXIS_W, y)],
                egui::Stroke::new(0.5, amber(64)),
            );
        }
        v += minor_step;
    }

    // Major grid lines + labels — rgba(245,180,60,0.20) full width, 6px tick rgba(245,180,60,0.60)
    for &val in &major_vals {
        let y = val_to_y(val, &rect, scale);
        painter.line_segment(
            [egui::pos2(rect.min.x + AXIS_W, y), egui::pos2(rect.max.x, y)],
            egui::Stroke::new(0.5, amber(51)),
        );
        // 6px tick on axis
        painter.line_segment(
            [egui::pos2(rect.min.x + AXIS_W - 6.0, y), egui::pos2(rect.min.x + AXIS_W, y)],
            egui::Stroke::new(0.5, amber(153)),
        );
        // Label — right-aligned to axis, 0 sits BELOW (TOP baseline), others sit ABOVE (BOTTOM baseline)
        let label = format!("{}", val as u32);
        let align = if val < 0.1 { egui::Align2::RIGHT_TOP } else { egui::Align2::RIGHT_BOTTOM };
        painter.text(egui::pos2(rect.min.x + AXIS_W - 8.0, y), align, &label, font.clone(), amber(191));
    }

    // Axis separator — 1px vertical at x=AXIS_W, rgba(255,255,255,0.08)
    painter.line_segment(
        [egui::pos2(rect.min.x + AXIS_W, rect.min.y), egui::pos2(rect.min.x + AXIS_W, rect.max.y - PAD_BOTTOM)],
        egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 20)),
    );

    // Bottom baseline — horizontal at y=(height-PAD_BOTTOM), rgba(255,255,255,0.08)
    let bottom_y = rect.max.y - PAD_BOTTOM;
    painter.line_segment(
        [egui::pos2(rect.min.x + AXIS_W, bottom_y), egui::pos2(rect.max.x, bottom_y)],
        egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 20)),
    );

    // Bottom ticks — major every 25% (6px, amber 0.40), minor between (3px, amber 0.15)
    let trace_w = rect.width() - AXIS_W - PAD_RIGHT;
    for i in 0..=20 {
        let frac = i as f32 / 20.0;
        let x = rect.min.x + AXIS_W + frac * trace_w;
        let is_major = i % 5 == 0;
        let tick_h = if is_major { 6.0 } else { 3.0 };
        let color = if is_major { amber(102) } else { amber(38) };
        painter.line_segment(
            [egui::pos2(x, bottom_y), egui::pos2(x, bottom_y + tick_h)],
            egui::Stroke::new(0.5, color),
        );
    }

    // Parade section separators — rgba(255,255,255,0.05) within trace area
    if is_parade && section_count == 3 {
        let third = trace_w / 3.0;
        for i in 1..3 {
            let x = rect.min.x + AXIS_W + third * i as f32;
            painter.line_segment(
                [egui::pos2(x, rect.min.y + PAD_TOP), egui::pos2(x, bottom_y)],
                egui::Stroke::new(0.5, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 13)),
            );
        }
    }
}

/// Apply 5-tap horizontal Gaussian low pass filter to a ColorImage.
fn apply_low_pass_filter(image: &mut egui::ColorImage) {
    let w = image.size[0];
    let h = image.size[1];
    let weights = [0.05_f32, 0.20, 0.50, 0.20, 0.05];
    let src = image.pixels.clone();
    for y in 0..h {
        for x in 2..w.saturating_sub(2) {
            let idx = x + y * w;
            let mut r = 0.0_f32;
            let mut g = 0.0_f32;
            let mut b = 0.0_f32;
            for (di, &wt) in weights.iter().enumerate() {
                let si = idx - 2 + di;
                r += src[si].r() as f32 * wt;
                g += src[si].g() as f32 * wt;
                b += src[si].b() as f32 * wt;
            }
            image.pixels[idx] = egui::Color32::from_rgb(r as u8, g as u8, b as u8);
        }
    }
}

/// Show the complete scope panel with header, scale, and traces.
pub fn show_scopes(
    ui: &mut egui::Ui,
    state: &mut ScopeState,
    mode: ScopeMode,
    frame_buffer: &SharedFrameBuffer,
    _quality: klipp_core::ScopeQuality,
    skin_tone_angle: f32,
    is_playing: bool,
    scope_cursor: Option<crate::state::ScopeCursorData>,
    show_cursor: bool,
) {
    let render_h: usize = 512;

    let mi = mode_index(mode);

    // ── Check for GPU-rendered scope texture from engine ──────────────────
    // Minimize mutex hold time: snapshot data, drop lock, then render CPU scopes outside lock
    let scope_result = {
        // Phase 1: under lock — check staleness, grab GPU tex or snapshot frame data
        enum ScopeAction {
            None,
            GpuReady(egui::ColorImage, u64),
            CpuRender(Vec<u8>, u32, u32, u64),
        }
        let action = {
            let fb = frame_buffer.lock().unwrap();
            if fb.width == 0 || fb.height == 0 || fb.data.is_empty() {
                ScopeAction::None
            } else {
                let gen = fb.generation;
                let stale = state.textures[mi].is_none()
                    || state.frame_generations[mi] != gen
                    || state.render_sizes[mi] != (fb.width as usize, render_h)
                    || state.last_settings_generation != state.settings_generation;

                let throttled = is_playing && state.last_render_time
                    .map_or(false, |t| t.elapsed().as_millis() < 80);
                if stale && !throttled {
                    let has_gpu_tex = !fb.scope_texture.is_empty() && fb.scope_texture_w > 0
                        && fb.scope_texture_mode == mi as u8;
                    if has_gpu_tex {
                        let pixels: Vec<egui::Color32> = fb.scope_texture.chunks_exact(4)
                            .map(|c| egui::Color32::from_rgba_premultiplied(c[0], c[1], c[2], c[3]))
                            .collect();
                        let image = egui::ColorImage {
                            size: [fb.scope_texture_w as usize, fb.scope_texture_h as usize],
                            pixels,
                        };
                        ScopeAction::GpuReady(image, gen)
                    } else {
                        // Snapshot frame data — drop lock before CPU render
                        let (data, w, h) = if !fb.scope_preview.is_empty() && fb.scope_preview_w > 0 {
                            (fb.scope_preview.clone(), fb.scope_preview_w, fb.scope_preview_h)
                        } else {
                            (fb.data.clone(), fb.width, fb.height)
                        };
                        ScopeAction::CpuRender(data, w, h, gen)
                    }
                } else {
                    ScopeAction::None
                }
            }
        }; // lock dropped here

        // Phase 2: outside lock — CPU scope rendering (no mutex contention)
        match action {
            ScopeAction::None => None,
            ScopeAction::GpuReady(image, gen) => Some((ScopeImage::Ready(image), gen)),
            ScopeAction::CpuRender(src, w, h, gen) => {
                let step: u32 = if w > 1280 { 4 } else if w > 640 { 3 } else { 2 };
                let render_w = w as usize;
                let image = match mode {
                    ScopeMode::Waveform => render_waveform_direct(&src, w, h, render_w, render_h),
                    ScopeMode::Parade => render_parade_direct(&src, w, h, render_w, render_h),
                    ScopeMode::Histogram => {
                        let hist = klipp_color::scopes::compute_histogram_sampled(&src, w, h, step);
                        render_histogram_from_bins(&hist.r, &hist.g, &hist.b, &hist.luma, render_w, render_h)
                    }
                    ScopeMode::Vectorscope => render_vectorscope(&src, w, h, step, render_w, render_h, skin_tone_angle),
                };
                Some((ScopeImage::Ready(image), gen))
            }
        }
    };

    if let Some((scope_img, gen)) = scope_result {
        state.frame_generations[mi] = gen;
        state.last_settings_generation = state.settings_generation;
        state.last_render_time = Some(Instant::now());
        for idx in 0..4 {
            if idx != mi { state.frame_generations[idx] = u64::MAX; }
        }

        let mut image = match scope_img {
            ScopeImage::Ready(img) => img,
        };
        state.render_sizes[mi] = (image.size[0], image.size[1]);

        // Post-processing: threshold bloom (extract bright → gaussian blur → additive blend)
        if state.bloom && mode != ScopeMode::Vectorscope {
            let w = image.size[0];
            let h = image.size[1];
            let threshold = 0.25_f32;
            let bloom_strength = 0.4_f32;
            let radius = 8usize;

            // 1. Extract bright pass
            let mut bright: Vec<[f32; 3]> = image.pixels.iter().map(|p| {
                let r = p.r() as f32 / 255.0;
                let g = p.g() as f32 / 255.0;
                let b = p.b() as f32 / 255.0;
                let lum = r * 0.3 + g * 0.6 + b * 0.1;
                if lum > threshold {
                    [r, g, b]
                } else {
                    [0.0, 0.0, 0.0]
                }
            }).collect();

            // 2. Separable Gaussian blur (horizontal then vertical)
            for _pass in 0..1 {
                // Horizontal
                let src = bright.clone();
                for y in 0..h {
                    for x in radius..w.saturating_sub(radius) {
                        let mut sum = [0.0_f32; 3];
                        let mut wt_sum = 0.0_f32;
                        for dx in 0..=radius * 2 {
                            let sx = x + dx - radius;
                            let d = (dx as f32 - radius as f32).abs();
                            let wt = (-d * d / (2.0 * (radius as f32 * 0.5).powi(2))).exp();
                            sum[0] += src[y * w + sx][0] * wt;
                            sum[1] += src[y * w + sx][1] * wt;
                            sum[2] += src[y * w + sx][2] * wt;
                            wt_sum += wt;
                        }
                        bright[y * w + x] = [sum[0] / wt_sum, sum[1] / wt_sum, sum[2] / wt_sum];
                    }
                }
                // Vertical
                let src = bright.clone();
                for x in 0..w {
                    for y in radius..h.saturating_sub(radius) {
                        let mut sum = [0.0_f32; 3];
                        let mut wt_sum = 0.0_f32;
                        for dy in 0..=radius * 2 {
                            let sy = y + dy - radius;
                            let d = (dy as f32 - radius as f32).abs();
                            let wt = (-d * d / (2.0 * (radius as f32 * 0.5).powi(2))).exp();
                            sum[0] += src[sy * w + x][0] * wt;
                            sum[1] += src[sy * w + x][1] * wt;
                            sum[2] += src[sy * w + x][2] * wt;
                            wt_sum += wt;
                        }
                        bright[y * w + x] = [sum[0] / wt_sum, sum[1] / wt_sum, sum[2] / wt_sum];
                    }
                }
            }

            // 3. Additive blend back
            for (pixel, bloom) in image.pixels.iter_mut().zip(bright.iter()) {
                let r = (pixel.r() as f32 / 255.0 + bloom[0] * bloom_strength).min(1.0);
                let g = (pixel.g() as f32 / 255.0 + bloom[1] * bloom_strength).min(1.0);
                let b = (pixel.b() as f32 / 255.0 + bloom[2] * bloom_strength).min(1.0);
                *pixel = egui::Color32::from_rgb(
                    (r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8,
                );
            }
        }

        // Post-processing: horizontal low pass filter (5-tap gaussian)
        if state.low_pass && mode != ScopeMode::Vectorscope {
            apply_low_pass_filter(&mut image);
        }

        if let Some(ref mut tex) = state.textures[mi] {
            tex.set(image, egui::TextureOptions::LINEAR);
        } else {
            state.textures[mi] = Some(ui.ctx().load_texture(
                format!("scope_{mi}"), image, egui::TextureOptions::LINEAR,
            ));
        }
    }

    // Display the cached texture with scale overlay
    let available = ui.available_size();
    if let Some(tex) = &state.textures[mi] {
        ui.ctx().request_repaint();

        let is_vector = mode == ScopeMode::Vectorscope;
        let (full_rect, _) = ui.allocate_exact_size(available, egui::Sense::hover());

        // Dark background
        ui.painter().rect_filled(full_rect, 0.0, egui::Color32::from_rgb(0x04, 0x04, 0x07));

        if is_vector {
            // Vectorscope: centered square, no scale
            let side = full_rect.width().min(full_rect.height());
            let offset_x = (full_rect.width() - side) / 2.0;
            let offset_y = (full_rect.height() - side) / 2.0;
            let img_rect = egui::Rect::from_min_size(
                egui::pos2(full_rect.min.x + offset_x, full_rect.min.y + offset_y),
                egui::vec2(side, side),
            );
            ui.painter().image(tex.id(), img_rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE);

            // Vectorscope labels
            let label_font = egui::FontId::monospace(8.0);
            let label_color = egui::Color32::from_rgb(0x90, 0x78, 0x30);
            let cx = img_rect.center().x;
            let cy = img_rect.center().y;
            let r = side / 2.0 - 4.0;
            for (name, cb, cr) in &[("R", 0.17_f32, 0.29_f32), ("Mg", 0.28, -0.05), ("B", 0.12, -0.34),
                                     ("Cy", -0.17, -0.29), ("G", -0.28, 0.05), ("Yl", -0.12, 0.34)] {
                let tx = cx + cb * r * 2.0;
                let ty = cy - cr * r * 2.0 - 6.0;
                ui.painter().text(egui::pos2(tx, ty), egui::Align2::CENTER_BOTTOM, name, label_font.clone(), label_color);
            }
        } else {
            // Waveform/Parade/Histogram: scope image + scale overlay
            let img_rect = egui::Rect::from_min_max(
                egui::pos2(full_rect.min.x + AXIS_W, full_rect.min.y + PAD_TOP),
                egui::pos2(full_rect.max.x - PAD_RIGHT, full_rect.max.y - PAD_BOTTOM),
            );
            ui.painter().image(tex.id(), img_rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE);

            // Draw scale system on top
            // Read scale from the scope_state (passed via the ScopeState struct)
            let scale = state.scale;
            let is_parade = mode == ScopeMode::Parade;
            draw_scale(ui, full_rect, scale, is_parade, 3);
        }

        // ── Scope cursor indicator ──
        if show_cursor {
            if let Some(ref cur) = scope_cursor {
                let scope_type = crate::state::ScopeType::from(mode);
                super::scope_window::draw_scope_cursor_indicator(
                    &ui.painter_at(full_rect), scope_type, cur, full_rect,
                );
            }
        }
    } else {
        ui.label(egui::RichText::new("No frame data").color(egui::Color32::from_rgb(0x50, 0x50, 0x60)).size(10.0));
    }
}

// ── Scope rendering pipeline ──────────────────────────────────────────────
//
// Technique (matching FCP/DaVinci quality):
// 1. Accumulate into f32 RGB buffer (no clamping — allows values > 1.0)
// 2. Apply bloom: extract bright areas, gaussian blur, blend back
// 3. Tone-map from HDR accumulation to display range
// 4. Apply gamma correction for perceptual brightness
// 5. Convert to u8 ColorImage

/// Accumulation buffer — f32 per channel, allows HDR values > 1.0
struct AccumBuffer {
    data: Vec<[f32; 3]>,
    w: usize,
    h: usize,
}

impl AccumBuffer {
    fn new(w: usize, h: usize) -> Self {
        Self { data: vec![[0.0; 3]; w * h], w, h }
    }

    #[inline]
    fn add(&mut self, x: usize, y: usize, r: f32, g: f32, b: f32) {
        if x < self.w && y < self.h {
            let idx = y * self.w + x;
            self.data[idx][0] += r;
            self.data[idx][1] += g;
            self.data[idx][2] += b;
        }
    }

    /// Apply bloom: extract bright pixels, blur, blend back
    #[inline(never)]
    #[allow(dead_code)]
    fn apply_bloom(&mut self, radius: usize, threshold: f32, strength: f32) {
        let w = self.w;
        let h = self.h;
        // Extract bright pixels
        let mut bright: Vec<[f32; 3]> = self.data.iter().map(|p| {
            let lum = p[0] * 0.2126 + p[1] * 0.7152 + p[2] * 0.0722;
            if lum > threshold {
                let excess = lum - threshold;
                [p[0] * excess, p[1] * excess, p[2] * excess]
            } else {
                [0.0; 3]
            }
        }).collect();

        // Simple box blur (fast approximation of gaussian)
        for _pass in 0..2 {
            // Horizontal
            let mut temp = bright.clone();
            for y in 0..h {
                for x in 0..w {
                    let mut sum = [0.0_f32; 3];
                    let mut count = 0.0;
                    let x_start = x.saturating_sub(radius);
                    let x_end = (x + radius + 1).min(w);
                    for bx in x_start..x_end {
                        sum[0] += bright[y * w + bx][0];
                        sum[1] += bright[y * w + bx][1];
                        sum[2] += bright[y * w + bx][2];
                        count += 1.0;
                    }
                    temp[y * w + x] = [sum[0] / count, sum[1] / count, sum[2] / count];
                }
            }
            bright = temp;
            // Vertical
            let mut temp = bright.clone();
            for y in 0..h {
                for x in 0..w {
                    let mut sum = [0.0_f32; 3];
                    let mut count = 0.0;
                    let y_start = y.saturating_sub(radius);
                    let y_end = (y + radius + 1).min(h);
                    for by in y_start..y_end {
                        sum[0] += bright[by * w + x][0];
                        sum[1] += bright[by * w + x][1];
                        sum[2] += bright[by * w + x][2];
                        count += 1.0;
                    }
                    temp[y * w + x] = [sum[0] / count, sum[1] / count, sum[2] / count];
                }
            }
            bright = temp;
        }

        // Blend bloom back
        for (p, b) in self.data.iter_mut().zip(bright.iter()) {
            p[0] += b[0] * strength;
            p[1] += b[1] * strength;
            p[2] += b[2] * strength;
        }
    }

    /// Convert to ColorImage with tone mapping + fast gamma approximation.
    fn to_image(&self, bg: [f32; 3], gamma: f32) -> egui::ColorImage {
        // Pre-compute gamma LUT (256 entries) — avoids powf() per pixel per channel
        let inv_gamma = 1.0 / gamma;
        let gamma_lut: [u8; 256] = {
            let mut lut = [0u8; 256];
            for i in 0..256 {
                let v = i as f32 / 255.0;
                lut[i] = (v.powf(inv_gamma) * 255.0).min(255.0) as u8;
            }
            lut
        };

        let pixels = self.data.iter().map(|p| {
            // Fast filmic tone mapping (no powf, just rational approximation)
            let tone = |x: f32| -> f32 {
                let x = x.max(0.0);
                let a = x * (x * 0.6 + 0.4);
                let b = x * (x * 0.6 + 0.85) + 0.15;
                (a / b).min(1.0)
            };

            let r = (tone(p[0]) * 255.0).min(255.0) as u8;
            let g = (tone(p[1]) * 255.0).min(255.0) as u8;
            let b = (tone(p[2]) * 255.0).min(255.0) as u8;

            // Gamma via LUT + background add
            let r = gamma_lut[r as usize].saturating_add(bg[0] as u8);
            let g = gamma_lut[g as usize].saturating_add(bg[1] as u8);
            let b = gamma_lut[b as usize].saturating_add(bg[2] as u8);
            egui::Color32::from_rgb(r, g, b)
        }).collect();

        egui::ColorImage { size: [self.w, self.h], pixels }
    }
}

/// Render waveform using DaVinci's additive accumulation technique.
/// Each video pixel maps to a point in scope space. Additive blending + log tone mapping = natural glow.
pub(crate) fn render_waveform_direct(rgba: &[u8], video_w: u32, video_h: u32, scope_w: usize, scope_h: usize) -> egui::ColorImage {
    // 1. Accumulate: each video pixel adds 1 at (scope_x, luma * scope_h)
    let mut accum = vec![0u32; scope_w * scope_h];

    for y in 0..video_h {
        for x in 0..video_w {
            let idx = ((y * video_w + x) * 4) as usize;
            if idx + 2 >= rgba.len() { break; }
            let r = rgba[idx] as f32 / 255.0;
            let g = rgba[idx + 1] as f32 / 255.0;
            let b = rgba[idx + 2] as f32 / 255.0;
            let luma = (0.2126 * r + 0.7152 * g + 0.0722 * b).clamp(0.0, 0.999);

            let sx = (x as usize * scope_w) / video_w as usize;
            let sy = scope_h - 1 - (luma * scope_h as f32) as usize;
            if sx < scope_w && sy < scope_h {
                accum[sy * scope_w + sx] += 1;
            }
        }
    }

    // 2. Resolve: phosphor color ramp (mix base_color → white based on intensity)
    let knee = 3.0_f32;
    let base_r = 0.0_f32;
    let base_g = 0.9_f32;
    let base_b = 0.0_f32;

    let pixels: Vec<egui::Color32> = accum.iter().map(|&count| {
        if count == 0 {
            return egui::Color32::BLACK;
        }
        let c = count as f32;
        let intensity = (c / (c + knee)).sqrt().clamp(0.0, 1.0);

        // Phosphor ramp: mix(base_color, white, intensity³)
        let white_mix = intensity * intensity * intensity; // cubic — stays colored longer
        let r = (base_r + (1.0 - base_r) * white_mix) * intensity;
        let g = (base_g + (1.0 - base_g) * white_mix) * intensity;
        let b = (base_b + (1.0 - base_b) * white_mix) * intensity;

        egui::Color32::from_rgb(
            (r * 255.0).min(255.0) as u8,
            (g * 255.0).min(255.0) as u8,
            (b * 255.0).min(255.0) as u8,
        )
    }).collect();

    egui::ColorImage { size: [scope_w, scope_h], pixels }
}

/// Render waveform for a specific channel (Y=luma, R, G, B).
/// When `colorize` is true, traces show actual pixel colors instead of monochrome.
pub(crate) fn render_waveform_channel(
    rgba: &[u8], video_w: u32, video_h: u32, scope_w: usize, scope_h: usize,
    channel: crate::state::WaveformChannel, colorize: bool,
) -> egui::ColorImage {
    use crate::state::WaveformChannel;

    if colorize {
        // Colorized mode: accumulate actual RGB colors
        let mut buf = AccumBuffer::new(scope_w, scope_h);
        for y in 0..video_h {
            for x in 0..video_w {
                let idx = ((y * video_w + x) * 4) as usize;
                if idx + 2 >= rgba.len() { break; }
                let r = rgba[idx] as f32 / 255.0;
                let g = rgba[idx + 1] as f32 / 255.0;
                let b = rgba[idx + 2] as f32 / 255.0;
                let val = match channel {
                    WaveformChannel::Y => (0.2126 * r + 0.7152 * g + 0.0722 * b).clamp(0.0, 0.999),
                    WaveformChannel::R => r.clamp(0.0, 0.999),
                    WaveformChannel::G => g.clamp(0.0, 0.999),
                    WaveformChannel::B => b.clamp(0.0, 0.999),
                };
                let sx = (x as usize * scope_w) / video_w as usize;
                let sy = scope_h - 1 - (val * scope_h as f32) as usize;
                if sx < scope_w && sy < scope_h {
                    buf.add(sx, sy, r * 0.5, g * 0.5, b * 0.5);
                }
            }
        }
        return buf.to_image([0.0, 0.0, 0.0], 1.8);
    }

    // Monochrome mode: simple count accumulation with phosphor ramp
    let mut accum = vec![0u32; scope_w * scope_h];

    for y in 0..video_h {
        for x in 0..video_w {
            let idx = ((y * video_w + x) * 4) as usize;
            if idx + 2 >= rgba.len() { break; }
            let r = rgba[idx] as f32 / 255.0;
            let g = rgba[idx + 1] as f32 / 255.0;
            let b = rgba[idx + 2] as f32 / 255.0;

            let val = match channel {
                WaveformChannel::Y => (0.2126 * r + 0.7152 * g + 0.0722 * b).clamp(0.0, 0.999),
                WaveformChannel::R => r.clamp(0.0, 0.999),
                WaveformChannel::G => g.clamp(0.0, 0.999),
                WaveformChannel::B => b.clamp(0.0, 0.999),
            };

            let sx = (x as usize * scope_w) / video_w as usize;
            let sy = scope_h - 1 - (val * scope_h as f32) as usize;
            if sx < scope_w && sy < scope_h {
                accum[sy * scope_w + sx] += 1;
            }
        }
    }

    let knee = 3.0_f32;
    let (base_r, base_g, base_b) = match channel {
        WaveformChannel::Y => (0.0, 0.9, 0.0), // green luma trace (standard)
        WaveformChannel::R => (0.9, 0.0, 0.0),
        WaveformChannel::G => (0.0, 0.9, 0.0),
        WaveformChannel::B => (0.0, 0.0, 0.9),
    };

    let pixels: Vec<egui::Color32> = accum.iter().map(|&count| {
        if count == 0 { return egui::Color32::BLACK; }
        let c = count as f32;
        let intensity = (c / (c + knee)).sqrt().clamp(0.0, 1.0);
        let white_mix = intensity * intensity * intensity;
        let r = (base_r + (1.0 - base_r) * white_mix) * intensity;
        let g = (base_g + (1.0 - base_g) * white_mix) * intensity;
        let b = (base_b + (1.0 - base_b) * white_mix) * intensity;
        egui::Color32::from_rgb(
            (r * 255.0).min(255.0) as u8,
            (g * 255.0).min(255.0) as u8,
            (b * 255.0).min(255.0) as u8,
        )
    }).collect();

    egui::ColorImage { size: [scope_w, scope_h], pixels }
}

/// Render histogram as 3 stacked R/G/B bands (for floating window).
pub(crate) fn render_histogram_stacked(
    r_bins: &[u32; 256], g_bins: &[u32; 256], b_bins: &[u32; 256],
    rw: usize, rh: usize,
) -> egui::ColorImage {
    let band_h = rh / 3;
    let mut pixels = vec![egui::Color32::BLACK; rw * rh];

    let channels: [(&[u32; 256], [f32; 3], usize); 3] = [
        (r_bins, [1.0, 0.12, 0.08], 0),
        (g_bins, [0.08, 0.95, 0.12], band_h),
        (b_bins, [0.12, 0.25, 1.0], band_h * 2),
    ];

    for (bins, color, y_offset) in &channels {
        let max_count = bins.iter().copied().max().unwrap_or(1).max(1) as f32;
        let bin_scale = rw as f32 / 256.0;

        for px_x in 0..rw {
            let bin = ((px_x as f32 / bin_scale) as usize).min(255);
            let count = bins[bin];
            if count == 0 { continue; }
            let height_frac = (count as f32 / max_count).sqrt();
            let height_px = (height_frac * band_h as f32) as usize;

            for dy in 0..height_px.min(band_h) {
                let y = y_offset + band_h - 1 - dy;
                if y >= rh { continue; }
                let fade = 1.0 - (dy as f32 / height_px.max(1) as f32) * 0.5;
                let r = (color[0] * fade * 255.0).min(255.0) as u8;
                let g = (color[1] * fade * 255.0).min(255.0) as u8;
                let b = (color[2] * fade * 255.0).min(255.0) as u8;
                pixels[y * rw + px_x] = egui::Color32::from_rgb(r, g, b);
            }
        }
    }

    // Thin separators between bands
    let sep_color = egui::Color32::from_rgb(0x18, 0x18, 0x1c);
    for &y in &[band_h, band_h * 2] {
        if y < rh {
            for x in 0..rw {
                pixels[y * rw + x] = sep_color;
            }
        }
    }

    egui::ColorImage { size: [rw, rh], pixels }
}

/// Render parade using DaVinci's additive accumulation technique.
/// Three separate accumulation buffers for R, G, B displayed side by side.
pub(crate) fn render_parade_direct(rgba: &[u8], video_w: u32, video_h: u32, scope_w: usize, scope_h: usize) -> egui::ColorImage {
    let gap = 2usize;
    let section_w = (scope_w - gap * 2) / 3;

    // 1. Accumulate into three separate buffers
    let mut accum_r = vec![0u32; section_w * scope_h];
    let mut accum_g = vec![0u32; section_w * scope_h];
    let mut accum_b = vec![0u32; section_w * scope_h];

    for y in 0..video_h {
        for x in 0..video_w {
            let idx = ((y * video_w + x) * 4) as usize;
            if idx + 2 >= rgba.len() { break; }
            let rv = rgba[idx] as f32 / 255.0;
            let gv = rgba[idx + 1] as f32 / 255.0;
            let bv = rgba[idx + 2] as f32 / 255.0;

            let sx = (x as usize * section_w) / video_w as usize;
            if sx >= section_w { continue; }

            let ry = scope_h - 1 - (rv.clamp(0.0, 0.999) * scope_h as f32) as usize;
            let gy = scope_h - 1 - (gv.clamp(0.0, 0.999) * scope_h as f32) as usize;
            let by = scope_h - 1 - (bv.clamp(0.0, 0.999) * scope_h as f32) as usize;

            if ry < scope_h { accum_r[ry * section_w + sx] += 1; }
            if gy < scope_h { accum_g[gy * section_w + sx] += 1; }
            if by < scope_h { accum_b[by * section_w + sx] += 1; }
        }
    }

    // 2. Resolve: phosphor color ramp per channel (mix base → white)
    let knee = 3.0_f32;
    let bg = egui::Color32::BLACK;
    let gap_color = egui::Color32::from_rgb(0x06, 0x06, 0x06);

    // Pure saturated base colors — no secondary channel bleed
    let bases: [[f32; 3]; 3] = [
        [0.9, 0.0, 0.0],  // red
        [0.0, 0.9, 0.0],  // green
        [0.0, 0.0, 0.9],  // blue
    ];

    let resolve = |count: f32, base: &[f32; 3]| -> (u8, u8, u8) {
        let intensity = (count / (count + knee)).sqrt().clamp(0.0, 1.0);
        let white_mix = intensity * intensity * intensity; // cubic — stays colored longer
        let r = (base[0] + (1.0 - base[0]) * white_mix) * intensity;
        let g = (base[1] + (1.0 - base[1]) * white_mix) * intensity;
        let b = (base[2] + (1.0 - base[2]) * white_mix) * intensity;
        ((r * 255.0).min(255.0) as u8, (g * 255.0).min(255.0) as u8, (b * 255.0).min(255.0) as u8)
    };

    let mut pixels = vec![bg; scope_w * scope_h];

    for i in 0..section_w * scope_h {
        let r_count = accum_r[i] as f32;
        let g_count = accum_g[i] as f32;
        let b_count = accum_b[i] as f32;
        if r_count == 0.0 && g_count == 0.0 && b_count == 0.0 { continue; }

        let py = i / section_w;
        let local_x = i % section_w;

        if r_count > 0.0 {
            let px = local_x;
            let (ro, go, bo) = resolve(r_count, &bases[0]);
            pixels[py * scope_w + px] = egui::Color32::from_rgb(ro, go, bo);
        }
        if g_count > 0.0 {
            let px = section_w + gap + local_x;
            let (ro, go, bo) = resolve(g_count, &bases[1]);
            pixels[py * scope_w + px] = egui::Color32::from_rgb(ro, go, bo);
        }
        if b_count > 0.0 {
            let px = (section_w + gap) * 2 + local_x;
            if px < scope_w {
                let (ro, go, bo) = resolve(b_count, &bases[2]);
                pixels[py * scope_w + px] = egui::Color32::from_rgb(ro, go, bo);
            }
        }
    }

    // Gap lines
    for py in 0..scope_h {
        for dx in 0..gap {
            pixels[py * scope_w + section_w + dx] = gap_color;
            pixels[py * scope_w + section_w * 2 + gap + dx] = gap_color;
        }
    }

    egui::ColorImage { size: [scope_w, scope_h], pixels }
}

#[inline(never)]
pub(crate) fn render_histogram_from_bins(r_bins: &[u32; 256], g_bins: &[u32; 256], b_bins: &[u32; 256], luma_bins: &[u32; 256], rw: usize, rh: usize) -> egui::ColorImage {
    let mut buf = AccumBuffer::new(rw, rh);

    let max_count = r_bins.iter().chain(g_bins.iter()).chain(b_bins.iter())
        .copied().max().unwrap_or(1).max(1) as f32;

    let bin_scale = rw as f32 / 256.0;
    let channels: [(&[u32; 256], f32, f32, f32); 4] = [
        (luma_bins, 0.4, 0.4, 0.4),
        (r_bins,    1.0, 0.12, 0.08),
        (g_bins,    0.08, 0.95, 0.12),
        (b_bins,    0.12, 0.25, 1.0),
    ];

    for (data, cr, cg, cb) in &channels {
        for px_x in 0..rw {
            let bin = ((px_x as f32 / bin_scale) as usize).min(255);
            let count = data[bin];
            if count == 0 { continue; }
            let height_frac = (count as f32 / max_count).sqrt();
            let height_px = (height_frac * rh as f32) as usize;

            for dy in 0..height_px.min(rh) {
                let y = rh - 1 - dy;
                let fade = 1.0 - (dy as f32 / height_px.max(1) as f32) * 0.6;
                buf.add(px_x, y, cr * fade * 0.7, cg * fade * 0.7, cb * fade * 0.7);
            }
        }
    }

    buf.to_image([6.0, 6.0, 10.0], 1.6)
}

#[inline(never)]
pub(crate) fn render_vectorscope(rgba: &[u8], width: u32, height: u32, step: u32, rw: usize, rh: usize, skin_tone_angle: f32) -> egui::ColorImage {
    let size = rw.min(rh);
    let mut buf = AccumBuffer::new(size, size);
    let center = size as f32 / 2.0;
    let radius = center - 8.0;

    // 1. Rainbow-colored outer ring (hue spectrum like FCP)
    for gy in 0..size {
        for gx in 0..size {
            let dx = gx as f32 - center;
            let dy = gy as f32 - center;
            let dist = (dx * dx + dy * dy).sqrt();

            // Outer rainbow ring at 100% radius
            if (dist - radius).abs() < 1.5 {
                // Angle → hue color
                let angle = dy.atan2(dx); // -PI to PI
                let hue = (angle / std::f32::consts::PI * 0.5 + 0.5).fract(); // 0..1
                let (r, g, b) = hue_to_rgb(hue);
                let alpha = 1.0 - ((dist - radius).abs() / 1.5); // anti-alias
                buf.add(gx, gy, r * 0.25 * alpha, g * 0.25 * alpha, b * 0.25 * alpha);
            }

            // Inner guide circles — very subtle grey
            for frac in [0.25_f32, 0.50, 0.75] {
                if (dist - radius * frac).abs() < 0.6 {
                    let alpha = 1.0 - ((dist - radius * frac).abs() / 0.6);
                    buf.add(gx, gy, 0.025 * alpha, 0.025 * alpha, 0.035 * alpha);
                }
            }

            // Crosshairs — very subtle
            if dist < radius {
                if dx.abs() < 0.5 {
                    buf.add(gx, gy, 0.02, 0.02, 0.03);
                }
                if dy.abs() < 0.5 {
                    buf.add(gx, gy, 0.02, 0.02, 0.03);
                }
            }
        }
    }

    // 2. Skin tone reference line (I-line) from center at configurable angle
    // Convert from the standard vectorscope convention (123 deg = skin tone I-line)
    // to our coordinate system: angle in degrees, measured counter-clockwise from +X axis.
    // The vectorscope uses Cb/Cr axes where skin tone is at ~33 deg in our rendering space.
    // User-facing angle of 123 deg maps to 33 deg in the rendered Cb/Cr space.
    let skin_rad = (skin_tone_angle - 90.0).to_radians();
    for i in 0..(radius as usize) {
        let t = i as f32;
        let sx = (center + skin_rad.cos() * t) as usize;
        let sy = (center - skin_rad.sin() * t) as usize;
        if sx < size && sy < size {
            // Semi-transparent amber/warm colored line
            buf.add(sx, sy, 0.10, 0.07, 0.02);
            // Wider: draw neighboring pixels for visibility
            if sx + 1 < size { buf.add(sx + 1, sy, 0.05, 0.035, 0.01); }
            if sx > 0 { buf.add(sx - 1, sy, 0.05, 0.035, 0.01); }
        }
    }
    // "I" label at the end of the skin tone line
    {
        let end_t = radius - 12.0;
        let lx = (center + skin_rad.cos() * end_t) as usize;
        let ly = (center - skin_rad.sin() * end_t) as usize;
        // Draw a simple 3x5 "I" glyph
        let glyph = [
            (0, 0), (1, 0), (2, 0),  // top bar
            (1, 1), (1, 2), (1, 3),   // stem
            (0, 4), (1, 4), (2, 4),   // bottom bar
        ];
        for (dx, dy) in &glyph {
            let gx = lx.wrapping_add(*dx as usize);
            let gy = ly.wrapping_add(*dy as usize);
            if gx < size && gy < size {
                buf.add(gx, gy, 0.15, 0.10, 0.03);
            }
        }
    }

    // 3. Color target boxes at standard positions
    let targets = [
        (0.1688_f32, 0.2933_f32, "R"),
        (0.2833, -0.0500, "MG"),
        (0.1150, -0.3433, "B"),
        (-0.1688, -0.2933, "CY"),
        (-0.2833, 0.0500, "G"),
        (-0.1150, 0.3433, "YL"),
    ];
    for (cb_t, cr_t, _label) in &targets {
        let tx = (center + cb_t * radius * 2.0) as usize;
        let ty = (center - cr_t * radius * 2.0) as usize;
        // Draw small box outline
        for dx in 0..5_usize {
            for dy in 0..5_usize {
                let bx = tx.saturating_sub(2) + dx;
                let by = ty.saturating_sub(2) + dy;
                if bx < size && by < size && (dx == 0 || dx == 4 || dy == 0 || dy == 4) {
                    buf.add(bx, by, 0.12, 0.12, 0.15);
                }
            }
        }
    }

    // 4. Plot data points with ACTUAL pixel color + saturation-weighted brightness
    // Pixels with more color (higher saturation) plot brighter — low-saturation pixels
    // near center are dimmed to prevent the white blob from drowning everything out.
    let sample_step = (step * 3).max(3) as usize;
    for y in (0..height).step_by(sample_step) {
        for x in (0..width).step_by(sample_step) {
            let idx = ((y * width + x) * 4) as usize;
            if idx + 2 >= rgba.len() { continue; }
            let r = rgba[idx] as f32 / 255.0;
            let g = rgba[idx + 1] as f32 / 255.0;
            let b = rgba[idx + 2] as f32 / 255.0;
            let cb = -0.1146 * r - 0.3854 * g + 0.5000 * b;
            let cr = 0.5000 * r - 0.4542 * g - 0.0458 * b;

            let px = (center + cb * radius * 2.0) as usize;
            let py = (center - cr * radius * 2.0) as usize;
            if px < size && py < size {
                // Distance from center = saturation level
                let sat = (cb * cb + cr * cr).sqrt() * 2.0; // 0..~1
                // Boost colored pixels, dim neutral ones
                // This prevents the neutral white blob from drowning out colors
                let boost = 0.1 + sat * 1.5; // neutral=0.1, saturated=1.6
                // Use the actual pixel color, boosted by saturation
                buf.add(px, py, r * boost, g * boost, b * boost);
            }
        }
    }

    buf.to_image([4.0, 4.0, 7.0], 2.2)
}

/// Render CIE 1931 xy chromaticity diagram.
/// Plots image pixels in CIE xy space with spectral locus outline and sRGB gamut triangle.
pub(crate) fn render_cie_chromaticity(rgba: &[u8], width: u32, height: u32, step: u32, rw: usize, rh: usize) -> egui::ColorImage {
    let size = rw.min(rh);
    let mut buf = AccumBuffer::new(size, size);

    // The CIE xy diagram spans roughly x=[0, 0.8], y=[0, 0.9].
    // Map to pixel coords with some margin.
    let margin = 12.0;
    let plot_size = size as f32 - margin * 2.0;
    let xy_to_px = |x: f32, y: f32| -> (usize, usize) {
        let px = (margin + x / 0.8 * plot_size) as usize;
        // y=0 at bottom, y=0.9 at top → invert
        let py = (margin + (1.0 - y / 0.9) * plot_size) as usize;
        (px.min(size - 1), py.min(size - 1))
    };

    // ── 1. Spectral locus outline (CIE 1931 standard observer, 380–700nm) ──
    // Subset of xy coordinates along the spectral locus boundary
    let locus: &[(f32, f32)] = &[
        (0.1741, 0.0050), (0.1740, 0.0050), (0.1738, 0.0049), // 380-400nm
        (0.1736, 0.0049), (0.1733, 0.0048), (0.1726, 0.0048),
        (0.1714, 0.0051), (0.1689, 0.0069), (0.1644, 0.0109),
        (0.1566, 0.0177), (0.1440, 0.0297), (0.1241, 0.0578), // 440-460nm
        (0.0913, 0.1327), (0.0687, 0.2007), (0.0454, 0.2950),
        (0.0235, 0.4127), (0.0082, 0.5384), (0.0039, 0.6548),
        (0.0139, 0.7502), (0.0389, 0.8120), (0.0743, 0.8338), // 520-540nm
        (0.1142, 0.8262), (0.1547, 0.8059), (0.1929, 0.7816),
        (0.2296, 0.7543), (0.2658, 0.7243), (0.3016, 0.6923),
        (0.3373, 0.6589), (0.3731, 0.6245), (0.4087, 0.5896), // 580-600nm
        (0.4441, 0.5547), (0.4788, 0.5202), (0.5125, 0.4866),
        (0.5448, 0.4544), (0.5752, 0.4242), (0.6029, 0.3965),
        (0.6270, 0.3725), (0.6482, 0.3514), (0.6658, 0.3340), // 640-660nm
        (0.6801, 0.3197), (0.6915, 0.3083), (0.7006, 0.2993),
        (0.7079, 0.2920), (0.7140, 0.2859), (0.7190, 0.2809),
        (0.7230, 0.2770), (0.7260, 0.2740), (0.7283, 0.2717), // 700-780nm
    ];

    // Draw locus as a connected line
    let locus_color = 0.12_f32;
    for pair in locus.windows(2) {
        let (x0, y0) = xy_to_px(pair[0].0, pair[0].1);
        let (x1, y1) = xy_to_px(pair[1].0, pair[1].1);
        // Bresenham-ish: just draw both endpoints; AccumBuffer handles it
        if x0 < size && y0 < size { buf.add(x0, y0, locus_color, locus_color, locus_color * 1.2); }
        if x1 < size && y1 < size { buf.add(x1, y1, locus_color, locus_color, locus_color * 1.2); }
        // Interpolate some intermediate points for smoother line
        for t in 1..8 {
            let f = t as f32 / 8.0;
            let ix = (pair[0].0 + (pair[1].0 - pair[0].0) * f, pair[0].1 + (pair[1].1 - pair[0].1) * f);
            let (px, py) = xy_to_px(ix.0, ix.1);
            if px < size && py < size { buf.add(px, py, locus_color * 0.6, locus_color * 0.6, locus_color * 0.8); }
        }
    }
    // Close the locus: line from last spectral point to first (purple line)
    if let (Some(first), Some(last)) = (locus.first(), locus.last()) {
        for t in 0..20 {
            let f = t as f32 / 20.0;
            let ix = (last.0 + (first.0 - last.0) * f, last.1 + (first.1 - last.1) * f);
            let (px, py) = xy_to_px(ix.0, ix.1);
            if px < size && py < size { buf.add(px, py, locus_color * 0.8, 0.02, locus_color * 0.8); }
        }
    }

    // ── 2. sRGB / Rec.709 gamut triangle ──
    let srgb_r = (0.6400_f32, 0.3300_f32);
    let srgb_g = (0.3000, 0.6000);
    let srgb_b = (0.1500, 0.0600);
    let gamut_verts = [(srgb_r, srgb_g), (srgb_g, srgb_b), (srgb_b, srgb_r)];
    let gamut_colors = [
        (0.15_f32, 0.06, 0.06), // R-G edge: reddish
        (0.06, 0.12, 0.06),     // G-B edge: greenish
        (0.06, 0.06, 0.15),     // B-R edge: bluish
    ];
    for (edge, color) in gamut_verts.iter().zip(gamut_colors.iter()) {
        for t in 0..60 {
            let f = t as f32 / 60.0;
            let x = edge.0.0 + (edge.1.0 - edge.0.0) * f;
            let y = edge.0.1 + (edge.1.1 - edge.0.1) * f;
            let (px, py) = xy_to_px(x, y);
            if px < size && py < size {
                buf.add(px, py, color.0, color.1, color.2);
                // Thicken
                if px + 1 < size { buf.add(px + 1, py, color.0 * 0.5, color.1 * 0.5, color.2 * 0.5); }
            }
        }
    }

    // ── 3. D65 white point marker ──
    let (wpx, wpy) = xy_to_px(0.3127, 0.3290);
    for dx in 0..3_usize {
        for dy in 0..3_usize {
            let gx = wpx.saturating_sub(1) + dx;
            let gy = wpy.saturating_sub(1) + dy;
            if gx < size && gy < size { buf.add(gx, gy, 0.2, 0.2, 0.2); }
        }
    }

    // ── 4. Plot image data ──
    let sample_step = (step * 3).max(3) as usize;
    for y in (0..height).step_by(sample_step) {
        for x in (0..width).step_by(sample_step) {
            let idx = ((y * width + x) * 4) as usize;
            if idx + 2 >= rgba.len() { continue; }
            // sRGB → linear
            let sr = rgba[idx] as f32 / 255.0;
            let sg = rgba[idx + 1] as f32 / 255.0;
            let sb = rgba[idx + 2] as f32 / 255.0;
            let to_linear = |v: f32| -> f32 {
                if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
            };
            let lr = to_linear(sr);
            let lg = to_linear(sg);
            let lb = to_linear(sb);

            // Linear sRGB → CIE XYZ (D65)
            let cap_x = 0.4124564 * lr + 0.3575761 * lg + 0.1804375 * lb;
            let cap_y = 0.2126729 * lr + 0.7151522 * lg + 0.0721750 * lb;
            let z = 0.0193339 * lr + 0.1191920 * lg + 0.9503041 * lb;

            let sum = cap_x + cap_y + z;
            if sum < 0.001 { continue; } // skip near-black

            // XYZ → xy chromaticity
            let cx = cap_x / sum;
            let cy = cap_y / sum;

            let (px, py) = xy_to_px(cx, cy);
            if px < size && py < size {
                // Plot with actual pixel color, brightness-weighted
                let brightness = cap_y.sqrt().min(1.0);
                let boost = 0.15 + brightness * 0.6;
                buf.add(px, py, sr * boost, sg * boost, sb * boost);
            }
        }
    }

    buf.to_image([4.0, 4.0, 7.0], 2.0)
}

/// Convert hue (0..1) to RGB for the rainbow ring
pub fn hue_to_rgb(h: f32) -> (f32, f32, f32) {
    let h = h * 6.0;
    let c = 1.0_f32;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    }
}

#[allow(dead_code)]
fn draw_ire_lines(pixels: &mut [egui::Color32], w: usize, h: usize) {
    // Very subtle dashed lines — should not compete with the trace data
    let line_color = egui::Color32::from_rgb(0x24, 0x22, 0x14);
    for &ire in &[0.0_f32, 0.25, 0.50, 0.75, 1.0] {
        let y = h - 1 - ((ire * (h - 1) as f32) as usize).min(h - 1);
        for x in (0..w).step_by(8) {
            // 2-on 6-off — very sparse dashes
            for dx in 0..2 {
                let px = x + dx;
                if px < w {
                    let idx = y * w + px;
                    if idx < pixels.len() {
                        // Blend with existing — don't overwrite trace data
                        let existing = pixels[idx];
                        let r = (existing.r() as u16 + line_color.r() as u16).min(255) as u8;
                        let g = (existing.g() as u16 + line_color.g() as u16).min(255) as u8;
                        let b = (existing.b() as u16 + line_color.b() as u16).min(255) as u8;
                        pixels[idx] = egui::Color32::from_rgb(r, g, b);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ScopeScale;

    #[test]
    fn test_settings_gen_increments_on_bloom_toggle() {
        let mut state = ScopeState::default();
        let gen_before = state.settings_generation;
        state.bloom = !state.bloom;
        state.increment_settings_generation();
        assert!(state.settings_generation > gen_before);
    }

    #[test]
    fn test_settings_gen_increments_on_low_pass_toggle() {
        let mut state = ScopeState::default();
        let gen_before = state.settings_generation;
        state.low_pass = !state.low_pass;
        state.increment_settings_generation();
        assert!(state.settings_generation > gen_before);
    }

    #[test]
    fn test_val_to_y_10bit_range() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 300.0));
        // 1023 should map to PAD_TOP
        let y_top = val_to_y(1023.0, &rect, ScopeScale::TenBit);
        assert!((y_top - PAD_TOP).abs() < 1.0, "1023 at y={y_top}, expected ~{PAD_TOP}");
        // 0 should map to height - PAD_BOTTOM
        let y_bot = val_to_y(0.0, &rect, ScopeScale::TenBit);
        assert!((y_bot - (300.0 - PAD_BOTTOM)).abs() < 1.0, "0 at y={y_bot}, expected ~{}", 300.0 - PAD_BOTTOM);
    }

    #[test]
    fn test_val_to_y_ire_range() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 300.0));
        let y_top = val_to_y(100.0, &rect, ScopeScale::Ire);
        assert!((y_top - PAD_TOP).abs() < 1.0, "100 IRE at y={y_top}, expected ~{PAD_TOP}");
        let y_bot = val_to_y(0.0, &rect, ScopeScale::Ire);
        assert!((y_bot - (300.0 - PAD_BOTTOM)).abs() < 1.0, "0 IRE at y={y_bot}, expected ~{}", 300.0 - PAD_BOTTOM);
    }

    #[test]
    fn test_low_pass_filter_changes_pixels() {
        // Create a 10x1 image with a spike at position 5
        let mut pixels = vec![egui::Color32::BLACK; 10];
        pixels[5] = egui::Color32::from_rgb(255, 0, 0);
        let mut image = egui::ColorImage { size: [10, 1], pixels };
        apply_low_pass_filter(&mut image);
        // After filtering, neighbors of the spike should be non-zero
        assert!(image.pixels[4].r() > 0, "Left neighbor should pick up red from spike");
        assert!(image.pixels[6].r() > 0, "Right neighbor should pick up red from spike");
        // Center should be reduced from 255
        assert!(image.pixels[5].r() < 255, "Center spike should be reduced by filter");
    }

    // ── Phase 2 tests ──────────────────────────────────────────────────

    #[test]
    fn test_pad_top_prevents_label_clipping() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 400.0));
        let y = val_to_y(1023.0, &rect, ScopeScale::TenBit);
        assert!(y >= PAD_TOP - 1.0, "1023 label at y={} is too close to top edge", y);
    }

    #[test]
    fn test_major_ticks_10bit() {
        let ticks = major_ticks(ScopeScale::TenBit);
        assert_eq!(ticks, vec![0, 128, 256, 384, 512, 640, 768, 896, 1023]);
    }

    #[test]
    fn test_major_ticks_ire() {
        let ticks = major_ticks(ScopeScale::Ire);
        assert_eq!(ticks, vec![0, 10, 20, 30, 40, 50, 60, 70, 80, 90, 100]);
    }

    #[test]
    fn test_major_ticks_percent() {
        let ticks = major_ticks(ScopeScale::Percent);
        assert_eq!(ticks, vec![0, 10, 20, 30, 40, 50, 60, 70, 80, 90, 100]);
    }

    #[test]
    fn test_axis_w_is_40() {
        assert_eq!(AXIS_W, 40.0);
    }

    // ── Phase 3 tests ──────────────────────────────────────────────────

    #[test]
    fn test_scope_layout_pane_count() {
        use crate::state::ScopeLayout;
        assert_eq!(ScopeLayout::OnePane.pane_count(), 1);
        assert_eq!(ScopeLayout::TwoPane.pane_count(), 2);
        assert_eq!(ScopeLayout::FourPane.pane_count(), 4);
        assert_eq!(ScopeLayout::NinePane.pane_count(), 9);
    }

    #[test]
    fn test_pane_config_default() {
        use crate::state::{PaneConfig, ScopeLayout, ScopeType};
        let config = PaneConfig::default_for_layout(ScopeLayout::FourPane);
        assert_eq!(config.len(), 4);
        assert_eq!(config[0].scope_type, ScopeType::Parade);
        assert_eq!(config[1].scope_type, ScopeType::Waveform);
        assert_eq!(config[2].scope_type, ScopeType::Vectorscope);
        assert_eq!(config[3].scope_type, ScopeType::Histogram);
    }

    #[test]
    fn test_waveform_channel_luma_formula() {
        let r = 1.0_f32;
        let g = 0.0_f32;
        let b = 0.0_f32;
        let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        assert!((luma - 0.2126).abs() < 0.0001);
    }

    #[test]
    fn test_scope_window_state_persistence() {
        use crate::state::{ScopeLayout, ScopeWindowState};
        let state = ScopeWindowState {
            open: true,
            layout: ScopeLayout::FourPane,
            ..Default::default()
        };
        let json = serde_json::to_string(&state).unwrap();
        let restored: ScopeWindowState = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.layout, ScopeLayout::FourPane);
        assert!(restored.open);
    }

    // ── Phase 4 tests ──────────────────────────────────────────────────

    #[test]
    fn test_waveform_brightness_affects_gain() {
        use crate::state::{PaneScopeSettings, compute_tone_map_gain};
        let settings = PaneScopeSettings { waveform_brightness: 0.5, ..Default::default() };
        let gain = compute_tone_map_gain(&settings);
        let settings_bright = PaneScopeSettings { waveform_brightness: 1.0, ..Default::default() };
        let gain_bright = compute_tone_map_gain(&settings_bright);
        assert!(gain_bright > gain, "Brighter setting should produce higher gain");
    }

    #[test]
    fn test_graticule_brightness_affects_alpha() {
        use crate::state::graticule_alpha;
        let alpha_dim = graticule_alpha(0.25);
        let alpha_bright = graticule_alpha(1.0);
        assert!(alpha_bright > alpha_dim);
        assert!(alpha_dim > 0.0, "Even dim graticule should be visible");
    }

    #[test]
    fn test_reference_levels_render_when_enabled() {
        use crate::state::{PaneScopeSettings, compute_reference_lines};
        let settings = PaneScopeSettings {
            show_reference_levels: true,
            reference_low: 64.0,
            reference_high: 940.0,
            ..Default::default()
        };
        let lines = compute_reference_lines(&settings, 300.0);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].y < lines[1].y, "High value should be above low value on screen");
    }

    #[test]
    fn test_skin_tone_angle_constant() {
        use crate::state::SKIN_TONE_ANGLE_DEGREES;
        assert_eq!(SKIN_TONE_ANGLE_DEGREES, 123.0);
    }

    #[test]
    fn test_vectorscope_range_rings() {
        use crate::state::{PaneScopeSettings, compute_saturation_rings};
        let settings = PaneScopeSettings { low_range: 0.30, high_range: 0.70, ..Default::default() };
        let rings = compute_saturation_rings(&settings, 200.0);
        assert_eq!(rings.len(), 2);
        assert!(rings[0].radius < rings[1].radius, "Low range ring should have smaller radius");
    }
}
