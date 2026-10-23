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
    pub transform_scale_y_keyframes: Vec<Keyframe>,
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
            transform_scale_y_keyframes: p.transform_scale_y_keyframes.into(),
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
        f(&mut self.view_mut().grade_versions)
    }

    /// Which grade version is in use, if one is.
    pub fn active_grade_version(&self) -> Option<usize> {
        self.picture.view().active_grade_version
    }

    /// Say which grade version is in use.
    pub fn set_active_grade_version(&mut self, version: Option<usize>) {
        self.view_mut().active_grade_version = version;
    }

    /// The clip's sound modifiers: its volume's, pan's and gain's stacks,
    /// which aren't picture animation (M4 step 12f). Until the v1 view
    /// goes, those of its view's modifiers.
    pub fn sound_modifiers(&self) -> std::collections::HashMap<klipp_core::ParamId, klipp_core::ModifierStack> {
        use klipp_core::ParamId as P;
        self.picture.view().param_modifiers.0.iter().filter(|(id, _)| matches!(id, P::Volume | P::Pan | P::Gain)).map(|(id, m)| (id.clone(), m.clone())).collect()
    }

    /// On a render copy carrying its track's effects, the clip's own
    /// effects and audio chain; `None` on the document.
    pub fn own_effects(&self) -> Option<&OwnEffects> {
        self.picture.view().own_effects.as_deref()
    }

    /// Set or clear the clip's own effects, which mark a render copy as
    /// carrying its track's effects (see [`crate::Sequence::bake_track_effects`]).
    pub fn set_own_effects(&mut self, own: Option<OwnEffects>) {
        self.view_mut().own_effects = own.map(Box::new);
    }

    /// On a render copy whose grade groups are baked in, change the clip's
    /// own grade in `f`; `None`, and `f` not run, on the document.
    pub fn edit_own_grade<R>(&mut self, f: impl FnOnce(&mut klipp_nodes::NodeGraph) -> R) -> Option<R> {
        self.view_mut().own_node_graph.as_deref_mut().map(f)
    }

    /// Whether the clip's picture holds a grade correction's mix or mask
    /// channel (M3 step 17a), which only it keeps among the clip's fields:
    /// a copy of the grade's attributes can't carry it.
    pub fn holds_grade_mix(&self) -> bool {
        self.picture.truth().is_some_and(|t| t.picture.grade_mix_used())
    }

    /// The grade as the renderer runs it: [`GradeAccess::grade`], with each
    /// correction's mix and mask channel set as its run setting
    /// (`correction_blend`) when the clip's picture holds them (M3 step
    /// 17a). Only a picture-truth clip's can; any other clip's grade is
    /// lent as it is. A copy, never the stored grade, so the settings
    /// stay with the picture that saves them.
    ///
    /// [`GradeAccess::grade`]: crate::GradeAccess::grade
    ///
    /// A matte into the Output's alpha (#898) is taken where the grade ends
    /// (`with_output_alpha_applied`), so the effects the renderer splices
    /// in before the Output come after it and move picture and alpha
    /// together.
    pub fn grade_to_run(&self) -> std::borrow::Cow<'_, klipp_nodes::NodeGraph> {
        let grade = self.grade_with_blends();
        match grade.with_output_alpha_applied() {
            Some(applied) => std::borrow::Cow::Owned(applied),
            None => grade,
        }
    }

    /// The grade with each correction's mix and mask channel set as its
    /// run setting, as [`Self::grade_to_run`] reads it, nothing added.
    fn grade_with_blends(&self) -> std::borrow::Cow<'_, klipp_nodes::NodeGraph> {
        use crate::GradeAccess;
        let Some(truth) = self.picture.truth().filter(|t| t.picture.grade_mix_used()) else { return std::borrow::Cow::Borrowed(self.grade()) };
        let mut run = self.grade().clone();
        let channel_key = klipp_graph_format::ParamKey::from("mask.channel");
        let grade = truth.picture.graph.nodes.iter().filter_map(|n| n.group.as_ref()).flat_map(|g| g.graph.nodes.iter());
        for n in grade.filter(|n| n.mix.is_some() || n.params.contains_key(&channel_key)) {
            let channel = match n.params.get(&channel_key) {
                Some(klipp_graph_format::ParamValue::Text(c)) => c.parse().unwrap_or(klipp_effects::mask_mix::MaskChannel::Red),
                _ => klipp_effects::mask_mix::MaskChannel::Red,
            };
            if let Some(node) = run.nodes.iter_mut().find(|m| m.id == n.id) {
                node.params.correction_blend = Some(klipp_nodes::params::EffectBlend { mix: n.mix.unwrap_or(1.0), channel });
            }
        }
        std::borrow::Cow::Owned(run)
    }

    /// The clip's grade as a version keeps it: its fields, each
    /// correction's mix and mask channel written in as its run setting
    /// (`correction_blend`), which the fields alone don't hold (#1322).
    pub fn grade_for_version(&self) -> klipp_nodes::NodeGraph {
        self.grade_with_blends().into_owned()
    }

    /// Version `n`'s corrections' mix and mask channel, as its grade holds
    /// them as run settings (#1322): what the file keeps in its body.
    pub fn version_blends(&self, n: usize) -> Vec<klipp_graph_lift::sequence_grade_mix::Blend> {
        use crate::GradeAccess;
        self.grade_versions().get(n).map(|v| crate::grade_roots::blends_of(v.grade())).unwrap_or_default()
    }

    /// Set version `n`'s corrections' mix and mask channel from `blends`,
    /// as a loader reads them from its body (#1322).
    pub fn set_version_blends(&mut self, n: usize, blends: &[klipp_graph_lift::sequence_grade_mix::Blend]) -> Result<(), String> {
        use crate::GradeAccess;
        let mut grade = self.grade_versions().get(n).map(|v| v.grade().clone()).ok_or_else(|| format!("no grade version {n}"))?;
        crate::grade_roots::set_blends(&mut grade, blends)?;
        self.edit_grade_versions(|vs| vs[n].set_grade(grade));
        Ok(())
    }

    /// Put `grade`, a version's as [`Self::grade_for_version`] gives it,
    /// in as the clip's grade (#1322): its fields without the run
    /// settings, and each correction's mix and mask channel set in the
    /// picture by path, those the picture held that `grade` doesn't
    /// cleared. Refused, and nothing changed, for one inside a compound,
    /// which the picture can't hold yet.
    pub fn set_grade_from_version(&mut self, grade: klipp_nodes::NodeGraph) -> Result<(), String> {
        use crate::GradeAccess;
        use klipp_graph_format::path::{NodePath, Root};
        use klipp_graph_lift::ops::GraphOp;
        let blends = crate::grade_roots::blends_of(&grade);
        if blends.iter().any(|b| !grade.nodes.iter().any(|n| n.id == b.node)) {
            return Err("a correction inside a compound doesn't keep a mix or mask channel yet".into());
        }
        let mut fields = grade;
        crate::grade_roots::set_blends(&mut fields, &[])?;
        // The grade nodes the picture gives a mix or channel now.
        let channel_key = klipp_graph_format::ParamKey::from("mask.channel");
        let held: Vec<klipp_nodes::NodeId> = self
            .picture
            .truth()
            .map(|t| t.picture.graph.nodes.iter().filter_map(|n| n.group.as_ref()).flat_map(|g| g.graph.nodes.iter()).filter(|n| n.mix.is_some() || n.params.contains_key(&channel_key)).map(|n| n.id).collect())
            .unwrap_or_default();
        self.set_grade(fields);
        let kept: Vec<klipp_nodes::NodeId> = self.grade().nodes.iter().map(|n| n.id).collect();
        let nodes: Vec<klipp_nodes::NodeId> = held.into_iter().chain(blends.iter().map(|b| b.node)).filter(|n| kept.contains(n)).collect();
        if nodes.is_empty() {
            return Ok(());
        }
        let picture = self.picture_v2()?;
        let group = picture.picture.graph.nodes.iter().find(|n| n.type_id.as_str() == klipp_graph_format::lifted::types::GROUP).map(|n| n.id).ok_or("the clip's picture has no grade group")?;
        let mut ops = Vec::new();
        let mut done = Vec::new();
        for node in nodes {
            if done.contains(&node) {
                continue;
            }
            done.push(node);
            let blend = blends.iter().find(|b| b.node == node);
            let path = NodePath::new(Root::Clip { clip: self.id.0 }, [group, node]);
            ops.push(GraphOp::SetMix { path: path.clone(), mix: blend.and_then(|b| b.mix) });
            ops.push(GraphOp::SetMaskChannel { path, channel: blend.and_then(|b| b.channel.clone()) });
        }
        self.apply_picture_ops(&ops).map(|_| ())
    }

    /// The clip's picture lifted to revision 2, with its grade versions'
    /// bodies: from the file if the clip hasn't changed since it was read,
    /// otherwise lifted once and kept until the picture next changes.
    /// Works on a shared clip; two readers get the same picture.
    /// On a picture-truth clip, its picture, which is the truth.
    pub fn picture_v2(&self) -> Result<std::sync::Arc<LiftedPicture>, String> {
        if let Some(truth) = self.picture.truth() {
            return Ok(truth.clone());
        }
        self.picture.lifted().get_or_lift(self.lift_key(), || self.lift_picture())
    }

    /// Whether the clip's picture is its truth: it holds what the clip's
    /// fields can't (an effect's mix or mask, a published grade port), and
    /// the fields are a view of it (M2 step 9c).
    pub fn is_picture_truth(&self) -> bool {
        self.picture.truth().is_some()
    }

    /// Take `stored`, the clip's picture as its file holds it, as the
    /// clip's lifted picture, if it is exactly what lifting the clip gives.
    /// If it isn't, nothing is kept and the difference is the error: a
    /// picture that disagrees with the clip must never be written back.
    ///
    /// A picture-truth clip's stored picture is its truth and is taken as
    /// it is: the fields the loader read are its view, made from it
    /// (`klipp_graph_lift::delift::view_of`), and a lift of them is not
    /// the picture, so there is nothing to compare it with.
    pub fn adopt_stored_picture(&mut self, stored: LiftedPicture) -> Result<(), String> {
        // The effects keep the ids the file saved them under (M3
        // proposal 6), not their slots' ids.
        let held = self.picture.effect_ids().clone();
        if let Some(ids) = stored_effect_ids(&stored.picture, self.builtin_effects().len()) {
            self.adopt_effect_ids(ids);
        }
        if stored.picture.is_truth() {
            self.picture.set_truth(Some(std::sync::Arc::new(stored)));
            return Ok(());
        }
        let lifted = self.lift_picture()?;
        if lifted != stored {
            *self.picture.effect_ids_mut() = held;
            let which = if lifted.picture != stored.picture { "picture" } else { "grade versions" };
            return Err(format!("the file's lifted {which} differs from the clip's own"));
        }
        self.picture.adopt(self.lift_key(), stored);
        Ok(())
    }

    /// Whether the clip holds its lifted picture now, from the file or a
    /// lift since its last change, lifted for this clip as it is.
    pub fn has_lifted_picture(&self) -> bool {
        self.picture.truth().is_some() || self.picture.lifted().holds(self.lift_key())
    }

    fn lift_key(&self) -> crate::picture_store::LiftKey {
        crate::picture_store::LiftKey { clip: self.id, kind: lift_kind_tag(&self.kind) }
    }

    pub(crate) fn lift_picture(&self) -> Result<LiftedPicture, String> {
        // As the file's writer builds the clip's record: the picture
        // fields through the one mapping, read back from text.
        let text = serde_json::to_string(&crate::picture_v1::ClipPictureV1::of(self)).map_err(|e| e.to_string())?;
        let mut record: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        record.insert("id".into(), self.id.0.to_string().into());
        record.insert("kind".into(), serde_json::json!({ "type": lift_kind_tag(&self.kind) }));
        // Each effect's node keeps the id the clip holds for it (M3
        // proposal 6); the record a file holds names none.
        record.insert("effect_ids".into(), self.effect_ids_for_picture().iter().map(|id| serde_json::Value::from(id.as_uuid().to_string())).collect());
        let (picture, versions) = klipp_graph_lift::revision2::lift_picture(&record)?;
        Ok(LiftedPicture { picture, versions })
    }
}

/// The ids `picture` gives the clip's built-in effects, in list order:
/// each effect node by the slot it holds. `None` unless every slot of the
/// `len` is held once.
fn stored_effect_ids(picture: &klipp_graph_format::lifted::ClipPicture, len: usize) -> Option<Vec<crate::EffectId>> {
    let mut ids = vec![None; len];
    for n in picture.graph.nodes.iter().filter(|n| n.type_id.as_str() != klipp_graph_format::types::LEGACY_OFX) {
        let Some(slot) = n.legacy.as_ref().and_then(|l| l.slot) else { continue };
        let at = ids.get_mut(slot as usize)?;
        if at.replace(crate::EffectId::of_node(n.id)).is_some() {
            return None;
        }
    }
    ids.into_iter().collect()
}

/// The kind's `type` as the file writes it, as far as the lift reads it.
pub(crate) fn lift_kind_tag(kind: &crate::ClipKind) -> &'static str {
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
        Keyframe { time_secs: 0.0, value, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }
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

    /// #2460: a write that reaches the fields of a picture-truth clip
    /// without its guard (`Clip::view_mut`) is refused, in a release build
    /// as in a debug one: the fields and the picture stay as they were,
    /// still agreeing, and the refusal is said.
    #[test]
    fn a_field_write_that_skips_the_guard_is_refused_in_every_build() {
        let mut clip = clip();
        let mut truth = (*clip.picture_v2().unwrap()).clone();
        truth.picture.requires.push(klipp_graph_format::lifted::PICTURE_TRUTH_CAPABILITY.into());
        let truth = std::sync::Arc::new(truth);
        clip.picture.set_truth(Some(truth.clone()));
        let before = clip.picture.view().active_grade_version;
        crate::take_picture_refusal();
        clip.picture.view_mut().active_grade_version = Some(7);
        assert_eq!(clip.picture.view().active_grade_version, before, "the write did not land in the fields");
        assert!(std::sync::Arc::ptr_eq(clip.picture.truth().unwrap(), &truth), "the picture is the same one");
        assert!(crate::take_picture_refusal().is_some_and(|why| why.contains("skipped its guard")), "and the refusal is said");
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
