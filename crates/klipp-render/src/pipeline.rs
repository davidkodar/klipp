use crate::stage::{ClipContext, FrameContext, ProcessingStage};
use crate::stages::{BuiltinEffectsStage, ColorGradeStage, OFXEffectsStage};
use klipp_core::ExportSettings;
use std::path::PathBuf;

/// Central coordinator that runs frames through an ordered chain of processing stages.
///
/// Default chain (per Gap 3 decision — 3 stages, not 5):
///   1. ColorGradeStage — full color pipeline + film grain + global LUT
///   2. OFXEffectsStage — OFX plugin effects
///   3. BuiltinEffectsStage — built-in GPU/CPU effects (blur, crop, etc.)
pub struct RenderPipeline {
    stages: Vec<Box<dyn ProcessingStage>>,
}

impl RenderPipeline {
    /// Build the default stage chain.
    /// To add a new stage to Klipp: add it here. Export automatically includes it.
    pub fn default_stages(global_lut_path: Option<PathBuf>) -> Self {
        Self {
            stages: vec![
                Box::new(ColorGradeStage::new(global_lut_path)),
                Box::new(OFXEffectsStage::new()),
                Box::new(BuiltinEffectsStage::new()),
            ],
        }
    }

    /// Build a pipeline with custom stages (for testing).
    pub fn with_stages(stages: Vec<Box<dyn ProcessingStage>>) -> Self {
        Self { stages }
    }

    /// Notify all stages that the export is beginning.
    pub fn begin_export(&mut self, settings: &ExportSettings) {
        for stage in &mut self.stages {
            stage.begin_export(settings);
        }
    }

    /// Notify all stages that a new clip is beginning.
    pub fn begin_clip(&mut self, ctx: &ClipContext) {
        for stage in &mut self.stages {
            stage.begin_clip(ctx);
        }
    }

    /// Notify all stages that the current clip has ended.
    pub fn end_clip(&mut self) {
        for stage in &mut self.stages {
            stage.end_clip();
        }
    }

    /// Process one frame through the full stage chain.
    pub fn process_frame(
        &mut self,
        frame: Vec<u8>,
        width: u32,
        height: u32,
        ctx: &FrameContext,
    ) -> Vec<u8> {
        let mut result = frame;
        for stage in &mut self.stages {
            result = stage.process(result, width, height, ctx);
        }
        result
    }

    /// Notify all stages that the export has completed or been cancelled.
    pub fn end_export(&mut self) {
        for stage in &mut self.stages {
            stage.end_export();
        }
    }

    /// Number of stages in the pipeline.
    pub fn stage_count(&self) -> usize {
        self.stages.len()
    }

    /// Names of all stages (for debug logging).
    pub fn stage_names(&self) -> Vec<&'static str> {
        self.stages.iter().map(|s| s.name()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A test stage that appends a byte marker to track processing order.
    struct MarkerStage {
        name: &'static str,
        marker: u8,
    }

    impl ProcessingStage for MarkerStage {
        fn name(&self) -> &'static str {
            self.name
        }

        fn process(
            &mut self,
            mut frame: Vec<u8>,
            _width: u32,
            _height: u32,
            _ctx: &FrameContext,
        ) -> Vec<u8> {
            frame.push(self.marker);
            frame
        }
    }

    fn make_frame_context() -> FrameContext {
        FrameContext {
            export_frame: 0,
            clip_frame: 0,
            clip_time_secs: 0.0,
            clip: None,
            fps: 24.0,
            track_index: 0,
        }
    }

    #[test]
    fn pipeline_runs_stages_in_order() {
        let mut pipeline = RenderPipeline::with_stages(vec![
            Box::new(MarkerStage {
                name: "A",
                marker: 0xAA,
            }),
            Box::new(MarkerStage {
                name: "B",
                marker: 0xBB,
            }),
            Box::new(MarkerStage {
                name: "C",
                marker: 0xCC,
            }),
        ]);

        let ctx = make_frame_context();
        let result = pipeline.process_frame(vec![], 1, 1, &ctx);

        assert_eq!(result, vec![0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn pipeline_empty_stages_passthrough() {
        let mut pipeline = RenderPipeline::with_stages(vec![]);
        let ctx = make_frame_context();
        let input = vec![1, 2, 3, 4];
        let result = pipeline.process_frame(input.clone(), 1, 1, &ctx);
        assert_eq!(result, input);
    }

    #[test]
    fn default_pipeline_has_three_stages() {
        let pipeline = RenderPipeline::default_stages(None);
        assert_eq!(pipeline.stage_count(), 3);
        assert_eq!(
            pipeline.stage_names(),
            vec!["Color Grade", "OFX Effects", "Built-in Effects"]
        );
    }

    #[test]
    fn identity_grade_passthrough() {
        // Default (disabled) color grade should not modify the frame
        let mut pipeline = RenderPipeline::default_stages(None);
        let settings = ExportSettings {
            output_path: std::path::PathBuf::from("/tmp/test.mp4"),
            format: klipp_core::ExportFormat::Mp4,
            video_codec: klipp_core::ExportVideoCodec::H264,
            resolution: None,
            fps: None,
            crf: 18,
            preset: klipp_core::EncoderPreset::Medium,
            audio_codec: klipp_core::ExportAudioCodec::Aac,
            audio_bitrate_kbps: Some(192), video_bitrate_kbps: None, rate_control: klipp_core::RateControl::Crf,
            burn_in_subtitles: true, target_lufs: None };
        pipeline.begin_export(&settings);

        let ctx = make_frame_context();
        // 2x2 red RGBA frame
        let input: Vec<u8> = vec![255, 0, 0, 255].repeat(4);
        let result = pipeline.process_frame(input.clone(), 2, 2, &ctx);
        // No clip context → no grade → no effects → passthrough
        assert_eq!(result, input);
    }
}
