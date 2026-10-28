//! Which clip the Colour page grades (#2952): the playhead stays where it
//! is on the switch, a selected clip among those stacked at the playhead
//! is the one graded, and a selection elsewhere is named with "Go to it".

use crate::state::{AppState, Workspace};
use klipp_core::{ClipId, RationalTime};

fn tc(s: f64) -> RationalTime {
    RationalTime::from_secs_f64(s, 25)
}

/// V1 holding `lower` (0–2 s) and `later` (4–6 s), V2 above holding
/// `upper` (0–2 s); the playhead at 1 s, on the Edit page.
struct Cut {
    state: AppState,
    lower: ClipId,
    upper: ClipId,
    later: ClipId,
}

fn cut() -> Cut {
    let mut state = AppState::default();
    let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/interview.mp4".into(), None);
    item.filename = "interview.mp4".into();
    let media = state.media_pool.add(item);
    let seq = &mut state.sequences[0];
    seq.add_video_track("V2".into());
    let video: Vec<usize> = seq.tracks.iter().enumerate().filter(|(_, t)| t.kind == klipp_timeline::TrackKind::Video).map(|(i, _)| i).collect();
    let (v2, v1) = (video[0], video[1]);
    let clip = |a, b| klipp_timeline::Clip::media(media, tc(a), tc(b), tc(0.0), tc(b - a));
    let (lower, upper, later) = (clip(0.0, 2.0), clip(0.0, 2.0), clip(4.0, 6.0));
    let ids = (lower.id, upper.id, later.id);
    seq.tracks[v1].clips = vec![lower, later];
    seq.tracks[v2].clips = vec![upper];
    state.playhead = tc(1.0);
    Cut { state, lower: ids.0, upper: ids.1, later: ids.2 }
}

/// The page switched to, as the app's follow takes it.
fn to_colour_page(state: &mut AppState) -> Option<ClipId> {
    state.active_workspace = Workspace::Color;
    state.grade_at_playhead()
}

/// The switch leaves the playhead where it was, whatever is selected: a
/// clip elsewhere, one under another track, or none.
#[test]
fn switching_to_the_colour_page_leaves_the_playhead() {
    for pick in 0..3 {
        let Cut { mut state, lower, later, .. } = cut();
        let selected = [Some(later), Some(lower), None][pick];
        state.selected_clip = selected;
        to_colour_page(&mut state);
        assert_eq!(state.playhead, tc(1.0), "{selected:?} selected");
    }
}

/// Two clips stacked at the playhead and the lower one selected: the page
/// grades the lower one, and its header names it and its track.
#[test]
fn the_selected_clip_among_stacked_ones_is_graded() {
    let Cut { mut state, lower, upper, .. } = cut();
    state.selected_clip = Some(lower);
    assert_eq!(to_colour_page(&mut state), Some(lower));
    assert_eq!(state.selected_clip, Some(lower));
    assert_eq!(state.grading_title(lower), "Grading: interview.mp4 · V1");
    assert_eq!(state.colour_elsewhere_hint(), None, "the selection is the clip graded");
    // Nothing selected: the top one, as the viewer shows.
    state.selected_clip = None;
    state.playhead = tc(1.5);
    assert_eq!(state.grade_at_playhead(), Some(upper));
    assert_eq!(state.grading_title(upper), "Grading: interview.mp4 · V2");
}

/// The selection elsewhere: the page grades the clip at the playhead, the
/// header says the selection is elsewhere, and "Go to it", clicked, moves
/// the playhead onto it and grades it; nothing moves before the click.
#[test]
fn a_selection_elsewhere_is_one_click_away() {
    let Cut { mut state, upper, later, .. } = cut();
    state.selected_clip = Some(later);
    assert_eq!(to_colour_page(&mut state), Some(upper));
    assert_eq!(state.colour_elsewhere_hint(), Some(later));

    let ctx = egui::Context::default();
    let theme = crate::theme::KlippTheme::default();
    let frame = |events: Vec<egui::Event>, t: f64| {
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(360.0, 600.0))), time: Some(t), events, ..Default::default() };
        let mut clicked = false;
        let out = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| clicked = super::colour_target::elsewhere_hint(ui, &theme));
        });
        let texts: Vec<(String, egui::Pos2)> = out.shapes.iter().filter_map(|s| match &s.shape {
            egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)),
            _ => None,
        }).collect();
        (clicked, texts)
    };
    let (_, texts) = frame(Vec::new(), 0.0);
    assert!(texts.iter().any(|(t, _)| t == "Selected clip is elsewhere —"), "{texts:?}");
    let at = texts.iter().find(|(t, _)| t == "Go to it").map(|(_, p)| *p).expect("Go to it is drawn");
    assert_eq!(state.playhead, tc(1.0), "the hint alone moves nothing");
    let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    let mut clicked = frame(vec![egui::Event::PointerMoved(at)], 0.1).0;
    clicked |= frame(vec![egui::Event::PointerMoved(at), button(true)], 0.2).0;
    clicked |= frame(vec![egui::Event::PointerMoved(at), button(false)], 0.3).0;
    clicked |= frame(vec![egui::Event::PointerGone], 0.4).0;
    assert!(clicked, "Go to it clicked");

    assert_eq!(state.go_to_for_grading(later, None), Some(tc(4.0)));
    assert_eq!((state.playhead, state.selected_clip), (tc(4.0), Some(later)));
    assert_eq!(state.grade_at_playhead(), Some(later), "the page grades it there");
    assert_eq!(state.colour_elsewhere_hint(), None);
}
