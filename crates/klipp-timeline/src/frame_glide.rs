//! Frame Glide (#611): a clip's move between two framings, composed
//! directly over the picture — where the shot starts and where it ends —
//! instead of keyframing position and scale by hand.
//!
//! A glide is stored on the clip and written into the clip's own
//! position and scale keyframes: there is no second rendering model.
//! The keys run over the clip's *visible* length, so they are written
//! again whenever that changes ([`refresh`], from the flatten that both
//! the viewer and the export render): a trimmed clip keeps Start on its
//! first frame and End on its last. Keys edited by hand afterwards are
//! the editor's: the glide only rewrites keys that are still its own.

use klipp_core::{Interpolation, Keyframe};

use crate::{Clip, Sequence};

pub use klipp_core::frame_glide::{FrameGlide, GlideEasing, GlideFrame};

/// Share of the move's length a Bezier handle reaches, as the Ken Burns
/// presets ease.
const EASE: f64 = 0.42;

/// The position x, position y and scale keys of `glide` over `len_secs`
/// of source time (the clip's visible length), for a `frame_w` x
/// `frame_h` frame and a picture scaled about `anchor`.
pub fn keys(glide: &FrameGlide, len_secs: f64, anchor: [f32; 2], frame_w: f32, frame_h: f32) -> [Vec<Keyframe>; 3] {
    let len = len_secs.max(1e-3);
    let (s0, x0, y0) = glide.start.transform_about(anchor, frame_w, frame_h);
    let (s1, x1, y1) = glide.end.transform_about(anchor, frame_w, frame_h);
    let (out_handle, in_handle) = match glide.easing {
        GlideEasing::Linear => (0.0, 0.0),
        GlideEasing::EaseIn => (EASE * len, 0.0),
        GlideEasing::EaseOut => (0.0, EASE * len),
        GlideEasing::EaseInOut => (EASE * len, EASE * len),
    };
    let interpolation = if glide.easing == GlideEasing::Linear { Interpolation::Linear } else { Interpolation::Bezier };
    let pair = |a: f32, b: f32| {
        vec![
            Keyframe { time_secs: 0.0, value: a, interpolation, tangent_out: (out_handle, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() },
            Keyframe { time_secs: len, value: b, interpolation, tangent_out: (0.0, 0.0), tangent_in: (-in_handle, 0.0), handles: Default::default() },
        ]
    };
    [pair(x0, x1), pair(y0, y1), pair(s0, s1)]
}

/// The clip's visible length in source seconds, the span its keys run on.
pub fn visible_len_secs(clip: &Clip) -> f64 {
    (clip.source_out.to_secs_f64() - clip.source_in.to_secs_f64()).abs()
}

/// The point the clip's picture scales about, as fractions of the frame:
/// its placement's anchor, the centre where it has none.
pub fn anchor(clip: &Clip) -> [f32; 2] {
    match clip.picture_v2() {
        Ok(lifted) => [lifted.picture.placement.anchor_x.unwrap_or(0.5), lifted.picture.placement.anchor_y.unwrap_or(0.5)],
        Err(_) => [0.5, 0.5],
    }
}

/// The keys of the clip's placement lane `key` (`pos_x`, `pos_y`,
/// `scale`, `scale_y`), as its picture holds them.
pub fn lane(clip: &Clip, key: &str) -> Vec<Keyframe> {
    let Ok(lifted) = clip.picture_v2() else { return Vec::new() };
    lifted
        .picture
        .placement_anim
        .get(&klipp_graph_format::ParamKey::from(key))
        .map(|a| a.keys.iter().filter_map(|k| serde_json::from_value(k.clone()).ok()).collect())
        .unwrap_or_default()
}

/// The clip's position x, position y and scale lanes, and whether its
/// height is keyed apart (which a glide would not drive).
fn lanes(clip: &Clip) -> ([Vec<Keyframe>; 3], bool) {
    ([lane(clip, "pos_x"), lane(clip, "pos_y"), lane(clip, "scale")], !lane(clip, "scale_y").is_empty())
}

/// The clip's lanes set to `keys` on its placement, by path: each lane's
/// modifier stack kept; the height following the width again (#1471).
fn set_lanes(clip: &mut Clip, [x, y, s]: [Vec<Keyframe>; 3]) {
    use klipp_graph_lift::ops::GraphOp;
    let Some(at) = clip.placement_path() else { return };
    let modifiers = |key: &str| clip.picture_v2().ok().and_then(|p| p.picture.placement_anim.get(&klipp_graph_format::ParamKey::from(key)).and_then(|a| a.modifiers.clone()));
    let anim = |key: &str, keys: Vec<Keyframe>| {
        let modifiers = modifiers(key);
        let keys: Vec<serde_json::Value> = keys.iter().filter_map(|k| serde_json::to_value(k).ok()).collect();
        let anim = (!keys.is_empty() || modifiers.is_some()).then_some(klipp_graph_format::Animation { keys, modifiers });
        GraphOp::SetAnim { path: at.clone(), key: klipp_graph_format::ParamKey::from(key), anim }
    };
    let ops = [anim("pos_x", x), anim("pos_y", y), anim("scale", s), anim("scale_y", Vec::new())];
    if let Err(why) = clip.apply_picture_ops(&ops) {
        tracing::warn!("frame glide: clip {:?}: {why}", clip.id);
    }
}

/// `clip` held at rest for framing (#611): its position and scale each
/// one key at rest, so the viewer shows the whole picture whatever its
/// placement is set to where nothing keys it.
pub fn hold_at_rest(clip: &mut Clip) {
    let rest = |value: f32| vec![Keyframe { time_secs: 0.0, value, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }];
    set_lanes(clip, [rest(0.0), rest(0.0), rest(1.0)]);
}

/// Whether `clip`'s position and scale keys are still the ones its glide
/// wrote, over any length: two keys each, at 0 and one shared end, with
/// the glide's values. Keys edited by hand since are not.
pub fn keys_are_the_glide(clip: &Clip, frame_w: f32, frame_h: f32) -> bool {
    let Some(glide) = clip.frame_glide else { return false };
    let (tracks, height_keyed) = lanes(clip);
    if height_keyed {
        return false;
    }
    let Some(end) = tracks[0].get(1).map(|k| k.time_secs) else { return false };
    let want = keys(&glide, end, anchor(clip), frame_w, frame_h);
    tracks.iter().zip(want.iter()).all(|(have, want)| {
        have.len() == 2
            && have.iter().zip(want.iter()).all(|(h, w)| {
                (h.time_secs - w.time_secs).abs() < 1e-4 && (h.value - w.value).abs() < 1e-3 && h.interpolation == w.interpolation
            })
    })
}

/// Write `clip`'s glide into its position and scale keys over its visible
/// length, when the keys are still the glide's own (or the clip has none
/// of either yet). Whether anything changed.
pub fn write(clip: &mut Clip, frame_w: f32, frame_h: f32) -> bool {
    let Some(glide) = clip.frame_glide else { return false };
    if !keys_are_free(clip, frame_w, frame_h) {
        return false;
    }
    let want = keys(&glide, visible_len_secs(clip), anchor(clip), frame_w, frame_h);
    let (have, _) = lanes(clip);
    if have == want {
        return false;
    }
    set_lanes(clip, want);
    true
}

/// Whether `clip`'s position and scale keys are the glide's to replace:
/// it has none, or they are still the ones its glide wrote.
pub fn keys_are_free(clip: &Clip, frame_w: f32, frame_h: f32) -> bool {
    let (tracks, height_keyed) = lanes(clip);
    let none = tracks.iter().all(|t| t.is_empty()) && !height_keyed;
    none || keys_are_the_glide(clip, frame_w, frame_h)
}

/// `clip` with its glide set to `glide` (#611), keys and all, or taken
/// off with `None`. Animation of the clip's own on position or scale is
/// replaced only with `replace_animation`; otherwise the edit is refused,
/// with why. Taking a glide off takes its keys with it; keys edited by
/// hand stay.
pub fn set(clip: &Clip, glide: Option<FrameGlide>, replace_animation: bool, frame_w: f32, frame_h: f32) -> Result<Clip, String> {
    let free = keys_are_free(clip, frame_w, frame_h);
    let mut new = clip.clone();
    match glide {
        Some(g) => {
            if !free && !replace_animation {
                return Err("the clip's position or scale is animated already: replace that animation to glide it".into());
            }
            // The keys are the placement's own, and a clip always has a
            // placement: nothing to add (M3 step 13).
            set_lanes(&mut new, Default::default());
            new.frame_glide = Some(g);
            write(&mut new, frame_w, frame_h);
        }
        None => {
            if free {
                set_lanes(&mut new, Default::default());
            }
            new.frame_glide = None;
        }
    }
    Ok(new)
}

/// Every glide in `seq` written over its clip's current length: run in
/// the flatten the viewer and the export render from, so a trim never
/// leaves End short of the clip's last frame.
pub fn refresh(seq: &mut Sequence) {
    let (w, h) = (seq.resolution.width as f32, seq.resolution.height as f32);
    for track in &mut seq.tracks {
        for clip in track.clips.iter_mut().filter(|c| c.frame_glide.is_some()) {
            write(clip, w, h);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::RationalTime;

    fn tc(s: f64) -> klipp_core::Timecode {
        RationalTime::from_secs_f64(s, 25)
    }

    /// The clip's scale lane set by hand, by path as the inspector sets it.
    fn set_scale(clip: &mut Clip, keys: Vec<Keyframe>) {
        let [x, y] = [lane(clip, "pos_x"), lane(clip, "pos_y")];
        set_lanes(clip, [x, y, keys]);
    }

    fn clip(len: f64) -> Clip {
        Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(len), tc(1.0), tc(1.0 + len))
    }

    #[test]
    fn a_framing_and_its_transform_are_one_thing() {
        let f = GlideFrame { x: 0.1, y: -0.05, width: 0.5 };
        let (s, px, py) = f.transform(1920.0, 1080.0);
        assert_eq!(s, 2.0);
        assert!((px - -384.0).abs() < 1e-3 && (py - 108.0).abs() < 1e-3, "{px} {py}");
        let back = GlideFrame::from_transform(s, px, py, 1920.0, 1080.0);
        assert!((back.x - f.x).abs() < 1e-6 && (back.y - f.y).abs() < 1e-6 && (back.width - f.width).abs() < 1e-6, "{back:?}");
        assert_eq!(GlideFrame::FULL.transform(1920.0, 1080.0), (1.0, -0.0, -0.0));
    }

    /// The glide runs over the visible length: a trim moves End to the
    /// clip's new last frame.
    #[test]
    fn the_glide_follows_a_trim() {
        let mut c = clip(4.0);
        c.frame_glide = Some(FrameGlide { start: GlideFrame::FULL, end: GlideFrame { x: 0.0, y: 0.0, width: 0.5 }, easing: GlideEasing::Linear });
        assert!(write(&mut c, 1920.0, 1080.0));
        assert_eq!(lane(&c, "scale").iter().map(|k| (k.time_secs, k.value)).collect::<Vec<_>>(), vec![(0.0, 1.0), (4.0, 2.0)]);
        c.source_out = tc(3.6);
        assert!(write(&mut c, 1920.0, 1080.0), "the trim rewrites the keys");
        assert!((lane(&c, "scale")[1].time_secs - 2.6).abs() < 1e-9);
        assert!(!write(&mut c, 1920.0, 1080.0), "nothing to do twice");
    }

    /// Keys edited by hand are the editor's: the glide leaves them alone.
    #[test]
    fn hand_edited_keys_are_not_overwritten() {
        let mut c = clip(4.0);
        c.frame_glide = Some(FrameGlide { end: GlideFrame { x: 0.2, y: 0.0, width: 0.6 }, ..FrameGlide::default() });
        write(&mut c, 1920.0, 1080.0);
        assert!(keys_are_the_glide(&c, 1920.0, 1080.0));
        let mut hand = lane(&c, "scale");
        hand[1].value = 3.0;
        set_scale(&mut c, hand);
        c.source_out = tc(3.0);
        assert!(!write(&mut c, 1920.0, 1080.0));
        assert_eq!(lane(&c, "scale")[1].value, 3.0);
        // And existing animation on a clip that never had a glide is never touched.
        let mut animated = clip(4.0);
        set_scale(&mut animated, vec![Keyframe { time_secs: 1.0, value: 1.5, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }]);
        animated.frame_glide = Some(FrameGlide::default());
        assert!(!write(&mut animated, 1920.0, 1080.0));
    }

    /// Setting a glide writes its keys; changing it rewrites them; taking it
    /// off takes them away. A clip animated by hand is refused unless the
    /// animation is to be replaced.
    #[test]
    fn a_glide_is_set_changed_and_taken_off() {
        let push = FrameGlide { end: GlideFrame { x: 0.0, y: 0.0, width: 0.5 }, ..FrameGlide::default() };
        let c = set(&clip(4.0), Some(push), false, 1920.0, 1080.0).unwrap();
        assert_eq!(lane(&c, "scale").iter().map(|k| k.value).collect::<Vec<_>>(), vec![1.0, 2.0]);
        let pull = FrameGlide { start: push.end, end: push.start, ..push };
        let c = set(&c, Some(pull), false, 1920.0, 1080.0).unwrap();
        assert_eq!(lane(&c, "scale").iter().map(|k| k.value).collect::<Vec<_>>(), vec![2.0, 1.0], "its own keys are rewritten");
        let off = set(&c, None, false, 1920.0, 1080.0).unwrap();
        assert!(off.frame_glide.is_none() && lane(&off, "scale").is_empty());

        let mut animated = clip(4.0);
        set_scale(&mut animated, vec![Keyframe { time_secs: 1.0, value: 1.5, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }]);
        assert!(set(&animated, Some(push), false, 1920.0, 1080.0).is_err(), "never silently overwritten");
        let replaced = set(&animated, Some(push), true, 1920.0, 1080.0).unwrap();
        assert_eq!(lane(&replaced, "scale").len(), 2);
        // Off again on a clip whose keys were edited by hand: the keys stay.
        let mut edited = c.clone();
        let mut hand = lane(&edited, "scale");
        hand[0].value = 1.3;
        set_scale(&mut edited, hand);
        assert_eq!(lane(&set(&edited, None, false, 1920.0, 1080.0).unwrap(), "scale").len(), 2);
    }

    /// The viewer and the export both render from the flatten, and the
    /// flatten writes each glide over its clip's length as it is now: a
    /// clip trimmed since its keys were written moves End to its new end.
    #[test]
    fn the_flatten_renders_the_glide_over_the_trimmed_clip() {
        let mut seq = Sequence::new_default();
        let mut c = clip(4.0);
        c.frame_glide = Some(FrameGlide { end: GlideFrame { x: 0.0, y: 0.0, width: 0.5 }, easing: GlideEasing::Linear, ..FrameGlide::default() });
        let (w, h) = (seq.resolution.width as f32, seq.resolution.height as f32);
        write(&mut c, w, h);
        c.source_out = tc(3.0);
        c.timeline_out = tc(2.0);
        let id = c.id;
        seq.tracks[0].clips = vec![c];
        let flat = crate::flatten::flatten_sequence(&seq, std::slice::from_ref(&seq));
        let rendered = flat.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == id).expect("the clip");
        assert_eq!(lane(rendered, "scale").iter().map(|k| (k.time_secs, k.value)).collect::<Vec<_>>(), vec![(0.0, 1.0), (2.0, 2.0)]);
    }

    /// The four easings: linear keys, or Bezier handles on the slow ends.
    #[test]
    fn the_easings_shape_the_keys() {
        let g = |easing| keys(&FrameGlide { easing, ..FrameGlide::default() }, 4.0, [0.5, 0.5], 1920.0, 1080.0)[2].clone();
        let lin = g(GlideEasing::Linear);
        assert!(lin.iter().all(|k| k.interpolation == Interpolation::Linear));
        let ein = g(GlideEasing::EaseIn);
        assert!(ein[0].tangent_out.0 > 0.0 && ein[1].tangent_in.0 == 0.0, "slow start only");
        let eout = g(GlideEasing::EaseOut);
        assert!(eout[0].tangent_out.0 == 0.0 && eout[1].tangent_in.0 < 0.0, "slow end only");
        let both = g(GlideEasing::EaseInOut);
        assert!(both[0].tangent_out.0 > 0.0 && both[1].tangent_in.0 < 0.0);
    }
}
