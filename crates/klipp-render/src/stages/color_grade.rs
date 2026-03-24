use crate::snapshot::ClipColorGradeSnapshot;
use crate::stage::{ClipContext, FrameContext, ProcessingStage};
use klipp_color::pipeline::{ColorGradeParams, CurveLuts};
use klipp_color::ColorPipeline;
use klipp_core::{ColorGradeParam as P, ExportSettings, FilmGrainSettings};
use klipp_effects::gpu::GpuEffectsPipeline;
use std::path::PathBuf;

/// Processing stage that applies per-clip color grading, film grain, and the global LUT.
///
/// Per Gap 3 decision: film grain and global LUT stay inside this stage (not separate),
/// because they're logically part of the per-clip grade / global pipeline.
pub struct ColorGradeStage {
    pipeline: ColorPipeline,
    gpu: Option<GpuEffectsPipeline>,
    /// Cached grade params — rebuilt in `begin_clip`, reused per frame.
    cached_params: Option<ColorGradeParams>,
    /// Cached grain settings for the current clip.
    cached_grain: FilmGrainSettings,
    /// Export-wide frame counter for grain seed.
    export_frame_counter: u32,
}

impl ColorGradeStage {
    pub fn new(global_lut_path: Option<PathBuf>) -> Self {
        let mut pipeline = ColorPipeline::new();

        // Load global LUT if provided
        if let Some(ref lut_path) = global_lut_path {
            if let Err(e) = pipeline.load_global_lut(lut_path) {
                tracing::warn!("ColorGradeStage: failed to load global LUT: {e}");
            }
        }

        // Create GPU pipeline for export thread (separate device from playback)
        let gpu = GpuEffectsPipeline::new();
        if gpu.is_none() {
            tracing::info!("ColorGradeStage: GPU unavailable, using CPU fallback");
        }

        Self {
            pipeline,
            gpu,
            cached_params: None,
            cached_grain: FilmGrainSettings::default(),
            export_frame_counter: 0,
        }
    }

    /// Build `ColorGradeParams` from snapshot data, pre-computing curve LUTs.
    fn build_params(grade: &ClipColorGradeSnapshot) -> ColorGradeParams {
        // Build curve LUTs if any curve is non-identity
        let curve_luts = {
            let c = &grade.curves;
            let m_id = klipp_color::curves::is_identity(&c.master);
            let r_id = klipp_color::curves::is_identity(&c.red);
            let g_id = klipp_color::curves::is_identity(&c.green);
            let b_id = klipp_color::curves::is_identity(&c.blue);
            if m_id && r_id && g_id && b_id {
                None
            } else {
                Some(CurveLuts {
                    master: klipp_color::curves::build_curve_lut(&c.master),
                    red: klipp_color::curves::build_curve_lut(&c.red),
                    green: klipp_color::curves::build_curve_lut(&c.green),
                    blue: klipp_color::curves::build_curve_lut(&c.blue),
                })
            }
        };

        ColorGradeParams {
            exposure: grade.exposure,
            contrast: grade.contrast,
            saturation: grade.saturation,
            temperature: grade.temperature,
            tint: grade.tint,
            highlights: grade.highlights,
            shadows: grade.shadows,
            whites: grade.whites,
            blacks: grade.blacks,
            lift: klipp_color::wheel_offset_to_rgb(grade.lift_x, grade.lift_y, grade.lift_master),
            gamma: klipp_color::wheel_offset_to_rgb(grade.gamma_x, grade.gamma_y, grade.gamma_master),
            gain: klipp_color::wheel_offset_to_rgb(grade.gain_x, grade.gain_y, grade.gain_master),
            offset: klipp_color::wheel_offset_to_rgb(
                grade.offset_x,
                grade.offset_y,
                grade.offset_master,
            ),
            lift_sat: grade.lift_sat,
            lift_bri: grade.lift_bri,
            gamma_sat: grade.gamma_sat,
            gamma_bri: grade.gamma_bri,
            gain_sat: grade.gain_sat,
            gain_bri: grade.gain_bri,
            offset_sat: grade.offset_sat,
            offset_bri: grade.offset_bri,
            hue: grade.hue,
            global_mix: grade.global_mix,
            curve_luts,
            curves_preserve_luma: grade.curves_preserve_luma,
            curves_mix: grade.curves_mix,
            huesat: grade.huesat.clone(),
            huesat_mix: grade.huesat_mix,
            qualifier: grade.qualifier.clone(),
            masks: grade.masks.clone(),
            input_log_format: grade.log_format,
        }
    }

    /// Apply keyframe overrides to cached params at the given clip time.
    fn apply_keyframes(params: &mut ColorGradeParams, grade: &ClipColorGradeSnapshot, clip_time: f64) {
        if grade.keyframe_tracks.is_empty() {
            return;
        }

        for (kf_param, kf_value) in grade.keyframe_tracks.evaluate_all(clip_time) {
            match kf_param {
                P::Exposure => params.exposure = kf_value,
                P::Contrast => params.contrast = kf_value,
                P::Saturation => params.saturation = kf_value,
                P::Temperature => params.temperature = kf_value,
                P::Tint => params.tint = kf_value,
                P::Highlights => params.highlights = kf_value,
                P::Shadows => params.shadows = kf_value,
                P::Whites => params.whites = kf_value,
                P::Blacks => params.blacks = kf_value,
                P::Hue => params.hue = kf_value,
                P::GlobalMix => params.global_mix = kf_value,
                P::CurvesMix => params.curves_mix = kf_value,
                P::CurvesPreserveLuma => params.curves_preserve_luma = kf_value > 0.5,
                P::HueSatMix => params.huesat_mix = kf_value,
                // Wheel params (LiftX etc.) would need re-conversion through
                // wheel_offset_to_rgb — skip for now, matching playback engine.
                _ => {}
            }
        }
    }
}

impl ProcessingStage for ColorGradeStage {
    fn name(&self) -> &'static str {
        "Color Grade"
    }

    fn begin_export(&mut self, _settings: &ExportSettings) {
        self.export_frame_counter = 0;
    }

    fn begin_clip(&mut self, ctx: &ClipContext) {
        let grade = &ctx.clip.color_grade;
        if grade.enabled {
            self.cached_params = Some(Self::build_params(grade));
        } else {
            self.cached_params = None;
        }
        self.cached_grain = grade.grain.clone();
    }

    fn end_clip(&mut self) {
        self.cached_params = None;
        self.cached_grain = FilmGrainSettings::default();
    }

    fn process(
        &mut self,
        frame: Vec<u8>,
        width: u32,
        height: u32,
        ctx: &FrameContext,
    ) -> Vec<u8> {
        let mut rgba = frame;

        // 1. Apply per-clip color grade
        if let Some(ref base_params) = self.cached_params {
            let mut params = base_params.clone();

            // Evaluate keyframes at exact clip time
            if let Some(ref clip) = ctx.clip {
                Self::apply_keyframes(&mut params, &clip.color_grade, ctx.clip_time_secs);
            }

            // Apply full grade pipeline (CPU path — includes all 12 stages + qualifier/masks)
            // matte_preview is always false during export
            rgba = self.pipeline.apply_clip_grade_full(rgba, &params, width, height, false);
        }

        // 2. Film grain
        if self.cached_grain.enabled && self.cached_grain.amount > 0.001 {
            let seed = self.export_frame_counter;
            if let Some(ref mut gpu) = self.gpu {
                rgba = gpu.apply_grain_gpu(
                    rgba,
                    width,
                    height,
                    self.cached_grain.amount,
                    self.cached_grain.size,
                    self.cached_grain.shadows,
                    self.cached_grain.highlights,
                    self.cached_grain.monochrome,
                    seed,
                );
            } else {
                // CPU fallback: simple Wang hash grain
                apply_grain_cpu(&mut rgba, &self.cached_grain, seed);
            }
        }

        // 3. Global LUT (applied via pipeline's internal LUT data)
        if let Some(ref lut_data) = self.pipeline.global_lut {
            let opacity = self.pipeline.global_lut_opacity;
            klipp_color::apply_3d_lut(&mut rgba, lut_data, opacity);
        }

        self.export_frame_counter += 1;
        rgba
    }

    fn end_export(&mut self) {}
}

/// CPU fallback for film grain (simple Wang hash noise).
fn apply_grain_cpu(rgba: &mut [u8], grain: &FilmGrainSettings, seed: u32) {
    let amount = grain.amount;
    for (i, chunk) in rgba.chunks_exact_mut(4).enumerate() {
        let hash = wang_hash(seed.wrapping_mul(65521).wrapping_add(i as u32));
        let noise = ((hash & 0xFF) as f32 / 255.0 - 0.5) * amount * 128.0;
        if grain.monochrome {
            for c in &mut chunk[..3] {
                *c = (*c as f32 + noise).clamp(0.0, 255.0) as u8;
            }
        } else {
            for (j, c) in chunk[..3].iter_mut().enumerate() {
                let h = wang_hash(hash.wrapping_add(j as u32));
                let n = ((h & 0xFF) as f32 / 255.0 - 0.5) * amount * 128.0;
                *c = (*c as f32 + n).clamp(0.0, 255.0) as u8;
            }
        }
    }
}

fn wang_hash(mut key: u32) -> u32 {
    key = (key ^ 61) ^ (key >> 16);
    key = key.wrapping_mul(9);
    key ^= key >> 4;
    key = key.wrapping_mul(0x27d4eb2d);
    key ^= key >> 15;
    key
}
