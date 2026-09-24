//! A precomposition's sound: where it goes, what it keeps, and that the
//! viewer's timeline and the export snapshot take it from the same place.
//!
//! A surface cannot carry sound, so flattening puts a scene's audio on
//! runtime tracks of the containing sequence — one per scene track, the
//! scene track's and the containing track's controls composed into it —
//! and the export snapshot reads those tracks and nothing else for it.

use klipp_core::{ClipId, ExportSettings, Timecode, WorkingColorspace};
use klipp_media::{MediaItem, MediaPool};
use klipp_render::snapshot::SequenceSnapshot;
use klipp_timeline::flatten::flatten_sequence;
use klipp_timeline::{Clip, Sequence, Track, TrackKind};

fn tc(s: f64) -> Timecode {
    Timecode::from_secs_f64(s, 24)
}

/// A scene: one audio clip of a synthetic file, four seconds long.
fn scene(pool: &mut MediaPool) -> Sequence {
    let mut item = MediaItem::synthetic_for_test("/tmp/precomposition-sound.wav".into(), None);
    item.has_audio = true;
    item.duration = tc(4.0);
    let id = pool.add(item);
    let mut s = Sequence::new_default();
    s.name = "Scene".into();
    let mut t = Track::new("sound", TrackKind::Audio);
    t.append_clip(id, tc(4.0));
    s.tracks = vec![t];
    s
}

/// A sequence with `scene` laid in once, over its whole length.
fn laid_in(scene: &Sequence) -> Sequence {
    let mut o = Sequence::new_default();
    o.tracks[0].clips = vec![Clip::precomposition(scene.id, tc(0.0), tc(4.0))];
    o
}

fn sound_tracks(flat: &Sequence) -> Vec<&Track> {
    flat.tracks.iter().filter(|t| t.precomposition_sound).collect()
}

fn snapshot(seq: &Sequence, all: &[Sequence], pool: &MediaPool) -> SequenceSnapshot {
    SequenceSnapshot::from_sequence_with_all(seq, all, pool, &ExportSettings::default(), None, false, WorkingColorspace::Rec709Linear).unwrap()
}

/// Two instances of a middle scene, each holding the same leaf, are two
/// leaves: named under the instance they were reached through, and
/// sounding at their own source time when the instances are offset.
#[test]
fn a_leaf_reached_through_two_instances_is_two_leaves() {
    let mut pool = MediaPool::new();
    let leaf = scene(&mut pool);
    let middle = laid_in(&leaf);
    let mut root = laid_in(&middle);
    let mut second = Track::new("second", TrackKind::Video);
    let mut later = Clip::precomposition(middle.id, tc(0.0), tc(3.0));
    later.source_in = tc(1.0);
    later.source_out = tc(4.0);
    second.clips = vec![later];
    root.tracks.push(second);

    let flat = flatten_sequence(&root, &[root.clone(), middle.clone(), leaf.clone()]);
    let leaf_ids: Vec<ClipId> = flat
        .tracks
        .iter()
        .filter_map(|t| t.clips.first()?.precomp.as_ref())
        .map(|middle_instance| {
            let leaf_instance = middle_instance.tracks[0].clips[0].precomp.as_ref().expect("leaf resolves");
            leaf_instance.tracks[0].clips[0].id
        })
        .collect();
    assert_eq!(leaf_ids.len(), 2, "two instances of the middle scene");
    assert_ne!(leaf_ids[0], leaf_ids[1], "the leaf's clip is named under the instance it was reached through");
    assert_ne!(leaf_ids[0], leaf.tracks[0].clips[0].id, "and never by the scene's own name");

    // The sound came up two levels, at those names, offset by each root
    // instance's window.
    let sound = sound_tracks(&flat);
    assert_eq!(sound.len(), 2, "one runtime track per instance: {}", sound.len());
    let mut starts: Vec<(ClipId, f64)> = sound.iter().map(|t| (t.clips[0].id, t.clips[0].source_in.to_secs_f64())).collect();
    starts.sort_by(|a, b| a.1.total_cmp(&b.1));
    assert_eq!(starts[0].1, 0.0);
    assert_eq!(starts[1].1, 1.0, "the later instance starts a second into the scene");
    assert!(leaf_ids.contains(&starts[0].0) && leaf_ids.contains(&starts[1].0), "the sound carries the instances' ids");
}

/// A precomposition that is switched off is off: no scene, no sound.
#[test]
fn a_disabled_precomposition_makes_no_sound() {
    let mut pool = MediaPool::new();
    let s = scene(&mut pool);
    let mut o = laid_in(&s);
    o.tracks[0].clips[0].enabled = false;
    let flat = flatten_sequence(&o, &[o.clone(), s.clone()]);
    assert!(flat.tracks[0].clips[0].precomp.is_none(), "no scene for a clip that is off");
    assert!(sound_tracks(&flat).is_empty(), "and no sound");
    assert!(snapshot(&o, &[o.clone(), s], &pool).audio_tracks.iter().all(|t| t.clips.is_empty()), "nor in the export");
}

/// The scene track's controls and the containing track's are composed
/// into the runtime track: muted if either is, volumes multiplied, pans
/// added, solo settled inside the scene and the containing track's solo
/// carried.
#[test]
fn a_scenes_sound_keeps_its_controls_and_takes_its_carriers() {
    let mut pool = MediaPool::new();
    let mut s = scene(&mut pool);
    s.tracks[0].volume = 0.5;
    s.tracks[0].pan = -0.25;
    // A second scene track, soloed: the first falls silent inside the scene.
    let mut soloed = Track::new("lead", TrackKind::Audio);
    soloed.append_clip(s.tracks[0].clips[0].media_id().unwrap(), tc(4.0));
    soloed.solo = true;
    s.tracks.push(soloed);

    let mut o = laid_in(&s);
    o.tracks[0].volume = 0.5;
    o.tracks[0].pan = -0.5;
    o.tracks[0].solo = true;
    let flat = flatten_sequence(&o, &[o.clone(), s.clone()]);
    let sound = sound_tracks(&flat);
    assert_eq!(sound.len(), 2);
    let (first, lead) = (sound[0], sound[1]);
    assert!(first.muted, "a sibling of a soloed scene track is silent");
    assert!(!lead.muted, "the soloed one sounds");
    assert!(first.solo && lead.solo, "the carrier's solo is carried, and the scene's own solo is not a solo out here");
    assert!((first.volume - 0.25).abs() < 1e-6, "volumes multiply: 0.5 × 0.5");
    assert!((first.pan + 0.75).abs() < 1e-6, "pans add: −0.25 + −0.5");

    // And a muted scene track stays silent, whatever the carrier says.
    let mut muted = scene(&mut pool);
    muted.tracks[0].muted = true;
    let o = laid_in(&muted);
    let flat = flatten_sequence(&o, &[o.clone(), muted.clone()]);
    assert!(sound_tracks(&flat).iter().all(|t| t.muted), "a muted scene track is a muted runtime track");
    let snap = snapshot(&o, &[o.clone(), muted], &pool);
    assert!(snap.audio_tracks.iter().filter(|t| !t.clips.is_empty()).all(|t| t.muted), "and muted in the export");
}

/// The export takes a precomposition's sound from the runtime tracks the
/// viewer's timeline has — wherever the precomposition sits — and from
/// nowhere else, so the two carry the same clips at the same places.
#[test]
fn the_export_hears_what_the_viewer_hears_once() {
    let mut pool = MediaPool::new();
    let s = scene(&mut pool);
    // On a video track, and on an audio track: the same sound each way.
    for kind in [TrackKind::Video, TrackKind::Audio] {
        let mut o = Sequence::new_default();
        let mut t = Track::new("carrier", kind);
        t.clips = vec![Clip::precomposition(s.id, tc(1.0), tc(4.0))];
        o.tracks = vec![t];
        let flat = flatten_sequence(&o, &[o.clone(), s.clone()]);
        let viewer: Vec<(u64, f64, f32, bool)> = sound_tracks(&flat)
            .iter()
            .flat_map(|t| t.clips.iter().map(move |c| ((c.timeline_in.to_secs_f64() * 24.0).round() as u64, c.source_in.to_secs_f64(), t.volume, t.muted)))
            .collect();
        let snap = snapshot(&o, &[o.clone(), s.clone()], &pool);
        let export: Vec<(u64, f64, f32, bool)> = snap
            .audio_tracks
            .iter()
            .flat_map(|t| t.clips.iter().map(move |c| (c.timeline_start_frame, c.source_start_secs, t.volume, t.muted)))
            .collect();
        assert_eq!(viewer.len(), 1, "the viewer hears the scene once on a {kind:?} track: {viewer:?}");
        assert_eq!(export, viewer, "and the export hears exactly that");
        assert_eq!(export[0].0, 24, "a second in, where the precomposition starts");
    }
}

/// The whole way: a real file, through the scene, the runtime track,
/// the export snapshot and the mixdown. The file's samples are its own
/// source time (`v = t/4 − 1`, as `export_audio_lands_where_the_timeline_says`
/// uses), so a frame of the mix says which second of the scene it is.
/// Needs the ffmpeg CLI to write the file; skips without it.
#[test]
fn a_precompositions_sound_is_mixed_down_from_where_the_scene_puts_it() {
    let rate = 48000;
    let ramp = {
        let ok = std::process::Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success());
        if !ok { return }
        let out = klipp_testing::scratch::scratch_file("klipp-precomp-audio", "ramp.flac");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-y", "-v", "error", "-f", "lavfi", "-i"])
            .arg(format!("aevalsrc=t/4-1|t/4-1:s={rate}:d=8"))
            .args(["-c:a", "flac", "-sample_fmt", "s16"])
            .arg(&out)
            .status()
            .is_ok_and(|s| s.success());
        if !ok { return }
        out
    };
    let mut pool = MediaPool::new();
    let mut item = MediaItem::synthetic_for_test(ramp, None);
    item.has_audio = true;
    item.duration = tc(8.0);
    let id = pool.add(item);
    let mut s = Sequence::new_default();
    let mut t = Track::new("sound", TrackKind::Audio);
    t.append_clip(id, tc(8.0));
    s.tracks = vec![t];

    // Laid in two seconds along, starting two seconds into the scene.
    let mut o = Sequence::new_default();
    let mut pre = Clip::precomposition(s.id, tc(2.0), tc(6.0));
    pre.source_in = tc(2.0);
    pre.source_out = tc(6.0);
    o.tracks[0].clips = vec![pre];
    let snap = snapshot(&o, &[o.clone(), s], &pool);
    let mut mix = klipp_render::AudioMixdown::new(&snap).unwrap();
    let secs_of = |v: f32| (v as f64 + 1.0) * 4.0;
    assert!(mix.get_frame_audio(47).iter().all(|&v| v == 0.0), "silence before the scene");
    // The mixdown ramps a clip's first two milliseconds up from silence,
    // so the start is read a little past that and moved back.
    let f48 = mix.get_frame_audio(48);
    let start = secs_of(f48[200 * 2]) - 200.0 / rate as f64;
    assert!((start - 2.0).abs() < 0.0005, "frame 48 starts at scene second {start:.4}, not 2.0");
    let f72 = mix.get_frame_audio(72);
    assert!((secs_of(f72[0]) - 3.0).abs() < 0.0005, "frame 72 is scene second {:.4}, not 3.0", secs_of(f72[0]));
}

/// A linked pair's sound settings live on the picture, and the picture
/// stays inside the scene where the mixer cannot see it. The sound clip
/// that reaches the mix carries them instead — level, pan, fades and
/// automation — through one instance and through a scene inside a
/// scene, exactly as the scene played them on its own.
#[test]
fn a_linked_pictures_sound_settings_come_out_with_its_sound() {
    let mut pool = MediaPool::new();
    let mut item = MediaItem::synthetic_for_test("/tmp/precomposition-linked.wav".into(), None);
    item.has_audio = true;
    item.duration = tc(4.0);
    let id = pool.add(item);
    let mut scene = Sequence::new_default();
    scene.name = "Scene".into();
    let mut v = Track::new("picture", TrackKind::Video);
    v.append_clip(id, tc(4.0));
    let mut a = Track::new("sound", TrackKind::Audio);
    a.append_clip(id, tc(4.0));
    v.clips[0].linked_clip_id = Some(a.clips[0].id);
    a.clips[0].linked_clip_id = Some(v.clips[0].id);
    v.clips[0].volume = 0.25;
    v.clips[0].pan = -0.5;
    v.clips[0].fade_in_secs = 0.3;
    klipp_core::keyframe::upsert_keyframe(&mut v.clips[0].volume_keyframes, 1.0, 0.25);
    klipp_core::keyframe::upsert_keyframe(&mut v.clips[0].volume_keyframes, 3.0, 1.0);
    scene.tracks = vec![v, a];

    let shapes = |snap: &SequenceSnapshot| -> Vec<(f32, f32, f32, Vec<(f64, f32)>)> {
        snap.audio_tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .map(|c| (c.shape.volume, c.shape.pan, c.shape.fade_in_secs, c.shape.volume_keyframes.iter().map(|k| (k.time_secs, k.value)).collect()))
            .collect()
    };
    let direct = shapes(&SequenceSnapshot::from_sequence(&scene, &pool, &ExportSettings::default(), None, false, WorkingColorspace::Rec709Linear).unwrap());
    assert_eq!(direct.len(), 1, "the pair sounds once on its own");
    assert_eq!(direct[0].0, 0.25, "with the picture's level");

    // Laid in whole, the same settings reach the mix.
    let whole = shapes(&snapshot(&laid_in(&scene), &[laid_in(&scene), scene.clone()], &pool));
    assert_eq!(whole, direct, "the precomposition plays the pair as the scene did");

    // Laid in from a second into the scene, the automation moves with
    // the start: the key at scene second 1 is now at clip second 0.
    let mut o = Sequence::new_default();
    let mut pre = Clip::precomposition(scene.id, tc(0.0), tc(3.0));
    pre.source_in = tc(1.0);
    pre.source_out = tc(4.0);
    o.tracks[0].clips = vec![pre];
    let trimmed = shapes(&snapshot(&o, &[o.clone(), scene.clone()], &pool));
    assert_eq!(trimmed[0].3, vec![(0.0, 0.25), (2.0, 1.0)], "automation keeps its place in the scene: {:?}", trimmed[0].3);

    // And through a scene inside a scene.
    let middle = laid_in(&scene);
    let root = laid_in(&middle);
    let nested = shapes(&snapshot(&root, &[root.clone(), middle.clone(), scene.clone()], &pool));
    assert_eq!(nested, direct, "two levels down, still the picture's settings");
}
