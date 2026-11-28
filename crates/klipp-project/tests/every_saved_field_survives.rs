//! Every field of a sequence, a track, a transition, a marker and a media
//! item comes back from a save and a load, as a clip's does
//! (`every_clip_field_survives.rs`).
//!
//! The project file spells each of them out field by field, by hand, so a
//! field added and forgotten there compiled and was quietly dropped on
//! reopen. Here each is written out whole, with no `..`: a new field does
//! not compile until it is given a value here, one away from a plain one's,
//! and then this test says whether a save keeps it.

use klipp_core::{MediaId, RationalTime};
use klipp_graph_format::path::{NodePath, Root};
use klipp_media::{MediaItem, MediaPool};
use klipp_project::{load_project_strict, save_project};
use klipp_timeline::{Clip, Marker, Sequence, Track, TrackKind, Transition};
use serde_json::{json, Value};

fn t(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

/// A value of any saved type, from its serde form.
fn de<T: serde::de::DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).unwrap()
}

/// The value written out whole, and the names of its fields as written.
macro_rules! whole {
    ($ty:ident { $($field:ident : $value:expr),* $(,)? }) => {
        ($ty { $($field: $value),* }, [$(stringify!($field)),*])
    };
}

/// Each field `fields` names, other than those in `skip`, is away from
/// `plain`'s value: a save that dropped it, giving the default back, is seen.
fn away_from_plain<T: serde::Serialize>(what: &str, value: &T, plain: &T, fields: &[&str], skip: &[&str]) {
    let (form, plain) = (serde_json::to_value(value).unwrap(), serde_json::to_value(plain).unwrap());
    for name in fields.iter().filter(|f| !skip.contains(f)) {
        let field = form.get(*name).unwrap_or_else(|| panic!("{what}'s `{name}` is left at its default in this test: give it a value a save has to keep"));
        assert_ne!(plain.get(*name), Some(field), "{what}'s `{name}` is left at its default in this test: give it a value a save has to keep");
    }
}

/// The fields of `before` that `after` lost or changed.
fn lost<T: serde::Serialize>(what: &str, before: &T, after: &T, fields: &[&str], skip: &[&str]) -> Vec<String> {
    let (before, after) = (serde_json::to_value(before).unwrap(), serde_json::to_value(after).unwrap());
    fields
        .iter()
        .filter(|f| !skip.contains(f))
        .filter(|f| before.get(**f) != after.get(**f))
        .map(|f| format!("{what}.{f}: saved {} came back {}", before.get(*f).unwrap_or(&Value::Null), after.get(*f).unwrap_or(&Value::Null)))
        .collect()
}

#[test]
fn every_field_of_a_sequence_track_transition_marker_and_media_item_survives_a_save_and_a_load() {
    let plain_seq = Sequence::new_default();
    let plain_track = Track::new("V1", TrackKind::Video);
    let media = MediaId::new();
    let a = Clip::media(media, t(0.0), t(4.0), t(0.0), t(4.0));
    let b = Clip::media(media, t(4.0), t(8.0), t(4.0), t(8.0));
    let (a_id, b_id) = (a.id, b.id);

    let plain_transition = Transition {
        id: klipp_core::TransitionId::new(),
        kind: klipp_core::TransitionKind::CrossDissolve,
        duration: t(0.5),
        clip_a_id: a_id,
        clip_b_id: b_id,
        user_params: klipp_core::TransitionUserParams::default(),
        alignment: klipp_core::TransitionAlignment::default(),
    };
    let (transition, transition_fields) = whole!(Transition {
        id: plain_transition.id,
        kind: klipp_core::TransitionKind::DipToBlack,
        duration: t(1.0),
        clip_a_id: a_id,
        clip_b_id: b_id,
        user_params: klipp_core::TransitionUserParams { param0: 0.37, ..Default::default() },
        alignment: klipp_core::TransitionAlignment::StartAtCut,
    });
    away_from_plain("the transition", &transition, &plain_transition, &transition_fields, &["id", "clip_a_id", "clip_b_id"]);

    let (track, track_fields) = whole!(Track {
        id: plain_track.id,
        name: "Interview".into(),
        kind: TrackKind::Video,
        clips: vec![a, b],
        transitions: vec![transition],
        opacity: 0.5,
        volume: 0.75,
        muted: true,
        solo: true,
        visible: false,
        pan: 0.25,
        locked: true,
        frozen: true,
        lock_while_frozen: true,
        sync_lock: !plain_track.sync_lock,
        targeted: !plain_track.targeted,
        source_patch: !plain_track.source_patch,
        caption_style: Some(de(json!({ "font": "Inter", "size": 40.0, "color": [255, 255, 0, 255], "bold": true, "outline": 2.0, "outline_color": [0, 0, 0, 255], "backing": 0.5, "shadow": true, "y": 0.9 }))),
        language: Some("sv".into()),
        role: Some(de(json!("dialogue"))),
        magnetic_role: klipp_timeline::TrackRole::Primary,
        storyline_anchor: None,
        custom_height: Some(120.0),
        colour_label: 4,
        channel_waveforms: true,
        pre_collapse_height: None,
        precomposition_sound: false,
        source_file: None,
        record_armed: false,
        automation_lane_visible: Some(klipp_core::AudioParam::Pan),
        automation_lane_track: true,
        automation_lane_height: Some(80.0),
        effects: klipp_timeline::TrackEffects { audio: vec![de(json!({ "effect_type": "eq", "enabled": true, "wet_dry": 0.5, "params": [["gain", 3.0]] }))], ..Default::default() },
        automation: de(json!({ "mode": "Off", "lanes": { "Volume": [{ "time_secs": 1.0, "value": 0.5, "interpolation": "Linear" }] } })),
        sends: Vec::new(),
    });
    // Not saved, on purpose, or not on a video track: the session's own,
    // or a sound track's, which the sound track below carries.
    let track_skip = [
        "id", "kind", "clips", "transitions", // compared one by one
        "pre_collapse_height", // the height to go back to after a collapse, for the session
        "precomposition_sound", // made by a precomposition's flattening, never saved
        "source_file",          // a track made from a file for the session (#3207)
        "record_armed",         // arming for a recording is the session's, not the project's
        "storyline_anchor",     // a connected storyline's, below
        "sends",                // a sound track's, below
    ];
    away_from_plain("the track", &track, &plain_track, &track_fields, &track_skip);

    let mut music = Track::new("Music", TrackKind::Audio);
    let bus = klipp_timeline::AuxBus::new("Verb");
    music.sends = vec![klipp_timeline::AuxSend { level: 0.5, pre_fader: true, ..klipp_timeline::AuxSend::new(bus.id) }];
    let mut storyline = Track::new("Storyline", TrackKind::Video);
    storyline.magnetic_role = klipp_timeline::TrackRole::ConnectedStoryline;
    storyline.storyline_anchor = Some(de(json!({ "anchor_clip_id": a_id, "offset": t(1.0), "alignment": "StartAligned" })));

    let (marker, marker_fields) = whole!(Marker {
        id: klipp_core::MarkerId::new(),
        position: t(2.0),
        label: "Chapter one".into(),
        color: klipp_core::MarkerColor::Purple,
        duration: Some(t(1.5)),
        comment: "here".into(),
        kind: klipp_core::MarkerKind::Chapter,
        review: Some(klipp_timeline::ReviewNote { author: "Ana".into(), status: klipp_core::ReviewStatus::WontDo, round: 2 }),
    });
    let plain_marker = Marker { id: marker.id, position: t(2.0), label: String::new(), color: klipp_core::MarkerColor::Red, duration: None, comment: String::new(), kind: klipp_core::MarkerKind::None, review: None };
    away_from_plain("the marker", &marker, &plain_marker, &marker_fields, &["id", "position"]);

    let mut timeline = klipp_nodes::NodeGraph::default();
    let tone = timeline.ensure_corrector();
    let seq_id = plain_seq.id;
    let (seq, seq_fields) = whole!(Sequence {
        id: seq_id,
        name: "Cut two".into(),
        fps: klipp_core::Fps { num: 25, den: 1 },
        resolution: klipp_core::Resolution { width: 3840, height: 2160 },
        first_clip_autoset: klipp_timeline::FirstClipAutoSet { resolution: false, fps: false, rate_answered: true },
        tracks: vec![track.clone(), storyline.clone(), music.clone()],
        in_point: Some(t(1.0)),
        out_point: Some(t(7.0)),
        markers: vec![marker.clone()],
        last_playhead: Some(t(3.0)),
        brief: Some(klipp_timeline::Brief { title: "Launch".into(), target_secs: Some(90.0), ..Default::default() }),
        edit_plan: Some(klipp_timeline::EditPlan { approved: true, notes: "second pass".into(), ..Default::default() }),
        reviews: vec![de(json!({ "finding_id": "gap-1", "decision": "accepted", "rationale": "on purpose", "revision": 3 }))],
        covers: vec![klipp_timeline::cover::Cover { id: klipp_core::CoverId::new(), name: "Hook".into(), at_secs: 1.5, size: "youtube".into(), crop: None, text: None, with_titles: true }],
        awaiting_refit: true,
        version_of: Some(klipp_timeline::VersionOf { master: klipp_core::SequenceId::new(), cut: 7 }),
        camera: de(json!({ "zoom_keyframes": [{ "time_secs": 0.0, "value": 1.5, "interpolation": "Linear" }], "shutter_deg": 180.0 })),
        timeline_graph: Some(de(serde_json::to_value(&timeline).unwrap())),
        grade_groups: vec![de(json!({ "id": uuid::Uuid::new_v4(), "name": "Day" }))],
        drop_frame: true,
        title_nits: 300.0,
        node_labels: vec![klipp_timeline::NodeLabel { node: NodePath::new(Root::TimelineGrade { sequence: seq_id.0 }, [tone]), colour: 6 }],
        buses: vec![bus],
    });
    away_from_plain("the sequence", &seq, &plain_seq, &seq_fields, &["id", "tracks", "fps"]);

    let plain_item = MediaItem::synthetic_for_test("/media/a.mov".into(), None);
    let (item, item_fields) = whole!(MediaItem {
        id: media,
        path: "/media/a.mov".into(),
        filename: "a.mov".into(),
        duration: t(60.0),
        fps: klipp_core::Fps { num: 25, den: 1 },
        resolution: klipp_core::Resolution { width: 3840, height: 2160 },
        video_codec: "prores".into(),
        audio_codec: "pcm_s24le".into(),
        has_audio: !plain_item.has_audio,
        file_size_bytes: 123_456,
        kind: klipp_media::MediaItemKind::File,
        sample_rate: Some(96_000),
        audio_channels: Some(6),
        offline: false,
        is_still: false,
        proxy_path: Some("/media/proxies/a.mov".into()),
        proxy_ready: true,
        default_source_in: Some(t(2.0)),
        default_source_out: Some(t(9.0)),
        rating: 4,
        tags: vec!["good".into()],
        user_notes: "the long take".into(),
        bin: Some("Day 1".into()),
        bin_id: Some(klipp_core::BinId::new()),
        parent_media_id: Some(MediaId::new()),
        added_at: 1_700_000_000,
        fingerprint: Some(42),
        cache_key: None,
        file_modified_at: Some(1_700_000_001),
        media_created_at: Some(1_600_000_000),
        color_label: klipp_media::ColorLabel::Orange,
        colour_space: klipp_media::ColourSpace::Hdr10,
        log_format: klipp_core::LogFormat::SLog3,
        log_format_user_set: true,
        source_rotation: 90,
        camera_make: Some("Sony".into()),
        camera_model: Some("FX3".into()),
        encoder: Some("Lavf".into()),
        bit_depth: Some(10),
        video_bitrate_kbps: Some(100_000),
        audio_bitrate_kbps: Some(2_304),
        color_primaries: Some("bt2020".into()),
        color_transfer: Some("smpte2084".into()),
        pixel_format: Some("yuv422p10le".into()),
        detected_log_format: klipp_core::LogFormat::default(),
        shot: klipp_core::ShotMetadata { reel_name: Some("A001".into()), iso: Some(800), ..Default::default() },
        metadata_read: true,
        truncated_from_secs: Some(58.5),
        stream_ends: klipp_core::StreamEnds { picture: Some(59.9), sound: Some(60.0) },
        dolby_vision_profile: Some(8),
        log: klipp_core::ClipLog { scene: "12".into(), take: "3".into(), good_take: true, ..Default::default() },
        synced_audio: Some(de(json!({ "audio_media_id": MediaId::new(), "offset_secs": 0.25, "method": "Waveform" }))),
        markers: vec![de(json!({ "secs": 3.0, "duration_secs": 0.0, "name": "nice", "kind": "Favourite" }))],
        screen_text: "SALE".into(),
        audio_role: Some(de(json!({ "role": "Music", "confidence": 0.9, "confident": true }))),
        span: Some(klipp_core::ClipSpan { first: MediaId::new(), index: 1, count: 3 }),
        skim_file: None,
        needs_reprobe: false,
        waveform_data: None,
        audio_cache: None,
    });
    // Worked out again on load, from the file and the cache, or the
    // session's own.
    let item_derived = [
        "id", "path", "filename", "kind",
        "offline",             // whether the file is there now
        "is_still",            // read from the file
        "cache_key",           // made from the path on save
        "detected_log_format", // detected again from the file's metadata
        "skim_file",           // found again in the cache
        "proxy_ready",         // true only while the proxy file is there
        "metadata_read",       // not saved; nothing reads it since the probe on open went
        "needs_reprobe", "waveform_data", "audio_cache", // the session's
    ];
    away_from_plain("the media item", &item, &plain_item, &item_fields, &item_derived);

    let mut pool = MediaPool::new();
    pool.add(item.clone());
    let dir = klipp_testing::scratch::fresh_dir("klipp_every_saved_field_survives");
    save_project(&dir, std::slice::from_ref(&seq), &pool).unwrap();
    let (back, back_pool, ..) = load_project_strict(&dir).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let back_seq = back.iter().find(|s| s.id == seq_id).expect("the sequence came back");
    let back_item = back_pool.get(&media).expect("the media item came back");

    let mut lost_fields = lost("sequence", &seq, back_seq, &seq_fields, &["tracks"]);
    for (before, after) in seq.tracks.iter().zip(&back_seq.tracks) {
        lost_fields.extend(lost(&format!("track {}", before.name), before, after, &track_fields, &["clips", "transitions", "pre_collapse_height", "precomposition_sound", "source_file", "record_armed"]));
    }
    assert_eq!(back_seq.tracks.len(), seq.tracks.len(), "every track came back");
    lost_fields.extend(lost("transition", &transition, &back_seq.tracks[0].transitions[0], &transition_fields, &[]));
    lost_fields.extend(lost("marker", &marker, &back_seq.markers[0], &marker_fields, &[]));
    lost_fields.extend(lost("media item", &item, back_item, &item_fields, &item_derived[4..]));
    assert!(lost_fields.is_empty(), "fields a save and a load lost:\n{}", lost_fields.join("\n"));
}
