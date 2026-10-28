//! First-time help each frame (#2933): the Welcome window and the tour,
//! drawn after the layout so the tour's callouts find this frame's
//! controls (`crate::welcome`).

use super::*;

impl KlippApp {
    pub(super) fn show_first_time_help(&mut self, ctx: &egui::Context) {
        if crate::welcome::open_at_startup(&mut self.state, &mut self.prefs.general) {
            let _ = self.prefs.save();
        }
        // Never over the launch screen: Help → Welcome waits for a project.
        if self.state.project_path.is_none() {
            return;
        }
        let actions = crate::welcome::show_welcome(ctx, &mut self.state, &self.key_bindings, self.prefs.general.show_welcome, &self.theme);
        for action in actions {
            match action {
                crate::welcome::WelcomeAction::Keymap(preset) => {
                    self.key_bindings.set_preset(preset);
                    let _ = self.key_bindings.save();
                }
                crate::welcome::WelcomeAction::ShowAtStartup(on) => {
                    self.prefs.general.show_welcome = on;
                    let _ = self.prefs.save();
                }
                crate::welcome::WelcomeAction::SetUpAssistant => self.state.show_assistant_setup = true,
            }
        }
        crate::welcome::show_tour(ctx, &mut self.state, &self.key_bindings, &self.theme);
    }
}
