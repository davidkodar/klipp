use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use klipp_core::{
    CurveSet, FilmGrainSettings, HueSatCurves, LogFormat, MaskStack, QualifierSettings,
};

/// All possible parameter values a node can have.
/// Each node kind uses only a subset of these fields.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NodeParams {
    // InputTransform
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_format: Option<LogFormat>,

    // Exposure
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposure: Option<f32>,

    // Contrast
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contrast: Option<f32>,
    // Clarity (midtone detail)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clarity: Option<f32>,

    // Saturation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saturation: Option<f32>,
    // Vibrance
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vibrance: Option<f32>,

    // ToneControls
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub highlights: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadows: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub whites: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blacks: Option<f32>,

    // ColorWheels
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lift_x: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lift_y: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lift_master: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gamma_x: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gamma_y: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gamma_master: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gain_x: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gain_y: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gain_master: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_x: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_y: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_master: Option<f32>,

    // Log Wheels (HDR mode: Low/Mid/High)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_low_x: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_low_y: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_low_master: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_mid_x: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_mid_y: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_mid_master: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_high_x: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_high_y: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_high_master: Option<f32>,
    /// Whether wheels operate in log mode (true) or linear mode (false/default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wheels_log_mode: Option<bool>,

    // WheelSatBri
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lift_sat: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lift_bri: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gamma_sat: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gamma_bri: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gain_sat: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gain_bri: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_sat: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset_bri: Option<f32>,

    // ColorBalance
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tint: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hue: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global_mix: Option<f32>,

    // RGBCurves
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curves: Option<CurveSet>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curves_preserve_luma: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curves_mix: Option<f32>,

    // HueSatCurves
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub huesat: Option<HueSatCurves>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub huesat_mix: Option<f32>,

    // Qualifier
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qualifier: Option<QualifierSettings>,

    // Mask
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub masks: Option<MaskStack>,

    // Lut3D
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lut_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lut_opacity: Option<f32>,

    // FilmGrain
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grain: Option<FilmGrainSettings>,

    // Denoise
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denoise_luma: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denoise_chroma: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denoise_radius: Option<u32>,

    // Blend / Layer / ApplyMatte
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mix: Option<f32>,
}

impl NodeParams {
    /// Check if all params are at their default (identity) values — no grading applied.
    pub fn is_default(&self) -> bool {
        self.exposure.is_none()
            && self.contrast.is_none()
            && self.saturation.is_none()
            && self.temperature.is_none()
            && self.tint.is_none()
            && self.hue.is_none()
            && self.highlights.is_none()
            && self.shadows.is_none()
            && self.whites.is_none()
            && self.blacks.is_none()
            && self.lift_x.is_none()
            && self.lift_y.is_none()
            && self.lift_master.is_none()
            && self.gamma_x.is_none()
            && self.gamma_y.is_none()
            && self.gamma_master.is_none()
            && self.gain_x.is_none()
            && self.gain_y.is_none()
            && self.gain_master.is_none()
            && self.offset_x.is_none()
            && self.offset_y.is_none()
            && self.offset_master.is_none()
            && self.curves.is_none()
            && self.huesat.is_none()
            && self.global_mix.is_none()
            && self.log_format.is_none()
            && self.qualifier.is_none()
            && self.masks.is_none()
    }

    /// Apply a keyframe override by param name.
    pub fn apply_override(&mut self, param_name: &str, value: f32) {
        match param_name {
            "exposure" => self.exposure = Some(value),
            "contrast" => self.contrast = Some(value),
            "clarity" => self.clarity = Some(value),
            "saturation" => self.saturation = Some(value),
            "vibrance" => self.vibrance = Some(value),
            "temperature" => self.temperature = Some(value),
            "tint" => self.tint = Some(value),
            "highlights" => self.highlights = Some(value),
            "shadows" => self.shadows = Some(value),
            "whites" => self.whites = Some(value),
            "blacks" => self.blacks = Some(value),
            "lift_x" => self.lift_x = Some(value),
            "lift_y" => self.lift_y = Some(value),
            "lift_master" => self.lift_master = Some(value),
            "gamma_x" => self.gamma_x = Some(value),
            "gamma_y" => self.gamma_y = Some(value),
            "gamma_master" => self.gamma_master = Some(value),
            "gain_x" => self.gain_x = Some(value),
            "gain_y" => self.gain_y = Some(value),
            "gain_master" => self.gain_master = Some(value),
            "offset_x" => self.offset_x = Some(value),
            "offset_y" => self.offset_y = Some(value),
            "offset_master" => self.offset_master = Some(value),
            "log_low_x" => self.log_low_x = Some(value),
            "log_low_y" => self.log_low_y = Some(value),
            "log_low_master" => self.log_low_master = Some(value),
            "log_mid_x" => self.log_mid_x = Some(value),
            "log_mid_y" => self.log_mid_y = Some(value),
            "log_mid_master" => self.log_mid_master = Some(value),
            "log_high_x" => self.log_high_x = Some(value),
            "log_high_y" => self.log_high_y = Some(value),
            "log_high_master" => self.log_high_master = Some(value),
            "wheels_log_mode" => self.wheels_log_mode = Some(value > 0.5),
            "lift_sat" => self.lift_sat = Some(value),
            "lift_bri" => self.lift_bri = Some(value),
            "gamma_sat" => self.gamma_sat = Some(value),
            "gamma_bri" => self.gamma_bri = Some(value),
            "gain_sat" => self.gain_sat = Some(value),
            "gain_bri" => self.gain_bri = Some(value),
            "offset_sat" => self.offset_sat = Some(value),
            "offset_bri" => self.offset_bri = Some(value),
            "hue" => self.hue = Some(value),
            "global_mix" => self.global_mix = Some(value),
            "curves_mix" => self.curves_mix = Some(value),
            "curves_preserve_luma" => self.curves_preserve_luma = Some(value > 0.5),
            "huesat_mix" => self.huesat_mix = Some(value),
            "lut_opacity" => self.lut_opacity = Some(value),
            "mix" => self.mix = Some(value),
            "denoise_luma" => self.denoise_luma = Some(value),
            "denoise_chroma" => self.denoise_chroma = Some(value),
            "denoise_radius" => self.denoise_radius = Some(value as u32),
            _ => {}
        }
    }
}

// ── Keyframe tracks for nodes ──────────────────────────────────────────────

/// Keyframe tracks for a single node.
/// Each track identifies a parameter by name string.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NodeKeyframeTracks {
    #[serde(default)]
    pub tracks: Vec<NodeKeyframeTrack>,
}

/// A single keyframe track animating one parameter of a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeKeyframeTrack {
    /// e.g. "exposure", "lift_x"
    pub param_name: String,
    /// Sorted by time. Reuses existing Keyframe struct.
    pub keyframes: Vec<klipp_core::Keyframe>,
}

impl NodeKeyframeTracks {
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty() || self.tracks.iter().all(|t| t.keyframes.is_empty())
    }

    /// Evaluate all animated params at clip_time_secs.
    /// Returns owned (param_name, value) pairs.
    pub fn evaluate_all(&self, time_secs: f64) -> Vec<(String, f32)> {
        self.tracks
            .iter()
            .filter_map(|t| t.evaluate(time_secs).map(|v| (t.param_name.clone(), v)))
            .collect()
    }

    /// Find or create a track for the given parameter.
    pub fn track_mut(&mut self, param_name: &str) -> &mut NodeKeyframeTrack {
        if let Some(idx) = self.tracks.iter().position(|t| t.param_name == param_name) {
            &mut self.tracks[idx]
        } else {
            self.tracks.push(NodeKeyframeTrack {
                param_name: param_name.to_string(),
                keyframes: Vec::new(),
            });
            self.tracks.last_mut().unwrap()
        }
    }

    /// Check if a specific parameter has keyframes.
    pub fn has_keyframes(&self, param_name: &str) -> bool {
        self.tracks
            .iter()
            .any(|t| t.param_name == param_name && !t.keyframes.is_empty())
    }
}

impl NodeKeyframeTrack {
    /// Evaluate the track at a given time (seconds relative to clip source_in).
    /// Returns None if the track has no keyframes.
    pub fn evaluate(&self, time_secs: f64) -> Option<f32> {
        let kfs = &self.keyframes;
        if kfs.is_empty() {
            return None;
        }
        if kfs.len() == 1 {
            return Some(kfs[0].value);
        }

        // Before first keyframe: hold first value
        if time_secs <= kfs[0].time_secs {
            return Some(kfs[0].value);
        }
        // After last keyframe: hold last value
        if time_secs >= kfs[kfs.len() - 1].time_secs {
            return Some(kfs[kfs.len() - 1].value);
        }

        // Binary search for the segment
        let idx = kfs.partition_point(|kf| kf.time_secs <= time_secs);
        let idx = idx.saturating_sub(1);
        let kf0 = &kfs[idx];
        let kf1 = &kfs[(idx + 1).min(kfs.len() - 1)];

        let dt = kf1.time_secs - kf0.time_secs;
        if dt <= 0.0 {
            return Some(kf0.value);
        }

        let t = ((time_secs - kf0.time_secs) / dt) as f32;

        match kf0.interpolation {
            klipp_core::Interpolation::Linear => Some(kf0.value + (kf1.value - kf0.value) * t),
            klipp_core::Interpolation::Step => Some(kf0.value),
            klipp_core::Interpolation::Bezier => {
                let p0 = kf0.value;
                let p1 = kf0.value + kf0.tangent_out.1;
                let p2 = kf1.value + kf1.tangent_in.1;
                let p3 = kf1.value;
                let u = 1.0 - t;
                let u2 = u * u;
                let t2 = t * t;
                Some(u2 * u * p0 + 3.0 * u2 * t * p1 + 3.0 * u * t2 * p2 + t2 * t * p3)
            }
        }
    }

    /// Add a keyframe at the given time. If one exists at that time (±1ms), update it.
    pub fn set_keyframe(&mut self, time_secs: f64, value: f32) {
        let epsilon = 0.001;
        for kf in &mut self.keyframes {
            if (kf.time_secs - time_secs).abs() < epsilon {
                kf.value = value;
                return;
            }
        }
        let idx = self.keyframes.partition_point(|kf| kf.time_secs < time_secs);
        self.keyframes.insert(
            idx,
            klipp_core::Keyframe {
                time_secs,
                value,
                interpolation: klipp_core::Interpolation::Linear,
                tangent_out: (0.0, 0.0),
                tangent_in: (0.0, 0.0),
            },
        );
    }

    /// Remove the keyframe nearest to the given time (within ±10ms).
    pub fn remove_keyframe(&mut self, time_secs: f64) -> bool {
        let epsilon = 0.01;
        if let Some(idx) = self
            .keyframes
            .iter()
            .position(|kf| (kf.time_secs - time_secs).abs() < epsilon)
        {
            self.keyframes.remove(idx);
            true
        } else {
            false
        }
    }
}
