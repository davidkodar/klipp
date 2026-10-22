//! Undo, redo and edits applied: the document's view, its effects run, and
//! edits applied forward, inverted or as one step (#890).

use super::*;

impl KlippApp {
    pub(super) fn apply_edits_as_one(&mut self, edits: &[EditCommand], label: &str) {
        if edits.is_empty() {
            return;
        }
        // An open path drag is its own step, closed before this edit is
        // applied (M3 step 2): the socket's edits reach here without passing
        // the dispatch boundary.
        self.state.close_path_drag_before_edit();
        for e in edits {
            self.apply_edit_forward(e);
        }
        // One step, or the open batch's edits (a socket or agent batch).
        self.state.push_edits_as_one(edits, label);
        self.state.dirty = true;
        self.state.media_usage_cache_dirty = true;
        self.sync_sequence_to_engine();
        let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
        tracing::info!("applied \"{label}\": {} edits", edits.len());
    }

    pub(super) fn apply_undo(&mut self) {
        // The active sequence's step or the project's, whichever is the
        // latest (#1986); another sequence's stack is never reached.
        if let Some(entry) = self.state.take_undo() {
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
        if let Some(entry) = self.state.take_redo() {
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
        // The project's colour set-up an undo or redo put back (#1875),
        // before the sequence goes to the engine: the engine's output LUT
        // with it, sent only when it changed.
        if let Some(colour) = &effects.project_colour {
            let (lut, on) = (self.state.global_lut_path.clone(), self.state.global_lut_enabled);
            crate::panels::colour_management::take_project_colour(&mut self.state, colour);
            if lut != colour.output_lut {
                let _ = self.cmd_tx.send(match &colour.output_lut {
                    Some(path) => AppCommand::LoadGlobalLut { path: path.clone() },
                    None => AppCommand::ClearGlobalLut,
                });
                self.publish_global_lut();
            }
            if on != colour.output_lut_enabled || (lut != colour.output_lut && colour.output_lut.is_some()) {
                let _ = self.cmd_tx.send(AppCommand::SetGlobalLutEnabled(colour.output_lut_enabled));
            }
        }
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
        for &clip_id in &effects.rerender_titles {
            self.render_title_to_viewer(clip_id);
        }
        // After the media sync: the pictures are made for the settings
        // the file has now.
        for media_id in self.state.pictures_to_remake(&effects) {
            self.refresh_media_pictures(media_id);
        }
        // Files read again for what an edit pointed them at (#2327).
        for media_id in &effects.reprobe_media {
            if let Some(path) = self.state.media_pool.get(media_id).filter(|m| !m.offline).map(|m| m.path.clone()) {
                let _ = self.cmd_tx.send(AppCommand::LoadMediaFile { path, media_id: *media_id });
            }
        }
    }

    fn apply_edit_inverse(&mut self, cmd: &EditCommand) {
        let effects = self.document_view().apply_inverse(cmd);
        self.run_document_effects(effects);
    }

    pub(super) fn apply_edit_forward(&mut self, cmd: &EditCommand) {
        let effects = self.document_view().apply_forward(cmd);
        self.run_document_effects(effects);
    }
}
