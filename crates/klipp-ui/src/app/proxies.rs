//! Proxies: made in the background, their progress and their results (#826).

use super::*;

impl KlippApp {
    pub(super) fn spawn_proxy_generation(&mut self, media_id: MediaId) {
        let item = match self.state.media_pool.get(&media_id) {
            Some(item) => item.clone(),
            None => return,
        };
        if item.proxy_ready {
            return;
        }
        if self.state.proxy_generating.contains(&media_id) {
            tracing::info!("Proxy already generating for {:?}, skipping", media_id);
            return;
        }
        let cache_dir = self.prefs.proxy.proxy_cache_dir.clone().unwrap_or_else(|| {
            dirs::cache_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join("klipp")
                .join("proxies")
        });
        let _ = std::fs::create_dir_all(&cache_dir);
        let proxy_path = cache_dir.join(format!("{}.mp4", media_id));
        if proxy_path.exists() {
            // A file at the proxy path is not a proxy until it has been
            // checked against the source: a generation killed part-way
            // (a crash, a power cut) leaves a playable file that is
            // shorter than the clip, and the timeline then plays the
            // clip's tail as black. New proxies are written to a
            // `.partial` name and renamed on success, so this mostly
            // guards files from before that; it is one header probe.
            let probed = klipp_engine::probe_media_header(&proxy_path).ok().and_then(|h| h.duration_secs);
            if transcode_is_complete(probed, item.duration.to_secs_f64()) {
                let file_size = std::fs::metadata(&proxy_path).map(|m| m.len()).unwrap_or(0);
                tracing::info!(
                    "Proxy already exists for {}: {} ({:.1} MB)",
                    item.filename, proxy_path.display(), file_size as f64 / (1024.0 * 1024.0)
                );
                if let Some(item) = self.state.media_pool.get_mut(&media_id) {
                    item.proxy_path = Some(proxy_path);
                    item.proxy_ready = true;
                }
                return;
            }
            tracing::warn!(
                "Proxy for {} at {} is incomplete ({:?} s of {:.2} s); regenerating",
                item.filename, proxy_path.display(), probed, item.duration.to_secs_f64()
            );
            let _ = std::fs::remove_file(&proxy_path);
        }
        let partial_path = partial_proxy_path(&proxy_path);
        let _ = std::fs::remove_file(&partial_path);
        let res_label = self.prefs.proxy.proxy_resolution.label();
        let scale_arg = self.prefs.proxy.proxy_resolution.ffmpeg_scale_arg().to_string();
        tracing::info!(
            "Generating proxy for {} — resolution: {}, scale: {}, output: {}",
            item.filename, res_label, scale_arg, proxy_path.display()
        );
        self.state.proxy_generating.insert(media_id);
        self.state.proxy_start_times.insert(media_id, std::time::Instant::now());
        self.state.proxy_toast_dismissed = false; // Show toast for new proxy
        self.state.proxy_status_message = Some((
            format!("Generating proxy for {}... ({})", item.filename, res_label),
            std::time::Instant::now(),
        ));
        let source_path = item.path.clone();
        let total_duration_secs = item.duration.to_secs_f64();
        let tx = self.proxy_event_tx.clone();
        let progress_tx = self.proxy_progress_tx.clone();
        let proxy_out = partial_path;
        let child_pid: Arc<std::sync::atomic::AtomicU32> = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let pid_clone = child_pid.clone();
        self.proxy_children.push(child_pid);
        std::thread::spawn(move || {
            // At most a few transcodes at once (#1360).
            let _slot = TRANSCODE_SLOTS.acquire();
            let _ = tx.send(AppEvent::ProxyGenerationStarted { media_id });

            // Helper: spawn ffmpeg with progress tracking, wait for result
            let run_ffmpeg = |args: &[&str], pid_ref: &Arc<std::sync::atomic::AtomicU32>,
                              ptx: &Sender<(MediaId, f32)>, dur: f64|
                -> Result<(), String> {
                // Ends with the app, killed or not (#1361).
                let mut cmd = klipp_jobs::ends_with_app("ffmpeg");
                cmd.arg("-nostdin").arg("-i").arg(&source_path);
                for a in args { cmd.arg(a); }
                cmd.arg("-progress").arg("pipe:1")
                   .arg("-y").arg(&proxy_out)
                   .stdout(std::process::Stdio::piped())
                   .stderr(std::process::Stdio::piped());
                let mut child = cmd.spawn().map_err(|e| e.to_string())?;
                pid_ref.store(child.id(), std::sync::atomic::Ordering::Relaxed);
                let stdout = child.stdout.take();
                let ptx2 = ptx.clone();
                let reader_handle = std::thread::spawn(move || {
                    if let Some(stdout) = stdout {
                        use std::io::BufRead;
                        let reader = std::io::BufReader::new(stdout);
                        for line in reader.lines().map_while(Result::ok) {
                            if let Some(time_str) = line.strip_prefix("out_time_us=") {
                                if let Ok(us) = time_str.trim().parse::<i64>() {
                                    let secs = us as f64 / 1_000_000.0;
                                    let pct = if dur > 0.0 {
                                        (secs / dur).min(1.0) as f32
                                    } else { 0.0 };
                                    let _ = ptx2.send((media_id, pct));
                                }
                            }
                        }
                    }
                });
                let status = child.wait().map_err(|e| e.to_string())?;
                let _ = reader_handle.join();
                pid_ref.store(0, std::sync::atomic::Ordering::Relaxed);
                if status.success() { Ok(()) } else { Err("ffmpeg exited with error".into()) }
            };

            // Try GPU (NVENC) first — much faster on NVIDIA GPUs
            let nvenc_args: Vec<&str> = vec![
                "-vf", &scale_arg,
                "-c:v", "h264_nvenc", "-preset", "p1", "-cq", "30",
                "-c:a", "aac", "-b:a", "64k",
            ];
            let result = run_ffmpeg(&nvenc_args, &pid_clone, &progress_tx, total_duration_secs);

            let result = match result {
                Ok(()) => {
                    tracing::info!("Proxy generated with NVENC (GPU)");
                    Ok(())
                }
                Err(nvenc_err) => {
                    tracing::info!("NVENC failed ({}), retrying with CPU (libx264)", nvenc_err);
                    let _ = std::fs::remove_file(&proxy_out); // clean up partial file
                    let _ = progress_tx.send((media_id, 0.0)); // reset progress
                    let cpu_args: Vec<&str> = vec![
                        "-vf", &scale_arg,
                        "-c:v", "libx264", "-crf", "28", "-preset", "ultrafast",
                        "-c:a", "aac", "-b:a", "64k",
                    ];
                    let r = run_ffmpeg(&cpu_args, &pid_clone, &progress_tx, total_duration_secs);
                    if r.is_ok() { tracing::info!("Proxy generated with libx264 (CPU)"); }
                    r
                }
            };

            // The proxy exists at its real name only once it is whole.
            let result = result.and_then(|()| {
                std::fs::rename(&proxy_out, &proxy_path).map_err(|e| format!("moving the finished proxy into place: {e}"))
            });
            match result {
                Ok(()) => {
                    let _ = tx.send(AppEvent::ProxyGenerationComplete { media_id, path: proxy_path });
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&proxy_out);
                    let _ = tx.send(AppEvent::ProxyGenerationFailed { media_id, error: e });
                }
            }
        });
    }

    pub(super) fn drain_proxy_events(&mut self) {
        while let Ok(event) = self.proxy_event_rx.try_recv() {
            if super::event_writes::event_writes(&event) {
                self.close_drag_for_result();
            }
            match event {
                AppEvent::ProxyGenerationStarted { media_id } => {
                    self.note_proxy_failure(media_id, None);
                    let filename = self.state.media_pool.get(&media_id)
                        .map(|m| m.filename.clone())
                        .unwrap_or_else(|| format!("{media_id}"));
                    tracing::info!("Proxy generation started for {}", filename);
                }
                AppEvent::ProxyGenerationComplete { media_id, path } => {
                    self.state.proxy_generating.remove(&media_id);
                    self.state.proxy_progress.remove(&media_id);
                    let elapsed_secs = self.state.proxy_start_times.remove(&media_id)
                        .map(|t| t.elapsed().as_secs())
                        .unwrap_or(0);
                    let file_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    let filename = self.state.media_pool.get(&media_id)
                        .map(|m| m.filename.clone())
                        .unwrap_or_else(|| format!("{media_id}"));
                    tracing::info!(
                        "Proxy complete for {}: {} ({:.1} MB, {}s)",
                        filename, path.display(), file_size as f64 / (1024.0 * 1024.0), elapsed_secs
                    );
                    let time_str = if elapsed_secs >= 60 {
                        format!("{}m {:02}s", elapsed_secs / 60, elapsed_secs % 60)
                    } else {
                        format!("{}s", elapsed_secs)
                    };
                    self.state.proxy_status_message = Some((
                        format!("Proxy ready for {} ({:.1} MB, took {})", filename, file_size as f64 / (1024.0 * 1024.0), time_str),
                        std::time::Instant::now(),
                    ));
                    if let Some(item) = self.state.media_pool.get_mut(&media_id) {
                        item.proxy_path = Some(path);
                        item.proxy_ready = true;
                    }
                    self.sync_media_pool_to_engine();
                }
                AppEvent::ProxyGenerationFailed { media_id, error } => {
                    self.note_proxy_failure(media_id, Some(error.clone()));
                    self.state.proxy_generating.remove(&media_id);
                    self.state.proxy_progress.remove(&media_id);
                    self.state.proxy_start_times.remove(&media_id);
                    let filename = self.state.media_pool.get(&media_id)
                        .map(|m| m.filename.clone())
                        .unwrap_or_else(|| format!("{media_id}"));
                    tracing::warn!("Proxy generation failed for {}: {}", filename, error);
                    self.state.proxy_status_message = Some((
                        format!("Proxy failed for {}: {}", filename, error),
                        std::time::Instant::now(),
                    ));
                }
                AppEvent::SkimTranscodeProgress { media_id, progress } => {
                    let filename = self.state.media_pool.get(&media_id)
                        .map(|m| m.filename.clone())
                        .unwrap_or_default();
                    self.state.skim_transcode_status = Some((filename, progress));
                }
                AppEvent::SkimTranscodeComplete { media_id, path } => {
                    // Wire skim_file for the originating MediaId AND
                    // every dedupe waiter that registered against the
                    // same output path. Without this, a second
                    // MediaId pointing to the same source file (e.g.
                    // project pool path vs file browser absolute
                    // path) would never learn the cache file exists
                    // and would keep falling back to live source
                    // decode on hover.
                    let waiters = self.state.skim_in_flight_paths
                        .remove(&path)
                        .unwrap_or_else(|| vec![media_id]);
                    for mid in &waiters {
                        self.state.skim_transcoding.remove(mid);
                        if let Some(item) = self.state.media_pool.get_mut(mid) {
                            item.skim_file = Some(path.clone());
                        }
                    }
                    if self.state.skim_transcoding.is_empty() {
                        self.state.skim_transcode_status = None;
                        self.state.skim_transcode_done_at = Some(std::time::Instant::now());
                    }
                    self.sync_media_pool_to_engine();
                    let filename = self.state.media_pool.get(&media_id)
                        .map(|m| m.filename.clone())
                        .unwrap_or_default();
                    tracing::info!(
                        "Skim transcode complete for {}: {} ({} media id(s) wired)",
                        filename, path.display(), waiters.len(),
                    );
                }
                AppEvent::SkimTranscodeFailed { media_id, path, error } => {
                    // Drop the originator AND any dedupe waiters
                    // from `skim_transcoding`. They'll re-request
                    // naturally on the next hover/import; we don't
                    // auto-respawn here because the same race could
                    // re-trigger.
                    let waiters = self.state.skim_in_flight_paths
                        .remove(&path)
                        .unwrap_or_else(|| vec![media_id]);
                    for mid in &waiters {
                        self.state.skim_transcoding.remove(mid);
                    }
                    if self.state.skim_transcoding.is_empty() {
                        self.state.skim_transcode_status = None;
                    }
                    let filename = self.state.media_pool.get(&media_id)
                        .map(|m| m.filename.clone())
                        .unwrap_or_default();
                    tracing::warn!(
                        "Skim transcode failed for {}: {} (cleared {} waiter(s))",
                        filename, error, waiters.len(),
                    );
                }
                AppEvent::BrowserSkimFrameReady { media_id, rgba, width, height, target } => {
                    if !rgba.is_empty() && width > 0 && height > 0 {
                        self.state.browser_skim_pending = Some((media_id, rgba, width, height, target));
                    } else {
                        // Nothing was shown for it: the next request for
                        // this frame is not a repeat.
                        self.state.skim_last_requested = None;
                    }
                    // Always reset — flag must never stay stuck regardless of decode outcome
                    self.state.skim_decode_in_flight = false;
                    self.state.skim_decode_started_at = None;
                }
                AppEvent::MediaFileChanged { path, deleted } => {
                    // Collect all matching media IDs up-front — a single file
                    // path can back multiple items (e.g. subclips sharing a
                    // parent source). We need every one to stay in sync.
                    let matches: Vec<(klipp_core::MediaId, String)> = self
                        .state
                        .media_pool
                        .all()
                        .filter(|item| item.path == path)
                        .map(|item| (item.id, item.filename.clone()))
                        .collect();

                    if matches.is_empty() {
                        // Nothing to do — watcher may have caught a stale path.
                    } else if deleted || !path.exists() {
                        let first_filename = matches[0].1.clone();
                        for (id, _) in &matches {
                            if let Some(item) = self.state.media_pool.get_mut(id) {
                                item.offline = true;
                            }
                            if !self.state.offline_media_ids.contains(id) {
                                self.state.offline_media_ids.push(*id);
                            }
                        }
                        // Track and surface — toast points at File menu rather than
                        // popping a modal mid-edit, which would interrupt the user.
                        self.state.toast.show(format!(
                            "Media file offline: {first_filename}. Use File \u{2192} Relink Media\u{2026}"
                        ));
                        tracing::warn!("Media file deleted externally: {:?}", path);
                    } else {
                        // File was modified. Always mark for re-probe; if
                        // round-trip is enabled, dispatch the same re-probe
                        // command RelinkMedia uses so metadata, thumbnails,
                        // and waveforms refresh without user action.
                        let round_trip = self.prefs.external_tools.round_trip_reload;
                        for (id, _) in &matches {
                            if let Some(item) = self.state.media_pool.get_mut(id) {
                                item.needs_reprobe = true;
                            }
                            if round_trip {
                                let _ = self.cmd_tx.send(AppCommand::LoadMediaFile {
                                    path: path.clone(),
                                    media_id: *id,
                                });
                            }
                        }
                        if round_trip {
                            self.state.toast.show("Clip reloaded after external edit.");
                        }
                        tracing::info!("Media file modified externally: {:?}", path);
                    }
                }
                AppEvent::DeflickerProgress { clip_id, progress } => {
                    self.state.deflicker_progress.insert(clip_id, progress);
                }
                done @ (AppEvent::DeflickerComplete { .. } | AppEvent::StabilizationComplete { .. }) => {
                    apply_analysis_result(&mut self.state, &done);
                }
                AppEvent::DeflickerFailed { clip_id, error } => {
                    self.state.deflicker_in_progress.remove(&clip_id);
                    self.state.deflicker_progress.remove(&clip_id);
                    tracing::warn!("Deflicker analysis failed for {clip_id:?}: {error}");
                    self.state.toast.show(format!("Deflicker analysis failed: {error}"));
                }
                AppEvent::StabilizationProgress { clip_id, progress } => {
                    self.state.stabilization_progress.insert(clip_id, progress);
                }
                AppEvent::StabilizationFailed { clip_id, error } => {
                    self.state.stabilization_in_progress.remove(&clip_id);
                    self.state.stabilization_progress.remove(&clip_id);
                    tracing::warn!("Stabilization failed for {:?}: {}", clip_id, error);
                    self.state.toast.show(format!("Stabilization failed: {error}"));
                }
                AppEvent::SceneDetectProgress { clip_id, progress } => {
                    self.state.scene_detect_progress.insert(clip_id, progress);
                }
                AppEvent::SceneDetectComplete { clip_id, cut_times } => {
                    self.state.scene_detect_cancel.remove(&clip_id);
                    let running = self.state.scene_detect_in_progress.remove(&clip_id);
                    self.state.scene_detect_progress.remove(&clip_id);
                    let split = self.state.scene_split_pending.remove(&clip_id);
                    // Cancelled: what it found by then is not used. (The next
                    // event, not out of the drain: that would drop the rest.)
                    if !running {
                        continue;
                    }
                    if split {
                        self.split_at_scene_cuts(clip_id, &cut_times);
                        continue;
                    }
                    let color = self.prefs.editing.scene_detect_marker_color;
                    let mut added = 0usize;
                    let mut found_clip = false;
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(clip_id) {
                            found_clip = true;
                            // detect_scenes emits source-media time; ClipMarker.time_secs
                            // is clip-relative (see sequence.rs split logic at line 584).
                            let source_in_secs = clip.source_in.to_secs_f64();
                            let clip_dur = clip.duration().to_secs_f64().max(1e-9);
                            for (i, &cut_secs) in cut_times.iter().enumerate() {
                                let rel = cut_secs - source_in_secs;
                                if rel > 0.0 && rel < clip_dur {
                                    clip.clip_markers.push(klipp_timeline::ClipMarker {
                                        time_secs: rel,
                                        name: format!("Scene {}", i + 1),
                                        color,
                                        comment: String::new(),
                                    });
                                    added += 1;
                                }
                            }
                            clip.clip_markers.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                            break;
                        }
                    }
                    self.sync_sequence_to_engine();
                    if !found_clip {
                        self.state.toast.show("Scene detection: clip no longer exists (was it split?)".to_string());
                    } else {
                        self.state.toast.show(format!(
                            "Detected {} scene cut{}.",
                            added,
                            if added == 1 { "" } else { "s" }
                        ));
                    }
                    tracing::info!(
                        "Scene detect: added {} marker(s) to clip={:?} (from {} raw cuts)",
                        added, clip_id, cut_times.len()
                    );
                    self.state.dirty = true;
                }
                AppEvent::SceneDetectFailed { clip_id, error } => {
                    self.state.scene_detect_in_progress.remove(&clip_id);
                    self.state.scene_detect_progress.remove(&clip_id);
                    self.state.scene_detect_cancel.remove(&clip_id);
                    self.state.scene_split_pending.remove(&clip_id);
                    tracing::warn!("Scene detection failed for {:?}: {}", clip_id, error);
                    self.state.toast.show(format!("Scene detection failed: {error}"));
                }
                AppEvent::TrackingProgress { clip_id, progress } => {
                    self.state.tracking_progress.insert(clip_id, progress);
                }
                AppEvent::TrackingComplete { clip_id, point_index, positions, start_frame, motion, corners } => {
                    self.state.tracking_in_progress.remove(&clip_id);
                    self.state.tracking_progress.remove(&clip_id);
                    let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                    if let Some(clip) = seq.all_clips_mut().find(|c| c.id == clip_id) {
                        if let Some(tp) = clip.track_points.get_mut(point_index) {
                            tp.positions = positions;
                            tp.start_frame = start_frame;
                            tp.motion = motion;
                            tp.corners = corners;
                        }
                    }
                    // If TrackMaskRegion registered an auto-link for
                    // this analysis, consume it now: dispatch
                    // GenerateKeyframesFromTrack so the mask's centre
                    // immediately follows the tracked feature.
                    let pending = self.state.pending_mask_track_link.remove(&(clip_id, point_index));
                    if let Some(target) = pending {
                        let mut needs_refresh = false;
                        let cmd = AppCommand::GenerateKeyframesFromTrack {
                            clip_id, point_index, target,
                        };
                        let _ = self.handle_stabilization_commands(&cmd, &mut needs_refresh);
                        self.state.toast.show("Mask tracking complete");
                    } else {
                        self.state.toast.show("Point tracking complete");
                    }
                }
                AppEvent::TrackingFailed { clip_id, error } => {
                    self.state.tracking_in_progress.remove(&clip_id);
                    self.state.tracking_progress.remove(&clip_id);
                    tracing::warn!("Point tracking failed for {:?}: {}", clip_id, error);
                    self.state.toast.show(format!("Point tracking failed: {error}"));
                }
                AppEvent::SurfaceTracked { clip_id, surface, name, corners, reference_secs, keys, source_size, media_fps, held_secs, direction, interactive, track_area, exclude } => {
                    self.state.tracking_in_progress.remove(&clip_id);
                    self.state.tracking_progress.remove(&clip_id);
                    self.state.surface_track_cancel.remove(&clip_id);
                    self.apply_surface_track(clip_id, klipp_timeline::Surface {
                        id: surface,
                        name,
                        corners,
                        reference_secs,
                        keys: keys.into_iter().map(super::surfaces::surface_key_of).collect(),
                        source_size,
                        media_fps,
                        track_area,
                        exclude,
                    }, held_secs, direction, interactive);
                }
                AppEvent::SurfaceCorrected { clip_id, surface, source_secs, corners, spans } => {
                    self.state.tracking_in_progress.remove(&clip_id);
                    self.state.tracking_progress.remove(&clip_id);
                    self.state.surface_track_cancel.remove(&clip_id);
                    self.apply_surface_correction(clip_id, surface, source_secs, corners, spans);
                }
                AppEvent::SurfaceSnapped { clip_id, at_secs, corners, then_track, name, why, request_id } => {
                    self.state.surface_snaps_pending = self.state.surface_snaps_pending.saturating_sub(1);
                    self.apply_surface_snap(clip_id, at_secs, corners, then_track, name, why, request_id);
                }
                AppEvent::SurfaceTrackFailed { clip_id, surface, error } => {
                    self.state.tracking_in_progress.remove(&clip_id);
                    self.state.tracking_progress.remove(&clip_id);
                    self.state.surface_track_cancel.remove(&clip_id);
                    if error == "cancelled" {
                        self.state.toast.show("Surface tracking stopped");
                    } else {
                        tracing::warn!("Surface tracking failed for {:?} ({surface}): {}", clip_id, error);
                        self.state.toast.show(format!("Surface tracking failed: {error}"));
                    }
                }
                AppEvent::MediaSyncComplete { video_id, sync } => {
                    self.apply_media_sync(video_id, sync);
                }
                AppEvent::MediaSyncFailed { video_id, error } => {
                    tracing::warn!("Media sync failed for {:?}: {}", video_id, error);
                    self.state.toast.show(format!("Sync failed: {error}"));
                }
                AppEvent::MulticamSyncReady { plan, sequence_id, at_secs } => {
                    self.apply_multicam_plan(plan, sequence_id, at_secs);
                }
                AppEvent::AudioSyncComplete { clip_id, offset_samples, ref_timeline_in_secs } => {
                    self.apply_audio_sync_result(clip_id, offset_samples, ref_timeline_in_secs);
                }
                AppEvent::AudioSyncFailed { clip_id, error } => {
                    tracing::warn!("Audio sync failed for {:?}: {}", clip_id, error);
                    self.state.toast.show(format!("Audio sync failed: {error}"));
                }
                AppEvent::DenoiserProfileLearned { clip_id, effect_index, params, source_secs } => {
                    self.set_audio_effect_params(clip_id, effect_index, &params, "Learn Noise Profile");
                    self.state.toast.show(format!("Noise profile learned at {}", format_profile_time(source_secs)));
                }
                AppEvent::NormalizeMeasured { clip_id, effect_index, key, lufs } => {
                    self.normalize_measured(clip_id, effect_index, key, lufs);
                }
                AppEvent::WaterMeasured { clip_id, key, water } => {
                    self.water_measured(clip_id, key, water);
                }
                AppEvent::DenoiserProfileFailed { error } => {
                    tracing::warn!("Denoiser profile learning failed: {}", error);
                    self.state.toast.show(format!("Could not learn noise profile: {error}"));
                }
                _ => {}
            }
        }
    }

    pub(super) fn drain_proxy_progress(&mut self) {
        while let Ok((media_id, pct)) = self.proxy_progress_rx.try_recv() {
            self.state.proxy_progress.insert(media_id, pct);
        }
        self.publish_proxies();
    }

    /// The proxies as the control socket sees them (#2478): whether
    /// playback uses them and the ones being made. Failures are noted as
    /// they come.
    fn publish_proxies(&self) {
        let Ok(mut shared) = self.state.control_shared.proxies.lock() else { return };
        shared.use_proxies = self.state.use_proxies;
        shared.making = self.state.proxy_generating.iter().map(|id| (*id, self.state.proxy_progress.get(id).copied().unwrap_or(0.0))).collect();
    }

    /// A proxy's failure for the control socket, or a new start clearing
    /// the last one.
    fn note_proxy_failure(&self, media_id: MediaId, error: Option<String>) {
        let Ok(mut shared) = self.state.control_shared.proxies.lock() else { return };
        shared.failed.retain(|(id, _)| *id != media_id);
        if let Some(e) = error {
            shared.failed.push((media_id, e));
        }
    }
}

/// The skim and proxy transcodes running together: one per four cores,
/// at least one. Each ffmpeg encode uses several cores and hundreds of
/// megabytes; a project opened with dozens of new files started one each
/// at once (#1360).
pub(super) static TRANSCODE_SLOTS: std::sync::LazyLock<klipp_jobs::Slots> = std::sync::LazyLock::new(|| klipp_jobs::Slots::per_cores(4));

/// `m:ss.s` for a source position in toasts.
pub(super) fn format_profile_time(secs: f64) -> String {
    let secs = secs.max(0.0);
    format!("{}:{:04.1}", (secs / 60.0).floor() as u64, secs % 60.0)
}

/// Where a proxy is written while ffmpeg is still producing it. It is
/// renamed to the real name once whole, so a file at the real name is a
/// finished proxy — never one a crash cut short.
pub(super) fn partial_proxy_path(proxy: &Path) -> PathBuf {
    proxy.with_extension("partial.mp4")
}

/// Whether a file found at the proxy path covers the source: within two
/// seconds or two percent of its duration, whichever is more (the proxy
/// encode may drop or pad a few frames at the end). `None` — the file
/// does not even probe — is not a proxy.
pub(super) fn transcode_is_complete(proxy_secs: Option<f64>, source_secs: f64) -> bool {
    let Some(p) = proxy_secs else { return false };
    let slack = (source_secs * 0.02).max(2.0);
    p + slack >= source_secs
}

/// Whether the skim transcode at `path` covers a source `source_secs`
/// long (see [`transcode_is_complete`]). One header probe. A skim that
/// ends before its media plays as a shorter clip when playback
/// substitutes it, and every hover past its end fails.
pub(super) fn skim_file_is_complete(path: &Path, source_secs: f64) -> bool {
    let probed = klipp_engine::probe_media_header(path).ok().and_then(|h| h.duration_secs);
    let complete = transcode_is_complete(probed, source_secs);
    if !complete {
        tracing::warn!(
            "Skim transcode {} is incomplete ({probed:?} s of {source_secs:.2} s); discarding it",
            path.display()
        );
    }
    complete
}

/// What a finished deflicker or stabilization analysis does: the clip is
/// no longer being measured, and the analysis, kept as its file's asset
/// (M4 step 9), is looked up on the next frame (`ensure_analyses`). One
/// place, so the drain and the inspector parity's result-during-a-drag
/// case apply a result alike; the drain closes an open drag before it
/// (`event_writes`). `false` for any other event.
pub(super) fn apply_analysis_result(state: &mut crate::state::AppState, event: &AppEvent) -> bool {
    match *event {
        AppEvent::DeflickerComplete { clip_id } => {
            state.deflicker_in_progress.remove(&clip_id);
            state.deflicker_progress.remove(&clip_id);
            state.toast.show("Deflicker analysis complete");
        }
        AppEvent::StabilizationComplete { clip_id } => {
            state.stabilization_in_progress.remove(&clip_id);
            state.stabilization_progress.remove(&clip_id);
            state.toast.show("Stabilization analysis complete");
        }
        _ => return false,
    }
    state.analysis_checked = None;
    true
}

/// The skim cache key for a media item, as the transcode names it.
pub(super) fn skim_cache_key_for(item: &klipp_media::MediaItem) -> Option<String> {
    let base = klipp_media::disk_cache::compute_cache_key(&item.path)?;
    let hdr = matches!(item.colour_space, klipp_media::ColourSpace::Hdr10 | klipp_media::ColourSpace::Hlg);
    Some(klipp_media::disk_cache::skim_cache_key(&base, hdr, item.source_rotation))
}

#[cfg(test)]
mod proxy_completeness_tests {
    use super::transcode_is_complete;

    #[test]
    fn a_truncated_transcode_is_not_one() {
        assert!(transcode_is_complete(Some(120.0), 120.0));
        assert!(transcode_is_complete(Some(118.5), 120.0), "a few frames short at the end is the encoder");
        assert!(transcode_is_complete(Some(590.0), 600.0), "two percent on a long clip");
        assert!(!transcode_is_complete(Some(60.0), 120.0), "half the clip is a cut-short generation");
        assert!(!transcode_is_complete(None, 120.0), "a file that does not probe is not one");
    }
}
