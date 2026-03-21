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
            state.sequence.resolution.width, state.sequence.resolution.height, fps
        );
        status_item(ui, theme, &res_str);

        // Right group
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(10.0);

            // Proxy status
            let proxy_label = if state.use_proxies { "Active" } else { "None" };
            status_label_value(ui, theme, "Proxy:", proxy_label);

            // Audio info
            status_item(ui, theme, "PipeWire \u{00b7} 48kHz");

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
