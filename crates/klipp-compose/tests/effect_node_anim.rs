//! An effect node's own animation gives every keyframable parameter the
//! value the list's index-keyed keys gave it (node-graph M2 step 14).
//! The clip is lifted (`picture_v2`), its effect node read through
//! `EffectNodeAnim`, and the result compared, to the bit, with today's
//! read: the `"{index}:{name}"` keys and the `ParamId::Effect` stacks.

use klipp_compose::eval::{eval_context, eval_effect_with_modifiers, EffectNodeAnim};
use klipp_core::{Interpolation, Keyframe, ModifierStack, ParamId, RationalTime, Timecode};
use klipp_effects::BuiltinEffect as E;
use klipp_timeline::Clip;
use klipp_timeline::test_clip;

const TIMES: [f64; 6] = [0.0, 0.3, 0.5, 1.0, 1.7, 3.2];

fn tc(secs: f64) -> Timecode {
    RationalTime::from_secs_f64(secs, 24)
}

fn key(time_secs: f64, value: f32) -> Keyframe {
    Keyframe { time_secs, value, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }
}

/// A clip holding `effects`, with `keys` under `"{slot}:{name}"` and
/// `stacks` under `ParamId::Effect`.
fn clip(effects: Vec<E>, keys: &[(usize, &str, Vec<Keyframe>)], stacks: &[(usize, &str, ModifierStack)]) -> Clip {
    let mut clip = Clip {
        picture: klipp_timeline::ClipPictureParts { builtin_effects: effects, ..Default::default() }.into(),
        ..Clip::gap(tc(0.0), tc(4.0))
    };
    clip.edit_effect_keyframes(|m| {
        for (slot, name, k) in keys {
            m.insert(format!("{slot}:{name}"), k.clone());
        }
    });
    clip.edit_param_modifiers(|m| {
        for (slot, name, s) in stacks {
            m.insert(ParamId::Effect { effect_index: *slot, param_name: (*name).to_owned() }, s.clone());
        }
    });
    clip
}

/// The lifted node of the effect at `slot`.
fn node(clip: &Clip, slot: u32) -> klipp_graph_format::Node {
    let lifted = clip.picture_v2().expect("the clip lifts");
    lifted
        .picture
        .graph
        .nodes
        .iter()
        .find(|n| n.legacy.as_ref().is_some_and(|l| l.slot == Some(slot) && l.payload.get("type").is_some()))
        .cloned()
        .unwrap_or_else(|| panic!("no effect node for slot {slot} in {:#?}", lifted.picture.graph.nodes))
}

/// The effect at `slot` at clip time `t`, as the list's keys give it.
fn by_index(clip: &Clip, slot: usize, t: f64) -> serde_json::Value {
    let mut e = test_clip::effects(clip)[slot].clone();
    let ctx = eval_context(clip, tc(t), 24.0);
    e.animate(&mut |name, v| {
        let kfs = clip.effect_keyframes().get(&format!("{slot}:{name}")).map(|v| v.as_slice()).unwrap_or(&[]);
        eval_effect_with_modifiers(kfs, clip.param_modifiers(), slot, name, t, v, &ctx)
    });
    serde_json::to_value(e).unwrap()
}

/// The effect at `slot` at clip time `t`, as its lifted node gives it.
fn by_node(clip: &Clip, slot: usize, t: f64) -> serde_json::Value {
    let anim = EffectNodeAnim::of(&node(clip, slot as u32)).expect("the node's animation reads");
    let mut e = test_clip::effects(clip)[slot].clone();
    anim.animate(&mut e, t, &eval_context(clip, tc(t), 24.0));
    serde_json::to_value(e).unwrap()
}

/// Each parameter's stored value, by the name `animate` asks for it.
fn values(e: &E) -> Vec<(&'static str, f32)> {
    let mut out = Vec::new();
    e.clone().animate(&mut |name, v| {
        out.push((name, v));
        v
    });
    out
}

/// A second key far enough from `v` to move it, whole parameters included.
fn moved(v: f32) -> f32 {
    v + 1.0 + v.abs() * 0.5
}

/// Every keyframable parameter of every registered effect, keyed alone
/// on its own clip, resolves from the node exactly as from the list.
/// Opacity is the one effect the render doesn't animate through its
/// keys (its keys join the clip's opacity), so it is left out, as
/// `clip_effects` leaves it out.
#[test]
fn every_keyframable_parameter_resolves_from_its_node() {
    let mut covered = 0;
    let mut keyframable = 0;
    for ty in klipp_effects::registry::effect_types() {
        let sample = (ty.sample)();
        // The registry's keyframable keys are the names `animate` asks
        // for, none skipped.
        let mut declared: Vec<&str> = ty.desc.params.iter().filter(|p| p.keyframable).map(|p| p.key).collect();
        let mut asked: Vec<&str> = values(&sample).into_iter().map(|(n, _)| n).collect();
        declared.sort_unstable();
        declared.dedup();
        asked.sort_unstable();
        asked.dedup();
        assert_eq!(declared, asked, "{}: the registry's keyframable keys and animate's names differ", ty.tag);
        if matches!(sample, E::Opacity { .. }) {
            continue;
        }
        keyframable += asked.len();
        for (name, v) in values(&sample) {
            let c = clip(vec![sample.clone()], &[(0, name, vec![key(0.0, v), key(1.0, moved(v))])], &[]);
            for t in TIMES {
                assert_eq!(by_node(&c, 0, t), by_index(&c, 0, t), "{} {name} at {t}", ty.tag);
            }
            // The key took: at its second key the effect isn't the sample.
            assert_ne!(by_node(&c, 0, 1.0), serde_json::to_value(&sample).unwrap(), "{} {name}: the key changed nothing", ty.tag);
            covered += 1;
        }
    }
    assert_eq!(covered, keyframable);
    assert!(covered > 100, "only {covered} parameters checked");
}

/// A key saved under an alias (ChromaKey's `spill`, which drives
/// `soft_edge`) lands on the field it always drove, every alias of the
/// lift's table included.
#[test]
fn aliased_keys_land_on_their_field() {
    for (tag, from, to) in klipp_graph_lift::names::KEYFRAME_ALIASES {
        let ty = klipp_effects::registry::effect_types().find(|t| t.tag == tag).expect("the alias names a registered effect");
        let sample = (ty.sample)();
        let v = values(&sample).into_iter().find(|(n, _)| *n == from).expect("animate asks for the alias").1;
        let c = clip(vec![sample], &[(0, from, vec![key(0.0, v), key(1.0, moved(v))])], &[]);
        let n = node(&c, 0);
        assert!(n.anim.contains_key(&klipp_graph_format::ParamKey::new(to)), "{tag}: the node animates {to}");
        for t in TIMES {
            assert_eq!(by_node(&c, 0, t), by_index(&c, 0, t), "{tag} {from} at {t}");
        }
    }
}

/// The corner pin's corners are keyed element by element, under the
/// registry's element keys.
#[test]
fn element_keys_land_on_their_corner() {
    let ty = klipp_effects::registry::effect_types().find(|t| t.tag == "corner_pin").expect("a corner pin");
    let sample = (ty.sample)();
    let keys: Vec<(usize, &str, Vec<Keyframe>)> =
        values(&sample).into_iter().enumerate().map(|(i, (n, v))| (0, n, vec![key(0.0, v), key(1.0 + i as f64 * 0.25, v + 0.1)])).collect();
    assert!(keys.len() >= 8, "{} corner keys", keys.len());
    let c = clip(vec![sample], &keys, &[]);
    for t in TIMES {
        assert_eq!(by_node(&c, 0, t), by_index(&c, 0, t), "at {t}");
    }
}

/// One stack per modifier kind, on a keyed and on an unkeyed parameter.
fn stacks() -> Vec<(&'static str, ModifierStack)> {
    let one = |json: &str| ModifierStack { modifiers: vec![serde_json::from_str(json).expect("a modifier")] };
    vec![
        ("oscillate", one(r#"{"type":"oscillate","amplitude":0.5,"frequency":2.0,"phase":0.25,"waveform":"Triangle"}"#)),
        ("wiggle", one(r#"{"type":"wiggle","amplitude":0.7,"frequency":3.0,"blend":"Multiply"}"#)),
        ("throw", one(r#"{"type":"throw","velocity":1.5,"acceleration":-0.25}"#)),
        ("ramp", one(r#"{"type":"ramp","start_value":0.0,"end_value":2.0,"start_norm":0.1,"end_norm":0.9,"blend":"Replace"}"#)),
        ("loop", one(r#"{"type":"loop","period_secs":1.25}"#)),
        ("ping_pong", one(r#"{"type":"ping_pong","period_secs":0.8}"#)),
        ("clamp", one(r#"{"type":"clamp","min":-0.5,"max":2.5}"#)),
    ]
}

/// A parameter's modifier stack rides with it onto its node, and the
/// stack on another slot's parameter of the same name stays there.
#[test]
fn modifiers_ride_with_their_parameter() {
    let blur = E::GaussianBlur { radius: 3.0 };
    let vignette = E::Vignette { amount: 0.5, size: 0.5, softness: 0.5, roundness: 0.5 };
    for (kind, stack) in stacks() {
        let c = clip(
            vec![blur.clone(), vignette.clone(), blur.clone()],
            &[(0, "radius", vec![key(0.0, 3.0), key(2.0, 9.0)])],
            &[(0, "radius", stack.clone()), (1, "amount", stack.clone())],
        );
        for t in TIMES {
            for slot in 0..3 {
                assert_eq!(by_node(&c, slot, t), by_index(&c, slot, t), "{kind}: slot {slot} at {t}");
            }
        }
        assert!(EffectNodeAnim::of(&node(&c, 2)).unwrap().is_empty(), "{kind}: the third effect animates nothing");
    }
}

/// Whole parameters (counts, radii in pixels) round between keys as
/// `animate` always rounded them.
#[test]
fn whole_parameters_round_as_before() {
    let cases = [
        (E::Posterize { levels: 4.0 }, "levels", 2.0, 7.5),
        (E::MedianFilter { radius: 1 }, "radius", 1.0, 2.6),
    ];
    for (effect, name, from, to) in cases {
        let c = clip(vec![effect.clone()], &[(0, name, vec![key(0.0, from), key(1.0, to)])], &[]);
        for t in [0.1, 0.33, 0.5, 0.71, 0.9] {
            assert_eq!(by_node(&c, 0, t), by_index(&c, 0, t), "{name} at {t}");
        }
    }
}
