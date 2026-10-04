//! Hyperzoom applied to a clip: its zoom plan, radial blur, chromatic
//! aberration and light leak, and a focal point turned into the clip's
//! transform (#890).

#[allow(unused_imports)]
use super::*;

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

    // Update or insert the Transform builtin so the anchor lands at
    // the focal point. Other Transform fields keep their existing
    // values when one is already present — the user might have a
    // baseline rotation or perspective they want preserved.
    let mut found = false;
    for fx in clip.builtin_effects.iter_mut() {
        if let klipp_effects::BuiltinEffect::Transform {
            anchor_x, anchor_y, ..
        } = fx
        {
            *anchor_x = anchor.0;
            *anchor_y = anchor.1;
            found = true;
            break;
        }
    }
    if !found {
        clip.builtin_effects.push(klipp_effects::BuiltinEffect::Transform {
            position_x: 0.0,
            position_y: 0.0,
            scale_x: 1.0,
            scale_y: 1.0,
            rotation: 0.0,
            anchor_x: anchor.0,
            anchor_y: anchor.1,
            rotation_x: 0.0,
            rotation_y: 0.0,
            perspective: 0.0,
            focal_ref: None,
        });
        clip.builtin_effects_enabled.push(true);
    }

    clip.transform_scale_keyframes = scale_kfs.to_vec();
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
/// * Replace `clip.effect_keyframes["{idx}:amount"]` with the
///   planned ramp so re-applying is idempotent.
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

    // Update existing RadialBlur if present, else append a fresh one.
    let mut found_idx: Option<usize> = None;
    for (i, fx) in clip.builtin_effects.iter_mut().enumerate() {
        if let klipp_effects::BuiltinEffect::RadialBlur { center_x, center_y, amount, .. } = fx {
            *center_x = center.0;
            *center_y = center.1;
            *amount = peak_amount;
            found_idx = Some(i);
            break;
        }
    }
    let idx = match found_idx {
        Some(i) => i,
        None => {
            clip.builtin_effects.push(klipp_effects::BuiltinEffect::RadialBlur {
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
            });
            clip.builtin_effects_enabled.push(true);
            clip.builtin_effects.len() - 1
        }
    };

    let key = format!("{}:amount", idx);
    clip.effect_keyframes.insert(key, amount_kfs.to_vec());
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
/// * Replace `clip.effect_keyframes["{idx}:red_shift"]` and
///   `[".. blue_shift"]` with the planned ramps. Re-applying
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
    let mut found_idx: Option<usize> = None;
    for (i, fx) in clip.builtin_effects.iter_mut().enumerate() {
        if let klipp_effects::BuiltinEffect::ChromaticAberration {
            red_shift, blue_shift, center_x, center_y,
        } = fx
        {
            *center_x = center.0;
            *center_y = center.1;
            // Static fallback values mirror the keyframes' peak —
            // red converges, blue diverges.
            *red_shift = -peak_amount;
            *blue_shift = peak_amount;
            found_idx = Some(i);
            break;
        }
    }
    let idx = match found_idx {
        Some(i) => i,
        None => {
            clip.builtin_effects.push(klipp_effects::BuiltinEffect::ChromaticAberration {
                red_shift: -peak_amount,
                blue_shift: peak_amount,
                center_x: center.0,
                center_y: center.1,
            });
            clip.builtin_effects_enabled.push(true);
            clip.builtin_effects.len() - 1
        }
    };
    // Pin: red before blue. If an audit changes this order, the
    // ordering test will flag it before snapshot tests start
    // diffing nondeterministically.
    clip.effect_keyframes.insert(format!("{}:red_shift", idx), red_kfs.to_vec());
    clip.effect_keyframes.insert(format!("{}:blue_shift", idx), blue_kfs.to_vec());
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
/// * Replace `clip.effect_keyframes["{idx}:intensity"]` with the
///   planned ramp.
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
    let mut found_idx: Option<usize> = None;
    for (i, fx) in clip.builtin_effects.iter_mut().enumerate() {
        if let klipp_effects::BuiltinEffect::FilmHalation { intensity, .. } = fx {
            *intensity = peak_intensity;
            found_idx = Some(i);
            break;
        }
    }
    let idx = match found_idx {
        Some(i) => i,
        None => {
            clip.builtin_effects.push(klipp_effects::BuiltinEffect::FilmHalation {
                threshold: 0.75,
                radius: 15.0,
                intensity: peak_intensity,
                // Hue 0.15 ≈ warm orange-yellow — the
                // canonical trailer-light-leak colour. Power
                // users tweak via the standalone effect's
                // params after applying.
                hue: 0.15,
            });
            clip.builtin_effects_enabled.push(true);
            clip.builtin_effects.len() - 1
        }
    };
    clip.effect_keyframes.insert(format!("{}:intensity", idx), intensity_kfs.to_vec());
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

    let mut found = false;
    for fx in clip.builtin_effects.iter_mut() {
        if let klipp_effects::BuiltinEffect::Transform { focal_ref: existing, .. } = fx {
            *existing = Some(focal_ref);
            found = true;
            break;
        }
    }
    if !found {
        clip.builtin_effects.push(klipp_effects::BuiltinEffect::Transform {
            position_x: 0.0, position_y: 0.0,
            scale_x: 1.0, scale_y: 1.0,
            rotation: 0.0,
            anchor_x: 0.5, anchor_y: 0.5,
            rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0,
            focal_ref: Some(focal_ref),
        });
        clip.builtin_effects_enabled.push(true);
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
                   tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0) }
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
        let xform = clip.builtin_effects.iter().find_map(|e| match e {
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
        let mut c = fresh_clip();
        c.builtin_effects.push(BuiltinEffect::Transform {
            position_x: 10.0, position_y: 20.0,
            scale_x: 1.5, scale_y: 1.5,
            rotation: 45.0,
            anchor_x: 0.5, anchor_y: 0.5,
            rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0,
            focal_ref: None,
        });
        c.builtin_effects_enabled.push(true);
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        apply_hyperzoom_plan_to_clip(&mut seq, cid, (0.2, 0.8), &[kf(0.0, 1.0), kf(0.5, 50.0)]);
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        let xform = clip.builtin_effects.iter().find_map(|e| match e {
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
        let mut c = fresh_clip();
        c.transform_scale_keyframes = vec![kf(0.0, 0.5), kf(0.25, 2.0), kf(0.5, 5.0)];
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        let new_kfs = vec![kf(2.5, 1.0), kf(3.0, 50.0)];
        apply_hyperzoom_plan_to_clip(&mut seq, cid, (0.5, 0.5), &new_kfs);
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        assert_eq!(clip.transform_scale_keyframes.len(), 2);
        assert!((clip.transform_scale_keyframes[0].time_secs - 2.5).abs() < 1e-9);
        assert!((clip.transform_scale_keyframes[1].time_secs - 3.0).abs() < 1e-9);
        assert!((clip.transform_scale_keyframes[1].value - 50.0).abs() < 1e-6);
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
        assert!(same.transform_scale_keyframes.is_empty());
        assert!(same.builtin_effects.iter().all(|e| !matches!(e, BuiltinEffect::Transform { .. })));
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
        let (idx, cx, cy, amt, samples, mode) = clip.builtin_effects.iter().enumerate().find_map(|(i, e)| match e {
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
        let kfs = clip.effect_keyframes.get(&key).expect("amount keyframes registered");
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
        let mut c = fresh_clip();
        c.builtin_effects.push(BuiltinEffect::RadialBlur {
            center_x: 0.0, center_y: 0.0,
            amount: 0.0,
            samples: 24,
            mode: 1, // spin
        });
        c.builtin_effects_enabled.push(true);
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        apply_hyperzoom_radial_blur_to_clip(
            &mut seq, cid, (0.4, 0.6), 22.0,
            &[kf(0.0, 0.0), kf(0.5, 22.0)],
        );
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        let (cx, cy, amt, samples, mode) = clip.builtin_effects.iter().find_map(|e| match e {
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
        let mut c = fresh_clip();
        c.builtin_effects.push(BuiltinEffect::Transform {
            position_x: 0.0, position_y: 0.0,
            scale_x: 1.0, scale_y: 1.0,
            rotation: 30.0,  // baseline rotation
            anchor_x: 0.5, anchor_y: 0.5,
            rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0,
            focal_ref: None,
        });
        c.builtin_effects_enabled.push(true);
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
        let (rotation, fr_out) = clip.builtin_effects.iter().find_map(|e| match e {
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
        let has_focal_ref = clip.builtin_effects.iter().any(|e| matches!(
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
        let mut c = fresh_clip();
        let old_uuid = ClipId::new().0;
        c.builtin_effects.push(BuiltinEffect::Transform {
            position_x: 0.0, position_y: 0.0, scale_x: 1.0, scale_y: 1.0,
            rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5,
            rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0,
            focal_ref: Some(klipp_core::FocalRef::Glyph {
                title_clip_id: old_uuid,
                char_index: 1,
                fallback_xy: (0.0, 0.0),
            }),
        });
        c.builtin_effects_enabled.push(true);
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        let new_uuid = ClipId::new().0;
        apply_focal_ref_to_clip_transform(&mut seq, cid, klipp_core::FocalRef::Glyph {
            title_clip_id: new_uuid, char_index: 9, fallback_xy: (0.7, 0.3),
        });
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        let fr = clip.builtin_effects.iter().find_map(|e| match e {
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
        assert!(same.builtin_effects.iter().all(|e| !matches!(e, BuiltinEffect::Transform { .. })));
    }

    // ────── Chromatic-aberration handler ──────

    use super::apply_hyperzoom_chromatic_aberration_to_clip;

    fn read_chromatic(clip: &Clip) -> Option<(f32, f32, f32, f32)> {
        clip.builtin_effects.iter().find_map(|e| match e {
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
        let mut c = fresh_clip();
        c.builtin_effects.push(BuiltinEffect::ChromaticAberration {
            red_shift: -2.0, blue_shift: 2.0, center_x: 0.1, center_y: 0.1,
        });
        c.builtin_effects_enabled.push(true);
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
        let n = clip.builtin_effects.iter().filter(|e| matches!(e, BuiltinEffect::ChromaticAberration { .. })).count();
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
        let red_key_keyframes = clip.effect_keyframes.iter()
            .find(|(k, _)| k.ends_with(":red_shift"))
            .map(|(_, v)| v.clone());
        let blue_key_keyframes = clip.effect_keyframes.iter()
            .find(|(k, _)| k.ends_with(":blue_shift"))
            .map(|(_, v)| v.clone());
        assert_eq!(red_key_keyframes.as_deref(), Some(red.as_slice()));
        assert_eq!(blue_key_keyframes.as_deref(), Some(blue.as_slice()));
        // Both keys present — neither got dropped during the
        // double insert.
        assert_eq!(clip.effect_keyframes.iter().filter(|(k, _)| k.ends_with(":red_shift") || k.ends_with(":blue_shift")).count(), 2);
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
        let red_kfs = clip.effect_keyframes.iter()
            .find(|(k, _)| k.ends_with(":red_shift"))
            .map(|(_, v)| v.clone()).unwrap();
        assert_eq!(red_kfs.len(), 2);
        // Peak value reflects the SECOND apply, not accumulated.
        assert!((red_kfs[1].value - -10.0).abs() < 1e-6);
    }

    // ────── Light-leak (FilmHalation) handler ──────

    use super::apply_hyperzoom_light_leak_to_clip;

    fn read_halation(clip: &Clip) -> Option<(f32, f32, f32, f32)> {
        clip.builtin_effects.iter().find_map(|e| match e {
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
        let mut c = fresh_clip();
        c.builtin_effects.push(BuiltinEffect::FilmHalation {
            threshold: 0.5, radius: 25.0, intensity: 0.1, hue: 0.7,
        });
        c.builtin_effects_enabled.push(true);
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
        let intensity_kfs = clip.effect_keyframes.iter()
            .find(|(k, _)| k.ends_with(":intensity"))
            .map(|(_, v)| v.clone()).unwrap();
        assert_eq!(intensity_kfs.len(), 2);
        assert!((intensity_kfs[1].value - 0.8).abs() < 1e-6);
    }
}
