//! Access to OFX effects and track picture effects. Their saved storage is
//! private so the storage can change without changing editing/render callers.

use klipp_effects::BuiltinEffect;
use klipp_plugin::AppliedEffect;
use serde::{Deserialize, Serialize};

use crate::{Clip, TrackEffects};

/// A clip's OFX list, saved as the original array.
///
/// ```
/// fn clear(clip: &mut klipp_timeline::Clip) {
///     clip.edit_ofx_effects(|effects| effects.clear());
///     assert!(clip.ofx_effects().is_empty());
/// }
/// ```
///
/// ```compile_fail
/// fn clear(clip: &mut klipp_timeline::Clip) {
///     clip.effects.0.clear();
/// }
/// ```
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OfxEffects(Vec<AppliedEffect>);

impl From<Vec<AppliedEffect>> for OfxEffects {
    fn from(effects: Vec<AppliedEffect>) -> Self {
        Self(effects)
    }
}

/// A track's built-in effects and their flags, saved under their original
/// `video` and `video_enabled` keys. Missing flags continue to mean enabled.
///
/// ```
/// fn clear(track: &mut klipp_timeline::TrackEffects) {
///     track.edit_video_effects(|effects, enabled| {
///         effects.clear();
///         enabled.clear();
///     });
///     assert!(track.video_effects().is_empty());
/// }
/// ```
///
/// ```compile_fail
/// fn clear(track: &mut klipp_timeline::TrackEffects) {
///     track.video_effect_list.video.clear();
/// }
/// ```
/// ```compile_fail
/// fn clear(track: &mut klipp_timeline::TrackEffects) {
///     track.video_effect_list.video_enabled.clear();
/// }
/// ```
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackEffectList {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    video: Vec<BuiltinEffect>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    video_enabled: Vec<bool>,
}

impl TrackEffectList {
    /// Construct from saved values without padding or trimming flags.
    pub fn new(video: Vec<BuiltinEffect>, enabled: Vec<bool>) -> Self {
        Self { video, video_enabled: enabled }
    }
}

impl Clip {
    pub fn ofx_effects(&self) -> &[AppliedEffect] {
        &self.picture.view().effects.0
    }

    /// Edit one entry without changing the list's order.
    pub fn edit_ofx_effect<R>(&mut self, index: usize, f: impl FnOnce(&mut AppliedEffect) -> R) -> Option<R> {
        self.view_mut().effects.0.get_mut(index).map(f)
    }

    /// Edit the list without changing its existing ordering or parameters.
    pub fn edit_ofx_effects<R>(&mut self, edit: impl FnOnce(&mut Vec<AppliedEffect>) -> R) -> R {
        edit(&mut self.view_mut().effects.0)
    }

    pub fn set_ofx_effects(&mut self, effects: Vec<AppliedEffect>) {
        self.view_mut().effects = effects.into();
    }
}

impl TrackEffects {
    pub fn video_effects(&self) -> &[BuiltinEffect] {
        &self.video_effect_list.video
    }

    /// The exact saved flags; absent entries mean enabled.
    pub fn video_effects_enabled(&self) -> &[bool] {
        &self.video_effect_list.video_enabled
    }

    /// Edit values and flags together. The caller retains its existing flag
    /// policy; this boundary does not normalize legacy short/surplus lists.
    pub fn edit_video_effects<R>(&mut self, edit: impl FnOnce(&mut Vec<BuiltinEffect>, &mut Vec<bool>) -> R) -> R {
        edit(&mut self.video_effect_list.video, &mut self.video_effect_list.video_enabled)
    }

    pub fn set_video_effect_list(&mut self, effects: Vec<BuiltinEffect>, enabled: Vec<bool>) {
        self.video_effect_list = TrackEffectList::new(effects, enabled);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Sequence;
    use klipp_core::Timecode;

    /// These full-owner fixtures were serialized before #1131. In particular,
    /// flattening the track store must not add a wrapper key, drop or add a
    /// field, or normalize the saved enabled flags. Compared as JSON values:
    /// since a clip's picture fields were gathered into one flattened struct
    /// (#1089, M1b 2a) they are written together, so the order of a clip's
    /// keys moved, which nothing reading this JSON depends on (the project
    /// file has its own writer; the render key sorts every map).
    #[test]
    fn legacy_owner_bytes_are_unchanged() {
        for (bytes, flags) in [
            (include_str!("../tests/fixtures/effect-access-v1/empty.json"), &[][..]),
            (include_str!("../tests/fixtures/effect-access-v1/missing-flags.json"), &[][..]),
            (include_str!("../tests/fixtures/effect-access-v1/short-flags.json"), &[false][..]),
            (include_str!("../tests/fixtures/effect-access-v1/surplus-flags.json"), &[false, true, false][..]),
        ] {
            let seq: Sequence = serde_json::from_str(bytes).unwrap();
            let written: serde_json::Value = serde_json::from_str(&serde_json::to_string(&seq).unwrap()).unwrap();
            assert_eq!(written, serde_json::from_str::<serde_json::Value>(bytes).unwrap());
            let track = &seq.tracks[0];
            assert_eq!(track.effects.video_effects_enabled(), flags);
            let clip = &track.clips[0];
            if !flags.is_empty() {
                assert_eq!(clip.ofx_effects().len(), 2);
                assert!(clip.ofx_effects()[0].enabled);
                assert!(!clip.ofx_effects()[1].enabled);
                assert_eq!(track.effects.video_effects().len(), 2);
            }
        }
    }

    #[test]
    fn ofx_edits_keep_order_parameters_and_closure_results() {
        let mut clip = Clip::gap(Timecode::from_frames(0, 24), Timecode::from_frames(24, 24));
        let a = AppliedEffect { plugin_name: "A".into(), enabled: true, ..Default::default() };
        let b = AppliedEffect { plugin_name: "B".into(), ..Default::default() };
        clip.set_ofx_effects(vec![a, b]);
        clip.edit_ofx_effect(0, |v| v.param_values.insert("amount".into(), klipp_plugin::OfxValue::Double(0.25))).unwrap();
        assert!(clip.edit_ofx_effect(2, |_| ()).is_none());
        let removed = clip.edit_ofx_effects(|effects| {
            effects.swap(0, 1);
            effects.remove(0)
        });
        assert_eq!(removed.plugin_name, "B");
        assert_eq!(clip.ofx_effects()[0].plugin_name, "A");
        assert!(matches!(clip.ofx_effects()[0].param_values["amount"], klipp_plugin::OfxValue::Double(v) if v == 0.25));
        assert!(clip.ofx_effects()[0].enabled);
    }

    #[test]
    fn track_edits_preserve_the_callers_flag_policy() {
        let effects = vec![BuiltinEffect::Opacity { value: 0.5 }, BuiltinEffect::GaussianBlur { radius: 2.0 }];
        for flags in [vec![], vec![false], vec![false, true, false]] {
            let mut track = TrackEffects::default();
            track.set_video_effect_list(effects.clone(), flags.clone());
            assert_eq!(track.video_effects(), effects);
            assert_eq!(track.video_effects_enabled(), flags);
            let got = track.edit_video_effects(|video, enabled| {
                video.remove(0);
                if !enabled.is_empty() {
                    enabled.remove(0);
                }
                video.len()
            });
            assert_eq!(got, 1);
            assert_eq!(track.video_effects_enabled(), flags.get(1..).unwrap_or_default());
        }
    }
}
