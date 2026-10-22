//! What the playback engine reports back each frame: frames shown, playback
//! state, seeks landed, errors, and what follows from them (#826).

use super::*;

impl KlippApp {
    pub(super) fn drain_events(&mut self, ctx: &egui::Context) {
        self.sync_system_theme(ctx);
        self.drain_control_inbox();
        self.apply_analysis_markers();
        self.advance_refits();
        self.propose_audio_roles();
        if std::mem::take(&mut self.state.matched_follow_sync) {
            self.sync_sequence_to_engine();
        }
        while let Ok(event) = self.event_rx.try_recv() {
            if super::event_writes::event_writes(&event) {
                self.close_drag_for_result();
            }
            match event {
                AppEvent::PlaybackStarted => {
                    self.state.playback = PlaybackState::Playing;
                    self.state.playhead_anchor = self.state.playhead;
                    self.state.playhead_anchor_wall = std::time::Instant::now();
                    if self.prefs.editing.playhead_follows_playback
                        && self.state.playhead_follow == crate::state::PlayheadFollow::Off
                        && !self.state.playhead_follow_chosen
                    {
                        self.state.playhead_follow = crate::state::PlayheadFollow::Page;
                    }
                }
                AppEvent::PlaybackPaused => {
                    self.state.playback = PlaybackState::Paused;
                }
                // Render In to Out (#924).
                AppEvent::RenderRangeProgress { done, total } => {
                    self.state.render_range = Some((done, total));
                }
                AppEvent::RenderRangeFinished { rendered, passed_over, cancelled, note } => {
                    self.state.render_range = None;
                    let frames = |n: usize| if n == 1 { "1 frame".to_string() } else { format!("{n} frames") };
                    let mut said = match (&note, cancelled) {
                        (Some(why), _) => format!("Nothing rendered: {why}"),
                        (None, true) => format!("Render stopped, {} rendered", frames(rendered)),
                        (None, false) => format!("Rendered {}", frames(rendered)),
                    };
                    if passed_over > 0 {
                        said.push_str(&format!(" ({} passed over: changed or unreadable)", frames(passed_over)));
                    }
                    self.state.toast.show(said);
                }
                AppEvent::NodeInputSampled { clip_id, node_id, nx, ny, sample } => {
                    match self.state.hue_curve_awaiting {
                        Some((c, n, curve)) if (c, n) == (clip_id, node_id) => {
                            self.state.hue_curve_awaiting = None;
                            self.range_hue_curve_on_sample(clip_id, node_id, curve, nx, ny, sample);
                        }
                        _ => {
                            if !self.stroke_sample(clip_id, node_id, nx, ny, sample.clone()) {
                                self.key_qualifier_on_sample(clip_id, node_id, nx, ny, sample);
                            }
                        }
                    }
                }
                AppEvent::PlaybackStopped => {
                    self.state.playback = PlaybackState::Stopped;
                }
                AppEvent::SourcePlaybackStarted => {
                    self.state.source_playback = PlaybackState::Playing;
                }
                AppEvent::SourcePlaybackPaused => {
                    self.state.source_playback = PlaybackState::Paused;
                }
                AppEvent::SourcePlayheadMoved(secs) => {
                    let rate = self.state.fps.rate();
                    self.state.source_playhead = Timecode::from_secs_f64(secs, rate);
                }
                AppEvent::LoopedBack => {
                    // Engine looped back — the following PlayheadMoved carries the new position.
                    // Pre-reset smooth playhead so it doesn't animate from old position.
                    // The PlayheadMoved handler will set the final anchor.
                }
                AppEvent::PlayheadMoved(tc) => {
                    let prev = self.state.playhead;
                    if self.state.playback == PlaybackState::Playing {
                        // During playback: only update anchor, don't touch playhead
                        self.state.playhead_anchor = tc;
                        self.state.playhead_anchor_wall = std::time::Instant::now();
                    } else {
                        // When not playing: set playhead directly
                        self.state.playhead = tc;
                        self.state.playhead_smooth_secs = tc.to_secs_f64();
                        self.state.playhead_anchor = tc;
                        self.state.playhead_anchor_wall = std::time::Instant::now();
                    }
                    let delta_ms = (tc.to_secs_f64() - prev.to_secs_f64()).abs() * 1000.0;
                    if delta_ms > 50.0 {
                        tracing::info!(
                            "PLAYHEAD_DIAG event=PlayheadMoved prev={:.3}s new={:.3}s delta={:.1}ms state={:?}",
                            prev.to_secs_f64(), tc.to_secs_f64(), delta_ms, self.state.playback,
                        );
                    }
                }
                AppEvent::MediaLoaded(payload) if self.state.retired_media_ids.contains(&payload.media_id) => {
                    tracing::debug!(
                        "Dropping the probe of {:?}: its project is no longer open",
                        payload.path.file_name().unwrap_or_default(),
                    );
                }
                AppEvent::MediaLoaded(payload) => {
                    let klipp_core::MediaLoadedPayload {
                        media_id,
                        path,
                        duration,
                        fps,
                        resolution,
                        video_codec,
                        audio_codec,
                        has_audio,
                        file_size_bytes,
                        sample_rate,
                        audio_channels,
                        is_still,
                        file_modified_at,
                        media_created_at,
                        colour_space,
                        source_rotation,
                        camera_make,
                        camera_model,
                        encoder,
                        bit_depth,
                        video_bitrate_kbps,
                        audio_bitrate_kbps,
                        color_primaries,
                        color_transfer,
                        pixel_format,
                        shot,
                        truncated_from_secs,
                    } = *payload;
                    let filename = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.to_string_lossy().to_string());
                    let _is_audio_only = video_codec.is_empty();

                    // For still images: override duration and fps with defaults
                    let (duration, fps, is_still) = if is_still {
                        let still_dur_secs = self.prefs.editing.default_still_duration_secs;
                        let still_fps = klipp_core::Fps::pal_25(); // sensible default
                        let still_duration = klipp_core::RationalTime::from_secs_f64(
                            still_dur_secs,
                            still_fps.rate(),
                        );
                        (still_duration, still_fps, true)
                    } else {
                        (duration, fps, false)
                    };

                    let mut media_item = MediaItem {
                        id: media_id,
                        path: path.clone(),
                        kind: klipp_media::MediaItemKind::File,
                        filename,
                        duration,
                        fps,
                        resolution,
                        video_codec,
                        audio_codec,
                        has_audio,
                        file_size_bytes,
                        sample_rate,
                        audio_channels,
                        offline: false,
                        is_still,
                        proxy_path: None,
                        proxy_ready: false,
                        default_source_in: None,
                        default_source_out: None,
                        // Copy star rating from file browser cache (if the user
                        // rated this file before importing it to the pool).
                        rating: self.browser.file_ratings.get(&path).copied().unwrap_or(0),
                        tags: Vec::new(),
                        user_notes: String::new(),
                        bin: None,
                        bin_id: None,
                        parent_media_id: None,
                        added_at: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0),
                        fingerprint: compute_media_fingerprint(&path),
                        file_modified_at,
                        media_created_at,
                        color_label: Default::default(),
                        colour_space: klipp_media::ColourSpace::from_u8(colour_space),
                        needs_reprobe: false,
                        log_format: Default::default(),
                        source_rotation,
                        camera_make,
                        camera_model,
                        encoder,
                        bit_depth,
                        video_bitrate_kbps,
                        audio_bitrate_kbps,
                        color_primaries,
                        color_transfer,
                        pixel_format,
                        shot,
                        metadata_read: true,
                        truncated_from_secs,
                        detected_log_format: Default::default(),
                        log_format_user_set: false,
                        skim_file: None,
                        waveform_data: None,
                        audio_cache: None,
                        log: Default::default(),
                        synced_audio: None,
                        markers: Vec::new(),
                        screen_text: String::new(),
                        audio_role: None,
                        span: None,
                    };

                    // Auto-detect log format from metadata
                    let detected = media_item.detect_log_format();
                    if detected != klipp_core::LogFormat::Srgb {
                        tracing::info!("Auto-detected log format {:?} for {:?}", detected, path.file_name().unwrap_or_default());
                    }
                    media_item.detected_log_format = detected;

                    if source_rotation != 0 {
                        tracing::info!("Media {:?} has source rotation: {}°", path.file_name().unwrap_or_default(), source_rotation);
                    }

                    // A re-probe (relink, or a file edited outside Klipp)
                    // keeps what the user said about the item and, when
                    // the file itself is unchanged, the caches built from
                    // it. Replacing the item wholesale lost the rating,
                    // tags and bin on every relink.
                    let previous = self.state.media_pool.remove(&media_id);
                    let file_changed = previous.as_ref().is_some_and(|old| !klipp_media::same_file_as_probed(old, &media_item));
                    if let Some(old) = &previous {
                        klipp_media::merge_reprobed(old, &mut media_item);
                    }
                    // A re-probe that finds the item as it was stored, as
                    // every open's does for an unchanged file, leaves
                    // nothing to save (#1353).
                    let saves_alike = previous.as_ref().is_some_and(|old| klipp_project::media_items_save_alike(old, &media_item));
                    let filmstrips_enabled = self.state.project_auto_filmstrips
                        .unwrap_or(self.prefs.editing.auto_generate_filmstrips);
                    let skim_enabled = self.state.project_auto_skim
                        .unwrap_or(self.prefs.editing.auto_generate_skim);
                    if let Some(claimed) = media_item.truncated_from_secs {
                        let held = media_item.duration.to_secs_f64();
                        self.state.toast.show(format!(
                            "{} is cut short: it holds {} of the {} its header claims",
                            media_item.filename,
                            klipp_media::describe_secs(held),
                            klipp_media::describe_secs(claimed),
                        ));
                    }
                    let span_path = media_item.path.clone();
                    // What is in the footage, found in the background as it
                    // arrives: shots, loudness, silence. Stored by the file's
                    // cache key, so a file seen before costs nothing.
                    let skip_analysis = self.state.pending_skip_analysis.remove(&media_id);
                    if media_item.kind == klipp_media::MediaItemKind::File && !skip_analysis {
                        // With the brief's spellings and language, when there is one.
                        self.apply_brief_to_transcription();
                        klipp_analysis::service::request(&media_item);
                    }
                    self.state.complete_pool_import(media_item, previous.is_none());
                    self.refresh_spans_beside(&span_path);
                    // A recorded take lands where it was punched in (#114).
                    self.place_take(media_id);
                    if !saves_alike {
                        self.state.dirty = true;
                    }
                    self.sync_media_pool_to_engine();
                    if previous.is_some() {
                        tracing::info!(
                            "Reloaded {:?}: {}",
                            path.file_name().unwrap_or_default(),
                            if file_changed { "the file changed; its caches are rebuilt" } else { "the file is unchanged" }
                        );
                        if file_changed {
                            // The in-memory pictures were of the old
                            // file; the disk caches are content-keyed and
                            // miss on their own.
                            self.state.thumbnail_cache.set_filmstrip(media_id, Vec::new());
                            self.state.filmstrip_in_progress.remove(&media_id);
                            self.spawn_thumbnail(media_id, path.clone(), duration);
                            if filmstrips_enabled {
                                self.spawn_filmstrip(media_id, path.clone(), duration);
                            }
                            if has_audio {
                                self.spawn_audio_conform(media_id, path.clone());
                            }
                            if skim_enabled {
                                self.state.pending_skim_transcodes.push(media_id);
                            }
                        }
                        self.state.pending_media_path = None;
                    } else {
                        // A new file in the pool leaves the sequence's rate
                        // alone: setting it from the file showed a 30 fps
                        // sequence as 24 once 24 fps clips came in (#2356).
                        self.state.playback = PlaybackState::Stopped;

                        // Record as recently-used for the file browser Recents view.
                        self.prefs.file_browser.add_recent_file(path.clone());
                        self.state.recent_files_snapshot = self.prefs.file_browser.recent_files.clone();
                        self.prefs_dirty = true;

                        // Clear pending path — media goes to pool only.
                        // User adds to timeline via double-click or drag-and-drop.
                        if self.state.pending_media_path.is_some() {
                            self.state.pending_media_path = None;
                        }

                        // If this import asked for a timeline destination
                        // (the Audio Library's "Add at playhead" / "Add to
                        // timeline"), drop it now that it is in the pool.
                        // Through the UI's own router: `cmd_tx` goes to the
                        // engine, which has no handler for placing media,
                        // so a send there was silently dropped.
                        if let Some(drop) = self.state.pending_media_drop.remove(&media_id) {
                            self.send_commands(vec![AppCommand::PlaceMedia { media_id, drop }]);
                        }

                        // Start thumbnail (always) + optional filmstrip/audio/skim generation
                        self.spawn_thumbnail(media_id, path.clone(), duration);
                        if filmstrips_enabled {
                            self.spawn_filmstrip(media_id, path.clone(), duration);
                            self.state.filmstrip_generating = true;
                            self.state.filmstrip_generate_start = std::time::Instant::now();
                            self.state.filmstrip_toast_dismissed = false;
                        }
                        if has_audio {
                            self.spawn_audio_conform(media_id, path.clone());
                        }
                        if skim_enabled {
                            self.state.pending_skim_transcodes.push(media_id);
                        }

                        // Auto-generate proxy for heavy media (disabled by default, opt-in via preferences)
                        if self.prefs.proxy.auto_generate_on_import && !is_still {
                            // Only proxy media that benefits: 4K+ or high bitrate (>100 Mbps)
                            let needs_proxy = resolution.width >= 3840
                                || resolution.width * resolution.height > 3840 * 2160
                                || video_bitrate_kbps.unwrap_or(0) > 100_000;
                            if needs_proxy {
                                self.spawn_proxy_generation(media_id);
                            }
                        }
                    }
                }
                AppEvent::MediaLoadFailed { media_id, path, error } => {
                    self.state.pending_undo_imports.remove(&media_id);
                    self.state.pending_skip_analysis.remove(&media_id);
                    self.state.pending_media_path = None;
                    tracing::error!("Failed to load media: {error}");
                    // Said, not only logged: an import that did nothing
                    // looked like one still on its way (#654).
                    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.display().to_string());
                    self.state.toast.show(format!("Could not import {name}: {error}"));
                }
                AppEvent::FrameReady => {
                    self.upload_frame_to_egui(ctx);
                    self.state.perf_frame_count += 1;
                    ctx.request_repaint();
                }
                AppEvent::ClipAppended { .. }
                | AppEvent::ClipRemoved { .. }
                | AppEvent::ClipMoved { .. }
                | AppEvent::ClipTrimmed { .. }
                | AppEvent::ClipSelected(_) => {}
                AppEvent::ProjectSaved { .. }
                | AppEvent::ProjectLoaded { .. }
                | AppEvent::ProjectError { .. } => {}
                AppEvent::ExportProgress { .. }
                | AppEvent::ExportComplete { .. }
                | AppEvent::ExportError { .. } => {
                    // Export events come through export_event_rx channel
                }
                AppEvent::ThumbnailReady { .. } => {
                    // Thumbnails come through thumb_rx channel instead
                }
                AppEvent::MatteMaskReady { consumer_clip_id, rgba, width, height } => {
                    // Phase 4C: install the latest matte preview into a
                    // texture handle stashed alongside the consumer
                    // clip-id. Old handle goes through defer_drop_texture
                    // because dropping a TextureHandle inline on
                    // eframe-wgpu trips "Texture has been destroyed"
                    // (see feedback_defer_drop_texture).
                    let stale = self.state.matte_thumb_handle.take();
                    if let Some(h) = stale {
                        self.defer_drop_texture(h);
                    }
                    let pixels: Vec<egui::Color32> = rgba
                        .chunks_exact(4)
                        .map(|c| egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]))
                        .collect();
                    let img = egui::ColorImage {
                        size: [width as usize, height as usize],
                        pixels,
                    };
                    let tex = ctx.load_texture(
                        format!("matte_thumb_{}", consumer_clip_id.0.as_simple()),
                        img,
                        egui::TextureOptions::NEAREST,
                    );
                    self.state.matte_thumb_for = Some(consumer_clip_id);
                    self.state.matte_thumb_handle = Some(tex);
                }
                AppEvent::EngineStats { dropped_frames, hw_decode } => {
                    self.state.perf_dropped_frames = dropped_frames;
                    self.state.perf_hw_decode = hw_decode;
                }
                AppEvent::SourceFrameReady
                | AppEvent::SourceMediaLoaded { .. }
                | AppEvent::ProxyGenerationStarted { .. }
                | AppEvent::ProxyGenerationComplete { .. }
                | AppEvent::ProxyGenerationFailed { .. }
                | AppEvent::AudioConformStarted { .. }
                | AppEvent::AudioConformComplete { .. }
                | AppEvent::AudioConformFailed { .. } => {
                    // Audio conform events come through audio_conform_rx channel
                }
                AppEvent::TrackPeakLevels { track_index, left, right } => {
                    // Grow peak levels vec if needed
                    while self.state.track_peak_levels.len() <= track_index {
                        self.state.track_peak_levels.push((0.0, 0.0));
                    }
                    let (ref mut dl, ref mut dr) = self.state.track_peak_levels[track_index];
                    // Peak hold: only update if new value is higher (decay happens per-frame)
                    *dl = dl.max(left);
                    *dr = dr.max(right);
                }
                AppEvent::MasterPeakLevels { left, right } => {
                    self.state.master_peak_left = self.state.master_peak_left.max(left);
                    self.state.master_peak_right = self.state.master_peak_right.max(right);
                    // Latch clip indicator if peak exceeds 1.0 (0 dBFS)
                    if left >= 1.0 || right >= 1.0 {
                        self.state.clip_indicator = true;
                    }
                }
                AppEvent::DeliveryLoudness { target_lufs, gain_db, export_gain_db, measuring } => {
                    let d = &mut self.state.delivery_loudness;
                    d.target_lufs = target_lufs;
                    d.measuring = measuring;
                    // A measurement starting keeps the gain playback has.
                    if !measuring {
                        d.gain_db = gain_db;
                        d.export_gain_db = export_gain_db;
                    }
                }
                AppEvent::LufsReadings { momentary, short_term, integrated, loudness_range, true_peak_dbfs } => {
                    self.state.lufs_momentary = momentary;
                    self.state.lufs_short_term = short_term;
                    self.state.lufs_integrated = integrated;
                    self.state.lufs_lra = loudness_range;
                    self.state.lufs_true_peak_dbfs = true_peak_dbfs;
                    // Append to the rolling history ring for the loudness
                    // history scope. Cap at 600 = ~60s at 10Hz; the LUFS
                    // meter hops every 100ms so the cadence is stable.
                    self.state.lufs_short_term_history.push_back(short_term);
                    if self.state.lufs_short_term_history.len() > 600 {
                        self.state.lufs_short_term_history.pop_front();
                    }
                    // Latch clip indicator if true peak exceeds 0 dBFS
                    if true_peak_dbfs > 0.0 {
                        self.state.clip_indicator = true;
                    }
                }
                // Skim/stabilization events come through proxy_event_rx, handled in drain_proxy_events
                AppEvent::SkimTranscodeProgress { .. }
                | AppEvent::SkimTranscodeComplete { .. }
                | AppEvent::SkimTranscodeFailed { .. }
                | AppEvent::BrowserSkimFrameReady { .. }
                | AppEvent::MediaFileChanged { .. }
                | AppEvent::DeflickerProgress { .. }
                | AppEvent::DeflickerComplete { .. }
                | AppEvent::DeflickerFailed { .. }
                | AppEvent::StabilizationProgress { .. }
                | AppEvent::StabilizationComplete { .. }
                | AppEvent::StabilizationFailed { .. }
                | AppEvent::TrackingProgress { .. }
                | AppEvent::TrackingComplete { .. }
                | AppEvent::TrackingFailed { .. }
                | AppEvent::SurfaceTracked { .. }
                | AppEvent::SurfaceTrackFailed { .. }
                | AppEvent::SurfaceSnapped { .. }
                | AppEvent::SceneDetectProgress { .. }
                | AppEvent::SceneDetectComplete { .. }
                | AppEvent::SceneDetectFailed { .. } => {}
                // Audio sync events come through proxy_event_rx (our generic
                // background-job channel) and are handled in drain_proxy_events.
                AppEvent::AudioSyncComplete { .. }
                | AppEvent::AudioSyncFailed { .. }
                | AppEvent::MediaSyncComplete { .. }
                | AppEvent::MediaSyncFailed { .. }
                | AppEvent::MulticamSyncReady { .. }
                | AppEvent::DenoiserProfileLearned { .. }
                | AppEvent::DenoiserProfileFailed { .. } => {}
            }
        }

        // ── Audio spectrum: refresh FFT bins once per UI frame ──
        //
        // Runs after all events drain so the PCM ring has its latest samples.
        // `try_lock` both on the ring (non-blocking vs. the mix thread) and
        // on the computed bins so the UI never stalls behind the audio
        // callback. When the ring is empty (silence / paused) the previous
        // bins hold — giving a natural "freeze on pause" visual.
        {
            const FFT_SIZE: usize = 2048;
            const NUM_BINS: usize = 256;
            const SMOOTH: f32 = 0.25; // 0 = instant, 1 = frozen
            // Scratch buffer — avoids allocating in hot path but keeps the
            // spectrum module's signature ergonomic. Kept on the stack.
            let mut new_bins = [0.0f32; NUM_BINS];
            let have_samples = if let Ok(ring) = self.state.spectrum_ring.try_lock() {
                if ring.len() >= 2 {
                    klipp_audio::spectrum::compute_spectrum(
                        &ring,
                        self.state.spectrum_sample_rate,
                        FFT_SIZE,
                        NUM_BINS,
                        &mut new_bins,
                    );
                    true
                } else {
                    false
                }
            } else {
                false
            };
            if have_samples {
                // Ensure destination is sized correctly (bootstrap).
                if self.state.spectrum_bins.len() != NUM_BINS {
                    self.state.spectrum_bins = vec![-96.0f32; NUM_BINS];
                }
                for (old, new) in self.state.spectrum_bins.iter_mut().zip(new_bins.iter()) {
                    *old = *old * (1.0 - SMOOTH) + *new * SMOOTH;
                }

                // ── Spectrogram history slice ──
                //
                // Store the raw dBFS bins (clamped to -96..=0 by
                // `compute_spectrum`). The per-pane normalisation from
                // dB floor → [0, 1] now happens in render_audio_spectrogram
                // so that each pane's `spectrogram_db_floor` pref (F-156)
                // can independently set its contrast range. Cap history
                // at 300 slices ≈ ~10 s at 30 fps.
                let slice: Vec<f32> = self.state.spectrum_bins.clone();
                self.state.spectrum_history.push_back(slice);
                if self.state.spectrum_history.len() > 300 {
                    self.state.spectrum_history.pop_front();
                }
            }
        }
    }
}
