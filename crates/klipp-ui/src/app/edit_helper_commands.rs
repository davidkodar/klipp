//! The editing helpers (#988): their dialog opened, and the op it asks
//! for run as one undo step.

use super::*;

impl KlippApp {
    /// The editing helper commands: `false` for any other.
    pub(super) fn handle_edit_helper_commands(&mut self, cmd: &AppCommand) -> bool {
        match cmd {
            AppCommand::OpenEditHelper { kind, clip_id } => {
                self.state.edit_helper = Some(crate::edit_helpers::EditHelperDialog::new(*kind, *clip_id));
                true
            }
            AppCommand::RunEditHelper => {
                let Some(dialog) = self.state.edit_helper.take() else { return true };
                let op = dialog.op(self.state.sequence(), &self.state.selected_clips);
                match klipp_document::ops::resolve(self.state.sequence(), &self.state.media_pool, std::slice::from_ref(&op)) {
                    Ok(plan) => {
                        let what = plan.steps.first().map(|s| s.what.clone()).unwrap_or_default();
                        let edits: Vec<EditCommand> = plan.steps.into_iter().map(|s| s.edit).collect();
                        self.apply_edits_as_one(&edits, dialog.title());
                        self.state.toast.show(format!("{}: {what}", dialog.title()));
                    }
                    Err(r) => {
                        self.state.toast.show(format!("{} unchanged: {}. {}", dialog.title(), r.why, r.hint));
                        // Left open, to change what was refused.
                        self.state.edit_helper = Some(dialog);
                    }
                }
                true
            }
            _ => false,
        }
    }
}
