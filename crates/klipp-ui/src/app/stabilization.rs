//! Stabilization: a clip's motion analysed and smoothed (#826).

use klipp_timeline::GradeAccess;
use super::*;

impl KlippApp {
    pub(super) fn handle_stabilization_commands(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        match cmd {
            AppCommand::CancelDeflicker { clip_id } => {
                // The worker checks the set each frame and stops.
                self.state.deflicker_in_progress.remove(clip_id);
                self.state.deflicker_progress.remove(clip_id);
                true
            }
            AppCommand::AnalyzeDeflicker { clip_id } => {
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                let found = seq
                    .tracks
                    .iter()
                    .flat_map(|t| t.clips.iter())
                    .find(|c| c.id == *clip_id)
                    .and_then(|c| {
                        let item = self.state.media_pool.get(&c.media_id()?)?;
                        let fps = item.fps.as_f64();
                        let frames = klipp_analysis::assets::frames_of(item.duration.to_secs_f64(), fps);
                        let key = klipp_analysis::assets::asset_key(klipp_analysis::assets::AssetKind::Deflicker, item);
                        Some((item.path.clone(), fps, frames, key))
                    });
                let Some((path, fps, frame_count, key)) = found else { return true };
                if frame_count < 2 {
                    self.state.toast.show("Deflicker needs a clip of at least two frames");
                    return true;
                }
                let clip_id = *clip_id;
                self.state.deflicker_in_progress.insert(clip_id);
                self.state.deflicker_progress.insert(clip_id, 0.0);
                let event_tx = self.proxy_event_tx.clone();
                std::thread::spawn(move || {
                    // The analyser an export runs too, kept as the file's
                    // asset (M4 step 9): every clip of it reads it, from
                    // here on and after a reopen, whichever made it; the
                    // renderer and the inspector find it there.
                    let Some(key) = key else {
                        let _ = event_tx.send(AppEvent::DeflickerFailed { clip_id, error: "the media's file isn't there to analyse".into() });
                        return;
                    };
                    let sent = std::cell::Cell::new(0.0f32);
                    let progress = |p: f32| {
                        // One event a percent, not one a frame: a
                        // 4-minute clip is 6000 frames.
                        if p - sent.get() >= 0.01 || p >= 1.0 {
                            sent.set(p);
                            let _ = event_tx.send(AppEvent::DeflickerProgress { clip_id, progress: p });
                        }
                        true
                    };
                    if let Err(error) = klipp_engine::analyse_into(&mut klipp_analysis::assets::CacheStore::default(), key, &path, fps, frame_count, &progress) {
                        let _ = event_tx.send(AppEvent::DeflickerFailed { clip_id, error });
                        return;
                    }
                    let _ = event_tx.send(AppEvent::DeflickerComplete { clip_id });
                });
                true
            }
            AppCommand::AnalyzeStabilization { clip_id } => {
                // Find clip -> get media_id -> get media path, fps, duration
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                let clip_info = seq.tracks.iter().flat_map(|t| t.clips.iter())
                    .find(|c| c.id == *clip_id)
                    .and_then(|c| {
                        let mid = c.media_id()?;
                        let item = self.state.media_pool.get(&mid)?;
                        let fps = item.fps.as_f64();
                        let dur_secs = item.duration.to_secs_f64();
                        let frame_count = klipp_analysis::assets::frames_of(dur_secs, fps);
                        let key = klipp_analysis::assets::asset_key(klipp_analysis::assets::AssetKind::Stabilisation, item);
                        Some((item.path.clone(), fps, frame_count, key))
                    });

                if let Some((path, fps, frame_count, key)) = clip_info {
                    let clip_id = *clip_id;
                    self.state.stabilization_in_progress.insert(clip_id);
                    self.state.stabilization_progress.insert(clip_id, 0.0);
                    let event_tx = self.proxy_event_tx.clone();

                    std::thread::spawn(move || {
                        // The analyser an export runs too, kept as the
                        // file's asset (M4 step 9): the renderer and the
                        // inspector find it there, smoothed as each effect
                        // is set, whichever made it.
                        let Some(key) = key else {
                            let _ = event_tx.send(AppEvent::StabilizationFailed { clip_id, error: "the media's file isn't there to analyse".into() });
                            return;
                        };
                        let sent = std::cell::Cell::new(0.0f32);
                        let progress = |p: f32| {
                            // One event a percent, not one a frame.
                            if p - sent.get() >= 0.01 || p >= 1.0 {
                                sent.set(p);
                                let _ = event_tx.send(AppEvent::StabilizationProgress { clip_id, progress: p });
                            }
                            true
                        };
                        match klipp_engine::analyse_into(&mut klipp_analysis::assets::CacheStore::default(), key, &path, fps, frame_count, &progress) {
                            Ok(_) => {
                                let _ = event_tx.send(AppEvent::StabilizationComplete { clip_id });
                            }
                            Err(error) => {
                                let _ = event_tx.send(AppEvent::StabilizationFailed { clip_id, error });
                            }
                        }
                    });
                }
                true
            }
            AppCommand::CancelStabilization { clip_id } => {
                // Currently no cancel flag — just remove from in-progress
                self.state.stabilization_in_progress.remove(clip_id);
                self.state.stabilization_progress.remove(clip_id);
                true
            }
            AppCommand::DetectScenes { clip_id } => {
                // Pipelined decode + histogram analysis — same shape as
                // AnalyzeStabilization, but calling scene_detect::detect_scenes
                // on the consumer side and emitting cut times.
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                let clip_info = seq.tracks.iter().flat_map(|t| t.clips.iter())
                    .find(|c| c.id == *clip_id)
                    .and_then(|c| {
                        let mid = c.media_id()?;
                        let item = self.state.media_pool.get(&mid)?;
                        let fps = item.fps.as_f64();
                        let dur_secs = item.duration.to_secs_f64();
                        let frame_count = (dur_secs * fps).round() as usize;
                        let source_in_secs = c.source_in.to_secs_f64();
                        let source_out_secs = c.source_out.to_secs_f64();
                        Some((item.path.clone(), fps, frame_count, source_in_secs, source_out_secs))
                    });

                if let Some((path, fps, frame_count, source_in_secs, source_out_secs)) = clip_info {
                    let clip_id = *clip_id;
                    self.state.scene_detect_in_progress.insert(clip_id);
                    self.state.scene_detect_progress.insert(clip_id, 0.0);
                    // Cancel used to forget the run in the UI and leave its
                    // thread decoding to the end; this stops it (#616).
                    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                    self.state.scene_detect_cancel.insert(clip_id, std::sync::Arc::clone(&cancel));
                    let event_tx = self.proxy_event_tx.clone();
                    let sensitivity = self.prefs.editing.scene_detect_sensitivity;
                    let min_gap = self.prefs.editing.scene_detect_min_gap_secs;
                    self.state.toast.show(format!(
                        "Analysing scenes\u{2026} ({} frames)", frame_count
                    ));
                    tracing::info!(
                        "Scene detect: starting clip={:?} fps={} frames={} sens={} gap={}s src=[{:.3},{:.3})",
                        clip_id, fps, frame_count, sensitivity, min_gap, source_in_secs, source_out_secs
                    );

                    std::thread::spawn(move || {
                        let t0 = std::time::Instant::now();
                        let (frame_tx, frame_rx) = std::sync::mpsc::sync_channel::<(Vec<u8>, u32, u32)>(3);
                        let path_for_producer = path.clone();
                        let event_tx_open = event_tx.clone();
                        // A walk that stops on an error must not come back
                        // as the cuts of a shorter clip.
                        let decode_error = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
                        let decode_error_producer = std::sync::Arc::clone(&decode_error);
                        let producer = std::thread::Builder::new()
                            .name("klipp-scene-detect".to_string())
                            .spawn(move || {
                                let mut walker = match klipp_engine::SequentialFrameDecoder::open(&path_for_producer, 240) {
                                    Ok(w) => w,
                                    Err(e) => {
                                        let _ = event_tx_open.send(AppEvent::SceneDetectFailed { clip_id, error: format!("decoder open: {e}") });
                                        return;
                                    }
                                };
                                loop {
                                    match walker.next_frame() {
                                        Ok(Some(f)) => {
                                            if frame_tx.send(f).is_err() {
                                                break;
                                            }
                                        }
                                        Ok(None) => break,
                                        Err(e) => {
                                            *decode_error_producer.lock().unwrap_or_else(|p| p.into_inner()) = Some(e);
                                            break;
                                        }
                                    }
                                }
                            });
                        let producer = match producer {
                            Ok(h) => h,
                            Err(e) => {
                                let _ = event_tx.send(AppEvent::SceneDetectFailed { clip_id, error: format!("spawn producer: {e}") });
                                return;
                            }
                        };

                        let cuts = klipp_effects::scene_detect::detect_scenes(
                            fps,
                            sensitivity,
                            min_gap,
                            source_in_secs,
                            source_out_secs,
                            frame_count,
                            &mut || frame_rx.recv().ok(),
                            &|p| !cancel.load(std::sync::atomic::Ordering::Relaxed) && event_tx.send(AppEvent::SceneDetectProgress { clip_id, progress: p }).is_ok(),
                        );
                        drop(frame_rx);
                        let _ = producer.join();
                        if let Some(error) = decode_error.lock().unwrap_or_else(|p| p.into_inner()).take() {
                            tracing::warn!("Scene detect: clip={:?} stopped: {error}", clip_id);
                            let _ = event_tx.send(AppEvent::SceneDetectFailed { clip_id, error });
                            return;
                        }
                        tracing::info!(
                            "Scene detect: finished clip={:?} cuts={} elapsed={:.2}s",
                            clip_id, cuts.len(), t0.elapsed().as_secs_f64()
                        );
                        let _ = event_tx.send(AppEvent::SceneDetectComplete { clip_id, cut_times: cuts });
                    });
                }
                true
            }
            AppCommand::CancelSceneDetect { clip_id } => {
                // The thread reads the flag at its next frame and stops; what
                // it found by then is dropped, as the run is no longer in
                // progress when its result arrives.
                if let Some(flag) = self.state.scene_detect_cancel.remove(clip_id) {
                    flag.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                self.state.scene_detect_in_progress.remove(clip_id);
                self.state.scene_detect_progress.remove(clip_id);
                self.state.scene_split_pending.remove(clip_id);
                self.state.toast.show("Scene detection cancelled; the clip is unchanged".to_string());
                true
            }
            AppCommand::DetectSceneCuts { clip_id } => {
                if let Some(why) = self.state.scene_cut_refusal(*clip_id) {
                    self.state.toast.show(format!("No scene cuts to find: {why}"));
                    return true;
                }
                if self.state.scene_detect_in_progress.contains(clip_id) {
                    self.state.toast.show("Scenes are already being found in this clip".to_string());
                    return true;
                }
                // The detection Detect Scenes runs; on finishing, a split
                // where it would have left markers.
                self.state.scene_split_pending.insert(*clip_id);
                self.send_commands(vec![AppCommand::DetectScenes { clip_id: *clip_id }]);
                true
            }
            AppCommand::JoinThroughEdit { clip_id, next } => {
                let side = if *next { klipp_document::ops::JoinSide::Next } else { klipp_document::ops::JoinSide::Previous };
                let op = klipp_document::ops::Op::Join { clip_id: klipp_document::ops::ClipRef::Id(*clip_id), side };
                match klipp_document::ops::resolve(self.state.sequence(), &self.state.media_pool, std::slice::from_ref(&op)) {
                    Ok(plan) => {
                        let edits: Vec<EditCommand> = plan.steps.iter().map(|s| s.edit.clone()).collect();
                        self.apply_edits_as_one(&edits, "Join Through Edit");
                    }
                    Err(r) => self.state.toast.show(format!("Nothing to join: {}. {}", r.why, capitalise(&r.hint))),
                }
                true
            }
            AppCommand::SplitAtSceneMarkers { clip_id } => {
                // Razor the clip at every scene marker it carries. Pushes a
                // single compound history entry so Ctrl+Z undoes the whole
                // split at once.
                let cid = *clip_id;
                let active = self.state.active_sequence_idx;
                // Find the track index holding the clip.
                let track_idx = match self.state.sequences[active].tracks.iter()
                    .position(|t| t.find_clip(cid).is_some())
                {
                    Some(i) => i,
                    None => { return true; }
                };
                if self.state.sequences[active].tracks[track_idx].locked { return true; }
                // Snapshot the original clip + its source fps so we can
                // convert marker times (clip-relative seconds) into timeline
                // RationalTimes at the correct rate.
                let (original, position, marker_times_secs, source_in_secs, fps) = {
                    let track = &self.state.sequences[active].tracks[track_idx];
                    let clip = match track.find_clip(cid) {
                        Some(c) => c.clone(),
                        None => { return true; }
                    };
                    let pos = match track.clips.iter().position(|c| c.id == cid) {
                        Some(p) => p,
                        None => { return true; }
                    };
                    let mut times: Vec<f64> = clip.clip_markers.iter()
                        .map(|m| m.time_secs).collect();
                    times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                    let fps = clip.timeline_in.rate.max(1);
                    let source_in_secs = clip.source_in.to_secs_f64();
                    (clip, pos, times, source_in_secs, fps)
                };
                let _ = source_in_secs; // marker.time_secs is already clip-relative
                if marker_times_secs.is_empty() {
                    self.state.toast.show("No scene markers on this clip — run Detect Scenes first".to_string());
                    return true;
                }
                // Convert clip-relative marker seconds → absolute timeline times
                // at the clip's rate. Guard against markers too close to clip
                // edges or to each other (< 1 frame apart).
                let clip_dur_secs = original.duration().to_secs_f64();
                let min_piece_secs = 1.0 / fps as f64;
                let timeline_in_secs = original.timeline_in.to_secs_f64();
                let mut cuts: Vec<klipp_core::RationalTime> = Vec::new();
                let mut last_secs = 0.0f64;
                for t in &marker_times_secs {
                    if *t <= min_piece_secs || *t >= (clip_dur_secs - min_piece_secs) { continue; }
                    if (t - last_secs) < min_piece_secs { continue; }
                    last_secs = *t;
                    let abs_secs = timeline_in_secs + *t;
                    cuts.push(klipp_core::RationalTime::from_secs_f64(abs_secs, fps));
                }
                if cuts.is_empty() {
                    self.state.toast.show("Scene markers are too close to clip edges to split".to_string());
                    return true;
                }

                // Progressive blade: each blade replaces the current right-hand
                // piece with two new pieces. Collect all resulting clips in
                // left-to-right order for the compound history entry.
                let mut new_clips: Vec<klipp_timeline::Clip> = Vec::new();
                let mut current_id = cid;
                {
                    let seq_fps = self.state.sequences[active].fps;
                    let track = &mut self.state.sequences[active].tracks[track_idx];
                    for cut in &cuts {
                        match track.blade_at(current_id, *cut, seq_fps) {
                            Some((clip_a, clip_b)) => {
                                new_clips.push(clip_a);
                                current_id = clip_b.id;
                            }
                            None => {
                                tracing::warn!("SplitAtSceneMarkers: blade_at returned None at cut {:?} on clip {:?}", cut, current_id);
                                break;
                            }
                        }
                    }
                    // Capture the final right-hand piece.
                    if let Some(final_clip) = track.find_clip(current_id).cloned() {
                        new_clips.push(final_clip);
                    }
                    // Scene markers on the split pieces would now sit exactly
                    // at the clip boundaries (each cut consumed its marker) —
                    // clear them so the result looks clean. Markers on the
                    // original (pre-split) clip are preserved in the history
                    // entry for undo.
                    for c in track.clips.iter_mut() {
                        if new_clips.iter().any(|nc| nc.id == c.id) {
                            c.clip_markers.clear();
                        }
                    }
                    for c in new_clips.iter_mut() {
                        c.clip_markers.clear();
                    }
                }

                if new_clips.len() < 2 {
                    // Nothing meaningfully changed — rollback by restoring original.
                    let track = &mut self.state.sequences[active].tracks[track_idx];
                    track.clips.retain(|c| c.id != current_id);
                    track.clips.insert(position.min(track.clips.len()), original.clone());
                    track.sort_clips();
                    self.state.toast.show("Split failed — markers did not produce valid pieces".to_string());
                    return true;
                }

                let n_pieces = new_clips.len();
                self.state.push_history(EditCommand::LegacySplitAtSceneMarkers {
                        track_index: track_idx,
                        original_clip: original,
                        position,
                        new_clips,
                    });
                self.sync_sequence_to_engine();
                self.state.dirty = true;
                self.state.toast.show(format!(
                    "Split clip into {} pieces at {} scene marker{}",
                    n_pieces, cuts.len(),
                    if cuts.len() == 1 { "" } else { "s" }
                ));
                true
            }
            AppCommand::ToggleTrackingMode => {
                self.state.tracking_mode = !self.state.tracking_mode;
                true
            }
            AppCommand::TrackSurface { clip_id, corners, at_secs, name, surface, direction, interactive, track, exclude } => {
                let clip_id = *clip_id;
                if !klipp_timeline::surface::quad_is_usable(corners) {
                    self.state.toast.show("The surface's corners need to enclose an area");
                    return true;
                }
                if self.state.tracking_in_progress.contains(&clip_id) {
                    self.state.toast.show("This clip is already being tracked");
                    return true;
                }
                let at = at_secs.unwrap_or(self.state.playhead_smooth_secs);
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                let frame = [seq.resolution.width, seq.resolution.height];
                let fps = seq.fps.as_f64();
                let Some(clip) = seq.all_clips().find(|c| c.id == clip_id) else { return true };
                let Some(item) = clip.media_id().and_then(|m| self.state.media_pool.get(&m)) else {
                    self.state.toast.show("Only a clip from a file can be tracked");
                    return true;
                };
                let media_fps = item.fps.as_f64().max(1.0);
                let path = item.path.clone();
                let frame_count = (item.duration.to_secs_f64() * media_fps).round() as usize;
                let clip_time = (at - clip.timeline_in.to_secs_f64()).clamp(0.0, (clip.timeline_out - clip.timeline_in).to_secs_f64());
                let first = (clip.source_in.to_secs_f64() * media_fps).floor() as usize;
                let last = ((clip.source_out.to_secs_f64() * media_fps).ceil() as usize).min(frame_count.saturating_sub(1)).max(first);
                let reference = ((clip.source_secs_at(clip_time, fps) * media_fps).round() as usize).clamp(first, last);
                let existing = surface.and_then(|id| clip.surfaces.iter().find(|s| s.id == id));
                if *direction != 0 && existing.is_none() {
                    self.state.toast.show("A track continues only on a surface the clip already has");
                    return true;
                }
                let id = surface.unwrap_or_else(uuid::Uuid::new_v4);
                let name = name.clone()
                    .or_else(|| existing.map(|s| s.name.clone()))
                    .unwrap_or_else(|| format!("Surface {}", clip.surfaces.len() + 1));
                let (w, h) = (item.resolution.width.max(1) as f64, item.resolution.height.max(1) as f64);
                let turned = item.source_rotation.rem_euclid(180) == 90;
                // The corners come as the frame shows the clip: back into its
                // own picture, as the tracker reads it.
                let shown = if turned { [h as u32, w as u32] } else { [w as u32, h as u32] };
                let when = clip.timeline_in + klipp_core::Timecode::from_secs_f64(clip_time, clip.timeline_in.rate.max(1));
                let placed = klipp_compose::eval::evaluate_clip_own_transform(clip, clip_time, &klipp_compose::eval::eval_context(clip, when, fps));
                let on_source = klipp_timeline::surface::frame_quad_in_source(corners, shown, frame, Some(placed));
                if !klipp_timeline::surface::quad_is_usable(&on_source) {
                    self.state.toast.show("The surface's corners need to be on the clip's picture");
                    return true;
                }
                // The area followed and the parts left out, into the clip's
                // own picture; or the surface's own, carried to this frame.
                let carried = existing.and_then(|s| s.map_shown_at(clip.source_secs_at(clip_time, fps)).map(|m| (s, m)));
                let track_on_source = match track {
                    Some(t) => Some(klipp_timeline::surface::frame_quad_in_source(t, shown, frame, Some(placed))),
                    None => carried.and_then(|(s, m)| s.track_area.map(|a| klipp_timeline::surface::mapped(&m, &a))),
                };
                if track_on_source.is_some_and(|t| !klipp_timeline::surface::quad_is_usable(&t)) {
                    self.state.toast.show("The area to follow needs to enclose an area on the clip's picture");
                    return true;
                }
                let exclude_on_source: Vec<Vec<[f32; 2]>> = match exclude {
                    Some(given) => given.iter().map(|e| klipp_timeline::surface::frame_points_in_source(e, shown, frame, Some(placed))).collect(),
                    None => carried.map(|(s, m)| s.exclude.iter().map(|e| e.iter().map(|p| klipp_timeline::surface::mapped_point(&m, *p)).collect()).collect()).unwrap_or_default(),
                };
                // The decoder bounds the picture's width as shown, which for
                // a turned phone shot is the file's height.
                let shown_w = if turned { h } else { w };
                let scale = (SURFACE_TRACK_SIDE as f64 / shown_w).min(1.0);
                let pixels = ((w * scale) * (h * scale)) as usize;
                let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                self.state.surface_track_cancel.insert(clip_id, cancel.clone());
                self.state.surface_tool = None;
                self.state.toast.show(match direction {
                    0 => format!("Tracking the surface \u{201c}{name}\u{201d}\u{2026}"),
                    _ => format!("Continuing the track of \u{201c}{name}\u{201d}\u{2026}"),
                });
                self.start_surface_track(SurfaceTrackJob { clip_id, surface: id, name, path, media_fps, frames: (first, last), reference, corners: on_source, track: track_on_source, exclude: exclude_on_source, pixels, direction: *direction, interactive: *interactive, cancel });
                true
            }
            AppCommand::OpenSurfaceTool { clip_id, surface } => {
                self.open_surface_tool(*clip_id, *surface);
                true
            }
            AppCommand::SnapSurface { clip_id, click, at_secs, then_track, name } => {
                self.start_surface_snap(*clip_id, *click, *at_secs, *then_track, name.clone());
                true
            }
            AppCommand::RemoveSurface { clip_id, surface } => {
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                let Some(old) = seq.all_clips().find(|c| c.id == *clip_id).cloned() else { return true };
                let mut new = old.clone();
                new.surfaces.retain(|s| s.id != *surface);
                if new.surfaces.len() != old.surfaces.len() {
                    let edit = EditCommand::ReplaceClip { description: "Remove Surface", old: Box::new(old), new: Box::new(new) };
                    self.apply_edits_as_one(std::slice::from_ref(&edit), "Remove Surface");
                }
                true
            }
            AppCommand::TrackMaskRegion { clip_id, node_id, mask_index, perspective } => {
                // The window as drawn at the playhead, followed through the
                // clip in move, size and turn (#238): a track point holds
                // the result, linked to the mask, and the TrackingComplete
                // handler writes it onto the window as keyframes.
                let clip_id = *clip_id;
                let node_id = *node_id;
                let mask_index = *mask_index;
                let perspective = *perspective;
                let playhead_secs = self.state.playhead_smooth_secs;
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                let aspect = seq.resolution.height as f32 / seq.resolution.width.max(1) as f32;
                let Some(clip) = seq.all_clips().find(|c| c.id == clip_id) else { return true };
                let Some(item) = clip.media_id().and_then(|m| self.state.media_pool.get(&m)) else {
                    self.state.toast.show("Only a clip from a file can be tracked");
                    return true;
                };
                let media_fps = item.fps.as_f64().max(1.0);
                let path = item.path.clone();
                let frame_count = (item.duration.to_secs_f64() * media_fps).round() as usize;
                // The grade's clock is the clip's own, from its first frame
                // on the timeline; the tracker's is the file's frames.
                let clip_time = (playhead_secs - clip.timeline_in.to_secs_f64()).max(0.0);
                // Through its speed and in its direction (#617).
                let source_secs = clip.source_secs_at(clip_time, seq.fps.as_f64());
                let reference = (source_secs * media_fps).round() as usize;
                let first = (clip.source_in.to_secs_f64() * media_fps).floor() as usize;
                let last = ((clip.source_out.to_secs_f64() * media_fps).ceil() as usize).min(frame_count.saturating_sub(1)).max(first);
                let window = clip.grade().nodes.iter()
                    .find(|n| n.id == node_id)
                    .and_then(|n| n.params.masks.as_ref())
                    .and_then(|ms| ms.masks.get(mask_index))
                    .map(|m| m.evaluated_at(clip_time).shape);
                let Some(shape) = window else {
                    self.state.toast.show("Couldn't read the window");
                    return true;
                };
                let quad = window_quad(&shape, aspect);
                let pivot = shape.pivot();
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                let Some(clip) = seq.all_clips_mut().find(|c| c.id == clip_id) else { return true };
                let index = clip.track_points.len();
                clip.track_points.push(klipp_core::TrackPoint {
                    name: format!("Mask {} tracker", mask_index + 1),
                    start_frame: reference,
                    positions: vec![(pivot.0 as f64, pivot.1 as f64)],
                    linked_mask: Some((node_id, mask_index)),
                    reference_frame: Some(reference),
                    ..Default::default()
                });
                self.state.active_track_point = Some(index);
                self.state.pending_mask_track_link.insert(
                    (clip_id, index),
                    klipp_core::TrackTarget::MaskCenter { node_id, mask_index },
                );
                self.state.toast.show("Tracking the window\u{2026}");
                self.start_window_track(WindowTrackJob { clip_id, point_index: index, path, media_fps, frames: (first, last), reference, quad, pivot, perspective });
                self.state.dirty = true;
                true
            }
            AppCommand::AddTrackPoint { clip_id, x, y } => {
                add_track_point(&mut self.state, *clip_id, *x, *y);
                true
            }
            AppCommand::RemoveTrackPoint { clip_id, index } => {
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                if let Some(clip) = seq.all_clips_mut().find(|c| c.id == *clip_id) {
                    if *index < clip.track_points.len() {
                        clip.track_points.remove(*index);
                    }
                }
                if self.state.active_track_point == Some(*index) {
                    self.state.active_track_point = None;
                }
                self.state.dirty = true;
                true
            }
            AppCommand::CancelTrackAnalysis { clip_id } => {
                self.state.tracking_in_progress.remove(clip_id);
                self.state.tracking_progress.remove(clip_id);
                // A surface track stops where it is, and its result is dropped.
                if let Some(cancel) = self.state.surface_track_cancel.remove(clip_id) {
                    cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                true
            }
            AppCommand::AnalyzeTrackPoint { clip_id, point_index } => {
                let clip_id = *clip_id;
                let point_index = *point_index;
                // Get clip info for tracking
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                let clip_info = seq.tracks.iter().flat_map(|t| t.clips.iter())
                    .find(|c| c.id == clip_id)
                    .and_then(|c| {
                        let mid = c.media_id()?;
                        let item = self.state.media_pool.get(&mid)?;
                        let fps = item.fps.as_f64();
                        let dur_secs = item.duration.to_secs_f64();
                        let frame_count = (dur_secs * fps).round() as usize;
                        let tp = c.track_points.get(point_index)?;
                        let start_frame = tp.start_frame;
                        let initial_pos = tp.positions.first().copied()?;
                        let search_radius = tp.search_radius;
                        Some((item.path.clone(), fps, frame_count, start_frame, initial_pos, search_radius))
                    });

                if let Some((path, fps, frame_count, ref_frame, initial_pos_norm, search_radius)) = clip_info {
                    self.state.tracking_in_progress.insert(clip_id);
                    self.state.tracking_progress.insert(clip_id, 0.0);
                    let event_tx = self.proxy_event_tx.clone();
                    let pref_patch_size = self.prefs.editing.track_patch_size;
                    let pref_loss_threshold = self.prefs.editing.track_loss_threshold;

                    std::thread::spawn(move || {
                        // Decode a frame to get dimensions for coordinate conversion
                        let first_frame = klipp_engine::generate_thumbnail_at_size(&path, 0.0, 480);
                        let (sample_w, sample_h) = match &first_frame {
                            Ok((_, w, h)) => (*w, *h),
                            Err(_) => (960, 540),
                        };
                        // Convert normalized pos to half-res pixel coordinates
                        let half_w = sample_w / 2;
                        let half_h = sample_h / 2;
                        let px = initial_pos_norm.0 * half_w as f64;
                        let py = initial_pos_norm.1 * half_h as f64;

                        // Track forward from reference frame
                        let forward_result = klipp_effects::stabilization::track_point(
                            (px, py),
                            ref_frame,
                            fps,
                            frame_count,
                            &mut |time_secs| {
                                klipp_engine::generate_thumbnail_at_size(&path, time_secs, 480).ok()
                            },
                            search_radius / 2, // half-res
                            pref_patch_size, // patch_half (from prefs)
                            pref_loss_threshold, // loss threshold (from prefs)
                            true,
                            &|p| {
                                let _ = event_tx.send(AppEvent::TrackingProgress { clip_id, progress: p * 0.5 });
                                true
                            },
                        );

                        // Track backward from reference frame
                        let backward_result = klipp_effects::stabilization::track_point(
                            (px, py),
                            ref_frame,
                            fps,
                            frame_count,
                            &mut |time_secs| {
                                klipp_engine::generate_thumbnail_at_size(&path, time_secs, 480).ok()
                            },
                            search_radius / 2,
                            pref_patch_size,
                            pref_loss_threshold,
                            false,
                            &|p| {
                                let _ = event_tx.send(AppEvent::TrackingProgress { clip_id, progress: 0.5 + p * 0.5 });
                                true
                            },
                        );

                        // Merge results: backward (reversed) + forward
                        let mut all_positions: Vec<(usize, f64, f64)> = Vec::new();
                        if let Ok(mut bwd) = backward_result {
                            bwd.reverse();
                            all_positions.extend(bwd);
                        }
                        if let Ok(fwd) = forward_result {
                            // Skip the reference frame (already in backward results)
                            for entry in fwd.into_iter().skip(1) {
                                all_positions.push(entry);
                            }
                        }

                        // Sort by frame index
                        all_positions.sort_by_key(|&(idx, _, _)| idx);

                        // Convert back to normalized coordinates
                        let normalized: Vec<(f64, f64)> = all_positions.iter()
                            .map(|&(_, x, y)| (x / half_w as f64, y / half_h as f64))
                            .collect();

                        let start_frame = all_positions.first().map(|&(idx, _, _)| idx).unwrap_or(ref_frame);

                        let _ = event_tx.send(AppEvent::TrackingComplete {
                            clip_id,
                            point_index,
                            positions: normalized,
                            start_frame,
                            motion: Vec::new(),
                            corners: Vec::new(),
                        });
                    });
                }
                true
            }
            AppCommand::GenerateKeyframesFromTrack { clip_id, point_index, target } => {
                let clip_id = *clip_id;
                let point_index = *point_index;
                let target = target.clone();
                let fps = self.state.fps.as_f64();
                // A window track's edit, pushed and synced once the
                // sequence is let go.
                let mut window_edit: Option<(EditCommand, usize)> = None;
                // The position lanes a transform target lays down.
                let mut position_lanes: Option<Vec<(&str, Vec<klipp_core::Keyframe>)>> = None;

                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                if let Some(clip) = seq.all_clips_mut().find(|c| c.id == clip_id) {
                    if let Some(tp) = clip.track_points.get(point_index) {
                        let positions = tp.positions.clone();
                        let start_frame = tp.start_frame;

                        match target {
                            klipp_core::TrackTarget::TransformPosition => {
                                // The position lanes, laid by path once the
                                // sequence is let go (M4 step 12c).
                                let key = |t: f64, value: f32| klipp_core::Keyframe { time_secs: t, value, interpolation: klipp_core::Interpolation::Linear, tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0), handles: Default::default() };
                                let at = |i: usize| (start_frame + i) as f64 / fps;
                                position_lanes = Some(vec![
                                    ("pos_x", positions.iter().enumerate().map(|(i, &(nx, _))| key(at(i), (nx - 0.5) as f32)).collect()),
                                    ("pos_y", positions.iter().enumerate().map(|(i, &(_, ny))| key(at(i), (ny - 0.5) as f32)).collect()),
                                ]);
                            }
                            klipp_core::TrackTarget::MaskCenter { node_id, mask_index } if !tp.corners.is_empty() => {
                                // A window track in perspective (#238): the
                                // corners' course on the window, one undo
                                // step.
                                let track = window_perspective_track(clip, tp, &self.state.media_pool, fps);
                                let count = track.as_ref().map_or(0, |t| t.keys.len());
                                let old_graph = clip.grade().clone();
                                clip.edit_grade(|graph| {
                                    if let Some(mask) = graph.nodes.iter_mut().find(|n| n.id == node_id)
                                        .and_then(|n| n.params.masks.as_mut())
                                        .and_then(|ms| ms.masks.get_mut(mask_index))
                                    {
                                        mask.keyframes.clear_track();
                                        mask.keyframes.track_perspective = track.filter(|t| !t.keys.is_empty());
                                    }
                                });
                                let new_graph = clip.grade().clone();
                                if let Some(tp) = clip.track_points.get_mut(point_index) {
                                    tp.linked_mask = Some((node_id, mask_index));
                                }
                                window_edit = Some((EditCommand::SetNodeGraph { clip_id, old_graph, new_graph }, count));
                            }
                            klipp_core::TrackTarget::MaskCenter { node_id, mask_index } if !tp.motion.is_empty() => {
                                // A window track (#238): the window's move,
                                // size and turn, as keyframes on its own
                                // clock, one undo step.
                                let keys = window_track_keys(clip, tp, &self.state.media_pool, fps);
                                let old_graph = clip.grade().clone();
                                clip.edit_grade(|graph| {
                                    if let Some(mask) = graph.nodes.iter_mut().find(|n| n.id == node_id)
                                        .and_then(|n| n.params.masks.as_mut())
                                        .and_then(|ms| ms.masks.get_mut(mask_index))
                                    {
                                        mask.keyframes.clear_track();
                                        let k = |t: f64, v: f32| klipp_core::Keyframe { time_secs: t, value: v, interpolation: klipp_core::Interpolation::Linear, tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0), handles: Default::default() };
                                        for &(t, dx, dy, scale, rot) in &keys {
                                            mask.keyframes.track_x.push(k(t, dx));
                                            mask.keyframes.track_y.push(k(t, dy));
                                            mask.keyframes.track_scale.push(k(t, scale));
                                            mask.keyframes.track_rotation.push(k(t, rot));
                                        }
                                    }
                                });
                                let new_graph = clip.grade().clone();
                                if let Some(tp) = clip.track_points.get_mut(point_index) {
                                    tp.linked_mask = Some((node_id, mask_index));
                                }
                                window_edit = Some((EditCommand::SetNodeGraph { clip_id, old_graph, new_graph }, keys.len()));
                            }
                            klipp_core::TrackTarget::MaskCenter { node_id, mask_index } => {
                                if let Some(tp) = clip.track_points.get_mut(point_index) {
                                    tp.linked_mask = Some((node_id, mask_index));
                                }
                                clip.edit_grade(|graph| {
                                    if let Some(node) = graph.nodes.iter_mut().find(|n| n.id == node_id) {
                                        if let Some(mask_stack) = &mut node.params.masks {
                                            if let Some(mask) = mask_stack.masks.get_mut(mask_index) {
                                                mask.keyframes.center_x.clear();
                                                mask.keyframes.center_y.clear();
                                                for (i, &(nx, ny)) in positions.iter().enumerate() {
                                                    let frame_idx = start_frame + i;
                                                    let t = frame_idx as f64 / fps;
                                                    mask.keyframes.center_x.push(klipp_core::Keyframe {
                                                        time_secs: t,
                                                        value: nx as f32,
                                                        interpolation: klipp_core::Interpolation::Linear,
                                                        tangent_in: (0.0, 0.0),
                                                        tangent_out: (0.0, 0.0),
                                                        handles: Default::default(),
                                                    });
                                                    mask.keyframes.center_y.push(klipp_core::Keyframe {
                                                        time_secs: t,
                                                        value: ny as f32,
                                                        interpolation: klipp_core::Interpolation::Linear,
                                                        tangent_in: (0.0, 0.0),
                                                        tangent_out: (0.0, 0.0),
                                                        handles: Default::default(),
                                                    });
                                                }
                                                self.state.toast.show("Generated mask center keyframes from tracker");
                                            }
                                        }
                                    }
                                });
                            }
                        }
                    }
                }
                if let Some(lanes) = position_lanes {
                    match self.state.set_lanes(clip_id, &lanes, "Keyframes From Tracker") {
                        Ok(()) => {
                            self.sync_sequence_to_engine();
                            self.state.toast.show("Generated transform keyframes from tracker");
                        }
                        Err(why) => self.state.toast.show(why),
                    }
                }
                if let Some((edit, frames)) = window_edit {
                    if let EditCommand::SetNodeGraph { old_graph, new_graph, .. } = &edit {
                        if old_graph != new_graph {
                            self.state.push_history(edit.clone());
                        }
                    }
                    self.sync_sequence_to_engine();
                    self.state.toast.show(format!("The window follows the picture over {frames} frames"));
                }
                self.state.dirty = true;
                true
            }
            _ => false,
        }
    }

    /// Starts the analyses the active sequence's effects need and haven't
    /// got (M4 step 9): a Deflicker, Stabilisation or Rolling Shutter
    /// effect in a clip's picture whose file has no asset of its kind in
    /// the analysis cache is analysed by itself, once a file, with no
    /// button to press. A found asset is kept in `analyses`, by its key,
    /// for the inspector. Looks every two seconds at most, at once after
    /// an analysis completes, and soon after any edit of the sequence,
    /// whichever path made it, with a frame asked for to look on.
    pub(super) fn ensure_analyses(&mut self, ctx: &egui::Context) {
        use klipp_analysis::assets::{self, AssetKind};
        match self.state.analysis_due(self.sequence.revision(), std::time::Instant::now()) {
            crate::state::AnalysisDue::Now => {}
            crate::state::AnalysisDue::After(wait) => {
                ctx.request_repaint_after(wait);
                return;
            }
            crate::state::AnalysisDue::NotYet => return,
        }
        let uses = analysis_uses(&mut self.state);
        let cache = assets::CacheStore::default();
        let keys: Vec<(assets::AssetKey, bool)> = uses.iter().map(|u| (u.key.clone(), !u.running)).collect();
        let missing = self.state.keep_analyses(&cache, &keys);
        for crate::state::AnalysisUse { key, kind, clip: clip_id, .. } in self.state.analyses_to_start(&uses, &missing, std::time::Instant::now()) {
            let cmd = match kind {
                AssetKind::Deflicker => AppCommand::AnalyzeDeflicker { clip_id },
                AssetKind::Stabilisation => AppCommand::AnalyzeStabilization { clip_id },
                AssetKind::RollingShutter => {
                    // No progress of its own to show: the asset is all, and
                    // the next look finds it.
                    let Some(item) = self.state.sequences.get(self.state.active_sequence_idx).and_then(|s| s.all_clips().find(|c| c.id == clip_id)).and_then(|c| c.media_id()).and_then(|m| self.state.media_pool.get(&m)) else { continue };
                    let (path, fps) = (item.path.clone(), item.fps.as_f64());
                    let frames = assets::frames_of(item.duration.to_secs_f64(), fps);
                    let ended = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                    self.state.analysis_threads.insert(key.clone(), ended.clone());
                    std::thread::spawn(move || {
                        if let Err(e) = klipp_engine::analyse_into(&mut assets::CacheStore::default(), key, &path, fps, frames, &|_| true) {
                            tracing::warn!("rolling-shutter analysis of {}: {e}", path.display());
                        }
                        ended.store(true, std::sync::atomic::Ordering::SeqCst);
                    });
                    continue;
                }
            };
            let mut refresh = false;
            self.handle_stabilization_commands(&cmd, &mut refresh);
        }
    }
}

/// Each analysis the active sequence's clips read, once a key: every
/// enabled analysis effect in a clip's picture (what the renderer draws),
/// with whether a run of it is making it now. A clip whose media has no
/// original to analyse is noted in `analysis_unavailable` instead.
pub(super) fn analysis_uses(state: &mut crate::state::AppState) -> Vec<crate::state::AnalysisUse> {
    use klipp_analysis::assets::{self, AssetKind};
    let mut uses: Vec<crate::state::AnalysisUse> = Vec::new();
    let Some(seq) = state.sequences.get(state.active_sequence_idx) else { return uses };
    for clip in seq.tracks.iter().flat_map(|t| t.clips.iter()) {
        let effects = match clip.picture_effects() {
            Ok(effects) => effects,
            Err(e) => {
                tracing::warn!("clip {}: its effects aren't read for analysis: {e}", clip.id.0);
                continue;
            }
        };
        for effect in effects.iter().filter(|e| e.enabled) {
            let Some(kind) = klipp_compose::render::asset_kind(&effect.effect) else { continue };
            let Some(item) = clip.media_id().and_then(|m| state.media_pool.get(&m)) else { continue };
            // No key without the original: the inspector says so.
            let Some(key) = assets::asset_key(kind, item) else {
                state.analysis_unavailable.insert((clip.id, kind));
                continue;
            };
            state.analysis_unavailable.remove(&(clip.id, kind));
            let running = match kind {
                AssetKind::Deflicker => state.deflicker_in_progress.contains(&clip.id),
                AssetKind::Stabilisation => state.stabilization_in_progress.contains(&clip.id),
                AssetKind::RollingShutter => state.analysis_threads.get(&key).is_some_and(|ended| !ended.load(std::sync::atomic::Ordering::SeqCst)),
            };
            match uses.iter_mut().find(|u| u.key == key) {
                Some(u) => u.running |= running,
                None => uses.push(crate::state::AnalysisUse { key, kind, clip: clip.id, running }),
            }
        }
    }
    uses
}

#[cfg(test)]
mod analysis_tests {
    use super::analysis_uses;
    use crate::state::{AnalysisDue, AnalysisUse, AppState, ANALYSIS_EDIT_GAP, ANALYSIS_EVERY, ANALYSIS_RETRY};
    use klipp_analysis::assets::{AssetKey, AssetKind};
    use klipp_core::{ClipId, RationalTime};
    use klipp_timeline::{Clip, EffectEdit};
    use std::time::{Duration, Instant};

    fn key() -> AssetKey {
        AssetKey { kind: AssetKind::Deflicker, media: "a".into(), width: 480, height: 270, fps: (24, 1) }
    }

    /// A folder removed when dropped.
    struct TempDir(std::path::PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A state whose media pool holds one item, its file there to key.
    fn with_media() -> (TempDir, AppState) {
        let dir = TempDir(std::env::temp_dir().join(format!("klipp-analysis-uses-{}-{}", std::process::id(), uuid::Uuid::new_v4())));
        std::fs::create_dir_all(&dir.0).unwrap();
        let path = dir.0.join("a.mp4");
        std::fs::write(&path, b"not a video, a file to key").unwrap();
        let mut item = klipp_media::MediaItem::synthetic_for_test(path, None);
        item.fps = klipp_core::Fps::new(24, 1);
        item.resolution = klipp_core::Resolution { width: 1920, height: 1080 };
        let mut state = AppState::default();
        state.media_pool.add(item);
        (dir, state)
    }

    fn used(running: bool) -> AnalysisUse {
        AnalysisUse { key: key(), kind: AssetKind::Deflicker, clip: ClipId::new(), running }
    }

    /// An edit is looked at soon, never left to the two-second look: one
    /// within the edit gap of the last look waits out the gap and says so,
    /// for a frame to be asked for then (a graph op over the socket is
    /// followed by no frame while the window is idle); nothing edited
    /// waits the whole interval.
    #[test]
    fn an_edit_is_looked_at_soon_with_a_frame_asked_for() {
        let shared = klipp_timeline::SharedSequence::new(klipp_timeline::Sequence::new_default());
        let mut state = AppState::default();
        let now = Instant::now();
        assert_eq!(state.analysis_due(shared.revision(), now), AnalysisDue::Now, "the first look");
        assert_eq!(state.analysis_due(shared.revision(), now + Duration::from_millis(500)), AnalysisDue::NotYet, "nothing edited, within the interval");
        shared.write().unwrap().name = "edited".into();
        let edited = now + Duration::from_millis(600);
        assert_eq!(state.analysis_due(shared.revision(), edited), AnalysisDue::Now, "an edit past the gap: at once");
        shared.write().unwrap().name = "edited again".into();
        let soon = edited + Duration::from_millis(100);
        assert_eq!(state.analysis_due(shared.revision(), soon), AnalysisDue::After(ANALYSIS_EDIT_GAP - Duration::from_millis(100)), "within the gap: a frame asked for");
        assert_eq!(state.analysis_due(shared.revision(), edited + ANALYSIS_EDIT_GAP), AnalysisDue::Now, "the gap out");
        assert_eq!(state.analysis_due(shared.revision(), edited + ANALYSIS_EDIT_GAP + ANALYSIS_EVERY), AnalysisDue::Now, "and the interval after");
    }

    /// A Deflicker added over the control socket, as `drain_control_inbox`
    /// applies a graph-op batch (the batch's edit built by
    /// `graph_ops_edit`, applied forward to the document, the sequence
    /// synced to the engine), is looked at on a frame asked for
    /// for it though the last look was a moment before, and the look reads it
    /// from the clip's picture. (d9's live check: the add landed 0.18 s
    /// after other edits, and the idle window drew no frame for 116 s.)
    #[test]
    fn a_deflicker_added_over_the_socket_is_looked_at_on_its_frame() {
        use klipp_graph_format::path::{NodePath, Root};
        use klipp_graph_lift::ops::GraphOp;
        let (_dir, mut state) = with_media();
        let media = state.media_pool.all().next().unwrap().id;
        let tc = |f| RationalTime::from_frames(f, 24);
        let mut clip = Clip::media(media, tc(0), tc(24), tc(0), tc(24));
        clip.apply_effect_edits(&[EffectEdit::Add(&klipp_effects::BuiltinEffect::GaussianBlur { radius: 2.0 })]).expect("through the picture").unwrap();
        let (clip_id, blur) = (clip.id, clip.picture_effects().unwrap()[0].node);
        let seq = state.sequence_mut();
        let v = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        seq.tracks[v].clips = vec![clip];
        let shared = klipp_timeline::SharedSequence::new(state.sequences[state.active_sequence_idx].clone());
        let queue = crossbeam_queue::ArrayQueue::new(8);
        let t = Instant::now();
        assert_eq!(state.analysis_due(shared.revision(), t), AnalysisDue::Now, "the look before the add");
        assert!(analysis_uses(&mut state).is_empty(), "nothing to analyse yet");
        // The op control's klipp_graph "add" makes: a legacy effect node
        // holding the registry's sample Deflicker, after the blur.
        let deflicker = (klipp_effects::registry::effect_type("deflicker").expect("deflicker").sample)();
        // The type an effect node of the picture has, the blur's.
        let type_id = state.sequences[state.active_sequence_idx].all_clips().next().unwrap().picture_v2().unwrap().picture.graph.nodes.iter().find(|n| n.id == blur).unwrap().type_id.clone();
        let node: klipp_graph_format::Node = serde_json::from_value(serde_json::json!({
            "id": uuid::Uuid::new_v4(), "type_id": type_id, "label": "",
            "legacy": { "payload": serde_json::to_value(&deflicker).unwrap() },
        }))
        .unwrap();
        let op = GraphOp::AddNode { after: NodePath::new(Root::Clip { clip: clip_id.0 }, [blur]), node: Box::new(node) };
        let outcome = crate::app::graph_ops::graph_ops_edit(&state.sequences[state.active_sequence_idx], &[op]).expect("the batch");
        let mut view = klipp_document::apply::DocumentView { sequences: &mut state.sequences, active: state.active_sequence_idx, media_pool: &mut state.media_pool, fps: &mut state.fps };
        let _ = view.apply_forward(&outcome.edit);
        crate::app::engine_sync::sync_sequence(&state, &shared, &queue);
        // 0.18 s after the last look: a frame asked for when the gap is
        // out, and the look then.
        let wait = match state.analysis_due(shared.revision(), t + Duration::from_millis(180)) {
            AnalysisDue::After(wait) => wait,
            other => panic!("the add isn't waited for: {other:?}"),
        };
        assert_eq!(state.analysis_due(shared.revision(), t + Duration::from_millis(180) + wait), AnalysisDue::Now, "the add is looked at on the frame asked for");
        let uses = analysis_uses(&mut state);
        assert_eq!(uses.iter().map(|u| (u.kind, u.clip)).collect::<Vec<_>>(), [(AssetKind::Deflicker, clip_id)], "read from the picture");
    }

    /// A run that ended without its asset is started again, once the
    /// retry has passed; never while it runs, nor on every look.
    #[test]
    fn a_failed_run_is_started_again_after_the_retry() {
        let mut state = AppState::default();
        let t = Instant::now();
        assert_eq!(state.analyses_to_start(&[used(false)], &[key()], t).len(), 1, "missing: started");
        assert!(state.analyses_to_start(&[used(true)], &[], t + Duration::from_secs(1)).is_empty(), "running");
        assert!(state.analyses_to_start(&[used(false)], &[key()], t + Duration::from_secs(2)).is_empty(), "ended without it: waits");
        assert!(state.analyses_to_start(&[used(false)], &[key()], t + Duration::from_secs(2) + ANALYSIS_RETRY / 2).is_empty());
        assert_eq!(state.analyses_to_start(&[used(false)], &[key()], t + Duration::from_secs(2) + ANALYSIS_RETRY).len(), 1, "started again");
    }

    /// A run that made its asset is let go: an asset gone later is made
    /// again at once.
    #[test]
    fn an_asset_gone_later_is_made_again() {
        let mut state = AppState::default();
        let t = Instant::now();
        assert_eq!(state.analyses_to_start(&[used(false)], &[key()], t).len(), 1);
        assert!(state.analyses_to_start(&[used(false)], &[], t + Duration::from_secs(1)).is_empty(), "ended, the asset there");
        assert!(state.analysis_started.is_empty(), "let go");
        assert_eq!(state.analyses_to_start(&[used(false)], &[key()], t + Duration::from_secs(2)).len(), 1, "gone: made again");
    }

    /// The effects are read from the clip's picture, as the renderer reads
    /// them: a Deflicker added through it is used, one switched off isn't.
    #[test]
    fn the_analyses_used_are_the_pictures() {
        let (_dir, mut state) = with_media();
        let media = state.media_pool.all().next().unwrap().id;
        let tc = |f| RationalTime::from_frames(f, 24);
        let deflicker = (klipp_effects::registry::effect_type("deflicker").expect("deflicker").sample)();
        let mut on = Clip::media(media, tc(0), tc(24), tc(0), tc(24));
        on.apply_effect_edits(&[EffectEdit::Add(&deflicker)]).expect("through the picture").unwrap();
        let mut off = Clip::media(media, tc(24), tc(48), tc(0), tc(24));
        off.apply_effect_edits(&[EffectEdit::Add(&deflicker), EffectEdit::SetEnabled(0, false)]).expect("through the picture").unwrap();
        let (on_id, off_id) = (on.id, off.id);
        let seq = state.sequence_mut();
        let v = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        seq.tracks[v].clips = vec![on, off];
        let uses = analysis_uses(&mut state);
        assert_eq!(uses.len(), 1, "{uses:?}");
        assert_eq!((uses[0].kind, uses[0].clip, uses[0].running), (AssetKind::Deflicker, on_id, false));
        assert_ne!(uses[0].clip, off_id);
    }
}

/// A track point added on `clip_id` at (`x`, `y`), normalised, on the
/// frame at the playhead, searching as far as the tracking preferences
/// say; it becomes the active point and the point-placing mode ends.
pub(super) fn add_track_point(state: &mut AppState, clip_id: klipp_core::ClipId, x: f64, y: f64) {
    let fps = state.fps.as_f64();
    let playhead_secs = state.playhead_smooth_secs;
    let search_radius = state.track_search_radius;
    let seq = &mut state.sequences[state.active_sequence_idx];
    if let Some(clip) = seq.all_clips_mut().find(|c| c.id == clip_id) {
        let clip_in_secs = clip.timeline_in.to_secs_f64();
        let source_in_secs = clip.source_in.to_secs_f64();
        let clip_time = playhead_secs - clip_in_secs + source_in_secs;
        let frame_idx = (clip_time * fps).round() as usize;
        let count = clip.track_points.len();
        clip.track_points.push(klipp_core::TrackPoint {
            name: format!("Tracker {}", count + 1),
            start_frame: frame_idx,
            positions: vec![(x, y)],
            search_radius,
            ..Default::default()
        });
        state.active_track_point = Some(count);
    }
    state.tracking_mode = false;
    state.dirty = true;
}

#[cfg(test)]
mod track_point_radius_tests {
    use super::*;

    /// #1816: a new track point searches as far as the tracking
    /// preference says: 48 gives 48, 16 gives 16.
    #[test]
    fn a_new_point_takes_the_preferences_search_radius() {
        for radius in [48, 16] {
            let mut state = AppState::default();
            let t = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
            let clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0));
            let id = clip.id;
            state.sequences[0].tracks[0].clips.push(clip);
            state.track_search_radius = radius;
            add_track_point(&mut state, id, 0.25, 0.75);
            let points = &state.sequence().all_clips().find(|c| c.id == id).unwrap().track_points;
            assert_eq!(points.len(), 1);
            assert_eq!((points[0].search_radius, points[0].positions[0]), (radius, (0.25, 0.75)));
        }
    }
}
