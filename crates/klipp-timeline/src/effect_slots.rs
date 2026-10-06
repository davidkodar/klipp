//! A clip's built-in effects are addressed by their place in the list:
//! the enabled flags run parallel to it, keyframes are keyed
//! `"{index}:{param}"` and modifiers by `ParamId::Effect { effect_index }`.
//! Removing or moving an effect has to carry all three along, or they
//! stay at the old place and animate whichever effect lands there.

use crate::sequence::Clip;
use klipp_core::{ClipId, ParamId};
use klipp_effects::BuiltinEffect;

/// An effect entry's identity within this editing session. Never persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EffectId(uuid::Uuid);

impl EffectId {
    fn fresh() -> Self {
        Self(uuid::Uuid::new_v4())
    }

    pub fn as_uuid(self) -> uuid::Uuid {
        self.0
    }
}

/// Ephemeral sidecar for the raw effect list. Access it through `Clip`'s API.
///
/// The ids belong to the clip they were made for, named by its `ClipId`. A
/// clone keeps them: the same clip, edited and written back, keeps every
/// effect's identity. A clip that becomes a new clip takes a new `ClipId`
/// (a duplicate, a paste, a split's halves, an instance), and its effects
/// then get fresh ids the next time they are read, without each of those
/// places having to ask; [`Clip::refresh_effect_ids`] asks explicitly.
/// Loading starts empty and reconciles lazily.
#[derive(Debug, Default, Clone)]
pub struct EffectIds {
    /// The clip the ids were made for.
    owner: Option<ClipId>,
    ids: Vec<EffectId>,
}

impl Clip {
    /// Reconcile legacy direct writers by length. Existing prefix ids survive;
    /// new tail entries get fresh ids. Same-length raw edits cannot be detected
    /// during this transition and must move to the id API before relying on ids.
    fn reconcile_effect_ids(&mut self) {
        if self.effect_ids.owner != Some(self.id) {
            // A new clip: its effects are new effects.
            self.effect_ids = EffectIds { owner: Some(self.id), ids: Vec::new() };
        }
        let len = self.builtin_effects.len();
        self.effect_ids.ids.truncate(len);
        self.effect_ids.ids.resize_with(len, EffectId::fresh);
    }

    /// Effect identities, values and effective enabled flags in list order.
    /// A short flag list still means `true` for missing entries. Reading does
    /// not pad that persisted list; only the skipped id sidecar is reconciled.
    pub fn effect_entries(
        &mut self,
    ) -> impl ExactSizeIterator<Item = (EffectId, &BuiltinEffect, bool)> {
        self.reconcile_effect_ids();
        self.effect_ids
            .ids
            .iter()
            .copied()
            .zip(&self.builtin_effects)
            .enumerate()
            .map(|(i, (id, effect))| {
                (
                    id,
                    effect,
                    self.builtin_effects_enabled.get(i).copied().unwrap_or(true),
                )
            })
    }

    /// Give every effect a fresh id: for a caller that makes a new clip of
    /// this one without giving it a new `ClipId`.
    pub fn refresh_effect_ids(&mut self) {
        self.effect_ids = EffectIds::default();
        self.reconcile_effect_ids();
    }

    pub fn index_of(&mut self, id: EffectId) -> Option<usize> {
        self.reconcile_effect_ids();
        self.effect_ids.ids.iter().position(|current| *current == id)
    }

    pub fn effect(&mut self, id: EffectId) -> Option<&BuiltinEffect> {
        let index = self.index_of(id)?;
        self.builtin_effects.get(index)
    }

    /// Append an enabled entry with a new id.
    pub fn push_effect(&mut self, effect: BuiltinEffect) -> EffectId {
        self.insert_effect(self.builtin_effects.len(), effect)
            .expect("the list end is a valid insertion point")
    }

    /// Insert an enabled entry. An index beyond the end leaves the list alone.
    /// Inserting before existing entries also shifts orphan animation indices;
    /// an index at `usize::MAX` cannot shift, so that insertion is refused.
    pub fn insert_effect(&mut self, at: usize, effect: BuiltinEffect) -> Option<EffectId> {
        self.reconcile_effect_ids();
        let len = self.builtin_effects.len();
        if at > len {
            return None;
        }
        if at < len
            && (self.effect_keyframes.keys().any(|key| {
                key.split_once(':')
                    .and_then(|(i, _)| i.parse::<usize>().ok())
                    == Some(usize::MAX)
            }) || self.param_modifiers.keys().any(|id| {
                matches!(
                    id,
                    ParamId::Effect {
                        effect_index: usize::MAX,
                        ..
                    }
                )
            }))
        {
            return None;
        }
        let id = EffectId::fresh();
        self.builtin_effects.insert(at, effect);
        self.effect_ids.ids.insert(at, id);
        self.builtin_effects_enabled.resize(len, true);
        self.builtin_effects_enabled.insert(at, true);
        // Move orphan indices too: leaving one at the old end would collide
        // with the last occupied slot's animation as that slot moves up.
        if at < len {
            self.remap_effect_slots(|i| Some(if i >= at { i + 1 } else { i }));
        }
        Some(id)
    }

    pub fn remove_effect(&mut self, id: EffectId) -> Option<BuiltinEffect> {
        let index = self.index_of(id)?;
        self.remove_builtin_effect(index)
    }

    /// Move to an existing list position. Invalid ids/positions and no-ops
    /// return false, as the old index-based operation does.
    pub fn move_effect(&mut self, id: EffectId, to: usize) -> bool {
        let Some(from) = self.index_of(id) else {
            return false;
        };
        self.move_builtin_effect(from, to)
    }

    pub fn set_effect_enabled(&mut self, id: EffectId, enabled: bool) -> bool {
        let Some(index) = self.index_of(id) else {
            return false;
        };
        self.builtin_effects_enabled
            .resize(self.builtin_effects.len(), true);
        self.builtin_effects_enabled[index] = enabled;
        true
    }

    /// Replace only the value: its id, enabled flag and animation stay put.
    pub fn replace_effect(&mut self, id: EffectId, effect: BuiltinEffect) -> bool {
        let Some(index) = self.index_of(id) else {
            return false;
        };
        self.builtin_effects[index] = effect;
        true
    }

    /// Replace the whole list with enabled entries and fresh ids. All effect
    /// keyframes and effect-targeted modifiers are dropped; clip-level
    /// modifiers (opacity, transform, audio) are unrelated and remain.
    pub fn set_effects(&mut self, effects: Vec<BuiltinEffect>) {
        self.reconcile_effect_ids();
        self.effect_ids.ids = (0..effects.len()).map(|_| EffectId::fresh()).collect();
        self.builtin_effects_enabled = vec![true; effects.len()];
        self.builtin_effects = effects;
        self.effect_keyframes.clear();
        self.param_modifiers
            .retain(|id, _| !matches!(id, ParamId::Effect { .. }));
    }

    /// Remove built-in effect `index` with its enabled flag, keyframes
    /// and modifiers, and move those of the effects after it down one
    /// place with them.
    pub fn remove_builtin_effect(&mut self, index: usize) -> Option<BuiltinEffect> {
        self.reconcile_effect_ids();
        if index >= self.builtin_effects.len() {
            return None;
        }
        let effect = self.builtin_effects.remove(index);
        self.effect_ids.ids.remove(index);
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
        self.reconcile_effect_ids();
        let n = self.builtin_effects.len();
        if from >= n || to >= n || from == to {
            return false;
        }
        let effect = self.builtin_effects.remove(from);
        self.builtin_effects.insert(to, effect);
        let id = self.effect_ids.ids.remove(from);
        self.effect_ids.ids.insert(to, id);
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
            .filter_map(|(key, kfs)| {
                match key
                    .split_once(':')
                    .and_then(|(i, name)| Some((i.parse::<usize>().ok()?, name)))
                {
                    Some((i, name)) => map(i).map(|j| (format!("{j}:{name}"), kfs)),
                    None => Some((key, kfs)),
                }
            })
            .collect();
        self.param_modifiers = std::mem::take(&mut self.param_modifiers)
            .into_iter()
            .filter_map(|(id, stack)| match id {
                ParamId::Effect {
                    effect_index,
                    param_name,
                } => map(effect_index).map(|j| {
                    (
                        ParamId::Effect {
                            effect_index: j,
                            param_name,
                        },
                        stack,
                    )
                }),
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
            c.effect_keyframes.insert(
                format!("{i}:{name}"),
                vec![Keyframe {
                    time_secs: 0.0,
                    value: i as f32 * 10.0,
                    interpolation: Interpolation::Linear,
                    tangent_out: (0.0, 0.0),
                    tangent_in: (0.0, 0.0),
                }],
            );
            c.param_modifiers.insert(
                ParamId::Effect {
                    effect_index: i,
                    param_name: name.into(),
                },
                ModifierStack::default(),
            );
        }
        c.param_modifiers
            .insert(ParamId::Opacity, ModifierStack::default());
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
                c.effect_keyframes
                    .get(&format!("{i}:{name}"))
                    .map(|k| k[0].value)
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
        assert!(matches!(
            c.remove_builtin_effect(0),
            Some(BuiltinEffect::GaussianBlur { .. })
        ));
        assert_eq!(c.builtin_effects_enabled, vec![false, true]);
        assert_eq!(keyed(&c), vec![Some(10.0), Some(20.0)]);
        assert_eq!(
            c.effect_keyframes.len(),
            2,
            "the removed effect's keyframes stayed"
        );
        assert_eq!(modified(&c), vec![0, 1]);
        assert!(
            c.param_modifiers.contains_key(&ParamId::Opacity),
            "a clip modifier was dropped"
        );
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
                assert_eq!(
                    c.builtin_effects_enabled[now], before.builtin_effects_enabled[was],
                    "{from}->{to}"
                );
                assert_eq!(
                    keyed(&c)[now],
                    keyed(&before)[was],
                    "{from}->{to}: keyframes left behind"
                );
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

#[cfg(test)]
#[path = "effect_slots_tests.rs"]
mod api_tests;

/// A clip's whole effect state, for undo: the list, its flags, its session
/// ids, the effect keyframes and the effect-targeted modifiers, together.
/// Unlike cloning a [`Clip`], which starts a new identity domain, a
/// snapshot keeps the ids, so restoring it gives every effect back the
/// identity it had.
#[derive(Debug, Clone)]
pub struct EffectsState {
    effects: Vec<BuiltinEffect>,
    enabled: Vec<bool>,
    ids: Vec<EffectId>,
    keyframes: std::collections::HashMap<String, Vec<klipp_core::Keyframe>>,
    modifiers: Vec<(ParamId, klipp_core::ModifierStack)>,
}

impl EffectsState {
    /// A state from values taken elsewhere (a copied clip's effects, to
    /// paste), with fresh ids: the entries are new to the clip it lands on.
    /// `modifiers` keeps only effect-targeted stacks.
    pub fn fresh(
        effects: Vec<BuiltinEffect>,
        enabled: Vec<bool>,
        keyframes: std::collections::HashMap<String, Vec<klipp_core::Keyframe>>,
        modifiers: impl IntoIterator<Item = (ParamId, klipp_core::ModifierStack)>,
    ) -> Self {
        let ids = (0..effects.len()).map(|_| EffectId::fresh()).collect();
        let modifiers = modifiers.into_iter().filter(|(id, _)| matches!(id, ParamId::Effect { .. })).collect();
        Self { effects, enabled, ids, keyframes, modifiers }
    }
}

/// Access by [`EffectId`] to what callers otherwise address by a place they
/// computed: an effect's value, a parameter's keyframes and modifier stack.
/// Each keys exactly as the saved form does (`"{index}:{param}"`,
/// `ParamId::Effect { effect_index, param_name }`), so a caller moved onto
/// these behaves as before.
impl Clip {
    /// The id of the entry at `index`, for an index a list widget holds:
    /// resolve it once, at the edge, and address the effect by id after.
    pub fn effect_id_at(&mut self, index: usize) -> Option<EffectId> {
        self.reconcile_effect_ids();
        self.effect_ids.ids.get(index).copied()
    }

    /// The effect's value, to edit in place: the counterpart of
    /// [`Clip::replace_effect`]. Its id, flag and animation stay put.
    pub fn effect_mut(&mut self, id: EffectId) -> Option<&mut BuiltinEffect> {
        let index = self.index_of(id)?;
        self.builtin_effects.get_mut(index)
    }

    /// The keyframes of the effect's `param`, created empty when it has
    /// none. Stored under `"{index}:{param}"`.
    pub fn effect_keyframes_mut(&mut self, id: EffectId, param: &str) -> Option<&mut Vec<klipp_core::Keyframe>> {
        let index = self.index_of(id)?;
        Some(self.effect_keyframes.entry(format!("{index}:{param}")).or_default())
    }

    /// The keyframes of the effect's `param`, if it has any; creates none.
    pub fn existing_effect_keyframes_mut(&mut self, id: EffectId, param: &str) -> Option<&mut Vec<klipp_core::Keyframe>> {
        let index = self.index_of(id)?;
        self.effect_keyframes.get_mut(&format!("{index}:{param}"))
    }

    /// Take the keyframes of the effect's `param` away.
    pub fn remove_effect_keyframes(&mut self, id: EffectId, param: &str) -> Option<Vec<klipp_core::Keyframe>> {
        let index = self.index_of(id)?;
        self.effect_keyframes.remove(&format!("{index}:{param}"))
    }

    /// The modifier stack of the effect's `param`, created empty when it
    /// has none. Stored under `ParamId::Effect { effect_index, param_name }`.
    pub fn effect_modifiers_mut(&mut self, id: EffectId, param: &str) -> Option<&mut klipp_core::ModifierStack> {
        let index = self.index_of(id)?;
        let key = ParamId::Effect { effect_index: index, param_name: param.to_owned() };
        Some(self.param_modifiers.entry(key).or_default())
    }

    /// The modifier stack of the effect's `param`, if it has one; creates
    /// none.
    pub fn existing_effect_modifiers_mut(&mut self, id: EffectId, param: &str) -> Option<&mut klipp_core::ModifierStack> {
        let index = self.index_of(id)?;
        self.param_modifiers.get_mut(&ParamId::Effect { effect_index: index, param_name: param.to_owned() })
    }

    /// Take the modifier stack of the effect's `param` away.
    pub fn remove_effect_modifiers(&mut self, id: EffectId, param: &str) -> Option<klipp_core::ModifierStack> {
        let index = self.index_of(id)?;
        self.param_modifiers.remove(&ParamId::Effect { effect_index: index, param_name: param.to_owned() })
    }

    /// The whole effect state, ids included, for an undo to restore.
    pub fn effects_state(&mut self) -> EffectsState {
        self.reconcile_effect_ids();
        EffectsState {
            effects: self.builtin_effects.clone(),
            enabled: self.builtin_effects_enabled.clone(),
            ids: self.effect_ids.ids.clone(),
            keyframes: self.effect_keyframes.clone(),
            modifiers: self
                .param_modifiers
                .iter()
                .filter(|(id, _)| matches!(id, ParamId::Effect { .. }))
                .map(|(id, m)| (id.clone(), m.clone()))
                .collect(),
        }
    }

    /// Put back exactly what [`Clip::effects_state`] took: list, flags, ids,
    /// effect keyframes and effect modifiers. Clip-level modifiers
    /// (opacity, transform, audio) are not part of it and stay as they are.
    pub fn restore_effects_state(&mut self, state: EffectsState) {
        self.builtin_effects = state.effects;
        self.builtin_effects_enabled = state.enabled;
        // The ids belong to this clip, whose snapshot they were.
        self.effect_ids = EffectIds { owner: Some(self.id), ids: state.ids };
        self.effect_keyframes = state.keyframes;
        self.param_modifiers.retain(|id, _| !matches!(id, ParamId::Effect { .. }));
        self.param_modifiers.extend(state.modifiers);
    }
}

