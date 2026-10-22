//! A clip's picture as revision 1 wrote it: the one mapping from an
//! in-memory [`Clip`] to the picture fields of its saved record (#1089,
//! phase 2b M1b).
//!
//! Save fills its `ClipJson` from [`ClipPictureV1::of`], and the picture
//! store lifts the same value, so the file and the store can't drift
//! apart. The serde attributes here are the file's: a lane that is empty
//! is written as `null`, built-in effects go through `to_value` (which
//! widens their f32 digits, as the file always has), and modifier keys
//! are [`ParamId::to_key`](klipp_core::ParamId::to_key) strings.

use crate::{Clip, GradeAccess};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

fn default_visible() -> bool {
    true
}

/// The picture fields of a clip's saved record, with the file's serde.
/// Destructure it exhaustively where it is used, so a field added here
/// can't be left out of the file or the store.
#[derive(Debug, Serialize, Deserialize)]
pub struct ClipPictureV1 {
    /// Always written: the clip's grade.
    #[serde(default)]
    pub node_graph: Option<klipp_nodes::NodeGraph>,
    /// A retired field, still written as `null`.
    #[serde(default)]
    pub color_grade: Option<serde_json::Value>,
    #[serde(default)]
    pub effects: Vec<EffectJson>,
    #[serde(default)]
    pub builtin_effects: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub builtin_effects_enabled: Vec<bool>,
    #[serde(default)]
    pub opacity_keyframes: Option<Vec<klipp_core::Keyframe>>,
    #[serde(default)]
    pub transform_pos_x_keyframes: Option<Vec<klipp_core::Keyframe>>,
    #[serde(default)]
    pub transform_pos_y_keyframes: Option<Vec<klipp_core::Keyframe>>,
    #[serde(default)]
    pub transform_scale_keyframes: Option<Vec<klipp_core::Keyframe>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform_scale_y_keyframes: Option<Vec<klipp_core::Keyframe>>,
    #[serde(default)]
    pub transform_rotation_keyframes: Option<Vec<klipp_core::Keyframe>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform_tilt_x_keyframes: Option<Vec<klipp_core::Keyframe>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform_tilt_y_keyframes: Option<Vec<klipp_core::Keyframe>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_keyframes: Option<HashMap<String, Vec<klipp_core::Keyframe>>>,
    /// Every modifier stack, the sound's (`volume`, `pan`, `gain`)
    /// included: the lift keeps those on the clip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param_modifiers: Option<HashMap<String, klipp_core::ModifierStack>>,
    #[serde(default)]
    pub grade_versions: Vec<GradeVersionJson>,
    #[serde(default)]
    pub active_grade_version: Option<usize>,
}

fn lane(keys: &[klipp_core::Keyframe]) -> Option<Vec<klipp_core::Keyframe>> {
    (!keys.is_empty()).then(|| keys.to_vec())
}

impl ClipPictureV1 {
    /// The picture fields `c` saves as.
    pub fn of(c: &Clip) -> Self {
        Self {
            node_graph: Some(c.grade().clone()),
            color_grade: None,
            effects: c.ofx_effects().iter().map(EffectJson::of).collect(),
            builtin_effects: if c.builtin_effects().is_empty() {
                None
            } else {
                serde_json::to_value(c.builtin_effects()).ok()
            },
            builtin_effects_enabled: c.builtin_effects_enabled().to_vec(),
            opacity_keyframes: lane(c.opacity_keyframes()),
            transform_pos_x_keyframes: lane(c.transform_pos_x_keyframes()),
            transform_pos_y_keyframes: lane(c.transform_pos_y_keyframes()),
            transform_scale_keyframes: lane(c.transform_scale_keyframes()),
            transform_scale_y_keyframes: lane(c.transform_scale_y_keyframes()),
            transform_rotation_keyframes: lane(c.transform_rotation_keyframes()),
            transform_tilt_x_keyframes: lane(c.transform_tilt_x_keyframes()),
            transform_tilt_y_keyframes: lane(c.transform_tilt_y_keyframes()),
            effect_keyframes: (!c.effect_keyframes().is_empty()).then(|| c.effect_keyframes().clone()),
            param_modifiers: (!c.param_modifiers().is_empty())
                .then(|| c.param_modifiers().iter().map(|(k, v)| (k.to_key(), v.clone())).collect()),
            grade_versions: c
                .picture.view().grade_versions
                .iter()
                .map(|gv| GradeVersionJson { name: gv.name.clone(), node_graph: gv.grade().clone() })
                .collect(),
            active_grade_version: c.picture.view().active_grade_version,
        }
    }
}

/// An OFX effect as the file holds it.
#[derive(Debug, Serialize, Deserialize)]
pub struct EffectJson {
    pub plugin_name: String,
    #[serde(default = "default_visible")]
    pub enabled: bool,
    #[serde(default)]
    pub param_values: HashMap<String, OfxValueJson>,
    /// The context it was applied in; a Filter's is left out, so a file
    /// with none reads as one (#1144).
    #[serde(default, skip_serializing_if = "is_filter")]
    pub context: klipp_plugin::OfxCategory,
    /// A Transition's amount; 0 is left out.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub transition_amount: f64,
    /// The entry's own id (#1500); an entry without one is written as
    /// before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

fn is_filter(c: &klipp_plugin::OfxCategory) -> bool {
    *c == klipp_plugin::OfxCategory::Filter
}

fn is_zero(t: &f64) -> bool {
    *t == 0.0
}

/// OFX parameter value for JSON serialisation.
/// Uses tagged format for unambiguous round-trips.
/// Old projects (untagged) are handled by a custom Deserialize fallback.
#[derive(Debug, Serialize)]
#[serde(tag = "t", content = "v")]
pub enum OfxValueJson {
    Double(f64),
    Double2([f64; 2]),
    Double3([f64; 3]),
    Int(i32),
    Int2([i32; 2]),
    Int3([i32; 3]),
    Bool(bool),
    Choice(i32),
    String(String),
    Rgba([f64; 4]),
    Rgb([f64; 3]),
}

// Custom Deserialize: try tagged first, then fall back to untagged for old projects.
impl<'de> serde::Deserialize<'de> for OfxValueJson {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de;
        use serde_json::Value;

        let v = Value::deserialize(deserializer)?;

        // Tagged format: {"t": "Double", "v": 1.0}
        if let Some(tag) = v.get("t").and_then(|t| t.as_str()) {
            return match tag {
                "Double" => v.get("v").and_then(|x| x.as_f64()).map(OfxValueJson::Double)
                    .ok_or_else(|| de::Error::custom("invalid Double value")),
                "Double2" => serde_json::from_value(v.get("v").cloned().unwrap_or_default())
                    .map(OfxValueJson::Double2).map_err(de::Error::custom),
                "Double3" => serde_json::from_value(v.get("v").cloned().unwrap_or_default())
                    .map(OfxValueJson::Double3).map_err(de::Error::custom),
                "Int" => v.get("v").and_then(|x| x.as_i64()).map(|i| OfxValueJson::Int(i as i32))
                    .ok_or_else(|| de::Error::custom("invalid Int value")),
                "Int2" => serde_json::from_value(v.get("v").cloned().unwrap_or_default())
                    .map(OfxValueJson::Int2).map_err(de::Error::custom),
                "Int3" => serde_json::from_value(v.get("v").cloned().unwrap_or_default())
                    .map(OfxValueJson::Int3).map_err(de::Error::custom),
                "Bool" => v.get("v").and_then(|x| x.as_bool()).map(OfxValueJson::Bool)
                    .ok_or_else(|| de::Error::custom("invalid Bool value")),
                "Choice" => v.get("v").and_then(|x| x.as_i64()).map(|i| OfxValueJson::Choice(i as i32))
                    .ok_or_else(|| de::Error::custom("invalid Choice value")),
                "String" => v.get("v").and_then(|x| x.as_str()).map(|s| OfxValueJson::String(s.to_string()))
                    .ok_or_else(|| de::Error::custom("invalid String value")),
                "Rgba" => serde_json::from_value(v.get("v").cloned().unwrap_or_default())
                    .map(OfxValueJson::Rgba).map_err(de::Error::custom),
                "Rgb" => serde_json::from_value(v.get("v").cloned().unwrap_or_default())
                    .map(OfxValueJson::Rgb).map_err(de::Error::custom),
                _ => Err(de::Error::custom(format!("unknown OfxValueJson tag: {tag}"))),
            };
        }

        // Legacy untagged format: bare JSON values
        if let Some(b) = v.as_bool() {
            return Ok(OfxValueJson::Bool(b));
        }
        if let Some(s) = v.as_str() {
            return Ok(OfxValueJson::String(s.to_string()));
        }
        if let Some(n) = v.as_i64() {
            // Old format stored Int as i64
            return Ok(OfxValueJson::Int(n as i32));
        }
        if let Some(n) = v.as_f64() {
            return Ok(OfxValueJson::Double(n));
        }

        Err(de::Error::custom("cannot parse OfxValueJson"))
    }
}

impl EffectJson {
    /// The OFX effect `effect` as the file holds it.
    pub fn of(effect: &klipp_plugin::AppliedEffect) -> Self {
        use klipp_plugin::OfxValue as V;
        EffectJson {
            plugin_name: effect.plugin_name.clone(),
            enabled: effect.enabled,
            param_values: effect
                .param_values
                .iter()
                .map(|(k, v)| {
                    let json_val = match v {
                        V::Double(d) => OfxValueJson::Double(*d),
                        V::Double2(a, b) => OfxValueJson::Double2([*a, *b]),
                        V::Double3(a, b, c) => OfxValueJson::Double3([*a, *b, *c]),
                        V::Int(i) => OfxValueJson::Int(*i),
                        V::Int2(a, b) => OfxValueJson::Int2([*a, *b]),
                        V::Int3(a, b, c) => OfxValueJson::Int3([*a, *b, *c]),
                        V::Bool(b) => OfxValueJson::Bool(*b),
                        V::Choice(c) => OfxValueJson::Choice(*c),
                        V::String(s) => OfxValueJson::String(s.clone()),
                        V::Rgba(r, g, b, a) => OfxValueJson::Rgba([*r, *g, *b, *a]),
                        V::Rgb(r, g, b) => OfxValueJson::Rgb([*r, *g, *b]),
                    };
                    (k.clone(), json_val)
                })
                .collect(),
            context: effect.context,
            transition_amount: effect.transition_amount,
            id: effect.id.clone(),
        }
    }

    /// The OFX effect the file's entry holds: the inverse of [`Self::of`],
    /// what a load and a reader of the clip's picture make of it.
    pub fn to_effect(&self) -> klipp_plugin::AppliedEffect {
        use klipp_plugin::OfxValue as V;
        klipp_plugin::AppliedEffect {
            plugin_name: self.plugin_name.clone(),
            enabled: self.enabled,
            param_values: self
                .param_values
                .iter()
                .map(|(k, v)| {
                    let val = match v {
                        OfxValueJson::Double(d) => V::Double(*d),
                        OfxValueJson::Double2([a, b]) => V::Double2(*a, *b),
                        OfxValueJson::Double3([a, b, c]) => V::Double3(*a, *b, *c),
                        OfxValueJson::Int(i) => V::Int(*i),
                        OfxValueJson::Int2([a, b]) => V::Int2(*a, *b),
                        OfxValueJson::Int3([a, b, c]) => V::Int3(*a, *b, *c),
                        OfxValueJson::Bool(b) => V::Bool(*b),
                        OfxValueJson::Choice(c) => V::Choice(*c),
                        OfxValueJson::String(s) => V::String(s.clone()),
                        OfxValueJson::Rgba([r, g, b, a]) => V::Rgba(*r, *g, *b, *a),
                        OfxValueJson::Rgb([r, g, b]) => V::Rgb(*r, *g, *b),
                    };
                    (k.clone(), val)
                })
                .collect(),
            context: self.context,
            transition_amount: self.transition_amount,
            id: self.id.clone(),
            ..Default::default()
        }
    }
}

/// A grade version as the file holds it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GradeVersionJson {
    pub name: String,
    pub node_graph: klipp_nodes::NodeGraph,
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{Interpolation, Keyframe, MediaId, ModifierStack, ParamId, RationalTime};

    fn key(time_secs: f64, value: f32) -> Keyframe {
        Keyframe { time_secs, value, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }
    }

    /// The bytes the three private lane types save as: each the plain
    /// list or map it holds, with empty lanes `null` or left out exactly
    /// as the file has always had them.
    #[test]
    fn the_picture_fields_save_as_the_file_has_them() {
        let t = |n| RationalTime::from_frames(n, 24);
        let mut clip = Clip::media(MediaId::new(), t(0), t(48), t(0), t(48));
        clip.edit_opacity_keyframes(|v| v.extend([key(0.0, 1.0), key(1.5, 0.25)]));
        clip.edit_effect_keyframes(|m| m.insert("0:radius".into(), vec![key(0.5, 2.0)]));
        clip.edit_param_modifiers(|m| m.insert(ParamId::Opacity, ModifierStack::default()));
        let v = serde_json::to_value(ClipPictureV1::of(&clip)).unwrap();
        let lanes = serde_json::json!({
            "opacity_keyframes": v["opacity_keyframes"],
            "transform_pos_x_keyframes": v["transform_pos_x_keyframes"],
            "transform_tilt_x_keyframes": v.get("transform_tilt_x_keyframes"),
            "effect_keyframes": v["effect_keyframes"],
            "param_modifiers": v["param_modifiers"],
        });
        assert_eq!(
            serde_json::to_string(&lanes).unwrap(),
            concat!(
                r#"{"effect_keyframes":{"0:radius":[{"interpolation":"Linear","tangent_in":[0.0,0.0],"tangent_out":[0.0,0.0],"time_secs":0.5,"value":2.0}]},"#,
                r#""opacity_keyframes":[{"interpolation":"Linear","tangent_in":[0.0,0.0],"tangent_out":[0.0,0.0],"time_secs":0.0,"value":1.0},"#,
                r#"{"interpolation":"Linear","tangent_in":[0.0,0.0],"tangent_out":[0.0,0.0],"time_secs":1.5,"value":0.25}],"#,
                r#""param_modifiers":{"opacity":{"modifiers":[]}},"#,
                r#""transform_pos_x_keyframes":null,"transform_tilt_x_keyframes":null}"#,
            )
        );
        // An empty tilt lane is left out of the record, not written as null.
        assert!(v.get("transform_tilt_x_keyframes").is_none());
    }
}
