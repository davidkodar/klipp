//! Stabilization: a clip's motion analysed and smoothed (#826).

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
                        let frames = (item.duration.to_secs_f64() * fps).round() as usize;
                        // The window the effect is set to, so the gains
                        // that come back match what the user sees.
                        let window = c.builtin_effects.iter().find_map(|e| match e {
                            klipp_effects::BuiltinEffect::Deflicker { window_secs, .. } => Some(*window_secs as f64),
                            _ => None,
                        }).unwrap_or(1.0);
                        Some((item.path.clone(), fps, frames, window))
                    });
                let Some((path, fps, frame_count, window)) = found else { return true };
                if frame_count < 2 {
                    self.state.toast.show("Deflicker needs a clip of at least two frames");
                    return true;
                }
                let clip_id = *clip_id;
                self.state.deflicker_in_progress.insert(clip_id);
                self.state.deflicker_progress.insert(clip_id, 0.0);
                let event_tx = self.proxy_event_tx.clone();
                let sequence = self.sequence.clone();
                std::thread::spawn(move || {
                    // A 480-wide proxy: the measurement is a mean over
                    // the frame, and it does not get more true at 4K.
                    let mut decoder = match klipp_engine::SequentialFrameDecoder::open(&path, 480) {
                        Ok(d) => d,
                        Err(e) => {
                            let _ = event_tx.send(AppEvent::DeflickerFailed { clip_id, error: format!("decoder open: {e}") });
                            return;
                        }
                    };
                    let mut luma = Vec::with_capacity(frame_count);
                    let mut last_sent = 0.0f32;
                    loop {
                        match decoder.next_frame() {
                            Ok(Some((rgba, w, h))) => {
                                luma.push(klipp_effects::deflicker::frame_luma(&rgba, w, h));
                                let p = (luma.len() as f32 / frame_count.max(1) as f32).min(1.0);
                                // One event per percent, not per frame:
                                // a 4-minute clip is 6000 frames and the
                                // UI does not need 6000 redraws.
                                if p - last_sent >= 0.01 {
                                    last_sent = p;
                                    let _ = event_tx.send(AppEvent::DeflickerProgress { clip_id, progress: p });
                                }
                            }
                            Ok(None) => break,
                            Err(e) => {
                                let _ = event_tx.send(AppEvent::DeflickerFailed { clip_id, error: e });
                                return;
                            }
                        }
                    }
                    if luma.len() < 2 {
                        let _ = event_tx.send(AppEvent::DeflickerFailed { clip_id, error: "the clip decoded fewer than two frames".into() });
                        return;
                    }
                    let data = klipp_effects::deflicker::DeflickerData { luma, fps };
                    let gains = klipp_effects::deflicker::DeflickerGains::refresh(None, &data, window);
                    if let Ok(mut seq) = sequence.write() {
                        for track in seq.tracks.iter_mut() {
                            for clip in track.clips.iter_mut() {
                                if clip.id == clip_id {
                                    clip.deflicker_data = Some(data.clone());
                                    clip.deflicker_gains = Some(gains.clone());
                                }
                            }
                        }
                    }
                    let _ = event_tx.send(AppEvent::DeflickerComplete { clip_id });
                });
                true
            }
            AppCommand::RegainDeflicker { clip_id } => {
                // A mean over a window, per frame — microseconds, so
                // it is safe on every slider tick, as the stabilization
                // re-smooth is.
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                if let Some(clip) = seq.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).find(|c| c.id == *clip_id) {
                    let window = clip.builtin_effects.iter().find_map(|e| match e {
                        klipp_effects::BuiltinEffect::Deflicker { window_secs, .. } => Some(*window_secs as f64),
                        _ => None,
                    }).unwrap_or(1.0);
                    if let Some(data) = clip.deflicker_data.as_ref() {
                        clip.deflicker_gains = Some(klipp_effects::deflicker::DeflickerGains::refresh(
                            clip.deflicker_gains.clone(),
                            data,
                            window,
                        ));
                    }
                }
                self.state.dirty = true;
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
                        let frame_count = (dur_secs * fps).round() as usize;
                        // Pull the smoothing slider's current value off the effect
                        // so the initial smooth uses what the user already set,
                        // not a hardcoded 0.5.
                        let smoothing = c.builtin_effects.iter().find_map(|e| {
                            if let klipp_effects::BuiltinEffect::VideoStabilization { smoothing, .. } = e {
                                Some(*smoothing as f64)
                            } else {
                                None
                            }
                        }).unwrap_or(0.5);
                        Some((item.path.clone(), fps, dur_secs, frame_count, smoothing))
                    });

                if let Some((path, fps, duration_secs, frame_count, smoothing)) = clip_info {
                    let clip_id = *clip_id;
                    self.state.stabilization_in_progress.insert(clip_id);
                    self.state.stabilization_progress.insert(clip_id, 0.0);
                    let event_tx = self.proxy_event_tx.clone();
                    let sequence = self.sequence.clone();

                    std::thread::spawn(move || {
                        // Pipelined decode + analyze. The decoder runs on a producer
                        // thread, the analyzer (FAST-9 / match / RANSAC) runs here on
                        // the consumer side. Frames flow over a small bounded channel
                        // so the two halves overlap — wall time becomes ~max(decode,
                        // analyze) × frames instead of (decode + analyze) × frames.
                        //
                        // Channel is sync_channel(3): tiny memory bound (~1.5 MB at
                        // 480p), enough buffering to hide jitter.
                        //
                        // Producer exits cleanly when the consumer drops the receiver
                        // (analyze_clip returns). No explicit cancel flag needed.
                        let (frame_tx, frame_rx) = std::sync::mpsc::sync_channel::<(Vec<u8>, u32, u32)>(3);
                        let path_for_producer = path.clone();
                        let event_tx_open = event_tx.clone();
                        // Why the walk stopped, if it stopped on an error.
                        // The consumer sees only a closed channel, and would
                        // otherwise report "failed to decode frame N" for a
                        // packet the container could not read or a frame the
                        // decoder could not.
                        let decode_error = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
                        let decode_error_producer = std::sync::Arc::clone(&decode_error);
                        let producer = std::thread::Builder::new()
                            .name("klipp-stab-decode".to_string())
                            .spawn(move || -> std::time::Duration {
                                let mut walker = match klipp_engine::SequentialFrameDecoder::open(&path_for_producer, 480) {
                                    Ok(w) => w,
                                    Err(e) => {
                                        let _ = event_tx_open.send(AppEvent::StabilizationFailed { clip_id, error: format!("decoder open: {e}") });
                                        return std::time::Duration::ZERO;
                                    }
                                };
                                let mut decode_wall = std::time::Duration::ZERO;
                                loop {
                                    let t0 = std::time::Instant::now();
                                    let frame = walker.next_frame();
                                    decode_wall += t0.elapsed();
                                    match frame {
                                        Ok(Some(f)) => {
                                            // send blocks if consumer is behind — that's
                                            // exactly the backpressure we want.
                                            if frame_tx.send(f).is_err() {
                                                // Consumer dropped (analyze done/aborted).
                                                break;
                                            }
                                        }
                                        Ok(None) => break, // EOF
                                        Err(e) => {
                                            *decode_error_producer.lock().unwrap_or_else(|p| p.into_inner()) = Some(e);
                                            break;
                                        }
                                    }
                                }
                                decode_wall
                            });
                        let producer = match producer {
                            Ok(h) => h,
                            Err(e) => {
                                let _ = event_tx.send(AppEvent::StabilizationFailed { clip_id, error: format!("spawn producer: {e}") });
                                return;
                            }
                        };

                        let analyze_start = std::time::Instant::now();
                        let mut consumer_stall = std::time::Duration::ZERO;
                        let mut decoded_frames = 0usize;
                        let result = klipp_effects::stabilization::analyze_clip(
                            fps,
                            frame_count,
                            &mut |_time_secs| {
                                let t0 = std::time::Instant::now();
                                let f = frame_rx.recv().ok();
                                consumer_stall += t0.elapsed();
                                if f.is_some() { decoded_frames += 1; }
                                f
                            },
                            &|progress| {
                                let _ = event_tx.send(AppEvent::StabilizationProgress { clip_id, progress });
                                true // no cancel support yet
                            },
                        );
                        // Drop receiver so the producer's next send() errors out and
                        // it exits cleanly. (Implicit drop happens at scope end too,
                        // but explicit makes the intent clear.)
                        drop(frame_rx);
                        let decode_wall = producer.join().unwrap_or(std::time::Duration::ZERO);
                        let total = analyze_start.elapsed();
                        let analysis_wall = total.saturating_sub(consumer_stall);
                        tracing::info!(
                            "Stabilization analyze (pipelined): {} frames in {:.2}s — decode {:.2}s, analysis {:.2}s, consumer stall {:.2}s ({:.0}% of total)",
                            decoded_frames,
                            total.as_secs_f64(),
                            decode_wall.as_secs_f64(),
                            analysis_wall.as_secs_f64(),
                            consumer_stall.as_secs_f64(),
                            100.0 * consumer_stall.as_secs_f64() / total.as_secs_f64().max(1e-6),
                        );
                        match result {
                            Ok(data) => {
                                let window_secs = (smoothing.clamp(0.0, 1.0) * duration_secs).max(0.05);
                                let smoothed = klipp_effects::stabilization::smooth_trajectory(&data, window_secs);
                                if let Ok(mut seq) = sequence.write() {
                                    klipp_engine::seq_write_holder_set(klipp_engine::SeqWriteHolder::UiStabilizationComplete);
                                    for track in &mut seq.tracks {
                                        if let Some(clip) = track.find_clip_mut(clip_id) {
                                            clip.stabilization_data = Some(data);
                                            clip.smoothed_trajectory = Some(smoothed);
                                            break;
                                        }
                                    }
                                    klipp_engine::seq_write_holder_set(klipp_engine::SeqWriteHolder::None);
                                }
                                let _ = event_tx.send(AppEvent::StabilizationComplete { clip_id });
                            }
                            Err(e) => {
                                let error = decode_error.lock().unwrap_or_else(|p| p.into_inner()).take().unwrap_or(e);
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
            AppCommand::ResmoothStabilization { clip_id } => {
                // Re-run gaussian smoothing on the existing baked trajectory using
                // the current smoothing slider value. Microseconds — safe to call
                // every slider tick.
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                if let Some(clip) = seq.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).find(|c| c.id == *clip_id) {
                    // Find the current smoothing value from the VideoStabilization effect.
                    let smoothing = clip.builtin_effects.iter().find_map(|e| {
                        if let klipp_effects::BuiltinEffect::VideoStabilization { smoothing, .. } = e {
                            Some(*smoothing as f64)
                        } else {
                            None
                        }
                    }).unwrap_or(0.5);
                    if let Some(data) = clip.stabilization_data.as_ref() {
                        let duration_secs = data.frame_count as f64 / data.fps.max(1.0);
                        let window_secs = (smoothing.clamp(0.0, 1.0) * duration_secs).max(0.05);
                        let smoothed = klipp_effects::stabilization::smooth_trajectory(data, window_secs);
                        clip.smoothed_trajectory = Some(smoothed);
                    }
                }
                self.state.dirty = true;
                true
            }
            AppCommand::ToggleTrackingMode => {
                self.state.tracking_mode = !self.state.tracking_mode;
                true
            }
            AppCommand::TrackSurface { clip_id, corners, at_secs, name, surface, direction, interactive } => {
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
                self.start_surface_track(SurfaceTrackJob { clip_id, surface: id, name, path, media_fps, frames: (first, last), reference, corners: on_source, pixels, direction: *direction, interactive: *interactive, cancel });
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
                let window = clip.node_graph.nodes.iter()
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
                // Create a new track point at the given normalized position
                let fps = self.state.fps.as_f64();
                let playhead_secs = self.state.playhead_smooth_secs;
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                if let Some(clip) = seq.all_clips_mut().find(|c| c.id == *clip_id) {
                    let clip_in_secs = clip.timeline_in.to_secs_f64();
                    let source_in_secs = clip.source_in.to_secs_f64();
                    let clip_time = playhead_secs - clip_in_secs + source_in_secs;
                    let frame_idx = (clip_time * fps).round() as usize;
                    let count = clip.track_points.len();
                    let tp = klipp_core::TrackPoint {
                        name: format!("Tracker {}", count + 1),
                        start_frame: frame_idx,
                        positions: vec![(*x, *y)],
                        ..Default::default()
                    };
                    clip.track_points.push(tp);
                    self.state.active_track_point = Some(count);
                }
                self.state.tracking_mode = false;
                self.state.dirty = true;
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

                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                if let Some(clip) = seq.all_clips_mut().find(|c| c.id == clip_id) {
                    if let Some(tp) = clip.track_points.get(point_index) {
                        let positions = tp.positions.clone();
                        let start_frame = tp.start_frame;

                        match target {
                            klipp_core::TrackTarget::TransformPosition => {
                                clip.transform_pos_x_keyframes.clear();
                                clip.transform_pos_y_keyframes.clear();
                                for (i, &(nx, ny)) in positions.iter().enumerate() {
                                    let frame_idx = start_frame + i;
                                    let t = frame_idx as f64 / fps;
                                    let pos_x = (nx - 0.5) as f32;
                                    let pos_y = (ny - 0.5) as f32;
                                    clip.transform_pos_x_keyframes.push(klipp_core::Keyframe {
                                        time_secs: t,
                                        value: pos_x,
                                        interpolation: klipp_core::Interpolation::Linear,
                                        tangent_in: (0.0, 0.0),
                                        tangent_out: (0.0, 0.0),
                                    });
                                    clip.transform_pos_y_keyframes.push(klipp_core::Keyframe {
                                        time_secs: t,
                                        value: pos_y,
                                        interpolation: klipp_core::Interpolation::Linear,
                                        tangent_in: (0.0, 0.0),
                                        tangent_out: (0.0, 0.0),
                                    });
                                }
                                self.state.toast.show("Generated transform keyframes from tracker");
                            }
                            klipp_core::TrackTarget::MaskCenter { node_id, mask_index } if !tp.corners.is_empty() => {
                                // A window track in perspective (#238): the
                                // corners' course on the window, one undo
                                // step.
                                let track = window_perspective_track(clip, tp, &self.state.media_pool, fps);
                                let count = track.as_ref().map_or(0, |t| t.keys.len());
                                let old_graph = clip.node_graph.clone();
                                if let Some(mask) = clip.node_graph.nodes.iter_mut().find(|n| n.id == node_id)
                                    .and_then(|n| n.params.masks.as_mut())
                                    .and_then(|ms| ms.masks.get_mut(mask_index))
                                {
                                    mask.keyframes.clear_track();
                                    mask.keyframes.track_perspective = track.filter(|t| !t.keys.is_empty());
                                }
                                let new_graph = clip.node_graph.clone();
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
                                let old_graph = clip.node_graph.clone();
                                if let Some(mask) = clip.node_graph.nodes.iter_mut().find(|n| n.id == node_id)
                                    .and_then(|n| n.params.masks.as_mut())
                                    .and_then(|ms| ms.masks.get_mut(mask_index))
                                {
                                    mask.keyframes.clear_track();
                                    let k = |t: f64, v: f32| klipp_core::Keyframe { time_secs: t, value: v, interpolation: klipp_core::Interpolation::Linear, tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0) };
                                    for &(t, dx, dy, scale, rot) in &keys {
                                        mask.keyframes.track_x.push(k(t, dx));
                                        mask.keyframes.track_y.push(k(t, dy));
                                        mask.keyframes.track_scale.push(k(t, scale));
                                        mask.keyframes.track_rotation.push(k(t, rot));
                                    }
                                }
                                let new_graph = clip.node_graph.clone();
                                if let Some(tp) = clip.track_points.get_mut(point_index) {
                                    tp.linked_mask = Some((node_id, mask_index));
                                }
                                window_edit = Some((EditCommand::SetNodeGraph { clip_id, old_graph, new_graph }, keys.len()));
                            }
                            klipp_core::TrackTarget::MaskCenter { node_id, mask_index } => {
                                if let Some(tp) = clip.track_points.get_mut(point_index) {
                                    tp.linked_mask = Some((node_id, mask_index));
                                }
                                if let Some(node) = clip.node_graph.nodes.iter_mut().find(|n| n.id == node_id) {
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
                                                });
                                                mask.keyframes.center_y.push(klipp_core::Keyframe {
                                                    time_secs: t,
                                                    value: ny as f32,
                                                    interpolation: klipp_core::Interpolation::Linear,
                                                    tangent_in: (0.0, 0.0),
                                                    tangent_out: (0.0, 0.0),
                                                });
                                            }
                                            self.state.toast.show("Generated mask center keyframes from tracker");
                                        }
                                    }
                                }
                            }
                        }
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
}
