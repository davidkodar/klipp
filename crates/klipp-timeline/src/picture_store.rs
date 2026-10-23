//! A clip's picture, as the clip stores it (#1089, phase 2b M1b).
//!
//! The picture's fields (its grade, effects, picture animation and grade
//! versions) are the in-memory truth through M1. Beside them sits the
//! picture lifted to revision 2, a cache: filled from the file on load,
//! or lifted on first use, and dropped by every change, which can only be
//! made through [`ClipPictureFields::view_mut`]. A clone shares the cache
//! until either side changes.
//!
//! A picture-truth clip (M2 step 9c) is the exception: its picture holds
//! what the fields can't (an effect's mix or mask, a published grade
//! port), so the picture is the truth and the fields are a view derived
//! from it, as far as they can hold it. The picture is replaced whole by
//! each edit, never changed in place, so clones stay apart. No reader may
//! write the view back on such a clip: an edit goes to the picture as
//! graph ops ([`crate::Clip::apply_picture_ops`]), or through the guard for
//! the writes that aren't ops yet (`Clip::view_mut`, `Clip::through_truth`).

use serde::{Deserialize, Serialize};
use crate::{GradeVersion, OwnEffects};
use std::sync::{Arc, OnceLock};

/// A clip's picture fields: private, read and changed through [`crate::Clip`]'s
/// methods. Saved flat into the clip, as the fields were before they were
/// gathered here.
///
/// ```compile_fail,E0616
/// fn replace(clip: &mut klipp_timeline::Clip, other: &klipp_timeline::Clip) {
///     clip.picture.view = other.picture.view.clone();
/// }
/// ```
///
/// Replacing the whole picture, which keeps its parts and its cache
/// together, does compile.
///
/// ```
/// fn replace(clip: &mut klipp_timeline::Clip, other: &klipp_timeline::Clip) {
///     clip.picture = other.picture.clone();
/// }
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClipPictureFields {
    #[serde(flatten)]
    view: PictureView,
    /// The picture lifted to revision 2; never saved with the clip's own
    /// serde, which is the view's.
    #[serde(skip)]
    lifted: Lifted,
    /// The built-in effects' ids for this session: not picture data, so
    /// keeping them current leaves the lifted picture as it is.
    #[serde(skip)]
    effect_ids: crate::effect_slots::EffectIds,
    /// A picture-truth clip's picture, which the view is derived from;
    /// `None` for a clip whose fields are the truth. Saved as the clip's
    /// stored picture, not with the clip's own serde.
    #[serde(skip)]
    truth: Option<Arc<LiftedPicture>>,
    /// Where a write that skipped the guard on a picture-truth clip goes,
    /// to be dropped: never the view, which the picture is (#2460).
    #[serde(skip)]
    stray: Option<Box<Self>>,
}

impl ClipPictureFields {
    /// The picture's fields, to read.
    pub(crate) fn view(&self) -> &PictureView {
        &self.view
    }

    /// The picture's fields, to change: the lifted picture no longer
    /// matches them, so it is dropped.
    ///
    /// On a picture-truth clip the fields are a view of the picture, so a
    /// write that reaches here is a writer that skipped the guard
    /// (`Clip::view_mut`): it is refused, in every build alike (#2460).
    /// It lands on a copy that is dropped, the view and the picture stay
    /// as they were and agree, and why is kept for
    /// [`crate::take_picture_refusal`]. A release build used to let the
    /// write land in the view, apart from the picture.
    pub(crate) fn view_mut(&mut self) -> &mut PictureView {
        if self.truth.is_some() {
            tracing::warn!("a field write on a picture-truth clip skipped the guard; refused, the clip's picture is kept");
            crate::picture_ops::refuse_picture_write("a write to a clip's fields that skipped its guard was not done".into());
            let stray = self.stray.insert(Box::new(Self::from(self.view.clone())));
            return &mut stray.view;
        }
        self.lifted = Lifted::default();
        &mut self.view
    }

    /// The view, written without its guard, for a test that sets a
    /// picture-truth clip's fields apart from its picture
    /// ([`crate::Clip::diverge_fields_for_test`]).
    #[cfg(any(test, feature = "test-clip"))]
    pub(crate) fn view_unguarded(&mut self) -> &mut PictureView {
        self.lifted = Lifted::default();
        &mut self.view
    }

    /// The picture-truth clip's picture, if this is one.
    pub(crate) fn truth(&self) -> Option<&Arc<LiftedPicture>> {
        self.truth.as_ref()
    }

    /// Take the picture-truth clip's picture out, so its view can be
    /// written as the picture's derivation; put back with [`Self::set_truth`].
    pub(crate) fn take_truth(&mut self) -> Option<Arc<LiftedPicture>> {
        self.truth.take()
    }

    /// Make `picture` this clip's truth (or none). The lifted cache goes:
    /// the truth is what a reader of the lifted picture gets.
    pub(crate) fn set_truth(&mut self, picture: Option<Arc<LiftedPicture>>) {
        self.lifted = Lifted::default();
        self.truth = picture;
    }

    pub(crate) fn effect_ids(&self) -> &crate::effect_slots::EffectIds {
        &self.effect_ids
    }

    pub(crate) fn effect_ids_mut(&mut self) -> &mut crate::effect_slots::EffectIds {
        &mut self.effect_ids
    }

    pub(crate) fn lifted(&self) -> &Lifted {
        &self.lifted
    }

    /// Keep `picture` as the lifted picture; the caller has checked it is
    /// what lifting the view gives.
    pub(crate) fn adopt(&mut self, key: LiftKey, picture: LiftedPicture) {
        self.lifted = Lifted::holding(key, picture);
    }
}

impl From<PictureView> for ClipPictureFields {
    fn from(view: PictureView) -> Self {
        Self { view, lifted: Lifted::default(), effect_ids: Default::default(), truth: None, stray: None }
    }
}

/// The picture's fields as M1 keeps them: the in-memory truth.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct PictureView {
    #[serde(default)]
    pub(crate) node_graph: crate::Grade,
    #[serde(default)]
    pub(crate) effects: crate::OfxEffects,
    /// Built-in effect storage; use the effect-list API to read or edit it.
    /// Flattening preserves the existing top-level saved keys.
    #[serde(flatten)]
    pub(crate) effect_list: crate::EffectList,
    /// On a render copy whose grade groups are baked in (see
    /// [`Sequence::bake_grade_groups`]), this clip's own grade, with
    /// `node_graph` its grade inside its group's. Never saved, and `None`
    /// on the document. What reads the render copy to describe or change
    /// a clip's grade reads [`Clip::own_grade`], or a group's nodes would
    /// be copied into the clip's own grade.
    #[serde(skip)]
    pub(crate) own_node_graph: Option<Box<klipp_nodes::NodeGraph>>,
    /// On a render copy carrying its track's effects (see
    /// [`Sequence::bake_track_effects`]), this clip's own effects and
    /// audio chain. Never saved, `None` on the document; `Some` marks the
    /// clip as carrying them, so they are never put on twice.
    #[serde(skip)]
    pub(crate) own_effects: Option<Box<OwnEffects>>,
    #[serde(default)]
    pub(crate) opacity_keyframes: crate::Keyframes,
    #[serde(default)]
    pub(crate) transform_pos_x_keyframes: crate::Keyframes,
    #[serde(default)]
    pub(crate) transform_pos_y_keyframes: crate::Keyframes,
    #[serde(default)]
    pub(crate) transform_scale_keyframes: crate::Keyframes,
    /// The height's scale keyed apart from the width's (#1471). Empty for
    /// a height that follows the width at the clip's aspect.
    #[serde(default, skip_serializing_if = "crate::Keyframes::is_empty")]
    pub(crate) transform_scale_y_keyframes: crate::Keyframes,
    #[serde(default)]
    pub(crate) transform_rotation_keyframes: crate::Keyframes,
    /// Tilt about the picture's horizontal axis, in degrees, in
    /// perspective: the card leans back or forward like a screen on a
    /// desk. Empty for a flat picture.
    #[serde(default, skip_serializing_if = "crate::Keyframes::is_empty")]
    pub(crate) transform_tilt_x_keyframes: crate::Keyframes,
    /// Tilt about the picture's vertical axis, in degrees, in
    /// perspective: the card swings like a door.
    #[serde(default, skip_serializing_if = "crate::Keyframes::is_empty")]
    pub(crate) transform_tilt_y_keyframes: crate::Keyframes,
    /// Per-effect keyframes: maps "{effect_index}:{param_name}" to keyframe track.
    #[serde(default)]
    pub(crate) effect_keyframes: crate::EffectKeyframes,
    /// Per-parameter modifier stacks (behaviors like Oscillate, Wiggle, etc.)
    #[serde(default, skip_serializing_if = "crate::ParamModifiers::is_empty")]
    pub(crate) param_modifiers: crate::ParamModifiers,
    /// Named grade versions. Each is a complete NodeGraph snapshot.
    /// The active grade is always `node_graph` — versions are stored copies.
    #[serde(default)]
    pub(crate) grade_versions: Vec<GradeVersion>,
    /// Index of the currently active grade version in grade_versions (None = working grade, not saved as version yet).
    #[serde(default)]
    pub(crate) active_grade_version: Option<usize>,
}

/// A clip's picture in revision 2, with its grade versions' lifted
/// bodies in order.
#[derive(Debug, Clone, PartialEq)]
pub struct LiftedPicture {
    pub picture: klipp_graph_format::lifted::ClipPicture,
    pub versions: Vec<serde_json::Value>,
}

/// What a lifted picture was lifted for: the clip's id and its kind's
/// family, which the lift reads beside the view (derived node ids come
/// from the id). Neither is in the view, so a clone given a new id (a
/// split's half, a duplicate) or a new kind could otherwise read a
/// picture lifted for the clip it came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LiftKey {
    pub(crate) clip: klipp_core::ClipId,
    pub(crate) kind: &'static str,
}

/// Clip pictures lifted (a cold lift: serialised, parsed and lifted) in
/// this process. A warm read takes the cached picture and counts nothing,
/// so in playback with no edit this holds still (M2 step 10c).
pub static COLD_LIFTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The lifted picture, filled once; shared by clones until one changes.
#[derive(Clone, Default)]
pub(crate) struct Lifted(Arc<OnceLock<(LiftKey, Result<Arc<LiftedPicture>, String>)>>);

impl Lifted {
    /// A cache already holding `picture`, as read from the file.
    pub(crate) fn holding(key: LiftKey, picture: LiftedPicture) -> Self {
        let cell = OnceLock::new();
        let _ = cell.set((key, Ok(Arc::new(picture))));
        Self(Arc::new(cell))
    }

    /// The picture lifted for `key`: the one held if it was lifted for
    /// `key`, otherwise lifted now. One lifted for another clip is never
    /// returned; it is left as it is (the cell is filled once) and the
    /// lift is not kept, until the next change empties the cell.
    pub(crate) fn get_or_lift(&self, key: LiftKey, lift: impl FnOnce() -> Result<LiftedPicture, String>) -> Result<Arc<LiftedPicture>, String> {
        let lift = || {
            COLD_LIFTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            lift()
        };
        let mut lift = Some(lift);
        let (held, picture) = self.0.get_or_init(|| (key, (lift.take().unwrap())().map(Arc::new)));
        if *held == key {
            return picture.clone();
        }
        match lift.take() {
            Some(lift) => lift().map(Arc::new),
            None => unreachable!("the cell was filled by this call, for this key"),
        }
    }

    /// Whether it holds a picture lifted for `key`.
    pub(crate) fn holds(&self, key: LiftKey) -> bool {
        self.0.get().is_some_and(|(held, _)| *held == key)
    }
}

impl std::fmt::Debug for Lifted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.get().is_some() { "Lifted(filled)" } else { "Lifted(empty)" })
    }
}
