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
                    if let Some(item) = self.state.media_pool.get(media_id) {
                        let duration = item.duration;
                        // Auto-set sequence resolution + FPS from first video clip
                        if !item.video_codec.is_empty() && item.resolution.width > 0 && item.resolution.height > 0 {
                            let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                            if seq.adopt_first_clip(item.resolution, item.fps).is_some() {
                                self.state.fps = seq.fps;
                            }
                        }
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
                                    Some(a) => self.state.push_history(EditCommand::SourceLinkedInsert {
                                        video_command: result.command,
                                        audio_track_index: a.track_index,
                                        audio_clips_before: a.clips_before,
                                        audio_clips_after: a.clips_after,
                                    }),
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

    /// If placing `media_id` onto the active sequence would introduce a
    /// video clip whose fps differs from the sequence's, opens the
    /// "Change Project Frame Rate?" modal and stashes `pending_cmd` for
    /// re-dispatch when the user answers. Returns `true` when the caller
    /// should abort the current handler (the append is deferred).
    ///
    /// Skipped when:
    /// - the one-shot bypass is set (prompt was just answered),
    /// - the media has no video track (audio-only),
    /// - the sequence has no video clips yet (silent first-clip auto-set
    ///   path in the append handler already handles this), or
    /// - fps already matches.
    pub(super) fn maybe_prompt_framerate_mismatch(
        &mut self,
        media_id: &klipp_core::MediaId,
        pending_cmd: AppCommand,
    ) -> bool {
        if self.suppress_fps_prompt {
            return false;
        }
        let (media_fps, is_video) = match self.state.media_pool.get(media_id) {
            Some(item) => (
                item.fps,
                !item.video_codec.is_empty()
                    && item.resolution.width > 0
                    && item.resolution.height > 0,
            ),
            None => return false,
        };
        if !is_video {
            return false;
        }
        let seq_idx = self.state.active_sequence_idx;
        let Some(seq) = self.state.sequences.get(seq_idx) else {
            return false;
        };
        let has_video_clips = seq
            .tracks
            .iter()
            .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
            .any(|t| !t.clips.is_empty());
        if !has_video_clips {
            return false;
        }
        let sequence_fps = seq.fps;
        if media_fps == sequence_fps {
            return false;
        }
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

        // FCP: the first video clip sets the sequence resolution + fps.
        let mut autoset = None;
        if has_video && item.resolution.width > 0 && item.resolution.height > 0 {
            let seq = &mut self.state.sequences[seq_idx];
            autoset = seq.adopt_first_clip(item.resolution, item.fps);
            if autoset.is_some() {
                self.state.fps = seq.fps;
            }
        }

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
        let new_in = new_clip.timeline_in;
        let new_out = new_clip.timeline_out;

        // Collect indices of clips to modify (process in reverse to preserve indices)
        let mut to_remove = Vec::new();
        let mut to_trim_left = Vec::new();  // trim in-point forward (clip extends past new_out)
        let mut to_trim_right = Vec::new(); // trim out-point back (clip starts before new_in)
        let mut to_split = Vec::new();      // clip fully contains new range — needs split

        for (i, clip) in track.clips.iter().enumerate() {
            let c_in = clip.timeline_in;
            let c_out = clip.timeline_out;

            // No overlap
            if c_out <= new_in || c_in >= new_out {
                continue;
            }

            if c_in >= new_in && c_out <= new_out {
                // Fully covered — remove
                to_remove.push(i);
            } else if c_in < new_in && c_out > new_out {
                // Clip contains the new range — split into two
                to_split.push(i);
            } else if c_in < new_in && c_out > new_in {
                // Overlaps from left — trim right edge back
                to_trim_right.push(i);
            } else if c_in < new_out && c_out > new_out {
                // Overlaps from right — trim left edge forward
                to_trim_left.push(i);
            }
        }

        // Handle splits first (creates a new clip)
        for &i in to_split.iter().rev() {
            let clip = track.clips[i].clone();
            let left_dur = new_in - clip.timeline_in;
            let right_offset = new_out - clip.timeline_in;

            // Left portion: keep original in, trim out to new_in
            track.clips[i].timeline_out = new_in;
            track.clips[i].source_out = clip.source_in + left_dur;
            // A glide is the whole clip's move; the pieces keep their keys (#611).
            track.clips[i].frame_glide = None;

            // Right portion: new clip starting at new_out
            let mut right_clip = klipp_timeline::Clip {
                id: klipp_core::ClipId::new(),
                kind: clip.kind.clone(),
                timeline_in: new_out,
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
            track.clips.insert(i + 1, right_clip);
        }

        // Trim left edges forward (clip starts before new_out, ends after)
        for &i in &to_trim_left {
            let clip = &track.clips[i];
            let trim_amount = new_out - clip.timeline_in;
            track.clips[i].source_in += trim_amount;
            track.clips[i].timeline_in = new_out;
        }

        // Trim right edges back (clip starts before new_in, ends after)
        for &i in &to_trim_right {
            let clip = &track.clips[i];
            let trim_amount = clip.timeline_out - new_in;
            track.clips[i].source_out -= trim_amount;
            track.clips[i].timeline_out = new_in;
        }

        // Remove fully covered clips (reverse order)
        for &i in to_remove.iter().rev() {
            track.clips.remove(i);
        }

        // Insert the new clip
        track.clips.push(new_clip);
        track.sort_clips();
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
        }

        // Remove fully covered (reverse order)
        for &i in to_remove.iter().rev() {
            track.clips.remove(i);
        }
    }
}
