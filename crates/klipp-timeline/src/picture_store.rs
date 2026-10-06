//! A clip's picture, as the clip stores it (#1089, phase 2b M1b).
//!
//! The picture's fields (its grade, effects, picture animation and grade
//! versions) are the in-memory truth through M1. Beside them sits the
//! picture lifted to revision 2, a cache: filled from the file on load,
//! or lifted on first use, and dropped by every change, which can only be
//! made through [`ClipPictureFields::view_mut`]. A clone shares the cache
//! until either side changes.

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
}

impl ClipPictureFields {
    /// The picture's fields, to read.
    pub(crate) fn view(&self) -> &PictureView {
        &self.view
    }

    /// The picture's fields, to change: the lifted picture no longer
    /// matches them, so it is dropped.
    pub(crate) fn view_mut(&mut self) -> &mut PictureView {
        self.lifted = Lifted::default();
        &mut self.view
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
        Self { view, lifted: Lifted::default() }
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
