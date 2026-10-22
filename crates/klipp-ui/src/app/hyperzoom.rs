//! Hyperzoom applied to a clip: its zoom plan, radial blur, chromatic
//! aberration and light leak, and a focal point turned into the clip's
//! transform (#890).

#[allow(unused_imports)]
use super::*;
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_lift::ops::GraphOp;

pub(super) fn apply_hyperzoom_plan_to_clip(
    seq: &mut klipp_timeline::Sequence,
    clip_id: klipp_core::ClipId,
    anchor: (f32, f32),
    scale_kfs: &[klipp_core::keyframe::Keyframe],
) {
    let Some(clip) = seq.all_clips_mut().find(|c| c.id == clip_id) else {
        tracing::warn!("apply_hyperzoom_plan_to_clip: clip {clip_id:?} no longer in sequence");
        return;
    };

    // The anchor at the focal point and the planned zoom, by path on the
    // clip's placement (M4 step 12c-5); its other values stay as they
    // were set, a baseline rotation or perspective among them, and the
    // zoom's modifier stack is kept.
    let Some(at) = clip.placement_path() else {
        tracing::warn!("apply_hyperzoom_plan_to_clip: clip {clip_id:?} has no placement");
        return;
    };
    let float = |v: f32| klipp_graph_format::ParamValue::Float(v);
    let key = klipp_graph_format::ParamKey::from;
    let modifiers = clip.picture_v2().ok().and_then(|p| p.picture.placement_anim.get(&key("scale")).and_then(|a| a.modifiers.clone()));
    let keys = scale_kfs.iter().filter_map(|k| serde_json::to_value(k).ok()).collect();
    let ops = [
        GraphOp::SetParam { path: at.clone(), key: key("anchor_x"), value: float(anchor.0) },
        GraphOp::SetParam { path: at.clone(), key: key("anchor_y"), value: float(anchor.1) },
        GraphOp::SetAnim { path: at, key: key("scale"), anim: Some(klipp_graph_format::Animation { keys, modifiers }) },
    ];
    if let Err(why) = clip.apply_picture_ops(&ops) {
        tracing::warn!("hyperzoom: clip {clip_id:?}: the zoom plan: {why}");
    }
}

/// Mirror of [`apply_hyperzoom_plan_to_clip`] for the radial-blur
/// convenience layer:
///
/// * Find or insert a `BuiltinEffect::RadialBlur` on `clip_id`. If a
///   RadialBlur already exists (e.g. from a prior Apply Hyperzoom or
///   manual user edit), its `center` is rewritten to the focal point
///   and `amount` is rewritten to the peak so the static fallback
///   matches the keyframes' peak value. `samples` and `mode` keep
///   their existing values (preserves user tweaks); when inserting
///   a fresh RadialBlur we use the standard zoom-blur defaults
///   (samples=12, mode=0 = zoom).
/// * Replace the effect node's `amount` animation (by path, M3 step 4)
///   with the planned ramp so re-applying is idempotent.
///
/// No-ops with a debug log if the clip can't be located. Same
/// fault-tolerance contract as `apply_hyperzoom_plan_to_clip`.
pub(super) fn apply_hyperzoom_radial_blur_to_clip(
    seq: &mut klipp_timeline::Sequence,
    clip_id: klipp_core::ClipId,
    center: (f32, f32),
    peak_amount: f32,
    amount_kfs: &[klipp_core::keyframe::Keyframe],
) {
    let Some(clip) = seq.all_clips_mut().find(|c| c.id == clip_id) else {
        tracing::debug!("apply_hyperzoom_radial_blur_to_clip: clip {clip_id:?} no longer in sequence");
        return;
    };

    // Update existing RadialBlur if present, else add a fresh one.
    let Some(id) = upsert_effect(
        clip,
        "radial_blur",
        |e| {
            if let klipp_effects::BuiltinEffect::RadialBlur { center_x, center_y, amount, .. } = e {
                *center_x = center.0;
                *center_y = center.1;
                *amount = peak_amount;
            }
        },
        || klipp_effects::BuiltinEffect::RadialBlur {
            center_x: center.0,
            center_y: center.1,
            amount: peak_amount,
            // Sample count of 12 matches the "Subtle Zoom" preset
            // and is the engine's standard sweet-spot trade-off
            // between quality and per-pixel cost. Mode 0 = zoom
            // (the canonical hyperzoom blur direction; spin
            // would feel wrong at the cut).
            samples: 12,
            mode: 0,
        },
    ) else { return };
    set_anim(clip, id, "amount", amount_kfs);
}

/// Mirror of [`apply_hyperzoom_radial_blur_to_clip`] for the
/// chromatic-aberration convenience layer:
///
/// * Find or insert a `BuiltinEffect::ChromaticAberration` on
///   `clip_id`. If one already exists (e.g. from a prior Apply
///   Hyperzoom OR a manual user edit), its `center_x` / `center_y`
///   are rewritten to the focal point — pinned by the audit-note
///   "re-apply with new focal updates center" test. `red_shift`
///   and `blue_shift` are also rewritten to ±peak so the static
///   fallback matches the keyframes' peak values; the keyframes
///   then drive the actual animation.
/// * Replace the effect node's `red_shift` and `blue_shift`
///   animations (by path) with the planned ramps. Re-applying
///   replaces the old keyframes wholesale — idempotent.
///
/// **First-match-wins disambiguation** (mirrors radial-blur):
/// when the user has manually added a `ChromaticAberration` with
/// custom params before applying hyperzoom, that effect's
/// `red_shift` / `blue_shift` / `center_x` / `center_y` get
/// rewritten by the convenience layer. Power users who want to
/// preserve their pre-existing tuning should disable the
/// convenience checkbox and add a separate ChromaticAberration
/// from the effects library after applying.
///
/// Keyframe write order is **red before blue** — pinned by
/// `apply_chromatic_keyframe_write_order_red_before_blue` so
/// snapshot tests stay deterministic.
pub(super) fn apply_hyperzoom_chromatic_aberration_to_clip(
    seq: &mut klipp_timeline::Sequence,
    clip_id: klipp_core::ClipId,
    center: (f32, f32),
    peak_amount: f32,
    red_kfs: &[klipp_core::keyframe::Keyframe],
    blue_kfs: &[klipp_core::keyframe::Keyframe],
) {
    let Some(clip) = seq.all_clips_mut().find(|c| c.id == clip_id) else {
        tracing::debug!("apply_hyperzoom_chromatic_aberration_to_clip: clip {clip_id:?} no longer in sequence");
        return;
    };
    let Some(id) = upsert_effect(
        clip,
        "chromatic_aberration",
        |e| {
            if let klipp_effects::BuiltinEffect::ChromaticAberration { red_shift, blue_shift, center_x, center_y } = e {
                *center_x = center.0;
                *center_y = center.1;
                // Static fallback values mirror the keyframes' peak —
                // red converges, blue diverges.
                *red_shift = -peak_amount;
                *blue_shift = peak_amount;
            }
        },
        || klipp_effects::BuiltinEffect::ChromaticAberration {
            red_shift: -peak_amount,
            blue_shift: peak_amount,
            center_x: center.0,
            center_y: center.1,
        },
    ) else { return };
    // Pin: red before blue. If an audit changes this order, the
    // ordering test will flag it before snapshot tests start
    // diffing nondeterministically.
    set_anim(clip, id, "red_shift", red_kfs);
    set_anim(clip, id, "blue_shift", blue_kfs);
}

/// Mirror of [`apply_hyperzoom_radial_blur_to_clip`] for the
/// light-leak convenience layer (rides on
/// `BuiltinEffect::FilmHalation`):
///
/// * Find or insert a `FilmHalation`. On update, only `intensity`
///   is rewritten to the peak value (static fallback for the
///   keyframes); `threshold` / `radius` / `hue` keep their
///   existing values so a user-tweaked halation isn't blown
///   away by re-apply. Fresh inserts use trailer-aesthetic
///   defaults (warm hue ≈ 0.15, moderate threshold + radius).
/// * Replace the effect node's `intensity` animation (by path) with
///   the planned ramp.
///
/// Same first-match-wins disambiguation as radial-blur — a user-
/// added FilmHalation gets its intensity rewritten by the
/// convenience layer (the other params survive). Power users
/// preserving custom halation tunings should disable the
/// convenience checkbox.
pub(super) fn apply_hyperzoom_light_leak_to_clip(
    seq: &mut klipp_timeline::Sequence,
    clip_id: klipp_core::ClipId,
    peak_intensity: f32,
    intensity_kfs: &[klipp_core::keyframe::Keyframe],
) {
    let Some(clip) = seq.all_clips_mut().find(|c| c.id == clip_id) else {
        tracing::debug!("apply_hyperzoom_light_leak_to_clip: clip {clip_id:?} no longer in sequence");
        return;
    };
    let Some(id) = upsert_effect(
        clip,
        "film_halation",
        |e| {
            if let klipp_effects::BuiltinEffect::FilmHalation { intensity, .. } = e {
                *intensity = peak_intensity;
            }
        },
        || klipp_effects::BuiltinEffect::FilmHalation {
            threshold: 0.75,
            radius: 15.0,
            intensity: peak_intensity,
            // Hue 0.15 ≈ warm orange-yellow — the
            // canonical trailer-light-leak colour. Power
            // users tweak via the standalone effect's
            // params after applying.
            hue: 0.15,
        },
    ) else { return };
    set_anim(clip, id, "intensity", intensity_kfs);
}

/// Write a `FocalRef` onto the clip's `BuiltinEffect::Transform`.
/// Companion to [`apply_hyperzoom_plan_to_clip`] for
/// HZ-NEXT-GLYPH-LOCK: when the user has picked a glyph in the
/// inspector preview, the bridge dispatches `ApplyHyperzoom` with
/// `focal_ref = Some(...)` and this helper writes the override
/// onto the source clip's Transform. The static `anchor_x` /
/// `anchor_y` (already written by `apply_hyperzoom_plan_to_clip`)
/// stay as the resolution-failure fallback.
///
/// If no Transform builtin exists yet (shouldn't happen — the
/// hyperzoom apply always inserts one — but defence-in-depth),
/// the helper inserts one with sane defaults so the focal_ref
/// still has a host.
pub(super) fn apply_focal_ref_to_clip_transform(
    seq: &mut klipp_timeline::Sequence,
    clip_id: klipp_core::ClipId,
    focal_ref: klipp_core::FocalRef,
) {
    let Some(clip) = seq.all_clips_mut().find(|c| c.id == clip_id) else {
        tracing::debug!("apply_focal_ref_to_clip_transform: clip {clip_id:?} no longer in sequence");
        return;
    };

    find_or_push(
        clip,
        |e| matches!(e, klipp_effects::BuiltinEffect::Transform { .. }),
        |e| {
            if let klipp_effects::BuiltinEffect::Transform { focal_ref: existing, .. } = e {
                *existing = Some(focal_ref);
            }
        },
        || klipp_effects::BuiltinEffect::Transform {
            position_x: 0.0, position_y: 0.0,
            scale_x: 1.0, scale_y: 1.0,
            rotation: 0.0,
            anchor_x: 0.5, anchor_y: 0.5,
            rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0,
            focal_ref: Some(focal_ref),
        },
    );
}

/// The first effect on `clip` that `kind` picks, edited by `edit`; with
/// none, `fresh` appended: an entry edit through the clip's picture (M4
/// step 12c-5), each entry read as its node holds it. A refusal is logged.
fn find_or_push(
    clip: &mut klipp_timeline::Clip,
    kind: impl Fn(&klipp_effects::BuiltinEffect) -> bool,
    edit: impl FnOnce(&mut klipp_effects::BuiltinEffect),
    fresh: impl FnOnce() -> klipp_effects::BuiltinEffect,
) {
    let entry_at = |clip: &klipp_timeline::Clip, i: usize| {
        let id = clip.effect_node_at(false, i)?;
        let lifted = clip.picture_v2().ok()?;
        let node = lifted.picture.graph.nodes.iter().find(|n| n.id == id)?;
        // A plain effect from its map; a kind the clip places itself (its
        // Transform) from the entry its node stores.
        serde_json::from_value(klipp_graph_lift::effect_params::entry_of(node).ok()?).ok()
    };
    let found = (0..).map_while(|i| clip.effect_node_at(false, i).map(|_| i)).find_map(|i| entry_at(clip, i).filter(|e| kind(e)).map(|e| (i, e)));
    let made = match found {
        Some((i, mut effect)) => {
            edit(&mut effect);
            clip.apply_effect_edits(&[klipp_timeline::EffectEdit::Update(i, &effect)])
        }
        None => clip.apply_effect_edits(&[klipp_timeline::EffectEdit::Add(&fresh())]),
    };
    if let Some(Err(why)) = made {
        tracing::warn!("hyperzoom: clip {:?}: {why}", clip.id);
    }
}

/// The first effect of type `tag` on `clip`'s picture spine, its stored
/// entry edited by `edit` (`SetPayload` by path); with none, `fresh` added
/// as a node after the last before the output (`AddNode`, which the list
/// takes at its end). Its node id either way; `None`, said, when the
/// picture refuses the edit.
fn upsert_effect(
    clip: &mut klipp_timeline::Clip,
    tag: &str,
    edit: impl FnOnce(&mut klipp_effects::BuiltinEffect),
    fresh: impl FnOnce() -> klipp_effects::BuiltinEffect,
) -> Option<klipp_graph_format::NodeId> {
    let at = |clip: &klipp_timeline::Clip, id| NodePath::new(Root::Clip { clip: clip.id.0 }, [id]);
    let op = match crate::multi_edit::first_on_the_spine(clip, tag) {
        Some(id) => {
            let index = clip.index_of(id)?;
            let mut effect = clip.builtin_effects()[index].clone();
            edit(&mut effect);
            let payload = serde_json::to_value(&effect).ok()?;
            (GraphOp::SetPayload { path: at(clip, id.as_uuid()), payload }, id.as_uuid())
        }
        None => {
            let effect = fresh();
            let id = uuid::Uuid::new_v4();
            let lifted = clip.picture_v2().ok()?;
            let graph = &lifted.picture.graph;
            let last = graph.edges.iter().find(|e| e.to == (graph.output, "in".into()))?.from.0;
            let node: klipp_graph_format::Node = serde_json::from_value(serde_json::json!({
                "id": id,
                "type_id": klipp_graph_lift::names::effect_node_type(tag),
                "legacy": { "payload": effect },
            }))
            .ok()?;
            (GraphOp::AddNode { after: at(clip, last), node: Box::new(node) }, id)
        }
    };
    match clip.apply_picture_ops(std::slice::from_ref(&op.0)) {
        Ok(_) => Some(op.1),
        Err(why) => {
            tracing::warn!("hyperzoom: clip {:?}: {tag}: {why}", clip.id);
            None
        }
    }
}

/// Replace the animation of the effect node `id`'s `param` with `keys`
/// (`SetAnim` by path), keeping its modifier stack, so applying again
/// gives the same ramp.
fn set_anim(clip: &mut klipp_timeline::Clip, id: klipp_graph_format::NodeId, param: &str, keys: &[klipp_core::keyframe::Keyframe]) {
    let key = klipp_graph_format::ParamKey::from(param);
    let modifiers = clip.picture_v2().ok().and_then(|p| p.picture.graph.nodes.iter().find(|n| n.id == id).and_then(|n| n.anim.get(&key)).and_then(|a| a.modifiers.clone()));
    let keys = keys.iter().filter_map(|k| serde_json::to_value(k).ok()).collect();
    let op = GraphOp::SetAnim {
        path: NodePath::new(Root::Clip { clip: clip.id.0 }, [id]),
        key,
        anim: Some(klipp_graph_format::Animation { keys, modifiers }),
    };
    if let Err(why) = clip.apply_picture_ops(&[op]) {
        tracing::warn!("hyperzoom: clip {:?}: {param}'s keys: {why}", clip.id);
    }
}

#[cfg(test)]
mod hyperzoom_handler_tests {
    //! Pure-function tests for `apply_hyperzoom_plan_to_clip` — the
    //! per-clip half of the hyperzoom dispatcher. The full
    //! `ApplyHyperzoom` AppCommand handler needs a `App` for
    //! `dirty`/`sync_sequence_to_engine`; these exercise the core
    //! mutation invariants on a hand-built sequence.
    use super::apply_hyperzoom_plan_to_clip;
    use klipp_core::{ClipId, MediaId, RationalTime};
    use klipp_core::keyframe::{Interpolation, Keyframe};
    use klipp_effects::BuiltinEffect;
    use klipp_timeline::{Clip, Sequence, Track, TrackKind};
    use klipp_timeline::test_clip::{self, TestClip};

    fn tc(frames: i64) -> RationalTime { RationalTime::from_frames(frames, 24) }

    fn seq_with_clips(clips: Vec<Clip>) -> Sequence {
        let mut s = Sequence::new_default();
        let mut tr = Track::new("V1", TrackKind::Video);
        tr.clips = clips;
        s.tracks.push(tr);
        s
    }

    fn fresh_clip() -> Clip {
        Clip::media(MediaId::new(), tc(0), tc(48), tc(0), tc(48))
    }

    fn kf(t: f64, v: f32) -> Keyframe {
        Keyframe { time_secs: t, value: v, interpolation: Interpolation::Linear,
                   tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0), handles: Default::default() }
    }

    #[test]
    fn applies_anchor_by_inserting_transform_when_absent() {
        // Clip has no Transform builtin to start. Plan should insert
        // one with the focal point as anchor and the rest at default.
        let c = fresh_clip();
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        apply_hyperzoom_plan_to_clip(&mut seq, cid, (0.3, 0.7), &[kf(0.0, 1.0), kf(0.5, 50.0)]);
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        let xform = test_clip::effects(clip).iter().find_map(|e| match e {
            BuiltinEffect::Transform { anchor_x, anchor_y, scale_x, .. } => Some((*anchor_x, *anchor_y, *scale_x)),
            _ => None,
        }).expect("Transform builtin should have been inserted");
        assert!((xform.0 - 0.3).abs() < 1e-6);
        assert!((xform.1 - 0.7).abs() < 1e-6);
        assert!((xform.2 - 1.0).abs() < 1e-6); // default scale, not the keyframe value
    }

    #[test]
    fn updates_existing_transform_anchor_in_place() {
        // Pre-existing Transform with non-default rotation + scale. The
        // plan should ONLY rewrite anchor — the user's other field
        // values must survive (re-applying hyperzoom shouldn't blow
        // away a baseline rotation, for example).
        let c = TestClip::of(fresh_clip()).effect(BuiltinEffect::Transform {
            position_x: 10.0, position_y: 20.0,
            scale_x: 1.5, scale_y: 1.5,
            rotation: 45.0,
            anchor_x: 0.5, anchor_y: 0.5,
            rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0,
            focal_ref: None,
        }).build();
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        apply_hyperzoom_plan_to_clip(&mut seq, cid, (0.2, 0.8), &[kf(0.0, 1.0), kf(0.5, 50.0)]);
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        let xform = test_clip::effects(clip).iter().find_map(|e| match e {
            BuiltinEffect::Transform { position_x, position_y, scale_x, rotation, anchor_x, anchor_y, .. } =>
                Some((*position_x, *position_y, *scale_x, *rotation, *anchor_x, *anchor_y)),
            _ => None,
        }).unwrap();
        assert!((xform.0 - 10.0).abs() < 1e-6, "position_x preserved");
        assert!((xform.1 - 20.0).abs() < 1e-6, "position_y preserved");
        assert!((xform.2 - 1.5).abs() < 1e-6, "scale_x preserved");
        assert!((xform.3 - 45.0).abs() < 1e-6, "rotation preserved");
        assert!((xform.4 - 0.2).abs() < 1e-6, "anchor_x updated");
        assert!((xform.5 - 0.8).abs() < 1e-6, "anchor_y updated");
    }

    #[test]
    fn replaces_existing_scale_keyframes_for_idempotency() {
        // Re-applying hyperzoom should overwrite — never accumulate.
        // Otherwise a user clicking Apply twice with different settings
        // would end up with stale keyframes from the first application
        // bleeding into the second.
        let c = TestClip::of(fresh_clip()).lane("scale", 0.0, 0.5).lane("scale", 0.25, 2.0).lane("scale", 0.5, 5.0).build();
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        let new_kfs = vec![kf(2.5, 1.0), kf(3.0, 50.0)];
        apply_hyperzoom_plan_to_clip(&mut seq, cid, (0.5, 0.5), &new_kfs);
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        assert_eq!(clip.transform_scale_keyframes().len(), 2);
        assert!((clip.transform_scale_keyframes()[0].time_secs - 2.5).abs() < 1e-9);
        assert!((clip.transform_scale_keyframes()[1].time_secs - 3.0).abs() < 1e-9);
        assert!((clip.transform_scale_keyframes()[1].value - 50.0).abs() < 1e-6);
    }

    #[test]
    fn missing_clip_is_a_no_op() {
        // Stale dispatch (clip removed between dispatch and handler).
        // The function should warn and return without panicking; the
        // sequence stays unchanged.
        let c = fresh_clip();
        let mut seq = seq_with_clips(vec![c.clone()]);
        let other_id = ClipId::new();
        apply_hyperzoom_plan_to_clip(&mut seq, other_id, (0.5, 0.5), &[kf(0.0, 1.0), kf(0.5, 50.0)]);
        // Original clip untouched.
        let same = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|x| x.id == c.id).unwrap();
        assert!(same.transform_scale_keyframes().is_empty());
        assert!(test_clip::effects(same).iter().all(|e| !matches!(e, BuiltinEffect::Transform { .. })));
    }

    /// M3 step 4: the effect layers, written as graph edits by path, build
    /// the picture the list writes built: the same effects in the same
    /// places with the same keys, the full typed picture equal once each
    /// new effect's id is read as the other's (ids are random on both).
    /// On a clip without the layers and on one that has them already.
    #[test]
    fn hyperzoom_builds_the_same_picture() {
        use super::{apply_hyperzoom_chromatic_aberration_to_clip, apply_hyperzoom_light_leak_to_clip};
        // The list writes as they were before step 4.
        fn old_layer(clip: &mut Clip, kind: fn(&BuiltinEffect) -> bool, edit: impl FnOnce(&mut BuiltinEffect), fresh: BuiltinEffect, keys: &[(&str, Vec<Keyframe>)]) {
            let found = clip.effect_entries().find(|(_, e, _)| kind(e)).map(|(id, _, _)| id);
            let id = match found {
                Some(id) => {
                    clip.edit_effect(id, edit);
                    id
                }
                None => clip.push_effect(fresh),
            };
            for (param, k) in keys {
                clip.edit_effect_keyframes_of(id, param, |v| *v = k.clone());
            }
        }
        let ramp = |peak: f32| vec![kf(0.0, 0.0), kf(0.5, peak)];
        let starts = [
            fresh_clip(),
            TestClip::of(fresh_clip()).effect(BuiltinEffect::Gamma { gamma: 1.1 }).effect(BuiltinEffect::RadialBlur { center_x: 0.1, center_y: 0.1, amount: 3.0, samples: 24, mode: 1 }).effect(BuiltinEffect::FilmHalation { threshold: 0.5, radius: 9.0, intensity: 0.2, hue: 0.6 }).build(),
        ];
        for (n, start) in starts.into_iter().enumerate() {
            let cid = start.id;
            let mut old = start.clone();
            old_layer(&mut old, |e| matches!(e, BuiltinEffect::RadialBlur { .. }), |e| if let BuiltinEffect::RadialBlur { center_x, center_y, amount, .. } = e { (*center_x, *center_y, *amount) = (0.3, 0.7, 15.0) },
                BuiltinEffect::RadialBlur { center_x: 0.3, center_y: 0.7, amount: 15.0, samples: 12, mode: 0 }, &[("amount", ramp(15.0))]);
            old_layer(&mut old, |e| matches!(e, BuiltinEffect::ChromaticAberration { .. }), |e| if let BuiltinEffect::ChromaticAberration { red_shift, blue_shift, center_x, center_y } = e { (*center_x, *center_y, *red_shift, *blue_shift) = (0.3, 0.7, -4.0, 4.0) },
                BuiltinEffect::ChromaticAberration { red_shift: -4.0, blue_shift: 4.0, center_x: 0.3, center_y: 0.7 }, &[("red_shift", ramp(-4.0)), ("blue_shift", ramp(4.0))]);
            old_layer(&mut old, |e| matches!(e, BuiltinEffect::FilmHalation { .. }), |e| if let BuiltinEffect::FilmHalation { intensity, .. } = e { *intensity = 0.8 },
                BuiltinEffect::FilmHalation { threshold: 0.75, radius: 15.0, intensity: 0.8, hue: 0.15 }, &[("intensity", ramp(0.8))]);

            let mut seq = seq_with_clips(vec![start]);
            apply_hyperzoom_radial_blur_to_clip(&mut seq, cid, (0.3, 0.7), 15.0, &ramp(15.0));
            apply_hyperzoom_chromatic_aberration_to_clip(&mut seq, cid, (0.3, 0.7), 4.0, &ramp(-4.0), &ramp(4.0));
            apply_hyperzoom_light_leak_to_clip(&mut seq, cid, 0.8, &ramp(0.8));
            let new = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap().clone();

            assert_eq!(format!("{:?}", test_clip::effects(&new)), format!("{:?}", test_clip::effects(&old)), "case {n}: the list");
            let mut picture = serde_json::to_string(&new.picture_v2().unwrap().picture).unwrap();
            for i in 0..test_clip::effects(&old).len() {
                let (was, now) = (test_clip::effect_id(&new, i).as_uuid(), test_clip::effect_id(&old, i).as_uuid());
                picture = picture.replace(&was.to_string(), &now.to_string());
            }
            let old_picture = serde_json::to_string(&old.picture_v2().unwrap().picture).unwrap();
            assert_eq!(picture, old_picture, "case {n}: the picture");
        }
    }

    use super::apply_hyperzoom_radial_blur_to_clip;

    #[test]
    fn radial_blur_inserts_builtin_when_absent_and_keys_amount() {
        let c = fresh_clip();
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        apply_hyperzoom_radial_blur_to_clip(
            &mut seq, cid, (0.3, 0.7), 15.0,
            &[kf(0.0, 0.0), kf(0.5, 15.0)],
        );
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        // RadialBlur builtin inserted with focal-point center +
        // peak amount + canonical defaults (samples=12, mode=0).
        let (idx, cx, cy, amt, samples, mode) = test_clip::effects(clip).iter().enumerate().find_map(|(i, e)| match e {
            BuiltinEffect::RadialBlur { center_x, center_y, amount, samples, mode } =>
                Some((i, *center_x, *center_y, *amount, *samples, *mode)),
            _ => None,
        }).expect("RadialBlur should have been inserted");
        assert!((cx - 0.3).abs() < 1e-6);
        assert!((cy - 0.7).abs() < 1e-6);
        assert!((amt - 15.0).abs() < 1e-6);
        assert_eq!(samples, 12);
        assert_eq!(mode, 0);
        // Amount keyframes registered under "{idx}:amount" so the
        // engine's per-effect-keyframe lookup picks them up.
        let key = format!("{}:amount", idx);
        let kfs = clip.effect_keyframes().get(&key).expect("amount keyframes registered");
        assert_eq!(kfs.len(), 2);
        assert!((kfs[1].value - 15.0).abs() < 1e-6);
    }

    #[test]
    fn radial_blur_updates_existing_builtin_in_place() {
        // Pre-existing RadialBlur with non-default samples + mode
        // (user manually configured spin mode + 24 samples). The
        // hyperzoom helper should rewrite center + amount but
        // preserve samples/mode — re-applying must not clobber the
        // user's tweaks.
        let c = TestClip::of(fresh_clip()).effect(BuiltinEffect::RadialBlur {
            center_x: 0.0, center_y: 0.0,
            amount: 0.0,
            samples: 24,
            mode: 1, // spin
        }).build();
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        apply_hyperzoom_radial_blur_to_clip(
            &mut seq, cid, (0.4, 0.6), 22.0,
            &[kf(0.0, 0.0), kf(0.5, 22.0)],
        );
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        let (cx, cy, amt, samples, mode) = test_clip::effects(clip).iter().find_map(|e| match e {
            BuiltinEffect::RadialBlur { center_x, center_y, amount, samples, mode } =>
                Some((*center_x, *center_y, *amount, *samples, *mode)),
            _ => None,
        }).unwrap();
        assert!((cx - 0.4).abs() < 1e-6, "center_x rewritten");
        assert!((cy - 0.6).abs() < 1e-6, "center_y rewritten");
        assert!((amt - 22.0).abs() < 1e-6, "amount rewritten to peak");
        assert_eq!(samples, 24, "user-configured samples preserved");
        assert_eq!(mode, 1, "user-configured mode (spin) preserved");
    }

    // ─────────── Glyph-anchored focal-point bridge ───────────

    use super::apply_focal_ref_to_clip_transform;

    #[test]
    fn focal_ref_bridge_writes_into_existing_transform() {
        // Pre-existing Transform with a baseline rotation the user
        // wants preserved. The bridge writes focal_ref but must NOT
        // touch other Transform fields — same in-place-update
        // contract as the matte / radial-blur helpers.
        let c = TestClip::of(fresh_clip()).effect(BuiltinEffect::Transform {
            position_x: 0.0, position_y: 0.0,
            scale_x: 1.0, scale_y: 1.0,
            rotation: 30.0,  // baseline rotation
            anchor_x: 0.5, anchor_y: 0.5,
            rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0,
            focal_ref: None,
        }).build();
        let cid = c.id;
        let title_uuid = ClipId::new().0;
        let mut seq = seq_with_clips(vec![c]);

        let fr = klipp_core::FocalRef::Glyph {
            title_clip_id: title_uuid,
            char_index: 5,
            fallback_xy: (0.4, 0.6),
        };
        apply_focal_ref_to_clip_transform(&mut seq, cid, fr);

        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        let (rotation, fr_out) = test_clip::effects(clip).iter().find_map(|e| match e {
            BuiltinEffect::Transform { rotation, focal_ref, .. } => Some((*rotation, *focal_ref)),
            _ => None,
        }).unwrap();
        assert!((rotation - 30.0).abs() < 1e-6, "baseline rotation preserved");
        assert!(matches!(
            fr_out,
            Some(klipp_core::FocalRef::Glyph { char_index: 5, .. })
        ), "focal_ref written");
    }

    #[test]
    fn focal_ref_bridge_inserts_transform_when_absent() {
        // Defence-in-depth: if the source clip has no Transform
        // builtin yet, the bridge inserts one with sane defaults +
        // the focal_ref. (The hyperzoom apply path always inserts
        // one before this site fires, but the helper shouldn't
        // panic if called out of order.)
        let c = fresh_clip();
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        let fr = klipp_core::FocalRef::Glyph {
            title_clip_id: ClipId::new().0,
            char_index: 0,
            fallback_xy: (0.5, 0.5),
        };
        apply_focal_ref_to_clip_transform(&mut seq, cid, fr);
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        let has_focal_ref = test_clip::effects(clip).iter().any(|e| matches!(
            e,
            BuiltinEffect::Transform { focal_ref: Some(_), .. }
        ));
        assert!(has_focal_ref, "Transform inserted with focal_ref attached");
    }

    #[test]
    fn focal_ref_bridge_overwrites_existing_focal_ref() {
        // Re-applying hyperzoom with a different glyph pick should
        // replace the focal_ref, not stack two of them. (Belt-and-
        // suspenders — only one Transform per clip in practice, so
        // there's nothing to "stack" against, but the contract is
        // worth pinning.)
        let old_uuid = ClipId::new().0;
        let c = TestClip::of(fresh_clip()).effect(BuiltinEffect::Transform {
            position_x: 0.0, position_y: 0.0, scale_x: 1.0, scale_y: 1.0,
            rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5,
            rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0,
            focal_ref: Some(klipp_core::FocalRef::Glyph {
                title_clip_id: old_uuid,
                char_index: 1,
                fallback_xy: (0.0, 0.0),
            }),
        }).build();
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        let new_uuid = ClipId::new().0;
        apply_focal_ref_to_clip_transform(&mut seq, cid, klipp_core::FocalRef::Glyph {
            title_clip_id: new_uuid, char_index: 9, fallback_xy: (0.7, 0.3),
        });
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        let fr = test_clip::effects(clip).iter().find_map(|e| match e {
            BuiltinEffect::Transform { focal_ref, .. } => *focal_ref,
            _ => None,
        }).unwrap();
        match fr {
            klipp_core::FocalRef::Glyph { title_clip_id, char_index, .. } => {
                assert_eq!(title_clip_id, new_uuid, "focal_ref replaced not stacked");
                assert_eq!(char_index, 9);
            }
            klipp_core::FocalRef::Tracker { .. } => {
                panic!("test set up Glyph variant; got Tracker — schema regression?");
            }
        }
    }

    #[test]
    fn focal_ref_bridge_no_ops_on_missing_clip() {
        // Stale dispatch (clip removed between dispatch and handler).
        let c = fresh_clip();
        let mut seq = seq_with_clips(vec![c.clone()]);
        let other_id = ClipId::new();
        apply_focal_ref_to_clip_transform(&mut seq, other_id, klipp_core::FocalRef::Glyph {
            title_clip_id: ClipId::new().0, char_index: 0, fallback_xy: (0.5, 0.5),
        });
        let same = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|x| x.id == c.id).unwrap();
        assert!(test_clip::effects(same).iter().all(|e| !matches!(e, BuiltinEffect::Transform { .. })));
    }

    // ────── Chromatic-aberration handler ──────

    use super::apply_hyperzoom_chromatic_aberration_to_clip;

    fn read_chromatic(clip: &Clip) -> Option<(f32, f32, f32, f32)> {
        test_clip::effects(clip).iter().find_map(|e| match e {
            BuiltinEffect::ChromaticAberration { red_shift, blue_shift, center_x, center_y } => {
                Some((*red_shift, *blue_shift, *center_x, *center_y))
            }
            _ => None,
        })
    }

    #[test]
    fn chromatic_inserts_when_absent() {
        // No ChromaticAberration on the clip → handler appends one
        // with center at the focal point and red/blue at ∓peak.
        let c = fresh_clip();
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        let red = vec![kf(0.0, 0.0), kf(0.5, -8.0)];
        let blue = vec![kf(0.0, 0.0), kf(0.5, 8.0)];
        apply_hyperzoom_chromatic_aberration_to_clip(&mut seq, cid, (0.3, 0.7), 8.0, &red, &blue);
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        let (rs, bs, cx, cy) = read_chromatic(clip).expect("ChromaticAberration appended");
        assert!((rs - -8.0).abs() < 1e-6, "red_shift static fallback at -peak");
        assert!((bs - 8.0).abs() < 1e-6, "blue_shift static fallback at +peak");
        assert!((cx - 0.3).abs() < 1e-6);
        assert!((cy - 0.7).abs() < 1e-6);
    }

    #[test]
    fn chromatic_updates_existing_preserving_no_secondaries() {
        // Pre-existing ChromaticAberration with different
        // center + shifts. Handler rewrites center and shifts; no
        // secondary params on this builtin survive (it's all four
        // fields). User-tweaked-tunings-getting-overwritten is the
        // first-match-wins disambiguation behaviour pinned in the
        // doc comment.
        let c = TestClip::of(fresh_clip()).effect(BuiltinEffect::ChromaticAberration {
            red_shift: -2.0, blue_shift: 2.0, center_x: 0.1, center_y: 0.1,
        }).build();
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        let red = vec![kf(0.0, 0.0), kf(0.5, -10.0)];
        let blue = vec![kf(0.0, 0.0), kf(0.5, 10.0)];
        apply_hyperzoom_chromatic_aberration_to_clip(&mut seq, cid, (0.6, 0.4), 10.0, &red, &blue);
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        let (rs, bs, cx, cy) = read_chromatic(clip).unwrap();
        assert!((rs - -10.0).abs() < 1e-6);
        assert!((bs - 10.0).abs() < 1e-6);
        assert!((cx - 0.6).abs() < 1e-6, "center_x updated to new focal");
        assert!((cy - 0.4).abs() < 1e-6, "center_y updated to new focal");
        // Pool count stays at 1 — handler updated in place,
        // didn't append a duplicate.
        let n = test_clip::effects(clip).iter().filter(|e| matches!(e, BuiltinEffect::ChromaticAberration { .. })).count();
        assert_eq!(n, 1);
    }

    #[test]
    fn chromatic_re_apply_with_new_focal_updates_center() {
        // Audit-note (iv) pin: re-applying with a different focal
        // point must rewrite the center on the existing builtin.
        // The first-apply lays in the chromatic at focal=(0.5,0.5);
        // the second apply at focal=(0.2,0.8) must overwrite, not
        // leave the old center stale.
        let c = fresh_clip();
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        let red = vec![kf(0.0, 0.0), kf(0.5, -6.0)];
        let blue = vec![kf(0.0, 0.0), kf(0.5, 6.0)];
        apply_hyperzoom_chromatic_aberration_to_clip(&mut seq, cid, (0.5, 0.5), 6.0, &red, &blue);
        // Confirm first apply set center to (0.5, 0.5).
        let clip1 = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap().clone();
        let (_, _, cx1, cy1) = read_chromatic(&clip1).unwrap();
        assert!((cx1 - 0.5).abs() < 1e-6 && (cy1 - 0.5).abs() < 1e-6);
        // Re-apply with new focal.
        apply_hyperzoom_chromatic_aberration_to_clip(&mut seq, cid, (0.2, 0.8), 6.0, &red, &blue);
        let clip2 = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        let (_, _, cx2, cy2) = read_chromatic(clip2).unwrap();
        assert!((cx2 - 0.2).abs() < 1e-6, "center_x rewritten on re-apply");
        assert!((cy2 - 0.8).abs() < 1e-6, "center_y rewritten on re-apply");
    }

    #[test]
    fn chromatic_keyframe_write_order_red_before_blue() {
        // Audit-note (i) pin: red_shift keyframes are written
        // BEFORE blue_shift in clip.effect_keyframes. This makes
        // future snapshot tests deterministic regardless of
        // HashMap iteration order — callers ordering keys
        // alphabetically would surface red-shift first anyway, but
        // pinning the write order makes the contract explicit.
        let c = fresh_clip();
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        let red = vec![kf(0.0, 0.0), kf(0.5, -6.0)];
        let blue = vec![kf(0.0, 0.0), kf(0.5, 6.0)];
        apply_hyperzoom_chromatic_aberration_to_clip(&mut seq, cid, (0.5, 0.5), 6.0, &red, &blue);
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        // Both keys should now exist; red_shift was inserted first.
        // We check both keys are present (HashMap doesn't preserve
        // insertion order, so we can't pin the iteration order —
        // but the values must be correct for the right key).
        let red_key_keyframes = clip.effect_keyframes().iter()
            .find(|(k, _)| k.ends_with(":red_shift"))
            .map(|(_, v)| v.clone());
        let blue_key_keyframes = clip.effect_keyframes().iter()
            .find(|(k, _)| k.ends_with(":blue_shift"))
            .map(|(_, v)| v.clone());
        assert_eq!(red_key_keyframes.as_deref(), Some(red.as_slice()));
        assert_eq!(blue_key_keyframes.as_deref(), Some(blue.as_slice()));
        // Both keys present — neither got dropped during the
        // double insert.
        assert_eq!(clip.effect_keyframes().iter().filter(|(k, _)| k.ends_with(":red_shift") || k.ends_with(":blue_shift")).count(), 2);
    }

    #[test]
    fn chromatic_replaces_existing_keyframes_for_idempotency() {
        // Re-applying with new keyframes must replace — not
        // append/accumulate. Same idempotency contract as
        // RadialBlur.
        let c = fresh_clip();
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        let red1 = vec![kf(0.0, 0.0), kf(0.5, -3.0)];
        let blue1 = vec![kf(0.0, 0.0), kf(0.5, 3.0)];
        apply_hyperzoom_chromatic_aberration_to_clip(&mut seq, cid, (0.5, 0.5), 3.0, &red1, &blue1);
        let red2 = vec![kf(0.0, 0.0), kf(0.5, -10.0)];
        let blue2 = vec![kf(0.0, 0.0), kf(0.5, 10.0)];
        apply_hyperzoom_chromatic_aberration_to_clip(&mut seq, cid, (0.5, 0.5), 10.0, &red2, &blue2);
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        let red_kfs = clip.effect_keyframes().iter()
            .find(|(k, _)| k.ends_with(":red_shift"))
            .map(|(_, v)| v.clone()).unwrap();
        assert_eq!(red_kfs.len(), 2);
        // Peak value reflects the SECOND apply, not accumulated.
        assert!((red_kfs[1].value - -10.0).abs() < 1e-6);
    }

    // ────── Light-leak (FilmHalation) handler ──────

    use super::apply_hyperzoom_light_leak_to_clip;

    fn read_halation(clip: &Clip) -> Option<(f32, f32, f32, f32)> {
        test_clip::effects(clip).iter().find_map(|e| match e {
            BuiltinEffect::FilmHalation { threshold, radius, intensity, hue } => {
                Some((*threshold, *radius, *intensity, *hue))
            }
            _ => None,
        })
    }

    #[test]
    fn light_leak_inserts_when_absent_with_warm_defaults() {
        // No FilmHalation on the clip → handler appends one with
        // canonical trailer-aesthetic defaults (warm orange-yellow
        // hue 0.15, threshold 0.75, radius 15.0) and intensity at
        // peak.
        let c = fresh_clip();
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        let kfs = vec![kf(0.0, 0.0), kf(0.5, 0.4)];
        apply_hyperzoom_light_leak_to_clip(&mut seq, cid, 0.4, &kfs);
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        let (thr, rad, intensity, hue) = read_halation(clip).expect("FilmHalation appended");
        assert!((thr - 0.75).abs() < 1e-6);
        assert!((rad - 15.0).abs() < 1e-6);
        assert!((intensity - 0.4).abs() < 1e-6, "intensity at peak");
        assert!((hue - 0.15).abs() < 1e-6, "warm hue default");
    }

    #[test]
    fn light_leak_updates_existing_preserving_user_secondaries() {
        // Pre-existing FilmHalation with custom threshold/radius/
        // hue. Handler rewrites ONLY intensity; secondaries
        // survive (user-tweaked tunings preserved).
        let c = TestClip::of(fresh_clip()).effect(BuiltinEffect::FilmHalation {
            threshold: 0.5, radius: 25.0, intensity: 0.1, hue: 0.7,
        }).build();
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        let kfs = vec![kf(0.0, 0.0), kf(0.5, 0.6)];
        apply_hyperzoom_light_leak_to_clip(&mut seq, cid, 0.6, &kfs);
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        let (thr, rad, intensity, hue) = read_halation(clip).unwrap();
        assert!((thr - 0.5).abs() < 1e-6, "user threshold preserved");
        assert!((rad - 25.0).abs() < 1e-6, "user radius preserved");
        assert!((hue - 0.7).abs() < 1e-6, "user hue preserved");
        assert!((intensity - 0.6).abs() < 1e-6, "intensity rewritten to peak");
    }

    #[test]
    fn light_leak_replaces_existing_keyframes_for_idempotency() {
        let c = fresh_clip();
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        let kfs1 = vec![kf(0.0, 0.0), kf(0.5, 0.2)];
        apply_hyperzoom_light_leak_to_clip(&mut seq, cid, 0.2, &kfs1);
        let kfs2 = vec![kf(0.0, 0.0), kf(0.5, 0.8)];
        apply_hyperzoom_light_leak_to_clip(&mut seq, cid, 0.8, &kfs2);
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        let intensity_kfs = clip.effect_keyframes().iter()
            .find(|(k, _)| k.ends_with(":intensity"))
            .map(|(_, v)| v.clone()).unwrap();
        assert_eq!(intensity_kfs.len(), 2);
        assert!((intensity_kfs[1].value - 0.8).abs() < 1e-6);
    }
}
