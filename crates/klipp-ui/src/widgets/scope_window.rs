use crate::state::{
    PaneConfig, PaneScopeSettings, ScopeOptions, ScopeLayout, ScopeScale,
    ScopeType, ScopeWindowState, VectorscopeScaleStyle, WaveformChannel, WaveformPaintMode,
};
use crate::theme::KlippTheme;
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
    /// The frame read in Lab — lightness points, cast, skin — as a line
    /// of text, made at most a few times a second.
    pub colour_read: String,
    colour_gen: u64,
    colour_at: Option<Instant>,
    /// Per pane, the engine scope mode it showed this frame and the
    /// physical size it drew the image at: what the engine is asked to draw.
    pub shown_sizes: [Option<(u8, (u32, u32))>; 9],
    /// The scopes measure PQ light: level graticules are in nits.
    pub hdr_levels: bool,
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
            colour_read: String::new(),
            colour_gen: u64::MAX,
            colour_at: None,
            shown_sizes: [None; 9],
            hdr_levels: false,
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
    basis: klipp_color::scopes::ScopeBasis,
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

    // The picture's own value there, if the engine is publishing samples.
    let linear_working = (!fb.cie_samples.is_empty()).then(|| {
        let sx = ((nx * fb.cie_samples_w as f32) as u32).min(fb.cie_samples_w.saturating_sub(1));
        let sy = ((ny * fb.cie_samples_h as f32) as u32).min(fb.cie_samples_h.saturating_sub(1));
        let i = ((sy * fb.cie_samples_w + sx) * 3) as usize;
        fb.cie_samples
            .get(i..i + 3)
            .map(|s| [s[0], s[1], s[2]])
    })
    .flatten();

    Some(crate::state::ScopeCursorData::from_display(
        nx, ny, sr, sg, sb, basis, linear_working,
    ))
}

/// Show the floating scope window.
#[allow(clippy::too_many_arguments)]
pub fn show_scope_window(
    ctx: &egui::Context,
    state: &mut ScopeWindowState,
    scope_options: &mut ScopeOptions,
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
    theme: &KlippTheme,
    basis: klipp_color::scopes::ScopeBasis,
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

        // The Lab readout, refreshed when the frame has changed and not
        // more than a few times a second: a few milliseconds of a
        // quarter-million samples.
        {
            let fb = frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            let due = runtime.colour_at.is_none_or(|t| t.elapsed().as_millis() >= 400);
            if fb.width > 0 && fb.height > 0 && !fb.data.is_empty() && fb.generation != runtime.colour_gen && due {
                let m = klipp_color::measure::measure_frame(&fb.data, fb.width, fb.height, &[]);
                let cast = if m.cast() < 3.0 { "neutral".to_string() } else { format!("cast {:.0} {}", m.cast(), klipp_color::measure::cast_name(m.cast_a, m.cast_b)) };
                let skin = match m.skin_all {
                    Some(s) => {
                        let mark = if (klipp_color::measure::SKIN_HUE_LOW..=klipp_color::measure::SKIN_HUE_HIGH).contains(&s.hue) { "on band" } else { "OFF band" };
                        format!(" · skin {:.0}° {mark} ({:.0}%)", s.hue, s.pct)
                    }
                    None => String::new(),
                };
                runtime.colour_read = format!("L* {:.0}/{:.0}/{:.0} · {cast}{skin}", m.l_p1, m.l_p50, m.l_p99);
                runtime.colour_gen = fb.generation;
                runtime.colour_at = Some(Instant::now());
            }
        }
        viewer_pointer_pos.and_then(|pos| {
            let image_rect = viewer_image_rect?;
            let fb = frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            let result = sample_viewer_pixel(pos, image_rect, &fb, sample_radius, basis);
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
                .fill(theme.bg_panel_alt)
                .stroke(egui::Stroke::new(1.0_f32, theme.border_light))
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
                            ui.label(egui::RichText::new("Scopes").size(11.0).color(theme.text_secondary));
                            ui.separator();
                            if !runtime.colour_read.is_empty() {
                                ui.label(egui::RichText::new(&runtime.colour_read).size(10.0).monospace().color(theme.text_secondary))
                                    .on_hover_text("The frame read in CIELAB: black, mid and white lightness (L*), the cast on the neutrals, and skin-coloured pixels' hue against the 40–65° band real skin sits in. The same numbers klipp_frame_stats and the check read.");
                                ui.separator();
                            }
                            // Layout buttons
                            for (layout, label) in [(ScopeLayout::OnePane, "1"), (ScopeLayout::TwoPane, "2"),
                                (ScopeLayout::FourPane, "4"), (ScopeLayout::NinePane, "9")] {
                                let active = state.layout == layout;
                                let color = if active { theme.accent } else { theme.text_dim };
                                if ui.add(egui::Button::new(egui::RichText::new(label).size(9.0).color(color))
                                    .fill(if active { theme.accent_subtle } else { egui::Color32::TRANSPARENT })
                                    .min_size(egui::vec2(16.0, 14.0))).clicked() {
                                    state.layout = layout;
                                    state.panes = PaneConfig::default_for_layout(layout);
                                    *runtime = ScopeWindowRuntime::default();
                                }
                            }
                            // Right-aligned: close button
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.add(egui::Button::new(
                                    egui::RichText::new("x").size(11.0).color(theme.text_dim)
                                ).frame(false)).clicked() {
                                    state.open = false;
                                }
                                // ··· menu
                                let dot_color = if runtime.dot_menu_open { theme.accent } else { theme.text_dim };
                                if ui.add(egui::Button::new(
                                    egui::RichText::new("\u{2022}\u{2022}\u{2022}").size(10.0).color(dot_color)
                                ).frame(false)).clicked() {
                                    runtime.dot_menu_open = !runtime.dot_menu_open;
                                }
                            });
                        });

                        // ── Pane grid ──
                        draw_pane_grid(ui, state, scope_options, runtime, frame_buffer, skin_tone_angle, is_playing, scope_cursor, spectrum_bins, correlation_samples, lufs_history, lufs_target, spectrum_history, theme, basis);

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

/// Lay `state.layout`'s grid of panes across the `ui`'s remaining area.
///
/// Shared by the floating scope window and the Colour workspace's
/// docked scopes, so a pane behaves and looks the same wherever it is
/// drawn — same header, same type dropdown, same per-pane settings —
/// and neither can drift from the other.
///
/// The caller owns the `ScopeWindowState` and `ScopeWindowRuntime`, so
/// the dock and the window keep their own pane sets and their own
/// textures: opening the floating window does not disturb the dock.
#[allow(clippy::too_many_arguments)]
pub fn draw_pane_grid(
    ui: &mut egui::Ui,
    state: &mut ScopeWindowState,
    scope_options: &mut ScopeOptions,
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
    theme: &KlippTheme,
    basis: klipp_color::scopes::ScopeBasis,
) {
    // A layout change leaves the pane list the wrong length; refill it
    // before indexing. The floating window does this before its cursor
    // sampling, the dock has no such step, so it belongs here too.
    let needed = state.layout.pane_count();
    if state.panes.len() != needed {
        state.panes = PaneConfig::default_for_layout(state.layout);
    }

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
            // Per-pane border in pure #000000 so it blends into the
            // pane's own true-black interior (matching the scope
            // texture and the main-dock paint path).
            ui.painter().rect_stroke(pane_rect, 0.0,
                egui::Stroke::new(0.5_f32, egui::Color32::BLACK));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(pane_rect));
            draw_pane(&mut child, pane_idx, state, scope_options, runtime, frame_buffer, skin_tone_angle, is_playing, scope_cursor, spectrum_bins, correlation_samples, lufs_history, lufs_target, spectrum_history, theme, basis);
        }
    }
}

// ── Per-pane rendering ───────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn draw_pane(
    ui: &mut egui::Ui,
    pane_idx: usize,
    state: &mut ScopeWindowState,
    scope_options: &mut ScopeOptions,
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
    theme: &KlippTheme,
    basis: klipp_color::scopes::ScopeBasis,
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

    // Scope display surface — themed via `scope_bg` so each theme
    // can pick a backdrop tuned to its trace colours. Dark + Solarized
    // stay near-black; Light theme uses a softer dark grey so the
    // scope panel doesn't punch a stark black hole through the
    // otherwise light UI while still keeping the trace colours
    // (which are designed for dark backgrounds) legible.
    p.rect_filled(content_rect, 0.0, theme.scope_bg);

    // Header background
    p.rect_filled(header_rect, 0.0, theme.bg_deep);

    // Scope type label (clickable for dropdown)
    let type_text = format!("{} \u{25be}", scope_type.label());
    p.text(
        egui::pos2(header_rect.min.x + 6.0, header_rect.center().y),
        egui::Align2::LEFT_CENTER,
        &type_text,
        egui::FontId::proportional(10.0),
        theme.text_secondary,
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
        theme.accent
    } else {
        theme.text_dim
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
                            .color(theme.text_dim).strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add(egui::Button::new(
                                egui::RichText::new("x").size(10.0)
                                    .color(theme.text_secondary)
                            ).frame(false)).clicked() {
                                runtime.settings_pane = None;
                            }
                        });
                    });
                    ui.separator();
                    draw_pane_settings(ui, pane_idx, state, scope_options, theme);
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

    let vectorscope_zoom = scope_options.vectorscope_zoom;
    let histogram_display = scope_options.histogram_display;
    render_pane_scope(ui, pane_idx, state, runtime, frame_buffer, skin_tone_angle, is_playing, content_rect, scope_cursor, spectrum_bins, correlation_samples, lufs_history, lufs_target, spectrum_history, theme, basis, vectorscope_zoom, histogram_display);
}

#[allow(clippy::too_many_arguments)]
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
    theme: &KlippTheme,
    basis: klipp_color::scopes::ScopeBasis,
    vectorscope_zoom: f32,
    histogram_display: klipp_core::HistogramDisplayMode,
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
            .is_some_and(|t| t.elapsed().as_millis() < 33);
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
                    // F-156: dB floor is per-pane so each pane can
                    // pick its own dynamic range.
                    super::scopes::render_audio_spectrogram(
                        spectrum_history,
                        render_w,
                        render_h,
                        pane.settings.spectrogram_db_floor,
                    )
                }
                _ => unreachable!("is_audio_only gates this match"),
            };
            let gen = runtime.frame_gens[pane_idx].wrapping_add(1);
            Some((image, gen))
        }
    } else {
        let fb = frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
        if fb.width == 0 || fb.height == 0 || fb.data.is_empty() {
            None
        } else {
            // A video scope's image is stale when the engine has drawn that
            // scope again, not merely a frame; the chromaticity scope is drawn
            // here from the frame's samples, so it follows the frame.
            let gen = pane
                .scope_type
                .to_scope_mode()
                .and_then(|sm| fb.scope_textures.get(&(super::scopes::mode_index(sm) as u8)))
                .map_or(fb.generation | (1 << 63), |e| e.generation);
            let stale = runtime.textures[pane_idx].is_none()
                || runtime.frame_gens[pane_idx] != gen;

            if stale {
                // Lift the engine-drawn texture from `fb.scope_textures`
                // keyed on the pane's mode. CIE chromaticity is the one
                // scope drawn here, from the engine's samples.
                let scope_texture: Option<egui::ColorImage> = pane.scope_type
                    .to_scope_mode()
                    .and_then(|sm| {
                        let mode_byte = match sm {
                            crate::state::ScopeMode::Waveform => 0u8,
                            crate::state::ScopeMode::Parade => 1,
                            crate::state::ScopeMode::Vectorscope => 2,
                            crate::state::ScopeMode::Histogram => 3,
                        };
                        fb.scope_textures.get(&mode_byte).and_then(|entry| {
                            crate::ui_util::checked_image([entry.w as usize, entry.h as usize], &entry.data, "scope")
                        })
                    });

                if pane.scope_type == ScopeType::FalseColour {
                    // The displayed frame repainted by exposure. This
                    // one *does* read `fb.data`, unlike the CIE plot:
                    // false colour is a statement about the picture as
                    // shown, so the display transform belongs in it.
                    let mut look = klipp_color::measure::LookTarget::preset("broadcast_neutral").unwrap_or_default();
                    if let Some(l) = state.look_skin_l {
                        look.skin_l = Some(l);
                    }
                    let painted = klipp_color::false_colour::repaint(&fb.data, fb.width, fb.height, &look);
                    Some((
                        egui::ColorImage::from_rgba_unmultiplied([fb.width as usize, fb.height as usize], &painted),
                        fb.generation,
                    ))
                } else if let Some(image) = scope_texture {
                    Some((image, gen))
                } else if pane.scope_type == ScopeType::CIEChromaticity
                    && !fb.cie_samples.is_empty()
                {
                    // The engine's linear working-space samples, not
                    // `fb.data`: that has been through the display
                    // transform, so in a wide-gamut project it is the view
                    // after the gamut has been mapped to the screen, and
                    // plotting it would draw the mapping instead of the
                    // picture. Empty until the engine's next tick sees this
                    // pane's mode — the pane keeps its previous image, as
                    // the other modes do.
                    let (w, h) = (fb.cie_samples_w, fb.cie_samples_h);
                    let render_w = (fb.width as usize).min(960);
                    let image = super::scopes::render_cie_chromaticity(
                        &fb.cie_samples,
                        w,
                        h,
                        basis.working_space(),
                        render_w,
                        render_h,
                    );
                    Some((image, gen))
                } else {
                    // The engine has not drawn this mode yet (first frame
                    // on a fresh pane, a mode switch): it catches up on its
                    // next scope tick.
                    None
                }
            } else {
                None
            }
        }
    };

    if let Some((image, gen)) = scope_image {
        runtime.frame_gens[pane_idx] = gen;
        runtime.last_render[pane_idx] = Some(Instant::now());

        let new_size = egui::vec2(image.size[0] as f32, image.size[1] as f32);
        match runtime.textures[pane_idx] {
            Some(ref mut tex) if tex.size_vec2() == new_size => {
                tex.set(image, egui::TextureOptions::LINEAR);
            }
            _ => {
                let name = format!("scope_win_{pane_idx}");
                let h = crate::app::alloc_texture_logged(ui.ctx(), &name, image, egui::TextureOptions::LINEAR);
                runtime.textures[pane_idx] = Some(h);
            }
        }
    }

    // Paint directly into content_rect (already allocated by draw_pane)
    let r = content_rect;
    let painter = ui.painter_at(r);

    // Pure #000000 background for the pane interior — matches the
    // GPU scope texture's own true-black interior so the areas
    // around the texture (vectorscope centred-square margins, axis
    // padding, pane-to-pane gutters) don't read as a grey halo
    // against the trace. Mirrors the same fix used by the main-dock
    // scope path in `show_scopes`.
    painter.rect_filled(r, 0.0, egui::Color32::BLACK);

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
            )
            .shrink(4.0);
            let (img_rect, px) = super::scopes::pixel_rect(ui.ctx(), img_rect);
            if pane.scope_type == ScopeType::Vectorscope {
                runtime.shown_sizes[pane_idx] = Some((2, px));
            }
            let bri = pane.settings.vectorscope_brightness;
            let tint_val = (bri * 2.0).min(1.0);
            let tint = egui::Color32::from_rgb(
                (tint_val * 255.0) as u8, (tint_val * 255.0) as u8, (tint_val * 255.0) as u8,
            );
            painter.image(tex.id(), img_rect, uv, tint);

            // Rich broadcast vectorscope graticule — mirrors the main-
            // dock overlay in `widgets/scopes.rs`: rainbow hue ring at
            // 100%, three inner range rings (25 / 50 / 75 %), centre
            // crosshair, skin-tone I-line, target boxes + labels.
            // Only for proper vectorscope panes; CIE chromaticity still
            // relies on its baked-in colour gamut.
            if pane.scope_type == ScopeType::Vectorscope {
                let label_font = egui::FontId::monospace(8.0);
                let label_color = theme.scope_label;
                let cx = img_rect.center().x;
                let cy = img_rect.center().y;
                // The trace's full scale (±0.5) is the image's half side;
                // zoom magnifies the plane about the centre.
                let rad = side / 2.0 - 4.0;
                let zoom = vectorscope_zoom.max(0.25);
                let graticule = pane.settings.show_graticule;

                // 1. Range rings at 25 / 50 / 75 %.
                let inner_ring_color = egui::Color32::from_rgba_unmultiplied(
                    label_color.r(), label_color.g(), label_color.b(), 40,
                );
                for frac in [0.25_f32, 0.50, 0.75] {
                    if graticule && frac * zoom <= 1.0 {
                        painter.circle_stroke(
                            egui::pos2(cx, cy),
                            rad * frac * zoom,
                            egui::Stroke::new(0.6_f32, inner_ring_color),
                        );
                    }
                }

                // 2. Rainbow hue ring at 100% — 120 tinted segments.
                let segments = if graticule && zoom <= 1.0 { 120 } else { 0 };
                for i in 0..segments {
                    let a0 = (i as f32) / (segments as f32) * std::f32::consts::TAU;
                    let a1 = ((i + 1) as f32) / (segments as f32) * std::f32::consts::TAU;
                    let hue = (a0 / std::f32::consts::TAU).fract();
                    let (rh, gh, bh) = super::scopes::hue_to_rgb(hue);
                    let seg_color = egui::Color32::from_rgba_unmultiplied(
                        (rh * 255.0) as u8,
                        (gh * 255.0) as u8,
                        (bh * 255.0) as u8,
                        140,
                    );
                    let p0 = egui::pos2(cx + a0.cos() * rad, cy + a0.sin() * rad);
                    let p1 = egui::pos2(cx + a1.cos() * rad, cy + a1.sin() * rad);
                    painter.line_segment([p0, p1], egui::Stroke::new(1.4_f32, seg_color));
                }

                // 3. Centre crosshair.
                let cross_color = egui::Color32::from_rgba_unmultiplied(
                    label_color.r(), label_color.g(), label_color.b(), 30,
                );
                painter.line_segment(
                    [egui::pos2(cx - rad, cy), egui::pos2(cx + rad, cy)],
                    egui::Stroke::new(0.5_f32, cross_color),
                );
                painter.line_segment(
                    [egui::pos2(cx, cy - rad), egui::pos2(cx, cy + rad)],
                    egui::Stroke::new(0.5_f32, cross_color),
                );

                // 4. Skin-tone line at the preferred angle + "I" label,
                // and the NTSC I/Q axes when asked for.
                if pane.settings.show_iq_lines {
                    let iq_color = egui::Color32::from_rgba_unmultiplied(
                        label_color.r(), label_color.g(), label_color.b(), 70,
                    );
                    for angle in [123.0_f32, 33.0] {
                        let (dx, dy) = super::scopes::skin_line_direction(angle);
                        painter.line_segment(
                            [egui::pos2(cx - dx * rad, cy - dy * rad), egui::pos2(cx + dx * rad, cy + dy * rad)],
                            egui::Stroke::new(0.6_f32, iq_color),
                        );
                    }
                }
                let (dx, dy) = super::scopes::skin_line_direction(skin_tone_angle);
                let i_line_len = rad * 0.92;
                let i_color = egui::Color32::from_rgb(220, 170, 80);
                painter.line_segment(
                    [
                        egui::pos2(cx, cy),
                        egui::pos2(cx + dx * i_line_len, cy + dy * i_line_len),
                    ],
                    egui::Stroke::new(1.0_f32, i_color),
                );
                painter.text(
                    egui::pos2(cx + dx * (i_line_len + 8.0), cy + dy * (i_line_len + 8.0)),
                    egui::Align2::CENTER_CENTER,
                    "I",
                    label_font.clone(),
                    i_color,
                );

                // 5. Target boxes + labels at the six chroma points.
                let target_color = egui::Color32::from_rgba_unmultiplied(
                    label_color.r(), label_color.g(), label_color.b(), 180,
                );
                let box_half = 4.0;
                for (name, cb, cr) in &klipp_color::scopes::vectorscope_targets(basis) {
                    if !graticule || (cb * zoom).abs() > 0.5 || (cr * zoom).abs() > 0.5 {
                        continue;
                    }
                    let tx = cx + cb * rad * 2.0 * zoom;
                    let ty = cy - cr * rad * 2.0 * zoom;
                    let rect = egui::Rect::from_center_size(
                        egui::pos2(tx, ty),
                        egui::vec2(box_half * 2.0, box_half * 2.0),
                    );
                    painter.rect_stroke(
                        rect,
                        0.0,
                        egui::Stroke::new(1.0_f32, target_color),
                    );
                    let mag = (cb * cb + cr * cr).sqrt().max(0.001);
                    let ux = cb / mag;
                    let uy = -cr / mag;
                    let label_offset = 10.0;
                    painter.text(
                        egui::pos2(tx + ux * label_offset, ty + uy * label_offset),
                        egui::Align2::CENTER_CENTER,
                        *name,
                        label_font.clone(),
                        label_color,
                    );
                }
            } else {
                // CIE chromaticity — preserve the legacy ring overlay
                // (no I-line / target diamonds; the gamut shape itself
                // is baked into the source texture).
                let rings = crate::state::compute_saturation_rings(&pane.settings, side);
                let ring_alpha = crate::state::graticule_alpha(pane.settings.vectorscope_graticule_brightness);
                let ring_color = egui::Color32::from_rgba_unmultiplied(
                    theme.accent.r(), theme.accent.g(), theme.accent.b(), (ring_alpha * 80.0) as u8,
                );
                for ring in &rings {
                    painter.circle_stroke(img_rect.center(), ring.radius, egui::Stroke::new(0.5_f32, ring_color));
                }
            }
        } else if pane.scope_type == ScopeType::Histogram {
            let (img_rect, px) = super::scopes::pixel_rect(ui.ctx(), super::scopes::histogram_image_rect(r));
            runtime.shown_sizes[pane_idx] = Some((3, px));
            painter.image(tex.id(), img_rect, uv, egui::Color32::WHITE);
            let scale = super::scopes::graticule_scale(pane.settings.scale, runtime.hdr_levels);
            super::scopes::draw_histogram_scale(&painter, img_rect, scale, state.show_legal_range, histogram_display, theme);
        } else if pane.scope_type == ScopeType::Spectrogram {
            // F-157: dedicated spectrogram branch. Fills the full
            // content rect (no Waveform-style AXIS_W margin) and
            // overlays time-axis labels + dB-floor reference along
            // the bottom edge. Previously fell through to the
            // Waveform/Parade paint with scale overlays that
            // don't apply to a frequency-vs-time plot.
            painter.image(tex.id(), r, uv, egui::Color32::WHITE);

            // Time axis — approximate, since history capacity (300
            // slices) combined with ~33 ms refresh cadence yields
            // ~10 s of rolling window. Five tick labels along the
            // bottom: -10s, -8s, -5s, -2s, now.
            let label_font = egui::FontId::monospace(9.0);
            let label_color = egui::Color32::from_rgba_unmultiplied(
                theme.accent.r(), theme.accent.g(), theme.accent.b(), 200,
            );
            let shadow_color = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 180);
            let bottom_pad = 2.0;
            let ticks: &[(f32, &str)] = &[
                (0.00, "-10s"),
                (0.20, "-8s"),
                (0.50, "-5s"),
                (0.80, "-2s"),
                (1.00, "now"),
            ];
            for (frac, label) in ticks {
                let x = r.min.x + frac * r.width();
                let y = r.max.y - bottom_pad - 1.0;
                let align = if *frac < 0.01 {
                    egui::Align2::LEFT_BOTTOM
                } else if *frac > 0.99 {
                    egui::Align2::RIGHT_BOTTOM
                } else {
                    egui::Align2::CENTER_BOTTOM
                };
                // Drop-shadow for legibility against the bright
                // high-magnitude regions.
                painter.text(
                    egui::pos2(x + 1.0, y + 1.0),
                    align,
                    *label,
                    label_font.clone(),
                    shadow_color,
                );
                painter.text(egui::pos2(x, y), align, *label, label_font.clone(), label_color);
            }

            // dB-floor reference in top-right — "-72 … 0 dBFS" so
            // users know what the colour gradient maps to.
            let floor = pane.settings.spectrogram_db_floor;
            let db_label = format!("{floor:.0} … 0 dBFS");
            let tr = egui::pos2(r.max.x - 4.0, r.min.y + 2.0);
            painter.text(
                egui::pos2(tr.x + 1.0, tr.y + 1.0),
                egui::Align2::RIGHT_TOP,
                &db_label,
                label_font.clone(),
                shadow_color,
            );
            painter.text(
                tr,
                egui::Align2::RIGHT_TOP,
                &db_label,
                label_font,
                label_color,
            );
        } else {
            // Waveform/Parade: scope image in trace area + scale overlay
            let scale = super::scopes::graticule_scale(pane.settings.scale, runtime.hdr_levels);
            let (img_rect, px) = super::scopes::pixel_rect(ui.ctx(), egui::Rect::from_min_max(
                egui::pos2(r.min.x + super::scopes::AXIS_W, r.min.y + super::scopes::PAD_TOP),
                egui::pos2(r.max.x - super::scopes::PAD_RIGHT, r.max.y - super::scopes::PAD_BOTTOM),
            ));
            match pane.scope_type {
                ScopeType::Waveform => runtime.shown_sizes[pane_idx] = Some((0, px)),
                ScopeType::Parade => runtime.shown_sizes[pane_idx] = Some((1, px)),
                _ => {}
            }
            let bri = pane.settings.waveform_brightness;
            let tint_val = (bri * 2.0).min(1.0).clamp(0.0, 1.0);
            let g = (tint_val * 255.0) as u8;
            // Per-pane Y / R / G / B channel filter — applied as an egui
            // image tint. The waveform texture paints each channel
            // in its own colour (R = red, G = green, B = blue), so
            // multiplying the texture by a channel-coloured tint keeps
            // only that channel's trace and masks the other two to zero.
            // Y maps to a white tint (no channel mask — all traces shown,
            // approximating combined luma without a separate luma render).
            let (tr, tg, tb) = match pane.settings.waveform_channel {
                crate::state::WaveformChannel::Y => (g, g, g),
                crate::state::WaveformChannel::R => (g, 0, 0),
                crate::state::WaveformChannel::G => (0, g, 0),
                crate::state::WaveformChannel::B => (0, 0, g),
            };
            let tint = egui::Color32::from_rgb(tr, tg, tb);
            painter.image(tex.id(), img_rect, uv, tint);

            let is_parade = pane.scope_type == ScopeType::Parade;
            super::scopes::draw_scale(ui, r, scale, state.show_legal_range, is_parade, 3, theme);

            // Reference level lines (amber dashed)
            if pane.settings.show_reference_levels {
                let lines = crate::state::compute_reference_lines(&pane.settings, r.height());
                let amber_ref = egui::Color32::from_rgba_unmultiplied(
                    theme.accent.r(), theme.accent.g(), theme.accent.b(), 140,
                );
                for line in &lines {
                    let y = r.min.y + line.y;
                    let x0 = r.min.x + super::scopes::AXIS_W;
                    let x1 = r.max.x - super::scopes::PAD_RIGHT;
                    let mut x = x0;
                    while x < x1 {
                        painter.line_segment(
                            [egui::pos2(x, y), egui::pos2((x + 6.0).min(x1), y)],
                            egui::Stroke::new(1.0_f32, amber_ref),
                        );
                        x += 10.0;
                    }
                }
            }
        }

        // ── Scope cursor indicator ──
        if pane.settings.show_scope_cursor {
            if let Some(cur) = scope_cursor {
                draw_scope_cursor_indicator(&painter, pane.scope_type, &cur, r, basis, vectorscope_zoom, histogram_display);
            }
        }

        // Continuous repaint only while playback is producing new
        // frames. Paused/idle scopes update on grade/param changes via
        // SequenceUpdated — no need for a 60 fps idle loop burning CPU.
        if is_playing {
            ui.ctx().request_repaint();
        }
    }
}

/// Draw a small circle/crosshair on the scope showing where the hovered pixel maps.
pub fn draw_scope_cursor_indicator(
    painter: &egui::Painter,
    scope_type: ScopeType,
    cur: &crate::state::ScopeCursorData,
    rect: egui::Rect,
    basis: klipp_color::scopes::ScopeBasis,
    vectorscope_zoom: f32,
    histogram_display: klipp_core::HistogramDisplayMode,
) {
    let cursor_color = egui::Color32::from_rgb(0xff, 0xff, 0xff);
    let outline_color = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 200);
    let amber = egui::Color32::from_rgb(0xf5, 0xb4, 0x3c);
    let radius = 5.0;

    // Helper: draw a visible indicator dot at a position
    let draw_dot = |p: &egui::Painter, pos: egui::Pos2| {
        p.circle_filled(pos, 2.0, amber);
        p.circle_stroke(pos, radius, egui::Stroke::new(1.5_f32, outline_color));
        p.circle_stroke(pos, radius, egui::Stroke::new(1.0_f32, cursor_color));
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
            let r_radius = side / 2.0 - 4.0;
            let x = cx + cur.cb * r_radius * 2.0 * vectorscope_zoom;
            let y = cy - cur.cr * r_radius * 2.0 * vectorscope_zoom;
            draw_dot(painter, egui::pos2(x, y));
            // Crosshair
            let ch_color = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 120);
            painter.line_segment([egui::pos2(x - 10.0, y), egui::pos2(x + 10.0, y)], egui::Stroke::new(0.5_f32, ch_color));
            painter.line_segment([egui::pos2(x, y - 10.0), egui::pos2(x, y + 10.0)], egui::Stroke::new(0.5_f32, ch_color));
        }
        ScopeType::Histogram => {
            // A mark at the pointer's level in each panel, one per channel
            // the panel draws, in that channel's colour.
            let img = super::scopes::histogram_image_rect(rect);
            let marks = [
                (1u32, cur.r, egui::Color32::from_rgb(255, 96, 84)),
                (2, cur.g, egui::Color32::from_rgb(96, 255, 112)),
                (4, cur.b, egui::Color32::from_rgb(112, 150, 255)),
                (8, cur.luma, amber),
            ];
            for (panel, channels) in super::scopes::histogram_screen_panels(img, histogram_display, painter.ctx().pixels_per_point()) {
                for (bit, value, color) in marks {
                    if channels & bit == 0 {
                        continue;
                    }
                    let x = panel.min.x + value.clamp(0.0, 1.0) * panel.width();
                    painter.line_segment(
                        [egui::pos2(x, panel.min.y), egui::pos2(x, panel.max.y)],
                        egui::Stroke::new(1.0_f32, color),
                    );
                }
            }
        }
        // The cursor has nothing to point at on a false colour: the
        // picture is where it already is, and the reading is its colour.
        ScopeType::FalseColour => {}
        ScopeType::CIEChromaticity => {
            // Read the picture, exactly as the plot does — and draw nothing
            // rather than guess when the samples are not there yet, since a
            // dot in the wrong place is worse than no dot.
            let Some(rgb) = cur.linear_working else { return };
            let to_xyz = klipp_color::primaries::rgb_to_xyz_matrix(
                &klipp_color::primaries::working_primaries(basis.working_space()),
            );
            let xyz = klipp_color::primaries::matrix3_mul_vec(to_xyz, rgb);
            let sum = xyz[0] + xyz[1] + xyz[2];
            if sum > 0.001 {
                let cxv = xyz[0] / sum;
                let cyv = xyz[1] / sum;
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

fn draw_pane_settings(
    ui: &mut egui::Ui,
    pane_idx: usize,
    state: &mut ScopeWindowState,
    scope_options: &mut ScopeOptions,
    theme: &KlippTheme,
) {
    let scope_type = state.panes[pane_idx].scope_type;
    match scope_type {
        ScopeType::Waveform => draw_waveform_settings(ui, pane_idx, state, theme),
        ScopeType::Parade => draw_parade_settings(ui, pane_idx, state, theme),
        ScopeType::Vectorscope => draw_vectorscope_settings(ui, pane_idx, state, theme),
        ScopeType::Histogram => draw_histogram_settings(ui, pane_idx, state, theme),
        ScopeType::CIEChromaticity => {
            ui.label("No settings available.");
        }
        ScopeType::FalseColour => {
            ui.label("No settings available.");
            ui.small("The picture repainted by exposure. Purple is at black and red is at white — both mean detail already gone.");
            ui.small("Pink means skin is exposed as the look asks, taken from the brief's skin level, so it agrees with the skin findings rather than being a second opinion.");
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
            // F-156: configurable dB floor. Lower = more dynamic range
            // visible (deeper noise floor), higher = compresses loud-
            // only view. Range floored at -96 (engine clamp) and
            // -12 (meaningful minimum dynamic range).
            ui.horizontal(|ui| {
                ui.label("dB floor:");
                let mut v = state.panes[pane_idx].settings.spectrogram_db_floor;
                if ui.add(
                    egui::DragValue::new(&mut v)
                        .range(-96.0..=-12.0)
                        .speed(0.5)
                        .suffix(" dBFS")
                        .fixed_decimals(0),
                ).changed() {
                    state.panes[pane_idx].settings.spectrogram_db_floor = v;
                }
            });
            let floor = state.panes[pane_idx].settings.spectrogram_db_floor;
            ui.small("Scrolling FFT waterfall — ~10 s of history.".to_string());
            ui.small("x = time (oldest left), y = freq (log).".to_string());
            ui.small(format!("colour = dB magnitude, {floor:.0} … 0 dBFS."));
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

    // The main dock's scope options, here too so the floating window
    // has full parity. Audio scopes (Spectrum / Correlation /
    // LoudnessHistory / Spectrogram) and CIE chromaticity are not drawn
    // by the engine's scopes, so the block is skipped for them.
    let is_video_scope = matches!(
        state.panes[pane_idx].scope_type,
        ScopeType::Waveform | ScopeType::Parade | ScopeType::Vectorscope | ScopeType::Histogram
    );
    if is_video_scope {
        ui.separator();
        ui.label(egui::RichText::new("FFMPEG OPTIONS").size(9.0).color(theme.text_dim).strong());
        ui.small("Global — also editable from the main-dock gear.");
        ui.add_space(2.0);
        draw_scope_options(ui, scope_options, theme);
    }
}

fn draw_waveform_settings(ui: &mut egui::Ui, pane_idx: usize, state: &mut ScopeWindowState, theme: &KlippTheme) {
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
                    .color(if is_sel { *color } else { theme.text_dim })
            ).min_size(egui::vec2(30.0, 20.0))
             .fill(if is_sel { color.linear_multiply(0.2) } else { theme.bg_panel });
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
        .color(theme.text_dim).strong());
    ui.radio_value(&mut s.waveform_paint_mode, WaveformPaintMode::Green, "Green");
    ui.radio_value(&mut s.waveform_paint_mode, WaveformPaintMode::Yellow, "Yellow");
    ui.radio_value(&mut s.waveform_paint_mode, WaveformPaintMode::White, "White");
    ui.radio_value(&mut s.waveform_paint_mode, WaveformPaintMode::Classic, "Classic (RGB)");

    ui.separator();

    // Brightness sliders
    ui.label(egui::RichText::new("DISPLAY").size(9.0)
        .color(theme.text_dim).strong());
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
        .color(theme.text_dim).strong());
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
            ui.painter().circle_filled(
                egui::pos2(ui.available_rect_before_wrap().max.x - 20.0, ui.available_rect_before_wrap().center().y),
                3.0, theme.accent,
            );
            ui.add(egui::DragValue::new(&mut s.reference_low).range(0.0..=1023.0).speed(1.0));
        });
        ui.horizontal(|ui| {
            ui.label("High");
            ui.painter().circle_filled(
                egui::pos2(ui.available_rect_before_wrap().max.x - 20.0, ui.available_rect_before_wrap().center().y),
                3.0, theme.accent,
            );
            ui.add(egui::DragValue::new(&mut s.reference_high).range(0.0..=1023.0).speed(1.0));
        });
    }
}

fn draw_vectorscope_settings(ui: &mut egui::Ui, pane_idx: usize, state: &mut ScopeWindowState, theme: &KlippTheme) {
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
        .color(theme.text_dim).strong());
    ui.checkbox(&mut s.show_skin_tone_indicator, "Show Skin Tone Indicator");
    ui.checkbox(&mut s.show_iq_lines, "Show I/Q Lines")
        .on_hover_text("Draw NTSC I and Q colour axes as reference lines");
    ui.checkbox(&mut s.show_graticule, "Show Graticule");
}

fn draw_parade_settings(ui: &mut egui::Ui, pane_idx: usize, state: &mut ScopeWindowState, theme: &KlippTheme) {
    let s = &mut state.panes[pane_idx].settings;

    ui.label(egui::RichText::new("DISPLAY").size(9.0)
        .color(theme.text_dim).strong());
    ui.checkbox(&mut s.low_pass, "Low Pass Filter");
    ui.checkbox(&mut s.bloom, "Bloom on dense regions");
    ui.checkbox(&mut s.overlay_on_viewer, "Overlay on viewer");

    ui.separator();

    ui.label(egui::RichText::new("QUALITY").size(9.0)
        .color(theme.text_dim).strong());
    ui.horizontal(|ui| {
        ui.radio_value(&mut s.quality, klipp_core::ScopeQuality::Low, "Draft");
        ui.radio_value(&mut s.quality, klipp_core::ScopeQuality::Medium, "Standard");
        ui.radio_value(&mut s.quality, klipp_core::ScopeQuality::High, "Full");
    });

    ui.separator();

    ui.label(egui::RichText::new("SCALE").size(9.0)
        .color(theme.text_dim).strong());
    ui.radio_value(&mut s.scale, ScopeScale::TenBit, "0 \u{2013} 1023 (10-bit)");
    ui.radio_value(&mut s.scale, ScopeScale::Ire, "0 \u{2013} 100 IRE");
    ui.radio_value(&mut s.scale, ScopeScale::Percent, "0 \u{2013} 100%");
}

fn draw_histogram_settings(ui: &mut egui::Ui, pane_idx: usize, state: &mut ScopeWindowState, theme: &KlippTheme) {
    let s = &mut state.panes[pane_idx].settings;

    // The layout and count scale are global, in the options below; the
    // level units are this pane's.
    ui.label(egui::RichText::new("SCALE").size(9.0)
        .color(theme.text_dim).strong());
    ui.radio_value(&mut s.scale, ScopeScale::TenBit, "0 \u{2013} 1023 (10-bit)");
    ui.radio_value(&mut s.scale, ScopeScale::Ire, "0 \u{2013} 100 IRE");
    ui.radio_value(&mut s.scale, ScopeScale::Percent, "0 \u{2013} 100%");
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

                // The level units of every waveform, parade and histogram
                // pane at once; a pane's own settings can still differ.
                ui.menu_button("Waveform Scale Style", |ui| {
                    let scales = [
                        (ScopeScale::TenBit, "0 \u{2013} 1023 (10-bit)"),
                        (ScopeScale::Ire, "0 \u{2013} 100 IRE"),
                        (ScopeScale::Percent, "0 \u{2013} 100%"),
                    ];
                    for (val, label) in &scales {
                        let prefix = if state.waveform_scale == *val { "\u{2022} " } else { "   " };
                        if ui.button(format!("{prefix}{label}")).clicked() {
                            state.waveform_scale = *val;
                            for pane in &mut state.panes {
                                pane.settings.scale = *val;
                            }
                            ui.close_menu();
                        }
                    }
                    ui.separator();
                    let legal_prefix = if state.show_legal_range { "\u{2022} " } else { "   " };
                    if ui.button(format!("{legal_prefix}Show legal range (64\u{2013}940)")).clicked() {
                        state.show_legal_range = !state.show_legal_range;
                        ui.close_menu();
                    }
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

/// The scope options shared by the main-dock scope-settings popover (in
/// `app.rs`) and the floating scope window's settings popover: both edit
/// the one `ScopeOptions` on `AppState`, so the two agree.
pub fn draw_scope_options(
    ui: &mut egui::Ui,
    state: &mut ScopeOptions,
    theme: &crate::theme::KlippTheme,
) {
    ui.label(egui::RichText::new("WAVEFORM / PARADE").size(9.0).color(theme.text_dim).strong());
    ui.checkbox(&mut state.waveform_luma_only, "Luma (B&W) only")
        .on_hover_text("Draw the waveform and parade as one white Y′ trace instead of red, green and blue.");
    ui.checkbox(&mut state.waveform_envelope, "Envelope")
        .on_hover_text("Draw each column's lowest and highest level as a line, to see clipping and the picture's range at a glance.");
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Intensity").size(9.0).color(theme.text_dim));
        ui.add(
            egui::Slider::new(&mut state.waveform_intensity, 0.0..=1.0)
                .show_value(true)
                .step_by(0.01),
        ).on_hover_text("Trace brightness. 0.5 draws the density as it is; 1 is six times brighter, 0 six times dimmer.");
    });

    ui.add_space(4.0);
    ui.label(egui::RichText::new("VECTORSCOPE").size(9.0).color(theme.text_dim).strong());
    ui.checkbox(&mut state.vectorscope_colour, "Colour")
        .on_hover_text("Tint the trace by the hue at each point. Off draws it mono.");
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Zoom").size(9.0).color(theme.text_dim));
        for (zoom, label) in [(1.0_f32, "1×"), (2.0, "2×"), (4.0, "4×")] {
            ui.selectable_value(&mut state.vectorscope_zoom, zoom, label);
        }
    })
    .response
    .on_hover_text("Magnify the chroma plane about the centre; the graticule follows.");
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Luma low").size(9.0).color(theme.text_dim));
        ui.add(
            egui::Slider::new(&mut state.vectorscope_luma_lo, 0.0..=1.0)
                .show_value(true)
                .step_by(0.01),
        ).on_hover_text("Plot only pixels whose Y′ is at least this: the chroma of the highlights alone.");
    });
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Luma high").size(9.0).color(theme.text_dim));
        ui.add(
            egui::Slider::new(&mut state.vectorscope_luma_hi, 0.0..=1.0)
                .show_value(true)
                .step_by(0.01),
        ).on_hover_text("Plot only pixels whose Y′ is at most this: the chroma of the shadows alone.");
    });

    ui.add_space(4.0);
    ui.label(egui::RichText::new("HISTOGRAM").size(9.0).color(theme.text_dim).strong());
    for (mode, label) in [
        (klipp_core::HistogramDisplayMode::Overlay, "RGB overlay"),
        (klipp_core::HistogramDisplayMode::Parade, "RGB parade"),
        (klipp_core::HistogramDisplayMode::Stack, "RGB stacked"),
        (klipp_core::HistogramDisplayMode::Luma, "Luma"),
    ] {
        ui.radio_value(&mut state.histogram_display, mode, label);
    }
    ui.add_space(2.0);
    ui.radio_value(&mut state.histogram_levels, klipp_core::HistogramLevels::Linear, "Linear count");
    ui.radio_value(&mut state.histogram_levels, klipp_core::HistogramLevels::Logarithmic, "Logarithmic count")
        .on_hover_text("Height by the logarithm of the count, so a small population beside a large one still shows.");
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Fill").size(9.0).color(theme.text_dim));
        ui.add(
            egui::Slider::new(&mut state.histogram_fill, 0.0..=1.0)
                .show_value(true)
                .step_by(0.01),
        ).on_hover_text("Brightness of the area under each curve. The curve itself is always drawn.");
    });
}

/// Show the engine's one low-pass filter and one bloom in every pane and in
/// the ··· menu. Every scope is drawn from one measurement with one glow
/// setting, so a pane cannot hold a value of its own; call before drawing
/// the window.
pub fn show_shared_toggles(state: &mut ScopeWindowState, low_pass: bool, bloom: bool) {
    state.global_low_pass = low_pass;
    for pane in &mut state.panes {
        pane.settings.low_pass = low_pass;
        pane.settings.bloom = bloom;
    }
}

/// After drawing the window: a checkbox that no longer shows the shared
/// value was just clicked, in a pane or in the ··· menu, and what it now
/// shows becomes the shared value.
pub fn take_shared_toggles(state: &ScopeWindowState, low_pass: &mut bool, bloom: &mut bool) {
    if state.global_low_pass != *low_pass {
        *low_pass = state.global_low_pass;
    } else if let Some(pane) = state.panes.iter().find(|p| p.settings.low_pass != *low_pass) {
        *low_pass = pane.settings.low_pass;
    }
    if let Some(pane) = state.panes.iter().find(|p| p.settings.bloom != *bloom) {
        *bloom = pane.settings.bloom;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_floating_pane_shows_and_sets_the_shared_low_pass_and_bloom() {
        let mut state = ScopeWindowState::default();
        assert!(state.panes.len() > 1, "the default layout has several panes");
        let (mut low_pass, mut bloom) = (true, false);

        show_shared_toggles(&mut state, low_pass, bloom);
        assert!(state.global_low_pass);
        assert!(state.panes.iter().all(|p| p.settings.low_pass && !p.settings.bloom), "every pane shows the shared values");

        take_shared_toggles(&state, &mut low_pass, &mut bloom);
        assert!(low_pass && !bloom, "nothing clicked, nothing changes");

        // One pane's two checkboxes clicked.
        state.panes[1].settings.low_pass = false;
        state.panes[1].settings.bloom = true;
        take_shared_toggles(&state, &mut low_pass, &mut bloom);
        assert!(!low_pass && bloom, "a pane's click sets the shared values");

        // The ··· menu's filter, which sets every pane as well.
        show_shared_toggles(&mut state, low_pass, bloom);
        state.global_low_pass = true;
        for pane in &mut state.panes {
            pane.settings.low_pass = true;
        }
        take_shared_toggles(&state, &mut low_pass, &mut bloom);
        assert!(low_pass && bloom, "the menu's click sets the filter and leaves bloom");
    }
}
