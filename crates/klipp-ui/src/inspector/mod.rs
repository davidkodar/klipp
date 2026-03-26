pub mod video_tab;
pub mod audio_tab;
pub mod effects_tab;
pub mod info_tab;

use crate::layout_state::InspectorTab;
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;

#[derive(Default)]
pub struct NewInspectorPanel {
    show_add_builtin: bool,
    show_add_ofx: bool,
    pub tag_input: String,
}


impl NewInspectorPanel {
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        state: &AppState,
        theme: &KlippTheme,
        active_tab: &mut InspectorTab,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();

        // Resolve selected clip
        let clip_id = state.selected_clip;
        let clip_info = clip_id.and_then(|cid| {
            state
                .sequence()
                .tracks
                .iter()
                .enumerate()
                .find_map(|(ti, t)| t.find_clip(cid).map(|c| (ti, c)))
        });
        let media = clip_info
            .and_then(|(_, c)| c.media_id().and_then(|mid| state.media_pool.get(&mid)));

        // ── Header ──
        let header_rect = egui::Rect::from_min_size(
            ui.cursor().min,
            egui::vec2(ui.available_width(), 42.0),
        );
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(header_rect), |ui| {
            ui.painter().rect_filled(header_rect, 0.0, theme.bg_deep);
            ui.painter().hline(
                header_rect.x_range(),
                header_rect.max.y,
                egui::Stroke::new(1.0, theme.border),
            );
            ui.add_space(9.0);
            ui.vertical(|ui| {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new("INSPECTOR")
                        .size(10.0)
                        .color(theme.text_dim),
                );
                let (clip_name, name_color) = if let Some((_, c)) = clip_info {
                    let name = media
                        .map(|m| m.filename.clone())
                        .unwrap_or_else(|| format!("{:?}", c.kind));
                    (name, theme.text_primary)
                } else {
                    ("No selection".into(), theme.text_secondary)
                };
                ui.label(
                    egui::RichText::new(clip_name)
                        .size(13.0)
                        .strong()
                        .color(name_color),
                );
            });
        });
        ui.advance_cursor_after_rect(header_rect);

        // ── Tab bar ──
        let tab_rect = egui::Rect::from_min_size(
            ui.cursor().min,
            egui::vec2(ui.available_width(), 28.0),
        );
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(tab_rect), |ui| {
            ui.painter().rect_filled(tab_rect, 0.0, theme.bg_deep);
            ui.painter().hline(
                tab_rect.x_range(),
                tab_rect.max.y,
                egui::Stroke::new(1.0, theme.border),
            );
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let tabs = [
                    (InspectorTab::Video, "VIDEO"),
                    (InspectorTab::Audio, "AUDIO"),
                    (InspectorTab::Effects, "EFFECTS"),
                    (InspectorTab::Info, "INFO"),
                ];
                let tab_w = ui.available_width() / 4.0;
                for (tab, label) in &tabs {
                    let is_active = *active_tab == *tab;

                    let (tab_rect, resp) = ui.allocate_exact_size(
                        egui::vec2(tab_w, 26.0),
                        egui::Sense::click(),
                    );

                    let text_color = if is_active {
                        theme.text_accent
                    } else if resp.hovered() {
                        theme.text_secondary
                    } else {
                        theme.text_dim
                    };

                    ui.painter().text(
                        tab_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        *label,
                        egui::FontId::proportional(10.0),
                        text_color,
                    );

                    if is_active {
                        ui.painter().hline(
                            tab_rect.x_range(),
                            tab_rect.max.y - 1.0,
                            egui::Stroke::new(2.0, theme.accent),
                        );
                    }
                    if resp.clicked() {
                        *active_tab = *tab;
                    }
                }
            });
        });
        ui.advance_cursor_after_rect(tab_rect);

        // ── Tab content ──
        egui::ScrollArea::vertical().id_salt("inspector_body_scroll").show(ui, |ui| {
            ui.add_space(10.0);
            match active_tab {
                InspectorTab::Video => {
                    if let Some((_, clip)) = clip_info {
                        let clip_time_secs = (state.playhead_smooth_secs - clip.timeline_in.to_secs_f64()).max(0.0);
                        video_tab::show(ui, theme, clip, &mut cmds, clip_time_secs);
                    } else {
                        no_selection(ui, theme);
                    }
                }
                InspectorTab::Audio => {
                    if let Some((track_idx, clip)) = clip_info {
                        let peak = audio_tab::find_clip_peak_levels(clip, track_idx, state);
                        // Get waveform peaks from the media item's audio cache
                        // Convert (min, max) pairs to absolute max for display
                        let media_peaks: Option<Vec<f32>> = clip.media_id()
                            .and_then(|mid| state.media_pool.get(&mid))
                            .and_then(|item| item.audio_cache.as_ref())
                            .map(|cache| cache.peaks.iter().map(|(lo, hi)| lo.abs().max(hi.abs())).collect());
                        let peaks_slice = media_peaks.as_deref();
                        let source_channels = clip.media_id()
                            .and_then(|mid| state.media_pool.get(&mid))
                            .and_then(|item| item.audio_channels);
                        let clip_time_secs = (state.playhead_smooth_secs - clip.timeline_in.to_secs_f64()).max(0.0);
                        audio_tab::show(ui, theme, clip, &mut cmds, peak, peaks_slice, source_channels, clip_time_secs);
                    } else {
                        no_selection(ui, theme);
                    }
                }
                InspectorTab::Effects => {
                    if let Some((_, clip)) = clip_info {
                        let clip_time_secs = (state.playhead_smooth_secs - clip.timeline_in.to_secs_f64()).max(0.0);
                        effects_tab::show(ui, theme, state, clip, &mut cmds, &mut self.show_add_builtin, &mut self.show_add_ofx, clip_time_secs);
                    } else {
                        no_selection(ui, theme);
                    }
                }
                InspectorTab::Info => {
                    if let (Some((track_idx, clip)), Some(media)) = (clip_info, media) {
                        info_tab::show(ui, state, theme, track_idx, clip, media, &mut self.tag_input, &mut cmds);
                    } else {
                        no_selection(ui, theme);
                    }
                }
            }
        });

        cmds
    }
}

fn no_selection(ui: &mut egui::Ui, theme: &KlippTheme) {
    ui.centered_and_justified(|ui| {
        ui.label(
            egui::RichText::new("No selection")
                .size(11.0)
                .color(theme.text_dim),
        );
    });
}
