//! Whether playback's picture can change from one timeline frame to the
//! next when the primary track's file has no new frame for it: a layer
//! over it with a frame of its own, a title or generator that moves, a
//! clip that animates, a transition, a clip coming or going. The engine
//! draws the frame again only then, so a lone 24 fps clip in a 30 fps
//! sequence is drawn 24 times a second and a 30 fps layer over it 30, and
//! playback's health counts as due only the frames that could change.

use std::collections::HashMap;

use klipp_core::{ClipId, Timecode};
use klipp_media::MediaPool;
use klipp_timeline::{Clip, ClipKind, Sequence, TrackKind};

/// Whether anything but the primary's own picture can differ between
/// `prev` and `at` on `seq` at `fps`. `primary` is the clip the primary
/// decoder plays: its file's frames are its own path's, so only its
/// animation counts here. `titles` keeps, per title clip, whether it moves
/// at all, as working that out reads the whole title.
pub fn picture_changes(seq: &Sequence, pool: &MediaPool, primary: Option<ClipId>, prev: Timecode, at: Timecode, titles: &mut HashMap<ClipId, bool>) -> bool {
    let fps = seq.fps.as_f64();
    for track in seq.tracks.iter().filter(|t| t.visible && matches!(t.kind, TrackKind::Video | TrackKind::Text)) {
        let now = track.clip_at_timecode(at).map(|(_, c)| c);
        let before = track.clip_at_timecode(prev).map(|(_, c)| c);
        // A clip comes or goes, or another takes its place.
        if now.map(|c| c.id) != before.map(|c| c.id) {
            return true;
        }
        let Some(c) = now.filter(|c| c.enabled) else { continue };
        // A transition draws both its clips and its own progress.
        if track.transitions.iter().any(|t| t.clip_a_id == c.id || t.clip_b_id == c.id) && in_transition(track, c, at) {
            return true;
        }
        if animates(c) {
            return true;
        }
        if Some(c.id) == primary {
            continue;
        }
        let moves = match &c.kind {
            ClipKind::Media { media_id } => match pool.get(media_id) {
                // A still holds its one picture.
                Some(item) if item.is_still => false,
                // A video layer changes when the file frame nearest the
                // timeline frame does, as export picks it (#696). Frozen,
                // its frames are kept per timeline frame and change with it.
                Some(item) if !track.frozen => {
                    let rate = item.fps.as_f64().max(1.0);
                    let frame = |t: Timecode| (klipp_compose::render::source_secs(c, t, fps) * rate).round() as i64;
                    frame(at) != frame(prev)
                }
                _ => true,
            },
            ClipKind::Title(_) => *titles.entry(c.id).or_insert_with(|| title_moves(c)),
            ClipKind::Generator(g) => generator_moves(g),
            // A nest, a precomposition, anything else: drawn each frame.
            _ => true,
        };
        if moves {
            return true;
        }
    }
    false
}

/// The clip changes its own look over time: a placement or opacity key
/// that changes, a modifier (wiggle and the like), an effect's key that
/// changes, or an effect that moves by itself (grain, flicker, a pin to a
/// tracked surface).
fn animates(c: &Clip) -> bool {
    let changing = |keys: &[klipp_core::Keyframe]| keys.windows(2).any(|w| (w[0].value - w[1].value).abs() > 1e-6);
    if c.has_param_modifiers() || c.has_changing_transform_keys() || changing(c.opacity_keyframes()) {
        return true;
    }
    // A picture's fade is its opacity's keys, above; a clip's own fades
    // are its sound's.
    c.effect_keyframes().values().any(|k| changing(k)) || c.enabled_effects().any(klipp_compose::render::effect_varies_in_time)
}

/// Whether `c` is inside one of `track`'s transitions at `at`.
fn in_transition(track: &klipp_timeline::Track, c: &Clip, at: Timecode) -> bool {
    track.transitions.iter().filter(|t| t.clip_a_id == c.id || t.clip_b_id == c.id).any(|t| {
        let half = t.duration.to_secs_f64();
        let cut = if t.clip_a_id == c.id { c.timeline_out } else { c.timeline_in };
        (at.to_secs_f64() - cut.to_secs_f64()).abs() <= half
    })
}

/// A title that moves at all: an entrance or exit, a loop, an animation by
/// character, a key on any of its objects. Saved, a title leaves each of
/// these out while it is still, so their presence is the answer.
fn title_moves(c: &Clip) -> bool {
    let ClipKind::Title(td) = &c.kind else { return false };
    let Ok(v) = serde_json::to_value(td) else { return true };
    moving_json(&v)
}

fn moving_json(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Object(m) => m.iter().any(|(k, v)| {
            let set = !(v.is_null() || v.as_array().is_some_and(|a| a.is_empty()));
            (set && (k == "motion" || k == "loops" || k == "char_animation" || k.ends_with("_keyframes"))) || moving_json(v)
        }),
        serde_json::Value::Array(a) => a.iter().any(moving_json),
        _ => false,
    }
}

/// The generators that move by themselves; the others are a still picture.
fn generator_moves(g: &klipp_core::GeneratorData) -> bool {
    use klipp_core::GeneratorData as G;
    matches!(g, G::LivingGradient { .. } | G::Grain { .. } | G::Countdown | G::Timer { .. })
}

/// What playback is due to show a second, for its health: the sequence's
/// rate while anything but the primary's picture can change, or with no
/// primary file playing; else the primary's own rate at its speed, the
/// sequence's at most. A lone 24 fps clip in a 30 fps sequence is due 24;
/// a still, whose rate is 0 here, nothing.
pub fn due_rate(seq_fps: f64, primary_fps: Option<f64>, speed: f64, others_change: bool) -> f64 {
    match primary_fps {
        Some(f) if !others_change => (f.max(0.0) * speed.abs()).min(seq_fps),
        _ => seq_fps,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lone_slower_clip_is_due_at_its_own_rate() {
        assert_eq!(due_rate(30.0, Some(24.0), 1.0, false), 24.0);
        assert_eq!(due_rate(30.0, Some(24.0), 1.0, true), 30.0, "a layer over it changes every frame");
        assert_eq!(due_rate(30.0, Some(60.0), 1.0, false), 30.0, "never more than the sequence's");
        assert_eq!(due_rate(60.0, Some(30.0), 1.0, false), 30.0);
        assert_eq!(due_rate(30.0, None, 1.0, false), 30.0, "no primary: the sequence's rate");
        assert_eq!(due_rate(30.0, Some(0.0), 1.0, false), 0.0, "a still alone changes nothing");
        assert_eq!(due_rate(30.0, Some(0.0), 1.0, true), 30.0);
        assert_eq!(due_rate(30.0, Some(24.0), 0.5, false), 12.0, "at half speed, half its frames");
        assert!((due_rate(30000.0 / 1001.0, Some(24000.0 / 1001.0), 1.0, false) - 23.976).abs() < 1e-3);
    }

    #[test]
    fn a_title_is_moving_only_with_motion_loops_or_keys() {
        let still = serde_json::json!({"objects": [{"content": {"TextBox": {"text": "Hi"}}, "fx": {}, "transform": {"center": {"x": 0.5}}}]});
        assert!(!moving_json(&still));
        let entering = serde_json::json!({"objects": [{"fx": {"motion": {"enter": {"kind": "Fade"}}}}]});
        assert!(moving_json(&entering));
        let looping = serde_json::json!({"objects": [{"fx": {"loops": [{"Wave": {}}]}}]});
        assert!(moving_json(&looping));
        let keyed = serde_json::json!({"objects": [{"transform": {"center_x_keyframes": [{"value": 0.1}]}}]});
        assert!(moving_json(&keyed));
        let empty_lanes = serde_json::json!({"objects": [{"transform": {"center_x_keyframes": [], "char_animation": null}}]});
        assert!(!moving_json(&empty_lanes));
    }
}
