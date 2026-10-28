//! The guard on effect parameter values (#2995): every number an effect
//! takes has a safe range, and a value from anywhere is held inside it.

use klipp_effects::registry::{effect_type, effect_types, guard_entry, guard_number, NotFinite, ParamKind};

/// Every number of every effect has a hard range, so nothing written to
/// it can make a render run or allocate without bound.
#[test]
fn every_effect_number_has_a_hard_range() {
    let missing: Vec<String> = effect_types()
        .flat_map(|t| t.desc.params.iter().map(move |d| (t.tag, d)))
        .filter(|(_, d)| matches!(d.kind, ParamKind::Float | ParamKind::Whole) && d.hard().is_none())
        .map(|(tag, d)| format!("{tag}.{}", d.key))
        .collect();
    assert!(missing.is_empty(), "numbers with no hard range (give them .within()): {missing:?}");
}

/// The default sits inside the hard range, so a reset is never clamped.
#[test]
fn every_default_is_inside_its_hard_range() {
    for t in effect_types() {
        for d in t.desc.params {
            if let Some((lo, hi)) = d.hard() {
                assert!(lo <= hi && (lo..=hi).contains(&d.default), "{}.{}: default {} outside {lo}..={hi}", t.tag, d.key, d.default);
            }
        }
    }
}

/// A number past the range is clamped, a not-number is refused for an
/// edit and defaulted for a file, and a key that names no number passes.
#[test]
fn the_guard_clamps_refuses_and_defaults() {
    let t = effect_type("gaussian_blur").expect("blur is registered");
    let d = t.desc.params.iter().find(|d| d.hard().is_some()).expect("blur has a number");
    let (lo, hi) = d.hard().unwrap();
    let (now, g) = guard_number(t.tag, d.key, 1e30, NotFinite::Refuse).unwrap();
    assert_eq!(now, hi);
    assert_eq!(g.unwrap().now, hi);
    assert_eq!(guard_number(t.tag, d.key, -1e30, NotFinite::Refuse).unwrap().0, lo);
    assert!(guard_number(t.tag, d.key, f64::NAN, NotFinite::Refuse).is_err());
    assert_eq!(guard_number(t.tag, d.key, f64::INFINITY, NotFinite::Default).unwrap().0, d.default);
    let inside = (lo + hi) / 2.0;
    assert_eq!(guard_number(t.tag, d.key, inside as f64, NotFinite::Refuse).unwrap(), (inside, None));
    assert_eq!(guard_number(t.tag, "no_such_key", 1e30, NotFinite::Refuse).unwrap().1, None);
}

/// Over a whole saved entry: every number back inside, a null (how serde
/// writes NaN) given its default, and the entry still reads.
#[test]
fn a_hostile_entry_is_made_safe() {
    for t in effect_types() {
        let mut entry = serde_json::to_value((t.sample)()).unwrap();
        let mut planted = 0;
        for d in t.desc.params.iter().filter(|d| d.hard().is_some() && !d.derived) {
            let slot = match d.index {
                Some(i) => entry.get_mut(d.field).and_then(|a| a.get_mut(i)),
                None => entry.get_mut(d.field),
            };
            if let Some(slot) = slot {
                *slot = if planted % 2 == 0 { serde_json::json!(1e300) } else { serde_json::Value::Null };
                planted += 1;
            }
        }
        guard_entry(&mut entry, NotFinite::Default).unwrap();
        for d in t.desc.params.iter().filter(|d| d.hard().is_some() && !d.derived) {
            let v = match d.index {
                Some(i) => entry.get(d.field).and_then(|a| a.get(i)),
                None => entry.get(d.field),
            };
            if let Some(v) = v.and_then(|v| v.as_f64()) {
                let (lo, hi) = d.hard().unwrap();
                assert!((lo as f64..=hi as f64).contains(&v), "{}.{} = {v} outside {lo}..={hi}", t.tag, d.key);
            }
        }
        let read: Result<klipp_effects::BuiltinEffect, _> = serde_json::from_value(entry.clone());
        assert!(read.is_ok(), "{} no longer reads after the guard: {entry}", t.tag);
    }
}

/// Every effect as a reset leaves it passes the guard untouched, refused
/// nothing: no number of a fresh effect is out of range or missing.
#[test]
fn a_fresh_effect_passes_the_guard_untouched() {
    for t in effect_types() {
        let mut entry = serde_json::to_value((t.sample)()).unwrap();
        let changed = guard_entry(&mut entry, NotFinite::Refuse).unwrap_or_else(|e| panic!("{}: {e}", t.tag));
        assert!(changed.is_empty(), "{}: {changed:?}", t.tag);
    }
}
