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
    pub qualifier: klipp_core::QualifierSettings,
    pub masks: klipp_core::MaskStack,
    /// Input device transform — camera log format for this clip
    pub input_log_format: klipp_core::LogFormat,
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
    /// When `matte_preview` is true, the output is replaced with a B&W matte visualisation
    /// (white = inside qualifier/mask, black = outside) and the grade is not applied.
    pub fn apply_clip_grade_full(&self, mut rgba: Vec<u8>, params: &ColorGradeParams, width: u32, height: u32, matte_preview: bool) -> Vec<u8> {
        // 0. Input Device Transform (camera log → Rec.709)
        transforms::apply_input_transform(&mut rgba, params.input_log_format);

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
        // 12. Secondary color correction (qualifier + masks)
        if params.qualifier.enabled || !params.masks.is_empty() {
            // Compute combined matte
            let matte = if params.qualifier.enabled {
                let mut matte = crate::qualifier::generate_qualifier_matte(
                    &rgba, width, height, &params.qualifier,
                );
                // Combine with mask matte (intersection: multiply)
                if !params.masks.is_empty() {
                    let mask_matte = crate::masks::generate_mask_matte(width, height, &params.masks);
                    for (m, mm) in matte.iter_mut().zip(mask_matte.iter()) {
                        *m *= mm;
                    }
                }
                matte
            } else {
                crate::masks::generate_mask_matte(width, height, &params.masks)
            };

            // Matte preview: replace output with B&W matte visualisation
            if matte_preview {
                for (i, chunk) in rgba.chunks_exact_mut(4).enumerate() {
                    let v = (matte[i] * 255.0) as u8;
                    chunk[0] = v;
                    chunk[1] = v;
                    chunk[2] = v;
                    // alpha unchanged
                }
                return rgba;
            }

            // Apply grade through matte
            if params.qualifier.enabled {
                crate::qualifier::apply_qualified_grade(&mut rgba, &matte, &params.qualifier);
            } else {
                // Masks without qualifier: apply as vignette
                for (i, chunk) in rgba.chunks_exact_mut(4).enumerate() {
                    let m = matte[i];
                    if m < 0.999 {
                        let dim = 0.7 + 0.3 * m;
                        chunk[0] = (chunk[0] as f32 * dim) as u8;
                        chunk[1] = (chunk[1] as f32 * dim) as u8;
                        chunk[2] = (chunk[2] as f32 * dim) as u8;
                    }
                }
            }
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

    // ─── Phase 2: Matte preview integration tests ────────────────────

    fn make_identity_params() -> ColorGradeParams {
        ColorGradeParams {
            exposure: 0.0, contrast: 0.0, saturation: 1.0,
            temperature: 0.0, tint: 0.0,
            highlights: 0.0, shadows: 0.0, whites: 0.0, blacks: 0.0,
            lift: (0.0, 0.0, 0.0), gamma: (0.0, 0.0, 0.0),
            gain: (0.0, 0.0, 0.0), offset: (0.0, 0.0, 0.0),
            lift_sat: 0.0, lift_bri: 0.0,
            gamma_sat: 0.0, gamma_bri: 0.0,
            gain_sat: 0.0, gain_bri: 0.0,
            offset_sat: 0.0, offset_bri: 0.0,
            hue: 0.0, global_mix: 1.0,
            curve_luts: None, curves_preserve_luma: false, curves_mix: 1.0,
            huesat: Default::default(), huesat_mix: 1.0,
            qualifier: Default::default(), masks: Default::default(),
            input_log_format: Default::default(),
        }
    }

    #[test]
    fn matte_preview_outputs_greyscale() {
        // Create a frame with varied colours + narrow qualifier
        let pipeline = ColorPipeline::new();
        let mut params = make_identity_params();
        params.qualifier.enabled = true;
        params.qualifier.hue_center = 0.0; // select red
        params.qualifier.hue_width = 30.0;
        params.qualifier.hue_soft = 10.0;

        // 4-pixel frame: red, green, blue, grey
        let frame = vec![
            255, 0, 0, 255,     // red → should be selected
            0, 255, 0, 255,     // green → excluded
            0, 0, 255, 255,     // blue → excluded
            128, 128, 128, 255, // grey → excluded (zero saturation)
        ];

        let output = pipeline.apply_clip_grade_full(frame, &params, 2, 2, true);

        // Matte preview: every pixel should be greyscale (R == G == B)
        for (i, chunk) in output.chunks_exact(4).enumerate() {
            assert_eq!(chunk[0], chunk[1],
                "pixel {i}: R={} != G={} — not greyscale", chunk[0], chunk[1]);
            assert_eq!(chunk[1], chunk[2],
                "pixel {i}: G={} != B={} — not greyscale", chunk[1], chunk[2]);
        }
    }

    #[test]
    fn matte_preview_white_inside_selection() {
        let pipeline = ColorPipeline::new();
        let mut params = make_identity_params();
        params.qualifier.enabled = true;
        params.qualifier.hue_center = 0.0;
        params.qualifier.hue_width = 30.0;
        params.qualifier.hue_soft = 10.0;

        // Pure red pixel — dead centre of hue selection
        let frame = vec![255, 0, 0, 255];
        let output = pipeline.apply_clip_grade_full(frame, &params, 1, 1, true);
        // Matte should be high (≥ 200) — inside selection
        assert!(output[0] >= 200,
            "red pixel matte should be bright: got {}", output[0]);
    }

    #[test]
    fn matte_preview_black_outside_selection() {
        let pipeline = ColorPipeline::new();
        let mut params = make_identity_params();
        params.qualifier.enabled = true;
        params.qualifier.hue_center = 0.0;
        params.qualifier.hue_width = 30.0;
        params.qualifier.hue_soft = 10.0;

        // Pure blue pixel — far outside red selection
        let frame = vec![0, 0, 255, 255];
        let output = pipeline.apply_clip_grade_full(frame, &params, 1, 1, true);
        // Matte should be low (≤ 30) — outside selection
        assert!(output[0] <= 30,
            "blue pixel matte should be dark: got {}", output[0]);
    }

    #[test]
    fn matte_preview_disabled_shows_grade() {
        let pipeline = ColorPipeline::new();
        let mut params = make_identity_params();
        params.qualifier.enabled = true;
        params.qualifier.hue_center = 0.0;
        params.qualifier.hue_width = 30.0;
        params.qualifier.hue_soft = 10.0;
        params.qualifier.exposure = 1.0; // boost exposure in qualifier

        // Varied frame
        let frame = vec![
            255, 0, 0, 255,    // red (selected, will be boosted)
            0, 255, 0, 255,    // green (not selected)
        ];

        let output = pipeline.apply_clip_grade_full(frame, &params, 2, 1, false);

        // With matte_preview=false, the grade should be applied normally
        // At least one pixel should NOT be greyscale (the graded output has colour)
        let has_colour = output.chunks_exact(4).any(|c| c[0] != c[1] || c[1] != c[2]);
        assert!(has_colour, "with matte_preview=false, output should have colour");
    }
}
