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
use klipp_timeline::Sequence;

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
/// playhead, re-render a title's overlay. Merged across a batch, so a
/// host performs each once.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Effects {
    pub sync_sequence: bool,
    pub sync_media_pool: bool,
    pub notify_engine: bool,
    pub set_playhead: bool,
    pub rerender_titles: Vec<ClipId>,
}

impl Effects {
    pub fn merge(&mut self, other: Effects) {
        self.sync_sequence |= other.sync_sequence;
        self.sync_media_pool |= other.sync_media_pool;
        self.notify_engine |= other.notify_engine;
        self.set_playhead |= other.set_playhead;
        self.rerender_titles.extend(other.rerender_titles);
    }
}

impl DocumentView<'_> {
    pub fn apply_inverse(&mut self, cmd: &EditCommand) -> Effects {
        let mut effects = Effects::default();
        match cmd {
            EditCommand::Batch(commands) => {
                for c in commands.iter().rev() {
                    effects.merge(self.apply_inverse(c));
                }
            }
            EditCommand::AddTrack { index, track } => {
                let seq = &mut self.sequences[self.active];
                if seq.tracks.get(*index).is_some_and(|t| t.id == track.id) {
                    seq.tracks.remove(*index);
                    seq.renumber_tracks();
                }
            }
            EditCommand::SetBrief { old, .. } => {
                self.sequences[self.active].brief = old.as_deref().cloned();
            }
            EditCommand::SetEditPlan { old, .. } => {
                self.sequences[self.active].edit_plan = old.as_deref().cloned();
            }
            EditCommand::SetClipRole { clip_id, old, .. } => {
                if let Some(c) = self.sequences[self.active].tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).find(|c| c.id == *clip_id) {
                    c.role = *old;
                }
            }
            EditCommand::SetSequenceCamera { old, .. } => {
                self.sequences[self.active].camera = (**old).clone();
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
            EditCommand::SetSequenceFormat { old, .. } => {
                let seq = &mut self.sequences[self.active];
                seq.resolution = old.0;
                seq.fps = old.1;
                *self.fps = old.1;
            }
            EditCommand::AddTitleClip { clip, track_index, created_track } => {
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
                old_source_in, old_source_out, ..
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
                if let Some(lid) = linked_id {
                    for track in &mut self.sequences[self.active].tracks {
                        if let Some(c) = track.find_clip_mut(lid) {
                            c.source_in = *old_source_in;
                            c.source_out = *old_source_out;
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
            EditCommand::SetNodeGraph { clip_id, old_graph, .. } => {
                for track in &mut self.sequences[self.active].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.node_graph = old_graph.clone();
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
                        if let Some(fx) = clip.builtin_effects.get_mut(*effect_index) {
                            *fx = old_effect.clone();
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
                        while clip.builtin_effects_enabled.len() < clip.builtin_effects.len() {
                            clip.builtin_effects_enabled.push(true);
                        }
                        if let Some(val) = clip.builtin_effects_enabled.get_mut(*index) {
                            *val = !*val;
                        }
                        break;
                    }
                }
            }
            EditCommand::SetOFXEffectEnabled { clip_id, effect_index, old_enabled, .. } => {
                for track in &mut self.sequences[self.active].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        if let Some(effect) = clip.effects.get_mut(*effect_index) {
                            effect.enabled = *old_enabled;
                        }
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
            EditCommand::SetBuiltinEffects { clip_id, old_effects, old_enabled, .. } => {
                for track in &mut self.sequences[self.active].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.builtin_effects = old_effects.clone();
                        clip.builtin_effects_enabled = old_enabled.clone();
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
            EditCommand::Batch(commands) => {
                for c in commands {
                    effects.merge(self.apply_forward(c));
                }
            }
            EditCommand::AddTrack { index, track } => {
                let seq = &mut self.sequences[self.active];
                let idx = (*index).min(seq.tracks.len());
                seq.tracks.insert(idx, track.clone());
                seq.renumber_tracks();
            }
            EditCommand::SetBrief { new, .. } => {
                self.sequences[self.active].brief = new.as_deref().cloned();
            }
            EditCommand::SetEditPlan { new, .. } => {
                self.sequences[self.active].edit_plan = new.as_deref().cloned();
            }
            EditCommand::SetClipRole { clip_id, new, .. } => {
                if let Some(c) = self.sequences[self.active].tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).find(|c| c.id == *clip_id) {
                    c.role = *new;
                }
            }
            EditCommand::SetSequenceCamera { new, .. } => {
                self.sequences[self.active].camera = (**new).clone();
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
            EditCommand::SetSequenceFormat { new, .. } => {
                let seq = &mut self.sequences[self.active];
                seq.resolution = new.0;
                seq.fps = new.1;
                *self.fps = new.1;
            }
            EditCommand::AddTitleClip { clip, track_index, created_track } => {
                let seq = &mut self.sequences[self.active];
                if *created_track {
                    // Re-create the track
                    let track_num = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Text).count() + 1;
                    let mut track = klipp_timeline::Track::new(&format!("Text {track_num}"), klipp_timeline::TrackKind::Text);
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
                new_source_in, new_source_out, ..
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
                if let Some(lid) = linked_id {
                    for track in &mut self.sequences[self.active].tracks {
                        if let Some(c) = track.find_clip_mut(lid) {
                            c.source_in = *new_source_in;
                            c.source_out = *new_source_out;
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
                    if let Some(c) = track.clips.iter_mut().find(|c| c.id == *clip_a_id) {
                        c.fade_out_secs = dur_secs;
                    }
                    if let Some(c) = track.clips.iter_mut().find(|c| c.id == *clip_b_id) {
                        c.fade_in_secs = dur_secs;
                    }
                }
            }
            EditCommand::SetNodeGraph { clip_id, new_graph, .. } => {
                for track in &mut self.sequences[self.active].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.node_graph = new_graph.clone();
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
                        if let Some(fx) = clip.builtin_effects.get_mut(*effect_index) {
                            *fx = new_effect.clone();
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
                        while clip.builtin_effects_enabled.len() < clip.builtin_effects.len() {
                            clip.builtin_effects_enabled.push(true);
                        }
                        if let Some(val) = clip.builtin_effects_enabled.get_mut(*index) {
                            *val = !*val;
                        }
                        break;
                    }
                }
            }
            EditCommand::SetOFXEffectEnabled { clip_id, effect_index, new_enabled, .. } => {
                for track in &mut self.sequences[self.active].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        if let Some(effect) = clip.effects.get_mut(*effect_index) {
                            effect.enabled = *new_enabled;
                        }
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
            EditCommand::SetBuiltinEffects { clip_id, new_effects, new_enabled, .. } => {
                for track in &mut self.sequences[self.active].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.builtin_effects = new_effects.clone();
                        clip.builtin_effects_enabled = new_enabled.clone();
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
