//! Clips for tests, built through the clip's picture (M4 step 12-0): the
//! one builder every crate's tests use, so the v1 view's accessors are
//! called from here and nowhere in a test.
//!
//! A clip is built as an edit would make it: its plain effects through
//! picture ops (`apply_effect_edits`), its keys and lanes through graph
//! ops on their nodes. Two kinds of call to the v1 view remain, here only
//! and pinned by the ratchet (`tests/v1_view_ratchet.rs`):
//!
//! - a list holding a kind the clip places itself (Transform, Opacity,
//!   Speed Ramp, Temporal Noise Reduction) is written whole
//!   (`set_effect_list`): no op adds one, disables one or puts one at a
//!   place yet (step 12f gives them ops);
//! - the readers read the list back from the picture by unlifting it
//!   (`unlift`): the list the picture holds, which is what a test checks.
//!
//! Built only for tests: other crates enable the `test-clip` feature from
//! their `[dev-dependencies]`, never their `[dependencies]` (a scan holds
//! that, `tests/test_clip_feature.rs`).

use klipp_core::{Interpolation, Keyframe, MediaId, RationalTime};
use klipp_effects::BuiltinEffect;
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_format::ParamKey;
use klipp_graph_lift::ops::GraphOp;

use crate::{Clip, EffectEdit, EffectId};

/// What a built clip's keys are put on.
enum Keyed {
    /// The effect at a place in its list, by parameter.
    Effect(usize, String),
    /// A clip property through the placement node (`opacity`, `pos_x`,
    /// `pos_y`, `scale`, `rotation`, `tilt_x`, `tilt_y`, ...: the keys
    /// `klipp_graph_lift::clip_properties` names).
    Lane(String),
}

/// A clip being built: its effects, in list order, each on or off, and
/// its keys.
pub struct TestClip {
    clip: Clip,
    effects: Vec<(BuiltinEffect, bool)>,
    keys: Vec<(Keyed, f64, f32)>,
    ofx: Vec<klipp_plugin::AppliedEffect>,
}

fn tc(secs: f64) -> RationalTime {
    RationalTime::from_secs_f64(secs, 24)
}

impl TestClip {
    /// A media clip of `secs` seconds at 24 fps, its source from 0.
    pub fn media(secs: f64) -> Self {
        Self::of(Clip::media(MediaId::new(), tc(0.0), tc(secs), tc(0.0), tc(secs)))
    }

    /// `clip` (any kind, placed anywhere), its effects to be added.
    pub fn of(clip: Clip) -> Self {
        Self { clip, effects: Vec::new(), keys: Vec::new(), ofx: Vec::new() }
    }

    /// `effect` added, on.
    pub fn effect(mut self, effect: BuiltinEffect) -> Self {
        self.effects.push((effect, true));
        self
    }

    /// Each of `effects` added, on, in order.
    pub fn effects(mut self, effects: impl IntoIterator<Item = BuiltinEffect>) -> Self {
        self.effects.extend(effects.into_iter().map(|e| (e, true)));
        self
    }

    /// `entry`, an OFX plug-in, put on after the plug-ins asked for before
    /// it. The clip's plug-ins are these alone: a clip given any has none
    /// of its own kept.
    pub fn plugin(mut self, entry: klipp_plugin::AppliedEffect) -> Self {
        self.ofx.push(entry);
        self
    }

    /// The effect added last, off.
    pub fn off(mut self) -> Self {
        if let Some(last) = self.effects.last_mut() {
            last.1 = false;
        }
        self
    }

    /// A linear key of `param` of the effect at `index`, at `secs` into
    /// the clip.
    pub fn key(mut self, index: usize, param: &str, secs: f64, value: f32) -> Self {
        self.keys.push((Keyed::Effect(index, param.into()), secs, value));
        self
    }

    /// A linear key of the clip property `param` (`opacity`, `pos_x`, ...).
    pub fn lane(mut self, param: &str, secs: f64, value: f32) -> Self {
        self.keys.push((Keyed::Lane(param.into()), secs, value));
        self
    }

    /// The clip, its picture holding what was asked. Panics, naming it,
    /// on what the picture refuses: a test's fixture is wrong.
    pub fn build(self) -> Clip {
        let mut clip = self.clip;
        if self.effects.iter().all(|(e, _)| crate::takes_picture_ops(e)) {
            // Added after the effects the clip already has: a place in the
            // list counts them too.
            let had = clip.effect_ids_for_picture().len();
            let mut edits: Vec<EffectEdit> = self.effects.iter().map(|(e, _)| EffectEdit::Add(e)).collect();
            edits.extend(self.effects.iter().enumerate().filter(|(_, (_, on))| !on).map(|(i, _)| EffectEdit::SetEnabled(had + i, false)));
            if !edits.is_empty() {
                clip.apply_effect_edits(&edits).expect("plain effects take picture ops").unwrap_or_else(|e| panic!("the picture refuses the effects: {e}"));
            }
        } else {
            // A kind the clip places itself: no op puts one in a list yet
            // (step 12f). The builder's one write through the v1 view,
            // after the effects the clip already has.
            let (mut effects, mut on) = listed(&clip);
            for (e, flag) in self.effects {
                effects.push(e);
                on.push(flag);
            }
            clip.set_effect_list(effects, on);
        }
        if !self.ofx.is_empty() {
            // No op puts a plug-in on yet (step 12b-0 adds them): the
            // builder's one write of the plug-in list.
            clip.set_ofx_effects(self.ofx);
        }
        for (keyed, secs, value) in self.keys {
            let keyframe = serde_json::to_value(Keyframe { time_secs: secs, value, interpolation: Interpolation::Linear, tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0), handles: Default::default() }).expect("a keyframe serialises");
            let (path, key) = match keyed {
                Keyed::Effect(index, param) => {
                    let node = clip.effect_node_at(false, index).unwrap_or_else(|| panic!("no effect at {index} to key"));
                    (NodePath::new(Root::Clip { clip: clip.id.0 }, [node]), param)
                }
                Keyed::Lane(param) => (clip.placement_path().expect("a placement node"), param),
            };
            let what = format!("{key} at {secs}");
            clip.apply_picture_ops(&[GraphOp::AddKeyAt { path, key: ParamKey::from(key.as_str()), keyframe }]).unwrap_or_else(|e| panic!("the picture refuses the key {what}: {e}"));
        }
        clip
    }
}

/// The clip's built-in effect list and its flags, as its picture holds
/// them: read back by unlifting the picture (the builder's one read
/// through the v1 form).
fn listed(clip: &Clip) -> (Vec<BuiltinEffect>, Vec<bool>) {
    let lifted = clip.picture_v2().expect("the clip's picture");
    let v1 = klipp_graph_lift::unlift::clip(&lifted.picture).expect("the picture reads back");
    // A clip with none reads back null for both, which is none.
    let field = |name: &str| v1.get(name).filter(|v| !v.is_null()).cloned().unwrap_or_else(|| serde_json::json!([]));
    let effects: Vec<BuiltinEffect> = serde_json::from_value(field("builtin_effects")).expect("the effects read back");
    let stored: Vec<bool> = serde_json::from_value(field("builtin_effects_enabled")).unwrap_or_default();
    // A short flag list means on for the rest.
    let on = (0..effects.len()).map(|i| stored.get(i).copied().unwrap_or(true)).collect();
    (effects, on)
}

/// The clip's OFX plug-ins, as its list holds them.
pub fn plugins(clip: &Clip) -> Vec<klipp_plugin::AppliedEffect> {
    clip.ofx_effects().to_vec()
}

/// The clip's effect keys by `"{slot}:{param}"`, as its list holds them,
/// orphans (a slot past the list) included.
pub fn effect_keys(clip: &Clip) -> std::collections::HashMap<String, Vec<Keyframe>> {
    clip.effect_keyframes().clone()
}

/// Keys on the list place `slot`'s `param`, written to the clip's list
/// form: the way to make an orphan (a slot past the list), which no node
/// holds and only that form can.
pub fn orphan_keys(clip: &mut Clip, slot: usize, param: &str, keys: Vec<Keyframe>) {
    clip.edit_effect_keyframes(|k| {
        k.insert(format!("{slot}:{param}"), keys);
    });
}

/// The clip's built-in effects, in list order, as its picture holds them.
pub fn effects(clip: &Clip) -> Vec<BuiltinEffect> {
    listed(clip).0
}

/// Each built-in effect's flag, in list order, as its picture holds it.
pub fn enabled(clip: &Clip) -> Vec<bool> {
    listed(clip).1
}

/// The id of the built-in effect at `index`, as the clip's picture names
/// its node.
pub fn effect_id(clip: &Clip, index: usize) -> EffectId {
    clip.effect_ids_for_picture().get(index).copied().unwrap_or_else(|| panic!("no effect at {index}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_effects::BuiltinEffect as E;

    fn transform(x: f32) -> E {
        E::Transform { position_x: x, position_y: 0.0, scale_x: 1.0, scale_y: 1.0, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None }
    }

    /// Built, then read back from the picture: the same list and flags.
    /// One case for each kind the clip places itself, beside plain ones.
    #[test]
    fn a_built_clip_reads_back_as_built() {
        let cases: Vec<Vec<(E, bool)>> = vec![
            vec![(E::GaussianBlur { radius: 2.0 }, true), (E::Vibrance { amount: 0.3 }, false)],
            vec![(E::Gamma { gamma: 1.1 }, true), (transform(5.0), false), (E::GaussianBlur { radius: 1.0 }, true)],
            vec![(E::Opacity { value: 0.9 }, true), (E::Opacity { value: 0.2 }, true), (E::Opacity { value: 0.6 }, false)],
            vec![(E::SpeedRamp { speed: 0.5, blend_mode: 1 }, true), (E::Gamma { gamma: 1.2 }, true)],
            vec![(E::TemporalDenoise { strength: 0.8, threshold: 0.1 }, true), (E::Gamma { gamma: 1.2 }, false)],
        ];
        for case in cases {
            let mut b = TestClip::media(2.0);
            for (e, on) in &case {
                b = b.effect(e.clone());
                if !on {
                    b = b.off();
                }
            }
            let clip = b.build();
            let want: Vec<E> = case.iter().map(|(e, _)| e.clone()).collect();
            let want_on: Vec<bool> = case.iter().map(|(_, on)| *on).collect();
            assert_eq!(effects(&clip), want, "{case:?}");
            assert_eq!(enabled(&clip), want_on, "{case:?}");
            assert_eq!((0..case.len()).map(|i| effect_id(&clip, i)).collect::<std::collections::HashSet<_>>().len(), case.len(), "each its own id");
        }
    }

    /// A clip that already has effects keeps them: what is added goes
    /// after them, a placed kind as well as a plain one.
    #[test]
    fn a_built_clip_keeps_the_effects_it_had() {
        let had = TestClip::media(2.0).effect(E::GaussianBlur { radius: 2.0 }).build();
        let plain = TestClip::of(had.clone()).effect(E::Gamma { gamma: 1.2 }).build();
        assert_eq!(effects(&plain), [E::GaussianBlur { radius: 2.0 }, E::Gamma { gamma: 1.2 }]);
        // Switched off by its own place, after the ones the clip had.
        let off = TestClip::of(had.clone()).effect(E::Gamma { gamma: 1.2 }).off().build();
        assert_eq!(enabled(&off), [true, false], "the new one off, not the one it had");
        let placed = TestClip::of(had).effect(transform(3.0)).off().build();
        assert_eq!(effects(&placed), [E::GaussianBlur { radius: 2.0 }, transform(3.0)]);
        assert_eq!(enabled(&placed), [true, false]);
    }

    /// Keys land on the effect and the clip property asked for, as the
    /// picture holds them.
    #[test]
    fn keys_land_where_asked() {
        let clip = TestClip::media(2.0).effect(E::GaussianBlur { radius: 2.0 }).key(0, "radius", 0.0, 1.0).key(0, "radius", 1.0, 4.0).lane("opacity", 0.5, 0.25).build();
        let node = clip.effect_node_at(false, 0).unwrap();
        let state = clip.node_key_state(&NodePath::new(Root::Clip { clip: clip.id.0 }, [node]), &ParamKey::from("radius")).expect("the blur's radius");
        assert_eq!(state.keys.len(), 2, "two keys on the radius");
        // The opacity is a clip property: its keys are the picture's own,
        // not the placement node's.
        let picture = clip.picture_v2().expect("the clip's picture").picture.clone();
        assert_eq!(picture.opacity_anim.map_or(0, |a| a.keys.len()), 1, "one key on the opacity");
    }
}
