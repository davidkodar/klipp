use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_engine::PlaybackState;

/// Render the 26px status bar at the bottom of the window.
pub fn show(ui: &mut egui::Ui, state: &AppState, theme: &KlippTheme) {
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 14.0;
        ui.add_space(10.0);

        // Status dot — uses role colors from theme
        let (dot_color, dot_glow) = match state.playback {
            PlaybackState::Playing => (
                theme.role_dialogue, // green
                egui::Color32::from_rgba_premultiplied(
                    theme.role_dialogue.r(), theme.role_dialogue.g(), theme.role_dialogue.b(), 128,
                ),
            ),
            PlaybackState::Idle => (
                theme.role_sfx, // amber
                egui::Color32::from_rgba_premultiplied(
                    theme.role_sfx.r(), theme.role_sfx.g(), theme.role_sfx.b(), 80,
                ),
            ),
            _ => (
                theme.role_dialogue,
                egui::Color32::from_rgba_premultiplied(
                    theme.role_dialogue.r(), theme.role_dialogue.g(), theme.role_dialogue.b(), 128,
                ),
            ),
        };
        let (dot_rect, _) = ui.allocate_exact_size(egui::vec2(5.0, 5.0), egui::Sense::hover());
        ui.painter().circle_filled(dot_rect.center(), 2.5, dot_color);
        ui.painter().circle_filled(dot_rect.center(), 4.0, dot_glow);

        let status_label = match state.playback {
            PlaybackState::Playing => "Playing",
            _ => "Ready",
        };
        status_item(ui, theme, status_label);

        // Project name
        let project_name = state
            .project_path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "Untitled".into());
        status_label_value(ui, theme, "Project:", &project_name);

        // Duration
        let dur = state.sequence_duration();
        let dur_str = dur.to_timecode_string();
        status_label_value(ui, theme, "Duration:", &dur_str);

        // Resolution + FPS
        let fps = state.fps.as_f64() as u32;
        let res_str = format!(
            "{}x{} \u{00b7} {}fps",
            state.sequence().resolution.width, state.sequence().resolution.height, fps
        );
        status_item(ui, theme, &res_str);

        // Project media size
        let total_bytes: u64 = state.media_pool.all().map(|m| m.file_size_bytes).sum();
        if total_bytes > 0 {
            status_label_value(ui, theme, "Media:", &format_size(total_bytes));
        }

        // Edit mode indicator
        {
            let (mode_label, mode_color) = match state.edit_mode {
                klipp_timeline::magnetic::EditMode::Insert => (
                    "INSERT",
                    egui::Color32::from_rgb(232, 160, 32),
                ),
                klipp_timeline::magnetic::EditMode::Overwrite => (
                    "OVERWRITE",
                    egui::Color32::from_rgb(60, 120, 220),
                ),
            };
            let (badge_rect, _) =
                ui.allocate_exact_size(egui::vec2(56.0, 14.0), egui::Sense::hover());
            ui.painter().rect_filled(
                badge_rect,
                3.0,
                mode_color.linear_multiply(0.18),
            );
            ui.painter().rect_stroke(
                badge_rect,
                3.0,
                egui::Stroke::new(1.0, mode_color.linear_multiply(0.4)),
            );
            ui.painter().text(
                badge_rect.center(),
                egui::Align2::CENTER_CENTER,
                mode_label,
                egui::FontId::monospace(8.5),
                mode_color,
            );
        }

        // Link indicator for selected clip
        if let Some(cid) = state.selected_clip {
            let is_linked = state.sequence().tracks.iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == cid)
                .and_then(|c| c.linked_clip_id)
                .is_some();
            let link_label = if is_linked { "\u{1F517}" } else { "\u{26D3}\u{FE0E}" };
            let link_color = if is_linked {
                egui::Color32::from_rgb(100, 200, 120)
            } else {
                egui::Color32::from_rgb(120, 120, 130)
            };
            ui.add_space(6.0);
            ui.colored_label(link_color, egui::RichText::new(link_label).size(11.0));
        }

        // Right group
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(10.0);

            // Proxy status
            let proxy_label = if state.use_proxies { "Active" } else { "None" };
            status_label_value(ui, theme, "Proxy:", proxy_label);

            // Audio backend status
            let (audio_dot_rect, _) = ui.allocate_exact_size(egui::vec2(5.0, 5.0), egui::Sense::hover());
            if state.audio_backend_ok {
                ui.painter().circle_filled(audio_dot_rect.center(), 2.5, egui::Color32::from_rgb(80, 200, 80));
            } else {
                ui.painter().circle_filled(audio_dot_rect.center(), 2.5, egui::Color32::from_rgb(210, 50, 50));
            }
            let sr_khz = state.audio_sample_rate as f32 / 1000.0;
            let sr_label = if sr_khz == sr_khz.floor() {
                format!("{}kHz", sr_khz as u32)
            } else {
                format!("{:.1}kHz", sr_khz)
            };
            let audio_label = if state.audio_backend_ok {
                format!("PipeWire \u{00b7} {sr_label}")
            } else {
                "Audio \u{00b7} No Device".to_string()
            };
            status_item(ui, theme, &audio_label);

            // Filmstrip generation progress
            if state.filmstrip_generating {
                let elapsed = state.filmstrip_generate_start.elapsed().as_secs_f64();
                let current_frames = state.filmstrip_in_progress.iter().next()
                    .and_then(|id| state.thumbnail_cache.filmstrip(id))
                    .map(|f| f.len())
                    .unwrap_or(0);
                let done = state.filmstrip_completed;
                let total = state.filmstrip_total_queued;
                let progress_text = if total > 1 {
                    format!("Filmstrip {}/{} — {} frames ({:.0}s)", done + 1, total, current_frames, elapsed)
                } else {
                    format!("Filmstrip — {} frames ({:.0}s)", current_frames, elapsed)
                };
                ui.label(
                    egui::RichText::new(progress_text)
                        .size(10.0)
                        .monospace()
                        .color(theme.text_dim),
                );
                ui.spinner();
            }

            // Skim pre-render progress (active or recently completed)
            let skim_done_fade = state.skim_transcode_done_at.map(|t| {
                let elapsed = t.elapsed().as_secs_f32();
                (1.0 - elapsed / 3.0).clamp(0.0, 1.0) // fade over 3 seconds
            });
            let show_skim_active = state.skim_transcode_status.is_some();
            let show_skim_done = skim_done_fade.is_some_and(|f| f > 0.0);

            if show_skim_active || show_skim_done {
                let (label_text, progress, alpha) = if show_skim_active {
                    let (ref filename, prog) = state.skim_transcode_status.as_ref().unwrap();
                    let short = if filename.len() > 20 {
                        format!("{}...", &filename[..17])
                    } else {
                        filename.clone()
                    };
                    (format!("Pre-rendering: {short}"), *prog, 1.0_f32)
                } else {
                    let fade = skim_done_fade.unwrap();
                    ("Pre-render complete".to_string(), 1.0, fade)
                };

                let pct = (progress * 100.0) as u32;
                let text_alpha = (alpha * 255.0) as u8;
                let bar_color = if show_skim_active {
                    egui::Color32::from_rgb(0x70, 0xb0, 0x90)
                } else {
                    egui::Color32::from_rgb(0x50, 0xd0, 0x70) // green for done
                };

                ui.label(
                    egui::RichText::new(format!("{pct}%"))
                        .size(10.0)
                        .monospace()
                        .color(theme.text_secondary.linear_multiply(alpha)),
                );

                // Progress bar
                let bar_w = 60.0;
                let bar_h = 3.0;
                let (bar_rect, _) =
                    ui.allocate_exact_size(egui::vec2(bar_w, bar_h), egui::Sense::hover());
                ui.painter().rect_filled(bar_rect, 2.0, theme.bg_raised.linear_multiply(alpha));
                let fill_w = (progress * bar_w).clamp(0.0, bar_w);
                if fill_w > 0.0 {
                    ui.painter().rect_filled(
                        egui::Rect::from_min_size(
                            bar_rect.left_top(),
                            egui::vec2(fill_w, bar_h),
                        ),
                        2.0,
                        bar_color.linear_multiply(alpha),
                    );
                }

                ui.label(
                    egui::RichText::new(&label_text)
                        .size(10.0)
                        .monospace()
                        .color(egui::Color32::from_rgba_premultiplied(
                            theme.text_dim.r(), theme.text_dim.g(), theme.text_dim.b(), text_alpha,
                        )),
                );

                ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
            }

            // Render progress (if active)
            if let Some((label, progress)) = state.render_queue.active_job_progress() {
                let pct = (progress * 100.0) as u32;
                ui.label(
                    egui::RichText::new(format!("{pct}%"))
                        .size(10.0)
                        .monospace()
                        .color(theme.text_secondary),
                );

                // Progress bar
                let bar_w = 70.0;
                let bar_h = 3.0;
                let (bar_rect, _) =
                    ui.allocate_exact_size(egui::vec2(bar_w, bar_h), egui::Sense::hover());
                ui.painter()
                    .rect_filled(bar_rect, 2.0, theme.bg_raised);
                let fill_w = (progress * bar_w).clamp(0.0, bar_w);
                if fill_w > 0.0 {
                    ui.painter().rect_filled(
                        egui::Rect::from_min_size(
                            bar_rect.left_top(),
                            egui::vec2(fill_w, bar_h),
                        ),
                        2.0,
                        theme.accent,
                    );
                }

                status_label_value(ui, theme, "Rendering:", label);
            }
        });
    });
}

fn status_item(ui: &mut egui::Ui, theme: &KlippTheme, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(10.0)
            .monospace()
            .color(theme.text_secondary),
    );
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 * 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

fn status_label_value(ui: &mut egui::Ui, theme: &KlippTheme, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.label(
            egui::RichText::new(label)
                .size(10.0)
                .monospace()
                .color(theme.text_dim),
        );
        ui.label(
            egui::RichText::new(value)
                .size(10.0)
                .monospace()
                .color(theme.text_secondary),
        );
    });
}
