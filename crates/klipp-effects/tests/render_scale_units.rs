//! The registry's render-scale rules are the effect's own (node-graph M2
//! step 13, the acceptance spec's G1): every parameter of every registered
//! effect, scaled one by one with `registry::scaled_value`, gives exactly
//! what `BuiltinEffect::at_render_scale` gives, field for field and bit for
//! bit, at the render scales a preview and a proxy use.

use klipp_effects::registry::{effect_types, scaled_value, EffectType, ParamKind};
use klipp_effects::BuiltinEffect;
use serde_json::Value;

const SCALES: [f32; 4] = [1.0, 0.5, 0.333, 0.25];

/// The sample with each number set well inside its slider's range (70 %
/// of the way up, away from the values a scale leaves alone) and each
/// whole number set to `whole`, so the rounding and the floor at one are
/// reached.
fn probe(t: &EffectType, whole: u32) -> BuiltinEffect {
    let mut v = serde_json::to_value((t.sample)()).unwrap();
    for p in t.desc.params {
        let slot = match p.index {
            Some(i) => v.get_mut(p.field).and_then(|a| a.get_mut(i)),
            None => v.get_mut(p.field),
        };
        let Some(slot) = slot else { continue };
        match p.kind {
            ParamKind::Whole if slot.is_u64() => *slot = Value::from(whole),
            ParamKind::Float if slot.is_number() => {
                let (lo, hi) = p.soft.or(p.range).unwrap_or((0.0, 10.0));
                *slot = Value::from((lo + 0.7 * (hi - lo)) as f64);
            }
            _ => {}
        }
    }
    serde_json::from_value(v).expect("the probe deserialises")
}

/// `effect` with each parameter scaled by the registry's rule.
fn by_the_registry(t: &EffectType, effect: &BuiltinEffect, k: f32) -> BuiltinEffect {
    let mut v = serde_json::to_value(effect).unwrap();
    for p in t.desc.params.iter().filter(|p| p.scaling.by_the_effect()) {
        let slot = match p.index {
            Some(i) => v.get_mut(p.field).and_then(|a| a.get_mut(i)),
            None => v.get_mut(p.field),
        };
        let slot = slot.unwrap_or_else(|| panic!("{}.{} isn't in the saved effect", t.desc.id, p.field));
        let scaled = scaled_value(p, slot.as_f64().expect("a number") as f32, k);
        *slot = if p.kind == ParamKind::Whole { Value::from(scaled as u64) } else { Value::from(scaled as f64) };
    }
    serde_json::from_value(v).unwrap()
}

#[test]
fn native_scaling_equals_at_render_scale() {
    let mut scaled = 0;
    for t in effect_types() {
        for whole in [0, 1, 2, 7] {
            let effect = probe(t, whole);
            for k in SCALES {
                let want = effect.clone().at_render_scale(k);
                let got = by_the_registry(t, &effect, k);
                assert_eq!(serde_json::to_value(&got).unwrap(), serde_json::to_value(&want).unwrap(), "{} (whole {whole}) at {k}", t.desc.id);
                if want != effect {
                    scaled += 1;
                }
            }
        }
    }
    assert!(scaled > 0, "nothing was scaled: the probe reaches no pixel size");
}

/// The rules' edges, by hand.
#[test]
fn the_rules_edges() {
    use klipp_effects::BuiltinEffect as E;
    let find = |tag: &str, field: &str| {
        let t = effect_types().find(|t| t.tag == tag).unwrap();
        *t.desc.params.iter().find(|p| p.field == field).unwrap()
    };
    let mosaic = find("mosaic", "block_size");
    let dilate = find("dilate", "radius");
    let median = find("median_filter", "radius");
    assert_eq!(scaled_value(&mosaic, 2.0, 0.25), 1.0, "a block is never below a pixel");
    assert_eq!(E::Mosaic { block_size: 2.0 }.at_render_scale(0.25), E::Mosaic { block_size: 1.0 });
    assert_eq!(scaled_value(&dilate, 1.0, 0.333), 1.0, "a whole radius stays at one");
    assert_eq!(scaled_value(&dilate, 0.0, 0.333), 0.0, "and zero stays zero");
    assert_eq!(scaled_value(&median, 7.0, 0.333), 2.0, "round(2.331)");
    let grain = find("film_grain", "size");
    assert_eq!(scaled_value(&grain, 1.5, 0.25), 1.5, "a size per 1080 lines scales itself");
    for k in [1.0, f32::NAN, 0.0, -1.0] {
        assert_eq!(scaled_value(&find("gaussian_blur", "radius"), 6.0, k), 6.0, "{k} leaves it");
    }
}
