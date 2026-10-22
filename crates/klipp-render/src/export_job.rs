//! Top-level export coordinator.
//!
//! # Dataflow
//!
//! ```text
//!   Sequence + ExportSettings
//!           │
//!           ▼
//!   SequenceSnapshot   ← immutable copy: the flattened timeline, the media
//!           │            files, audio, subtitles, chapters
//!           │
//!           ├──► klipp_compose::Renderer   (the viewer's renderer)
//!           │        │  ◄── ExportFrameSource: one sequential decoder per clip
//!           │        ▼
//!           │    display-referred linear frame
//!           │        │
//!           │        ▼
//!           ├──► Encoder: export display transform (8- or 16-bit) → YUV
//!           │
//!           └──► AudioMixdown → Encoder (audio)
//! ```
//!
//! Video is rendered by `klipp_compose` — the same code, on the same
//! flattened timeline, as the viewer — so an export is what was on screen.
//! Subtitles are muxed from the snapshot's `subtitle_tracks`, soft-embedded
//! or burned in depending on `ExportSettings`.

use crate::audio_mixdown::AudioMixdown;
use crate::encoder::Encoder;
use crate::export_frames::{export_renderer, ExportFrame, ExportFrameMaker};
use crate::error::RenderError;
use crate::frame_source::ExportFrameSource;
use crate::snapshot::{SequenceSnapshot, SubtitleClipSnapshot};
use klipp_core::ExportSettings;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tracing::{info, warn};
use uuid::Uuid;

/// Top-level export coordinator. Ties together the renderer, the encoder
/// and the audio mixdown.
pub struct ExportJob {
    pub id: Uuid,
    pub output_path: PathBuf,
    pub settings: ExportSettings,
    pub snapshot: SequenceSnapshot,
    pub cancel: Arc<AtomicBool>,
    pub progress: Arc<AtomicU64>,
    pub total_frames: u64,
    /// Shared string for hardware info reporting (set early in run()).
    pub hw_info: Arc<std::sync::Mutex<String>>,
    /// Current render stage (e.g. "Encoding video", "Finalizing").
    pub render_stage: Arc<std::sync::Mutex<String>>,
    /// Loudness normalization result (set during run() if normalization enabled).
    pub loudness_result: Arc<std::sync::Mutex<Option<String>>>,
    /// Where the export's time goes, updated about once a second while it
    /// renders and left at its last figures when it ends (#1924).
    pub timings: Arc<std::sync::Mutex<Option<klipp_core::ExportTimings>>>,
    /// Runs an analysis the export needs and hasn't got (M4 step 9).
    /// `None`: a missing analysis refuses the export.
    pub analyser: Option<crate::Analyser>,
}

impl ExportJob {
    /// Run the export. Blocks until complete, cancelled, or error.
    /// Called from a background thread.
    fn set_stage(stage: &Arc<std::sync::Mutex<String>>, text: &str) {
        if let Ok(mut s) = stage.lock() {
            *s = text.to_string();
        }
    }

    pub fn run(self) -> Result<(), RenderError> {
        let partial_path = partial_path_for(&self.output_path);
        let result = self.run_to_partial();
        // A failed render leaves no `.partial` behind — except the one
        // failure where the partial is the finished encode and the
        // rename into place is what failed.
        if let Err(e) = &result {
            if !matches!(e, RenderError::Publish(_)) {
                let _ = std::fs::remove_file(&partial_path);
            }
        }
        result
    }

    fn run_to_partial(self) -> Result<(), RenderError> {
        // Early cancellation check before any initialization
        if self.cancel.load(Ordering::Relaxed) {
            return Err(RenderError::Cancelled);
        }

        info!(
            "ExportJob {}: {}x{} @{:.3}fps ({}/{}), {} total frames, range {:?} → {:?}",
            self.id,
            self.snapshot.width,
            self.snapshot.height,
            self.snapshot.fps,
            self.snapshot.fps_num,
            self.snapshot.fps_den,
            self.total_frames,
            self.snapshot.start_frame(),
            self.output_path,
        );
        let timeline = &self.snapshot.video.sequence;
        info!(
            "ExportJob {}: {} tracks ({} clips), {} audio tracks, duration_frames={}",
            self.id,
            timeline.tracks.len(),
            timeline.tracks.iter().map(|t| t.clips.len()).sum::<usize>(),
            self.snapshot.audio_tracks.len(),
            self.snapshot.duration_frames,
        );

        // Every analysis the range reads, ready before the first frame:
        // a clip is never written without the correction it shows.
        let needs = crate::pre_analysis::needs(&self.snapshot);
        if !needs.is_empty() {
            let mut store = klipp_analysis::assets::CacheStore::default();
            crate::pre_analysis::ensure(&needs, &mut store, self.analyser.as_ref(), &self.cancel, &|s| Self::set_stage(&self.render_stage, s))?;
        }

        Self::set_stage(&self.render_stage, "Initializing\u{2026}");

        // ─── Initialise all systems ───────────────────────────────
        let mut renderer = export_renderer(&self.snapshot, &format!("ExportJob {}", self.id));
        let mut frames = ExportFrameSource::new(&self.snapshot.video.media, self.snapshot.fps, self.snapshot.working_colorspace);
        let mut audio_mixdown = AudioMixdown::new(&self.snapshot)?;

        // R-5: encode to a `.partial` sibling so the final path only
        // appears after a clean finalise. Crash / power loss / kill -9
        // now leaves a `.partial` next to the target instead of a
        // truncated but official-looking output file. We compose the
        // sibling by appending `.partial` to the full filename so the
        // original extension (mp4 / mov / mkv) survives for ffmpeg's
        // muxer inference + any downstream stale-cleanup tooling.
        let partial_path = partial_path_for(&self.output_path);
        // Remove any stale partial from a prior aborted run so the
        // encoder opens cleanly instead of refusing because the file
        // already exists on some muxer backends.
        let _ = std::fs::remove_file(&partial_path);

        // Determine frame range
        let start_frame = self.snapshot.start_frame();
        let end_frame = self.snapshot.end_frame();

        // ─── Pass 1: Loudness measurement (if normalization enabled) ──
        let ceiling_dbtp = self.settings.true_peak_dbtp.unwrap_or(DEFAULT_PEAK_CEILING_DBTP);
        // How long the loudness measurement and its fit's trials took, and
        // how many trials it ran (#1924).
        let t_loudness = std::time::Instant::now();
        let trials = std::cell::Cell::new(0u32);
        let mut delivery = if let Some(target_lufs) = self.settings.target_lufs {
            Self::set_stage(&self.render_stage, "Measuring loudness\u{2026}");
            let total_frames = self.total_frames;
            let analysis = measure_delivery(&self.snapshot, target_lufs, ceiling_dbtp, &self.cancel, |done, of| {
                // Measurement is roughly the first tenth of the work.
                self.progress.store((done * total_frames) / (of * 10), Ordering::Relaxed);
            })?;
            info!(
                "ExportJob {}: loudness measurement complete — integrated={:.1} LUFS, peak={:.3}, \
                 requested_gain={:.1} dB, applied_gain={:.1} dB, achieved={:.1} LUFS, peak_limited={}",
                self.id, analysis.integrated_lufs, analysis.peak_sample,
                analysis.requested_gain_db, analysis.applied_gain_db,
                analysis.achieved_lufs, analysis.peak_limited,
            );
            if analysis.peak_limited {
                info!(
                    "ExportJob {}: the peaks would pass the ceiling at {:.1} dB of gain; a limiter holds them",
                    self.id, analysis.applied_gain_db,
                );
            }
            // Provisional, for the queue panel while the render runs; the
            // measurement of what was written replaces it at the end.
            if let Ok(mut r) = self.loudness_result.lock() {
                *r = Some(format!("Loudness: {target_lufs:.1} LUFS requested\u{2026}"));
            }
            // A limited delivery's gain and limiter margin are measured
            // through the chain, not taken from the arithmetic (#781), and
            // through the delivery's own encoder when it is lossy: the codec
            // rings round transients and adds peak of its own (#1930).
            let (snapshot, cancel, id, stage) = (&self.snapshot, &self.cancel, self.id, &self.render_stage);
            let codec = self.delivered_audio_codec();
            let (rate, bitrate) = (snapshot.sample_rate, self.settings.audio_bitrate_kbps);
            // Why the file's peak couldn't be checked through its encoder,
            // once a trial failed: the delivery then holds the ceiling on
            // the mix's meter, and the status says so.
            let unverified: std::cell::RefCell<Option<String>> = Default::default();
            let trial_path = trial_path_for(&partial_path);
            let trial_run = |trial: &mut DeliveryChain, pass: Option<usize>| -> Result<Option<f32>, RenderError> {
                trials.set(trials.get() + 1);
                let what = match pass {
                    Some(n) => format!("Fitting loudness under the ceiling, trial {n} of up to {LIMITED_GAIN_PASSES}"),
                    None => format!("Checking the {} file's true peak", codec.label()),
                };
                let mut file = None;
                if unverified.borrow().is_none() {
                    match CodecTrial::open(&trial_path, codec, bitrate, rate) {
                        Ok(f) => file = f,
                        Err(e) => *unverified.borrow_mut() = Some(e.to_string()),
                    }
                }
                let mut trial_mix = AudioMixdown::new(snapshot)?;
                let frames = end_frame.saturating_sub(start_frame).max(1);
                let mut shown = u64::MAX;
                for frame_num in start_frame..end_frame {
                    if cancel.load(Ordering::Relaxed) {
                        return Err(RenderError::Cancelled);
                    }
                    let pct = (frame_num - start_frame) * 100 / frames;
                    if pct != shown {
                        shown = pct;
                        Self::set_stage(stage, &format!("{what}\u{2026} {pct}%"));
                    }
                    let mut audio = trial_mix.get_frame_audio(frame_num);
                    trial.process(&mut audio);
                    if let Some(f) = &mut file {
                        if let Err(e) = f.write(&audio) {
                            *unverified.borrow_mut() = Some(e.to_string());
                            file = None;
                        }
                    }
                }
                let Some(f) = file else { return Ok(None) };
                Self::set_stage(stage, &format!("{what}: reading the {} file back\u{2026}", codec.label()));
                match f.peak(cancel) {
                    Ok(p) => Ok(p),
                    Err(RenderError::Cancelled) => Err(RenderError::Cancelled),
                    Err(e) => {
                        *unverified.borrow_mut() = Some(e.to_string());
                        Ok(None)
                    }
                }
            };
            let mut limit = analysis.peak_limited;
            if !limit && crate::audio_only::trial_muxer(codec).is_some() {
                // Under the ceiling as the live meter reads the mix; the
                // file may not be, encoded and read as an outside check
                // reads it.
                let mut trial = DeliveryChain::limited_by(rate, target_lufs, analysis.applied_gain_db, false, ceiling_dbtp, LIMITER_CEILING_MARGIN_DB);
                if let Some(peak) = trial_run(&mut trial, None)? {
                    if peak > ceiling_dbtp {
                        info!("ExportJob {id}: the {} file would peak at {peak:.2} dBTP, over the {ceiling_dbtp:.1} ceiling; a limiter holds it", codec.label());
                        limit = true;
                    }
                }
            }
            let mut fit_note = None;
            let (gain_db, margin_db) = if limit {
                match fit_limited(rate, target_lufs, ceiling_dbtp, analysis.applied_gain_db, id, trial_run)? {
                    Some(fit) => {
                        fit_note = fit.note;
                        (fit.gain_db, fit.margin_db)
                    },
                    None => {
                        // No trial measured a loudness (a silent mix): the
                        // arithmetic, said as not checked.
                        unverified.borrow_mut().get_or_insert_with(|| "no trial of the mix measured a loudness".into());
                        (analysis.applied_gain_db, LIMITER_CEILING_MARGIN_DB)
                    }
                }
            } else {
                (analysis.applied_gain_db, LIMITER_CEILING_MARGIN_DB)
            };
            let mut chain = DeliveryChain::limited_by(self.snapshot.sample_rate, target_lufs, gain_db, limit, ceiling_dbtp, margin_db);
            chain.fit_note = fit_note;
            if let Some(why) = unverified.into_inner() {
                warn!("ExportJob {id}: the ceiling was not verified through the {} encoder: {why}", codec.label());
                chain.unverified = Some(format!("the ceiling was not checked through the {} encoder ({why})", codec.label()));
            }
            chain
        } else {
            DeliveryChain::unity()
        };

        let loudness = if self.settings.target_lufs.is_some() { (t_loudness.elapsed().as_secs_f64(), trials.get()) } else { (0.0, 0) };

        // Sound alone: the same mix and delivery, no picture (#1813).
        if self.settings.format.is_audio_only() {
            return self.write_audio_only(audio_mixdown, delivery, start_frame, end_frame, loudness);
        }

        let mut encoder =
            Encoder::open(&partial_path, &self.settings, &self.snapshot)?;

        let mut export_frame_num: u64 = 0;

        let mut hw_reported = false;
        let codec_label = self.settings.video_codec.label();
        Self::set_stage(&self.render_stage, &format!("Encoding video ({codec_label})\u{2026}"));

        // Where a frame's time goes. The export had no equivalent of the
        // graph executor's PERF line, so "the render is slow" could not
        // be turned into "which of the three stages is slow" without
        // rebuilding with a stopwatch in it.
        let mut t_render = std::time::Duration::ZERO;
        let mut t_pad = std::time::Duration::ZERO;
        let mut t_encode = std::time::Duration::ZERO;
        let mut t_audio = std::time::Duration::ZERO;
        let loop_start = std::time::Instant::now();

        // Each frame as an export makes it: one decision, shared with
        // anything that must render what an export would (#1156).
        let mut maker = ExportFrameMaker::new(&self.settings, &self.snapshot, &encoder, format!("ExportJob {}", self.id));
        let (out_w, out_h) = maker.out_size();

        // The figures a status reads (#1924), filled from the counters
        // above about once a second and at the end.
        let mut timings = klipp_core::ExportTimings {
            loudness_verify_secs: loudness.0,
            loudness_trials: loudness.1,
            video_encoder: Some(encoder.video_encoder_name().to_string()),
            audio_encoder: encoder.audio_encoder_name().map(str::to_string),
            ..Default::default()
        };
        let mut published = std::time::Instant::now();
        let publish = |timings: &mut klipp_core::ExportTimings, done: u64, frames: &ExportFrameSource, t: [std::time::Duration; 4]| {
            let per = |d: std::time::Duration| d.as_secs_f64() * 1000.0 / done.max(1) as f64;
            timings.frames = done;
            timings.wall_secs = loop_start.elapsed().as_secs_f64();
            (timings.render_ms, timings.pad_ms, timings.encode_ms, timings.audio_ms) = (per(t[0]), per(t[1]), per(t[2]), per(t[3]));
            (timings.decode_ms, timings.convert_ms) = (per(frames.t_decode), per(frames.t_convert));
            if let Ok(mut shared) = self.timings.lock() {
                *shared = Some(timings.clone());
            }
        };

        // ─── THE MAIN RENDER LOOP ─────────────────────────────────
        for frame_num in start_frame..end_frame {
            // Check cancellation
            if self.cancel.load(Ordering::Relaxed) {
                // Clean up partial file (atomic-export: we were writing
                // to partial_path, not output_path — nothing ever landed
                // at output_path, so only the `.partial` needs to go).
                drop(encoder);
                let _ = std::fs::remove_file(&partial_path);
                info!("ExportJob {}: cancelled at frame {}", self.id, frame_num);
                return Err(RenderError::Cancelled);
            }

            // ─── Render, the viewer's way, and encode ─────────────
            let time = klipp_core::RationalTime {
                value: frame_num as i64 * self.snapshot.fps_den as i64,
                rate: self.snapshot.fps_num,
            };
            let t0 = std::time::Instant::now();

            let frame = maker.frame(&mut renderer, timeline, time, &mut frames)?;
            t_render += t0.elapsed().saturating_sub(maker.last_pad);
            t_pad += maker.last_pad;
            // Where the frame was composited, and why not on the GPU.
            match maker.last_cpu_why() {
                None => timings.gpu_composited += 1,
                Some(why) => timings.cpu_frame(why),
            }
            if published.elapsed() >= std::time::Duration::from_secs(1) {
                published = std::time::Instant::now();
                publish(&mut timings, export_frame_num, &frames, [t_render, t_pad, t_encode, t_audio]);
            }
            // The hardware line, on the first frame whichever way it went:
            // a frame made on the GPU used to skip it, and a finished
            // export said nothing of its encoder (#1924).
            if !hw_reported {
                hw_reported = true;
                let hw_str = format!(
                    "Decode: {} | Encode: {}",
                    frames.hw_accel_name().unwrap_or("N/A"),
                    encoder.hw_info(),
                );
                info!("ExportJob {}: {}", self.id, hw_str);
                if let Ok(mut hw) = self.hw_info.lock() {
                    *hw = hw_str;
                }
            }
            let picture = match frame {
                ExportFrame::Planes { planes, light } => {
                    let t2 = std::time::Instant::now();
                    // `light` is the frame's MaxCLL/MaxFALL contribution for a
                    // PQ delivery, measured by the shader that wrote these
                    // planes; `None` on every other path.
                    encoder.encode_yuv420_planes(&planes, out_w, out_h, light)?;
                    t_encode += t2.elapsed();

                    let t3 = std::time::Instant::now();
                    let mut audio = audio_mixdown.get_frame_audio(frame_num);
                    delivery.process(&mut audio);
                    encoder.encode_audio_samples(&audio)?;
                    t_audio += t3.elapsed();

                    export_frame_num += 1;
                    self.progress.store(export_frame_num, Ordering::Relaxed);
                    continue;
                }
                ExportFrame::Linear(picture) => picture,
            };
            let t2 = std::time::Instant::now();
            encoder.encode_linear_frame(&picture, out_w, out_h)?;
            t_encode += t2.elapsed();

            // ─── Encode audio (with loudness normalization gain if enabled) ──
            let t3 = std::time::Instant::now();
            let mut audio = audio_mixdown.get_frame_audio(frame_num);
            delivery.process(&mut audio);
            encoder.encode_audio_samples(&audio)?;
            t_audio += t3.elapsed();

            // ─── Update progress ──────────────────────────────────
            export_frame_num += 1;
            self.progress.store(export_frame_num, Ordering::Relaxed);
        }

        publish(&mut timings, export_frame_num, &frames, [t_render, t_pad, t_encode, t_audio]);
        let frames_done = export_frame_num.max(1);
        let per = |d: std::time::Duration| d.as_secs_f64() * 1000.0 / frames_done as f64;
        info!(
            "ExportJob {}: {frames_done} frames in {:.1}s ({:.1} fps) | per frame: \
             render {:.1}ms (decode {:.1}ms, source convert {:.1}ms)  pad {:.1}ms  \
             encode {:.1}ms  audio {:.1}ms",
            self.id,
            loop_start.elapsed().as_secs_f64(),
            frames_done as f64 / loop_start.elapsed().as_secs_f64().max(0.001),
            per(t_render), per(frames.t_decode), per(frames.t_convert),
            per(t_pad), per(t_encode), per(t_audio),
        );
        let (dl, rot) = frames.decoder_stage_times();
        info!(
            "ExportJob {}: inside decode — download + colour {:.1}ms, rotation {:.1}ms",
            self.id, per(dl), per(rot),
        );

        // What the file measures, from the meter on the samples written.
        if let Some(report) = delivery.report() {
            info!("ExportJob {}: {report}", self.id);
            if let Ok(mut r) = self.loudness_result.lock() {
                *r = Some(report);
            }
        }

        Self::set_stage(&self.render_stage, "Finalizing file\u{2026}");

        // ─── Finalise ─────────────────────────────────────────────
        // ─── Subtitle embedding ───────────────────────────────────
        if let Some(sub_stream_idx) = encoder.subtitle_stream_idx {
            Self::encode_subtitles(
                &mut encoder,
                sub_stream_idx,
                &self.snapshot,
                &self.settings,
            )?;
        }

        // `encoder.finalise()` consumes the encoder, writes the
        // trailer, and flushes the muxer to disk. After this returns
        // Ok the `.partial` file is a fully-valid container.
        encoder.finalise()?;

        // What the written file measures, decoded, beside the mix's meter
        // (#1930): the ceiling is held against the file.
        if let Some(status) = delivery_status(&delivery, measure_written(&partial_path, self.duration_secs(), &self.cancel)?) {
            info!("ExportJob {}: {status}", self.id);
            if let Ok(mut r) = self.loudness_result.lock() {
                *r = Some(status);
            }
        }

        // R-5: atomic publish. The rename is the single observable
        // moment when the output path appears — concurrent readers
        // never see a truncated MP4.
        //
        // `fs::rename` is atomic within the same filesystem (rename(2)
        // on Linux). If `output_path` already exists (e.g. a previous
        // export to the same name that the UI forgot to gate on
        // DestinationExists) the rename overwrites it atomically.
        // A cross-filesystem move would fail — ExportJob validates the
        // dest dir earlier, so this only matters for pathological
        // edge cases; we log + keep the `.partial` in place so the
        // user can finish the move manually.
        if let Err(e) = std::fs::rename(&partial_path, &self.output_path) {
            tracing::warn!(
                "ExportJob {}: rename {:?} → {:?} failed: {e}. The finished \
                 encode is at the .partial path; rename manually.",
                self.id, partial_path, self.output_path,
            );
            return Err(RenderError::Publish(format!(
                "{e} (encode complete at {:?})",
                partial_path
            )));
        }

        info!(
            "ExportJob {}: complete, {} frames → {:?}",
            self.id, export_frame_num, self.output_path
        );

        Ok(())
    }

    /// The audio codec the file is written with: the settings', or, for
    /// sound alone, what the audio-only writer resolves them to.
    fn delivered_audio_codec(&self) -> klipp_core::ExportAudioCodec {
        if self.settings.format.is_audio_only() {
            crate::audio_only::resolve(self.settings.format, self.settings.audio_codec).1
        } else {
            self.settings.audio_codec
        }
    }

    /// How long the delivery runs, in seconds.
    fn duration_secs(&self) -> f64 {
        let fps = self.snapshot.fps.max(1.0);
        (self.snapshot.end_frame().saturating_sub(self.snapshot.start_frame())) as f64 / fps
    }

    /// The mix alone, frame by frame through `delivery`, as MP3, WAV or
    /// M4A (#1813), then published as the video export is. Without an MP3
    /// encoder the file is WAV, at the same name with `.wav`, and the
    /// export's hardware line says so.
    fn write_audio_only(&self, mut mixdown: AudioMixdown, mut delivery: DeliveryChain, start_frame: u64, end_frame: u64, loudness: (f64, u32)) -> Result<(), RenderError> {
        let (format, codec, fallback) = crate::audio_only::resolve(self.settings.format, self.settings.audio_codec);
        let output_path = if format == self.settings.format { self.output_path.clone() } else { self.output_path.with_extension(format.extension()) };
        if let Some(note) = &fallback {
            tracing::warn!("ExportJob {}: {note} → {:?}", self.id, output_path);
        }
        if let Ok(mut hw) = self.hw_info.lock() {
            *hw = fallback.clone().unwrap_or_else(|| format!("{} audio", codec.label()));
        }
        let partial_path = partial_path_for(&output_path);
        let _ = std::fs::remove_file(&partial_path);
        Self::set_stage(&self.render_stage, &format!("Encoding audio ({})\u{2026}", codec.label()));
        let mut writer = crate::audio_only::AudioOnly::open(&partial_path, format, codec, self.settings.audio_bitrate_kbps, self.snapshot.sample_rate)?;
        let mut done: u64 = 0;
        let started = std::time::Instant::now();
        let mut timings = klipp_core::ExportTimings { loudness_verify_secs: loudness.0, loudness_trials: loudness.1, audio_encoder: Some(codec.ffmpeg_codec().to_string()), ..Default::default() };
        for frame_num in start_frame..end_frame {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(RenderError::Cancelled);
            }
            let mut audio = mixdown.get_frame_audio(frame_num);
            delivery.process(&mut audio);
            writer.write(&audio)?;
            done += 1;
            self.progress.store(done, Ordering::Relaxed);
        }
        // Sound alone: every frame's time is its sound's (#1924).
        timings.frames = done;
        timings.wall_secs = started.elapsed().as_secs_f64();
        timings.audio_ms = timings.wall_secs * 1000.0 / done.max(1) as f64;
        if let Ok(mut shared) = self.timings.lock() {
            *shared = Some(timings);
        }
        if let Some(report) = delivery.report() {
            info!("ExportJob {}: {report}", self.id);
            if let Ok(mut r) = self.loudness_result.lock() {
                *r = Some(report);
            }
        }
        Self::set_stage(&self.render_stage, "Finalizing file\u{2026}");
        writer.finish()?;
        if let Some(status) = delivery_status(&delivery, measure_written(&partial_path, self.duration_secs(), &self.cancel)?) {
            info!("ExportJob {}: {status}", self.id);
            if let Ok(mut r) = self.loudness_result.lock() {
                *r = Some(status);
            }
        }
        if let Err(e) = std::fs::rename(&partial_path, &output_path) {
            return Err(RenderError::Publish(format!("{e} (encode complete at {partial_path:?})")));
        }
        info!("ExportJob {}: complete, {done} frames of sound → {output_path:?}", self.id);
        Ok(())
    }

    /// Encode all subtitle clips as packets and write them to the output.
    /// For MP4/MOV, inserts empty gap packets between subtitles.
    fn encode_subtitles(
        encoder: &mut Encoder,
        sub_stream_idx: usize,
        snapshot: &SequenceSnapshot,
        settings: &ExportSettings,
    ) -> Result<(), RenderError> {
        // Flatten all subtitle clips, sorted by start frame
        let mut all_clips: Vec<&SubtitleClipSnapshot> = snapshot
            .subtitle_tracks
            .iter()
            .flat_map(|t| &t.clips)
            .collect();
        all_clips.sort_by_key(|c| c.timeline_start_frame);

        let needs_gaps = matches!(
            settings.format,
            klipp_core::ExportFormat::Mp4 | klipp_core::ExportFormat::Mov
        );
        let is_micro = needs_gaps; // mov_text uses 1/1000000, others use 1/1000

        // Gap from frame 0 to first subtitle (MP4 only)
        if needs_gaps {
            if let Some(first) = all_clips.first() {
                if first.timeline_start_frame > 0 {
                    let pkt = Self::build_gap_packet(
                        sub_stream_idx, 0, first.timeline_start_frame,
                        snapshot.fps, is_micro,
                    );
                    encoder.write_subtitle_packet(pkt)?;
                }
            }
        }

        for (i, clip) in all_clips.iter().enumerate() {
            let pkt = Self::build_text_packet(
                sub_stream_idx, clip, snapshot.fps, is_micro,
            );
            encoder.write_subtitle_packet(pkt)?;

            // Gap to next subtitle (MP4 only)
            if needs_gaps {
                if let Some(next) = all_clips.get(i + 1) {
                    if next.timeline_start_frame > clip.timeline_end_frame {
                        let pkt = Self::build_gap_packet(
                            sub_stream_idx,
                            clip.timeline_end_frame,
                            next.timeline_start_frame,
                            snapshot.fps,
                            is_micro,
                        );
                        encoder.write_subtitle_packet(pkt)?;
                    }
                }
            }
        }

        info!("Encoded {} subtitle clips", all_clips.len());
        Ok(())
    }

    /// Build a subtitle packet containing text content.
    /// `microseconds=true` for mov_text (1/1_000_000 timebase + 2-byte length prefix);
    /// `microseconds=false` for subrip/webvtt (1/1000 timebase + raw UTF-8).
    fn build_text_packet(
        stream_idx: usize,
        clip: &SubtitleClipSnapshot,
        fps: f64,
        microseconds: bool,
    ) -> ffmpeg_next::Packet {
        let start_secs = clip.timeline_start_frame as f64 / fps;
        let end_secs = clip.timeline_end_frame as f64 / fps;
        let duration_secs = end_secs - start_secs;

        let text_bytes = clip.text.as_bytes();
        let data: Vec<u8> = if microseconds {
            // mov_text binary format: 2-byte big-endian length prefix + UTF-8 text
            let len = text_bytes.len().min(u16::MAX as usize);
            let mut d = Vec::with_capacity(2 + len);
            d.extend_from_slice(&(len as u16).to_be_bytes());
            d.extend_from_slice(&text_bytes[..len]);
            d
        } else {
            // subrip/webvtt: raw UTF-8 text
            text_bytes.to_vec()
        };

        let mut packet = ffmpeg_next::Packet::copy(&data);
        packet.set_stream(stream_idx);

        if microseconds {
            packet.set_pts(Some((start_secs * 1_000_000.0) as i64));
            packet.set_dts(Some((start_secs * 1_000_000.0) as i64));
            packet.set_duration((duration_secs * 1_000_000.0) as i64);
        } else {
            packet.set_pts(Some((start_secs * 1_000.0) as i64));
            packet.set_dts(Some((start_secs * 1_000.0) as i64));
            packet.set_duration((duration_secs * 1_000.0) as i64);
        }

        packet
    }

    /// Build an empty gap packet (MP4/MOV mov_text only).
    fn build_gap_packet(
        stream_idx: usize,
        start_frame: u64,
        end_frame: u64,
        fps: f64,
        microseconds: bool,
    ) -> ffmpeg_next::Packet {
        let start_secs = start_frame as f64 / fps;
        let duration_secs = (end_frame - start_frame) as f64 / fps;

        // Empty mov_text: 2-byte zero length prefix
        let mut packet = ffmpeg_next::Packet::copy(&[0u8, 0u8]);
        packet.set_stream(stream_idx);

        if microseconds {
            packet.set_pts(Some((start_secs * 1_000_000.0) as i64));
            packet.set_dts(Some((start_secs * 1_000_000.0) as i64));
            packet.set_duration((duration_secs * 1_000_000.0) as i64);
        } else {
            packet.set_pts(Some((start_secs * 1_000.0) as i64));
            packet.set_dts(Some((start_secs * 1_000.0) as i64));
            packet.set_duration((duration_secs * 1_000.0) as i64);
        }

        packet
    }
}

// ═══════════════════════════════════════════════════════════════════
// Aspect-ratio-preserving fit for resolution overrides
// ═══════════════════════════════════════════════════════════════════

/// Place a `w` × `h` linear frame at (`x`, `y`) on a black `dst_w` ×
/// `dst_h` canvas.
pub(crate) fn pad_linear(frame: Vec<f32>, w: u32, h: u32, dst_w: u32, dst_h: u32, x: u32, y: u32) -> Vec<f32> {
    if (w, h) == (dst_w, dst_h) {
        return frame;
    }
    let mut out = [0.0f32, 0.0, 0.0, 1.0].repeat((dst_w as usize) * (dst_h as usize));
    for row in 0..h as usize {
        let src = &frame[row * w as usize * 4..(row + 1) * w as usize * 4];
        let o = ((y as usize + row) * dst_w as usize + x as usize) * 4;
        out[o..o + src.len()].copy_from_slice(src);
    }
    out
}

/// Compute fitted dimensions and centering offset to fit `(src_w, src_h)`
/// R-5: derive the `.partial` sibling path for atomic export. Appends
/// `.partial` to the full filename so the original extension survives
/// (ffmpeg's muxer selection + any downstream stale-cleanup tool that
/// greps for the base name + `.partial`).
pub fn partial_path_for(final_path: &Path) -> PathBuf {
    let mut buf = final_path.as_os_str().to_owned();
    buf.push(".partial");
    PathBuf::from(buf)
}

/// Size and offset that fit a `(src_w, src_h)` picture
/// into `(target_w, target_h)` while preserving aspect ratio.
/// Returns (fitted_w, fitted_h, offset_x, offset_y).
/// Fitted dimensions are forced even for YUV420p chroma subsampling compatibility.
pub(crate) fn fit_to_target(src_w: u32, src_h: u32, target_w: u32, target_h: u32) -> (u32, u32, u32, u32) {
    if src_w == 0 || src_h == 0 || target_w == 0 || target_h == 0 {
        return (target_w & !1, target_h & !1, 0, 0);
    }
    let src_aspect = src_w as f64 / src_h as f64;
    let target_aspect = target_w as f64 / target_h as f64;

    let (fitted_w, fitted_h) = if src_aspect > target_aspect {
        // Source wider than target — fit to target width, letterbox top/bottom
        let fw = target_w;
        let fh = ((target_w as f64 / src_aspect).round() as u32).min(target_h);
        (fw & !1, fh & !1) // force even
    } else {
        // Source taller than target — fit to target height, pillarbox left/right
        let fh = target_h;
        let fw = ((target_h as f64 * src_aspect).round() as u32).min(target_w);
        (fw & !1, fh & !1) // force even
    };

    let offset_x = (target_w - fitted_w) / 2;
    let offset_y = (target_h - fitted_h) / 2;
    (fitted_w, fitted_h, offset_x, offset_y)
}


// ═══════════════════════════════════════════════════════════════════
// EBU R128 loudness normalization — two-pass measurement + static gain
// ═══════════════════════════════════════════════════════════════════

/// The true-peak ceiling of a delivery when the settings name none:
/// -1 dBTP, what EBU R128 and the -14 streaming convention ask. ATSC
/// A/85 and Netflix ask -2, and say so through `true_peak_dbtp`. Gain
/// that would push the measured peak past the ceiling is still applied
/// in full, and a limiter holds the peaks under it.
const DEFAULT_PEAK_CEILING_DBTP: f32 = klipp_core::DEFAULT_TRUE_PEAK_DBTP;

/// The limiter that holds a normalised delivery under the ceiling. It
/// detects sample peaks, so its ceiling sits under the true-peak one:
/// inter-sample peaks of limited speech and music run a few tenths of a
/// decibel over the samples, and a lossy encode adds a tenth or two of
/// its own: at -1.3 the AAC of a limited interview measured -0.9 dBTP.
/// So the limiter's ceiling sits 0.7 dB under the true-peak one, and its
/// threshold 1.3 dB under that — -1.7 and -3.0 for a -1 dBTP delivery.
const LIMITER_CEILING_MARGIN_DB: f32 = 0.7;
const LIMITER_KNEE_DB: f32 = 1.3;
/// How many trials a limited delivery's gain and margin are fitted in,
/// each a run of the mix through the chain and, for the file's true peak,
/// an encode of the sound alone and a decode (#1930): seconds for a
/// long mix, no picture. The delivery is the best trial measured. And
/// how far past the arithmetic gain the corrections may take it.
const LIMITED_GAIN_PASSES: usize = 5;
const LIMITED_GAIN_EXTRA_DB: f32 = 3.0;
/// The least loudness a dB of gain is taken to buy when the next gain is
/// stepped by the slope two trials measured (#2361): through a limiter a
/// dB of gain buys less than a dB, and stepping as if it bought one fell
/// short trial after trial. Held above zero so one noisy pair can't send
/// the gain off.
const MIN_LOUDNESS_SLOPE: f32 = 0.25;
/// How far under the true-peak ceiling the limiter's own ceiling may be
/// pushed when a pass through it measures true peaks over: bright
/// transients (a whoosh, a hi-hat) run further over their samples than
/// the margin allows for (#781).
const LIMITER_CEILING_MARGIN_MAX_DB: f32 = 4.5;
/// Taken off with each correction on top of the overshoot measured, so
/// the next pass lands under the ceiling rather than on it.
const LIMITER_OVERSHOOT_PAD_DB: f32 = 0.05;
/// Taken off on top of that when the overshoot was measured through a
/// lossy codec: its peak moves by a few tenths with any change to what it
/// encodes, so a correction aimed at the ceiling itself lands either side
/// of it (#1930).
const CODEC_OVERSHOOT_PAD_DB: f32 = 0.25;

/// A measured fit and any limit that prevented convergence.
#[derive(Debug, PartialEq)]
struct LimitedFit {
    gain_db: f32,
    margin_db: f32,
    note: Option<String>,
}

/// The gain and the limiter margin that bring a limited delivery to
/// `target_lufs` with its true peak under `ceiling_dbtp`, starting from
/// the arithmetic gain. A limiter takes loudness with the peaks it
/// holds, so the gain that reaches the target through it is a little
/// more than the arithmetic says; and it holds sample peaks, so the true
/// peak between them can pass its ceiling. Measured, not guessed: `run`
/// sends the whole mix through a trial chain, the gain takes up the
/// loudness shortfall and the margin the true peak over the ceiling.
fn fit_limited(
    sample_rate: u32,
    target_lufs: f32,
    ceiling_dbtp: f32,
    arithmetic_gain_db: f32,
    id: impl std::fmt::Display,
    mut run: impl FnMut(&mut DeliveryChain, Option<usize>) -> Result<Option<f32>, RenderError>,
) -> Result<Option<LimitedFit>, RenderError> {
    /// One measured trial: its gain and margin, how far over the ceiling
    /// its file was, and its loudness.
    struct Tried {
        gain_db: f32,
        margin_db: f32,
        over: f32,
        lufs: f32,
    }
    let (mut gain_db, mut margin_db) = (arithmetic_gain_db, LIMITER_CEILING_MARGIN_DB);
    // No gain at or above one a trial found over the ceiling: the loudness
    // can't climb back to a peak already measured (#1930). A lossy codec's
    // overshoot moves a few tenths with any change to what it encodes, so
    // one trial over only through the codec doesn't set it; a second does
    // (#2361).
    let mut gain_cap = f32::INFINITY;
    let mut codec_overs = 0;
    // Whether the extra-gain limit held a step back, for the note.
    let mut held_by_extra = false;
    let mut tried: Vec<Tried> = Vec::new();
    for pass in 0..LIMITED_GAIN_PASSES {
        let mut trial = DeliveryChain::limited_by(sample_rate, target_lufs, gain_db, true, ceiling_dbtp, margin_db);
        let coded = run(&mut trial, Some(pass + 1))?;
        let Some(got) = trial.measured_lufs().filter(|l| l.is_finite()) else { break };
        let shortfall = target_lufs - got;
        // Over the ceiling as mixed, or as the delivery's file decodes
        // (#1930): whichever is further.
        let mixed = trial.measured_true_peak_dbtp().map_or(0.0, |p| p - ceiling_dbtp);
        let coded = coded.map(|p| p - ceiling_dbtp);
        let over = coded.map_or(mixed, |c| c.max(mixed));
        info!(
            "ExportJob {id}: limited pass {}: {gain_db:.2} dB of gain under a {:.2} dB margin measures {got:.2} LUFS, \
             {shortfall:+.2} from the target, true peak {mixed:+.2} dB from the ceiling as mixed{}",
            pass + 1, margin_db,
            coded.map_or(String::new(), |c| format!(", {c:+.2} as encoded")),
        );
        // What a dB of gain bought between this trial and an earlier one at
        // the same margin: the latest on the target's other side, so the
        // next gain lies between the two; or, short of the target, the
        // last one, to step up by. Stepping down goes a dB a dB: below
        // the limiter that is what a dB buys, and a slope measured in it
        // would overshoot.
        let same_margin = |t: &&Tried| (t.margin_db - margin_db).abs() < 1e-3 && (gain_db - t.gain_db).abs() > 0.05;
        let partner = tried
            .iter()
            .rev()
            .filter(same_margin)
            .find(|t| (target_lufs - t.lufs) * shortfall < 0.0)
            .or_else(|| tried.last().filter(same_margin).filter(|_| shortfall > 0.0));
        let slope = partner.map(|t| ((got - t.lufs) / (gain_db - t.gain_db)).clamp(MIN_LOUDNESS_SLOPE, 1.0));
        tried.push(Tried { gain_db, margin_db, over, lufs: got });
        if (shortfall.abs() < 0.1 && over <= 0.0) || pass + 1 == LIMITED_GAIN_PASSES {
            break;
        }
        if over > 0.0 {
            // Over: the ceiling first, with room for a lossy codec, whose
            // overshoot moves a little with every change to its input. The
            // limiter's margin takes it while it can, from where its
            // ceiling is or from the peaks going into it when they are
            // under it and it held nothing, costing loudness only on the
            // peaks it catches; past its widest, the gain takes the rest
            // (a peak between samples the limiter leaves alone, or one the
            // codec rings up), the loudness short by that.
            let pad = LIMITER_OVERSHOOT_PAD_DB + if coded.is_some() { CODEC_OVERSHOOT_PAD_DB } else { 0.0 };
            // The ceiling held this step, not the extra-gain limit.
            held_by_extra = false;
            if mixed <= 0.0 {
                codec_overs += 1;
            }
            if mixed > 0.0 || codec_overs > 1 {
                gain_cap = gain_cap.min(gain_db);
            }
            let now = (ceiling_dbtp - margin_db).min(trial.limiter_input_peak_dbfs());
            let wanted = ceiling_dbtp - (now - over - pad);
            let wider = wanted.min(LIMITER_CEILING_MARGIN_MAX_DB).max(margin_db);
            margin_db = wider;
            // Too loud as well: down to the target, which lowers the peak
            // too. Never up while over.
            gain_db += shortfall.min(0.0) - (wanted - wider).max(0.0);
        } else {
            // Under: the loudness, by the slope measured when there is one
            // (a dB of gain through the limiter buys less than a dB), never
            // more than the limiter can take without pumping, nor back to a
            // gain found over. A margin widened by an earlier trial gives
            // back what this one measured under the ceiling, past the pad,
            // so the limiter stops taking loudness it needn't (#2361).
            let step = shortfall / slope.unwrap_or(1.0);
            let extra = arithmetic_gain_db + LIMITED_GAIN_EXTRA_DB;
            held_by_extra = gain_db + step > extra;
            gain_db = (gain_db + step).min(extra).min(gain_cap - LIMITER_OVERSHOOT_PAD_DB);
            let room = -over - LIMITER_OVERSHOOT_PAD_DB - if coded.is_some() { CODEC_OVERSHOOT_PAD_DB } else { 0.0 };
            if shortfall > 0.0 && room > 0.0 && margin_db > LIMITER_CEILING_MARGIN_DB {
                margin_db = (margin_db - room).max(LIMITER_CEILING_MARGIN_DB);
            }
        }
        // Back where a trial was: one under the ceiling would measure the
        // same, so the fit is as near as it gets. One over may have been
        // the codec's noise and is measured once more, never a third time.
        let before: Vec<&Tried> = tried.iter().filter(|t| (t.gain_db - gain_db).abs() < 1e-3 && (t.margin_db - margin_db).abs() < 1e-3).collect();
        if before.iter().any(|t| t.over <= 0.0) || before.len() >= 2 {
            break;
        }
    }
    // The delivery is a trial that was measured, never a correction nobody
    // measured: the one under the ceiling nearest the target loudness, or
    // with none under, the one least over, which the status then reports;
    // none when no trial measured a loudness.
    let best = tried
        .iter()
        .filter(|t| t.over <= 0.0)
        .min_by(|a, b| (a.lufs - target_lufs).abs().total_cmp(&(b.lufs - target_lufs).abs()))
        .or_else(|| tried.iter().min_by(|a, b| a.over.total_cmp(&b.over)));
    Ok(best.map(|t| LimitedFit {
        gain_db: t.gain_db,
        margin_db: t.margin_db,
        note: ((t.lufs - target_lufs).abs() >= 0.1 || t.over > 0.0)
            .then(|| {
                // Why, and every trial, so a miss can be read from the
                // report once the app's log is gone (#2361).
                let why = if held_by_extra {
                    format!("the limiter would have had to take more than {LIMITED_GAIN_EXTRA_DB} dB")
                } else {
                    "prioritising the peak ceiling".to_string()
                };
                let trials = tried
                    .iter()
                    .map(|t| format!("{:+.1} dB/{:.1} margin: {:.1} LUFS, {:+.1} dB peak", t.gain_db, t.margin_db, t.lufs, t.over))
                    .collect::<Vec<_>>()
                    .join("; ");
                let stopped = if tried.len() == LIMITED_GAIN_PASSES {
                    format!("fitting stopped at the {LIMITED_GAIN_PASSES}-trial limit")
                } else {
                    "fitting could go no further".to_string()
                };
                format!("{stopped}; using the best measured trial, {why} (trials: {trials})")
            }),
    }))
}

/// Where the trial encodes of a delivery's sound go (#1930): beside the
/// `.partial` file, on the disk the export writes to.
fn trial_path_for(partial: &Path) -> PathBuf {
    let mut buf = partial.as_os_str().to_owned();
    buf.push(".peak-trial");
    PathBuf::from(buf)
}

/// A trial encode of the delivery's sound (#1930): written as the export
/// writes it, frame by frame while the mix runs, then decoded and its true
/// peak read as FFmpeg's EBU R128 meter reads it
/// ([`crate::normalize::true_peak_dbtp`]). A lossy codec's ringing is in
/// it, and so is every inter-sample peak the live meter's 4x filter
/// misses. The file is removed when the trial is dropped.
struct CodecTrial {
    writer: Option<crate::audio_only::AudioOnly>,
    path: PathBuf,
    samples: u64,
    rate: u32,
}

impl CodecTrial {
    /// `None` for no audio, or a codec this FFmpeg can't encode.
    fn open(path: &Path, codec: klipp_core::ExportAudioCodec, bitrate_kbps: Option<u32>, rate: u32) -> Result<Option<Self>, RenderError> {
        let Some(mux) = crate::audio_only::trial_muxer(codec) else { return Ok(None) };
        if !crate::audio_only::has_encoder(codec) {
            return Ok(None);
        }
        let _ = std::fs::remove_file(path);
        let writer = crate::audio_only::AudioOnly::open_muxer(path, mux, codec, bitrate_kbps, rate)?;
        Ok(Some(Self { writer: Some(writer), path: path.to_path_buf(), samples: 0, rate }))
    }

    /// Interleaved stereo, in order.
    fn write(&mut self, interleaved: &[f32]) -> Result<(), RenderError> {
        self.samples += interleaved.len() as u64;
        match &mut self.writer {
            Some(w) => w.write(interleaved),
            None => Ok(()),
        }
    }

    /// The file's true peak, dBTP; `None` for a trial of silence.
    fn peak(mut self, cancel: &AtomicBool) -> Result<Option<f32>, RenderError> {
        let secs = self.samples as f64 / 2.0 / self.rate.max(1) as f64;
        let Some(writer) = self.writer.take().filter(|_| self.samples > 0) else { return Ok(None) };
        writer.finish()?;
        // A second over the length: an encoder's padding at the end.
        let peak = crate::normalize::true_peak_dbtp(&self.path, secs + 1.0, cancel)? as f32;
        Ok(peak.is_finite().then_some(peak))
    }
}

impl Drop for CodecTrial {
    fn drop(&mut self) {
        // The writer first: it holds the file open.
        self.writer = None;
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The written file at `path`, `secs` long, decoded and metered: what the
/// delivery measures as anyone else will measure it (#1930), its loudness
/// by the BS.1770 meter and its true peak as FFmpeg's EBU R128 meter
/// reads one, read in windows. `None` when it has no sound or can't be
/// read; a cancel stops it.
fn measure_written(path: &Path, secs: f64, cancel: &AtomicBool) -> Result<Option<klipp_core::normalize::Level>, RenderError> {
    // A second over the length: an encoder's padding at the end.
    let secs = secs.max(0.0) + 1.0;
    match crate::normalize::file_level(path, secs, cancel) {
        Ok(level) => Ok((level.integrated_lufs.is_finite() || level.true_peak_dbtp.is_finite()).then_some(level)),
        Err(RenderError::Cancelled) => Err(RenderError::Cancelled),
        Err(e) => {
            warn!("the written file at {path:?} could not be measured: {e}");
            Ok(None)
        }
    }
}

/// What a normalised delivery's status says once its file is written
/// (#1930): the mix as metered before the encoder, and the written file
/// decoded and metered, the ceiling held against the file's figure. With
/// the file unreadable, the mix's report as before.
fn delivery_status(delivery: &DeliveryChain, written: Option<klipp_core::normalize::Level>) -> Option<String> {
    let mix = delivery.report()?;
    let Some(file) = written else {
        let why = delivery.unverified.as_ref().map_or(String::new(), |why| format!("; {why}"));
        return Some(format!("{mix} (the written file could not be measured{why})"));
    };
    let (lufs, peak) = (delivery.measured_lufs()?, delivery.measured_true_peak_dbtp()?);
    let ceiling = delivery.ceiling_dbtp;
    let target = delivery.target_report(file.integrated_lufs as f32);
    let unverified = delivery.unverified.as_ref().map_or(String::new(), |why| format!("; {why}"));
    let base = format!(
        "Loudness — mix: {lufs:.1} LUFS, true peak {peak:.1} dBTP · written file: {:.1} LUFS, true peak {:.1} dBTP{target}{unverified}",
        file.integrated_lufs, file.true_peak_dbtp,
    );
    let over = file.true_peak_dbtp as f32 - ceiling;
    Some(if over > 0.05 {
        format!("{base} \u{2014} the written file is {over:.1} dB over the {ceiling:.1} dBTP ceiling")
    } else if file.true_peak_dbtp.is_finite() {
        format!("{base} — the written file meets the {ceiling:.1} dBTP ceiling")
    } else {
        format!("{base} — the written file's peak ceiling could not be verified")
    })
}

/// The delivery's sound: the normalisation gain, a limiter when that
/// gain would push the peaks past the ceiling, and a meter on what is
/// written so the job reports what the file measures, not what the
/// arithmetic promised.
struct DeliveryChain {
    gain: f32,
    limiter: Option<klipp_audio::dsp::limiter::Limiter>,
    ctx: klipp_audio::dsp::AudioProcessContext,
    meter: Option<klipp_audio::LufsMeter>,
    target_lufs: Option<f32>,
    ceiling_dbtp: f32,
    /// The highest sample that went into the limiter, linear (#1930): where
    /// the limiter's ceiling has to come down to before it holds anything.
    limiter_input_peak: f32,
    /// Why the ceiling wasn't checked through the delivery's encoder, when
    /// a trial of it failed (#1930): the status says so.
    unverified: Option<String>,
    /// Why fitting stopped before meeting both targets, if known.
    fit_note: Option<String>,
}

impl DeliveryChain {
    /// No normalisation: the mix as it is.
    fn unity() -> Self {
        Self {
            gain: 1.0,
            limiter: None,
            ctx: klipp_audio::dsp::AudioProcessContext { sample_rate: 48000, channels: 2, clip_time_secs: 0.0 },
            meter: None,
            target_lufs: None,
            ceiling_dbtp: DEFAULT_PEAK_CEILING_DBTP,
            limiter_input_peak: 0.0,
            unverified: None,
            fit_note: None,
        }
    }

    /// `limited_by` with the limiter's usual margin.
    #[cfg(test)]
    fn normalising(sample_rate: u32, target_lufs: f32, gain_db: f32, limit: bool, ceiling_dbtp: f32) -> Self {
        Self::limited_by(sample_rate, target_lufs, gain_db, limit, ceiling_dbtp, LIMITER_CEILING_MARGIN_DB)
    }

    /// `gain_db` towards `target_lufs`; with `limit`, a limiter after it
    /// holding the peaks under `ceiling_dbtp`, its own ceiling `margin_db`
    /// under that (`fit_limited` widens it for bright transients).
    fn limited_by(sample_rate: u32, target_lufs: f32, gain_db: f32, limit: bool, ceiling_dbtp: f32, margin_db: f32) -> Self {
        use klipp_audio::dsp::AudioEffect;
        let limiter = limit.then(|| {
            let mut l = klipp_audio::dsp::limiter::Limiter::new(sample_rate);
            let ceiling = ceiling_dbtp - margin_db;
            l.set_param("threshold", ceiling - LIMITER_KNEE_DB);
            l.set_param("ceiling", ceiling);
            // No lookahead: it would delay the sound against the picture.
            // The ceiling is a hard stop, so a transient the envelope has
            // not caught yet cannot pass it either.
            l.set_param("lookahead", 0.0);
            l.set_param("attack", 0.5);
            l.set_param("release", 80.0);
            l
        });
        Self {
            gain: db_to_linear(gain_db),
            limiter,
            ctx: klipp_audio::dsp::AudioProcessContext { sample_rate, channels: 2, clip_time_secs: 0.0 },
            meter: Some(klipp_audio::LufsMeter::new(sample_rate as f32, true)),
            target_lufs: Some(target_lufs),
            ceiling_dbtp,
            limiter_input_peak: 0.0,
            unverified: None,
            fit_note: None,
        }
    }

    /// One frame's interleaved stereo, in place.
    fn process(&mut self, audio: &mut [f32]) {
        use klipp_audio::dsp::AudioEffect;
        if self.gain != 1.0 {
            for s in audio.iter_mut() {
                *s *= self.gain;
            }
        }
        if let Some(l) = &mut self.limiter {
            self.limiter_input_peak = audio.iter().fold(self.limiter_input_peak, |p, s| p.max(s.abs()));
            l.process(audio, &self.ctx);
        }
        if let Some(m) = &mut self.meter {
            m.process(audio);
        }
    }

    /// The highest sample that went into the limiter, dBFS.
    fn limiter_input_peak_dbfs(&self) -> f32 {
        20.0 * self.limiter_input_peak.max(1e-9).log10()
    }

    /// The integrated loudness of what went through so far.
    fn measured_lufs(&self) -> Option<f32> {
        self.meter.as_ref().map(|m| m.readings().integrated)
    }

    /// The true peak of what went through so far, in dBTP.
    fn measured_true_peak_dbtp(&self) -> Option<f32> {
        self.meter.as_ref().map(|m| m.readings().true_peak_dbfs)
    }

    /// Describe the target against the measured signal, never infer success
    /// from the presence of a limiter. The final report uses decoded audio.
    fn target_report(&self, measured: f32) -> String {
        let Some(target) = self.target_lufs else { return String::new() };
        let delta = measured - target;
        let outcome = if !measured.is_finite() {
            "loudness target could not be verified".to_string()
        } else if delta.abs() < 0.1 {
            "loudness target met within 0.1 LU".to_string()
        } else if delta < 0.0 {
            format!("{:.1} LU below target", -delta)
        } else {
            format!("{delta:.1} LU above target")
        };
        let limited = if self.limiter.is_some() { "; peak limiting applied" } else { "" };
        let why = self.fit_note.as_ref().map_or(String::new(), |why| format!("; {why}"));
        format!(" (requested {target:.1} LUFS; {outcome}{limited}{why})")
    }

    /// What was written, measured. `None` without normalisation.
    fn report(&self) -> Option<String> {
        let r = self.meter.as_ref()?.readings();
        self.target_lufs?;
        let base = format!("Loudness: {:.1} LUFS, true peak {:.1} dBTP{}",
            r.integrated, r.true_peak_dbfs, self.target_report(r.integrated));
        // Held against the ceiling the delivery asked for, so a file that
        // would fail its spec's peak check says so here first.
        Some(if r.true_peak_dbfs > self.ceiling_dbtp + 0.05 {
            format!("{base} \u{2014} over the {:.1} dBTP ceiling", self.ceiling_dbtp)
        } else {
            base
        })
    }
}

/// Convert a gain in decibels to a linear amplitude multiplier.
/// Shared with klipp-audio's normalize DSP — same formula as
/// klipp-audio/src/dsp/normalize.rs:81.
fn db_to_linear(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

/// Convert a linear amplitude to decibels.
#[cfg(test)]
fn linear_to_db(linear: f32) -> f32 {
    if linear <= 0.0 { f32::NEG_INFINITY } else { 20.0 * linear.log10() }
}

/// The loudness of `snapshot`'s mix and the gain a delivery at
/// `target_lufs` gives it: the export's measurement pass, which playback
/// also runs to hear what the export will deliver (#1565). The meter
/// takes the summed mix unclamped, with 4x true-peak oversampling, so a
/// hot mix measures as it is and inter-sample peaks are caught.
/// `progress` hears the frames measured and the frames in all, every
/// hundred frames.
pub fn measure_delivery(
    snapshot: &SequenceSnapshot,
    target_lufs: f32,
    ceiling_dbtp: f32,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, u64),
) -> Result<LoudnessAnalysis, RenderError> {
    let (start_frame, end_frame) = (snapshot.start_frame(), snapshot.end_frame());
    let mut mixdown = AudioMixdown::new(snapshot)?;
    let mut meter = klipp_audio::LufsMeter::new(snapshot.sample_rate as f32, true);
    let total = end_frame - start_frame;
    for (i, frame_num) in (start_frame..end_frame).enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(RenderError::Cancelled);
        }
        meter.process(&mixdown.get_frame_audio_unclamped(frame_num));
        if i % 100 == 0 && total > 0 {
            progress(i as u64, total);
        }
    }
    let readings = meter.readings();
    Ok(LoudnessAnalysis::compute(target_lufs, readings.integrated, readings.true_peak_linear, ceiling_dbtp))
}

/// Result of the loudness measurement pass.
#[derive(Debug, Clone)]
pub struct LoudnessAnalysis {
    pub integrated_lufs: f32,
    pub peak_sample: f32,
    pub requested_gain_db: f32,
    pub applied_gain_db: f32,
    pub achieved_lufs: f32,
    pub peak_limited: bool,
}

impl LoudnessAnalysis {
    /// Compute the normalization gain from measured loudness and peak.
    /// `target_lufs`: the desired integrated loudness.
    /// `integrated_lufs`: the measured integrated loudness of the mix.
    /// `peak_sample`: the absolute value of the loudest sample (0.0–1.0+).
    /// `ceiling_dbtp`: the true-peak ceiling the delivery is held under.
    pub fn compute(target_lufs: f32, integrated_lufs: f32, peak_sample: f32, ceiling_dbtp: f32) -> Self {
        let requested_gain_db = target_lufs - integrated_lufs;
        let requested_gain_linear = db_to_linear(requested_gain_db);
        let peak_after_gain = peak_sample * requested_gain_linear;

        if peak_after_gain <= db_to_linear(ceiling_dbtp) {
            // Normal case — requested gain fits within headroom
            Self {
                integrated_lufs,
                peak_sample,
                requested_gain_db,
                applied_gain_db: requested_gain_db,
                achieved_lufs: target_lufs,
                peak_limited: false,
            }
        } else {
            // The peaks would pass the ceiling: the gain is applied in full
            // and a limiter holds them. It used to be reduced so the loudest
            // sample landed at the ceiling, which delivered a file five
            // decibels under what was asked for whenever the source was
            // already near full scale, which speech recordings are.
            Self {
                integrated_lufs,
                peak_sample,
                requested_gain_db,
                applied_gain_db: requested_gain_db,
                achieved_lufs: target_lufs,
                peak_limited: true,
            }
        }
    }
}

impl LoudnessAnalysis {
    /// The gain playback gives the mix to be heard as this delivery: the
    /// export's gain, but held so the measured true peak stays under
    /// `ceiling_dbtp`, since playback has no limiter. A peak-limited
    /// delivery so plays a little under the export, and never clips
    /// (#1565).
    pub fn playback_gain_db(&self, ceiling_dbtp: f32) -> f32 {
        // Nothing to hear, or nothing measured: unity.
        if !self.applied_gain_db.is_finite() || self.peak_sample <= 0.0 || !self.peak_sample.is_finite() {
            return 0.0;
        }
        self.applied_gain_db.min(ceiling_dbtp - 20.0 * self.peak_sample.log10())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::*;

    fn make_test_snapshot() -> SequenceSnapshot {
        SequenceSnapshot {
            fps: 24.0,
            fps_num: 24,
            fps_den: 1,
            width: 64,
            height: 64,
            sample_rate: 48000,
            duration_frames: 100,
            export_range: None,
            video: Default::default(),
            audio_tracks: vec![],
            pan_law: Default::default(),
            global_lut_path: None,
            subtitle_tracks: vec![], chapters: vec![],
            working_colorspace: klipp_core::WorkingColorspace::Rec709Linear, grade_encoding: klipp_core::GradeEncoding::Bt1886,
        }
    }

    #[test]
    fn cancellation_removes_partial_file() {
        let dir = std::env::temp_dir().join("klipp_export_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("cancel_test.mp4");

        let _ = std::fs::remove_file(&path); // Clean up from previous runs
        let cancel = Arc::new(AtomicBool::new(true)); // Cancel immediately

        let job = ExportJob {
            id: Uuid::new_v4(),
            output_path: path.clone(),
            settings: ExportSettings {
                pan_law: Default::default(),
                output_path: path.clone(),
                format: klipp_core::ExportFormat::Mp4,
                video_codec: klipp_core::ExportVideoCodec::H264,
                resolution: None,
                fps: None,
                crf: 23,
                preset: klipp_core::EncoderPreset::Fast,
                audio_codec: klipp_core::ExportAudioCodec::Copy,
                audio_bitrate_kbps: None, video_bitrate_kbps: None, rate_control: klipp_core::RateControl::Crf,
                burn_in_subtitles: true, embed_subtitles: false, target_lufs: None, true_peak_dbtp: None, embed_chapters: false, captions_608: false, hdr10plus: false,
                bit_depth: 0, hdr_mode: klipp_core::HdrMode::Sdr, tone_map_op: klipp_core::ToneMapOp::Hable,
                odt: klipp_core::OdtChoice::MatchViewer,
                mastering_peak_nits: klipp_core::DEFAULT_MASTERING_PEAK_NITS },
            snapshot: make_test_snapshot(),
            cancel: cancel.clone(),
            progress: Arc::new(AtomicU64::new(0)),
            hw_info: Arc::new(std::sync::Mutex::new(String::new())),
            render_stage: Arc::new(std::sync::Mutex::new(String::new())),
            loudness_result: Arc::new(std::sync::Mutex::new(None)),
            timings: Default::default(),
            total_frames: 100,
            analyser: None,
        };

        let result = job.run();
        assert!(matches!(result, Err(RenderError::Cancelled)));
        // R-5: the final path never appears on a cancelled run (we
        // write to the `.partial` sibling and only rename on success).
        assert!(!path.exists(), "output path should not appear on cancel");
        // And the `.partial` sibling must also be cleaned up so the
        // next run starts clean.
        let partial = partial_path_for(&path);
        assert!(
            !partial.exists(),
            ".partial should be cleaned up on cancel, still exists at {:?}",
            partial,
        );
    }

    #[test]
    fn partial_path_for_preserves_extension() {
        assert_eq!(
            partial_path_for(Path::new("/tmp/video.mp4")),
            PathBuf::from("/tmp/video.mp4.partial"),
        );
        assert_eq!(
            partial_path_for(Path::new("/tmp/video.mov")),
            PathBuf::from("/tmp/video.mov.partial"),
        );
        // No extension — still appends .partial so the muxer can't
        // accidentally pick it up as a finished artefact.
        assert_eq!(
            partial_path_for(Path::new("/tmp/nameonly")),
            PathBuf::from("/tmp/nameonly.partial"),
        );
    }

    #[test]
    fn progress_updates() {
        let progress = Arc::new(AtomicU64::new(0));
        // Just verify the progress counter mechanism
        progress.store(42, Ordering::Relaxed);
        assert_eq!(progress.load(Ordering::Relaxed), 42);
    }

    /// #1565: playback takes the export's gain where the peaks fit under
    /// the ceiling; where the export limits them, playback is held so its
    /// true peak lands on the ceiling, and a silent mix plays at unity.
    #[test]
    fn playback_gain_is_the_exports_held_under_the_ceiling() {
        let fits = LoudnessAnalysis::compute(-14.0, -18.1, 0.3, -1.0);
        assert!((fits.playback_gain_db(-1.0) - 4.1).abs() < 1e-3, "{}", fits.playback_gain_db(-1.0));
        let limited = LoudnessAnalysis::compute(-14.0, -20.0, 0.9, -1.0);
        let gain = limited.playback_gain_db(-1.0);
        assert!(gain < limited.applied_gain_db, "{gain} under the export's {}", limited.applied_gain_db);
        assert!((0.9 * db_to_linear(gain) - db_to_linear(-1.0)).abs() < 1e-4, "the peak lands on the ceiling");
        let silence = LoudnessAnalysis::compute(-14.0, f32::NEG_INFINITY, 0.0, -1.0);
        assert_eq!(silence.playback_gain_db(-1.0), 0.0);
    }

    /// #1565: the measurement playback runs is the export's: a mix with
    /// no sound measures as silence and gives unity, and it stops when
    /// cancelled.
    #[test]
    fn measure_delivery_measures_and_stops_when_cancelled() {
        let snap = make_test_snapshot();
        let a = measure_delivery(&snap, -14.0, -1.0, &AtomicBool::new(false), |_, _| {}).unwrap();
        assert!(!a.integrated_lufs.is_finite() || a.integrated_lufs < -70.0, "{}", a.integrated_lufs);
        assert_eq!(a.playback_gain_db(-1.0), 0.0);
        let cancelled = measure_delivery(&snap, -14.0, -1.0, &AtomicBool::new(true), |_, _| {});
        assert!(matches!(cancelled, Err(RenderError::Cancelled)));
    }

    // ── LoudnessAnalysis math tests ─────────────────────────────────

    #[test]
    fn loudness_normal_case_no_peak_limiting() {
        // Quiet mix at -20 LUFS, peak at 0.5 (-6 dBFS). Target -14.
        // Requested gain: +6 dB. Peak after gain: 0.5 * 2.0 = 1.0 > 0.891? Yes!
        // Actually that's peak-limited. Use lower peak instead.
        let a = LoudnessAnalysis::compute(-14.0, -20.0, 0.3, -1.0);
        // gain = +6 dB, peak after gain = 0.3 * ~2.0 = ~0.6 < 0.891
        assert!(!a.peak_limited);
        assert!((a.requested_gain_db - 6.0).abs() < 0.01);
        assert!((a.applied_gain_db - 6.0).abs() < 0.01);
        assert!((a.achieved_lufs - (-14.0)).abs() < 0.01);
    }

    #[test]
    fn loudness_over_the_ceiling_keeps_the_gain_and_limits() {
        // Mix at -20 LUFS, peak at 0.9 (-0.9 dBFS). Target -14: +6 dB
        // would put the peak at 1.8. The gain stays and a limiter holds
        // the peaks; it used to be cut to -0.08 dB, delivering -19.9 LUFS.
        let a = LoudnessAnalysis::compute(-14.0, -20.0, 0.9, -1.0);
        assert!(a.peak_limited);
        assert!((a.requested_gain_db - 6.0).abs() < 0.01);
        assert!((a.applied_gain_db - 6.0).abs() < 0.01);
        assert!((a.achieved_lufs - (-14.0)).abs() < 0.01);
    }

    #[test]
    fn a_limited_delivery_stays_under_the_ceiling() {
        // A tone near full scale, six decibels of gain on it: without
        // the limiter every sample would clip.
        let sr = 48000;
        let mut chain = DeliveryChain::normalising(sr, -14.0, 6.0, true, -1.0);
        let mut peak: f32 = 0.0;
        for block in 0..50 {
            let mut audio: Vec<f32> = (0..1600)
                .flat_map(|i| {
                    let t = (block * 1600 + i) as f32 / sr as f32;
                    let v = 0.9 * (2.0 * std::f32::consts::PI * 440.0 * t).sin();
                    [v, v]
                })
                .collect();
            chain.process(&mut audio);
            peak = audio.iter().fold(peak, |p, s| p.max(s.abs()));
        }
        assert!(peak <= db_to_linear(-1.0 - LIMITER_CEILING_MARGIN_DB) + 1e-4, "peak {peak}");
        assert!(peak > 0.5, "the tone is still there: {peak}");
        let report = chain.report().unwrap();
        assert!(report.contains("true peak") && report.contains("peak limiting applied"), "{report}");

        // Gain that fits needs no limiter and passes the samples as they are.
        let mut plain = DeliveryChain::normalising(sr, -14.0, 0.0, false, -1.0);
        let mut audio = vec![0.25f32; 400];
        plain.process(&mut audio);
        assert!(audio.iter().all(|s| (*s - 0.25).abs() < 1e-6));
        assert!(DeliveryChain::unity().report().is_none());
    }

    /// #781: bright transients whose true peak runs further over their
    /// samples than the limiter's margin. A bed at about the target with
    /// a 5 ms burst of 12 kHz four times a second, sampled 33° off its
    /// crests so it peaks 1.5 dB above every sample: the fixed margin lets
    /// the bursts through over the ceiling; the fitted one holds them and
    /// the loudness stays on the target.
    #[test]
    fn a_limited_delivery_holds_its_true_peak_under_the_ceiling() {
        let sr = 48000;
        let mix: Vec<f32> = (0..sr as usize * 4)
            .flat_map(|i| {
                let bed = 0.2 * (std::f32::consts::TAU * 200.0 * i as f32 / sr as f32).sin();
                let burst = if i % (sr as usize / 4) < 240 { (std::f32::consts::FRAC_PI_2 * i as f32 + 33f32.to_radians()).sin() } else { 0.0 };
                [bed + burst, bed + burst]
            })
            .collect();
        let feed = |chain: &mut DeliveryChain| {
            for block in mix.chunks(3200) {
                let mut audio = block.to_vec();
                chain.process(&mut audio);
            }
        };

        let mut fixed = DeliveryChain::normalising(sr, -14.0, 0.0, true, -1.0);
        feed(&mut fixed);
        assert!(fixed.report().unwrap().contains("over the -1.0 dBTP ceiling"), "the case needs the fit: {}", fixed.report().unwrap());

        let LimitedFit { gain_db, margin_db, .. } = fit_limited(sr, -14.0, -1.0, 0.0, "test", |trial, _| {
            feed(trial);
            Ok(None)
        })
        .unwrap()
        .expect("a measured trial");
        assert!(margin_db > LIMITER_CEILING_MARGIN_DB, "the margin was not widened: {margin_db}");
        let mut fitted = DeliveryChain::limited_by(sr, -14.0, gain_db, true, -1.0, margin_db);
        feed(&mut fitted);
        let peak = fitted.measured_true_peak_dbtp().unwrap();
        assert!(peak <= -1.0, "true peak {peak:.2} dBTP over the -1.0 ceiling");
        let lufs = fitted.measured_lufs().unwrap();
        assert!((lufs + 14.0).abs() < 0.3, "the loudness left the target: {lufs:.2} LUFS");
        assert!(!fitted.report().unwrap().contains("over the"), "{}", fitted.report().unwrap());
    }

    /// A loud, dense mix (hits half the time over a quiet bed) and its
    /// arithmetic gain, the gain that would bring it to `target` unlimited.
    fn dense_mix(sr: u32, target: f32) -> (Vec<f32>, f32) {
        let mix: Vec<f32> = (0..sr as usize * 4)
            .flat_map(|i| {
                let t = i as f32 / sr as f32;
                let bed = 0.03 * (std::f32::consts::TAU * 150.0 * t).sin();
                let hit = if i % (sr as usize / 10) < sr as usize / 20 { 0.7 * (std::f32::consts::TAU * 400.0 * t).sin() } else { 0.0 };
                [bed + hit, bed + hit]
            })
            .collect();
        let mut plain = DeliveryChain::limited_by(sr, target, 0.0, false, -1.0, LIMITER_CEILING_MARGIN_DB);
        plain.process(&mut mix.clone());
        let arithmetic = target - plain.measured_lufs().unwrap();
        (mix, arithmetic)
    }

    /// #2361: through the limiter a dB of gain buys less than a dB of
    /// loudness. Stepped as if it bought one, the fit crawled a tenth a
    /// trial and stopped at the limit short of the target, blaming the
    /// ceiling with a dB and more of headroom; stepped by the slope two
    /// trials measured, it lands on it in a few. Past what the limiter may
    /// take, it goes to that limit and says that is why.
    #[test]
    fn a_limited_fit_steps_by_the_measured_slope() {
        let sr = 48000;
        let fit_at = |target: f32| {
            let (mix, arithmetic) = dense_mix(sr, target);
            let feed = |chain: &mut DeliveryChain| {
                for block in mix.chunks(3200) {
                    chain.process(&mut block.to_vec());
                }
            };
            let mut passes = 0;
            let fit = fit_limited(sr, target, -1.0, arithmetic, "test", |trial, _| {
                passes += 1;
                feed(trial);
                Ok(None)
            })
            .unwrap()
            .expect("a measured trial");
            let mut fitted = DeliveryChain::limited_by(sr, target, fit.gain_db, true, -1.0, fit.margin_db);
            feed(&mut fitted);
            (fit, arithmetic, passes, fitted.measured_lufs().unwrap(), fitted.measured_true_peak_dbtp().unwrap())
        };
        let (fit, _, passes, lufs, peak) = fit_at(-6.7);
        assert!((lufs + 6.7).abs() < 0.15 && fit.note.is_none(), "{lufs:.2} LUFS against -6.7: {fit:?}");
        assert!(passes <= 3 && peak <= -1.0, "{passes} passes, true peak {peak:.2} dBTP");
        let (fit, arithmetic, ..) = fit_at(-6.0);
        assert!((fit.gain_db - arithmetic - LIMITED_GAIN_EXTRA_DB).abs() < 1e-3, "to the limit: {fit:?}");
        assert!(fit.note.unwrap().contains("more than 3 dB"));
    }

    /// #2361: one trial over only through the codec, whose overshoot moves
    /// a few tenths with any change to what it encodes, doesn't cap the
    /// gain; a widened margin gives back what later trials measure under
    /// the ceiling. The fit still reaches the target.
    #[test]
    fn one_noisy_codec_overshoot_does_not_hold_the_fit_short() {
        let sr = 48000;
        let target = -8.0;
        let (mix, arithmetic) = dense_mix(sr, target);
        let mut pass = 0;
        let fit = fit_limited(sr, target, -1.0, arithmetic, "test", |trial, _| {
            pass += 1;
            for block in mix.chunks(3200) {
                trial.process(&mut block.to_vec());
            }
            // The first trial's encode rings up 0.8 dB over the ceiling.
            Ok(Some(if pass == 1 { -0.2 } else { trial.measured_true_peak_dbtp().unwrap() }))
        })
        .unwrap()
        .expect("a measured trial");
        let mut fitted = DeliveryChain::limited_by(sr, target, fit.gain_db, true, -1.0, fit.margin_db);
        for block in mix.chunks(3200) {
            fitted.process(&mut block.to_vec());
        }
        let lufs = fitted.measured_lufs().unwrap();
        assert!((lufs - target).abs() < 0.15 && fit.note.is_none(), "{lufs:.2} LUFS against {target}: {fit:?}");
    }

    /// #1930: with no trial measuring a loudness, the fit gives no gain
    /// rather than one nothing measured; the export says the ceiling
    /// wasn't checked.
    #[test]
    fn a_fit_with_nothing_measured_gives_no_gain() {
        let mut passes = 0;
        let fit = fit_limited(48000, -14.0, -1.0, 2.0, "test", |_, _| {
            passes += 1;
            Ok(Some(0.0))
        })
        .unwrap();
        assert_eq!(fit, None, "a gain no trial measured");
        assert_eq!(passes, 1, "nothing to correct from");
    }

    /// #1930: a trial encode goes beside the `.partial` file, stops on a
    /// cancel while it is read back, and leaves no file behind either way.
    #[test]
    fn a_trial_encode_stops_on_cancel_and_leaves_nothing() {
        let codec = klipp_core::ExportAudioCodec::Pcm16;
        let dir = klipp_testing::scratch::scratch_dir("klipp-codec-trial");
        let partial = partial_path_for(&dir.join("out.mp4"));
        let path = trial_path_for(&partial);
        assert_eq!(path.parent(), partial.parent(), "beside the partial");
        let tone: Vec<f32> = (0..48000 * 2).map(|i| 0.25 * ((i / 2) as f32 * 0.05).sin()).collect();
        for cancelled in [true, false] {
            let mut trial = CodecTrial::open(&path, codec, None, 48000).unwrap().expect("a WAV trial");
            trial.write(&tone).unwrap();
            assert!(path.exists(), "written as the mix runs");
            let got = trial.peak(&AtomicBool::new(cancelled));
            if cancelled {
                assert!(matches!(got, Err(RenderError::Cancelled)), "{got:?}");
            } else {
                let peak = got.unwrap().expect("a peak");
                assert!((peak - 20.0 * 0.25f32.log10()).abs() < 0.2, "{peak}");
            }
            assert!(!path.exists(), "the trial file is removed");
        }
    }

    /// #1930: a trial encode that failed is said in the status, beside the
    /// written file's figures.
    #[test]
    fn an_unverified_ceiling_is_said_in_the_status() {
        let sr = 48000;
        let mut chain = DeliveryChain::limited_by(sr, -14.0, 0.0, true, -1.0, LIMITER_CEILING_MARGIN_DB);
        let mut tone: Vec<f32> = (0..sr as usize * 4).flat_map(|i| {
            let v = 0.2 * (i as f32 * 440.0 * std::f32::consts::TAU / sr as f32).sin();
            [v, v]
        }).collect();
        chain.process(&mut tone);
        let file = klipp_core::normalize::Level { integrated_lufs: -14.0, sample_peak_dbfs: -2.0, true_peak_dbtp: -1.5 };
        assert!(!delivery_status(&chain, Some(file)).unwrap().contains("not checked"));
        chain.unverified = Some("the ceiling was not checked through the AAC encoder (open audio encoder: refused)".into());
        let status = delivery_status(&chain, Some(file)).unwrap();
        assert!(status.contains("written file") && status.contains("not checked through the AAC encoder"), "{status}");
        assert!(delivery_status(&chain, None).unwrap().contains("not checked through the AAC encoder"));
    }

    #[test]
    fn written_loudness_shortfall_is_not_reported_as_target_success() {
        let mut chain = DeliveryChain::normalising(48000, -16.0, 0.0, true, -3.0);
        let mut tone = vec![0.1; 48000 * 2];
        chain.process(&mut tone);
        chain.fit_note = Some("fitting stopped at the 5-trial limit".into());
        let status = delivery_status(&chain, Some(klipp_core::normalize::Level {
            integrated_lufs: -16.7, sample_peak_dbfs: -3.4, true_peak_dbtp: -3.2,
        })).unwrap();
        assert!(status.contains("0.7 LU below target"), "{status}");
        assert!(status.contains("meets the -3.0 dBTP ceiling"), "{status}");
        assert!(status.contains("5-trial limit"), "{status}");
        assert!(!status.contains("to reach it") && !status.contains("target met"), "{status}");
        assert!(chain.target_report(-15.5).contains("0.5 LU above target"));
        assert!(chain.target_report(-16.04).contains("target met within 0.1 LU"));
        assert!(chain.target_report(f32::NEG_INFINITY).contains("could not be verified"));
        assert!(delivery_status(&chain, None).unwrap().contains("written file could not be measured"));
    }

    /// A fit that can't reach the target says so, with every trial it
    /// measured; one held where it was stops rather than measure the same
    /// trial again (#2361).
    #[test]
    fn a_fit_that_cannot_converge_reports_its_trials() {
        let mut passes = 0;
        let fit = fit_limited(48000, -16.0, -3.0, 0.0, "test", |trial, _| {
            // A fixed metered signal cannot converge as gain changes.
            trial.meter.as_mut().unwrap().process(&vec![0.01; 48000 * 2]);
            passes += 1;
            Ok(Some(-6.0))
        }).unwrap().unwrap();
        assert!(passes < LIMITED_GAIN_PASSES, "no trial run twice: {passes}");
        let note = fit.note.unwrap();
        assert!(note.contains("using the best measured trial") && note.matches(" LUFS").count() == passes, "{note}");
    }

    #[test]
    fn an_atsc_delivery_is_held_under_minus_two_and_says_when_it_is_over() {
        // The same hot tone, delivered to ATSC A/85's -2 dBTP.
        let sr = 48000;
        let tone = |block: usize| -> Vec<f32> {
            (0..1600)
                .flat_map(|i| {
                    let t = (block * 1600 + i) as f32 / sr as f32;
                    let v = 0.9 * (2.0 * std::f32::consts::PI * 440.0 * t).sin();
                    [v, v]
                })
                .collect()
        };
        let mut atsc = DeliveryChain::normalising(sr, -24.0, 6.0, true, -2.0);
        let mut peak: f32 = 0.0;
        for block in 0..50 {
            let mut audio = tone(block);
            atsc.process(&mut audio);
            peak = audio.iter().fold(peak, |p, s| p.max(s.abs()));
        }
        // The limiter holds the samples 0.7 dB under the true-peak ceiling:
        // a full decibel lower than an R128 delivery's.
        assert!(peak <= db_to_linear(-2.7) + 1e-4, "peak {}", linear_to_db(peak));
        assert!(!atsc.report().unwrap().contains("over the"), "{}", atsc.report().unwrap());

        // The measurement pass flags the need to limit against the ceiling
        // asked for, not a fixed -1: a peak at -1.5 dBTP after gain fits
        // R128 and does not fit A/85.
        let peak_at = db_to_linear(-1.5);
        assert!(!LoudnessAnalysis::compute(-23.0, -23.0, peak_at, -1.0).peak_limited);
        assert!(LoudnessAnalysis::compute(-24.0, -24.0, peak_at, -2.0).peak_limited);

        // A file that ends up over its ceiling says so in the job's report.
        let mut unlimited = DeliveryChain::normalising(sr, -24.0, 6.0, false, -2.0);
        for block in 0..50 {
            let mut audio = tone(block);
            unlimited.process(&mut audio);
        }
        assert!(unlimited.report().unwrap().contains("over the -2.0 dBTP ceiling"), "{}", unlimited.report().unwrap());
    }

    #[test]
    fn loudness_mix_already_too_loud_negative_gain() {
        // Mix at -10 LUFS, peak at 0.8. Target -14.
        // Requested gain: -4 dB (reduce volume). No peak concern.
        let a = LoudnessAnalysis::compute(-14.0, -10.0, 0.8, -1.0);
        assert!(!a.peak_limited);
        assert!((a.requested_gain_db - (-4.0)).abs() < 0.01);
        assert!((a.applied_gain_db - (-4.0)).abs() < 0.01);
        assert!((a.achieved_lufs - (-14.0)).abs() < 0.01);
    }

    #[test]
    fn loudness_edge_case_peak_exactly_at_ceiling() {
        // After gain, peak lands exactly at -1 dBFS (0.891)
        // Mix at -20 LUFS, target -14, need peak such that 0.891 / gain = peak
        let gain_linear = db_to_linear(6.0); // ~2.0
        let peak = db_to_linear(DEFAULT_PEAK_CEILING_DBTP) / gain_linear; // ~0.4455
        let a = LoudnessAnalysis::compute(-14.0, -20.0, peak, -1.0);
        assert!(!a.peak_limited); // Exactly at ceiling, not over
        assert!((a.achieved_lufs - (-14.0)).abs() < 0.01);
    }

    #[test]
    fn loudness_zero_gain_when_already_at_target() {
        let a = LoudnessAnalysis::compute(-14.0, -14.0, 0.5, -1.0);
        assert!(!a.peak_limited);
        assert!(a.requested_gain_db.abs() < 0.01);
        assert!(a.applied_gain_db.abs() < 0.01);
        assert!((a.achieved_lufs - (-14.0)).abs() < 0.01);
    }

    #[test]
    fn db_linear_roundtrip() {
        for db in [-20.0, -6.0, 0.0, 3.0, 6.0, 12.0] {
            let linear = db_to_linear(db);
            let back = linear_to_db(linear);
            assert!((back - db).abs() < 0.001, "roundtrip failed for {db} dB");
        }
    }

    // ── fit_to_target math tests ────────────────────────────────

    #[test]
    fn fit_equal_aspect_16_9() {
        // 1920×1080 → 1280×720: same 16:9 ratio, no padding
        let (fw, fh, ox, oy) = fit_to_target(1920, 1080, 1280, 720);
        assert_eq!((fw, fh), (1280, 720));
        assert_eq!((ox, oy), (0, 0));
    }

    #[test]
    fn fit_portrait_into_landscape() {
        // 2160×3840 (9:16 portrait) → 1280×720 (16:9 landscape): pillarbox
        let (fw, fh, ox, oy) = fit_to_target(2160, 3840, 1280, 720);
        // fitted_w = (720 * 2160/3840) = 405 → 404 (even)
        assert_eq!(fh, 720);
        assert_eq!(fw, 404);
        assert!(fw % 2 == 0 && fh % 2 == 0, "dims must be even");
        assert_eq!(ox, (1280 - 404) / 2); // 438
        assert_eq!(oy, 0);
    }

    #[test]
    fn fit_landscape_into_portrait() {
        // 3840×2160 (16:9) → 720×1280 (9:16 portrait): letterbox
        let (fw, fh, ox, oy) = fit_to_target(3840, 2160, 720, 1280);
        // fitted_w = 720, fitted_h = 720 * 2160/3840 = 405 → 404 (even)
        assert_eq!(fw, 720);
        assert_eq!(fh, 404);
        assert!(fw % 2 == 0 && fh % 2 == 0);
        assert_eq!(ox, 0);
        assert_eq!(oy, (1280 - 404) / 2); // 438
    }

    #[test]
    fn fit_identity() {
        let (fw, fh, ox, oy) = fit_to_target(2160, 3840, 2160, 3840);
        assert_eq!((fw, fh), (2160, 3840));
        assert_eq!((ox, oy), (0, 0));
    }

    #[test]
    fn fit_dims_always_even() {
        // Check a case that would produce odd fitted dims without the &!1 fix
        let (fw, fh, _, _) = fit_to_target(1001, 1000, 500, 500);
        assert!(fw % 2 == 0, "fitted_w must be even, got {fw}");
        assert!(fh % 2 == 0, "fitted_h must be even, got {fh}");
    }

    #[test]
    fn fit_small_into_large() {
        // 100×100 → 1920×1080: pillarbox (source is square, target is wide)
        let (fw, fh, ox, oy) = fit_to_target(100, 100, 1920, 1080);
        assert_eq!(fh, 1080);
        assert_eq!(fw, 1080); // square source fits to height
        assert!(fw % 2 == 0 && fh % 2 == 0);
        assert_eq!(ox, (1920 - 1080) / 2); // 420
        assert_eq!(oy, 0);
    }

    #[test]
    fn fit_tiny_target_no_panic() {
        let (fw, fh, _, _) = fit_to_target(1920, 1080, 4, 4);
        assert!(fw <= 4 && fh <= 4);
        assert!(fw % 2 == 0 && fh % 2 == 0);
    }
}
