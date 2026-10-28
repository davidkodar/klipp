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
            if crate::welcome::apply(action, &mut self.state, &mut self.prefs, &mut self.key_bindings) {
                let _ = self.prefs.save();
            }
        }
        crate::welcome::show_tour(ctx, &mut self.state, &self.key_bindings, &self.theme);
        crate::welcome::show_small_screen_note(ctx, &mut self.state, &self.theme);
    }
}
