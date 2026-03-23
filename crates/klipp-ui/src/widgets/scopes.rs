use crate::state::ScopeMode;
use klipp_engine::SharedFrameBuffer;

/// Persistent state for scope rendering (texture cache + throttling).
pub struct ScopeState {
    texture: Option<egui::TextureHandle>,
    last_update: std::time::Instant,
    last_mode: Option<ScopeMode>,
    /// Cached vectorscope points for egui painter rendering.
    cached_vs_points: Vec<(f32, f32)>,
}

impl Default for ScopeState {
    fn default() -> Self {
        Self {
            texture: None,
            last_update: std::time::Instant::now(),
            last_mode: None,
            cached_vs_points: Vec::new(),
        }
    }
}

const SCOPE_W: usize = 384;
const SCOPE_H: usize = 300;
const UPDATE_INTERVAL_MS: u128 = 100; // ~10fps

/// Render scopes into the given UI area.
pub fn show_scopes(
    ui: &mut egui::Ui,
    state: &mut ScopeState,
    mode: ScopeMode,
    frame_buffer: &SharedFrameBuffer,
) {
    let now = std::time::Instant::now();
    let mode_changed = state.last_mode != Some(mode);
    let should_update = mode_changed || now.duration_since(state.last_update).as_millis() >= UPDATE_INTERVAL_MS;

    if should_update {
        state.last_update = now;
        state.last_mode = Some(mode);

        let fb = frame_buffer.lock().unwrap();
        if fb.width > 0 && fb.height > 0 && !fb.data.is_empty() {
            if mode == ScopeMode::Vectorscope {
                // Cache points for painter-based rendering
                let vs = klipp_color::scopes::compute_vectorscope(&fb.data, fb.width, fb.height);
                state.cached_vs_points = vs.points;
                state.texture = None; // not used for vectorscope
            } else {
                let image = match mode {
                    ScopeMode::Waveform => render_waveform(&fb.data, fb.width, fb.height),
                    ScopeMode::Parade => render_parade(&fb.data, fb.width, fb.height),
                    ScopeMode::Histogram => render_histogram(&fb.data, fb.width, fb.height),
                    ScopeMode::Vectorscope => unreachable!(),
                };
                state.texture = Some(ui.ctx().load_texture(
                    "scope",
                    image,
                    egui::TextureOptions::LINEAR,
                ));
            }
        }
    }

    if mode == ScopeMode::Vectorscope {
        draw_vectorscope_painter(ui, &state.cached_vs_points);
    } else if let Some(tex) = &state.texture {
        let available = ui.available_size();
        let aspect = SCOPE_H as f32 / SCOPE_W as f32;
        let scope_w = available.x.min(SCOPE_W as f32 * 2.0);
        let scope_h = (scope_w * aspect).min(available.y - 4.0);
        let scope_w = if scope_h < scope_w * aspect {
            scope_h / aspect
        } else {
            scope_w
        };
        let size = egui::vec2(scope_w, scope_h.max(20.0));

        // Allocate rect, draw image, then overlay labels
        let (img_rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
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
                // IRE labels on left edge: 0, 25, 50, 75, 100
                for &(ire, label) in &[(100, "100"), (75, "75"), (50, "50"), (25, "25"), (0, "0")] {
                    let y = img_rect.max.y - (ire as f32 / 100.0) * img_rect.height();
                    let pos = egui::pos2(img_rect.min.x + 2.0, y);
                    // Shadow
                    ui.painter().text(egui::pos2(pos.x + 0.5, pos.y + 0.5),
                        egui::Align2::LEFT_CENTER, label, label_font.clone(), shadow_color);
                    ui.painter().text(pos,
                        egui::Align2::LEFT_CENTER, label, label_font.clone(), label_color);
                }
            }
            ScopeMode::Histogram => {
                // Bottom axis: 0 and 255 at edges, 128 at midpoint
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
            _ => {}
        }
    } else {
        let text_dim = egui::Color32::from_rgb(0x50, 0x50, 0x60);
        ui.label(egui::RichText::new("No frame data").color(text_dim).size(10.0));
    }
}

fn render_waveform(rgba: &[u8], width: u32, height: u32) -> egui::ColorImage {
    let wf = klipp_color::scopes::compute_waveform(rgba, width, height, SCOPE_W as u32);
    let mut pixels = vec![egui::Color32::from_rgb(0x0a, 0x0a, 0x10); SCOPE_W * SCOPE_H];

    // Find max bin count for normalization
    let max_count = wf.bins.iter()
        .flat_map(|col| col.iter())
        .copied()
        .max()
        .unwrap_or(1)
        .max(1);

    for (col_idx, col) in wf.bins.iter().enumerate() {
        for (bin_idx, &count) in col.iter().enumerate() {
            if count == 0 { continue; }
            // bin_idx 0 = bottom, 255 = top → invert y
            let y = SCOPE_H - 1 - (bin_idx * SCOPE_H / 256);
            if y >= SCOPE_H || col_idx >= SCOPE_W { continue; }

            let intensity = (count as f32 / max_count as f32).sqrt(); // sqrt for better visibility
            let brightness = (intensity * 255.0).min(255.0) as u8;

            // Color: green in safe range (16-235), amber near clip, red at illegal
            let color = if !(8..=247).contains(&bin_idx) {
                egui::Color32::from_rgb(brightness, brightness / 4, brightness / 6) // red: illegal
            } else if !(16..=235).contains(&bin_idx) {
                egui::Color32::from_rgb(brightness, brightness * 2 / 3, brightness / 4) // amber: warning
            } else {
                egui::Color32::from_rgb(brightness / 3, brightness, brightness / 3) // green: safe
            };

            pixels[y * SCOPE_W + col_idx] = color;
        }
    }

    // Draw IRE scale lines at 0%, 25%, 50%, 75%, 100%
    draw_ire_lines(&mut pixels, SCOPE_W, SCOPE_H);

    egui::ColorImage {
        size: [SCOPE_W, SCOPE_H],
        pixels,
    }
}

/// Draw IRE scale lines (0%, 25%, 50%, 75%, 100%) on a scope image.
/// Uses dashed lines for subtlety and small tick marks on the left edge.
fn draw_ire_lines(pixels: &mut [egui::Color32], w: usize, h: usize) {
    let line_color = egui::Color32::from_rgb(0x50, 0x44, 0x20); // subtle amber
    let tick_color = egui::Color32::from_rgb(0x90, 0x70, 0x28); // brighter for ticks
    let ire_levels: [(f32, bool); 5] = [
        (0.0, true), (0.25, false), (0.50, true), (0.75, false), (1.0, true),
    ];

    for &(ire, is_major) in &ire_levels {
        let y = h - 1 - ((ire * (h - 1) as f32) as usize).min(h - 1);
        let dash_color = if is_major { line_color } else {
            egui::Color32::from_rgb(0x30, 0x2c, 0x18)
        };

        // Dashed horizontal line (4px on, 4px off)
        for x in 0..w {
            if (x / 4) % 2 == 0 {
                let idx = y * w + x;
                if idx < pixels.len() {
                    let existing = pixels[idx];
                    let r = (existing.r() as u16 + dash_color.r() as u16).min(255) as u8;
                    let g = (existing.g() as u16 + dash_color.g() as u16).min(255) as u8;
                    let b = (existing.b() as u16 + dash_color.b() as u16).min(255) as u8;
                    pixels[idx] = egui::Color32::from_rgb(r, g, b);
                }
            }
        }

        // Left edge tick mark (solid, 6px wide)
        let tick_w = if is_major { 8 } else { 4 };
        for x in 0..tick_w.min(w) {
            let idx = y * w + x;
            if idx < pixels.len() {
                pixels[idx] = tick_color;
            }
        }
    }
}

fn render_parade(rgba: &[u8], width: u32, height: u32) -> egui::ColorImage {
    // Each channel gets 1/3 of the display width with 1px gap
    let section_w = (SCOPE_W - 2) / 3; // 2px for gaps
    let parade = klipp_color::scopes::compute_parade(rgba, width, height, section_w as u32);
    let mut pixels = vec![egui::Color32::from_rgb(0x0a, 0x0a, 0x10); SCOPE_W * SCOPE_H];

    // Find max across all channels
    let max_count = parade.red.iter()
        .flat_map(|col| col.iter())
        .chain(parade.green.iter().flat_map(|col| col.iter()))
        .chain(parade.blue.iter().flat_map(|col| col.iter()))
        .copied()
        .max()
        .unwrap_or(1)
        .max(1);

    type ParadeChannel<'a> = (&'a Vec<[u32; 256]>, usize, fn(u8) -> egui::Color32);
    let channels: [ParadeChannel; 3] = [
        (&parade.red,   0,                  |b| egui::Color32::from_rgb(b, b / 4, b / 6)),
        (&parade.green, section_w + 1,      |b| egui::Color32::from_rgb(b / 4, b, b / 4)),
        (&parade.blue,  (section_w + 1) * 2, |b| egui::Color32::from_rgb(b / 4, b / 3, b)),
    ];

    for (data, x_offset, color_fn) in &channels {
        for (col_idx, col) in data.iter().enumerate() {
            let px_x = x_offset + col_idx;
            if px_x >= SCOPE_W { continue; }
            for (bin_idx, &count) in col.iter().enumerate() {
                if count == 0 { continue; }
                let y = SCOPE_H - 1 - (bin_idx * SCOPE_H / 256);
                if y >= SCOPE_H { continue; }
                let intensity = (count as f32 / max_count as f32).sqrt();
                let brightness = (intensity * 255.0).min(255.0) as u8;
                pixels[y * SCOPE_W + px_x] = color_fn(brightness);
            }
        }
    }

    // Draw 1px gap lines between channels
    let gap_color = egui::Color32::from_rgb(0x18, 0x18, 0x20);
    for y in 0..SCOPE_H {
        pixels[y * SCOPE_W + section_w] = gap_color;
        pixels[y * SCOPE_W + (section_w + 1) * 2] = gap_color;
    }

    // IRE scale lines
    draw_ire_lines(&mut pixels, SCOPE_W, SCOPE_H);

    egui::ColorImage {
        size: [SCOPE_W, SCOPE_H],
        pixels,
    }
}

/// Draw vectorscope using egui's anti-aliased painter (no pixel buffer).
fn draw_vectorscope_painter(ui: &mut egui::Ui, points: &[(f32, f32)]) {
    let available = ui.available_size();
    let scope_size = available.x.min(available.y).clamp(100.0, 500.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(scope_size, scope_size), egui::Sense::hover());

    let painter = ui.painter_at(rect);
    let center = rect.center();
    let radius = scope_size / 2.0 - 8.0;

    // Background
    painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(0x0a, 0x0a, 0x10));

    // Guide circles: 25%, 50%, 75%, 100%
    let ring_color = egui::Color32::from_rgb(0x1e, 0x1e, 0x28);
    let ring_stroke = egui::Stroke::new(0.5, ring_color);
    painter.circle_stroke(center, radius * 0.25, ring_stroke);
    painter.circle_stroke(center, radius * 0.50, ring_stroke);
    painter.circle_stroke(center, radius * 0.75, ring_stroke);
    let outer_stroke = egui::Stroke::new(0.8, egui::Color32::from_rgb(0x2a, 0x2a, 0x36));
    painter.circle_stroke(center, radius, outer_stroke);

    // Crosshair
    let cross_color = egui::Color32::from_rgb(0x1e, 0x1e, 0x28);
    let cross_stroke = egui::Stroke::new(0.5, cross_color);
    painter.line_segment(
        [egui::pos2(rect.min.x, center.y), egui::pos2(rect.max.x, center.y)],
        cross_stroke,
    );
    painter.line_segment(
        [egui::pos2(center.x, rect.min.y), egui::pos2(center.x, rect.max.y)],
        cross_stroke,
    );

    // Color target boxes at standard hue positions (R, Mg, B, Cy, G, Yl) — 75% color bars
    let targets: [(f32, f32, egui::Color32); 6] = [
        ( 0.1688,  0.2933, egui::Color32::from_rgb(0x60, 0x30, 0x30)), // Red
        ( 0.2833, -0.0500, egui::Color32::from_rgb(0x55, 0x28, 0x50)), // Magenta
        ( 0.1150, -0.3433, egui::Color32::from_rgb(0x30, 0x30, 0x60)), // Blue
        (-0.1688, -0.2933, egui::Color32::from_rgb(0x28, 0x50, 0x50)), // Cyan
        (-0.2833,  0.0500, egui::Color32::from_rgb(0x28, 0x50, 0x28)), // Green
        (-0.1150,  0.3433, egui::Color32::from_rgb(0x50, 0x50, 0x28)), // Yellow
    ];
    for (cb, cr, color) in &targets {
        let tx = center.x + cb * radius * 2.0;
        let ty = center.y - cr * radius * 2.0;
        let target_rect = egui::Rect::from_center_size(
            egui::pos2(tx, ty),
            egui::vec2(6.0, 6.0),
        );
        painter.rect_stroke(target_rect, 0.0, egui::Stroke::new(1.0, *color));
    }

    // Plot data points — use tiny semi-transparent dots for density visualization
    if !points.is_empty() {
        let dot_color = egui::Color32::from_rgba_unmultiplied(0x60, 0xdd, 0x60, 18);
        let bright_color = egui::Color32::from_rgba_unmultiplied(0xaa, 0xff, 0xaa, 40);

        // Bin into a grid for density-based rendering
        let grid_size = 128usize;
        let mut density = vec![0u32; grid_size * grid_size];
        for &(cb, cr) in points {
            let nx = 0.5 + cb; // 0..1
            let ny = 0.5 - cr; // 0..1
            let gx = (nx * (grid_size - 1) as f32) as usize;
            let gy = (ny * (grid_size - 1) as f32) as usize;
            if gx < grid_size && gy < grid_size {
                density[gy * grid_size + gx] += 1;
            }
        }

        let max_density = density.iter().max().copied().unwrap_or(1).max(1);

        // Draw density as circles
        for gy in 0..grid_size {
            for gx in 0..grid_size {
                let count = density[gy * grid_size + gx];
                if count == 0 { continue; }

                let nx = gx as f32 / (grid_size - 1) as f32;
                let ny = gy as f32 / (grid_size - 1) as f32;
                let px = center.x + (nx - 0.5) * radius * 2.0;
                let py = center.y + (ny - 0.5) * radius * 2.0;

                let intensity = (count as f32 / max_density as f32).sqrt();
                let color = if intensity > 0.3 { bright_color } else { dot_color };
                let dot_r = 1.0 + intensity * 2.0;
                painter.circle_filled(egui::pos2(px, py), dot_r, color);
            }
        }
    }

    // Skin tone reference line (~123° from positive Cr axis, i.e. ~33° from horizontal)
    // In Cb/Cr space: angle ≈ 33° → direction (cos33°, sin33°) ≈ (0.838, 0.545)
    let skin_angle = 33.0_f32.to_radians();
    let skin_color = egui::Color32::from_rgba_unmultiplied(0x90, 0x70, 0x40, 60);
    painter.line_segment(
        [center, egui::pos2(
            center.x + skin_angle.cos() * radius,
            center.y - skin_angle.sin() * radius,
        )],
        egui::Stroke::new(1.0, skin_color),
    );

    // Labels at target positions
    let label_color = egui::Color32::from_rgb(0x60, 0x60, 0x70);
    let label_font = egui::FontId::proportional(9.0);
    let shadow_color = egui::Color32::from_rgb(0x06, 0x06, 0x0a);
    let labels = [("R", 0.1688_f32, 0.2933_f32), ("Mg", 0.2833, -0.0500), ("B", 0.1150, -0.3433),
                  ("Cy", -0.1688, -0.2933), ("G", -0.2833, 0.0500), ("Yl", -0.1150, 0.3433)];
    for (name, cb, cr) in &labels {
        let tx = center.x + cb * radius * 2.0;
        let ty = center.y - cr * radius * 2.0 - 8.0;
        // Shadow then label
        painter.text(egui::pos2(tx + 0.5, ty + 0.5), egui::Align2::CENTER_BOTTOM, name, label_font.clone(), shadow_color);
        painter.text(egui::pos2(tx, ty), egui::Align2::CENTER_BOTTOM, name, label_font.clone(), label_color);
    }

    // "Skin" label on the skin tone line
    let skin_label_x = center.x + skin_angle.cos() * radius * 0.55;
    let skin_label_y = center.y - skin_angle.sin() * radius * 0.55 - 6.0;
    painter.text(
        egui::pos2(skin_label_x, skin_label_y),
        egui::Align2::CENTER_BOTTOM,
        "I",
        egui::FontId::proportional(8.0),
        egui::Color32::from_rgb(0x70, 0x58, 0x30),
    );
}

fn render_histogram(rgba: &[u8], width: u32, height: u32) -> egui::ColorImage {
    let hist = klipp_color::scopes::compute_histogram(rgba, width, height);
    let mut pixels = vec![egui::Color32::from_rgb(0x0a, 0x0a, 0x10); SCOPE_W * SCOPE_H];

    // Find max across all channels for normalization
    let max_count = hist.r.iter()
        .chain(hist.g.iter())
        .chain(hist.b.iter())
        .copied()
        .max()
        .unwrap_or(1)
        .max(1);

    // Draw each channel as a filled area with alpha-weighted additive blend
    // Histogram uses bins scaled to display width
    let bin_scale = SCOPE_W as f32 / 256.0;
    let channels: [(&[u32; 256], u8, u8, u8, f32); 4] = [
        (&hist.luma, 140, 140, 140, 0.15),
        (&hist.r,    220,  40,  40, 0.20),
        (&hist.g,     40, 200,  40, 0.20),
        (&hist.b,     40,  60, 220, 0.20),
    ];

    for (data, cr, cg, cb, alpha) in &channels {
        for (bin, &count) in data.iter().enumerate() {
            let x_start = (bin as f32 * bin_scale) as usize;
            let x_end = (((bin + 1) as f32 * bin_scale) as usize).min(SCOPE_W);
            let height_px = ((count as f64 / max_count as f64).sqrt() * SCOPE_H as f64) as usize;
            for x in x_start..x_end {
                for dy in 0..height_px.min(SCOPE_H) {
                    let y = SCOPE_H - 1 - dy;
                    let existing = pixels[y * SCOPE_W + x];
                    let r = (existing.r() as f32 + *cr as f32 * alpha).min(255.0) as u8;
                    let g = (existing.g() as f32 + *cg as f32 * alpha).min(255.0) as u8;
                    let b = (existing.b() as f32 + *cb as f32 * alpha).min(255.0) as u8;
                    pixels[y * SCOPE_W + x] = egui::Color32::from_rgb(r, g, b);
                }
            }
        }
    }

    egui::ColorImage {
        size: [SCOPE_W, SCOPE_H],
        pixels,
    }
}
