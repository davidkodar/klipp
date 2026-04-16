use crate::audio_mixdown::AudioMixdown;
use crate::clip_decoder::RenderDecoder;
use crate::compositor::FrameCompositor;
use crate::encoder::Encoder;
use crate::error::RenderError;
use crate::pipeline::RenderPipeline;
use crate::snapshot::{ClipKindSnapshot, SequenceSnapshot, SubtitleClipSnapshot};
use crate::stage::ClipContext;
use klipp_core::{ClipId, ExportSettings};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tracing::info;
use uuid::Uuid;

/// Top-level export coordinator. Ties together all render pipeline components.
///
/// Gap 11 fix: one pipeline per video track (each track has its own begin_clip/end_clip
/// state so per-clip color grades don't bleed across tracks).
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
        info!(
            "ExportJob {}: {} video tracks, {} audio tracks, duration_frames={}",
            self.id,
            self.snapshot.video_tracks.len(),
            self.snapshot.audio_tracks.len(),
            self.snapshot.duration_frames,
        );
        for (ti, track) in self.snapshot.video_tracks.iter().enumerate() {
            info!(
                "  V{}: {} clips, visible={}, opacity={}",
                ti, track.clips.len(), track.visible, track.opacity,
            );
            for (ci, clip) in track.clips.iter().enumerate() {
                let kind = match &clip.kind {
                    ClipKindSnapshot::Media { path, .. } => format!("Media({})", path.file_name().unwrap_or_default().to_string_lossy()),
                    ClipKindSnapshot::Title(_) => "Title".to_string(),
                    ClipKindSnapshot::Generator(_) => "Generator".to_string(),
                    ClipKindSnapshot::AdjustmentLayer => "AdjustmentLayer".to_string(),
                    ClipKindSnapshot::Gap => "Gap".to_string(),
                };
                info!(
                    "    clip[{}]: {} frames [{}-{}), src [{:.3}s-{:.3}s], kind={}",
                    ci, clip.timeline_end_frame - clip.timeline_start_frame,
                    clip.timeline_start_frame, clip.timeline_end_frame,
                    clip.source_start_secs, clip.source_end_secs,
                    kind,
                );
            }
        }

        Self::set_stage(&self.render_stage, "Initializing\u{2026}");

        // ─── Initialise all systems ───────────────────────────────
        // Gap 11: one pipeline per video track
        let mut pipelines: Vec<RenderPipeline> = self
            .snapshot
            .video_tracks
            .iter()
            .map(|_| RenderPipeline::default_stages(self.snapshot.global_lut_path.clone()))
            .collect();

        for pipeline in &mut pipelines {
            pipeline.begin_export(&self.settings);
        }

        let mut compositor = FrameCompositor::new();
        let mut audio_mixdown = AudioMixdown::new(&self.snapshot)?;
        let mut encoder =
            Encoder::open(&self.output_path, &self.settings, &self.snapshot)?;

        // Pre-open all clip decoders (seek to their start positions)
        let mut decoders: HashMap<ClipId, RenderDecoder> = HashMap::new();
        for track in &self.snapshot.video_tracks {
            for clip in &track.clips {
                if let ClipKindSnapshot::Media { ref path, .. } = clip.kind {
                    let decoder = RenderDecoder::open(
                        path,
                        clip.source_start_secs,
                        clip.source_end_secs,
                        self.snapshot.width,
                        self.snapshot.height,
                        self.snapshot.fps,
                        clip.source_rotation,
                    )?;
                    decoders.insert(clip.id, decoder);
                }
            }
        }

        // Report hardware info
        {
            let decode_hw = decoders
                .values()
                .next()
                .map(|d| d.hw_accel_name())
                .unwrap_or("N/A");
            let encode_hw = encoder.hw_info();
            let hw_str = format!("Decode: {} | Encode: {}", decode_hw, encode_hw);
            info!("ExportJob {}: {}", self.id, hw_str);
            if let Ok(mut hw) = self.hw_info.lock() {
                *hw = hw_str;
            }
        }

        // Determine frame range
        let start_frame = self.snapshot.start_frame();
        let end_frame = self.snapshot.end_frame();

        // ─── Pass 1: Loudness measurement (if normalization enabled) ──
        let loudness_gain_linear = if let Some(target_lufs) = self.settings.target_lufs {
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
                let audio = measure_mixdown.get_frame_audio(frame_num);
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
                    "ExportJob {}: peak fallback engaged — requested {:.1} LUFS but achieved {:.1} LUFS \
                     to stay within -1 dBFS ceiling",
                    self.id, target_lufs, analysis.achieved_lufs,
                );
            }
            // Store result string for the render queue details panel
            let result_str = if analysis.peak_limited {
                format!("Loudness: {:.1} LUFS (requested {:.1}, peak limited)", analysis.achieved_lufs, target_lufs)
            } else {
                format!("Loudness: {:.1} LUFS", analysis.achieved_lufs)
            };
            if let Ok(mut r) = self.loudness_result.lock() { *r = Some(result_str); }
            db_to_linear(analysis.applied_gain_db)
        } else {
            1.0 // No normalization — unity gain
        };

        // Per-track current clip tracking (Gap 11)
        let mut current_clips: Vec<Option<ClipId>> =
            vec![None; self.snapshot.video_tracks.len()];

        let mut export_frame_num: u64 = 0;

        // Determine if aspect-ratio-preserving fit-and-pad is needed.
        // When the export resolution differs from the sequence AND aspect ratios
        // don't match, we letterbox/pillarbox each frame before encoding.
        let needs_fit = match &self.settings.resolution {
            Some(res) => {
                let (tw, th) = (res.width & !1, res.height & !1);
                tw != self.snapshot.width || th != self.snapshot.height
            }
            None => false,
        };
        let fit_target = self.settings.resolution.as_ref().map(|r| (r.width & !1, r.height & !1));

        let codec_label = self.settings.video_codec.label();
        Self::set_stage(&self.render_stage, &format!("Encoding video ({codec_label})\u{2026}"));

        // ─── THE MAIN RENDER LOOP ─────────────────────────────────
        for frame_num in start_frame..end_frame {
            // Check cancellation
            if self.cancel.load(Ordering::Relaxed) {
                // Clean up partial file
                drop(encoder);
                let _ = std::fs::remove_file(&self.output_path);
                info!("ExportJob {}: cancelled at frame {}", self.id, frame_num);
                return Err(RenderError::Cancelled);
            }

            // ─── Per-track clip boundary tracking (Gap 11) ────────
            for (track_idx, track) in self.snapshot.video_tracks.iter().enumerate() {
                let active_clip = track.clips.iter().find(|c| {
                    frame_num >= c.timeline_start_frame && frame_num < c.timeline_end_frame
                });

                let active_id = active_clip.map(|c| c.id);

                if active_id != current_clips[track_idx] {
                    // Clip boundary change on this track
                    if current_clips[track_idx].is_some() {
                        pipelines[track_idx].end_clip();
                    }
                    if let Some(clip) = active_clip {
                        pipelines[track_idx].begin_clip(&ClipContext {
                            clip: Arc::new(clip.clone()),
                            sequence_fps: self.snapshot.fps,
                            sequence_width: self.snapshot.width,
                            sequence_height: self.snapshot.height,
                        });
                    }
                    current_clips[track_idx] = active_id;
                }
            }

            // ─── Composite all video tracks with per-clip effects ──
            // Each clip's effects (color grade, OFX, builtins) are applied to its
            // decoded frame before compositing, so every clip renders with its own
            // effects. This replaces the previous "topmost clip only" approach.
            let rgba = compositor.composite_frame(
                frame_num,
                export_frame_num,
                &self.snapshot,
                &mut decoders,
                &mut pipelines,
            )?;

            // Black frame if nothing to composite
            let processed = rgba.unwrap_or_else(|| {
                vec![0u8; (self.snapshot.width * self.snapshot.height * 4) as usize]
            });

            // ─── Encode video frame (with aspect-preserving fit if needed) ──
            if needs_fit {
                let (tw, th) = fit_target.unwrap();
                let padded = fit_and_pad_rgba(&processed, self.snapshot.width, self.snapshot.height, tw, th)?;
                encoder.encode_video_frame(&padded, tw, th)?;
            } else {
                encoder.encode_video_frame(&processed, self.snapshot.width, self.snapshot.height)?;
            }

            // ─── Encode audio (with loudness normalization gain if enabled) ──
            let mut audio = audio_mixdown.get_frame_audio(frame_num);
            if loudness_gain_linear != 1.0 {
                for s in &mut audio { *s *= loudness_gain_linear; }
            }
            encoder.encode_audio_samples(&audio)?;

            // ─── Update progress ──────────────────────────────────
            export_frame_num += 1;
            self.progress.store(export_frame_num, Ordering::Relaxed);
        }

        Self::set_stage(&self.render_stage, "Finalizing file\u{2026}");

        // ─── Finalise ─────────────────────────────────────────────
        for (track_idx, clip_id) in current_clips.iter().enumerate() {
            if clip_id.is_some() {
                pipelines[track_idx].end_clip();
            }
        }
        for pipeline in &mut pipelines {
            pipeline.end_export();
        }

        // ─── Subtitle embedding ───────────────────────────────────
        if let Some(sub_stream_idx) = encoder.subtitle_stream_idx {
            Self::encode_subtitles(
                &mut encoder,
                sub_stream_idx,
                &self.snapshot,
                &self.settings,
            )?;
        }

        encoder.finalise()?;

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

/// Compute fitted dimensions and centering offset to fit `(src_w, src_h)`
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

/// Take an RGBA buffer at (src_w, src_h) and return a new RGBA buffer at
/// (target_w, target_h) with the source content fitted in the center
/// preserving aspect ratio, padded with opaque black on the unused area.
/// Uses ffmpeg swscale (Lanczos) for the resize step.
fn fit_and_pad_rgba(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    target_w: u32,
    target_h: u32,
) -> Result<Vec<u8>, RenderError> {
    let (fitted_w, fitted_h, offset_x, offset_y) = fit_to_target(src_w, src_h, target_w, target_h);

    // If fitted equals target, just resize directly (no padding needed)
    if fitted_w == target_w && fitted_h == target_h {
        let mut scaler = ffmpeg_next::software::scaling::Context::get(
            ffmpeg_next::format::Pixel::RGBA, src_w, src_h,
            ffmpeg_next::format::Pixel::RGBA, target_w, target_h,
            ffmpeg_next::software::scaling::Flags::LANCZOS,
        ).map_err(|e| RenderError::Encode(format!("fit scaler: {e}")))?;
        let mut src_frame = ffmpeg_next::frame::Video::new(ffmpeg_next::format::Pixel::RGBA, src_w, src_h);
        let stride = src_frame.stride(0);
        let row_bytes = src_w as usize * 4;
        for (i, chunk) in src.chunks_exact(row_bytes).enumerate() {
            src_frame.data_mut(0)[i * stride..i * stride + row_bytes].copy_from_slice(chunk);
        }
        let mut dst_frame = ffmpeg_next::frame::Video::new(ffmpeg_next::format::Pixel::RGBA, target_w, target_h);
        scaler.run(&src_frame, &mut dst_frame).map_err(|e| RenderError::Encode(format!("fit scale: {e}")))?;
        let dst_stride = dst_frame.stride(0);
        let dst_row = target_w as usize * 4;
        let mut out = vec![0u8; target_w as usize * target_h as usize * 4];
        for y in 0..target_h as usize {
            out[y * dst_row..y * dst_row + dst_row].copy_from_slice(&dst_frame.data(0)[y * dst_stride..y * dst_stride + dst_row]);
        }
        return Ok(out);
    }

    // Resize source to fitted dimensions
    let mut scaler = ffmpeg_next::software::scaling::Context::get(
        ffmpeg_next::format::Pixel::RGBA, src_w, src_h,
        ffmpeg_next::format::Pixel::RGBA, fitted_w, fitted_h,
        ffmpeg_next::software::scaling::Flags::LANCZOS,
    ).map_err(|e| RenderError::Encode(format!("fit scaler: {e}")))?;
    let mut src_frame = ffmpeg_next::frame::Video::new(ffmpeg_next::format::Pixel::RGBA, src_w, src_h);
    let stride = src_frame.stride(0);
    let row_bytes = src_w as usize * 4;
    for (i, chunk) in src.chunks_exact(row_bytes).enumerate() {
        src_frame.data_mut(0)[i * stride..i * stride + row_bytes].copy_from_slice(chunk);
    }
    let mut fitted_frame = ffmpeg_next::frame::Video::new(ffmpeg_next::format::Pixel::RGBA, fitted_w, fitted_h);
    scaler.run(&src_frame, &mut fitted_frame).map_err(|e| RenderError::Encode(format!("fit scale: {e}")))?;

    // Create black canvas at target dims (opaque black: R=0 G=0 B=0 A=255)
    let target_pixels = target_w as usize * target_h as usize;
    let mut canvas = vec![0u8; target_pixels * 4];
    // Set alpha to 255 for all pixels
    for pixel in canvas.chunks_exact_mut(4) {
        pixel[3] = 255;
    }

    // Blit fitted content onto canvas at offset
    let fitted_stride = fitted_frame.stride(0);
    let fitted_row_bytes = fitted_w as usize * 4;
    let canvas_row_bytes = target_w as usize * 4;
    for y in 0..fitted_h as usize {
        let src_start = y * fitted_stride;
        let dst_y = y + offset_y as usize;
        let dst_start = dst_y * canvas_row_bytes + offset_x as usize * 4;
        canvas[dst_start..dst_start + fitted_row_bytes]
            .copy_from_slice(&fitted_frame.data(0)[src_start..src_start + fitted_row_bytes]);
    }

    Ok(canvas)
}

// ═══════════════════════════════════════════════════════════════════
// EBU R128 loudness normalization — two-pass measurement + static gain
// ═══════════════════════════════════════════════════════════════════

/// -1 dBFS as a linear amplitude value. Used as the peak ceiling for
/// the honest-fallback approach: if gain-adjusted audio would exceed
/// this level, the gain is reduced so the loudest sample lands here.
const PEAK_CEILING_LINEAR: f32 = 0.891; // 10^(-1.0/20.0) ≈ 0.891

/// Convert a gain in decibels to a linear amplitude multiplier.
/// Shared with klipp-audio's normalize DSP — same formula as
/// klipp-audio/src/dsp/normalize.rs:81.
fn db_to_linear(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

/// Convert a linear amplitude to decibels.
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
            // Peak fallback — reduce gain so loudest sample lands at -1 dBFS
            let safe_gain_linear = PEAK_CEILING_LINEAR / peak_sample;
            let safe_gain_db = linear_to_db(safe_gain_linear);
            Self {
                integrated_lufs,
                peak_sample,
                requested_gain_db,
                applied_gain_db: safe_gain_db,
                achieved_lufs: integrated_lufs + safe_gain_db,
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
            video_tracks: vec![],
            audio_tracks: vec![],
            global_lut_path: None,
            subtitle_tracks: vec![], chapters: vec![],
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
                bit_depth: 0, hdr_mode: klipp_core::HdrMode::Sdr },
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
        // Partial file should be removed
        assert!(!path.exists(), "partial file should be cleaned up");
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
    fn loudness_peak_fallback_engaged() {
        // Mix at -20 LUFS, peak at 0.9 (-0.9 dBFS). Target -14.
        // Requested gain: +6 dB → peak after gain: 0.9 * 2.0 = 1.8 > 0.891 → limited
        let a = LoudnessAnalysis::compute(-14.0, -20.0, 0.9);
        assert!(a.peak_limited);
        assert!((a.requested_gain_db - 6.0).abs() < 0.01);
        // Safe gain: 0.891 / 0.9 ≈ 0.99 → ~-0.08 dB
        assert!(a.applied_gain_db < a.requested_gain_db);
        assert!(a.achieved_lufs < -14.0); // Couldn't reach target — achieved is quieter
        // Peak after applied gain should be at ceiling
        let peak_after = a.peak_sample * db_to_linear(a.applied_gain_db);
        assert!((peak_after - PEAK_CEILING_LINEAR).abs() < 0.001);
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
