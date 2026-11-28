//! Every field of a clip comes back from a save and a load.
//!
//! The project file spells a clip out field by field, by hand, so a field
//! added to `Clip` and forgotten there compiled and was quietly dropped on
//! reopen. Here the clip is written out whole, with no `..`: a new field
//! does not compile until it is given a value here, one away from a plain
//! clip's, and then this test says whether a save keeps it.

use klipp_core::{GroupId, Keyframe, MediaId, MulticamSound, RationalTime};
use klipp_media::MediaPool;
use klipp_project::{load_project_strict, save_project};
use klipp_timeline::{Clip, Sequence, Track, TrackKind};
use serde_json::{json, Value};

fn t(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

/// A value of any saved type, from its serde form.
fn de<T: serde::de::DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).unwrap()
}

fn key(time_secs: f64, value: f32) -> Keyframe {
    de(json!({ "time_secs": time_secs, "value": value, "interpolation": "Linear" }))
}

/// The clip written out whole, and the names of its fields as written.
macro_rules! whole_clip {
    ($($field:ident : $value:expr),* $(,)?) => {
        (Clip { $($field: $value),* }, [$(stringify!($field)),*])
    };
}

/// Not saved, on purpose: the session's caches, made again after a load.
const NOT_SAVED: [&str; 2] = [
    "precomp",   // a compound's flattened scene, rebuilt from its sequence
    "baked_own", // the clip's own effects baked for the session
];

/// Saved, but with no value of their own to compare with a plain clip's:
/// the clip's identity and place, the same on both, and the picture, whose
/// fields the save takes apart whole (`ClipPictureV1`), so the compiler
/// asks for a new one there.
const SAME_AS_PLAIN: [&str; 6] = ["kind", "timeline_in", "timeline_out", "source_in", "source_out", "picture"];

#[test]
fn every_field_of_a_clip_survives_a_save_and_a_load() {
    let media = MediaId::new();
    let plain = Clip::media(media, t(2.0), t(6.0), t(1.0), t(5.0));
    // What the clip points at, on the sequence too, so nothing it names is
    // missing when it is read back.
    let other = Clip::media(MediaId::new(), t(0.0), t(8.0), t(0.0), t(8.0));
    let take = Clip::media(MediaId::new(), t(2.0), t(6.0), t(0.0), t(4.0));
    let take_id = klipp_core::TakeId::new();
    let surface = uuid::Uuid::new_v4();
    let group = GroupId::new();

    let (clip, fields) = whole_clip! {
        id: klipp_core::ClipId::new(),
        kind: plain.kind.clone(),
        timeline_in: plain.timeline_in,
        timeline_out: plain.timeline_out,
        source_in: plain.source_in,
        source_out: plain.source_out,
        picture: plain.picture.clone(),
        effects_switched_off: vec![uuid::Uuid::new_v4()],
        linked_clip_id: Some(other.id),
        linked_title_asset_id: Some(MediaId::new()),
        group_id: Some(GroupId::new()),
        grade_group: Some(de(json!(uuid::Uuid::new_v4()))),
        magnetic_anchor: Some(de(json!({ "anchor_clip_id": other.id, "offset": t(0.5), "alignment": "EndAligned" }))),
        gain: 1.5,
        volume: 0.5,
        pan: -0.25,
        fade_in_secs: 0.5,
        fade_out_secs: 0.75,
        fade_in_curve: klipp_timeline::FadeCurve::SCurve,
        fade_out_curve: klipp_timeline::FadeCurve::Exponential,
        channel_config: klipp_timeline::ChannelConfig::MonoDownmix,
        volume_keyframes: vec![key(0.0, 0.5), key(1.0, 1.0)],
        pan_keyframes: vec![key(0.5, -0.5)],
        gain_keyframes: vec![key(0.25, 1.25)],
        matte_feather_keyframes: vec![key(0.0, 4.0)],
        matte_choke_keyframes: vec![key(0.0, 2.0)],
        clip_markers: vec![de(json!({ "time_secs": 1.5, "name": "beat", "color": "Green", "comment": "here" }))],
        frame_glide: Some(de(json!({ "start": { "x": 0.1, "y": 0.2, "width": 0.5 }, "end": { "x": 0.3, "y": 0.1, "width": 0.8 }, "easing": "EaseIn" }))),
        role: Some(de(json!("dialogue"))),
        role_source: klipp_core::RoleSource::ByHand,
        matched_to: Some(other.id),
        speed: 0.5,
        speed_keyframes: vec![key(0.0, 1.0), key(2.0, 0.5)],
        keep_pitch: true,
        sound_apart: true,
        composition: Some(klipp_timeline::CompositionId(uuid::Uuid::new_v4())),
        composition_versions: vec![de(json!({ "name": "B", "id": uuid::Uuid::new_v4() }))],
        hold: Some(t(1.5)),
        source_rotation: 90,
        enabled: false,
        solo: true,
        blend_mode: klipp_core::BlendMode::Screen,
        depth: 2.0,
        matte_source: Some(de(json!({ "source_clip_id": other.id, "channel": "Alpha", "invert": true, "feather_px": 3.0, "choke_px": 1.0 }))),
        matte_stencil: Some(de(json!({ "channel": "Red", "invert": true, "feather_px": 2.0, "choke_px": 0.5 }))),
        consumed_as_matte: true,
        transform_link: Some(de(json!({ "target": other.id, "mode": "Additive", "inherit_scale": false, "inherit_position": true, "inherit_rotation": false, "inherit_anchor": true }))),
        multicam_group: Some(group),
        multicam_angle: 2,
        multicam_cuts: vec![(1.0, 1), (3.0, 2)],
        multicam_sound: MulticamSound::Angle(1),
        audio_effects: vec![de(json!({ "effect_type": "eq", "enabled": false, "wet_dry": 0.5, "params": [["gain", 3.0]] }))],
        audio_effect_keyframes: [("eq.gain".to_string(), vec![key(0.0, 3.0)])].into(),
        track_points: vec![de(json!({ "id": uuid::Uuid::new_v4(), "name": "eye", "color": [255, 0, 0, 255], "positions": [[0.5, 0.5]], "start_frame": 3, "enabled": true, "search_radius": 24 }))],
        surfaces: vec![de(json!({ "id": surface, "name": "wall", "corners": [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]], "reference_secs": 1.0 }))],
        surface_pin: Some(de(json!({ "host": other.id, "surface": uuid::Uuid::new_v4(), "inset": 0.1 }))),
        name: Some("the shot".into()),
        colour_label: 3,
        grade_flag: klipp_core::GradeFlag::Review,
        // The active take is the one in the slot, the clip itself; another
        // holds its own clip.
        audition: Some(Box::new(de(json!({ "takes": [{ "id": take_id, "name": "Take 1" }, { "id": klipp_core::TakeId::new(), "name": "Take 2", "clip": take }], "active": take_id })))),
        precomp: None,
        baked_own: None,
    };
    let id = clip.id;

    // Every field the test sets is away from a plain clip's, so a save that
    // dropped it, giving the default back, is seen.
    let mut plain_form = serde_json::to_value(&plain).unwrap();
    plain_form.as_object_mut().unwrap().remove("id");
    let form = serde_json::to_value(&clip).unwrap();
    for name in fields.iter().filter(|f| !NOT_SAVED.contains(f) && !SAME_AS_PLAIN.contains(f) && **f != "id") {
        let value = form.get(*name).unwrap_or_else(|| panic!("`{name}` is left at its default in this test: give it a value a save has to keep"));
        assert_ne!(plain_form.get(*name), Some(value), "`{name}` is left at its default in this test: give it a value a save has to keep");
    }

    let mut seq = Sequence::new_default();
    seq.tracks[0].clips = vec![clip];
    let mut below = Track::new("V2", TrackKind::Video);
    below.clips = vec![other];
    seq.tracks.push(below);
    let dir = klipp_testing::scratch::fresh_dir("klipp_every_clip_field_survives");
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let (back, ..) = load_project_strict(&dir).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let back = back[0].all_clips().find(|c| c.id == id).expect("the clip came back");
    let back_form = serde_json::to_value(back).unwrap();

    let lost: Vec<String> = fields
        .iter()
        .filter(|f| !NOT_SAVED.contains(f))
        .filter(|f| form.get(**f) != back_form.get(**f))
        .map(|f| format!("{f}: saved {} came back {}", form.get(*f).unwrap_or(&Value::Null), back_form.get(*f).unwrap_or(&Value::Null)))
        .collect();
    assert!(lost.is_empty(), "fields a save and a load lost:\n{}", lost.join("\n"));
}

fn round_trip(label: &str, clip: Clip) -> Clip {
    let id = clip.id;
    let mut seq = Sequence::new_default();
    seq.tracks[0].clips = vec![clip];
    let dir = klipp_testing::scratch::fresh_dir(&format!("klipp_every_clip_field_{label}"));
    save_project(&dir, &[seq], &MediaPool::new()).unwrap();
    let (back, ..) = load_project_strict(&dir).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let clip = back[0].all_clips().find(|c| c.id == id).expect("the clip came back").clone();
    clip
}

/// The All effects switch turned a clip's effects off and remembered which,
/// by their picture nodes: reopened, it still reads off, and the nodes it
/// names are the clip's, so switching it on brings those back. It read on
/// after a reopen, with the effects still off.
#[test]
fn the_effects_the_all_effects_switch_turned_off_are_remembered_after_a_reopen() {
    let mut clip = Clip::media(MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
    let nodes = |c: &Clip| c.picture_v2().unwrap().picture.graph.nodes.iter().map(|n| n.id).collect::<Vec<_>>();
    let before = nodes(&clip);
    clip.effects_switched_off = before.clone();

    let back = round_trip("switched_off", clip);
    assert_eq!(nodes(&back), before, "the picture's nodes keep their ids");
    assert_eq!(back.effects_switched_off, before, "the switch remembers the same nodes");
}

/// A file naming more switched-off effects than any clip has is read with
/// the first thousand and twenty-four.
#[test]
fn a_file_naming_endless_switched_off_effects_is_read_with_a_bounded_list() {
    let mut clip = Clip::media(MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
    clip.effects_switched_off = (0..5000).map(|_| uuid::Uuid::new_v4()).collect();
    let first = clip.effects_switched_off[..1024].to_vec();
    assert_eq!(round_trip("endless_switched_off", clip).effects_switched_off, first);
}
