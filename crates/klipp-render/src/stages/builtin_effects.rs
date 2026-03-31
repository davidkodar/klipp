use crate::clip_decoder::decode_frame_at_size;
use crate::snapshot::{ClipKindSnapshot, ClipSnapshot};
use crate::stage::{ClipContext, FrameContext, ProcessingStage};
use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_effects::rolling_shutter::{self, Homography};
use klipp_effects::BuiltinEffect;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// RSC motion estimation cache: keyed by (clip_time_ms) -> (h_prev, h_next).
struct RscMotionCache {
    entries: HashMap<i64, (Homography, Homography)>,
}

impl RscMotionCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    fn get(&self, time_ms: i64) -> Option<&(Homography, Homography)> {
        self.entries.get(&time_ms)
    }

    fn insert(&mut self, time_ms: i64, h_prev: Homography, h_next: Homography) {
        self.entries.insert(time_ms, (h_prev, h_next));
    }

    fn clear(&mut self) {
        self.entries.clear();
    }
}

/// Processing stage that applies built-in effects (blur, crop, transform, etc.).
/// Uses GPU (wgpu compute shaders) when available, CPU fallback otherwise.
///
/// Also handles temporal effects that require frame context:
/// - Rolling Shutter Correction: decodes adjacent frames for motion estimation
/// - Video Stabilization: looks up pre-computed correction homographies
pub struct BuiltinEffectsStage {
    gpu: Option<GpuEffectsPipeline>,
    /// Source media path for the current clip (needed for RSC adjacent frame decode).
    current_source_path: Option<PathBuf>,
    /// Current clip snapshot (for stabilization data access).
    current_clip: Option<Arc<ClipSnapshot>>,
    /// Sequence fps (for frame duration calculation).
    sequence_fps: f64,
    /// Sequence dimensions (for RSC row transform height).
    sequence_height: u32,
    /// RSC motion estimation cache for the current clip.
    rsc_cache: RscMotionCache,
}

impl BuiltinEffectsStage {
    pub fn new() -> Self {
        let gpu = GpuEffectsPipeline::new();
        if gpu.is_some() {
            tracing::info!("BuiltinEffectsStage: GPU pipeline available");
        }
        Self {
            gpu,
            current_source_path: None,
            current_clip: None,
            sequence_fps: 24.0,
            sequence_height: 1080,
            rsc_cache: RscMotionCache::new(),
        }
    }

    /// Prepare RSC row transforms for the current frame by decoding adjacent frames
    /// at low resolution and running motion estimation.
    fn prepare_rsc(
        &mut self,
        clip_time_secs: f64,
        strength: f32,
        readout_ratio: f32,
        height: u32,
    ) {
        if strength <= f32::EPSILON {
            return;
        }

        let gpu = match self.gpu.as_mut() {
            Some(g) => g,
            None => return,
        };

        let source_path = match &self.current_source_path {
            Some(p) => p.clone(),
            None => return,
        };

        let frame_pts_ms = (clip_time_secs * 1000.0) as i64;

        // Check cache first
        let cached = self.rsc_cache.get(frame_pts_ms).cloned();

        let row_transforms = if let Some((h_prev, h_next)) = cached {
            rolling_shutter::compute_row_transforms(&h_prev, &h_next, height, readout_ratio, strength)
        } else {
            // Decode adjacent frames at 480p for motion estimation
            let frame_dur = if self.sequence_fps > 0.0 {
                1.0 / self.sequence_fps
            } else {
                1.0 / 30.0
            };
            let prev_secs = (clip_time_secs - frame_dur).max(0.0);
            let next_secs = clip_time_secs + frame_dur;

            let prev_frame = decode_frame_at_size(&source_path, prev_secs, 480);
            let curr_frame = decode_frame_at_size(&source_path, clip_time_secs, 480);
            let next_frame = decode_frame_at_size(&source_path, next_secs, 480);

            if let (Some((prev_rgba, pw, ph)), Some((curr_rgba, cw, ch)), Some((next_rgba, nw, nh))) =
                (prev_frame, curr_frame, next_frame)
            {
                if let Some((h_prev, h_next)) = rolling_shutter::estimate_motion(
                    &prev_rgba, pw, ph, &curr_rgba, cw, ch, &next_rgba, nw, nh,
                ) {
                    self.rsc_cache.insert(frame_pts_ms, h_prev, h_next);
                    rolling_shutter::compute_row_transforms(
                        &h_prev, &h_next, height, readout_ratio, strength,
                    )
                } else {
                    rolling_shutter::compute_row_transforms(
                        &Homography::identity(),
                        &Homography::identity(),
                        height,
                        readout_ratio,
                        strength,
                    )
                }
            } else {
                vec![]
            }
        };

        if !row_transforms.is_empty() {
            gpu.upload_rsc_row_transforms(&row_transforms);
        }
    }

    /// Upload stabilization correction homography for the current frame.
    fn prepare_stabilization(&mut self, clip_time_secs: f64) {
        let gpu = match self.gpu.as_mut() {
            Some(g) => g,
            None => return,
        };

        let clip = match &self.current_clip {
            Some(c) => c,
            None => return,
        };

        let smoothed = match &clip.smoothed_trajectory {
            Some(s) => s,
            None => return,
        };

        if smoothed.corrections.is_empty() {
            return;
        }

        // Compute frame index from clip time and source duration
        let source_dur =
            (clip.source_end_secs - clip.source_start_secs).max(0.001);
        let source_fps = smoothed.corrections.len() as f64 / source_dur;
        let frame_idx = (clip_time_secs * source_fps).round() as usize;
        let frame_idx = frame_idx.min(smoothed.corrections.len() - 1);
        let correction = &smoothed.corrections[frame_idx];

        let h_f32: [f32; 12] = [
            correction.h[0][0] as f32,
            correction.h[0][1] as f32,
            correction.h[0][2] as f32,
            correction.h[1][0] as f32,
            correction.h[1][1] as f32,
            correction.h[1][2] as f32,
            correction.h[2][0] as f32,
            correction.h[2][1] as f32,
            correction.h[2][2] as f32,
            0.0,
            0.0,
            0.0,
        ];

        gpu.upload_stabilization_homography(&h_f32);
    }
}

impl Default for BuiltinEffectsStage {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessingStage for BuiltinEffectsStage {
    fn name(&self) -> &'static str {
        "Built-in Effects"
    }

    fn begin_clip(&mut self, ctx: &ClipContext) {
        self.current_clip = Some(ctx.clip.clone());
        self.sequence_fps = ctx.sequence_fps;
        self.sequence_height = ctx.sequence_height;
        self.rsc_cache.clear();

        // Extract source path for RSC adjacent frame decoding
        self.current_source_path = match &ctx.clip.kind {
            ClipKindSnapshot::Media { path, .. } => Some(path.clone()),
            _ => None,
        };
    }

    fn end_clip(&mut self) {
        self.current_clip = None;
        self.current_source_path = None;
        self.rsc_cache.clear();
    }

    fn process(
        &mut self,
        frame: Vec<u8>,
        width: u32,
        height: u32,
        ctx: &FrameContext,
    ) -> Vec<u8> {
        let clip = match &ctx.clip {
            Some(c) => c,
            None => return frame,
        };

        if clip.builtin_effects.is_empty() {
            return frame;
        }

        // Prepare temporal effects before GPU dispatch

        // Rolling Shutter Correction: decode adjacent frames, run motion estimation
        for effect in &clip.builtin_effects {
            if let BuiltinEffect::RollingShutterCorrection {
                strength,
                readout_ratio,
            } = effect
            {
                self.prepare_rsc(ctx.clip_time_secs, *strength, *readout_ratio, height);
                break;
            }
        }

        // Video Stabilization: look up pre-computed correction homography
        if clip.builtin_effects.iter().any(|e| {
            matches!(e, BuiltinEffect::VideoStabilization { .. })
        }) {
            self.prepare_stabilization(ctx.clip_time_secs);
        }

        // Prefer GPU path
        if let Some(ref mut gpu) = self.gpu {
            return gpu.apply_effects(&clip.builtin_effects, frame, width, height);
        }

        // CPU fallback
        klipp_effects::apply_effects(&clip.builtin_effects, frame, width, height)
    }
}
