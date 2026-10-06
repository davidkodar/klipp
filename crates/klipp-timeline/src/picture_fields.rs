//! The parts of a clip's picture as plain data, and the clip's methods for
//! the picture fields that had none (#1089, M1b 2a).
//!
//! [`ClipPictureFields`] is private inside, so a clip's picture can't be
//! changed one field at a time from outside klipp-timeline. Code that
//! builds a picture from scratch (load, import) fills a
//! [`ClipPictureParts`] and converts it.

use crate::{ClipPictureFields, Clip, GradeVersion, OwnEffects};
use klipp_core::{Keyframe, ModifierStack, ParamId};
use std::collections::HashMap;

/// A clip's picture, field by field, as plain values.
#[derive(Debug, Default, Clone)]
pub struct ClipPictureParts {
    pub grade: klipp_nodes::NodeGraph,
    pub builtin_effects: Vec<klipp_effects::BuiltinEffect>,
    /// Parallel to `builtin_effects`; a missing entry means enabled.
    pub builtin_effects_enabled: Vec<bool>,
    pub ofx_effects: Vec<klipp_plugin::AppliedEffect>,
    pub opacity_keyframes: Vec<Keyframe>,
    pub transform_pos_x_keyframes: Vec<Keyframe>,
    pub transform_pos_y_keyframes: Vec<Keyframe>,
    pub transform_scale_keyframes: Vec<Keyframe>,
    pub transform_rotation_keyframes: Vec<Keyframe>,
    pub transform_tilt_x_keyframes: Vec<Keyframe>,
    pub transform_tilt_y_keyframes: Vec<Keyframe>,
    /// Keyed `"{effect index}:{param}"`.
    pub effect_keyframes: HashMap<String, Vec<Keyframe>>,
    pub param_modifiers: HashMap<ParamId, ModifierStack>,
    pub grade_versions: Vec<GradeVersion>,
    pub active_grade_version: Option<usize>,
}

impl From<ClipPictureParts> for ClipPictureFields {
    fn from(p: ClipPictureParts) -> Self {
        Self {
            node_graph: p.grade.into(),
            effects: p.ofx_effects.into(),
            effect_list: crate::EffectList::new(p.builtin_effects, p.builtin_effects_enabled),
            own_node_graph: None,
            own_effects: None,
            opacity_keyframes: p.opacity_keyframes.into(),
            transform_pos_x_keyframes: p.transform_pos_x_keyframes.into(),
            transform_pos_y_keyframes: p.transform_pos_y_keyframes.into(),
            transform_scale_keyframes: p.transform_scale_keyframes.into(),
            transform_rotation_keyframes: p.transform_rotation_keyframes.into(),
            transform_tilt_x_keyframes: p.transform_tilt_x_keyframes.into(),
            transform_tilt_y_keyframes: p.transform_tilt_y_keyframes.into(),
            effect_keyframes: p.effect_keyframes.into(),
            param_modifiers: p.param_modifiers.into(),
            grade_versions: p.grade_versions,
            active_grade_version: p.active_grade_version,
        }
    }
}

impl Clip {
    /// The clip's saved grade versions, in order.
    pub fn grade_versions(&self) -> &[GradeVersion] {
        &self.picture.grade_versions
    }

    /// Change the grade versions in `f`, and give back what `f` returns.
    pub fn edit_grade_versions<R>(&mut self, f: impl FnOnce(&mut Vec<GradeVersion>) -> R) -> R {
        f(&mut self.picture.grade_versions)
    }

    /// Which grade version is in use, if one is.
    pub fn active_grade_version(&self) -> Option<usize> {
        self.picture.active_grade_version
    }

    /// Say which grade version is in use.
    pub fn set_active_grade_version(&mut self, version: Option<usize>) {
        self.picture.active_grade_version = version;
    }

    /// On a render copy carrying its track's effects, the clip's own
    /// effects and audio chain; `None` on the document.
    pub fn own_effects(&self) -> Option<&OwnEffects> {
        self.picture.own_effects.as_deref()
    }

    /// Set or clear the clip's own effects, which mark a render copy as
    /// carrying its track's effects (see [`crate::Sequence::bake_track_effects`]).
    pub fn set_own_effects(&mut self, own: Option<OwnEffects>) {
        self.picture.own_effects = own.map(Box::new);
    }

    /// On a render copy whose grade groups are baked in, change the clip's
    /// own grade in `f`; `None`, and `f` not run, on the document.
    pub fn edit_own_grade<R>(&mut self, f: impl FnOnce(&mut klipp_nodes::NodeGraph) -> R) -> Option<R> {
        self.picture.own_node_graph.as_deref_mut().map(f)
    }
}
