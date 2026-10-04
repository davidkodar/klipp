//! Undo, redo and edits applied: the document's view, its effects run, and
//! edits applied forward, inverted or as one step (#890).

use super::*;

impl KlippApp {
    pub(super) fn apply_edits_as_one(&mut self, edits: &[EditCommand], label: &str) {
        if edits.is_empty() {
            return;
        }
        self.state.active_history_mut().begin_batch();
        for e in edits {
            self.apply_edit_forward(e);
            self.state.push_history(e.clone());
        }
        self.state.active_history_mut().end_batch_as(label);
        self.state.dirty = true;
        self.state.media_usage_cache_dirty = true;
        self.sync_sequence_to_engine();
        let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
        tracing::info!("applied \"{label}\": {} edits", edits.len());
    }

    pub(super) fn apply_undo(&mut self) {
        // History is per-sequence — ctrl+z can never reach into another
        // sequence's stack, so there's no cross-sequence navigation to
        // toast about.
        if let Some(entry) = self.state.active_history_mut().undo() {
            self.apply_edit_inverse(&entry.command);
            // And redraw the frame: paused, a sync alone shows the
            // change only when the engine happens to see it (a multicam
            // cut undone kept showing the angle cut to).
            self.sync_and_refresh_frame();
            self.state.dirty = true;
            self.state.media_usage_cache_dirty = true;
        }
    }

    pub(super) fn apply_redo(&mut self) {
        if let Some(entry) = self.state.active_history_mut().redo() {
            self.apply_edit_forward(&entry.command);
            // And redraw the frame: paused, a sync alone shows the
            // change only when the engine happens to see it (a multicam
            // cut undone kept showing the angle cut to).
            self.sync_and_refresh_frame();
            self.state.dirty = true;
            self.state.media_usage_cache_dirty = true;
        }
    }

    /// The document as a command runs against it: the UI's own state,
    /// borrowed field by field.
    fn document_view(&mut self) -> klipp_document::apply::DocumentView<'_> {
        klipp_document::apply::DocumentView {
            sequences: &mut self.state.sequences,
            active: self.state.active_sequence_idx,
            media_pool: &mut self.state.media_pool,
            fps: &mut self.state.fps,
        }
    }

    /// What the document asked for once a command ran, done here where
    /// the engine and the viewer are.
    fn run_document_effects(&mut self, effects: klipp_document::apply::Effects) {
        if effects.sync_sequence {
            self.sync_sequence_to_engine();
        }
        if effects.sync_media_pool {
            self.sync_media_pool_to_engine();
        }
        if effects.notify_engine {
            let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
        }
        if effects.set_playhead {
            let _ = self.cmd_tx.send(AppCommand::SetPlayhead(self.state.playhead));
        }
        for clip_id in effects.rerender_titles {
            self.render_title_to_viewer(clip_id);
        }
    }

    fn apply_edit_inverse(&mut self, cmd: &EditCommand) {
        let effects = self.document_view().apply_inverse(cmd);
        self.run_document_effects(effects);
    }

    fn apply_edit_forward(&mut self, cmd: &EditCommand) {
        let effects = self.document_view().apply_forward(cmd);
        self.run_document_effects(effects);
    }
}
