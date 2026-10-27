//! Clips: the commands on a clip itself, its basic operations, selection and
//! role, and the rest of its edits (#826).

use klipp_timeline::GradeAccess;
use super::*;

impl KlippApp {
    pub(super) fn handle_clip_commands(&mut self, cmd: &AppCommand, needs_refresh: &mut bool) -> bool {
        if self.handle_clip_basic_ops(cmd, needs_refresh) { return true; }
        if self.handle_clip_selection_role(cmd, needs_refresh) { return true; }
        if self.handle_clip_edit_misc(cmd, needs_refresh) { return true; }

        // Generator parameter editing
        if let AppCommand::UpdateGeneratorData { clip_id, data } = cmd {
            let old = self.snapshot_generator(*clip_id);
            for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                    if let klipp_timeline::ClipKind::Generator(ref mut gd) = clip.kind {
                        *gd = data.clone();
                    }
                }
            }
            if let Some(old_data) = old {
                self.push_generator_undo(*clip_id, old_data);
            }
            self.sync_sequence_to_engine();
            let _ = self.cmd_tx.send(AppCommand::SetPlayhead(self.state.playhead));
            self.state.dirty = true;
            return true;
        }

        false
    }

    /// Each of `clip_ids` given the name and colour `change` makes of its
    /// own (#920), as one undo step named `description`. A clip's linked
    /// partner (its picture or its sound) is named with it, so the two
    /// keep reading as one shot. Clips it leaves as they were are not in
    /// the step, and nothing is pushed when none changes.
    pub(super) fn set_clip_labels(&mut self, clip_ids: &[klipp_core::ClipId], description: &'static str, change: impl Fn(&klipp_timeline::ClipLabel) -> klipp_timeline::ClipLabel) {
        let seq = &mut self.state.sequences[self.state.active_sequence_idx];
        let targets = with_linked_partners(seq, clip_ids);
        let mut changes = Vec::new();
        for clip in seq.all_clips_mut().filter(|c| targets.contains(&c.id)) {
            let old = clip.own_label();
            let new = change(&old);
            if new != old {
                clip.set_own_label(&new);
                changes.push((clip.id, old, new));
            }
        }
        if changes.is_empty() {
            return;
        }
        // Through push_history, so a rename counts toward autosave.
        self.state.push_history(crate::history::EditCommand::SetClipLabels { description, changes });
        self.sync_sequence_to_engine();
        self.state.dirty = true;
    }

    fn handle_clip_basic_ops(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        match cmd {
                // LoadMediaFile, ImportMedia, AppendClip, AppendClipToTrack,
                // ImportMediaLinked — handled by handle_media_commands
                AppCommand::RemoveClip { clip_id } => {
                    self.remove_clip_impl(*clip_id);
                    true
                }
                AppCommand::MoveClip {
                    clip_id,
                    new_timeline_in,
                } => {
                    if self.refused_by_lock(clip_id) { return true; }
                    // Snapshot the link/group info and compute the delta before
                    // we start mutating. The linked partner (set up by
                    // ImportMediaLinked for A/V pairs) must follow the primary
                    // by the same delta so linked clips stay phase-locked.
                    let (group_id, linked_id, delta) = {
                        let clip = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                            .flat_map(|t| t.clips.iter())
                            .find(|c| c.id == *clip_id);
                        match clip {
                            // The Linked switch off moves the clip alone (#916).
                            Some(c) => (c.group_id, c.linked_clip_id.filter(|_| self.state.linked_selection), *new_timeline_in - c.timeline_in),
                            None => (None, None, klipp_core::RationalTime { value: 0, rate: 1 }),
                        }
                    };

                    // Move primary clip and capture its old/new bounds.
                    let mut primary_history: Option<(klipp_core::RationalTime, klipp_core::RationalTime, klipp_core::RationalTime, klipp_core::RationalTime)> = None;
                    if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.iter_mut()
                        .find(|t| t.clips.iter().any(|c| c.id == *clip_id))
                    {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let old_in = clip.timeline_in;
                            let old_out = clip.timeline_out;
                            let dur = clip.duration();
                            clip.timeline_in = *new_timeline_in;
                            clip.timeline_out = *new_timeline_in + dur;
                            primary_history = Some((old_in, old_out, clip.timeline_in, clip.timeline_out));
                        }
                        track.sort_clips();
                    }

                    // Move the linked partner by the same delta — but skip if
                    // it's on a locked track, or if it's already in the same
                    // group (group propagation below would double-move it).
                    let partner_in_same_group = match (linked_id, group_id) {
                        (Some(lid), Some(gid)) => self.state.sequences[self.state.active_sequence_idx]
                            .tracks.iter()
                            .flat_map(|t| t.clips.iter())
                            .find(|c| c.id == lid)
                            .is_some_and(|c| c.group_id == Some(gid)),
                        _ => false,
                    };
                    let mut linked_partner: Option<crate::history::LinkedMovePartner> = None;
                    if let Some(lid) = linked_id {
                        if !partner_in_same_group && !self.clip_track_is_locked(&lid) && delta.value != 0 {
                            if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.iter_mut()
                                .find(|t| t.clips.iter().any(|c| c.id == lid))
                            {
                                if let Some(lclip) = track.find_clip_mut(lid) {
                                    let old_in = lclip.timeline_in;
                                    let old_out = lclip.timeline_out;
                                    lclip.timeline_in += delta;
                                    lclip.timeline_out += delta;
                                    linked_partner = Some(crate::history::LinkedMovePartner {
                                        partner_id: lid,
                                        old_timeline_in: old_in,
                                        old_timeline_out: old_out,
                                        new_timeline_in: lclip.timeline_in,
                                        new_timeline_out: lclip.timeline_out,
                                    });
                                }
                                track.sort_clips();
                            }
                        }
                    }

                    if let Some((old_in, old_out, new_in, new_out)) = primary_history {
                        self.state.push_history(EditCommand::LegacyMoveClip {
                                clip_id: *clip_id,
                                old_timeline_in: old_in,
                                old_timeline_out: old_out,
                                new_timeline_in: new_in,
                                new_timeline_out: new_out,
                                linked_partner,
                            });
                    }

                    // Propagate move to grouped clips (skip primary AND the
                    // already-moved linked partner to avoid double-moving).
                    if let Some(gid) = group_id {
                        if delta.value != 0 {
                            for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                                for clip in &mut track.clips {
                                    if clip.group_id == Some(gid)
                                        && clip.id != *clip_id
                                        && Some(clip.id) != linked_id
                                    {
                                        clip.timeline_in += delta;
                                        clip.timeline_out += delta;
                                    }
                                }
                                track.sort_clips();
                            }
                        }
                    }

                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    true
                }
                AppCommand::MoveClipToTrack { clip_id, new_timeline_in, from_track, to_track } => {
                    if self.refused_by_lock(clip_id) { return true; }
                    if let Some(name) = self.state.sequences[self.state.active_sequence_idx].tracks.get(*to_track).filter(|t| t.locked).map(|t| t.name.clone()) {
                        self.state.toast.show(format!("{name} is locked"));
                        return true;
                    }
                    let from = *from_track;
                    let to = *to_track;
                    let new_in = *new_timeline_in;
                    if let Some(src_track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(from) {
                        if let Some(clip) = src_track.find_clip(*clip_id).cloned() {
                            let old_in = clip.timeline_in;
                            let old_out = clip.timeline_out;
                            let dur = clip.duration();
                            let linked_id = clip.linked_clip_id;
                            let delta = new_in - old_in;
                            let mut moved_clip = clip.clone();
                            moved_clip.timeline_in = new_in;
                            moved_clip.timeline_out = new_in + dur;
                            src_track.remove_clip(*clip_id);

                            if let Some(dst_track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(to) {
                                dst_track.clips.push(moved_clip.clone());
                                dst_track.sort_clips();
                            }

                            // Shift the linked partner on its own track by the
                            // same delta (A/V partners live on different track
                            // kinds, so the partner doesn't cross tracks — only
                            // its timeline bounds move).
                            let mut linked_partner: Option<crate::history::LinkedMovePartner> = None;
                            if let Some(lid) = linked_id {
                                if !self.clip_track_is_locked(&lid) && delta.value != 0 {
                                    if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.iter_mut()
                                        .find(|t| t.clips.iter().any(|c| c.id == lid))
                                    {
                                        if let Some(lclip) = track.find_clip_mut(lid) {
                                            let p_old_in = lclip.timeline_in;
                                            let p_old_out = lclip.timeline_out;
                                            lclip.timeline_in += delta;
                                            lclip.timeline_out += delta;
                                            linked_partner = Some(crate::history::LinkedMovePartner {
                                                partner_id: lid,
                                                old_timeline_in: p_old_in,
                                                old_timeline_out: p_old_out,
                                                new_timeline_in: lclip.timeline_in,
                                                new_timeline_out: lclip.timeline_out,
                                            });
                                        }
                                        track.sort_clips();
                                    }
                                }
                            }

                            self.state.push_history(EditCommand::LegacyMoveClipToTrack {
                                clip: moved_clip,
                                old_timeline_in: old_in,
                                old_timeline_out: old_out,
                                new_timeline_in: new_in,
                                from_track: from,
                                to_track: to,
                                linked_partner,
                            });
                            self.sync_sequence_to_engine();
                            // Force engine to re-evaluate decoders at current playhead
                            let _ = self.cmd_tx.send(AppCommand::SeekTo(self.state.playhead));
                            self.state.dirty = true;
                        }
                    }
                    true
                }
                AppCommand::SwapAdjacentClips { clip_a, clip_b } => {
                    if self.try_swap_adjacent_clips(*clip_a, *clip_b) {
                        self.sync_sequence_to_engine();
                        let _ = self.cmd_tx.send(AppCommand::SeekTo(self.state.playhead));
                        self.state.dirty = true;
                    }
                    true
                }
                AppCommand::ReplaceClipMedia { clip_id, new_media_id } => {
                    if self.try_replace_clip_media(*clip_id, *new_media_id) {
                        self.sync_sequence_to_engine();
                        let _ = self.cmd_tx.send(AppCommand::SeekTo(self.state.playhead));
                        self.state.dirty = true;
                    }
                    true
                }
                AppCommand::TrimClip { .. } => {
                    match crate::clip_edits::trim(self.state.sequence(), &self.state.media_pool, cmd) {
                        Ok(Some(edit)) => self.apply_edits_as_one(&[edit], "Trim Clip"),
                        Ok(None) => {},
                        Err(why) => {
                            self.state.toast.show(format!("Trim unchanged: {why}"));
                            // A live preview may already have reached the
                            // engine; publish the restored document too.
                            self.sync_sequence_to_engine();
                        }
                    }
                    true
                }
                AppCommand::RollTrimClip {
                    clip_a_id,
                    clip_b_id,
                    new_a_source_out,
                    new_a_timeline_out,
                    new_b_source_in,
                    new_b_timeline_in,
                } => {
                    if self.refused_by_lock(clip_a_id) || self.refused_by_lock(clip_b_id) { return true; }

                    // Save old values for undo, then apply
                    let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                    let old_a = seq.tracks.iter().flat_map(|t| t.clips.iter())
                        .find(|c| c.id == *clip_a_id)
                        .map(|c| (c.source_out, c.timeline_out));
                    let old_b = seq.tracks.iter().flat_map(|t| t.clips.iter())
                        .find(|c| c.id == *clip_b_id)
                        .map(|c| (c.source_in, c.timeline_in));

                    if let (Some((old_a_src_out, old_a_tl_out)), Some((old_b_src_in, old_b_tl_in))) = (old_a, old_b) {
                        // The roll and its linked partners, one undo step.
                        let batching = self.state.active_history_mut().is_batching();
                        if !batching {
                            self.state.active_history_mut().begin_batch();
                        }
                        self.state.push_history(EditCommand::LegacyRollTrimClip {
                                clip_a_id: *clip_a_id,
                                clip_b_id: *clip_b_id,
                                old_a_source_out: old_a_src_out,
                                old_a_timeline_out: old_a_tl_out,
                                old_b_source_in: old_b_src_in,
                                old_b_timeline_in: old_b_tl_in,
                                new_a_source_out: *new_a_source_out,
                                new_a_timeline_out: *new_a_timeline_out,
                                new_b_source_in: *new_b_source_in,
                                new_b_timeline_in: *new_b_timeline_in,
                            });

                        // Apply to clip A
                        let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                        for track in &mut seq.tracks {
                            if let Some(c) = track.find_clip_mut(*clip_a_id) {
                                c.source_out = *new_a_source_out;
                                c.timeline_out = *new_a_timeline_out;
                            }
                            if let Some(c) = track.find_clip_mut(*clip_b_id) {
                                c.source_in = *new_b_source_in;
                                c.timeline_in = *new_b_timeline_in;
                            }
                        }

                        // Linked partners follow by the same amounts. They were
                        // given A's and B's own source times, which is wrong
                        // whenever a partner's source starts elsewhere (sound
                        // recorded apart and synced, a slipped partner), and
                        // the undo above never put them back (#911).
                        let a_tl = *new_a_timeline_out - old_a_tl_out;
                        let a_src = *new_a_source_out - old_a_src_out;
                        let b_tl = *new_b_timeline_in - old_b_tl_in;
                        let b_src = *new_b_source_in - old_b_src_in;
                        let linked_a = seq.all_clips().find(|c| c.id == *clip_a_id).and_then(|c| c.linked_clip_id);
                        let linked_b = seq.all_clips().find(|c| c.id == *clip_b_id).and_then(|c| c.linked_clip_id);
                        let mut partner_edits = Vec::new();
                        for track in &mut seq.tracks {
                            for c in track.clips.iter_mut() {
                                let old = c.clone();
                                if Some(c.id) == linked_a && c.id != *clip_b_id {
                                    c.timeline_out += a_tl;
                                    c.source_out += a_src;
                                } else if Some(c.id) == linked_b && c.id != *clip_a_id {
                                    c.timeline_in += b_tl;
                                    c.source_in += b_src;
                                } else {
                                    continue;
                                }
                                partner_edits.push(EditCommand::ReplaceClip { description: "Roll", old: Box::new(old), new: Box::new(c.clone()) });
                            }
                        }
                        // B's picture moved against its start: a multicam
                        // primary's cuts stay on the picture they cut.
                        partner_edits.extend(klipp_document::ops::keep_angle_cuts(seq, *clip_b_id, b_tl.to_secs_f64()));
                        for edit in partner_edits {
                            self.state.push_history(edit);
                        }
                        if !batching {
                            self.state.active_history_mut().end_batch_as("Roll");
                        }
                    }

                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    true
                }
                AppCommand::SlipClip { clip_id, new_source_in: _, new_source_out: _ } => {
                    if self.refused_by_lock(clip_id) { return true; }
                    // Slip was already applied during drag.  Undo entry was pushed by the timeline panel.
                    // Just sync the engine.
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    true
                }
                AppCommand::SlideClip {
                    clip_id,
                    new_timeline_in: _, new_timeline_out: _,
                    left_clip_id: _, new_left_out: _,
                    right_clip_id: _, new_right_in: _,
                } => {
                    if self.refused_by_lock(clip_id) { return true; }
                    // Slide was already applied during drag.  Undo entry was pushed by the timeline panel.
                    // Just sync the engine.
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    true
                }
                // Undo/Redo/RubberBand/SequenceUpdated — handled by handle_playback_commands
                // Export/RenderQueue/Preferences — handled by handle_export_commands
                // SetSpeed — handled by handle_playback_commands (forwards to engine)
                _ => false,
        }
    }

    fn handle_clip_selection_role(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        match cmd {
                AppCommand::CenterTimelineOnClip(clip_id) => {
                    let cid = *clip_id;
                    let seq = &self.state.sequences[self.state.active_sequence_idx];
                    if let Some(c) = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid) {
                        // Phase 4A: scroll the timeline so the clip's
                        // start sits a small constant inset from the
                        // left edge. Proper viewport-aware centring
                        // would need the timeline's draw width (only
                        // known at paint time); the inset is enough
                        // to make the clip immediately visible from
                        // a "jump to source" gesture without needing
                        // a deferred state field.
                        const LEFT_INSET_PX: f32 = 80.0;
                        let zoom = self.state.timeline_zoom.max(1.0);
                        let secs = c.timeline_in.to_secs_f64() as f32;
                        let target = (secs * zoom) - LEFT_INSET_PX;
                        self.state.timeline_scroll_x = target.max(0.0);
                    }
                    true
                }
                AppCommand::SelectClip(id) => {
                    // With the style eyedropper up, a title clicked gives
                    // its look to the one it was picked for, which stays
                    // selected (#1718).
                    if let (Some(target), Some(from)) = (self.state.title_style_pick, *id) {
                        let is_title = self.state.sequence().all_clips().any(|c| c.id == from && c.title_data().is_some());
                        if from != target && is_title {
                            self.state.title_style_pick = None;
                            self.send_commands(vec![AppCommand::PasteTitleStyle { from: Some(from), to: vec![target] }]);
                            return true;
                        }
                    }
                    // A clip selected ends an edit point's (#913).
                    if id.is_some() {
                        self.state.selected_edits.clear();
                        self.state.trim_entry = None;
                    }
                    let newly_selected = self.state.selected_clip != *id;
                    if id.is_some() {
                        self.state.selected_gap = None;
                    }
                    self.state.selected_clip = *id;
                    if let Some(clip_id) = *id {
                        let seq = &self.state.sequences[self.state.active_sequence_idx];
                        let title = seq.tracks.iter()
                            .find_map(|t| t.clips.iter().find(|c| c.id == clip_id))
                            .filter(|c| c.title_data().is_some());
                        if let Some(clip) = title {
                            // The viewer shows a title only where the engine
                            // draws it, at the playhead. Selecting one the
                            // playhead is outside of brings the playhead to
                            // it, so edits to it can be seen. Only on the
                            // selection itself: moving the playhead away
                            // afterwards is left alone.
                            let seek = newly_selected
                                .then(|| title_seek_on_select(clip, self.state.playhead, seq.fps.rate()))
                                .flatten();
                            if let Some(t) = seek {
                                let _ = self.cmd_tx.send(AppCommand::SeekTo(t));
                            }
                            self.render_title_to_viewer(clip_id);
                        } else if newly_selected
                            && self.state.active_workspace == crate::state::Workspace::Color
                        {
                            // On the Colour page the viewer and the scopes
                            // must show the shot whose grade the panel is
                            // editing. Selecting a clip the playhead is not
                            // in brings the playhead to it.
                            let picture = seq.tracks.iter()
                                .filter(|t| matches!(t.kind, klipp_timeline::TrackKind::Video))
                                .find_map(|t| t.clips.iter().find(|c| c.id == clip_id));
                            let seek = picture.and_then(|clip| {
                                colour_seek_on_select(clip, self.state.playhead, self.graded_positions.get(&clip_id).copied())
                            });
                            if let Some(t) = seek {
                                self.state.playhead = t;
                                self.state.playhead_smooth_secs = t.to_secs_f64();
                                self.state.playhead_anchor = t;
                                self.state.playhead_anchor_wall = std::time::Instant::now();
                                // The follow below must not re-pick the
                                // clip from this position: a clip under a
                                // higher track would lose the selection.
                                self.last_graded_playhead = Some(t);
                                let _ = self.cmd_tx.send(AppCommand::SeekTo(t));
                            }
                        }
                    } else {
                        // Deselecting — clear title overlay if not in edit mode
                        if !self.state.viewer_text_edit {
                            if let Some(old) = self.viewer.title_overlay_texture.take() {
                                self.defer_drop_texture(old);
                            }
                        }
                    }
                    true
                }
                // Node graph, color grading, keyframes, LUT — handled by handle_color_commands
                AppCommand::SetClipDepth { clip_id, depth } => {
                    let depth = depth.clamp(0.0, 20.0);
                    let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                    if let Some((ti, ci)) = seq.find_clip(*clip_id) {
                        let clip = &mut seq.tracks[ti].clips[ci];
                        let old = clip.depth;
                        if (old - depth).abs() > 1e-6 {
                            clip.depth = depth;
                            let cid = *clip_id;
                            let history = self.state.active_history_mut();
                            let merged = history.merge_into_last(|last| match last {
                                crate::history::EditCommand::SetClipDepth { clip_id, new, .. } if *clip_id == cid => {
                                    *new = depth;
                                    true
                                }
                                _ => false,
                            });
                            if !merged {
                                history.push(crate::history::HistoryEntry {
                                    command: crate::history::EditCommand::SetClipDepth { clip_id: cid, old, new: depth },
                                });
                            }
                            self.sync_sequence_to_engine();
                            self.state.dirty = true;
                        }
                    }
                    true
                }
                AppCommand::SetTemplateOverride { clip_id, parameter, value } => {
                    let selected = self.state.sequences[self.state.active_sequence_idx].find_clip(*clip_id)
                        .and_then(|(ti, ci)| match &self.state.sequences[self.state.active_sequence_idx].tracks[ti].clips[ci].kind {
                            klipp_timeline::ClipKind::TemplatePrecomposition { sequence_id, template_instance } => Some((*sequence_id, template_instance.clone())),
                            _ => None,
                        });
                    let Some((sequence_id, old)) = selected else { return true };
                    let Some(template) = self.state.composition_templates.templates().find(|template| template.id == old.template_id) else {
                        self.state.toast.show("This template is not available in the local library");
                        return true;
                    };
                    let Some(parameter_def) = template.parameters.get(parameter) else {
                        self.state.toast.show(format!("Unknown template parameter '{parameter}'"));
                        return true;
                    };
                    if let Err(error) = parameter_def.validate(value) {
                        self.state.toast.show(error);
                        return true;
                    }
                    let mut new = old.clone();
                    new.overrides.insert(parameter.clone(), value.clone());
                    let Some(template_sequence) = self.state.sequences.iter().find(|sequence| sequence.id == sequence_id) else {
                        self.state.toast.show("Template composition is missing");
                        return true;
                    };
                    if let Err(error) = klipp_timeline::flatten::validate_template_instance(&new, template_sequence) {
                        self.state.toast.show(error);
                        return true;
                    }
                    if old != new {
                        if let Some((ti, ci)) = self.state.sequences[self.state.active_sequence_idx].find_clip(*clip_id) {
                            if let klipp_timeline::ClipKind::TemplatePrecomposition { template_instance, .. } = &mut self.state.sequences[self.state.active_sequence_idx].tracks[ti].clips[ci].kind {
                                *template_instance = new.clone();
                            }
                        }
                        let cid = *clip_id;
                        let history = self.state.active_history_mut();
                        let merged = history.merge_into_last(|last| match last {
                            crate::history::EditCommand::SetTemplateInstance { clip_id, parameter: last_parameter, new: last_new, .. } if *clip_id == cid && last_parameter == parameter => {
                                **last_new = new.clone();
                                true
                            }
                            _ => false,
                        });
                        if !merged {
                            history.push(crate::history::HistoryEntry {
                                command: crate::history::EditCommand::SetTemplateInstance { clip_id: cid, parameter: parameter.clone(), old: Box::new(old), new: Box::new(new) },
                            });
                        }
                        self.sync_sequence_to_engine();
                        self.state.dirty = true;
                    }
                    true
                }
                AppCommand::SaveCompositionTemplate { sequence_id, name } => {
                    let Some(sequence) = self.state.sequences.iter().find(|sequence| sequence.id.0 == *sequence_id).cloned() else {
                        self.state.toast.show("The selected composition is missing");
                        return true;
                    };
                    match crate::parameterized_templates::save_composition_as_template(&mut self.state.composition_templates, &sequence, &self.state.sequences, name.clone()) {
                        Ok(true) => self.state.toast.show(format!("Updated the template '{name}'")),
                        Ok(false) => self.state.toast.show(format!("Saved '{name}' as a template")),
                        Err(error) => self.state.toast.show(format!("Could not save template: {error}")),
                    }
                    true
                }
                AppCommand::SetSequenceShutter { sequence_idx, shutter_deg, samples } => {
                    if let Some(seq) = self.state.sequences.get_mut(*sequence_idx) {
                        let old = seq.camera.clone();
                        seq.camera.shutter_deg = shutter_deg.clamp(0.0, 360.0);
                        seq.camera.shutter_samples = (*samples).clamp(2, 32);
                        if seq.camera != old {
                            let new = seq.camera.clone();
                            let id = seq.id;
                            self.state.history_for_mut(id).push(crate::history::HistoryEntry {
                                command: crate::history::EditCommand::SetSequenceCamera { old: Box::new(old), new: Box::new(new) },
                            });
                            self.sync_sequence_to_engine();
                            self.state.dirty = true;
                        }
                    }
                    true
                }
                AppCommand::SetSequenceTitleNits { sequence_idx, nits } => {
                    if let Some(seq) = self.state.sequences.get_mut(*sequence_idx) {
                        let range = klipp_timeline::TITLE_NITS_RANGE;
                        let new = if nits.is_finite() { nits.clamp(*range.start(), *range.end()) } else { klipp_timeline::DEFAULT_TITLE_NITS };
                        let old = seq.title_nits;
                        if new != old {
                            seq.title_nits = new;
                            let id = seq.id;
                            self.state.history_for_mut(id).push(crate::history::HistoryEntry {
                                command: crate::history::EditCommand::SetSequenceTitleNits { old, new },
                            });
                            self.sync_sequence_to_engine();
                            self.state.dirty = true;
                        }
                    }
                    true
                }
                AppCommand::SetSequenceCameraKey { sequence_idx, param, time_secs, value } => {
                    self.key_sequence_camera(*sequence_idx, *param, *time_secs, Some(*value));
                    true
                }
                AppCommand::RemoveSequenceCameraKey { sequence_idx, param, time_secs } => {
                    self.key_sequence_camera(*sequence_idx, *param, *time_secs, None);
                    true
                }
                AppCommand::MoveSequenceCameraKey { sequence_idx, param, index, new_time_secs, new_value } => {
                    self.edit_sequence_camera(*sequence_idx, |camera| { move_camera_key(camera, *param, *index, *new_time_secs, *new_value); });
                    true
                }
                AppCommand::SetSequenceCameraKeyInterpolation { sequence_idx, param, index, interpolation } => {
                    self.edit_sequence_camera(*sequence_idx, |camera| set_camera_key_interpolation(camera, *param, *index, *interpolation));
                    true
                }
                AppCommand::SetSequenceCameraKeyTangent { sequence_idx, param, index, tangent_in, tangent_out } => {
                    self.edit_sequence_camera(*sequence_idx, |camera| set_camera_key_tangent(camera, *param, *index, *tangent_in, *tangent_out));
                    true
                }
                AppCommand::EaseSequenceCameraKey { sequence_idx, param, index, ease } => {
                    self.edit_sequence_camera(*sequence_idx, |camera| klipp_core::keyframe::ease_into(crate::timeline::camera_lane::keys_mut(camera, *param), *index, *ease));
                    true
                }
                AppCommand::BeginSequenceCameraDrag { sequence_idx } => {
                    if let Some(seq) = self.state.sequences.get(*sequence_idx) {
                        let before = Box::new(seq.camera.clone());
                        let id = seq.id;
                        self.state.history_for_mut(id).begin_drag(
                            camera_drag_key(*sequence_idx),
                            crate::history::HistoryEntry {
                                command: crate::history::EditCommand::SetSequenceCamera { old: before.clone(), new: before },
                            },
                        );
                    }
                    true
                }
                AppCommand::EndSequenceCameraDrag { sequence_idx } => {
                    if let Some(seq) = self.state.sequences.get(*sequence_idx) {
                        let now = Box::new(seq.camera.clone());
                        let id = seq.id;
                        let history = self.state.history_for_mut(id);
                        let unchanged = matches!(
                            history.pending_old_entry_ref(),
                            Some(crate::history::HistoryEntry { command: crate::history::EditCommand::SetSequenceCamera { old, .. } }) if *old == now
                        );
                        if unchanged {
                            history.abandon_pending_drag();
                        } else {
                            history.end_drag(crate::history::HistoryEntry {
                                command: crate::history::EditCommand::SetSequenceCamera { old: now.clone(), new: now },
                            });
                        }
                    }
                    true
                }
                AppCommand::RenameClips { clip_ids, name } => {
                    let name = name.as_deref().map(str::trim).filter(|n| !n.is_empty()).map(str::to_string);
                    self.set_clip_labels(clip_ids, "Rename Clip", |label| klipp_timeline::ClipLabel { name: name.clone(), ..label.clone() });
                    true
                }
                AppCommand::SetClipColourLabels { clip_ids, label: colour } => {
                    let colour = if klipp_media::ColorLabel::from_u8(*colour).to_u8() == *colour { *colour } else { 0 };
                    self.set_clip_labels(clip_ids, "Clip Colour", |label| klipp_timeline::ClipLabel { colour, ..label.clone() });
                    true
                }
                AppCommand::SetClipGradeFlags { clip_ids, flag } => {
                    self.set_clip_labels(clip_ids, "Grade Flag", |label| klipp_timeline::ClipLabel { flag: *flag, ..label.clone() });
                    true
                }
                AppCommand::SetClipRole { clip_id, role } => {
                    // One undo step, and a role chosen by hand wins over the
                    // sound's (#2216, #2229).
                    if let Some(edit) = super::audio_roles::hand_role_edit(self.state.sequence(), *clip_id, *role) {
                        self.apply_edits_as_one(&[edit], "Set Clip Role");
                    }
                    true
                }
                // Speed keyframes, audio/video/effect keyframes, keyframe drag,
                // clip audio params, audio drag — handled by handle_keyframe_commands

                // Effect drag, reorder, presets, audio DSP, modifiers — handled by handle_effects_commands

                // Transitions — handled by handle_transition_commands

                // Title clips — handled by handle_title_commands

                // Phase 4: Blade
                AppCommand::BladeClip { track_index, clip_id, cut_at } => {
                    self.blade_clip_at(*track_index, *clip_id, *cut_at);
                    true
                }
                _ => false,
        }
    }

    fn handle_clip_edit_misc(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        match cmd {
                // Phase 4: Cut/Copy/Paste
                AppCommand::CutClip { clip_id } => {
                    // The selection when the clip is part of one, each
                    // clip with its linked partner, onto the clipboard,
                    // then removed as Delete removes them, one undo step
                    // (#458, #845). What a remove refused (a locked track)
                    // stays, and stays off the clipboard: a Cut that cut
                    // nothing was a silent Copy over what it held.
                    let ids = crate::clip_clipboard::selection_for(*clip_id, self.state.selected_clip, &self.state.selected_clips);
                    if let Some(cb) = crate::clip_clipboard::copy(self.state.sequence(), &ids) {
                        let batching = self.state.active_history_mut().is_batching();
                        if !batching {
                            self.state.active_history_mut().begin_batch();
                        }
                        for c in &cb.clips {
                            if self.state.sequence().find_clip(c.clip.id).is_some() {
                                self.remove_clip_impl(c.clip.id);
                            }
                        }
                        if !batching {
                            self.state.active_history_mut().end_batch_as("Cut");
                        }
                        let mut cut = cb;
                        cut.clips.retain(|c| self.state.sequence().find_clip(c.clip.id).is_none());
                        if !cut.clips.is_empty() {
                            if cut.clips.len() > 1 {
                                self.state.toast.show(format!("Cut {} clips", cut.clips.len()));
                            }
                            self.state.system_clipboard_text = Some(clips_clipboard_text(cut.clips.len()));
                            self.state.clipboard = Some(cut);
                        }
                    }
                    true
                }
                AppCommand::CopyClip { clip_id } => {
                    let cid = *clip_id;
                    // Its attributes too: copy a clip, then Paste
                    // Attributes onto others. Paste Attributes stayed
                    // greyed out after Copy, with nothing saying why (#325).
                    let attributes = self.state.sequence().all_clips().find(|c| c.id == cid).map(|c| crate::state::AttributesClipboard::of_in(self.state.sequence(), c));
                    if attributes.is_some() {
                        self.state.attributes_clipboard = attributes;
                    }
                    // The whole selection when the clip is part of one (#845).
                    let ids = crate::clip_clipboard::selection_for(cid, self.state.selected_clip, &self.state.selected_clips);
                    if let Some(cb) = crate::clip_clipboard::copy(self.state.sequence(), &ids) {
                        if cb.clips.len() > 1 {
                            self.state.toast.show(format!("Copied {} clips", cb.clips.len()));
                        }
                        self.state.system_clipboard_text = Some(clips_clipboard_text(cb.clips.len()));
                        self.state.clipboard = Some(cb);
                    }
                    true
                }
                AppCommand::PasteClip => {
                    if let Some(cb) = self.state.clipboard.clone() {
                        self.paste_clipboard(&cb, self.state.playhead, crate::clip_clipboard::PasteMode::Overwrite, "Paste");
                    }
                    true
                }
                // From the selects sequence above the cut (#82): its range
                // copied whole, every track, and put in as one step.
                AppCommand::PasteClipsFrom { from, ids, at, overwrite, track, linked } => {
                    // From the other of two stacked timelines; onto its own
                    // sequence, nothing.
                    if *from == self.state.sequence().id {
                        return true;
                    }
                    let Some(source) = self.state.sequences.iter().find(|s| s.id == *from) else { return true };
                    let Some(cb) = crate::clip_clipboard::copy(source, ids) else {
                        self.state.toast.show("Those clips are gone from the other timeline");
                        return true;
                    };
                    let cb = crate::clip_clipboard::carried(source, self.state.sequence(), cb, ids, *track, *linked);
                    let (mode, label) = if *overwrite {
                        (crate::clip_clipboard::PasteMode::Overwrite, "Overwrite from the Other Timeline")
                    } else {
                        (crate::clip_clipboard::PasteMode::Insert, "Insert from the Other Timeline")
                    };
                    self.paste_clipboard(&cb, *at, mode, label);
                    true
                }
                AppCommand::EditFromSelects { sequence_id, range_in, range_out, at, overwrite } => {
                    if *sequence_id == self.state.sequence().id {
                        self.state.toast.show("The selects and the cut are the same sequence: open another as selects");
                        return true;
                    }
                    let Some(selects) = self.state.sequences.iter().find(|s| s.id == *sequence_id) else { return true };
                    let Some(cb) = crate::clip_clipboard::copy_range(selects, *range_in, *range_out) else {
                        self.state.toast.show("Nothing in the selects' marked range");
                        return true;
                    };
                    let at = at.or(self.state.sequence().in_point).unwrap_or(self.state.playhead);
                    let (mode, label) = if *overwrite {
                        (crate::clip_clipboard::PasteMode::Overwrite, "Overwrite from Selects")
                    } else {
                        (crate::clip_clipboard::PasteMode::Insert, "Insert from Selects")
                    };
                    self.paste_clipboard(&cb, at, mode, label);
                    true
                }
                AppCommand::PasteClipInsert => {
                    if let Some(cb) = self.state.clipboard.clone() {
                        self.paste_clipboard(&cb, self.state.playhead, crate::clip_clipboard::PasteMode::Insert, "Paste Insert");
                    }
                    true
                }
                // Ctrl+D — Duplicate selected clip (place immediately after original)
                // Copy Attributes: snapshot a clip's effects/audio/transform/speed
                AppCommand::CopyAttributes { clip_id } => {
                    let cid = *clip_id;
                    let seq = &self.state.sequences[self.state.active_sequence_idx];
                    if let Some(clip) = seq.all_clips().find(|c| c.id == cid) {
                        self.state.attributes_clipboard = Some(crate::state::AttributesClipboard::of_in(seq, clip));
                        self.state.toast.show("Attributes copied".to_string());
                    }
                    true
                }
                // Paste Attributes: apply selected attribute categories to target clip(s)
                AppCommand::PasteAttributes { effects, color_grade, audio, transform, speed } => {
                    // One whole-clip snapshot either side per target, as
                    // one undo step. It used to write the clips in place
                    // with no history, and to stop at the first clip in
                    // the sequence that was not a target — so it pasted
                    // to whichever targets came before that, skipped the
                    // engine sync, and said nothing.
                    if let Some(attrs) = self.state.attributes_clipboard.clone() {
                        // The grade would arrive without its corrections'
                        // mixes (M3 step 17a): refused, never pasted whole.
                        if *color_grade && attrs.grade_mix_held {
                            self.state.toast.show(GRADE_MIX_NOT_COPIED);
                            return true;
                        }
                        let what = crate::state::PasteChoice {
                            effects: *effects,
                            color_grade: *color_grade,
                            audio: *audio,
                            transform: *transform,
                            speed: *speed,
                        };
                        let mut targets = self.state.selected_clips.clone();
                        if let Some(primary) = self.state.selected_clip {
                            if !targets.contains(&primary) {
                                targets.push(primary);
                            }
                        }
                        let (mut edits, labels) = paste_attributes(&self.state.sequences[self.state.active_sequence_idx], &targets, &attrs, what);
                        let count = edits.len();
                        // The grade's node labels, in the same step (#1866).
                        edits.extend(labels);
                        self.apply_edits_as_one(&edits, "Paste Attributes");
                        self.state.toast.show(format!("Attributes pasted to {count} clip(s)"));
                    }
                    true
                }
                AppCommand::DuplicateClip { clip_id } => {
                    let cid = *clip_id;
                    // Several clips selected: all of them, with their
                    // partners and transitions, after the last of them, one
                    // undo step (#845). One clip keeps its own path below.
                    let ids = crate::clip_clipboard::selection_for(cid, self.state.selected_clip, &self.state.selected_clips);
                    if ids.len() > 1 {
                        if let Some(cb) = crate::clip_clipboard::copy(self.state.sequence(), &ids) {
                            let after = cb.clips.iter().map(|c| c.clip.timeline_out).max().unwrap_or(self.state.playhead);
                            self.paste_clipboard(&cb, after, crate::clip_clipboard::PasteMode::Overwrite, "Duplicate");
                        }
                        return true;
                    }
                    let seq = &self.state.sequences[self.state.active_sequence_idx];
                    let found = seq.tracks.iter().enumerate().find_map(|(ti, t)| {
                        t.find_clip(cid).map(|c| (ti, c.clone()))
                    });
                    if let Some((track_index, clip)) = found {
                        let new_id = self.state.duplicate_after_on(track_index, &clip);
                        self.state.selected_clip = Some(new_id);
                        self.state.selected_clips.clear();
                        self.state.dirty = true;
                    }
                    self.sync_sequence_to_engine();
                    true
                }
                // Toggle clip enabled/disabled
                AppCommand::SetClipEnabled { clip_id, enabled } => {
                    let cid = *clip_id;
                    let en = *enabled;
                    let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                    if let Some(clip) = seq.all_clips_mut().find(|c| c.id == cid) {
                        clip.enabled = en;
                    }
                    self.state.dirty = true;
                    self.sync_sequence_to_engine();
                    true
                }
                AppCommand::SetClipSolo { clip_id, solo } => {
                    let cid = *clip_id;
                    let s = *solo;
                    let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                    if let Some(clip) = seq.all_clips_mut().find(|c| c.id == cid) {
                        clip.solo = s;
                    }
                    self.state.dirty = true;
                    self.sync_sequence_to_engine();
                    true
                }

                // ── Track matte / blend mode (Phase 1 data-model handlers) ────
                AppCommand::SetClipBlendMode { clip_id, mode } => {
                    let cid = *clip_id;
                    let m = *mode;
                    let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                    if let Some(clip) = seq.all_clips_mut().find(|c| c.id == cid) {
                        clip.blend_mode = m;
                    }
                    self.state.dirty = true;
                    self.sync_sequence_to_engine();
                    true
                }
                AppCommand::SetClipMatte {
                    clip_id, source_clip_id, channel, invert, feather_px, choke_px,
                } => {
                    let consumer_id = *clip_id;
                    let source_id = *source_clip_id;
                    let seq_idx = self.state.active_sequence_idx;

                    // Reject self-reference: a clip cannot be its own matte.
                    if consumer_id == source_id {
                        tracing::warn!("SetClipMatte: refusing self-reference for clip {consumer_id:?}");
                        return true;
                    }

                    // Validate against the active sequence: source must exist,
                    // must not be an AdjustmentLayer (no own pixels), and the
                    // bind must not introduce a cycle.
                    {
                        let seq = &self.state.sequences[seq_idx];
                        let source = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == source_id);
                        let Some(source) = source else {
                            tracing::warn!("SetClipMatte: source clip {source_id:?} not found in active sequence");
                            return true;
                        };
                        if matches!(source.kind, klipp_timeline::ClipKind::AdjustmentLayer) {
                            tracing::warn!("SetClipMatte: cannot bind an AdjustmentLayer as matte source — it has no own pixels");
                            return true;
                        }
                        if matte_chain_creates_cycle(seq, consumer_id, source_id) {
                            tracing::warn!("SetClipMatte: rejecting bind that would create a matte cycle ({consumer_id:?} ← {source_id:?})");
                            return true;
                        }
                    }

                    let binding = klipp_timeline::MatteBinding {
                        source_clip_id: source_id,
                        channel: matte_channel_from_u8(*channel),
                        invert: *invert,
                        feather_px: *feather_px,
                        choke_px: *choke_px,
                    };
                    let seq = &mut self.state.sequences[seq_idx];
                    if let Some(consumer) = seq.all_clips_mut().find(|c| c.id == consumer_id) {
                        consumer.matte_source = Some(binding);
                    }
                    // Default-on: hide the source clip from the visible composite.
                    // Pros can flip this back via SetClipConsumedAsMatte.
                    if let Some(src) = seq.all_clips_mut().find(|c| c.id == source_id) {
                        src.consumed_as_matte = true;
                    }
                    self.state.dirty = true;
                    self.sync_sequence_to_engine();
                    true
                }
                AppCommand::ClearClipMatte { clip_id } => {
                    let consumer_id = *clip_id;
                    let seq_idx = self.state.active_sequence_idx;
                    // Capture which source the consumer was pointing at, then
                    // clear the binding.
                    let prior_source = {
                        let seq = &mut self.state.sequences[seq_idx];
                        if let Some(consumer) = seq.all_clips_mut().find(|c| c.id == consumer_id) {
                            let prior = consumer.matte_source.as_ref().map(|b| b.source_clip_id);
                            consumer.matte_source = None;
                            prior
                        } else {
                            None
                        }
                    };
                    // If nothing else references the source, return it to the
                    // visible composite (clear `consumed_as_matte`).
                    if let Some(sid) = prior_source {
                        let still_referenced = self.state.sequences[seq_idx].tracks.iter()
                            .flat_map(|t| t.clips.iter())
                            .any(|c| c.matte_source.as_ref().is_some_and(|b| b.source_clip_id == sid));
                        if !still_referenced {
                            let seq = &mut self.state.sequences[seq_idx];
                            if let Some(src) = seq.all_clips_mut().find(|c| c.id == sid) {
                                src.consumed_as_matte = false;
                            }
                        }
                    }
                    self.state.dirty = true;
                    self.sync_sequence_to_engine();
                    true
                }
                AppCommand::SetClipTransformLink {
                    clip_id, target, mode, inherit_scale, inherit_position, inherit_rotation, inherit_anchor,
                } => {
                    let consumer_id = *clip_id;
                    let target_id = *target;
                    let seq_idx = self.state.active_sequence_idx;

                    // Reject self-link.
                    if consumer_id == target_id {
                        tracing::warn!("SetClipTransformLink: refusing self-link for clip {consumer_id:?}");
                        return true;
                    }

                    // Validate target exists, is not an AdjustmentLayer
                    // (no own transform — we want pixel-bearing clips
                    // as targets), and the bind would not introduce a
                    // cycle.
                    {
                        let seq = &self.state.sequences[seq_idx];
                        let Some(target_clip) = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == target_id) else {
                            tracing::warn!("SetClipTransformLink: target clip {target_id:?} not found in active sequence");
                            return true;
                        };
                        if matches!(target_clip.kind, klipp_timeline::ClipKind::AdjustmentLayer) {
                            tracing::warn!("SetClipTransformLink: AdjustmentLayer cannot be a transform-link target — it has no transform of its own");
                            return true;
                        }
                        if transform_link_chain_creates_cycle(seq, consumer_id, target_id) {
                            tracing::warn!("SetClipTransformLink: rejecting bind that would create a transform-link cycle ({consumer_id:?} ← {target_id:?})");
                            return true;
                        }
                    }

                    let link_mode = if *mode == 1 { klipp_timeline::LinkMode::Additive } else { klipp_timeline::LinkMode::Absolute };
                    // SetClipTransformLink today only takes a ClipId
                    // target — wrap into the Clip variant. The
                    // tracker-target path (HZ-MATTE-NEXT-TRACKER-DRIVER
                    // commit 3) will surface as a separate AppCommand
                    // or a discriminator on this one.
                    let link = klipp_timeline::TransformLink {
                        target: klipp_timeline::TransformLinkTarget::Clip(target_id),
                        mode: link_mode,
                        inherit_scale: *inherit_scale,
                        inherit_position: *inherit_position,
                        inherit_rotation: *inherit_rotation,
                        inherit_anchor: *inherit_anchor,
                    };
                    let seq = &mut self.state.sequences[seq_idx];
                    if let Some(consumer) = seq.all_clips_mut().find(|c| c.id == consumer_id) {
                        consumer.transform_link = Some(link);
                    }
                    self.state.dirty = true;
                    self.sync_sequence_to_engine();
                    true
                }
                AppCommand::SetClipTransformLinkTracker {
                    clip_id, host_clip_id, point_index, mode,
                    inherit_scale, inherit_position, inherit_rotation, inherit_anchor,
                } => {
                    let consumer_id = *clip_id;
                    let host_id = *host_clip_id;
                    let pt_idx = *point_index;
                    let seq_idx = self.state.active_sequence_idx;

                    // Validate: host clip exists; named tracker
                    // exists on it; binding doesn't close a cycle
                    // (a Tracker variant can't form a transform_link
                    // chain on its own, but a host clip whose own
                    // transform_link points back at consumer would
                    // — defence-in-depth covers that).
                    {
                        let seq = &self.state.sequences[seq_idx];
                        let Some(host) = seq.tracks.iter().flat_map(|t| t.clips.iter())
                            .find(|c| c.id == host_id) else {
                            tracing::warn!(
                                "SetClipTransformLinkTracker: host clip {host_id:?} not found in active sequence"
                            );
                            return true;
                        };
                        if host.track_points.get(pt_idx as usize).is_none() {
                            tracing::warn!(
                                "SetClipTransformLinkTracker: tracker {pt_idx} doesn't exist on clip {host_id:?}"
                            );
                            return true;
                        }
                        if transform_link_chain_creates_cycle(seq, consumer_id, host_id) {
                            tracing::warn!(
                                "SetClipTransformLinkTracker: rejecting bind that would close a cycle ({consumer_id:?} ← tracker on {host_id:?})"
                            );
                            return true;
                        }
                    }

                    let link_mode = if *mode == 1 { klipp_timeline::LinkMode::Additive } else { klipp_timeline::LinkMode::Absolute };
                    let link = klipp_timeline::TransformLink {
                        target: klipp_timeline::TransformLinkTarget::Tracker {
                            clip_id: host_id, point_index: pt_idx,
                        },
                        mode: link_mode,
                        inherit_scale: *inherit_scale,
                        inherit_position: *inherit_position,
                        inherit_rotation: *inherit_rotation,
                        inherit_anchor: *inherit_anchor,
                    };
                    let seq = &mut self.state.sequences[seq_idx];
                    if let Some(consumer) = seq.all_clips_mut().find(|c| c.id == consumer_id) {
                        consumer.transform_link = Some(link);
                    }
                    self.state.dirty = true;
                    self.sync_sequence_to_engine();
                    true
                }
                AppCommand::ClearClipTransformLink { clip_id } => {
                    let cid = *clip_id;
                    let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                    if let Some(clip) = seq.all_clips_mut().find(|c| c.id == cid) {
                        clip.transform_link = None;
                    }
                    self.state.dirty = true;
                    self.sync_sequence_to_engine();
                    true
                }
                AppCommand::AutoLinkClipsBeneathSource {
                    source_clip_id, ramp_secs, mode,
                    inherit_scale, inherit_position, inherit_rotation, inherit_anchor,
                } => {
                    let src_id = *source_clip_id;
                    let seq_idx = self.state.active_sequence_idx;
                    let link_mode = if *mode == 1 {
                        klipp_timeline::LinkMode::Additive
                    } else {
                        klipp_timeline::LinkMode::Absolute
                    };
                    let targets = {
                        let seq = &self.state.sequences[seq_idx];
                        crate::hyperzoom::collect_auto_link_targets(seq, src_id, *ramp_secs)
                    };
                    if targets.is_empty() {
                        // Likely benign — no qualifying clip beneath the
                        // source within the ramp window. Logging at
                        // debug rather than warn so it doesn't pollute
                        // the panel during normal use.
                        tracing::debug!(
                            "AutoLinkClipsBeneathSource: no clips matched for source {src_id:?} \
                             within {ramp_secs} s ramp window"
                        );
                        return true;
                    }
                    let inherit = (*inherit_scale, *inherit_position, *inherit_rotation, *inherit_anchor);
                    let seq = &mut self.state.sequences[seq_idx];
                    for cid in &targets {
                        if let Some(clip) = seq.all_clips_mut().find(|c| c.id == *cid) {
                            clip.transform_link = Some(klipp_timeline::TransformLink {
                                target: klipp_timeline::TransformLinkTarget::Clip(src_id),
                                mode: link_mode,
                                inherit_scale: inherit.0,
                                inherit_position: inherit.1,
                                inherit_rotation: inherit.2,
                                inherit_anchor: inherit.3,
                            });
                        }
                    }
                    tracing::info!(
                        "AutoLinkClipsBeneathSource: linked {} clip(s) beneath {src_id:?} → {src_id:?} ({:?})",
                        targets.len(), link_mode,
                    );
                    self.state.dirty = true;
                    self.sync_sequence_to_engine();
                    true
                }
                AppCommand::ApplyHyperzoom {
                    source_clip_id, target_clip_id, focal_x_norm, focal_y_norm,
                    duration_secs, max_scale, easing, hold_secs,
                    add_radial_blur, radial_blur_amount, focal_ref,
                    add_whoosh, whoosh_variant, whoosh_custom_path,
                    add_chromatic_aberration, chromatic_aberration_amount,
                    add_light_leak, light_leak_intensity,
                } => {
                    let src_id = *source_clip_id;
                    let seq_idx = self.state.active_sequence_idx;

                    // Resolve source + target. If `target_clip_id` is
                    // None, the handler walks the source's track and
                    // picks the next clip; absent a "next" clip the
                    // operation is a no-op (warn only — we don't want
                    // a partial application that left a dangling
                    // ramp on the source).
                    let (src_dur_secs, src_timeline_out, tgt_id, tgt_dur_secs) = {
                        let seq = &self.state.sequences[seq_idx];
                        let mut found: Option<(f64, klipp_core::Timecode, klipp_core::ClipId, f64)> = None;
                        'outer: for track in &seq.tracks {
                            for (idx, c) in track.clips.iter().enumerate() {
                                if c.id == src_id {
                                    let src_dur = c.duration().to_secs_f64().max(0.0);
                                    let src_out = c.timeline_out;
                                    let tgt = match target_clip_id {
                                        Some(t) => track.clips.iter().find(|c| c.id == *t).cloned(),
                                        None => track.clips.get(idx + 1).cloned(),
                                    };
                                    let Some(tgt) = tgt else {
                                        tracing::warn!(
                                            "ApplyHyperzoom: source {src_id:?} has no resolvable target clip — \
                                             pass an explicit target or ensure a clip follows it on the same track"
                                        );
                                        return true;
                                    };
                                    found = Some((src_dur, src_out, tgt.id, tgt.duration().to_secs_f64().max(0.0)));
                                    break 'outer;
                                }
                            }
                        }
                        let Some(found) = found else {
                            tracing::warn!("ApplyHyperzoom: source clip {src_id:?} not found in active sequence");
                            return true;
                        };
                        found
                    };

                    if src_id == tgt_id {
                        tracing::warn!("ApplyHyperzoom: refusing self-target ({src_id:?})");
                        return true;
                    }

                    let plan = crate::hyperzoom::plan_hyperzoom(crate::hyperzoom::HyperzoomInput {
                        source_clip_dur_secs: src_dur_secs,
                        target_clip_dur_secs: tgt_dur_secs,
                        focal_x_norm: *focal_x_norm,
                        focal_y_norm: *focal_y_norm,
                        duration_secs: *duration_secs,
                        max_scale: *max_scale,
                        easing: crate::hyperzoom::HyperzoomEasing::from_u8(*easing),
                        hold_secs: *hold_secs,
                        add_radial_blur: *add_radial_blur,
                        radial_blur_amount: *radial_blur_amount,
                        add_chromatic_aberration: *add_chromatic_aberration,
                        chromatic_aberration_amount: *chromatic_aberration_amount,
                        add_light_leak: *add_light_leak,
                        light_leak_intensity: *light_leak_intensity,
                    });

                    // Each clip's picture before, for the one undo step the
                    // apply records (M3 step 4: the effect layers are graph
                    // edits by path, recorded as picture edits).
                    let picture_of = |seq: &klipp_timeline::Sequence, id| seq.all_clips().find(|c| c.id == id).map(|c| c.picture.clone());
                    let before = [src_id, tgt_id].map(|id| (id, picture_of(&self.state.sequences[seq_idx], id)));
                    let seq = &mut self.state.sequences[seq_idx];
                    apply_hyperzoom_plan_to_clip(seq, src_id, plan.source_anchor, &plan.source_scale_kfs);
                    apply_hyperzoom_plan_to_clip(seq, tgt_id, plan.target_anchor, &plan.target_scale_kfs);
                    if let Some(rb) = plan.radial_blur.as_ref() {
                        apply_hyperzoom_radial_blur_to_clip(seq, src_id, rb.center, rb.peak_amount, &rb.source_amount_kfs);
                        apply_hyperzoom_radial_blur_to_clip(seq, tgt_id, rb.center, rb.peak_amount, &rb.target_amount_kfs);
                    }
                    if let Some(ca) = plan.chromatic_aberration.as_ref() {
                        apply_hyperzoom_chromatic_aberration_to_clip(
                            seq, src_id, ca.center, ca.peak_amount,
                            &ca.source_red_kfs, &ca.source_blue_kfs,
                        );
                        apply_hyperzoom_chromatic_aberration_to_clip(
                            seq, tgt_id, ca.center, ca.peak_amount,
                            &ca.target_red_kfs, &ca.target_blue_kfs,
                        );
                    }
                    if let Some(ll) = plan.light_leak.as_ref() {
                        apply_hyperzoom_light_leak_to_clip(
                            seq, src_id, ll.peak_intensity, &ll.source_intensity_kfs,
                        );
                        apply_hyperzoom_light_leak_to_clip(
                            seq, tgt_id, ll.peak_intensity, &ll.target_intensity_kfs,
                        );
                    }

                    // Glyph-anchored focal-point override. Written
                    // ONLY on the source clip — the target's anchor
                    // is the cut-frame zoom-out point, not a value
                    // worth live-tracking (audit decision per
                    // HZ-NEXT-GLYPH-LOCK question (f); revisit only
                    // if a real "track the same letter on both sides
                    // of the cut" use case shows up). focal_ref=None
                    // (the back-compat path) skips this entirely and
                    // anchor_x/y from apply_hyperzoom_plan_to_clip
                    // remain the only authority on focal position.
                    if let Some(fr) = focal_ref {
                        apply_focal_ref_to_clip_transform(seq, src_id, *fr);
                    }

                    // Whoosh stinger (HZ-NEXT-WHOOSH). Opt-in via
                    // `add_whoosh = true`. Folded into the same
                    // dispatch arm so apply + whoosh are a single
                    // undo step — same pattern as the radial-blur
                    // convenience layer. The pure helper does the
                    // variant resolution + pool registration + clip
                    // insertion; we only record the undo entry on
                    // success.
                    self.state.active_history_mut().begin_batch();
                    for (clip_id, before) in before {
                        let after = picture_of(&self.state.sequences[seq_idx], clip_id);
                        if let (Some(before), Some(after)) = (before, after) {
                            let edit = klipp_timeline::PictureEdit { before, after };
                            self.state.push_history(EditCommand::PictureEdit { description: "Hyperzoom", clip_id, edit: Box::new(edit) });
                        }
                    }
                    if let Some(inserted) = maybe_insert_whoosh_for_apply(
                        &mut self.state,
                        seq_idx,
                        src_timeline_out,
                        *add_whoosh,
                        *whoosh_variant,
                        whoosh_custom_path.as_deref(),
                    ) {
                        self.state.push_history(EditCommand::LegacyAppendClip {
                            clip: inserted.clip,
                            track_index: inserted.track_index,
                        });
                    }
                    self.state.active_history_mut().end_batch_as("Apply Hyperzoom");

                    self.state.dirty = true;
                    self.sync_sequence_to_engine();
                    true
                }
                AppCommand::OpenSaveHyperzoomPresetDialog { source_clip } => {
                    // Pin the source clip so the dialog reads the
                    // right scratch even if the user navigates the
                    // timeline before clicking Save. Clear any
                    // stale name + error from a prior open.
                    self.state.show_save_hyperzoom_preset_dialog = true;
                    self.state.save_hyperzoom_preset_source_clip = Some(*source_clip);
                    self.state.save_hyperzoom_preset_name.clear();
                    self.state.save_hyperzoom_preset_error = None;
                    true
                }
                AppCommand::SetClipConsumedAsMatte { clip_id, consumed } => {
                    let cid = *clip_id;
                    let c = *consumed;
                    let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                    if let Some(clip) = seq.all_clips_mut().find(|c| c.id == cid) {
                        clip.consumed_as_matte = c;
                    }
                    self.state.dirty = true;
                    self.sync_sequence_to_engine();
                    true
                }
                AppCommand::SetClipStencil { clip_id, channel, invert, feather_px, choke_px } => {
                    let cid = *clip_id;
                    let stencil = klipp_timeline::StencilSettings {
                        channel: matte_channel_from_u8(*channel),
                        invert: *invert,
                        feather_px: *feather_px,
                        choke_px: *choke_px,
                    };
                    let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                    if let Some(clip) = seq.all_clips_mut().find(|c| c.id == cid) {
                        clip.matte_stencil = Some(stencil);
                    }
                    self.state.dirty = true;
                    self.sync_sequence_to_engine();
                    true
                }
                AppCommand::ClearClipStencil { clip_id } => {
                    let cid = *clip_id;
                    let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                    if let Some(clip) = seq.all_clips_mut().find(|c| c.id == cid) {
                        clip.matte_stencil = None;
                    }
                    self.state.dirty = true;
                    self.sync_sequence_to_engine();
                    true
                }
                AppCommand::UseClipAboveAsMatte { clip_id, channel } => {
                    let cid = *clip_id;
                    let chan = *channel;
                    // Resolve the consumer's track index.
                    let seq = &self.state.sequences[self.state.active_sequence_idx];
                    let track_idx = seq.tracks.iter().position(|t| t.clips.iter().any(|c| c.id == cid));
                    let Some(track_idx) = track_idx else {
                        return true;
                    };
                    if let Some((source_id, _kind)) = crate::panels::timeline::find_matte_candidate_above(&self.state, track_idx, cid) {
                        // Re-dispatch as SetClipMatte so the cycle /
                        // self-ref / AdjustmentLayer guards in that
                        // handler are the single source of truth. Through
                        // the UI's dispatcher: the engine ignores it (#437).
                        self.send_commands(vec![AppCommand::SetClipMatte {
                            clip_id: cid,
                            source_clip_id: source_id,
                            channel: chan,
                            invert: false,
                            feather_px: 0.0,
                            choke_px: 0.0,
                        }]);
                    } else {
                        self.state.toast.show("No clip above to use as matte. Stack a clip on a higher track first.");
                    }
                    true
                }
                AppCommand::ToggleClipStencil { clip_id } => {
                    let cid = *clip_id;
                    let currently_on = self.state.sequences[self.state.active_sequence_idx]
                        .tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid)
                        .map(|c| c.matte_stencil.is_some()).unwrap_or(false);
                    let next_cmd = if currently_on {
                        AppCommand::ClearClipStencil { clip_id: cid }
                    } else {
                        AppCommand::SetClipStencil {
                            clip_id: cid,
                            channel: 1, // Luminance default
                            invert: false,
                            feather_px: 0.0,
                            choke_px: 0.0,
                        }
                    };
                    // The UI handles both; the engine ignores them (#437).
                    self.send_commands(vec![next_cmd]);
                    true
                }

                // Grade versions — handled by handle_color_commands
                // Select All clips on timeline
                AppCommand::SelectAllClips => {
                    self.select_clips_in(None, klipp_core::SelectRange::All);
                    true
                }
                AppCommand::SelectClipsForward => {
                    self.select_clips_in(None, klipp_core::SelectRange::Forward);
                    true
                }
                AppCommand::SelectClipsBackward => {
                    self.select_clips_in(None, klipp_core::SelectRange::Backward);
                    true
                }
                AppCommand::SelectClipsIn { track, range } => {
                    self.select_clips_in(*track, *range);
                    true
                }
                // Selection by what the clips are (#925).
                AppCommand::SelectInverse | AppCommand::SelectSameSource | AppCommand::SelectByColour { .. } => {
                    let mut selected = self.state.selected_clips.clone();
                    selected.extend(self.state.selected_clip);
                    let (seq, at) = (self.state.sequence(), self.state.playhead);
                    let ids = match cmd {
                        AppCommand::SelectInverse => crate::selection::invert(seq, &selected, at),
                        AppCommand::SelectSameSource => crate::selection::same_source(seq, &selected, at),
                        AppCommand::SelectByColour { label } => crate::selection::with_colour(seq, *label, at),
                        _ => Vec::new(),
                    };
                    if ids.is_empty() {
                        self.state.toast.show("No clips to select (locked tracks are left out)");
                    } else {
                        self.state.selected_clip = ids.first().copied();
                        self.state.selected_clips = ids;
                    }
                    true
                }
                AppCommand::CloseAllGaps { track_index } => {
                    let closed = self.state.sequence_mut().tracks.get_mut(*track_index).filter(|t| !t.locked).map(crate::selection::close_all_gaps).unwrap_or(0);
                    if closed > 0 {
                        self.state.dirty = true;
                        self.sync_sequence_to_engine();
                    }
                    self.state.toast.show(match closed {
                        0 => "No gaps to close on that track".to_string(),
                        1 => "Closed 1 gap".to_string(),
                        n => format!("Closed {n} gaps"),
                    });
                    true
                }
                AppCommand::SetClipKeepPitch { clip_id, keep_pitch } => {
                    let seq = self.state.sequence_mut();
                    let partner = seq.all_clips().find(|c| c.id == *clip_id).and_then(|c| c.linked_clip_id);
                    let mut changed = false;
                    for id in std::iter::once(*clip_id).chain(partner) {
                        if let Some((t, c)) = seq.find_clip(id) {
                            let clip = &mut seq.tracks[t].clips[c];
                            changed |= clip.keep_pitch != *keep_pitch;
                            clip.keep_pitch = *keep_pitch;
                        }
                    }
                    if changed {
                        self.state.dirty = true;
                        self.sync_sequence_to_engine();
                    }
                    true
                }
                AppCommand::GoToNextGap => {
                    match crate::selection::next_gap(self.state.sequence(), self.state.playhead) {
                        Some((_, at, _)) => {
                            self.state.playhead = at;
                            let _ = self.cmd_tx.send(AppCommand::SeekTo(at));
                        }
                        None => self.state.toast.show("No gap after the playhead"),
                    }
                    true
                }
                AppCommand::SyncSelectedClips { by_markers } => {
                    let Some(anchor) = self.state.selected_clip else {
                        self.state.toast.show("Select the clip to line the others up with");
                        return true;
                    };
                    let mut selected = self.state.selected_clips.clone();
                    selected.extend(self.state.selected_clip);
                    let result = if *by_markers {
                        crate::timeline_sync::by_markers(self.state.sequence(), &selected, anchor)
                    } else {
                        crate::timeline_sync::by_timecode(self.state.sequence(), &self.state.media_pool, &selected, anchor)
                    };
                    match result {
                        Ok((moves, skipped)) => {
                            let n = moves.len();
                            self.send_commands(moves.into_iter().map(|(clip_id, new_timeline_in)| AppCommand::MoveClip { clip_id, new_timeline_in }).collect());
                            let how = if *by_markers { "markers" } else { "timecode" };
                            self.state.toast.show(match (n, skipped.first()) {
                                (_, None) => format!("Synced {n} clip(s) by {how}"),
                                (_, Some(why)) => format!("Synced {n} clip(s) by {how}; {} left: {why}", skipped.len()),
                            });
                        }
                        Err(why) => self.state.toast.show(format!("Not synced: {why}")),
                    }
                    true
                }
                AppCommand::OpenSpeedDialog { clip_id } => {
                    let spd = self.state.sequence().all_clips().find(|c| c.id == *clip_id).map(|c| c.speed).unwrap_or(1.0);
                    self.state.speed_dialog_value = spd.abs() * 100.0;
                    self.state.speed_dialog_reverse = spd < 0.0;
                    let keep = self.state.sequence().all_clips().any(|c| c.id == *clip_id && c.keep_pitch);
                    self.state.speed_dialog_keep_pitch = keep;
                    self.state.show_speed_dialog = true;
                    true
                }
                AppCommand::SubclipFromClip { clip_id } => {
                    let found = self.state.sequence().find_clip(*clip_id).map(|(t, c)| {
                        let clip = &self.state.sequence().tracks[t].clips[c];
                        (clip.media_id(), clip.source_in, clip.source_out, clip.name.clone())
                    });
                    match found {
                        Some((Some(media_id), from, to, name)) if to > from => self.make_subclip(media_id, from, to, name),
                        _ => self.state.toast.show("A subclip is made from a media clip"),
                    }
                    true
                }
                AppCommand::SelectClipsOnSelectedTrack { range } => {
                    let track = self.state.selected_clip.and_then(|id| crate::selection::track_of(self.state.sequence(), id));
                    match track {
                        Some(track) => self.select_clips_in(Some(track), *range),
                        None => self.state.toast.show("Select a clip on the track first".to_string()),
                    }
                    true
                }
                // Frame grab: save current viewer frame as PNG
                AppCommand::FrameGrab => {
                    let fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                    if !fb.data.is_empty() && fb.width > 0 && fb.height > 0 {
                        let w = fb.width;
                        let h = fb.height;
                        // Convert RGBA to RGB
                        let rgb: Vec<u8> = fb.data.chunks_exact(4)
                            .flat_map(|px| [px[0], px[1], px[2]])
                            .collect();
                        drop(fb);
                        // Default filename from playhead timecode (colons are
                        // illegal on Windows paths, so swap them for hyphens).
                        let default_name = format!(
                            "frame_{}.png",
                            self.state.timecode_display().position(self.state.playhead).replace([':', ';'], "-")
                        );
                        crate::file_dialog::spawn(move || {
                            if let Some(path) = rfd::FileDialog::new()
                                .set_title("Save Frame As")
                                .add_filter("PNG Image", &["png"])
                                .set_file_name(&default_name)
                                .save_file()
                            {
                                if save_rgb_as_png(&rgb, w, h, &path) {
                                    crate::file_dialog::notify(format!("Frame saved to {}", path.display()));
                                } else {
                                    crate::file_dialog::notify("Failed to save frame");
                                }
                            }
                            Vec::new()
                        });
                    } else {
                        self.state.toast.show("No frame available".to_string());
                    }
                    true
                }
                // Extract frame to bin: save PNG + import as still — no file dialog.
                AppCommand::ExtractFrameToBin => {
                    let (rgba, w, h) = {
                        let fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                        if fb.data.is_empty() || fb.width == 0 || fb.height == 0 {
                            drop(fb);
                            self.state.toast.show("No frame available".to_string());
                            return true;
                        }
                        (fb.data.clone(), fb.width, fb.height)
                    };

                    // Output directory: the saved project's .klipp dir, or a
                    // temp fallback so the feature still works in an unsaved
                    // project. save_freeze_frame_png appends media/stills/.
                    let stills_root: std::path::PathBuf = match self.writable_project_dir() {
                        Some(dir) => dir,
                        None => klipp_project::temp_still_roots()[1].clone(),
                    };

                    // Timecode-based filename + nanos suffix for uniqueness
                    // when extracting the same frame more than once.
                    let tc_str = self.state.timecode_display().position(self.state.playhead).replace([':', ';'], "-");
                    let nanos = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.subsec_nanos())
                        .unwrap_or(0);
                    let filename = format!("frame_{tc_str}_{nanos}.png");

                    match klipp_project::save_freeze_frame_png(&stills_root, &filename, &rgba, w, h) {
                        Ok(png_path) => {
                            // Route straight to the engine — LoadMediaFile will
                            // probe the PNG, detect it as a still, and push a
                            // MediaLoaded event that the UI handler turns into
                            // a MediaItem with is_still = true.
                            let media_id = klipp_core::MediaId::new();
                            let _ = self.cmd_tx.send(AppCommand::LoadMediaFile {
                                path: png_path.clone(),
                                media_id,
                            });
                            self.state.toast.show("Frame extracted to bin".to_string());
                        }
                        Err(e) => {
                            tracing::warn!("extract frame: {e}");
                            self.state.toast.show(format!("Failed to extract frame: {e}"));
                        }
                    }
                    true
                }
            _ => false,
        }
    }
}

impl KlippApp {
    /// Check if the track containing a given clip is locked.
    pub(super) fn clip_track_is_locked(&self, clip_id: &ClipId) -> bool {
        self.state.sequences[self.state.active_sequence_idx].tracks.iter()
            .find(|t| t.clips.iter().any(|c| c.id == *clip_id))
            .is_some_and(|t| t.locked)
    }

    /// Swap a clip's backing media while preserving timeline_in/out,
    /// source_in/out, effects, transitions, and linked partner.
    /// Auto-clamps source_out + timeline_out by the same delta when
    /// the new media is shorter than the existing trim window.
    /// Returns true on success; toasts + returns false on validation
    /// failure (locked track, unknown clip / media, identical media,
    /// non-Media kind).
    fn try_replace_clip_media(
        &mut self,
        clip_id: ClipId,
        new_media_id: MediaId,
    ) -> bool {
        if self.clip_track_is_locked(&clip_id) {
            self.state.toast.show("Replace: track is locked");
            return false;
        }
        // Resolve the new media's source duration so we know whether
        // to clamp. None → media isn't in the pool.
        let (new_media_dur, new_media_in) = match self.state.media_pool.get(&new_media_id) {
            Some(item) => (item.duration, item.default_source_in),
            None => {
                self.state.toast.show("Replace: target media not found in pool");
                return false;
            }
        };
        // The new media's In mark, when it has one, is where the clip
        // now starts in it (#919); the step is recorded whole.
        let undo_before = new_media_in.and_then(|_| crate::tracks_undo::before_labelled(&self.state, "Replace From Bin"));
        let seq = &mut self.state.sequences[self.state.active_sequence_idx];
        let Some(clip) = seq.all_clips_mut().find(|c| c.id == clip_id) else {
            self.state.toast.show("Replace: clip not found");
            return false;
        };
        // Only Media-kind clips can have their media swapped — Title /
        // Generator / Sequence clips have a different backing model
        // and the gesture doesn't apply.
        let old_media_id = match clip.kind {
            klipp_timeline::ClipKind::Media { media_id } => media_id,
            _ => {
                self.state.toast.show("Replace: only media clips can be swapped");
                return false;
            }
        };
        if old_media_id == new_media_id {
            return false; // no-op, no toast
        }
        if let (Some(src_in), Some(before)) = (new_media_in, undo_before) {
            let len = clip.source_out.to_secs_f64() - clip.source_in.to_secs_f64();
            let out = (src_in.to_secs_f64() + len).min(new_media_dur.to_secs_f64());
            let lost = len - (out - src_in.to_secs_f64());
            clip.kind = klipp_timeline::ClipKind::Media { media_id: new_media_id };
            clip.source_in = src_in;
            clip.source_out = klipp_core::RationalTime::from_secs_f64(out, src_in.rate);
            if lost > 0.0 {
                clip.timeline_out = clip.timeline_out - klipp_core::RationalTime::from_secs_f64(lost / clip.speed.abs().max(0.1), clip.timeline_out.rate);
            }
            crate::tracks_undo::record(&mut self.state, before);
            return true;
        }

        // Clamp source_out (and timeline_out by the same delta) when
        // the new media is shorter than the existing trim window.
        // Source_in stays untouched — if user trimmed in by 5s on
        // the old clip and the new media is at least source_in long,
        // we keep the head trim. Both clamps are applied to the same
        // timing reference (`clip.source_out`), so the visible clip
        // length never grows; it only shortens to fit.
        let old_source_out = clip.source_out;
        let old_timeline_out = clip.timeline_out;
        let new_source_out = if old_source_out > new_media_dur {
            new_media_dur
        } else {
            old_source_out
        };
        let dt_secs = old_source_out.to_secs_f64() - new_source_out.to_secs_f64();
        let new_timeline_out = if dt_secs > 0.0 {
            // Convert delta to the timeline_out's rate to subtract.
            let dt_at_timeline_rate = klipp_core::RationalTime::from_secs_f64(
                dt_secs.max(0.0),
                old_timeline_out.rate,
            );
            old_timeline_out - dt_at_timeline_rate
        } else {
            old_timeline_out
        };

        clip.kind = klipp_timeline::ClipKind::Media { media_id: new_media_id };
        clip.source_out = new_source_out;
        clip.timeline_out = new_timeline_out;

        self.state.push_history(EditCommand::LegacyReplaceClipMedia {
            clip_id,
            old_media_id,
            new_media_id,
            old_source_out,
            new_source_out,
            old_timeline_out,
            new_timeline_out,
        });
        true
    }

    /// Wrapper around [`klipp_timeline::Sequence::try_swap_adjacent_clips`]
    /// that surfaces failure reasons as toasts and pushes a history
    /// entry on success. The actual validation + mutation logic lives
    /// in the timeline crate so it's testable without app scaffolding.
    fn try_swap_adjacent_clips(
        &mut self,
        clip_a: ClipId,
        clip_b: ClipId,
    ) -> bool {
        let seq = &mut self.state.sequences[self.state.active_sequence_idx];
        match seq.try_swap_adjacent_clips(clip_a, clip_b) {
            Ok(result) => {
                let linked_a_partner = result.partners.as_ref().map(|p| p.a_partner);
                let linked_b_partner = result.partners.as_ref().map(|p| p.b_partner);
                self.state.push_history(EditCommand::LegacySwapClips {
                    clip_a_id: clip_a,
                    a_old_in: result.a_old_in,
                    a_old_out: result.a_old_out,
                    clip_b_id: clip_b,
                    b_old_in: result.b_old_in,
                    b_old_out: result.b_old_out,
                    linked_a_partner,
                    linked_b_partner,
                });
                true
            }
            Err(reason) => {
                self.state.toast.show(reason);
                false
            }
        }
    }

    /// Check if a track uses magnetic editing (Primary, Connected, or ConnectedStoryline).
    /// Returns false when the magnetic timeline pref is disabled.
    pub(super) fn is_magnetic_track(&self, track_index: usize) -> bool {
        self.state.magnetic_timeline_enabled
            && matches!(
                self.state.sequences[self.state.active_sequence_idx].tracks.get(track_index).map(|t| &t.magnetic_role),
                Some(klipp_timeline::TrackRole::Primary | klipp_timeline::TrackRole::Connected | klipp_timeline::TrackRole::ConnectedStoryline)
            )
    }
}

/// What copied or cut clips leave on the system clipboard: a line
/// saying what was copied, so Ctrl+V reaches Klipp (see
/// `AppState::system_clipboard_text`) and a paste elsewhere shows text
/// rather than whatever was there before.
pub(super) fn clips_clipboard_text(count: usize) -> String {
    match count {
        1 => "1 clip copied in Klipp".into(),
        n => format!("{n} clips copied in Klipp"),
    }
}

/// A duplicate of `clip` placed right after it: a new id, its own copy of
/// every field, and a picture-truth clip's picture with it.
impl crate::state::AppState {
    /// A copy of `clip` put right after it on track `track_index`, one
    /// undo step with its node labels on the copy's nodes (#1753): the
    /// copy's id.
    pub(super) fn duplicate_after_on(&mut self, track_index: usize, clip: &klipp_timeline::Clip) -> klipp_core::ClipId {
        let new_clip = duplicate_after(clip);
        let new_id = new_clip.id;
        let seq = &self.sequences[self.active_sequence_idx];
        let carried = klipp_timeline::carry_node_labels(&seq.node_labels, clip, &new_clip);
        let batch = !carried.is_empty() && !self.active_history_mut().is_batching();
        if batch {
            self.active_history_mut().begin_batch();
        }
        let track = &mut self.sequences[self.active_sequence_idx].tracks[track_index];
        KlippApp::overwrite_insert_clip_on_track(track, new_clip.clone());
        self.push_history(EditCommand::LegacyPasteClip { clip: new_clip, track_index });
        if !carried.is_empty() {
            let seq = &mut self.sequences[self.active_sequence_idx];
            let old = seq.node_labels.clone();
            seq.node_labels.extend(carried);
            let new = seq.node_labels.clone();
            self.push_history(EditCommand::SetNodeLabels { old, new });
        }
        if batch {
            self.active_history_mut().end_batch_as("Duplicate");
        }
        new_id
    }
}

pub(super) fn duplicate_after(clip: &klipp_timeline::Clip) -> klipp_timeline::Clip {
    let new_id = klipp_core::ClipId::new();
    let timeline_in = clip.timeline_out;
    let dur = clip.duration();
    let mut new_clip = klipp_timeline::Clip {
        id: new_id,
        kind: clip.kind.clone(),
        timeline_in,
        timeline_out: timeline_in + dur,
        source_in: clip.source_in,
        source_out: clip.source_out,
        picture: klipp_timeline::ClipPictureParts {
            ofx_effects: clip.ofx_effects().to_vec(),
            builtin_effects: clip.builtin_effects().to_vec(),
            builtin_effects_enabled: clip.builtin_effects_enabled().to_vec(),
            opacity_keyframes: clip.opacity_keyframes().to_vec(),
            transform_pos_x_keyframes: clip.transform_pos_x_keyframes().to_vec(),
            transform_pos_y_keyframes: clip.transform_pos_y_keyframes().to_vec(),
            transform_scale_keyframes: clip.transform_scale_keyframes().to_vec(),
            transform_scale_y_keyframes: clip.transform_scale_y_keyframes().to_vec(),
            transform_rotation_keyframes: clip.transform_rotation_keyframes().to_vec(),
            transform_tilt_x_keyframes: clip.transform_tilt_x_keyframes().to_vec(),
            transform_tilt_y_keyframes: clip.transform_tilt_y_keyframes().to_vec(),
            effect_keyframes: clip.effect_keyframes().clone(),
            param_modifiers: clip.param_modifiers().clone(),
            grade_versions: clip.grade_versions().to_vec(),
            active_grade_version: clip.active_grade_version(),
            ..Default::default()
        }
        .into(),
        linked_clip_id: None,
        linked_title_asset_id: None,
        group_id: None,
        grade_group: None,
        magnetic_anchor: None,
        gain: clip.gain,
        volume: clip.volume,
        pan: clip.pan,
        fade_in_secs: clip.fade_in_secs,
        fade_out_secs: clip.fade_out_secs,
        fade_in_curve: clip.fade_in_curve,
        fade_out_curve: clip.fade_out_curve,
        channel_config: clip.channel_config,
        volume_keyframes: clip.volume_keyframes.clone(),
        pan_keyframes: clip.pan_keyframes.clone(),
        gain_keyframes: clip.gain_keyframes.clone(),
        matte_feather_keyframes: clip.matte_feather_keyframes.clone(),
        matte_choke_keyframes: clip.matte_choke_keyframes.clone(),
        clip_markers: clip.clip_markers.clone(),
        frame_glide: None,
        role: clip.role,
        role_source: clip.role_source,
        matched_to: None,
        speed: clip.speed,
        speed_keyframes: clip.speed_keyframes.clone(),
        keep_pitch: clip.keep_pitch,
        hold: None,
        source_rotation: clip.source_rotation,
        enabled: clip.enabled,
        solo: clip.solo,
        blend_mode: clip.blend_mode,
        depth: clip.depth,
        matte_source: clip.matte_source.clone(),
        matte_stencil: clip.matte_stencil.clone(),
        consumed_as_matte: clip.consumed_as_matte,
        transform_link: None,
        multicam_group: clip.multicam_group,
        multicam_angle: clip.multicam_angle,
        multicam_cuts: clip.multicam_cuts.clone(),
        multicam_sound: clip.multicam_sound,
        audio_effects: clip.audio_effects.clone(),
        audio_effect_keyframes: clip.audio_effect_keyframes.clone(),
        track_points: Vec::new(),
        surfaces: clip.surfaces.clone(),
        surface_pin: clip.surface_pin,
        name: clip.name.clone(),
        colour_label: clip.colour_label,
        grade_flag: Default::default(),
        precomp: None,
        baked_own: None,
    };
    new_clip.set_grade(clip.grade().clone());
    // A picture-truth clip's picture (its masks, mixes)
    // goes with the copy made from its fields.
    new_clip.take_picture_truth_of(clip);
    new_clip
}

#[cfg(test)]
mod duplicate_label_tests {
    use klipp_graph_format::path::{NodePath, Root};
    use klipp_timeline::{grade_group_id, GradeAccess, NodeLabel};

    /// #1753: a duplicated clip's grade node keeps its colour label on the
    /// copy, in the duplicate's one undo step: undone, the copy and its
    /// label go; redone, both come back.
    #[test]
    fn a_duplicate_keeps_its_node_labels_in_one_step() {
        let t = |s| klipp_core::RationalTime::from_secs_f64(s, 25);
        let mut clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0));
        let node = clip.edit_grade(|g| g.ensure_corrector());
        let on = |c: klipp_core::ClipId| NodePath::new(Root::Clip { clip: c.0 }, [grade_group_id(c), node]);
        let mut state = crate::state::AppState::default();
        state.sequences[0].tracks[0].clips.push(clip.clone());
        state.sequences[0].node_labels = vec![NodeLabel { node: on(clip.id), colour: 1 }];
        let copy = state.duplicate_after_on(0, &clip);
        assert_eq!(state.sequences[0].node_label(&on(copy)), 1, "the copy's node has the label");
        assert!(state.sequences[0].holds_node(&on(copy)));
        assert_eq!(state.sequences[0].node_label(&on(clip.id)), 1, "the original keeps its own");
        assert_eq!(state.active_history().undo_count(), 1, "one step");

        let step = |state: &mut crate::state::AppState, forward: bool| {
            let entry = if forward { state.active_history_mut().redo() } else { state.active_history_mut().undo() }.unwrap();
            let mut view = klipp_document::apply::DocumentView { sequences: &mut state.sequences, active: 0, media_pool: &mut state.media_pool, fps: &mut state.fps };
            if forward { view.apply_forward(&entry.command) } else { view.apply_inverse(&entry.command) }
        };
        step(&mut state, false);
        assert!(state.sequences[0].find_clip(copy).is_none());
        assert_eq!(state.sequences[0].node_labels.len(), 1, "only the original's label");
        step(&mut state, true);
        assert!(state.sequences[0].find_clip(copy).is_some());
        assert_eq!(state.sequences[0].node_label(&on(copy)), 1);
    }
}

#[cfg(test)]
mod linked_clipboard_tests {

    fn t(s: f64) -> klipp_core::Timecode {
        klipp_core::Timecode::from_secs_f64(s, 25)
    }

    /// #458: a clip cut or copied takes its linked sound, each from its
    /// own track; pasted, both land on their own tracks at the playhead,
    /// at their own offset, linked to each other and not to the originals.
    #[test]
    fn a_linked_pair_goes_to_the_clipboard_and_comes_back_linked() {
        let mut pool = klipp_media::MediaPool::new();
        let media = pool.add(klipp_media::MediaItem::synthetic_for_test("/tmp/pair.mov".into(), None));
        let mut seq = klipp_timeline::Sequence::new_default();
        let mut v = klipp_timeline::Clip::media(media, t(2.0), t(6.0), t(0.0), t(4.0));
        // A J-cut: the sound starts 0.4 s before the picture.
        let mut a = klipp_timeline::Clip::media(media, t(1.6), t(6.0), t(0.0), t(4.4));
        v.linked_clip_id = Some(a.id);
        a.linked_clip_id = Some(v.id);
        let (vid, aid) = (v.id, a.id);
        let mut vt = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        vt.clips = vec![v];
        let mut at = klipp_timeline::Track::new("A1", klipp_timeline::TrackKind::Audio);
        at.clips = vec![a];
        seq.tracks = vec![vt, at];

        let cb = crate::clip_clipboard::copy(&seq, &[vid]).expect("on the clipboard");
        assert_eq!(cb.clips.iter().map(|c| (c.clip.id, c.track)).collect::<Vec<_>>(), vec![(aid, 1), (vid, 0)], "both, each from its own track, in time order");

        let pasted = crate::clip_clipboard::paste(&seq, &cb, t(10.0), crate::clip_clipboard::PasteMode::Overwrite);
        let find = |id: klipp_core::ClipId| pasted.sequence.tracks.iter().enumerate().find_map(|(ti, tr)| tr.clips.iter().find(|c| c.id == id).map(|c| (c.clone(), ti)));
        let (pa, ta) = find(pasted.ids[0]).unwrap();
        let (pv, tv) = find(pasted.ids[1]).unwrap();
        assert_eq!((ta, tv), (1, 0));
        assert_eq!((pa.timeline_in, pv.timeline_in), (t(10.0), t(10.4)), "the J-cut's offset kept");
        assert_eq!((pa.linked_clip_id, pv.linked_clip_id), (Some(pv.id), Some(pa.id)), "linked to each other");
        assert!(pa.id != aid && pv.id != vid);
        // A clip with no partner comes alone.
        let mut lone = seq.clone();
        lone.tracks[0].clips[0].linked_clip_id = None;
        assert_eq!(crate::clip_clipboard::copy(&lone, &[vid]).unwrap().clips.len(), 1);
    }
}

/// `clip_ids` and the clips linked to them, each once: a picture's sound,
/// or a sound's picture (#920).
fn with_linked_partners(seq: &klipp_timeline::Sequence, clip_ids: &[klipp_core::ClipId]) -> Vec<klipp_core::ClipId> {
    let mut out = clip_ids.to_vec();
    for clip in seq.all_clips() {
        let partner = match clip.linked_clip_id {
            Some(other) if clip_ids.contains(&clip.id) => Some(other),
            Some(other) if clip_ids.contains(&other) => Some(clip.id),
            _ => None,
        };
        if let Some(id) = partner.filter(|id| !out.contains(id)) {
            out.push(id);
        }
    }
    out
}

#[cfg(test)]
mod linked_partner_tests {
    use super::with_linked_partners;

    #[test]
    fn a_picture_brings_its_sound_and_a_sound_its_picture() {
        let tc = |v| klipp_core::RationalTime { value: v, rate: 24 };
        let mut seq = klipp_timeline::Sequence::new_default();
        let media = klipp_core::MediaId::new();
        let mut picture = klipp_timeline::Clip::media(media, tc(0), tc(24), tc(0), tc(24));
        let mut sound = klipp_timeline::Clip::media(media, tc(0), tc(24), tc(0), tc(24));
        picture.linked_clip_id = Some(sound.id);
        sound.linked_clip_id = Some(picture.id);
        let alone = klipp_timeline::Clip::media(media, tc(24), tc(48), tc(0), tc(24));
        let (p, s, a) = (picture.id, sound.id, alone.id);
        seq.tracks[0].clips = vec![picture, alone];
        seq.tracks.last_mut().unwrap().clips = vec![sound];
        assert_eq!(with_linked_partners(&seq, &[p]), vec![p, s]);
        assert_eq!(with_linked_partners(&seq, &[s]), vec![s, p]);
        assert_eq!(with_linked_partners(&seq, &[p, s]), vec![p, s], "each once");
        assert_eq!(with_linked_partners(&seq, &[a]), vec![a]);
    }
}
