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
//! * the pixel-sized parameters, against
//!   [`BuiltinEffect::at_render_scale`];
//! * whether it draws outside the picture, against
//!   [`klipp_effects::draws_outside_the_picture`].
//!
//! Every variant of `BuiltinEffect`, read from the enum's source, is
//! either described or on [`NOT_YET_DESCRIBED`], and a family's issue
//! (#1005-#1008) moves its names off that list as it lands.

use std::collections::{BTreeMap, BTreeSet};

use klipp_effects::registry::{effect_types, EffectType, ParamKind, Unit};
use klipp_effects::BuiltinEffect;
use serde_json::Value;

/// Variants with no description yet, by the family issue that adds them.
const NOT_YET_DESCRIBED: &[&str] = &[
    // #1005: blur, sharpen and detail.
    "GaussianBlur", "FocusBlur", "Sharpen", "DirectionalBlur", "RadialBlur", "TiltShift", "Denoise", "MedianFilter",
    "Dilate", "Erode", "Emboss", "EdgeDetect", "Mosaic",
    // #1006: light, glow and look.
    "Bloom", "Glow", "LightRays", "Diffusion", "LensFlare", "OuterGlow", "LightWrap", "AnamorphicStreak", "FilmHalation",
    "ChromaticAberration", "Vignette", "DropShadow",
    // #1007: geometry and frame.
    "Crop", "Transform", "RoundedCorners", "FillBars", "Mirror", "LensDistortion", "Letterbox", "CornerPin",
    // #1008: film, generator, key, temporal and plug-in.
    "FilmGrain", "OldFilm", "Vhs", "NoiseGenerator", "ChromaKey", "RollingShutterCorrection", "TemporalDenoise",
    "Deflicker", "VideoStabilization", "SpeedRamp", "Frei0r",
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

#[test]
fn the_sample_is_saved_under_the_tag_with_exactly_the_described_fields() {
    for t in effect_types() {
        let (tag, fields) = saved(&(t.sample)());
        assert_eq!(tag, t.tag, "{}: the sample is saved as {tag:?}", t.desc.id);
        let saved_fields: BTreeSet<&str> = fields.keys().map(String::as_str).collect();
        // A field the compositor writes every frame is not saved.
        let described: BTreeSet<&str> = t.desc.params.iter().filter(|p| !p.derived).map(|p| p.field).collect();
        assert_eq!(saved_fields, described, "{}: saved fields and described parameters differ", t.desc.id);
        let mut seen = BTreeSet::new();
        for p in t.desc.params {
            assert!(seen.insert(p.field), "{}: {} is described twice", t.desc.id, p.field);
        }
    }
}

#[test]
fn defaults_are_the_samples_values_and_inside_their_ranges() {
    for t in effect_types() {
        let (_, fields) = saved(&(t.sample)());
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
                    if let Some((lo, hi)) = p.soft {
                        assert!(lo <= p.default && p.default <= hi, "{at}: default {} outside the slider {lo}..{hi}", p.default);
                    }
                    if p.derived {
                        continue;
                    }
                    let v = fields[p.field].as_f64().unwrap_or_else(|| panic!("{at}: not a number"));
                    assert!((v as f32 - p.default).abs() < 1e-6, "{at}: default {} but the sample has {v}", p.default);
                }
                ParamKind::Bool => {
                    let v = fields[p.field].as_bool().unwrap_or_else(|| panic!("{at}: not a bool"));
                    assert_eq!(v, p.default != 0.0, "{at}: default and sample differ");
                }
                ParamKind::Choice(names) => {
                    let i = p.default as usize;
                    assert!(i < names.len(), "{at}: default index {i} has no choice");
                    let v = fields[p.field].as_str().unwrap_or_else(|| panic!("{at}: not a choice"));
                    assert!(v.eq_ignore_ascii_case(names[i]), "{at}: default {:?} but the sample has {v:?}", names[i]);
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

/// The sample with every number set to a distinct non-zero value, so a
/// parameter scaled from zero cannot hide.
fn probe(t: &EffectType) -> BuiltinEffect {
    let mut v = serde_json::to_value((t.sample)()).unwrap();
    let Value::Object(map) = &mut v else { unreachable!() };
    for (i, (k, val)) in map.iter_mut().enumerate() {
        if k == "type" {
            continue;
        }
        if val.is_u64() {
            *val = Value::from(3 + i as u64);
        } else if val.is_f64() {
            *val = Value::from(0.375 + i as f64);
        }
    }
    serde_json::from_value(v).expect("the probe deserialises")
}

#[test]
fn pixel_sized_parameters_are_the_ones_the_render_scales() {
    for t in effect_types() {
        let before = probe(t);
        let (_, a) = saved(&before);
        let (_, b) = saved(&before.clone().at_render_scale(2.0));
        let scaled: BTreeSet<&str> = a.iter().filter(|(k, v)| b.get(*k) != Some(*v)).map(|(k, _)| k.as_str()).collect();
        let described: BTreeSet<&str> = t.desc.params.iter().filter(|p| p.unit == Unit::CanvasPx).map(|p| p.field).collect();
        assert_eq!(described, scaled, "{}: CanvasPx parameters differ from what at_render_scale scales", t.desc.id);
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
