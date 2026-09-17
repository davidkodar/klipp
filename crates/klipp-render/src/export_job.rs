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
use crate::error::RenderError;
use crate::frame_source::ExportFrameSource;
use crate::snapshot::{SequenceSnapshot, SubtitleClipSnapshot};
use klipp_core::ExportSettings;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tracing::{debug, info};
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

        Self::set_stage(&self.render_stage, "Initializing\u{2026}");

        // ─── Initialise all systems ───────────────────────────────
        // The viewer's renderer. OFX plugin discovery is slow; only when a
        // clip uses a plugin.
        let mut ofx = klipp_plugin::OfxHost::new();
        if timeline.tracks.iter().flat_map(|t| &t.clips).any(|c| !c.effects.is_empty()) {
            ofx.discover_plugins();
        }
        let mut renderer = klipp_compose::Renderer::new(ofx, klipp_title::TitleRenderer::new());
        renderer.set_working_colorspace(self.snapshot.working_colorspace);
        // Tetrahedral, always: the trilinear option exists so the viewer
        // can stay responsive, and a delivery is not the place to trade
        // LUT accuracy for speed.
        renderer.set_lut_tetrahedral(true);
        if let Some(path) = &self.snapshot.global_lut_path {
            match klipp_color::lut::load_lut(path) {
                Ok(lut) => renderer.set_global_lut(Some(klipp_compose::GlobalLut {
                    lut,
                    opacity: self.snapshot.video.global_lut_opacity,
                })),
                Err(e) => tracing::warn!("ExportJob {}: global LUT {:?} failed to load: {e}", self.id, path),
            }
        }
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
        let mut encoder =
            Encoder::open(&partial_path, &self.settings, &self.snapshot)?;

        // Determine frame range
        let start_frame = self.snapshot.start_frame();
        let end_frame = self.snapshot.end_frame();

        // ─── Pass 1: Loudness measurement (if normalization enabled) ──
        let mut delivery = if let Some(target_lufs) = self.settings.target_lufs {
            Self::set_stage(&self.render_stage, "Measuring loudness\u{2026}");
            let mut measure_mixdown = AudioMixdown::new(&self.snapshot)?;
            // Always request 4× true-peak oversampling for export — the
            // peak-ceiling check below relies on detecting inter-sample
            // peaks so the rendered file stays within -1 dBFS true-peak.
            let mut meter = klipp_audio::LufsMeter::new(self.snapshot.sample_rate as f32, true);
            let measure_total = end_frame - start_frame;
            for (i, frame_num) in (start_frame..end_frame).enumerate() {
                if self.cancel.load(Ordering::Relaxed) {
                    return Err(RenderError::Cancelled);
                }
                // R-3: feed the UN-clamped summed mix to the LUFS meter
                // so over-1.0 energy is measured as-is. Clamping first
                // (the old behaviour via get_frame_audio) capped the
                // apparent loudness of hot mixes and caused
                // loudness-normalize to over-shoot the requested target.
                let audio = measure_mixdown.get_frame_audio_unclamped(frame_num);
                meter.process(&audio);
                // Report measurement progress (roughly first ~10-15% of total work)
                if i % 100 == 0 && measure_total > 0 {
                    self.progress.store((i as u64 * self.total_frames) / (measure_total * 10), Ordering::Relaxed);
                }
            }
            let readings = meter.readings();
            // Use the meter's true-peak (oversampled) for the -1 dBFS gate
            // — same units as the old hand-rolled sample peak (linear 0..1+),
            // but catches inter-sample peaks the previous code missed.
            let analysis = LoudnessAnalysis::compute(target_lufs, readings.integrated, readings.true_peak_linear);
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
            // A limiter takes loudness with the peaks it holds, so the gain
            // that reaches the target through it is a little more than the
            // arithmetic says. Measured, not guessed: the mix goes through
            // the chain again and the gain takes up the shortfall.
            let mut gain_db = analysis.applied_gain_db;
            if analysis.peak_limited {
                for pass in 0..LIMITED_GAIN_PASSES {
                    let mut trial = DeliveryChain::normalising(self.snapshot.sample_rate, target_lufs, gain_db, true);
                    let mut trial_mix = AudioMixdown::new(&self.snapshot)?;
                    for frame_num in start_frame..end_frame {
                        if self.cancel.load(Ordering::Relaxed) {
                            return Err(RenderError::Cancelled);
                        }
                        let mut audio = trial_mix.get_frame_audio(frame_num);
                        trial.process(&mut audio);
                    }
                    let Some(got) = trial.measured_lufs().filter(|l| l.is_finite()) else { break };
                    let shortfall = target_lufs - got;
                    info!(
                        "ExportJob {}: limited pass {}: {gain_db:.2} dB of gain measures {got:.2} LUFS, {shortfall:+.2} from the target",
                        self.id, pass + 1,
                    );
                    if shortfall.abs() < 0.1 {
                        break;
                    }
                    // Never more than the limiter can take without pumping.
                    gain_db = (gain_db + shortfall).min(analysis.applied_gain_db + LIMITED_GAIN_EXTRA_DB);
                }
            }
            DeliveryChain::normalising(self.snapshot.sample_rate, target_lufs, gain_db, analysis.peak_limited)
        } else {
            DeliveryChain::unity()
        };

        let mut export_frame_num: u64 = 0;

        // Output size, and where the sequence's picture sits in it: an
        // export resolution with another aspect is letterboxed or
        // pillarboxed, not stretched.
        let (out_w, out_h) = match &self.settings.resolution {
            Some(r) => (r.width & !1, r.height & !1),
            None => (self.snapshot.width, self.snapshot.height),
        };
        let (fit_w, fit_h, fit_x, fit_y) = if (out_w, out_h) == (self.snapshot.width, self.snapshot.height) {
            (out_w, out_h, 0, 0)
        } else {
            fit_to_target(self.snapshot.width, self.snapshot.height, out_w, out_h)
        };
        let mut hw_reported = false;
        // An HLG or PQ delivery renders display light for the encoder's
        // HDR encode (an ACES PQ transform renders its own from SDR).
        let render_opts = klipp_compose::RenderOptions {
            hdr_peak_nits: self.settings.renders_hdr().then(|| self.settings.hdr_peak_nits()),
            ..Default::default()
        };

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

        // Whether a frame can be converted to the encoder's planes on the
        // GPU, and with what. Requires a delivery the shader can finish
        // (`gpu_yuv420_params`) and a picture that fills the output — a
        // letterboxed export is padded on the CPU, which needs the frame
        // there anyway.
        let mut gpu_yuv = if (fit_w, fit_h) == (out_w, out_h) {
            encoder.gpu_yuv420_params()
        } else {
            None
        };
        debug!(
            "ExportJob {}: GPU frame conversion {} (fit {fit_w}x{fit_h}, out {out_w}x{out_h}, \
             encoder format {:?})",
            self.id,
            if gpu_yuv.is_some() { "available" } else { "unavailable" },
            encoder.video_format(),
        );

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

            // The frame converted on the GPU, when the whole render is one
            // clip's grade and the delivery is one this can finish: then
            // 12.4 MB of Y'CbCr planes come back instead of a 132 MB
            // linear frame, and the per-pixel display-and-convert pass on
            // the CPU does not happen at all.
            //
            // `gpu_yuv_frames` latches off after a miss. The gate inside
            // `render_yuv420_on_gpu` is structural and cheap, but if the
            // grade itself comes back on the CPU — a node with no GPU
            // dispatch — then this frame is rendered twice, and without
            // the latch every remaining frame would be too.
            let planes = gpu_yuv.and_then(|target| {
                renderer.render_yuv420_on_gpu(
                    timeline, time, fit_w, fit_h, &mut frames, render_opts, target,
                )
            });
            if let Some((planes, light)) = planes {
                frames.end_frame()?;
                t_render += t0.elapsed();
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
            if gpu_yuv.is_some() && renderer.gpu_export_declined_after_work() {
                // The miss came after the frame was rendered — a grade
                // coming back on the CPU, a stack failing part-way — so
                // this frame was rendered twice, and every frame after it
                // would be. A miss at the gate costs nothing and is a
                // property of this frame alone: a plugin clip's stretch of
                // the timeline goes the ordinary way and the GPU path
                // resumes after it.
                gpu_yuv = None;
                debug!(
                    "ExportJob {}: GPU frame conversion off from frame {frame_num}",
                    self.id
                );
            }

            let picture = renderer.render(timeline, time, fit_w, fit_h, &mut frames, render_opts);
            frames.end_frame()?;
            t_render += t0.elapsed();
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
            let t1 = std::time::Instant::now();
            let picture = pad_linear(picture, fit_w, fit_h, out_w, out_h, fit_x, fit_y);
            t_pad += t1.elapsed();
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
fn pad_linear(frame: Vec<f32>, w: u32, h: u32, dst_w: u32, dst_h: u32, x: u32, y: u32) -> Vec<f32> {
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
fn partial_path_for(final_path: &Path) -> PathBuf {
    let mut buf = final_path.as_os_str().to_owned();
    buf.push(".partial");
    PathBuf::from(buf)
}

/// Size and offset that fit a `(src_w, src_h)` picture
/// into `(target_w, target_h)` while preserving aspect ratio.
/// Returns (fitted_w, fitted_h, offset_x, offset_y).
/// Fitted dimensions are forced even for YUV420p chroma subsampling compatibility.
fn fit_to_target(src_w: u32, src_h: u32, target_w: u32, target_h: u32) -> (u32, u32, u32, u32) {
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

/// -1 dBFS as a linear amplitude value: the true-peak ceiling of a
/// delivery. Gain that would push the measured peak past it is still
/// applied in full, and a limiter holds the peaks under it.
const PEAK_CEILING_LINEAR: f32 = 0.891; // 10^(-1.0/20.0) ≈ 0.891

/// The limiter that holds a normalised delivery under the ceiling. It
/// detects sample peaks, so its ceiling sits under the true-peak one:
/// inter-sample peaks of limited speech and music run a few tenths of a
/// decibel over the samples.
const LIMITER_THRESHOLD_DB: f32 = -2.5;
const LIMITER_CEILING_DB: f32 = -1.3;
/// How many times a limited delivery's gain is corrected against a
/// measurement through the limiter, and how far past the arithmetic
/// gain the corrections may take it.
const LIMITED_GAIN_PASSES: usize = 3;
const LIMITED_GAIN_EXTRA_DB: f32 = 3.0;

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
        }
    }

    /// `gain_db` towards `target_lufs`; with `limit`, a limiter after it.
    fn normalising(sample_rate: u32, target_lufs: f32, gain_db: f32, limit: bool) -> Self {
        use klipp_audio::dsp::AudioEffect;
        let limiter = limit.then(|| {
            let mut l = klipp_audio::dsp::limiter::Limiter::new(sample_rate);
            l.set_param("threshold", LIMITER_THRESHOLD_DB);
            l.set_param("ceiling", LIMITER_CEILING_DB);
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
            l.process(audio, &self.ctx);
        }
        if let Some(m) = &mut self.meter {
            m.process(audio);
        }
    }

    /// The integrated loudness of what went through so far.
    fn measured_lufs(&self) -> Option<f32> {
        self.meter.as_ref().map(|m| m.readings().integrated)
    }

    /// What was written, measured. `None` without normalisation.
    fn report(&self) -> Option<String> {
        let r = self.meter.as_ref()?.readings();
        let target = self.target_lufs?;
        Some(if self.limiter.is_some() {
            format!(
                "Loudness: {:.1} LUFS, true peak {:.1} dBTP (requested {target:.1}; the peaks were limited to reach it)",
                r.integrated, r.true_peak_dbfs
            )
        } else {
            format!("Loudness: {:.1} LUFS, true peak {:.1} dBTP", r.integrated, r.true_peak_dbfs)
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
    pub fn compute(target_lufs: f32, integrated_lufs: f32, peak_sample: f32) -> Self {
        let requested_gain_db = target_lufs - integrated_lufs;
        let requested_gain_linear = db_to_linear(requested_gain_db);
        let peak_after_gain = peak_sample * requested_gain_linear;

        if peak_after_gain <= PEAK_CEILING_LINEAR {
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
            working_colorspace: klipp_core::WorkingColorspace::Rec709Linear,
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
                burn_in_subtitles: true, embed_subtitles: false, target_lufs: None, embed_chapters: false,
                bit_depth: 0, hdr_mode: klipp_core::HdrMode::Sdr, tone_map_op: klipp_core::ToneMapOp::Hable,
                odt: klipp_core::OdtChoice::MatchViewer,
                mastering_peak_nits: klipp_core::DEFAULT_MASTERING_PEAK_NITS },
            snapshot: make_test_snapshot(),
            cancel: cancel.clone(),
            progress: Arc::new(AtomicU64::new(0)),
            hw_info: Arc::new(std::sync::Mutex::new(String::new())),
            render_stage: Arc::new(std::sync::Mutex::new(String::new())),
            loudness_result: Arc::new(std::sync::Mutex::new(None)),
            total_frames: 100,
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

    // ── LoudnessAnalysis math tests ─────────────────────────────────

    #[test]
    fn loudness_normal_case_no_peak_limiting() {
        // Quiet mix at -20 LUFS, peak at 0.5 (-6 dBFS). Target -14.
        // Requested gain: +6 dB. Peak after gain: 0.5 * 2.0 = 1.0 > 0.891? Yes!
        // Actually that's peak-limited. Use lower peak instead.
        let a = LoudnessAnalysis::compute(-14.0, -20.0, 0.3);
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
        let a = LoudnessAnalysis::compute(-14.0, -20.0, 0.9);
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
        let mut chain = DeliveryChain::normalising(sr, -14.0, 6.0, true);
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
        assert!(peak <= db_to_linear(LIMITER_CEILING_DB) + 1e-4, "peak {peak}");
        assert!(peak > 0.5, "the tone is still there: {peak}");
        let report = chain.report().unwrap();
        assert!(report.contains("true peak") && report.contains("limited"), "{report}");

        // Gain that fits needs no limiter and passes the samples as they are.
        let mut plain = DeliveryChain::normalising(sr, -14.0, 0.0, false);
        let mut audio = vec![0.25f32; 400];
        plain.process(&mut audio);
        assert!(audio.iter().all(|s| (*s - 0.25).abs() < 1e-6));
        assert!(DeliveryChain::unity().report().is_none());
    }

    #[test]
    fn loudness_mix_already_too_loud_negative_gain() {
        // Mix at -10 LUFS, peak at 0.8. Target -14.
        // Requested gain: -4 dB (reduce volume). No peak concern.
        let a = LoudnessAnalysis::compute(-14.0, -10.0, 0.8);
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
        let peak = PEAK_CEILING_LINEAR / gain_linear; // ~0.4455
        let a = LoudnessAnalysis::compute(-14.0, -20.0, peak);
        assert!(!a.peak_limited); // Exactly at ceiling, not over
        assert!((a.achieved_lufs - (-14.0)).abs() < 0.01);
    }

    #[test]
    fn loudness_zero_gain_when_already_at_target() {
        let a = LoudnessAnalysis::compute(-14.0, -14.0, 0.5);
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
