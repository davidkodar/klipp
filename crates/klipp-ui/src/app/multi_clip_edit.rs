//! One edit made to many selected clips: the others found, snapshotted
//! before, and the change carried over after (#890).

use super::*;

impl KlippApp {
    /// The other selected clips an Inspector change on `clip` goes to: the
    /// rest of the selection, when `clip` is the one the Inspector shows
    /// and one of several selected (#843).
    fn multi_edit_others(&self, clip: klipp_core::ClipId) -> Vec<klipp_core::ClipId> {
        if self.state.selected_clip != Some(clip) || !self.state.selected_clips.contains(&clip) {
            return Vec::new();
        }
        self.state.selected_clips.iter().copied().filter(|&c| c != clip).collect()
    }

    /// The clip the Inspector edits and the `others`, as they are: the
    /// edited one too, since not every setting's handler records its
    /// change, and the step has to give all of them back.
    fn multi_edit_snapshot(&self, clip: klipp_core::ClipId, others: &[klipp_core::ClipId]) -> Vec<klipp_timeline::Clip> {
        std::iter::once(&clip).chain(others).filter_map(|id| self.state.sequence().all_clips().find(|c| c.id == *id).cloned()).collect()
    }

    /// Before `cmd` is handled: note the other clips as they are, and open
    /// the one undo step the change on all of them will be.
    pub(super) fn multi_edit_pre(&mut self, cmd: &AppCommand) -> Option<MultiEditPost> {
        use crate::multi_edit::Phase;
        let (clip, phase) = crate::multi_edit::classify(cmd)?;
        let others = self.multi_edit_others(clip);
        if others.is_empty() {
            return None;
        }
        let mut in_drag = self.multi_edit.as_ref().is_some_and(|m| m.primary == clip);
        // An audio plugin's slider coalesces in its own handler under a
        // history drag key: while that drag is open, a value is a live
        // step of it, not a change of its own (#843 review).
        if let AppCommand::UpdateAudioEffectParam { effect_index, param_name, .. } = cmd {
            let key = format!("audio_chain:{}:{}:{}", clip.0, effect_index, param_name);
            if !in_drag && self.state.active_history().is_dragging(&key) {
                self.multi_edit = Some(MultiEditSession { primary: clip, before: self.multi_edit_snapshot(clip, &others) });
                in_drag = true;
            }
        }
        match phase {
            Phase::Live => Some(MultiEditPost { cmd: cmd.clone(), others, finish: false, opened_batch: false }),
            Phase::Begin => {
                self.multi_edit = Some(MultiEditSession { primary: clip, before: self.multi_edit_snapshot(clip, &others) });
                None
            }
            Phase::Update if in_drag => Some(MultiEditPost { cmd: cmd.clone(), others, finish: false, opened_batch: false }),
            Phase::Abandon => in_drag.then(|| MultiEditPost { cmd: cmd.clone(), others, finish: true, opened_batch: false }),
            Phase::End if !in_drag => None,
            Phase::Update | Phase::End | Phase::Redo => {
                if !in_drag {
                    self.multi_edit = Some(MultiEditSession { primary: clip, before: self.multi_edit_snapshot(clip, &others) });
                }
                // Inside a batch already open (an agent's, a palette
                // action's), the step joins it: opening another would drop
                // what the open one has collected (#843 review).
                let opened_batch = !self.state.active_history().is_batching();
                if opened_batch {
                    self.state.active_history_mut().begin_batch();
                }
                Some(MultiEditPost { cmd: cmd.clone(), others, finish: true, opened_batch })
            }
        }
    }

    /// After `post.cmd` was handled on its clip: the same on the others,
    /// live while dragging, and on the last step recorded with it as one.
    pub(super) fn multi_edit_post(&mut self, post: MultiEditPost) {
        use crate::multi_edit::Phase;
        let Some((primary_id, phase)) = crate::multi_edit::classify(&post.cmd) else { return };
        if phase == Phase::Abandon {
            // Taken back: the others as they were. The edited clip is its
            // own handler's to put back (a drag that added an effect takes
            // it off again, which its snapshot from the drag's start would
            // undo).
            if let Some(session) = self.multi_edit.take() {
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                for before in session.before.into_iter().filter(|c| c.id != primary_id) {
                    if let Some(c) = seq.tracks.iter_mut().find_map(|t| t.find_clip_mut(before.id)) {
                        *c = before;
                    }
                }
                self.sync_sequence_to_engine();
            }
            return;
        }
        if phase == Phase::Redo {
            for other in &post.others {
                if let Some(cmd) = crate::multi_edit::retarget(&post.cmd, *other) {
                    self.send_commands(vec![cmd]);
                }
            }
        } else {
            let primary = self.state.sequence().all_clips().find(|c| c.id == primary_id).cloned();
            if let Some(primary) = primary {
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                for other in &post.others {
                    if let Some(c) = seq.tracks.iter_mut().find_map(|t| t.find_clip_mut(*other)) {
                        crate::multi_edit::copy_to(&post.cmd, &primary, c);
                    }
                }
                self.sync_sequence_to_engine();
            }
        }
        if !post.finish {
            return;
        }
        // One undo step: the clip the Inspector edited, recorded by its own
        // handler, and each other clip whole, before and after.
        if phase != Phase::Redo {
            if let Some(session) = self.multi_edit.as_ref() {
                let edits: Vec<EditCommand> = session
                    .before
                    .iter()
                    .filter_map(|before| {
                        let now = self.state.sequence().all_clips().find(|c| c.id == before.id)?.clone();
                        Some(EditCommand::ReplaceClip { description: "Edit Selection", old: Box::new(before.clone()), new: Box::new(now) })
                    })
                    .collect();
                for e in edits {
                    self.state.push_history(e);
                }
            }
        }
        self.multi_edit = None;
        if post.opened_batch {
            let n = post.others.len() + 1;
            self.state.active_history_mut().end_batch_as(&format!("Edit {n} Clips"));
        }
        self.state.dirty = true;
    }
}

/// An Inspector change being made on several selected clips (#843):
/// the clip it edits, and how it and the others were before it.
pub(super) struct MultiEditSession {
    pub(super) primary: klipp_core::ClipId,
    pub(super) before: Vec<klipp_timeline::Clip>,
}

/// What to do after an Inspector command was handled on its own clip.
pub(super) struct MultiEditPost {
    pub(super) cmd: AppCommand,
    pub(super) others: Vec<klipp_core::ClipId>,
    /// The last step: record it and close the undo step.
    pub(super) finish: bool,
    /// Whether this step opened the undo batch it closes; inside one
    /// already open it joins that instead.
    pub(super) opened_batch: bool,
}
