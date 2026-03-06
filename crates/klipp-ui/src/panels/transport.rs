use crate::state::AppState;
use klipp_core::AppCommand;
use klipp_engine::PlaybackState;

pub struct TransportBar;

impl Default for TransportBar {
    fn default() -> Self {
        Self
    }
}

impl TransportBar {
    pub fn show(&mut self, ui: &mut egui::Ui, state: &AppState) -> Vec<AppCommand> {
        let mut cmds = Vec::new();
        let enabled = state.playback != PlaybackState::Idle;
        let fps_val = state.fps.as_f64();

        ui.horizontal_centered(|ui| {
            ui.add_space(16.0);

            // Go to start
            if ui
                .add_enabled(enabled, egui::Button::new("\u{23EE}").min_size(egui::vec2(32.0, 32.0)))
                .on_hover_text("Go to start")
                .clicked()
            {
                cmds.push(AppCommand::SeekTo(klipp_core::Timecode::zero()));
            }

            // -10s
            if ui
                .add_enabled(enabled, egui::Button::new("\u{23EA}").min_size(egui::vec2(32.0, 32.0)))
                .on_hover_text("-10s")
                .clicked()
            {
                cmds.push(AppCommand::SeekRelative(-(fps_val * 10.0) as i64));
            }

            // Step back
            if ui
                .add_enabled(enabled, egui::Button::new("\u{25C2}").min_size(egui::vec2(32.0, 32.0)))
                .on_hover_text("Step back")
                .clicked()
            {
                cmds.push(AppCommand::SeekRelative(-1));
            }

            // Play / Pause
            let play_label = if state.playback == PlaybackState::Playing {
                "\u{23F8}"
            } else {
                "\u{25B6}"
            };
            if ui
                .add_enabled(
                    enabled,
                    egui::Button::new(play_label).min_size(egui::vec2(40.0, 40.0)),
                )
                .on_hover_text("Play/Pause")
                .clicked()
            {
                if state.playback == PlaybackState::Playing {
                    cmds.push(AppCommand::Pause);
                } else {
                    cmds.push(AppCommand::Play);
                }
            }

            // Step forward
            if ui
                .add_enabled(enabled, egui::Button::new("\u{25B8}").min_size(egui::vec2(32.0, 32.0)))
                .on_hover_text("Step forward")
                .clicked()
            {
                cmds.push(AppCommand::SeekRelative(1));
            }

            // +10s
            if ui
                .add_enabled(enabled, egui::Button::new("\u{23E9}").min_size(egui::vec2(32.0, 32.0)))
                .on_hover_text("+10s")
                .clicked()
            {
                cmds.push(AppCommand::SeekRelative((fps_val * 10.0) as i64));
            }

            // Go to end
            if ui
                .add_enabled(enabled, egui::Button::new("\u{23ED}").min_size(egui::vec2(32.0, 32.0)))
                .on_hover_text("Go to end")
                .clicked()
            {
                cmds.push(AppCommand::SeekTo(state.duration));
            }

            // Timecode display
            ui.add_space(24.0);
            ui.label(
                egui::RichText::new(state.playhead.format_hmsf(&state.fps))
                    .monospace()
                    .size(18.0)
                    .color(egui::Color32::WHITE),
            );
        });

        cmds
    }

    pub fn handle_keyboard(
        &self,
        ctx: &egui::Context,
        state: &AppState,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();

        if state.playback == PlaybackState::Idle {
            return cmds;
        }

        // Only handle shortcuts when no text edit has focus
        if ctx.memory(|m| m.focused().is_some()) {
            return cmds;
        }

        ctx.input(|i| {
            if i.key_pressed(egui::Key::Space) {
                if state.playback == PlaybackState::Playing {
                    cmds.push(AppCommand::Pause);
                } else {
                    cmds.push(AppCommand::Play);
                }
            }
            if i.key_pressed(egui::Key::ArrowLeft) {
                cmds.push(AppCommand::SeekRelative(-1));
            }
            if i.key_pressed(egui::Key::ArrowRight) {
                cmds.push(AppCommand::SeekRelative(1));
            }
            if i.key_pressed(egui::Key::Home) {
                cmds.push(AppCommand::SeekTo(klipp_core::Timecode::zero()));
            }
            if i.key_pressed(egui::Key::End) {
                cmds.push(AppCommand::SeekTo(state.duration));
            }
        });

        cmds
    }
}
