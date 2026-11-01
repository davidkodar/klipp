//! The audition commands (#3209): an edit planned as the `alternatives`
//! op and applied as one undo step, or refused with why; the picker
//! opened or closed.

use super::*;

impl KlippApp {
    /// The audition commands: `false` for any other.
    pub(super) fn handle_audition_commands(&mut self, cmd: &AppCommand) -> bool {
        match cmd {
            AppCommand::EditAudition { clip_id, edit } => {
                match crate::auditions::plan(self.state.sequence(), &self.state.media_pool, *clip_id, edit) {
                    Ok((edits, label)) => self.apply_edits_as_one(&edits, &label),
                    Err(why) => self.state.toast.show(why),
                }
                true
            }
            AppCommand::OpenAuditionPicker { clip_id } => {
                self.state.audition_picker = *clip_id;
                true
            }
            _ => false,
        }
    }
}
