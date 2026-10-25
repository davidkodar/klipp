//! #2633: an edit that made a track for what it put down is undone with
//! the track. Each path below makes its track and records its step the
//! way its handler does; then the step undone gives the timeline it
//! started from, field for field, and redone the one it made.

use super::linked_audio::attach_linked_audio_to_clip;
use crate::history::EditCommand;
use crate::state::AppState;
use klipp_core::{MediaId, RationalTime};
use klipp_timeline::magnetic::MagneticEditor;
use klipp_timeline::{Clip, ClipKind, Sequence, TrackKind};

fn t(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 24)
}

/// A state whose timeline has one clip on its primary video track and
/// no audio track; its tracks as recorded, as after an open.
fn state() -> AppState {
    let mut s = AppState::default();
    let seq = &mut s.sequences[0];
    seq.tracks.retain(|tr| tr.kind != TrackKind::Audio);
    let primary = seq.primary_track_index().expect("a primary track");
    seq.tracks[primary].clips.push(Clip::media(MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0)));
    s.mark_saved();
    s
}

fn timeline(s: &AppState) -> serde_json::Value {
    serde_json::to_value(s.sequence()).unwrap()
}

/// The timeline with its tracks' ids left out: a track an import made
/// again on redo is a new one, the same in all else.
fn contents(s: &AppState) -> serde_json::Value {
    let mut v = timeline(s);
    for track in v["tracks"].as_array_mut().into_iter().flatten() {
        track["id"] = serde_json::Value::Null;
    }
    v
}

fn first_diff(a: &serde_json::Value, b: &serde_json::Value, path: String) -> Option<String> {
    use serde_json::Value::*;
    match (a, b) {
        (Object(x), Object(y)) => { for k in x.keys().chain(y.keys()) { if let Some(d) = first_diff(x.get(k).unwrap_or(&Null), y.get(k).unwrap_or(&Null), format!("{path}.{k}")) { return Some(d); } } None }
        (Array(x), Array(y)) => { if x.len() != y.len() { return Some(format!("{path}: len {} vs {}", x.len(), y.len())); } for (i, (p, q)) in x.iter().zip(y).enumerate() { if let Some(d) = first_diff(p, q, format!("{path}[{i}]")) { return Some(d); } } None }
        _ => (a != b).then(|| format!("{path}: {a} vs {b}")),
    }
}

fn undo(s: &mut AppState) {
    let entry = s.take_undo().expect("a step to undo");
    let active = s.active_sequence_idx;
    klipp_document::apply::DocumentView { sequences: &mut s.sequences, active, media_pool: &mut s.media_pool, fps: &mut s.fps }.apply_inverse(&entry.command);
    s.note_recorded_tracks();
}

fn redo(s: &mut AppState) {
    let entry = s.take_redo().expect("a step to redo");
    let active = s.active_sequence_idx;
    klipp_document::apply::DocumentView { sequences: &mut s.sequences, active, media_pool: &mut s.media_pool, fps: &mut s.fps }.apply_forward(&entry.command);
    s.note_recorded_tracks();
}

/// `edit`, which makes its track and pushes its step, then undone and
/// redone: the timeline before it, then after it.
fn round_trip(path: &str, edit: impl FnOnce(&mut AppState)) {
    let mut s = state();
    let before = timeline(&s);
    let tracks = s.sequence().tracks.len();
    edit(&mut s);
    let after = contents(&s);
    assert!(s.sequence().tracks.len() > tracks, "{path}: the edit made a track");
    undo(&mut s);
    assert_eq!(timeline(&s), before, "{path}: undone, the timeline as it was, its new track gone: {:?}", first_diff(&timeline(&s), &before, String::new()));
    redo(&mut s);
    assert_eq!(contents(&s), after, "{path}: redone, the timeline the edit made: {:?}", first_diff(&contents(&s), &after, String::new()));
    s.dirty = true;
    s.settle_unsaved();
    assert!(s.unsaved);
    undo(&mut s);
    s.dirty = true;
    s.settle_unsaved();
    assert!(!s.unsaved, "{path}: undone, the project as saved");
}

/// The issue's case: appended to the primary track with the file's sound
/// linked under it, on an audio track made for it, recorded as the
/// magnetic append and the audio track's clips.
#[test]
fn an_append_that_made_an_audio_track() {
    round_trip("append", |s| {
        let media = MediaId::new();
        let result = MagneticEditor::append_primary(s.sequence_mut(), Clip::media(media, t(0.0), t(0.0), t(0.0), t(3.0)));
        let klipp_timeline::magnetic_commands::MagneticEditCommand::AppendPrimary { ref appended_clip, .. } = result.command else { panic!("an append") };
        let audio = attach_linked_audio_to_clip(s.sequence_mut(), appended_clip.id, None).expect("its sound");
        assert!(audio.created_track_at.is_some(), "on a track made for it");
        // As the append records it: the clip linked to its sound.
        let mut video_command = result.command;
        if let klipp_timeline::magnetic_commands::MagneticEditCommand::AppendPrimary { ref mut appended_clip, .. } = video_command {
            appended_clip.linked_clip_id = Some(audio.clip.id);
        }
        s.push_history(EditCommand::SourceLinkedInsert {
            video_command,
            audio_track_index: audio.track_index,
            audio_clips_before: audio.clips_before,
            audio_clips_after: audio.clips_after,
        });
    });
}

/// The source's Insert onto the primary track (`apply_source_magnetic_edit`):
/// the audio track made before the magnetic edit, the clips linked by ids
/// made first, recorded as the magnetic insert and the audio track's clips.
#[test]
fn an_insert_that_made_an_audio_track() {
    round_trip("insert", |s| {
        let media = MediaId::new();
        let mut video = Clip::media(media, t(4.0), t(6.0), t(0.0), t(2.0));
        let aid = klipp_core::ClipId::new();
        video.linked_clip_id = Some(aid);
        let vid = video.id;
        s.sequence_mut().add_audio_track("Audio 1".into());
        let ai = s.sequence().tracks.iter().position(|tr| tr.kind == TrackKind::Audio).unwrap();
        let audio_clips_before = s.sequence().tracks[ai].clips.clone();
        // At the cut: undoing an insert that splits a clip leaves a piece
        // behind whether or not it made a track, and is not this.
        let result = MagneticEditor::insert_primary(s.sequence_mut(), video, t(4.0), None, None);
        let mut audio = Clip::media(media, t(4.0), t(6.0), t(0.0), t(2.0));
        (audio.id, audio.linked_clip_id) = (aid, Some(vid));
        super::KlippApp::overwrite_insert_clip_on_track(&mut s.sequence_mut().tracks[ai], audio);
        let audio_clips_after = s.sequence().tracks[ai].clips.clone();
        s.push_history(EditCommand::SourceLinkedInsert { video_command: result.command, audio_track_index: ai, audio_clips_before, audio_clips_after });
    });
}

/// A clip dropped below the last track: the track made there, then the
/// clip appended to it, recorded as the append alone ("Append Clip").
#[test]
fn a_drop_below_the_last_track() {
    round_trip("drop below", |s| {
        s.sequence_mut().add_video_track("Video 2".into());
        let clip_id = s.sequence_mut().tracks[0].append_clip(MediaId::new(), t(3.0));
        let clip = s.sequence().tracks[0].find_clip(clip_id).unwrap().clone();
        s.push_history(EditCommand::LegacyAppendClip { clip, track_index: 0 });
    });
}

/// A title dropped on a video track goes on a text track made at the
/// top, recorded as the title appended.
#[test]
fn a_title_on_a_new_text_track() {
    round_trip("title", |s| {
        s.sequence_mut().tracks.insert(0, klipp_timeline::Track::new("Text 1", TrackKind::Text));
        let td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Hello".into(), ..Default::default() });
        let clip = Clip { kind: ClipKind::Title(td), source_in: t(0.0), source_out: t(5.0), ..Clip::gap(t(0.0), t(5.0)) };
        s.sequence_mut().tracks[0].clips.push(clip.clone());
        s.push_history(EditCommand::LegacyAppendClip { clip, track_index: 0 });
    });
}

/// Steps that take their own track away are left as they are: an import
/// onto new tracks (the playhead placement and multicam's), and a pool
/// drop that records its audio track.
#[test]
fn a_step_that_records_its_own_track_is_not_given_another() {
    round_trip("import", |s| {
        let seq = s.sequence_mut();
        seq.add_audio_track("Audio 1".into());
        let ai = seq.tracks.iter().position(|tr| tr.kind == TrackKind::Audio).unwrap();
        let audio = Clip::media(MediaId::new(), t(4.0), t(6.0), t(0.0), t(2.0));
        seq.tracks[ai].clips.push(audio.clone());
        s.push_history(EditCommand::LinkedImport { video: None, audio: Some((audio, ai)), created_video_track_at: None, created_audio_track_at: Some(ai), autoset: None });
        let top = s.active_history().undo_stamped().last().map(|(_, e)| e.command.clone());
        assert!(matches!(top, Some(EditCommand::LinkedImport { .. })), "recorded as it was: {top:?}");
    });
    round_trip("pool drop", |s| {
        let seq = s.sequence().clone();
        let primary = seq.primary_track_index().unwrap();
        let clip = Clip::media(MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0));
        let (edits, _) = super::placement::media_drop_edits(&seq, clip, primary, Some(None), t(4.0), crate::clip_clipboard::PasteMode::Insert, "Insert").unwrap();
        let active = s.active_sequence_idx;
        let mut view = klipp_document::apply::DocumentView { sequences: &mut s.sequences, active, media_pool: &mut s.media_pool, fps: &mut s.fps };
        for e in &edits {
            view.apply_forward(e);
        }
        s.push_edits_as_one(&edits, "Insert");
        let adds = |c: &EditCommand| -> usize {
            fn count(c: &EditCommand) -> usize {
                match c {
                    EditCommand::AddTrack { .. } => 1,
                    EditCommand::Batch(cs) => cs.iter().map(count).sum(),
                    EditCommand::Named { edit, .. } => count(edit),
                    _ => 0,
                }
            }
            count(c)
        };
        let top = s.active_history().undo_stamped().last().map(|(_, e)| e.command.clone()).unwrap();
        assert_eq!(adds(&top), 1, "its own added track, not a second");
    });
}

/// A track made with no step of its own, before an edit elsewhere on the
/// timeline, still goes with that edit: nothing is left behind empty.
#[test]
fn a_sequence_never_seen_records_from_its_first_frame() {
    let mut s = state();
    s.recorded_tracks.clear();
    s.seed_recorded_tracks();
    let before = timeline(&s);
    s.sequence_mut().add_video_track("Video 2".into());
    let clip_id = s.sequence_mut().tracks[0].append_clip(MediaId::new(), t(2.0));
    let clip = s.sequence().tracks[0].find_clip(clip_id).unwrap().clone();
    s.push_history(EditCommand::LegacyAppendClip { clip, track_index: 0 });
    undo(&mut s);
    assert_eq!(timeline(&s), before);
    let _: &Sequence = s.sequence();
}

