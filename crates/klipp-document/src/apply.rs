//! A command applied forward or in reverse over the document, with the
//! side effects the host must perform afterwards returned rather than
//! done: the document does not know the engine or the viewer.
//!
//! Moved verbatim from the UI's `apply_edit_forward` /
//! `apply_edit_inverse`; every `self.state.sequences[active]` became
//! `self.sequences[self.active]`, every engine sync a flag.

use crate::history::EditCommand;
use klipp_core::{ClipId, Fps, MediaId};
use klipp_media::MediaPool;
use klipp_timeline::{GradeAccess, Sequence};

/// The document as a command runs against it: the sequences, which one
/// is active, the media pool, and the project frame rate (which an
/// import may set).
pub struct DocumentView<'a> {
    pub sequences: &'a mut Vec<Sequence>,
    pub active: usize,
    pub media_pool: &'a mut MediaPool,
    pub fps: &'a mut Fps,
}

/// What a host does once a command has run: push the sequence or the
/// media pool to the engine, tell it the sequence changed, re-seat the
/// playhead, re-render a title's overlay, make a file's pictures again.
/// Merged across a batch, so a host performs each once.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Effects {
    pub sync_sequence: bool,
    pub sync_media_pool: bool,
    pub notify_engine: bool,
    pub set_playhead: bool,
    pub rerender_titles: Vec<ClipId>,
    /// Files whose decode settings changed (a camera encoding), so their
    /// thumbnail and filmstrip are made again for the new ones.
    pub refresh_media_pictures: Vec<MediaId>,
    /// Files to read again, for what an edit pointed them at (#2327).
    pub reprobe_media: Vec<MediaId>,
    /// The project's colour set-up as a command left it (#1875), for the
    /// host to take on and hand to the engine.
    pub project_colour: Option<Box<klipp_core::ProjectColour>>,
}

impl Effects {
    pub fn merge(&mut self, other: Effects) {
        self.sync_sequence |= other.sync_sequence;
        self.sync_media_pool |= other.sync_media_pool;
        self.notify_engine |= other.notify_engine;
        self.set_playhead |= other.set_playhead;
        self.rerender_titles.extend(other.rerender_titles);
        self.refresh_media_pictures.extend(other.refresh_media_pictures);
        self.reprobe_media.extend(other.reprobe_media);
        if other.project_colour.is_some() {
            self.project_colour = other.project_colour;
        }
    }
}

/// A converted clip's picture put back (#1875), in whichever sequence
/// holds it. One no longer there (its sequence or the clip gone) is said,
/// never passed over quietly.
fn put_picture(sequences: &mut [Sequence], grade: &crate::history::ConvertedGrade, before: bool) {
    let clip = sequences
        .iter_mut()
        .filter(|s| s.id == grade.sequence)
        .flat_map(|s| s.tracks.iter_mut())
        .find_map(|t| t.find_clip_mut(grade.clip));
    match clip {
        Some(clip) => clip.restore_picture(if before { &grade.edit.before } else { &grade.edit.after }),
        None => tracing::warn!(clip = ?grade.clip, "a converted clip is no longer in its sequence; its grade was not put back"),
    }
}

/// One of the sequence's own grades put back as an edit's undo or redo
/// stored it. It was the sequence's when the edit was made; one that no
/// longer is (its group gone) is said, never passed over quietly.
fn set_root_grade(seq: &mut klipp_timeline::Sequence, root: klipp_graph_format::path::Root, grade: Option<&klipp_nodes::NodeGraph>) {
    if let Err(why) = seq.set_root_grade(root, grade.cloned()) {
        debug_assert!(false, "a grade step's root is gone: {why}");
        tracing::warn!(?root, %why, "undo/redo: a grade step's root is gone; the grade is left as it is");
    }
}

/// A graph edit lands on the clip it names, or, under the sequence's
/// timeline-grade id, on the timeline grade, under a grade group stage's
/// id, on that stage, or under a track's grade id, on that track's grade
/// (#1408): the Colour workspace edits those through
/// the same node commands as a clip's grade.
fn set_node_graph(seq: &mut klipp_timeline::Sequence, clip_id: ClipId, graph: &klipp_nodes::NodeGraph) {
    for track in &mut seq.tracks {
        if let Some(clip) = track.find_clip_mut(clip_id) {
            clip.set_grade(graph.clone());
            return;
        }
    }
    if clip_id == seq.timeline_grade_clip_id() {
        seq.timeline_grade_slot().set(Some(graph.clone()));
        return;
    }
    // A grade group's pre-clip or post-clip grade, under its stage's
    // editing id (#237).
    if let Some((group, stage)) = seq.grade_group_stage_of(clip_id) {
        if let Some(g) = seq.grade_groups.iter_mut().find(|g| g.id == group) {
            g.stage_slot(stage).set(Some(graph.clone()));
        }
        return;
    }
    // A track's grade, under its track's editing id (#1408).
    if let Some(track) = seq.track_grade_of(clip_id) {
        if let Some(t) = seq.tracks.iter_mut().find(|t| t.id == track) {
            t.effects.grade_slot().set(Some(graph.clone()));
        }
    }
}

/// The format `graph`'s Input Transform holds, if it has one.
fn input_transform(graph: &klipp_nodes::NodeGraph) -> Option<Option<klipp_core::LogFormat>> {
    graph.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::InputTransform).map(|n| n.params.log_format)
}

/// `graph` given `to`'s camera encoding and nothing else, for a clip
/// graded since the step left it at `from`: its Input Transform's format
/// set to `to`'s, or, when `to` has none (the step put it in), taken out
/// as the splice put it in: what it feeds fed from the Source again, in
/// place, and the Source's edge into it gone. Every other node stays as
/// the later edits left it.
///
/// The Input Transform is the step's only while it is still as the step
/// left it, its node and its wiring. One edited since (another format,
/// its params, or rewired) is the later work, and is left as it is.
fn encoding_only(graph: &mut klipp_nodes::NodeGraph, from: &klipp_nodes::NodeGraph, to: &klipp_nodes::NodeGraph) {
    let it_of = |g: &klipp_nodes::NodeGraph| g.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::InputTransform).cloned();
    let wiring = |g: &klipp_nodes::NodeGraph, id| g.edges.iter().filter(|e| e.from_node == id || e.to_node == id).map(|e| (e.from_node, e.from_port, e.to_node, e.to_port)).collect::<Vec<_>>();
    let (Some(now), Some(was)) = (it_of(graph), it_of(from)) else { return };
    if now != was || wiring(graph, now.id) != wiring(from, was.id) {
        return;
    }
    let id = now.id;
    match input_transform(to) {
        Some(format) => {
            if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == id) {
                node.params.log_format = format;
            }
        }
        None => {
            let fed: Vec<_> = graph.edges.iter().filter(|e| e.to_node == id).map(|e| (e.from_node, e.from_port)).collect();
            let [(source, source_port)] = fed[..] else { return };
            graph.edges.retain(|e| e.to_node != id);
            for edge in graph.edges.iter_mut().filter(|e| e.from_node == id) {
                edge.from_node = source;
                edge.from_port = source_port;
            }
            graph.nodes.retain(|n| n.id != id);
            graph.node_positions.remove(&id);
        }
    }
}

impl DocumentView<'_> {
    /// A file's camera encoding set to `format` (with whether the user set
    /// it), and each of its clips, in whichever sequence holds it, from the
    /// grade the step left (`from`) to the one it goes to (`to`).
    ///
    /// The step spans sequences, but each sequence keeps its own history,
    /// so a clip may have been graded since, in a sequence whose stack
    /// knows nothing of this step. A clip still as the step left it takes
    /// `to` whole; one graded since takes only `to`'s encoding, and keeps
    /// the later work.
    fn set_media_log_format<'g>(&mut self, media_id: Option<MediaId>, (format, user_set): (klipp_core::LogFormat, bool), grades: impl Iterator<Item = (ClipId, &'g klipp_nodes::NodeGraph, &'g klipp_nodes::NodeGraph)>) -> Effects {
        if let Some(item) = media_id.and_then(|id| self.media_pool.get_mut(&id)) {
            item.log_format = format;
            item.log_format_user_set = user_set;
        }
        // NodeGraph has no PartialEq: compared as saved.
        let same = |a: &klipp_nodes::NodeGraph, b: &klipp_nodes::NodeGraph| serde_json::to_value(a).ok() == serde_json::to_value(b).ok();
        for (clip_id, from, to) in grades {
            if let Some(clip) = self.sequences.iter_mut().find_map(|s| s.all_clips_mut().find(|c| c.id == clip_id)) {
                if same(clip.grade(), from) {
                    clip.set_grade(to.clone());
                } else if let (None, Some(Some(format))) = (input_transform(clip.grade()), input_transform(to)) {
                    // Graded since without one: put the Input Transform in,
                    // as the step did.
                    clip.set_input_log_format(format);
                } else {
                    clip.edit_grade(|graph| encoding_only(graph, from, to));
                }
            }
        }
        Effects { sync_sequence: true, sync_media_pool: media_id.is_some(), notify_engine: true, refresh_media_pictures: media_id.into_iter().collect(), ..Default::default() }
    }

    /// Media item `media_id` pointed at `path`, there or not as `offline`
    /// says: the pool synced and its pictures made again from that file.
    fn relink(&mut self, media_id: MediaId, path: &std::path::Path, offline: bool) -> Effects {
        if let Some(item) = self.media_pool.get_mut(&media_id) {
            item.path = path.to_path_buf();
            item.offline = offline;
        }
        Effects { sync_sequence: true, sync_media_pool: true, notify_engine: true, refresh_media_pictures: vec![media_id], ..Default::default() }
    }

    pub fn apply_inverse(&mut self, cmd: &EditCommand) -> Effects {
        let mut effects = Effects::default();
        match cmd {
            EditCommand::SetProjectColour { old, grades, .. } => {
                for g in grades.iter().rev() {
                    put_picture(self.sequences, g, true);
                }
                effects.project_colour = Some(old.clone());
                effects.sync_sequence = true;
            }
            EditCommand::AddSequences { sequences } => {
                // Taken away from the end, past the active sequence: one
                // the user has opened since stays, as the active index is
                // not this view's to move.
                for s in sequences.iter().rev() {
                    if let Some(i) = self.sequences.iter().position(|have| have.id == s.id).filter(|i| *i > self.active) {
                        self.sequences.remove(i);
                    }
                }
                effects.sync_sequence = true;
            }
            EditCommand::Batch(commands) => {
                for c in commands.iter().rev() {
                    effects.merge(self.apply_inverse(c));
                }
            }
            EditCommand::Named { edit, .. } => effects.merge(self.apply_inverse(edit)),
            EditCommand::AddTrack { index, track } => {
                let seq = &mut self.sequences[self.active];
                if seq.tracks.get(*index).is_some_and(|t| t.id == track.id) {
                    seq.tracks.remove(*index);
                    seq.renumber_tracks();
                }
            }
            EditCommand::RemoveTrack { index, track } => {
                let seq = &mut self.sequences[self.active];
                let idx = (*index).min(seq.tracks.len());
                seq.tracks.insert(idx, track.clone());
                seq.renumber_tracks();
            }
            EditCommand::SetTrackSettings { changes } => {
                let seq = &mut self.sequences[self.active];
                for (id, old, _) in changes {
                    if let Some(t) = seq.tracks.iter_mut().find(|t| t.id == *id) {
                        t.set_settings(old);
                    }
                }
            }
            EditCommand::SetBrief { old, .. } => {
                self.sequences[self.active].brief = old.as_deref().cloned();
            }
            EditCommand::SetEditPlan { old, .. } => {
                self.sequences[self.active].edit_plan = old.as_deref().cloned();
            }
            EditCommand::SetReviews { old, .. } => {
                self.sequences[self.active].reviews = old.clone();
            }
            EditCommand::SetClipMatch { clip_id, old, .. } => {
                if let Some(c) = self.sequences[self.active].tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).find(|c| c.id == *clip_id) {
                    c.matched_to = *old;
                }
            }
            EditCommand::SetClipRole { clip_id, old, old_source, .. } => {
                if let Some(c) = self.sequences[self.active].tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).find(|c| c.id == *clip_id) {
                    c.role = *old;
                    c.role_source = *old_source;
                }
            }
            EditCommand::ProposeClipRoles { changes } => {
                for (clip_id, old, _) in changes {
                    if let Some(c) = self.sequences[self.active].tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).find(|c| c.id == *clip_id) {
                        c.role = *old;
                    }
                }
            }
            EditCommand::SetSequenceCamera { old, .. } => {
                self.sequences[self.active].camera = (**old).clone();
            }
            EditCommand::SetSequenceTitleNits { old, .. } => {
                self.sequences[self.active].title_nits = *old;
            }
            EditCommand::SetNodeLabels { old, .. } => {
                self.sequences[self.active].node_labels = old.clone();
            }
            EditCommand::SetClipDepth { clip_id, old, .. } => {
                if let Some(c) = self.sequences[self.active].tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).find(|c| c.id == *clip_id) {
                    c.depth = *old;
                }
            }
            EditCommand::SetTemplateInstance { clip_id, old, .. } => {
                if let Some(c) = self.sequences[self.active].tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).find(|c| c.id == *clip_id) {
                    if let klipp_timeline::ClipKind::TemplatePrecomposition { template_instance, .. } = &mut c.kind {
                        *template_instance = (**old).clone();
                    }
                }
            }
            EditCommand::SetMarkers { old, .. } => {
                self.sequences[self.active].markers = old.clone();
            }
            EditCommand::SetTracks { old, .. } => {
                self.sequences[self.active].tracks = old.clone();
            }
            EditCommand::SetTrack { track_index, old, .. } => {
                if let Some(t) = self.sequences[self.active].tracks.get_mut(*track_index).filter(|t| t.id == old.id) {
                    *t = (**old).clone();
                }
            }
            EditCommand::SetTrackContents { track_index, old, .. } => {
                if let Some(track) = self.sequences[self.active].tracks.get_mut(*track_index) {
                    track.clips = old.clips.clone();
                    track.transitions = old.transitions.clone();
                }
            }
            EditCommand::SetGradeGroups { old, .. } => {
                self.sequences[self.active].grade_groups = old.clone();
            }
            EditCommand::SetClipGradeGroups { changes } => {
                let seq = &mut self.sequences[self.active];
                for (clip_id, old, _) in changes {
                    if let Some(c) = seq.all_clips_mut().find(|c| c.id == *clip_id) {
                        c.grade_group = *old;
                    }
                }
            }
            EditCommand::SetClipLabels { changes, .. } => {
                let seq = &mut self.sequences[self.active];
                for (clip_id, old, _) in changes {
                    if let Some(c) = seq.all_clips_mut().find(|c| c.id == *clip_id) {
                        c.set_own_label(old);
                    }
                }
            }
            EditCommand::SetSequenceFormat { old, .. } => {
                let seq = &mut self.sequences[self.active];
                seq.resolution = old.0;
                seq.fps = old.1;
                *self.fps = old.1;
            }
            EditCommand::AddTitleClip { clip, track_index, created_track, .. } => {
                let seq = &mut self.sequences[self.active];
                if *created_track {
                    // Remove the entire auto-created track
                    if *track_index < seq.tracks.len() {
                        seq.tracks.remove(*track_index);
                    }
                } else if let Some(track) = seq.tracks.get_mut(*track_index) {
                    track.remove_clip(clip.id);
                }
            }
            EditCommand::LegacyAppendClip { clip, track_index } => {
                if let Some(track) = self.sequences[self.active].tracks.get_mut(*track_index) {
                    track.remove_clip(clip.id);
                }
            }
            EditCommand::LinkedImport { video, audio, created_video_track_at, created_audio_track_at, autoset } => {
                let seq = &mut self.sequences[self.active];
                if let Some(fps) = crate::history::apply_linked_import_inverse(
                    seq, video, audio, *created_video_track_at, *created_audio_track_at, autoset,
                ) {
                    *self.fps = fps;
                }
            }
            EditCommand::LegacyRemoveClip { clip, track_index, position } => {
                if let Some(track) = self.sequences[self.active].tracks.get_mut(*track_index) {
                    let pos = (*position).min(track.clips.len());
                    track.clips.insert(pos, clip.clone());
                }
            }
            EditCommand::LegacyRippleRemove { removed_clip, track_index, position, shifted_clips } => {
                if let Some(track) = self.sequences[self.active].tracks.get_mut(*track_index) {
                    // Restore shifted clips to their original positions
                    for (cid, old_in, old_out) in shifted_clips {
                        if let Some(c) = track.clips.iter_mut().find(|c| c.id == *cid) {
                            c.timeline_in = *old_in;
                            c.timeline_out = *old_out;
                        }
                    }
                    // Re-insert the removed clip
                    let pos = (*position).min(track.clips.len());
                    track.clips.insert(pos, removed_clip.clone());
                    track.sort_clips();
                }
            }
            EditCommand::LegacyMoveClip { clip_id, old_timeline_in, old_timeline_out, linked_partner, .. } => {
                if let Some(track) = self.sequences[self.active].tracks.iter_mut()
                    .find(|t| t.clips.iter().any(|c| c.id == *clip_id))
                {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.timeline_in = *old_timeline_in;
                        clip.timeline_out = *old_timeline_out;
                    }
                    track.sort_clips();
                }
                if let Some(p) = linked_partner {
                    if let Some(track) = self.sequences[self.active].tracks.iter_mut()
                        .find(|t| t.clips.iter().any(|c| c.id == p.partner_id))
                    {
                        if let Some(clip) = track.find_clip_mut(p.partner_id) {
                            clip.timeline_in = p.old_timeline_in;
                            clip.timeline_out = p.old_timeline_out;
                        }
                        track.sort_clips();
                    }
                }
            }
            EditCommand::LegacySwapClips {
                clip_a_id, a_old_in, a_old_out,
                clip_b_id, b_old_in, b_old_out,
                linked_a_partner, linked_b_partner,
            } => {
                // Restore both primaries to their pre-swap positions.
                for (id, t_in, t_out) in [
                    (*clip_a_id, *a_old_in, *a_old_out),
                    (*clip_b_id, *b_old_in, *b_old_out),
                ] {
                    if let Some(track) = self.sequences[self.active].tracks.iter_mut()
                        .find(|t| t.clips.iter().any(|c| c.id == id))
                    {
                        if let Some(clip) = track.find_clip_mut(id) {
                            clip.timeline_in = t_in;
                            clip.timeline_out = t_out;
                        }
                        track.sort_clips();
                    }
                }
                // Restore linked partners if the swap propagated to them.
                for partner in [linked_a_partner, linked_b_partner].iter().filter_map(|p| p.as_ref()) {
                    let (pid, p_in, p_out) = partner;
                    if let Some(track) = self.sequences[self.active].tracks.iter_mut()
                        .find(|t| t.clips.iter().any(|c| c.id == *pid))
                    {
                        if let Some(clip) = track.find_clip_mut(*pid) {
                            clip.timeline_in = *p_in;
                            clip.timeline_out = *p_out;
                        }
                        track.sort_clips();
                    }
                }
            }
            EditCommand::LegacyReplaceClipMedia {
                clip_id,
                old_media_id,
                old_source_out,
                old_timeline_out,
                ..
            } => {
                if let Some(track) = self.sequences[self.active].tracks.iter_mut()
                    .find(|t| t.clips.iter().any(|c| c.id == *clip_id))
                {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.kind = klipp_timeline::ClipKind::Media { media_id: *old_media_id };
                        clip.source_out = *old_source_out;
                        clip.timeline_out = *old_timeline_out;
                    }
                }
            }
            EditCommand::LegacyTrimClip {
                clip_id,
                old_source_in, old_source_out,
                old_timeline_in, old_timeline_out, ..
            } => {
                if let Some(track) = self.sequences[self.active].tracks.iter_mut()
                    .find(|t| t.clips.iter().any(|c| c.id == *clip_id))
                {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.source_in = *old_source_in;
                        clip.source_out = *old_source_out;
                        clip.timeline_in = *old_timeline_in;
                        clip.timeline_out = *old_timeline_out;
                    }
                }
            }
            EditCommand::LegacyRippleTrimClip {
                clip_id,
                old_source_in, old_source_out,
                old_timeline_in, old_timeline_out,
                shifted_clips, ..
            } => {
                if let Some(track) = self.sequences[self.active].tracks.iter_mut()
                    .find(|t| t.clips.iter().any(|c| c.id == *clip_id))
                {
                    // Restore the trimmed clip to its original values
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.source_in = *old_source_in;
                        clip.source_out = *old_source_out;
                        clip.timeline_in = *old_timeline_in;
                        clip.timeline_out = *old_timeline_out;
                    }
                    // Restore downstream clips to their pre-ripple positions
                    for (cid, orig_in, orig_out) in shifted_clips {
                        if let Some(c) = track.find_clip_mut(*cid) {
                            c.timeline_in = *orig_in;
                            c.timeline_out = *orig_out;
                        }
                    }
                    track.sort_clips();
                }
            }
            EditCommand::LegacyRollTrimClip {
                clip_a_id, clip_b_id,
                old_a_source_out, old_a_timeline_out,
                old_b_source_in, old_b_timeline_in, ..
            } => {
                let seq = &mut self.sequences[self.active];
                for track in &mut seq.tracks {
                    if let Some(c) = track.find_clip_mut(*clip_a_id) {
                        c.source_out = *old_a_source_out;
                        c.timeline_out = *old_a_timeline_out;
                    }
                    if let Some(c) = track.find_clip_mut(*clip_b_id) {
                        c.source_in = *old_b_source_in;
                        c.timeline_in = *old_b_timeline_in;
                    }
                }
            }
            EditCommand::LegacySlipClip {
                clip_id,
                old_source_in, old_source_out, new_source_in, ..
            } => {
                // Undo slip: restore original source in/out
                let seq = &mut self.sequences[self.active];
                // Also undo linked partner
                let linked_id = seq.tracks.iter().flat_map(|t| t.clips.iter())
                    .find(|c| c.id == *clip_id).and_then(|c| c.linked_clip_id);
                for track in &mut seq.tracks {
                    if let Some(c) = track.find_clip_mut(*clip_id) {
                        c.source_in = *old_source_in;
                        c.source_out = *old_source_out;
                    }
                }
                // The partner back by what it moved: its own range, a J or L
                // cut's offset kept (#458).
                if let Some(lid) = linked_id {
                    let back = *old_source_in - *new_source_in;
                    for track in &mut self.sequences[self.active].tracks {
                        if let Some(c) = track.find_clip_mut(lid) {
                            c.source_in += back;
                            c.source_out += back;
                        }
                    }
                }
            }
            EditCommand::LegacySlideClip {
                clip_id,
                old_timeline_in, old_timeline_out,
                left_clip_id, old_left_out,
                right_clip_id, old_right_in, ..
            } => {
                // Undo slide: restore original timeline positions for clip and neighbors
                let seq = &mut self.sequences[self.active];
                // Also undo linked partner
                let linked_id = seq.tracks.iter().flat_map(|t| t.clips.iter())
                    .find(|c| c.id == *clip_id).and_then(|c| c.linked_clip_id);
                let delta = *old_timeline_in - seq.tracks.iter().flat_map(|t| t.clips.iter())
                    .find(|c| c.id == *clip_id).map(|c| c.timeline_in).unwrap_or(*old_timeline_in);
                for track in &mut seq.tracks {
                    if let Some(c) = track.find_clip_mut(*clip_id) {
                        c.timeline_in = *old_timeline_in;
                        c.timeline_out = *old_timeline_out;
                    }
                    if let Some(lid) = left_clip_id {
                        if let (Some(c), Some(old_out)) = (track.find_clip_mut(*lid), old_left_out) {
                            // Restore left neighbor's out point (and source_out by same delta)
                            let current_out = c.timeline_out;
                            let dt = *old_out - current_out;
                            c.timeline_out = *old_out;
                            c.source_out += dt;
                        }
                    }
                    if let Some(rid) = right_clip_id {
                        if let (Some(c), Some(old_in)) = (track.find_clip_mut(*rid), old_right_in) {
                            let current_in = c.timeline_in;
                            let dt = *old_in - current_in;
                            c.timeline_in = *old_in;
                            c.source_in += dt;
                        }
                    }
                    track.sort_clips();
                }
                // Undo linked partner
                if let Some(lid) = linked_id {
                    for track in &mut self.sequences[self.active].tracks {
                        if let Some(c) = track.find_clip_mut(lid) {
                            let dur = c.duration();
                            c.timeline_in += delta;
                            c.timeline_out = c.timeline_in + dur;
                        }
                    }
                }
            }
            EditCommand::LegacyBladeClip { track_index, original_clip, clip_a, clip_b, position } => {
                if let Some(track) = self.sequences[self.active].tracks.get_mut(*track_index) {
                    // Undo: remove clip_a and clip_b, insert original
                    track.clips.retain(|c| c.id != clip_a.id && c.id != clip_b.id);
                    let pos = (*position).min(track.clips.len());
                    track.clips.insert(pos, original_clip.clone());
                    track.sort_clips();
                }
            }
            EditCommand::LegacySplitAtSceneMarkers { track_index, original_clip, position, new_clips } => {
                if let Some(track) = self.sequences[self.active].tracks.get_mut(*track_index) {
                    let new_ids: std::collections::HashSet<_> = new_clips.iter().map(|c| c.id).collect();
                    track.clips.retain(|c| !new_ids.contains(&c.id));
                    let pos = (*position).min(track.clips.len());
                    track.clips.insert(pos, original_clip.clone());
                    track.sort_clips();
                }
            }
            EditCommand::LegacyCutClip { clip, track_index, position, shifted_clips, .. } => {
                // Undo cut = re-insert clip + restore shifted positions
                if let Some(track) = self.sequences[self.active].tracks.get_mut(*track_index) {
                    for (cid, old_in, old_out) in shifted_clips {
                        if let Some(c) = track.clips.iter_mut().find(|c| c.id == *cid) {
                            c.timeline_in = *old_in;
                            c.timeline_out = *old_out;
                        }
                    }
                    let pos = (*position).min(track.clips.len());
                    track.clips.insert(pos, clip.clone());
                    track.sort_clips();
                }
            }
            EditCommand::LegacyInsertEdit { edit, audio, created_audio_track_at } => {
                let seq = &mut self.sequences[self.active];
                crate::history::apply_insert_edit_inverse(seq, edit, audio, *created_audio_track_at);
            }
            EditCommand::LegacyPasteClip { clip, track_index } => {
                // Undo paste = remove the pasted clip
                if let Some(track) = self.sequences[self.active].tracks.get_mut(*track_index) {
                    track.remove_clip(clip.id);
                }
            }
            EditCommand::LegacyMoveClipToTrack { clip, old_timeline_in, old_timeline_out, from_track, to_track, linked_partner, .. } => {
                // Undo: move clip back from to_track to from_track with original position
                if let Some(dst) = self.sequences[self.active].tracks.get_mut(*to_track) {
                    dst.remove_clip(clip.id);
                }
                if let Some(src) = self.sequences[self.active].tracks.get_mut(*from_track) {
                    let mut restored = clip.clone();
                    restored.timeline_in = *old_timeline_in;
                    restored.timeline_out = *old_timeline_out;
                    src.clips.push(restored);
                    src.sort_clips();
                }
                if let Some(p) = linked_partner {
                    if let Some(track) = self.sequences[self.active].tracks.iter_mut()
                        .find(|t| t.clips.iter().any(|c| c.id == p.partner_id))
                    {
                        if let Some(clip) = track.find_clip_mut(p.partner_id) {
                            clip.timeline_in = p.old_timeline_in;
                            clip.timeline_out = p.old_timeline_out;
                        }
                        track.sort_clips();
                    }
                }
            }
            EditCommand::Magnetic(cmd) => {
                klipp_timeline::magnetic::MagneticEditor::apply_inverse(
                    &mut self.sequences[self.active], cmd);
            }
            EditCommand::SourceLinkedInsert {
                video_command,
                audio_track_index,
                audio_clips_before,
                ..
            } => {
                klipp_timeline::magnetic::MagneticEditor::apply_inverse(
                    &mut self.sequences[self.active],
                    video_command,
                );
                if let Some(track) = self.sequences[self.active]
                    .tracks
                    .get_mut(*audio_track_index)
                {
                    track.clips = audio_clips_before.clone();
                }
            }
            EditCommand::AddMediaToPool { item } => {
                // Undo import: remove the item from pool
                self.media_pool.remove(&item.id);
                effects.sync_media_pool = true;
            }
            EditCommand::RemoveMediaFromPool { item, .. } => {
                // Undo delete: re-insert the full MediaItem snapshot
                self.media_pool.add(item.clone());
                effects.sync_media_pool = true;
            }
            EditCommand::CreateBin { bin } => {
                self.media_pool.bins.retain(|b| b.id != bin.id);
            }
            EditCommand::MoveMediaToBin { media_id, from_bin, from_bin_id, .. } => {
                // Undo: restore to original bin (both legacy string and bin_id)
                if let Some(item) = self.media_pool.get_mut(media_id) {
                    item.bin = from_bin.clone();
                    item.bin_id = *from_bin_id;
                }
            }
            EditCommand::SetMediaTags { media_id, old_tags, .. } => {
                if let Some(item) = self.media_pool.get_mut(media_id) {
                    item.tags = old_tags.clone();
                }
            }
            EditCommand::SetMediaRating { media_id, old_rating, .. } => {
                if let Some(item) = self.media_pool.get_mut(media_id) {
                    item.rating = *old_rating;
                }
            }
            EditCommand::SetMediaColorLabel { media_id, old_label, .. } => {
                if let Some(item) = self.media_pool.get_mut(media_id) {
                    item.color_label = klipp_media::ColorLabel::from_u8(*old_label);
                }
            }
            EditCommand::RelinkMedia { media_id, old_path, old_offline, old_item, .. } => {
                effects.merge(self.relink(*media_id, old_path, *old_offline));
                // The old file's details, as they were.
                if let Some(item) = self.media_pool.get_mut(media_id) {
                    item.take_probed_details(old_item);
                }
            }
            EditCommand::RenameBin { old_name, new_name } => {
                // Undo: rename Bin record back from new_name to old_name
                for bin in &mut self.media_pool.bins {
                    if bin.name == *new_name {
                        bin.name = old_name.clone();
                        break;
                    }
                }
                // Undo: restore legacy item.bin field
                let ids: Vec<MediaId> = self.media_pool.all()
                    .filter(|m| m.bin.as_deref() == Some(new_name.as_str()))
                    .map(|m| m.id)
                    .collect();
                for id in ids {
                    if let Some(item) = self.media_pool.get_mut(&id) {
                        item.bin = Some(old_name.clone());
                        // bin_id intentionally unchanged — rename doesn't change identity
                    }
                }
            }
            EditCommand::BatchRenameMediaItems { changes } => {
                // Undo: restore every old filename.
                for (media_id, old_filename, _new) in changes {
                    if let Some(item) = self.media_pool.get_mut(media_id) {
                        item.filename = old_filename.clone();
                    }
                }
            }
            EditCommand::CreateSubclip { subclip, parent_id: _ } => {
                self.media_pool.remove(&subclip.id);
            }
            EditCommand::MoveMarkers { moved, dropped } => {
                let markers = &mut self.sequences[self.active].markers;
                for mv in moved {
                    if let Some(m) = markers.iter_mut().find(|m| m.id == mv.id) {
                        m.position = mv.old_position;
                        m.duration = mv.old_duration;
                    }
                }
                for (index, marker) in dropped {
                    if !markers.iter().any(|m| m.id == marker.id) {
                        let at = (*index).min(markers.len());
                        markers.insert(at, marker.clone());
                    }
                }
            }
            EditCommand::SetRange { old_in, old_out, .. } => {
                let seq = &mut self.sequences[self.active];
                seq.in_point = *old_in;
                seq.out_point = *old_out;
            }
            EditCommand::AddTransition { track_index, transition, clip_a_id, clip_b_id, old_fade_out_a, old_fade_in_b, replaced } => {
                if let Some(track) = self.sequences[self.active].tracks.get_mut(*track_index) {
                    track.transitions.retain(|t| t.id != transition.id);
                    if let Some(prev) = replaced {
                        track.transitions.push(*prev);
                    }
                    if let Some(c) = track.clips.iter_mut().find(|c| c.id == *clip_a_id) {
                        c.fade_out_secs = *old_fade_out_a;
                    }
                    if let Some(c) = track.clips.iter_mut().find(|c| c.id == *clip_b_id) {
                        c.fade_in_secs = *old_fade_in_b;
                    }
                }
            }
            EditCommand::RetargetTransition { track_index, transition_id, old_a, .. } => {
                if let Some(t) = self.sequences[self.active].tracks.get_mut(*track_index).and_then(|t| t.transitions.iter_mut().find(|t| t.id == *transition_id)) {
                    t.clip_a_id = *old_a;
                }
            }
            EditCommand::RemoveTransition { track_index, transition, old_fade_out_a, old_fade_in_b } => {
                if let Some(track) = self.sequences[self.active].tracks.get_mut(*track_index) {
                    track.transitions.retain(|t| t.id != transition.id);
                    track.transitions.push(*transition);
                    if let Some(c) = track.clips.iter_mut().find(|c| c.id == transition.clip_a_id) {
                        c.fade_out_secs = *old_fade_out_a;
                    }
                    if let Some(c) = track.clips.iter_mut().find(|c| c.id == transition.clip_b_id) {
                        c.fade_in_secs = *old_fade_in_b;
                    }
                }
            }
            EditCommand::ReplaceClip { old, .. } => {
                for track in &mut self.sequences[self.active].tracks {
                    if let Some(clip) = track.find_clip_mut(old.id) {
                        *clip = (**old).clone();
                        break;
                    }
                }
                effects.sync_sequence = true;
            }
            EditCommand::SetNodeGraph { clip_id, old_graph, .. } => {
                set_node_graph(&mut self.sequences[self.active], *clip_id, old_graph);
            }
            EditCommand::SetMediaLogFormat { media_id, old, grades, .. } => {
                effects.merge(self.set_media_log_format(*media_id, *old, grades.iter().map(|(id, before, after)| (*id, after, before))));
            }
            EditCommand::SetRootGrade { root, old, .. } => {
                set_root_grade(&mut self.sequences[self.active], *root, old.as_deref());
                effects.sync_sequence = true;
            }
            EditCommand::PictureEdit { clip_id, edit, .. } => {
                for track in &mut self.sequences[self.active].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.restore_picture(&edit.before);
                        break;
                    }
                }
            }
            EditCommand::EditKeyframes { clip_id, linked_clip_id, old_state, old_linked_state, .. } => {
                for track in &mut self.sequences[self.active].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        old_state.apply_to_clip(clip);
                        break;
                    }
                }
                if let (Some(partner_id), Some(partner_state)) = (linked_clip_id, old_linked_state) {
                    for track in &mut self.sequences[self.active].tracks {
                        if let Some(clip) = track.find_clip_mut(*partner_id) {
                            partner_state.apply_to_clip(clip);
                            break;
                        }
                    }
                }
            }
            EditCommand::EditBuiltinEffect { clip_id, effect_index, old_effect, .. } => {
                for track in &mut self.sequences[self.active].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        if let Some(id) = clip.effect_id_at(*effect_index) {
                            clip.replace_effect(id, old_effect.clone());
                        }
                        break;
                    }
                }
            }
            EditCommand::ToggleBuiltinEffectEnabled { clip_id, index } => {
                // Undo is the same action as the original: flip the bool. Growing
                // the parallel vec mirrors the forward handler so undo from a
                // missing-entry state still works.
                for track in &mut self.sequences[self.active].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        toggle_effect(clip, *index);
                        break;
                    }
                }
            }
            EditCommand::SetOFXEffectEnabled { clip_id, effect_index, old_enabled, .. } => {
                for track in &mut self.sequences[self.active].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.edit_ofx_effect(*effect_index, |effect| {
                            effect.enabled = *old_enabled;
                        });
                        break;
                    }
                }
            }
            EditCommand::EditClipAudio { clip_id, old_gain, old_volume, old_pan, old_fade_in, old_fade_out, old_channel_config, .. } => {
                for track in &mut self.sequences[self.active].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.gain = *old_gain;
                        clip.volume = *old_volume;
                        clip.pan = *old_pan;
                        clip.fade_in_secs = *old_fade_in;
                        clip.fade_out_secs = *old_fade_out;
                        clip.channel_config = *old_channel_config;
                        break;
                    }
                }
            }
            EditCommand::SetClipSpeed { clip_id, old_speed, old_timeline_out, .. } => {
                for track in &mut self.sequences[self.active].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.speed = *old_speed;
                        clip.timeline_out = *old_timeline_out;
                        break;
                    }
                }
            }
            EditCommand::EditTitle { clip_id, old_data, .. } => {
                for track in &mut self.sequences[self.active].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.kind = klipp_timeline::ClipKind::Title(old_data.clone());
                        break;
                    }
                }
                effects.rerender_titles.push(*clip_id);
            }
            EditCommand::EditGenerator { clip_id, old_data, .. } => {
                for track in &mut self.sequences[self.active].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.kind = klipp_timeline::ClipKind::Generator(old_data.clone());
                        break;
                    }
                }
                effects.sync_sequence = true;
                effects.set_playhead = true;
            }
            EditCommand::SetBuiltinEffects { clip_id, old_effects, old_enabled, old_ids, .. } => {
                for track in &mut self.sequences[self.active].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.set_effect_list_with_ids(old_effects.clone(), old_enabled.clone(), old_ids);
                        break;
                    }
                }
                effects.sync_sequence = true;
                effects.notify_engine = true;
            }
            EditCommand::EditAudioChain { clip_id, old_state, .. } => {
                for track in &mut self.sequences[self.active].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        old_state.apply_to_clip(clip);
                        break;
                    }
                }
                effects.sync_sequence = true;
                effects.notify_engine = true;
            }
        }
        effects
    }

    pub fn apply_forward(&mut self, cmd: &EditCommand) -> Effects {
        let mut effects = Effects::default();
        match cmd {
            EditCommand::SetProjectColour { new, grades, .. } => {
                for g in grades {
                    put_picture(self.sequences, g, false);
                }
                effects.project_colour = Some(new.clone());
                effects.sync_sequence = true;
            }
            EditCommand::Batch(commands) => {
                for c in commands {
                    effects.merge(self.apply_forward(c));
                }
            }
            EditCommand::Named { edit, .. } => effects.merge(self.apply_forward(edit)),
            EditCommand::AddSequences { sequences } => {
                // Appended, so the active sequence keeps its place.
                for s in sequences {
                    if !self.sequences.iter().any(|have| have.id == s.id) {
                        self.sequences.push(s.clone());
                    }
                }
                effects.sync_sequence = true;
            }
            EditCommand::AddTrack { index, track } => {
                let seq = &mut self.sequences[self.active];
                let idx = (*index).min(seq.tracks.len());
                seq.tracks.insert(idx, track.clone());
                seq.renumber_tracks();
            }
            EditCommand::RemoveTrack { index, track } => {
                let seq = &mut self.sequences[self.active];
                if seq.tracks.get(*index).is_some_and(|t| t.id == track.id) {
                    seq.tracks.remove(*index);
                    seq.renumber_tracks();
                }
            }
            EditCommand::SetTrackSettings { changes } => {
                let seq = &mut self.sequences[self.active];
                for (id, _, new) in changes {
                    if let Some(t) = seq.tracks.iter_mut().find(|t| t.id == *id) {
                        t.set_settings(new);
                    }
                }
            }
            EditCommand::SetBrief { new, .. } => {
                self.sequences[self.active].brief = new.as_deref().cloned();
            }
            EditCommand::SetEditPlan { new, .. } => {
                self.sequences[self.active].edit_plan = new.as_deref().cloned();
            }
            EditCommand::SetReviews { new, .. } => {
                self.sequences[self.active].reviews = new.clone();
            }
            EditCommand::SetClipMatch { clip_id, new, .. } => {
                if let Some(c) = self.sequences[self.active].tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).find(|c| c.id == *clip_id) {
                    c.matched_to = *new;
                }
            }
            EditCommand::SetClipRole { clip_id, new, .. } => {
                if let Some(c) = self.sequences[self.active].tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).find(|c| c.id == *clip_id) {
                    c.role = *new;
                    c.role_source = klipp_core::RoleSource::ByHand;
                }
            }
            EditCommand::ProposeClipRoles { changes } => {
                for (clip_id, _, new) in changes {
                    if let Some(c) = self.sequences[self.active].tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).find(|c| c.id == *clip_id) {
                        c.role = *new;
                        c.role_source = klipp_core::RoleSource::Proposed;
                    }
                }
            }
            EditCommand::SetSequenceCamera { new, .. } => {
                self.sequences[self.active].camera = (**new).clone();
            }
            EditCommand::SetSequenceTitleNits { new, .. } => {
                self.sequences[self.active].title_nits = *new;
            }
            EditCommand::SetNodeLabels { new, .. } => {
                self.sequences[self.active].node_labels = new.clone();
            }
            EditCommand::SetClipDepth { clip_id, new, .. } => {
                if let Some(c) = self.sequences[self.active].tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).find(|c| c.id == *clip_id) {
                    c.depth = *new;
                }
            }
            EditCommand::SetTemplateInstance { clip_id, new, .. } => {
                if let Some(c) = self.sequences[self.active].tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).find(|c| c.id == *clip_id) {
                    if let klipp_timeline::ClipKind::TemplatePrecomposition { template_instance, .. } = &mut c.kind {
                        *template_instance = (**new).clone();
                    }
                }
            }
            EditCommand::SetMarkers { new, .. } => {
                self.sequences[self.active].markers = new.clone();
            }
            EditCommand::SetTracks { new, .. } => {
                self.sequences[self.active].tracks = new.clone();
            }
            EditCommand::SetTrack { track_index, new, .. } => {
                if let Some(t) = self.sequences[self.active].tracks.get_mut(*track_index).filter(|t| t.id == new.id) {
                    *t = (**new).clone();
                }
            }
            EditCommand::SetTrackContents { track_index, new, .. } => {
                if let Some(track) = self.sequences[self.active].tracks.get_mut(*track_index) {
                    track.clips = new.clips.clone();
                    track.transitions = new.transitions.clone();
                }
            }
            EditCommand::SetGradeGroups { new, .. } => {
                self.sequences[self.active].grade_groups = new.clone();
            }
            EditCommand::SetClipGradeGroups { changes } => {
                let seq = &mut self.sequences[self.active];
                for (clip_id, _, new) in changes {
                    if let Some(c) = seq.all_clips_mut().find(|c| c.id == *clip_id) {
                        c.grade_group = *new;
                    }
                }
            }
            EditCommand::SetClipLabels { changes, .. } => {
                let seq = &mut self.sequences[self.active];
                for (clip_id, _, new) in changes {
                    if let Some(c) = seq.all_clips_mut().find(|c| c.id == *clip_id) {
                        c.set_own_label(new);
                    }
                }
            }
            EditCommand::SetSequenceFormat { new, .. } => {
                let seq = &mut self.sequences[self.active];
                seq.resolution = new.0;
                seq.fps = new.1;
                *self.fps = new.1;
            }
            EditCommand::AddTitleClip { clip, track_index, created_track, track_id } => {
                let seq = &mut self.sequences[self.active];
                if *created_track {
                    // Re-create the track
                    let track_num = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Text).count() + 1;
                    let mut track = klipp_timeline::Track::new(&format!("Text {track_num}"), klipp_timeline::TrackKind::Text);
                    // The id the edit was made with: made anew on each
                    // redo, the track was another to what named it (#2802).
                    if let Some(id) = track_id {
                        track.id = *id;
                    }
                    track.clips.push(clip.clone());
                    let idx = (*track_index).min(seq.tracks.len());
                    seq.tracks.insert(idx, track);
                } else if let Some(track) = seq.tracks.get_mut(*track_index) {
                    track.clips.push(clip.clone());
                    track.sort_clips();
                }
            }
            EditCommand::LegacyAppendClip { clip, track_index } => {
                if let Some(track) = self.sequences[self.active].tracks.get_mut(*track_index) {
                    track.clips.push(clip.clone());
                    track.sort_clips();
                }
            }
            EditCommand::LinkedImport { video, audio, created_video_track_at, created_audio_track_at, autoset } => {
                let seq = &mut self.sequences[self.active];
                if let Some(fps) = crate::history::apply_linked_import_forward(
                    seq, video, audio, *created_video_track_at, *created_audio_track_at, autoset,
                ) {
                    *self.fps = fps;
                }
            }
            EditCommand::LegacyRemoveClip { clip, .. } => {
                if let Some(track) = self.sequences[self.active].tracks.iter_mut()
                    .find(|t| t.clips.iter().any(|c| c.id == clip.id))
                {
                    track.remove_clip(clip.id);
                }
            }
            EditCommand::LegacyRippleRemove { removed_clip, .. } => {
                if let Some(track) = self.sequences[self.active].tracks.iter_mut()
                    .find(|t| t.clips.iter().any(|c| c.id == removed_clip.id))
                {
                    track.ripple_remove(removed_clip.id);
                }
            }
            EditCommand::LegacyMoveClip { clip_id, new_timeline_in, new_timeline_out, linked_partner, .. } => {
                if let Some(track) = self.sequences[self.active].tracks.iter_mut()
                    .find(|t| t.clips.iter().any(|c| c.id == *clip_id))
                {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.timeline_in = *new_timeline_in;
                        clip.timeline_out = *new_timeline_out;
                    }
                    track.sort_clips();
                }
                if let Some(p) = linked_partner {
                    if let Some(track) = self.sequences[self.active].tracks.iter_mut()
                        .find(|t| t.clips.iter().any(|c| c.id == p.partner_id))
                    {
                        if let Some(clip) = track.find_clip_mut(p.partner_id) {
                            clip.timeline_in = p.new_timeline_in;
                            clip.timeline_out = p.new_timeline_out;
                        }
                        track.sort_clips();
                    }
                }
            }
            EditCommand::LegacySwapClips {
                clip_a_id, clip_b_id,
                a_old_in, b_old_in,
                linked_a_partner, linked_b_partner, ..
            } => {
                // Re-apply the swap. Use stored old positions to
                // identify which clip was "left" (smaller start) and
                // recompute the new positions deterministically — the
                // forward path's "preserve span" semantics.
                let (left_id, right_id) = if a_old_in.to_secs_f64() <= b_old_in.to_secs_f64() {
                    (*clip_a_id, *clip_b_id)
                } else {
                    (*clip_b_id, *clip_a_id)
                };
                Self::reapply_swap_on_track(
                    &mut self.sequences[self.active],
                    left_id,
                    right_id,
                );
                // Re-apply on linked partners using their stored old
                // positions to determine left/right order at the
                // original swap time.
                if let (Some((la_id, la_in_old, _)), Some((lb_id, lb_in_old, _)))
                    = (linked_a_partner, linked_b_partner)
                {
                    let (l_left_id, l_right_id) = if la_in_old.to_secs_f64() <= lb_in_old.to_secs_f64() {
                        (*la_id, *lb_id)
                    } else {
                        (*lb_id, *la_id)
                    };
                    Self::reapply_swap_on_track(
                        &mut self.sequences[self.active],
                        l_left_id,
                        l_right_id,
                    );
                }
            }
            EditCommand::LegacyReplaceClipMedia {
                clip_id,
                new_media_id,
                new_source_out,
                new_timeline_out,
                ..
            } => {
                if let Some(track) = self.sequences[self.active].tracks.iter_mut()
                    .find(|t| t.clips.iter().any(|c| c.id == *clip_id))
                {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.kind = klipp_timeline::ClipKind::Media { media_id: *new_media_id };
                        clip.source_out = *new_source_out;
                        clip.timeline_out = *new_timeline_out;
                    }
                }
            }
            EditCommand::LegacyTrimClip {
                clip_id,
                new_source_in, new_source_out,
                new_timeline_in, new_timeline_out, ..
            } => {
                if let Some(track) = self.sequences[self.active].tracks.iter_mut()
                    .find(|t| t.clips.iter().any(|c| c.id == *clip_id))
                {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.source_in = *new_source_in;
                        clip.source_out = *new_source_out;
                        clip.timeline_in = *new_timeline_in;
                        clip.timeline_out = *new_timeline_out;
                    }
                }
            }
            EditCommand::LegacyRippleTrimClip {
                clip_id,
                old_timeline_out,
                new_source_in, new_source_out,
                new_timeline_in, new_timeline_out,
                shifted_clips, ..
            } => {
                if let Some(track) = self.sequences[self.active].tracks.iter_mut()
                    .find(|t| t.clips.iter().any(|c| c.id == *clip_id))
                {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.source_in = *new_source_in;
                        clip.source_out = *new_source_out;
                        clip.timeline_in = *new_timeline_in;
                        clip.timeline_out = *new_timeline_out;
                    }
                    // Re-apply ripple shift to downstream clips
                    let ripple_delta = *new_timeline_out - *old_timeline_out;
                    for (cid, orig_in, orig_out) in shifted_clips {
                        if let Some(c) = track.find_clip_mut(*cid) {
                            c.timeline_in = *orig_in + ripple_delta;
                            c.timeline_out = *orig_out + ripple_delta;
                        }
                    }
                    track.sort_clips();
                }
            }
            EditCommand::LegacyRollTrimClip {
                clip_a_id, clip_b_id,
                new_a_source_out, new_a_timeline_out,
                new_b_source_in, new_b_timeline_in, ..
            } => {
                let seq = &mut self.sequences[self.active];
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
            }
            EditCommand::LegacySlipClip {
                clip_id,
                old_source_in, new_source_in, new_source_out, ..
            } => {
                // Redo slip: apply new source in/out
                let seq = &mut self.sequences[self.active];
                let linked_id = seq.tracks.iter().flat_map(|t| t.clips.iter())
                    .find(|c| c.id == *clip_id).and_then(|c| c.linked_clip_id);
                for track in &mut seq.tracks {
                    if let Some(c) = track.find_clip_mut(*clip_id) {
                        c.source_in = *new_source_in;
                        c.source_out = *new_source_out;
                    }
                }
                // The partner moves by as much as the clip, through its own
                // range: copying the clip's range onto it collapsed a J or L
                // cut and left the sound's source a different length from its
                // place on the timeline (#458).
                if let Some(lid) = linked_id {
                    let by = *new_source_in - *old_source_in;
                    for track in &mut self.sequences[self.active].tracks {
                        if let Some(c) = track.find_clip_mut(lid) {
                            c.source_in += by;
                            c.source_out += by;
                        }
                    }
                }
            }
            EditCommand::LegacySlideClip {
                clip_id,
                new_timeline_in, new_timeline_out,
                left_clip_id, new_left_out,
                right_clip_id, new_right_in, ..
            } => {
                // Redo slide: apply new timeline positions for clip and neighbors
                let seq = &mut self.sequences[self.active];
                let linked_id = seq.tracks.iter().flat_map(|t| t.clips.iter())
                    .find(|c| c.id == *clip_id).and_then(|c| c.linked_clip_id);
                let old_in = seq.tracks.iter().flat_map(|t| t.clips.iter())
                    .find(|c| c.id == *clip_id).map(|c| c.timeline_in).unwrap_or(*new_timeline_in);
                let delta = *new_timeline_in - old_in;
                for track in &mut seq.tracks {
                    if let Some(c) = track.find_clip_mut(*clip_id) {
                        c.timeline_in = *new_timeline_in;
                        c.timeline_out = *new_timeline_out;
                    }
                    if let Some(lid) = left_clip_id {
                        if let (Some(c), Some(new_out)) = (track.find_clip_mut(*lid), new_left_out) {
                            let current_out = c.timeline_out;
                            let dt = *new_out - current_out;
                            c.timeline_out = *new_out;
                            c.source_out += dt;
                        }
                    }
                    if let Some(rid) = right_clip_id {
                        if let (Some(c), Some(new_in)) = (track.find_clip_mut(*rid), new_right_in) {
                            let current_in = c.timeline_in;
                            let dt = *new_in - current_in;
                            c.timeline_in = *new_in;
                            c.source_in += dt;
                        }
                    }
                    track.sort_clips();
                }
                if let Some(lid) = linked_id {
                    for track in &mut self.sequences[self.active].tracks {
                        if let Some(c) = track.find_clip_mut(lid) {
                            let dur = c.duration();
                            c.timeline_in += delta;
                            c.timeline_out = c.timeline_in + dur;
                        }
                    }
                }
            }
            EditCommand::LegacyBladeClip { track_index, original_clip, clip_a, clip_b, .. } => {
                // Redo blade: remove original, insert clip_a and clip_b
                if let Some(track) = self.sequences[self.active].tracks.get_mut(*track_index) {
                    track.remove_clip(original_clip.id);
                    track.clips.push(clip_a.clone());
                    track.clips.push(clip_b.clone());
                    track.sort_clips();
                }
            }
            EditCommand::LegacySplitAtSceneMarkers { track_index, original_clip, new_clips, .. } => {
                // Redo: remove the original, re-insert every piece.
                if let Some(track) = self.sequences[self.active].tracks.get_mut(*track_index) {
                    track.remove_clip(original_clip.id);
                    for c in new_clips {
                        track.clips.push(c.clone());
                    }
                    track.sort_clips();
                }
            }
            EditCommand::LegacyCutClip { clip, .. } => {
                // Redo cut = ripple remove
                if let Some(track) = self.sequences[self.active].tracks.iter_mut()
                    .find(|t| t.clips.iter().any(|c| c.id == clip.id))
                {
                    track.ripple_remove(clip.id);
                }
            }
            EditCommand::LegacyInsertEdit { edit, audio, created_audio_track_at } => {
                let seq = &mut self.sequences[self.active];
                crate::history::apply_insert_edit_forward(seq, edit, audio, *created_audio_track_at);
            }
            EditCommand::LegacyPasteClip { clip, track_index } => {
                // Redo paste = re-insert
                if let Some(track) = self.sequences[self.active].tracks.get_mut(*track_index) {
                    track.clips.push(clip.clone());
                    track.sort_clips();
                }
            }
            EditCommand::LegacyMoveClipToTrack { clip, from_track, to_track, new_timeline_in, linked_partner, .. } => {
                // Redo: move from from_track to to_track at new position
                if let Some(src) = self.sequences[self.active].tracks.get_mut(*from_track) {
                    src.remove_clip(clip.id);
                }
                if let Some(dst) = self.sequences[self.active].tracks.get_mut(*to_track) {
                    let mut moved = clip.clone();
                    moved.timeline_in = *new_timeline_in;
                    let dur = clip.timeline_out - clip.timeline_in;
                    moved.timeline_out = *new_timeline_in + dur;
                    dst.clips.push(moved);
                    dst.sort_clips();
                }
                if let Some(p) = linked_partner {
                    if let Some(track) = self.sequences[self.active].tracks.iter_mut()
                        .find(|t| t.clips.iter().any(|c| c.id == p.partner_id))
                    {
                        if let Some(clip) = track.find_clip_mut(p.partner_id) {
                            clip.timeline_in = p.new_timeline_in;
                            clip.timeline_out = p.new_timeline_out;
                        }
                        track.sort_clips();
                    }
                }
            }
            EditCommand::Magnetic(cmd) => {
                klipp_timeline::magnetic::MagneticEditor::apply_forward(
                    &mut self.sequences[self.active], cmd);
            }
            EditCommand::SourceLinkedInsert {
                video_command,
                audio_track_index,
                audio_clips_after,
                ..
            } => {
                klipp_timeline::magnetic::MagneticEditor::apply_forward(
                    &mut self.sequences[self.active],
                    video_command,
                );
                if let Some(track) = self.sequences[self.active]
                    .tracks
                    .get_mut(*audio_track_index)
                {
                    track.clips = audio_clips_after.clone();
                }
            }
            EditCommand::AddMediaToPool { item } => {
                // Redo import: re-add the item
                self.media_pool.add(item.clone());
                effects.sync_media_pool = true;
            }
            EditCommand::RemoveMediaFromPool { item, .. } => {
                // Redo delete: remove again
                self.media_pool.remove(&item.id);
                effects.sync_media_pool = true;
            }
            EditCommand::CreateBin { bin } => {
                if !self.media_pool.bins.iter().any(|b| b.id == bin.id) {
                    self.media_pool.bins.push(bin.clone());
                }
            }
            EditCommand::MoveMediaToBin { media_id, to_bin, to_bin_id, .. } => {
                // Redo: apply both legacy string and bin_id
                if let Some(item) = self.media_pool.get_mut(media_id) {
                    item.bin = to_bin.clone();
                    item.bin_id = *to_bin_id;
                }
            }
            EditCommand::SetMediaTags { media_id, new_tags, .. } => {
                if let Some(item) = self.media_pool.get_mut(media_id) {
                    item.tags = new_tags.clone();
                }
            }
            EditCommand::SetMediaRating { media_id, new_rating, .. } => {
                if let Some(item) = self.media_pool.get_mut(media_id) {
                    item.rating = *new_rating;
                }
            }
            EditCommand::RelinkMedia { media_id, new_path, new_offline, .. } => {
                effects.merge(self.relink(*media_id, new_path, *new_offline));
                // The new file's details, read from it again.
                if !*new_offline {
                    effects.reprobe_media.push(*media_id);
                }
            }
            EditCommand::SetMediaColorLabel { media_id, new_label, .. } => {
                if let Some(item) = self.media_pool.get_mut(media_id) {
                    item.color_label = klipp_media::ColorLabel::from_u8(*new_label);
                }
            }
            EditCommand::RenameBin { old_name, new_name } => {
                // Redo: rename Bin record from old_name to new_name
                for bin in &mut self.media_pool.bins {
                    if bin.name == *old_name {
                        bin.name = new_name.clone();
                        break;
                    }
                }
                // Redo: update legacy item.bin field
                let ids: Vec<MediaId> = self.media_pool.all()
                    .filter(|m| m.bin.as_deref() == Some(old_name.as_str()))
                    .map(|m| m.id)
                    .collect();
                for id in ids {
                    if let Some(item) = self.media_pool.get_mut(&id) {
                        item.bin = Some(new_name.clone());
                        // bin_id intentionally unchanged — rename doesn't change identity
                    }
                }
            }
            EditCommand::BatchRenameMediaItems { changes } => {
                // Redo: re-apply every new filename.
                for (media_id, _old, new_filename) in changes {
                    if let Some(item) = self.media_pool.get_mut(media_id) {
                        item.filename = new_filename.clone();
                    }
                }
            }
            EditCommand::CreateSubclip { subclip, parent_id: _ } => {
                self.media_pool.add(subclip.clone());
            }
            EditCommand::MoveMarkers { moved, dropped } => {
                let markers = &mut self.sequences[self.active].markers;
                for mv in moved {
                    if let Some(m) = markers.iter_mut().find(|m| m.id == mv.id) {
                        m.position = mv.new_position;
                        m.duration = mv.new_duration;
                    }
                }
                markers.retain(|m| !dropped.iter().any(|(_, d)| d.id == m.id));
            }
            EditCommand::SetRange { new_in, new_out, .. } => {
                let seq = &mut self.sequences[self.active];
                seq.in_point = *new_in;
                seq.out_point = *new_out;
            }
            EditCommand::AddTransition { track_index, transition, clip_a_id, clip_b_id, .. } => {
                let dur_secs = transition.duration.to_secs_f64() as f32;
                if let Some(track) = self.sequences[self.active].tracks.get_mut(*track_index) {
                    track.transitions.retain(|t| !(t.clip_a_id == *clip_a_id && t.clip_b_id == *clip_b_id));
                    track.transitions.push(*transition);
                    // An audio track's transition is the crossfade itself:
                    // fading the clips as well would dip the join.
                    if track.kind != klipp_timeline::TrackKind::Audio {
                        if let Some(c) = track.clips.iter_mut().find(|c| c.id == *clip_a_id) {
                            c.fade_out_secs = dur_secs;
                        }
                        if let Some(c) = track.clips.iter_mut().find(|c| c.id == *clip_b_id) {
                            c.fade_in_secs = dur_secs;
                        }
                    }
                }
            }
            EditCommand::RetargetTransition { track_index, transition_id, new_a, .. } => {
                if let Some(t) = self.sequences[self.active].tracks.get_mut(*track_index).and_then(|t| t.transitions.iter_mut().find(|t| t.id == *transition_id)) {
                    t.clip_a_id = *new_a;
                }
            }
            EditCommand::RemoveTransition { track_index, transition, .. } => {
                // The dip a transition wrote on its clips goes with it: a
                // fade at the crossfaded end of each clip as long as the
                // transition, on an audio track too, where a transition
                // dropped before crossfades existed dipped the clips and
                // that dip would otherwise outlive it. A fade of any other
                // length is the user's own and stays: adding the
                // crossfade left it alone, so removing it does too.
                // Within half a frame: the transition's length is whole
                // frames, the fade written with it may not be.
                let dip_secs = transition.duration.to_secs_f64() as f32;
                let seq = &mut self.sequences[self.active];
                let slack = (0.5 / seq.fps.as_f64().max(1.0)) as f32 + 1e-4;
                let is_dip = |fade: f32| (fade - dip_secs).abs() <= slack;
                if let Some(track) = seq.tracks.get_mut(*track_index) {
                    track.transitions.retain(|t| t.id != transition.id);
                    if let Some(c) = track.clips.iter_mut().find(|c| c.id == transition.clip_a_id && is_dip(c.fade_out_secs)) {
                        c.fade_out_secs = 0.0;
                    }
                    if let Some(c) = track.clips.iter_mut().find(|c| c.id == transition.clip_b_id && is_dip(c.fade_in_secs)) {
                        c.fade_in_secs = 0.0;
                    }
                }
            }
            EditCommand::ReplaceClip { new, .. } => {
                for track in &mut self.sequences[self.active].tracks {
                    if let Some(clip) = track.find_clip_mut(new.id) {
                        *clip = (**new).clone();
                        break;
                    }
                }
                effects.sync_sequence = true;
            }
            EditCommand::SetNodeGraph { clip_id, new_graph, .. } => {
                set_node_graph(&mut self.sequences[self.active], *clip_id, new_graph);
            }
            EditCommand::SetMediaLogFormat { media_id, new, grades, .. } => {
                effects.merge(self.set_media_log_format(*media_id, *new, grades.iter().map(|(id, before, after)| (*id, before, after))));
            }
            EditCommand::SetRootGrade { root, new, .. } => {
                set_root_grade(&mut self.sequences[self.active], *root, new.as_deref());
                effects.sync_sequence = true;
            }
            EditCommand::PictureEdit { clip_id, edit, .. } => {
                for track in &mut self.sequences[self.active].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.restore_picture(&edit.after);
                        break;
                    }
                }
            }
            EditCommand::EditKeyframes { clip_id, linked_clip_id, new_state, new_linked_state, .. } => {
                for track in &mut self.sequences[self.active].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        new_state.apply_to_clip(clip);
                        break;
                    }
                }
                if let (Some(partner_id), Some(partner_state)) = (linked_clip_id, new_linked_state) {
                    for track in &mut self.sequences[self.active].tracks {
                        if let Some(clip) = track.find_clip_mut(*partner_id) {
                            partner_state.apply_to_clip(clip);
                            break;
                        }
                    }
                }
            }
            EditCommand::EditBuiltinEffect { clip_id, effect_index, new_effect, .. } => {
                for track in &mut self.sequences[self.active].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        if let Some(id) = clip.effect_id_at(*effect_index) {
                            clip.replace_effect(id, new_effect.clone());
                        }
                        break;
                    }
                }
            }
            EditCommand::ToggleBuiltinEffectEnabled { clip_id, index } => {
                // Redo flips again — restores the post-toggle state that the
                // user originally produced.
                for track in &mut self.sequences[self.active].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        toggle_effect(clip, *index);
                        break;
                    }
                }
            }
            EditCommand::SetOFXEffectEnabled { clip_id, effect_index, new_enabled, .. } => {
                for track in &mut self.sequences[self.active].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.edit_ofx_effect(*effect_index, |effect| {
                            effect.enabled = *new_enabled;
                        });
                        break;
                    }
                }
            }
            EditCommand::EditClipAudio { clip_id, new_gain, new_volume, new_pan, new_fade_in, new_fade_out, new_channel_config, .. } => {
                for track in &mut self.sequences[self.active].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.gain = *new_gain;
                        clip.volume = *new_volume;
                        clip.pan = *new_pan;
                        clip.fade_in_secs = *new_fade_in;
                        clip.fade_out_secs = *new_fade_out;
                        clip.channel_config = *new_channel_config;
                        break;
                    }
                }
            }
            EditCommand::SetClipSpeed { clip_id, new_speed, new_timeline_out, .. } => {
                for track in &mut self.sequences[self.active].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.speed = *new_speed;
                        clip.timeline_out = *new_timeline_out;
                        break;
                    }
                }
            }
            EditCommand::EditTitle { clip_id, new_data, .. } => {
                for track in &mut self.sequences[self.active].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.kind = klipp_timeline::ClipKind::Title(new_data.clone());
                        break;
                    }
                }
                effects.rerender_titles.push(*clip_id);
            }
            EditCommand::EditGenerator { clip_id, new_data, .. } => {
                for track in &mut self.sequences[self.active].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.kind = klipp_timeline::ClipKind::Generator(new_data.clone());
                        break;
                    }
                }
                effects.sync_sequence = true;
                effects.set_playhead = true;
            }
            EditCommand::SetBuiltinEffects { clip_id, new_effects, new_enabled, new_ids, .. } => {
                for track in &mut self.sequences[self.active].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.set_effect_list_with_ids(new_effects.clone(), new_enabled.clone(), new_ids);
                        break;
                    }
                }
                effects.sync_sequence = true;
                effects.notify_engine = true;
            }
            EditCommand::EditAudioChain { clip_id, new_state, .. } => {
                for track in &mut self.sequences[self.active].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        new_state.apply_to_clip(clip);
                        break;
                    }
                }
                effects.sync_sequence = true;
                effects.notify_engine = true;
            }
        }
        effects
    }

    /// Re-apply the "preserve span" swap to two clips (already
    /// validated as adjacent on the same track) given their pre-swap
    /// left/right identity. Used by the redo path for
    /// `EditCommand::LegacySwapClips`. Caller pre-computes which clip
    /// was "left" (smaller start) at the original swap time.
    fn reapply_swap_on_track(
        seq: &mut klipp_timeline::Sequence,
        left_id: ClipId,
        right_id: ClipId,
    ) {
        if let Some(track) = seq.tracks.iter_mut()
            .find(|t| t.clips.iter().any(|c| c.id == left_id))
        {
            let mut left_in = klipp_core::RationalTime { value: 0, rate: 1 };
            let mut left_dur = left_in;
            let mut right_dur = left_in;
            for clip in &track.clips {
                if clip.id == left_id {
                    left_in = clip.timeline_in;
                    left_dur = clip.timeline_out - clip.timeline_in;
                } else if clip.id == right_id {
                    right_dur = clip.timeline_out - clip.timeline_in;
                }
            }
            let new_right_in = left_in;
            let new_right_out = left_in + right_dur;
            let new_left_in = new_right_out;
            let new_left_out = new_left_in + left_dur;
            for clip in track.clips.iter_mut() {
                if clip.id == left_id {
                    clip.timeline_in = new_left_in;
                    clip.timeline_out = new_left_out;
                } else if clip.id == right_id {
                    clip.timeline_in = new_right_in;
                    clip.timeline_out = new_right_out;
                }
            }
            track.sort_clips();
        }
    }
}

/// Flip a built-in effect's enabled flag, the effect named by its place:
/// what `ToggleBuiltinEffectEnabled` does, forward and back alike.
fn toggle_effect(clip: &mut klipp_timeline::Clip, index: usize) {
    if let Some(id) = clip.effect_id_at(index) {
        let on = clip.effect_entries().nth(index).is_none_or(|(_, _, on)| on);
        clip.set_effect_enabled(id, !on);
    }
}

#[cfg(test)]
mod media_log_format_tests {
    use super::*;

    /// A file's camera encoding is one step across its clips in every
    /// sequence: undo puts back the media's format and each clip's grade
    /// (an Input Transform taken out again), redo puts the new ones (#1396).
    #[test]
    fn a_camera_encoding_change_undoes_and_redoes_whole() {
        let media = MediaId::new();
        let mut pool = MediaPool::new();
        pool.add(crate::history::bin_undo_tests::test_media_item(media));
        let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
        let clip = || klipp_timeline::Clip::media(media, tc(0.0), tc(2.0), tc(0.0), tc(2.0));
        let mut sequences = vec![Sequence::new_default(), Sequence::new_default()];
        let (a, b) = (clip(), clip());
        let (a_id, b_id) = (a.id, b.id);
        sequences[0].tracks[0].clips.push(a);
        sequences[1].tracks[0].clips.push(b);
        let grade = |seqs: &[Sequence], id| seqs.iter().find_map(|s| s.all_clips().find(|c| c.id == id)).unwrap().grade().clone();
        let before = (grade(&sequences, a_id), grade(&sequences, b_id));
        // What the change makes: an Input Transform on each clip, S-Log3.
        let mut after_seqs = sequences.clone();
        for s in &mut after_seqs {
            for c in s.all_clips_mut() {
                c.set_input_log_format(klipp_core::LogFormat::SLog3);
            }
        }
        let after = (grade(&after_seqs, a_id), grade(&after_seqs, b_id));
        let user = |p: &MediaPool| p.get(&media).map(|i| (i.log_format, i.log_format_user_set)).unwrap();
        let old = user(&pool);
        let edit = EditCommand::SetMediaLogFormat {
            media_id: Some(media),
            old,
            new: (klipp_core::LogFormat::SLog3, true),
            grades: vec![(a_id, before.0.clone(), after.0.clone()), (b_id, before.1.clone(), after.1.clone())],
        };
        let mut fps = sequences[0].fps;
        let mut view = DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps };
        let effects = view.apply_forward(&edit);
        assert!(effects.sync_sequence && effects.sync_media_pool);
        assert_eq!(effects.refresh_media_pictures, vec![media], "the file's pictures made again");
        let json = |g: &klipp_nodes::NodeGraph| serde_json::to_value(g).unwrap();
        assert_eq!(user(view.media_pool), (klipp_core::LogFormat::SLog3, true));
        assert_eq!(json(&grade(view.sequences, a_id)), json(&after.0));
        assert_eq!(json(&grade(view.sequences, b_id)), json(&after.1), "the other sequence's clip too");
        view.apply_inverse(&edit);
        assert_eq!(user(view.media_pool), old);
        assert_eq!(json(&grade(view.sequences, a_id)), json(&before.0));
        assert_eq!(json(&grade(view.sequences, b_id)), json(&before.1));
        assert_ne!(json(&before.0), json(&after.0), "the change put an Input Transform in");
    }

    /// Undoing a camera encoding in one sequence leaves alone the grade
    /// work done since on the file's clip in another, whose own history
    /// knows nothing of the step: that clip loses only the step's Input
    /// Transform, and redo gives it back without undoing the work either.
    #[test]
    fn undoing_a_camera_encoding_keeps_later_grade_work_in_another_sequence() {
        let media = MediaId::new();
        let mut pool = MediaPool::new();
        pool.add(crate::history::bin_undo_tests::test_media_item(media));
        let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
        let clip = || klipp_timeline::Clip::media(media, tc(0.0), tc(2.0), tc(0.0), tc(2.0));
        let mut sequences = vec![Sequence::new_default(), Sequence::new_default()];
        let (a, b) = (clip(), clip());
        let (a_id, b_id) = (a.id, b.id);
        sequences[0].tracks[0].clips.push(a);
        sequences[1].tracks[0].clips.push(b);
        let grade = |seqs: &[Sequence], id| seqs.iter().find_map(|s| s.all_clips().find(|c| c.id == id)).unwrap().grade().clone();
        let before = (grade(&sequences, a_id), grade(&sequences, b_id));
        let mut after_seqs = sequences.clone();
        for s in &mut after_seqs {
            for c in s.all_clips_mut() {
                c.set_input_log_format(klipp_core::LogFormat::SLog3);
            }
        }
        let after = (grade(&after_seqs, a_id), grade(&after_seqs, b_id));
        let edit = EditCommand::SetMediaLogFormat {
            media_id: Some(media),
            old: (pool.get(&media).unwrap().log_format, pool.get(&media).unwrap().log_format_user_set),
            new: (klipp_core::LogFormat::SLog3, true),
            grades: vec![(a_id, before.0.clone(), after.0.clone()), (b_id, before.1.clone(), after.1.clone())],
        };
        let mut fps = sequences[0].fps;
        let mut view = DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps };
        view.apply_forward(&edit);
        // Later, in the other sequence: B's Corrector graded.
        let graded = |g: &klipp_nodes::NodeGraph| g.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Corrector).and_then(|n| n.params.exposure);
        let input_transform = |g: &klipp_nodes::NodeGraph| g.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::InputTransform).map(|n| n.params.log_format);
        let b_clip = view.sequences[1].all_clips_mut().find(|c| c.id == b_id).unwrap();
        b_clip.edit_grade(|g| g.nodes.iter_mut().filter(|n| n.kind == klipp_nodes::NodeKind::Corrector).for_each(|n| n.params.exposure = Some(1.5)));
        assert_eq!(graded(&grade(view.sequences, b_id)), Some(1.5));
        view.apply_inverse(&edit);
        let json = |g: &klipp_nodes::NodeGraph| serde_json::to_value(g).unwrap();
        assert_eq!(json(&grade(view.sequences, a_id)), json(&before.0), "A, untouched since, back whole");
        let b_now = grade(view.sequences, b_id);
        // B as it was before the step, with the work done since: the
        // same, byte for byte, as saved.
        let mut expected = before.1.clone();
        expected.nodes.iter_mut().filter(|n| n.kind == klipp_nodes::NodeKind::Corrector).for_each(|n| n.params.exposure = Some(1.5));
        assert_eq!(json(&b_now), json(&expected), "B keeps the work done since and loses only the step's Input Transform");
        view.apply_forward(&edit);
        let b_again = grade(view.sequences, b_id);
        assert_eq!(graded(&b_again), Some(1.5), "redo keeps B's work too");
        assert_eq!(input_transform(&b_again), Some(Some(klipp_core::LogFormat::SLog3)), "redo gives B the encoding");
    }

    /// An Input Transform the step put in, then edited in another
    /// sequence (set to another format there), is that sequence's later
    /// work: undoing the step leaves it, and the rest of that grade,
    /// exactly as they are.
    #[test]
    fn undoing_a_camera_encoding_keeps_an_input_transform_edited_since() {
        let media = MediaId::new();
        let mut pool = MediaPool::new();
        pool.add(crate::history::bin_undo_tests::test_media_item(media));
        let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
        let clip = klipp_timeline::Clip::media(media, tc(0.0), tc(2.0), tc(0.0), tc(2.0));
        let id = clip.id;
        let mut sequences = vec![Sequence::new_default(), Sequence::new_default()];
        sequences[1].tracks[0].clips.push(clip);
        let grade = |seqs: &[Sequence]| seqs[1].all_clips().find(|c| c.id == id).unwrap().grade().clone();
        let before = grade(&sequences);
        let mut after = sequences.clone();
        after[1].all_clips_mut().for_each(|c| c.set_input_log_format(klipp_core::LogFormat::SLog3));
        let after = grade(&after);
        let edit = EditCommand::SetMediaLogFormat { media_id: Some(media), old: (klipp_core::LogFormat::Srgb, false), new: (klipp_core::LogFormat::SLog3, true), grades: vec![(id, before, after)] };
        let mut fps = sequences[0].fps;
        let mut view = DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps };
        view.apply_forward(&edit);
        view.sequences[1].all_clips_mut().find(|c| c.id == id).unwrap().edit_grade(|g| {
            g.nodes.iter_mut().filter(|n| n.kind == klipp_nodes::NodeKind::InputTransform).for_each(|n| n.params.log_format = Some(klipp_core::LogFormat::LogC3));
        });
        let edited = serde_json::to_value(grade(view.sequences)).unwrap();
        view.apply_inverse(&edit);
        assert_eq!(serde_json::to_value(grade(view.sequences)).unwrap(), edited, "the edited Input Transform, and the grade, as they were");
    }
}

#[cfg(test)]
mod timeline_grade_tests {
    use super::*;

    #[test]
    fn a_graph_edit_under_the_timeline_grade_id_undoes_and_redoes_the_timeline_grade() {
        let mut sequences = vec![Sequence::new_default()];
        let id = sequences[0].timeline_grade_clip_id();
        let mut new_graph = klipp_nodes::NodeGraph::default();
        new_graph.ensure_corrector();
        let edit = EditCommand::SetNodeGraph { clip_id: id, old_graph: klipp_nodes::NodeGraph::default(), new_graph: new_graph.clone() };
        let mut pool = MediaPool::new();
        let mut fps = sequences[0].fps;
        let mut view = DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps };
        view.apply_forward(&edit);
        let len = |s: &Sequence| s.timeline_grade().map(|g| g.nodes.len());
        assert_eq!(len(&view.sequences[0]), Some(new_graph.nodes.len()));
        view.apply_inverse(&edit);
        assert_eq!(len(&view.sequences[0]), Some(klipp_nodes::NodeGraph::default().nodes.len()));
    }
}

#[cfg(test)]
mod grade_group_tests {
    use super::*;
    use klipp_core::Timecode;
    use klipp_timeline::{GradeGroup, GradeStage};

    /// #237: making a group, putting a clip in it, and grading its
    /// post-clip stage under the stage's editing id each undo and redo.
    #[test]
    fn a_clips_name_and_colour_undo_and_redo() {
        let mut seq = Sequence::new_default();
        let clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), Timecode::zero(), Timecode { value: 24, rate: 24 }, Timecode::zero(), Timecode { value: 24, rate: 24 });
        let clip_id = clip.id;
        seq.tracks[0].clips.push(clip);
        let mut sequences = vec![seq];
        let mut pool = MediaPool::new();
        let mut fps = sequences[0].fps;
        let mut view = DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps };

        let named = klipp_timeline::ClipLabel { name: Some("Wide, take 3".into()), colour: 4, flag: klipp_timeline::GradeFlag::Approved };
        let edit = EditCommand::SetClipLabels { description: "Rename Clip", changes: vec![(clip_id, klipp_timeline::ClipLabel::default(), named.clone())] };
        let label = |view: &DocumentView| view.sequences[0].all_clips().find(|c| c.id == clip_id).unwrap().own_label();
        view.apply_forward(&edit);
        assert_eq!(label(&view), named);
        assert_eq!(view.sequences[0].all_clips().find(|c| c.id == clip_id).unwrap().grade_flag, klipp_timeline::GradeFlag::Approved, "the grade flag (#899) goes with the label");
        assert_eq!(view.sequences[0].all_clips().find(|c| c.id == clip_id).unwrap().label("a.mov"), "Wide, take 3");
        view.apply_inverse(&edit);
        assert_eq!(label(&view), klipp_timeline::ClipLabel::default());
        assert_eq!(view.sequences[0].all_clips().find(|c| c.id == clip_id).unwrap().label("a.mov"), "a.mov", "with no name of its own it shows its media's");
    }

    /// #1723: a sequence's title brightness undoes and redoes.
    #[test]
    fn a_title_brightness_undoes_and_redoes() {
        let mut sequences = vec![Sequence::new_default()];
        let mut pool = MediaPool::new();
        let mut fps = sequences[0].fps;
        let mut view = DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps };
        let edit = EditCommand::SetSequenceTitleNits { old: klipp_timeline::DEFAULT_TITLE_NITS, new: 600.0 };
        view.apply_forward(&edit);
        assert_eq!(view.sequences[0].title_nits, 600.0);
        view.apply_inverse(&edit);
        assert_eq!(view.sequences[0].title_nits, klipp_timeline::DEFAULT_TITLE_NITS);
        assert_eq!(edit.label(), "Title Brightness");
    }

    /// #1753: the labels on a sequence's nodes undo and redo, whole.
    #[test]
    fn node_labels_undo_and_redo() {
        use klipp_graph_format::path::{NodePath, Root};
        let mut sequences = vec![Sequence::new_default()];
        let mut pool = MediaPool::new();
        let mut fps = sequences[0].fps;
        let node = NodePath::new(Root::TimelineGrade { sequence: sequences[0].id.0 }, [uuid::Uuid::new_v4()]);
        let mut view = DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps };
        let new = view.sequences[0].node_labels_with(&node, 3);
        let edit = EditCommand::SetNodeLabels { old: Vec::new(), new };
        view.apply_forward(&edit);
        assert_eq!(view.sequences[0].node_label(&node), 3);
        view.apply_inverse(&edit);
        assert_eq!(view.sequences[0].node_label(&node), 0);
        view.apply_forward(&edit);
        assert_eq!(view.sequences[0].node_label(&node), 3);
        assert_eq!(edit.label(), "Node Colour Label");
    }

    #[test]
    fn a_grade_group_and_its_grades_undo_and_redo() {
        let mut seq = Sequence::new_default();
        let clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), Timecode::zero(), Timecode { value: 24, rate: 24 }, Timecode::zero(), Timecode { value: 24, rate: 24 });
        let clip_id = clip.id;
        seq.tracks[0].clips.push(clip);
        let group = GradeGroup::new("Interview");
        let (gid, post_id) = (group.id, group.stage_clip_id(GradeStage::Post));
        let mut sequences = vec![seq];
        let mut pool = MediaPool::new();
        let mut fps = sequences[0].fps;
        let mut view = DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps };

        let make = EditCommand::SetGradeGroups { old: vec![], new: vec![group] };
        let join = EditCommand::SetClipGradeGroups { changes: vec![(clip_id, None, Some(gid))] };
        let mut look = klipp_nodes::NodeGraph::default();
        look.ensure_lut3d();
        let empty = klipp_nodes::NodeGraph::default();
        let grade = EditCommand::SetNodeGraph { clip_id: post_id, old_graph: empty.clone(), new_graph: look.clone() };
        for e in [&make, &join, &grade] {
            view.apply_forward(e);
        }
        let seq = &view.sequences[0];
        assert_eq!(seq.grade_groups[0].stage(klipp_timeline::GradeStage::Post), Some(&look), "the post-clip grade landed on the group");
        assert_eq!(seq.all_clips().find(|c| c.id == clip_id).unwrap().grade_group, Some(gid));
        assert_eq!(seq.grade_group_stage_of(post_id), Some((gid, GradeStage::Post)));

        view.apply_inverse(&grade);
        assert_eq!(view.sequences[0].grade_groups[0].stage(klipp_timeline::GradeStage::Post), Some(&empty));
        view.apply_inverse(&join);
        assert_eq!(view.sequences[0].all_clips().find(|c| c.id == clip_id).unwrap().grade_group, None);
        view.apply_inverse(&make);
        assert!(view.sequences[0].grade_groups.is_empty());
    }
}

#[cfg(test)]
mod track_settings_tests {
    use super::*;
    use klipp_timeline::{Track, TrackKind, TrackPaste};

    /// #846: one track's attributes pasted onto two others is one step:
    /// both take them, and one undo puts both back. A paste of the levels
    /// alone leaves the effects where they were.
    #[test]
    fn a_paste_onto_two_tracks_is_one_step() {
        let mut seq = Sequence::new_default();
        let mut source = Track::new("Dialogue", TrackKind::Audio);
        source.volume = 0.5;
        source.effects.audio = vec![klipp_core::AudioEffectInstance { effect_type: "eq".into(), enabled: true, wet_dry: 1.0, params: vec![] }];
        let mut b = Track::new("A2", TrackKind::Audio);
        b.effects.audio = vec![klipp_core::AudioEffectInstance { effect_type: "limiter".into(), enabled: true, wet_dry: 1.0, params: vec![] }];
        let c = Track::new("A3", TrackKind::Audio);
        seq.tracks = vec![source.clone(), b, c];
        let before: Vec<_> = seq.tracks.iter().map(Track::settings).collect();
        let copied = source.settings();
        let paste = |seq: &Sequence, what: TrackPaste| EditCommand::SetTrackSettings {
            changes: seq.tracks[1..].iter().map(|t| (t.id, t.settings(), t.pasted(&copied, what))).collect(),
        };
        let edit = paste(&seq, TrackPaste::ALL);
        let levels_only = paste(&seq, TrackPaste { levels: true, picture: false, sound: false });

        let mut sequences = vec![seq];
        let mut pool = MediaPool::new();
        let mut fps = sequences[0].fps;
        let mut view = DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps };
        view.apply_forward(&edit);
        for t in &view.sequences[0].tracks[1..] {
            assert_eq!(t.settings(), copied, "{} takes the dialogue track's settings", t.name);
        }
        view.apply_inverse(&edit);
        let now: Vec<_> = view.sequences[0].tracks.iter().map(Track::settings).collect();
        assert_eq!(now, before, "one undo puts both back");

        view.apply_forward(&levels_only);
        let b = &view.sequences[0].tracks[1];
        assert_eq!(b.volume, 0.5, "the level is pasted");
        assert_eq!(b.effects.audio[0].effect_type, "limiter", "the track's own chain stays");
    }
}

#[cfg(test)]
mod effect_writer_tests {
    //! The effect-list writers undo and redo through the clip's effect API
    //! (#1074): each keeps every keyframe and modifier on the effect it
    //! animates, and an edit, a toggle or a keyframe edit keeps the ids.
    use super::*;
    use klipp_core::{ModifierStack, ParamId, Timecode};
    use klipp_effects::BuiltinEffect;
    use klipp_timeline::test_clip::{self, TestClip};

    fn blur(radius: f32) -> BuiltinEffect {
        BuiltinEffect::GaussianBlur { radius }
    }

    fn vignette(amount: f32) -> BuiltinEffect {
        BuiltinEffect::Vignette { amount, size: 0.5, softness: 0.5, roundness: 0.5 }
    }

    /// A clip with a blur and a vignette, each with keyframes, and a
    /// modifier on the vignette.
    fn doc() -> (Vec<Sequence>, ClipId) {
        let mut seq = Sequence::new_default();
        let mut clip = klipp_timeline::Clip::media(MediaId::new(), Timecode::zero(), Timecode { value: 48, rate: 24 }, Timecode::zero(), Timecode { value: 48, rate: 24 });
        clip.set_effects(vec![blur(2.0), vignette(0.4)]);
        let mut clip = TestClip::of(clip).key(0, "radius", 0.0, 3.0).key(1, "amount", 0.0, 0.7).build();
        clip.edit_param_modifiers(|v| v.insert(ParamId::Effect { effect_index: 1, param_name: "amount".into() }, ModifierStack::default()));
        let id = clip.id;
        seq.tracks[0].clips.push(clip);
        (vec![seq], id)
    }

    fn clip(seqs: &mut [Sequence], id: ClipId) -> &mut klipp_timeline::Clip {
        seqs[0].all_clips_mut().find(|c| c.id == id).unwrap()
    }

    /// Each effect's own animation, read through its id: the blur's radius
    /// keys, the vignette's amount keys and whether it has its modifier.
    fn animation(c: &mut klipp_timeline::Clip) -> (Option<f32>, Option<f32>, bool) {
        let (b, v) = (test_clip::effect_id(c, 0), test_clip::effect_id(c, 1));
        let radius = c.edit_existing_effect_keyframes(b, "radius", |k| k[0].value);
        let amount = c.edit_existing_effect_keyframes(v, "amount", |k| k[0].value);
        let modifier = c.edit_existing_effect_modifiers(v, "amount", |_| ()).is_some();
        (radius, amount, modifier)
    }

    const ALL: (Option<f32>, Option<f32>, bool) = (Some(3.0), Some(0.7), true);

    #[test]
    fn an_effect_edit_keeps_its_id_and_animation() {
        let (mut seqs, id) = doc();
        let before = test_clip::effect_id(clip(&mut seqs, id), 1);
        let edit = EditCommand::EditBuiltinEffect { clip_id: id, effect_index: 1, old_effect: vignette(0.4), new_effect: vignette(0.9) };
        let mut pool = MediaPool::new();
        let mut fps = seqs[0].fps;
        let mut view = DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps };
        for (forward, want) in [(true, vignette(0.9)), (false, vignette(0.4)), (true, vignette(0.9))] {
            if forward { view.apply_forward(&edit) } else { view.apply_inverse(&edit) };
            let c = clip(view.sequences, id);
            assert_eq!(test_clip::effects(c)[1], want);
            assert_eq!(animation(c), ALL);
            assert_eq!(test_clip::effect_id(c, 1), before, "the same effect, edited");
        }
    }

    #[test]
    fn a_toggle_flips_one_flag_and_keeps_the_animation() {
        let (mut seqs, id) = doc();
        let before = test_clip::effect_id(clip(&mut seqs, id), 1);
        let edit = EditCommand::ToggleBuiltinEffectEnabled { clip_id: id, index: 1 };
        let mut pool = MediaPool::new();
        let mut fps = seqs[0].fps;
        let mut view = DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps };
        let flags = |c: &mut klipp_timeline::Clip| c.effect_entries().map(|(_, _, on)| on).collect::<Vec<_>>();
        for (forward, want) in [(true, [true, false]), (false, [true, true]), (true, [true, false])] {
            if forward { view.apply_forward(&edit) } else { view.apply_inverse(&edit) };
            let c = clip(view.sequences, id);
            assert_eq!(flags(c), want);
            assert_eq!(animation(c), ALL);
            assert_eq!(test_clip::effect_id(c, 1), before);
        }
    }

    #[test]
    fn a_whole_list_set_leaves_the_animation_by_place() {
        // As it always has: the keys stay at their places, whatever comes
        // to stand there.
        let (mut seqs, id) = doc();
        let edit = EditCommand::SetBuiltinEffects {
            clip_id: id,
            old_effects: vec![blur(2.0), vignette(0.4)],
            old_enabled: vec![true, true],
            new_effects: vec![blur(5.0), vignette(0.4), blur(1.0)],
            new_enabled: vec![true, false, true],
            old_ids: Vec::new(),
            new_ids: Vec::new(),
        };
        let mut pool = MediaPool::new();
        let mut fps = seqs[0].fps;
        let mut view = DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps };
        view.apply_forward(&edit);
        let c = clip(view.sequences, id);
        assert_eq!((test_clip::effects(c).len(), test_clip::enabled(c)), (3, vec![true, false, true]));
        assert_eq!(animation(c), ALL);
        view.apply_inverse(&edit);
        let c = clip(view.sequences, id);
        assert_eq!(test_clip::effects(c), vec![blur(2.0), vignette(0.4)]);
        assert_eq!(animation(c), ALL);
    }

    #[test]
    fn a_keyframe_edit_restores_the_effect_keys_and_keeps_the_ids() {
        let (mut seqs, id) = doc();
        let c = clip(&mut seqs, id);
        let ids = (test_clip::effect_id(c, 0), test_clip::effect_id(c, 1));
        let old_state = crate::history::KeyframeSnapshot::from_clip(c);
        let mut new_state = old_state.clone();
        new_state.effect_keyframes.remove("0:radius");
        let edit = EditCommand::EditKeyframes { clip_id: id, linked_clip_id: None, description: "Keys", old_state, new_state, old_linked_state: None, new_linked_state: None };
        let mut pool = MediaPool::new();
        let mut fps = seqs[0].fps;
        let mut view = DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps };
        view.apply_forward(&edit);
        let c = clip(view.sequences, id);
        assert_eq!(animation(c), (None, Some(0.7), true));
        assert_eq!((test_clip::effect_id(c, 0), test_clip::effect_id(c, 1)), ids, "a keyframe edit is no new effect");
        view.apply_inverse(&edit);
        assert_eq!(animation(clip(view.sequences, id)), ALL);
    }
}

#[cfg(test)]
mod picture_edit_tests {
    use super::*;
    use klipp_core::Timecode;
    use klipp_graph_format::path::{NodePath, Root};
    use klipp_graph_format::{types, ParamValue};
    use klipp_graph_lift::ops::GraphOp;

    /// A batch of graph ops on a clip is one history entry: undone, the
    /// picture is the one before, effect ids included; redone, the one
    /// after.
    #[test]
    fn a_picture_edit_undoes_and_redoes_the_clips_picture_whole() {
        let mut seq = Sequence::new_default();
        let clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), Timecode::zero(), Timecode { value: 48, rate: 24 }, Timecode::zero(), Timecode { value: 48, rate: 24 });
        let mut clip = klipp_timeline::test_clip::TestClip::of(clip).effects([klipp_effects::BuiltinEffect::GaussianBlur { radius: 3.0 }, klipp_effects::BuiltinEffect::Sharpen { amount: 0.5 }]).build();
        let clip_id = clip.id;
        let ids: Vec<_> = clip.effect_entries().map(|(id, _, _)| id).collect();
        let fx: Vec<_> = clip.picture_v2().unwrap().picture.graph.nodes.iter().filter(|n| n.type_id.as_str() == types::LEGACY_EFFECT).map(|n| n.id).collect();
        let at = |id| NodePath::new(Root::Clip { clip: clip_id.0 }, [id]);
        let mut edited = clip.clone();
        let edit = edited
            .apply_picture_ops(&[
                GraphOp::SetParam { path: at(fx[0]), key: "radius".into(), value: ParamValue::Float(5.0) },
                GraphOp::MoveAfter { path: at(fx[0]), after: fx[1] },
            ])
            .unwrap();
        seq.tracks[0].clips.push(edited);
        let entry = EditCommand::PictureEdit { description: "Edit Effects", clip_id, edit: Box::new(edit) };
        let mut sequences = vec![seq];
        let mut pool = MediaPool::new();
        let mut fps = sequences[0].fps;
        let mut view = DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps };
        let order = |view: &mut DocumentView| -> Vec<_> {
            view.sequences[0].tracks[0].find_clip_mut(clip_id).unwrap().effect_entries().map(|(id, e, _)| (id, e.clone())).collect()
        };
        view.apply_inverse(&entry);
        let undone = order(&mut view);
        assert_eq!(undone.iter().map(|(id, _)| *id).collect::<Vec<_>>(), ids, "the effects' ids and order are back");
        assert!(matches!(undone[0].1, klipp_effects::BuiltinEffect::GaussianBlur { radius } if radius == 3.0));
        view.apply_forward(&entry);
        let redone = order(&mut view);
        assert_eq!(redone.iter().map(|(id, _)| *id).collect::<Vec<_>>(), vec![ids[1], ids[0]]);
        assert!(matches!(redone[1].1, klipp_effects::BuiltinEffect::GaussianBlur { radius } if radius == 5.0));
        assert_eq!(entry.label(), "Edit Effects");
    }
}

#[cfg(test)]
mod relink_tests {
    use super::*;

    /// A relink undone points the item back at its old file, offline as it
    /// was; redone, at the new one; each time the pool is synced and the
    /// item's pictures made again (#2086).
    #[test]
    fn a_relink_is_undone_and_redone() {
        let mut pool = MediaPool::new();
        let mut item = klipp_media::MediaItem::synthetic_for_test("/old/a.mov".into(), None);
        item.offline = true;
        item.duration = klipp_core::RationalTime::from_secs_f64(10.0, 24);
        item.video_codec = "prores".into();
        let old_item = Box::new(item.clone());
        let id = pool.add(item);
        let mut sequences = vec![klipp_timeline::Sequence::new_default()];
        let mut fps = klipp_core::Fps::film_24();
        let edit = EditCommand::RelinkMedia { media_id: id, old_path: "/old/a.mov".into(), old_offline: true, old_item, new_path: "/new/a.mov".into(), new_offline: false };
        let mut view = DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps };
        let effects = view.apply_forward(&edit);
        assert!(effects.sync_media_pool && effects.refresh_media_pictures == vec![id]);
        assert_eq!(effects.reprobe_media, vec![id], "the new file read again");
        // As the re-probe of the new file leaves it: another length and codec.
        let item = view.media_pool.get_mut(&id).unwrap();
        item.duration = klipp_core::RationalTime::from_secs_f64(4.0, 24);
        item.video_codec = "h264".into();
        item.rating = 3;
        assert_eq!((view.media_pool.get(&id).unwrap().path.as_path(), view.media_pool.get(&id).unwrap().offline), (std::path::Path::new("/new/a.mov"), false));
        view.apply_inverse(&edit);
        let item = view.media_pool.get(&id).unwrap();
        assert_eq!((item.path.as_path(), item.offline), (std::path::Path::new("/old/a.mov"), true));
        assert_eq!((item.duration.to_secs_f64(), item.video_codec.as_str()), (10.0, "prores"), "the old file's details back");
        assert_eq!(item.rating, 3, "what the person said kept");
        assert_eq!(edit.scope(), crate::history::Scope::Project);
    }
}
