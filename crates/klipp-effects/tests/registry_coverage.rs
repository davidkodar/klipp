//! The effect registry describes every built-in effect, and describes it
//! the way the code behaves (#1003).
//!
//! Each description is checked against the code it summarises, not
//! against itself:
//!
//! * the saved form of its sample effect: the tag, and the fields, which
//!   must be exactly the described parameters;
//! * every default, inside its ranges, and equal to the sample's value;
//! * the keyframable parameters, against [`BuiltinEffect::animate`];
//! * the parameters the effect scales to the render size, against
//!   [`BuiltinEffect::at_render_scale`], and every pixel-sized
//!   parameter naming who scales it;
//! * whether it draws outside the picture, against
//!   [`klipp_effects::draws_outside_the_picture`].
//!
//! Every variant of `BuiltinEffect`, read from the enum's source, is
//! either described or on [`NOT_YET_DESCRIBED`], and a family's issue
//! (#1005-#1008) moves its names off that list as it lands.

use std::collections::{BTreeMap, BTreeSet};

use klipp_effects::registry::{effect_types, EffectType, ParamDesc, ParamKind, Scaling, Unit};
use klipp_effects::BuiltinEffect;
use serde_json::Value;

/// Variants with no description yet, by the family issue that adds them.
const NOT_YET_DESCRIBED: &[&str] = &[
];

/// The variant names of `pub enum BuiltinEffect`, read from its source so
/// that a variant added without a description fails here.
fn declared_variants() -> BTreeSet<String> {
    let src = include_str!("../src/lib.rs");
    let start = src.find("pub enum BuiltinEffect {").expect("the enum");
    let body = &src[start..];
    let open = body.find('{').unwrap();
    let mut depth = 0;
    let mut end = open;
    for (i, c) in body[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = open + i;
                    break;
                }
            }
            _ => {}
        }
    }
    body[open + 1..end]
        .lines()
        .filter_map(|l| {
            let rest = l.strip_prefix("    ")?;
            if !rest.chars().next()?.is_ascii_uppercase() {
                return None;
            }
            let name: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
            Some(name)
        })
        .collect()
}

/// The variant name of an effect, from its `Debug` form.
fn variant_name(e: &BuiltinEffect) -> String {
    format!("{e:?}").chars().take_while(|c| c.is_ascii_alphanumeric()).collect()
}

/// The saved form of an effect: its tag and its fields.
fn saved(e: &BuiltinEffect) -> (String, BTreeMap<String, Value>) {
    let Value::Object(mut map) = serde_json::to_value(e).expect("an effect serialises") else {
        panic!("{e:?} did not serialise to an object");
    };
    let tag = map.remove("type").and_then(|t| t.as_str().map(str::to_owned)).expect("a tag");
    (tag, map.into_iter().collect())
}

/// `t`'s sample as saved, with each described field that saving skips at
/// its default (a crop without a shape saves as it did before it had one,
/// #2930) filled in with the default the registry describes, and checked:
/// the filled-in fields read back as the sample itself, so skipping them
/// loses nothing and the description is the field's real default.
fn saved_with_defaults(t: &EffectType) -> (String, BTreeMap<String, Value>) {
    let sample = (t.sample)();
    let (tag, mut fields) = saved(&sample);
    let mut filled = Vec::new();
    let skipped: Vec<&ParamDesc> = t.desc.params.iter().filter(|p| !p.derived && p.index.is_none() && !fields.contains_key(p.field)).collect();
    for p in skipped {
        let v = match p.kind {
            ParamKind::Bool => Value::Bool(p.default != 0.0),
            ParamKind::Whole => serde_json::json!(p.default as i64),
            ParamKind::Float => serde_json::json!(p.default),
            _ => continue,
        };
        fields.insert(p.field.to_string(), v);
        filled.push(p.field);
    }
    if !filled.is_empty() {
        let mut object: serde_json::Map<String, Value> = fields.clone().into_iter().collect();
        object.insert("type".into(), Value::String(tag.clone()));
        let back: BuiltinEffect = serde_json::from_value(Value::Object(object)).unwrap_or_else(|e| panic!("{}: {e}", t.desc.id));
        assert_eq!(back, sample, "{}: {filled:?}, skipped when saved, are not at the defaults the registry describes", t.desc.id);
    }
    (tag, fields)
}

#[test]
fn every_variant_is_described_or_listed_as_not_yet() {
    let declared = declared_variants();
    assert!(declared.len() > 40, "the variant scan found only {declared:?}");
    let described: BTreeSet<String> = effect_types().map(|t| variant_name(&(t.sample)())).collect();
    let pending: BTreeSet<String> = NOT_YET_DESCRIBED.iter().map(|s| s.to_string()).collect();

    let both: Vec<_> = described.intersection(&pending).collect();
    assert!(both.is_empty(), "described, and still on NOT_YET_DESCRIBED (take them off): {both:?}");
    let known: BTreeSet<String> = described.union(&pending).cloned().collect();
    let missing: Vec<_> = declared.difference(&known).collect();
    assert!(missing.is_empty(), "variants neither described nor listed as not yet: {missing:?}");
    let stale: Vec<_> = known.difference(&declared).collect();
    assert!(stale.is_empty(), "names that are not variants of BuiltinEffect: {stale:?}");
}

#[test]
fn ids_and_tags_are_unique() {
    let mut ids = BTreeSet::new();
    let mut tags = BTreeSet::new();
    for t in effect_types() {
        assert!(t.desc.id.starts_with("klipp.fx."), "{}: an effect's id starts with klipp.fx.", t.desc.id);
        assert!(ids.insert(t.desc.id), "{} is described twice", t.desc.id);
        assert!(tags.insert(t.tag), "tag {} is described twice", t.tag);
    }
}

/// The saved value a parameter describes: the field, or one element of it.
fn value_of<'a>(fields: &'a BTreeMap<String, Value>, p: &ParamDesc) -> &'a Value {
    let v = &fields[p.field];
    match p.index {
        None => v,
        Some(i) => v.get(i).unwrap_or_else(|| panic!("{}[{i}]: no such element", p.field)),
    }
}

#[test]
fn the_sample_is_saved_under_the_tag_with_exactly_the_described_fields() {
    for t in effect_types() {
        let (tag, fields) = saved_with_defaults(t);
        assert_eq!(tag, t.tag, "{}: the sample is saved as {tag:?}", t.desc.id);
        let saved_fields: BTreeSet<&str> = fields.keys().map(String::as_str).collect();
        // A field the compositor writes every frame is not saved.
        let described: BTreeSet<&str> = t.desc.params.iter().filter(|p| !p.derived).map(|p| p.field).collect();
        assert_eq!(saved_fields, described, "{}: saved fields and described parameters differ", t.desc.id);
        // Each field is described once, or once per element, never both.
        let mut seen = BTreeSet::new();
        for p in t.desc.params {
            assert!(seen.insert((p.field, p.index)), "{}: {}{:?} is described twice", t.desc.id, p.field, p.index);
        }
        for p in t.desc.params.iter().filter(|p| p.index.is_some()) {
            assert!(!seen.contains(&(p.field, None)), "{}: {} is described both whole and by element", t.desc.id, p.field);
            if !p.derived {
                let len = fields[p.field].as_array().map_or(0, Vec::len);
                let elements = t.desc.params.iter().filter(|q| q.field == p.field).count();
                assert_eq!(elements, len, "{}: {} has {len} elements, {elements} described", t.desc.id, p.field);
            }
        }
    }
}

#[test]
fn defaults_are_the_samples_values_and_inside_their_ranges() {
    for t in effect_types() {
        let (_, fields) = saved_with_defaults(t);
        for p in t.desc.params {
            let at = format!("{}.{}", t.desc.id, p.field);
            if let (Some((lo, hi)), Some((slo, shi))) = (p.range, p.soft) {
                assert!(lo <= slo && shi <= hi, "{at}: the slider reaches outside the accepted range");
            }
            match p.kind {
                ParamKind::Float | ParamKind::Whole => {
                    if let Some((lo, hi)) = p.range {
                        assert!(lo <= p.default && p.default <= hi, "{at}: default {} outside {lo}..{hi}", p.default);
                    }
                    // The slider's range is the inspector's hint, and a reset can
                    // sit outside it (a transform's perspective resets to 0,
                    // "unset", read as 1); the accepted range above cannot.
                    if p.derived {
                        continue;
                    }
                    let v = value_of(&fields, p).as_f64().unwrap_or_else(|| panic!("{at}: not a number"));
                    assert!((v as f32 - p.default).abs() < 1e-6, "{at}: default {} but the sample has {v}", p.default);
                }
                ParamKind::Bool => {
                    let v = value_of(&fields, p).as_bool().unwrap_or_else(|| panic!("{at}: not a bool"));
                    assert_eq!(v, p.default != 0.0, "{at}: default and sample differ");
                }
                ParamKind::Choice(names) => {
                    let i = p.default as usize;
                    assert!(i < names.len(), "{at}: default index {i} has no choice");
                    // Saved by name (an enum) or by index (a u32).
                    match value_of(&fields, p) {
                        Value::String(v) => assert!(v.eq_ignore_ascii_case(names[i]), "{at}: default {:?} but the sample has {v:?}", names[i]),
                        Value::Number(n) => assert_eq!(n.as_u64(), Some(i as u64), "{at}: default index {i} but the sample has {n}"),
                        other => panic!("{at}: a choice saved as {other}"),
                    }
                }
                ParamKind::Colour | ParamKind::Other => {
                    assert!(p.default.is_nan(), "{at}: a colour or other value has no single default number");
                    assert!(!p.keyframable, "{at}: only numbers can be keyframed");
                }
            }
        }
    }
}

#[test]
fn keyframable_parameters_are_the_ones_the_render_animates() {
    for t in effect_types() {
        let mut animated = BTreeSet::new();
        (t.sample)().animate(&mut |name, v| {
            animated.insert(name);
            v
        });
        let described: BTreeSet<&str> = t.desc.params.iter().filter(|p| p.keyframable).map(|p| p.key).collect();
        assert_eq!(described, animated, "{}: keyframable parameters differ from BuiltinEffect::animate", t.desc.id);
    }
}

/// The sample with every number, array elements included, set to a
/// distinct non-zero value, so a parameter scaled from zero cannot hide.
fn probe(t: &EffectType) -> BuiltinEffect {
    let mut v = serde_json::to_value((t.sample)()).unwrap();
    let Value::Object(map) = &mut v else { unreachable!() };
    let mut n = 0u64;
    let mut fresh = |val: &mut Value| {
        n += 1;
        if val.is_u64() {
            *val = Value::from(2 + n);
        } else if val.is_f64() {
            *val = Value::from(0.375 + n as f64);
        }
    };
    for (k, val) in map.iter_mut() {
        if k == "type" {
            continue;
        }
        match val {
            Value::Array(items) if items.iter().all(Value::is_f64) => items.iter_mut().for_each(&mut fresh),
            other => fresh(other),
        }
    }
    serde_json::from_value(v).expect("the probe deserialises")
}

/// The (field, element) places whose saved values differ between two
/// saved forms.
fn changed(a: &BTreeMap<String, Value>, b: &BTreeMap<String, Value>) -> BTreeSet<(String, Option<usize>)> {
    let mut out = BTreeSet::new();
    for (k, va) in a {
        let vb = &b[k];
        match (va, vb) {
            (Value::Array(xa), Value::Array(xb)) if xa.len() == xb.len() => {
                out.extend(xa.iter().zip(xb).enumerate().filter(|(_, (x, y))| x != y).map(|(i, _)| (k.clone(), Some(i))));
            }
            _ if va != vb => {
                out.insert((k.clone(), None));
            }
            _ => {}
        }
    }
    out
}

#[test]
fn parameters_scaled_by_the_effect_are_the_ones_at_render_scale_scales() {
    for t in effect_types() {
        let before = probe(t);
        let (_, a) = saved(&before);
        let (_, b) = saved(&before.clone().at_render_scale(2.0));
        let described: BTreeSet<(String, Option<usize>)> =
            t.desc.params.iter().filter(|p| p.scaling.by_the_effect()).map(|p| (p.field.to_string(), p.index)).collect();
        assert_eq!(described, changed(&a, &b), "{}: AtRenderScale parameters differ from what at_render_scale scales", t.desc.id);
    }
}

#[test]
fn every_pixel_sized_parameter_names_who_scales_it() {
    for t in effect_types() {
        for p in t.desc.params {
            let at = format!("{}.{}", t.desc.id, p.key);
            match p.unit {
                Unit::CanvasPx => assert_ne!(p.scaling, Scaling::None, "{at}: canvas pixels that nothing scales"),
                _ => assert_eq!(p.scaling, Scaling::None, "{at}: scaled, but not measured in canvas pixels"),
            }
        }
    }
}

#[test]
fn drawing_outside_the_picture_agrees_with_the_compositor() {
    for t in effect_types() {
        assert_eq!(
            t.desc.draws_outside,
            klipp_effects::draws_outside_the_picture(&(t.sample)()),
            "{}: draws_outside differs from draws_outside_the_picture",
            t.desc.id
        );
    }
}
