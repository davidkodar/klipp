//! Media: files loaded into the pool, proxies made, missing files relinked,
//! the pool's items and bins, and the browser's commands (#826).

use super::*;

impl KlippApp {
    pub(super) fn handle_media_commands(&mut self, cmd: &AppCommand, needs_refresh: &mut bool) -> bool {
        // INLINE: LoadMediaFile uses two-phase dispatch. It conditionally returns
        // true (duplicate found, skip engine) or false (stage pending_media_path
        // and let the dispatch loop's fallthrough forwarder send the command to
        // the engine for probing).
        if let AppCommand::LoadMediaFile { ref path, media_id } = cmd {
            // A media id already in the pool is a re-probe — a relink,
            // or a file edited outside Klipp and reloaded — not an
            // import. It goes to the engine as it is: the duplicate
            // checks below would find the item itself, say "already in
            // the media pool", and drop it, so a relinked clip kept its
            // old duration and an externally edited one never reloaded.
            if self.state.media_pool.get(media_id).is_some() {
                return false;
            }
            if let Some(existing) = self.state.media_pool.find_by_path(path) {
                let existing_id = existing.id;
                self.state.pending_undo_imports.remove(media_id);
                self.state.toast.show(format!(
                    "'{}' is already in the media pool",
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                ));
                self.state.source_media_id = Some(existing_id);
                if let Some(place) = retarget_pending_drop(&mut self.state.pending_media_drop, *media_id, existing_id) {
                    self.send_commands(vec![place]);
                }
                return true;
            }
            if let Some(fp) = compute_media_fingerprint(path) {
                if let Some(existing) = self.state.media_pool.find_by_fingerprint(fp) {
                    let existing_id = existing.id;
                    self.state.pending_undo_imports.remove(media_id);
                    let existing_name = existing.filename.clone();
                    self.state.toast.show(format!(
                        "'{}' appears to be the same file as '{}'",
                        path.file_name().unwrap_or_default().to_string_lossy(),
                        existing_name,
                    ));
                    self.state.source_media_id = Some(existing_id);
                    if let Some(place) = retarget_pending_drop(&mut self.state.pending_media_drop, *media_id, existing_id) {
                        self.send_commands(vec![place]);
                    }
                    return true;
                }
            }
            self.state.pending_media_path = Some(path.clone());
            return false;
        }

        if self.handle_media_loading(cmd, needs_refresh) { return true; }
        if self.handle_media_pool(cmd, needs_refresh) { return true; }
        if self.handle_media_proxies(cmd, needs_refresh) { return true; }
        if self.handle_media_relinking(cmd, needs_refresh) { return true; }
        if self.handle_media_bins(cmd, needs_refresh) { return true; }
        false
    }

    fn handle_media_loading(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        match cmd {
                AppCommand::ImportMedia { ref path } | AppCommand::ImportMediaWithAnalysis { ref path, .. }
                    if path.extension().and_then(|e| e.to_str()).and_then(klipp_core::media_extensions::camera_raw_video_refusal).is_some() =>
                {
                    // Camera RAW video is a non-goal (#85): say why.
                    let why = path.extension().and_then(|e| e.to_str()).and_then(klipp_core::media_extensions::camera_raw_video_refusal).unwrap_or_default();
                    self.state.toast.show(why);
                    true
                }
                AppCommand::ImportMedia { ref path } | AppCommand::ImportMediaWithAnalysis { ref path, .. } => {
                    // Import media: load file to get metadata, then add to pool
                    let media_id = klipp_core::MediaId::new();
                    self.state.pending_undo_imports.insert(media_id);
                    if matches!(cmd, AppCommand::ImportMediaWithAnalysis { analyze: false, .. }) {
                        self.state.pending_skip_analysis.insert(media_id);
                    }
                    self.state.pending_media_path = None; // Don't auto-append
                    let _ = self.cmd_tx.send(AppCommand::LoadMediaFile {
                        path: path.clone(),
                        media_id,
                    });
                    true
                }
                AppCommand::ImportMediaTo { ref path, drop } => {
                    // Import, and remember where to drop it once it loads.
                    let media_id = klipp_core::MediaId::new();
                    self.state.pending_media_path = None;
                    if *drop == klipp_core::events::ClipDrop::PoolOnly {
                        self.state.pending_undo_imports.insert(media_id);
                    }
                    if *drop != klipp_core::events::ClipDrop::PoolOnly {
                        self.state.pending_media_drop.insert(media_id, *drop);
                    }
                    // Routed, not sent to the engine directly, so the
                    // duplicate check runs: a file already in the pool is
                    // placed from there instead of being imported twice.
                    self.send_commands(vec![AppCommand::LoadMediaFile {
                        path: path.clone(),
                        media_id,
                    }]);
                    true
                }
                AppCommand::PlaceMedia { media_id, drop } => {
                    match drop {
                        klipp_core::events::ClipDrop::PoolOnly => {}
                        klipp_core::events::ClipDrop::EndOfTimeline => {
                            // The existing linked-import path already appends
                            // onto V1/A1 and links video to its audio.
                            self.send_commands(vec![AppCommand::ImportMediaLinked {
                                media_id: *media_id,
                                on_new_tracks: false,
                            }]);
                        }
                        klipp_core::events::ClipDrop::AtPlayhead => {
                            self.place_media_at_playhead(*media_id);
                        }
                        klipp_core::events::ClipDrop::AppendToTrack { track_index } => {
                            self.send_commands(vec![AppCommand::AppendClipToTrack {
                                media_id: *media_id,
                                track_index: *track_index,
                            }]);
                        }
                    }
                    true
                }
                AppCommand::ImportMediaFolder { ref directory } => {
                    let media_extensions = klipp_core::media_extensions::all_media_extensions();
                    let card = klipp_media::card::card_kind(directory);
                    let mut imported = 0u32;
                    let mut skipped = 0u32;
                    for entry in walkdir::WalkDir::new(directory)
                        .into_iter()
                        .filter_map(|e| e.ok())
                    {
                        if entry.file_type().is_file() {
                            if let Some(ext) = entry.path().extension().and_then(|e| e.to_str()) {
                                if media_extensions.iter().any(|me| me.eq_ignore_ascii_case(ext)) {
                                    // A card's proxies, thumbnails and
                                    // indexes are not clips.
                                    if klipp_media::card::is_card_side_file(entry.path()) {
                                        skipped += 1;
                                        continue;
                                    }
                                    let media_id = klipp_core::MediaId::new();
                                    self.state.pending_undo_imports.insert(media_id);
                                    let _ = self.cmd_tx.send(AppCommand::LoadMediaFile {
                                        path: entry.path().to_path_buf(),
                                        media_id,
                                    });
                                    imported += 1;
                                }
                            }
                        }
                    }
                    if imported > 0 {
                        self.state.toast.show(match card {
                            Some(kind) => format!("Importing {imported} clips from a {kind}"),
                            None => format!("Importing {imported} files from folder"),
                        });
                    }
                    tracing::info!("Folder import: {} files from {:?} ({} card extras skipped)", imported, directory, skipped);
                    true
                }
                AppCommand::AppendClip { media_id } => {
                    if self.maybe_prompt_framerate_mismatch(media_id, cmd.clone()) {
                        return true;
                    }
                    self.suppress_fps_prompt = false;
                    if let Some(item) = self.state.media_pool.get(media_id) {
                        let duration = item.duration;
                        let is_audio_only = item.video_codec.is_empty();

                        // Auto-set sequence resolution + FPS from first video clip (FCP behavior)
                        if !is_audio_only && item.resolution.width > 0 && item.resolution.height > 0 {
                            let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                            if seq.adopt_first_clip(item.resolution, item.fps).is_some() {
                                self.state.fps = seq.fps;
                            }
                        }

                        // Route: audio-only to the audio track the source is
                        // patched to, video to the video one (#915); with no
                        // patch, the first of the kind.
                        let target_kind = if is_audio_only {
                            klipp_timeline::TrackKind::Audio
                        } else {
                            klipp_timeline::TrackKind::Video
                        };
                        let track_index = self.state.sequences[self.state.active_sequence_idx]
                            .source_track(target_kind)
                            .unwrap_or(0);
                        let appended_id = {
                            let track = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(track_index);
                            track.map(|t| t.append_clip(*media_id, duration))
                        };
                        if let Some(clip_id) = appended_id {
                            // The media's rotation belongs on the clip, as the
                            // drag-and-drop path sets it. Without it a phone
                            // clip plays upright — the viewer's decoder takes
                            // rotation from the media item — and exports
                            // sideways, because export takes it from the clip.
                            if !is_audio_only {
                                let rotation = item.source_rotation;
                                if let Some(c) = self.state.sequences[self.state.active_sequence_idx]
                                    .tracks[track_index]
                                    .find_clip_mut(clip_id)
                                {
                                    c.source_rotation = rotation;
                                }
                            }
                            self.apply_input_defaults(clip_id, *media_id);
                            // The file's audio, linked to this clip. Without
                            // it the timeline is silent and so is every
                            // render made from it.
                            let audio = self.append_linked_audio(*media_id, clip_id);
                            let clip = self.state.sequences[self.state.active_sequence_idx].tracks[track_index].find_clip(clip_id).unwrap().clone();
                            match audio {
                                // One history entry for the pair, so a
                                // single undo takes both back.
                                Some(a) => self.state.push_history(EditCommand::LinkedImport {
                                    video: Some((clip, track_index)),
                                    audio: Some((a.clip, a.track_index)),
                                    created_video_track_at: None,
                                    created_audio_track_at: a.created_track_at,
                                    autoset: None,
                                }),
                                None => self.state.push_history(EditCommand::LegacyAppendClip {
                                    clip,
                                    track_index,
                                }),
                            }
                            self.sync_sequence_to_engine();
                            self.state.selected_clip = Some(clip_id);
                            self.state.dirty = true;
                        }
                    }
                    true
                }
                AppCommand::AttachLinkedAudio { clip_id } => {
                    let seq_idx = self.state.active_sequence_idx;
                    // Only for media that has sound to attach; asking
                    // for it on a silent file would otherwise lay down a
                    // clip of nothing.
                    let has_audio = self.state.sequences[seq_idx]
                        .all_clips()
                        .find(|c| c.id == *clip_id)
                        .and_then(|c| match c.kind {
                            klipp_timeline::ClipKind::Media { media_id } => Some(media_id),
                            _ => None,
                        })
                        .and_then(|m| self.state.media_pool.get(&m))
                        .is_some_and(|i| i.has_audio || i.synced_audio.is_some());
                    if !has_audio {
                        self.state.toast.show("That clip's media has no audio");
                        return true;
                    }
                    let external = self.state.sequences[seq_idx]
                        .all_clips()
                        .find(|c| c.id == *clip_id)
                        .and_then(|c| c.media_id())
                        .and_then(|m| self.external_audio_of(m));
                    match attach_linked_audio_to_clip(&mut self.state.sequences[seq_idx], *clip_id, external) {
                        Some(a) => {
                            self.state.push_history(EditCommand::LinkedImport {
                                video: None,
                                audio: Some((a.clip, a.track_index)),
                                created_video_track_at: None,
                                created_audio_track_at: a.created_track_at,
                                autoset: None,
                            });
                            self.sync_sequence_to_engine();
                            self.state.dirty = true;
                            self.state.toast.show("Audio attached");
                        }
                        None => {
                            self.state.toast.show("That clip already has its audio");
                        }
                    }
                    true
                }
                AppCommand::AppendMediaBatchToTrack { media_ids, track_index } => {
                    let Some(kind) = self.state.sequences[self.state.active_sequence_idx].tracks.get(*track_index).map(|t| t.kind) else {
                        return true;
                    };
                    let (fit, skipped): (Vec<_>, Vec<_>) = media_ids.iter().partition(|id| {
                        self.state.media_pool.get(id).is_some_and(|item| crate::drag_drop::media_fits_track(item, kind))
                    });
                    if fit.is_empty() {
                        self.state.toast.show(format!("None of the {} clips can go on that track", media_ids.len()));
                        return true;
                    }
                    // One prompt for the batch, on its first video clip; answered,
                    // the batch comes back with the prompt suppressed.
                    if let Some(first) = fit.iter().find(|id| self.state.media_pool.get(id).is_some_and(|i| !i.video_codec.is_empty())) {
                        if self.maybe_prompt_framerate_mismatch(first, cmd.clone()) {
                            return true;
                        }
                    }
                    self.suppress_fps_prompt = false;
                    self.state.active_history_mut().begin_batch();
                    for id in &fit {
                        self.append_media_to_track(id, track_index);
                    }
                    self.state.active_history_mut().end_batch();
                    if !skipped.is_empty() {
                        self.state.toast.show(format!(
                            "Added {} clip(s); {} skipped — {} on a {} track",
                            fit.len(),
                            skipped.len(),
                            if matches!(kind, klipp_timeline::TrackKind::Audio) { "video" } else { "audio-only" },
                            match kind { klipp_timeline::TrackKind::Audio => "audio", klipp_timeline::TrackKind::Video => "video", _ => "text" },
                        ));
                    }
                    true
                }
                AppCommand::AppendClipToTrack { media_id, track_index } => {
                    // Synthetic title asset path — read-only fork
                    // before any framerate-prompt logic. Creates a
                    // title clip linked to the asset rather than a
                    // media clip; the MediaItem is just a discoverable
                    // entry in the pool.
                    let item_kind = self.state.media_pool.get(media_id).map(|i| i.kind.clone());
                    if matches!(item_kind, Some(klipp_media::MediaItemKind::SyntheticTitle)) {
                        self.append_title_asset_to_timeline(*media_id, *track_index);
                        return true;
                    }
                    if self.maybe_prompt_framerate_mismatch(media_id, cmd.clone()) {
                        return true;
                    }
                    self.suppress_fps_prompt = false;
                    self.append_media_to_track(media_id, track_index);
                    true
                }
                AppCommand::InsertAudioMediaClip { media_id, track_index, timeline_in } => {
                    // Audio-clip-only insertion at a specific
                    // timeline position. Caller is responsible for
                    // ensuring `media_id` is already in the pool —
                    // this command does not import or probe (which
                    // is what makes it cheap enough for opt-in
                    // sweetener layers like the hyperzoom whoosh to
                    // dispatch synchronously inside another
                    // handler).
                    let active_idx = self.state.active_sequence_idx;
                    let result = insert_audio_media_clip_into_seq(
                        &mut self.state.sequences[active_idx],
                        &self.state.media_pool,
                        *media_id,
                        *track_index,
                        *timeline_in,
                    );
                    match result {
                        Ok(inserted) => {
                            self.state.push_history(EditCommand::LegacyAppendClip {
                                clip: inserted.clip,
                                track_index: inserted.track_index,
                            });
                            self.state.dirty = true;
                        }
                        Err(e) => {
                            tracing::warn!(
                                "InsertAudioMediaClip rejected for media={:?} track={:?}: {e}",
                                media_id, track_index,
                            );
                        }
                    }
                    true
                }
                AppCommand::ImportMediaLinked { media_id, on_new_tracks } => {
                    if self.maybe_prompt_framerate_mismatch(media_id, cmd.clone()) {
                        return true;
                    }
                    self.suppress_fps_prompt = false;
                    if let Some(item) = self.state.media_pool.get(media_id).cloned() {
                        // Accumulate everything this gesture mutates so a
                        // single ctrl+z can revert it as one atomic unit.
                        let mut hist_video: Option<(klipp_timeline::Clip, usize)> = None;
                        let mut hist_audio: Option<(klipp_timeline::Clip, usize)> = None;
                        let mut hist_created_video_track_at: Option<usize> = None;
                        let mut hist_created_audio_track_at: Option<usize> = None;
                        let mut hist_autoset: Option<((klipp_core::Resolution, klipp_core::Fps), (klipp_core::Resolution, klipp_core::Fps))> = None;

                        // Auto-set sequence resolution + FPS from first video clip
                        if !item.video_codec.is_empty() && item.resolution.width > 0 && item.resolution.height > 0 {
                            let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                            hist_autoset = seq.adopt_first_clip(item.resolution, item.fps);
                            if hist_autoset.is_some() {
                                self.state.fps = seq.fps;
                            }
                        }
                        // Check for browser range selection (trimmed source in/out)
                        let range = self.state.browser_range_selection.take()
                            .filter(|r| r.media_id == *media_id);

                        let (use_duration, source_in_override, source_out_override) = if let Some(ref sel) = range {
                            let rate = item.fps.num;
                            let src_in = klipp_core::RationalTime {
                                value: (sel.range_in_secs * rate as f64).round() as i64,
                                rate,
                            };
                            let src_out = klipp_core::RationalTime {
                                value: (sel.range_out_secs * rate as f64).round() as i64,
                                rate,
                            };
                            let dur = src_out - src_in;
                            (dur, Some(src_in), Some(src_out))
                        } else {
                            source_marks_range(&item)
                        };

                        let has_video = !item.video_codec.is_empty();
                        let has_audio = item.has_audio;

                        // Helper closure to apply range override to a clip
                        let apply_range = |track: &mut klipp_timeline::Track, clip_id: klipp_core::ClipId| {
                            if let (Some(si), Some(so)) = (source_in_override, source_out_override) {
                                if let Some(c) = track.find_clip_mut(clip_id) {
                                    c.source_in = si;
                                    c.source_out = so;
                                }
                            }
                        };

                        if has_video && has_audio {
                            // Shift mode: always force a fresh V track on
                            // top of the stack and a fresh A track at the
                            // bottom of the audio group. Default mode:
                            // V1 / first existing A track (auto-create A
                            // only if missing).
                            let video_track_idx = if *on_new_tracks {
                                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                                let video_count = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video).count();
                                seq.add_video_track(format!("Video {}", video_count + 1));
                                // add_video_track inserts at index 0.
                                hist_created_video_track_at = Some(0);
                                Some(0usize)
                            } else {
                                self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                                    .position(|t| t.kind == klipp_timeline::TrackKind::Video)
                            };
                            let audio_track_idx = if *on_new_tracks {
                                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                                let audio_count = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Audio).count();
                                seq.add_audio_track(format!("Audio {}", audio_count + 1));
                                let new_idx = seq.tracks.len() - 1;
                                hist_created_audio_track_at = Some(new_idx);
                                Some(new_idx)
                            } else {
                                match self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                                    .position(|t| t.kind == klipp_timeline::TrackKind::Audio)
                                {
                                    Some(i) => Some(i),
                                    None => {
                                        // Auto-create audio track to host the imported audio.
                                        let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                                        let audio_count = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Audio).count();
                                        seq.add_audio_track(format!("Audio {}", audio_count + 1));
                                        let new_idx = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Audio);
                                        hist_created_audio_track_at = new_idx;
                                        new_idx
                                    }
                                }
                            };

                            if let (Some(vi), Some(ai)) = (video_track_idx, audio_track_idx) {
                                let video_clip_id = self.state.sequences[self.state.active_sequence_idx].tracks[vi].append_clip(*media_id, use_duration);
                                apply_range(&mut self.state.sequences[self.state.active_sequence_idx].tracks[vi], video_clip_id);
                                let audio_clip_id = self.state.sequences[self.state.active_sequence_idx].tracks[ai].append_clip(*media_id, use_duration);
                                apply_range(&mut self.state.sequences[self.state.active_sequence_idx].tracks[ai], audio_clip_id);

                                if let Some(vc) = self.state.sequences[self.state.active_sequence_idx].tracks[vi].find_clip_mut(video_clip_id) {
                                    vc.linked_clip_id = Some(audio_clip_id);
                                    vc.source_rotation = item.source_rotation;
                                }
                                if let Some(ac) = self.state.sequences[self.state.active_sequence_idx].tracks[ai].find_clip_mut(audio_clip_id) {
                                    ac.linked_clip_id = Some(video_clip_id);
                                }

                                // Seed the IDT with the auto-detected log
                                // format on the video clip (audio clip has
                                // no InputTransform path). Without this,
                                // V+A drag-import lands the camera-log
                                // footage with no IDT applied — the user
                                // sees a flat raw-log image and has to
                                // pick the format manually for every
                                // imported clip.
                                self.apply_input_defaults(video_clip_id, *media_id);

                                if let Some(vc) = self.state.sequences[self.state.active_sequence_idx].tracks[vi].find_clip(video_clip_id) {
                                    hist_video = Some((vc.clone(), vi));
                                }
                                if let Some(ac) = self.state.sequences[self.state.active_sequence_idx].tracks[ai].find_clip(audio_clip_id) {
                                    hist_audio = Some((ac.clone(), ai));
                                }

                                self.state.selected_clip = Some(video_clip_id);
                                self.sync_sequence_to_engine();
                                self.state.dirty = true;
                            }
                        } else {
                            let target_kind = if has_video {
                                klipp_timeline::TrackKind::Video
                            } else {
                                klipp_timeline::TrackKind::Audio
                            };
                            let track_idx = if *on_new_tracks {
                                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                                let count = seq.tracks.iter().filter(|t| t.kind == target_kind).count();
                                if has_video {
                                    seq.add_video_track(format!("Video {}", count + 1));
                                    hist_created_video_track_at = Some(0);
                                    0usize
                                } else {
                                    seq.add_audio_track(format!("Audio {}", count + 1));
                                    let idx = seq.tracks.len() - 1;
                                    hist_created_audio_track_at = Some(idx);
                                    idx
                                }
                            } else {
                                self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                                    .position(|t| t.kind == target_kind)
                                    .unwrap_or(0)
                            };
                            if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(track_idx) {
                                let clip_id = track.append_clip(*media_id, use_duration);
                                apply_range(track, clip_id);
                                if has_video {
                                    if let Some(c) = track.find_clip_mut(clip_id) {
                                        c.source_rotation = item.source_rotation;
                                    }
                                }
                                // Seed the IDT with the auto-detected log
                                // format (video-only path; audio has no
                                // InputTransform).
                                if has_video {
                                    self.apply_input_defaults(clip_id, *media_id);
                                }
                                if let Some(c) = self.state.sequences[self.state.active_sequence_idx].tracks[track_idx].find_clip(clip_id) {
                                    let snap = (c.clone(), track_idx);
                                    if has_video { hist_video = Some(snap); } else { hist_audio = Some(snap); }
                                }
                                self.state.selected_clip = Some(clip_id);
                                self.sync_sequence_to_engine();
                                self.state.dirty = true;
                            }
                        }

                        if hist_video.is_some() || hist_audio.is_some() {
                            self.state.active_history_mut().push(crate::history::HistoryEntry {
                                command: crate::history::EditCommand::LinkedImport {
                                    video: hist_video,
                                    audio: hist_audio,
                                    created_video_track_at: hist_created_video_track_at,
                                    created_audio_track_at: hist_created_audio_track_at,
                                    autoset: hist_autoset,
                                },
                            });
                        }
                    }
                    true
                }
            _ => false,
        }
    }

    fn handle_media_proxies(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        match cmd {
                AppCommand::GenerateProxy { media_id } => {
                    self.spawn_proxy_generation(*media_id);
                    true
                }
                AppCommand::CancelProxy { media_id } => {
                    // Kill all running proxy ffmpeg processes — we don't track which PID
                    // maps to which media_id, so kill all running ones. This is safe because
                    // proxy generation is infrequent and usually one-at-a-time.
                    for pid_arc in &self.proxy_children {
                        let pid = pid_arc.load(std::sync::atomic::Ordering::Relaxed);
                        if pid != 0 {
                            #[cfg(unix)]
                            {
                                use std::process::Command;
                                let _ = Command::new("kill").arg(pid.to_string()).spawn();
                            }
                        }
                    }
                    self.state.proxy_generating.remove(media_id);
                    let filename = self.state.media_pool.get(media_id)
                        .map(|m| m.filename.clone())
                        .unwrap_or_default();
                    tracing::info!("Proxy generation cancelled for {}", filename);
                    self.state.proxy_status_message = Some((
                        format!("Proxy cancelled for {}", filename),
                        std::time::Instant::now(),
                    ));
                    true
                }
                AppCommand::SetMulticamMode { on, open } => {
                    self.state.set_multicam_mode(*on, *open);
                    true
                }
                AppCommand::SetViewerOverlay { overlay, on } => {
                    let o = &mut self.state.viewer_overlays;
                    match overlay {
                        klipp_core::ViewerOverlay::SafeAreas => o.safe_areas = *on,
                        klipp_core::ViewerOverlay::Grid => o.grid = *on,
                        klipp_core::ViewerOverlay::Checkerboard => o.checkerboard = *on,
                    }
                    true
                }
                AppCommand::SetUseProxies(enabled) => {
                    self.state.use_proxies = *enabled;
                    // Forward to engine
                    let _ = self.cmd_tx.send(cmd.clone());
                    true
                }
            _ => false,
        }
    }

    fn handle_media_relinking(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        match cmd {
                AppCommand::ExportMetadataSheet { path, ale } => {
                    let mut items: Vec<&klipp_media::MediaItem> = self.state.media_pool.all().filter(|m| matches!(m.kind, klipp_media::MediaItemKind::File)).collect();
                    items.sort_by_key(|m| m.filename.to_lowercase());
                    let text = if *ale {
                        klipp_project::metadata_sheet::export_ale(&items, self.state.fps)
                    } else {
                        klipp_project::metadata_sheet::export_csv(&items)
                    };
                    let n = items.len();
                    match std::fs::write(path, text) {
                        Ok(()) => self.state.toast.show(format!("Wrote the metadata of {n} clips to {}", path.file_name().unwrap_or_default().to_string_lossy())),
                        Err(e) => self.state.toast.show(format!("Could not write the sheet: {e}")),
                    }
                    true
                }
                AppCommand::ImportMetadataSheet { path } => {
                    let text = match std::fs::read_to_string(path) {
                        Ok(t) => t,
                        Err(e) => {
                            self.state.toast.show(format!("Could not read the sheet: {e}"));
                            return true;
                        }
                    };
                    let rows = match klipp_project::metadata_sheet::parse_sheet(&text) {
                        Ok(r) => r,
                        Err(e) => {
                            self.state.toast.show(format!("Could not read the sheet: {e}"));
                            return true;
                        }
                    };
                    let (mut applied, mut unmatched) = (0usize, 0usize);
                    for row in &rows {
                        let ids: Vec<klipp_core::MediaId> = self.state.media_pool.all().filter(|m| klipp_project::metadata_sheet::row_names(m, &row.name)).map(|m| m.id).collect();
                        if ids.is_empty() {
                            unmatched += 1;
                        }
                        for id in ids {
                            if let Some(item) = self.state.media_pool.get_mut(&id) {
                                if klipp_project::metadata_sheet::apply_row(item, row) {
                                    applied += 1;
                                }
                            }
                        }
                    }
                    if applied > 0 {
                        self.state.dirty = true;
                    }
                    self.state.toast.show(match unmatched {
                        0 => format!("Read the sheet into {applied} clips"),
                        n => format!("Read the sheet into {applied} clips; {n} rows named no clip in the pool"),
                    });
                    true
                }
                AppCommand::RefreshMediaMetadata { media_ids } => {
                    // The same re-probe a relink uses: the file is read
                    // again and the item keeps what the user said about it.
                    let files: Vec<(klipp_core::MediaId, std::path::PathBuf)> = media_ids
                        .iter()
                        .filter_map(|id| self.state.media_pool.get(id))
                        .filter(|m| matches!(m.kind, klipp_media::MediaItemKind::File) && !m.offline)
                        .map(|m| (m.id, m.path.clone()))
                        .collect();
                    let n = files.len();
                    for (media_id, path) in files {
                        let _ = self.cmd_tx.send(AppCommand::LoadMediaFile { path, media_id });
                    }
                    self.state.toast.show(match n {
                        0 => "No online files to read".to_string(),
                        1 => "Reading the file's metadata".to_string(),
                        n => format!("Reading the metadata of {n} files"),
                    });
                    true
                }
                AppCommand::CleanOrphanedCaches => {
                    let cleaned = self.clean_orphaned_caches();
                    self.state.toast.show(cleaned);
                    true
                }
                AppCommand::ClearCacheCategory { path } => {
                    // Wipe the directory then recreate it so later writes
                    // (proxy encoder, filmstrip writer, audio peaks writer,
                    // render cache) don't fail on a missing parent.
                    //
                    // Audio peaks lives in `audio-peaks` AND the heavy PCM
                    // cache lives in `audio-cache`. When the user clears
                    // the audio category the panel passes `audio-peaks` as
                    // the primary path; we also wipe the sibling here so
                    // the UI size stays honest with what's on disk.
                    let is_audio_peaks = path.file_name()
                        .and_then(|n| n.to_str())
                        .map(|s| s == "audio-peaks")
                        .unwrap_or(false);

                    let wipe = |p: &std::path::Path| -> std::io::Result<()> {
                        if p.exists() {
                            std::fs::remove_dir_all(p)?;
                        }
                        std::fs::create_dir_all(p)?;
                        Ok(())
                    };

                    match wipe(path) {
                        Ok(()) => {
                            if is_audio_peaks {
                                if let Some(parent) = path.parent() {
                                    let sibling = parent.join("audio-cache");
                                    if let Err(e) = wipe(&sibling) {
                                        tracing::warn!(
                                            "Failed to wipe sibling audio-cache at {}: {e}",
                                            sibling.display()
                                        );
                                    }
                                }
                            }
                            tracing::info!("Cleared cache directory: {}", path.display());
                            self.state.toast.show(format!(
                                "Cleared {}",
                                path.file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("cache")
                            ));
                            self.refresh_cache_categories();
                        }
                        Err(e) => {
                            tracing::warn!("Failed to clear {}: {e}", path.display());
                            self.state.toast.show(format!("Clear failed: {e}"));
                        }
                    }
                    true
                }
                AppCommand::RefreshCacheManager => {
                    self.refresh_cache_categories();
                    true
                }
                AppCommand::OpenCacheManager => {
                    self.refresh_cache_categories();
                    self.state.show_cache_manager = true;
                    true
                }
                AppCommand::RelinkMediaByHand { media_id, new_path } => {
                    // What the project knew of the clip, taken before the
                    // relink overwrites it, then checked against the file
                    // off the UI thread (it is read with ffmpeg).
                    if let Some(item) = self.state.media_pool.get(media_id) {
                        let wanted = klipp_media::relink::Wanted::from_item(item);
                        let (tx, path) = (self.relink_check_tx.clone(), new_path.clone());
                        let _ = std::thread::Builder::new().name("klipp-relink-check".into()).spawn(move || {
                            let verdict = klipp_media::relink::check_pick(&wanted, &path);
                            let _ = tx.send((wanted.filename, path, verdict));
                        });
                    }
                    self.relink_media(*media_id, new_path);
                    self.state.dirty = true;
                    true
                }
                AppCommand::RelinkMedia { media_id, new_path } => {
                    // A step undo takes back (#2086); several relinked at
                    // once arrive in one batch, one step.
                    let before = self.state.media_pool.get(media_id).cloned();
                    self.relink_media(*media_id, new_path);
                    if let (Some(old), Some(item)) = (before, self.state.media_pool.get(media_id)) {
                        let edit = crate::history::EditCommand::RelinkMedia { media_id: *media_id, old_path: old.path.clone(), old_offline: old.offline, old_item: Box::new(old), new_path: item.path.clone(), new_offline: item.offline };
                        self.state.push_history(edit);
                    }
                    self.state.dirty = true;
                    true
                }
                AppCommand::TranscribeClip { media_id } => {
                    let info = self.state.media_pool.get(media_id).map(|item| {
                        if item.has_audio {
                            brief_to_transcription(self.state.sequences.get(self.state.active_sequence_idx).and_then(|s| s.brief.as_ref()));
                            klipp_analysis::service::request_transcription(item);
                        }
                        (item.has_audio, item.filename.clone())
                    });
                    if let Some((has_audio, filename)) = info {
                        if has_audio {
                            self.state.toast.show(format!("Transcribing {filename}\u{2026}"));
                        } else {
                            self.state.toast.show(format!("{filename} has no sound to transcribe"));
                        }
                    }
                    true
                }
                AppCommand::RelinkAllFromDirectory { directory } => {
                    // Searched and scored off the UI thread; the relink
                    // dialog shows what it found, and relinks only what
                    // the user keeps ticked.
                    self.relink_panel.request_scan(directory.clone());
                    self.state.show_relink_dialog = true;
                    true
                }
                AppCommand::ShowRelinkDialog => {
                    self.state.show_relink_dialog = true;
                    true
                }
            _ => false,
        }
    }

    fn handle_media_pool(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        match cmd {
                AppCommand::AddMediaTag { media_id, tag } => {
                    if let Some(item) = self.state.media_pool.get_mut(media_id) {
                        if !item.tags.contains(tag) {
                            item.tags.push(tag.clone());
                            self.state.dirty = true;
                        }
                    }
                    // The shared pool too: the control socket reads tags
                    // from there.
                    self.sync_media_pool_to_engine();
                    true
                }
                AppCommand::RemoveMediaTag { media_id, tag_index } => {
                    if let Some(item) = self.state.media_pool.get_mut(media_id) {
                        if *tag_index < item.tags.len() {
                            item.tags.remove(*tag_index);
                            self.state.dirty = true;
                        }
                    }
                    self.sync_media_pool_to_engine();
                    true
                }
                // Media pool management
                AppCommand::RemoveFromPool { media_id } => {
                    let usage = media_usage_count(*media_id, &self.state.sequences);
                    if usage > 0 {
                        self.state.show_remove_media_confirm = Some(*media_id);
                    } else if self.state.media_pool.get(media_id).is_some() {
                        // Cascade: drop any subclips pointing at this parent.
                        // Subclips are virtual (no files on disk), so removing
                        // them doesn't require confirmation even if the
                        // parent prompt would otherwise have fired.
                        let child_ids: Vec<klipp_core::MediaId> = self.state.media_pool
                            .all()
                            .filter(|i| i.parent_media_id == Some(*media_id))
                            .map(|i| i.id)
                            .collect();
                        for child_id in &child_ids {
                            self.state.media_pool.remove(child_id);
                        }
                        // Pool removals don't go on the timeline-edit undo
                        // stack — see the comment on the import path.
                        self.state.media_pool.remove(media_id);
                        if !child_ids.is_empty() {
                            let n = child_ids.len();
                            self.state.toast.show(format!(
                                "Removed clip and {n} subclip{}.",
                                if n == 1 { "" } else { "s" }
                            ));
                        }
                        self.state.dirty = true;
                        self.sync_media_pool_to_engine();
                    }
                    true
                }
                AppCommand::ConfirmRemoveFromPool { media_id } => {
                    // End audio skim if the removed media might be active
                    if self.state.skim_audio_session.is_some() {
                        if let Some(ref session) = self.state.skim_audio_session {
                            session.stop();
                        }
                        klipp_engine::audio_output::flush_skim_consumer(&self.skim_consumer);
                        klipp_engine::audio_output::set_skim_consumer(&self.skim_consumer, None);
                        self.state.skim_audio_session = None;
                    }
                    // Cascade: drop any subclips pointing at this parent.
                    let child_ids: Vec<klipp_core::MediaId> = self.state.media_pool
                        .all()
                        .filter(|i| i.parent_media_id == Some(*media_id))
                        .map(|i| i.id)
                        .collect();
                    for child_id in &child_ids {
                        self.state.media_pool.remove(child_id);
                    }
                    // Pool removals don't go on the timeline-edit undo stack
                    // (FCP / Premiere convention). The user has already
                    // confirmed via the modal, and we don't want a stray
                    // ctrl+z on a timeline edit to resurrect deleted media.
                    self.state.media_pool.remove(media_id);
                    if !child_ids.is_empty() {
                        let n = child_ids.len();
                        self.state.toast.show(format!(
                            "Removed clip and {n} subclip{}.",
                            if n == 1 { "" } else { "s" }
                        ));
                    }
                    self.state.show_remove_media_confirm = None;
                    self.state.dirty = true;
                    self.sync_media_pool_to_engine();
                    true
                }
                AppCommand::SyncMediaPair { video_id, audio_id } => {
                    self.spawn_media_sync(*video_id, *audio_id);
                    true
                }
                AppCommand::ClearSyncedAudio { video_id } => {
                    if let Some(item) = self.state.media_pool.get_mut(video_id) {
                        if item.synced_audio.take().is_some() {
                            self.state.dirty = true;
                        }
                    }
                    true
                }
                AppCommand::AddMediaMarker { media_id, marker } => {
                    self.state.add_media_marker(*media_id, marker.clone());
                    true
                }
                AppCommand::RemoveMediaMarker { media_id, index } => {
                    if let Some(item) = self.state.media_pool.get_mut(media_id) {
                        if *index < item.markers.len() {
                            item.markers.remove(*index);
                            self.state.dirty = true;
                        }
                    }
                    true
                }
                AppCommand::RenameMediaMarker { media_id, index, name } => {
                    if let Some(m) = self.state.media_pool.get_mut(media_id).and_then(|i| i.markers.get_mut(*index)) {
                        if m.name != *name {
                            m.name = name.clone();
                            self.state.dirty = true;
                        }
                    }
                    true
                }
                AppCommand::SetMediaLog { media_id, log } => {
                    if let Some(item) = self.state.media_pool.get_mut(media_id) {
                        if item.log != *log {
                            item.log = log.clone();
                            self.state.dirty = true;
                        }
                    }
                    true
                }
                AppCommand::SetMediaRating { media_id, rating } => {
                    if let Some(item) = self.state.media_pool.get_mut(media_id) {
                        let old_rating = item.rating;
                        let new_rating = (*rating).min(5);
                        if old_rating != new_rating {
                            item.rating = new_rating;
                            // The pool's own record: a project step, reachable from any
                            // sequence (#1986).
                            self.state.push_history(crate::history::EditCommand::SetMediaRating {
                                media_id: *media_id,
                                old_rating,
                                new_rating,
                            });
                            self.state.dirty = true;
                        }
                    }
                    true
                }
                AppCommand::SetMediaTags { media_id, tags } => {
                    if let Some(item) = self.state.media_pool.get_mut(media_id) {
                        let old_tags = item.tags.clone();
                        if old_tags != *tags {
                            item.tags = tags.clone();
                            // The pool's own record: a project step, reachable from any
                            // sequence (#1986).
                            self.state.push_history(crate::history::EditCommand::SetMediaTags {
                                media_id: *media_id,
                                old_tags,
                                new_tags: tags.clone(),
                            });
                            self.state.dirty = true;
                        }
                    }
                    true
                }
                AppCommand::SetMediaColorLabel { media_id, label } => {
                    if let Some(item) = self.state.media_pool.get_mut(media_id) {
                        let old_label = item.color_label.to_u8();
                        if old_label != *label {
                            item.color_label = klipp_media::ColorLabel::from_u8(*label);
                            // The pool's own record: a project step, reachable from any
                            // sequence (#1986).
                            self.state.push_history(crate::history::EditCommand::SetMediaColorLabel {
                                media_id: *media_id,
                                old_label,
                                new_label: *label,
                            });
                            self.state.dirty = true;
                        }
                    }
                    true
                }
                AppCommand::DeleteUnusedMedia => {
                    let unused_count = self.state.media_pool.all()
                        .filter(|item| media_usage_count(item.id, &self.state.sequences) == 0)
                        .count();
                    if unused_count == 0 {
                        self.state.toast.show("All media is used in the timeline");
                    } else {
                        let total_bytes: u64 = self.state.media_pool.all()
                            .filter(|item| media_usage_count(item.id, &self.state.sequences) == 0)
                            .map(|m| m.file_size_bytes)
                            .sum();
                        self.state.show_delete_unused_confirm = Some((unused_count, total_bytes));
                    }
                    true
                }
                AppCommand::ConfirmDeleteUnusedMedia => {
                    let unused_ids: Vec<klipp_core::MediaId> = self.state.media_pool.all()
                        .filter(|item| media_usage_count(item.id, &self.state.sequences) == 0)
                        .map(|m| m.id)
                        .collect();
                    // Pool removals don't go on the timeline-edit undo stack
                    // (see the import path). The user confirmed the cleanup
                    // via the modal; ctrl+z on a timeline edit shouldn't
                    // resurrect deleted media one item at a time.
                    for id in &unused_ids {
                        self.state.media_pool.remove(id);
                    }
                    self.state.show_delete_unused_confirm = None;
                    self.state.dirty = true;
                    self.sync_media_pool_to_engine();
                    self.state.toast.show(format!("Removed {} unused media items", unused_ids.len()));
                    true
                }
                AppCommand::ImportImageSequence { pattern, start_frame, frame_count, fps } => {
                    // Import an image sequence as a single media item.
                    // Compute duration from frame count and fps.
                    let fps_val = klipp_core::Fps::new((*fps * 1000.0) as u32, 1000);
                    let duration = klipp_core::RationalTime {
                        value: *frame_count as i64,
                        rate: fps_val.rate(),
                    };
                    // Use the pattern path as the media path (ffmpeg image2 demuxer)
                    let path = std::path::PathBuf::from(pattern);
                    let display_name = path.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| format!("Sequence ({frame_count} frames)"));
                    // Probe first frame for resolution
                    let first_frame_path = pattern.replacen(
                        &format!("%0{}d", pattern.matches('0').count().max(1)),
                        &format!("{start_frame}"),
                        1,
                    );
                    let file_size: u64 = std::fs::metadata(&first_frame_path).map(|m| m.len()).unwrap_or(0)
                        * (*frame_count as u64);
                    let media_item = klipp_media::MediaItem {
                        id: klipp_core::MediaId::new(),
                        path,
                        kind: klipp_media::MediaItemKind::File,
                        filename: display_name,
                        duration,
                        fps: fps_val,
                        resolution: klipp_core::Resolution { width: 1920, height: 1080 }, // will be overridden by probe
                        video_codec: "ImageSequence".to_string(),
                        audio_codec: String::new(),
                        has_audio: false,
                        file_size_bytes: file_size,
                        sample_rate: None,
                        audio_channels: None,
                        offline: false,
                        is_still: false,
                        proxy_path: None,
                        proxy_ready: false,
                        default_source_in: None,
                        default_source_out: None,
                        rating: 0,
                        tags: vec!["sequence".to_string()],
                        user_notes: format!("{frame_count} frames, start #{start_frame}"),
                        bin: None,
                        bin_id: None,
                        parent_media_id: None,
                        added_at: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0),
                        fingerprint: None,
                        file_modified_at: None,
                        media_created_at: None,
                        color_label: Default::default(),
                        colour_space: Default::default(),
                        needs_reprobe: false,
                        log_format: Default::default(),
                        source_rotation: 0,
                        camera_make: None,
                        camera_model: None,
                        encoder: None,
                        bit_depth: None,
                        video_bitrate_kbps: None,
                        audio_bitrate_kbps: None,
                        color_primaries: None,
                        color_transfer: None,
                        pixel_format: None,
                        detected_log_format: Default::default(),
                        log_format_user_set: false,
                        skim_file: None,
                        waveform_data: None,
                        audio_cache: None,
                        shot: Default::default(),
                        metadata_read: false,
                        truncated_from_secs: None,
                        log: Default::default(),
                        synced_audio: None,
                        markers: Vec::new(),
                        screen_text: String::new(),
                        audio_role: None,
                        span: None,
                    };
                    let span_path = media_item.path.clone();
                    self.state.media_pool.add(media_item);
                    self.refresh_spans_beside(&span_path);
                    // Imports stay off the timeline-edit undo stack (FCP /
                    // Premiere convention). Mixing them in caused two
                    // surprises: every drop-from-FILES tab took two ctrl+z
                    // to fully reverse, and a failed timeline-add (race
                    // when the AppendClipToTrack ran before the async load
                    // finished) left only the import in history — so
                    // ctrl+z removed the imported file instead of the
                    // expected timeline clip. The bin's "Remove from
                    // project" affordance is the supported way to take
                    // an item out.
                    self.state.dirty = true;
                    self.sync_media_pool_to_engine();
                    self.state.toast.show(format!("Imported image sequence: {frame_count} frames at {fps}fps"));
                    true
                }
            _ => false,
        }
    }

    fn handle_media_bins(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        match cmd {
                AppCommand::SaveSmartBin { bin } => {
                    let bins = &mut self.state.media_pool.smart_bins;
                    match bins.iter_mut().find(|b| b.id == bin.id) {
                        Some(existing) => *existing = bin.clone(),
                        None => bins.push(bin.clone()),
                    }
                    self.state.dirty = true;
                    true
                }
                AppCommand::DeleteSmartBin { id } => {
                    self.state.media_pool.smart_bins.retain(|b| b.id != *id);
                    self.state.dirty = true;
                    true
                }
                AppCommand::CreateBin { name } => {
                    // Create a root-level bin in the bins collection
                    let _bin_id = self.state.media_pool.find_or_create_bin(name);
                    tracing::info!("Bin created: {}", name);
                    self.state.dirty = true;
                    true
                }
                AppCommand::CreateSubBin { parent_id, name } => {
                    let bin = klipp_media::Bin {
                        id: klipp_core::BinId::new(),
                        name: name.clone(),
                        parent_id: Some(*parent_id),
                    };
                    self.state.media_pool.bins.push(bin);
                    tracing::info!("Sub-bin created: {} (parent: {})", name, parent_id);
                    self.state.dirty = true;
                    true
                }
                AppCommand::DeleteBin { name } => {
                    // Move all items in this bin back to root
                    let ids: Vec<MediaId> = self.state.media_pool.all()
                        .filter(|m| m.bin.as_deref() == Some(name.as_str()))
                        .map(|m| m.id)
                        .collect();
                    for id in ids {
                        if let Some(item) = self.state.media_pool.get_mut(&id) {
                            item.bin = None;
                            item.bin_id = None;
                        }
                    }
                    // Remove the bin from the collection
                    self.state.media_pool.bins.retain(|b| b.name != *name);
                    tracing::info!("Bin deleted: {}", name);
                    self.state.dirty = true;
                    true
                }
                AppCommand::MoveItemToBin { media_id, bin } => {
                    // Also sync bin_id from bin name
                    let new_bin_id = bin.as_ref().map(|name| self.state.media_pool.find_or_create_bin(name));
                    if let Some(item) = self.state.media_pool.get_mut(media_id) {
                        let from_bin = item.bin.clone();
                        let from_bin_id = item.bin_id;
                        if from_bin != *bin {
                            item.bin = bin.clone();
                            item.bin_id = new_bin_id;
                            // The pool's own record: a project step, reachable from any
                            // sequence (#1986).
                            self.state.push_history(crate::history::EditCommand::MoveMediaToBin {
                                media_id: *media_id,
                                from_bin,
                                to_bin: bin.clone(),
                                from_bin_id,
                                to_bin_id: new_bin_id,
                            });
                            self.state.dirty = true;
                        }
                    }
                    true
                }
                AppCommand::OpenBatchRenameDialog { media_ids } => {
                    self.state.batch_rename_ids = media_ids.clone();
                    self.state.show_batch_rename_dialog = true;
                    true
                }
                AppCommand::RenameMediaItem { media_id, new_filename } => {
                    if let Some(item) = self.state.media_pool.get_mut(media_id) {
                        let old = item.filename.clone();
                        if old != *new_filename {
                            item.filename = new_filename.clone();
                            // Synthetic title assets keep their
                            // display_name in sync with the pool's
                            // filename so the Inspector banner and
                            // Media-tab card label always agree.
                            if let Some(asset) = self.state.title_assets.get_mut(media_id) {
                                asset.display_name = new_filename.clone();
                            }
                            self.state.push_history(crate::history::EditCommand::BatchRenameMediaItems {
                                    changes: vec![(*media_id, old, new_filename.clone())],
                                });
                            self.state.dirty = true;
                        }
                    }
                    true
                }
                AppCommand::BatchRenameMediaItems { changes } => {
                    // Snapshot old names, apply new ones, push a single undo
                    // entry. Items whose new name matches the old are skipped
                    // entirely so undo only captures real changes.
                    let mut undo_changes: Vec<(MediaId, String, String)> = Vec::new();
                    for (media_id, new_filename) in changes {
                        if let Some(item) = self.state.media_pool.get_mut(media_id) {
                            let old = item.filename.clone();
                            if old != *new_filename {
                                item.filename = new_filename.clone();
                                undo_changes.push((*media_id, old, new_filename.clone()));
                            }
                        }
                    }
                    if !undo_changes.is_empty() {
                        let count = undo_changes.len();
                        self.state.push_history(crate::history::EditCommand::BatchRenameMediaItems {
                                changes: undo_changes,
                            });
                        self.state.toast.show(format!(
                            "Renamed {} item{}.",
                            count,
                            if count == 1 { "" } else { "s" }
                        ));
                        self.state.dirty = true;
                    }
                    true
                }
                AppCommand::RenameBin { old_name, new_name } => {
                    // Rename the Bin record itself
                    for bin in &mut self.state.media_pool.bins {
                        if bin.name == *old_name {
                            bin.name = new_name.clone();
                            break;
                        }
                    }
                    // Update legacy item.bin field (will be removed when UI migrates to bin_id)
                    let ids: Vec<MediaId> = self.state.media_pool.all()
                        .filter(|m| m.bin.as_deref() == Some(old_name.as_str()))
                        .map(|m| m.id)
                        .collect();
                    for id in ids {
                        if let Some(item) = self.state.media_pool.get_mut(&id) {
                            item.bin = Some(new_name.clone());
                            // bin_id intentionally unchanged — rename doesn't change the bin's identity
                        }
                    }
                    self.state.push_history(crate::history::EditCommand::RenameBin {
                        old_name: old_name.clone(),
                        new_name: new_name.clone(),
                    });
                    self.state.dirty = true;
                    tracing::info!("Bin renamed: {} -> {}", old_name, new_name);
                    true
                }
            _ => false,
        }
    }

    pub(super) fn handle_browser_commands(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        match cmd {
            AppCommand::RevealInFileManager { path } => {
                if let Some(parent) = path.parent() {
                    #[cfg(target_os = "linux")]
                    {
                        let _ = crate::ui_util::open_external(parent);
                    }
                }
                true
            }
            AppCommand::OpenInExternalApp { media_id } => {
                // Look up the item; bail silently if it vanished between menu
                // draw and dispatch (race with RemoveFromPool, etc).
                let (path, filename, offline, is_still, has_audio, has_video) = match self.state.media_pool.get(media_id) {
                    Some(item) => (
                        item.path.clone(),
                        item.filename.clone(),
                        item.offline,
                        item.is_still,
                        item.has_audio,
                        !item.video_codec.is_empty(),
                    ),
                    None => return true,
                };

                if offline {
                    self.state.toast.show("Clip is offline \u{2014} cannot open in external app.");
                    return true;
                }

                // Routing: stills always go to the image editor; anything
                // without a video stream (or pure audio files) → audio
                // editor; otherwise video editor. `has_audio && !has_video`
                // catches audio-only containers where `video_codec` is blank.
                let app = if is_still {
                    &self.prefs.external_tools.image_editor
                } else if !has_video && has_audio {
                    &self.prefs.external_tools.audio_editor
                } else {
                    &self.prefs.external_tools.video_editor
                };

                #[cfg(target_os = "linux")]
                {
                    let spawn_result = if app.trim().is_empty() {
                        crate::ui_util::open_external(&path)
                    } else {
                        crate::ui_util::spawn_reaped(std::process::Command::new(app).arg(&path)).map(|_| ())
                    };
                    match spawn_result {
                        Ok(_) => self.state.toast.show(format!("Opened in external app: {filename}")),
                        Err(err) => self.state.toast.show(format!("Failed to open external app: {err}")),
                    }
                }
                #[cfg(not(target_os = "linux"))]
                {
                    // macOS / Windows: deferred — no xdg-open equivalent wired up yet.
                    let _ = (app, &path, &filename);
                    self.state.toast.show("External app handoff is Linux-only for now.");
                }

                true
            }
            AppCommand::RevealMediaInBrowser { media_id } => {
                // Switch to Media tab
                self.layout.active_browser_tab = crate::layout_state::BrowserTab::Media;
                // Clear search so item is visible
                self.browser.search_query.clear();
                // Select the item
                self.state.selected_media_ids.clear();
                self.state.selected_media_ids.insert(*media_id);
                self.state.last_clicked_media_id = Some(*media_id);
                // Switch to correct bin
                if let Some(item) = self.state.media_pool.get(media_id) {
                    self.browser.media_selected_bin = item.bin.clone();
                }
                // Request scroll on next frame
                self.browser.scroll_to_media_id = Some(*media_id);
                true
            }
            AppCommand::StartSkimTranscode { media_id } => {
                self.handle_skim_transcode(*media_id);
                true
            }
            AppCommand::RegenerateSkimTranscode { media_id } => {
                let mid = *media_id;
                // Delete existing skim file and cache
                if let Some(item) = self.state.media_pool.get_mut(&mid) {
                    if let Some(ref skim_path) = item.skim_file {
                        let _ = std::fs::remove_file(skim_path);
                    }
                    item.skim_file = None;
                }
                self.state.skim_transcoding.remove(&mid);
                self.handle_skim_transcode(mid);
                true
            }
            AppCommand::RegenerateFilmstrip { media_id } => {
                let mid = *media_id;
                // Delete disk cache
                if let Some(item) = self.state.media_pool.get(&mid) {
                    // All heights and pipeline versions. (This deleted
                    // `{key}.strip`, a name that stopped matching the real
                    // files when the height joined it, so "regenerate" just
                    // reloaded the old strip.)
                    if let Some(key) = klipp_engine::video_decoder::picture_cache_key(&item.path) {
                        klipp_media::disk_cache::remove_filmstrip_cache(&key);
                    }
                }
                // Clear in-memory filmstrip
                self.state.thumbnail_cache.set_filmstrip(mid, Vec::new());
                if let Some(strip) = self.state.filmstrip_textures.remove(&mid) {
                    self.defer_drop_textures(strip.into_iter().map(|(_, t)| t));
                }
                self.state.filmstrip_in_progress.remove(&mid);
                // Re-queue
                if let Some(item) = self.state.media_pool.get(&mid) {
                    let path = item.path.clone();
                    let duration = item.duration;
                    self.state.filmstrip_generating = true;
                    self.state.filmstrip_generate_start = std::time::Instant::now();
                    self.spawn_filmstrip(mid, path, duration);
                }
                true
            }
            AppCommand::BrowserSkimFrame { media_id, time_secs, target } => {
                self.request_skim_frame(*media_id, *time_secs, *target);
                true
            }
            AppCommand::BrowserSkimAudioStart { media_id, source_path, time_secs } => {
                // Audition has exclusive access — file/media browser
                // skim is suppressed while it's playing so the cursor
                // doesn't kick the audition off the cpal slot.
                if self.state.audition_active {
                    return true;
                }
                // Stop any existing session
                if let Some(ref session) = self.state.skim_audio_session {
                    session.stop();
                }
                self.state.skim_audio_session = None;

                // Look up PCM path from the media item's audio cache entry (content-hash based)
                // Fall back to media-ID-based path for older caches
                let pcm_path = self.state.media_pool.get(media_id)
                    .and_then(|item| item.audio_cache.as_ref())
                    .map(|cache| cache.pcm_path.clone())
                    .unwrap_or_else(|| {
                        let cache_dir = dirs::cache_dir()
                            .unwrap_or_else(|| PathBuf::from("/tmp"))
                            .join("klipp")
                            .join("audio-cache");
                        cache_dir.join(format!("{}.pcm", media_id))
                    });

                tracing::info!(
                    "BrowserSkimAudioStart: media={:?} pcm={:?} pcm_exists={} muted={} vol={} cpal_rate={}",
                    media_id,
                    pcm_path,
                    pcm_path.exists(),
                    self.state.master_muted,
                    self.state.master_volume,
                    self.cpal_sample_rate,
                );

                let initial_volume = if self.state.master_muted {
                    0.0
                } else {
                    self.state.master_volume
                };
                if let Some(session) = klipp_engine::skim_audio::SkimAudioSession::new(
                    &pcm_path,
                    source_path.clone(),
                    self.cpal_sample_rate,
                    self.skim_consumer.clone(),
                    initial_volume,
                ) {
                    session.seek_and_play(*time_secs);
                    self.state.skim_audio_session = Some(session);
                    tracing::info!(
                        "BrowserSkimAudioStart: session created and playing from {:.2}s",
                        time_secs
                    );
                } else {
                    tracing::warn!(
                        "BrowserSkimAudioStart: session creation FAILED (PCM cache missing or load error)"
                    );
                }
                true
            }
            AppCommand::BrowserSkimAudioStartFromPath { source_path, time_secs } => {
                if self.state.audition_active {
                    return true;
                }
                // Stop any existing session first.
                if let Some(ref session) = self.state.skim_audio_session {
                    session.stop();
                }
                self.state.skim_audio_session = None;

                // No PCM cache, no MediaId — decode the source file directly.
                // First-hover cost is the decode time (cached after that, so
                // wiggling the cursor across the same row is free).
                if let Some(session) = klipp_engine::skim_audio::SkimAudioSession::new_from_source_path(
                    source_path.clone(),
                    self.cpal_sample_rate,
                    self.skim_consumer.clone(),
                    if self.state.master_muted { 0.0 } else { self.state.master_volume },
                    *time_secs,
                ) {
                    self.state.skim_audio_session = Some(session);
                }
                true
            }
            AppCommand::BrowserSkimAudioUpdate { time_secs, .. } => {
                // Critical: this is the path that was killing the
                // audition. When the cursor stops moving (speed = 0),
                // `determine_skim_mode` returns Silent and we'd call
                // `session.silence()` on whatever session is in the
                // slot — including the audition's. Bail out when
                // audition owns the session.
                if self.state.audition_active {
                    return true;
                }
                if let Some(ref session) = self.state.skim_audio_session {
                    // Update volume dynamically
                    let vol = if self.state.master_muted { 0.0 } else { self.state.master_volume };
                    session.set_volume(vol);

                    let mode = klipp_engine::skim_audio::determine_skim_mode(self.state.skim_speed_px_per_sec);
                    if mode == klipp_engine::skim_audio::SkimMode::Playing {
                        session.seek_and_play(*time_secs);
                    } else {
                        session.silence();
                    }
                }
                true
            }
            AppCommand::BrowserSkimAudioEnd => {
                // Cursor-skim End is suppressed while audition owns
                // the session. The cursor handlers fire End when the
                // mouse leaves a clip; without this guard, mousing
                // away from a timeline/file row would tear down the
                // audition's playback.
                if self.state.audition_active {
                    return true;
                }
                if let Some(ref session) = self.state.skim_audio_session {
                    session.stop();
                }
                // Disconnect skim consumer from cpal callback
                klipp_engine::audio_output::flush_skim_consumer(&self.skim_consumer);
                klipp_engine::audio_output::set_skim_consumer(&self.skim_consumer, None);
                self.state.skim_audio_session = None;
                self.state.skim_speed_px_per_sec = 0.0;
                true
            }
            AppCommand::AuditionSeek { time_secs } => {
                if let Some(ref session) = self.state.skim_audio_session {
                    session.seek_and_play(*time_secs);
                }
                true
            }
            AppCommand::AuditionStart { media_id, source_path, time_secs } => {
                // Stop any cursor-skim session first.
                if let Some(ref session) = self.state.skim_audio_session {
                    session.stop();
                }
                self.state.skim_audio_session = None;

                // Look up the conformed-PCM path the same way
                // BrowserSkimAudioStart does — by media_id with the
                // legacy fallback location.
                let pcm_path = self
                    .state
                    .media_pool
                    .get(media_id)
                    .and_then(|item| item.audio_cache.as_ref())
                    .map(|cache| cache.pcm_path.clone())
                    .unwrap_or_else(|| {
                        let cache_dir = dirs::cache_dir()
                            .unwrap_or_else(|| PathBuf::from("/tmp"))
                            .join("klipp")
                            .join("audio-cache");
                        cache_dir.join(format!("{}.pcm", media_id))
                    });
                tracing::info!(
                    "AuditionStart: media={:?} pcm_exists={} muted={} vol={}",
                    media_id,
                    pcm_path.exists(),
                    self.state.master_muted,
                    self.state.master_volume,
                );
                let initial_volume = if self.state.master_muted {
                    0.0
                } else {
                    self.state.master_volume
                };
                if let Some(session) = klipp_engine::skim_audio::SkimAudioSession::new(
                    &pcm_path,
                    source_path.clone(),
                    self.cpal_sample_rate,
                    self.skim_consumer.clone(),
                    initial_volume,
                ) {
                    session.seek_and_play(*time_secs);
                    self.state.skim_audio_session = Some(session);
                    self.state.audition_active = true;
                    tracing::info!(
                        "AuditionStart: session created and playing from {:.2}s (audition lock acquired)",
                        time_secs
                    );
                } else {
                    tracing::warn!(
                        "AuditionStart: session creation FAILED (PCM cache missing)"
                    );
                }
                true
            }
            AppCommand::AuditionSetVolume(vol) => {
                if let Some(ref session) = self.state.skim_audio_session {
                    session.set_volume((*vol).clamp(0.0, 1.5));
                }
                true
            }
            AppCommand::AuditionEnd => {
                if let Some(ref session) = self.state.skim_audio_session {
                    session.stop();
                }
                klipp_engine::audio_output::flush_skim_consumer(&self.skim_consumer);
                klipp_engine::audio_output::set_skim_consumer(&self.skim_consumer, None);
                self.state.skim_audio_session = None;
                self.state.audition_active = false;
                tracing::info!("AuditionEnd: audition lock released");
                true
            }
            AppCommand::RefineFilmstrip { media_id, start_secs, end_secs, interval_secs } => {
                // A targeted filmstrip pass for the requested range. One
                // at a time per file and a few in all: the timeline asks
                // again every half second while its tiles are sparse, and
                // each pass is a thread with a hardware decoder of its
                // own. Unbounded, a half-hour source piled up a thousand
                // of them in ten minutes, each with a CUDA context, and
                // the driver fell over on one (a SIGSEGV on a filmstrip
                // thread, 1106 threads, 2026-09-17). A request that finds
                // its file busy is dropped; the timeline asks again.
                const REFINES_AT_ONCE: usize = 2;
                let busy = {
                    let running = self.filmstrip_refining.lock().unwrap_or_else(|e| e.into_inner());
                    running.contains(media_id) || running.len() >= REFINES_AT_ONCE
                };
                if busy {
                    return true;
                }
                if let Some(item) = self.state.media_pool.get(media_id) {
                    let path = item.path.clone();
                    let tx = self.filmstrip_tx.clone();
                    let mid = *media_id;
                    let s = *start_secs;
                    let e = *end_secs;
                    let iv = *interval_secs;
                    let running = std::sync::Arc::clone(&self.filmstrip_refining);
                    running.lock().unwrap_or_else(|e| e.into_inner()).insert(mid);
                    /// Takes the file off the running set however the pass ends.
                    struct Running(std::sync::Arc<std::sync::Mutex<std::collections::HashSet<MediaId>>>, MediaId);
                    impl Drop for Running {
                        fn drop(&mut self) {
                            self.0.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.1);
                        }
                    }
                    let guard = Running(running, mid);
                    let _ = std::thread::Builder::new()
                        .name("klipp-filmstrip-refine".into())
                        .spawn(move || {
                            let _guard = guard;
                            let outcome = klipp_engine::generate_filmstrip_range(
                                &path,
                                s,
                                e,
                                iv,
                                48,
                                |time, rgba, w, h| {
                                    if time >= s - 0.01 && time <= e + 0.01 {
                                        let _ = tx.send(FilmstripResult::Frame {
                                            media_id: mid,
                                            frame: klipp_media::FilmstripFrame {
                                                time_secs: time,
                                                rgba,
                                                width: w,
                                                height: h,
                                            },
                                        });
                                    }
                                },
                                None, // refine path — not yet wrapped in a ThreadJob
                            );
                            if let Err(err) = outcome {
                                tracing::warn!("Filmstrip refine failed for {:?}: {err}", path);
                            }
                        })
                        .ok();
                }
                true
            }
            _ => false,
        }
    }
}

impl KlippApp {
    fn relink_media(&mut self, media_id: MediaId, new_path: &Path) {
        let Some(item) = self.state.media_pool.get_mut(&media_id) else { return };
        let changed = item.path != new_path;
        item.path = new_path.to_path_buf();
        item.offline = !new_path.exists();
        if changed {
            if let Some(proxy) = item.proxy_path.take() {
                let _ = std::fs::remove_file(&proxy);
                let _ = std::fs::remove_file(partial_proxy_path(&proxy));
            }
            item.proxy_ready = false;
            item.skim_file = None;
            item.waveform_data = None;
            item.audio_cache = None;
            item.fingerprint = compute_media_fingerprint(new_path);
            self.state.thumbnail_cache.set_filmstrip(media_id, Vec::new());
        }
        self.state.offline_media_ids.retain(|id| *id != media_id);
        if new_path.exists() {
            let _ = self.cmd_tx.send(AppCommand::LoadMediaFile {
                path: new_path.to_path_buf(),
                media_id,
            });
            // Its pictures, from the file it now names. The re-probe
            // alone makes none: a moved file has the same size and date,
            // so it reads as unchanged, and an item that was offline at
            // open never had a thumbnail made in the first place.
            self.refresh_media_pictures(media_id);
        }
        self.sync_media_pool_to_engine();
    }

    /// Seed the clip's InputTransform node `log_format` from the
    /// source media. User per-media override (`MediaItem.log_format`,
    /// set whenever the user picks a format on any clip of this
    /// media) wins over the heuristic (`detected_log_format`); the
    /// heuristic wins over Srgb default. Skipped entirely when the
    /// effective format is Srgb so we don't insert pointless
    /// `Some(Srgb)` overrides on plain SDR clips. The project's input
    /// defaults (#466) fill in where the file says nothing, and its input
    /// LUT heads the new clip's grade.
    pub(super) fn apply_input_defaults(&mut self, clip_id: klipp_core::ClipId, media_id: klipp_core::MediaId) {
        let Some(media) = self.state.media_pool.get(&media_id) else { return };
        // The project's input defaults (#466): its format where the file
        // says none, and its input LUT at the head of the grade.
        let effective = crate::panels::colour_management::new_clip_input_format(media, self.state.project_default_input_format);
        let lut = self.state.project_default_input_lut.clone().filter(|_| !media.video_codec.is_empty() || media.is_still);
        let seq = &mut self.state.sequences[self.state.active_sequence_idx];
        if let Some(clip) = seq.all_clips_mut().find(|c| c.id == clip_id) {
            if effective != klipp_core::LogFormat::Srgb {
                clip.set_input_log_format(effective);
            }
            if let Some(lut) = lut {
                crate::panels::colour_management::add_input_lut(clip, &lut);
            }
        }
    }

    /// A favourite or rejected stretch of the source clip: between its
    /// in and out points, or the whole clip when none are set.
    pub(super) fn source_range_marker(&self, kind: klipp_core::MediaMarkerKind) -> Option<klipp_core::MediaMarker> {
        let media = self.state.media_pool.get(&self.state.source_media_id?)?;
        let from = media.default_source_in.map(|t| t.to_secs_f64()).unwrap_or(0.0);
        let to = media.default_source_out.map(|t| t.to_secs_f64()).unwrap_or_else(|| media.duration.to_secs_f64());
        (to > from).then(|| klipp_core::MediaMarker::range(kind, from, to))
    }

    pub(super) fn apply_media_sync(&mut self, video_id: klipp_core::MediaId, sync: klipp_core::SyncedAudio) {
        let audio_name = self.state.media_pool.get(&sync.audio_media_id).map(|m| m.filename.clone()).unwrap_or_default();
        let Some(video) = self.state.media_pool.get_mut(&video_id) else { return };
        let message = format!("Synced {} to {audio_name}: {}", video.filename, sync.describe());
        video.synced_audio = Some(sync);
        self.state.dirty = true;
        tracing::info!("{message}");
        self.state.toast.show(message);
    }

    /// Kick off a project consolidation on a background thread. Sets up the
    /// `consolidate_progress` channel on state so the main loop picks up
    /// progress + completion messages.
    pub(super) fn start_consolidation(
        &mut self,
        dest_dir: std::path::PathBuf,
        include_proxies: bool,
        include_skims: bool,
        create_zip: bool,
        delete_folder_after_zip: bool,
        mode: klipp_project::consolidate::ConsolidateMode,
    ) {
        let Some(source_dir) = self.state.project_path.clone() else {
            self.state.toast.show(
                "Save the project before consolidating.".to_string(),
            );
            return;
        };
        if self.state.consolidate_progress.is_some() {
            self.state.toast.show(
                "Consolidation already in progress.".to_string(),
            );
            return;
        }

        let media_pool = self.state.media_pool.clone();
        let sequences = self.state.sequences.clone();
        let color = klipp_project::ColorSettings {
            global_lut_path: self.state.global_lut_path.clone(),
            global_lut_opacity: self.state.global_lut_opacity,
            global_lut_enabled: self.state.global_lut_enabled,
            track_header_width: self.state.track_header_width,
            auto_generate_filmstrips: self.state.project_auto_filmstrips,
            auto_generate_skim: self.state.project_auto_skim,
            working_colorspace: self.state.project_working_colorspace,
            grade_encoding: self.state.project_grade_encoding,
            default_odt: self.state.project_default_odt,
            default_input_format: self.state.project_default_input_format,
            default_input_lut: self.state.project_default_input_lut.clone(),
            viewing_lut: self.state.project_viewing_lut.clone(),
            scopes_after_viewing_lut: self.state.project_scopes_after_viewing_lut,
        };
        let options = klipp_project::consolidate::ConsolidateOptions {
            dest_dir,
            include_proxies,
            include_skims,
            mode,
            looks: self.state.looks.clone(),
        };

        let (tx, rx) = std::sync::mpsc::channel();
        self.state.consolidate_progress = Some(crate::state::ConsolidateProgress {
            files_done: 0,
            files_total: 0,
            current_file: String::new(),
            expects_archive: create_zip,
            archiving: false,
            archive_bytes_done: 0,
            archive_bytes_total: 0,
            receiver: rx,
        });
        self.state.show_consolidate_dialog = false;

        // Capture the destination once for Phase 2 so later state edits can't
        // redirect the archive target mid-run.
        let phase2_dest = options.dest_dir.clone();

        let tx_progress = tx.clone();
        std::thread::spawn(move || {
            let result = klipp_project::consolidate::consolidate_project(
                &source_dir,
                &media_pool,
                &sequences,
                &color,
                options,
                Some(Box::new(move |done, total, name| {
                    let _ = tx_progress.send(
                        crate::state::ConsolidateMessage::Progress {
                            done,
                            total,
                            filename: name.to_string(),
                        },
                    );
                })),
            );
            let phase1_ok = result.is_ok();
            let _ = tx.send(crate::state::ConsolidateMessage::Done(
                result.map_err(|e| e.to_string()),
            ));

            if create_zip && phase1_ok {
                // Place the archive next to the consolidated folder.
                // `<dest>/MyProject/` → `<dest>/MyProject.zip`.
                let zip_path = phase2_dest.with_extension("zip");
                let tx_arch = tx.clone();
                let progress_fn: klipp_project::consolidate::ArchiveProgressFn =
                    Box::new(move |bytes_done: u64, bytes_total: u64, name: &str| {
                        let _ = tx_arch.send(
                            crate::state::ConsolidateMessage::ArchiveProgress {
                                bytes_done,
                                bytes_total,
                                filename: name.to_string(),
                            },
                        );
                    });
                let archive_result = klipp_project::consolidate::execute_zip_folder(
                    &phase2_dest,
                    &zip_path,
                    Some(&progress_fn),
                );
                match archive_result {
                    Ok(()) => {
                        let _ = tx.send(
                            crate::state::ConsolidateMessage::ArchiveDone(Ok(zip_path.clone())),
                        );
                        if delete_folder_after_zip {
                            // Best-effort cleanup — the archive is already on
                            // disk, so a failure here isn't fatal.
                            if let Err(e) = std::fs::remove_dir_all(&phase2_dest) {
                                tracing::warn!(
                                    "delete folder after archiving failed: {e}",
                                );
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(
                            crate::state::ConsolidateMessage::ArchiveDone(Err(e.to_string())),
                        );
                    }
                }
            }
        });
    }
}

pub(super) fn media_usage_count(media_id: MediaId, sequences: &[Sequence]) -> usize {
    sequences.iter()
        .flat_map(|seq| seq.tracks.iter())
        .flat_map(|t| &t.clips)
        .filter(|c| c.media_id() == Some(media_id))
        .count()
}

/// Compute a content fingerprint: FNV-1a hash of first 64KB + file_size.
/// Returns None if the file can't be read.
pub(super) fn compute_media_fingerprint(path: &Path) -> Option<u64> {
    klipp_media::relink::fingerprint(path)
}

/// Detect if a set of paths forms a numbered image sequence.
/// Returns (ffmpeg_pattern, start_frame, frame_count) if detected.
pub fn detect_image_sequence(paths: &[PathBuf]) -> Option<(String, u32, u32)> {
    if paths.len() < 2 { return None; }

    // All must be in the same directory with the same extension
    let dir = paths[0].parent()?;
    let ext = paths[0].extension()?.to_str()?;
    if !paths.iter().all(|p| {
        p.parent() == Some(dir)
            && p.extension().and_then(|e| e.to_str()) == Some(ext)
    }) {
        return None;
    }

    // Only image extensions (delegated to the canonical klipp_core list,
    // so any future image format automatically participates in image
    // sequence detection).
    if !klipp_core::media_extensions::is_image(ext) {
        return None;
    }

    // Extract numeric suffix from each filename: name_NNNN.ext
    let mut frames: Vec<(u32, &PathBuf)> = paths.iter()
        .filter_map(|p| {
            let stem = p.file_stem()?.to_str()?;
            // Find trailing digits
            let digit_start = stem.rfind(|c: char| !c.is_ascii_digit())?;
            let num_str = &stem[digit_start + 1..];
            if num_str.is_empty() { return None; }
            let n: u32 = num_str.parse().ok()?;
            Some((n, p))
        })
        .collect();

    if frames.len() < 2 { return None; }
    frames.sort_by_key(|(n, _)| *n);

    let start = frames[0].0;
    let end = frames.last()?.0;
    let count = frames.len() as u32;

    // Must be mostly contiguous (allow up to 5% gaps)
    let expected = end - start + 1;
    if (count as f64) < (expected as f64 * 0.95) { return None; }

    // Build ffmpeg pattern: determine padding width
    let sample_stem = frames[0].1.file_stem()?.to_str()?;
    let digit_start = sample_stem.rfind(|c: char| !c.is_ascii_digit())?;
    let prefix = &sample_stem[..=digit_start];
    let padding = sample_stem.len() - digit_start - 1;
    let pattern = format!("{}/{prefix}%0{padding}d.{ext}", dir.display());

    Some((pattern, start, count))
}

pub(super) fn is_video_ext(path: &std::path::Path) -> bool {
    klipp_core::media_extensions::path_is_video(path)
}

/// What a pool clip puts on the timeline when no range was picked on its
/// browser thumbnail: the In/Out marked in the source viewer, else the
/// whole clip, as `(duration, source_in, source_out)`. The same rule as
/// Insert and Overwrite; hover-skimming the tile never marks anything.
pub(super) fn source_marks_range(item: &klipp_media::MediaItem) -> (Timecode, Option<Timecode>, Option<Timecode>) {
    if item.default_source_in.is_none() && item.default_source_out.is_none() {
        return (item.duration, None, None);
    }
    let raw_in = item.default_source_in.unwrap_or(Timecode { value: 0, rate: item.duration.rate });
    let raw_out = item.default_source_out.unwrap_or(item.duration);
    if raw_in >= raw_out {
        return (item.duration, None, None);
    }
    (raw_out - raw_in, Some(raw_in), Some(raw_out))
}

/// A file that turns out to be in the pool already is not imported
/// again, but a destination it was given (a drop on a track, "add at
/// playhead") still holds: the item already there is placed instead.
pub(super) fn retarget_pending_drop(
    pending: &mut std::collections::HashMap<klipp_core::MediaId, klipp_core::events::ClipDrop>,
    requested: klipp_core::MediaId,
    existing: klipp_core::MediaId,
) -> Option<AppCommand> {
    let drop = pending.remove(&requested)?;
    Some(AppCommand::PlaceMedia { media_id: existing, drop })
}

#[cfg(test)]
mod external_tools_tests {
    use crate::prefs::ExternalToolsPrefs;

    #[test]
    fn external_tools_prefs_default_round_trip_enabled() {
        let prefs = ExternalToolsPrefs::default();
        assert!(prefs.round_trip_reload);
        assert!(prefs.image_editor.is_empty());
        assert!(prefs.audio_editor.is_empty());
        assert!(prefs.video_editor.is_empty());
    }

    /// Mirrors the routing in the `OpenInExternalApp` handler: stills always
    /// go to the image editor.
    #[test]
    fn external_app_routing_still_uses_image_editor() {
        let prefs = ExternalToolsPrefs {
            image_editor: "gimp".into(),
            audio_editor: "audacity".into(),
            video_editor: "kdenlive".into(),
            round_trip_reload: true,
        };
        let is_still = true;
        let has_video = false;
        let has_audio = false;
        let app = if is_still {
            &prefs.image_editor
        } else if !has_video && has_audio {
            &prefs.audio_editor
        } else {
            &prefs.video_editor
        };
        assert_eq!(app, "gimp");
    }

    /// Mirrors the routing in the `OpenInExternalApp` handler: audio-only
    /// files (has_audio, no video_codec) route to the audio editor.
    #[test]
    fn external_app_routing_audio_only() {
        let prefs = ExternalToolsPrefs {
            image_editor: "gimp".into(),
            audio_editor: "audacity".into(),
            video_editor: "kdenlive".into(),
            round_trip_reload: true,
        };
        let is_still = false;
        let has_video = false;
        let has_audio = true;
        let app = if is_still {
            &prefs.image_editor
        } else if !has_video && has_audio {
            &prefs.audio_editor
        } else {
            &prefs.video_editor
        };
        assert_eq!(app, "audacity");
    }

    /// Video clips route to the video editor.
    #[test]
    fn external_app_routing_video_uses_video_editor() {
        let prefs = ExternalToolsPrefs {
            image_editor: "gimp".into(),
            audio_editor: "audacity".into(),
            video_editor: "kdenlive".into(),
            round_trip_reload: true,
        };
        let is_still = false;
        let has_video = true;
        let has_audio = true;
        let app = if is_still {
            &prefs.image_editor
        } else if !has_video && has_audio {
            &prefs.audio_editor
        } else {
            &prefs.video_editor
        };
        assert_eq!(app, "kdenlive");
    }
}

#[cfg(test)]
mod source_marks_range_tests {
    use super::source_marks_range;
    use klipp_core::Timecode;

    fn t(secs: i64) -> Timecode { Timecode { value: secs * 30, rate: 30 } }

    fn item(secs: i64) -> klipp_media::MediaItem {
        // Any item will do: only the duration and the marks are read.
        klipp_media::MediaItem::new_synthetic_title(
            klipp_core::MediaId::new(), "clip".into(), t(secs), klipp_core::Fps::new(30, 1),
        )
    }

    /// Issue #261: an unmarked clip goes on whole.
    #[test]
    fn an_unmarked_clip_is_added_whole() {
        assert_eq!(source_marks_range(&item(140)), (t(140), None, None));
    }

    #[test]
    fn marked_in_and_out_are_used() {
        let mut m = item(140);
        m.default_source_in = Some(t(10));
        m.default_source_out = Some(t(40));
        assert_eq!(source_marks_range(&m), (t(30), Some(t(10)), Some(t(40))));

        // Only an In: from there to the end.
        m.default_source_out = None;
        assert_eq!(source_marks_range(&m), (t(130), Some(t(10)), Some(t(140))));

        // Marks the wrong way round fall back to the whole clip.
        m.default_source_out = Some(t(5));
        assert_eq!(source_marks_range(&m), (t(140), None, None));
    }
}

#[cfg(test)]
mod retarget_drop_tests {
    use super::{retarget_pending_drop, AppCommand};
    use klipp_core::events::ClipDrop;
    use klipp_core::MediaId;

    #[test]
    fn a_duplicate_with_a_destination_places_the_existing_item() {
        let (requested, existing) = (MediaId::new(), MediaId::new());
        let mut pending = std::collections::HashMap::new();
        pending.insert(requested, ClipDrop::AppendToTrack { track_index: 0 });
        let cmd = retarget_pending_drop(&mut pending, requested, existing);
        assert!(matches!(
            cmd,
            Some(AppCommand::PlaceMedia { media_id, drop: ClipDrop::AppendToTrack { track_index: 0 } }) if media_id == existing
        ));
        assert!(pending.is_empty(), "the stale destination was left behind");
    }

    #[test]
    fn a_duplicate_with_no_destination_places_nothing() {
        let mut pending = std::collections::HashMap::new();
        assert!(retarget_pending_drop(&mut pending, MediaId::new(), MediaId::new()).is_none());
    }
}

impl crate::state::AppState {
    /// The editor's marker on media item `id`, in time order: an edit.
    pub(crate) fn add_media_marker(&mut self, id: klipp_core::MediaId, marker: klipp_core::MediaMarker) {
        if let Some(item) = self.media_pool.get_mut(&id) {
            item.markers.push(marker);
            item.markers.sort_by(|a, b| a.secs.total_cmp(&b.secs));
            self.dirty = true;
        }
    }
}
