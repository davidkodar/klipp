use crate::state::{
    HistogramScale, PaneConfig, PaneScopeSettings, ScopeLayout, ScopeScale, ScopeType,
    ScopeWindowState, VectorscopeScaleStyle, WaveformChannel, WaveformPaintMode,
    WaveformScaleStyle,
};
use klipp_engine::SharedFrameBuffer;
use std::time::Instant;

/// Runtime state for the floating scope window (not serialized).
pub struct ScopeWindowRuntime {
    /// One texture per pane slot (max 9 panes).
    textures: [Option<egui::TextureHandle>; 9],
    /// Frame generation per pane.
    frame_gens: [u64; 9],
    /// Last render time per pane.
    last_render: [Option<Instant>; 9],
    /// Which pane's settings panel is open (None = none).
    pub settings_pane: Option<usize>,
    /// Which pane's type dropdown is open (None = none).
    pub type_dropdown_pane: Option<usize>,
    /// Whether the ··· menu is open.
    pub dot_menu_open: bool,
    /// Whether the pointer is currently over the scope window (blocks background interaction).
    pub pointer_over: bool,
}

impl Default for ScopeWindowRuntime {
    fn default() -> Self {
        Self {
            textures: [None, None, None, None, None, None, None, None, None],
            frame_gens: [u64::MAX; 9],
            last_render: [None; 9],
            settings_pane: None,
            type_dropdown_pane: None,
            dot_menu_open: false,
            pointer_over: false,
        }
    }
}

/// Sample a pixel (or box-averaged area) from the frame buffer at a viewer pointer position.
/// Converts screen coordinates to image pixel coordinates accounting for letterboxing.
pub fn sample_viewer_pixel(
    pointer_pos: egui::Pos2,
    image_rect: egui::Rect,
    fb: &klipp_engine::FrameBuffer,
    sample_radius: u32,
) -> Option<crate::state::ScopeCursorData> {
    if !image_rect.contains(pointer_pos) {
        return None;
    }
    let nx = ((pointer_pos.x - image_rect.min.x) / image_rect.width()).clamp(0.0, 1.0);
    let ny = ((pointer_pos.y - image_rect.min.y) / image_rect.height()).clamp(0.0, 1.0);
    if fb.width == 0 || fb.height == 0 || fb.data.is_empty() {
        return None;
    }
    let px = ((nx * fb.width as f32) as u32).min(fb.width - 1);
    let py = ((ny * fb.height as f32) as u32).min(fb.height - 1);

    let r = sample_radius as i32;
    let mut sum_r = 0u32;
    let mut sum_g = 0u32;
    let mut sum_b = 0u32;
    let mut count = 0u32;
    for dy in -r..=r {
        for dx in -r..=r {
            let sx = (px as i32 + dx).clamp(0, fb.width as i32 - 1) as u32;
            let sy = (py as i32 + dy).clamp(0, fb.height as i32 - 1) as u32;
            let idx = ((sy * fb.width + sx) * 4) as usize;
            if idx + 3 <= fb.data.len() {
                sum_r += fb.data[idx] as u32;
                sum_g += fb.data[idx + 1] as u32;
                sum_b += fb.data[idx + 2] as u32;
                count += 1;
            }
        }
    }
    if count == 0 {
        return None;
    }
    let sr = (sum_r / count) as f32 / 255.0;
    let sg = (sum_g / count) as f32 / 255.0;
    let sb = (sum_b / count) as f32 / 255.0;
    Some(crate::state::ScopeCursorData::from_srgb(nx, sr, sg, sb))
}

/// Show the floating scope window.
pub fn show_scope_window(
    ctx: &egui::Context,
    state: &mut ScopeWindowState,
    runtime: &mut ScopeWindowRuntime,
    frame_buffer: &SharedFrameBuffer,
    skin_tone_angle: f32,
    is_playing: bool,
    viewer_image_rect: Option<egui::Rect>,
    viewer_pointer_pos: Option<egui::Pos2>,
    locked_cursor_data: Option<crate::state::ScopeCursorData>,
    spectrum_bins: &[f32],
    // `correlation_samples`: interleaved stereo PCM (LRLR…) for the
    // correlation scope. Collected at the call site from the last ~4k
    // samples of the shared spectrum ring.
    // `lufs_history`: oldest-first short-term LUFS for the loudness-history
    // scope. `lufs_target` is drawn as the reference line.
    correlation_samples: &[f32],
    lufs_history: &[f32],
    lufs_target: f32,
    // `spectrum_history`: oldest-first slices of normalised FFT bins
    // (each slice is one `Vec<f32>` of 256 values in 0.0–1.0) for the
    // spectrogram waterfall scope. Collected at the call site.
    spectrum_history: &[Vec<f32>],
) {
    if !state.open {
        return;
    }

    // Sample scope cursor: use locked data if available, otherwise live sample.
    let scope_cursor = if let Some(locked) = locked_cursor_data {
        Some(locked)
    } else {
        let sample_radius = state.panes.iter()
            .filter(|p| p.settings.show_scope_cursor)
            .map(|p| p.settings.scope_cursor_sample_radius)
            .max()
            .unwrap_or(2);

        viewer_pointer_pos.and_then(|pos| {
            let image_rect = viewer_image_rect?;
            let fb = frame_buffer.lock().unwrap();
            let result = sample_viewer_pixel(pos, image_rect, &fb, sample_radius);
            if result.is_some() {
                ctx.request_repaint();
            }
            result
        })
    };

    // Ensure pane count matches layout
    let needed = state.layout.pane_count();
    if state.panes.len() != needed {
        state.panes = PaneConfig::default_for_layout(state.layout);
    }

    let area_id = egui::Id::new("scope_area_v1");

    let area_resp = egui::Area::new(area_id)
        .movable(true)
        .order(egui::Order::Middle)
        .default_pos([100.0, 60.0])
        .show(ctx, |ui| {
            let panel_frame = egui::Frame::none()
                .fill(egui::Color32::from_rgb(0x1c, 0x1c, 0x20))
                .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(0x3a, 0x3a, 0x40)))
                .rounding(4.0)
                .inner_margin(0.0)
                .shadow(egui::Shadow { offset: egui::vec2(0.0, 2.0), blur: 12.0, spread: 0.0, color: egui::Color32::from_black_alpha(100) });

            panel_frame.show(ui, |ui| {
                egui::Resize::default()
                    .id(area_id.with("resize"))
                    .default_size([700.0, 500.0])
                    .min_size([300.0, 220.0])
                    .show(ui, |ui| {
                        // ── Header bar ──
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 3.0;
                            // Scopes label
                            ui.label(egui::RichText::new("Scopes").size(11.0).color(egui::Color32::from_rgb(0x90, 0x90, 0x98)));
                            ui.separator();
                            // Layout buttons
                            for (layout, label) in [(ScopeLayout::OnePane, "1"), (ScopeLayout::TwoPane, "2"),
                                (ScopeLayout::FourPane, "4"), (ScopeLayout::NinePane, "9")] {
                                let active = state.layout == layout;
                                let color = if active { egui::Color32::from_rgb(0xf5, 0xb4, 0x3c) } else { egui::Color32::from_rgb(0x60, 0x60, 0x68) };
                                if ui.add(egui::Button::new(egui::RichText::new(label).size(9.0).color(color))
                                    .fill(if active { egui::Color32::from_rgb(0x2a, 0x25, 0x18) } else { egui::Color32::TRANSPARENT })
                                    .min_size(egui::vec2(16.0, 14.0))).clicked() {
                                    state.layout = layout;
                                    state.panes = PaneConfig::default_for_layout(layout);
                                    *runtime = ScopeWindowRuntime::default();
                                }
                            }
                            // Right-aligned: close button
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.add(egui::Button::new(
                                    egui::RichText::new("x").size(11.0).color(egui::Color32::from_rgb(0x80, 0x80, 0x88))
                                ).frame(false)).clicked() {
                                    state.open = false;
                                }
                                // ··· menu
                                let dot_color = if runtime.dot_menu_open { egui::Color32::from_rgb(0xf5, 0xb4, 0x3c) } else { egui::Color32::from_rgb(0x70, 0x70, 0x78) };
                                if ui.add(egui::Button::new(
                                    egui::RichText::new("\u{2022}\u{2022}\u{2022}").size(10.0).color(dot_color)
                                ).frame(false)).clicked() {
                                    runtime.dot_menu_open = !runtime.dot_menu_open;
                                }
                            });
                        });

                        // ── Pane grid ──
                        let (cols, rows) = state.layout.grid();
                        let cursor = ui.cursor().min;
                        let avail = ui.available_size();
                        let grid_w = avail.x.max(100.0);
                        let grid_h = avail.y.max(80.0);
                        let gap = 2.0;
                        let pane_w = ((grid_w - (cols.saturating_sub(1)) as f32 * gap) / cols as f32).max(40.0);
                        let pane_h = ((grid_h - (rows.saturating_sub(1)) as f32 * gap) / rows as f32).max(30.0);

                        for row in 0..rows {
                            for col in 0..cols {
                                let pane_idx = row * cols + col;
                                if pane_idx >= state.panes.len() { break; }
                                let x = cursor.x + col as f32 * (pane_w + gap);
                                let y = cursor.y + row as f32 * (pane_h + gap);
                                let pane_rect = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(pane_w, pane_h));
                                ui.painter().rect_stroke(pane_rect, 0.0,
                                    egui::Stroke::new(0.5, egui::Color32::from_rgb(0x2a, 0x2a, 0x30)));
                                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(pane_rect));
                                draw_pane(&mut child, pane_idx, state, runtime, frame_buffer, skin_tone_angle, is_playing, scope_cursor, spectrum_bins, correlation_samples, lufs_history, lufs_target, spectrum_history);
                            }
                        }

                        // ··· menu popup
                        if runtime.dot_menu_open {
                            draw_dot_menu(ui, state, runtime);
                        }
                    });
            });
        });

    // Block background interaction when pointer is over scope window
    if let Some(hover_pos) = ctx.input(|i| i.pointer.hover_pos()) {
        if area_resp.response.rect.contains(hover_pos) {
            runtime.pointer_over = true;
        }
    }
}

// ── Per-pane rendering ───────────────────────────────────────────────────

fn draw_pane(
    ui: &mut egui::Ui,
    pane_idx: usize,
    state: &mut ScopeWindowState,
    runtime: &mut ScopeWindowRuntime,
    frame_buffer: &SharedFrameBuffer,
    skin_tone_angle: f32,
    is_playing: bool,
    scope_cursor: Option<crate::state::ScopeCursorData>,
    spectrum_bins: &[f32],
    correlation_samples: &[f32],
    lufs_history: &[f32],
    lufs_target: f32,
    spectrum_history: &[Vec<f32>],
) {
    let pane_rect = ui.max_rect();
    let scope_type = state.panes[pane_idx].scope_type;

    // ── Bottom header bar (scope type label + settings gear) ──
    let header_h = 20.0;
    let header_rect = egui::Rect::from_min_max(
        egui::pos2(pane_rect.min.x, pane_rect.max.y - header_h),
        pane_rect.max,
    );

    // ── Content area (everything above header) ──
    let content_rect = egui::Rect::from_min_max(
        pane_rect.min,
        egui::pos2(pane_rect.max.x, header_rect.min.y),
    );

    // Allocate the full pane to prevent layout expansion
    ui.allocate_rect(pane_rect, egui::Sense::hover());

    // Use painter_at to ensure we can paint anywhere in the pane
    let p = ui.painter_at(pane_rect);

    // Dark background for content
    p.rect_filled(content_rect, 0.0, egui::Color32::from_rgb(0x04, 0x04, 0x07));

    // Header background
    p.rect_filled(header_rect, 0.0, egui::Color32::from_rgb(0x1a, 0x1a, 0x1e));

    // Scope type label (clickable for dropdown)
    let type_text = format!("{} \u{25be}", scope_type.label());
    p.text(
        egui::pos2(header_rect.min.x + 6.0, header_rect.center().y),
        egui::Align2::LEFT_CENTER,
        &type_text,
        egui::FontId::proportional(10.0),
        egui::Color32::from_rgb(0xc0, 0xc0, 0xc8),
    );
    let type_click_rect = egui::Rect::from_min_size(header_rect.min, egui::vec2(header_rect.width() - 24.0, header_h));
    if ui.allocate_rect(type_click_rect, egui::Sense::click()).clicked() {
        runtime.type_dropdown_pane = if runtime.type_dropdown_pane == Some(pane_idx) { None } else {
            runtime.settings_pane = None;
            Some(pane_idx)
        };
    }

    // Settings gear icon (right side of header)
    let gear_rect = egui::Rect::from_min_size(
        egui::pos2(header_rect.max.x - 22.0, header_rect.min.y),
        egui::vec2(22.0, header_h),
    );
    let gear_color = if runtime.settings_pane == Some(pane_idx) {
        egui::Color32::from_rgb(0xf5, 0xb4, 0x3c)
    } else {
        egui::Color32::from_rgb(0x70, 0x70, 0x78)
    };
    p.text(
        gear_rect.center(), egui::Align2::CENTER_CENTER,
        "\u{2699}", egui::FontId::proportional(12.0), gear_color,
    );
    if ui.allocate_rect(gear_rect, egui::Sense::click()).clicked() {
        runtime.settings_pane = if runtime.settings_pane == Some(pane_idx) { None } else {
            runtime.type_dropdown_pane = None;
            Some(pane_idx)
        };
    }

    // Waveform channel pills in header
    if scope_type == ScopeType::Waveform {
        let channels = [
            (WaveformChannel::Y, egui::Color32::from_rgb(0xc0, 0xc0, 0xc0)),
            (WaveformChannel::R, egui::Color32::from_rgb(0xd0, 0x40, 0x40)),
            (WaveformChannel::G, egui::Color32::from_rgb(0x40, 0xb0, 0x40)),
            (WaveformChannel::B, egui::Color32::from_rgb(0x40, 0x50, 0xd0)),
        ];
        let pill_w = 16.0;
        let total_w = channels.len() as f32 * (pill_w + 2.0);
        let start_x = header_rect.center().x - total_w / 2.0;
        let current_ch = state.panes[pane_idx].settings.waveform_channel;
        for (i, (ch, color)) in channels.iter().enumerate() {
            let bx = start_x + i as f32 * (pill_w + 2.0);
            let pill_rect = egui::Rect::from_min_size(
                egui::pos2(bx, header_rect.min.y + 2.0),
                egui::vec2(pill_w, header_h - 4.0),
            );
            let is_active = current_ch == *ch;
            if is_active {
                p.rect_filled(pill_rect, 2.0, color.linear_multiply(0.25));
            }
            p.text(
                pill_rect.center(), egui::Align2::CENTER_CENTER,
                ch.label(), egui::FontId::monospace(8.0),
                if is_active { *color } else { color.linear_multiply(0.4) },
            );
            if ui.allocate_rect(pill_rect, egui::Sense::click()).clicked() {
                state.panes[pane_idx].settings.waveform_channel = *ch;
            }
        }
    }

    // ── Type dropdown popup ──
    if runtime.type_dropdown_pane == Some(pane_idx) {
        let dd_resp = egui::Area::new(egui::Id::new(("scope_type_dd", pane_idx)))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(header_rect.min.x + 4.0, header_rect.min.y - 5.0 * 22.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(140.0);
                    for st in ScopeType::ALL {
                        let is_current = scope_type == st;
                        let prefix = if is_current { "\u{2022} " } else { "   " };
                        if ui.selectable_label(is_current, format!("{prefix}{}", st.label())).clicked() {
                            state.panes[pane_idx].scope_type = st;
                            runtime.type_dropdown_pane = None;
                            runtime.textures[pane_idx] = None;
                            runtime.frame_gens[pane_idx] = u64::MAX;
                        }
                    }
                });
            });
        // Close on click outside
        if ui.input(|i| i.pointer.primary_clicked()) {
            if let Some(hover_pos) = ui.input(|i| i.pointer.hover_pos()) {
                if !dd_resp.response.rect.contains(hover_pos)
                    && !type_click_rect.contains(hover_pos)
                {
                    runtime.type_dropdown_pane = None;
                }
            }
        }
    }

    // ── Settings panel popup ──
    if runtime.settings_pane == Some(pane_idx) {
        let settings_resp = egui::Area::new(egui::Id::new(("scope_settings", pane_idx)))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(header_rect.max.x - 200.0, header_rect.min.y - 250.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(185.0);
                    // Close button at the top
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("SETTINGS").size(9.0)
                            .color(egui::Color32::from_rgb(0x80, 0x80, 0x88)).strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add(egui::Button::new(
                                egui::RichText::new("x").size(10.0)
                                    .color(egui::Color32::from_rgb(0x90, 0x90, 0x98))
                            ).frame(false)).clicked() {
                                runtime.settings_pane = None;
                            }
                        });
                    });
                    ui.separator();
                    draw_pane_settings(ui, pane_idx, state);
                });
            });
        // Close on click outside the settings panel
        if ui.input(|i| i.pointer.primary_clicked()) {
            if let Some(hover_pos) = ui.input(|i| i.pointer.hover_pos()) {
                if !settings_resp.response.rect.contains(hover_pos)
                    && !gear_rect.contains(hover_pos)
                {
                    runtime.settings_pane = None;
                }
            }
        }
    }

    // ── Render scope content ──
    if content_rect.width() < 10.0 || content_rect.height() < 10.0 {
        return;
    }

    render_pane_scope(ui, pane_idx, state, runtime, frame_buffer, skin_tone_angle, is_playing, content_rect, scope_cursor, spectrum_bins, correlation_samples, lufs_history, lufs_target, spectrum_history);
}

fn render_pane_scope(
    ui: &mut egui::Ui,
    pane_idx: usize,
    state: &ScopeWindowState,
    runtime: &mut ScopeWindowRuntime,
    frame_buffer: &SharedFrameBuffer,
    skin_tone_angle: f32,
    is_playing: bool,
    content_rect: egui::Rect,
    scope_cursor: Option<crate::state::ScopeCursorData>,
    spectrum_bins: &[f32],
    correlation_samples: &[f32],
    lufs_history: &[f32],
    lufs_target: f32,
    spectrum_history: &[Vec<f32>],
) {
    let pane = &state.panes[pane_idx];
    let render_h: usize = 256; // Smaller for multi-pane to stay fast

    // Audio-only scopes (spectrum, correlation, loudness history) have no
    // video-frame dependency — render them directly from shared audio
    // state and short-circuit the fb lock/throttle. Bump generation every
    // frame so the texture updates; throttle lightly so we don't re-upload
    // on every repaint when audio is idle.
    let is_audio_only = matches!(
        pane.scope_type,
        ScopeType::AudioSpectrum
            | ScopeType::AudioCorrelation
            | ScopeType::LoudnessHistory
            | ScopeType::Spectrogram
    );
    let scope_image = if is_audio_only {
        let throttled = runtime.last_render[pane_idx]
            .map_or(false, |t| t.elapsed().as_millis() < 33);
        if throttled {
            None
        } else {
            let render_w = 512usize;
            let image = match pane.scope_type {
                ScopeType::AudioSpectrum => {
                    super::scopes::render_audio_spectrum(spectrum_bins, render_w, render_h)
                }
                ScopeType::AudioCorrelation => {
                    // Correlation is a square scope — render square so the
                    // Lissajous figure isn't visually stretched. The outer
                    // paint layer will letterbox if the pane isn't square.
                    let side = render_h;
                    super::scopes::render_audio_correlation(correlation_samples, side, side)
                }
                ScopeType::LoudnessHistory => {
                    super::scopes::render_lufs_history(lufs_history, lufs_target, render_w, render_h)
                }
                ScopeType::Spectrogram => {
                    super::scopes::render_audio_spectrogram(spectrum_history, render_w, render_h)
                }
                _ => unreachable!("is_audio_only gates this match"),
            };
            let gen = runtime.frame_gens[pane_idx].wrapping_add(1);
            Some((image, gen))
        }
    } else {
        let fb = frame_buffer.lock().unwrap();
        if fb.width == 0 || fb.height == 0 || fb.data.is_empty() {
            None
        } else {
            let gen = fb.generation;
            let stale = runtime.textures[pane_idx].is_none()
                || runtime.frame_gens[pane_idx] != gen;

            let throttled = is_playing && runtime.last_render[pane_idx]
                .map_or(false, |t| t.elapsed().as_millis() < 80);

            if stale && !throttled {
                let (src, w, h) = (&fb.data[..], fb.width, fb.height);
                let render_w = (w as usize).min(960); // Cap render width for perf
                let step: u32 = if w > 1280 { 4 } else if w > 640 { 3 } else { 2 };

                let image = match pane.scope_type {
                    ScopeType::Waveform => {
                        super::scopes::render_waveform_channel(
                            src, w, h, render_w, render_h,
                            pane.settings.waveform_channel,
                            pane.settings.colorize,
                            pane.settings.waveform_paint_mode,
                        )
                    }
                    ScopeType::Parade => {
                        super::scopes::render_parade_direct(src, w, h, render_w, render_h)
                    }
                    ScopeType::Histogram => {
                        let hist = klipp_color::scopes::compute_histogram_sampled(src, w, h, step);
                        super::scopes::render_histogram_stacked(
                            &hist.r, &hist.g, &hist.b,
                            render_w, render_h,
                            pane.settings.histogram_scale,
                        )
                    }
                    ScopeType::Vectorscope => {
                        super::scopes::render_vectorscope(
                            src, w, h, step, render_w, render_h,
                            skin_tone_angle,
                            pane.settings.show_iq_lines,
                        )
                    }
                    ScopeType::CIEChromaticity => {
                        super::scopes::render_cie_chromaticity(src, w, h, step, render_w, render_h)
                    }
                    ScopeType::AudioSpectrum
                    | ScopeType::AudioCorrelation
                    | ScopeType::LoudnessHistory
                    | ScopeType::Spectrogram => unreachable!(
                        "audio-only scopes are rendered before the frame-buffer match"
                    ),
                };
                Some((image, gen))
            } else {
                None
            }
        }
    };

    if let Some((image, gen)) = scope_image {
        runtime.frame_gens[pane_idx] = gen;
        runtime.last_render[pane_idx] = Some(Instant::now());

        if let Some(ref mut tex) = runtime.textures[pane_idx] {
            tex.set(image, egui::TextureOptions::LINEAR);
        } else {
            runtime.textures[pane_idx] = Some(ui.ctx().load_texture(
                format!("scope_win_{pane_idx}"), image, egui::TextureOptions::LINEAR,
            ));
        }
    }

    // Paint directly into content_rect (already allocated by draw_pane)
    let r = content_rect;
    let painter = ui.painter_at(r);

    if let Some(tex) = &runtime.textures[pane_idx] {
        let is_vector = pane.scope_type == ScopeType::Vectorscope
            || pane.scope_type == ScopeType::CIEChromaticity;
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));

        if is_vector {
            let side = r.width().min(r.height());
            let offset_x = (r.width() - side) / 2.0;
            let offset_y = (r.height() - side) / 2.0;
            let img_rect = egui::Rect::from_min_size(
                egui::pos2(r.min.x + offset_x, r.min.y + offset_y),
                egui::vec2(side, side),
            );
            let bri = pane.settings.vectorscope_brightness;
            let tint_val = (bri * 2.0).min(1.0);
            let tint = egui::Color32::from_rgb(
                (tint_val * 255.0) as u8, (tint_val * 255.0) as u8, (tint_val * 255.0) as u8,
            );
            painter.image(tex.id(), img_rect, uv, tint);

            // Saturation rings
            let rings = crate::state::compute_saturation_rings(&pane.settings, side);
            let ring_alpha = crate::state::graticule_alpha(pane.settings.vectorscope_graticule_brightness);
            let ring_color = egui::Color32::from_rgba_unmultiplied(0xf5, 0xb4, 0x3c, (ring_alpha * 80.0) as u8);
            for ring in &rings {
                painter.circle_stroke(img_rect.center(), ring.radius, egui::Stroke::new(0.5, ring_color));
            }

            // Vectorscope color target labels (R, Mg, B, Cy, G, Yl)
            if pane.scope_type == ScopeType::Vectorscope {
                let label_font = egui::FontId::monospace(8.0);
                let label_color = egui::Color32::from_rgb(0x90, 0x78, 0x30);
                let cx = img_rect.center().x;
                let cy = img_rect.center().y;
                let rad = side / 2.0 - 4.0;
                for (name, cb, cr) in &[
                    ("R",  0.17_f32,  0.29_f32), ("Mg",  0.28, -0.05), ("B",  0.12, -0.34),
                    ("Cy", -0.17,    -0.29),     ("G",  -0.28,  0.05), ("Yl", -0.12,  0.34),
                ] {
                    let tx = cx + cb * rad * 2.0;
                    let ty = cy - cr * rad * 2.0 - 6.0;
                    painter.text(egui::pos2(tx, ty), egui::Align2::CENTER_BOTTOM, *name, label_font.clone(), label_color);
                }
            }
        } else if pane.scope_type == ScopeType::Histogram {
            painter.image(tex.id(), r, uv, egui::Color32::WHITE);
        } else {
            // Waveform/Parade: scope image in trace area + scale overlay
            let scale = pane.settings.scale;
            let img_rect = egui::Rect::from_min_max(
                egui::pos2(r.min.x + super::scopes::AXIS_W, r.min.y + super::scopes::PAD_TOP),
                egui::pos2(r.max.x - super::scopes::PAD_RIGHT, r.max.y - super::scopes::PAD_BOTTOM),
            );
            let bri = pane.settings.waveform_brightness;
            let tint_val = (bri * 2.0).min(1.0);
            let tint = egui::Color32::from_rgb(
                (tint_val * 255.0) as u8, (tint_val * 255.0) as u8, (tint_val * 255.0) as u8,
            );
            painter.image(tex.id(), img_rect, uv, tint);

            let is_parade = pane.scope_type == ScopeType::Parade;
            super::scopes::draw_scale(ui, r, scale, is_parade, 3);

            // Reference level lines (amber dashed)
            if pane.settings.show_reference_levels {
                let lines = crate::state::compute_reference_lines(&pane.settings, r.height());
                let amber_ref = egui::Color32::from_rgba_unmultiplied(0xf5, 0xb4, 0x3c, 140);
                for line in &lines {
                    let y = r.min.y + line.y;
                    let x0 = r.min.x + super::scopes::AXIS_W;
                    let x1 = r.max.x - super::scopes::PAD_RIGHT;
                    let mut x = x0;
                    while x < x1 {
                        painter.line_segment(
                            [egui::pos2(x, y), egui::pos2((x + 6.0).min(x1), y)],
                            egui::Stroke::new(1.0, amber_ref),
                        );
                        x += 10.0;
                    }
                }
            }
        }

        // ── Scope cursor indicator ──
        if pane.settings.show_scope_cursor {
            if let Some(cur) = scope_cursor {
                draw_scope_cursor_indicator(&painter, pane.scope_type, &cur, r);
            }
        }

        ui.ctx().request_repaint();
    }
}

/// Draw a small circle/crosshair on the scope showing where the hovered pixel maps.
pub fn draw_scope_cursor_indicator(
    painter: &egui::Painter,
    scope_type: ScopeType,
    cur: &crate::state::ScopeCursorData,
    rect: egui::Rect,
) {
    let cursor_color = egui::Color32::from_rgb(0xff, 0xff, 0xff);
    let outline_color = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 200);
    let amber = egui::Color32::from_rgb(0xf5, 0xb4, 0x3c);
    let radius = 5.0;

    // Helper: draw a visible indicator dot at a position
    let draw_dot = |p: &egui::Painter, pos: egui::Pos2| {
        p.circle_filled(pos, 2.0, amber);
        p.circle_stroke(pos, radius, egui::Stroke::new(1.5, outline_color));
        p.circle_stroke(pos, radius, egui::Stroke::new(1.0, cursor_color));
    };

    match scope_type {
        ScopeType::Waveform => {
            let x = rect.min.x + super::scopes::AXIS_W
                + cur.x_frac * (rect.width() - super::scopes::AXIS_W - super::scopes::PAD_RIGHT);
            let draw_h = rect.height() - super::scopes::PAD_TOP - super::scopes::PAD_BOTTOM;
            let y = rect.min.y + super::scopes::PAD_TOP + (1.0 - cur.luma) * draw_h;
            draw_dot(painter, egui::pos2(x, y));
        }
        ScopeType::Parade => {
            let trace_w = rect.width() - super::scopes::AXIS_W - super::scopes::PAD_RIGHT;
            let section_w = trace_w / 3.0;
            let draw_h = rect.height() - super::scopes::PAD_TOP - super::scopes::PAD_BOTTOM;
            let base_x = rect.min.x + super::scopes::AXIS_W;
            for (i, val) in [cur.r, cur.g, cur.b].iter().enumerate() {
                let x = base_x + i as f32 * section_w + cur.x_frac * section_w;
                let y = rect.min.y + super::scopes::PAD_TOP + (1.0 - val) * draw_h;
                draw_dot(painter, egui::pos2(x, y));
            }
        }
        ScopeType::Vectorscope => {
            let side = rect.width().min(rect.height());
            let cx = rect.center().x;
            let cy = rect.center().y;
            let r_radius = side / 2.0 - 8.0;
            let x = cx + cur.cb * r_radius * 2.0;
            let y = cy - cur.cr * r_radius * 2.0;
            draw_dot(painter, egui::pos2(x, y));
            // Crosshair
            let ch_color = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 120);
            painter.line_segment([egui::pos2(x - 10.0, y), egui::pos2(x + 10.0, y)], egui::Stroke::new(0.5, ch_color));
            painter.line_segment([egui::pos2(x, y - 10.0), egui::pos2(x, y + 10.0)], egui::Stroke::new(0.5, ch_color));
        }
        ScopeType::Histogram => {
            let x = rect.min.x + cur.luma * rect.width();
            painter.line_segment(
                [egui::pos2(x, rect.min.y), egui::pos2(x, rect.max.y)],
                egui::Stroke::new(1.0, amber),
            );
        }
        ScopeType::CIEChromaticity => {
            let to_linear = |v: f32| -> f32 {
                if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
            };
            let lr = to_linear(cur.r);
            let lg = to_linear(cur.g);
            let lb = to_linear(cur.b);
            let cap_x = 0.4124564 * lr + 0.3575761 * lg + 0.1804375 * lb;
            let cap_y = 0.2126729 * lr + 0.7151522 * lg + 0.0721750 * lb;
            let z = 0.0193339 * lr + 0.1191920 * lg + 0.9503041 * lb;
            let sum = cap_x + cap_y + z;
            if sum > 0.001 {
                let cxv = cap_x / sum;
                let cyv = cap_y / sum;
                let side = rect.width().min(rect.height());
                let margin = 12.0;
                let plot_size = side - margin * 2.0;
                let offset_x = (rect.width() - side) / 2.0;
                let offset_y = (rect.height() - side) / 2.0;
                let px = rect.min.x + offset_x + margin + cxv / 0.8 * plot_size;
                let py = rect.min.y + offset_y + margin + (1.0 - cyv / 0.9) * plot_size;
                draw_dot(painter, egui::pos2(px, py));
            }
        }
        ScopeType::AudioSpectrum
        | ScopeType::AudioCorrelation
        | ScopeType::LoudnessHistory
        | ScopeType::Spectrogram => {
            // Audio-only scopes have no colour-picker overlay — the cursor
            // is a video-pixel sample and there is no video pixel under
            // these scopes. Intentionally do nothing.
        }
    }
}

// ── Per-pane settings panels ─────────────────────────────────────────────

fn draw_pane_settings(ui: &mut egui::Ui, pane_idx: usize, state: &mut ScopeWindowState) {
    let scope_type = state.panes[pane_idx].scope_type;
    match scope_type {
        ScopeType::Waveform => draw_waveform_settings(ui, pane_idx, state),
        ScopeType::Parade => draw_parade_settings(ui, pane_idx, state),
        ScopeType::Vectorscope => draw_vectorscope_settings(ui, pane_idx, state),
        ScopeType::Histogram => draw_histogram_settings(ui, pane_idx, state),
        ScopeType::CIEChromaticity => {
            ui.label("No settings available.");
        }
        ScopeType::AudioSpectrum => {
            ui.label("No settings available.");
            ui.small("Frequency axis: 20 Hz … Nyquist, log-spaced.");
            ui.small("Amplitude axis: −96 … 0 dBFS.");
        }
        ScopeType::AudioCorrelation => {
            ui.label("No settings available.");
            ui.small("Lissajous of L vs R (45° rotated).");
            ui.small("Bottom strip: Pearson correlation \u{2212}1 … +1.");
        }
        ScopeType::LoudnessHistory => {
            ui.label("No settings available.");
            ui.small("Short-term LUFS over the last ~60 s.");
            ui.small("Green band = \u{00B1}1 LU around the loudness target.");
        }
        ScopeType::Spectrogram => {
            ui.label("No settings available.");
            ui.small("Scrolling FFT waterfall — ~10 s of history.");
            ui.small("x = time, y = frequency (log), colour = magnitude.");
        }
    }

    ui.separator();
    ui.checkbox(&mut state.panes[pane_idx].settings.show_scope_cursor, "Show Picker Overlay");

    // Sample box size control
    if state.panes[pane_idx].settings.show_scope_cursor {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label("Sample:");
            let r = &mut state.panes[pane_idx].settings.scope_cursor_sample_radius;
            if ui.small_button("\u{2212}").clicked() && *r > 0 {
                *r -= 1;
            }
            let side = *r * 2 + 1;
            ui.label(format!("{side}\u{00D7}{side}"));
            if ui.small_button("+").clicked() && *r < 7 {
                *r += 1;
            }
        });
    }

    ui.add_space(6.0);
    if ui.add(egui::Button::new("Reset View").min_size(egui::vec2(170.0, 22.0))).clicked() {
        state.panes[pane_idx].settings = PaneScopeSettings::default();
    }
}

fn draw_waveform_settings(ui: &mut egui::Ui, pane_idx: usize, state: &mut ScopeWindowState) {
    let s = &mut state.panes[pane_idx].settings;

    // Channel selector: Y R G B
    ui.horizontal(|ui| {
        let channels = [
            (WaveformChannel::Y, egui::Color32::from_rgb(0xc0, 0xc0, 0xc0)),
            (WaveformChannel::R, egui::Color32::from_rgb(0xd0, 0x40, 0x40)),
            (WaveformChannel::G, egui::Color32::from_rgb(0x40, 0xb0, 0x40)),
            (WaveformChannel::B, egui::Color32::from_rgb(0x40, 0x50, 0xd0)),
        ];
        for (ch, color) in &channels {
            let is_sel = s.waveform_channel == *ch;
            let btn = egui::Button::new(
                egui::RichText::new(ch.label()).size(11.0)
                    .color(if is_sel { *color } else { egui::Color32::from_rgb(0x60, 0x60, 0x68) })
            ).min_size(egui::vec2(30.0, 20.0))
             .fill(if is_sel { color.linear_multiply(0.2) } else { egui::Color32::from_rgb(0x28, 0x28, 0x2c) });
            if ui.add(btn).clicked() {
                s.waveform_channel = *ch;
            }
        }
    });

    ui.add_space(2.0);
    ui.checkbox(&mut s.colorize, "Colorize");
    ui.checkbox(&mut s.extents, "Extents");

    ui.separator();

    // Paint mode — monochrome base colour (ignored when Colorize is on).
    ui.label(egui::RichText::new("PAINT MODE").size(9.0)
        .color(egui::Color32::from_rgb(0x80, 0x80, 0x88)).strong());
    ui.radio_value(&mut s.waveform_paint_mode, WaveformPaintMode::Green, "Green");
    ui.radio_value(&mut s.waveform_paint_mode, WaveformPaintMode::Yellow, "Yellow");
    ui.radio_value(&mut s.waveform_paint_mode, WaveformPaintMode::White, "White");
    ui.radio_value(&mut s.waveform_paint_mode, WaveformPaintMode::Classic, "Classic (RGB)");

    ui.separator();

    // Brightness sliders
    ui.label(egui::RichText::new("DISPLAY").size(9.0)
        .color(egui::Color32::from_rgb(0x80, 0x80, 0x88)).strong());
    ui.horizontal(|ui| {
        ui.label("Waveform");
        ui.add(egui::Slider::new(&mut s.waveform_brightness, 0.0..=1.0).show_value(false));
    });
    ui.horizontal(|ui| {
        ui.label("Graticule");
        ui.add(egui::Slider::new(&mut s.graticule_brightness, 0.0..=1.0).show_value(false));
    });

    ui.separator();

    ui.checkbox(&mut s.low_pass, "Low Pass Filter");
    ui.checkbox(&mut s.bloom, "Bloom on dense regions");

    ui.separator();

    ui.label(egui::RichText::new("QUALITY").size(9.0)
        .color(egui::Color32::from_rgb(0x80, 0x80, 0x88)).strong());
    ui.horizontal(|ui| {
        ui.radio_value(&mut s.quality, klipp_core::ScopeQuality::Low, "Draft");
        ui.radio_value(&mut s.quality, klipp_core::ScopeQuality::Medium, "Standard");
        ui.radio_value(&mut s.quality, klipp_core::ScopeQuality::High, "Full");
    });

    ui.separator();

    // Reference levels
    ui.checkbox(&mut s.show_reference_levels, "Show Reference Levels");
    if s.show_reference_levels {
        ui.horizontal(|ui| {
            ui.label("Low");
            let amber = egui::Color32::from_rgb(0xf5, 0xb4, 0x3c);
            ui.painter().circle_filled(
                egui::pos2(ui.available_rect_before_wrap().max.x - 20.0, ui.available_rect_before_wrap().center().y),
                3.0, amber,
            );
            ui.add(egui::DragValue::new(&mut s.reference_low).range(0.0..=1023.0).speed(1.0));
        });
        ui.horizontal(|ui| {
            ui.label("High");
            let amber = egui::Color32::from_rgb(0xf5, 0xb4, 0x3c);
            ui.painter().circle_filled(
                egui::pos2(ui.available_rect_before_wrap().max.x - 20.0, ui.available_rect_before_wrap().center().y),
                3.0, amber,
            );
            ui.add(egui::DragValue::new(&mut s.reference_high).range(0.0..=1023.0).speed(1.0));
        });
    }
}

fn draw_vectorscope_settings(ui: &mut egui::Ui, pane_idx: usize, state: &mut ScopeWindowState) {
    let s = &mut state.panes[pane_idx].settings;

    // Brightness sliders
    ui.horizontal(|ui| {
        ui.label("Vectorscope");
        ui.add(egui::Slider::new(&mut s.vectorscope_brightness, 0.0..=1.0).show_value(false));
    });
    ui.horizontal(|ui| {
        ui.label("Graticule");
        ui.add(egui::Slider::new(&mut s.vectorscope_graticule_brightness, 0.0..=1.0).show_value(false));
    });

    ui.separator();

    // Range inputs
    ui.horizontal(|ui| {
        ui.label("Low Range");
        ui.add(egui::DragValue::new(&mut s.low_range).range(0.0..=1.0).speed(0.01).fixed_decimals(2));
    });
    ui.horizontal(|ui| {
        ui.label("High Range");
        ui.add(egui::DragValue::new(&mut s.high_range).range(0.0..=1.0).speed(0.01).fixed_decimals(2));
    });

    ui.separator();

    ui.label(egui::RichText::new("ADDITIONAL GAMUT").size(9.0)
        .color(egui::Color32::from_rgb(0x80, 0x80, 0x88)).strong());
    ui.checkbox(&mut s.show_2x_zoom, "Show 2x Zoom");
    ui.checkbox(&mut s.show_skin_tone_indicator, "Show Skin Tone Indicator");
    ui.checkbox(&mut s.show_iq_lines, "Show I/Q Lines")
        .on_hover_text("Draw NTSC I and Q color axes as reference lines");
    ui.checkbox(&mut s.show_graticule, "Show Graticule");
}

fn draw_parade_settings(ui: &mut egui::Ui, pane_idx: usize, state: &mut ScopeWindowState) {
    let s = &mut state.panes[pane_idx].settings;

    ui.label(egui::RichText::new("DISPLAY").size(9.0)
        .color(egui::Color32::from_rgb(0x80, 0x80, 0x88)).strong());
    ui.checkbox(&mut s.low_pass, "Low Pass Filter");
    ui.checkbox(&mut s.bloom, "Bloom on dense regions");
    ui.checkbox(&mut s.overlay_on_viewer, "Overlay on viewer");

    ui.separator();

    ui.label(egui::RichText::new("QUALITY").size(9.0)
        .color(egui::Color32::from_rgb(0x80, 0x80, 0x88)).strong());
    ui.horizontal(|ui| {
        ui.radio_value(&mut s.quality, klipp_core::ScopeQuality::Low, "Draft");
        ui.radio_value(&mut s.quality, klipp_core::ScopeQuality::Medium, "Standard");
        ui.radio_value(&mut s.quality, klipp_core::ScopeQuality::High, "Full");
    });

    ui.separator();

    ui.label(egui::RichText::new("SCALE").size(9.0)
        .color(egui::Color32::from_rgb(0x80, 0x80, 0x88)).strong());
    ui.radio_value(&mut s.scale, ScopeScale::TenBit, "0 \u{2013} 1023 (10-bit)");
    ui.radio_value(&mut s.scale, ScopeScale::Ire, "0 \u{2013} 100 IRE");
    ui.radio_value(&mut s.scale, ScopeScale::Percent, "0 \u{2013} 100%");
}

fn draw_histogram_settings(ui: &mut egui::Ui, pane_idx: usize, state: &mut ScopeWindowState) {
    let s = &mut state.panes[pane_idx].settings;

    ui.label(egui::RichText::new("DISPLAY").size(9.0)
        .color(egui::Color32::from_rgb(0x80, 0x80, 0x88)).strong());
    ui.checkbox(&mut s.low_pass, "Low Pass Filter");
    ui.checkbox(&mut s.bloom, "Bloom on dense regions");

    ui.separator();

    ui.label(egui::RichText::new("SCALE").size(9.0)
        .color(egui::Color32::from_rgb(0x80, 0x80, 0x88)).strong());
    ui.radio_value(&mut s.histogram_scale, HistogramScale::Sqrt, "\u{221A} Sqrt (default)");
    ui.radio_value(&mut s.histogram_scale, HistogramScale::Linear, "Linear");
    ui.radio_value(&mut s.histogram_scale, HistogramScale::Log, "Log");
}

// ── ··· Menu ─────────────────────────────────────────────────────────────

fn draw_dot_menu(
    ui: &mut egui::Ui,
    state: &mut ScopeWindowState,
    runtime: &mut ScopeWindowRuntime,
) {
    let menu_id = egui::Id::new("scope_window_dot_menu");
    egui::Area::new(menu_id)
        .order(egui::Order::Foreground)
        .fixed_pos(egui::pos2(
            ui.max_rect().max.x - 210.0,
            ui.min_rect().min.y + 30.0,
        ))
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(190.0);
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 3.0);

                // Waveform Scale Style submenu
                ui.menu_button("Waveform Scale Style", |ui| {
                    let scales = [
                        (WaveformScaleStyle::TenBit, "0 \u{2013} 1023 (10-bit)"),
                        (WaveformScaleStyle::Ire, "0 \u{2013} 100 IRE"),
                        (WaveformScaleStyle::Percent, "0 \u{2013} 100%"),
                    ];
                    for (val, label) in &scales {
                        let prefix = if state.waveform_scale == *val { "\u{2022} " } else { "   " };
                        if ui.button(format!("{prefix}{label}")).clicked() {
                            state.waveform_scale = *val;
                            ui.close_menu();
                        }
                    }
                    ui.separator();
                    let legal_prefix = if false { "\u{2022} " } else { "   " };
                    let _ = ui.button(format!("{legal_prefix}Show legal range (64\u{2013}940)"));
                });

                // Vectorscope Scale Style submenu
                ui.menu_button("Vectorscope Scale Style", |ui| {
                    for val in [VectorscopeScaleStyle::SeventyFive, VectorscopeScaleStyle::Hundred] {
                        let prefix = if state.vectorscope_scale == val { "\u{2022} " } else { "   " };
                        if ui.button(format!("{prefix}{}", val.label())).clicked() {
                            state.vectorscope_scale = val;
                            ui.close_menu();
                        }
                    }
                });

                ui.separator();

                // Display Qualifier Focus
                ui.checkbox(&mut state.qualifier_focus, "Display Qualifier Focus");

                // Low Pass Filter
                let mut lp = state.global_low_pass;
                if ui.checkbox(&mut lp, "Low Pass Filter").changed() {
                    state.global_low_pass = lp;
                    // Apply to all panes
                    for pane in &mut state.panes {
                        pane.settings.low_pass = lp;
                    }
                }

                ui.separator();

                // Ratio submenu
                ui.menu_button("Ratio", |ui| {
                    for (label, _) in [("Equal (1:1)", 0), ("Wide (2:1)", 1), ("Tall (1:2)", 2)] {
                        if ui.button(label).clicked() {
                            ui.close_menu();
                        }
                    }
                });

                // Quality submenu
                ui.menu_button("Quality", |ui| {
                    let qualities = [
                        (klipp_core::ScopeQuality::Low, "Draft (every 4px)"),
                        (klipp_core::ScopeQuality::Medium, "Standard (every 2px)"),
                        (klipp_core::ScopeQuality::High, "Full (every px)"),
                    ];
                    for (val, label) in &qualities {
                        let prefix = if state.global_quality == *val { "\u{2022} " } else { "   " };
                        if ui.button(format!("{prefix}{label}")).clicked() {
                            state.global_quality = *val;
                            for pane in &mut state.panes {
                                pane.settings.quality = *val;
                            }
                            ui.close_menu();
                        }
                    }
                });
            });
        });

    // Close on click outside
    if ui.input(|i| i.pointer.primary_clicked()) && !runtime.dot_menu_open {
        runtime.dot_menu_open = false;
    }
}
