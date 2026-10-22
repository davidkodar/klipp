//! The timeline's editing: tracks made, navigation and markers, cuts, trims
//! and blades, match frame, subclips, freeze frames, and paste (#826).

use super::*;

impl KlippApp {
    pub(super) fn handle_timeline_commands(&mut self, cmd: &AppCommand, needs_refresh: &mut bool) -> bool {
        if self.handle_timeline_track_creation(cmd, needs_refresh) { return true; }
        if self.handle_timeline_navigation_markers(cmd, needs_refresh) { return true; }
        if self.handle_timeline_editing(cmd, needs_refresh) { return true; }
        false
    }

    fn handle_timeline_track_creation(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        match cmd {
                // Phase 4: Track properties
                AppCommand::SetTrackOpacity { track_index, opacity } => {
                    if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*track_index) {
                        track.opacity = opacity.clamp(0.0, 1.0);
                        tracing::debug!("SetTrackOpacity: track[{}]={} name={}", track_index, track.opacity, track.name);
                    }
                    self.sync_sequence_to_engine();
                    true
                }
                AppCommand::SetTrackVisibility { track_index, visible } => {
                    if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*track_index) {
                        track.visible = *visible;
                    }
                    true
                }
                AppCommand::SetTrackVolume { track_index, volume } => {
                    if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*track_index) {
                        track.volume = volume.clamp(0.0, 2.0);
                    }
                    let _ = self.cmd_tx.send(cmd.clone());
                    true
                    // Fall through to forward to engine (updates AudioMixer)
                }
                AppCommand::SetTrackMute { track_index, muted } => {
                    if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*track_index) {
                        track.muted = *muted;
                    }
                    let _ = self.cmd_tx.send(cmd.clone());
                    true
                    // Fall through to forward to engine
                }
                AppCommand::SetTrackSolo { track_index, solo } => {
                    if *solo {
                        // Enable solo on this track, disable on all others
                        let solo_idx = *track_index;
                        for (i, track) in self.state.sequences[self.state.active_sequence_idx].tracks.iter_mut().enumerate() {
                            track.solo = i == solo_idx;
                        }
                    } else {
                        // Unsolo: clear solo on all tracks
                        for track in self.state.sequences[self.state.active_sequence_idx].tracks.iter_mut() {
                            track.solo = false;
                        }
                    }
                    let _ = self.cmd_tx.send(cmd.clone());
                    true
                    // Fall through to forward to engine
                }
                AppCommand::SetTrackPan { track_index, pan } => {
                    if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*track_index) {
                        track.pan = pan.clamp(-1.0, 1.0);
                    }
                    let _ = self.cmd_tx.send(cmd.clone());
                    true
                    // Fall through to forward to engine
                }
                // Subtitles/FCPXML/EDL — handled by handle_export_commands
                // Create adjustment layer at playhead
                AppCommand::CreateAdjustmentLayer => {
                    let fps_rate = self.state.sequences[self.state.active_sequence_idx].fps.rate();
                    let ph = self.state.playhead.rescale_to(fps_rate);
                    // 5s default, expressed in seconds rather than
                    // raw ticks — `from_frames(120, fps_rate)` gives
                    // 5 ms on fractional-fps projects (e.g. 23.976
                    // NTSC where rate = 24000).
                    let duration = klipp_core::RationalTime::from_secs_f64(5.0, fps_rate);
                    let timeline_out = ph + duration;
                    let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                    // Find or create the top video track
                    let track_idx = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video)
                        .unwrap_or_else(|| {
                            seq.tracks.insert(0, klipp_timeline::Track::new("Adjustment", klipp_timeline::TrackKind::Video));
                            0
                        });
                    let clip = klipp_timeline::Clip {
                        id: klipp_core::ClipId::new(),
                        kind: klipp_timeline::ClipKind::AdjustmentLayer,
                        timeline_in: ph,
                        timeline_out,
                        source_in: klipp_core::RationalTime::from_secs_f64(0.0, fps_rate),
                        source_out: duration,
                        ..klipp_timeline::Clip::gap(ph, timeline_out)
                    };
                    let clip_id = clip.id;
                    seq.tracks[track_idx].clips.push(clip);
                    seq.tracks[track_idx].sort_clips();
                    self.state.selected_clip = Some(clip_id);
                    self.state.toast.show("Adjustment layer created".to_string());
                    self.state.dirty = true;
                    self.sync_sequence_to_engine();
                    true
                }
                AppCommand::InsertFreezeFrame { duration_secs } => {
                    self.insert_freeze_frame(*duration_secs);
                    true
                }
                AppCommand::CreateGeneratorClip { track_index, data } => {
                    let fps_rate = self.state.sequences[self.state.active_sequence_idx].fps.rate();
                    let ph = self.state.playhead.rescale_to(fps_rate);
                    // 5s default, expressed in seconds rather than
                    // raw frames. `Fps::rate()` returns the numerator
                    // (e.g. 24000 for 23.976 NTSC), so a literal
                    // `from_frames(120, fps_rate)` produces a 5 ms
                    // clip on fractional-fps projects — visible only
                    // as a green flash before the playhead skips past.
                    let duration = klipp_core::RationalTime::from_secs_f64(5.0, fps_rate);
                    let timeline_out = ph + duration;
                    let label = match &data {
                        klipp_core::GeneratorData::SolidColor { .. } => "Solid Colour",
                        klipp_core::GeneratorData::LivingGradient { .. } => "Living Gradient",
                        klipp_core::GeneratorData::Grain { .. } => "Grain",
                        klipp_core::GeneratorData::Placeholder { .. } => "Placeholder",
                        klipp_core::GeneratorData::Gradient { .. } => "Gradient",
                        klipp_core::GeneratorData::ColorBars => "Colour Bars",
                        klipp_core::GeneratorData::ColorBars100 => "Colour Bars 100%",
                        klipp_core::GeneratorData::Countdown => "Countdown",
                        klipp_core::GeneratorData::Timer { .. } => "Timer",
                        klipp_core::GeneratorData::Slate { .. } => "Slate",
                        klipp_core::GeneratorData::StillFrame { .. } => "Freeze Frame",
                        klipp_core::GeneratorData::Polygon { .. } => "Polygon",
                        klipp_core::GeneratorData::Star { .. } => "Star",
                    };
                    let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                    let track_idx = track_index.unwrap_or_else(|| {
                        seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video)
                            .unwrap_or_else(|| {
                                seq.tracks.insert(0, klipp_timeline::Track::new("Generator", klipp_timeline::TrackKind::Video));
                                0
                            })
                    });
                    let clip = klipp_timeline::Clip {
                        id: klipp_core::ClipId::new(),
                        kind: klipp_timeline::ClipKind::Generator(data.clone()),
                        timeline_in: ph,
                        timeline_out,
                        source_in: klipp_core::RationalTime::from_secs_f64(0.0, fps_rate),
                        source_out: duration,
                        ..klipp_timeline::Clip::gap(ph, timeline_out)
                    };
                    let clip_id = clip.id;
                    seq.tracks[track_idx].clips.push(clip);
                    seq.tracks[track_idx].sort_clips();
                    // The new generator is the selection, the inspector
                    // opens on its parameters, and the playhead parks a
                    // little way in so the viewer shows it.
                    self.state.selected_clip = Some(clip_id);
                    self.state.selected_clips = vec![clip_id];
                    self.state.toast.show(format!("{label} generator created"));
                    self.state.dirty = true;
                    self.sync_sequence_to_engine();
                    let park = ph + klipp_core::RationalTime::from_secs_f64(0.25, fps_rate);
                    let _ = self.cmd_tx.send(AppCommand::SeekTo(park));
                    true
                }
                // Create compound clip: move selected clips into a new nested sequence
                AppCommand::CreateCompoundClip { name } => {
                    self.create_compound_clip_impl(name.clone());
                    true
                }
                // Open compound clip: drill into nested sequence
                AppCommand::OpenCompoundClip { clip_id } => {
                    let cid = *clip_id;
                    let seq = &self.state.sequences[self.state.active_sequence_idx];
                    let nested_id = seq.all_clips()
                        .find(|c| c.id == cid)
                        .and_then(|c| match &c.kind {
                            klipp_timeline::ClipKind::Sequence { sequence_id, .. } => Some(*sequence_id),
                            _ => None,
                        });
                    if let Some(sid) = nested_id {
                        if let Some(target_idx) = self.state.sequences.iter().position(|s| s.id == sid) {
                            // Defensive: a compound clip already in the
                            // project should never form a cycle (the
                            // CreateCompoundClip guard rejects creation that
                            // would do so). Trip a debug_assert if data
                            // integrity has been violated.
                            let parent_seq_id =
                                self.state.sequences[self.state.active_sequence_idx].id;
                            debug_assert!(
                                !would_create_cycle(sid, parent_seq_id, &self.state.sequences),
                                "Existing compound clip is already a cycle — data integrity issue"
                            );
                            // Push current position to breadcrumb stack
                            self.state.compound_clip_stack.push((
                                self.state.active_sequence_idx,
                                self.state.playhead,
                            ));
                            // Switch to nested sequence
                            self.state.active_sequence_idx = target_idx;
                            self.state.fps = self.state.sequences[target_idx].fps;
                            // From its start: the parent's scroll left it
                            // looking empty, scrolled past its end (#910).
                            self.state.timeline_scroll_x = 0.0;
                            self.state.playhead = klipp_core::RationalTime::from_secs_f64(
                                0.0, self.state.sequences[target_idx].fps.rate());
                            self.state.selected_clip = None;
                            self.state.selected_clips.clear();
                            self.sync_sequence_to_engine();
                        }
                    }
                    true
                }
                // Close compound clip: navigate back to parent sequence
                AppCommand::CloseCompoundClip => {
                    if let Some((parent_idx, parent_playhead)) = self.state.compound_clip_stack.pop() {
                        if parent_idx < self.state.sequences.len() {
                            self.state.active_sequence_idx = parent_idx;
                            self.state.fps = self.state.sequences[parent_idx].fps;
                            self.state.playhead = parent_playhead;
                            // The parent's playhead in view again.
                            self.state.timeline_scroll_x = (parent_playhead.to_secs_f64() as f32 * self.state.timeline_zoom - 80.0).max(0.0);
                            self.state.selected_clip = None;
                            self.state.selected_clips.clear();
                            self.sync_sequence_to_engine();
                        }
                    }
                    true
                }
                // Decompose a compound clip — restore its inner clips to the
                // parent timeline at their original positions, remove the
                // compound clip itself, and delete the now-orphaned inner
                // sequence. Inverse of CreateCompoundClip.
                AppCommand::DecomposeCompoundClip { clip_id } => {
                    self.decompose_compound_clip_impl(*clip_id);
                    true
                }
                // Create multicam clip: group clips at playhead across tracks as angles
                AppCommand::CreateMulticamClip => {
                    let ph = self.state.playhead;
                    let seq = &self.state.sequences[self.state.active_sequence_idx];
                    // Collect video clips at the playhead — one per
                    // video track. `continue` on non-video tracks
                    // skips them; using `return` here previously
                    // silently aborted the whole operation as soon
                    // as the loop hit the first audio track, so
                    // multicam never fired on any sequence that had
                    // an audio track interleaved above the bottom-
                    // most video track.
                    let mut angle_clips: Vec<(usize, klipp_core::ClipId)> = Vec::new();
                    for (ti, track) in seq.tracks.iter().enumerate() {
                        if !matches!(track.kind, klipp_timeline::TrackKind::Video) { continue; }
                        if let Some((_, clip)) = track.clip_at_timecode(ph) {
                            if clip.media_id().is_some() {
                                angle_clips.push((ti, clip.id));
                            }
                        }
                    }
                    if angle_clips.len() < 2 {
                        self.state.toast.show("Need 2+ video clips at playhead for multicam".to_string());
                        return true;
                    }
                    let group_id = klipp_core::GroupId::new();
                    let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                    for (angle_idx, (ti, cid)) in angle_clips.iter().enumerate() {
                        if let Some(clip) = seq.tracks[*ti].clips.iter_mut().find(|c| c.id == *cid) {
                            clip.multicam_group = Some(group_id);
                            clip.multicam_angle = angle_idx as u8;
                        }
                    }
                    self.state.toast.show(format!("Created multicam group with {} angles", angle_clips.len()));
                    self.state.dirty = true;
                    self.sync_sequence_to_engine();
                    true
                }
                AppCommand::CreateSyncedMulticam { media_ids, method } => {
                    self.spawn_multicam_sync(media_ids.clone(), *method);
                    true
                }
                // Switch multicam angle: record a cut in the multicam cut list
                AppCommand::SwitchMulticamAngle { angle } => {
                    self.switch_multicam_angle(*angle, self.state.playhead);
                    true
                }
                AppCommand::SwitchMulticamAngleAt { angle, at } => {
                    self.switch_multicam_angle(*angle, *at);
                    true
                }
                AppCommand::SetMulticamSound { sound, at } => {
                    self.set_multicam_sound(*sound, at.unwrap_or(self.state.playhead));
                    true
                }
                // Fit timeline zoom to show all clips
                _ => false,
        }
    }

    fn handle_timeline_navigation_markers(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        match cmd {
                // The selected clips, all of them, filling the timeline's
                // width (#911): the menu item ran Fit to Window, and the key
                // took only the clip last clicked.
                AppCommand::ZoomToSelection => {
                    let seq = &self.state.sequences[self.state.active_sequence_idx];
                    let span = seq
                        .all_clips()
                        .filter(|c| self.state.selected_clip == Some(c.id) || self.state.selected_clips.contains(&c.id))
                        .fold(None, |span: Option<(f64, f64)>, c| {
                            let (a, b) = (c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64());
                            Some(span.map_or((a, b), |(lo, hi)| (lo.min(a), hi.max(b))))
                        });
                    if let Some((start, end)) = span {
                        let dur = ((end - start) as f32).max(0.1);
                        let content_width = self.state.last_timeline_content_width;
                        self.state.timeline_zoom = (content_width / dur * 0.9).clamp(0.1, 2000.0);
                        self.state.timeline_scroll_x = (start as f32 * self.state.timeline_zoom - 20.0).max(0.0);
                    }
                    true
                }
                AppCommand::FitTimelineToWindow => {
                    let seq = &self.state.sequences[self.state.active_sequence_idx];
                    let max_secs = seq.tracks.iter()
                        .flat_map(|t| t.clips.iter())
                        .map(|c| c.timeline_out.to_secs_f64())
                        .fold(0.0f64, f64::max);
                    let avail_w = self.state.last_timeline_content_width;
                    let raw_zoom = if max_secs > 0.01 { avail_w / (max_secs as f32 * 1.05) } else { 0.0 };
                    let clamped = raw_zoom.clamp(0.1, 2000.0);
                    let total_track_h: f32 = seq.tracks.iter()
                        .map(crate::panels::timeline::track_row_height)
                        .sum();
                    let track_scale = crate::panels::timeline::fit_track_scale(
                        total_track_h,
                        self.state.last_timeline_tracks_height,
                    );
                    tracing::info!(
                        "TL_ZOOM FitTimelineToWindow max_secs={:.3} avail_w={:.1} raw_zoom={:.4} clamped={:.4} total_track_h={:.1} avail_track_h={:.1} track_scale={:.3}",
                        max_secs, avail_w, raw_zoom, clamped, total_track_h,
                        self.state.last_timeline_tracks_height, track_scale
                    );
                    self.state.timeline_track_scale = track_scale;
                    self.state.timeline_scroll_y = 0.0;
                    if max_secs > 0.01 {
                        self.state.timeline_zoom = clamped;
                        self.state.timeline_scroll_x = 0.0;
                    }
                    true
                }
                AppCommand::SetTimelineZoom { zoom, scroll_x } => {
                    let clamped = zoom.clamp(0.1, 2000.0);
                    tracing::info!(
                        "TL_ZOOM SetTimelineZoom requested={:.4} clamped={:.4} scroll_x={:?} prev_zoom={:.4} prev_scroll={:.1}",
                        zoom, clamped, scroll_x, self.state.timeline_zoom, self.state.timeline_scroll_x
                    );
                    self.state.timeline_zoom = clamped;
                    let target_scroll = scroll_x
                        .map(|sx| sx.max(0.0))
                        .unwrap_or(self.state.timeline_scroll_x);
                    // Clamp scroll to valid range for the NEW zoom.
                    // Previously the plain zoom-out buttons left the
                    // old scroll value in place — when that value
                    // pointed past the end of all content at the new
                    // (smaller) zoom, the viewport showed nothing and
                    // every clip was culled HL (left of view).
                    let total_secs = self.state.sequence_duration().to_secs_f64() as f32;
                    let content_w = self.state.last_timeline_content_width.max(1.0);
                    let max_scroll = (total_secs * clamped - content_w).max(0.0);
                    self.state.timeline_scroll_x = target_scroll.clamp(0.0, max_scroll);
                    true
                }
                AppCommand::ZoomTimelineToFit => {
                    let dur = self.state.sequence_duration().to_secs_f64() as f32;
                    let w = self.state.last_timeline_content_width;
                    let raw = if dur > 0.0 && w > 0.0 { w / dur } else { 0.0 };
                    let clamped = raw.clamp(0.1, 2000.0);
                    let total_track_h: f32 = self.state.sequence().tracks.iter()
                        .map(crate::panels::timeline::track_row_height)
                        .sum();
                    let track_scale = crate::panels::timeline::fit_track_scale(
                        total_track_h,
                        self.state.last_timeline_tracks_height,
                    );
                    tracing::info!(
                        "TL_ZOOM ZoomTimelineToFit dur={:.3} w={:.1} raw={:.4} clamped={:.4} total_track_h={:.1} avail_track_h={:.1} track_scale={:.3}",
                        dur, w, raw, clamped, total_track_h,
                        self.state.last_timeline_tracks_height, track_scale
                    );
                    self.state.timeline_track_scale = track_scale;
                    self.state.timeline_scroll_y = 0.0;
                    if dur > 0.0 && w > 0.0 {
                        self.state.timeline_zoom = clamped;
                        self.state.timeline_scroll_x = 0.0;
                    }
                    true
                }
                // Apply motion preset (Ken Burns, PiP)
                AppCommand::ApplyMotionPreset { clip_id, preset } => {
                    let cid = *clip_id;
                    let preset = preset.clone();
                    // Capture sequence resolution before the mutable
                    // borrow of `seq` so the PiP helpers can read it
                    // without borrow conflicts.
                    let res = self.state.sequences[self.state.active_sequence_idx].resolution;
                    let Some(dur) = self.state.sequences[self.state.active_sequence_idx].all_clips().find(|c| c.id == cid).map(|c| c.duration().to_secs_f64()) else { return true };
                    let key = |time_secs: f64, value: f32| klipp_core::Keyframe { time_secs, value, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
                    let lanes: Vec<(&str, Vec<klipp_core::Keyframe>)> = match preset.as_str() {
                        "ken_burns_zoom_in" => vec![("scale", vec![key(0.0, 1.0), key(dur, 1.4)])],
                        "ken_burns_zoom_out" => vec![("scale", vec![key(0.0, 1.4), key(dur, 1.0)])],
                        "ken_burns_pan_left" => vec![("scale", vec![key(0.0, 1.3), key(dur, 1.3)]), ("pos_x", vec![key(0.0, 100.0), key(dur, -100.0)])],
                        // PiP corners. Resolution-aware so the
                        // clip lands roughly the same distance
                        // from the frame edge regardless of
                        // 720p / 1080p / 4K. Magnitudes:
                        //   scale  = 0.30  (clip is 30 % of frame)
                        //   margin = 0.04  (4 % from frame edge)
                        //   offset = frame_dim * (0.5 - 0.15 - 0.04)
                        //          = frame_dim * 0.31
                        // For 1080p that's (595, 335) — close to
                        // the original eyeballed (600, 300) so
                        // existing 1080p projects barely shift.
                        // Sign-per-corner is encoded in
                        // `pip_offsets_for_corner`.
                        "pip_top_left" | "pip_top_right" | "pip_bottom_left" | "pip_bottom_right" => {
                            let (px, py) = pip_offsets_for_corner(res, preset.as_str());
                            vec![("scale", vec![key(0.0, 0.3)]), ("pos_x", vec![key(0.0, px)]), ("pos_y", vec![key(0.0, py)])]
                        }
                        _ => Vec::new(),
                    };
                    // Its lanes by path, one step (M4 step 12c).
                    if !lanes.is_empty() {
                        match self.state.set_lanes(cid, &lanes, "Apply Motion Preset") {
                            Ok(()) => {
                                self.state.toast.show(format!("Applied {} preset", preset.replace('_', " ")));
                                self.state.dirty = true;
                            }
                            Err(why) => self.state.toast.show(why),
                        }
                    }
                    self.sync_sequence_to_engine();
                    true
                }
                // Next edit point: the nearest clip boundary, marker, In or Out after the playhead (#921)
                AppCommand::NextEditPoint => {
                    let ph = self.state.playhead;
                    let seq = &self.state.sequences[self.state.active_sequence_idx];
                    let points = crate::navigation::edit_points(seq);
                    if let Some(next) = crate::navigation::step_to(&points, ph, true) {
                        self.state.playhead = next;
                        let _ = self.cmd_tx.send(AppCommand::SeekTo(next));
                    }
                    true
                }
                // Previous edit point: the nearest clip boundary, marker, In or Out before the playhead (#921)
                AppCommand::PrevEditPoint => {
                    let ph = self.state.playhead;
                    let seq = &self.state.sequences[self.state.active_sequence_idx];
                    let points = crate::navigation::edit_points(seq);
                    if let Some(prev) = crate::navigation::step_to(&points, ph, false) {
                        self.state.playhead = prev;
                        let _ = self.cmd_tx.send(AppCommand::SeekTo(prev));
                    }
                    true
                }
                // Close gap: remove empty space on a track by shifting downstream clips left
                AppCommand::CloseGap { track_index, gap_start, gap_end } => {
                    let ti = *track_index;
                    let start = *gap_start;
                    let end = *gap_end;
                    let gap_dur = end - start;
                    if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(ti) {
                        // Remove any explicit Gap clips in this range
                        track.clips.retain(|c| {
                            !(matches!(c.kind, klipp_timeline::ClipKind::Gap)
                              && c.timeline_in >= start && c.timeline_out <= end)
                        });
                        // Shift all clips that start at or after gap_end left by gap duration
                        for c in track.clips.iter_mut() {
                            if c.timeline_in >= end {
                                c.timeline_in -= gap_dur;
                                c.timeline_out -= gap_dur;
                            }
                        }
                        track.sort_clips();
                        self.state.dirty = true;
                    }
                    self.sync_sequence_to_engine();
                    true
                }
                // Phase 4: Sequence markers
                AppCommand::SetSequenceInPoint(tc) => {
                    self.state.sequences[self.state.active_sequence_idx].in_point = Some(*tc);
                    self.sync_sequence_to_engine();
                    true
                }
                AppCommand::SetSequenceOutPoint(tc) => {
                    self.state.sequences[self.state.active_sequence_idx].out_point = Some(*tc);
                    self.sync_sequence_to_engine();
                    true
                }
                AppCommand::ClearSequenceInPoint => {
                    self.state.sequences[self.state.active_sequence_idx].in_point = None;
                    self.sync_sequence_to_engine();
                    true
                }
                AppCommand::ClearSequenceOutPoint => {
                    self.state.sequences[self.state.active_sequence_idx].out_point = None;
                    self.sync_sequence_to_engine();
                    true
                }
                // Phase 4: Source monitor
                _ => false,
        }
    }

    fn handle_timeline_editing(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        match cmd {
                AppCommand::LoadSourceMedia { media_id } => {
                    // Clear stale source I/O from previous source clip
                    if let Some(old_mid) = self.state.source_media_id {
                        if old_mid != *media_id {
                            if let Some(old_item) = self.state.media_pool.get_mut(&old_mid) {
                                old_item.default_source_in = None;
                                old_item.default_source_out = None;
                            }
                        }
                    }
                    self.state.source_media_id = Some(*media_id);
                    self.state.source_playhead = Timecode::zero();
                    self.state.viewer_mode = ViewerMode::Source;
                    // Drop any frame texture from the previous clip so the
                    // viewer doesn't flash a stale image while the new
                    // first-frame decode is in flight.
                    if !matches!(&self.state.source_video_texture, Some((mid, _)) if mid == media_id) {
                        if let Some((_, old)) = self.state.source_video_texture.take() {
                            self.defer_drop_texture(old);
                        }
                    }
                    // Bind the engine's source decoder to this media so the
                    // transport (Play / J / K / L / Space / arrows) drives
                    // real playback. The legacy `start_source_frame_decode_at`
                    // call below stays as the cold-start fallback — it
                    // produces a thumbnail-style first frame in
                    // `source_video_texture` immediately while the engine
                    // decoder warms up. Once the engine starts pushing into
                    // `source_frame_buffer`, the source viewer paints from
                    // that and the cached `source_video_texture` falls back
                    // to a placeholder role.
                    if let Some(item) = self.state.media_pool.get(media_id) {
                        if !item.offline && item.path.exists()
                            && !item.video_codec.is_empty()
                        {
                            // Prefer the skim file (1080p H.264 8-bit
                            // all-keyframes) over the source for the
                            // source viewer's persistent decoder.
                            // Scrubbing 4K HEVC = 50–100 ms per
                            // NVDEC seek; 1080p H.264 = 5–15 ms — the
                            // difference between "9 of 10 frames
                            // missed" on a drag and a frame-perfect
                            // scrub. Falls back to the original
                            // source when the skim hasn't built yet.
                            let decode_path = item
                                .skim_file
                                .as_ref()
                                .filter(|p| p.exists())
                                .cloned()
                                .unwrap_or_else(|| item.path.clone());
                            // Skim/proxy transcodes are autorotated by ffmpeg
                            // during encode, so re-applying item.source_rotation
                            // here would double-rotate (portrait iPhone clips
                            // ended up sideways in the source viewer). Use the
                            // path-aware helper instead.
                            let rotation = item.effective_rotation_for(&decode_path);
                            let _ = self.cmd_tx.send(AppCommand::SetSourceMedia {
                                media_id: *media_id,
                                path: decode_path,
                                source_rotation: rotation,
                            });
                        }
                    }
                    self.start_source_frame_decode_at(*media_id, 0.0);
                    true
                }
                AppCommand::SeekSource(tc) => {
                    let Some(mid) = self.state.source_media_id else {
                        return true;
                    };
                    let secs = tc.to_secs_f64();
                    // Clamp to media duration so the scrub bar can't hand
                    // us an out-of-range time on a near-rounding edge.
                    let clamped_secs = if let Some(item) = self.state.media_pool.get(&mid) {
                        let dur = item.duration.to_secs_f64();
                        secs.clamp(0.0, dur)
                    } else {
                        secs.max(0.0)
                    };
                    self.state.source_playhead = Timecode::from_secs_f64(clamped_secs, tc.rate);
                    // Engine-only path: the persistent NVDEC decoder
                    // bound by `SetSourceMedia` is now responsible for
                    // every seek's decoded frame (lands in
                    // `source_frame_buffer`). The legacy
                    // `start_source_frame_decode_at` was spawning a
                    // SOFTWARE-decode thread per SeekSource — at
                    // 30 Hz scrub-dispatch that pinned every CPU
                    // core in fresh-ffmpeg-context-open + software
                    // HEVC decode + swscale, starving the timeline
                    // engine to ~1 frame per 3 s. Removed; the
                    // engine seek alone produces the frame.
                    let _ = self.cmd_tx.send(AppCommand::SeekSource(
                        Timecode::from_secs_f64(clamped_secs, tc.rate),
                    ));
                    true
                }
                AppCommand::MatchFrameProgramToSource => {
                    self.run_match_frame_program_to_source();
                    true
                }
                AppCommand::MatchFrameSourceToProgram => {
                    self.run_match_frame_source_to_program();
                    true
                }
                AppCommand::OpenModelDownloadDialog { model_id } => {
                    self.request_model_download(model_id);
                    true
                }
                AppCommand::SetSourceInPoint(tc) => {
                    if let Some(mid) = self.state.source_media_id {
                        if let Some(item) = self.state.media_pool.get_mut(&mid) {
                            item.default_source_in = Some(*tc);
                        }
                    }
                    true
                }
                AppCommand::SetSourceOutPoint(tc) => {
                    if let Some(mid) = self.state.source_media_id {
                        if let Some(item) = self.state.media_pool.get_mut(&mid) {
                            item.default_source_out = Some(*tc);
                        }
                    }
                    true
                }
                AppCommand::CreateSubclipFromZone { source_media_id, name } => {
                    self.create_subclip_from_zone(*source_media_id, name.clone());
                    true
                }
                AppCommand::InsertSourceAtPlayhead | AppCommand::OverwriteEdit => {
                    if let Some(mid) = self.state.source_media_id {
                        if self.maybe_prompt_framerate_mismatch(&mid, cmd.clone()) {
                            return true;
                        }
                        self.suppress_fps_prompt = false;
                        let Some(item) = self.state.media_pool.get(&mid).cloned() else {
                            self.state.toast.show("Source clip is no longer in the media pool");
                            return true;
                        };
                        {
                            if item.offline {
                                self.state.toast.show(format!(
                                    "Cannot edit '{}' — media is offline",
                                    item.filename
                                ));
                                return true;
                            }
                            // Three-point placement from the source's and the
                            // sequence's marks: a sequence Out alone backtimes (#919).
                            let (timeline_in, source_in, source_out) = self.source_placement(&item);
                            let dur = source_out - source_in;
                            let is_audio_only = item.video_codec.is_empty();
                            let target_kind = if is_audio_only {
                                klipp_timeline::TrackKind::Audio
                            } else {
                                klipp_timeline::TrackKind::Video
                            };
                            let seq_idx = self.state.active_sequence_idx;
                            let has_target_track = self.state.sequences[seq_idx].tracks.iter()
                                .any(|t| t.kind == target_kind);
                            if !has_target_track {
                                self.state.toast.show(if is_audio_only {
                                    "No audio track in sequence"
                                } else {
                                    "No video track in sequence"
                                });
                                return true;
                            }
                            let new_clip = klipp_timeline::Clip::media(
                                mid,
                                timeline_in,
                                timeline_in + dur,
                                source_in,
                                source_out,
                            );
                            let new_id = new_clip.id;

                            // Magnetic path: video clip, primary track exists, primary matches
                            // target kind, magnetic timeline enabled. Everything else falls
                            // through to the legacy track.clips path.
                            // The track the source is patched to (#915); the
                            // magnetic path only when that is the primary.
                            let target = self.state.sequences[seq_idx].source_track(target_kind).expect("checked above");
                            let primary_idx = self.state.sequences[seq_idx].primary_track_index();
                            let use_magnetic = !is_audio_only
                                && primary_idx.is_some_and(|idx| {
                                    idx == target
                                        && self.is_magnetic_track(idx)
                                        && self.state.sequences[seq_idx].tracks[idx].kind == target_kind
                                });

                            if use_magnetic {
                                self.apply_source_magnetic_edit(
                                    seq_idx,
                                    &item,
                                    mid,
                                    new_clip,
                                    timeline_in,
                                    dur,
                                    source_in,
                                    source_out,
                                    klipp_timeline::magnetic::EditMode::Overwrite,
                                );
                            } else {
                                // Its sound under it, as an insert lays one; the
                                // edit is one undo step (tracks_undo, #919).
                                Self::overwrite_insert_clip_on_track(&mut self.state.sequences[seq_idx].tracks[target], new_clip);
                                if item.has_audio && !is_audio_only {
                                    let external = self.external_audio_of(mid);
                                    let _ = attach_linked_audio_to_clip(&mut self.state.sequences[seq_idx], new_id, external);
                                }
                            }
                            self.state.selected_clip = Some(new_id);
                            self.state.dirty = true;
                            self.sync_sequence_to_engine();
                        }
                    }
                    true
                }
                AppCommand::InsertEdit => {
                    // Ripple insert: push existing clips right, then insert source clip
                    if let Some(mid) = self.state.source_media_id {
                        if self.maybe_prompt_framerate_mismatch(&mid, cmd.clone()) {
                            return true;
                        }
                        self.suppress_fps_prompt = false;
                        let Some(item) = self.state.media_pool.get(&mid).cloned() else {
                            self.state.toast.show("Source clip is no longer in the media pool");
                            return true;
                        };
                        {
                            if item.offline {
                                self.state.toast.show(format!(
                                    "Cannot edit '{}' — media is offline",
                                    item.filename
                                ));
                                return true;
                            }
                            // Three-point placement from the source's and the
                            // sequence's marks: a sequence Out alone backtimes (#919).
                            let (timeline_in, source_in, source_out) = self.source_placement(&item);
                            let dur = source_out - source_in;
                            let is_audio_only = item.video_codec.is_empty();
                            let target_kind = if is_audio_only {
                                klipp_timeline::TrackKind::Audio
                            } else {
                                klipp_timeline::TrackKind::Video
                            };
                            let seq_idx = self.state.active_sequence_idx;
                            let has_target_track = self.state.sequences[seq_idx].tracks.iter()
                                .any(|t| t.kind == target_kind);
                            if !has_target_track {
                                self.state.toast.show(if is_audio_only {
                                    "No audio track in sequence"
                                } else {
                                    "No video track in sequence"
                                });
                                return true;
                            }
                            let new_clip = klipp_timeline::Clip::media(
                                mid,
                                timeline_in,
                                timeline_in + dur,
                                source_in,
                                source_out,
                            );
                            let new_id = new_clip.id;

                            // The track the source is patched to (#915); the
                            // magnetic path only when that is the primary.
                            let target = self.state.sequences[seq_idx].source_track(target_kind).expect("checked above");
                            let primary_idx = self.state.sequences[seq_idx].primary_track_index();
                            let use_magnetic = !is_audio_only
                                && primary_idx.is_some_and(|idx| {
                                    idx == target
                                        && self.is_magnetic_track(idx)
                                        && self.state.sequences[seq_idx].tracks[idx].kind == target_kind
                                });

                            if use_magnetic {
                                self.apply_source_magnetic_edit(
                                    seq_idx,
                                    &item,
                                    mid,
                                    new_clip,
                                    timeline_in,
                                    dur,
                                    source_in,
                                    source_out,
                                    klipp_timeline::magnetic::EditMode::Insert,
                                );
                            } else {
                                // Free tracks: the track-based insert. Every
                                // unlocked track is cut at the point and
                                // rippled by the clip's length, so they stay
                                // in sync; a straddling clip is split. See
                                // klipp_timeline::insert_edit.
                                let Some(mut edit) = self.state.sequences[seq_idx].insert_edit(target, new_clip, timeline_in) else {
                                    self.state.toast.show("Target track is locked");
                                    return true;
                                };
                                // The clip's sound goes under it as a linked
                                // audio clip, as the magnetic path lays one.
                                // The ripple already cleared that span on the
                                // audio track; a track is made if there is
                                // none. The record's clip takes the link so a
                                // redo brings it back linked.
                                let (audio, created_audio_track_at) = if item.has_audio && !is_audio_only {
                                    let external = self.state.sequences[seq_idx]
                            .all_clips()
                            .find(|c| c.id == new_id)
                            .and_then(|c| c.media_id())
                            .and_then(|m| self.external_audio_of(m));
                        match attach_linked_audio_to_clip(&mut self.state.sequences[seq_idx], new_id, external) {
                                        Some(a) => {
                                            edit.clip.linked_clip_id = Some(a.clip.id);
                                            (Some((a.clip, a.track_index)), a.created_track_at)
                                        }
                                        None => (None, None),
                                    }
                                } else {
                                    (None, None)
                                };
                                self.state.push_history(EditCommand::LegacyInsertEdit { edit, audio, created_audio_track_at });
                            }
                            self.state.selected_clip = Some(new_id);
                            self.state.dirty = true;
                            self.sync_sequence_to_engine();
                        }
                    }
                    true
                }
                AppCommand::LiftEdit => {
                    // Remove content between sequence in/out, leave gap
                    let (in_pt, out_pt) = match (
                        self.state.sequences[self.state.active_sequence_idx].in_point,
                        self.state.sequences[self.state.active_sequence_idx].out_point,
                    ) {
                        (Some(i), Some(o)) => (i, o),
                        _ => {
                            self.state.toast.show("Set sequence In and Out points first");
                            return true;
                        }
                    };
                    if in_pt >= out_pt {
                        self.state.toast.show("Out point must be after In point");
                        return true;
                    }
                    // Locked tracks are left as they are, and named (#908).
                    let mut skipped = Vec::new();
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if track.locked {
                            skipped.push(track.name.clone());
                            continue;
                        }
                        Self::lift_range(track, in_pt, out_pt);
                    }
                    if !skipped.is_empty() {
                        self.state.toast.show(format!("Lift left locked tracks as they were: {}", skipped.join(", ")));
                    }
                    self.state.dirty = true;
                    self.sync_sequence_to_engine();
                    true
                }
                AppCommand::ExtractEdit => {
                    // Remove content between sequence in/out, ripple left to close gap
                    let (in_pt, out_pt) = match (
                        self.state.sequences[self.state.active_sequence_idx].in_point,
                        self.state.sequences[self.state.active_sequence_idx].out_point,
                    ) {
                        (Some(i), Some(o)) => (i, o),
                        _ => {
                            self.state.toast.show("Set sequence In and Out points first");
                            return true;
                        }
                    };
                    if in_pt >= out_pt {
                        self.state.toast.show("Out point must be after In point");
                        return true;
                    }
                    let gap = out_pt - in_pt;
                    let mut skipped = Vec::new();
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if track.locked {
                            skipped.push(track.name.clone());
                            continue;
                        }
                        Self::lift_range(track, in_pt, out_pt);
                        // Ripple: shift clips at or after out_pt left by gap
                        for clip in &mut track.clips {
                            if clip.timeline_in >= out_pt {
                                clip.timeline_in -= gap;
                                clip.timeline_out -= gap;
                            }
                        }
                    }
                    if !skipped.is_empty() {
                        self.state.toast.show(format!("Extract left locked tracks as they were: {}", skipped.join(", ")));
                    }
                    self.state.dirty = true;
                    self.sync_sequence_to_engine();
                    true
                }
                // Magnetic: Ripple trim head to playhead
                AppCommand::RippleTrimHead { clip_id } => {
                    if self.refused_by_lock(clip_id) { return true; }
                    let seq = &self.state.sequences[self.state.active_sequence_idx];
                    if let Some((track_idx, clip_idx)) = seq.find_clip(*clip_id) {
                        let clip = &seq.tracks[track_idx].clips[clip_idx];
                        let playhead = self.state.playhead;
                        // Only trim if playhead is inside the clip
                        if playhead > clip.timeline_in && playhead < clip.timeline_out {
                            let new_timeline_in = playhead;
                            if self.is_magnetic_track(track_idx) {
                                let result = klipp_timeline::magnetic::MagneticEditor::trim_head(
                                    &mut self.state.sequences[self.state.active_sequence_idx], *clip_id, new_timeline_in);
                                self.state.push_history(EditCommand::Magnetic(result.command));
                            } else {
                                self.trim_clip_at(*clip_id, klipp_document::ops::Edge::In, playhead);
                                return true;
                            }
                            self.state.dirty = true;
                            self.sync_sequence_to_engine();
                        }
                    }
                    true
                }
                // Magnetic: Ripple trim tail to playhead
                AppCommand::RippleTrimTail { clip_id } => {
                    if self.refused_by_lock(clip_id) { return true; }
                    let seq = &self.state.sequences[self.state.active_sequence_idx];
                    if let Some((track_idx, clip_idx)) = seq.find_clip(*clip_id) {
                        let clip = &seq.tracks[track_idx].clips[clip_idx];
                        let playhead = self.state.playhead;
                        // Only trim if playhead is inside the clip
                        if playhead > clip.timeline_in && playhead < clip.timeline_out {
                            let new_timeline_out = playhead;
                            if self.is_magnetic_track(track_idx) {
                                let result = klipp_timeline::magnetic::MagneticEditor::trim_tail(
                                    &mut self.state.sequences[self.state.active_sequence_idx], *clip_id, new_timeline_out);
                                self.state.push_history(EditCommand::Magnetic(result.command));
                            } else {
                                self.trim_clip_at(*clip_id, klipp_document::ops::Edge::Out, playhead);
                                return true;
                            }
                            self.state.dirty = true;
                            self.sync_sequence_to_engine();
                        }
                    }
                    true
                }
                // Q — trim OUT edge of nearest clip at/before playhead.
                AppCommand::TrimOutToPlayhead => {
                    self.trim_to_playhead_dispatch(TrimToPlayheadDir::Out);
                    true
                }
                // W — trim IN edge of nearest clip at/after playhead.
                AppCommand::TrimInToPlayhead => {
                    self.trim_to_playhead_dispatch(TrimToPlayheadDir::In);
                    true
                }
                // Insert silence / gap on a specific track at the playhead.
                // Distinct from InsertGapAtPlayhead which routes through the
                // magnetic editor and only lands on the primary track. Used
                // for "Insert N seconds of silence on this audio track" —
                // shifts every clip on the target track whose timeline_in
                // is at or after the playhead by `duration_secs`. Clips that
                // straddle the playhead stay where they are (the conventional
                // "click at a boundary" case is the common one; clip-splitting
                // mid-clip is a separate workflow).
                AppCommand::InsertGapOnTrack { track_idx, duration_secs } => {
                    let track_idx = *track_idx;
                    let duration_secs = *duration_secs;
                    if duration_secs <= 0.0 {
                        return true;
                    }
                    let playhead = self.state.playhead;
                    let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                    let Some(track) = seq.tracks.get_mut(track_idx) else {
                        self.state.toast.show("Insert silence — track not found");
                        return true;
                    };
                    let rate = if playhead.rate > 1 { playhead.rate } else { track.clips.first()
                        .map(|c| c.timeline_in.rate)
                        .unwrap_or(24) };
                    let delta = klipp_core::RationalTime::from_secs_f64(duration_secs, rate);
                    let mut shifted = 0usize;
                    for clip in &mut track.clips {
                        if clip.timeline_in >= playhead {
                            clip.timeline_in += delta;
                            clip.timeline_out += delta;
                            shifted += 1;
                        }
                    }
                    track.sort_clips();
                    self.state.toast.show(format!(
                        "Inserted {duration_secs:.2}s silence on track {track_idx} ({shifted} clip(s) rippled)"
                    ));
                    self.state.dirty = true;
                    self.sync_sequence_to_engine();
                    true
                }
                // Magnetic: Insert 1-second gap at playhead
                AppCommand::InsertGapAtPlayhead => {
                    let playhead = self.state.playhead;
                    let rate = if playhead.rate > 1 { playhead.rate } else { 24 };
                    let one_sec = klipp_core::RationalTime::from_frames(rate as i64, rate);
                    // Resolve user snap config so a gap inserted at the
                    // playhead respects the same snap rules as a clip
                    // dragged onto the timeline.
                    let active_seq = &self.state.sequences[self.state.active_sequence_idx];
                    let pps = self.state.timeline_zoom;
                    let grid_interval_secs = crate::timeline::grid_snap::resolve_grid_interval_secs(
                        self.state.grid_snap_interval,
                        active_seq.fps.rate(),
                        pps,
                    );
                    let snap_cfg = self.state.snap;
                    let result = klipp_timeline::magnetic::MagneticEditor::insert_gap(
                        &mut self.state.sequences[self.state.active_sequence_idx],
                        one_sec, playhead, grid_interval_secs, Some(&snap_cfg));
                    self.state.push_history(EditCommand::Magnetic(result.command));
                    self.state.dirty = true;
                    self.sync_sequence_to_engine();
                    true
                }
            _ => false,
        }
    }

    fn trim_clip_at(&mut self, clip_id: ClipId, edge: klipp_document::ops::Edge, target: klipp_core::Timecode) {
        match klipp_document::ops::trim_at(self.state.sequence(), &self.state.media_pool, clip_id, edge, target, true) {
            Ok(edit) => self.apply_edits_as_one(&[edit], "Ripple Trim"),
            Err(r) => self.state.toast.show(format!("Trim unchanged: {}", r.why)),
        }
    }

    /// Select what `range` takes of `track` (every track when `None`),
    /// the first of it the primary, and say so when it is nothing (#844).
    pub(super) fn select_clips_in(&mut self, track: Option<usize>, range: klipp_core::SelectRange) {
        let ids = crate::selection::clips_in(self.state.sequence(), track, range, self.state.playhead);
        if ids.is_empty() {
            let where_ = if track.is_some() { "on that track" } else { "on the timeline" };
            self.state.toast.show(format!("No clips to select {where_} (locked tracks are left out)"));
            return;
        }
        self.state.selected_clip = ids.first().copied();
        self.state.selected_clips = ids;
    }

    /// Lay `cb` into the open sequence at `at`, the new clips selected,
    /// every track it changed one undo step under `label` (#845).
    pub(super) fn paste_clipboard(&mut self, cb: &crate::clip_clipboard::ClipboardContent, at: klipp_core::Timecode, mode: crate::clip_clipboard::PasteMode, label: &'static str) {
        let before = self.state.sequence().clone();
        let pasted = crate::clip_clipboard::paste(&before, cb, at, mode);
        if pasted.no_room {
            self.state.toast.show(format!("{label}: could not open room at the playhead (is the track locked?); nothing was pasted"));
            return;
        }
        if pasted.ids.is_empty() {
            self.state.toast.show(format!("{label}: no track of the clips' kind to put them on"));
            return;
        }
        let mut edits = crate::clip_clipboard::track_edits(&before, &pasted.sequence, &pasted.touched, label);
        // The copies' node labels, in the same step (#1753).
        if pasted.sequence.node_labels != before.node_labels {
            edits.push(EditCommand::SetNodeLabels { old: before.node_labels.clone(), new: pasted.sequence.node_labels.clone() });
        }
        self.apply_edits_as_one(&edits, label);
        self.state.selected_clip = pasted.ids.first().copied();
        self.state.selected_clips = if pasted.ids.len() > 1 { pasted.ids.clone() } else { Vec::new() };
        match (pasted.ids.len(), pasted.skipped) {
            (1, 0) => {}
            (n, 0) => self.state.toast.show(format!("{label}: {n} clips")),
            (n, k) => self.state.toast.show(format!("{label}: {n} clips; {k} had no track of their kind here")),
        }
    }

    pub(super) fn remove_clip_impl(&mut self, clip_id: klipp_core::ClipId) {
        if self.clip_track_is_locked(&clip_id) { return; }
        let Some((ti, _)) = self.state.sequence().find_clip(clip_id) else { return };
        if !self.is_magnetic_track(ti) {
            match crate::clip_edits::remove(self.state.sequence(), &self.state.media_pool, clip_id) {
                Ok(edit) => {
                    self.apply_edits_as_one(&[edit], "Remove Clip");
                    if self.state.selected_clip.is_some_and(|id| self.state.sequence().find_clip(id).is_none()) {
                        self.state.selected_clip = None;
                    }
                    return;
                }
                Err(r) => {
                    self.state.toast.show(format!("Remove unchanged: {}. Unlock the linked track or use Lift to leave a gap.", r.why));
                    return;
                }
            }
        }

        // Cross-field invariant: any clip that pointed at this one as a matte
        // source must have its binding cleared so we don't orphan it. Done
        // before the remove so the consumer's own state is consistent at
        // every observation point. Mirrors the pattern called out in the
        // `route-engine-state-through-AppCommand` memory.
        let seq = &mut self.state.sequences[self.state.active_sequence_idx];
        for c in seq.all_clips_mut() {
            if c.matte_source.as_ref().is_some_and(|b| b.source_clip_id == clip_id) {
                c.matte_source = None;
            }
            // Same cascade-clear policy for transform_link: a clip
            // pointing at the removed clip as its transform-link
            // target loses the binding so a stale ClipId never
            // outlives its target.
            if c.transform_link.as_ref().is_some_and(|tl| tl.target.referenced_clip_id() == clip_id) {
                c.transform_link = None;
            }
        }

        // A transition joining the clip (or its linked partner, which the
        // ripple delete takes too) goes with it, in the same undo step
        // (#979): left behind, it named a clip that was gone.
        let gone: Vec<klipp_core::ClipId> = std::iter::once(clip_id).chain(self.state.sequence().find_clip(clip_id).and_then(|(ti, ci)| self.state.sequence().tracks[ti].clips[ci].linked_clip_id)).collect();
        let transitions = klipp_document::ops::transitions_removed_with(self.state.sequence(), &gone);
        // One batch, unless a gesture removing several clips has one open.
        let own_batch = !transitions.is_empty() && !self.state.active_history_mut().is_batching();
        if own_batch {
            self.state.active_history_mut().begin_batch();
        }
        for e in &transitions {
            self.apply_edit_forward(e);
            self.state.push_history(e.clone());
        }
        let result = klipp_timeline::magnetic::MagneticEditor::ripple_delete(
            &mut self.state.sequences[self.state.active_sequence_idx], clip_id);
        self.state.push_history(EditCommand::Magnetic(result.command));
        if own_batch {
            self.state.active_history_mut().end_batch_as("Remove Clip");
        }
        if self.state.selected_clip == Some(clip_id) {
            self.state.selected_clip = None;
        }
        self.sync_sequence_to_engine();
        self.state.dirty = true;
    }

    /// Pro-NLE Q/W: pick a target clip by playhead proximity (no selection
    /// required) and ripple-trim its OUT (Q) or IN (W) edge to the playhead.
    /// Target track: the track of the current selection if any, otherwise the
    /// first video track, otherwise the first audio track.
    fn trim_to_playhead_dispatch(&mut self, dir: TrimToPlayheadDir) {
        let seq_idx = self.state.active_sequence_idx;
        let playhead = self.state.playhead;
        let seq = &self.state.sequences[seq_idx];

        let track_idx = if let Some(sel) = self.state.selected_clip {
            seq.tracks.iter().position(|t| t.find_clip(sel).is_some())
        } else {
            None
        }
        .or_else(|| seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video))
        .or_else(|| seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Audio));

        let Some(track_idx) = track_idx else {
            self.state.toast.show("No track to trim".to_string());
            return;
        };

        let target_clip = match dir {
            TrimToPlayheadDir::Out => seq.tracks[track_idx]
                .clip_with_out_at_or_before(&playhead)
                .map(|(_, c)| c.id),
            TrimToPlayheadDir::In => seq.tracks[track_idx]
                .clip_with_in_at_or_after(&playhead)
                .map(|(_, c)| c.id),
        };

        let Some(clip_id) = target_clip else {
            let msg = match dir {
                TrimToPlayheadDir::Out => "No clip to trim before playhead",
                TrimToPlayheadDir::In => "No clip to trim after playhead",
            };
            self.state.toast.show(msg.to_string());
            return;
        };

        // Reuse the existing handlers — they now correctly push undo and
        // ripple on legacy tracks after Gap 2.
        let cmd = match dir {
            TrimToPlayheadDir::Out => AppCommand::RippleTrimTail { clip_id },
            TrimToPlayheadDir::In => AppCommand::RippleTrimHead { clip_id },
        };
        let mut needs_refresh = false;
        self.handle_timeline_editing(&cmd, &mut needs_refresh);
    }

    /// Freeze the current viewer frame and insert it as a generator clip at
    /// the playhead. If a clip is under the playhead it is bladed first so
    /// the freeze slots cleanly between the two halves; downstream clips on
    /// the same track are rippled by `duration_secs` to make room.
    /// Create a subclip MediaItem from a parent's marked IN/OUT zone.
    /// Validates that the parent is not itself a subclip, that both IN and
    /// OUT are set, and that OUT > IN. Clones the parent's codec/fps/etc.,
    /// overrides `duration` to the zone length, and points the subclip at
    /// the same file with its own `default_source_in/out`. Pushes a
    /// `CreateSubclip` undo entry.
    /// Match Frame: program → source. Find the topmost video clip
    /// under the program playhead, load its media into the source
    /// viewer, and seek the source playhead to the source-time
    /// corresponding to the current program frame. Honours speed
    /// ramps + `clip.source_in`. No-op (with toast) when no media
    /// clip sits under the playhead.
    fn run_match_frame_program_to_source(&mut self) {
        let playhead = self.state.playhead;
        let seq = self.state.sequence();
        let fps = seq.fps.rate() as f64;

        // Layered pick:
        //   1. If a clip is selected (e.g. user right-clicked it
        //      and chose Match Frame) AND the playhead is inside
        //      that clip AND it has media, match against THAT clip.
        //      This is what makes right-click on an audio clip do
        //      the obvious thing — load its audio source — instead
        //      of silently switching to whatever video is at the
        //      same playhead. Pro NLEs honour the selection here.
        //   2. Else fall back to the topmost video clip at the
        //      playhead — this is the keyboard-F path and matches
        //      the engine's seek_in_sequence convention. Only
        //      targeted tracks are looked at (#915).
        //   3. Else walk audio tracks — handles pure-audio
        //      sequences (music edits, podcasts) where there's no
        //      video at the playhead at all. Without this fallback,
        //      F just toasts "no media clip at playhead" for those.
        let selected_hit = self.state.selected_clip.and_then(|sid| {
            seq.tracks.iter()
                .flat_map(|t| t.clips.iter())
                .find(|c| c.id == sid
                    && c.timeline_in <= playhead
                    && playhead < c.timeline_out)
                .and_then(|clip| {
                    let mid = clip.media_id()?;
                    let src = clip.source_time_for_sequence_time(playhead, fps)?;
                    Some((mid, src))
                })
        });
        let hit = selected_hit.or_else(|| {
            seq.tracks.iter()
                .enumerate()
                .filter(|(i, t)| t.kind == klipp_timeline::TrackKind::Video && seq.is_targeted(*i))
                .rev()
                .find_map(|(_, track)| {
                    track.clip_at_timecode(playhead).and_then(|(_, clip)| {
                        let mid = clip.media_id()?;
                        let src = clip.source_time_for_sequence_time(playhead, fps)?;
                        Some((mid, src))
                    })
                })
        }).or_else(|| {
            seq.tracks.iter()
                .enumerate()
                .filter(|(i, t)| t.kind == klipp_timeline::TrackKind::Audio && seq.is_targeted(*i))
                .rev()
                .find_map(|(_, track)| {
                    track.clip_at_timecode(playhead).and_then(|(_, clip)| {
                        let mid = clip.media_id()?;
                        let src = clip.source_time_for_sequence_time(playhead, fps)?;
                        Some((mid, src))
                    })
                })
        });
        let Some((media_id, source_time)) = hit else {
            self.state.toast.show("Match Frame: no media clip at playhead");
            return;
        };
        // Load the media into the source viewer if it isn't already
        // bound; LoadSourceMedia is idempotent w.r.t. seeking — it
        // resets source_playhead to zero — so we always follow it
        // with an explicit SeekSource.
        // Both through the app's own handlers: the engine handles
        // neither the load nor the playhead the seek sets (#437).
        let mut cmds = Vec::new();
        if self.state.source_media_id != Some(media_id) {
            cmds.push(AppCommand::LoadSourceMedia { media_id });
        }
        cmds.push(AppCommand::SeekSource(source_time));
        self.send_commands(cmds);
    }

    /// Match Frame: source → program. Walk every clip in the active
    /// sequence; for each one whose media matches the loaded source
    /// media AND whose source range contains the source playhead,
    /// compute the corresponding sequence time. Sort the matches and
    /// jump the program playhead to the first one strictly past the
    /// current playhead — wrapping to the earliest match if none.
    /// Pressing Shift+F repeatedly cycles through every place the
    /// source frame appears on the timeline. No-op (with toast) when
    /// no source media is loaded or no clip uses it at the loaded
    /// source-time.
    fn run_match_frame_source_to_program(&mut self) {
        let Some(media_id) = self.state.source_media_id else {
            self.state.toast.show("Match Frame: no source media loaded");
            return;
        };
        let source_time = self.state.source_playhead;
        let current_program_playhead = self.state.playhead;
        let seq = self.state.sequence();
        let fps = seq.fps.rate() as f64;
        // Search every track kind that can hold media — the loaded
        // source media may be referenced by video clips, audio
        // clips, or both (a single media file is often used as
        // both a linked video+audio pair). Walking only video
        // tracks would miss every occurrence on audio-only or
        // pure-audio uses of the source.
        let mut hits: Vec<Timecode> = seq
            .tracks
            .iter()
            .filter(|t| matches!(
                t.kind,
                klipp_timeline::TrackKind::Video | klipp_timeline::TrackKind::Audio
            ))
            .flat_map(|track| {
                track.clips.iter().filter_map(|clip| {
                    if clip.media_id() != Some(media_id) {
                        return None;
                    }
                    clip.sequence_time_for_source_time(source_time, fps)
                })
            })
            .collect();
        if hits.is_empty() {
            self.state.toast.show("Match Frame: source frame not on timeline");
            return;
        }
        hits.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        // Linked video+audio pairs reference the same source file
        // and produce the same sequence-time hit on both tracks.
        // Dedup post-sort so Shift+F cycle steps land on distinct
        // sequence positions instead of bouncing between the V and
        // A halves of the same pair.
        hits.dedup_by(|a, b| (a.to_secs_f64() - b.to_secs_f64()).abs() < 1e-6);
        // Pick the first hit strictly past the current playhead; if
        // none, wrap to the earliest. That gives a clean cycle: every
        // press lands on a different occurrence and eventually returns
        // to the first.
        let target = hits
            .iter()
            .find(|t| **t > current_program_playhead)
            .copied()
            .unwrap_or_else(|| hits[0]);
        let _ = self.cmd_tx.send(AppCommand::SeekTo(target));
    }

    fn create_subclip_from_zone(&mut self, source_media_id: klipp_core::MediaId, name: Option<String>) {
        // Clone the parent so we can release the immutable borrow before mutating.
        let parent = match self.state.media_pool.get(&source_media_id).cloned() {
            Some(p) => p,
            None => return,
        };
        if parent.is_subclip() {
            self.state.toast.show("Cannot create a subclip from a subclip.".to_string());
            return;
        }
        let zone_in = match parent.default_source_in {
            Some(tc) => tc,
            None => {
                self.state.toast.show(
                    "Set an IN point first (press I in the source viewer).".to_string(),
                );
                return;
            }
        };
        let zone_out = match parent.default_source_out {
            Some(tc) => tc,
            None => {
                self.state.toast.show(
                    "Set an OUT point first (press O in the source viewer).".to_string(),
                );
                return;
            }
        };
        if zone_out <= zone_in {
            self.state.toast.show("OUT point must be after IN point.".to_string());
            return;
        }
        self.make_subclip(source_media_id, zone_in, zone_out, name);
    }

    /// A subclip of `source_media_id` from `zone_in` to `zone_out`, as one
    /// undo step: the Source viewer's In to Out, or a timeline clip's
    /// source range (#925).
    pub(super) fn make_subclip(&mut self, source_media_id: klipp_core::MediaId, zone_in: Timecode, zone_out: Timecode, name: Option<String>) {
        let Some(parent) = self.state.media_pool.get(&source_media_id).cloned() else { return };
        // Auto-name: count existing subclips of this parent, suffix N+1.
        let n = self
            .state
            .media_pool
            .all()
            .filter(|i| i.parent_media_id == Some(source_media_id))
            .count()
            + 1;
        let parent_stem = split_stem_ext(&parent.filename).0;
        let auto_name = format!("{}_sub{}", parent_stem, n);
        let display_name = name.unwrap_or(auto_name);

        // Clone parent, then override the subclip-specific fields.
        let mut subclip = parent.clone();
        subclip.id = klipp_core::MediaId::new();
        subclip.filename = display_name.clone();
        subclip.duration = zone_out - zone_in;
        subclip.default_source_in = Some(zone_in);
        subclip.default_source_out = Some(zone_out);
        subclip.parent_media_id = Some(source_media_id);
        // Subclips are virtual — no standalone file footprint.
        subclip.file_size_bytes = 0;
        // Reset caches derived from the source file; they'll regenerate.
        subclip.proxy_path = None;
        subclip.proxy_ready = false;
        subclip.skim_file = None;
        subclip.needs_reprobe = false;
        subclip.waveform_data = None;
        subclip.audio_cache = None;

        self.state.media_pool.add(subclip.clone());
        self.state.push_history(crate::history::EditCommand::CreateSubclip {
                subclip,
                parent_id: source_media_id,
            });
        self.state.dirty = true;
        self.state.toast.show(format!("Subclip created: {display_name}"));
    }

    fn insert_freeze_frame(&mut self, duration_secs: f64) {
        // 1. Snapshot the viewer frame.
        let (rgba, width, height) = {
            let fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            if fb.data.is_empty() || fb.width == 0 || fb.height == 0 {
                drop(fb);
                self.state.toast.show("No frame to freeze yet".to_string());
                return;
            }
            (fb.data.clone(), fb.width, fb.height)
        };

        // 2. Work out the sidecar directory (saved project or temp fallback).
        let stills_root: std::path::PathBuf = match self.writable_project_dir() {
            Some(dir) => dir,
            None => klipp_project::temp_still_roots()[0].clone(),
        };
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let filename = format!("freeze_{nanos}.png");

        // 3. Atomic-write the PNG. Bail out with a toast on error.
        let png_path = match klipp_project::save_freeze_frame_png(
            &stills_root,
            &filename,
            &rgba,
            width,
            height,
        ) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("freeze frame: {e}");
                self.state.toast.show(format!("Freeze frame failed: {e}"));
                return;
            }
        };

        // 4. Pick target track + playhead snapped to sequence fps.
        let seq_idx = self.state.active_sequence_idx;
        let fps_rate = self.state.sequences[seq_idx].fps.rate();
        let ph = self.state.playhead.rescale_to(fps_rate);
        let duration_frames = (duration_secs * fps_rate as f64).round() as i64;
        let duration = klipp_core::RationalTime::from_frames(duration_frames.max(1), fps_rate);
        let track_idx = {
            let seq = &mut self.state.sequences[seq_idx];
            match seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video) {
                Some(i) => i,
                None => {
                    seq.tracks.insert(0, klipp_timeline::Track::new("Freeze", klipp_timeline::TrackKind::Video));
                    0
                }
            }
        };

        // 5. Blade any clip under the playhead on that track so the freeze
        //    inserts cleanly between halves. blade_clip_at pushes its own
        //    undo entry.
        let clip_under_ph = self.state.sequences[seq_idx].tracks[track_idx]
            .clip_at_timecode(ph)
            .map(|(_, c)| c.id);
        if let Some(cid) = clip_under_ph {
            // Only blade when the cut is interior — skip if the playhead sits
            // exactly on the clip's in or out edge (blade_at rejects edge cuts).
            let clip = self.state.sequences[seq_idx].tracks[track_idx]
                .find_clip(cid).cloned();
            if let Some(c) = clip {
                if c.timeline_in < ph && c.timeline_out > ph {
                    self.blade_clip_at(track_idx, cid, ph);
                }
            }
        }

        // 6. Ripple: shift every clip on the target track that starts at or
        //    after the playhead by +duration so the freeze has a gap to land
        //    in. Using a direct mutation rather than the magnetic editor so
        //    we don't touch other tracks.
        {
            let seq = &mut self.state.sequences[seq_idx];
            for c in seq.tracks[track_idx].clips.iter_mut() {
                if c.timeline_in >= ph {
                    c.timeline_in += duration;
                    c.timeline_out += duration;
                }
            }
        }

        // 7. Build and insert the freeze clip.
        let timeline_out = ph + duration;
        let clip = klipp_timeline::Clip {
            id: klipp_core::ClipId::new(),
            kind: klipp_timeline::ClipKind::Generator(klipp_core::GeneratorData::StillFrame {
                png_path: png_path.clone(),
                width,
                height,
            }),
            timeline_in: ph,
            timeline_out,
            source_in: klipp_core::RationalTime::from_secs_f64(0.0, fps_rate),
            source_out: duration,
            ..klipp_timeline::Clip::gap(ph, timeline_out)
        };
        let clip_id = clip.id;
        let clip_for_history = clip.clone();
        {
            let seq = &mut self.state.sequences[seq_idx];
            seq.tracks[track_idx].clips.push(clip);
            seq.tracks[track_idx].sort_clips();
        }

        // 8. Undo entry for the insertion. The blade already pushed its own.
        self.state.push_history(EditCommand::LegacyAppendClip {
                clip: clip_for_history,
                track_index: track_idx,
            });

        // 9. Finalize.
        self.state.selected_clip = Some(clip_id);
        self.state.toast.show(format!("Freeze frame inserted ({duration_secs:.1}s)"));
        self.state.dirty = true;
        self.sync_sequence_to_engine();
    }

    /// Cut a clip, and its linked partner with it, as one undo step.
    pub(super) fn blade_clip_at(&mut self, track_index: usize, clip_id: klipp_core::ClipId, cut_at: klipp_core::RationalTime) {
        // The clip and its partner are cut as two edits, one per track;
        // without a batch that took two undos, and one left the video
        // cut and its audio whole.
        let owns_batch = !self.state.active_history_mut().is_batching();
        if owns_batch {
            self.state.active_history_mut().begin_batch();
        }
        self.blade_clip_and_partner(track_index, clip_id, cut_at);
        if owns_batch {
            self.state.active_history_mut().end_batch();
        }
    }

    /// The cut itself: `klipp_document::blade::blade_linked`, which cuts
    /// the clip and the clip it is linked to together, or refuses the
    /// whole gesture — a pair across a lock is neither cut. Blades are
    /// queued one per track, so by the time one runs its clip may
    /// already have been cut through its partner (it now ends on the
    /// cut, or is gone); that refusal has nothing to say.
    fn blade_clip_and_partner(&mut self, track_index: usize, clip_id: klipp_core::ClipId, cut_at: klipp_core::RationalTime) {
        let magnetic: Vec<bool> = (0..self.state.sequence().tracks.len()).map(|ti| self.is_magnetic_track(ti)).collect();
        let seq = &mut self.state.sequences[self.state.active_sequence_idx];
        match klipp_document::blade::blade_linked(seq, track_index, clip_id, cut_at, |ti| magnetic.get(ti).copied().unwrap_or(false)) {
            Ok(edits) => {
                for edit in edits {
                    self.state.push_history(edit);
                }
                self.state.dirty = true;
                self.sync_sequence_to_engine();
            }
            Err(refusal) => {
                if let Some(message) = refusal.message() {
                    self.state.toast.show(message);
                }
            }
        }
    }
}

impl KlippApp {
    /// Detect Scene Cuts' end (#616): the clip split at `cut_times` (its
    /// file's seconds) as one undoable edit, its linked sound with it, and
    /// said how many. With none inside it, or a clip since changed so none
    /// fall inside, it is left as it is and that is said too.
    pub(super) fn split_at_scene_cuts(&mut self, clip_id: klipp_core::ClipId, cut_times: &[f64]) {
        let seq = self.state.sequence();
        let Some((name, media_fps)) = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == clip_id).and_then(|c| c.media_id()).and_then(|m| self.state.media_pool.get(&m)).map(|m| (m.filename.clone(), m.fps.as_f64())) else {
            self.state.toast.show("Scene cuts found, but the clip is gone".to_string());
            return;
        };
        let ops = klipp_document::ops::scene_cut_ops(seq, clip_id, cut_times, media_fps);
        if ops.is_empty() {
            self.state.toast.show(format!("No scene cuts found in {name}; it is unchanged"));
            return;
        }
        match klipp_document::ops::resolve(seq, &self.state.media_pool, &ops) {
            Ok(plan) => {
                let edits: Vec<EditCommand> = plan.steps.iter().map(|s| s.edit.clone()).collect();
                self.apply_edits_as_one(&edits, "Detect Scene Cuts");
                let n = ops.len();
                self.state.toast.show(format!("Split {name} at {n} scene cut{} into {} shots", if n == 1 { "" } else { "s" }, n + 1));
            }
            Err(r) => self.state.toast.show(format!("Scene cuts not applied: {}. {}", r.why, capitalise(&r.hint))),
        }
    }
}

/// Which edge `trim_to_playhead_dispatch` should move — Out for Q, In for W.
#[derive(Debug, Clone, Copy)]
pub(super) enum TrimToPlayheadDir {
    /// Q: trim the OUT edge of the clip ending at/before the playhead.
    Out,
    /// W: trim the IN edge of the clip starting at/after the playhead.
    In,
}

/// Compute the pixel offsets (in the project's working coordinate
/// space, where 0,0 is the frame centre) that place a 30 %-scaled
/// PiP clip in the named corner of the frame, with a 4 % margin.
/// Returns `(pos_x, pos_y)` ready to assign to a clip's transform
/// keyframes. Unknown corner strings fall through to (0, 0).
///
/// **Why resolution-aware**: the original PiP presets used hard-
/// coded offsets (±600, ±300) tuned for 1080p; on 720p the clip
/// pushed off-screen and on 4K it landed near the frame centre.
/// This formula keeps the visual result corner-anchored across
/// every project resolution.
///
/// **Y-sign convention**: Klipp's `transform.position_y` is
/// negative-up, positive-down (matches the existing `pip_top_right`
/// using `-300` for top and `pip_bottom_left` using `+300` for
/// bottom).
pub(super) fn pip_offsets_for_corner(
    res: klipp_core::Resolution,
    corner: &str,
) -> (f32, f32) {
    // Clip is 30 % scale; 4 % margin from frame edge:
    //   offset = frame_dim/2 - clip_dim/2 - margin
    //          = frame_dim * (0.5 - 0.15 - 0.04)
    //          = frame_dim * 0.31
    let offset_x = res.width as f32 * 0.31;
    let offset_y = res.height as f32 * 0.31;
    match corner {
        "pip_top_left"     => (-offset_x, -offset_y),
        "pip_top_right"    => ( offset_x, -offset_y),
        "pip_bottom_left"  => (-offset_x,  offset_y),
        "pip_bottom_right" => ( offset_x,  offset_y),
        _ => (0.0, 0.0),
    }
}
