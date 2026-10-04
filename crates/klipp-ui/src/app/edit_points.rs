//! Edit points trimmed from the keyboard (#913): selected with U (or a
//! click on a cut), their side cycled with U again, moved a frame or ten
//! with [ and ], by a typed +N / -N, or to the playhead with E. Each
//! trim is the document's own trim, so it is checked as a drag is and is
//! one undo step.

use super::*;
use crate::edit_point::{self, EditPoint};

impl KlippApp {
    /// The edit-point commands: `false` for any other.
    pub(super) fn handle_edit_point_commands(&mut self, cmd: &AppCommand) -> bool {
        match cmd {
            AppCommand::SelectNearestEditPoint => {
                match edit_point::nearest(self.state.sequence(), self.state.playhead, |_| true) {
                    Some(edit) => {
                        self.state.selected_edits = vec![edit];
                        self.state.selected_clip = None;
                        self.state.selected_clips.clear();
                    }
                    None => self.state.toast.show("No edit point on an unlocked track"),
                }
                true
            }
            AppCommand::CycleEditSide => {
                for edit in &mut self.state.selected_edits {
                    *edit = edit.cycled();
                }
                true
            }
            AppCommand::TrimEditPoints { frames } => {
                let frames = *frames;
                self.trim_edit_points(|_, _| frames, "Trim Edit");
                true
            }
            AppCommand::ExtendEditPoints => {
                let playhead = self.state.playhead;
                self.trim_edit_points(|seq, edit| edit_point::frames_to(seq, edit, playhead), "Extend Edit");
                true
            }
            _ => false,
        }
    }

    /// Each selected edit point moved by what `frames` says for it, all
    /// in one undo step; refused as a whole, with the reason, when any
    /// trim cannot be made.
    fn trim_edit_points(&mut self, frames: impl Fn(&klipp_timeline::Sequence, &EditPoint) -> i64, label: &str) {
        if self.state.selected_edits.is_empty() {
            self.state.toast.show("Select an edit point first (U, or click a cut)");
            return;
        }
        let ripple = self.state.ripple_enabled;
        let linked = self.state.linked_selection;
        let seq = self.state.sequence();
        let ops: Vec<klipp_document::ops::Op> = self
            .state
            .selected_edits
            .iter()
            .flat_map(|edit| edit_point::trim_ops(seq, edit, frames(seq, edit), ripple, linked))
            .collect();
        if ops.is_empty() {
            return;
        }
        match klipp_document::ops::resolve(seq, &self.state.media_pool, &ops) {
            Ok(plan) => {
                let edits: Vec<EditCommand> = plan.steps.into_iter().map(|s| s.edit).collect();
                self.apply_edits_as_one(&edits, label);
            }
            Err(refusal) => self.state.toast.show(format!("Trim unchanged: {}", refusal.why)),
        }
    }
}
