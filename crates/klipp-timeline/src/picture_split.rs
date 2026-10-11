//! A clip's picture cut in two at a blade (M4 step 12f): the halves'
//! animations, as the blade cut the v1 lanes. Keys before the cut stay on
//! the first half, and the rest go to the second, shifted to its start;
//! modifier stacks go to both. The placement and opacity lanes and the
//! effect nodes' keys are cut. The grade's nodes are kept whole on both
//! halves, as the blade kept the grade.

use klipp_graph_format::lifted::{types as lt, ClipPicture};

use crate::LiftedPicture;
use klipp_graph_format::Animation;

/// `anim` cut at `cut_secs`: its keys before the cut, then the rest
/// shifted to start at the cut. A half left with no keys and no modifier
/// stack has no animation.
fn split_anim(anim: &Animation, cut_secs: f64) -> (Option<Animation>, Option<Animation>) {
    let time = |k: &serde_json::Value| k.get("time_secs").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
    let mut before = Vec::new();
    let mut after = Vec::new();
    for k in &anim.keys {
        let t = time(k);
        if t < cut_secs {
            before.push(k.clone());
        } else {
            let mut shifted = k.clone();
            if let Some(o) = shifted.as_object_mut() {
                o.insert("time_secs".into(), serde_json::json!(t - cut_secs));
            }
            after.push(shifted);
        }
    }
    let half = |keys: Vec<serde_json::Value>| (!keys.is_empty() || anim.modifiers.is_some()).then(|| Animation { keys, modifiers: anim.modifiers.clone() });
    (half(before), half(after))
}

/// `lifted`'s picture cut at `cut_secs` of clip time, as two pictures:
/// the half before the cut and the half after it.
pub fn split_picture(lifted: &LiftedPicture, cut_secs: f64) -> (ClipPicture, ClipPicture) {
    let mut a = lifted.picture.clone();
    let mut b = lifted.picture.clone();
    let set = |lanes: &mut std::collections::BTreeMap<_, _>, key: &klipp_graph_format::ParamKey, anim: Option<Animation>| match anim {
        Some(anim) => lanes.insert(key.clone(), anim),
        None => lanes.remove(key),
    };
    for (key, anim) in &lifted.picture.placement_anim {
        let (x, y) = split_anim(anim, cut_secs);
        set(&mut a.placement_anim, key, x);
        set(&mut b.placement_anim, key, y);
    }
    if let Some(anim) = &lifted.picture.opacity_anim {
        (a.opacity_anim, b.opacity_anim) = split_anim(anim, cut_secs);
    }
    // An effect node: one holding an effect's entry, or a frei0r plug-in.
    let effect = |n: &klipp_graph_format::Node| n.type_id.as_str() == lt::FREI0R || klipp_graph_lift::effect_params::tag(n).is_some();
    for (na, nb) in a.graph.nodes.iter_mut().zip(b.graph.nodes.iter_mut()) {
        if !effect(na) {
            continue;
        }
        for (key, anim) in std::mem::take(&mut na.anim) {
            let (x, y) = split_anim(&anim, cut_secs);
            nb.anim.remove(&key);
            if let Some(x) = x {
                na.anim.insert(key.clone(), x);
            }
            if let Some(y) = y {
                nb.anim.insert(key, y);
            }
        }
    }
    (a, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(t: f64, v: f32) -> serde_json::Value {
        serde_json::json!({ "time_secs": t, "value": v, "interpolation": "Linear", "tangent_out": [0.0, 0.0], "tangent_in": [0.0, 0.0] })
    }

    #[test]
    fn a_lane_is_cut_at_the_blade_and_its_second_half_starts_at_zero() {
        let anim = Animation { keys: vec![key(0.0, 0.0), key(1.0, 1.0), key(2.0, 2.0)], modifiers: None };
        let (a, b) = split_anim(&anim, 1.5);
        let times = |a: &Option<Animation>| a.as_ref().map(|a| a.keys.iter().map(|k| k["time_secs"].as_f64().unwrap()).collect::<Vec<_>>());
        assert_eq!(times(&a), Some(vec![0.0, 1.0]));
        assert_eq!(times(&b), Some(vec![0.5]));
    }

    #[test]
    fn a_half_with_no_keys_keeps_only_a_modifier_stack() {
        let stack = serde_json::json!({ "modifiers": [] });
        let (a, b) = split_anim(&Animation { keys: vec![key(2.0, 1.0)], modifiers: Some(stack.clone()) }, 1.0);
        assert_eq!(a.map(|a| (a.keys.len(), a.modifiers)), Some((0, Some(stack))), "the stack goes to both halves");
        assert_eq!(b.map(|b| b.keys.len()), Some(1));
        assert_eq!(split_anim(&Animation { keys: vec![key(2.0, 1.0)], modifiers: None }, 1.0).0, None, "no keys and no stack: no animation");
    }
}

/// A blade cuts the clip's picture: each half has the keys on its side of
/// the cut, the second half's shifted to start at its own start; and a
/// render copy's track effects come back off whole.
#[cfg(test)]
mod blade_tests {
    use crate::test_clip::TestClip;
    use crate::{Clip, Track, TrackKind};
    use klipp_core::{MediaId, RationalTime};
    use klipp_effects::BuiltinEffect as E;
    use klipp_graph_format::path::{NodePath, Root};
    use klipp_graph_format::ParamKey;
    use klipp_graph_lift::ops::GraphOp;
    use serde_json::json;

    fn t(s: f64) -> RationalTime {
        RationalTime::from_secs_f64(s, 24)
    }

    fn key(time_secs: f64, value: f32) -> serde_json::Value {
        json!({ "time_secs": time_secs, "value": value, "interpolation": "Linear", "tangent_out": [0.0, 0.0], "tangent_in": [0.0, 0.0] })
    }

    /// A clip with a blur, made picture-truth by a node placed on the canvas,
    /// with its x keyed by path at 0 s and 1.5 s.
    fn keyed_truth_clip() -> Clip {
        let mut c = TestClip::of(Clip::media(MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0))).effect(E::GaussianBlur { radius: 2.0 }).build();
        let blur = c.picture_v2().unwrap().picture.graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("gaussian_blur")).unwrap().id;
        c.apply_picture_ops(&[GraphOp::SetNodeMeta { path: NodePath::new(Root::Clip { clip: c.id.0 }, [blur]), label: None, position: Some(json!({ "x": 40.0, "y": 0.0 })) }]).unwrap();
        let at = c.placement_path().unwrap();
        c.apply_picture_ops(&[
            GraphOp::AddKeyAt { path: at.clone(), key: ParamKey::from("pos_x"), keyframe: key(0.0, 0.0) },
            GraphOp::AddKeyAt { path: at, key: ParamKey::from("pos_x"), keyframe: key(1.5, 150.0) },
        ])
        .unwrap();
        assert!(c.is_picture_truth());
        c
    }

    #[test]
    fn each_half_of_a_bladed_clip_has_its_own_keys() {
        let clip = keyed_truth_clip();
        let id = clip.id;
        let mut track = Track::new("V", TrackKind::Video);
        track.clips = vec![clip];
        let (a, b) = track.split_at(id, t(1.0)).expect("a cut inside the clip");
        let times = |c: &Clip| {
            assert!(c.is_picture_truth(), "a picture-truth half");
            let lifted = c.picture_v2().unwrap();
            lifted.picture.placement_anim.get(&ParamKey::from("pos_x")).map_or(Vec::new(), |a| a.keys.iter().map(|k| k["time_secs"].as_f64().unwrap()).collect::<Vec<_>>())
        };
        assert_eq!(times(&a), vec![0.0], "the first half keeps the key before the cut");
        assert_eq!(times(&b), vec![0.5], "the second half has the key after it, from its own start");
    }

    /// A render copy's track effects come back off whole (M4 step 12f): the
    /// clip keeps its own effects, their node ids and keys, and keys on a
    /// track effect leave with it rather than landing on an own effect.
    #[test]
    fn unbaking_takes_the_track_effects_and_their_keys_back_off() {
        let mut clip = TestClip::of(Clip::media(MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0))).effect(E::GaussianBlur { radius: 2.0 }).build();
        let own_node = clip.effect_node_at(false, 0).unwrap();
        let own_path = NodePath::new(Root::Clip { clip: clip.id.0 }, [own_node]);
        clip.apply_picture_ops(&[GraphOp::AddKeyAt { path: own_path, key: ParamKey::from("radius"), keyframe: key(0.0, 1.0) }]).unwrap();
        let mut seq = crate::Sequence::new_default();
        let mut track = Track::new("V", TrackKind::Video);
        track.effects.video_effect_list = crate::TrackEffectList::new(vec![E::Invert { mix: 1.0 }], vec![true]);
        track.clips = vec![clip];
        seq.tracks = vec![track];
        seq.bake_track_effects();
        let baked = &mut seq.tracks[0].clips[0];
        assert_eq!(baked.baked_own.map(|b| b.effects), Some(1), "the bake counts the clip's own");
        let track_node = baked.effect_node_at(false, 1).expect("the track's effect after the clip's own");
        let track_path = NodePath::new(Root::Clip { clip: baked.id.0 }, [track_node]);
        baked.apply_picture_ops(&[GraphOp::AddKeyAt { path: track_path, key: ParamKey::from("mix"), keyframe: key(0.5, 0.5) }]).unwrap();

        let back = seq.with_own_grades();
        let c = &back.tracks[0].clips[0];
        assert!(c.baked_own.is_none(), "the copy is the document's clip again");
        assert_eq!(c.effect_node_at(false, 0), Some(own_node), "the own effect keeps its node id");
        assert!(c.effect_node_at(false, 1).is_none(), "the track's effect is gone");
        let lifted = c.picture_v2().unwrap();
        let keyed: Vec<&str> = lifted.picture.graph.nodes.iter().flat_map(|n| n.anim.keys().map(|k| k.as_str())).collect();
        assert_eq!(keyed, vec!["radius"], "only the own effect's keys are left: {keyed:?}");
    }
}
