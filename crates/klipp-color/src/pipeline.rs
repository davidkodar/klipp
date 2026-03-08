use crate::lut::{self, LutData};
use crate::transforms;
use std::path::{Path, PathBuf};

/// Global color pipeline — post-processing step on RGBA frames.
/// Disabled by default (pure passthrough, zero cost).
pub struct ColorPipeline {
    pub enabled: bool,
    pub global_lut: Option<LutData>,
    pub global_lut_path: Option<PathBuf>,
    pub global_lut_opacity: f32,
}

impl Default for ColorPipeline {
    fn default() -> Self {
        Self {
            enabled: false,
            global_lut: None,
            global_lut_path: None,
            global_lut_opacity: 1.0,
        }
    }
}

impl ColorPipeline {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load a .cube LUT file as the global LUT.
    pub fn load_global_lut(&mut self, path: &Path) -> Result<(), lut::LutError> {
        let lut_data = lut::load_cube(path)?;
        self.global_lut = Some(lut_data);
        self.global_lut_path = Some(path.to_path_buf());
        self.enabled = true;
        Ok(())
    }

    /// Clear the global LUT.
    pub fn clear_global_lut(&mut self) {
        self.global_lut = None;
        self.global_lut_path = None;
    }

    /// Apply per-clip color grade to an RGBA frame.
    /// Pass the individual grade parameters — avoids coupling klipp-color to klipp-timeline.
    pub fn apply_clip_grade(
        &self,
        mut rgba: Vec<u8>,
        exposure: f32,
        contrast: f32,
        saturation: f32,
        temperature: f32,
        tint: f32,
    ) -> Vec<u8> {
        transforms::apply_exposure(&mut rgba, exposure);
        transforms::apply_contrast(&mut rgba, contrast);
        transforms::apply_saturation(&mut rgba, saturation);
        transforms::apply_temperature_tint(&mut rgba, temperature, tint);
        rgba
    }

    /// Apply the global pipeline to an RGBA frame.
    /// Returns the input unchanged if pipeline is disabled.
    pub fn process_frame(&self, mut rgba: Vec<u8>, _width: u32, _height: u32) -> Vec<u8> {
        if !self.enabled {
            return rgba;
        }
        if let Some(lut) = &self.global_lut {
            lut::apply_3d_lut(&mut rgba, lut, self.global_lut_opacity);
        }
        rgba
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_pipeline_is_passthrough() {
        let pipeline = ColorPipeline::new();
        let input = vec![100, 100, 100, 255];
        let output = pipeline.process_frame(input.clone(), 1, 1);
        assert_eq!(input, output);
    }

    #[test]
    fn clip_grade_applies_exposure() {
        let pipeline = ColorPipeline::new();
        let input = vec![100, 100, 100, 255];
        let output = pipeline.apply_clip_grade(input, 1.0, 0.0, 1.0, 0.0, 0.0);
        assert!(output[0] > 100);
    }

    #[test]
    fn clip_grade_identity_is_passthrough() {
        let pipeline = ColorPipeline::new();
        let input = vec![100, 100, 100, 255];
        let output = pipeline.apply_clip_grade(input.clone(), 0.0, 0.0, 1.0, 0.0, 0.0);
        assert_eq!(input, output);
    }

    #[test]
    fn global_lut_applies_when_enabled() {
        let mut pipeline = ColorPipeline::new();
        pipeline.enabled = true;
        // All-black LUT
        let lut = LutData {
            size: 2,
            data: vec![0.0; 2 * 2 * 2 * 3],
        };
        pipeline.global_lut = Some(lut);
        pipeline.global_lut_opacity = 1.0;

        let input = vec![200, 200, 200, 255];
        let output = pipeline.process_frame(input, 1, 1);
        assert_eq!(output[0], 0);
        assert_eq!(output[1], 0);
        assert_eq!(output[2], 0);
        assert_eq!(output[3], 255);
    }

    #[test]
    fn global_lut_opacity_blends() {
        let mut pipeline = ColorPipeline::new();
        pipeline.enabled = true;
        let lut = LutData {
            size: 2,
            data: vec![0.0; 2 * 2 * 2 * 3],
        };
        pipeline.global_lut = Some(lut);
        pipeline.global_lut_opacity = 0.5;

        let input = vec![200, 200, 200, 255];
        let output = pipeline.process_frame(input, 1, 1);
        assert_eq!(output[0], 100);
    }
}
