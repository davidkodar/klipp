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
    }
}
fn ids(c: &mut Clip) -> Vec<EffectId> {
    c.effect_entries().map(|(id, _, _)| id).collect()
}

#[test]
fn reads_reconcile_direct_tail_edits_without_changing_saved_fields() {
    let mut c = clip();
    c.builtin_effects = vec![blur(1.0), blur(2.0)];
    c.builtin_effects_enabled = vec![false];
    let before = serde_json::to_value(&c).unwrap();
    let old = ids(&mut c);
    assert_eq!(serde_json::to_value(&c).unwrap(), before);
    assert_eq!(
        c.effect_entries().map(|(_, _, on)| on).collect::<Vec<_>>(),
        vec![false, true]
    );
    assert_eq!(ids(&mut c), old);
    c.builtin_effects.push(blur(3.0));
    let grown = ids(&mut c);
    assert_eq!(&grown[..2], &old);
    assert!(!old.contains(&grown[2]));
    c.builtin_effects.truncate(1);
    assert_eq!(c.index_of(old[1]), None);
    assert_eq!(ids(&mut c), vec![old[0]]);
    c.builtin_effects.push(blur(4.0));
    let fresh = ids(&mut c);
    assert_ne!(fresh[1], old[1]);
    assert_ne!(fresh[1], grown[2]);
}

#[test]
fn clone_and_load_rebuild_ids_and_ids_never_reach_json() {
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
    assert!(ids(&mut copy).iter().all(|id| !original.contains(id)));
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
    c.builtin_effects = vec![blur(1.0), blur(2.0), blur(3.0)];
    c.builtin_effects_enabled = vec![false];
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
        c.builtin_effects_enabled,
        vec![false],
        "old remove must not pad persisted flags"
    );
    assert!(c.move_builtin_effect(0, 1));
    assert_eq!(ids(&mut c), vec![original[1], original[0]]);
    assert_eq!(c.builtin_effects_enabled, vec![true, false]);
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
    c.effect_keyframes
        .insert(format!("{i}:radius"), vec![key(marker)]);
    c.param_modifiers.insert(
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
    assert_eq!(c.effect_keyframes.len(), expected.len());
    assert_eq!(c.param_modifiers.len(), expected.len() + 1);
    assert!(c.param_modifiers.contains_key(&ParamId::Opacity));
    for (i, row) in expected.iter().enumerate() {
        assert_eq!(actual[i], (row.id, blur(row.radius), row.enabled));
        assert_eq!(c.index_of(row.id), Some(i));
        assert_eq!(c.effect(row.id), Some(&blur(row.radius)));
        assert_eq!(
            c.effect_keyframes[&format!("{i}:radius")][0].value,
            row.marker
        );
        let stack = &c.param_modifiers[&ParamId::Effect {
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
        c.param_modifiers
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
    c.effect_keyframes
        .insert("1:radius".into(), vec![key(20.0)]);
    c.param_modifiers.insert(
        ParamId::Effect {
            effect_index: 1,
            param_name: "radius".into(),
        },
        ModifierStack::default(),
    );
    c.insert_effect(0, blur(2.0)).unwrap();
    assert_eq!(c.index_of(old), Some(1));
    assert_eq!(c.effect_keyframes["1:radius"][0].value, 10.0);
    assert_eq!(c.effect_keyframes["2:radius"][0].value, 20.0);
    assert_eq!(c.param_modifiers.len(), 2);
    assert!(c.param_modifiers.contains_key(&ParamId::Effect {
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
            c.param_modifiers.insert(
                ParamId::Effect {
                    effect_index: usize::MAX,
                    param_name: "radius".into(),
                },
                ModifierStack::default(),
            );
        } else {
            c.effect_keyframes
                .insert(format!("{}:radius", usize::MAX), vec![key(20.0)]);
        }
        let keys = c.effect_keyframes.clone();
        let modifiers = c.param_modifiers.clone();
        assert_eq!(c.insert_effect(0, blur(2.0)), None);
        assert_eq!(ids(&mut c), vec![old]);
        assert_eq!(c.builtin_effects, vec![blur(1.0)]);
        assert_eq!(c.builtin_effects_enabled, vec![true]);
        assert_eq!(c.effect_keyframes, keys);
        assert_eq!(c.param_modifiers.len(), modifiers.len());
        for (id, stack) in &modifiers {
            assert_eq!(
                serde_json::to_value(&c.param_modifiers[id]).unwrap(),
                serde_json::to_value(stack).unwrap()
            );
        }
        // Appending shifts no occupied slot and remains available.
        c.push_effect(blur(3.0));
        assert_eq!(c.effect_keyframes, keys);
        assert_eq!(c.param_modifiers.len(), modifiers.len());
        for (id, stack) in &modifiers {
            assert_eq!(
                serde_json::to_value(&c.param_modifiers[id]).unwrap(),
                serde_json::to_value(stack).unwrap()
            );
        }
    }
}
