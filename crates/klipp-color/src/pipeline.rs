use crate::lut::{self, LutData};
use crate::transforms;
use std::path::{Path, PathBuf};

/// All color grade parameters in a single struct (no klipp-timeline dependency).
#[derive(Debug, Clone)]
pub struct ColorGradeParams {
    pub exposure: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub temperature: f32,
    pub tint: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    /// Pre-converted RGB shift from wheel_offset_to_rgb
    pub lift: (f32, f32, f32),
    pub gamma: (f32, f32, f32),
    pub gain: (f32, f32, f32),
    /// Pre-computed curve LUTs (None = identity/skip)
    pub curve_luts: Option<CurveLuts>,
}

/// Pre-computed 256-entry lookup tables for RGB curves.
#[derive(Debug, Clone)]
pub struct CurveLuts {
    pub master: [u8; 256],
    pub red: [u8; 256],
    pub green: [u8; 256],
    pub blue: [u8; 256],
}

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

    /// Apply per-clip color grade to an RGBA frame using the full parameter set.
    pub fn apply_clip_grade_full(&self, mut rgba: Vec<u8>, params: &ColorGradeParams) -> Vec<u8> {
        transforms::apply_exposure(&mut rgba, params.exposure);
        transforms::apply_tone_controls(&mut rgba, params.highlights, params.shadows, params.whites, params.blacks);
        transforms::apply_lift_gamma_gain(&mut rgba, params.lift, params.gamma, params.gain);
        transforms::apply_contrast(&mut rgba, params.contrast);
        transforms::apply_saturation(&mut rgba, params.saturation);
        if let Some(luts) = &params.curve_luts {
            crate::curves::apply_curves(&mut rgba, &luts.master, &luts.red, &luts.green, &luts.blue);
        }
        transforms::apply_temperature_tint(&mut rgba, params.temperature, params.tint);
        rgba
    }

    /// Legacy: apply per-clip color grade with basic parameters only.
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
