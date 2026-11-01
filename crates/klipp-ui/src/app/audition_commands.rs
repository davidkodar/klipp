//! The audition commands (#3209): an edit planned as the `alternatives`
//! op and applied as one undo step, or refused with why; the picker
//! opened or closed; the takes played in their slot.
//!
//! A take is previewed by handing the engine the sequence with it in its
//! slot (`SharedSequence::stand_in`), which it draws and plays, picture
//! and sound, as it does any sequence; the document stays the app's, and
//! its history, and what is saved or exported. Anything but the transport
//! and the preview's own commands ends it first, as does a change from
//! the socket; it ends by giving the engine the document back.

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
            AppCommand::PreviewTakes { clip_id, take, all } => {
                self.start_take_preview(*clip_id, *take, *all);
                true
            }
            AppCommand::StepTakePreview { by } => {
                if let Some(p) = self.state.take_preview.as_mut() {
                    p.index = crate::auditions::stepped(p.index, p.order.len(), *by);
                    self.show_previewed_take();
                }
                true
            }
            AppCommand::PickPreviewedTake => {
                let cmds = crate::auditions::pick_previewed(&mut self.state, &self.sequence);
                self.send_commands(cmds);
                true
            }
            AppCommand::EndTakePreview => {
                self.end_take_preview(true);
                true
            }
            _ => false,
        }
    }

    /// The takes of `clip_id` played in its slot, from `take`.
    fn start_take_preview(&mut self, clip_id: klipp_core::ClipId, take: Option<klipp_core::TakeId>, all: bool) {
        let cmds = crate::auditions::start_preview(&mut self.state, &self.sequence, clip_id, take, all);
        self.send_commands(cmds);
    }

    /// The take the preview is on handed to the engine, and played from
    /// the start of its range.
    fn show_previewed_take(&mut self) {
        let cmds = crate::auditions::show_take(&mut self.state, &self.sequence);
        self.send_commands(cmds);
    }

    /// The take preview ended: the engine given the document back, the
    /// playback paused and, with `go_back`, the playhead where it was.
    pub(super) fn end_take_preview(&mut self, go_back: bool) {
        let cmds = crate::auditions::end_preview(&mut self.state, &self.sequence, go_back);
        self.send_commands(cmds);
    }

    /// Once a frame while a take is previewed (`auditions::follow_preview`).
    pub(super) fn follow_take_preview(&mut self) {
        let cmds = crate::auditions::follow_preview(&mut self.state, &self.sequence);
        self.send_commands(cmds);
    }
}
