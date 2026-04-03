use crate::state::{ScopeMode, ScopeScale};
use klipp_engine::SharedFrameBuffer;

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
    /// Incremented when any scope setting changes (triggers re-render).
    pub settings_generation: u64,
    last_settings_generation: u64,
}

fn mode_index(mode: ScopeMode) -> usize {
    match mode {
        ScopeMode::Waveform => 0,
        ScopeMode::Parade => 1,
        ScopeMode::Vectorscope => 2,
        ScopeMode::Histogram => 3,
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
            settings_generation: 0,
            last_settings_generation: u64::MAX,
        }
    }
}

// ── Scale system constants ─────────────────────────────────────────────
const AXIS_W: f32 = 44.0;
const PAD_TOP: f32 = 14.0;
const PAD_BOTTOM: f32 = 16.0;
const PAD_RIGHT: f32 = 8.0;

fn val_to_y(val: f32, rect: &egui::Rect, scale: ScopeScale) -> f32 {
    let max = scale.max_val();
    let draw_h = rect.height() - PAD_TOP - PAD_BOTTOM;
    rect.min.y + PAD_TOP + (1.0 - val / max) * draw_h
}

fn draw_scale(ui: &egui::Ui, rect: egui::Rect, scale: ScopeScale, is_parade: bool, section_count: usize) {
    let painter = ui.painter_at(rect);
    let amber = |a: u8| egui::Color32::from_rgba_unmultiplied(0xf5, 0xb4, 0x3c, a);
    let font = egui::FontId::monospace(9.0);

    let (major_vals, minor_step): (Vec<f32>, f32) = match scale {
        ScopeScale::TenBit => ((0..=8).map(|i| i as f32 * 128.0).chain(std::iter::once(1023.0)).collect(), 32.0),
        _ => ((0..=10).map(|i| i as f32 * 10.0).collect(), 5.0),
    };

    // Minor grid lines
    let max = scale.max_val();
    let mut v = 0.0;
    while v <= max {
        let y = val_to_y(v, &rect, scale);
        if !major_vals.iter().any(|&mv| (mv - v).abs() < 0.1) {
            painter.line_segment(
                [egui::pos2(rect.min.x + AXIS_W, y), egui::pos2(rect.max.x - PAD_RIGHT, y)],
                egui::Stroke::new(0.5, amber(10)),
            );
            // 3px tick
            painter.line_segment(
                [egui::pos2(rect.min.x + AXIS_W - 3.0, y), egui::pos2(rect.min.x + AXIS_W, y)],
                egui::Stroke::new(0.5, amber(20)),
            );
        }
        v += minor_step;
    }

    // Major grid lines + labels
    for &val in &major_vals {
        let y = val_to_y(val, &rect, scale);
        painter.line_segment(
            [egui::pos2(rect.min.x + AXIS_W, y), egui::pos2(rect.max.x - PAD_RIGHT, y)],
            egui::Stroke::new(0.5, amber(50)),
        );
        // 6px tick
        painter.line_segment(
            [egui::pos2(rect.min.x + AXIS_W - 6.0, y), egui::pos2(rect.min.x + AXIS_W, y)],
            egui::Stroke::new(0.5, amber(80)),
        );
        // Label
        let label = if scale == ScopeScale::TenBit { format!("{}", val as u32) } else { format!("{}", val as u32) };
        let align = if val < 0.1 { egui::Align2::RIGHT_TOP } else { egui::Align2::RIGHT_BOTTOM };
        painter.text(egui::pos2(rect.min.x + AXIS_W - 8.0, y), align, &label, font.clone(), amber(190));
    }

    // Axis separator
    painter.line_segment(
        [egui::pos2(rect.min.x + AXIS_W, rect.min.y), egui::pos2(rect.min.x + AXIS_W, rect.max.y - PAD_BOTTOM)],
        egui::Stroke::new(0.5, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 20)),
    );

    // Bottom tick row
    let bottom_y = rect.max.y - PAD_BOTTOM;
    painter.line_segment(
        [egui::pos2(rect.min.x + AXIS_W, bottom_y), egui::pos2(rect.max.x - PAD_RIGHT, bottom_y)],
        egui::Stroke::new(0.5, amber(30)),
    );
    let trace_w = rect.width() - AXIS_W - PAD_RIGHT;
    for i in 0..=20 {
        let frac = i as f32 / 20.0;
        let x = rect.min.x + AXIS_W + frac * trace_w;
        let is_major = i % 5 == 0;
        let tick_h = if is_major { 6.0 } else { 3.0 };
        let color = if is_major { amber(100) } else { amber(38) };
        painter.line_segment(
            [egui::pos2(x, bottom_y), egui::pos2(x, bottom_y + tick_h)],
            egui::Stroke::new(0.5, color),
        );
    }

    // Parade section separators
    if is_parade && section_count == 3 {
        let third = trace_w / 3.0;
        for i in 1..3 {
            let x = rect.min.x + AXIS_W + third * i as f32;
            painter.line_segment(
                [egui::pos2(x, rect.min.y + PAD_TOP), egui::pos2(x, bottom_y)],
                egui::Stroke::new(0.5, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 12)),
            );
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
) {
    let render_h: usize = 512;

    let mi = mode_index(mode);

    // ── Check for GPU-rendered scope texture from engine ──────────────────
    let scope_result = {
        let fb = frame_buffer.lock().unwrap();
        if fb.width == 0 || fb.height == 0 || fb.data.is_empty() {
            None
        } else {
            let gen = fb.generation;
            let stale = state.textures[mi].is_none()
                || state.frame_generations[mi] != gen
                || state.render_sizes[mi] != (fb.width as usize, render_h)
                || state.last_settings_generation != state.settings_generation;
            if stale {
                // Use GPU-rendered scope texture from engine if available and mode matches
                let has_gpu_tex = !fb.scope_texture.is_empty() && fb.scope_texture_w > 0
                    && fb.scope_texture_mode == mi as u8;
                tracing::trace!("Scope update: gen={} mode={} gpu_tex={} tex_mode={} tex_size={}x{}",
                    gen, mi, has_gpu_tex, fb.scope_texture_mode, fb.scope_texture_w, fb.scope_texture_h);
                if has_gpu_tex {
                    let pixels: Vec<egui::Color32> = fb.scope_texture.chunks_exact(4)
                        .map(|c| egui::Color32::from_rgba_premultiplied(c[0], c[1], c[2], c[3]))
                        .collect();
                    let image = egui::ColorImage {
                        size: [fb.scope_texture_w as usize, fb.scope_texture_h as usize],
                        pixels,
                    };
                    Some((ScopeImage::Ready(image), gen))
                } else {
                    let (src, w, h) = (&fb.data[..], fb.width, fb.height);
                    let step: u32 = if w > 1280 { 4 } else if w > 640 { 3 } else { 2 };
                    // Internal render width = source video width (downscale to display = thin traces)
                    let render_w = w as usize;

                    // Direct accumulation rendering (DaVinci technique)
                    let image = match mode {
                        ScopeMode::Waveform => render_waveform_direct(src, w, h, render_w, render_h),
                        ScopeMode::Parade => render_parade_direct(src, w, h, render_w, render_h),
                        ScopeMode::Histogram => {
                            let hist = klipp_color::scopes::compute_histogram_sampled(src, w, h, step);
                            render_histogram_from_bins(&hist.r, &hist.g, &hist.b, &hist.luma, render_w, render_h)
                        }
                        ScopeMode::Vectorscope => render_vectorscope(src, w, h, step, render_w, render_h, skin_tone_angle),
                    };
                    Some((ScopeImage::Ready(image), gen))
                }
            } else {
                None
            }
        }
    };

    if let Some((scope_img, gen)) = scope_result {
        state.frame_generations[mi] = gen;
        state.last_settings_generation = state.settings_generation;
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
fn render_waveform_direct(rgba: &[u8], video_w: u32, video_h: u32, scope_w: usize, scope_h: usize) -> egui::ColorImage {
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

/// Render parade using DaVinci's additive accumulation technique.
/// Three separate accumulation buffers for R, G, B displayed side by side.
fn render_parade_direct(rgba: &[u8], video_w: u32, video_h: u32, scope_w: usize, scope_h: usize) -> egui::ColorImage {
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
fn render_histogram_from_bins(r_bins: &[u32; 256], g_bins: &[u32; 256], b_bins: &[u32; 256], luma_bins: &[u32; 256], rw: usize, rh: usize) -> egui::ColorImage {
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
fn render_vectorscope(rgba: &[u8], width: u32, height: u32, step: u32, rw: usize, rh: usize, skin_tone_angle: f32) -> egui::ColorImage {
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
