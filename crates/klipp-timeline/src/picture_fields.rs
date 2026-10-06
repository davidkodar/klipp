//! The parts of a clip's picture as plain data, and the clip's methods for
//! the picture fields that had none (#1089, M1b 2a).
//!
//! [`ClipPictureFields`] is private inside, so a clip's picture can't be
//! changed one field at a time from outside klipp-timeline. Code that
//! builds a picture from scratch (load, import) fills a
//! [`ClipPictureParts`] and converts it.

use crate::{ClipPictureFields, Clip, GradeVersion, LiftedPicture, OwnEffects};
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
        crate::picture_store::PictureView::from(p).into()
    }
}

impl From<ClipPictureParts> for crate::picture_store::PictureView {
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
        &self.picture.view().grade_versions
    }

    /// Change the grade versions in `f`, and give back what `f` returns.
    pub fn edit_grade_versions<R>(&mut self, f: impl FnOnce(&mut Vec<GradeVersion>) -> R) -> R {
        f(&mut self.picture.view_mut().grade_versions)
    }

    /// Which grade version is in use, if one is.
    pub fn active_grade_version(&self) -> Option<usize> {
        self.picture.view().active_grade_version
    }

    /// Say which grade version is in use.
    pub fn set_active_grade_version(&mut self, version: Option<usize>) {
        self.picture.view_mut().active_grade_version = version;
    }

    /// On a render copy carrying its track's effects, the clip's own
    /// effects and audio chain; `None` on the document.
    pub fn own_effects(&self) -> Option<&OwnEffects> {
        self.picture.view().own_effects.as_deref()
    }

    /// Set or clear the clip's own effects, which mark a render copy as
    /// carrying its track's effects (see [`crate::Sequence::bake_track_effects`]).
    pub fn set_own_effects(&mut self, own: Option<OwnEffects>) {
        self.picture.view_mut().own_effects = own.map(Box::new);
    }

    /// On a render copy whose grade groups are baked in, change the clip's
    /// own grade in `f`; `None`, and `f` not run, on the document.
    pub fn edit_own_grade<R>(&mut self, f: impl FnOnce(&mut klipp_nodes::NodeGraph) -> R) -> Option<R> {
        self.picture.view_mut().own_node_graph.as_deref_mut().map(f)
    }

    /// The clip's picture lifted to revision 2, with its grade versions'
    /// bodies: from the file if the clip hasn't changed since it was read,
    /// otherwise lifted once and kept until the picture next changes.
    /// Works on a shared clip; two readers get the same picture.
    pub fn picture_v2(&self) -> Result<std::sync::Arc<LiftedPicture>, String> {
        self.picture.lifted().get_or_lift(self.lift_key(), || self.lift_picture())
    }

    /// Take `stored`, the clip's picture as its file holds it, as the
    /// clip's lifted picture, if it is exactly what lifting the clip gives.
    /// If it isn't, nothing is kept and the difference is the error: a
    /// picture that disagrees with the clip must never be written back.
    pub fn adopt_stored_picture(&mut self, stored: LiftedPicture) -> Result<(), String> {
        let lifted = self.lift_picture()?;
        if lifted != stored {
            let which = if lifted.picture != stored.picture { "picture" } else { "grade versions" };
            return Err(format!("the file's lifted {which} differs from the clip's own"));
        }
        self.picture.adopt(self.lift_key(), stored);
        Ok(())
    }

    /// Whether the clip holds its lifted picture now, from the file or a
    /// lift since its last change, lifted for this clip as it is.
    pub fn has_lifted_picture(&self) -> bool {
        self.picture.lifted().holds(self.lift_key())
    }

    fn lift_key(&self) -> crate::picture_store::LiftKey {
        crate::picture_store::LiftKey { clip: self.id, kind: lift_kind_tag(&self.kind) }
    }

    fn lift_picture(&self) -> Result<LiftedPicture, String> {
        // As the file's writer builds the clip's record: the picture
        // fields through the one mapping, read back from text.
        let text = serde_json::to_string(&crate::picture_v1::ClipPictureV1::of(self)).map_err(|e| e.to_string())?;
        let mut record: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        record.insert("id".into(), self.id.0.to_string().into());
        record.insert("kind".into(), serde_json::json!({ "type": lift_kind_tag(&self.kind) }));
        let (picture, versions) = klipp_graph_lift::revision2::lift_picture(&record)?;
        Ok(LiftedPicture { picture, versions })
    }
}

/// The kind's `type` as the file writes it, as far as the lift reads it.
fn lift_kind_tag(kind: &crate::ClipKind) -> &'static str {
    use crate::ClipKind as K;
    match kind {
        K::Media { .. } => "media",
        K::Title(_) => "title_v2",
        K::Gap => "gap",
        K::Sequence { .. } => "sequence",
        K::Generator(_) => "generator",
        K::AdjustmentLayer => "adjustment_layer",
        K::Precomposition { .. } => "precomposition",
        K::TemplatePrecomposition { .. } => "template_precomposition",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{Interpolation, MediaId, RationalTime};

    fn clip() -> Clip {
        let t = |n| RationalTime::from_frames(n, 24);
        Clip::media(MediaId::new(), t(0), t(48), t(0), t(48))
    }

    fn key(value: f32) -> Keyframe {
        Keyframe { time_secs: 0.0, value, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0) }
    }

    #[test]
    fn two_readers_on_one_clip_get_the_same_picture() {
        let clip = clip();
        let (a, b) = std::thread::scope(|s| {
            let a = s.spawn(|| clip.picture_v2().unwrap());
            let b = s.spawn(|| clip.picture_v2().unwrap());
            (a.join().unwrap(), b.join().unwrap())
        });
        assert!(std::sync::Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn a_change_drops_the_lifted_picture_and_the_next_read_lifts_the_change() {
        let mut clip = clip();
        let before = clip.picture_v2().unwrap();
        assert!(clip.has_lifted_picture());
        clip.edit_opacity_keyframes(|v| v.push(key(0.5)));
        assert!(!clip.has_lifted_picture(), "a change leaves no stale picture behind");
        let after = clip.picture_v2().unwrap();
        assert_ne!(before.picture, after.picture);
    }

    #[test]
    fn a_clone_that_changes_drops_only_its_own_lifted_picture() {
        let original = clip();
        let first = original.picture_v2().unwrap();
        let mut copy = original.clone();
        assert!(std::sync::Arc::ptr_eq(&first, &copy.picture_v2().unwrap()), "a clone shares it");
        copy.edit_opacity_keyframes(|v| v.push(key(0.5)));
        assert!(!copy.has_lifted_picture());
        assert!(original.has_lifted_picture(), "the original keeps its own");
        assert!(std::sync::Arc::ptr_eq(&first, &original.picture_v2().unwrap()));
    }

    #[test]
    fn a_stored_picture_is_kept_only_if_it_is_what_the_clip_lifts_to() {
        let source = clip();
        let stored = (*source.picture_v2().unwrap()).clone();
        let mut same = source.clone();
        same.edit_opacity_keyframes(|_| ()); // drops the shared one
        same.adopt_stored_picture(stored.clone()).unwrap();
        assert!(same.has_lifted_picture());
        let mut changed = source.clone();
        changed.edit_opacity_keyframes(|v| v.push(key(0.5)));
        assert!(changed.adopt_stored_picture(stored).is_err());
        assert!(!changed.has_lifted_picture(), "a picture that disagrees is never kept");
    }
}
