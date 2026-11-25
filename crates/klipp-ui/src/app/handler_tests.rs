//! The app's handlers driven as the app drives them: an app built by
//! [`KlippApp::for_test`], commands through `send_commands`, and what the
//! state became and what went to the engine read back. The first of the
//! handler tests that had to copy the dispatch by hand before.

use super::*;

/// A file's rating set from the media pool is one project step: the pool
/// holds it, ctrl+z names it and takes it back, redo puts it again, and a
/// rating it already has is no step at all.
#[test]
fn a_rating_is_one_step_undo_takes_back() {
    let (mut app, engine) = KlippApp::for_test();
    let id = app.state.document.media_pool.add(klipp_media::MediaItem::synthetic_for_test("/media/a.mov".into(), None));
    let rating = |app: &KlippApp| app.state.document.media_pool.get(&id).unwrap().rating;

    app.send_commands(vec![AppCommand::SetMediaRating { media_id: id, rating: 4 }]);
    assert_eq!(rating(&app), 4);
    assert!(app.state.dirty, "the project is unsaved");
    assert_eq!(app.state.undo_depth(), 1);

    app.send_commands(vec![AppCommand::SetMediaRating { media_id: id, rating: 4 }]);
    assert_eq!(app.state.undo_depth(), 1, "the same rating again is no step");

    app.send_commands(vec![AppCommand::Undo]);
    assert_eq!(rating(&app), 0);
    app.send_commands(vec![AppCommand::Redo]);
    assert_eq!(rating(&app), 4);
    // The pool's record: nothing of it goes to the engine.
    assert!(!engine.commands.try_iter().any(|c| matches!(c, AppCommand::SetMediaRating { .. })));
}

/// Files renamed together are one step, and a bin renamed takes its files'
/// name for it along; undo puts both back.
#[test]
fn renames_are_steps_undo_takes_back() {
    let (mut app, _engine) = KlippApp::for_test();
    let mut item = klipp_media::MediaItem::synthetic_for_test("/media/a.mov".into(), None);
    item.filename = "a.mov".into();
    item.bin = Some("Rushes".into());
    let id = app.state.document.media_pool.add(item);
    app.state.document.media_pool.bins.push(klipp_media::Bin { id: klipp_core::BinId::new(), name: "Rushes".into(), parent_id: None });

    app.send_commands(vec![AppCommand::BatchRenameMediaItems { changes: vec![(id, "Interview.mov".into())] }]);
    app.send_commands(vec![AppCommand::RenameBin { old_name: "Rushes".into(), new_name: "Selects".into() }]);
    let item = app.state.document.media_pool.get(&id).unwrap();
    assert_eq!((item.filename.as_str(), item.bin.as_deref(), app.state.document.media_pool.bins[0].name.as_str()), ("Interview.mov", Some("Selects"), "Selects"));
    assert_eq!(app.state.undo_depth(), 2);

    app.send_commands(vec![AppCommand::Undo, AppCommand::Undo]);
    let item = app.state.document.media_pool.get(&id).unwrap();
    assert_eq!((item.filename.as_str(), item.bin.as_deref(), app.state.document.media_pool.bins[0].name.as_str()), ("a.mov", Some("Rushes"), "Rushes"));
}

/// A key on a clip's matte feather or choke is a step undo takes back,
/// as a key on its opacity is. The keys' snapshot held every other track
/// but these two, so undoing one put nothing back.
#[test]
fn a_matte_key_is_a_step_undo_takes_back() {
    let (mut app, _engine) = KlippApp::for_test();
    let at = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    let clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), at(0.0), at(4.0), at(0.0), at(4.0));
    let id = clip.id;
    let v = app.state.sequence().tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
    let active = app.state.active_sequence_idx;
    app.state.document.sequences[active].tracks[v].clips.push(clip);
    let keys = |app: &KlippApp| {
        let c = app.state.sequence().all_clips().find(|c| c.id == id).unwrap();
        (c.matte_feather_keyframes.len(), c.matte_choke_keyframes.len(), c.opacity_keyframes().len())
    };
    for param in [klipp_core::VideoParam::MatteFeather, klipp_core::VideoParam::MatteChoke, klipp_core::VideoParam::Opacity] {
        app.send_commands(vec![AppCommand::AddVideoKeyframe { clip_id: id, param, time_secs: 1.0, value: 0.5 }]);
    }
    assert_eq!(keys(&app), (1, 1, 1));
    app.send_commands(vec![AppCommand::Undo]);
    assert_eq!(keys(&app), (1, 1, 0), "the opacity key back");
    app.send_commands(vec![AppCommand::Undo]);
    assert_eq!(keys(&app), (1, 0, 0), "the choke key back");
    app.send_commands(vec![AppCommand::Undo]);
    assert_eq!(keys(&app), (0, 0, 0), "the feather key back");
    app.send_commands(vec![AppCommand::Redo, AppCommand::Redo]);
    assert_eq!(keys(&app), (1, 1, 0));
}

/// A marker added, moved and taken off are a step each, undo walks them
/// back, and each reaches the shared sequence the control socket reads.
#[test]
fn markers_are_steps_and_reach_the_shared_sequence() {
    let (mut app, _engine) = KlippApp::for_test();
    let at = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    let markers = |app: &KlippApp| app.state.sequence().markers.iter().map(|m| (m.label.clone(), m.position)).collect::<Vec<_>>();
    let shared = |app: &KlippApp| app.sequence.snapshot().markers.len();

    app.send_commands(vec![AppCommand::AddMarker { position: at(2.0), label: "B".into(), color: klipp_core::MarkerColor::Red }]);
    app.send_commands(vec![AppCommand::AddMarker { position: at(1.0), label: "A".into(), color: klipp_core::MarkerColor::Red }]);
    assert_eq!(markers(&app), [("A".to_string(), at(1.0)), ("B".to_string(), at(2.0))], "in time order");
    assert_eq!(shared(&app), 2, "the shared sequence has them");
    let b = app.state.sequence().markers[1].id;
    app.send_commands(vec![AppCommand::MoveMarker { marker_id: b, position: at(3.0) }]);
    app.send_commands(vec![AppCommand::RemoveMarker { marker_id: b }]);
    assert_eq!((markers(&app).len(), app.state.undo_depth()), (1, 4), "four steps");
    app.send_commands(vec![AppCommand::UpdateMarkerLabel { marker_id: klipp_core::MarkerId::new(), label: "none".into() }]);
    assert_eq!(app.state.undo_depth(), 4, "a change to no marker is no step");

    app.send_commands(vec![AppCommand::Undo]);
    assert_eq!(markers(&app)[1], ("B".to_string(), at(3.0)), "back, where it was moved to");
    app.send_commands(vec![AppCommand::Undo, AppCommand::Undo]);
    assert_eq!(markers(&app), [("B".to_string(), at(2.0))]);
    assert_eq!(shared(&app), 1, "and the shared sequence with it");
}

/// Review comments imported are one step: one undo takes them all back.
/// The import recorded its step and the dispatcher recorded the change
/// again, so it took two.
#[test]
fn review_comments_imported_are_one_step() {
    let (mut app, _engine) = KlippApp::for_test();
    let path = std::env::temp_dir().join(format!("klipp-review-{}.txt", std::process::id()));
    std::fs::write(&path, "00:00:01 Too dark\n00:00:02 Cut sooner\n").unwrap();
    app.send_commands(vec![AppCommand::ImportReviewComments { path: path.clone() }]);
    let _ = std::fs::remove_file(&path);
    assert_eq!((app.state.sequence().markers.len(), app.state.undo_depth()), (2, 1));
    app.send_commands(vec![AppCommand::Undo]);
    assert!(app.state.sequence().markers.is_empty());
    assert_eq!(app.state.undo_depth(), 0);
}

/// A clip and its linked partner as the app holds them: a picture on the
/// first video track, its sound on the first audio track.
fn linked_pair(app: &mut KlippApp) -> (klipp_core::ClipId, klipp_core::ClipId) {
    let at = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    let media = klipp_core::MediaId::new();
    let (mut picture, mut sound) = (klipp_timeline::Clip::media(media, at(0.0), at(4.0), at(0.0), at(4.0)), klipp_timeline::Clip::media(media, at(0.0), at(4.0), at(0.0), at(4.0)));
    (picture.linked_clip_id, sound.linked_clip_id) = (Some(sound.id), Some(picture.id));
    let ids = (picture.id, sound.id);
    let active = app.state.active_sequence_idx;
    let tracks = &mut app.state.document.sequences[active].tracks;
    tracks.iter_mut().find(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap().clips.push(picture);
    tracks.iter_mut().find(|t| t.kind == klipp_timeline::TrackKind::Audio).unwrap().clips.push(sound);
    ids
}

/// An audio key added and removed on a picture goes to its linked sound
/// too, each one step that undo takes back from both, and the engine is
/// told each time.
#[test]
fn an_audio_key_is_one_step_for_the_pair() {
    let (mut app, engine) = KlippApp::for_test();
    let (picture, sound) = linked_pair(&mut app);
    let keys = |app: &KlippApp| {
        let n = |id| app.state.sequence().all_clips().find(|c| c.id == id).unwrap().volume_keyframes.len();
        (n(picture), n(sound))
    };
    let param = klipp_core::AudioParam::Volume;
    app.send_commands(vec![AppCommand::AddAudioKeyframe { clip_id: picture, param, time_secs: 1.0, value: 0.5 }]);
    app.send_commands(vec![AppCommand::AddAudioKeyframe { clip_id: picture, param, time_secs: 2.0, value: 0.8 }]);
    assert_eq!((keys(&app), app.state.undo_depth()), ((2, 2), 2));
    assert!(engine.commands.try_iter().any(|c| matches!(c, AppCommand::SequenceUpdated)), "the engine told");
    app.send_commands(vec![AppCommand::RemoveAudioKeyframe { clip_id: picture, param, time_secs: 1.0 }]);
    assert_eq!(keys(&app), (1, 1));
    app.send_commands(vec![AppCommand::RemoveAudioKeyframe { clip_id: picture, param, time_secs: 3.0 }]);
    assert_eq!(app.state.undo_depth(), 3, "taking off a key that isn't there is no step");

    app.send_commands(vec![AppCommand::Undo]);
    assert_eq!(keys(&app), (2, 2), "both back");
    app.send_commands(vec![AppCommand::Undo, AppCommand::Undo]);
    assert_eq!(keys(&app), (0, 0));
}

/// Audio keys scaled in time, a bump laid in and an interpolation set are
/// a step each, on the clip and its partner, and undo takes each back.
#[test]
fn audio_key_shapes_are_one_step_each_for_the_pair() {
    let (mut app, _engine) = KlippApp::for_test();
    let (picture, sound) = linked_pair(&mut app);
    let param = klipp_core::AudioParam::Volume;
    let keys = |app: &KlippApp, id| app.state.sequence().all_clips().find(|c| c.id == id).unwrap().volume_keyframes.clone();
    app.send_commands(vec![AppCommand::AddAudioKeyframe { clip_id: picture, param, time_secs: 1.0, value: 0.5 }]);
    app.send_commands(vec![AppCommand::ScaleAudioKeyframeTiming { clip_id: picture, param, factor: 2.0 }]);
    assert_eq!((keys(&app, picture)[0].time_secs, keys(&app, sound)[0].time_secs), (2.0, 2.0), "both scaled");
    app.send_commands(vec![AppCommand::InsertAudioKeyframeBump { clip_id: picture, param, center_time: 3.0, bump_value: 1.0, base_value: 0.5, duration_secs: 0.5, ramp_secs: 0.25 }]);
    assert_eq!((keys(&app, picture).len(), keys(&app, sound).len()), (5, 5), "a bump of four on each");
    app.send_commands(vec![AppCommand::SetAudioKeyframeInterpolation { clip_id: picture, param, keyframe_index: 0, interpolation: klipp_core::Interpolation::Bezier }]);
    assert_eq!(keys(&app, sound)[0].interpolation, klipp_core::Interpolation::Bezier);
    assert_eq!((app.state.undo_depth(), app.state.undo_label().as_deref()), (4, Some("Set Keyframe Interpolation")));

    app.send_commands(vec![AppCommand::Undo, AppCommand::Undo]);
    assert_eq!((keys(&app, picture).len(), keys(&app, sound)[0].interpolation), (1, klipp_core::Interpolation::Linear));
    app.send_commands(vec![AppCommand::Undo]);
    assert_eq!((keys(&app, picture)[0].time_secs, keys(&app, sound)[0].time_secs), (1.0, 1.0), "the scaling back");
}

/// An audio effect's key added and taken off are a step each; while its
/// slider is dragged, the keys the drag lays are one step, the drag's.
#[test]
fn an_audio_effect_key_is_a_step_and_a_drag_is_one() {
    let (mut app, _engine) = KlippApp::for_test();
    let (picture, _sound) = linked_pair(&mut app);
    let keys = |app: &KlippApp| app.state.sequence().all_clips().find(|c| c.id == picture).unwrap().audio_effect_keyframes.get("0:gain").map_or(0, Vec::len);
    let add = |t: f64| AppCommand::AddAudioEffectKeyframe { clip_id: picture, effect_index: 0, param_name: "gain".into(), time_secs: t, value: 0.5 };
    app.send_commands(vec![add(1.0)]);
    app.send_commands(vec![AppCommand::RemoveAudioEffectKeyframe { clip_id: picture, effect_index: 0, param_name: "gain".into(), time_secs: 1.0 }]);
    assert_eq!((keys(&app), app.state.undo_depth()), (0, 2));

    let drag_key = format!("audio_chain:{}:0:gain", picture.0);
    app.send_commands(vec![AppCommand::BeginAudioChainDrag { drag_key: drag_key.clone(), clip_id: picture }]);
    app.send_commands(vec![add(1.0), add(2.0), add(3.0)]);
    app.send_commands(vec![AppCommand::EndAudioChainDrag { drag_key, clip_id: picture }]);
    assert_eq!((keys(&app), app.state.undo_depth()), (3, 3), "the drag's three keys one step");
    app.send_commands(vec![AppCommand::Undo]);
    assert_eq!(keys(&app), 0, "the whole drag back");
}

/// A generator's setting changed is one step that counts toward autosave,
/// as every other edit does: it went to the history past the app's
/// recording, so an autosave never knew of it. Undo takes it back, and
/// the frame is shown again each way.
#[test]
fn a_generator_edit_is_a_step_autosave_counts() {
    let (mut app, engine) = KlippApp::for_test();
    let (id, _) = linked_pair(&mut app);
    let solid = |c: u8| klipp_core::GeneratorData::SolidColor { color: [c, 0, 0, 255] };
    app.state.document.sequences[app.state.active_sequence_idx].all_clips_mut().find(|c| c.id == id).unwrap().kind = klipp_timeline::ClipKind::Generator(solid(10));
    let data = |app: &KlippApp| app.state.sequence().all_clips().find(|c| c.id == id).unwrap().generator_data().cloned();
    let ops = app.state.history_ops_since_save;

    app.send_commands(vec![AppCommand::UpdateGeneratorData { clip_id: id, data: solid(200) }]);
    assert_eq!(data(&app), Some(solid(200)));
    assert_eq!((app.state.undo_depth(), app.state.history_ops_since_save), (1, ops + 1));
    assert!(app.state.dirty);
    assert!(engine.commands.try_iter().any(|c| matches!(c, AppCommand::SetPlayhead(_))), "the frame is shown again");
    app.send_commands(vec![AppCommand::UpdateGeneratorData { clip_id: id, data: solid(200) }]);
    assert_eq!(app.state.undo_depth(), 1, "the same setting again is no step");

    app.send_commands(vec![AppCommand::Undo]);
    assert_eq!(data(&app), Some(solid(10)));
}

/// A clip renamed names its linked partner with it, one step undo takes
/// back from both.
#[test]
fn a_rename_is_one_step_for_the_pair() {
    let (mut app, _engine) = KlippApp::for_test();
    let (picture, sound) = linked_pair(&mut app);
    let names = |app: &KlippApp| [picture, sound].map(|id| app.state.sequence().all_clips().find(|c| c.id == id).unwrap().own_label().name);

    app.send_commands(vec![AppCommand::RenameClips { clip_ids: vec![picture], name: Some(" Interview ".into()) }]);
    assert_eq!(names(&app), [Some("Interview".into()), Some("Interview".into())]);
    assert_eq!(app.state.undo_depth(), 1);
    app.send_commands(vec![AppCommand::RenameClips { clip_ids: vec![picture], name: Some("Interview".into()) }]);
    assert_eq!(app.state.undo_depth(), 1, "the same name again is no step");

    app.send_commands(vec![AppCommand::Undo]);
    assert_eq!(names(&app), [None, None]);
}

/// A clip's file replaced by a shorter one is one step: the clip shortens
/// to fit, and undo puts back the file and the length.
#[test]
fn a_clip_media_replaced_is_one_step() {
    let (mut app, _engine) = KlippApp::for_test();
    let at = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    let mut short = klipp_media::MediaItem::synthetic_for_test("/media/short.mov".into(), None);
    short.duration = at(2.0);
    let short = app.state.document.media_pool.add(short);
    let (id, _) = linked_pair(&mut app);
    let clip = |app: &KlippApp| app.state.sequence().all_clips().find(|c| c.id == id).map(|c| (c.media_id(), c.source_out, c.timeline_out)).unwrap();
    let before = clip(&app);

    app.send_commands(vec![AppCommand::ReplaceClipMedia { clip_id: id, new_media_id: short }]);
    assert_eq!(clip(&app), (Some(short), at(2.0), at(2.0)));
    assert_eq!(app.state.undo_depth(), 1);
    assert!(app.state.dirty);

    app.send_commands(vec![AppCommand::Undo]);
    assert_eq!(clip(&app), before);
    app.send_commands(vec![AppCommand::Redo]);
    assert_eq!(clip(&app), (Some(short), at(2.0), at(2.0)));
}

/// A generator's colour dragged sends a change each frame; the run is one
/// step, which undo takes back whole. A change after a pause, or after an
/// undo, is a step of its own.
#[test]
fn a_generator_colour_dragged_is_one_step() {
    let (mut app, _engine) = KlippApp::for_test();
    let (id, _) = linked_pair(&mut app);
    let solid = |c: u8| klipp_core::GeneratorData::SolidColor { color: [c, 0, 0, 255] };
    app.state.document.sequences[app.state.active_sequence_idx].all_clips_mut().find(|c| c.id == id).unwrap().kind = klipp_timeline::ClipKind::Generator(solid(10));
    let data = |app: &KlippApp| app.state.sequence().all_clips().find(|c| c.id == id).unwrap().generator_data().cloned();

    app.send_commands((20..30).map(|c| AppCommand::UpdateGeneratorData { clip_id: id, data: solid(c) }).collect());
    assert_eq!(data(&app), Some(solid(29)));
    assert_eq!(app.state.undo_depth(), 1, "one step for the drag");

    // A pause: the next change is its own step.
    app.state.generator_edit = app.state.generator_edit.map(|(c, at)| (c, at.checked_sub(std::time::Duration::from_secs(2)).unwrap_or(at)));
    app.send_commands(vec![AppCommand::UpdateGeneratorData { clip_id: id, data: solid(40) }]);
    assert_eq!(app.state.undo_depth(), 2);

    app.send_commands(vec![AppCommand::Undo]);
    assert_eq!(data(&app), Some(solid(29)));
    // After an undo, a change is a new step, not folded into the drag's.
    app.send_commands(vec![AppCommand::UpdateGeneratorData { clip_id: id, data: solid(50) }]);
    assert_eq!(app.state.undo_depth(), 2);
    app.send_commands(vec![AppCommand::Undo]);
    assert_eq!(data(&app), Some(solid(29)));
    app.send_commands(vec![AppCommand::Undo]);
    assert_eq!(data(&app), Some(solid(10)), "one undo takes back the whole drag");
}

/// A clip duplicated lands right after itself, over the clip that follows,
/// which it trims. Undo puts that clip back whole and redo trims it again:
/// the step held only the copy, so undo left the next clip cut short and
/// redo laid the copy over it.
#[test]
fn a_duplicate_over_the_next_clip_undoes_whole() {
    let (mut app, _engine) = KlippApp::for_test();
    let at = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    let media = klipp_core::MediaId::new();
    let (a, b) = (klipp_timeline::Clip::media(media, at(0.0), at(2.0), at(0.0), at(2.0)), klipp_timeline::Clip::media(media, at(2.0), at(6.0), at(0.0), at(4.0)));
    let (a_id, b_id) = (a.id, b.id);
    let active = app.state.active_sequence_idx;
    let track = app.state.document.sequences[active].tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
    app.state.document.sequences[active].tracks[track].clips.extend([a, b]);
    let spans = |app: &KlippApp| app.state.sequence().tracks[track].clips.iter().map(|c| (c.id, c.timeline_in, c.timeline_out)).collect::<Vec<_>>();
    let before = spans(&app);

    app.send_commands(vec![AppCommand::DuplicateClip { clip_id: a_id }]);
    let after = spans(&app);
    let copy = app.state.selected_clip.expect("the copy is selected");
    assert_eq!(after, vec![(a_id, at(0.0), at(2.0)), (copy, at(2.0), at(4.0)), (b_id, at(4.0), at(6.0))]);
    assert_eq!(app.state.undo_depth(), 1);

    app.send_commands(vec![AppCommand::Undo]);
    assert_eq!(spans(&app), before, "the next clip comes back whole");
    app.send_commands(vec![AppCommand::Redo]);
    assert_eq!(spans(&app), after, "redo trims it again");
}

/// Two shots side by side swapped, their sounds with them: each keeps its
/// length, one step undo puts back and redo makes again as it was made.
#[test]
fn a_swap_moves_the_pairs_and_redoes_as_made() {
    let (mut app, _engine) = KlippApp::for_test();
    let at = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    let media = klipp_core::MediaId::new();
    let pair = |from: f64, to: f64| {
        let (mut p, mut s) = (klipp_timeline::Clip::media(media, at(from), at(to), at(0.0), at(to - from)), klipp_timeline::Clip::media(media, at(from), at(to), at(0.0), at(to - from)));
        (p.linked_clip_id, s.linked_clip_id) = (Some(s.id), Some(p.id));
        (p, s)
    };
    let ((a, a_sound), (b, b_sound)) = (pair(0.0, 2.0), pair(2.0, 5.0));
    let (a_id, b_id) = (a.id, b.id);
    let active = app.state.active_sequence_idx;
    let tracks = &mut app.state.document.sequences[active].tracks;
    tracks.iter_mut().find(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap().clips.extend([a, b]);
    tracks.iter_mut().find(|t| t.kind == klipp_timeline::TrackKind::Audio).unwrap().clips.extend([a_sound, b_sound]);
    let spans = |app: &KlippApp| app.state.sequence().all_clips().map(|c| (c.id, (c.timeline_in, c.timeline_out))).collect::<std::collections::HashMap<_, _>>();
    let before = spans(&app);

    app.send_commands(vec![AppCommand::SwapAdjacentClips { clip_a: a_id, clip_b: b_id }]);
    let after = spans(&app);
    assert_eq!((after[&b_id], after[&a_id]), ((at(0.0), at(3.0)), (at(3.0), at(5.0))), "each keeps its length");
    let sound = |id| app.state.sequence().all_clips().find(|c| c.id == id).unwrap().linked_clip_id.unwrap();
    assert_eq!(after[&sound(a_id)], after[&a_id], "the sound goes with its picture");
    assert_eq!(app.state.undo_depth(), 1);

    app.send_commands(vec![AppCommand::Undo]);
    assert_eq!(spans(&app), before);
    app.send_commands(vec![AppCommand::Redo]);
    assert_eq!(spans(&app), after, "redo makes it as it was made");
}
