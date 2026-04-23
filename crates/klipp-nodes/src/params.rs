use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use klipp_core::{
    BlendMode, ColorWarperParams, CurveSet, FilmGrainSettings, HueSatCurves, LayerMixerInput,
    LogFormat, MaskStack, QualifierSettings,
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

    // ColorWarper
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warper_params: Option<ColorWarperParams>,

    // ColorMatch — pre-computed adjustments derived from running
    // klipp_color::match_colors against a user-picked reference frame.
    // None or default = pass-through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_match: Option<klipp_core::ColorMatchSettings>,

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

    // LayerMixer — per-port settings (None = default 2-input config,
    // composited Normal at full opacity). When the editor grows the
    // port count this Vec is grown in lockstep so `inputs[port]` is
    // always defined for any port the executor sees on an edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer_mixer: Option<LayerMixerParams>,
}

// ── LayerMixer ────────────────────────────────────────────────────

/// Per-input compositing settings for `NodeKind::LayerMixer`.
/// `LayerMixerInput` + `BlendMode` themselves live in `klipp_core` so
/// that `AppCommand` (also in klipp-core) can reference them without
/// inverting the crate dependency.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LayerMixerParams {
    /// One entry per input port. `inputs[0]` is port 0 (top of stack).
    pub inputs: Vec<LayerMixerInput>,
}

impl LayerMixerParams {
    /// True when every input is at default (Normal blend, opacity 1).
    /// Used by `NodeParams::is_default` so an unmodified mixer doesn't
    /// keep the clip out of identity-graph fast-paths.
    pub fn is_identity(&self) -> bool {
        self.inputs.iter().all(|i|
            i.blend_mode == BlendMode::Normal && (i.opacity - 1.0).abs() < 1e-4
        )
    }
}

impl NodeParams {
    /// Check if all params are at their default (identity) values — no grading applied.
    pub fn is_default(&self) -> bool {
        // Treat both None and Some(~0.0) as default for numeric fields
        let z = |f: &Option<f32>| f.map_or(true, |v| v.abs() < 0.0001);
        z(&self.exposure)
            && z(&self.contrast)
            && z(&self.clarity)
            && self.saturation.map_or(true, |v| (v - 1.0).abs() < 0.0001) // default is 1.0
            && z(&self.vibrance)
            && z(&self.temperature)
            && z(&self.tint)
            && z(&self.hue)
            && z(&self.highlights)
            && z(&self.shadows)
            && z(&self.whites)
            && z(&self.blacks)
            && z(&self.lift_x)
            && z(&self.lift_y)
            && z(&self.lift_master)
            && z(&self.gamma_x)
            && z(&self.gamma_y)
            && z(&self.gamma_master)
            && z(&self.gain_x)
            && z(&self.gain_y)
            && z(&self.gain_master)
            && z(&self.offset_x)
            && z(&self.offset_y)
            && z(&self.offset_master)
            && z(&self.log_low_x)
            && z(&self.log_low_y)
            && z(&self.log_low_master)
            && z(&self.log_mid_x)
            && z(&self.log_mid_y)
            && z(&self.log_mid_master)
            && z(&self.log_high_x)
            && z(&self.log_high_y)
            && z(&self.log_high_master)
            && self.wheels_log_mode.is_none()
            && z(&self.lift_sat)
            && z(&self.lift_bri)
            && z(&self.gamma_sat)
            && z(&self.gamma_bri)
            && z(&self.gain_sat)
            && z(&self.gain_bri)
            && z(&self.offset_sat)
            && z(&self.offset_bri)
            && z(&self.global_mix)
            && self.curves.is_none()
            && self.curves_preserve_luma.is_none()
            && z(&self.curves_mix)
            && self.huesat.is_none()
            && z(&self.huesat_mix)
            && self.log_format.is_none()
            && self.qualifier.is_none()
            && self.masks.is_none()
            && self.lut_path.is_none()
            && z(&self.lut_opacity)
            && self.grain.is_none()
            && self.warper_params.is_none()
            && z(&self.denoise_luma)
            && z(&self.denoise_chroma)
            && self.denoise_radius.is_none()
            && z(&self.mix)
            && self.layer_mixer.as_ref().map_or(true, |m| m.is_identity())
    }

    /// Apply a keyframe override by param name.
    /// Values near 0.0 are stored as None (neutral) to keep is_default() correct.
    pub fn apply_override(&mut self, param_name: &str, value: f32) {
        // For most params, near-zero means neutral → store None
        let n = if value.abs() < 0.0001 { None } else { Some(value) };
        match param_name {
            "exposure" => self.exposure = n,
            "contrast" => self.contrast = n,
            "clarity" => self.clarity = n,
            "saturation" => self.saturation = Some(value), // default is 1.0, not 0.0
            "vibrance" => self.vibrance = n,
            "temperature" => self.temperature = n,
            "tint" => self.tint = n,
            "highlights" => self.highlights = n,
            "shadows" => self.shadows = n,
            "whites" => self.whites = n,
            "blacks" => self.blacks = n,
            "lift_x" => self.lift_x = n,
            "lift_y" => self.lift_y = n,
            "lift_master" => self.lift_master = n,
            "gamma_x" => self.gamma_x = n,
            "gamma_y" => self.gamma_y = n,
            "gamma_master" => self.gamma_master = n,
            "gain_x" => self.gain_x = n,
            "gain_y" => self.gain_y = n,
            "gain_master" => self.gain_master = n,
            "offset_x" => self.offset_x = n,
            "offset_y" => self.offset_y = n,
            "offset_master" => self.offset_master = n,
            "log_low_x" => self.log_low_x = n,
            "log_low_y" => self.log_low_y = n,
            "log_low_master" => self.log_low_master = n,
            "log_mid_x" => self.log_mid_x = n,
            "log_mid_y" => self.log_mid_y = n,
            "log_mid_master" => self.log_mid_master = n,
            "log_high_x" => self.log_high_x = n,
            "log_high_y" => self.log_high_y = n,
            "log_high_master" => self.log_high_master = n,
            "wheels_log_mode" => self.wheels_log_mode = Some(value > 0.5),
            "lift_sat" => self.lift_sat = n,
            "lift_bri" => self.lift_bri = n,
            "gamma_sat" => self.gamma_sat = n,
            "gamma_bri" => self.gamma_bri = n,
            "gain_sat" => self.gain_sat = n,
            "gain_bri" => self.gain_bri = n,
            "offset_sat" => self.offset_sat = n,
            "offset_bri" => self.offset_bri = n,
            "hue" => self.hue = n,
            "global_mix" => self.global_mix = n,
            "curves_mix" => self.curves_mix = n,
            "curves_preserve_luma" => self.curves_preserve_luma = Some(value > 0.5),
            "huesat_mix" => self.huesat_mix = n,
            "lut_opacity" => self.lut_opacity = n,
            "mix" => self.mix = n,
            p if p.starts_with("warper_hue_") => {
                if let Ok(idx) = p["warper_hue_".len()..].parse::<usize>() {
                    if idx < 12 {
                        let w = self.warper_params.get_or_insert_with(Default::default);
                        w.hue_shifts[idx] = value;
                    }
                }
            }
            p if p.starts_with("warper_sat_") => {
                if let Ok(idx) = p["warper_sat_".len()..].parse::<usize>() {
                    if idx < 12 {
                        let w = self.warper_params.get_or_insert_with(Default::default);
                        w.sat_shifts[idx] = value;
                    }
                }
            }
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
