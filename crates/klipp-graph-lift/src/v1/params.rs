// Frozen from crates/klipp-nodes/src/params.rs at 7d309e9f. Do not follow app type changes.
use super::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
#[derive(PartialEq, Debug, Clone, Serialize, Deserialize, Default)]
pub struct NodeParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_format: Option<LogFormat>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exposure: Option<f32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contrast: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contrast_pivot: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clarity: Option<f32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saturation: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vibrance: Option<f32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub highlights: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadows: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub whites: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blacks: Option<f32>,

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wheels_log_mode: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_low_range: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_high_range: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub luma_mix: Option<f32>,

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

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tint: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hue: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global_mix: Option<f32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curves: Option<CurveSet>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curves_preserve_luma: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curves_mix: Option<f32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub huesat: Option<HueSatCurves>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub huesat_mix: Option<f32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qualifier: Option<QualifierSettings>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub luma_key: Option<LumaKeySettings>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub difference_key: Option<DifferenceKeySettings>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soft_clip: Option<SoftClipSettings>,

    // HDR zones (#240), added after the freeze: a node of it reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hdr_zones: Option<HdrZonesSettings>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub masks: Option<MaskStack>,

    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "super::assets::lut_path_serde"
    )]
    pub lut_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lut_opacity: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lut_input_space: Option<super::WorkingColorspace>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lut_output_space: Option<super::WorkingColorspace>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primaries_from: Option<super::WorkingColorspace>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primaries_to: Option<super::WorkingColorspace>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grain: Option<FilmGrainSettings>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warper_params: Option<ColorWarperParams>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_match: Option<super::ColorMatchSettings>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denoise_luma: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denoise_chroma: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denoise_radius: Option<u32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mix: Option<f32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer_mixer: Option<LayerMixerParams>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte_image_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte_channel: Option<MatteChannel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte_invert: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte_combine_op: Option<MatteCombineOp>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compound_subgraph: Option<super::graph::NodeGraph>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MatteCombineOp {
    #[default]
    Intersect,
    Union,
    Subtract,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MatteChannel {
    Alpha,
    #[default]
    Luminance,
    Red,
    Green,
    Blue,
}

#[derive(PartialEq, Debug, Clone, Serialize, Deserialize, Default)]
pub struct LayerMixerParams {
    pub inputs: Vec<LayerMixerInput>,
}

#[derive(PartialEq, Debug, Clone, Serialize, Deserialize, Default)]
pub struct NodeKeyframeTracks {
    #[serde(default)]
    pub tracks: Vec<NodeKeyframeTrack>,
}

#[derive(PartialEq, Debug, Clone, Serialize, Deserialize)]
pub struct NodeKeyframeTrack {
    pub param_name: String,
    pub keyframes: Vec<super::Keyframe>,
}
