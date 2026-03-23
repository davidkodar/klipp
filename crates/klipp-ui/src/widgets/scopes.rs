use crate::state::ScopeMode;
use klipp_engine::SharedFrameBuffer;

/// Persistent state for scope rendering (texture cache + throttling).
pub struct ScopeState {
    texture: Option<egui::TextureHandle>,
    last_update: std::time::Instant,
}

impl Default for ScopeState {
    fn default() -> Self {
        Self {
            texture: None,
            last_update: std::time::Instant::now(),
        }
    }
}

const SCOPE_W: usize = 256;
const SCOPE_H: usize = 200;
const UPDATE_INTERVAL_MS: u128 = 100; // ~10fps

/// Render scopes into the given UI area.
pub fn show_scopes(
    ui: &mut egui::Ui,
    state: &mut ScopeState,
    mode: ScopeMode,
    frame_buffer: &SharedFrameBuffer,
) {
    let now = std::time::Instant::now();
    let should_update = now.duration_since(state.last_update).as_millis() >= UPDATE_INTERVAL_MS;

    if should_update {
        state.last_update = now;

        // Read frame data
        let fb = frame_buffer.lock().unwrap();
        if fb.width > 0 && fb.height > 0 && !fb.data.is_empty() {
            let image = match mode {
                ScopeMode::Waveform => render_waveform(&fb.data, fb.width, fb.height),
                ScopeMode::Vectorscope => render_vectorscope(&fb.data, fb.width, fb.height),
                ScopeMode::Histogram => render_histogram(&fb.data, fb.width, fb.height),
            };
            state.texture = Some(ui.ctx().load_texture(
                "scope",
                image,
                egui::TextureOptions::NEAREST,
            ));
        }
    }

    // Draw scope
    if let Some(tex) = &state.texture {
        let available = ui.available_size();
        let scope_w = available.x.min(SCOPE_W as f32 * 2.0);
        let scope_h = scope_w * (SCOPE_H as f32 / SCOPE_W as f32);
        let size = egui::vec2(scope_w, scope_h);
        ui.image(egui::load::SizedTexture::new(tex.id(), size));
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

            // Color: green in safe range, amber near clip, red at clip
            let color = if !(10..=245).contains(&bin_idx) {
                egui::Color32::from_rgb(brightness, brightness / 3, brightness / 6) // red
            } else if !(25..=230).contains(&bin_idx) {
                egui::Color32::from_rgb(brightness, brightness * 2 / 3, brightness / 4) // amber
            } else {
                egui::Color32::from_rgb(brightness / 3, brightness, brightness / 3) // green
            };

            pixels[y * SCOPE_W + col_idx] = color;
        }
    }

    // Draw 0% and 100% reference lines
    let line_color = egui::Color32::from_rgba_unmultiplied(80, 80, 100, 60);
    for x in 0..SCOPE_W {
        pixels[x] = line_color; // top (100%)
        pixels[(SCOPE_H - 1) * SCOPE_W + x] = line_color; // bottom (0%)
        let mid = SCOPE_H / 2;
        pixels[mid * SCOPE_W + x] = line_color; // 50%
    }

    egui::ColorImage {
        size: [SCOPE_W, SCOPE_H],
        pixels,
    }
}

fn render_vectorscope(rgba: &[u8], width: u32, height: u32) -> egui::ColorImage {
    let vs = klipp_color::scopes::compute_vectorscope(rgba, width, height);
    let size = SCOPE_H; // square
    let mut pixels = vec![egui::Color32::from_rgb(0x0a, 0x0a, 0x10); size * size];
    let center = size / 2;
    let radius = center as f32 - 4.0;

    // Draw circle boundary
    for angle in 0..360 {
        let rad = (angle as f32).to_radians();
        let x = (center as f32 + rad.cos() * radius) as usize;
        let y = (center as f32 + rad.sin() * radius) as usize;
        if x < size && y < size {
            pixels[y * size + x] = egui::Color32::from_rgb(0x25, 0x25, 0x30);
        }
    }

    // Draw crosshair
    let line_color = egui::Color32::from_rgb(0x20, 0x20, 0x28);
    for i in 0..size {
        pixels[center * size + i] = line_color;
        pixels[i * size + center] = line_color;
    }

    // Draw 75% target ring
    let ring_r = radius * 0.75;
    for angle in 0..360 {
        let rad = (angle as f32).to_radians();
        let x = (center as f32 + rad.cos() * ring_r) as usize;
        let y = (center as f32 + rad.sin() * ring_r) as usize;
        if x < size && y < size {
            pixels[y * size + x] = egui::Color32::from_rgb(0x1c, 0x1c, 0x24);
        }
    }

    // Plot points
    for &(cb, cr) in &vs.points {
        // Map Cb/Cr (-0.5..0.5) to pixel coordinates
        let x = (center as f32 + cb * radius * 2.0) as usize;
        let y = (center as f32 - cr * radius * 2.0) as usize; // flip Y for display
        if x < size && y < size {
            let existing = pixels[y * size + x];
            // Accumulate brightness
            let r = (existing.r() as u16 + 8).min(255) as u8;
            let g = (existing.g() as u16 + 12).min(255) as u8;
            let b = (existing.b() as u16 + 6).min(255) as u8;
            pixels[y * size + x] = egui::Color32::from_rgb(r, g, b);
        }
    }

    egui::ColorImage {
        size: [size, size],
        pixels,
    }
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

    // Draw each channel as a filled area
    let channels: [(_, egui::Color32); 4] = [
        (&hist.luma, egui::Color32::from_rgba_unmultiplied(180, 180, 180, 40)),
        (&hist.r, egui::Color32::from_rgba_unmultiplied(220, 60, 60, 50)),
        (&hist.g, egui::Color32::from_rgba_unmultiplied(60, 200, 60, 50)),
        (&hist.b, egui::Color32::from_rgba_unmultiplied(60, 80, 220, 50)),
    ];

    for (data, color) in &channels {
        for (x, &count) in data.iter().enumerate() {
            if x >= SCOPE_W { break; }
            let height_px = ((count as f64 / max_count as f64).sqrt() * SCOPE_H as f64) as usize;
            for dy in 0..height_px.min(SCOPE_H) {
                let y = SCOPE_H - 1 - dy;
                let existing = pixels[y * SCOPE_W + x];
                // Additive blend
                let r = (existing.r() as u16 + color.r() as u16).min(255) as u8;
                let g = (existing.g() as u16 + color.g() as u16).min(255) as u8;
                let b = (existing.b() as u16 + color.b() as u16).min(255) as u8;
                pixels[y * SCOPE_W + x] = egui::Color32::from_rgb(r, g, b);
            }
        }
    }

    egui::ColorImage {
        size: [SCOPE_W, SCOPE_H],
        pixels,
    }
}
