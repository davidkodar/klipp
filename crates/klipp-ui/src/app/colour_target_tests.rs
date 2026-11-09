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
            egui::CentralPanel::default().show(ctx, |ui| clicked = super::colour_target::elsewhere_row(ui, &theme, true));
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

/// A clip picked on the Colour page away from the playhead leaves the
/// playhead where it is: the pick is named in the header with "Go to it"
/// instead. The pick took the playhead to the clip's first frame.
#[test]
fn a_pick_elsewhere_on_the_colour_page_leaves_the_playhead() {
    let Cut { mut state, upper, later, .. } = cut();
    assert_eq!(to_colour_page(&mut state), Some(upper));
    // As the timeline's pick does: selected, and the page's choice.
    state.selected_clip = Some(later);
    state.colour_clip_picked();
    assert_eq!(state.seek_on_select(later), None, "the pick moves no playhead");
    assert_eq!(state.playhead, tc(1.0));
    assert_eq!(state.colour_elsewhere_hint(), Some(later), "it is offered with Go to it");
}

/// A click on another clip while the playhead is on one: the page goes on
/// grading the clip at the playhead, names it in the header, and offers
/// the pick with "Go to it". The header said "Grading:" the clicked one,
/// while the viewer showed the clip at the playhead.
#[test]
fn a_click_elsewhere_keeps_the_page_on_the_clip_at_the_playhead() {
    let Cut { mut state, upper, later, .. } = cut();
    let here = Some(state.playhead);
    assert_eq!(to_colour_page(&mut state), Some(upper));
    assert!(!state.grade_needs_following(here), "settled");
    // The click: selected, and the page's choice.
    state.selected_clip = Some(later);
    state.colour_clip_picked();
    assert!(state.grade_needs_following(here), "a pick away from the playhead is looked at again");
    assert_eq!(state.grade_at_playhead(), Some(upper));
    assert_eq!(state.selected_clip, Some(upper));
    assert_eq!(state.grading_title(state.selected_clip.unwrap()), state.grading_title(upper), "the header names the clip at the playhead");
    assert_eq!(state.colour_elsewhere_hint(), Some(later), "the pick is offered with Go to it");
    assert_eq!(Some(state.playhead), here, "the playhead stays");
    assert!(!state.grade_needs_following(here), "settled again");
}

/// #3426: the page chose the clip at the playhead, with another selected.
/// A click on the chosen clip changes no selection, and is still the
/// choice that ends the hint.
#[test]
fn a_click_on_the_graded_clip_clears_the_hint() {
    let Cut { mut state, upper, later, .. } = cut();
    state.selected_clip = Some(later);
    assert_eq!(to_colour_page(&mut state), Some(upper), "the top clip at the playhead");
    assert_eq!(state.colour_elsewhere_hint(), Some(later));
    state.colour_clip_picked();
    assert_eq!(state.colour_elsewhere_hint(), None);
}

/// Picking on another page leaves what the Colour page remembers.
#[test]
fn a_pick_on_another_page_keeps_the_hint() {
    let Cut { mut state, later, .. } = cut();
    state.selected_clip = Some(later);
    to_colour_page(&mut state);
    state.active_workspace = Workspace::Edit;
    state.colour_clip_picked();
    assert_eq!(state.colour_elsewhere, Some(later));
}

/// Escape leaves nothing selected with the playhead where it was: the page
/// looks again and goes on grading the clip there, and a selection, of one
/// clip or of several, stays until the playhead moves.
#[test]
fn escape_keeps_the_page_on_the_clip_at_the_playhead() {
    let Cut { mut state, upper, later, .. } = cut();
    let here = Some(state.playhead);
    state.selected_clip = None;
    state.selected_clips.clear();
    assert!(state.grade_needs_following(here));
    assert_eq!(state.grade_at_playhead(), Some(upper));
    assert_eq!(state.selected_clip, Some(upper));
    assert!(!state.grade_needs_following(here), "settled");
    state.selected_clip = None;
    state.selected_clips = vec![upper, later];
    assert!(!state.grade_needs_following(here), "several selected are left alone");
    assert!(state.grade_needs_following(None), "the page was just opened");
}

/// Ctrl+click on a clip away from the playhead adds it to a selection of
/// several, as the timeline makes it: the page goes on grading the clip at
/// the playhead and keeps the several, so Apply Grade from Still and Looks
/// reach them. A single click away is still named with "Go to it".
#[test]
fn a_ctrl_click_selection_away_from_the_playhead_is_kept() {
    let Cut { mut state, upper, later, .. } = cut();
    assert_eq!(to_colour_page(&mut state), Some(upper));
    let here = Some(state.playhead);
    // The timeline's Ctrl+click on `later`: the primary seeded into the
    // set, `later` added and made the primary.
    state.selected_clips = vec![upper, later];
    state.selected_clip = Some(later);
    assert!(state.grade_needs_following(here), "a primary picked away is looked at again");
    assert_eq!(state.grade_at_playhead(), Some(upper), "the clip at the playhead is graded");
    assert_eq!(state.selected_clips, vec![upper, later], "the several are kept");
    assert!(!state.grade_needs_following(here), "settled");

    // One clip picked away: no set to keep.
    state.selected_clips.clear();
    state.selected_clip = Some(later);
    assert_eq!(state.grade_at_playhead(), Some(upper));
    assert!(state.selected_clips.is_empty());
    assert_eq!(state.colour_elsewhere_hint(), Some(later));
}

/// A clip's own menu says where a paste goes: the Colour page keeps the
/// clip at the playhead selected, so the paste went there, said it had
/// worked, and overwrote the wrong clip's grade.
#[test]
fn a_paste_from_a_clips_own_menu_goes_to_that_clip() {
    let Cut { mut state, upper, later, lower } = cut();
    assert_eq!(to_colour_page(&mut state), Some(upper));
    assert_eq!(state.paste_attributes_targets(), vec![upper], "from the Edit menu: the clip graded, as the header names it");
    state.paste_attributes_onto = Some(later);
    assert_eq!(state.paste_attributes_targets(), vec![later], "the clip right-clicked");
    assert_eq!(state.selected_clip, Some(upper), "and nothing else moved");
    assert_eq!(state.paste_attributes_named(&[later]), "\"interview.mp4\"");
    assert_eq!(state.paste_attributes_named(&[later, lower]), "2 clips");
    // Right-clicked inside a selection of several: all of them.
    state.selected_clips = vec![later, lower];
    let mut both = state.paste_attributes_targets();
    both.sort_by_key(|c| c.0);
    let mut want = vec![later, lower, upper];
    want.sort_by_key(|c| c.0);
    assert_eq!(both, want);
}

/// The hint coming and going leaves what is under it where it was.
#[test]
fn the_elsewhere_hint_does_not_move_the_rows_below_it() {
    let theme = crate::theme::KlippTheme::default();
    let below = |shown: bool| {
        let ctx = egui::Context::default();
        let mut y = 0.0;
        for _ in 0..2 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    super::colour_target::elsewhere_row(ui, &theme, shown);
                    y = ui.label("next row").rect.top();
                });
            });
        }
        y
    };
    assert_eq!(below(true), below(false));
}
