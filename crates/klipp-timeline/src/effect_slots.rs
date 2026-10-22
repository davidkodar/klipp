//! A clip's built-in effects are addressed by their place in the list:
//! the enabled flags run parallel to it, keyframes are keyed
//! `"{index}:{param}"` and modifiers by `ParamId::Effect { effect_index }`.
//! Removing or moving an effect has to carry all three along, or they
//! stay at the old place and animate whichever effect lands there.

use crate::sequence::Clip;
use klipp_core::{ClipId, ParamId};
use klipp_effects::BuiltinEffect;
use serde::{Deserialize, Serialize};

/// A clip's built-in effects, their saved flags and session identities.
/// Contents are private: edits go through the clip's effect-list API so
/// flags, keyframes and modifiers stay with their effect.
///
/// ```compile_fail
/// let mut effects = klipp_timeline::EffectList::default();
/// effects.builtin_effects.clear();
/// ```
/// ```compile_fail
/// let mut effects = klipp_timeline::EffectList::default();
/// effects.builtin_effects_enabled.push(false);
/// ```
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct EffectList {
    #[serde(default)]
    builtin_effects: Vec<BuiltinEffect>,
    #[serde(default)]
    builtin_effects_enabled: Vec<bool>,
}

impl EffectList {
    /// Construct an owner's list from saved or copied values. Preserve the
    /// flags exactly, including missing and surplus entries; ids start fresh.
    pub fn new(effects: Vec<BuiltinEffect>, enabled: Vec<bool>) -> Self {
        Self { builtin_effects: effects, builtin_effects_enabled: enabled }
    }
}

/// An effect entry's identity: its node's id in the clip's picture, kept
/// through a reorder, an insert and a save (M3 proposal 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EffectId(uuid::Uuid);

impl EffectId {
    /// A new effect's id: random, so no effect before or after a reload
    /// is ever given one another had. Minted by an edit that records the
    /// new list with its ids, so its undo and redo put it back as made.
    pub fn fresh() -> Self {
        Self(uuid::Uuid::new_v4())
    }

    pub fn as_uuid(self) -> uuid::Uuid {
        self.0
    }

    /// The effect whose picture node has id `id`.
    pub fn of_node(id: uuid::Uuid) -> Self {
        Self(id)
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
#[derive(Debug, Clone)]
pub(crate) struct EffectIds {
    /// The clip the ids were made for.
    owner: Option<ClipId>,
    pub(crate) ids: Vec<EffectId>,
    /// The next of the clip's run of new ids ([`Clip::next_effect_id`]):
    /// only ever rises, so an id an effect gave up is never given again.
    next: usize,
    /// What makes this run of new ids its own: random, made with the
    /// sidecar, so a clip read again starts a run no earlier session's
    /// ids are in (`next` isn't saved), while every lift of the clip in
    /// one session names a new entry alike.
    run: uuid::Uuid,
}

impl Default for EffectIds {
    fn default() -> Self {
        Self { owner: None, ids: Vec::new(), next: 0, run: uuid::Uuid::new_v4() }
    }
}

impl EffectIds {
    /// Hold `ids` for `owner`, the run carried on.
    fn hold(&mut self, owner: ClipId, ids: Vec<EffectId>, next: usize) {
        self.owner = Some(owner);
        self.ids = ids;
        self.next = next;
    }
}

impl Clip {
    /// Built-in values in list order, including disabled entries.
    pub fn builtin_effects(&self) -> &[BuiltinEffect] {
        &self.picture.view().effect_list.builtin_effects
    }

    /// The built-in effects switched on, in list order (a missing flag is
    /// on): what an analysis or a render reader runs.
    pub fn enabled_effects(&self) -> impl Iterator<Item = &BuiltinEffect> {
        let list = &self.picture.view().effect_list;
        list.builtin_effects.iter().enumerate().filter(|(i, _)| list.builtin_effects_enabled.get(*i).copied().unwrap_or(true)).map(|(_, e)| e)
    }

    /// The saved flags exactly as supplied. Missing entries mean enabled.
    pub fn builtin_effects_enabled(&self) -> &[bool] {
        &self.picture.view().effect_list.builtin_effects_enabled
    }

    /// Allocate ids after construction/loading, and refresh them when a copied
    /// clip takes a new ClipId. Existing entries retain their identity.
    pub(crate) fn reconcile_effect_ids(&mut self) {
        let (ids, next) = self.effect_ids_for_picture_and_next();
        self.picture.effect_ids_mut().hold(self.id, ids, next);
    }

    /// The `n`th id of the clip's run of new ids, or the first after it
    /// none of `taken` holds, and the next `n` (M3 proposal 6). Derived
    /// from the clip's id and the run's random part, so two lifts of one
    /// clip agree, and a run after a reload repeats no id of an earlier one.
    fn next_effect_id(&self, mut n: usize, taken: &[EffectId]) -> (EffectId, usize) {
        use klipp_graph_lift::ids::{entry_id, RootKind};
        let run = format!("effects+{}", self.picture.effect_ids().run);
        loop {
            let id = EffectId(entry_id(RootKind::Clip, self.id.0, &run, n));
            n += 1;
            if !taken.contains(&id) {
                return (id, n);
            }
        }
    }

    /// Each entry's id, as the clip's picture names its node, and the run's
    /// next. A clip just read, or a copy whose ids were made for another
    /// clip, gives each entry its slot's derived id: the one every lift
    /// named it by before ids were kept, so a clip from before keeps its
    /// effects' ids, and a copy under a new `ClipId` gets new ones. An entry
    /// added since takes the next of the run, never an id another entry
    /// gave up.
    fn effect_ids_for_picture_and_next(&self) -> (Vec<EffectId>, usize) {
        use klipp_graph_lift::ids::{entry_id, RootKind};
        let len = self.picture.view().effect_list.builtin_effects.len();
        let held = self.picture.effect_ids();
        if held.owner != Some(self.id) {
            return ((0..len).map(|slot| EffectId(entry_id(RootKind::Clip, self.id.0, "effects", slot))).collect(), 0);
        }
        let (mut ids, mut next): (Vec<EffectId>, usize) = (held.ids.iter().copied().take(len).collect(), held.next);
        while ids.len() < len {
            let (id, n) = self.next_effect_id(next, &ids);
            ids.push(id);
            next = n;
        }
        (ids, next)
    }

    /// Each entry's id, as the clip's picture names its node.
    pub(crate) fn effect_ids_for_picture(&self) -> Vec<EffectId> {
        self.effect_ids_for_picture_and_next().0
    }

    /// Take the ids a stored picture gives its effects, in list order, as
    /// this clip's (a clip read from a file): its effects keep the ids
    /// they were saved under.
    pub(crate) fn adopt_effect_ids(&mut self, ids: Vec<EffectId>) {
        let next = self.picture.effect_ids().next;
        self.picture.effect_ids_mut().hold(self.id, ids, next);
    }

    /// Effect identities, values and effective enabled flags in list order.
    /// A short flag list still means `true` for missing entries. Reading does
    /// not pad that persisted list; only the skipped id sidecar is reconciled.
    pub fn effect_entries(
        &mut self,
    ) -> impl ExactSizeIterator<Item = (EffectId, &BuiltinEffect, bool)> {
        self.reconcile_effect_ids();
        self.picture.effect_ids()
            .ids
            .iter()
            .copied()
            .zip(&self.picture.view().effect_list.builtin_effects)
            .enumerate()
            .map(|(i, (id, effect))| {
                (
                    id,
                    effect,
                    self.picture.view().effect_list.builtin_effects_enabled.get(i).copied().unwrap_or(true),
                )
            })
    }

    /// Give every effect a fresh id: for a caller that makes a new clip of
    /// this one without giving it a new `ClipId`.
    pub fn refresh_effect_ids(&mut self) {
        self.reconcile_effect_ids();
        let next = self.picture.effect_ids().next;
        self.picture.effect_ids_mut().hold(self.id, Vec::new(), next);
        self.reconcile_effect_ids();
    }

    pub fn index_of(&mut self, id: EffectId) -> Option<usize> {
        self.reconcile_effect_ids();
        self.picture.effect_ids().ids.iter().position(|current| *current == id)
    }

    pub fn effect(&mut self, id: EffectId) -> Option<&BuiltinEffect> {
        let index = self.index_of(id)?;
        self.picture.view().effect_list.builtin_effects.get(index)
    }

    /// Append an enabled entry with a new id.
    pub fn push_effect(&mut self, effect: BuiltinEffect) -> EffectId {
        self.through_truth("the effect list", |c| {
            c.insert_effect(c.picture.view().effect_list.builtin_effects.len(), effect)
                .expect("the list end is a valid insertion point")
        })
    }

    /// Insert an enabled entry. An index beyond the end leaves the list alone.
    /// Inserting before existing entries also shifts orphan animation indices;
    /// an index at `usize::MAX` cannot shift, so that insertion is refused.
    pub fn insert_effect(&mut self, at: usize, effect: BuiltinEffect) -> Option<EffectId> {
        self.insert_effect_as(at, effect, None)
    }

    /// [`Self::insert_effect`], the entry taking `id` (an effect node a graph
    /// op added, which keeps the id the op gave it). `None` if another entry
    /// holds `id`.
    pub fn insert_effect_with_id(&mut self, at: usize, effect: BuiltinEffect, id: EffectId) -> Option<EffectId> {
        self.insert_effect_as(at, effect, Some(id))
    }

    fn insert_effect_as(&mut self, at: usize, effect: BuiltinEffect, given: Option<EffectId>) -> Option<EffectId> {
        self.through_truth("the effect list", |c| {
            c.reconcile_effect_ids();
            if given.is_some_and(|id| c.picture.effect_ids().ids.contains(&id)) {
                return None;
            }
            let len = c.picture.view().effect_list.builtin_effects.len();
            if at > len {
                return None;
            }
            if at < len
                && (c.effect_keyframes().keys().any(|key| {
                    key.split_once(':')
                        .and_then(|(i, _)| i.parse::<usize>().ok())
                        == Some(usize::MAX)
                }) || c.param_modifiers().keys().any(|id| {
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
            // A new effect's id is random: unlike a counter it survives no
            // save it would have to, and an id an effect gave up, before a
            // reload or after, is never given again (a client holding the
            // old path is refused, never handed another effect).
            let id = given.unwrap_or_else(EffectId::fresh);
            c.view_mut().effect_list.builtin_effects.insert(at, effect);
            c.picture.effect_ids_mut().ids.insert(at, id);
            c.view_mut().effect_list.builtin_effects_enabled.resize(len, true);
            c.view_mut().effect_list.builtin_effects_enabled.insert(at, true);
            // Move orphan indices too, an append's included: an orphan at
            // the old end would otherwise be the new entry's animation, or
            // collide with the last slot's as that slot moves up (M4 step
            // 12b).
            // A key on `usize::MAX` names no slot and stays put (an insert
            // before the end refuses one, above).
            c.remap_effect_slots(|i| Some(if i >= at && i != usize::MAX { i + 1 } else { i }));
            Some(id)
        })
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

    /// On a picture-truth clip, through its guard (its picture-truth guard, `Clip::through_truth`).
    pub fn set_effect_enabled(&mut self, id: EffectId, enabled: bool) -> bool {
        let Some(index) = self.index_of(id) else {
            return false;
        };
        self.through_truth("an effect switched on or off", |c| {
            let list = &mut c.view_mut().effect_list;
            list.builtin_effects_enabled.resize(list.builtin_effects.len(), true);
            list.builtin_effects_enabled[index] = enabled;
        });
        true
    }

    /// Replace only the value: its id, enabled flag and animation stay put.
    /// On a picture-truth clip, through its guard (its picture-truth guard, `Clip::through_truth`).
    pub fn replace_effect(&mut self, id: EffectId, effect: BuiltinEffect) -> bool {
        let Some(index) = self.index_of(id) else {
            return false;
        };
        self.through_truth("an effect's settings", |c| c.view_mut().effect_list.builtin_effects[index] = effect);
        true
    }

    /// Replace the whole list with enabled entries and fresh ids. All effect
    /// keyframes and effect-targeted modifiers are dropped; clip-level
    /// modifiers (opacity, transform, audio) are unrelated and remain.
    pub fn set_effects(&mut self, effects: Vec<BuiltinEffect>) {
        self.through_truth("the effect list", |c| {
            c.reconcile_effect_ids();
            c.picture.effect_ids_mut().ids.clear();
            c.view_mut().effect_list.builtin_effects_enabled = vec![true; effects.len()];
            c.view_mut().effect_list.builtin_effects = effects;
            c.edit_effect_keyframes(|v| v.clear());
            c.view_mut().param_modifiers.0
                .retain(|id, _| !matches!(id, ParamId::Effect { .. }));
        })
    }

    /// Replace the whole list and its flags as given, the effect keyframes
    /// and modifiers left where they are, by place: a whole-list write that
    /// keeps its animation (an undo of a list edit, a render copy given its
    /// own effects back). Fresh ids, as [`Clip::set_effects`].
    ///
    /// On a picture-truth clip, through its guard (its picture-truth guard, `Clip::through_truth`):
    /// an entry at the same place and of the same type keeps its node, and
    /// a list that takes away an effect holding a mix or mask is refused.
    pub fn set_effect_list(&mut self, effects: Vec<BuiltinEffect>, enabled: Vec<bool>) {
        self.set_effect_list_with_ids(effects, enabled, &[]);
    }

    /// Each effect's id, in list order: its node's id in the clip's
    /// picture (M3 proposal 6), for an edit that puts the list back.
    pub fn effect_id_list(&self) -> Vec<EffectId> {
        self.effect_ids_for_picture()
    }

    /// Put `effects` in place under `ids`, one each and none twice, as an
    /// undo or a redo puts back the list it recorded: every effect keeps
    /// the node id a path, a selection or a key names it by (#1373). It
    /// used to clear the ids, so an undo gave every effect of the clip a
    /// new one.
    ///
    /// Without such ids (an edit that only knows the effects) it is the
    /// whole-list write it always was: every entry a new id, none borrowed
    /// from a neighbour a held path would then silently name, and a
    /// picture-truth clip's nodes kept by place and type.
    pub fn set_effect_list_with_ids(&mut self, effects: Vec<BuiltinEffect>, enabled: Vec<bool>, ids: &[EffectId]) {
        let exact = ids.len() == effects.len() && ids.iter().collect::<std::collections::HashSet<_>>().len() == ids.len();
        self.through_truth("the effect list", |c| {
            c.reconcile_effect_ids();
            c.picture.effect_ids_mut().ids.clear();
            c.view_mut().effect_list.builtin_effects = effects;
            c.view_mut().effect_list.builtin_effects_enabled = enabled;
            if exact {
                let (owner, next) = (c.id, c.picture.effect_ids().next);
                c.picture.effect_ids_mut().hold(owner, ids.to_vec(), next);
            }
            c.reconcile_effect_ids();
        });
    }

    /// Remove built-in effect `index` with its enabled flag, keyframes
    /// and modifiers, and move those of the effects after it down one
    /// place with them.
    pub fn remove_builtin_effect(&mut self, index: usize) -> Option<BuiltinEffect> {
        self.through_truth("the effect list", |c| {
            c.reconcile_effect_ids();
            if index >= c.picture.view().effect_list.builtin_effects.len() {
                return None;
            }
            let effect = c.view_mut().effect_list.builtin_effects.remove(index);
            c.picture.effect_ids_mut().ids.remove(index);
            if index < c.picture.view().effect_list.builtin_effects_enabled.len() {
                c.view_mut().effect_list.builtin_effects_enabled.remove(index);
            }
            c.remap_effect_slots(|i| match i.cmp(&index) {
                std::cmp::Ordering::Less => Some(i),
                std::cmp::Ordering::Equal => None,
                std::cmp::Ordering::Greater => Some(i - 1),
            });
            Some(effect)
        })
    }

    /// Move built-in effect `from` to place `to`, its enabled flag,
    /// keyframes and modifiers with it. False when either place is out
    /// of range or they are the same.
    pub fn move_builtin_effect(&mut self, from: usize, to: usize) -> bool {
        self.through_truth("the effect list", |c| {
            c.reconcile_effect_ids();
            let n = c.picture.view().effect_list.builtin_effects.len();
            if from >= n || to >= n || from == to {
                return false;
            }
            let effect = c.view_mut().effect_list.builtin_effects.remove(from);
            c.view_mut().effect_list.builtin_effects.insert(to, effect);
            let id = c.picture.effect_ids_mut().ids.remove(from);
            c.picture.effect_ids_mut().ids.insert(to, id);
            // A missing flag reads as enabled; fill them in so the moved
            // effect's flag has a place to go.
            if c.picture.view().effect_list.builtin_effects_enabled.len() < n {
                c.view_mut().effect_list.builtin_effects_enabled.resize(n, true);
            }
            let on = c.view_mut().effect_list.builtin_effects_enabled.remove(from);
            c.view_mut().effect_list.builtin_effects_enabled.insert(to, on);
            c.remap_effect_slots(|i| Some(moved_place(i, from, to)));
            true
        })
    }

    /// Re-key effect keyframes and modifiers by `map` (old place to new,
    /// `None` to drop them). Keys that do not name a place are kept.
    fn remap_effect_slots(&mut self, map: impl Fn(usize) -> Option<usize>) {
        let mut view = self.view_mut();
        view.effect_keyframes.0 = std::mem::take(&mut view.effect_keyframes.0)
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
        view.param_modifiers.0 = std::mem::take(&mut view.param_modifiers.0)
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
        c.view_mut().effect_list.builtin_effects = vec![
            BuiltinEffect::GaussianBlur { radius: 1.0 },
            BuiltinEffect::Sharpen { amount: 2.0 },
            BuiltinEffect::Vibrance { amount: 3.0 },
        ];
        c.view_mut().effect_list.builtin_effects_enabled = vec![true, false, true];
        for (i, name) in [(0, "radius"), (1, "amount"), (2, "amount")] {
            c.view_mut().effect_keyframes.0.insert(
                format!("{i}:{name}"),
                vec![Keyframe {
                    time_secs: 0.0,
                    value: i as f32 * 10.0,
                    interpolation: Interpolation::Linear,
                    tangent_out: (0.0, 0.0),
                    tangent_in: (0.0, 0.0),
                    handles: Default::default(),
                }],
            );
            c.view_mut().param_modifiers.0.insert(
                ParamId::Effect {
                    effect_index: i,
                    param_name: name.into(),
                },
                ModifierStack::default(),
            );
        }
        c.view_mut().param_modifiers.0
            .insert(ParamId::Opacity, ModifierStack::default());
        c
    }

    /// The keyframe value each effect's own parameter carries, by place.
    fn keyed(c: &Clip) -> Vec<Option<f32>> {
        c.picture.view().effect_list.builtin_effects
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let name = match e {
                    BuiltinEffect::GaussianBlur { .. } => "radius",
                    _ => "amount",
                };
                c.picture.view().effect_keyframes.0
                    .get(&format!("{i}:{name}"))
                    .map(|k| k[0].value)
            })
            .collect()
    }

    fn modified(c: &Clip) -> Vec<usize> {
        let mut v: Vec<usize> = c
            .picture.view().param_modifiers.0
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
        assert_eq!(c.picture.view().effect_list.builtin_effects_enabled, vec![false, true]);
        assert_eq!(keyed(&c), vec![Some(10.0), Some(20.0)]);
        assert_eq!(
            c.effect_keyframes().len(),
            2,
            "the removed effect's keyframes stayed"
        );
        assert_eq!(modified(&c), vec![0, 1]);
        assert!(
            c.param_modifiers().contains_key(&ParamId::Opacity),
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
                assert_eq!(c.picture.view().effect_list.builtin_effects[now], before.picture.view().effect_list.builtin_effects[was]);
                assert_eq!(
                    c.picture.view().effect_list.builtin_effects_enabled[now], before.picture.view().effect_list.builtin_effects_enabled[was],
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
            assert_eq!(c.picture.view().effect_keyframes, before.picture.view().effect_keyframes);
            assert_eq!(modified(&c), modified(&before));
        }
        let mut c = clip();
        assert!(!c.move_builtin_effect(1, 1));
        assert!(!c.move_builtin_effect(0, 3));
    }

    #[test]
    fn a_missing_enabled_flag_still_moves_as_enabled() {
        let mut c = clip();
        c.view_mut().effect_list.builtin_effects_enabled = vec![false];
        assert!(c.move_builtin_effect(0, 2));
        assert_eq!(c.picture.view().effect_list.builtin_effects_enabled, vec![true, true, false]);
    }
}

#[cfg(test)]
#[path = "effect_slots_tests.rs"]
mod api_tests;

/// A clip's whole effect state, for undo: the list, its flags, its session
/// ids, the effect keyframes and the effect-targeted modifiers, together.
/// A snapshot keeps the ids, so restoring it gives every effect back the
/// identity it had. A clone with the same ClipId also keeps those ids.
#[derive(Debug, Clone)]
pub struct EffectsState {
    effects: Vec<BuiltinEffect>,
    enabled: Vec<bool>,
    ids: Vec<EffectId>,
    keyframes: std::collections::HashMap<String, Vec<klipp_core::Keyframe>>,
    modifiers: Vec<(ParamId, klipp_core::ModifierStack)>,
}

impl EffectsState {
    /// A state as the clip's own picture holds it (M4 step 12b-0): each
    /// effect under the id its node has there.
    pub(crate) fn of_picture(
        effects: Vec<BuiltinEffect>,
        enabled: Vec<bool>,
        ids: Vec<EffectId>,
        keyframes: std::collections::HashMap<String, Vec<klipp_core::Keyframe>>,
        modifiers: Vec<(ParamId, klipp_core::ModifierStack)>,
    ) -> Self {
        Self { effects, enabled, ids, keyframes, modifiers }
    }

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

/// A clip whose fields say other than its picture, for a test (M4 step
/// 12a): the clip made picture-truth on the picture it has, then its
/// built-in list, their flags and its OFX list changed by `f` without its
/// picture. A reader that reads the picture sees the picture as it was;
/// one that reads the fields sees `f`'s. No edit makes such a clip.
#[cfg(any(test, feature = "test-clip"))]
impl Clip {
    #[doc(hidden)]
    pub fn diverge_fields_for_test(&mut self, f: impl FnOnce(&mut Vec<BuiltinEffect>, &mut Vec<bool>, &mut Vec<klipp_plugin::AppliedEffect>)) {
        let picture = self.picture_v2().expect("the clip's picture");
        self.picture.set_truth(Some(picture));
        let view = self.picture.view_unguarded();
        f(&mut view.effect_list.builtin_effects, &mut view.effect_list.builtin_effects_enabled, &mut view.effects.0);
    }
}

/// Access by [`EffectId`] to what callers otherwise address by a place they
/// computed: an effect's value, a parameter's keyframes and modifier stack.
/// Each keys exactly as the saved form does (`"{index}:{param}"`,
/// `ParamId::Effect { effect_index, param_name }`), so a caller moved onto
/// these behaves as before.
impl Clip {
    /// The picture node of the clip's effect at `index` of its built-in
    /// list, or of its plug-in list when `ofx` (M3 step 4): what a list
    /// widget's row is, addressed as the picture names it.
    pub fn effect_node_at(&self, ofx: bool, index: usize) -> Option<uuid::Uuid> {
        if ofx {
            use klipp_graph_lift::ids::{entry_id, RootKind};
            return (index < self.ofx_effects().len()).then(|| entry_id(RootKind::Clip, self.id.0, "ofx", index));
        }
        self.effect_ids_for_picture().get(index).map(|id| id.as_uuid())
    }

    /// Where the clip's effect node `node` sits: in its plug-in list or its
    /// built-in list, and at which place. The inverse of
    /// [`Clip::effect_node_at`].
    pub fn effect_slot_of_node(&self, node: uuid::Uuid) -> Option<(bool, usize)> {
        if let Some(i) = self.effect_ids_for_picture().iter().position(|id| id.as_uuid() == node) {
            return Some((false, i));
        }
        (0..self.ofx_effects().len()).find(|i| self.effect_node_at(true, *i) == Some(node)).map(|i| (true, i))
    }

    /// Switch the effect node `node` on or off, as the A/B compare's
    /// bypass does: true when the clip has it.
    pub fn toggle_effect_node(&mut self, node: uuid::Uuid) -> bool {
        match self.effect_slot_of_node(node) {
            Some((true, i)) => self.edit_ofx_effect(i, |e| e.enabled = !e.enabled).is_some(),
            Some((false, i)) => {
                let id = crate::EffectId::of_node(node);
                let on = self.builtin_effects_enabled().get(i).copied().unwrap_or(true);
                self.set_effect_enabled(id, !on)
            }
            None => false,
        }
    }

    /// The id of the entry at `index`, for an index a list widget holds:
    /// resolve it once, at the edge, and address the effect by id after.
    pub fn effect_id_at(&mut self, index: usize) -> Option<EffectId> {
        self.reconcile_effect_ids();
        self.picture.effect_ids().ids.get(index).copied()
    }

    /// Change the effect's value in `f`, in place: the counterpart of
    /// [`Clip::replace_effect`]. Its id, flag and animation stay put.
    /// `None` when `id` names no effect of the clip.
    ///
    /// On a picture-truth clip, through its guard (its picture-truth guard, `Clip::through_truth`).
    pub fn edit_effect<R>(&mut self, id: EffectId, f: impl FnOnce(&mut BuiltinEffect) -> R) -> Option<R> {
        let index = self.index_of(id)?;
        self.through_truth("an effect's settings", |c| c.view_mut().effect_list.builtin_effects.get_mut(index).map(f))
    }

    /// Change the keyframes of the effect's `param` in `f`, created empty
    /// when it has none. Stored under `"{index}:{param}"`.
    pub fn edit_effect_keyframes_of<R>(&mut self, id: EffectId, param: &str, f: impl FnOnce(&mut Vec<klipp_core::Keyframe>) -> R) -> Option<R> {
        let index = self.index_of(id)?;
        Some(f(self.view_mut().effect_keyframes.0.entry(format!("{index}:{param}")).or_default()))
    }

    /// Change the keyframes of the effect's `param` in `f`, if it has any;
    /// creates none.
    pub fn edit_existing_effect_keyframes<R>(&mut self, id: EffectId, param: &str, f: impl FnOnce(&mut Vec<klipp_core::Keyframe>) -> R) -> Option<R> {
        let index = self.index_of(id)?;
        self.view_mut().effect_keyframes.0.get_mut(&format!("{index}:{param}")).map(f)
    }

    /// Take the keyframes of the effect's `param` away.
    pub fn remove_effect_keyframes(&mut self, id: EffectId, param: &str) -> Option<Vec<klipp_core::Keyframe>> {
        let index = self.index_of(id)?;
        self.view_mut().effect_keyframes.0.remove(&format!("{index}:{param}"))
    }

    /// Change the modifier stack of the effect's `param` in `f`, created
    /// empty when it has none. Stored under `ParamId::Effect { effect_index,
    /// param_name }`.
    pub fn edit_effect_modifiers<R>(&mut self, id: EffectId, param: &str, f: impl FnOnce(&mut klipp_core::ModifierStack) -> R) -> Option<R> {
        let index = self.index_of(id)?;
        let key = ParamId::Effect { effect_index: index, param_name: param.to_owned() };
        Some(f(self.view_mut().param_modifiers.0.entry(key).or_default()))
    }

    /// Change the modifier stack of the effect's `param` in `f`, if it has
    /// one; creates none.
    pub fn edit_existing_effect_modifiers<R>(&mut self, id: EffectId, param: &str, f: impl FnOnce(&mut klipp_core::ModifierStack) -> R) -> Option<R> {
        let index = self.index_of(id)?;
        self.view_mut().param_modifiers.0.get_mut(&ParamId::Effect { effect_index: index, param_name: param.to_owned() }).map(f)
    }

    /// Take the modifier stack of the effect's `param` away.
    pub fn remove_effect_modifiers(&mut self, id: EffectId, param: &str) -> Option<klipp_core::ModifierStack> {
        let index = self.index_of(id)?;
        self.view_mut().param_modifiers.0.remove(&ParamId::Effect { effect_index: index, param_name: param.to_owned() })
    }

    /// Every effect keyframe at once, as a snapshot took them (a keyframe
    /// edit's undo): the list, flags, ids and modifiers stay as they are.
    pub fn restore_effect_keyframes(&mut self, keyframes: std::collections::HashMap<String, Vec<klipp_core::Keyframe>>) {
        self.edit_effect_keyframes(|v| *v = keyframes);
    }

    /// The whole effect state, ids included, for an undo to restore.
    pub fn effects_state(&mut self) -> EffectsState {
        self.reconcile_effect_ids();
        EffectsState {
            effects: self.picture.view().effect_list.builtin_effects.clone(),
            enabled: self.picture.view().effect_list.builtin_effects_enabled.clone(),
            ids: self.picture.effect_ids().ids.clone(),
            keyframes: self.effect_keyframes().clone(),
            modifiers: self
                .picture.view().param_modifiers.0
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
        self.through_truth("the effect list", |c| {
            c.view_mut().effect_list.builtin_effects = state.effects;
            c.view_mut().effect_list.builtin_effects_enabled = state.enabled;
            // The ids belong to this clip, whose snapshot they were.
            let next = c.picture.effect_ids().next;
            c.picture.effect_ids_mut().hold(c.id, state.ids, next);
            c.edit_effect_keyframes(|v| *v = state.keyframes);
            c.edit_param_modifiers(|v| v.retain(|id, _| !matches!(id, ParamId::Effect { .. })));
            c.edit_param_modifiers(|v| v.extend(state.modifiers));
        })
    }
}

#[cfg(test)]
mod set_effect_list_ids_tests {
    use crate::*;
    use klipp_effects::BuiltinEffect as E;

    fn clip_with(effects: &[E]) -> Clip {
        let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
        let mut c = Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
        for e in effects {
            c.push_effect(e.clone());
        }
        c
    }
    fn blur(r: f32) -> E { E::GaussianBlur { radius: r } }
    fn opacity(v: f32) -> E { E::Opacity { value: v } }

    #[test]
    fn given_ids_are_put_back_exactly() {
        let mut c = clip_with(&[blur(1.0), opacity(0.5)]);
        let ids = c.effect_id_list();
        c.set_effect_list(vec![blur(1.0)], vec![true]);
        c.set_effect_list_with_ids(vec![blur(1.0), opacity(0.5)], vec![true, true], &ids);
        assert_eq!(c.effect_id_list(), ids, "an undo's or a redo's ids come back as they were");
    }

    #[test]
    fn without_ids_no_effect_takes_an_id_another_had() {
        // Two Blurs, A then B, and A taken out by an edit that knows no
        // ids: the one left gets a new id, never A's handed on to B.
        let mut c = clip_with(&[blur(1.0), blur(2.0), opacity(0.5)]);
        let before = c.effect_id_list();
        c.set_effect_list(vec![blur(2.0), opacity(0.5)], vec![true, true]);
        assert!(c.effect_id_list().iter().all(|id| !before.contains(id)), "a whole-list write borrows no id");
    }

    #[test]
    fn ids_that_dont_fit_the_list_are_not_used() {
        let c = clip_with(&[blur(1.0), opacity(0.5)]);
        let ids = c.effect_id_list();
        // Too few, or one twice: taken as no ids at all.
        for given in [vec![ids[0]], vec![ids[0], ids[0]]] {
            let mut tried = c.clone();
            tried.set_effect_list_with_ids(vec![blur(1.0), opacity(0.5)], vec![true, true], &given);
            let now = tried.effect_id_list();
            assert_eq!(now.len(), 2);
            assert!(now.iter().all(|id| !ids.contains(id)), "{given:?}: no id given half-used");
        }
    }
}
