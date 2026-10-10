//! The placed kinds take graph ops (M4 step 12b-0): an edit that adds,
//! removes, moves or switches a Transform, an Opacity, a Speed Ramp or a
//! Temporal Noise Reduction goes through the clip's picture, and leaves
//! the roles the lift gives them exactly as the lift gives them to the
//! edited list. The clip's opacity is its last enabled Opacity, other
//! enabled ones multiply; its placement is its first enabled Transform,
//! the rest kept off; a blending Speed Ramp and the denoiser run at the
//! chain's head.
//!
//! Each case: a clip with list `before`, edited through
//! `apply_effect_edits`, against a clip given the edited list whole
//! (`after`) and lifted. The two pictures must hold the same roles: the
//! placement and its keys, the opacity and its keys, and the chain's
//! nodes in order with their types, kinds and flags. Node ids aren't
//! compared: an edit keeps its nodes', a whole list makes new ones.

use klipp_core::{MediaId, RationalTime};
use klipp_effects::BuiltinEffect as E;
use klipp_graph_format::lifted::types as lt;
use klipp_timeline::{Clip, EffectEdit};

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn clip(effects: Vec<E>, enabled: Vec<bool>) -> Clip {
    let mut c = Clip::media(MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    c.set_effect_list(effects, enabled);
    c
}

fn transform(x: f32) -> E {
    E::Transform { position_x: x, position_y: 0.0, scale_x: 1.0, scale_y: 1.0, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None }
}

fn opacity(v: f32) -> E {
    E::Opacity { value: v }
}

fn blur() -> E {
    E::GaussianBlur { radius: 2.0 }
}

/// What the lift makes of a clip's placed kinds, without node ids.
fn roles(c: &Clip) -> serde_json::Value {
    let p = c.picture_v2().expect("the clip's picture");
    let pic = &p.picture;
    let chain: Vec<serde_json::Value> = pic
        .graph
        .nodes
        .iter()
        .filter(|n| !matches!(n.type_id.as_str(), lt::GROUP))
        .map(|n| {
            // A node's kind: an effect's tag, a placement's mode.
            let kind = n.legacy.as_ref().and_then(|l| l.payload.get("type")).cloned().unwrap_or(serde_json::Value::Null);
            let mode = n.params.get(&klipp_graph_format::ParamKey::from("mode")).map(|m| format!("{m:?}"));
            serde_json::json!({ "type": n.type_id.as_str(), "kind": kind, "mode": mode, "enabled": n.enabled })
        })
        .collect();
    serde_json::json!({
        "placement": serde_json::to_value(&pic.placement).unwrap(),
        "placement_anim": serde_json::to_value(&pic.placement_anim).unwrap(),
        "opacity": pic.opacity,
        "opacity_anim": serde_json::to_value(&pic.opacity_anim).unwrap(),
        "chain": chain,
    })
}

/// `before` edited by `edits` holds the roles `after` lifts to.
fn holds(name: &str, before: (Vec<E>, Vec<bool>), edits: &[EffectEdit], after: (Vec<E>, Vec<bool>)) {
    let mut edited = clip(before.0, before.1);
    edited
        .apply_effect_edits(edits)
        .unwrap_or_else(|| panic!("{name}: the placed kinds take graph ops"))
        .unwrap_or_else(|e| panic!("{name}: the picture refused the edit: {e}"));
    let whole = clip(after.0.clone(), after.1.clone());
    assert_eq!(roles(&edited), roles(&whole), "{name}");
    assert_eq!((edited.builtin_effects().to_vec(), edited.builtin_effects_enabled().to_vec()), after, "{name}: the list it reads back as");
}

/// A second Opacity added: it becomes the anchor, and the first, still
/// enabled, multiplies.
#[test]
fn a_second_opacity_becomes_the_anchor() {
    let added = opacity(0.5);
    holds(
        "a second opacity",
        (vec![blur(), opacity(0.8)], vec![true, true]),
        &[EffectEdit::Add(&added)],
        (vec![blur(), opacity(0.8), opacity(0.5)], vec![true, true, true]),
    );
}

/// The anchor switched off: the Opacity before it is the anchor now.
#[test]
fn the_anchor_switched_off_hands_it_on() {
    holds(
        "the anchor off",
        (vec![opacity(0.8), blur(), opacity(0.5)], vec![true, true, true]),
        &[EffectEdit::SetEnabled(2, false)],
        (vec![opacity(0.8), blur(), opacity(0.5)], vec![true, true, false]),
    );
}

/// A Transform added to a clip with none: the placement is fixed at it.
#[test]
fn a_first_transform_fixes_the_placement() {
    let t = transform(40.0);
    holds("a first transform", (vec![blur()], vec![true]), &[EffectEdit::Add(&t)], (vec![blur(), transform(40.0)], vec![true, true]));
}

/// The fixed Transform removed: the next enabled one is the placement.
#[test]
fn removing_the_fixed_transform_fixes_the_next() {
    holds(
        "the fixed transform removed",
        (vec![transform(10.0), blur(), transform(30.0)], vec![true, true, true]),
        &[EffectEdit::Remove(0)],
        (vec![blur(), transform(30.0)], vec![true, true]),
    );
}

/// The fixed Transform switched off: the next enabled one is the
/// placement, and the first is kept off.
#[test]
fn the_fixed_transform_switched_off_hands_it_on() {
    holds(
        "the fixed transform off",
        (vec![transform(10.0), transform(30.0)], vec![true, true]),
        &[EffectEdit::SetEnabled(0, false)],
        (vec![transform(10.0), transform(30.0)], vec![false, true]),
    );
}

/// A blending Speed Ramp moved in the list: it still runs at the head.
#[test]
fn a_blending_ramp_moved_stays_at_the_head() {
    holds(
        "a ramp moved",
        (vec![blur(), E::SpeedRamp { speed: 0.5, blend_mode: 1 }, opacity(0.9)], vec![true, true, true]),
        &[EffectEdit::Move { from: 1, to: 0 }],
        (vec![E::SpeedRamp { speed: 0.5, blend_mode: 1 }, blur(), opacity(0.9)], vec![true, true, true]),
    );
}

/// Temporal Noise Reduction added: it runs at the head, before the grade.
#[test]
fn a_denoiser_added_runs_at_the_head() {
    let d = E::TemporalDenoise { strength: 0.6, threshold: 0.1 };
    holds(
        "a denoiser added",
        (vec![blur(), opacity(0.7)], vec![true, true]),
        &[EffectEdit::Add(&d)],
        (vec![blur(), opacity(0.7), E::TemporalDenoise { strength: 0.6, threshold: 0.1 }], vec![true, true, true]),
    );
}

/// One edit of several, mixing a placed kind and a plain effect, is one
/// picture edit with the roles of the list it ends at.
#[test]
fn placed_and_plain_edits_together() {
    let t = transform(5.0);
    let o = opacity(0.4);
    holds(
        "placed and plain together",
        (vec![blur(), opacity(0.8)], vec![true, true]),
        &[EffectEdit::Add(&t), EffectEdit::Remove(0), EffectEdit::Add(&o)],
        (vec![opacity(0.8), transform(5.0), opacity(0.4)], vec![true, true, true]),
    );
}
