//! Placing media on the timeline: appended to a track, at the playhead, on
//! new tracks, overwritten or inserted, a range lifted, and the frame-rate
//! prompt before it (#890).

use klipp_timeline::GradeAccess;
use super::*;

impl KlippApp {
    /// Append one media item to the end of a track with its linked audio,
    /// on the magnetic or the free path as the track calls for. The
    /// framerate prompt has already been answered or skipped by the caller.
    pub(super) fn append_media_to_track(&mut self, media_id: &klipp_core::MediaId, track_index: &usize) {
                    if let Some(item) = self.state.media_pool.get(media_id).cloned() {
                        let duration = item.duration;
                        self.adopt_first_clip_format(&item);
                        if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get(*track_index) {
                            if track.magnetic_role == klipp_timeline::TrackRole::Primary {
                                // Magnetic path: append via MagneticEditor
                                let source_in = klipp_core::RationalTime { value: 0, rate: duration.rate };
                                let source_out = duration;
                                let mut clip = klipp_timeline::Clip::media(
                                    *media_id,
                                    klipp_core::RationalTime::ZERO, // will be set by append_primary
                                    klipp_core::RationalTime::ZERO,
                                    source_in,
                                    source_out,
                                );
                                // Auto-apply detected log format, and carry
                                // the media's rotation (see AppendClip: the
                                // viewer takes rotation from the media item,
                                // export from the clip, so a clip without it
                                // plays upright and exports sideways).
                                if let Some(item) = self.state.media_pool.get(media_id) {
                                    clip.set_input_log_format(item.detected_log_format);
                                    clip.source_rotation = item.source_rotation;
                                }
                                let result = klipp_timeline::magnetic::MagneticEditor::append_primary(
                                    &mut self.state.sequences[self.state.active_sequence_idx], clip);
                                // Get the clip id from the result
                                let mut appended_id = None;
                                if let klipp_timeline::magnetic_commands::MagneticEditCommand::AppendPrimary { ref appended_clip, .. } = result.command {
                                    self.state.selected_clip = Some(appended_clip.id);
                                    appended_id = Some(appended_clip.id);
                                }
                                // The file's audio, linked to it.
                                // `SourceLinkedInsert` is the entry that
                                // carries a magnetic video edit plus an
                                // audio track's before and after, so the
                                // pair is still one undo.
                                let audio = appended_id.and_then(|id| self.append_linked_audio(*media_id, id));
                                match audio {
                                    Some(a) => {
                                        // The clip as it now is, linked: the
                                        // append recorded it before its sound
                                        // was put under it, so a redo put it
                                        // back unlinked (#2633).
                                        let mut video_command = result.command;
                                        if let klipp_timeline::magnetic_commands::MagneticEditCommand::AppendPrimary { ref mut appended_clip, .. } = video_command {
                                            appended_clip.linked_clip_id = Some(a.clip.id);
                                        }
                                        self.state.push_history(EditCommand::SourceLinkedInsert {
                                        video_command,
                                        audio_track_index: a.track_index,
                                        audio_clips_before: a.clips_before,
                                        audio_clips_after: a.clips_after,
                                    });
                                    }
                                    None => self.state.push_history(EditCommand::Magnetic(result.command)),
                                }
                                self.sync_sequence_to_engine();
                                self.state.dirty = true;
                            } else {
                                // Legacy path for Free tracks
                                let mid = *media_id;
                                let track = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*track_index).unwrap();
                                let clip_id = track.append_clip(mid, duration);
                                let rotation = self.state.media_pool.get(&mid).map_or(0, |i| i.source_rotation);
                                if let Some(c) = self.state.sequences[self.state.active_sequence_idx]
                                    .tracks[*track_index]
                                    .find_clip_mut(clip_id)
                                {
                                    c.source_rotation = rotation;
                                }
                                self.apply_input_defaults(clip_id, mid);
                                // The file's audio, linked to it.
                                let audio = self.append_linked_audio(mid, clip_id);
                                let clip = self.state.sequences[self.state.active_sequence_idx].tracks[*track_index].find_clip(clip_id).unwrap().clone();
                                match audio {
                                    Some(a) => self.state.push_history(EditCommand::LinkedImport {
                                        video: Some((clip, *track_index)),
                                        audio: Some((a.clip, a.track_index)),
                                        created_video_track_at: None,
                                        created_audio_track_at: a.created_track_at,
                                        autoset: None,
                                    }),
                                    None => self.state.push_history(EditCommand::LegacyAppendClip {
                                        clip,
                                        track_index: *track_index,
                                    }),
                                }
                                self.sync_sequence_to_engine();
                                self.state.selected_clip = Some(clip_id);
                                self.state.dirty = true;
                            }
                        }
                    }
    }

    /// A pool item dropped on track `track_index` at `at` (#2549): put
    /// in there, moving what follows along, or over what is there with
    /// `overwrite`, its sound linked under it, as one undo step. The
    /// framerate prompt has already been answered or skipped.
    pub(super) fn place_media_on_track_at(&mut self, media_id: klipp_core::MediaId, track_index: usize, at: klipp_core::Timecode, overwrite: bool) {
        let Some(item) = self.state.media_pool.get(&media_id).cloned() else {
            self.state.toast.show("That clip is no longer in the media pool");
            return;
        };
        if item.offline {
            self.state.toast.show(format!("Cannot add '{}' — media is offline", item.filename));
            return;
        }
        let has_video = !item.video_codec.is_empty();
        self.adopt_first_clip_format(&item);
        let start = klipp_core::Timecode { value: 0, rate: item.duration.rate };
        let mut clip = klipp_timeline::Clip::media(media_id, start, item.duration, start, item.duration);
        clip.source_rotation = item.source_rotation;
        super::media::input_defaults_on(&mut clip, &item, self.state.project_default_input_format, self.state.project_default_input_lut.as_deref());
        // Its sound, as an append brings it (see `append_linked_audio`).
        let external = self.external_audio_of(media_id);
        let audio = (has_video && self.state.append_brings_audio && (item.has_audio || external.is_some())).then_some(external);
        let (mode, label) = if overwrite {
            (crate::clip_clipboard::PasteMode::Overwrite, "Overwrite")
        } else {
            (crate::clip_clipboard::PasteMode::Insert, "Insert")
        };
        match media_drop_edits(self.state.sequence(), clip, track_index, audio, at, mode, label) {
            Ok((edits, id)) => {
                self.apply_edits_as_one(&edits, label);
                self.state.selected_clip = Some(id);
                self.state.selected_clips.clear();
            }
            Err(why) => self.state.toast.show(why),
        }
    }

    /// If placing `media_id` would put the first video clip on a sequence
    /// with a rate of its own and the clip is at another, opens the
    /// "Change Project Frame Rate?" modal and stashes `pending_cmd` for
    /// re-dispatch when the user answers. Returns `true` when the caller
    /// should abort the current handler (the placement is deferred).
    ///
    /// Never asked once the sequence holds video, nor when it follows its
    /// first clip (see [`klipp_timeline::Sequence::first_clip_rate_question`]),
    /// nor right after the prompt was answered, nor for sound.
    pub(super) fn maybe_prompt_framerate_mismatch(
        &mut self,
        media_id: &klipp_core::MediaId,
        pending_cmd: AppCommand,
    ) -> bool {
        if self.suppress_fps_prompt {
            return false;
        }
        let Some(item) = self.state.media_pool.get(media_id) else {
            return false;
        };
        if item.video_codec.is_empty() || item.resolution.width == 0 || item.resolution.height == 0 {
            return false;
        }
        let media_fps = item.fps;
        let seq_idx = self.state.active_sequence_idx;
        let Some(sequence_fps) = self.state.sequences.get(seq_idx).and_then(|seq| seq.first_clip_rate_question(media_fps)) else {
            return false;
        };
        self.framerate_mismatch_dialog = Some(
            crate::panels::framerate_mismatch_dialog::FrameRateMismatchDialog {
                sequence_idx: seq_idx,
                media_fps,
                sequence_fps,
                pending_cmd,
            },
        );
        true
    }

    /// The active sequence takes `item`'s size and rate when it is the
    /// first video clip and the sequence follows its first clip
    /// ([`klipp_timeline::Sequence::adopt_first_clip`]): the settings
    /// before and after, for the undo history. Every way a clip is placed
    /// runs it before working out timecodes at the sequence's rate; a path
    /// that left it out kept the 24/1 a new project starts at (#2550).
    #[allow(clippy::type_complexity)]
    pub(super) fn adopt_first_clip_format(
        &mut self,
        item: &klipp_media::MediaItem,
    ) -> Option<((klipp_core::Resolution, klipp_core::Fps), (klipp_core::Resolution, klipp_core::Fps))> {
        if item.video_codec.is_empty() {
            return None;
        }
        let seq = &mut self.state.sequences[self.state.active_sequence_idx];
        let change = seq.adopt_first_clip(item.resolution, item.fps)?;
        self.state.fps = seq.fps;
        Some(change)
    }

    /// Perform a Source-Viewer three-point edit (Insert or Overwrite) on the
    /// magnetic primary track, wiring up a linked audio partner on a
    /// connected audio track when the source has audio. Pushes exactly one
    /// history entry — `EditCommand::SourceLinkedInsert` when an audio
    /// partner was created, else `EditCommand::Magnetic`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn apply_source_magnetic_edit(
        &mut self,
        seq_idx: usize,
        item: &klipp_media::MediaItem,
        mid: klipp_core::MediaId,
        mut new_clip: klipp_timeline::Clip,
        timeline_in: Timecode,
        dur: Timecode,
        source_in: Timecode,
        source_out: Timecode,
        mode: klipp_timeline::magnetic::EditMode,
    ) {
        let has_linked_audio = item.has_audio;
        let video_clip_id = new_clip.id;

        // Pre-generate audio clip id so the video clip can carry
        // linked_clip_id into the magnetic command for later undo.
        let audio_clip_id = if has_linked_audio {
            let aid = klipp_core::ClipId::new();
            new_clip.linked_clip_id = Some(aid);
            Some(aid)
        } else {
            None
        };

        // Locate or auto-create the audio track BEFORE the magnetic edit so
        // we can snapshot its clip list — magnetic's connected-track cascade
        // may move anchored A1 clips, and the snapshot captures the true
        // pre-edit state.
        let audio_track_info: Option<(usize, Vec<klipp_timeline::Clip>)> = if has_linked_audio {
            let existing = self.state.sequences[seq_idx].tracks.iter()
                .position(|t| t.kind == klipp_timeline::TrackKind::Audio);
            let ai = existing.unwrap_or_else(|| {
                let seq = &mut self.state.sequences[seq_idx];
                let audio_count = seq.tracks.iter()
                    .filter(|t| t.kind == klipp_timeline::TrackKind::Audio)
                    .count();
                seq.add_audio_track(format!("Audio {}", audio_count + 1));
                seq.tracks.iter()
                    .position(|t| t.kind == klipp_timeline::TrackKind::Audio)
                    .expect("audio track just added")
            });
            let before = self.state.sequences[seq_idx].tracks[ai].clips.clone();
            Some((ai, before))
        } else {
            None
        };

        // Thread user snap config through the magnetic edit so an
        // insert from the source viewer respects the same snap rules
        // (master + per-target + grid) as a drag-from-source-to-track
        // edit. apply_inverse / apply_forward replays already-snapped
        // commands and don't re-resolve, so the snap-time choice is
        // recorded into the resulting MagneticEditCommand.
        let pps = self.state.timeline_zoom;
        let fps_rate = self.state.sequences[seq_idx].fps.rate();
        let grid_interval_secs = crate::timeline::grid_snap::resolve_grid_interval_secs(
            self.state.grid_snap_interval,
            fps_rate,
            pps,
        );
        let snap_cfg = self.state.snap;
        let result = klipp_timeline::magnetic::MagneticEditor::three_point_edit(
            &mut self.state.sequences[seq_idx],
            new_clip,
            timeline_in,
            true,
            mode,
            grid_interval_secs,
            Some(&snap_cfg),
        );

        match (audio_clip_id, audio_track_info) {
            (Some(aid), Some((ai, audio_clips_before))) => {
                let mut audio_clip = klipp_timeline::Clip::media(
                    mid,
                    timeline_in,
                    timeline_in + dur,
                    source_in,
                    source_out,
                );
                audio_clip.id = aid;
                audio_clip.linked_clip_id = Some(video_clip_id);
                // overwrite_insert_clip_on_track is safe for both modes:
                // for Insert, any anchored A1 clips have already been
                // cascaded by the magnetic primary edit, leaving a clean
                // slot; for non-anchored overlap, overwrite semantics
                // prevent pile-up. A per-mode audio ripple is a future
                // polish tracked separately.
                if let Some(track) = self.state.sequences[seq_idx].tracks.get_mut(ai) {
                    Self::overwrite_insert_clip_on_track(track, audio_clip);
                }
                let audio_clips_after = self.state.sequences[seq_idx].tracks[ai].clips.clone();
                self.state.push_history(EditCommand::SourceLinkedInsert {
                        video_command: result.command,
                        audio_track_index: ai,
                        audio_clips_before,
                        audio_clips_after,
                    });
            }
            _ => {
                self.state.push_history(EditCommand::Magnetic(result.command));
            }
        }
    }

    /// Drop an already-imported pool item on the timeline at the
    /// playhead, on fresh track(s) below the stack. Audio-only media
    /// lands on a new audio track; video media on a new video track,
    /// with its audio on a new audio track linked to it. Backs the
    /// Audio Library's "Add at playhead" and the media pool's
    /// right-click equivalent.
    pub(super) fn place_media_at_playhead(&mut self, media_id: klipp_core::MediaId) {
        let start = self.state.playhead;
        if let Some((_, mut entry)) = self.place_media_on_new_tracks(media_id, start) {
            self.refresh_placement_snapshot(self.state.active_sequence_idx, &mut entry);
            self.state.active_history_mut().push(crate::history::HistoryEntry { command: entry });
        }
        self.state.dirty = true;
        self.sync_sequence_to_engine();
    }

    /// Put `media_id` at `start` on a new video track and, for its sound,
    /// a new audio track, linked. Returns the video clip (when there is
    /// picture) and the history entry that undoes the placement; the
    /// caller pushes it, alone or in a batch.
    pub(super) fn place_media_on_new_tracks(
        &mut self,
        media_id: klipp_core::MediaId,
        start: klipp_core::Timecode,
    ) -> Option<(Option<klipp_core::ClipId>, crate::history::EditCommand)> {
        let Some(item) = self.state.media_pool.get(&media_id).cloned() else {
            self.state.toast.show("That clip is no longer in the media pool");
            return None;
        };
        if item.offline {
            self.state.toast.show(format!("Cannot add '{}' — media is offline", item.filename));
            return None;
        }
        let has_video = !item.video_codec.is_empty();
        let has_audio = item.has_audio;
        let rate = item.duration.rate;
        let source_in = klipp_core::Timecode { value: 0, rate };
        let source_out = item.duration;
        let end = start + item.duration;
        let seq_idx = self.state.active_sequence_idx;

        let autoset = self.adopt_first_clip_format(&item);

        let mut hist_video = None;
        let mut hist_audio = None;
        let mut created_video_track_at = None;
        let mut created_audio_track_at = None;
        let mut video_clip_id = None;

        // Video onto a new video track (inserts at index 0).
        if has_video {
            let seq = &mut self.state.sequences[seq_idx];
            let count = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video).count();
            seq.add_video_track(format!("Video {}", count + 1));
            let vi = 0usize;
            created_video_track_at = Some(vi);
            let mut clip = klipp_timeline::Clip::media(media_id, start, end, source_in, source_out);
            clip.source_rotation = item.source_rotation;
            let cid = clip.id;
            Self::overwrite_insert_clip_on_track(&mut seq.tracks[vi], clip);
            video_clip_id = Some(cid);
            self.apply_input_defaults(cid, media_id);
            if let Some(c) = self.state.sequences[seq_idx].tracks[vi].find_clip(cid) {
                hist_video = Some((c.clone(), vi));
            }
        }

        // Audio onto a new audio track (pushed to the end).
        if has_audio || !has_video {
            let seq = &mut self.state.sequences[seq_idx];
            let count = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Audio).count();
            seq.add_audio_track(format!("Audio {}", count + 1));
            let ai = seq.tracks.len() - 1;
            created_audio_track_at = Some(ai);
            let mut clip = klipp_timeline::Clip::media(media_id, start, end, source_in, source_out);
            if let Some(vid) = video_clip_id {
                clip.linked_clip_id = Some(vid);
            }
            let aid = clip.id;
            Self::overwrite_insert_clip_on_track(&mut seq.tracks[ai], clip);
            if let Some(vid) = video_clip_id {
                for track in self.state.sequences[seq_idx].tracks.iter_mut() {
                    if let Some(vc) = track.find_clip_mut(vid) {
                        vc.linked_clip_id = Some(aid);
                        break;
                    }
                }
            }
            if let Some(c) = self.state.sequences[seq_idx].tracks[ai].find_clip(aid) {
                hist_audio = Some((c.clone(), ai));
            }
            self.state.selected_clip = Some(video_clip_id.unwrap_or(aid));
        } else if let Some(vid) = video_clip_id {
            self.state.selected_clip = Some(vid);
        }

        (hist_video.is_some() || hist_audio.is_some()).then_some((
            video_clip_id,
            crate::history::EditCommand::LinkedImport {
                video: hist_video,
                audio: hist_audio,
                created_video_track_at,
                created_audio_track_at,
                autoset,
            },
        ))
    }

    /// Re-take a placement's clip snapshots from the sequence as it is
    /// now. `place_media_on_new_tracks` snapshots the picture before its
    /// sound is linked to it, and a multicam groups the clips after that;
    /// redo restores the snapshots, so without this it brought the clips
    /// back unlinked and ungrouped. The track indices stay as recorded.
    pub(super) fn refresh_placement_snapshot(&self, seq_idx: usize, entry: &mut crate::history::EditCommand) {
        let crate::history::EditCommand::LinkedImport { video, audio, .. } = entry else { return };
        let seq = &self.state.sequences[seq_idx];
        for (clip, _) in [video.as_mut(), audio.as_mut()].into_iter().flatten() {
            if let Some(now) = seq.tracks.iter().find_map(|t| t.find_clip(clip.id)) {
                *clip = now.clone();
            }
        }
    }

    /// Overwrite-insert a clip onto a track at a given position.
    /// Trims or removes any overlapping clips in the range [timeline_in, timeline_out].
    pub(crate) fn overwrite_insert_clip_on_track(
        track: &mut klipp_timeline::Track,
        new_clip: klipp_timeline::Clip,
    ) {
        klipp_document::clipboard::overwrite_insert_clip_on_track(track, new_clip)
    }

    /// Remove (lift) content in [range_in, range_out] from a track, leaving a gap.
    /// Clips fully inside the range are removed. Clips partially overlapping are trimmed.
    pub(super) fn lift_range(
        track: &mut klipp_timeline::Track,
        range_in: Timecode,
        range_out: Timecode,
    ) {
        let mut to_remove = Vec::new();
        let mut to_split = Vec::new();
        let mut to_trim_right = Vec::new(); // trim out-point back to range_in
        let mut to_trim_left = Vec::new();  // trim in-point forward to range_out

        for (i, clip) in track.clips.iter().enumerate() {
            let c_in = clip.timeline_in;
            let c_out = clip.timeline_out;
            if c_out <= range_in || c_in >= range_out {
                continue;
            }
            if c_in >= range_in && c_out <= range_out {
                to_remove.push(i);
            } else if c_in < range_in && c_out > range_out {
                to_split.push(i);
            } else if c_in < range_in && c_out > range_in {
                to_trim_right.push(i);
            } else if c_in < range_out && c_out > range_out {
                to_trim_left.push(i);
            }
        }

        // Handle splits (reverse order for safe insertion)
        for &i in to_split.iter().rev() {
            let clip = track.clips[i].clone();
            let left_dur = range_in - clip.timeline_in;

            // Left portion
            track.clips[i].timeline_out = range_in;
            track.clips[i].source_out = clip.source_in + left_dur;
            // A glide is the whole clip's move; the pieces keep their keys (#611).
            track.clips[i].frame_glide = None;
            track.clips[i].keep_angle_cuts_on_piece(0.0, left_dur.to_secs_f64());

            // Right portion
            let right_offset = range_out - clip.timeline_in;
            let mut right_clip = klipp_timeline::Clip {
                id: klipp_core::ClipId::new(),
                kind: clip.kind.clone(),
                timeline_in: range_out,
                timeline_out: clip.timeline_out,
                source_in: clip.source_in + right_offset,
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
                linked_clip_id: clip.linked_clip_id,
                linked_title_asset_id: clip.linked_title_asset_id,
                group_id: clip.group_id,
                grade_group: clip.grade_group,
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
                // A piece of the clip keeps its track (#684).
                surfaces: clip.surfaces.clone(),
                surface_pin: clip.surface_pin,
                name: clip.name.clone(),
                colour_label: clip.colour_label,
                grade_flag: Default::default(),
                precomp: None,
                baked_own: None,
            };
            right_clip.set_grade(clip.grade().clone());
            // A picture-truth clip's picture (its masks, mixes) goes with
            // the piece made from its fields.
            right_clip.take_picture_truth_of(&clip);
            right_clip.keep_angle_cuts_on_piece(right_offset.to_secs_f64(), (clip.timeline_out - clip.timeline_in).to_secs_f64());
            track.clips.insert(i + 1, right_clip);
        }

        // Trim right edges back
        for &i in &to_trim_right {
            let trim = track.clips[i].timeline_out - range_in;
            track.clips[i].source_out -= trim;
            track.clips[i].timeline_out = range_in;
        }

        // Trim left edges forward
        for &i in &to_trim_left {
            let trim = range_out - track.clips[i].timeline_in;
            track.clips[i].source_in += trim;
            track.clips[i].timeline_in = range_out;
            track.clips[i].keep_angle_cuts_on_picture(trim.to_secs_f64());
        }

        // Remove fully covered (reverse order)
        for &i in to_remove.iter().rev() {
            track.clips.remove(i);
        }
    }
}

/// The edits that put `clip`, a new clip starting at zero, on track
/// `track_index` at `at` (#2549), as an insert or an overwrite, with its
/// sound linked under it when `audio` is `Some` (the synced recording,
/// or the file's own sound for `Some(None)`): an audio track made for it
/// when there is none, all of it one undo step. The new clip's id, or
/// why nothing can be placed.
pub(super) fn media_drop_edits(
    seq: &klipp_timeline::Sequence,
    clip: klipp_timeline::Clip,
    track_index: usize,
    audio: Option<Option<super::linked_audio::ExternalAudio>>,
    at: klipp_core::Timecode,
    mode: crate::clip_clipboard::PasteMode,
    label: &'static str,
) -> Result<(Vec<EditCommand>, klipp_core::ClipId), &'static str> {
    match seq.tracks.get(track_index) {
        None => return Err("That track is gone"),
        Some(t) if t.locked => return Err("That track is locked"),
        Some(_) => {}
    }
    // The clip and its sound laid out on empty copies of the tracks, the
    // sound going to the track it is patched to, then pasted onto the
    // track it was dropped on.
    let mut laid = seq.clone();
    for track in &mut laid.tracks {
        track.clips.clear();
        track.transitions.clear();
    }
    let id = clip.id;
    laid.tracks[track_index].clips.push(clip);
    let mut before = seq.clone();
    let mut edits = Vec::new();
    if let Some(external) = audio {
        if let Some(index) = attach_linked_audio_to_clip(&mut laid, id, external).and_then(|a| a.created_track_at) {
            let mut track = laid.tracks[index].clone();
            track.clips.clear();
            before.tracks.insert(index, track.clone());
            edits.push(EditCommand::AddTrack { index, track });
        }
    }
    let cb = crate::clip_clipboard::copy(&laid, &[id]).ok_or("Nothing to place")?;
    let pasted = crate::clip_clipboard::paste_onto(&before, &cb, at, mode);
    if pasted.no_room {
        return Err("Could not make room there: is a track locked?");
    }
    let placed = pasted
        .ids
        .iter()
        .copied()
        .find(|&c| pasted.sequence.tracks[track_index].find_clip(c).is_some())
        .ok_or("That track cannot take this clip")?;
    edits.extend(crate::clip_clipboard::track_edits(&before, &pasted.sequence, &pasted.touched, label));
    Ok((edits, placed))
}

#[cfg(test)]
mod media_drop_tests {
    use super::super::linked_audio::attach_linked_audio_to_clip;
    use super::media_drop_edits;
    use crate::clip_clipboard::PasteMode;
    use klipp_core::ClipId;
    use klipp_document::apply::DocumentView;
    use klipp_document::history::EditCommand;
    use klipp_timeline::{Clip, Sequence, TrackKind};

    fn t(secs: i64) -> klipp_core::Timecode {
        klipp_core::Timecode { value: secs * 30, rate: 30 }
    }

    fn track_of(seq: &Sequence, kind: TrackKind) -> usize {
        seq.tracks.iter().position(|tr| tr.kind == kind).unwrap()
    }

    /// Three 4 s clips on the video track, each with its sound linked
    /// under it.
    fn timeline() -> (Sequence, Vec<ClipId>) {
        let mut seq = Sequence::new_default();
        let vi = track_of(&seq, TrackKind::Video);
        let media = klipp_core::MediaId::new();
        let ids: Vec<ClipId> = (0..3).map(|_| seq.tracks[vi].append_clip(media, t(4))).collect();
        for &id in &ids {
            attach_linked_audio_to_clip(&mut seq, id, None).expect("linked");
        }
        (seq, ids)
    }

    fn dropped() -> Clip {
        Clip::media(klipp_core::MediaId::new(), t(0), t(2), t(0), t(2))
    }

    fn run(seq: &Sequence, edits: &[EditCommand], forward: bool) -> Sequence {
        let mut seqs = vec![seq.clone()];
        let mut pool = klipp_media::MediaPool::default();
        let mut fps = seq.fps;
        let mut view = DocumentView { sequences: &mut seqs, active: 0, media_pool: &mut pool, fps: &mut fps };
        if forward {
            edits.iter().for_each(|e| { view.apply_forward(e); });
        } else {
            edits.iter().rev().for_each(|e| { view.apply_inverse(e); });
        }
        seqs.remove(0)
    }

    fn span(seq: &Sequence, id: ClipId) -> (klipp_core::Timecode, klipp_core::Timecode) {
        let c = seq.all_clips().find(|c| c.id == id).expect("on the timeline");
        (c.timeline_in, c.timeline_out)
    }

    /// #2549: a clip let go on the cut between the first and second
    /// clip in insert mode goes in at that cut, with its sound under it,
    /// and what follows moves along on both tracks. It used to go after
    /// the last clip.
    #[test]
    fn a_clip_dropped_on_a_cut_in_insert_mode_goes_in_at_the_cut() {
        let (seq, ids) = timeline();
        let (vi, ai) = (track_of(&seq, TrackKind::Video), track_of(&seq, TrackKind::Audio));
        let (edits, placed) = media_drop_edits(&seq, dropped(), vi, Some(None), t(4), PasteMode::Insert, "Insert").expect("placed");
        let after = run(&seq, &edits, true);
        assert_eq!(span(&after, placed), (t(4), t(6)), "at the cut it was let go on");
        assert!(after.tracks[vi].find_clip(placed).is_some(), "on the track it was let go on");
        assert_eq!(span(&after, ids[0]), (t(0), t(4)));
        assert_eq!(span(&after, ids[1]), (t(6), t(10)), "the second clip moved along by the new one");
        assert_eq!(span(&after, ids[2]), (t(10), t(14)));
        let sound = after.tracks[vi].find_clip(placed).unwrap().linked_clip_id.expect("its sound linked");
        assert!(after.tracks[ai].find_clip(sound).is_some(), "its sound on the audio track");
        assert_eq!(span(&after, sound), (t(4), t(6)), "under it");
        let second_sound = seq.all_clips().find(|c| c.id == ids[1]).unwrap().linked_clip_id.unwrap();
        assert_eq!(span(&after, second_sound), (t(6), t(10)), "the sound moved with its picture");
        // One undo gives the timeline back as it was.
        let undone = run(&after, &edits, false);
        assert!(undone.all_clips().all(|c| c.id != placed && c.id != sound));
        for &id in &ids {
            assert_eq!(span(&undone, id), span(&seq, id));
        }
    }

    /// In overwrite mode it lays over what is there from the cut and
    /// nothing moves.
    #[test]
    fn a_clip_dropped_on_a_cut_in_overwrite_mode_covers_what_follows() {
        let (seq, ids) = timeline();
        let vi = track_of(&seq, TrackKind::Video);
        let (edits, placed) = media_drop_edits(&seq, dropped(), vi, Some(None), t(4), PasteMode::Overwrite, "Overwrite").expect("placed");
        let after = run(&seq, &edits, true);
        assert_eq!(span(&after, placed), (t(4), t(6)));
        assert_eq!(span(&after, ids[0]), (t(0), t(4)));
        assert_eq!(span(&after, ids[1]), (t(6), t(8)), "the second clip's head is covered");
        assert_eq!(span(&after, ids[2]), (t(8), t(12)), "nothing moved");
    }

    /// It goes on the track it was let go on, even when another track of
    /// its kind is the one targeted.
    #[test]
    fn a_clip_goes_on_the_track_it_was_dropped_on() {
        let (mut seq, _) = timeline();
        let v1 = track_of(&seq, TrackKind::Video);
        seq.tracks[v1].targeted = true;
        seq.add_video_track("Video 2".into());
        let v2 = seq.tracks.iter().position(|tr| tr.kind == TrackKind::Video && tr.clips.is_empty()).unwrap();
        let (edits, placed) = media_drop_edits(&seq, dropped(), v2, None, t(4), PasteMode::Overwrite, "Overwrite").expect("placed");
        let after = run(&seq, &edits, true);
        assert!(after.tracks[v2].find_clip(placed).is_some(), "on the track it was let go on, not the targeted one");
    }

    /// With no audio track, one is made for its sound, and the undo
    /// takes it away again.
    #[test]
    fn a_missing_audio_track_is_made_in_the_same_step() {
        let mut seq = Sequence::new_default();
        seq.tracks.retain(|tr| tr.kind != TrackKind::Audio);
        let vi = track_of(&seq, TrackKind::Video);
        let (edits, placed) = media_drop_edits(&seq, dropped(), vi, Some(None), t(0), PasteMode::Insert, "Insert").expect("placed");
        assert!(edits.iter().any(|e| matches!(e, EditCommand::AddTrack { .. })));
        let after = run(&seq, &edits, true);
        let sound = after.tracks[vi].find_clip(placed).unwrap().linked_clip_id.expect("its sound linked");
        let ai = track_of(&after, TrackKind::Audio);
        assert!(after.tracks[ai].find_clip(sound).is_some());
        let undone = run(&after, &edits, false);
        assert!(undone.tracks.iter().all(|tr| tr.kind != TrackKind::Audio), "the made track goes with the undo");
    }

    #[test]
    fn a_locked_track_takes_nothing() {
        let (mut seq, _) = timeline();
        let vi = track_of(&seq, TrackKind::Video);
        seq.tracks[vi].locked = true;
        assert!(media_drop_edits(&seq, dropped(), vi, None, t(4), PasteMode::Insert, "Insert").is_err());
    }
}
