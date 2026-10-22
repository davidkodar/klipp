use super::*;
use klipp_core::{Interpolation, Keyframe, MediaId, Modifier, ModifierStack, RationalTime};
use std::collections::HashSet;

fn clip() -> Clip {
    let t = |n| RationalTime::from_frames(n, 24);
    Clip::media(MediaId::new(), t(0), t(48), t(0), t(48))
}
fn blur(radius: f32) -> BuiltinEffect {
    BuiltinEffect::GaussianBlur { radius }
}
fn key(value: f32) -> Keyframe {
    Keyframe {
        time_secs: 0.0,
        value,
        interpolation: Interpolation::Linear,
        tangent_in: (0.0, 0.0),
        tangent_out: (0.0, 0.0),
        handles: Default::default(),
    }
}
fn ids(c: &mut Clip) -> Vec<EffectId> {
    c.effect_entries().map(|(id, _, _)| id).collect()
}

#[test]
fn reads_reconcile_direct_tail_edits_without_changing_saved_fields() {
    let mut c = clip();
    c.view_mut().effect_list.builtin_effects = vec![blur(1.0), blur(2.0)];
    c.view_mut().effect_list.builtin_effects_enabled = vec![false];
    let before = serde_json::to_value(&c).unwrap();
    let old = ids(&mut c);
    assert_eq!(serde_json::to_value(&c).unwrap(), before);
    assert_eq!(
        c.effect_entries().map(|(_, _, on)| on).collect::<Vec<_>>(),
        vec![false, true]
    );
    assert_eq!(ids(&mut c), old);
    c.view_mut().effect_list.builtin_effects.push(blur(3.0));
    let grown = ids(&mut c);
    assert_eq!(&grown[..2], &old);
    assert!(!old.contains(&grown[2]));
    c.view_mut().effect_list.builtin_effects.truncate(1);
    assert_eq!(c.index_of(old[1]), None);
    assert_eq!(ids(&mut c), vec![old[0]]);
    c.view_mut().effect_list.builtin_effects.push(blur(4.0));
    let fresh = ids(&mut c);
    assert_ne!(fresh[1], old[1]);
    assert_ne!(fresh[1], grown[2]);
}

#[test]
fn a_clone_keeps_ids_a_load_rebuilds_them_and_ids_never_reach_json() {
    let mut c = clip();
    c.push_effect(blur(2.0));
    c.push_effect(blur(3.0));
    let original = ids(&mut c);
    let value = serde_json::to_value(&c).unwrap();
    assert!(value.get("effect_ids").is_none());
    let text = serde_json::to_string(&value).unwrap();
    for id in &original {
        assert!(!text.contains(&id.as_uuid().to_string()));
    }
    let mut copy = c.clone();
    assert_eq!(serde_json::to_value(&copy).unwrap(), value);
    assert_eq!(ids(&mut copy), original, "a clone is the same clip, with the same effects");
    let mut loaded: Clip = serde_json::from_value(value.clone()).unwrap();
    assert!(ids(&mut loaded).iter().all(|id| !original.contains(id)));
    assert_eq!(serde_json::to_value(&loaded).unwrap(), value);
    let mut forged = value.clone();
    forged["effect_ids"] = serde_json::json!([original[0].as_uuid().to_string()]);
    let mut loaded: Clip = serde_json::from_value(forged).unwrap();
    assert!(ids(&mut loaded).iter().all(|id| !original.contains(id)));
}

#[test]
fn invalid_operations_and_old_short_flags_keep_their_meaning() {
    let mut c = clip();
    c.view_mut().effect_list.builtin_effects = vec![blur(1.0), blur(2.0), blur(3.0)];
    c.view_mut().effect_list.builtin_effects_enabled = vec![false];
    let original = ids(&mut c);
    let missing = EffectId::fresh();
    let before = serde_json::to_value(&c).unwrap();
    assert_eq!(c.insert_effect(4, blur(4.0)), None);
    assert_eq!(c.remove_effect(missing), None);
    assert_eq!(c.effect(missing), None);
    assert!(!c.move_effect(missing, 0));
    assert!(!c.move_effect(original[0], 3));
    assert!(!c.move_effect(original[0], 0));
    assert!(!c.set_effect_enabled(missing, false));
    assert!(!c.replace_effect(missing, blur(9.0)));
    assert_eq!(serde_json::to_value(&c).unwrap(), before);
    assert!(c.remove_builtin_effect(2).is_some());
    assert_eq!(
        c.picture.view().effect_list.builtin_effects_enabled,
        vec![false],
        "old remove must not pad persisted flags"
    );
    assert!(c.move_builtin_effect(0, 1));
    assert_eq!(ids(&mut c), vec![original[1], original[0]]);
    assert_eq!(c.picture.view().effect_list.builtin_effects_enabled, vec![true, false]);
}

#[derive(Clone, Debug)]
struct Entry {
    id: EffectId,
    radius: f32,
    enabled: bool,
    marker: f32,
}

fn attach(c: &mut Clip, id: EffectId, marker: f32) {
    let i = c.index_of(id).unwrap();
    c.view_mut().effect_keyframes.0
        .insert(format!("{i}:radius"), vec![key(marker)]);
    c.view_mut().param_modifiers.0.insert(
        ParamId::Effect {
            effect_index: i,
            param_name: "radius".into(),
        },
        ModifierStack {
            modifiers: vec![Modifier::Clamp {
                min: marker,
                max: marker + 1.0,
                enabled: true,
            }],
        },
    );
}
fn check(c: &mut Clip, expected: &[Entry]) {
    let actual: Vec<_> = c
        .effect_entries()
        .map(|(id, e, on)| (id, e.clone(), on))
        .collect();
    assert_eq!(actual.len(), expected.len());
    assert_eq!(c.effect_keyframes().len(), expected.len());
    assert_eq!(c.param_modifiers().len(), expected.len() + 1);
    assert!(c.param_modifiers().contains_key(&ParamId::Opacity));
    for (i, row) in expected.iter().enumerate() {
        assert_eq!(actual[i], (row.id, blur(row.radius), row.enabled));
        assert_eq!(c.index_of(row.id), Some(i));
        assert_eq!(c.effect(row.id), Some(&blur(row.radius)));
        assert_eq!(
            c.picture.view().effect_keyframes.0[&format!("{i}:radius")][0].value,
            row.marker
        );
        let stack = &c.picture.view().param_modifiers.0[&ParamId::Effect {
            effect_index: i,
            param_name: "radius".into(),
        }];
        match stack.modifiers[0] {
            Modifier::Clamp { min, .. } => assert_eq!(min, row.marker),
            _ => panic!("modifier moved to another effect"),
        }
    }
}
fn next(state: &mut u64) -> usize {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state as usize
}

/// Deterministic property-style operation sequences, with an independent
/// ordered model of identities, values, flags and animation ownership.
#[test]
fn random_edits_keep_values_flags_and_animation_with_their_ids() {
    for seed in 1..=32 {
        let mut random = seed;
        let mut c = clip();
        c.view_mut().param_modifiers.0
            .insert(ParamId::Opacity, ModifierStack::default());
        let mut rows: Vec<Entry> = vec![];
        let mut seen = HashSet::new();
        for step in 0..256 {
            let marker = (seed * 1000 + step) as f32;
            let op = next(&mut random) % 8;
            let mut retired = None;
            match op {
                0 | 1 => {
                    let at = if op == 0 {
                        rows.len()
                    } else {
                        next(&mut random) % (rows.len() + 1)
                    };
                    let id = if op == 0 {
                        c.push_effect(blur(marker))
                    } else {
                        c.insert_effect(at, blur(marker)).unwrap()
                    };
                    assert!(seen.insert(id), "an id was reused");
                    rows.insert(
                        at,
                        Entry {
                            id,
                            radius: marker,
                            enabled: true,
                            marker,
                        },
                    );
                    attach(&mut c, id, marker);
                }
                2 | 7 if !rows.is_empty() => {
                    let at = next(&mut random) % rows.len();
                    let row = rows.remove(at);
                    let removed = if op == 2 {
                        c.remove_effect(row.id)
                    } else {
                        c.remove_builtin_effect(at)
                    };
                    assert_eq!(removed, Some(blur(row.radius)));
                    retired = Some(row.id);
                }
                3 if !rows.is_empty() => {
                    let from = next(&mut random) % rows.len();
                    let to = next(&mut random) % rows.len();
                    let id = rows[from].id;
                    let moved = if step % 2 == 0 {
                        c.move_effect(id, to)
                    } else {
                        c.move_builtin_effect(from, to)
                    };
                    assert_eq!(moved, from != to);
                    let row = rows.remove(from);
                    rows.insert(to, row);
                }
                4 if !rows.is_empty() => {
                    let at = next(&mut random) % rows.len();
                    rows[at].enabled = !rows[at].enabled;
                    assert!(c.set_effect_enabled(rows[at].id, rows[at].enabled));
                }
                5 if !rows.is_empty() => {
                    let at = next(&mut random) % rows.len();
                    rows[at].radius = marker;
                    assert!(c.replace_effect(rows[at].id, blur(marker)));
                }
                6 => {
                    let n = next(&mut random) % 5;
                    retired = rows.first().map(|r| r.id);
                    c.set_effects((0..n).map(|i| blur(marker + i as f32)).collect());
                    rows.clear();
                    for (i, id) in ids(&mut c).into_iter().enumerate() {
                        assert!(seen.insert(id));
                        let value = marker + i as f32;
                        rows.push(Entry {
                            id,
                            radius: value,
                            enabled: true,
                            marker: value,
                        });
                        attach(&mut c, id, value);
                    }
                }
                _ => {}
            }
            if let Some(id) = retired {
                assert_eq!(c.index_of(id), None);
            }
            check(&mut c, &rows);
        }
    }
}

#[test]
fn insertion_preserves_occupied_and_orphan_animation_without_collisions() {
    let mut c = clip();
    let old = c.push_effect(blur(1.0));
    attach(&mut c, old, 10.0);
    c.view_mut().effect_keyframes.0
        .insert("1:radius".into(), vec![key(20.0)]);
    c.view_mut().param_modifiers.0.insert(
        ParamId::Effect {
            effect_index: 1,
            param_name: "radius".into(),
        },
        ModifierStack::default(),
    );
    c.insert_effect(0, blur(2.0)).unwrap();
    assert_eq!(c.index_of(old), Some(1));
    assert_eq!(c.picture.view().effect_keyframes.0["1:radius"][0].value, 10.0);
    assert_eq!(c.picture.view().effect_keyframes.0["2:radius"][0].value, 20.0);
    assert_eq!(c.param_modifiers().len(), 2);
    assert!(c.param_modifiers().contains_key(&ParamId::Effect {
        effect_index: 2,
        param_name: "radius".into()
    }));
}

#[test]
fn insertion_refuses_unshiftable_animation_without_partial_edits() {
    for modifier in [false, true] {
        let mut c = clip();
        let old = c.push_effect(blur(1.0));
        if modifier {
            c.view_mut().param_modifiers.0.insert(
                ParamId::Effect {
                    effect_index: usize::MAX,
                    param_name: "radius".into(),
                },
                ModifierStack::default(),
            );
        } else {
            c.view_mut().effect_keyframes.0
                .insert(format!("{}:radius", usize::MAX), vec![key(20.0)]);
        }
        let keys = c.effect_keyframes().clone();
        let modifiers = c.param_modifiers().clone();
        assert_eq!(c.insert_effect(0, blur(2.0)), None);
        assert_eq!(ids(&mut c), vec![old]);
        assert_eq!(c.picture.view().effect_list.builtin_effects, vec![blur(1.0)]);
        assert_eq!(c.picture.view().effect_list.builtin_effects_enabled, vec![true]);
        assert_eq!(c.effect_keyframes(), &keys);
        assert_eq!(c.param_modifiers().len(), modifiers.len());
        for (id, stack) in &modifiers {
            assert_eq!(
                serde_json::to_value(&c.picture.view().param_modifiers.0[id]).unwrap(),
                serde_json::to_value(stack).unwrap()
            );
        }
        // Appending shifts no occupied slot and remains available.
        c.push_effect(blur(3.0));
        assert_eq!(c.effect_keyframes(), &keys);
        assert_eq!(c.param_modifiers().len(), modifiers.len());
        for (id, stack) in &modifiers {
            assert_eq!(
                serde_json::to_value(&c.picture.view().param_modifiers.0[id]).unwrap(),
                serde_json::to_value(stack).unwrap()
            );
        }
    }
}

// ── #1076: access by id, and the undo snapshot ──

fn stack(amplitude: f32) -> ModifierStack {
    ModifierStack { modifiers: vec![Modifier::Wiggle { amplitude, frequency: 1.0, blend: Default::default(), enabled: true }] }
}

fn amplitude(s: &ModifierStack) -> f32 {
    match s.modifiers.first() {
        Some(Modifier::Wiggle { amplitude, .. }) => *amplitude,
        _ => f32::NAN,
    }
}

#[test]
fn access_by_id_keys_as_the_saved_form() {
    let mut c = clip();
    c.push_effect(blur(1.0));
    let b = c.push_effect(blur(2.0));
    assert_eq!(c.effect_id_at(1), Some(b));
    assert_eq!(c.effect_id_at(2), None);
    c.edit_effect(b, |e| {
        if let BuiltinEffect::GaussianBlur { radius } = e {
            *radius = 5.0;
        }
    });
    assert_eq!(c.picture.view().effect_list.builtin_effects[1], blur(5.0));
    c.edit_effect_keyframes_of(b, "radius", |v| v.push(key(3.0))).unwrap();
    assert_eq!(c.picture.view().effect_keyframes.0["1:radius"], vec![key(3.0)]);
    c.edit_effect_modifiers(b, "radius", |v| v.modifiers.extend(stack(0.5).modifiers)).unwrap();
    let at = ParamId::Effect { effect_index: 1, param_name: "radius".into() };
    assert_eq!(amplitude(&c.picture.view().param_modifiers.0[&at]), 0.5);
    assert_eq!(c.remove_effect_keyframes(b, "radius"), Some(vec![key(3.0)]));
    assert!(c.remove_effect_modifiers(b, "radius").is_some());
    assert!(c.effect_keyframes().is_empty() && !c.param_modifiers().contains_key(&at));
    // An id no longer on the list reaches nothing.
    c.remove_effect(b);
    assert!(c.edit_effect(b, |_| ()).is_none() && c.edit_effect_keyframes_of(b, "radius", |_| ()).is_none() && c.edit_effect_modifiers(b, "radius", |_| ()).is_none());
}

/// Any mix of list operations and edits by id keeps each effect's keys on
/// it: every effect is tagged by its radius, and its keyframe and modifier
/// carry the same number.
#[test]
fn keys_stay_on_their_effect_through_any_mix_of_operations() {
    let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
    let mut next = |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n as u64) as usize
    };
    for _round in 0..200 {
        let mut c = clip();
        let mut tag = 1.0_f32;
        for _ in 0..12 {
            let len = c.picture.view().effect_list.builtin_effects.len();
            match next(5) {
                0 | 1 => {
                    let id = c.insert_effect(next(len + 1), blur(tag)).unwrap();
                    c.edit_effect_keyframes_of(id, "radius", |v| v.push(key(tag))).unwrap();
                    c.edit_effect_modifiers(id, "radius", |v| v.modifiers.extend(stack(tag).modifiers)).unwrap();
                    tag += 1.0;
                }
                2 if len > 0 => {
                    let id = c.effect_id_at(next(len)).unwrap();
                    c.remove_effect(id);
                }
                3 if len > 1 => {
                    let id = c.effect_id_at(next(len)).unwrap();
                    c.move_effect(id, next(len));
                }
                4 if len > 0 => {
                    let id = c.effect_id_at(next(len)).unwrap();
                    let on = next(2) == 0;
                    c.set_effect_enabled(id, on);
                }
                _ => {}
            }
        }
        for (i, fx) in c.picture.view().effect_list.builtin_effects.iter().enumerate() {
            let BuiltinEffect::GaussianBlur { radius } = fx else { unreachable!() };
            assert_eq!(c.picture.view().effect_keyframes.0[&format!("{i}:radius")], vec![key(*radius)], "keyframe on effect {i}");
            let m = &c.picture.view().param_modifiers.0[&ParamId::Effect { effect_index: i, param_name: "radius".into() }];
            assert_eq!(amplitude(m), *radius, "modifier on effect {i}");
        }
        assert_eq!(c.effect_keyframes().len(), c.picture.view().effect_list.builtin_effects.len(), "no key left behind");
    }
}

/// Everything a clip holds about its effects, plus its own modifiers, as
/// one comparable string (modifier stacks have no equality of their own).
fn effect_view(c: &Clip) -> String {
    let mut keys: Vec<_> = c.effect_keyframes().iter().collect();
    keys.sort_by(|a, b| a.0.cmp(b.0));
    let mut mods: Vec<String> = c.param_modifiers().iter().map(|(k, v)| format!("{k:?}={v:?}")).collect();
    mods.sort();
    format!("{:?}|{:?}|{:?}|{:?}", c.picture.view().effect_list.builtin_effects, c.picture.view().effect_list.builtin_effects_enabled, keys, mods)
}

/// A snapshot, any edits, a restore: exactly the snapshot, ids included,
/// with the clip's own modifiers left as they were.
#[test]
fn a_restored_snapshot_is_the_snapshot_ids_and_all() {
    let mut c = clip();
    let a = c.push_effect(blur(1.0));
    let b = c.push_effect(blur(2.0));
    c.edit_effect_keyframes_of(b, "radius", |v| v.push(key(2.0))).unwrap();
    c.edit_effect_modifiers(a, "radius", |v| v.modifiers.extend(stack(1.0).modifiers)).unwrap();
    c.set_effect_enabled(a, false);
    c.edit_param_modifiers(|v| v.insert(ParamId::Opacity, stack(9.0)));
    let saved = effect_view(&c);
    let state = c.effects_state();

    c.remove_effect(a);
    c.push_effect(blur(7.0));
    c.edit_effect_keyframes_of(b, "radius", |v| v.push(key(9.0))).unwrap();
    c.edit_param_modifiers(|v| v.insert(ParamId::Opacity, stack(9.0)));
    c.restore_effects_state(state);

    assert_eq!(effect_view(&c), saved);
    assert_eq!(ids(&mut c), vec![a, b], "the same identities, not fresh ones");
    assert_eq!(c.index_of(b), Some(1));
}

/// A clone keeps its ids (the same clip), and so does a snapshot, restored
/// even onto another clip.
#[test]
fn a_snapshot_and_a_clone_keep_ids() {
    let mut c = clip();
    let a = c.push_effect(blur(1.0));
    let mut copy = c.clone();
    assert_eq!(copy.effect_id_at(0), Some(a), "a clone is the same clip");
    let state = c.effects_state();
    let mut other = clip();
    other.restore_effects_state(state);
    assert_eq!(other.effect_id_at(0), Some(a), "a restored snapshot carries them");
}

#[test]
fn existing_access_creates_nothing() {
    let mut c = clip();
    let a = c.push_effect(blur(1.0));
    assert!(c.edit_existing_effect_keyframes(a, "radius", |_| ()).is_none());
    assert!(c.edit_existing_effect_modifiers(a, "radius", |_| ()).is_none());
    assert!(c.effect_keyframes().is_empty() && c.param_modifiers().is_empty(), "asking created nothing");
    c.edit_effect_keyframes_of(a, "radius", |v| v.push(key(1.0))).unwrap();
    c.edit_existing_effect_keyframes(a, "radius", |v| v.push(key(2.0))).unwrap();
    assert_eq!(c.picture.view().effect_keyframes.0["0:radius"].len(), 2);
}

/// A fresh state restored onto a clip takes the values, effect modifiers
/// only, and new ids; the clip's own modifiers stay.
#[test]
fn a_fresh_state_brings_values_with_new_ids() {
    let mut src = clip();
    let a = src.push_effect(blur(4.0));
    src.edit_effect_keyframes_of(a, "radius", |v| v.push(key(4.0))).unwrap();
    let mut dst = clip();
    let old = dst.push_effect(blur(1.0));
    dst.edit_param_modifiers(|v| v.insert(ParamId::Opacity, stack(2.0)));
    let mods = vec![(ParamId::Effect { effect_index: 0, param_name: "radius".into() }, stack(4.0)), (ParamId::Scale, stack(8.0))];
    dst.restore_effects_state(EffectsState::fresh(src.picture.view().effect_list.builtin_effects.clone(), src.picture.view().effect_list.builtin_effects_enabled.clone(), src.effect_keyframes().clone(), mods));
    assert_eq!(dst.picture.view().effect_list.builtin_effects, vec![blur(4.0)]);
    assert_eq!(dst.picture.view().effect_keyframes.0["0:radius"], vec![key(4.0)]);
    assert!(dst.param_modifiers().contains_key(&ParamId::Opacity), "the clip's own modifier stays");
    assert!(!dst.param_modifiers().contains_key(&ParamId::Scale), "a non-effect modifier isn't part of the state");
    let new = dst.effect_id_at(0).unwrap();
    assert_ne!(new, old);
    assert_ne!(new, a);
}

/// A clone keeps every effect's id: a clip cloned, edited and written back
/// is the same clip with the same effects (#1095 review).
#[test]
fn a_clone_edited_and_written_back_keeps_every_id() {
    let mut c = clip();
    for r in [1.0, 2.0, 3.0] {
        c.push_effect(blur(r));
    }
    let before = ids(&mut c);
    let mut copy = c.clone();
    let middle = ids(&mut copy)[1];
    copy.replace_effect(middle, blur(9.0));
    copy.move_effect(before[0], 2);
    c = copy;
    let mut after = ids(&mut c);
    after.sort_by_key(|i| i.as_uuid());
    let mut want = before.clone();
    want.sort_by_key(|i| i.as_uuid());
    assert_eq!(after, want, "the same ids, moved");
    assert_eq!(c.effect(middle), Some(&blur(9.0)));
}

/// A clip that becomes a new clip (a new `ClipId`: a duplicate, a paste, a
/// split's half, an instance) gives its effects fresh ids, and so does an
/// explicit refresh.
#[test]
fn a_new_clip_gives_its_effects_fresh_ids() {
    let mut c = clip();
    c.push_effect(blur(1.0));
    c.push_effect(blur(2.0));
    let original: HashSet<EffectId> = ids(&mut c).into_iter().collect();
    let mut dup = c.clone();
    dup.id = klipp_core::ClipId::new();
    let fresh: HashSet<EffectId> = ids(&mut dup).into_iter().collect();
    assert_eq!(fresh.len(), 2);
    assert!(fresh.is_disjoint(&original), "a duplicate's effects are new effects");
    assert_eq!(dup.picture.view().effect_list.builtin_effects, c.picture.view().effect_list.builtin_effects, "with the same values");
    let again: HashSet<EffectId> = ids(&mut dup).into_iter().collect();
    assert_eq!(again, fresh, "and they last");
    c.refresh_effect_ids();
    assert!(ids(&mut c).iter().all(|i| !original.contains(i)), "a refresh gives new ones");
}
