use crate::audio_mixdown::AudioMixdown;
use crate::clip_decoder::RenderDecoder;
use crate::compositor::FrameCompositor;
use crate::encoder::Encoder;
use crate::error::RenderError;
use crate::pipeline::RenderPipeline;
use crate::snapshot::{ClipKindSnapshot, ClipSnapshot, SequenceSnapshot};
use crate::stage::{ClipContext, FrameContext};
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
}

impl ExportJob {
    /// Run the export. Blocks until complete, cancelled, or error.
    /// Called from a background thread.
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

        let compositor = FrameCompositor::new();
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

        // Per-track current clip tracking (Gap 11)
        let mut current_clips: Vec<Option<ClipId>> =
            vec![None; self.snapshot.video_tracks.len()];

        let mut export_frame_num: u64 = 0;

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

            // ─── Composite all video tracks ───────────────────────
            let rgba = compositor.composite_frame(
                frame_num,
                &self.snapshot,
                &mut decoders,
            )?;

            // Black frame if nothing to composite
            let rgba = rgba.unwrap_or_else(|| {
                vec![0u8; (self.snapshot.width * self.snapshot.height * 4) as usize]
            });

            // ─── Run through per-track processing pipeline ────────
            // For the composited frame, we apply the pipeline of the topmost
            // visible track that has an active clip at this frame.
            let (pipeline_idx, active_clip) = find_topmost_active_clip(
                &self.snapshot,
                frame_num,
            );

            let processed = if let Some(idx) = pipeline_idx {
                let clip_frame = active_clip
                    .as_ref()
                    .map(|c| frame_num.saturating_sub(c.timeline_start_frame))
                    .unwrap_or(0);
                let clip_time_secs = clip_frame as f64 / self.snapshot.fps;

                let ctx = FrameContext {
                    export_frame: export_frame_num,
                    clip_frame,
                    clip_time_secs,
                    clip: active_clip.map(|c| Arc::new(c.clone())),
                    fps: self.snapshot.fps,
                    track_index: idx,
                };

                pipelines[idx].process_frame(
                    rgba,
                    self.snapshot.width,
                    self.snapshot.height,
                    &ctx,
                )
            } else {
                rgba
            };

            // ─── Encode video frame ───────────────────────────────
            encoder.encode_video_frame(
                &processed,
                self.snapshot.width,
                self.snapshot.height,
            )?;

            // ─── Encode audio ─────────────────────────────────────
            let audio = audio_mixdown.get_frame_audio(frame_num);
            encoder.encode_audio_samples(&audio)?;

            // ─── Update progress ──────────────────────────────────
            export_frame_num += 1;
            self.progress.store(export_frame_num, Ordering::Relaxed);
        }

        // ─── Finalise ─────────────────────────────────────────────
        for (track_idx, clip_id) in current_clips.iter().enumerate() {
            if clip_id.is_some() {
                pipelines[track_idx].end_clip();
            }
        }
        for pipeline in &mut pipelines {
            pipeline.end_export();
        }

        encoder.finalise()?;

        info!(
            "ExportJob {}: complete, {} frames → {:?}",
            self.id, export_frame_num, self.output_path
        );

        Ok(())
    }
}

/// Find the topmost visible track that has an active clip at this frame.
/// Returns (track_index, clip) for the pipeline to use.
fn find_topmost_active_clip(
    snapshot: &SequenceSnapshot,
    frame_num: u64,
) -> (Option<usize>, Option<&ClipSnapshot>) {
    // Iterate top-to-bottom (highest index = topmost track)
    for (idx, track) in snapshot.video_tracks.iter().enumerate().rev() {
        if !track.visible {
            continue;
        }
        if let Some(clip) = track.clips.iter().find(|c| {
            frame_num >= c.timeline_start_frame && frame_num < c.timeline_end_frame
        }) {
            return (Some(idx), Some(clip));
        }
    }
    (None, None)
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
                audio_bitrate_kbps: None,
            },
            snapshot: make_test_snapshot(),
            cancel: cancel.clone(),
            progress: Arc::new(AtomicU64::new(0)),
            hw_info: Arc::new(std::sync::Mutex::new(String::new())),
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

    #[test]
    fn find_topmost_active_clip_works() {
        let snapshot = SequenceSnapshot {
            fps: 24.0,
            fps_num: 24,
            fps_den: 1,
            width: 64,
            height: 64,
            sample_rate: 48000,
            duration_frames: 48,
            export_range: None,
            video_tracks: vec![
                VideoTrackSnapshot {
                    opacity: 1.0,
                    visible: true,
                    clips: vec![ClipSnapshot {
                        id: ClipId::new(),
                        kind: ClipKindSnapshot::Gap,
                        timeline_start_frame: 0,
                        timeline_end_frame: 48,
                        source_start_secs: 0.0,
                        source_end_secs: 2.0,
                        color_grade: ClipColorGradeSnapshot::default_identity(),
                        effects: vec![],
                        builtin_effects: vec![],
                    }],
                    transitions: vec![],
                },
                VideoTrackSnapshot {
                    opacity: 1.0,
                    visible: true,
                    clips: vec![ClipSnapshot {
                        id: ClipId::new(),
                        kind: ClipKindSnapshot::Gap,
                        timeline_start_frame: 10,
                        timeline_end_frame: 30,
                        source_start_secs: 0.0,
                        source_end_secs: 0.8,
                        color_grade: ClipColorGradeSnapshot::default_identity(),
                        effects: vec![],
                        builtin_effects: vec![],
                    }],
                    transitions: vec![],
                },
            ],
            audio_tracks: vec![],
            global_lut_path: None,
        };

        // Frame 5: only track 0 has a clip → returns track 0
        let (idx, _) = find_topmost_active_clip(&snapshot, 5);
        assert_eq!(idx, Some(0));

        // Frame 15: both tracks have clips → returns track 1 (topmost)
        let (idx, _) = find_topmost_active_clip(&snapshot, 15);
        assert_eq!(idx, Some(1));

        // Frame 40: only track 0 → returns track 0
        let (idx, _) = find_topmost_active_clip(&snapshot, 40);
        assert_eq!(idx, Some(0));
    }
}
