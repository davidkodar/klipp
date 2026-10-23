//! A project holding what an edit holds, saved, opened and saved again
//! (#2461): a multicam group with its cuts and sound, linked picture and
//! sound, audio keys, clip and sequence markers, a transform link, a matte
//! and a stencil, every track flag, in and out points and the playhead, a
//! hidden subtitle track with its language, caption style and captions,
//! and a second sequence. Each sequence comes back whole (its saved form,
//! every field), and the second save is the first byte for byte.

use klipp_core::{GroupId, Keyframe, MediaId, MulticamSound, RationalTime};
use klipp_media::MediaPool;
use klipp_project::{load_project_strict, save_project};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};
use serde_json::json;
use std::path::{Path, PathBuf};

/// The top-level lines a save stamps afresh every time.
const STAMPS: [&str; 4] = ["  \"generation\":", "  \"saved_at\":", "  \"klipp_version\":", "  \"platform\":"];

fn project_text(dir: &Path) -> String {
    let text = std::fs::read_to_string(dir.join("project.json")).unwrap();
    text.lines().filter(|l| !STAMPS.iter().any(|s| l.starts_with(s))).collect::<Vec<_>>().join("\n")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("klipp_rich_round_trip_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn t(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

fn key(time_secs: f64, value: f32) -> Keyframe {
    serde_json::from_value(json!({ "time_secs": time_secs, "value": value, "interpolation": "Linear" })).unwrap()
}

fn caption(text: &str, from: f64, to: f64) -> Clip {
    let td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: text.into(), font_size: 40.0, ..Default::default() });
    Clip { kind: ClipKind::Title(td), source_in: t(0.0), source_out: t(to - from), ..Clip::gap(t(from), t(to)) }
}

/// The edit: two camera angles of one take in a multicam group cut 0 → 1 →
/// 0, each with its sound linked both ways, the group playing angle 1's
/// sound; a title over them that follows angle 0's transform; a lower
/// clip used as a luma matte; a stencil; captions on a hidden English
/// subtitle track; and the sequence's own range, playhead and markers.
fn rich() -> Sequence {
    let (cam_a, cam_b, music) = (MediaId::new(), MediaId::new(), MediaId::new());
    let group = GroupId::new();
    let mut primary = Clip::media(cam_a, t(0.0), t(10.0), t(2.0), t(12.0));
    (primary.multicam_group, primary.multicam_angle, primary.multicam_cuts, primary.multicam_sound) = (Some(group), 0, vec![(3.0, 1), (7.5, 0)], MulticamSound::Angle(1));
    let mut angle = Clip::media(cam_b, t(0.0), t(10.0), t(1.5), t(11.5));
    (angle.multicam_group, angle.multicam_angle) = (Some(group), 1);
    let mut sound_a = Clip::media(cam_a, t(0.0), t(10.0), t(2.0), t(12.0));
    let mut sound_b = Clip::media(cam_b, t(0.0), t(10.0), t(1.5), t(11.5));
    (primary.linked_clip_id, sound_a.linked_clip_id) = (Some(sound_a.id), Some(primary.id));
    (angle.linked_clip_id, sound_b.linked_clip_id) = (Some(sound_b.id), Some(angle.id));
    sound_a.volume_keyframes = vec![key(0.0, 1.0), key(4.0, 0.25)];
    sound_a.pan_keyframes = vec![key(1.0, -0.5), key(6.0, 0.5)];
    sound_b.gain_keyframes = vec![key(2.0, 0.0), key(5.0, -6.0)];
    (sound_b.volume, sound_b.pan, sound_b.gain) = (0.8, 0.25, -3.0);
    primary.clip_markers = vec![serde_json::from_value(json!({ "time_secs": 1.25, "name": "Take good", "color": "Green", "comment": "the wave" })).unwrap()];

    let mut title = caption("A title", 1.0, 5.0);
    title.transform_link = Some(serde_json::from_value(json!({ "target": primary.id, "mode": "Additive", "inherit_scale": false, "inherit_position": true, "inherit_rotation": true, "inherit_anchor": false })).unwrap());

    // A matte source below and the clip it keys above; and a stencil.
    let mut matte = Clip::media(cam_a, t(10.0), t(14.0), t(0.0), t(4.0));
    matte.consumed_as_matte = true;
    let mut keyed = Clip::media(cam_b, t(10.0), t(14.0), t(0.0), t(4.0));
    keyed.matte_source = Some(serde_json::from_value(json!({ "source_clip_id": matte.id, "channel": "Luminance", "invert": true, "feather_px": 3.0, "choke_px": 1.5 })).unwrap());
    let mut stencil = caption("Stencil", 10.0, 14.0);
    stencil.matte_stencil = Some(serde_json::from_value(json!({ "channel": "Alpha", "invert": false, "feather_px": 2.0, "choke_px": 0.0 })).unwrap());

    let mut music_clip = Clip::media(music, t(0.0), t(14.0), t(0.0), t(14.0));
    music_clip.role = klipp_core::ClipRole::Music;

    let track = |name: &str, kind, clips: Vec<Clip>| {
        let mut tr = Track::new(name, kind);
        tr.clips = clips;
        tr
    };
    let mut text = track("T1", TrackKind::Text, vec![title, stencil]);
    (text.locked, text.colour_label) = (true, 3);
    let mut v2 = track("V2", TrackKind::Video, vec![angle, keyed]);
    (v2.visible, v2.custom_height, v2.sync_lock) = (false, Some(72.0), false);
    let v1 = track("V1", TrackKind::Video, vec![primary, matte]);
    let mut a1 = track("A1", TrackKind::Audio, vec![sound_a]);
    (a1.volume, a1.pan, a1.solo) = (0.9, -0.25, true);
    let mut a2 = track("A2", TrackKind::Audio, vec![sound_b]);
    a2.muted = true;
    let a3 = track("Music", TrackKind::Audio, vec![music_clip]);
    let mut subs = track("Subtitle 1", TrackKind::Subtitle, vec![caption("Hello there.", 0.5, 2.0), caption("And welcome\nto the show.", 2.2, 4.8)]);
    subs.language = Some("en".into());
    subs.visible = false;
    subs.caption_style = Some(klipp_timeline::CaptionStyle { size: 52.0, color: [255, 220, 0, 255], y: 0.8, shadow: true, ..Default::default() });

    let mut seq = Sequence::new_default();
    seq.name = "Rich".into();
    seq.tracks = vec![text, v2, v1, a1, a2, a3, subs];
    (seq.in_point, seq.out_point, seq.last_playhead) = (Some(t(1.0)), Some(t(12.5)), Some(t(6.25)));
    seq.markers = vec![serde_json::from_value(json!({ "id": klipp_core::MarkerId::new(), "position": t(3.0), "label": "Cut to B", "color": "Blue", "duration": t(1.5), "comment": "a note", "kind": "None" })).unwrap()];
    seq
}

/// A second, smaller sequence: a project holds more than one.
fn second() -> Sequence {
    let mut seq = Sequence::new_default();
    seq.name = "Short".into();
    seq.tracks[0].clips = vec![Clip::media(MediaId::new(), t(0.0), t(3.0), t(5.0), t(8.0))];
    seq
}

/// Each sequence as the save holds it: its whole saved form.
fn whole(seqs: &[Sequence]) -> Vec<serde_json::Value> {
    seqs.iter().map(|s| serde_json::to_value(s).unwrap()).collect()
}

#[test]
fn a_rich_project_comes_back_whole_and_saves_again_the_same() {
    let made = vec![rich(), second()];
    let first = scratch("first");
    save_project(&first, &made, &MediaPool::new()).unwrap();
    let loaded = load_project_strict(&first).unwrap();
    assert_eq!(loaded.0.len(), 2, "both sequences");
    for (made, back) in whole(&made).iter().zip(whole(&loaded.0)) {
        assert_eq!(&back, made, "the sequence comes back as it was saved, every field");
    }
    for (made, back) in made.iter().zip(&loaded.0) {
        for (tm, tb) in made.tracks.iter().zip(&back.tracks) {
            for (cm, cb) in tm.clips.iter().zip(&tb.clips) {
                assert_eq!(cb.picture_v2().unwrap().picture, cm.picture_v2().unwrap().picture, "clip {} on {}: its picture", cm.id.0, tm.name);
            }
        }
    }
    // The multicam group plays as it did: the same angle shown and heard.
    let (m, b) = (&made[0], &loaded.0[0]);
    for at in [1.0, 4.0, 8.0] {
        assert_eq!(b.multicam_at(None, t(at)).map(|g| g.active), m.multicam_at(None, t(at)).map(|g| g.active), "the angle shown at {at} s");
        for track in 3..5 {
            let (cm, cb) = (&m.tracks[track].clips[0], &b.tracks[track].clips[0]);
            assert_eq!(b.multicam_sounds_at(cb, t(at)), m.multicam_sounds_at(cm, t(at)), "{} heard at {at} s", m.tracks[track].name);
        }
    }

    let second = scratch("second");
    save_project(&second, &loaded.0, &loaded.1).unwrap();
    assert_eq!(project_text(&second), project_text(&first), "saved again byte for byte");
    let _ = std::fs::remove_dir_all(&first);
    let _ = std::fs::remove_dir_all(&second);
}

/// A clip whose blur is mixed at a half: only its picture can hold that,
/// so it is a picture-truth clip.
fn truth_clip(from: f64) -> Clip {
    use klipp_graph_format::path::{NodePath, Root};
    let mut c = Clip::media(MediaId::new(), t(from), t(from + 2.0), t(0.0), t(2.0));
    c.push_effect(klipp_effects::BuiltinEffect::GaussianBlur { radius: 4.0 });
    let blur = c.picture_v2().unwrap().picture.graph.nodes.iter().find(|n| n.type_id.as_str() == klipp_graph_format::types::LEGACY_EFFECT).unwrap().id;
    c.apply_picture_ops(&[klipp_graph_lift::ops::GraphOp::SetMix { path: NodePath::new(Root::Clip { clip: c.id.0 }, [blur]), mix: Some(0.5) }]).unwrap();
    assert!(c.is_picture_truth());
    c
}

/// Pinned (#2461): a clip's own serde writes its fields, the view, not its
/// picture; a picture-truth clip through serde comes back an ordinary clip
/// without what only its picture held. A project save keeps it, through
/// the clip's stored picture. Anything that moves clips through serde
/// (rather than clones or a project save) loses a picture-truth clip's
/// picture, and this test says so if that ever changes.
#[test]
fn a_picture_truth_clip_through_its_own_serde_is_its_view_and_a_save_keeps_it() {
    let c = truth_clip(0.0);
    let through: Clip = serde_json::from_value(serde_json::to_value(&c).unwrap()).unwrap();
    assert!(!through.is_picture_truth(), "serde writes the view, not the picture");
    assert_ne!(through.picture_v2().unwrap().picture, c.picture_v2().unwrap().picture, "the mix is not in the view");
    let mut seq = Sequence::new_default();
    seq.tracks[0].clips = vec![c.clone()];
    let dir = scratch("serde");
    save_project(&dir, std::slice::from_ref(&seq), &MediaPool::new()).unwrap();
    let (back, ..) = load_project_strict(&dir).unwrap();
    assert!(back[0].tracks[0].clips[0].is_picture_truth(), "a save keeps it");
    assert_eq!(back[0].tracks[0].clips[0].picture_v2().unwrap().picture, c.picture_v2().unwrap().picture);
    let _ = std::fs::remove_dir_all(&dir);
}
