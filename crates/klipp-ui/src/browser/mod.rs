pub mod media_tab;
pub mod effects_tab;
pub mod audio_fx_tab;
pub mod titles_tab;
pub mod transitions_tab;

use crate::layout_state::BrowserTab;
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;

pub struct BrowserPanel {
    pub search_query: String,
    // Media tab state (migrated from MediaPanel)
    pub media_sort_by: media_tab::SortBy,
    pub media_sort_ascending: bool,
    pub media_selected_bin: Option<String>,
    // Effects tab state
    pub effects_category: effects_tab::EffectCategory,
}

impl Default for BrowserPanel {
    fn default() -> Self {
        Self {
            search_query: String::new(),
            media_sort_by: media_tab::SortBy::Name,
            media_sort_ascending: true,
            media_selected_bin: None,
            effects_category: effects_tab::EffectCategory::All,
        }
    }
}

impl BrowserPanel {
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        active_tab: &mut BrowserTab,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();

        // ── Tab bar ──
        let tab_rect = ui.available_rect_before_wrap();
        ui.allocate_new_ui(
            egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
                tab_rect.min,
                egui::vec2(tab_rect.width(), 30.0),
            )),
            |ui| {
                ui.painter().rect_filled(
                    ui.max_rect(),
                    0.0,
                    theme.bg_deep,
                );
                ui.painter().hline(
                    ui.max_rect().x_range(),
                    ui.max_rect().max.y,
                    egui::Stroke::new(1.0, theme.border),
                );
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    let tabs = [
                        (BrowserTab::Media, "MEDIA"),
                        (BrowserTab::Effects, "EFFECTS"),
                        (BrowserTab::AudioFx, "AUDIO FX"),
                        (BrowserTab::Titles, "TITLES"),
                        (BrowserTab::Transitions, "TRANSITIONS"),
                    ];
                    for (tab, label) in &tabs {
                        let is_active = *active_tab == *tab;

                        // Allocate tab rect manually to avoid egui button hover styling
                        let text_galley = ui.painter().layout_no_wrap(
                            label.to_string(),
                            egui::FontId::proportional(10.0),
                            theme.text_dim,
                        );
                        let tab_w = text_galley.size().x + 24.0;
                        let (tab_rect, resp) = ui.allocate_exact_size(
                            egui::vec2(tab_w, 28.0),
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
            },
        );

        ui.add_space(30.0); // Skip past tab bar

        // ── Search bar ──
        let search_placeholder = match active_tab {
            BrowserTab::Media => "Search media\u{2026}",
            BrowserTab::Effects => "Search effects\u{2026}",
            BrowserTab::AudioFx => "Search audio effects\u{2026}",
            BrowserTab::Titles => "Search titles\u{2026}",
            BrowserTab::Transitions => "Search transitions\u{2026}",
        };

        // Styled search input: bg_raised, border, accent_dim on focus
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            egui::Frame::none()
                .fill(theme.bg_raised)
                .stroke(egui::Stroke::new(1.0, theme.border))
                .rounding(5.0)
                .inner_margin(egui::Margin::symmetric(9.0, 5.0))
                .show(ui, |ui| {
                    let search = egui::TextEdit::singleline(&mut self.search_query)
                        .hint_text(search_placeholder)
                        .desired_width(ui.available_width() - 4.0)
                        .font(egui::FontId::proportional(12.0))
                        .text_color(theme.text_primary)
                        .frame(false);
                    let resp = ui.add(search);
                    if resp.has_focus() {
                        // Redraw frame border with accent on focus
                        let frame_rect = resp.rect.expand2(egui::vec2(9.0, 5.0));
                        ui.painter().rect_stroke(
                            frame_rect,
                            5.0,
                            egui::Stroke::new(1.0, theme.accent_dim),
                        );
                    }
                });
        });
        ui.add_space(4.0);

        // ── Tab content ──
        match active_tab {
            BrowserTab::Media => {
                let tab_cmds = media_tab::show(
                    ui,
                    state,
                    theme,
                    &self.search_query,
                    &mut self.media_sort_by,
                    &mut self.media_sort_ascending,
                    &mut self.media_selected_bin,
                );
                cmds.extend(tab_cmds);
            }
            BrowserTab::Effects => {
                let tab_cmds = effects_tab::show(
                    ui,
                    state,
                    theme,
                    &self.search_query,
                    &mut self.effects_category,
                );
                cmds.extend(tab_cmds);
            }
            BrowserTab::AudioFx => {
                let tab_cmds = audio_fx_tab::show(ui, state, theme, &self.search_query);
                cmds.extend(tab_cmds);
            }
            BrowserTab::Titles => {
                let tab_cmds = titles_tab::show(ui, state, theme, &self.search_query);
                cmds.extend(tab_cmds);
            }
            BrowserTab::Transitions => {
                let tab_cmds = transitions_tab::show(ui, state, theme, &self.search_query);
                cmds.extend(tab_cmds);
            }
        }

        cmds
    }
}
