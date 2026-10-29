//! Background work on media: thumbnails, filmstrips and their queue, skim
//! frames and skim transcodes, the source viewer's frame, effect thumbnails (#826).

use super::*;

/// How wide a media item's poster thumbnail is decoded: the cache
/// preferences' thumbnail size, kept to a sane range.
pub(super) fn poster_width(prefs: &crate::prefs::MediaCachePrefs) -> u32 {
    prefs.thumbnail_height.clamp(160, 1920)
}

impl KlippApp {
    pub(super) fn spawn_thumbnail(
        &mut self,
        media_id: MediaId,
        path: PathBuf,
        duration: Timecode,
    ) {
        self.state.thumbnail_cache.set_pending(media_id);
        let tx = self.thumb_tx.clone();
        let duration_secs = duration.to_secs_f64();
        let seek_secs = duration_secs * 0.1;
        let cache_key = klipp_engine::video_decoder::picture_cache_key(&path);
        // Decode the media-pool poster at the cache preferences' size,
        // 480 wide unless changed, the Files browser's size. A small size
        // (160 px once) gave 50×90 thumbs for 4K portrait clips, which then
        // bilinear-upscaled to ~300×500 in the UI — visibly blurry.
        let max_width = poster_width(&self.prefs.media_cache);
        std::thread::spawn(move || {
            // Try loading from disk cache first (skip if file has rotation — regenerate to apply it)
            let has_rotation = klipp_engine::detect_rotation_from_path(&path) != 0;
            if let Some(ref key) = cache_key {
                if !has_rotation && klipp_media::disk_cache::thumbnail_cache_exists(key, max_width) {
                    match klipp_media::disk_cache::load_thumbnail_cache(key, max_width) {
                        Ok((rgba, w, h)) => {
                            tracing::info!("Thumbnail loaded from cache for {:?}", path);
                            let _ = tx.send(ThumbnailResult {
                                media_id,
                                rgba,
                                width: w,
                                height: h,
                            });
                            return;
                        }
                        Err(e) => {
                            tracing::warn!("Thumbnail cache load failed, regenerating: {e}");
                        }
                    }
                }
            }

            // Generate from source
            match klipp_engine::generate_thumbnail_at_size_non_black(&path, seek_secs, duration_secs, max_width) {
                Ok((rgba, w, h)) => {
                    // Write to cache
                    if let Some(ref key) = cache_key {
                        if let Err(e) = klipp_media::disk_cache::write_thumbnail_cache(key, max_width, &rgba, w, h) {
                            tracing::warn!("Failed to write thumbnail cache: {e}");
                        }
                    }
                    let _ = tx.send(ThumbnailResult {
                        media_id,
                        rgba,
                        width: w,
                        height: h,
                    });
                }
                Err(e) => {
                    tracing::warn!("Thumbnail generation failed for {:?}: {}", media_id, e);
                }
            }
        });
    }

    /// Drop a media item's thumbnail and filmstrip and request them again —
    /// from the disk cache when pictures for its current decode settings
    /// exist, freshly decoded otherwise.
    pub(super) fn refresh_media_pictures(&mut self, media_id: MediaId) {
        let Some((path, duration)) = self.state.media_pool.get(&media_id)
            .map(|m| (m.path.clone(), m.duration))
        else {
            return;
        };
        let stale = self.state.forget_media_pictures(media_id);
        self.defer_drop_textures(stale);
        self.spawn_thumbnail(media_id, path.clone(), duration);
        self.spawn_filmstrip(media_id, path, duration);
    }

    /// Queue a filmstrip for generation. Deduplicates and serializes — only one
    /// filmstrip generates at a time to avoid exhausting GPU NVDEC sessions.
    pub(super) fn spawn_filmstrip(
        &mut self,
        media_id: MediaId,
        path: PathBuf,
        duration: Timecode,
    ) {
        // Deduplicate: skip if already in progress or already queued
        if self.state.filmstrip_in_progress.contains(&media_id) {
            return;
        }
        if self.state.filmstrip_queue.iter().any(|(id, _, _)| *id == media_id) {
            return;
        }
        // Already have filmstrip frames in memory? Skip.
        if self.state.thumbnail_cache.filmstrip(&media_id).is_some_and(|f| !f.is_empty()) {
            return;
        }
        // Already have filmstrip on disk? Load it directly without showing the spinner.
        let filmstrip_height = self.prefs.media_cache.filmstrip_height.max(24);
        let cache_key = klipp_engine::video_decoder::picture_cache_key(&path);
        if let Some(ref key) = cache_key {
            if klipp_media::disk_cache::filmstrip_cache_exists(key, filmstrip_height) {
                // Load from disk cache on a background thread, skip the generating indicator
                let tx = self.filmstrip_tx.clone();
                let key_clone = key.clone();
                let path_clone = path.clone();
                std::thread::Builder::new()
                    .name("klipp-filmstrip-cache".into())
                    .spawn(move || {
                        match klipp_media::disk_cache::load_filmstrip_cache(&key_clone, filmstrip_height) {
                            Ok(frames) => {
                                tracing::info!("Filmstrip loaded from disk cache: {} frames for {:?}", frames.len(), path_clone);
                                for frame in frames {
                                    let _ = tx.send(FilmstripResult::Frame { media_id, frame });
                                }
                                let _ = tx.send(FilmstripResult::Done { media_id });
                            }
                            Err(e) => {
                                // Reported as unreadable, not as done:
                                // "done" left the clip with no filmstrip
                                // for the rest of the session, and the
                                // same corrupt file the next one.
                                tracing::warn!("Filmstrip disk cache load failed, regenerating: {e}");
                                klipp_media::disk_cache::remove_filmstrip_cache(&key_clone);
                                let _ = tx.send(FilmstripResult::CacheUnreadable { media_id });
                            }
                        }
                    })
                    .is_ok()
                    .then(|| self.filmstrip_cache_loads.insert(media_id));
                return;
            }
        }

        self.state.filmstrip_queue.push((media_id, path, duration));
        self.state.filmstrip_total_queued += 1;

        // Pump the queue if nothing is currently generating
        self.pump_filmstrip_queue();
    }

    /// Start the next filmstrip generation if nothing is in progress.
    fn pump_filmstrip_queue(&mut self) {
        if !self.state.filmstrip_in_progress.is_empty() {
            return; // One is already running
        }
        let Some((media_id, path, duration)) = self.state.filmstrip_queue.first().cloned() else {
            return;
        };
        self.state.filmstrip_queue.remove(0);
        self.state.filmstrip_in_progress.insert(media_id);

        let tx = self.filmstrip_tx.clone();
        let duration_secs = duration.to_secs_f64();
        let cache_key = klipp_engine::video_decoder::picture_cache_key(&path);
        let generate_if_missing = self.state.project_auto_filmstrips
            .unwrap_or(self.prefs.editing.auto_generate_filmstrips);
        // Target ~400 frames across the whole clip. The 0.2 s floor
        // keeps very short clips from producing too many closely-
        // spaced tiles. No upper clamp on interval — long clips widen
        // the interval so coverage always spans the full duration
        // instead of truncating at the engine's 500-frame cap (which
        // previously left the tail of any clip > ~10 min with no
        // filmstrip tiles at all). Zoom-in detail for any section is
        // added on demand by RefineFilmstrip.
        let interval = (duration_secs / 400.0).max(0.2);
        let filmstrip_height = self.prefs.media_cache.filmstrip_height.max(24);
        // Seed the per-job progress counter so the status bar shows
        // "N/total" from the very first frame. The engine still caps
        // at 500 as a safety ceiling; with the widened interval above
        // the expected count only approaches 500 on short clips where
        // the 0.2 s floor drives it up.
        let expected_frames = ((duration_secs / interval).ceil() as u32).clamp(1, 500);
        self.state.filmstrip_progress.insert(media_id, (0, expected_frames));
        // Wrap in a ThreadJob so the JobScheduler can pause us when
        // foreground playback / scrubbing kicks in. The pause check
        // lives inside `generate_filmstrip_incremental`'s per-frame
        // loop; this thread blocks on it cooperatively.
        let path_for_job = path.clone();
        let job = klipp_jobs::ThreadJob::spawn(
            format!("filmstrip:{}", path_for_job.display()),
            klipp_jobs::JobPriority::Background,
            klipp_jobs::ResourceClass::Nvdec,
            move |pause_token, _progress| {
                // Try loading from disk cache first — fast path,
                // doesn't touch the decoder or pause point.
                if let Some(ref key) = cache_key {
                    if klipp_media::disk_cache::filmstrip_cache_exists(key, filmstrip_height) {
                        match klipp_media::disk_cache::load_filmstrip_cache(key, filmstrip_height) {
                            Ok(frames) => {
                                tracing::info!("Filmstrip loaded from cache: {} frames for {:?}", frames.len(), path);
                                for frame in frames {
                                    let _ = tx.send(FilmstripResult::Frame {
                                        media_id,
                                        frame,
                                    });
                                }
                                let _ = tx.send(FilmstripResult::Done { media_id });
                                return;
                            }
                            Err(e) => {
                                tracing::warn!("Filmstrip cache load failed, regenerating: {e}");
                            }
                        }
                    }
                }

                // Skip fresh generation if preference is off (cache was already checked above)
                if !generate_if_missing {
                    tracing::info!("Filmstrip generation disabled by preference, skipping {:?}", path);
                    let _ = tx.send(FilmstripResult::Done { media_id });
                    return;
                }

                // Generate from source and collect frames for cache write
                let frames = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
                let frames_clone = frames.clone();
                // Clone the token before handing one copy to the
                // generator — we need the original to check
                // `is_cancelled()` after the call returns. Without
                // this guard a clip removed mid-strip writes a
                // partial cache that the next session mistakes for
                // a complete strip.
                let pause_token_for_check = Arc::clone(&pause_token);
                match klipp_engine::generate_filmstrip_incremental(
                    &path,
                    duration_secs,
                    interval,
                    filmstrip_height,
                    |time, rgba, w, h| {
                        let frame = klipp_media::FilmstripFrame {
                            time_secs: time,
                            rgba,
                            width: w,
                            height: h,
                        };
                        frames_clone.lock().unwrap_or_else(|e| e.into_inner()).push(frame.clone());
                        let _ = tx.send(FilmstripResult::Frame {
                            media_id,
                            frame,
                        });
                    },
                    Some(pause_token),
                ) {
                    Ok(outcome) => {
                        let count = outcome.tiles;
                        // Skip the cache write when the generator
                        // exited early due to cancellation — partial
                        // caches would otherwise pose as complete on
                        // next launch and never get re-generated.
                        if pause_token_for_check.is_cancelled() {
                            tracing::info!(
                                "Filmstrip: cancelled at {count} frames for {:?} — skipping partial cache write",
                                path,
                            );
                            let _ = tx.send(FilmstripResult::Done { media_id });
                            return;
                        }
                        // The same for a strip with no tiles: the decoder
                        // opened the file and then could not make a single
                        // one. Cached, that would be a clip with no
                        // filmstrip for ever; not cached, the next launch
                        // tries again.
                        if count == 0 && !outcome.skipped.is_empty() {
                            let (t, why) = &outcome.skipped[0];
                            tracing::warn!(
                                "Filmstrip: no tiles at all for {:?} (first at {t:.2}s: {why}) — not caching",
                                path,
                            );
                            let _ = tx.send(FilmstripResult::Done { media_id });
                            return;
                        }
                        tracing::info!("Filmstrip: generated {count} frames for {:?}", path);
                        // Write to cache
                        if let Some(ref key) = cache_key {
                            let cached_frames = frames.lock().unwrap_or_else(|e| e.into_inner());
                            if let Err(e) = klipp_media::disk_cache::write_filmstrip_cache(key, &cached_frames) {
                                tracing::warn!("Failed to write filmstrip cache: {e}");
                            } else {
                                tracing::info!("Filmstrip cache written: {} frames", cached_frames.len());
                            }
                        }
                        let _ = tx.send(FilmstripResult::Done { media_id });
                    }
                    Err(e) => {
                        tracing::warn!("Filmstrip generation failed for {:?}: {}", media_id, e);
                        let _ = tx.send(FilmstripResult::Done { media_id });
                    }
                }
            },
        );
        self.job_scheduler.submit(job as Arc<dyn klipp_jobs::BackgroundJob>);
    }

    /// Point `media_id` at `new_path` and re-probe it. Whatever was
    /// derived from the old file — proxy, skim, waveform, conform — is
    /// dropped here and the proxy file removed: the proxy is named by
    /// media id, and left in place it would be picked up as "already
    /// generated" for the new file. The re-probe's `MediaLoaded` then
    /// merges the new facts into the item, keeping the user's metadata.
    ///
    /// Only an offline item used to be re-probed; relinking an online
    /// clip to a different file kept the old duration, frame rate and
    /// resolution.
    /// The height to decode a skim frame at: the source's own height,
    /// never above 1080, and capped by the `skim_resolution` preference.
    ///
    /// `min`, not `max`. Written as `pref.max(source_height.min(1080))`
    /// the preference could only ever *raise* the height, so it was inert
    /// for every source at or above its own value — at the 720 default,
    /// every source from 720p up, which is most footage. "Resolution (max
    /// height in pixels) for skim frame decodes" has to be able to cap.
    ///
    /// The 240 floor is what `max` was accidentally providing: the
    /// preference has no UI, only a line in the prefs file, and nothing
    /// else stops a hand-edited nonsense value decoding a 2px frame into
    /// the viewer.
    pub(super) fn skim_decode_height(source_height: u32, pref: u32) -> u32 {
        source_height.min(1080).min(pref.max(240))
    }

    /// The timeline skim position queued behind the one just answered, if
    /// any, sent to the engine (#2945).
    pub(super) fn send_queued_timeline_skim(&mut self) {
        if let Some((time, raw)) = self.state.timeline_skim_queued.take() {
            self.state.timeline_skim_in_flight = Some((std::time::Instant::now(), raw));
            let _ = self.cmd_tx.send(AppCommand::SkimTimeline { time });
        }
    }

    /// Ask the skim worker for `media_id`'s frame at `time_secs`, from
    /// the skim transcode when there is one. Marks the request in flight;
    /// the frame (or an empty one on failure) clears it.
    pub(super) fn request_skim_frame(&mut self, media_id: MediaId, time_secs: f64, target: klipp_core::ViewerMode) {
        let Some(item) = self.state.media_pool.get(&media_id) else { return };
        // The frame last asked for is the one on screen. A pointer resting
        // on a clip keeps the UI redrawing, and every redraw used to ask
        // again: each repeat was a fresh seek that could land on a
        // neighbouring frame, so the viewer flickered.
        let frame = klipp_engine::skim::frame_index(time_secs, item.fps.as_f64());
        if self.state.skim_last_requested == Some((media_id, frame, target)) {
            return;
        }
        let (decode_path, fallback_path) = match item.skim_file.as_ref().filter(|p| p.exists()) {
            Some(skim) => (skim.clone(), Some(item.path.clone())),
            None => (item.path.clone(), None),
        };
        let rotation = item.effective_rotation_for(&decode_path);
        let source_height = item.resolution.height.max(240);
        let max_height =
            Self::skim_decode_height(source_height, self.prefs.playback.skim_resolution);
        self.state.skim_decode_in_flight = true;
        self.state.skim_decode_started_at = Some(std::time::Instant::now());
        self.state.skim_last_requested = Some((media_id, frame, target));
        self.skim_worker.request(klipp_engine::skim::SkimJob {
            path: decode_path,
            fallback_path,
            rotation,
            secs: time_secs,
            max_height,
            tag: (media_id, target),
        });
    }

    pub(super) fn start_source_frame_decode_at(&self, media_id: MediaId, seek_secs: f64) {
        let Some(item) = self.state.media_pool.get(&media_id) else { return; };
        if item.offline || !item.path.exists() {
            return;
        }
        // Audio-only media has no video frame to decode. (A waveform
        // display for audio-only sources is a future enhancement.)
        if item.video_codec.is_empty()
            || item.resolution.width == 0
            || item.resolution.height == 0
        {
            return;
        }
        let path = item.path.clone();
        let tx = self.source_frame_tx.clone();
        std::thread::spawn(move || {
            // Decode at 1920px on the long edge — source rect is at most
            // half the center column, so anything bigger is wasted upload
            // bandwidth. ffmpeg's seek lands on the nearest keyframe;
            // generate_thumbnail_at_size handles the precise-seek dance
            // internally for us.
            match klipp_engine::generate_thumbnail_at_size(&path, seek_secs, 1920) {
                Ok((rgba, width, height)) => {
                    let _ = tx.send(SourceFrameResult {
                        media_id,
                        rgba,
                        width,
                        height,
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        "Source frame decode failed for {:?} @ {:.3}s: {}",
                        path,
                        seek_secs,
                        e
                    );
                }
            }
        });
    }

    /// Drain queued source-viewer first-frame decodes and upload to a GPU
    /// texture. Stale results (`media_id` doesn't match the currently
    /// loaded source) are dropped so a slow decode of an earlier clip
    /// can't overwrite the texture for the current one.
    ///
    /// Uses the deferred-drop ring for replaced handles: under
    /// eframe-wgpu, dropping the old TextureHandle in the same frame
    /// that a submit still references its ID triggers a "Texture has
    /// been destroyed" validation panic. Jumping between media
    /// browser clips hits this exact path with every skim.
    pub(super) fn drain_source_frame(&mut self, ctx: &egui::Context) {
        while let Ok(result) = self.source_frame_rx.try_recv() {
            if self.state.source_media_id != Some(result.media_id) {
                continue;
            }
            let pixels: Vec<egui::Color32> = result
                .rgba
                .chunks_exact(4)
                .map(|p| egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
                .collect();
            let image = egui::ColorImage {
                size: [result.width as usize, result.height as usize],
                pixels,
            };
            let new_size = egui::vec2(result.width as f32, result.height as f32);
            // Reuse the existing handle only when the incoming frame
            // matches (same media, same size). Different media or a
            // size change requires a fresh handle with the old one
            // routed through the deferred-drop ring.
            let can_reuse = matches!(
                &self.state.source_video_texture,
                Some((mid, tex)) if *mid == result.media_id && tex.size_vec2() == new_size
            );
            if can_reuse {
                if let Some((_, ref mut tex)) = self.state.source_video_texture {
                    tex.set(image, egui::TextureOptions::LINEAR);
                }
            } else {
                if let Some((_, old)) = self.state.source_video_texture.take() {
                    tracing::debug!(
                        "TEX_DEFER_DROP id={:?} name=klipp-source-frame reason=source_media_or_size_change",
                        old.id(),
                    );
                    self.defer_drop_texture(old);
                }
                let handle = alloc_texture_logged(
                    ctx,
                    "klipp-source-frame",
                    image,
                    egui::TextureOptions::LINEAR,
                );
                if let Some((_, old)) = self.state.source_video_texture.take() {
                    self.defer_drop_texture(old);
                }
                self.state.source_video_texture = Some((result.media_id, handle));
            }
        }
    }

    pub(super) fn drain_thumbnails(&mut self) {
        while let Ok(result) = self.thumb_rx.try_recv() {
            self.state.thumbnail_cache.set_ready(
                result.media_id,
                result.rgba,
                result.width,
                result.height,
            );
        }

        // Drain consolidation progress messages from the background thread.
        // Keeps the status bar / dialog progress counter fresh, transitions
        // the UI out of Phase 1 when `Done` arrives, and — if the optional
        // archive phase was opted into — stays subscribed until `ArchiveDone`
        // arrives. `ArchiveProgress` is byte-granular; Phase 1 progress is
        // file-granular.
        let mut phase1_finished: Option<Result<klipp_project::consolidate::ConsolidateResult, String>> = None;
        let mut archive_finished: Option<Result<std::path::PathBuf, String>> = None;
        if let Some(progress) = self.state.consolidate_progress.as_mut() {
            while let Ok(msg) = progress.receiver.try_recv() {
                match msg {
                    crate::state::ConsolidateMessage::Progress { done, total, filename } => {
                        progress.files_done = done;
                        progress.files_total = total;
                        progress.current_file = filename;
                    }
                    crate::state::ConsolidateMessage::Done(r) => {
                        phase1_finished = Some(r);
                    }
                    crate::state::ConsolidateMessage::ArchiveProgress {
                        bytes_done,
                        bytes_total,
                        filename,
                    } => {
                        progress.archiving = true;
                        progress.archive_bytes_done = bytes_done;
                        progress.archive_bytes_total = bytes_total;
                        progress.current_file = filename;
                    }
                    crate::state::ConsolidateMessage::ArchiveDone(r) => {
                        archive_finished = Some(r);
                    }
                }
            }
        }
        // Fire Phase 1 toast (always). The progress handle stays alive if
        // archive messages are still expected (determined below).
        if let Some(result) = phase1_finished {
            match result {
                Ok(ref r) => {
                    let mut msg = format!("Project consolidated — {} file(s) copied", r.files_copied);
                    if r.files_trimmed > 0 {
                        msg.push_str(&format!(", {} cut to what is used", r.files_trimmed));
                    }
                    if !r.left_out.is_empty() {
                        msg.push_str(&format!(", {} unused left out", r.left_out.len()));
                    }
                    let whole = r.warnings.iter().filter(|w| w.contains("copied whole")).count();
                    if whole > 0 {
                        msg.push_str(&format!(", {whole} copied whole as they could not be cut cleanly"));
                    }
                    if !r.offline_skipped.is_empty() {
                        msg.push_str(&format!(", {} offline skipped", r.offline_skipped.len()));
                    }
                    // The titles' fonts (#3009): those carried, and those
                    // left out with why.
                    if !r.fonts_copied.is_empty() {
                        msg.push_str(&format!(", fonts carried: {}", r.fonts_copied.join(", ")));
                    }
                    let fonts_left: Vec<&str> = r.warnings.iter().filter_map(|w| w.strip_prefix("font ")).collect();
                    if !fonts_left.is_empty() {
                        msg.push_str(&format!("; {}", fonts_left.join("; ")));
                    }
                    msg.push('.');
                    // A file that could not be cut cleanly was copied whole.
                    for w in &r.warnings {
                        tracing::info!("consolidate: {w}");
                    }
                    self.state.toast.show(msg);
                }
                Err(ref e) => {
                    self.state.toast.show(format!("Consolidation failed: {e}"));
                }
            }
            // If Phase 1 failed OR the user didn't ask for an archive, we're
            // done. Otherwise keep the progress struct alive so we can still
            // receive ArchiveProgress / ArchiveDone in subsequent frames.
            let expects_archive = self
                .state
                .consolidate_progress
                .as_ref()
                .map(|p| p.expects_archive)
                .unwrap_or(false);
            let keep_alive = result.is_ok() && expects_archive && archive_finished.is_none();
            if !keep_alive {
                self.state.consolidate_progress = None;
            }
        }
        // Fire archive-phase toast when ArchiveDone arrives, and drop the
        // progress struct.
        if let Some(result) = archive_finished {
            self.state.consolidate_progress = None;
            match result {
                Ok(zip_path) => {
                    self.state.toast.show(format!(
                        "Archive created at {}",
                        zip_path.display(),
                    ));
                }
                Err(e) => {
                    self.state.toast.show(format!("Archive failed: {e}"));
                }
            }
        }
        // A clip relinked by hand to a file that is not it says so.
        while let Ok((was, picked, verdict)) = self.relink_check_rx.try_recv() {
            let name = picked.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            use klipp_media::relink::PickVerdict;
            match verdict {
                PickVerdict::Contradicted(why) => self.state.toast.show(format!(
                    "{was} relinked to {name}, but it may not be the same clip: {why}. Relink it again if that is wrong."
                )),
                PickVerdict::Confirmed(w) | PickVerdict::Unconfirmed(w) if !w.is_empty() => {
                    self.state.toast.show(format!("{was} relinked to {name}: {}", w.join(", ")))
                }
                _ => {}
            }
        }
        // Process up to 50 filmstrip frames per UI tick for fast progressive updates
        for _ in 0..50 {
            match self.filmstrip_rx.try_recv() {
                Ok(FilmstripResult::Frame { media_id, frame }) => {
                    self.state.thumbnail_cache.push_filmstrip_frame(media_id, frame);
                    // NOTE: previously this branch also did
                    //   `state.filmstrip_textures.remove(&media_id)`
                    // and defer-dropped every existing texture so the
                    // texture vec would get fully rebuilt by the upload
                    // pass. That made the upload path O(N²) — for a
                    // ~417-frame clip, ~19k texture allocations and
                    // 2-3s UI stalls under wgpu's command-buffer pressure.
                    // The upload pass at the bottom of update() now
                    // diffs against the existing vec by time_secs and
                    // only allocates the newly-arrived frame, so this
                    // remove() became actively harmful and was deleted.
                    // Bump per-job progress (frames_done, frames_expected).
                    // The expected count was set at spawn time from the
                    // duration / interval calculation; here we just
                    // increment the completed-frames counter.
                    if let Some(prog) = self.state.filmstrip_progress.get_mut(&media_id) {
                        prog.0 += 1;
                    }
                }
                Ok(FilmstripResult::CacheUnreadable { media_id }) => {
                    self.filmstrip_cache_loads.remove(&media_id);
                    self.state.filmstrip_in_progress.remove(&media_id);
                    self.state.filmstrip_progress.remove(&media_id);
                    self.state.thumbnail_cache.set_filmstrip(media_id, Vec::new());
                    if let Some((path, duration)) = self.state.media_pool.get(&media_id).map(|m| (m.path.clone(), m.duration)) {
                        self.spawn_filmstrip(media_id, path, duration);
                    }
                }
                Ok(FilmstripResult::Done { media_id }) => {
                    self.filmstrip_cache_loads.remove(&media_id);
                    let count = self.state.thumbnail_cache
                        .filmstrip(&media_id)
                        .map(|f| f.len())
                        .unwrap_or(0);
                    tracing::info!("Filmstrip complete for {:?}: {count} frames", media_id);
                    self.state.filmstrip_in_progress.remove(&media_id);
                    self.state.filmstrip_progress.remove(&media_id);
                    self.state.filmstrip_completed += 1;
                    if self.state.filmstrip_queue.is_empty() && self.state.filmstrip_in_progress.is_empty() {
                        self.state.filmstrip_generating = false;
                        self.state.filmstrip_total_queued = 0;
                        self.state.filmstrip_completed = 0;
                    }
                    // Start next queued filmstrip
                    self.pump_filmstrip_queue();
                }
                Err(_) => break,
            }
        }
        // Safety: if filmstrip_generating is stuck (thread died, channel broken),
        // force-clear after 60 seconds of no completions
        if self.state.filmstrip_generating
            && self.state.filmstrip_queue.is_empty()
            && self.state.filmstrip_in_progress.is_empty()
        {
            self.state.filmstrip_generating = false;
            self.state.filmstrip_total_queued = 0;
            self.state.filmstrip_completed = 0;
        }
    }

    pub(super) fn poll_effect_thumbnails(&mut self, ctx: &egui::Context) {
        // Generate all effect thumbnails using the embedded reference image.
        // This runs once at startup — no viewer frame needed.
        let effects: Vec<_> = self.state.effect_registry.effects.iter()
            .map(|e| (e.name.clone(), e.source.clone()))
            .collect();
        self.state.effect_thumbnails.generate_all_if_needed(&effects);
        // Poll completed thumbnails. Any displaced TextureHandle
        // (re-render of an effect that already had a thumbnail) is
        // routed into the deferred-drop bucket so its underlying
        // wgpu texture survives the current frame's submit — egui
        // BindGroups in flight still reference it. Hit #5 of the
        // defer-drop trap (memory: feedback_defer_drop_texture).
        let drops_slot = self.state.deferred_texture_drops_slot;
        self.state.effect_thumbnails.poll_and_upload(
            ctx,
            &mut self.state.deferred_texture_drops[drops_slot],
        );
    }

    pub(super) fn handle_skim_transcode(&mut self, media_id: MediaId) {
        if self.state.skim_transcoding.contains(&media_id) {
            return;
        }
        if let Some(item) = self.state.media_pool.get(&media_id) {
            let duration_secs = item.duration.to_secs_f64();
            if let Some(existing) = item.skim_file.as_ref().filter(|p| p.exists()) {
                if skim_file_is_complete(existing, duration_secs) {
                    return;
                }
                // Cut short by an older version or a crash: made again.
                let _ = std::fs::remove_file(existing);
                if let Some(item) = self.state.media_pool.get_mut(&media_id) {
                    item.skim_file = None;
                }
            }
            let Some(item) = self.state.media_pool.get(&media_id) else { return };
            // Dedupe by output path is done after `skim_path` is
            // computed below — see the `skim_in_flight_paths` check.
            let source_path = item.path.clone();
            let filename = item.filename.clone();
            // Skim transcode resolution: target = source height (capped at
            // 1080 for ≥4K sources so the file stays bounded on disk). For
            // anything ≤1080p, skim is source-resolution. Previous hardcoded
            // `scale_cuda=960:-2` gave every clip a 960×?? skim regardless
            // of source — that's 540p for 1080p input, 540p for 4K, which
            // is the root of "skim looks like shit" complaints.
            //
            // The QP/CRF value comes from the `skim_transcode_quality` pref
            // (values 18 High / 23 Standard / 28 Low / 32 Draft) — which
            // the UI already exposed but was ignored. Previously hardcoded
            // to 30 regardless of setting.
            let source_height = item.resolution.height.max(2);
            let target_height = source_height.min(1080);
            let skim_quality = self.prefs.media_cache.skim_transcode_quality;
            // Capture source_rotation for the encode-chain decision below.
            // Non-zero rotation (portrait phone clips) routes around the
            // nvenc/CUDA path because libavformat's autorotate filter only
            // inserts a CPU `transpose`, which can't sit between
            // `scale_cuda` and `h264_nvenc` — the autorotate is silently
            // skipped, and the skim ends up with sideways pixels that
            // disagree with how every other tool (and Klipp's own bin
            // thumbnail) renders the source. Routing portrait clips
            // through libx264 lets CPU autorotate kick in normally so the
            // skim has upright pixels matching the original's display
            // orientation.
            let source_rotation = item.source_rotation;

            // Detect HDR source (PQ/HLG/BT.2020). For HDR clips we
            // need to tone-map down to BT.709 SDR during the skim
            // encode — otherwise the resulting H.264 8-bit file
            // carries HDR metadata that the playback NV12 path
            // decodes as SDR, producing green/magenta blotches in
            // the viewer (the visible regression that prompted this
            // fix). The downstream cache key includes a `.hdrtm`
            // marker so old non-tone-mapped HDR skims are
            // invalidated rather than re-used.
            let is_hdr_source = matches!(
                item.colour_space,
                klipp_media::ColourSpace::Hdr10 | klipp_media::ColourSpace::Hlg,
            );
            let skim_dir = dirs::cache_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
                .join("klipp")
                .join("skim");
            let _ = std::fs::create_dir_all(&skim_dir);
            // The key carries `.hdrtm` for a tone-mapped HDR skim and
            // `.r<deg>` for a rotated source, so stale skims from before
            // either fix are not reused. One definition, shared with
            // the project loader's look-up (`skim_cache_key_for`).
            let base_key = klipp_media::disk_cache::compute_cache_key(&source_path)
                .unwrap_or_else(|| media_id.to_string());
            let cache_key = klipp_media::disk_cache::skim_cache_key(&base_key, is_hdr_source, source_rotation);
            let skim_path = klipp_media::disk_cache::skim_transcode_path(&cache_key);
            if skim_path.exists() {
                if skim_file_is_complete(&skim_path, duration_secs) {
                    tracing::info!("Skim cache hit for {:?}", source_path);
                    if let Some(item) = self.state.media_pool.get_mut(&media_id) {
                        item.skim_file = Some(skim_path);
                    }
                    self.sync_media_pool_to_engine();
                    return;
                }
                let _ = std::fs::remove_file(&skim_path);
            }

            // Already in-flight for this output path? Register as a
            // waiter and let the originating job's complete event
            // populate our `item.skim_file` too. Two MediaIds (e.g.
            // project pool path vs file browser absolute path) can
            // canonicalize to the same source file and would
            // otherwise both spawn ffmpeg jobs that race on the
            // shared parts/ directory.
            if let Some(waiters) = self.state.skim_in_flight_paths.get_mut(&skim_path) {
                if !waiters.contains(&media_id) {
                    waiters.push(media_id);
                    self.state.skim_transcoding.insert(media_id);
                    tracing::info!(
                        "Skim transcode dedupe: {:?} already in flight, registered MediaId({}) as waiter",
                        skim_path, media_id,
                    );
                }
                return;
            }

            // No cache — skip fresh generation if preference is off
            let skim_enabled = self.state.project_auto_skim
                .unwrap_or(self.prefs.editing.auto_generate_skim);
            if !skim_enabled {
                tracing::info!("Skim generation disabled by preference, skipping {:?}", source_path);
                return;
            }

            self.state.skim_transcoding.insert(media_id);
            self.state.skim_in_flight_paths.insert(skim_path.clone(), vec![media_id]);
            self.state.skim_transcode_status = Some((filename.clone(), 0.0));

            // Wrap the transcode in a SubprocessJob so the global
            // JobScheduler can SIGSTOP it the moment the user starts
            // playback / scrubbing. Critical for 4K60 clips where
            // ffmpeg saturates NVDEC + NVENC + disk and would
            // otherwise starve foreground decode for minutes.
            //
            // The job owns the *current* child PID. When the NVENC
            // attempt fails and we fall back to libx264, we
            // re-register the new child onto the same job so any
            // pause set during the gap is preserved (see
            // `register_child` for the gap-cancel/pause handling).
            let job = klipp_jobs::SubprocessJob::new(
                format!("skim-transcode:{filename}"),
                klipp_jobs::JobPriority::Background,
                klipp_jobs::ResourceClass::GpuMixed,
            );
            self.job_scheduler.submit(job.clone() as Arc<dyn klipp_jobs::BackgroundJob>);

            let event_tx = self.proxy_event_tx.clone();
            let job_for_thread = Arc::clone(&job);
            std::thread::spawn(move || {
                // At most a few transcodes at once; the rest wait here,
                // in the order they were asked for (#1360).
                let _slot = super::proxies::TRANSCODE_SLOTS.acquire();
                let job = job_for_thread;
                let run_ffmpeg = |args: &[&str], label: &str| -> Result<std::process::ExitStatus, std::io::Error> {
                    use std::io::{BufRead, Read};
                    // Use -progress pipe:1 for clean key=value progress on stdout.
                    // -hide_banner cuts the version preamble out of stderr so the
                    // captured tail contains only meaningful errors.
                    // -loglevel info keeps real errors but drops "frame=…" status spam.
                    let mut full_args: Vec<&str> = args.to_vec();
                    full_args.extend_from_slice(&["-nostdin", "-hide_banner", "-loglevel", "info", "-progress", "pipe:1"]);

                    // Echo the full command at debug level so we can reproduce
                    // failures from the log without guessing what was run.
                    tracing::debug!("ffmpeg [{label}] invoking: ffmpeg {}", full_args.join(" "));

                    // Ends with the app, killed or not (#1361).
                    let mut child = klipp_jobs::ends_with_app("ffmpeg")
                        .args(&full_args)
                        .stdout(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::piped())
                        .spawn()?;

                    // Hand the child to the SubprocessJob — needed
                    // BEFORE we read stdout so cancel() during the
                    // read loop can kill the process. Releases the
                    // child handle inside the job; we still need
                    // stdout / stderr, so take them back below.
                    let stdout = child.stdout.take();
                    let stderr = child.stderr.take();
                    job.register_child(child);

                    // Drain stderr in a background thread into a
                    // ring-buffered tail (last ~8 KiB). Without this
                    // ffmpeg can block once the OS pipe fills up,
                    // wedging the encode. We surface the tail on
                    // non-zero exit so the user log records the
                    // actual ffmpeg error message instead of just
                    // an opaque exit code.
                    let stderr_tail = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
                    let stderr_handle = stderr.map(|mut s| {
                        let buf = std::sync::Arc::clone(&stderr_tail);
                        std::thread::spawn(move || {
                            let mut chunk = [0u8; 4096];
                            let cap = 8192;
                            loop {
                                match s.read(&mut chunk) {
                                    Ok(0) | Err(_) => break,
                                    Ok(n) => {
                                        let mut g = buf.lock().unwrap_or_else(|e| e.into_inner());
                                        g.extend_from_slice(&chunk[..n]);
                                        // Trim from the front so we
                                        // keep only the most recent
                                        // `cap` bytes (errors are
                                        // usually at the end).
                                        if g.len() > cap {
                                            let drop = g.len() - cap;
                                            g.drain(..drop);
                                        }
                                    }
                                }
                            }
                        })
                    });

                    // Parse stdout for progress lines: "out_time_us=12345678"
                    if let Some(stdout) = stdout {
                        let reader = std::io::BufReader::new(stdout);
                        for line in reader.lines().map_while(Result::ok) {
                            if let Some(val) = line.strip_prefix("out_time_us=") {
                                if let Ok(us) = val.parse::<i64>() {
                                    if duration_secs > 0.0 && us > 0 {
                                        let secs = us as f64 / 1_000_000.0;
                                        let progress = (secs / duration_secs).clamp(0.0, 1.0) as f32;
                                        job.set_progress(progress);
                                        let _ = event_tx.send(AppEvent::SkimTranscodeProgress {
                                            media_id,
                                            progress,
                                        });
                                    }
                                }
                            }
                        }
                    }
                    // Pull the child back out of the job so we can
                    // wait() on it. Cancel-while-running is now
                    // racy: if cancel() landed first it took the
                    // child and killed it; take_child returns None;
                    // we synthesise an exit status indicating
                    // termination so the outer flow doesn't try the
                    // libx264 fallback for a cancelled run.
                    let mut child = match job.take_child() {
                        Some(c) => c,
                        None => {
                            // Cancelled — fabricate a non-success
                            // status so the caller treats it as
                            // failure and (importantly) doesn't fall
                            // through to libx264.
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::Interrupted,
                                "skim transcode cancelled",
                            ));
                        }
                    };
                    let status = child.wait();
                    // Reap the stderr drain thread so we have the
                    // tail captured by the time we surface it.
                    if let Some(h) = stderr_handle {
                        let _ = h.join();
                    }
                    if let Ok(s) = &status {
                        if !s.success() {
                            let tail = stderr_tail.lock().unwrap_or_else(|e| e.into_inner());
                            let snippet = String::from_utf8_lossy(&tail);
                            // Print the raw stderr verbatim with newlines,
                            // not the over-filtered cleaned version. Empty-tail
                            // bug from the previous formulation hid the actual
                            // error. Take the last ~4 KB so we don't spam logs
                            // on long ffmpeg runs but always show something.
                            let raw = snippet.replace('\r', "\n");
                            let trimmed = if raw.len() > 4096 {
                                let start = raw.len() - 4096;
                                // Snap to the next char boundary so we don't
                                // slice through a multi-byte UTF-8 sequence.
                                let mut i = start;
                                while i < raw.len() && !raw.is_char_boundary(i) { i += 1; }
                                format!("…(truncated)…\n{}", &raw[i..])
                            } else {
                                raw
                            };
                            tracing::warn!(
                                "ffmpeg [{label}] exited with {s}\n--- stderr ({} bytes captured) ---\n{}\n--- end stderr ---",
                                tail.len(), trimmed,
                            );
                        }
                    }
                    status
                };

                // Media only, as `klipp_media::input_args` gives it: a
                // list of other inputs is given to ffmpeg as nothing, so
                // the encode fails without opening anything.
                let src = if klipp_media::is_input_list(&source_path) {
                    tracing::warn!("skim: {} is a list of other inputs, not media; not encoded", source_path.display());
                    ""
                } else {
                    source_path.to_str().unwrap_or("")
                };

                // ─── Crash-safe segment-based encode ───────────────
                // Encode in 30-second chunks into a `<skim>.parts/`
                // directory. Each segment finalises independently, so
                // a killed run leaves N complete segments + at most
                // one partial. The next spawn detects this and resumes
                // from segment N (re-encoding 30s of overlap rather
                // than the entire 5-minute file). On success we concat
                // the segments back into one MKV via stream-copy and
                // atomically rename into the final skim path.
                const SEGMENT_DURATION_SECS: u32 = 30;
                let parts_dir = {
                    let stem = skim_path.file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("skim");
                    let mut p = skim_path.parent().map(|d| d.to_path_buf()).unwrap_or_default();
                    p.push(format!("{stem}.parts"));
                    p
                };
                let _ = std::fs::create_dir_all(&parts_dir);

                // Count segments. Last one is treated as suspect (may
                // have been mid-write when killed) so we re-encode
                // from there.
                let resume_from_segment: u32 = {
                    let entries: Vec<_> = std::fs::read_dir(&parts_dir).ok()
                        .map(|rd| rd.filter_map(|e| e.ok())
                            .filter(|e| {
                                let n = e.file_name();
                                let s = n.to_string_lossy();
                                s.starts_with("seg_") && s.ends_with(".mkv")
                            })
                            .collect())
                        .unwrap_or_default();
                    let total = entries.len() as u32;
                    // Drop the highest-numbered segment as potentially
                    // partial. If 0 segments exist, start from 0.
                    if total > 0 {
                        let last = entries.iter()
                            .filter_map(|e| {
                                let n = e.file_name();
                                let s = n.to_string_lossy();
                                let stem = s.strip_suffix(".mkv")?.strip_prefix("seg_")?;
                                stem.parse::<u32>().ok()
                            })
                            .max()
                            .unwrap_or(0);
                        // Delete the last segment (may be partial).
                        let last_path = parts_dir.join(format!("seg_{:05}.mkv", last));
                        let _ = std::fs::remove_file(&last_path);
                        last
                    } else {
                        0
                    }
                };
                if resume_from_segment > 0 {
                    tracing::info!(
                        "Skim transcode: resuming {:?} from segment {} ({}s in)",
                        source_path, resume_from_segment,
                        resume_from_segment * SEGMENT_DURATION_SECS,
                    );
                }

                // Filter chain depends on whether the source is HDR.
                //   * SDR: plain `scale_cuda` + `h264_nvenc` — this
                //     is already the fast path, no probing needed.
                //   * HDR: ask `klipp_engine::ffmpeg_caps` to pick
                //     the best chain available on this `ffmpeg` build.
                //     Tier 1 keeps everything on the GPU
                //     (`tonemap_cuda` + `h264_nvenc`); the worst
                //     case falls back to the previous CPU-only path.
                //     Cuts a 4K HDR transcode from ~30 min to ~3 min
                //     on a 4080.
                // CRITICAL: `format=yuv420p` forces 8-bit 4:2:0 output
                // even when the source is 10-bit. Without this, a 10-bit
                // SDR source (e.g. Sony S-Log3 HEVC Main 10 with BT.709
                // tags) propagated bit depth through `scale_cuda` →
                // `h264_nvenc` produced H.264 Hi10P, which NVDEC on
                // every consumer NVIDIA GPU rejects with "Bit depth 10
                // with this chroma format is not supported". The skim
                // then fell back to software decode, defeating its
                // entire purpose. Forcing yuv420p makes the skim
                // unambiguously NVDEC-decodable on playback.
                //
                // Skims are a browser-preview / hover-scrub artefact,
                // not a grading source — 8-bit precision is the right
                // trade. A future iteration could auto-detect 10-bit
                // input and use `hevc_nvenc -profile:v main10` instead
                // for users who want 10-bit-preserving skims.
                let nvenc_vf = format!("scale_cuda=-2:{target_height}:format=yuv420p");
                let cpu_vf_sdr = format!("scale=-2:{target_height},format=yuv420p");
                let qp_str = skim_quality.to_string();
                let start_ss = (resume_from_segment * SEGMENT_DURATION_SECS).to_string();
                let seg_time = SEGMENT_DURATION_SECS.to_string();
                let seg_start = resume_from_segment.to_string();
                let seg_pattern = parts_dir.join("seg_%05d.mkv").to_string_lossy().into_owned();

                let result = if source_rotation != 0 && !is_hdr_source {
                    // Portrait/rotated SDR source: skip nvenc/CUDA entirely.
                    // libavformat's autorotate filter only inserts a CPU
                    // `transpose`, which can't sit between scale_cuda and
                    // h264_nvenc — autorotate gets silently skipped, and
                    // the skim ends up sideways. libx264 runs on CPU
                    // frames where autorotate works normally, so the
                    // skim file has upright pixels. Trade ~3-5x realtime
                    // (nvenc) for ~1-1.5x realtime (libx264 ultrafast),
                    // but only for rotated clips and only the one-time
                    // background transcode — playback is unaffected.
                    tracing::info!(
                        "Skim transcode: portrait source ({}°) — using libx264 + autorotate",
                        source_rotation,
                    );
                    run_ffmpeg(&[
                        "-y",
                        "-ss", &start_ss,
                        "-protocol_whitelist", "file,pipe", "-i", src,
                        "-c:v", "libx264", "-g", "15", "-crf", &qp_str,
                        "-preset", "ultrafast",
                        "-an", "-vf", &cpu_vf_sdr,
                        "-f", "segment",
                        "-segment_time", &seg_time,
                        "-segment_format", "matroska",
                        "-reset_timestamps", "1",
                        "-segment_start_number", &seg_start,
                        &seg_pattern,
                    ], "skim-encode-sdr-libx264-portrait")
                } else if is_hdr_source {
                    let caps = klipp_engine::ffmpeg_caps::FfmpegCaps::detect();
                    let chain = caps.pick_hdr_chain(target_height);
                    tracing::info!(
                        "Skim transcode: HDR source detected — using {}",
                        chain.tier.label(),
                    );
                    // Build the args vector dynamically — tiers vary
                    // in input flags (`-hwaccel cuda` vs none) and
                    // encoder args.
                    let mut args: Vec<String> = vec!["-y".into()];
                    args.extend(chain.input_args.iter().cloned());
                    args.push("-ss".into());
                    args.push(start_ss.clone());
                    args.extend(["-protocol_whitelist".into(), "file,pipe".into(), "-i".into(), src.into()]);
                    args.push("-c:v".into());
                    args.push(chain.encoder.clone());
                    // GOP 15. `-g 1` (every frame a keyframe) gave
                    // instant per-frame seek but produced ~5× the
                    // bitrate of an FCP/DaVinci proxy. A 15-frame
                    // GOP keeps worst-case hover seek under ~30 ms
                    // on the skim decoder while matching industry
                    // proxy sizes. Paired with the Standard CRF the
                    // skim for a 35-min 4K clip drops from ~50 GB
                    // to the ~10 GB range.
                    args.push("-g".into()); args.push("15".into());
                    // Quality knob: nvenc uses -qp (constqp), libx264 uses -crf.
                    if chain.encoder == "h264_nvenc" {
                        args.push("-qp".into());
                    } else {
                        args.push("-crf".into());
                    }
                    args.push(qp_str.clone());
                    args.extend(chain.encoder_args.iter().cloned());
                    args.push("-an".into());
                    args.push("-vf".into());
                    args.push(chain.vf.clone());
                    args.push("-f".into()); args.push("segment".into());
                    args.push("-segment_time".into()); args.push(seg_time.clone());
                    args.push("-segment_format".into()); args.push("matroska".into());
                    args.push("-reset_timestamps".into()); args.push("1".into());
                    args.push("-segment_start_number".into()); args.push(seg_start.clone());
                    args.push(seg_pattern.clone());
                    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                    run_ffmpeg(&arg_refs, "skim-encode-hdr")
                } else {
                    // `-hwaccel_output_format cuda` is mandatory when
                    // we want `scale_cuda` (in `nvenc_vf`) to consume
                    // the decoded frames directly. Without it,
                    // `-hwaccel cuda` silently downloads each frame
                    // to a software pixel format (yuv420p10le for a
                    // 10-bit source), then `scale_cuda` rejects the
                    // CPU input with "Impossible to convert between
                    // the formats supported by the filter 'graph -1
                    // input from stream 0:0' and the filter
                    // 'auto_scale_0'" — the whole nvenc tier fails
                    // and we fall through to the libx264 CPU path
                    // (which is ~10× slower at 4K).
                    //
                    // The HDR chain in ffmpeg_caps already passes
                    // this pair; the SDR hardcoded path was missing
                    // the output-format flag.
                    run_ffmpeg(&[
                        "-y", "-hwaccel", "cuda",
                        "-hwaccel_output_format", "cuda",
                        "-ss", &start_ss,
                        "-protocol_whitelist", "file,pipe", "-i", src,
                        "-c:v", "h264_nvenc", "-g", "15", "-preset", "p1",
                        "-rc", "constqp", "-qp", &qp_str,
                        "-an", "-vf", &nvenc_vf,
                        "-f", "segment",
                        "-segment_time", &seg_time,
                        "-segment_format", "matroska",
                        "-reset_timestamps", "1",
                        "-segment_start_number", &seg_start,
                        &seg_pattern,
                    ], "skim-encode-sdr-nvenc")
                };

                let result = match &result {
                    Ok(status) if status.success() => result,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => result,
                    _ => {
                        if job.is_cancelled() {
                            result
                        } else {
                            tracing::info!("NVENC skim transcode failed, falling back to libx264");
                            // Fallback uses the SDR filter chain
                            // (HDR sources never reach this branch —
                            // they go straight to the libx264 + tone-
                            // map path above).
                            run_ffmpeg(&[
                                "-y",
                                "-ss", &start_ss,
                                "-protocol_whitelist", "file,pipe", "-i", src,
                                "-c:v", "libx264", "-g", "15", "-crf", &qp_str,
                                "-preset", "ultrafast",
                                "-an", "-vf", &cpu_vf_sdr,
                                "-f", "segment",
                                "-segment_time", &seg_time,
                                "-segment_format", "matroska",
                                "-reset_timestamps", "1",
                                "-segment_start_number", &seg_start,
                                &seg_pattern,
                            ], "skim-encode-sdr-libx264")
                        }
                    }
                };

                // Encode phase done. Now (on success) concat the
                // segments into one MKV. On failure or cancel, leave
                // the parts dir for the next spawn to resume from.
                let encode_succeeded = matches!(&result, Ok(s) if s.success());

                if encode_succeeded {
                    // Build the concat list. Segments numbered from 0
                    // upwards — collect, sort, write `file '...'`
                    // lines for ffmpeg's concat demuxer.
                    let mut segments: Vec<(u32, std::path::PathBuf)> = std::fs::read_dir(&parts_dir).ok()
                        .map(|rd| rd.filter_map(|e| e.ok())
                            .filter_map(|e| {
                                let n = e.file_name();
                                let s = n.to_string_lossy();
                                let stem = s.strip_suffix(".mkv")?.strip_prefix("seg_")?;
                                let idx: u32 = stem.parse().ok()?;
                                Some((idx, e.path()))
                            })
                            .collect())
                        .unwrap_or_default();
                    segments.sort_by_key(|(i, _)| *i);

                    if segments.is_empty() {
                        tracing::warn!("Skim transcode: encode succeeded but no segments found in {:?}", parts_dir);
                        job.mark_done();
                        let _ = event_tx.send(AppEvent::SkimTranscodeFailed {
                            media_id,
                            path: skim_path.clone(),
                            error: "no segments produced".into(),
                        });
                        return;
                    }

                    // ffmpeg concat demuxer needs single-quoted paths
                    // with internal `'` escaped as `'\''`. Our paths
                    // come from a sanitised cache key so they
                    // shouldn't contain quotes, but escape defensively.
                    let list_path = parts_dir.join("concat.txt");
                    let list_body: String = segments.iter()
                        .map(|(_, p)| {
                            let s = p.to_string_lossy().replace('\'', r"'\''");
                            format!("file '{s}'\n")
                        })
                        .collect();
                    if let Err(e) = std::fs::write(&list_path, &list_body) {
                        tracing::warn!("Skim transcode: failed to write concat list: {e}");
                        job.mark_done();
                        let _ = event_tx.send(AppEvent::SkimTranscodeFailed {
                            media_id,
                            path: skim_path.clone(),
                            error: format!("concat list write: {e}"),
                        });
                        return;
                    }
                    // Dump the first few segments + count + per-file
                    // sizes so a future "concat exited 234" debug
                    // session has enough context. Also catches the
                    // common failure mode where ffmpeg wrote a 0-byte
                    // segment that the concat demuxer can't probe.
                    {
                        let mut size_lines = Vec::with_capacity(segments.len().min(5));
                        for (i, p) in segments.iter().take(5) {
                            let sz = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
                            size_lines.push(format!("seg_{:05} = {} bytes", i, sz));
                        }
                        let zero_byte = segments.iter()
                            .filter(|(_, p)| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0) == 0)
                            .count();
                        tracing::info!(
                            "Skim concat: {} segments at {:?}; first sizes: {}{}",
                            segments.len(),
                            parts_dir,
                            size_lines.join(", "),
                            if zero_byte > 0 {
                                format!(" — WARNING: {zero_byte} zero-byte segment(s)")
                            } else { String::new() },
                        );
                        // First 4 lines of concat.txt — verifies the
                        // generated paths look sane (no special chars,
                        // quoting matches what ffmpeg expects).
                        let preview: String = list_body.lines().take(4).collect::<Vec<_>>().join("\n");
                        tracing::debug!("Skim concat: list_path={list_path:?} preview:\n{preview}");
                    }

                    // Concat into a `.partial` sibling so the final
                    // path is updated atomically only after the
                    // concat completes.
                    let mut concat_partial = skim_path.clone();
                    concat_partial.set_extension(format!(
                        "{}.partial",
                        skim_path.extension().and_then(|s| s.to_str()).unwrap_or("mkv")
                    ));
                    let _ = std::fs::remove_file(&concat_partial);
                    let list_str = list_path.to_string_lossy().into_owned();
                    let concat_dst = concat_partial.to_string_lossy().into_owned();
                    // First try `-c copy` — fast, no re-encode.
                    // `-f matroska` on the output is required because the
                    // `.partial` sibling extension can't be format-inferred
                    // by ffmpeg (exit 234: "Unable to choose an output
                    // format for …mkv.partial").
                    let concat_result = run_ffmpeg(&[
                        "-y", "-f", "concat", "-safe", "0",
                        "-i", &list_str,
                        "-c", "copy",
                        "-f", "matroska",
                        &concat_dst,
                    ], "skim-concat-copy");
                    // If stream-copy concat fails (most commonly on
                    // segments produced by libplacebo whose SPS/PPS
                    // can drift between segments), retry with a fast
                    // libx264 re-encode. This is much slower than
                    // `-c copy` but the fallback ensures we publish
                    // a usable skim instead of stranding the user
                    // with a complete .parts/ directory and no
                    // playable file.
                    let concat_result = match &concat_result {
                        Ok(s) if s.success() => concat_result,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => concat_result,
                        _ => {
                            if job.is_cancelled() {
                                concat_result
                            } else {
                                tracing::info!(
                                    "Skim concat: stream-copy failed, retrying with libx264 re-encode @ crf {qp_str}"
                                );
                                let _ = std::fs::remove_file(&concat_partial);
                                // Match the primary encoder's quality
                                // knob (skim_transcode_quality pref, same
                                // value the first-pass nvenc/libx264
                                // used). The previous hardcoded crf 23
                                // was visually-transparent quality —
                                // stricter than the segments themselves
                                // — and produced 3x-bloated output
                                // (e.g. 71 GB for a 35-min clip).
                                run_ffmpeg(&[
                                    "-y", "-f", "concat", "-safe", "0",
                                    "-i", &list_str,
                                    "-c:v", "libx264", "-preset", "ultrafast",
                                    "-crf", &qp_str,
                                    "-an",
                                    "-f", "matroska",
                                    &concat_dst,
                                ], "skim-concat-reencode")
                            }
                        }
                    };
                    job.mark_done();

                    match concat_result {
                        Ok(s) if s.success() => {
                            // Publish atomically; only now the cache
                            // is observable to any future spawn as
                            // a complete file.
                            if let Err(e) = std::fs::rename(&concat_partial, &skim_path) {
                                tracing::warn!(
                                    "Skim transcode: failed to publish {:?} → {:?}: {e}",
                                    concat_partial, skim_path,
                                );
                                let _ = std::fs::remove_file(&concat_partial);
                                let _ = event_tx.send(AppEvent::SkimTranscodeFailed {
                                    media_id,
                                    path: skim_path.clone(),
                                    error: format!("publish rename failed: {e}"),
                                });
                            } else {
                                // Successful — clean up the per-source
                                // parts dir so we don't leak disk.
                                let _ = std::fs::remove_dir_all(&parts_dir);
                                // Prune the pre-`.hdrtm` sibling if we
                                // just published the HDR variant. No
                                // reader consults the old path once the
                                // new one exists; without this the
                                // orphaned file lingers forever (seen:
                                // 23 GB non-hdrtm next to 71 GB hdrtm
                                // on the same source file).
                                if let Some(stem) = skim_path.file_stem().and_then(|s| s.to_str()) {
                                    if let Some(base) = stem.strip_suffix(".hdrtm") {
                                        let old = skim_path.with_file_name(format!("{base}.mkv"));
                                        if old.exists() {
                                            match std::fs::remove_file(&old) {
                                                Ok(()) => tracing::info!(
                                                    "Skim transcode: pruned stale pre-hdrtm sibling {:?}",
                                                    old,
                                                ),
                                                Err(e) => tracing::warn!(
                                                    "Skim transcode: failed to prune stale sibling {:?}: {e}",
                                                    old,
                                                ),
                                            }
                                        }
                                    }
                                }
                                let _ = event_tx.send(AppEvent::SkimTranscodeComplete {
                                    media_id,
                                    path: skim_path,
                                });
                            }
                        }
                        Ok(s) => {
                            let _ = std::fs::remove_file(&concat_partial);
                            let _ = event_tx.send(AppEvent::SkimTranscodeFailed {
                                media_id,
                                path: skim_path.clone(),
                                error: format!("concat ffmpeg exited with {s}"),
                            });
                        }
                        Err(e) => {
                            let _ = std::fs::remove_file(&concat_partial);
                            let _ = event_tx.send(AppEvent::SkimTranscodeFailed {
                                media_id,
                                path: skim_path.clone(),
                                error: format!("concat ffmpeg failed: {e}"),
                            });
                        }
                    }
                } else {
                    // Encode failed or was cancelled. Mark job done so
                    // the scheduler reaps it. Leave parts dir intact —
                    // a future spawn (after restart, or when the user
                    // toggles the source back in) will resume from
                    // the highest complete segment.
                    job.mark_done();
                    match result {
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                            tracing::info!("Skim transcode cancelled — parts preserved for resume");
                        }
                        Err(e) => {
                            let _ = event_tx.send(AppEvent::SkimTranscodeFailed {
                                media_id,
                                path: skim_path.clone(),
                                error: format!("Failed to run ffmpeg: {}", e),
                            });
                        }
                        Ok(s) => {
                            // Non-zero exit but not cancelled — log,
                            // emit failure, but still preserve parts
                            // for the next attempt to resume.
                            let _ = event_tx.send(AppEvent::SkimTranscodeFailed {
                                media_id,
                                path: skim_path.clone(),
                                error: format!("ffmpeg exited with {s}"),
                            });
                        }
                    }
                }
            });
        }
    }
}

impl KlippApp {
    pub(super) fn update_thumbnail_textures(&mut self, ctx: &egui::Context) {
        let media_ids: Vec<MediaId> = self.state.media_pool.all().map(|m| m.id).collect();
        for media_id in media_ids {
            // Upload single thumbnail if not yet done
            if !self.state.thumbnail_textures.contains_key(&media_id) {
                if let Some(ThumbnailEntry::Ready { rgba, width, height }) =
                    self.state.thumbnail_cache.get(&media_id)
                {
                    let w = *width as usize;
                    let h = *height as usize;
                    if rgba.len() == w * h * 4 {
                        let pixels: Vec<egui::Color32> = rgba
                            .chunks_exact(4)
                            .map(|p| {
                                egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3])
                            })
                            .collect();
                        let image = egui::ColorImage {
                            size: [w, h],
                            pixels,
                        };
                        let tex_name = format!("thumb-{}", media_id);
                        let tex = alloc_texture_logged(ctx, &tex_name, image, egui::TextureOptions::LINEAR);
                        if let Some(old) = self.state.thumbnail_textures.insert(media_id, tex) {
                            self.defer_drop_texture(old);
                        }
                    }
                }
            }

            // Upload filmstrip textures — reuse cached textures by time_secs
            // so that each new background frame only allocates its own texture,
            // not the entire filmstrip. Previously this path was O(N²) per clip
            // (every incoming frame triggered a full re-upload of N existing
            // frames + 1 new), churning ~19k texture allocations for a ~417-frame
            // clip and stalling the UI thread under wgpu's command-buffer pressure.
            //
            // Identity is `time_secs` quantised to microseconds so refinement
            // (which inserts new times BETWEEN existing ones, shifting indices)
            // still reuses correctly. Per `feedback_defer_drop_texture`, any
            // texture whose Arc-clone we relinquish mid-frame goes through
            // `deferred_texture_drops` rather than dropping inline — wgpu would
            // panic on the next submit otherwise.
            let existing_count = self.state.filmstrip_textures.get(&media_id).map(|t| t.len()).unwrap_or(0);
            let cache_count = self.state.thumbnail_cache.filmstrip(&media_id).map(|f| f.len()).unwrap_or(0);
            if cache_count != existing_count {
                if let Some(frames) = self.state.thumbnail_cache.filmstrip(&media_id) {
                    if !frames.is_empty() {
                        // Build a lookup table from the existing vec keyed by
                        // microsecond-quantised time. Each texture gets used at
                        // most once below (removed on hit), so leftovers at the
                        // end of the loop are stale entries to defer-drop.
                        let existing_vec = self.state.filmstrip_textures.remove(&media_id).unwrap_or_default();
                        let mut reuse: std::collections::HashMap<i64, egui::TextureHandle> =
                            std::collections::HashMap::with_capacity(existing_vec.len());
                        for (t, h) in existing_vec.into_iter() {
                            let key = (t * 1_000_000.0).round() as i64;
                            reuse.insert(key, h);
                        }
                        let mut new_vec: Vec<(f64, egui::TextureHandle)> = Vec::with_capacity(frames.len());
                        for (i, frame) in frames.iter().enumerate() {
                            let key = (frame.time_secs * 1_000_000.0).round() as i64;
                            if let Some(h) = reuse.remove(&key) {
                                new_vec.push((frame.time_secs, h));
                                continue;
                            }
                            let w = frame.width as usize;
                            let h_p = frame.height as usize;
                            if frame.rgba.len() != w * h_p * 4 {
                                continue;
                            }
                            let pixels: Vec<egui::Color32> = frame
                                .rgba
                                .chunks_exact(4)
                                .map(|p| {
                                    egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3])
                                })
                                .collect();
                            let image = egui::ColorImage {
                                size: [w, h_p],
                                pixels,
                            };
                            let tex_name = format!("filmstrip-{}-{}", media_id, i);
                            let tex = alloc_texture_logged(ctx, &tex_name, image, egui::TextureOptions::LINEAR);
                            new_vec.push((frame.time_secs, tex));
                        }
                        // Defer-drop any textures whose time_secs no longer
                        // appears in the cache (e.g. refinement replaced them).
                        // In the common "frames append" case `reuse` is empty
                        // and this is a no-op.
                        if !reuse.is_empty() {
                            let slot = self.state.deferred_texture_drops_slot;
                            self.state.deferred_texture_drops[slot].extend(reuse.into_values());
                        }
                        self.state.filmstrip_textures.insert(media_id, new_vec);
                    }
                }
            }
        }
    }
}

/// The skim decode height the `skim_resolution` preference asks for.
#[cfg(test)]
mod skim_decode_height_tests {
    use super::KlippApp;

    /// The whole point of the knob: it caps. Written with `max` it was
    /// inert for every source at or above its own value, which at the
    /// 720 default is most footage anyone edits.
    #[test]
    fn the_preference_caps_a_source_above_it() {
        assert_eq!(KlippApp::skim_decode_height(2160, 720), 720);
        assert_eq!(KlippApp::skim_decode_height(1080, 720), 720);
        assert_eq!(KlippApp::skim_decode_height(1080, 480), 480);
    }

    /// And it never upscales: a source shorter than the preference
    /// decodes at its own height, and nothing goes past 1080.
    #[test]
    fn the_source_height_and_the_1080_ceiling_still_bound_it() {
        assert_eq!(KlippApp::skim_decode_height(480, 720), 480);
        assert_eq!(KlippApp::skim_decode_height(4320, 4320), 1080);
    }

    /// `max` used to stop a nonsense value reaching the decoder. The
    /// preference has no UI, so keep a floor now that it can lower.
    #[test]
    fn a_nonsense_preference_cannot_decode_a_two_pixel_frame() {
        assert_eq!(KlippApp::skim_decode_height(1080, 2), 240);
        assert_eq!(KlippApp::skim_decode_height(1080, 0), 240);
    }
}

#[cfg(test)]
mod poster_width_tests {
    use super::*;

    /// #1816: a media item's thumbnail is decoded as wide as the cache
    /// preferences' thumbnail size: 320 gives a 320-wide picture, 640 a
    /// 640-wide one, from the same file.
    #[test]
    fn the_thumbnail_is_as_wide_as_the_preference() {
        let dir = std::env::temp_dir().join(format!("klipp-poster-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("still.png");
        let img = image::RgbImage::from_fn(1600, 900, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 128]));
        img.save(&path).unwrap();
        let decoded = |size: u32| {
            let prefs = crate::prefs::MediaCachePrefs { thumbnail_height: size, ..Default::default() };
            klipp_engine::generate_thumbnail_at_size_non_black(&path, 0.0, 0.0, poster_width(&prefs)).map(|(_, w, _)| w)
        };
        let (small, large) = (decoded(320), decoded(640));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(small, Ok(320));
        assert_eq!(large, Ok(640));
    }
}
