//! Background work on sound: conforming a file's audio, syncing a clip or a
//! picture and its sound by their waveforms, and taking the results (#826).

use super::*;

impl KlippApp {
    pub(super) fn spawn_audio_conform(&mut self, media_id: MediaId, path: PathBuf) {
        let tx = self.audio_conform_tx.clone();
        // Use XDG cache dir for audio cache (same area as proxies)
        let cache_dir = dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("klipp")
            .join("audio-cache");
        let cache_key = klipp_media::disk_cache::compute_cache_key(&path);
        // Wrap in a ThreadJob so the global JobScheduler can cancel
        // it on shutdown. The conform itself is one blocking call to
        // ffmpeg's audio decoder, so wait_while_paused only fires
        // before that call starts — mid-decode pause requires pushing
        // a token into klipp_media::audio_cache, which is a separate
        // refactor.
        //
        // Priority: UserInteractive. Audio skim (hover a clip to hear
        // it) silently returns nothing until the PCM cache exists, so
        // a Background-priority conform gets paused by user activity
        // the moment the user hovers a freshly-imported clip — the
        // exact moment the PCM is needed. UserInteractive is the only
        // priority the scheduler never pauses (apply_pause_state in
        // scheduler.rs). Conform is short (~3 s for a 15-min clip at
        // nice 0) so elevating it doesn't starve anything.
        let path_for_job = path.clone();
        let job = klipp_jobs::ThreadJob::spawn(
            format!("audio-conform:{}", path_for_job.display()),
            klipp_jobs::JobPriority::UserInteractive,
            klipp_jobs::ResourceClass::Cpu,
            move |pause_token, _progress| {
                // Honour any pre-decode pause / cancel.
                pause_token.wait_while_paused();
                if pause_token.is_cancelled() {
                    let _ = tx.send(AudioConformResult::Failed {
                        media_id,
                        error: "cancelled before start".into(),
                    });
                    return;
                }
                // Ensure cache directory exists
                if let Err(e) = std::fs::create_dir_all(&cache_dir) {
                    tracing::warn!("Failed to create audio cache dir: {e}");
                    let _ = tx.send(AudioConformResult::Failed {
                        media_id,
                        error: format!("cache dir: {e}"),
                    });
                    return;
                }

                // Check content-based cache first, then fall back to MediaId-based
                let (exists, pcm_path, peaks_path) = if let Some(ref key) = cache_key {
                    klipp_media::disk_cache::audio_conform_cache_exists(key)
                } else {
                    // No cache key (offline file?) — use old MediaId-based paths
                    let pcm = cache_dir.join(format!("{media_id}.pcm"));
                    let peaks = cache_dir.join(format!("{media_id}.peaks"));
                    let exists = pcm.exists() && peaks.exists();
                    (exists, pcm, peaks)
                };

                if exists {
                    tracing::info!("Audio cache hit for {:?}", path);
                    match klipp_media::audio_cache::load_peaks(&peaks_path) {
                        Ok(peaks) => {
                            let pcm_size = std::fs::metadata(&pcm_path)
                                .map(|m| m.len())
                                .unwrap_or(0);
                            let duration_secs = pcm_size as f64
                                / (48000.0 * 2.0 * 4.0);
                            let _ = tx.send(AudioConformResult::Complete {
                                media_id,
                                entry: klipp_media::AudioCacheEntry {
                                    media_id,
                                    pcm_path,
                                    channel_peaks: klipp_media::audio_cache::load_channel_peaks(&klipp_media::audio_cache::channel_peaks_path(&peaks_path)).ok(),
                                    peaks_path,
                                    sample_rate: 48000,
                                    channels: 2,
                                    duration_secs,
                                    peaks,
                                },
                            });
                        }
                        Err(e) => {
                            tracing::warn!("Failed to load cached peaks, re-conforming: {e}");
                            // One more pre-decode pause check before
                            // the heavy operation.
                            pause_token.wait_while_paused();
                            if !pause_token.is_cancelled() {
                                do_conform(media_id, &path, &cache_dir, &cache_key, &tx);
                            }
                        }
                    }
                    return;
                }

                pause_token.wait_while_paused();
                if !pause_token.is_cancelled() {
                    do_conform(media_id, &path, &cache_dir, &cache_key, &tx);
                }
            },
        );
        self.job_scheduler.submit(job as Arc<dyn klipp_jobs::BackgroundJob>);

        fn do_conform(
            media_id: MediaId,
            path: &Path,
            cache_dir: &Path,
            cache_key: &Option<String>,
            tx: &Sender<AudioConformResult>,
        ) {
            // Use content-based key for filenames if available, else MediaId
            let file_key = cache_key.as_deref()
                .map(|k| k.to_string())
                .unwrap_or_else(|| media_id.to_string());
            let pcm_path = cache_dir.join(format!("{file_key}.pcm"));
            let peaks_path = cache_dir.join(format!("{file_key}.peaks"));

            match klipp_media::audio_cache::conform_audio_to_paths(media_id, path, &pcm_path, &peaks_path) {
                Ok(entry) => {
                    let _ = tx.send(AudioConformResult::Complete { media_id, entry });
                }
                Err(e) => {
                    tracing::warn!("Audio conforming failed for {:?}: {e}", media_id);
                    let _ = tx.send(AudioConformResult::Failed {
                        media_id,
                        error: e.to_string(),
                    });
                }
            }
        }
    }

    /// Spawn a background job that cross-correlates a target clip's audio
    /// against the current reference clip's audio (#61). On completion the
    /// worker pushes `AudioSyncComplete` / `AudioSyncFailed` onto the
    /// proxy event channel, handled by `apply_audio_sync_result`.
    pub(super) fn spawn_audio_sync(&mut self, clip_id: klipp_core::ClipId) {
        let Some(ref_clip_id) = self.audio_reference_or_say() else { return };
        self.spawn_audio_sync_between(ref_clip_id, clip_id, false);
    }

    /// The reference clip set for audio sync, or a toast saying how to set one.
    fn audio_reference_or_say(&mut self) -> Option<klipp_core::ClipId> {
        match self.state.audio_reference_clip_id {
            Some(id) => Some(id),
            None => {
                self.state.toast.show(
                    "Set an audio reference first: right-click a clip \u{2192} Set as Audio Reference.".to_string(),
                );
                None
            }
        }
    }

    /// Line `clip_id`'s sound up with `ref_clip_id`'s and move it there.
    /// `strict` (Auto Align Audio, #615): only each clip's used source range
    /// is compared, and a match too weak to trust moves nothing.
    pub(super) fn spawn_audio_sync_between(&mut self, ref_clip_id: klipp_core::ClipId, clip_id: klipp_core::ClipId, strict: bool) {
        if ref_clip_id == clip_id {
            self.state.toast.show(
                "Target and reference are the same clip \u{2014} pick a different clip to align.".to_string(),
            );
            return;
        }

        let seq = &self.state.sequences[self.state.active_sequence_idx];
        let ref_clip = seq.all_clips().find(|c| c.id == ref_clip_id);
        let tgt_clip = seq.all_clips().find(|c| c.id == clip_id);
        let (ref_clip, tgt_clip) = match (ref_clip, tgt_clip) {
            (Some(a), Some(b)) => (a, b),
            _ => {
                self.state.toast.show("Audio sync: clip not found in active sequence.".to_string());
                return;
            }
        };

        let ref_media_id = match ref_clip.media_id() { Some(m) => m, None => {
            self.state.toast.show("Audio sync: reference clip has no media.".to_string());
            return;
        }};
        let tgt_media_id = match tgt_clip.media_id() { Some(m) => m, None => {
            self.state.toast.show("Audio sync: target clip has no media.".to_string());
            return;
        }};

        let ref_pcm_path = self.state.media_pool.get(&ref_media_id)
            .and_then(|m| m.audio_cache.as_ref())
            .map(|c| c.pcm_path.clone());
        let tgt_pcm_path = self.state.media_pool.get(&tgt_media_id)
            .and_then(|m| m.audio_cache.as_ref())
            .map(|c| c.pcm_path.clone());
        let (ref_path, tgt_path) = match (ref_pcm_path, tgt_pcm_path) {
            (Some(a), Some(b)) => (a, b),
            _ => {
                self.state.toast.show(
                    "Audio sync: audio cache not ready yet for one of the clips. Try again in a moment.".to_string(),
                );
                return;
            }
        };

        let ref_timeline_in_secs = ref_clip.timeline_in.to_secs_f64();
        // Each clip's portion of the media is [source_in, source_out]; subtract
        // those when resolving the offset so we correlate comparable regions.
        let ref_source_in_secs = ref_clip.source_in.to_secs_f64();
        let tgt_source_in_secs = tgt_clip.source_in.to_secs_f64();
        let ref_source_out_secs = ref_clip.source_out.to_secs_f64();
        let tgt_source_out_secs = tgt_clip.source_out.to_secs_f64();

        let tx = self.proxy_event_tx.clone();
        self.state.toast.show("Syncing audio\u{2026}".to_string());

        std::thread::Builder::new()
            .name("klipp-audio-sync".into())
            .spawn(move || {
                use klipp_analysis::audio_sync::{find_sync_offset, load_pcm_mono};
                const SR: u32 = 48_000;

                let ref_full = match load_pcm_mono(&ref_path) {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = tx.send(AppEvent::AudioSyncFailed {
                            clip_id,
                            error: format!("load reference PCM: {e}"),
                        });
                        return;
                    }
                };
                let tgt_full = match load_pcm_mono(&tgt_path) {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = tx.send(AppEvent::AudioSyncFailed {
                            clip_id,
                            error: format!("load target PCM: {e}"),
                        });
                        return;
                    }
                };

                // Slice each buffer to the active [source_in..] region so the
                // correlation compares what the user sees on the timeline,
                // not unused head trim.
                fn slice_from(buf: &[f32], start_secs: f64, sr: u32) -> &[f32] {
                    let start = ((start_secs.max(0.0)) * sr as f64) as usize;
                    if start >= buf.len() { &[] } else { &buf[start..] }
                }
                let mut ref_slice = slice_from(&ref_full, ref_source_in_secs, SR);
                let mut tgt_slice = slice_from(&tgt_full, tgt_source_in_secs, SR);
                if strict {
                    // What the timeline plays of each, trims and all.
                    let keep = |s: &[f32], secs: f64| s.len().min((secs.max(0.0) * SR as f64) as usize);
                    ref_slice = &ref_slice[..keep(ref_slice, ref_source_out_secs - ref_source_in_secs)];
                    tgt_slice = &tgt_slice[..keep(tgt_slice, tgt_source_out_secs - tgt_source_in_secs)];
                    let (offset, confidence) = klipp_analysis::audio_sync::find_sync_offset_scored(ref_slice, tgt_slice, SR, 60.0);
                    if confidence < klipp_core::sync::MIN_AUDIO_CONFIDENCE {
                        let _ = tx.send(AppEvent::AudioSyncFailed {
                            clip_id,
                            error: format!("the two sounds do not match well enough to trust (confidence {confidence:.2}); nothing was moved"),
                        });
                        return;
                    }
                    let _ = tx.send(AppEvent::AudioSyncComplete { clip_id, offset_samples: offset, ref_timeline_in_secs });
                    return;
                }

                let offset = find_sync_offset(ref_slice, tgt_slice, SR, 60.0);
                let _ = tx.send(AppEvent::AudioSyncComplete {
                    clip_id,
                    offset_samples: offset,
                    ref_timeline_in_secs,
                });
            })
            .ok();
    }

    /// Close any open path drag before a background job's result writes
    /// the document (#1327). A result arrives from a drained event, not
    /// through the dispatch boundary that closes a drag before any writer
    /// (`closes_path_drag`), so a drag left open would take the result into
    /// its step, or have the result's own step land inside it. Called at
    /// the top of each result that writes: a media load, a proxy, an audio
    /// conform, an audio sync, scene cuts, a learnt noise profile, a
    /// multicam plan. (A transcript goes to the analysis store, not the
    /// project, and an edit plan or the socket's edits go through
    /// `apply_edits_as_one`, which closes it already.)
    pub(super) fn close_drag_for_result(&mut self) {
        self.state.close_path_drag_before_edit();
    }

    /// Apply a completed audio-sync result: shift the target clip's timeline
    /// position so its audio lines up with the reference.
    pub(super) fn apply_audio_sync_result(
        &mut self,
        clip_id: klipp_core::ClipId,
        offset_samples: i64,
        ref_timeline_in_secs: f64,
    ) {
        self.close_drag_for_result();
        const SR: f64 = 48_000.0;
        let offset_secs = offset_samples as f64 / SR;

        // Positive offset_samples → target audio _starts later_ than the
        // reference in source time. To align playback, the target clip
        // should _start earlier_ on the timeline by that amount (i.e.
        // shifted left from the reference position).
        let new_in_secs = ref_timeline_in_secs - offset_secs;
        if new_in_secs < 0.0 {
            self.state.toast.show(format!(
                "Audio sync: computed offset {:.3}s would push clip before timeline start \u{2014} skipped.",
                offset_secs
            ));
            return;
        }

        // Find the target clip's current rate so we create the new timeline_in
        // at a rate that matches other clips in the sequence.
        let seq = &self.state.sequences[self.state.active_sequence_idx];
        let rate = seq.all_clips().find(|c| c.id == clip_id)
            .map(|c| c.timeline_in.rate)
            .unwrap_or_else(|| {
                let fps = seq.fps;
                (fps.num as f64 / fps.den as f64).round() as u32
            })
            .max(1);
        let new_timeline_in = klipp_core::Timecode::from_secs_f64(new_in_secs, rate);

        // Push MoveClip through the normal command path so history / magnetic
        // rules / sync-to-engine all behave the same as a drag. The UI's
        // dispatcher, not `cmd_tx`: the engine ignores MoveClip (#437).
        self.send_commands(vec![AppCommand::MoveClip {
            clip_id,
            new_timeline_in,
        }]);

        let fps_f = {
            let fps = self.state.sequence().fps;
            fps.num as f64 / fps.den.max(1) as f64
        };
        let frames = (offset_secs * fps_f).round() as i64;
        self.state.toast.show(format!(
            "Synced: offset {:+.3}s ({:+} frame{})",
            offset_secs, frames, if frames.abs() == 1 { "" } else { "s" },
        ));
    }

    pub(super) fn drain_audio_conforms(&mut self) {
        let mut changed = false;
        while let Ok(result) = self.audio_conform_rx.try_recv() {
            match result {
                AudioConformResult::Complete { media_id, entry } => {
                    self.close_drag_for_result();
                    tracing::info!(
                        "Audio conform complete for {:?}: {:.1}s, {} peaks",
                        media_id,
                        entry.duration_secs,
                        entry.peaks.len(),
                    );
                    if let Some(item) = self.state.media_pool.get_mut(&media_id) {
                        // Use conform peaks as waveform data for timeline display
                        if item.waveform_data.is_none() {
                            item.waveform_data = Some(entry.peaks.clone());
                        }
                        item.audio_cache = Some(entry);
                        changed = true;
                    }
                }
                AudioConformResult::Failed { media_id, error } => {
                    tracing::warn!("Audio conform failed for {:?}: {}", media_id, error);
                }
            }
        }
        if changed {
            self.sync_media_pool_to_engine();
        }
    }

    /// Pair a camera clip with a separate recording. The two files'
    /// timecodes settle it at once when both carry one; otherwise the
    /// two sounds are correlated on a thread and the result arrives as
    /// an event.
    pub(super) fn spawn_media_sync(&mut self, video_id: klipp_core::MediaId, audio_id: klipp_core::MediaId) {
        let (Some(video), Some(audio)) = (self.state.media_pool.get(&video_id), self.state.media_pool.get(&audio_id)) else {
            self.state.toast.show("Sync: one of the clips is no longer in the pool");
            return;
        };
        if let (Some(vt), Some(at)) = (&video.shot.start_timecode, &audio.shot.start_timecode) {
            let fps = video.fps;
            if let (Some(vs), Some(a_s)) = (klipp_core::timecode_to_secs(vt, fps), klipp_core::timecode_to_secs(at, fps)) {
                let sync = klipp_core::SyncedAudio { audio_media_id: audio_id, offset_secs: vs - a_s, method: klipp_core::SyncMethod::Timecode };
                self.apply_media_sync(video_id, sync);
                return;
            }
        }
        if !video.has_audio {
            self.state.toast.show("Sync: the clip has no sound to match against and no timecode to go by");
            return;
        }
        let (Some(video_pcm), Some(audio_pcm)) = (
            video.audio_cache.as_ref().map(|c| c.pcm_path.clone()),
            audio.audio_cache.as_ref().map(|c| c.pcm_path.clone()),
        ) else {
            self.state.toast.show("Sync: the audio cache is not ready for one of the clips yet; try again in a moment");
            return;
        };
        let tx = self.proxy_event_tx.clone();
        self.state.toast.show("Syncing by sound\u{2026}");
        std::thread::Builder::new()
            .name("klipp-media-sync".into())
            .spawn(move || {
                use klipp_analysis::audio_sync::{find_sync_offset, load_pcm_mono};
                const SR: u32 = 48_000;
                let (v, a) = match (load_pcm_mono(&video_pcm), load_pcm_mono(&audio_pcm)) {
                    (Ok(v), Ok(a)) => (v, a),
                    (Err(e), _) | (_, Err(e)) => {
                        let _ = tx.send(AppEvent::MediaSyncFailed { video_id, error: format!("load PCM: {e}") });
                        return;
                    }
                };
                // Positive: the recording's matching sound begins further
                // into its buffer, so audio time = video time + offset.
                let offset = find_sync_offset(&v, &a, SR, 120.0);
                let sync = klipp_core::SyncedAudio {
                    audio_media_id: audio_id,
                    offset_secs: offset as f64 / f64::from(SR),
                    method: klipp_core::SyncMethod::Waveform,
                };
                let _ = tx.send(AppEvent::MediaSyncComplete { video_id, sync });
            })
            .ok();
    }
}

#[cfg(test)]
mod result_drag_tests {
    /// Each background result that writes the document closes an open path
    /// drag first (#1327): the call is at the top of its handler, before
    /// anything is written. (The app the handlers run on is the whole
    /// window, so this holds the code, and `close_path_drag_before_edit`
    /// is held by the path-command tests.)
    #[test]
    fn every_result_that_writes_closes_an_open_drag_first() {
        let sources = [
            ("engine_events.rs", include_str!("engine_events.rs")),
            ("proxies.rs", include_str!("proxies.rs")),
            ("multicam.rs", include_str!("multicam.rs")),
            ("audio_jobs.rs", include_str!("audio_jobs.rs")),
        ];
        let handlers = [
            ("engine_events.rs", "AppEvent::MediaLoaded(payload) => {"),
            ("proxies.rs", "AppEvent::ProxyGenerationComplete { media_id, path } => {"),
            ("proxies.rs", "AppEvent::DenoiserProfileLearned { clip_id, effect_index, params, source_secs } => {"),
            ("proxies.rs", "if split {"),
            ("multicam.rs", "pub(super) fn apply_multicam_plan("),
            ("audio_jobs.rs", "AudioConformResult::Complete { media_id, entry } => {"),
            ("audio_jobs.rs", "pub(super) fn apply_audio_sync_result("),
        ];
        for (file, opening) in handlers {
            let src = sources.iter().find(|(f, _)| *f == file).unwrap().1;
            let at = src.find(opening).unwrap_or_else(|| panic!("{file}: no `{opening}`"));
            // The handler's first lines (a scene result closes it just
            // before its split, after the checks that write nothing).
            let head: String = src[..at].lines().rev().take(3).chain(src[at..].lines().take(9)).collect::<Vec<_>>().join("\n");
            assert!(head.contains("self.close_drag_for_result();"), "{file}: `{opening}` doesn't close an open drag first");
        }
    }
}
