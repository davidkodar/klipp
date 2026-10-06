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
                    &self.$field.0
                }

                #[doc = concat!("Change the clip's `", stringify!($field), "` in `f`, and give back what `f` returns.")]
                pub fn $edit<R>(&mut self, f: impl FnOnce(&mut Vec<Keyframe>) -> R) -> R {
                    f(&mut self.$field.0)
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
        &self.effect_keyframes.0
    }

    /// Change the per-effect keyframes in `f`, and give back what `f`
    /// returns. Prefer the effect-list API, which keeps them with their
    /// effect when it moves.
    pub fn edit_effect_keyframes<R>(&mut self, f: impl FnOnce(&mut HashMap<String, Vec<Keyframe>>) -> R) -> R {
        f(&mut self.effect_keyframes.0)
    }

    /// The parameter modifier stacks: the picture's, and the sound's
    /// (`volume`, `pan`, `gain`), which stay with the clip.
    pub fn param_modifiers(&self) -> &HashMap<ParamId, ModifierStack> {
        &self.param_modifiers.0
    }

    /// Change the parameter modifier stacks in `f`, and give back what `f`
    /// returns.
    pub fn edit_param_modifiers<R>(&mut self, f: impl FnOnce(&mut HashMap<ParamId, ModifierStack>) -> R) -> R {
        f(&mut self.param_modifiers.0)
    }
}

impl Clip {
    /// The keyframes of the video parameter `param`: a placement or opacity
    /// lane, or a track-matte radius, which isn't picture animation and
    /// stays a plain field.
    pub fn video_param_keyframes(&self, param: klipp_core::VideoParam) -> &[Keyframe] {
        use klipp_core::VideoParam as P;
        match param {
            P::Opacity => &self.opacity_keyframes.0,
            P::PositionX => &self.transform_pos_x_keyframes.0,
            P::PositionY => &self.transform_pos_y_keyframes.0,
            P::Scale => &self.transform_scale_keyframes.0,
            P::Rotation => &self.transform_rotation_keyframes.0,
            P::TiltX => &self.transform_tilt_x_keyframes.0,
            P::TiltY => &self.transform_tilt_y_keyframes.0,
            P::MatteFeather => &self.matte_feather_keyframes,
            P::MatteChoke => &self.matte_choke_keyframes,
        }
    }

    /// Change the keyframes of the video parameter `param` in `f`, and give
    /// back what `f` returns.
    pub fn edit_video_param_keyframes<R>(&mut self, param: klipp_core::VideoParam, f: impl FnOnce(&mut Vec<Keyframe>) -> R) -> R {
        use klipp_core::VideoParam as P;
        f(match param {
            P::Opacity => &mut self.opacity_keyframes.0,
            P::PositionX => &mut self.transform_pos_x_keyframes.0,
            P::PositionY => &mut self.transform_pos_y_keyframes.0,
            P::Scale => &mut self.transform_scale_keyframes.0,
            P::Rotation => &mut self.transform_rotation_keyframes.0,
            P::TiltX => &mut self.transform_tilt_x_keyframes.0,
            P::TiltY => &mut self.transform_tilt_y_keyframes.0,
            P::MatteFeather => &mut self.matte_feather_keyframes,
            P::MatteChoke => &mut self.matte_choke_keyframes,
        })
    }
}
