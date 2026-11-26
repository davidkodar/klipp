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
        // A title placed in a face Klipp's catalogue has is fetched (#2734),
        // captions placed in a batch too (#3304).
        let mut placed = Vec::new();
        for e in edits {
            placed_titles(e, &mut placed);
        }
        crate::missing_fonts::fetch_for(placed);
        // A template's compositions come in with it, a sequence for each
        // frame shape, and opened no tab each: a brand kit's intro and
        // outro opened six. Double-clicking the clip opens one.
        for e in edits {
            for id in added_sequences(e) {
                self.state.closed_sequence_tabs.insert(id);
            }
        }
        // One step, or the open batch's edits (a socket or agent batch).
        self.state.push_edits_as_one(edits, label);
        self.state.dirty = true;
        self.state.media_usage_cache_dirty = true;
        self.sync_sequence_to_engine();
        let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
        tracing::info!("applied \"{label}\": {} edits", edits.len());
    }

    /// Make `edit` as one step: applied to the document, what it asks of
    /// the app done (the engine synced, a file read again...), recorded,
    /// and the project marked unsaved. The one way a handler changes the
    /// project once it is on it (E1 step 3), in place of changing the
    /// sequence or pool itself and then recording what it did.
    pub(super) fn commit(&mut self, edit: EditCommand) {
        let effects = self.state.commit(edit);
        self.run_document_effects(effects);
    }

    /// An edit worked out by helpers that change the active sequence in
    /// place: `work` makes it there and returns the step it recorded,
    /// then the sequence is put back as it was and the step made through
    /// [`Self::commit`]. What the step records is then what is applied,
    /// now and on redo; a step that recorded less than its helpers did
    /// shows at once, not first on a redo. `None` is no step, and the
    /// sequence is put back all the same.
    pub(super) fn commit_worked_out(&mut self, work: impl FnOnce(&mut Self) -> Option<EditCommand>) {
        let active = self.state.active_sequence_idx;
        let before = self.state.document.sequences[active].clone();
        // A track the helpers made for it goes into the step while it is
        // still there to see; put back first, it would be gone, and what
        // the step put on it with it.
        let edit = work(self).map(|edit| self.state.with_made_tracks(edit));
        debug_assert_eq!(self.state.active_sequence_idx, active, "the edit stays on its sequence");
        self.state.document.sequences[active] = before;
        // The app's rate follows the sequence's: a first clip's format the
        // helpers took is the step's to take again.
        self.state.fps = self.state.document.sequences[active].fps;
        if let Some(edit) = edit {
            self.commit(edit);
        }
    }

    /// Clip `clip_id`'s keys, and those of `partner` with it, changed by
    /// `f` on copies of the two and committed as one keyframe step named
    /// `description`; nothing when the clip is gone or `f` changes no key.
    /// `f` changes keys only: the step puts back a clip's keyed tracks.
    pub(super) fn edit_clip_keys(&mut self, clip_id: klipp_core::ClipId, partner: Option<klipp_core::ClipId>, description: &'static str, f: impl FnOnce(&mut klipp_timeline::Clip, Option<&mut klipp_timeline::Clip>)) {
        if let Some(edit) = keys_edit(self.state.sequence(), clip_id, partner, description, f) {
            self.commit(edit);
        }
    }

    pub(super) fn apply_undo(&mut self) {
        // The active sequence's step or the project's, whichever is the
        // latest (#1986); another sequence's stack is never reached.
        if let Some(entry) = self.state.take_undo() {
            // A run of a generator's changes ends at an undo or redo:
            // the next change is a step of its own.
            self.state.generator_edit = None;
            self.apply_edit_inverse(&entry.command);
            self.state.note_recorded_tracks();
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
            // A run of a generator's changes ends at an undo or redo:
            // the next change is a step of its own.
            self.state.generator_edit = None;
            self.apply_edit_forward(&entry.command);
            self.state.note_recorded_tracks();
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
        self.state.document_view()
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
        // Veylvik compositions an undo or redo put back, before the
        // sequence goes: a clip naming one finds it.
        if !effects.compositions.is_empty() {
            self.veylvik_compositions_put_back(&effects.compositions);
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
            if let Some(path) = self.state.document.media_pool.get(media_id).filter(|m| !m.offline).map(|m| m.path.clone()) {
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

/// The keyframe step `f` makes on copies of clip `clip_id` and of
/// `partner`, named `description`: `None` when the clip is gone or `f`
/// changes no key. Shared by the app's commit and the windowless
/// inspector host's.
pub(super) fn keys_edit(seq: &klipp_timeline::Sequence, clip_id: klipp_core::ClipId, partner: Option<klipp_core::ClipId>, description: &'static str, f: impl FnOnce(&mut klipp_timeline::Clip, Option<&mut klipp_timeline::Clip>)) -> Option<EditCommand> {
    use crate::history::KeyframeSnapshot as Snap;
    let mut clip = seq.all_clips().find(|c| c.id == clip_id).cloned()?;
    let mut other = partner.and_then(|p| seq.all_clips().find(|c| c.id == p).cloned());
    let (old_state, old_linked_state) = (Snap::from_clip(&clip), other.as_ref().map(Snap::from_clip));
    f(&mut clip, other.as_mut());
    let (new_state, new_linked_state) = (Snap::from_clip(&clip), other.as_ref().map(Snap::from_clip));
    if old_state == new_state && old_linked_state == new_linked_state {
        return None;
    }
    let linked_clip_id = other.as_ref().map(|c| c.id);
    Some(EditCommand::EditKeyframes { clip_id, linked_clip_id, description, old_state, new_state, old_linked_state, new_linked_state })
}

/// The titles `e` places, in its batches and named steps too.
fn placed_titles<'a>(e: &'a EditCommand, out: &mut Vec<&'a klipp_timeline::TitleData>) {
    match e {
        EditCommand::AddTitleClip { clip, .. } | EditCommand::LegacyAppendClip { clip, .. } => out.extend(clip.title_data()),
        EditCommand::Batch(edits) => edits.iter().for_each(|e| placed_titles(e, out)),
        EditCommand::Named { edit, .. } => placed_titles(edit, out),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captions come as appended clips in a named batch; their titles are
    /// found there, so a face they name is fetched.
    #[test]
    fn titles_placed_in_a_named_batch_are_found() {
        let title = |t: &str| klipp_timeline::Clip { kind: klipp_timeline::ClipKind::Title(klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: t.into(), ..Default::default() })), ..klipp_timeline::Clip::gap(klipp_core::RationalTime::from_secs_f64(0.0, 24), klipp_core::RationalTime::from_secs_f64(1.0, 24)) };
        let batch = EditCommand::Named { label: "Captions".into(), edit: Box::new(EditCommand::Batch(vec![EditCommand::LegacyAppendClip { clip: title("a"), track_index: 0 }, EditCommand::LegacyAppendClip { clip: title("b"), track_index: 0 }])) };
        let mut found = Vec::new();
        placed_titles(&batch, &mut found);
        assert_eq!(found.iter().filter_map(|t| t.primary_text().map(|b| b.text.as_str())).collect::<Vec<_>>(), ["a", "b"]);
    }
}

/// The sequences `edit` brings into the project: a template's
/// compositions, copied in with it.
fn added_sequences(edit: &EditCommand) -> Vec<klipp_core::SequenceId> {
    match edit {
        EditCommand::AddSequences { sequences } => sequences.iter().map(|s| s.id).collect(),
        EditCommand::Named { edit, .. } => added_sequences(edit),
        EditCommand::Batch(edits) => edits.iter().flat_map(added_sequences).collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod added_sequences_tests {
    use crate::history::EditCommand;
    /// The compositions a template brings, however deep in its step.
    #[test]
    fn a_templates_compositions_are_found_in_its_step() {
        let (a, b) = (klipp_timeline::Sequence::new_default(), klipp_timeline::Sequence::new_default());
        let ids = vec![a.id, b.id];
        let step = EditCommand::Named { label: "Template".into(), edit: Box::new(EditCommand::Batch(vec![EditCommand::AddSequences { sequences: vec![a, b] }])) };
        assert_eq!(super::added_sequences(&step), ids);
        assert!(super::added_sequences(&EditCommand::Batch(Vec::new())).is_empty());
    }
}

