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

/// Every clip on every track, as the timeline shows it: where it sits and
/// what it is linked to, track by track.
fn layout(app: &KlippApp) -> Vec<Vec<(klipp_core::ClipId, klipp_core::RationalTime, klipp_core::RationalTime, Option<klipp_core::ClipId>)>> {
    app.state.sequence().tracks.iter().map(|t| t.clips.iter().map(|c| (c.id, c.timeline_in, c.timeline_out, c.linked_clip_id)).collect()).collect()
}

/// A source inserted on free tracks at the playhead, its sound under it:
/// the shot it lands in is split on both tracks and the rest moves right.
/// One step; undo puts the timeline back and redo makes it as the insert
/// made it, the split halves and the sound's link included.
#[test]
fn an_insert_edit_redoes_as_made() {
    let (mut app, _engine) = KlippApp::for_test();
    app.state.magnetic_timeline_enabled = false;
    let at = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    let (shot, _) = linked_pair(&mut app);
    let source = app.state.document.media_pool.add(klipp_media::MediaItem::synthetic_for_test("/media/source.mov".into(), None));
    app.state.source_media_id = Some(source);
    app.state.playhead = at(2.0);
    app.suppress_fps_prompt = true;
    let before = layout(&app);

    app.send_commands(vec![AppCommand::InsertEdit]);
    let after = layout(&app);
    assert_ne!(after, before, "the source went in");
    assert!(app.state.sequence().all_clips().any(|c| c.media_id() == Some(source) && c.linked_clip_id.is_some()), "with its sound linked");
    assert!(app.state.sequence().all_clips().all(|c| c.id != shot || c.timeline_out == at(2.0)), "the shot it landed in is split there");
    assert_eq!(app.state.undo_depth(), 1);

    app.send_commands(vec![AppCommand::Undo]);
    assert_eq!(layout(&app), before);
    app.send_commands(vec![AppCommand::Redo]);
    assert_eq!(layout(&app), after, "redo makes it as the insert made it");
}

/// The range marked to a clip is one step, undo clears it again.
#[test]
fn a_range_to_a_clip_is_one_step() {
    let (mut app, _engine) = KlippApp::for_test();
    let at = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    let (shot, _) = linked_pair(&mut app);
    app.state.selected_clip = Some(shot);
    let range = |app: &KlippApp| (app.state.sequence().in_point, app.state.sequence().out_point);
    let before = range(&app);

    app.send_commands(vec![AppCommand::SetRangeToClip]);
    assert_eq!(range(&app), (Some(at(0.0)), Some(at(4.0))));
    assert_eq!(app.state.undo_depth(), 1);
    assert!(app.state.dirty);
    app.send_commands(vec![AppCommand::Undo]);
    assert_eq!(range(&app), before);
}

/// A second of gap put in at the playhead on the primary storyline: one
/// step, undone whole, redone as made.
#[test]
fn a_gap_at_the_playhead_redoes_as_made() {
    let (mut app, _engine) = KlippApp::for_test();
    let at = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    let primary = app.state.sequence().primary_track_index().expect("a primary storyline");
    let active = app.state.active_sequence_idx;
    let media = klipp_core::MediaId::new();
    app.state.document.sequences[active].tracks[primary].clips.extend([
        klipp_timeline::Clip::media(media, at(0.0), at(2.0), at(0.0), at(2.0)),
        klipp_timeline::Clip::media(media, at(2.0), at(5.0), at(0.0), at(3.0)),
    ]);
    app.state.playhead = at(2.0);
    let before = layout(&app);

    app.send_commands(vec![AppCommand::InsertGapAtPlayhead]);
    let after = layout(&app);
    assert_eq!(after[primary].last().map(|c| c.2), Some(at(6.0)), "what follows moves a second on");
    assert_eq!(app.state.undo_depth(), 1);
    app.send_commands(vec![AppCommand::Undo]);
    assert_eq!(layout(&app), before);
    app.send_commands(vec![AppCommand::Redo]);
    assert_eq!(layout(&app), after);
}

/// A subclip made from a file's marked zone is one step: the pool holds
/// it, undo takes it out, redo puts it back.
#[test]
fn a_subclip_is_one_step() {
    let (mut app, _engine) = KlippApp::for_test();
    let at = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    let mut item = klipp_media::MediaItem::synthetic_for_test("/media/a.mov".into(), None);
    (item.default_source_in, item.default_source_out) = (Some(at(1.0)), Some(at(3.0)));
    let parent = app.state.document.media_pool.add(item);
    let subclips = |app: &KlippApp| app.state.document.media_pool.all().filter(|i| i.parent_media_id == Some(parent)).map(|i| (i.id, i.duration)).collect::<Vec<_>>();

    app.send_commands(vec![AppCommand::CreateSubclipFromZone { source_media_id: parent, name: None }]);
    let made = subclips(&app);
    assert_eq!(made.iter().map(|s| s.1).collect::<Vec<_>>(), [at(2.0)]);
    assert_eq!(app.state.undo_depth(), 1);
    assert!(app.state.dirty);
    app.send_commands(vec![AppCommand::Undo]);
    assert!(subclips(&app).is_empty());
    app.send_commands(vec![AppCommand::Redo]);
    assert_eq!(subclips(&app), made);
}

/// The timeline, every field, with the tracks' ids left out: a track an
/// edit made is made again on redo as a new one, the same in all else.
fn contents(app: &KlippApp) -> serde_json::Value {
    let mut v = serde_json::to_value(app.state.sequence()).unwrap();
    for track in v["tracks"].as_array_mut().into_iter().flatten() {
        track["id"] = serde_json::Value::Null;
    }
    v
}

/// `send` makes one step; undo gives the timeline back and redo the one
/// it made, field for field.
fn redoes_as_made(app: &mut KlippApp, send: Vec<AppCommand>) {
    redoes_as_done(app, |app| app.send_commands(send));
}

/// [`redoes_as_made`] for an edit `edit` makes some other way than by a
/// command: an event come back from a worker.
fn redoes_as_done(app: &mut KlippApp, edit: impl FnOnce(&mut KlippApp)) {
    app.suppress_fps_prompt = true;
    let before = contents(app);
    let depth = app.state.undo_depth();
    edit(app);
    let made = contents(app);
    assert_ne!(made, before, "the edit changed the timeline");
    assert_eq!(app.state.undo_depth(), depth + 1, "one step");
    assert!(app.state.dirty);
    app.send_commands(vec![AppCommand::Undo]);
    // Whole timelines are long: a mismatch names itself, not both.
    assert!(contents(app) == before, "undo gives the timeline back");
    app.send_commands(vec![AppCommand::Redo]);
    assert!(contents(app) == made, "redo makes it as the edit made it");
}

fn a_file(app: &mut KlippApp) -> klipp_core::MediaId {
    app.state.document.media_pool.add(klipp_media::MediaItem::synthetic_for_test("/media/shot.mov".into(), None))
}

/// A file appended to the primary storyline, its sound linked under it.
#[test]
fn an_append_to_the_storyline_redoes_as_made() {
    let (mut app, _engine) = KlippApp::for_test();
    linked_pair(&mut app);
    let media_id = a_file(&mut app);
    let track_index = app.state.sequence().primary_track_index().unwrap();
    redoes_as_made(&mut app, vec![AppCommand::AppendClipToTrack { media_id, track_index }]);
}

/// A file appended to a free video track, its sound linked under it, and
/// without its sound when the audio target is off.
#[test]
fn an_append_to_a_free_track_redoes_as_made() {
    for brings_audio in [true, false] {
        let (mut app, _engine) = KlippApp::for_test();
        app.state.append_brings_audio = brings_audio;
        linked_pair(&mut app);
        app.state.sequence_mut().add_video_track("Video 2".into());
        let track_index = app.state.sequence().tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video && t.magnetic_role != klipp_timeline::TrackRole::Primary).unwrap();
        let media_id = a_file(&mut app);
        redoes_as_made(&mut app, vec![AppCommand::AppendClipToTrack { media_id, track_index }]);
    }
}

/// A file put at the playhead on tracks of its own, linked.
#[test]
fn a_file_at_the_playhead_redoes_as_made() {
    let (mut app, _engine) = KlippApp::for_test();
    linked_pair(&mut app);
    app.state.playhead = klipp_core::RationalTime::from_secs_f64(1.0, 24);
    let media_id = a_file(&mut app);
    redoes_as_made(&mut app, vec![AppCommand::PlaceMedia { media_id, drop: klipp_core::events::ClipDrop::AtPlayhead }]);
}

/// The source laid on the primary storyline by Insert and by Overwrite,
/// its sound linked under it.
#[test]
fn a_source_edit_on_the_storyline_redoes_as_made() {
    for cmd in [AppCommand::InsertEdit, AppCommand::OverwriteEdit] {
        let (mut app, _engine) = KlippApp::for_test();
        linked_pair(&mut app);
        let source = a_file(&mut app);
        app.state.source_media_id = Some(source);
        app.state.playhead = klipp_core::RationalTime::from_secs_f64(4.0, 24);
        redoes_as_made(&mut app, vec![cmd]);
    }
}

/// The first file put on an empty timeline at the playhead: the sequence
/// takes its size and rate in the same step, and redo takes them again.
#[test]
fn a_first_file_at_the_playhead_redoes_its_format() {
    let (mut app, _engine) = KlippApp::for_test();
    let mut item = klipp_media::MediaItem::synthetic_for_test("/media/shot.mov".into(), None);
    (item.resolution, item.fps) = (klipp_core::Resolution { width: 3840, height: 2160 }, klipp_core::Fps::new(50, 1));
    let media_id = app.state.document.media_pool.add(item);
    redoes_as_made(&mut app, vec![AppCommand::PlaceMedia { media_id, drop: klipp_core::events::ClipDrop::AtPlayhead }]);
    assert_eq!(app.state.sequence().fps, klipp_core::Fps::new(50, 1), "the sequence took the file's rate");
    assert_eq!(app.state.fps, app.state.sequence().fps);
}

/// Appended and source-edited on the storyline with no audio track: the
/// audio track made for the sound is in the step, undone and redone with it.
#[test]
fn a_storyline_edit_that_makes_its_audio_track_redoes_as_made() {
    let sends = |media_id, track_index| vec![
        vec![AppCommand::AppendClipToTrack { media_id, track_index }],
        vec![AppCommand::InsertEdit],
        vec![AppCommand::OverwriteEdit],
    ];
    for i in 0..3 {
        let (mut app, _engine) = KlippApp::for_test();
        app.state.sequence_mut().tracks.retain(|t| t.kind != klipp_timeline::TrackKind::Audio);
        app.state.mark_saved();
        let media_id = a_file(&mut app);
        app.state.source_media_id = Some(media_id);
        let track_index = app.state.sequence().primary_track_index().unwrap();
        let tracks = app.state.sequence().tracks.len();
        redoes_as_made(&mut app, sends(media_id, track_index).swap_remove(i));
        assert_eq!(app.state.sequence().tracks.len(), tracks + 1, "an audio track made");
        assert!(app.state.sequence().tracks.iter().any(|t| t.kind == klipp_timeline::TrackKind::Audio && !t.clips.is_empty()), "the sound on it");
        app.send_commands(vec![AppCommand::Undo]);
        assert_eq!(app.state.sequence().tracks.len(), tracks, "undo takes the track away");
    }
}

/// A clip above the storyline connected to the primary clip nearest it,
/// disconnected again, and its track promoted to a storyline and broken
/// apart: each one step, redone as made.
#[test]
fn storyline_connections_redo_as_made() {
    let at = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    let (mut app, _engine) = KlippApp::for_test();
    linked_pair(&mut app);
    app.state.sequence_mut().add_video_track("Video 2".into());
    let above = app.state.sequence().tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video && t.magnetic_role != klipp_timeline::TrackRole::Primary).unwrap();
    let title = klipp_timeline::Clip::media(klipp_core::MediaId::new(), at(1.0), at(2.0), at(0.0), at(1.0));
    let clip_id = title.id;
    app.state.sequence_mut().tracks[above].clips.push(title);
    redoes_as_made(&mut app, vec![AppCommand::ConnectToPrimary { clip_id }]);
    redoes_as_made(&mut app, vec![AppCommand::DisconnectFromPrimary { clip_id }]);
    app.state.sequence_mut().tracks[above].magnetic_role = klipp_timeline::TrackRole::Connected;
    redoes_as_made(&mut app, vec![AppCommand::ToggleStoryline { clip_id }]);
    redoes_as_made(&mut app, vec![AppCommand::ToggleStoryline { clip_id }]);
}

/// Two linked pairs side by side on free tracks.
fn two_pairs(app: &mut KlippApp) -> [(klipp_core::ClipId, klipp_core::ClipId); 2] {
    let at = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    app.state.magnetic_timeline_enabled = false;
    for t in &mut app.state.sequence_mut().tracks {
        t.magnetic_role = klipp_timeline::TrackRole::Free;
    }
    let pair = |app: &mut KlippApp, from: f64, to: f64| {
        let media = klipp_core::MediaId::new();
        let (mut picture, mut sound) = (klipp_timeline::Clip::media(media, at(from), at(to), at(0.0), at(to - from)), klipp_timeline::Clip::media(media, at(from), at(to), at(0.0), at(to - from)));
        (picture.linked_clip_id, sound.linked_clip_id) = (Some(sound.id), Some(picture.id));
        let ids = (picture.id, sound.id);
        let tracks = &mut app.state.sequence_mut().tracks;
        tracks.iter_mut().find(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap().clips.push(picture);
        tracks.iter_mut().find(|t| t.kind == klipp_timeline::TrackKind::Audio).unwrap().clips.push(sound);
        ids
    };
    [pair(app, 0.0, 4.0), pair(app, 6.0, 10.0)]
}

/// A clip dragged along its track over the next pair, and onto another
/// track: laid over what is there, its sound with it and the covered
/// clip's sound cut the same way, one step redone as made.
#[test]
fn a_clip_dropped_over_a_pair_redoes_as_made() {
    let at = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    let (mut app, _engine) = KlippApp::for_test();
    let [(first, _), _] = two_pairs(&mut app);
    redoes_as_made(&mut app, vec![AppCommand::MoveClip { clip_id: first, new_timeline_in: at(7.0) }]);

    let (mut app, _engine) = KlippApp::for_test();
    let [(first, _), _] = two_pairs(&mut app);
    app.state.sequence_mut().add_video_track("Video 2".into());
    let tracks = &app.state.sequence().tracks;
    let from = tracks.iter().position(|t| t.clips.iter().any(|c| c.id == first)).unwrap();
    let to = tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video && t.clips.is_empty()).unwrap();
    redoes_as_made(&mut app, vec![AppCommand::MoveClipToTrack { clip_id: first, new_timeline_in: at(2.0), from_track: from, to_track: to }]);
}

/// A clip moved with the clips grouped with it: they move by the same
/// amount, in the same step, and undo puts them all back.
#[test]
fn a_grouped_clip_moved_takes_its_group_in_one_step() {
    let at = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    let (mut app, _engine) = KlippApp::for_test();
    app.state.linked_selection = false;
    let [(first, _), (second, _)] = two_pairs(&mut app);
    let group = Some(klipp_core::GroupId::new());
    for c in app.state.sequence_mut().tracks.iter_mut().flat_map(|t| t.clips.iter_mut()) {
        if c.id == first || c.id == second {
            c.group_id = group;
        }
    }
    let start = |app: &KlippApp, id| app.state.sequence().all_clips().find(|c| c.id == id).unwrap().timeline_in;
    redoes_as_made(&mut app, vec![AppCommand::MoveClip { clip_id: first, new_timeline_in: at(1.0) }]);
    assert_eq!((start(&app, first), start(&app, second)), (at(1.0), at(7.0)), "the group moved with it");
    app.send_commands(vec![AppCommand::Undo]);
    assert_eq!((start(&app, first), start(&app, second)), (at(0.0), at(6.0)), "undo puts the group back");
}

fn a_sound_file(app: &mut KlippApp) -> klipp_core::MediaId {
    let mut item = klipp_media::MediaItem::synthetic_for_test("/media/song.wav".into(), None);
    item.video_codec = String::new();
    app.state.document.media_pool.add(item)
}

/// A file appended to the end of the timeline, its sound linked, and a
/// sound file appended alone: each one step, redone as made.
#[test]
fn an_appended_file_redoes_as_made() {
    for sound_only in [false, true] {
        let (mut app, _engine) = KlippApp::for_test();
        linked_pair(&mut app);
        let media_id = if sound_only { a_sound_file(&mut app) } else { a_file(&mut app) };
        let depth = app.state.undo_depth();
        app.suppress_fps_prompt = true;
        let before = contents(&app);
        app.send_commands(vec![AppCommand::AppendClip { media_id }]);
        assert_ne!(contents(&app), before);
        // A sound alone is trimmed to the picture as a step of its own,
        // which its toast offers to undo.
        let steps = app.state.undo_depth() - depth;
        let made = contents(&app);
        for _ in 0..steps {
            app.send_commands(vec![AppCommand::Undo]);
        }
        assert!(contents(&app) == before, "undo gives the timeline back");
        for _ in 0..steps {
            app.send_commands(vec![AppCommand::Redo]);
        }
        assert!(contents(&app) == made, "redo makes it as the append made it");
    }
}

/// A picture without its sound given it back: one step, redone as made.
#[test]
fn sound_attached_to_a_picture_redoes_as_made() {
    let at = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    let (mut app, _engine) = KlippApp::for_test();
    let media_id = a_file(&mut app);
    let picture = klipp_timeline::Clip::media(media_id, at(0.0), at(3.0), at(0.0), at(3.0));
    let clip_id = picture.id;
    let video = app.state.sequence().tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
    app.state.sequence_mut().tracks[video].clips.push(picture);
    redoes_as_made(&mut app, vec![AppCommand::AttachLinkedAudio { clip_id }]);
}

/// A sound put on an audio track at a time: one step, redone as made.
#[test]
fn a_sound_inserted_at_a_time_redoes_as_made() {
    let (mut app, _engine) = KlippApp::for_test();
    linked_pair(&mut app);
    let media_id = a_sound_file(&mut app);
    let track_index = app.state.sequence().tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Audio).unwrap();
    redoes_as_made(&mut app, vec![AppCommand::InsertAudioMediaClip { media_id, track_index: Some(track_index), timeline_in: klipp_core::RationalTime::from_secs_f64(6.0, 24) }]);
}

/// A file imported onto the timeline, its sound linked, on the tracks
/// there are and on new ones; and a sound file on a new track.
#[test]
fn a_linked_import_redoes_as_made() {
    for (sound_only, on_new_tracks) in [(false, false), (false, true), (true, true), (true, false)] {
        let (mut app, _engine) = KlippApp::for_test();
        linked_pair(&mut app);
        let media_id = if sound_only { a_sound_file(&mut app) } else { a_file(&mut app) };
        redoes_as_made(&mut app, vec![AppCommand::ImportMediaLinked { media_id, on_new_tracks }]);
    }
}

/// A pair's volume keys reversed, then pasted from the clipboard: each one
/// step on both clips, redone as made.
#[test]
fn audio_keys_reversed_and_pasted_redo_as_made() {
    let (mut app, _engine) = KlippApp::for_test();
    let (picture, _sound) = linked_pair(&mut app);
    for (t, v) in [(0.5, 0.2), (2.0, 0.9)] {
        app.send_commands(vec![AppCommand::AddAudioKeyframe { clip_id: picture, param: klipp_core::AudioParam::Volume, time_secs: t, value: v }]);
    }
    redoes_as_made(&mut app, vec![AppCommand::ReverseAudioKeyframes { clip_id: picture, param: klipp_core::AudioParam::Volume }]);
    app.send_commands(vec![AppCommand::CopyAudioKeyframes { clip_id: picture, param: klipp_core::AudioParam::Volume }]);
    redoes_as_made(&mut app, vec![AppCommand::PasteAudioKeyframes { clip_id: picture, param: klipp_core::AudioParam::Pan }]);
}

/// Two files synced into a multicam group at the start of the timeline:
/// one step, redone as made.
fn a_multicam(app: &mut KlippApp) {
    let (a, b) = (a_file(app), a_file(app));
    let placement = |media_id, start_secs, is_reference| klipp_core::AnglePlacement { media_id, start_secs, method: Some(klipp_core::SyncMethod::Timecode), confidence: None, is_reference };
    let plan = klipp_core::MulticamPlan { placements: vec![placement(a, 0.0, true), placement(b, 0.5, false)], notes: Vec::new() };
    let sequence_id = app.state.sequence().id;
    redoes_as_done(app, |app| app.apply_multicam_plan(plan, &[a, b], sequence_id, 0.0));
}

/// A multicam made from synced angles, an angle cut to and the group's
/// sound chosen: each one step, redone as made.
#[test]
fn multicam_edits_redo_as_made() {
    let (mut app, _engine) = KlippApp::for_test();
    a_multicam(&mut app);
    let at = klipp_core::RationalTime::from_secs_f64(2.0, 24);
    redoes_as_made(&mut app, vec![AppCommand::SwitchMulticamAngleAt { angle: 2, at }]);
    redoes_as_made(&mut app, vec![AppCommand::SetMulticamSound { sound: klipp_core::MulticamSound::Angle(1), at: Some(at) }]);
}

/// A clip split at the scene markers it carries: one step, redone as made.
#[test]
fn a_split_at_scene_markers_redoes_as_made() {
    let (mut app, _engine) = KlippApp::for_test();
    let (picture, _) = linked_pair(&mut app);
    let clip = app.state.sequence_mut().tracks.iter_mut().find_map(|t| t.find_clip_mut(picture)).unwrap();
    for time_secs in [1.0, 2.5] {
        clip.clip_markers.push(klipp_timeline::ClipMarker { time_secs, name: "Scene".into(), color: klipp_core::MarkerColor::Green, comment: String::new() });
    }
    redoes_as_made(&mut app, vec![AppCommand::SplitAtSceneMarkers { clip_id: picture }]);
}

/// A word taken as right: one step on the sequence's brief, redone as
/// made. (The transcripts' language is left out: it is the transcriber's
/// for the whole process, and the filler tests read it.)
#[test]
fn a_word_taken_as_right_redoes_as_made() {
    let (mut app, _engine) = KlippApp::for_test();
    redoes_as_made(&mut app, vec![AppCommand::SpellingIgnore { word: "Teh".into() }]);
    super::transcription::brief_to_transcription(None);
}

/// A clip faded across the frame, and the fade taken off: each one step,
/// redone as made.
#[test]
fn an_opacity_gradient_redoes_as_made() {
    let (mut app, _engine) = KlippApp::for_test();
    let (picture, _) = linked_pair(&mut app);
    let mask = klipp_core::Mask { feather: 0.5, ..klipp_core::Mask::new(klipp_core::MaskShape::LinearGradient { start_x: 0.5, start_y: 0.4, end_x: 0.5, end_y: 0.9 }) };
    redoes_as_made(&mut app, vec![AppCommand::SetOpacityGradient { clip_id: picture, mask: Some(Box::new(mask)) }]);
    redoes_as_made(&mut app, vec![AppCommand::SetOpacityGradient { clip_id: picture, mask: None }]);
}

/// A cut-out clip's background put back: one step, redone as made.
#[test]
fn a_background_restored_redoes_as_made() {
    let (mut app, _engine) = KlippApp::for_test();
    let (picture, _) = linked_pair(&mut app);
    let clip = app.state.sequence_mut().tracks.iter_mut().find_map(|t| t.find_clip_mut(picture)).unwrap();
    klipp_timeline::GradeAccess::edit_grade(clip, |g| super::track_subject::bind_subject(g, klipp_nodes::SubjectMatte::Subject, None, true));
    redoes_as_made(&mut app, vec![AppCommand::RestoreBackground { clip_id: picture }]);
}

/// A storyline clip's end trimmed to the playhead, what follows closing
/// up: one step, redone as made.
#[test]
fn a_ripple_trim_on_the_storyline_redoes_as_made() {
    let (mut app, _engine) = KlippApp::for_test();
    let media_id = a_file(&mut app);
    let track_index = app.state.sequence().primary_track_index().unwrap();
    app.state.append_brings_audio = false;
    for _ in 0..2 {
        app.send_commands(vec![AppCommand::AppendClipToTrack { media_id, track_index }]);
    }
    let first = app.state.sequence().tracks[track_index].clips[0].id;
    app.state.selected_clip = Some(first);
    app.state.playhead = klipp_core::RationalTime::from_secs_f64(4.0, 24);
    redoes_as_made(&mut app, vec![AppCommand::RippleTrimTail { clip_id: first }]);
}

/// A file at another size and rate than a new sequence's.
fn a_smaller_file(app: &mut KlippApp) -> klipp_core::MediaId {
    let mut item = klipp_media::MediaItem::synthetic_for_test("/media/small.mov".into(), None);
    item.resolution = klipp_core::Resolution { width: 1280, height: 720 };
    item.fps = klipp_core::Fps::new(25, 1);
    item.duration = klipp_core::RationalTime { value: 250, rate: 25 };
    app.state.document.media_pool.add(item)
}

/// The first clip on an empty timeline, however it is placed: the
/// sequence takes its size and rate in the same step, so undo gives the
/// sequence its own format back and redo takes the clip's again. Most
/// ways recorded the clip alone, and an undone first clip left the
/// sequence at the clip's size.
#[test]
fn a_first_clip_undoes_its_format_whichever_way_it_is_placed() {
    let ways: [fn(klipp_core::MediaId, usize) -> AppCommand; 7] = [
        |media_id, track_index| AppCommand::AppendClipToTrack { media_id, track_index },
        |media_id, _| AppCommand::AppendClip { media_id },
        |_, _| AppCommand::InsertSourceAtPlayhead,
        |_, _| AppCommand::OverwriteEdit,
        |_, _| AppCommand::InsertEdit,
        |_, _| AppCommand::PlaceOnTopEdit,
        |_, _| AppCommand::FitToFillEdit,
    ];
    for (n, way) in ways.iter().enumerate() {
        let (mut app, _engine) = KlippApp::for_test();
        let media_id = a_smaller_file(&mut app);
        app.state.source_media_id = Some(media_id);
        let track_index = app.state.sequence().primary_track_index().unwrap();
        let format = |app: &KlippApp| (app.state.sequence().resolution, app.state.sequence().fps);
        let own = format(&app);
        if n == 6 {
            // Fit to Fill fills the timeline's marks.
            let seq = app.state.sequence_mut();
            (seq.in_point, seq.out_point) = (Some(klipp_core::RationalTime::from_secs_f64(0.0, 24)), Some(klipp_core::RationalTime::from_secs_f64(2.0, 24)));
        }
        redoes_as_made(&mut app, vec![way(media_id, track_index)]);
        assert_eq!(format(&app).0.width, 1280, "way {n}: the sequence took the clip's size");
        app.send_commands(vec![AppCommand::Undo]);
        assert_eq!(format(&app), own, "way {n}: undone, the sequence's own format");
        assert_eq!(app.state.fps, own.1, "way {n}: and the app's rate with it");
    }
}

/// A blade through a linked pair cuts both as one step, undone whole,
/// even with no batch open around it.
#[test]
fn a_blade_through_a_pair_is_one_step_and_redoes_as_made() {
    let (mut app, _engine) = KlippApp::for_test();
    let [(picture, _), _] = two_pairs(&mut app);
    let track = app.state.sequence().tracks.iter().position(|t| t.clips.iter().any(|c| c.id == picture)).unwrap();
    redoes_as_done(&mut app, |app| assert!(app.state.blade_one(track, picture, klipp_core::RationalTime::from_secs_f64(2.0, 24))));
    assert_eq!(app.state.sequence().all_clips().filter(|c| !c.kind.is_gap()).count(), 6, "both halves of both clips, the other pair whole");
}

/// A roll at a cut, the linked sounds rolling with it.
#[test]
fn a_roll_redoes_as_made() {
    let (mut app, _engine) = KlippApp::for_test();
    let at = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    let [(a, _), (b, _)] = two_pairs(&mut app);
    // The second pair moved up against the first, so they share a cut.
    for c in app.state.sequence_mut().all_clips_mut() {
        if c.timeline_in == at(6.0) {
            (c.timeline_in, c.timeline_out) = (at(4.0), at(8.0));
        }
    }
    redoes_as_done(&mut app, |app| app.state.roll_trim(a, b, (at(3.0), at(3.0)), (at(-1.0), at(3.0)), true));
}

/// A clip's input colour space, set on its file and the file's other
/// clips with it.
#[test]
fn an_input_colour_space_redoes_as_made() {
    let (mut app, _engine) = KlippApp::for_test();
    let media = a_file(&mut app);
    let at = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    let (one, two) = (klipp_timeline::Clip::media(media, at(0.0), at(2.0), at(0.0), at(2.0)), klipp_timeline::Clip::media(media, at(0.0), at(2.0), at(2.0), at(4.0)));
    let id = one.id;
    let video = app.state.sequence().tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
    app.state.sequence_mut().tracks[video].clips.extend([one, two]);
    let format = |app: &KlippApp| app.state.document.media_pool.get(&media).map(|i| (i.log_format, i.log_format_user_set)).unwrap();
    let was = format(&app);
    redoes_as_done(&mut app, |app| assert_eq!(app.state.set_clip_log_format(id, klipp_core::LogFormat::SLog3), Some(media)));
    assert_eq!(format(&app), (klipp_core::LogFormat::SLog3, true), "redone, the file's format");
    app.send_commands(vec![AppCommand::Undo]);
    assert_eq!(format(&app), was, "undone, the file's own");
}

/// A published title field changed: one step, put back and made again
/// as it was made.
#[test]
fn a_title_field_redoes_as_made() {
    let (mut app, _engine) = KlippApp::for_test();
    let mut td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Jane Doe".into(), ..Default::default() });
    td.published = td.default_fields();
    let at = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    let mut clip = klipp_timeline::Clip::gap(at(0.0), at(4.0));
    clip.kind = klipp_timeline::ClipKind::Title(td);
    let id = clip.id;
    let video = app.state.sequence().tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
    app.state.sequence_mut().tracks[video].clips.push(clip);
    redoes_as_done(&mut app, |app| app.state.set_title_field(id, 0, &klipp_core::TitleFieldValue::Text("John Roe".into()), true).unwrap());
}

/// A title added at the playhead is one step: a text track made for it
/// goes with its undo, and an empty text track the person already had
/// stays. That one used to be taken for made and went with the title.
#[test]
fn a_title_added_takes_back_only_the_track_made_for_it() {
    let (mut app, _engine) = KlippApp::for_test();
    // The tracks there are, as each frame notes them.
    app.state.seed_recorded_tracks();
    redoes_as_made(&mut app, vec![AppCommand::AddTitleAtPlayhead { over_clip: None }]);
    let (mut app, _engine) = KlippApp::for_test();
    app.state.sequence_mut().add_text_track("Lower Thirds".into());
    app.state.note_recorded_tracks();
    redoes_as_made(&mut app, vec![AppCommand::AddTitleAtPlayhead { over_clip: None }]);
    assert!(app.state.sequence().tracks.iter().any(|t| t.name == "Lower Thirds" && t.clips.len() == 1), "the title on the track the person had");
}

/// Three 2 s clips end to end on the primary storyline: their ids.
fn on_the_storyline(app: &mut KlippApp) -> [klipp_core::ClipId; 3] {
    let at = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    let p = app.state.sequence().tracks.iter().position(|t| t.magnetic_role == klipp_timeline::TrackRole::Primary).unwrap();
    let clips: Vec<_> = (0..3).map(|i| klipp_timeline::Clip::media(klipp_core::MediaId::new(), at(2.0 * i as f64), at(2.0 * i as f64 + 2.0), at(0.0), at(2.0))).collect();
    let ids = [clips[0].id, clips[1].id, clips[2].id];
    app.state.sequence_mut().tracks[p].clips = clips;
    ids
}

/// Storyline removals and block moves, effect edits and the All Effects
/// switch are each one step that redoes as it was made: the step makes
/// the edit, not a record of it next to one made in place.
#[test]
fn storyline_removals_moves_and_effect_edits_redo_as_made() {
    for lift in [false, true] {
        let (mut app, _engine) = KlippApp::for_test();
        let ids = on_the_storyline(&mut app);
        redoes_as_done(&mut app, |app| { super::selection_removal::remove_clips(&mut app.state, &[ids[1]], lift, "Delete").unwrap(); });
    }
    for overwrite in [false, true] {
        let (mut app, _engine) = KlippApp::for_test();
        let ids = on_the_storyline(&mut app);
        let at = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
        redoes_as_done(&mut app, |app| super::primary_block::move_block(&mut app.state, &[(ids[0], at(0.0))], &[], at(4.0), overwrite).unwrap());
    }
    let (mut app, _engine) = KlippApp::for_test();
    let ids = on_the_storyline(&mut app);
    redoes_as_made(&mut app, vec![AppCommand::AddBuiltinEffect { clip_id: ids[0], effect: klipp_effects::BuiltinEffect::Opacity { value: 0.5 } }]);
    redoes_as_made(&mut app, vec![AppCommand::SetEffectsSwitchedOff { clip_id: ids[0], nodes: vec![uuid::Uuid::new_v4()] }]);
}

/// A clip's depth and the sequence's shutter, title brightness and
/// camera keys are each one step that counts toward autosave and redoes
/// as made. They went onto the history past the edit's bookkeeping.
#[test]
fn clip_depth_and_sequence_settings_count_and_redo_as_made() {
    let (mut app, _engine) = KlippApp::for_test();
    let (clip_id, _) = linked_pair(&mut app);
    for step in [
        AppCommand::SetClipDepth { clip_id, depth: 4.0 },
        AppCommand::SetSequenceShutter { sequence_idx: 0, shutter_deg: 90.0, samples: 8 },
        AppCommand::SetSequenceTitleNits { sequence_idx: 0, nits: 600.0 },
        AppCommand::SetSequenceCameraKey { sequence_idx: 0, param: klipp_core::CameraParam::Zoom, time_secs: 1.0, value: 1.5 },
    ] {
        let counted = app.state.history_ops_since_save;
        app.state.dirty = false;
        redoes_as_made(&mut app, vec![step.clone()]);
        assert!(app.state.history_ops_since_save > counted, "{step:?}: counted toward autosave");
    }
}

/// A clip's level set from the inspector and gains set by Normalize are
/// each one step for the pair that redoes as made.
#[test]
fn clip_levels_and_normalized_gains_redo_as_made() {
    let (mut app, _engine) = KlippApp::for_test();
    let (picture, sound) = linked_pair(&mut app);
    let c = app.state.sequence().all_clips().find(|c| c.id == sound).unwrap().clone();
    redoes_as_made(&mut app, vec![AppCommand::UpdateClipAudio {
        clip_id: sound, gain: c.gain, volume: 0.5, pan: 0.25, fade_in_secs: 0.5, fade_out_secs: 0.0,
        fade_in_curve: c.fade_in_curve, fade_out_curve: c.fade_out_curve, channel_config: c.channel_config,
    }]);
    redoes_as_made(&mut app, vec![AppCommand::ApplyClipGains { gains: vec![(picture, 1.5)] }]);
}

/// Each change to a sound's audio chain, its effects and their keys, is
/// one step that redoes as it was made.
#[test]
fn audio_chain_edits_redo_as_made() {
    let (mut app, _engine) = KlippApp::for_test();
    let (_, clip_id) = linked_pair(&mut app);
    let param_name = || "frequency".to_string();
    let key = |t: f64| AppCommand::AddAudioEffectKeyframe { clip_id, effect_index: 0, param_name: param_name(), time_secs: t, value: 0.5 };
    for step in [
        vec![AppCommand::AddAudioEffect { clip_id, effect_type: "high_pass".into() }],
        vec![AppCommand::AddAudioEffect { clip_id, effect_type: "hum_removal".into() }],
        vec![AppCommand::UpdateAudioEffectParam { clip_id, effect_index: 0, param_name: param_name(), value: 120.0 }],
        vec![AppCommand::SetAudioEffectParams { clip_id, effect_index: 1, params: vec![("strength".into(), 0.8)] }],
        vec![AppCommand::SetAudioEffectWetDry { clip_id, effect_index: 0, wet_dry: 0.4 }],
        vec![AppCommand::ToggleAudioEffect { clip_id, effect_index: 1 }],
        vec![key(0.5)],
        vec![key(2.0)],
        vec![AppCommand::ReverseAudioEffectKeyframes { clip_id, effect_index: 0, param_name: param_name() }],
        vec![AppCommand::ScaleAudioEffectKeyframeTiming { clip_id, effect_index: 0, param_name: param_name(), factor: 0.5 }],
        vec![AppCommand::InsertAudioEffectKeyframeBump { clip_id, effect_index: 0, param_name: param_name(), center_time: 2.5, bump_value: 1.0, base_value: 0.2, duration_secs: 0.5, ramp_secs: 0.25 }],
        vec![AppCommand::ReorderAudioEffect { clip_id, from_index: 0, to_index: 1 }],
        vec![AppCommand::RemoveAudioEffect { clip_id, effect_index: 1 }],
    ] {
        redoes_as_made(&mut app, step);
    }
    let keyframes = [(0.5, 0.2), (1.5, 0.9)].map(|(time_secs, value)| klipp_core::Keyframe { time_secs, value, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }).to_vec();
    app.state.keyframe_clipboard = Some(crate::state::KeyframeClipboard { keyframes, source_duration_secs: 2.0 });
    redoes_as_made(&mut app, vec![AppCommand::PasteAudioEffectKeyframes { clip_id, effect_index: 0, param_name: "strength".into() }]);
}

/// A new bin, and items moved into a bin that is not there yet: each one
/// step whose undo takes the bin it made away, and whose redo makes it
/// again as it was.
#[test]
fn bins_made_by_a_step_go_with_its_undo() {
    let (mut app, _engine) = KlippApp::for_test();
    let (a, b) = (a_file(&mut app), a_sound_file(&mut app));
    let pool = |app: &KlippApp| {
        let p = &app.state.document.media_pool;
        let mut bins: Vec<_> = p.bins.iter().map(|b| (b.id, b.name.clone())).collect();
        bins.sort_by(|x, y| x.1.cmp(&y.1));
        (bins, [a, b].map(|id| p.get(&id).map(|m| (m.bin.clone(), m.bin_id))))
    };
    let step = |app: &mut KlippApp, send: AppCommand| {
        let before = pool(app);
        app.send_commands(vec![send]);
        let made = pool(app);
        assert_ne!(made, before, "the step changed the pool");
        app.send_commands(vec![AppCommand::Undo]);
        assert_eq!(pool(app), before, "undo takes it back, the bin it made too");
        app.send_commands(vec![AppCommand::Redo]);
        assert_eq!(pool(app), made, "redo makes it as it was made");
    };
    step(&mut app, AppCommand::NewBin { media_ids: vec![a] });
    step(&mut app, AppCommand::MoveItemsToBin { media_ids: vec![a, b], bin: Some("Selects".into()) });
    step(&mut app, AppCommand::MoveItemToBin { media_id: b, bin: None });
    step(&mut app, AppCommand::CreateBin { name: "B-roll".into() });
    let parent_id = app.state.document.media_pool.bins.iter().find(|b| b.name == "B-roll").unwrap().id;
    step(&mut app, AppCommand::CreateSubBin { parent_id, name: "Drone".into() });
    // Taken out with its item moved back to the root, and put back in its
    // place with the item in it.
    step(&mut app, AppCommand::DeleteBin { name: "Selects".into() });
    let order = |app: &KlippApp| app.state.document.media_pool.bins.iter().map(|b| b.name.clone()).collect::<Vec<_>>();
    let was = order(&app);
    app.send_commands(vec![AppCommand::DeleteBin { name: "New Bin".into() }, AppCommand::Undo]);
    assert_eq!(order(&app), was, "undone, each bin in its place");
}

/// A file renamed is one step, undone and redone.
#[test]
fn a_rename_of_a_file_redoes_as_made() {
    let (mut app, _engine) = KlippApp::for_test();
    let media_id = a_file(&mut app);
    let name = |app: &KlippApp| app.state.document.media_pool.get(&media_id).unwrap().filename.clone();
    let was = name(&app);
    app.send_commands(vec![AppCommand::RenameMediaItem { media_id, new_filename: "Interview".into() }]);
    assert_eq!((name(&app), app.state.undo_depth()), ("Interview".to_string(), 1));
    app.send_commands(vec![AppCommand::Undo]);
    assert_eq!(name(&app), was);
    app.send_commands(vec![AppCommand::Redo]);
    assert_eq!(name(&app), "Interview");
}

/// A track header's magnetic actions: a connected track promoted to a
/// storyline, broken apart again, and a track set as the primary, each
/// one step redone as it was made.
#[test]
fn track_header_storyline_actions_redo_as_made() {
    use crate::timeline::track_headers::{apply_track_action, TrackAction};
    let at = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    let (mut app, _engine) = KlippApp::for_test();
    linked_pair(&mut app);
    app.state.sequence_mut().add_video_track("Video 2".into());
    let above = app.state.sequence().tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video && t.magnetic_role != klipp_timeline::TrackRole::Primary).unwrap();
    app.state.sequence_mut().tracks[above].clips.push(klipp_timeline::Clip::media(klipp_core::MediaId::new(), at(1.0), at(2.0), at(0.0), at(1.0)));
    app.state.sequence_mut().tracks[above].magnetic_role = klipp_timeline::TrackRole::Connected;
    let mut cmds = Vec::new();
    for action in [TrackAction::PromoteToStoryline(above), TrackAction::BreakApartStoryline(above), TrackAction::SetAsPrimary(above)] {
        redoes_as_done(&mut app, |app| apply_track_action(&mut app.state, &mut cmds, action));
    }
    assert_eq!(app.state.sequence().primary_track_index(), Some(above));
}

/// A mixer routing change dragged: heard live as it moves, then one step
/// from where the drag began that undo and redo replay.
#[test]
fn a_routing_drag_is_one_step_redone_as_made() {
    let (mut app, _engine) = KlippApp::for_test();
    let mut cmds = Vec::new();
    redoes_as_done(&mut app, |app| crate::panels::aux_routing::drag_for_test(&mut app.state, &mut cmds));
}

/// A gesture's edit shown frame after frame, each worked out from where
/// the gesture began: cancelled, it gives the timeline back with no step
/// and nothing unsaved; ended, it is one step that counts toward autosave
/// and redoes as it was shown.
#[test]
fn a_live_edit_shown_cancels_to_the_start_and_ends_as_one_step() {
    let at = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    // Each frame's edit, from the timeline the gesture began on: the
    // middle clip's tail dragged back and forth, rippling the one after,
    // or its head.
    let frame = |start: &klipp_timeline::Sequence, id, k: usize| {
        let mut s = start.clone();
        let to = 0.5 + 1.4 * ((k as f64 * 0.37).sin() * 0.5 + 0.5);
        let cmd = if k.is_multiple_of(3) { klipp_timeline::magnetic::MagneticEditor::trim_head(&mut s, id, at(2.0 + to * 0.5)) } else { klipp_timeline::magnetic::MagneticEditor::trim_tail(&mut s, id, at(2.0 + to)) };
        klipp_document::history::EditCommand::Magnetic(cmd.command)
    };
    for end in [false, true] {
        let (mut app, _engine) = KlippApp::for_test();
        app.suppress_fps_prompt = true;
        let ids = on_the_storyline(&mut app);
        app.state.note_recorded_tracks();
        let start = app.state.sequence().clone();
        let before = contents(&app);
        let (depth, ops) = (app.state.undo_depth(), app.state.history_ops_since_save);
        let mut last = None;
        for k in 0..200 {
            let edit = frame(&start, ids[1], k);
            let _ = app.state.show_live(edit.clone());
            last = Some(edit);
        }
        assert!(app.state.is_live());
        assert_eq!(app.state.undo_depth(), depth, "nothing recorded while it is shown");
        // What is shown is the last frame's edit made once on the start.
        let mut once = KlippApp::for_test().0;
        once.state.document.sequences = vec![start.clone()];
        let _ = once.state.commit(last.unwrap());
        assert!(contents(&app) == contents(&once), "the last frame shown, made once on the start");
        if !end {
            let _ = app.state.cancel_live();
            assert!(!app.state.is_live());
            assert!(contents(&app) == before, "cancelled, the timeline as the gesture found it");
            assert_eq!((app.state.undo_depth(), app.state.history_ops_since_save, app.state.dirty), (depth, ops, false), "and no step");
            continue;
        }
        let shown = contents(&app);
        assert!(app.state.end_live());
        assert!(!app.state.is_live() && !app.state.end_live(), "ended once");
        assert!(contents(&app) == shown, "what lands is what was shown");
        assert_eq!((app.state.undo_depth(), app.state.history_ops_since_save), (depth + 1, ops + 1), "one step, toward autosave");
        assert!(app.state.dirty);
        app.send_commands(vec![AppCommand::Undo]);
        assert!(contents(&app) == before, "undo gives the timeline back");
        app.send_commands(vec![AppCommand::Redo]);
        assert!(contents(&app) == shown, "redo makes it as it was shown");
    }
}

/// A height handle dragged shows the track's height (every track's with
/// Ctrl), marked unsaved with no undo step.
#[test]
fn track_heights_shown_while_dragged() {
    let (mut app, _engine) = KlippApp::for_test();
    let depth = app.state.undo_depth();
    let n = app.state.sequence().tracks.len();
    assert!(n >= 2);
    app.state.show_track_height(1, 90.0, false);
    let heights = |app: &KlippApp| app.state.sequence().tracks.iter().map(|t| t.custom_height).collect::<Vec<_>>();
    assert_eq!(heights(&app)[1], Some(90.0));
    assert!(heights(&app).iter().enumerate().all(|(i, h)| i == 1 || h.is_none()), "the one track");
    app.state.show_track_height(0, 70.0, true);
    assert!(heights(&app).iter().all(|h| *h == Some(70.0)), "every track, with Ctrl");
    assert!(app.state.dirty);
    assert_eq!(app.state.undo_depth(), depth, "a height is no edit");
}

/// A clip let go near the storyline is moved and connected in one step,
/// to the storyline clip under where it landed; dragged away from the
/// storyline a connected clip is moved and let go in one step. One undo
/// takes back both and redo makes both. The connection was its own step
/// before the move, made where the clip had been: it took the storyline
/// clip nearest the clip's old place, and undo left it connected.
#[test]
fn a_drop_moves_and_connects_or_disconnects_as_one_step() {
    let at = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    let (mut app, _engine) = KlippApp::for_test();
    let ids = on_the_storyline(&mut app);
    app.state.sequence_mut().add_video_track("Video 2".into());
    let v2 = app.state.sequence().tracks.iter().position(|t| t.name == "Video 2").unwrap();
    let clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), at(0.5), at(1.5), at(0.0), at(1.0));
    let id = clip.id;
    app.state.sequence_mut().tracks[v2].clips.push(clip);
    app.state.note_recorded_tracks();
    let anchor = |app: &KlippApp| app.state.sequence().all_clips().find(|c| c.id == id).unwrap().magnetic_anchor.as_ref().map(|a| a.anchor_clip_id);
    redoes_as_made(&mut app, vec![AppCommand::DropClip { clip_id: id, new_timeline_in: at(4.5), magnet: klipp_core::Magnet::Connect }]);
    assert_eq!(app.state.sequence().all_clips().find(|c| c.id == id).unwrap().timeline_in, at(4.5));
    assert_eq!(anchor(&app), Some(ids[2]), "connected to the storyline clip it landed over");
    redoes_as_made(&mut app, vec![AppCommand::DropClip { clip_id: id, new_timeline_in: at(3.0), magnet: klipp_core::Magnet::Disconnect }]);
    assert_eq!(anchor(&app), None, "let go");
    // Dropped where it was, near the storyline: the connection alone.
    redoes_as_made(&mut app, vec![AppCommand::DropClip { clip_id: id, new_timeline_in: at(3.0), magnet: klipp_core::Magnet::Connect }]);
    assert_eq!(anchor(&app), Some(ids[1]));
}

/// Media or a title let go off the tracks makes its track and goes on it
/// as one step: undo takes away the clip and the track, redo makes both.
/// The timeline made the track itself before sending the drop.
#[test]
fn a_drop_off_the_tracks_makes_its_track_in_the_same_step() {
    let at = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    for kind in [klipp_core::NewTrack::Video, klipp_core::NewTrack::Audio, klipp_core::NewTrack::Text] {
        let (mut app, _engine) = KlippApp::for_test();
        linked_pair(&mut app);
        app.state.note_recorded_tracks();
        let media_id = a_file(&mut app);
        let tracks = app.state.sequence().tracks.len();
        let then = match kind {
            klipp_core::NewTrack::Text => AppCommand::CreateTitleClip { track_index: 0, timeline_in: at(2.0), duration_frames: 48 },
            _ => AppCommand::PlaceMedia { media_id, drop: klipp_core::events::ClipDrop::OnTrackAt { track_index: 0, at: at(2.0), overwrite: true } },
        };
        redoes_as_made(&mut app, vec![AppCommand::DropOnNewTrack { kind, then: Box::new(then) }]);
        let seq = app.state.sequence();
        assert!(seq.tracks.len() > tracks, "{kind:?}: a track made (and one for its sound, a picture with sound)");
        let made = seq.tracks.iter().find(|t| !t.clips.is_empty() && t.clips.iter().any(|c| c.timeline_in == at(2.0))).unwrap_or_else(|| panic!("{kind:?}: the clip placed"));
        let want = match kind {
            klipp_core::NewTrack::Video => klipp_timeline::TrackKind::Video,
            klipp_core::NewTrack::Audio => klipp_timeline::TrackKind::Audio,
            klipp_core::NewTrack::Text => klipp_timeline::TrackKind::Text,
        };
        assert_eq!(made.kind, want, "{kind:?}: on the new track");
        app.send_commands(vec![AppCommand::Undo]);
        assert_eq!(app.state.sequence().tracks.len(), tracks, "{kind:?}: undo takes the track away too");
    }
}

/// Each of a track's settings, a track added, moved or taken away, and a
/// clip linked or grouped is one step under its own name, recorded by its
/// handler: undo gives the timeline back, redo makes it again, and the
/// copy the engine plays and the control socket reads has it at once.
#[test]
fn track_settings_and_links_are_each_their_own_named_step() {
    use klipp_timeline::TrackKind;
    type Make = fn(&KlippApp, (klipp_core::ClipId, klipp_core::ClipId)) -> AppCommand;
    let cases: Vec<(&str, Make)> = vec![
        ("Lock Track", |a, _| AppCommand::SetTrackLocked { track_id: first(a, TrackKind::Video).1, locked: true }),
        ("Hide Track", |a, _| AppCommand::SetTrackVisibility { track_index: first(a, TrackKind::Video).0, visible: false }),
        ("Freeze Track", |a, _| AppCommand::SetTrackFrozen { track_id: first(a, TrackKind::Video).1, frozen: true }),
        ("Lock While Frozen", |a, _| AppCommand::SetTrackLockWhileFrozen { track_id: first(a, TrackKind::Video).1, on: true }),
        ("Rename Track", |a, _| AppCommand::RenameTrack { track_id: first(a, TrackKind::Audio).1, name: "Boat".into() }),
        ("Sync Lock Track", |a, _| AppCommand::SetTrackSyncLock { track_id: first(a, TrackKind::Audio).1, on: !a.state.sequence().tracks[first(a, TrackKind::Audio).0].sync_lock }),
        ("Automation Mode", |a, _| AppCommand::SetTrackAutomationMode { track_index: first(a, TrackKind::Audio).0, mode: klipp_core::automation::AutomationMode::Off }),
        ("Edit Track Automation", |a, _| AppCommand::SetTrackAutomationKeys { track_index: first(a, TrackKind::Audio).0, param: klipp_core::automation::TrackParam::Pan, keys: vec![klipp_core::Keyframe { time_secs: 1.0, value: 0.5, interpolation: klipp_core::Interpolation::Linear, tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0), handles: Default::default() }] }),
        ("Track Language", |a, _| AppCommand::SetTrackLanguage { track_id: first(a, TrackKind::Audio).1, language: Some("de".into()) }),
        ("Track Role", |a, _| AppCommand::SetTrackRole { track_id: first(a, TrackKind::Audio).1, role: klipp_core::roles::RoleRef::parse("music") }),
        ("Untarget Track", |a, _| AppCommand::SetTrackTargeted { track_id: first(a, TrackKind::Video).1, on: false }),
        ("Patch Source", |a, _| AppCommand::SetTrackSourcePatch { track_id: first(a, TrackKind::Video).1, on: true }),
        ("Track Colour", |a, _| AppCommand::SetTrackColourLabel { track_index: first(a, TrackKind::Video).0, label: 3 }),
        ("Add Track", |_, _| AppCommand::AddVideoTrack),
        ("Add Track", |_, _| AppCommand::AddAudioTrack),
        ("Add Track", |_, _| AppCommand::AddSubtitleTrack { name: "Subtitles".into() }),
        ("Add Track", |a, _| AppCommand::AddTrackAbove { track_index: first(a, TrackKind::Audio).0 }),
        ("Add Track", |a, _| AppCommand::AddTrackBelow { track_index: first(a, TrackKind::Video).0 }),
        ("Remove Track", |a, _| AppCommand::RemoveTrack { track_id: a.state.sequence().tracks.iter().find(|t| t.clips.is_empty()).unwrap().id }),
        ("Unlink", |_, (picture, _)| AppCommand::UnlinkClip { clip_id: picture }),
        ("Group", |_, (picture, sound)| AppCommand::GroupClips { clip_ids: vec![picture, sound] }),
    ];
    for (label, make) in cases {
        let (mut app, _engine) = KlippApp::for_test();
        let pair = linked_pair(&mut app);
        // A second picture track, so one can be taken away or moved.
        app.state.sequence_mut().add_video_track("Video 2".into());
        let cmd = make(&app, pair);
        redoes_as_done(&mut app, |app| {
            app.send_commands(vec![cmd.clone()]);
            // Read before the undo and redo, which tell the engine anyway.
            let seen = |tracks: &[klipp_timeline::Track]| serde_json::to_value(tracks.iter().map(|t| (&t.name, t.locked, t.visible, t.frozen, t.clips.len())).collect::<Vec<_>>()).unwrap();
            assert_eq!(seen(&app.sequence.snapshot().tracks), seen(&app.state.sequence().tracks), "{cmd:?}: the engine's copy has it");
        });
        assert_eq!(app.state.undo_label().as_deref(), Some(label), "{cmd:?}");
    }

    // A move among tracks of one kind; an ungroup; a refused edit is no step.
    let (mut app, _engine) = KlippApp::for_test();
    let (picture, sound) = linked_pair(&mut app);
    app.state.sequence_mut().add_video_track("Video 2".into());
    let videos: Vec<usize> = app.state.sequence().tracks.iter().enumerate().filter(|(_, t)| t.kind == TrackKind::Video).map(|(i, _)| i).collect();
    redoes_as_made(&mut app, vec![AppCommand::MoveTrack { from: videos[0], to: videos[1] }]);
    assert_eq!(app.state.undo_label().as_deref(), Some("Move Track"));
    app.send_commands(vec![AppCommand::GroupClips { clip_ids: vec![picture, sound] }]);
    let group = app.state.sequence().all_clips().find(|c| c.id == picture).unwrap().group_id.unwrap();
    redoes_as_made(&mut app, vec![AppCommand::UngroupClips { group_id: group }]);
    assert_eq!(app.state.undo_label().as_deref(), Some("Ungroup"));
    let depth = app.state.undo_depth();
    app.send_commands(vec![
        AppCommand::RenameTrack { track_id: klipp_core::TrackId::new(), name: "Gone".into() },
        AppCommand::MoveTrack { from: videos[0], to: first(&app, TrackKind::Audio).0 },
        AppCommand::SetTrackLocked { track_id: first(&app, TrackKind::Video).1, locked: app.state.sequence().tracks[first(&app, TrackKind::Video).0].locked },
    ]);
    assert_eq!(app.state.undo_depth(), depth, "a track gone, a move across kinds, a lock as it was: no step");
}

/// The index and id of the first track of `kind`.
fn first(app: &KlippApp, kind: klipp_timeline::TrackKind) -> (usize, klipp_core::TrackId) {
    let i = app.state.sequence().tracks.iter().position(|t| t.kind == kind).unwrap();
    (i, app.state.sequence().tracks[i].id)
}

/// A clip enabled, disabled or soloed, a gap closed or put in, a range
/// lifted or extracted, a generator or adjustment layer added, and the
/// source edits are each one step under their own name, recorded by
/// their handlers: undo gives the timeline back, redo makes it again.
#[test]
fn clip_and_timeline_edits_are_each_their_own_named_step() {
    use klipp_timeline::TrackKind;
    let at = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    type Make = fn(&mut KlippApp, klipp_core::ClipId) -> AppCommand;
    let cases: Vec<(&str, Make)> = vec![
        ("Disable Clip", |_, picture| AppCommand::SetClipEnabled { clip_id: picture, enabled: false }),
        ("Solo Clip", |_, picture| AppCommand::SetClipSolo { clip_id: picture, solo: true }),
        ("Close Gap", |_, _| AppCommand::CloseGap { track_index: 0, gap_start: klipp_core::RationalTime::from_secs_f64(4.0, 24), gap_end: klipp_core::RationalTime::from_secs_f64(6.0, 24) }),
        ("Close All Gaps", |_, _| AppCommand::CloseAllGaps { track_index: 0 }),
        ("Insert Gap", |_, _| AppCommand::InsertGapOnTrack { track_idx: 0, duration_secs: 1.0 }),
        ("Add Generator", |_, _| AppCommand::CreateGeneratorClip { track_index: None, at: None, data: klipp_core::GeneratorData::ColorBars }),
        ("Add Adjustment Layer", |_, _| AppCommand::CreateAdjustmentLayer),
        ("Overwrite", |a, _| {
            a.state.source_media_id = Some(a_file(a));
            AppCommand::OverwriteEdit
        }),
        // Patched to the free track on top: laid there in place, not
        // through the storyline's own step.
        ("Overwrite", |a, _| {
            a.state.source_media_id = Some(a_file(a));
            a.state.sequence_mut().patch_source_to(0);
            AppCommand::OverwriteEdit
        }),
        ("Place on Top", |a, _| {
            a.state.source_media_id = Some(a_file(a));
            AppCommand::PlaceOnTopEdit
        }),
    ];
    for (label, make) in cases {
        let (mut app, _engine) = KlippApp::for_test();
        let (picture, _) = linked_pair(&mut app);
        // A track of its own on top, clips at 0 and 6 s with a gap
        // between: the storyline under it has none.
        let mut v2 = klipp_timeline::Track::new("Video 2", TrackKind::Video);
        let m = klipp_core::MediaId::new();
        v2.clips = vec![klipp_timeline::Clip::media(m, at(0.0), at(4.0), at(0.0), at(4.0)), klipp_timeline::Clip::media(m, at(6.0), at(8.0), at(0.0), at(2.0))];
        app.state.sequence_mut().tracks.insert(0, v2);
        app.state.playhead = at(1.0);
        let cmd = make(&mut app, picture);
        redoes_as_made(&mut app, vec![cmd.clone()]);
        assert_eq!(app.state.undo_label().as_deref(), Some(label), "{cmd:?}");
    }

    // Lift and Extract use up the marks in the same step; undone, the
    // marks and the clips are back together. One that changed nothing
    // (every track locked) used nothing up and is no step.
    for (cmd, label) in [(AppCommand::LiftEdit, "Lift"), (AppCommand::ExtractEdit, "Extract")] {
        let (mut app, _engine) = KlippApp::for_test();
        linked_pair(&mut app);
        let marks = |app: &KlippApp| (app.state.sequence().in_point, app.state.sequence().out_point);
        (app.state.sequence_mut().in_point, app.state.sequence_mut().out_point) = (Some(at(1.0)), Some(at(2.0)));
        redoes_as_made(&mut app, vec![cmd.clone()]);
        assert_eq!(app.state.undo_label().as_deref(), Some(label));
        assert_eq!(marks(&app), (None, None), "{label}: the marks used up");
        app.send_commands(vec![AppCommand::Undo]);
        assert_eq!(marks(&app), (Some(at(1.0)), Some(at(2.0))), "{label} undone: the marks back");

        for t in &mut app.state.sequence_mut().tracks {
            t.locked = true;
        }
        let depth = app.state.undo_depth();
        app.send_commands(vec![cmd]);
        assert_eq!(app.state.undo_depth(), depth, "{label}: every track locked, no step");
        assert_eq!(marks(&app), (Some(at(1.0)), Some(at(2.0))), "{label}: nothing used up");
    }
}

/// A caption added or restyled, a transition's kind, a multicam made, a
/// clip replaced from the bin at its In mark and subtitles imported are
/// each one step under their own name, recorded by their handlers; a
/// transition's settings dragged are one step from where the drag began.
#[test]
fn captions_transitions_and_the_rest_are_each_their_own_named_step() {
    use klipp_timeline::TrackKind;
    let at = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    // Two clips end to end on a track of their own on top, a dissolve
    // between them; a caption track with its style drafted.
    let setup = |app: &mut KlippApp| {
        let mut v2 = klipp_timeline::Track::new("Video 2", TrackKind::Video);
        let m = klipp_core::MediaId::new();
        let (a, b) = (klipp_timeline::Clip::media(m, at(0.0), at(4.0), at(0.0), at(4.0)), klipp_timeline::Clip::media(m, at(4.0), at(8.0), at(4.0), at(8.0)));
        v2.transitions.push(klipp_timeline::Transition { id: klipp_core::TransitionId::new(), kind: klipp_core::TransitionKind::CrossDissolve, duration: at(1.0), clip_a_id: a.id, clip_b_id: b.id, user_params: Default::default(), alignment: Default::default() });
        v2.clips = vec![a, b];
        app.state.sequence_mut().tracks.insert(0, v2);
        let id = app.state.sequence_mut().add_subtitle_track("Subtitles".into());
        app.state.subtitle_editor.style_draft = Some((id, Default::default()));
        app.state.playhead = at(1.0);
        (app.state.sequence().tracks[0].transitions[0].id, id)
    };
    let other_kind = (1..=255u8).find(|&k| !matches!(klipp_engine::transition::transition_kind_from_id(k), klipp_core::TransitionKind::CrossDissolve | klipp_core::TransitionKind::SmoothCut)).unwrap();
    type Make = fn(&mut KlippApp, (klipp_core::TransitionId, klipp_core::TrackId), u8) -> AppCommand;
    let cases: Vec<(&str, Make)> = vec![
        ("Add Caption", |_, _, _| AppCommand::AddSubtitleAtPlayhead { timeline_in: klipp_core::RationalTime::from_secs_f64(1.0, 24), duration_frames: 48 }),
        ("Caption Style", |_, (_, track_id), _| AppCommand::SaveCaptionStyle { track_id, apply_to_all: false }),
        ("Transition Kind", |_, (transition_id, _), kind_id| AppCommand::SetTransitionKind { track_index: 0, transition_id, kind_id }),
        ("Transition Settings", |_, (transition_id, _), _| AppCommand::SetTransitionParam { track_index: 0, transition_id, user_params: klipp_core::TransitionUserParams { param0: 0.5, ..Default::default() } }),
        ("Create Multicam", |_, _, _| AppCommand::CreateMulticamClip),
        ("Replace From Bin", |a, _, _| {
            let mut item = klipp_media::MediaItem::synthetic_for_test("/media/other.mov".into(), None);
            item.default_source_in = Some(klipp_core::RationalTime::from_secs_f64(2.0, 24));
            let new_media_id = a.state.document.media_pool.add(item);
            let clip_id = a.state.sequence().tracks[0].clips[0].id;
            AppCommand::ReplaceClipMedia { clip_id, new_media_id }
        }),
    ];
    for (label, make) in cases {
        let (mut app, _engine) = KlippApp::for_test();
        linked_pair(&mut app);
        let ids = setup(&mut app);
        let cmd = make(&mut app, ids, other_kind);
        redoes_as_made(&mut app, vec![cmd.clone()]);
        assert_eq!(app.state.undo_label().as_deref(), Some(label), "{cmd:?}");
    }

    // A slider's run of changes is one step; undone, the drag's start.
    let (mut app, _engine) = KlippApp::for_test();
    let (transition_id, _) = setup(&mut app);
    let depth = app.state.undo_depth();
    let set = |app: &mut KlippApp, v: f32| app.send_commands(vec![AppCommand::SetTransitionParam { track_index: 0, transition_id, user_params: klipp_core::TransitionUserParams { param0: v, ..Default::default() } }]);
    for v in [0.1, 0.2, 0.3] {
        set(&mut app, v);
    }
    assert_eq!(app.state.undo_depth(), depth + 1, "one drag, one step");
    let param = |app: &KlippApp| app.state.sequence().tracks[0].transitions[0].user_params.param0;
    assert_eq!(param(&app), 0.3);
    assert_eq!(app.sequence.snapshot().tracks[0].transitions[0].user_params.param0, 0.3, "the engine's copy at the drag's end");
    app.send_commands(vec![AppCommand::Undo]);
    assert_eq!(param(&app), klipp_core::TransitionUserParams::default().param0, "undone: where the drag began");
    // After an undo, the next change is a step of its own.
    set(&mut app, 0.4);
    assert_eq!(app.state.undo_depth(), depth + 1);

    // Subtitles imported onto a track of their own, one step.
    let (mut app, _engine) = KlippApp::for_test();
    let path = std::env::temp_dir().join(format!("klipp-handler-test-{}.srt", uuid::Uuid::new_v4()));
    std::fs::write(&path, "1\n00:00:01,000 --> 00:00:02,000\nHello\n\n2\n00:00:03,000 --> 00:00:04,000\nThere\n").unwrap();
    redoes_as_made(&mut app, vec![AppCommand::ImportSubtitles { path: path.clone() }]);
    let _ = std::fs::remove_file(&path);
    assert_eq!(app.state.undo_label().as_deref(), Some("Import Subtitles"));
}

/// #1816: a track's height as a header drag commits it stops at the
/// timeline preferences' max track height (250 stops it at 250, 400
/// lets it reach 400) and never goes below the smallest track.
#[test]
fn a_committed_track_height_stops_at_the_preferences_cap() {
    let (mut app, _engine) = KlippApp::for_test();
    let track_id = app.state.sequence().tracks[0].id;
    for (max, asked, got) in [(250.0, 400.0, 250.0), (400.0, 400.0, 400.0), (250.0, 150.0, 150.0), (250.0, 5.0, 32.0)] {
        app.state.timeline_prefs.max_track_height = max;
        app.send_commands(vec![AppCommand::SetTrackCustomHeight { track_id, height: Some(asked) }]);
        assert_eq!(app.state.sequence().tracks[0].custom_height, Some(got), "max {max}, asked {asked}");
    }
    assert_eq!(crate::panels::timeline::track_height_cap(&crate::prefs::TimelinePrefs::default()), 300.0, "the default is the old fixed cap");
}

/// The clip menu's Veylvik actions pass over a clip they can't change and
/// say why, as the control socket says it (both ask
/// `klipp_veylvik::actions`): no composition to reset, the layers below
/// drawn under it already; with several clips, why the first was passed
/// over. One that can be changed is, as one step.
#[test]
fn a_veylvik_action_says_why_it_passes_a_clip_over() {
    use klipp_core::CompositionClipAction as A;
    let (mut app, _engine) = KlippApp::for_test();
    let t = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
    let c = klipp_veylvik::Composition::from_host_footage(klipp_veylvik::CLIP_MEDIA_KEY, "Clip", None, [64, 36], klipp_veylvik::Time { ticks: 24, rate: 1 }, 24).unwrap();
    let plain: std::sync::Arc<[u8]> = serde_json::to_vec(&c).unwrap().into();
    let (over_id, composed_id) = (klipp_timeline::CompositionId::new(), klipp_timeline::CompositionId::new());
    app.state.compositions.insert(over_id, klipp_veylvik::below::over_below(&plain).unwrap());
    app.state.compositions.insert(composed_id, plain);
    let bare = klipp_timeline::Clip::media(klipp_core::MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0));
    let over = klipp_timeline::Clip { composition: Some(over_id), ..klipp_timeline::Clip::media(klipp_core::MediaId::new(), t(2.0), t(4.0), t(0.0), t(2.0)) };
    let composed = klipp_timeline::Clip { composition: Some(composed_id), ..klipp_timeline::Clip::media(klipp_core::MediaId::new(), t(4.0), t(6.0), t(0.0), t(2.0)) };
    let gap = klipp_timeline::Clip::gap(t(6.0), t(8.0));
    let ids = [bare.id, over.id, composed.id, gap.id];
    app.state.document.sequences[0].tracks[0].clips = vec![bare.clone(), over.clone(), composed, gap];
    let said = |app: &KlippApp, clip: &klipp_timeline::Clip, action| klipp_veylvik::actions::passes_over(action, clip, &app.state.compositions).unwrap().to_string();

    assert_eq!(app.composition_clips(&ids[..1], A::Reset), Err(said(&app, &bare, A::Reset)));
    assert_eq!(app.composition_clips(&ids[1..2], A::OverBelow), Err(said(&app, &over, A::OverBelow)));
    let both = app.composition_clips(&[ids[0], ids[3]], A::Remove).unwrap_err();
    assert_eq!(both, format!("none of the clips was changed: {}", said(&app, &bare, A::Remove)));
    assert_eq!(app.state.undo_depth(), 0, "nothing changed, no step");

    assert!(app.composition_clips(&ids, A::Remove).is_ok(), "the two with one are changed");
    assert_eq!(app.state.undo_depth(), 1);
    let left = app.state.document.sequences[0].tracks[0].clips.iter().filter(|c| c.composition.is_some()).count();
    assert_eq!(left, 0, "both compositions removed, the others passed over");
}
