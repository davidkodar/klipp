//! A sound-only clip placed under the picture ends with it (#2983).

use super::music_trim::{trim_edits, undo_step_is_last, FADE_OUT_SECS, LABEL};
use crate::clip_clipboard::PasteMode;
use crate::state::AppState;
use klipp_core::{ClipId, MediaId, RationalTime};
use klipp_document::apply::DocumentView;
use klipp_timeline::{Clip, TrackKind};

fn tc(secs: f64) -> RationalTime {
    RationalTime::from_secs_f64(secs, 25)
}

/// A project with a 3-minute song in its pool (sound only, at 44.1 kHz)
/// and, with `picture`, a 20 s cut on its video track.
fn project(picture: bool) -> (AppState, MediaId) {
    let mut state = AppState::default();
    let mut song = klipp_media::MediaItem::synthetic_for_test("/tmp/song.mp3".into(), None);
    song.video_codec = String::new();
    song.has_audio = true;
    song.duration = RationalTime::from_secs_f64(180.0, 44_100);
    let song = state.media_pool.add(song);
    if picture {
        let v = state.sequences[0].tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap();
        state.sequences[0].tracks[v].clips.push(Clip::media(MediaId::new(), tc(0.0), tc(20.0), tc(0.0), tc(20.0)));
    }
    (state, song)
}

/// The song dropped on the audio track at `at`, as a drop places it, one
/// step: the clip.
fn drop_song(state: &mut AppState, song: MediaId, at: f64) -> ClipId {
    let item = state.media_pool.get(&song).unwrap().clone();
    let start = RationalTime { value: 0, rate: item.duration.rate };
    let clip = Clip::media(song, start, item.duration, start, item.duration);
    let a = state.sequences[0].tracks.iter().position(|t| t.kind == TrackKind::Audio).unwrap();
    let (edits, id) = super::placement::media_drop_edits(state.sequence(), clip, a, None, tc(at), PasteMode::Overwrite, "Overwrite").unwrap();
    apply(state, &edits, "Overwrite");
    id
}

fn apply(state: &mut AppState, edits: &[crate::history::EditCommand], label: &str) {
    let active = state.active_sequence_idx;
    let mut view = DocumentView { sequences: &mut state.sequences, active, media_pool: &mut state.media_pool, fps: &mut state.fps };
    for e in edits {
        view.apply_forward(e);
    }
    state.push_edits_as_one(edits, label);
}

fn clip(state: &AppState, id: ClipId) -> Clip {
    state.sequence().all_clips().find(|c| c.id == id).unwrap().clone()
}

/// Where the timeline ends, seconds.
fn length(state: &AppState) -> f64 {
    state.sequence().all_clips().map(|c| c.timeline_out.to_secs_f64()).fold(0.0, f64::max)
}

/// A 3-minute song under a 20 s cut: trimmed, as a step of its own, the
/// timeline stays 20 s, the song ends at 20 s of itself with a fade, and
/// undoing that one step gives its full length back.
#[test]
fn a_song_under_a_short_cut_ends_with_the_picture() {
    let (mut state, song) = project(true);
    let id = drop_song(&mut state, song, 0.0);
    assert!((length(&state) - 180.0).abs() < 0.01, "placed at full length first: {}", length(&state));

    let edits = trim_edits(state.sequence(), id).expect("trimmed");
    apply(&mut state, &edits, LABEL);
    let stamp = state.active_history().undo_stamped().last().map(|(s, _)| s).unwrap();
    assert_eq!(state.undo_label().as_deref(), Some(LABEL));
    assert!((length(&state) - 20.0).abs() < 0.01, "the timeline stays 20 s: {}", length(&state));
    let c = clip(&state, id);
    assert!((c.timeline_out.to_secs_f64() - 20.0).abs() < 0.01 && (c.source_out.to_secs_f64() - 20.0).abs() < 0.01, "out at 20 s: {:?} {:?}", c.timeline_out, c.source_out);
    assert_eq!(c.fade_out_secs, FADE_OUT_SECS, "a fade-out of about a second");

    // "Undo trim": that step, while it is the last.
    assert!(undo_step_is_last(&state, stamp));
    let entry = state.take_undo().unwrap();
    let active = state.active_sequence_idx;
    DocumentView { sequences: &mut state.sequences, active, media_pool: &mut state.media_pool, fps: &mut state.fps }.apply_inverse(&entry.command);
    let c = clip(&state, id);
    assert!((c.timeline_out.to_secs_f64() - 180.0).abs() < 0.01 && c.fade_out_secs == 0.0, "full length back, no fade: {:?}", c.timeline_out);
    assert!(!undo_step_is_last(&state, stamp), "and the button does nothing more");
}

/// With no picture, or the song placed past the picture's end, nothing is
/// trimmed.
#[test]
fn nothing_is_trimmed_without_picture_above() {
    let (mut state, song) = project(false);
    let id = drop_song(&mut state, song, 0.0);
    assert!(trim_edits(state.sequence(), id).is_none(), "no picture");
    assert!((clip(&state, id).timeline_out.to_secs_f64() - 180.0).abs() < 0.01, "full length kept");

    let (mut state, song) = project(true);
    let id = drop_song(&mut state, song, 25.0);
    assert!(trim_edits(state.sequence(), id).is_none(), "placed past the picture's end");
}

/// The note says what happened and offers the undo: its button, clicked,
/// is the undo of that step.
#[test]
fn the_note_offers_undo_trim() {
    use crate::widgets::toast::{ToastAction, ToastState};
    let mut toast = ToastState::default();
    toast.visible_duration = 3.0;
    toast.show_with_action(super::music_trim::note("Music"), "Undo trim", ToastAction::UndoStep(7));
    let ctx = egui::Context::default();
    let theme = crate::theme::KlippTheme::default();
    let run = |events: Vec<egui::Event>| {
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0))), events, ..Default::default() };
        let mut clicked = None;
        let out = ctx.run(input, |ctx| clicked = toast.render(ctx, &theme, None));
        let texts: Vec<(String, egui::Pos2)> = out.shapes.iter().filter_map(|s| match &s.shape {
            egui::Shape::Text(t) => Some((t.galley.text().to_owned(), t.pos + t.galley.size() / 2.0)),
            _ => None,
        }).collect();
        (texts, clicked)
    };
    run(Vec::new());
    let (texts, _) = run(Vec::new());
    assert!(texts.iter().any(|(t, _)| t == "Music trimmed to the end of the picture (full length available)"), "{texts:?}");
    let at = texts.iter().find(|(t, _)| t == "Undo trim").map(|(_, p)| *p).expect("Undo trim is offered");
    let b = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    let mut clicked = run(vec![egui::Event::PointerMoved(at), b(true)]).1;
    clicked = clicked.or(run(vec![b(false)]).1);
    assert_eq!(clicked, Some(ToastAction::UndoStep(7)));
}
