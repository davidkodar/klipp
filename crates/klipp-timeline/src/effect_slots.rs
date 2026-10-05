//! A clip's built-in effects are addressed by their place in the list:
//! the enabled flags run parallel to it, keyframes are keyed
//! `"{index}:{param}"` and modifiers by `ParamId::Effect { effect_index }`.
//! Removing or moving an effect has to carry all three along, or they
//! stay at the old place and animate whichever effect lands there.

use crate::sequence::Clip;
use klipp_core::ParamId;
use klipp_effects::BuiltinEffect;

impl Clip {
    /// Remove built-in effect `index` with its enabled flag, keyframes
    /// and modifiers, and move those of the effects after it down one
    /// place with them.
    pub fn remove_builtin_effect(&mut self, index: usize) -> Option<BuiltinEffect> {
        if index >= self.builtin_effects.len() {
            return None;
        }
        let effect = self.builtin_effects.remove(index);
        if index < self.builtin_effects_enabled.len() {
            self.builtin_effects_enabled.remove(index);
        }
        self.remap_effect_slots(|i| match i.cmp(&index) {
            std::cmp::Ordering::Less => Some(i),
            std::cmp::Ordering::Equal => None,
            std::cmp::Ordering::Greater => Some(i - 1),
        });
        Some(effect)
    }

    /// Move built-in effect `from` to place `to`, its enabled flag,
    /// keyframes and modifiers with it. False when either place is out
    /// of range or they are the same.
    pub fn move_builtin_effect(&mut self, from: usize, to: usize) -> bool {
        let n = self.builtin_effects.len();
        if from >= n || to >= n || from == to {
            return false;
        }
        let effect = self.builtin_effects.remove(from);
        self.builtin_effects.insert(to, effect);
        // A missing flag reads as enabled; fill them in so the moved
        // effect's flag has a place to go.
        if self.builtin_effects_enabled.len() < n {
            self.builtin_effects_enabled.resize(n, true);
        }
        let on = self.builtin_effects_enabled.remove(from);
        self.builtin_effects_enabled.insert(to, on);
        self.remap_effect_slots(|i| Some(moved_place(i, from, to)));
        true
    }

    /// Re-key effect keyframes and modifiers by `map` (old place to new,
    /// `None` to drop them). Keys that do not name a place are kept.
    fn remap_effect_slots(&mut self, map: impl Fn(usize) -> Option<usize>) {
        self.effect_keyframes = std::mem::take(&mut self.effect_keyframes)
            .into_iter()
            .filter_map(|(key, kfs)| match key.split_once(':').and_then(|(i, name)| Some((i.parse::<usize>().ok()?, name))) {
                Some((i, name)) => map(i).map(|j| (format!("{j}:{name}"), kfs)),
                None => Some((key, kfs)),
            })
            .collect();
        self.param_modifiers = std::mem::take(&mut self.param_modifiers)
            .into_iter()
            .filter_map(|(id, stack)| match id {
                ParamId::Effect { effect_index, param_name } => {
                    map(effect_index).map(|j| (ParamId::Effect { effect_index: j, param_name }, stack))
                }
                other => Some((other, stack)),
            })
            .collect();
    }
}

/// Where the effect at place `i` ends up when the one at `from` moves to `to`.
fn moved_place(i: usize, from: usize, to: usize) -> usize {
    if i == from {
        to
    } else if from < to && i > from && i <= to {
        i - 1
    } else if to < from && i >= to && i < from {
        i + 1
    } else {
        i
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::{Interpolation, Keyframe, MediaId, ModifierStack, RationalTime};

    fn clip() -> Clip {
        let tc = |f| RationalTime::from_frames(f, 24);
        let mut c = Clip::media(MediaId::new(), tc(0), tc(48), tc(0), tc(48));
        c.builtin_effects = vec![
            BuiltinEffect::GaussianBlur { radius: 1.0 },
            BuiltinEffect::Sharpen { amount: 2.0 },
            BuiltinEffect::Vibrance { amount: 3.0 },
        ];
        c.builtin_effects_enabled = vec![true, false, true];
        for (i, name) in [(0, "radius"), (1, "amount"), (2, "amount")] {
            c.effect_keyframes.insert(format!("{i}:{name}"), vec![Keyframe { time_secs: 0.0, value: i as f32 * 10.0, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0) }]);
            c.param_modifiers.insert(
                ParamId::Effect { effect_index: i, param_name: name.into() },
                ModifierStack::default(),
            );
        }
        c.param_modifiers.insert(ParamId::Opacity, ModifierStack::default());
        c
    }

    /// The keyframe value each effect's own parameter carries, by place.
    fn keyed(c: &Clip) -> Vec<Option<f32>> {
        c.builtin_effects
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let name = match e {
                    BuiltinEffect::GaussianBlur { .. } => "radius",
                    _ => "amount",
                };
                c.effect_keyframes.get(&format!("{i}:{name}")).map(|k| k[0].value)
            })
            .collect()
    }

    fn modified(c: &Clip) -> Vec<usize> {
        let mut v: Vec<usize> = c
            .param_modifiers
            .keys()
            .filter_map(|p| match p {
                ParamId::Effect { effect_index, .. } => Some(*effect_index),
                _ => None,
            })
            .collect();
        v.sort();
        v
    }

    #[test]
    fn removing_an_effect_takes_its_animation_and_keeps_the_rest_with_theirs() {
        let mut c = clip();
        assert!(matches!(c.remove_builtin_effect(0), Some(BuiltinEffect::GaussianBlur { .. })));
        assert_eq!(c.builtin_effects_enabled, vec![false, true]);
        assert_eq!(keyed(&c), vec![Some(10.0), Some(20.0)]);
        assert_eq!(c.effect_keyframes.len(), 2, "the removed effect's keyframes stayed");
        assert_eq!(modified(&c), vec![0, 1]);
        assert!(c.param_modifiers.contains_key(&ParamId::Opacity), "a clip modifier was dropped");
        assert_eq!(c.remove_builtin_effect(5), None);
    }

    #[test]
    fn moving_an_effect_carries_its_flag_keyframes_and_modifiers() {
        for (from, to) in [(0, 2), (2, 0), (1, 0), (0, 1)] {
            let before = clip();
            let mut c = before.clone();
            assert!(c.move_builtin_effect(from, to));
            let mut order: Vec<usize> = (0..3).collect();
            let moved = order.remove(from);
            order.insert(to, moved);
            for (now, &was) in order.iter().enumerate() {
                assert_eq!(c.builtin_effects[now], before.builtin_effects[was]);
                assert_eq!(c.builtin_effects_enabled[now], before.builtin_effects_enabled[was], "{from}->{to}");
                assert_eq!(keyed(&c)[now], keyed(&before)[was], "{from}->{to}: keyframes left behind");
            }
            assert_eq!(modified(&c), vec![0, 1, 2]);
            // And back again restores the clip exactly.
            assert!(c.move_builtin_effect(to, from));
            assert_eq!(c.effect_keyframes, before.effect_keyframes);
            assert_eq!(modified(&c), modified(&before));
        }
        let mut c = clip();
        assert!(!c.move_builtin_effect(1, 1));
        assert!(!c.move_builtin_effect(0, 3));
    }

    #[test]
    fn a_missing_enabled_flag_still_moves_as_enabled() {
        let mut c = clip();
        c.builtin_effects_enabled = vec![false];
        assert!(c.move_builtin_effect(0, 2));
        assert_eq!(c.builtin_effects_enabled, vec![true, true, false]);
    }
}
