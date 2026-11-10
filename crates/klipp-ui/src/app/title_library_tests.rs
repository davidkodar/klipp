//! The built-in title library applied as a drop on the timeline applies
//! it (#1724): every template in one drag, its fields edited after.

use super::titles::{add_title_clip, after_title_added, free_text_track};
use crate::browser::titles_tab::{Entry, TITLES};
use crate::state::AppState;
use klipp_core::TitleFieldValue as V;

fn title(state: &AppState, id: klipp_core::ClipId) -> klipp_timeline::TitleData {
    state.sequence().all_clips().find(|c| c.id == id).and_then(|c| c.title_data()).cloned().expect("the title")
}

fn drawn(r: &mut klipp_title::TitleRenderer, td: &klipp_timeline::TitleData) -> usize {
    r.render_with_window(td, 320, 180, None, 2.0, Some(5.0)).chunks_exact(4).filter(|p| p[3] > 8).count()
}

/// Every built-in template, dropped as the timeline drops a dragged
/// title (pending, then the clip `CreateTitleClip` makes), lands as one
/// undo step, marked as made from it and drawn; each of its fields then
/// takes a new value and reads it back, and the title is still drawn.
#[test]
fn every_built_in_template_applies_in_one_drag_and_its_fields_edit() {
    let mut r = klipp_title::TitleRenderer::new();
    for tpl in TITLES.iter() {
        let mut state = AppState { pending_title_data: Some(Entry::Builtin(tpl).data()), ..Default::default() };
        let frames = state.sequence().fps.rate() * 5;
        let at = state.playhead;
        let track = free_text_track(&mut state, at, frames);
        let (id, _) = add_title_clip(&mut state, track, at, frames);
        assert_eq!(state.active_history().undo_count(), 1, "{}: one step", tpl.name);
        let td = title(&state, id);
        assert_eq!(td.template.as_deref(), Some(tpl.name.as_str()), "{}", tpl.name);
        assert!(drawn(&mut r, &td) > 50, "{}: drawn once applied", tpl.name);
        let fields = td.live_fields();
        assert!(!fields.is_empty(), "{}: fields to edit", tpl.name);
        for (index, field, value) in fields.into_iter().map(|(i, f, v)| (i, f.label.clone(), v)) {
            let new = match value {
                V::Text(_) => V::Text("Edited".into()),
                V::Colour(_) => V::Colour([220, 60, 40, 255]),
                V::Speed(_) => V::Speed(2.0),
                V::Size(_) => V::Size(1.5),
                V::Font(_) => V::Font("Inter".into()),
                V::Duration(_) => V::Duration(6.0),
                V::Picture(p) => V::Picture(p),
            };
            state.set_title_field(id, index, &new, true).unwrap_or_else(|e| panic!("{}: {field}: {e}", tpl.name));
            assert_eq!(title(&state, id).field_value(index), Some(new), "{}: {field}", tpl.name);
        }
        assert!(state.active_history().undo_count() > 1, "{}: the edits are steps", tpl.name);
        assert!(drawn(&mut r, &title(&state, id)) > 50, "{}: still drawn after its fields changed", tpl.name);
    }
}

/// A title the app makes, from a template or the Text tool's default,
/// keeps its text readable over the picture (#2734).
#[test]
fn a_new_title_protects_its_contrast() {
    // A template on a backing of its own reads on it and keeps no halo
    // (#2840): the one checked here is one without.
    let backed = |t: &klipp_timeline::title_library::TitleTemplate| t.data().protect == klipp_timeline::ContrastProtect::Off;
    assert!(TITLES.iter().any(backed), "some template has a backing, and keeps no halo");
    let tpl = TITLES.iter().find(|t| !backed(t)).expect("a template without a backing");
    let mut state = AppState { pending_title_data: Some(Entry::Builtin(tpl).data()), ..Default::default() };
    let frames = state.sequence().fps.rate() * 5;
    let at = state.playhead;
    let track = free_text_track(&mut state, at, frames);
    let (id, _) = add_title_clip(&mut state, track, at, frames);
    assert_eq!(title(&state, id).protect, klipp_timeline::ContrastProtect::Auto, "{}", tpl.name);
    let td = super::titles::default_title_data(&crate::prefs::TitlePrefs::default());
    assert_eq!(td.protect, klipp_timeline::ContrastProtect::Auto);
}

/// A built-in dropped on a 9:16 sequence is laid out for it (#2734): its
/// name comes up out of the band at the bottom that phones cover.
#[test]
fn a_template_dropped_on_a_tall_sequence_is_laid_out_for_it() {
    let tpl = TITLES.iter().find(|t| t.name == "Name Bar").unwrap();
    let mut state = AppState { pending_title_data: Some(Entry::Builtin(tpl).data()), ..Default::default() };
    let i = state.active_sequence_idx;
    state.sequences[i].resolution = klipp_core::Resolution { width: 1080, height: 1920 };
    let frames = state.sequence().fps.rate() * 5;
    let at = state.playhead;
    let track = free_text_track(&mut state, at, frames);
    let (id, _) = add_title_clip(&mut state, track, at, frames);
    let td = title(&state, id);
    let y = td.objects.iter().find(|o| o.name == "Name").unwrap().transform.center.y;
    assert!(y < klipp_timeline::title_library::TALL_BAND_TOP - 0.05, "the name at {y}");
    assert_eq!(td.template.as_deref(), Some("Name Bar"), "still marked as made from it");
}

/// A title added from a template with an entrance leaves the playhead
/// where it is: nothing that follows the add seeks. It was moved a
/// second on, to where the title rests, unasked.
#[test]
fn adding_a_title_leaves_the_playhead_where_it_is() {
    let tpl = TITLES.iter().find(|t| t.name == "Accent Block").expect("Accent Block");
    let mut state = AppState { pending_title_data: Some(Entry::Builtin(tpl).data()), ..Default::default() };
    state.playhead = klipp_core::Timecode::from_frames(48, state.sequence().fps.rate());
    let frames = state.sequence().fps.rate() * 5;
    let at = state.playhead;
    let track = free_text_track(&mut state, at, frames);
    let (id, rest) = add_title_clip(&mut state, track, at, frames);
    assert!(rest.is_some_and(|r| r != at), "the template has an entrance to rest past");
    assert_eq!(state.playhead, at, "the add itself leaves the playhead");
    let follows = after_title_added(&state, id);
    assert!(!follows.iter().any(|c| matches!(c, klipp_core::AppCommand::SeekTo(_))), "nothing after it seeks: {follows:?}");
}

/// A title added has the frame at the playhead drawn again, with it in.
#[test]
fn adding_a_title_redraws_the_frame_at_the_playhead() {
    let tpl = TITLES.iter().find(|t| t.name == "Accent Block").expect("Accent Block");
    let mut state = AppState { pending_title_data: Some(Entry::Builtin(tpl).data()), ..Default::default() };
    let frames = state.sequence().fps.rate() * 5;
    let at = state.playhead;
    let track = free_text_track(&mut state, at, frames);
    let (id, _) = add_title_clip(&mut state, track, at, frames);
    let follows = after_title_added(&state, id);
    assert!(follows.iter().any(|c| matches!(c, klipp_core::AppCommand::SequenceUpdated)), "{follows:?}");
}


/// Centred lines with tracking stay centred on each other: the first line
/// grew only to the left and the second only to the right.
#[test]
fn tracked_centred_lines_stay_centred() {
    let mut r = klipp_title::TitleRenderer::new();
    let mut td = super::titles::default_title_data(&crate::prefs::TitlePrefs::default());
    {
        let tb = td.primary_text_mut().unwrap();
        tb.text = "Harbour Light\nEvening Tide".into();
        tb.alignment = klipp_timeline::TextAlignment::Center;
        tb.tracking = 20.0;
    }
    let (w, h) = (1280u32, 720u32);
    let px = r.render_with_window(&td, w, h, None, 2.0, Some(5.0));
    let ink = |row: u32| (0..w).filter(|&x| px[((row * w + x) * 4 + 3) as usize] > 8).collect::<Vec<_>>();
    // The rows with ink, split into lines at the first empty band.
    let rows: Vec<u32> = (0..h).filter(|&y| !ink(y).is_empty()).collect();
    let split = rows.windows(2).position(|w| w[1] > w[0] + 2).expect("two lines");
    let span = |ys: &[u32]| {
        let xs: Vec<u32> = ys.iter().flat_map(|&y| ink(y)).collect();
        (*xs.iter().min().unwrap(), *xs.iter().max().unwrap())
    };
    let (a, b) = (span(&rows[..=split]), span(&rows[split + 1..]));
    let centre = |s: (u32, u32)| (s.0 + s.1) as f32 / 2.0;
    assert!((centre(a) - centre(b)).abs() < 6.0, "line 1 {a:?}, line 2 {b:?}: centres {} and {}", centre(a), centre(b));
}
