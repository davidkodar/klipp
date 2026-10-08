//! A clip's picture animation, reached only through these accessors (phase
//! 2b, milestone 1): the placement and opacity keyframe lanes, the
//! per-effect keyframes and the parameter modifiers.
//!
//! Each field is read as a method of its own name and changed in a closure,
//! as the grade (`GradeAccess`) and the effect list are. Milestone 1 moves
//! the picture into the revision-2 model under these methods: a closure's
//! end is where a change is taken into the picture, which a handed-out
//! `&mut` could not offer. Callers don't move again.

use std::collections::HashMap;

use klipp_core::{Keyframe, ModifierStack, ParamId};
use serde::{Deserialize, Serialize};

use crate::Clip;

/// One of a clip's keyframe lanes. The field holding it stays public, so
/// whole-struct literals and struct update still build a `Clip` in any
/// crate; its contents are private, read and changed through the clip's
/// methods (`opacity_keyframes()`, `edit_opacity_keyframes(..)`, ...).
/// Saved as the list itself, so files are as they were.
///
/// The contents are private: reaching into them doesn't compile.
///
/// ```compile_fail,E0616
/// fn add(clip: &mut klipp_timeline::Clip, k: klipp_core::Keyframe) {
///     clip.opacity_keyframes.0.push(k);
/// }
/// ```
///
/// The same change through the method does.
///
/// ```
/// fn add(clip: &mut klipp_timeline::Clip, k: klipp_core::Keyframe) {
///     clip.edit_opacity_keyframes(|v| v.push(k));
/// }
/// ```
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Keyframes(pub(crate) Vec<Keyframe>);

impl From<Vec<Keyframe>> for Keyframes {
    fn from(v: Vec<Keyframe>) -> Self {
        Self(v)
    }
}

impl Keyframes {
    /// Whether the lane has no keyframes, for `skip_serializing_if`.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// A clip's per-effect keyframes, keyed `"{effect index}:{param}"`, behind
/// the clip's methods as [`Keyframes`] is.
///
/// ```compile_fail,E0616
/// fn clear(clip: &mut klipp_timeline::Clip) {
///     clip.effect_keyframes.0.clear();
/// }
/// ```
///
/// ```
/// fn clear(clip: &mut klipp_timeline::Clip) {
///     clip.edit_effect_keyframes(|m| m.clear());
/// }
/// ```
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EffectKeyframes(pub(crate) HashMap<String, Vec<Keyframe>>);

impl From<HashMap<String, Vec<Keyframe>>> for EffectKeyframes {
    fn from(m: HashMap<String, Vec<Keyframe>>) -> Self {
        Self(m)
    }
}

/// A clip's parameter modifier stacks, behind the clip's methods as
/// [`Keyframes`] is.
///
/// ```compile_fail,E0616
/// fn clear(clip: &mut klipp_timeline::Clip) {
///     clip.param_modifiers.0.clear();
/// }
/// ```
///
/// ```
/// fn clear(clip: &mut klipp_timeline::Clip) {
///     clip.edit_param_modifiers(|m| m.clear());
/// }
/// ```
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ParamModifiers(pub(crate) HashMap<ParamId, ModifierStack>);

impl From<HashMap<ParamId, ModifierStack>> for ParamModifiers {
    fn from(m: HashMap<ParamId, ModifierStack>) -> Self {
        Self(m)
    }
}

impl ParamModifiers {
    /// Whether there are no modifier stacks, for `skip_serializing_if`.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Read and edit methods for each keyframe lane, named after its field.
macro_rules! lanes {
    ($($field:ident, $edit:ident;)*) => {
        impl Clip {
            $(
                #[doc = concat!("The clip's `", stringify!($field), "`, in time order.")]
                pub fn $field(&self) -> &[Keyframe] {
                    &self.picture.view().$field.0
                }

                #[doc = concat!("Change the clip's `", stringify!($field), "` in `f`, and give back what `f` returns.")]
                pub fn $edit<R>(&mut self, f: impl FnOnce(&mut Vec<Keyframe>) -> R) -> R {
                    f(&mut self.view_mut().$field.0)
                }
            )*
        }
    };
}

lanes! {
    opacity_keyframes, edit_opacity_keyframes;
    transform_pos_x_keyframes, edit_transform_pos_x_keyframes;
    transform_pos_y_keyframes, edit_transform_pos_y_keyframes;
    transform_scale_keyframes, edit_transform_scale_keyframes;
    transform_rotation_keyframes, edit_transform_rotation_keyframes;
    transform_tilt_x_keyframes, edit_transform_tilt_x_keyframes;
    transform_tilt_y_keyframes, edit_transform_tilt_y_keyframes;
}

impl Clip {
    /// The per-effect keyframes, keyed `"{effect index}:{param}"`.
    pub fn effect_keyframes(&self) -> &HashMap<String, Vec<Keyframe>> {
        &self.picture.view().effect_keyframes.0
    }

    /// Change the per-effect keyframes in `f`, and give back what `f`
    /// returns. Prefer the effect-list API, which keeps them with their
    /// effect when it moves.
    pub fn edit_effect_keyframes<R>(&mut self, f: impl FnOnce(&mut HashMap<String, Vec<Keyframe>>) -> R) -> R {
        f(&mut self.view_mut().effect_keyframes.0)
    }

    /// The parameter modifier stacks: the picture's, and the sound's
    /// (`volume`, `pan`, `gain`), which stay with the clip.
    pub fn param_modifiers(&self) -> &HashMap<ParamId, ModifierStack> {
        &self.picture.view().param_modifiers.0
    }

    /// Change the parameter modifier stacks in `f`, and give back what `f`
    /// returns.
    pub fn edit_param_modifiers<R>(&mut self, f: impl FnOnce(&mut HashMap<ParamId, ModifierStack>) -> R) -> R {
        f(&mut self.view_mut().param_modifiers.0)
    }
}

impl Clip {
    /// The keyframes of the video parameter `param`: a placement or opacity
    /// lane, or a track-matte radius, which isn't picture animation and
    /// stays a plain field.
    pub fn video_param_keyframes(&self, param: klipp_core::VideoParam) -> &[Keyframe] {
        use klipp_core::VideoParam as P;
        match param {
            P::Opacity => &self.picture.view().opacity_keyframes.0,
            P::PositionX => &self.picture.view().transform_pos_x_keyframes.0,
            P::PositionY => &self.picture.view().transform_pos_y_keyframes.0,
            P::Scale => &self.picture.view().transform_scale_keyframes.0,
            P::Rotation => &self.picture.view().transform_rotation_keyframes.0,
            P::TiltX => &self.picture.view().transform_tilt_x_keyframes.0,
            P::TiltY => &self.picture.view().transform_tilt_y_keyframes.0,
            P::MatteFeather => &self.matte_feather_keyframes,
            P::MatteChoke => &self.matte_choke_keyframes,
        }
    }

    /// The value of the video parameter `param` where nothing animates it:
    /// the clip's placement and opacity as its picture holds them (the
    /// renderer's: the first Transform's place, the last enabled Opacity's
    /// value), or a track-matte radius. What a new key on its lane starts
    /// from.
    pub fn video_param_value(&self, param: klipp_core::VideoParam) -> f32 {
        use klipp_core::VideoParam as P;
        match param {
            P::MatteFeather => return self.matte_source.as_ref().map_or(0.0, |m| m.feather_px),
            P::MatteChoke => return self.matte_source.as_ref().map_or(0.0, |m| m.choke_px),
            _ => {}
        }
        let Ok(lifted) = self.picture_v2() else {
            return if param == P::Opacity || param == P::Scale { 1.0 } else { 0.0 };
        };
        let (p, opacity) = (&lifted.picture.placement, lifted.picture.opacity);
        match param {
            P::Opacity => opacity,
            P::PositionX => p.pos_x.unwrap_or(0.0),
            P::PositionY => p.pos_y.unwrap_or(0.0),
            P::Scale => p.scale.unwrap_or(1.0),
            P::Rotation => p.rotation.unwrap_or(0.0),
            P::TiltX => p.tilt_x.unwrap_or(0.0),
            P::TiltY => p.tilt_y.unwrap_or(0.0),
            P::MatteFeather | P::MatteChoke => 0.0,
        }
    }

    /// Change the keyframes of the video parameter `param` in `f`, and give
    /// back what `f` returns.
    pub fn edit_video_param_keyframes<R>(&mut self, param: klipp_core::VideoParam, f: impl FnOnce(&mut Vec<Keyframe>) -> R) -> R {
        use klipp_core::VideoParam as P;
        match param {
            P::MatteFeather => f(&mut self.matte_feather_keyframes),
            P::MatteChoke => f(&mut self.matte_choke_keyframes),
            _ => {
                let mut view = self.view_mut();
                f(match param {
                    P::Opacity => &mut view.opacity_keyframes.0,
                    P::PositionX => &mut view.transform_pos_x_keyframes.0,
                    P::PositionY => &mut view.transform_pos_y_keyframes.0,
                    P::Scale => &mut view.transform_scale_keyframes.0,
                    P::Rotation => &mut view.transform_rotation_keyframes.0,
                    P::TiltX => &mut view.transform_tilt_x_keyframes.0,
                    P::TiltY | P::MatteFeather | P::MatteChoke => &mut view.transform_tilt_y_keyframes.0,
                })
            }
        }
    }
}

impl Clip {
    /// The keys of parameter `param` of the effect at `index` of the
    /// clip's list: the old inspector's index commands' (until step 19);
    /// a reader by node reads [`Clip::node_key_state`].
    pub fn effect_param_keyframes(&self, index: usize, param: &str) -> &[Keyframe] {
        self.effect_keyframes().get(&format!("{index}:{param}")).map_or(&[], Vec::as_slice)
    }
}

/// A node's animated parameter, read by path from the clip's picture
/// (M3 step 3): its keys, its value where nothing animates it, and the
/// effect it belongs to (its saved tag), which names and ranges it.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeKeyState {
    pub keys: Vec<Keyframe>,
    pub value: Option<f32>,
    pub tag: Option<String>,
}

impl NodeKeyState {
    /// The parameter's value at clip time `t`: its keys', or its own.
    pub fn value_at(&self, t: f64) -> f32 {
        klipp_core::keyframe::evaluate(&self.keys, t, self.value.unwrap_or(0.0))
    }
}

impl Clip {
    /// The node of the clip's picture `path` names: a node of its top
    /// level, the clip's own (a spliced one isn't the clip's to edit), or
    /// one inside a group there (a grade node, in the grade group).
    fn picture_node<R>(&self, path: &klipp_graph_format::path::NodePath, f: impl FnOnce(&klipp_graph_format::Node) -> R) -> Option<R> {
        use klipp_graph_format::path::Root;
        if path.root != (Root::Clip { clip: self.id.0 }) {
            return None;
        }
        let lifted = self.picture_v2().ok()?;
        let (first, inner) = path.nodes.split_first()?;
        let mut node = lifted.picture.graph.nodes.iter().find(|n| n.id == *first && n.origin.is_none())?;
        for id in inner {
            node = node.group.as_ref()?.graph.nodes.iter().find(|n| n.id == *id)?;
        }
        Some(f(node))
    }

    /// The keys of the parameter `key` of the node at `path` in the clip's
    /// picture, its value where nothing animates it, and its effect's tag
    /// (M3 step 3): what the curve editor and keyframe selection read in
    /// place of the effect list's `"{index}:{param}"` keys. A grade node
    /// (or a merge lifted from the grade) keeps its keys on its own
    /// keyframe track (#1323), which is read for it. `None` when the path
    /// names no node of the clip's picture.
    pub fn node_key_state(&self, path: &klipp_graph_format::path::NodePath, key: &klipp_graph_format::ParamKey) -> Option<NodeKeyState> {
        self.picture_node(path, |n| {
            let keys = if matches!(n.type_id.as_str(), klipp_graph_format::types::LEGACY_NODE | klipp_graph_format::lifted::types::MERGE) {
                let tracks: Option<klipp_nodes::NodeKeyframeTracks> = n.legacy.as_ref().and_then(|l| l.keyframes.clone()).and_then(|k| serde_json::from_value(k).ok());
                tracks.and_then(|t| t.tracks.into_iter().find(|t| t.param_name == key.as_str())).map(|t| t.keyframes).unwrap_or_default()
            } else {
                n.anim.get(key).map(|a| a.keys.iter().filter_map(|k| serde_json::from_value::<Keyframe>(k.clone()).ok()).collect()).unwrap_or_default()
            };
            let value = match n.params.get(key) {
                Some(klipp_graph_format::ParamValue::Float(v)) => Some(*v),
                Some(klipp_graph_format::ParamValue::Int(v)) => Some(*v as f32),
                _ => None,
            };
            NodeKeyState { keys, value, tag: klipp_graph_lift::effect_params::tag(n).map(str::to_owned) }
        })
    }

    /// Every animated parameter of the clip's effects, by node path and
    /// key, in the order the picture runs them, with each effect's tag.
    pub fn animated_effect_keys(&self) -> Vec<(klipp_graph_format::path::NodePath, klipp_graph_format::ParamKey, String)> {
        use klipp_graph_format::path::{NodePath, Root};
        let Ok(lifted) = self.picture_v2() else { return Vec::new() };
        let graph = &lifted.picture.graph;
        // Back from the output along each node's primary input, as the
        // catalog names it (a merge's is its background).
        use klipp_graph_format::PortCatalog;
        let mut spine = Vec::new();
        let mut at = Some(graph.output);
        while let Some(id) = at.filter(|id| !spine.contains(id)) {
            spine.push(id);
            let primary = graph.nodes.iter().find(|n| n.id == id).and_then(|n| crate::RegistryCatalog.primary_input(n));
            at = primary.and_then(|p| graph.edges.iter().find(|e| e.to == (id, p.clone())).map(|e| e.from.0));
        }
        spine.reverse();
        let mut out = Vec::new();
        for id in spine {
            let Some(n) = graph.nodes.iter().find(|n| n.id == id && n.origin.is_none() && n.type_id.as_str() == klipp_graph_format::types::LEGACY_EFFECT) else { continue };
            let Some(tag) = klipp_graph_lift::effect_params::tag(n) else { continue };
            for (key, anim) in &n.anim {
                if !anim.keys.is_empty() {
                    out.push((NodePath::new(Root::Clip { clip: self.id.0 }, [n.id]), key.clone(), tag.to_owned()));
                }
            }
        }
        out
    }
}

impl Clip {
    /// The path of the clip's PLACEMENT node, through which its placement
    /// and its opacity are set as clip properties (M3 step 13): its keys
    /// are `klipp_graph_lift::clip_properties`'.
    pub fn placement_path(&self) -> Option<klipp_graph_format::path::NodePath> {
        use klipp_graph_format::path::{NodePath, Root};
        let lifted = self.picture_v2().ok()?;
        klipp_graph_lift::clip_properties::placement_node(&lifted.picture)?;
        // The stable address, whatever the node's id is now.
        Some(NodePath::new(Root::Clip { clip: self.id.0 }, [klipp_graph_lift::clip_properties::placement_id(self.id.0)]))
    }
}
