//! Edit points trimmed from the keyboard (#913): selected with U (or a
//! click on a cut), their side cycled with U again, moved a frame or ten
//! with [ and ], by a typed +N / -N, or to the playhead with E. Each
//! trim is the document's own trim, so it is checked as a drag is and is
//! one undo step.

use super::*;
use crate::edit_point::{self, EditPoint};

/// How far either side of an edit Play Around Edit plays (#914).
const PLAY_AROUND_SECS: f64 = 2.0;

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
            // Dynamic trimming (#914): armed at the cut, the playhead put
            // there for J or L to play from; K takes the edit points to
            // where it stopped, Esc goes back.
            AppCommand::BeginDynamicTrim => {
                let cut = self.state.selected_edits.first().and_then(|e| edit_point::cut(self.state.sequence(), e));
                if let Some(cut) = cut {
                    self.state.dynamic_trim = Some(cut);
                    self.state.playhead = cut;
                    let _ = self.cmd_tx.send(AppCommand::SeekTo(cut));
                }
                true
            }
            AppCommand::CommitDynamicTrim => {
                // The frame on screen when K went down is the one trimmed
                // to; the engine may play one more before it pauses, so the
                // playhead is put back on it.
                if self.state.dynamic_trim.take().is_some() {
                    let playhead = self.state.playhead;
                    self.trim_edit_points(|seq, edit| edit_point::frames_to(seq, edit, playhead), "Dynamic Trim");
                    self.send_commands(vec![AppCommand::SeekTo(playhead)]);
                }
                true
            }
            AppCommand::CancelDynamicTrim => {
                if let Some(cut) = self.state.dynamic_trim.take() {
                    self.state.playhead = cut;
                    self.send_commands(vec![AppCommand::Pause, AppCommand::SetSpeed(1.0), AppCommand::SeekTo(cut)]);
                }
                true
            }
            AppCommand::PlayAroundEdit => {
                let around = self
                    .state
                    .selected_edits
                    .first()
                    .and_then(|e| edit_point::cut(self.state.sequence(), e))
                    .unwrap_or(self.state.playhead);
                let rate = self.state.sequence().fps.rate();
                let secs = around.to_secs_f64();
                let end = self.state.sequence_duration().to_secs_f64();
                let from = klipp_core::RationalTime::from_secs_f64((secs - PLAY_AROUND_SECS).max(0.0), rate);
                let out = klipp_core::RationalTime::from_secs_f64((secs + PLAY_AROUND_SECS).min(end), rate);
                if out > from {
                    self.state.play_to_out = Some(crate::state::PlayToOut { from, out, asked: std::time::Instant::now(), started: false, looping: true });
                    self.state.playhead = from;
                    self.send_commands(vec![AppCommand::SetSpeed(1.0), AppCommand::SeekTo(from), AppCommand::Play]);
                }
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
