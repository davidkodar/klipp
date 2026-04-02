use crate::state::ScopeMode;
use klipp_engine::SharedFrameBuffer;

/// Either a ready GPU-rendered image or pre-computed bins for CPU rendering.
enum ScopeImage {
    Ready(egui::ColorImage),
    Bins(ScopeBins, usize, usize), // bins + cpu render width/height
}

/// Pre-computed scope data — avoids cloning the entire 8MB frame.
enum ScopeBins {
    Waveform(klipp_color::scopes::WaveformData),
    Parade(klipp_color::scopes::ParadeData),
    Histogram(klipp_color::scopes::HistogramData),
    Raw(Vec<u8>, u32, u32, u32), // vectorscope needs raw RGBA
}

/// Persistent state for scope rendering (texture cache + throttling).
pub struct ScopeState {
    /// One cached texture per scope mode — switching is instant.
    textures: [Option<egui::TextureHandle>; 4], // Waveform, Parade, Vectorscope, Histogram
    /// Frame generation counter used for each cached texture (from FrameBuffer.generation).
    frame_generations: [u64; 4],
    /// Render sizes used for each cached texture.
    render_sizes: [(usize, usize); 4],
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
        }
    }
}

/// Render scopes into the given UI area.
/// Each scope mode has its own cached texture — switching modes is instant.
pub fn show_scopes(
    ui: &mut egui::Ui,
    state: &mut ScopeState,
    mode: ScopeMode,
    frame_buffer: &SharedFrameBuffer,
    quality: klipp_core::ScopeQuality,
    skin_tone_angle: f32,
) {
    let available = ui.available_size();
    let ppp = ui.ctx().pixels_per_point();
    let render_w = ((available.x * ppp) as usize).clamp(100, 1920);
    let render_h = ((available.y * ppp) as usize).clamp(60, 1080);

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
                || state.render_sizes[mi] != (render_w, render_h);
            if stale {
                // Use GPU-rendered scope texture from engine if available and mode matches
                if !fb.scope_texture.is_empty() && fb.scope_texture_w > 0
                    && fb.scope_texture_mode == mi as u8 {
                    let pixels: Vec<egui::Color32> = fb.scope_texture.chunks_exact(4)
                        .map(|c| egui::Color32::from_rgba_premultiplied(c[0], c[1], c[2], c[3]))
                        .collect();
                    let image = egui::ColorImage {
                        size: [fb.scope_texture_w as usize, fb.scope_texture_h as usize],
                        pixels,
                    };
                    Some((ScopeImage::Ready(image), gen))
                } else {
                    // CPU fallback at CAPPED resolution (keep UI responsive)
                    let cpu_w = render_w.min(320);
                    let cpu_h = render_h.min(200);
                    let (src, w, h) = if !fb.scope_preview.is_empty() && fb.scope_preview_w > 0 {
                        (&fb.scope_preview[..], fb.scope_preview_w, fb.scope_preview_h)
                    } else {
                        (&fb.data[..], fb.width, fb.height)
                    };
                    let step: u32 = if w > 1280 { 4 } else if w > 640 { 3 } else { 2 };

                    let bins = match mode {
                        ScopeMode::Waveform => ScopeBins::Waveform(klipp_color::scopes::compute_waveform_sampled(src, w, h, cpu_w as u32, step)),
                        ScopeMode::Parade => {
                            let gap = 3;
                            let section_w = (cpu_w - gap * 2) / 3;
                            ScopeBins::Parade(klipp_color::scopes::compute_parade_sampled(src, w, h, section_w as u32, step))
                        }
                        ScopeMode::Histogram => ScopeBins::Histogram(klipp_color::scopes::compute_histogram_sampled(src, w, h, step)),
                        ScopeMode::Vectorscope => ScopeBins::Raw(src.to_vec(), w, h, step),
                    };
                    Some((ScopeImage::Bins(bins, cpu_w, cpu_h), gen))
                }
            } else {
                None
            }
        }
    };

    if let Some((scope_img, gen)) = scope_result {
        state.frame_generations[mi] = gen;
        state.render_sizes[mi] = (render_w, render_h);
        for idx in 0..4 {
            if idx != mi { state.frame_generations[idx] = u64::MAX; }
        }

        let image = match scope_img {
            ScopeImage::Ready(img) => img,
            ScopeImage::Bins(bins, cw, ch) => match bins {
                ScopeBins::Waveform(wf) => render_waveform_from_bins(&wf.bins, cw, ch),
                ScopeBins::Parade(p) => render_parade_from_bins(&p.red, &p.green, &p.blue, cw, ch),
                ScopeBins::Histogram(h) => render_histogram_from_bins(&h.r, &h.g, &h.b, &h.luma, cw, ch),
                ScopeBins::Raw(rgba, w, h, step) => render_vectorscope(&rgba, w, h, step, cw, ch, skin_tone_angle),
            },
        };

        if let Some(ref mut tex) = state.textures[mi] {
            tex.set(image, egui::TextureOptions::LINEAR);
        } else {
            state.textures[mi] = Some(ui.ctx().load_texture(
                format!("scope_{mi}"), image, egui::TextureOptions::LINEAR,
            ));
        }
    }

    // Display the cached texture for this mode
    if let Some(tex) = &state.textures[mi] {
        ui.ctx().request_repaint();

        // Vectorscope must be displayed as a perfect square (centered)
        // Other scopes fill the available area
        let is_vector = mode == ScopeMode::Vectorscope;
        let display_size = if is_vector {
            let side = available.x.min(available.y);
            egui::vec2(side, side)
        } else {
            available
        };

        // Center the scope in the available area
        let (full_rect, _) = ui.allocate_exact_size(available, egui::Sense::hover());
        let img_rect = if is_vector {
            // Center square in the allocated rect
            let offset_x = (full_rect.width() - display_size.x) / 2.0;
            let offset_y = (full_rect.height() - display_size.y) / 2.0;
            egui::Rect::from_min_size(
                egui::pos2(full_rect.min.x + offset_x, full_rect.min.y + offset_y),
                display_size,
            )
        } else {
            full_rect
        };

        // Dark background behind the scope
        ui.painter().rect_filled(full_rect, 0.0, egui::Color32::from_rgb(0x04, 0x04, 0x07));
        ui.painter().image(
            tex.id(), img_rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );

        // Overlay labels
        let label_font = egui::FontId::monospace(8.0);
        let label_color = egui::Color32::from_rgb(0x90, 0x78, 0x30);
        let shadow_color = egui::Color32::from_rgb(0x08, 0x08, 0x0c);

        match mode {
            ScopeMode::Waveform | ScopeMode::Parade => {
                for &(ire, label) in &[(100, "100"), (75, "75"), (50, "50"), (25, "25"), (0, "0")] {
                    let y = img_rect.max.y - (ire as f32 / 100.0) * img_rect.height();
                    let pos = egui::pos2(img_rect.min.x + 2.0, y);
                    ui.painter().text(egui::pos2(pos.x + 0.5, pos.y + 0.5),
                        egui::Align2::LEFT_CENTER, label, label_font.clone(), shadow_color);
                    ui.painter().text(pos,
                        egui::Align2::LEFT_CENTER, label, label_font.clone(), label_color);
                }
            }
            ScopeMode::Histogram => {
                let bottom_y = img_rect.max.y - 2.0;
                for &(val, x_frac) in &[("0", 0.0_f32), ("128", 0.5), ("255", 1.0)] {
                    let x = img_rect.min.x + x_frac * img_rect.width();
                    let align = if x_frac < 0.1 { egui::Align2::LEFT_BOTTOM }
                        else if x_frac > 0.9 { egui::Align2::RIGHT_BOTTOM }
                        else { egui::Align2::CENTER_BOTTOM };
                    ui.painter().text(egui::pos2(x + 0.5, bottom_y + 0.5),
                        align, val, label_font.clone(), shadow_color);
                    ui.painter().text(egui::pos2(x, bottom_y),
                        align, val, label_font.clone(), label_color);
                }
            }
            ScopeMode::Vectorscope => {
                // Labels at color target positions
                let cx = img_rect.center().x;
                let cy = img_rect.center().y;
                let r = img_rect.width().min(img_rect.height()) / 2.0 - 4.0;
                let labels = [("R", 0.17_f32, 0.29_f32), ("Mg", 0.28, -0.05), ("B", 0.12, -0.34),
                              ("Cy", -0.17, -0.29), ("G", -0.28, 0.05), ("Yl", -0.12, 0.34)];
                for (name, cb, cr) in &labels {
                    let tx = cx + cb * r * 2.0;
                    let ty = cy - cr * r * 2.0 - 6.0;
                    ui.painter().text(egui::pos2(tx, ty), egui::Align2::CENTER_BOTTOM,
                        name, label_font.clone(), label_color);
                }
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

#[inline(never)]
fn render_waveform_from_bins(bins: &[[u32; 256]], rw: usize, rh: usize) -> egui::ColorImage {
    let mut buf = AccumBuffer::new(rw, rh);

    // Fast max-based normalization (avoids expensive sort for p95)
    let max_count = bins.iter()
        .flat_map(|c| c.iter())
        .copied()
        .max()
        .unwrap_or(1)
        .max(1) as f32;
    let p95 = max_count * 0.7; // approximate p95 as 70% of max

    let log_max = (p95 + 1.0).ln();

    for (col_idx, col) in bins.iter().enumerate() {
        if col_idx >= rw { continue; }
        for (bin_idx, &count) in col.iter().enumerate() {
            if count == 0 { continue; }
            let y = rh - 1 - (bin_idx * rh / 256);
            if y >= rh { continue; }

            let log_i = (count as f32 + 1.0).ln() / log_max;
            let intensity = log_i.clamp(0.0, 1.0);

            let is_safe = (16..=235).contains(&bin_idx);
            let white = (intensity - 0.8).max(0.0) * 5.0;

            let (r, g, b) = if is_safe {
                (0.08 * intensity + white * 0.4,
                 1.0 * intensity + white * 0.0,
                 0.08 * intensity + white * 0.4)
            } else {
                (1.0 * intensity + white * 0.0,
                 0.06 * intensity + white * 0.4,
                 0.03 * intensity + white * 0.3)
            };

            buf.add(col_idx, y, r, g, b);

            if intensity > 0.7 {
                let gs = (intensity - 0.7) * 0.2;
                let (cr, cg, cb) = if is_safe { (0.06, 0.8, 0.06) } else { (0.8, 0.04, 0.02) };
                if y > 0 { buf.add(col_idx, y - 1, cr * gs, cg * gs, cb * gs); }
                if y + 1 < rh { buf.add(col_idx, y + 1, cr * gs, cg * gs, cb * gs); }
            }
        }
    }

    let mut img = buf.to_image([1.0, 1.0, 2.0], 1.6);
    draw_ire_lines(&mut img.pixels, rw, rh);
    img
}

#[inline(never)]
fn render_parade_from_bins(parade_red: &[[u32; 256]], parade_green: &[[u32; 256]], parade_blue: &[[u32; 256]], rw: usize, rh: usize) -> egui::ColorImage {
    let gap = 3;
    let section_w = (rw - gap * 2) / 3;
    let mut buf = AccumBuffer::new(rw, rh);

    // Use logarithmic normalization for DaVinci-style thin traces
    let max_count = [parade_red, parade_green, parade_blue].iter()
        .flat_map(|d| d.iter().flat_map(|c| c.iter()))
        .copied()
        .max()
        .unwrap_or(1)
        .max(1) as f32;

    // DaVinci-style parade: deep saturated colors, only hottest spots go white.
    // Key: keep the primary channel dominant, secondary channels very low.
    let channels: [(&[[u32; 256]], usize, [f32; 3]); 3] = [
        //                               x_offset     color (R, G, B)
        (parade_red,   0,                    [1.0, 0.08, 0.04]),
        (parade_green, section_w + gap,       [0.04, 1.0, 0.08]),
        (parade_blue,  (section_w + gap) * 2, [0.08, 0.12, 1.0]),
    ];

    let log_max = (max_count + 1.0).ln();

    for (data, x_off, color) in &channels {
        for (ci, col) in data.iter().enumerate() {
            let px_x = x_off + ci;
            if px_x >= rw { continue; }
            for (bi, &count) in col.iter().enumerate() {
                if count == 0 { continue; }
                let y = rh - 1 - (bi * rh / 256);
                if y >= rh { continue; }

                // Log intensity with gentler curve — keeps traces thin
                let log_i = (count as f32 + 1.0).ln() / log_max;
                let intensity = log_i.clamp(0.0, 1.0);

                // Only the very densest areas shift toward white (>80% log intensity)
                let white = (intensity - 0.8).max(0.0) * 5.0; // 0.0 at 80%, 1.0 at 100%
                let r = color[0] * intensity + white * (1.0 - color[0]) * 0.5;
                let g = color[1] * intensity + white * (1.0 - color[1]) * 0.5;
                let b = color[2] * intensity + white * (1.0 - color[2]) * 0.5;

                buf.add(px_x, y, r, g, b);

                // Subtle vertical glow only at very high density
                if intensity > 0.7 {
                    let gs = (intensity - 0.7) * 0.2;
                    if y > 0 { buf.add(px_x, y - 1, color[0] * gs, color[1] * gs, color[2] * gs); }
                    if y + 1 < rh { buf.add(px_x, y + 1, color[0] * gs, color[1] * gs, color[2] * gs); }
                }
            }
        }
    }

    let mut img = buf.to_image([1.0, 1.0, 2.0], 1.6);

    // Channel separator lines
    for &gap_x in &[section_w, (section_w + gap) * 2 - gap] {
        if gap_x < rw {
            for y in 0..rh {
                img.pixels[y * rw + gap_x] = egui::Color32::from_rgb(0x10, 0x10, 0x14);
            }
        }
    }

    draw_ire_lines(&mut img.pixels, rw, rh);
    img
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
