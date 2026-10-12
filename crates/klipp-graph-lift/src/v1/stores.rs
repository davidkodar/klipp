// Frozen picture store fields from v1 at 7d309e9f.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Serialize, Deserialize)]
pub struct ClipPicture {
    #[serde(default)]
    pub node_graph: Option<NodeGraph>,
    #[serde(default)]
    pub effects: Vec<EffectJson>,
    #[serde(default)]
    pub builtin_effects: Option<Vec<BuiltinEffect>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub builtin_effects_enabled: Vec<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade_group: Option<String>,
    #[serde(default)]
    pub opacity_keyframes: Option<Vec<Keyframe>>,
    #[serde(default)]
    pub transform_pos_x_keyframes: Option<Vec<Keyframe>>,
    #[serde(default)]
    pub transform_pos_y_keyframes: Option<Vec<Keyframe>>,
    #[serde(default)]
    pub transform_scale_keyframes: Option<Vec<Keyframe>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform_scale_y_keyframes: Option<Vec<Keyframe>>,
    #[serde(default)]
    pub transform_rotation_keyframes: Option<Vec<Keyframe>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform_tilt_x_keyframes: Option<Vec<Keyframe>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform_tilt_y_keyframes: Option<Vec<Keyframe>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte_feather_keyframes: Option<Vec<Keyframe>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte_choke_keyframes: Option<Vec<Keyframe>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_keyframes: Option<std::collections::HashMap<String, Vec<Keyframe>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param_modifiers: Option<HashMap<String, ModifierStack>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blend_mode: Option<BlendMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<f32>,
    #[serde(default)]
    pub grade_versions: Vec<GradeVersion>,
    #[serde(default)]
    pub active_grade_version: Option<usize>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GradeVersion {
    pub name: String,
    pub node_graph: NodeGraph,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Look {
    pub id: uuid::Uuid,
    pub name: String,
    pub node_graph: NodeGraph,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ToolScope {
    Wheels,
    Curves,
    HueSat,
    Warper,
    Color,
    FullGrade,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPreset {
    pub name: String,
    pub tool: ToolScope,
    pub params: NodeParams,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EffectPreset {
    pub name: String,
    pub description: String,
    pub effect_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub builtin: Option<BuiltinEffect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ofx_params: Option<HashMap<String, OfxValue>>,
    #[serde(default)]
    pub is_builtin: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TrackPicture {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<Vec<BuiltinEffect>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub video_enabled: Vec<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade: Option<NodeGraph>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct GroupPicture {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre: Option<NodeGraph>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post: Option<NodeGraph>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct StillPicture {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade: Option<NodeGraph>,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum OfxValue {
    Double(f64),
    Double2(f64, f64),
    Double3(f64, f64, f64),
    Int(i32),
    Int2(i32, i32),
    Int3(i32, i32, i32),
    Bool(bool),
    Choice(i32),
    String(String),
    Rgba(f64, f64, f64, f64),
    Rgb(f64, f64, f64),
}
