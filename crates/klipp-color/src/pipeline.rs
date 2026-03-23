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
    pub offset: (f32, f32, f32),
    /// Per-wheel sat/bri
    pub lift_sat: f32,
    pub lift_bri: f32,
    pub gamma_sat: f32,
    pub gamma_bri: f32,
    pub gain_sat: f32,
    pub gain_bri: f32,
    pub offset_sat: f32,
    pub offset_bri: f32,
    /// Global controls
    pub hue: f32,
    pub global_mix: f32,
    /// Pre-computed curve LUTs (None = identity/skip)
    pub curve_luts: Option<CurveLuts>,
    pub curves_preserve_luma: bool,
    pub curves_mix: f32,
    pub huesat: klipp_core::HueSatCurves,
    pub huesat_mix: f32,
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

    /// Load a LUT file (.cube, .3dl, .csp) as the global LUT.
    pub fn load_global_lut(&mut self, path: &Path) -> Result<(), lut::LutError> {
        let lut_data = lut::load_lut(path)?;
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
        // Save original for global mix if needed
        let original = if params.global_mix < 0.999 { Some(rgba.clone()) } else { None };

        // 1. Exposure
        transforms::apply_exposure(&mut rgba, params.exposure);
        // 2. Tone controls
        transforms::apply_tone_controls(&mut rgba, params.highlights, params.shadows, params.whites, params.blacks);
        // 3. Lift/Gamma/Gain/Offset wheels
        transforms::apply_lift_gamma_gain_offset(&mut rgba, params.lift, params.gamma, params.gain, params.offset);
        // 4. Per-wheel Sat/Bri
        transforms::apply_wheel_sat_bri(
            &mut rgba,
            params.lift_sat, params.lift_bri,
            params.gamma_sat, params.gamma_bri,
            params.gain_sat, params.gain_bri,
            params.offset_sat, params.offset_bri,
        );
        // 5. Contrast
        transforms::apply_contrast(&mut rgba, params.contrast);
        // 6. Saturation
        transforms::apply_saturation(&mut rgba, params.saturation);
        // 7. Curves (with optional preserve luma + mix)
        if let Some(luts) = &params.curve_luts {
            let pre_curves = if params.curves_preserve_luma || params.curves_mix < 0.999 {
                Some(rgba.clone())
            } else {
                None
            };
            crate::curves::apply_curves(&mut rgba, &luts.master, &luts.red, &luts.green, &luts.blue);
            // 7b. Preserve luma
            if params.curves_preserve_luma {
                if let Some(ref pre) = pre_curves {
                    for (g, o) in rgba.chunks_exact_mut(4).zip(pre.chunks_exact(4)) {
                        let luma_before = 0.2126 * o[0] as f32 + 0.7152 * o[1] as f32 + 0.0722 * o[2] as f32;
                        let luma_after = 0.2126 * g[0] as f32 + 0.7152 * g[1] as f32 + 0.0722 * g[2] as f32;
                        if luma_after > 0.5 {
                            let ratio = luma_before / luma_after;
                            g[0] = ((g[0] as f32 * ratio).clamp(0.0, 255.0)) as u8;
                            g[1] = ((g[1] as f32 * ratio).clamp(0.0, 255.0)) as u8;
                            g[2] = ((g[2] as f32 * ratio).clamp(0.0, 255.0)) as u8;
                        }
                    }
                }
            }
            // 7c. Curves mix
            if params.curves_mix < 0.999 {
                if let Some(ref pre) = pre_curves {
                    let mix = params.curves_mix;
                    for (g, o) in rgba.chunks_exact_mut(4).zip(pre.chunks_exact(4)) {
                        g[0] = ((o[0] as f32 + (g[0] as f32 - o[0] as f32) * mix).clamp(0.0, 255.0)) as u8;
                        g[1] = ((o[1] as f32 + (g[1] as f32 - o[1] as f32) * mix).clamp(0.0, 255.0)) as u8;
                        g[2] = ((o[2] as f32 + (g[2] as f32 - o[2] as f32) * mix).clamp(0.0, 255.0)) as u8;
                    }
                }
            }
        }
        // 8. Hue/Sat curves
        if !params.huesat.is_all_identity() {
            crate::huesat::apply_huesat_curves(&mut rgba, &params.huesat, params.huesat_mix);
        }
        // 9. Temperature/Tint
        transforms::apply_temperature_tint(&mut rgba, params.temperature, params.tint);
        // 10. Hue rotation
        transforms::apply_hue_rotation(&mut rgba, params.hue);
        // 11. Global mix
        if let Some(orig) = &original {
            transforms::apply_global_mix(&mut rgba, orig, params.global_mix);
        }
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
