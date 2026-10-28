//! The inspector's Colour tab (#2952): its Basic Correction rows edit the
//! grade the Colour page edits, and its Auto Colour is the `auto_colour`
//! op an assistant sends.

use crate::state::AppState;
use klipp_core::{AppCommand, ClipId};
use klipp_timeline::GradeAccess;

const WIDTH: f32 = 420.0;

/// A state holding one clip never graded, selected.
fn ungraded() -> (AppState, ClipId) {
    let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 25);
    let clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    let mut state = AppState::default();
    let v1 = state.sequences[0].tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
    let id = clip.id;
    state.sequences[0].tracks[v1].clips.push(clip);
    state.selected_clip = Some(id);
    (state, id)
}

/// Where a view drew which text, by its centre.
type Texts = Vec<(String, egui::Pos2)>;

fn texts(out: &egui::FullOutput) -> Texts {
    out.shapes.iter().filter_map(|s| match &s.shape {
        egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)),
        _ => None,
    }).collect()
}

/// The two views drawn headless as the app draws them, one frame each.
struct Views {
    state: AppState,
    clip: ClipId,
    tab: egui::Context,
    page: egui::Context,
    panel: crate::panels::node_inspector::NodeInspectorPanel,
    clock: f64,
    /// A status message drawn before the inspector, as the panels before
    /// it may draw one (#2962).
    busy: bool,
}

impl Views {
    fn new() -> Self {
        let (state, clip) = ungraded();
        let (tab, page) = (egui::Context::default(), egui::Context::default());
        crate::icons::install(&tab);
        crate::icons::install(&page);
        Self { state, clip, tab, page, panel: Default::default(), clock: 0.0, busy: false }
    }

    fn input(&mut self, events: Vec<egui::Event>) -> egui::RawInput {
        self.clock += 0.05;
        egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(WIDTH, 1600.0))), time: Some(self.clock), events, ..Default::default() }
    }

    /// The inspector's Colour tab, its snapshot taken as `main_layout`
    /// takes it: its commands applied as the app applies them, whether
    /// Auto Colour was pressed, and its texts.
    fn tab(&mut self, events: Vec<egui::Event>) -> (bool, Texts) {
        let input = self.input(events);
        let theme = crate::theme::KlippTheme::default();
        let clip = self.state.sequences[0].all_clips().find(|c| c.id == self.clip).unwrap().clone();
        let grade = crate::inspector::grade_view::snapshot_for(&clip, &[], false, &mut self.state.pending_corrector);
        let (mut cmds, mut auto) = (Vec::new(), false);
        let (state, busy) = (&self.state, self.busy);
        let out = self.tab.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                if busy {
                    ui.label("Saving…");
                }
                let rect = egui::Rect::from_min_size(egui::pos2(0.0, 40.0), egui::vec2(WIDTH, 1500.0));
                crate::ui_util::panel_ui(ui, rect, "inspector_panel", |ui| {
                    crate::inspector::color_and_luts::show_color_and_luts(ui, state, &theme, grade.as_ref(), &mut auto, &mut cmds);
                });
            });
        });
        self.apply(&cmds);
        (auto, texts(&out))
    }

    /// The Colour page's Adjustments tab, drawn as `main_layout` draws it.
    fn page(&mut self) -> Texts {
        let input = self.input(Vec::new());
        let theme = crate::theme::KlippTheme::default();
        let clip_id = self.clip;
        let frame = crate::inspector::keyframe_selection::FrameSelection { time_secs: 0.5, tolerance: 0.02, clip_start: klipp_core::RationalTime::from_secs_f64(0.0, 25) };
        let Self { state, page, panel, .. } = self;
        let mut cmds = Vec::new();
        let out = page.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let clip = state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip_id)).unwrap();
                panel.generated = crate::inspector::grade_view::snapshot_for(clip, &[], false, &mut state.pending_corrector);
                let (mut selected, mut tab) = (None, crate::state::ColorTab::Adjustments);
                cmds.extend(clip.edit_grade(|g| panel.show_simple(ui, g, clip_id, &mut selected, &mut tab, &theme, frame)));
                panel.generated = None;
            });
        });
        assert!(cmds.is_empty(), "drawing the page changed nothing: {cmds:?}");
        texts(&out)
    }

    fn apply(&mut self, cmds: &[AppCommand]) {
        for cmd in cmds {
            let mut cmd = cmd.clone();
            crate::inspector::keyframe_selection::resolve_command(&self.state.sequences[0], &mut cmd);
            // A gesture's open and close change nothing by themselves.
            let marks = matches!(cmd, AppCommand::BeginParamEditAt { .. } | AppCommand::EndParamEditAt { .. });
            let applied = self.state.path_command(&cmd);
            assert!(applied == Some(true) || (marks && applied.is_some()), "{cmd:?} applied ({applied:?}): {:?}", self.state.toast.message_for_test());
        }
    }

    fn exposure(&self) -> Option<f32> {
        let clip = self.state.sequences[0].all_clips().find(|c| c.id == self.clip).unwrap();
        clip.grade().nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Corrector)?.params.exposure
    }
}

/// The texts drawn on the row labelled `label`, right of it.
fn row_values(texts: &Texts, label: &str) -> Vec<String> {
    let Some((_, at)) = texts.iter().find(|(t, _)| t == label) else { return Vec::new() };
    texts.iter().filter(|(t, p)| t != label && !t.is_empty() && (p.y - at.y).abs() < 4.0 && p.x > at.x).map(|(t, _)| t.clone()).collect()
}

/// Exposure dragged up on the inspector's Colour tab, on a clip never
/// graded: the first edit makes the Corrector the Colour page edits, and
/// the page's Exposure row then reads what the tab's does.
#[test]
fn exposure_from_the_tab_is_the_colour_pages() {
    let mut views = Views::new();
    let (_, drawn) = views.tab(Vec::new());
    for key in ["Exposure", "Contrast", "Saturation", "Temperature", "Tint", "Auto Colour", "Open in Colour page", "INPUT COLOUR SPACE"] {
        assert!(drawn.iter().any(|(t, _)| t == key), "{key} is not drawn: {drawn:?}");
    }
    assert_eq!(views.exposure(), None, "a clip never graded has no Corrector yet");
    let (_, at) = drawn.iter().find(|(t, _)| t == "Exposure").cloned().unwrap();
    let value = drawn.iter().find(|(t, p)| t.ends_with(" stops") && (p.y - at.y).abs() < 4.0).map(|(_, p)| *p).expect("the exposure value is drawn");
    let mut events = vec![egui::Event::PointerMoved(value), egui::Event::PointerButton { pos: value, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE }];
    views.tab(std::mem::take(&mut events));
    for dx in 1..=8 {
        views.tab(vec![egui::Event::PointerMoved(value + egui::vec2(dx as f32 * 10.0, 0.0))]);
    }
    let end = value + egui::vec2(80.0, 0.0);
    views.tab(vec![egui::Event::PointerButton { pos: end, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }]);
    views.tab(vec![egui::Event::PointerGone]);
    let set = views.exposure().expect("the drag made the Corrector and set its exposure");
    assert!(set > 0.05, "dragged right, exposure went up: {set}");
    let tab = row_values(&views.tab(Vec::new()).1, "Exposure");
    let page = row_values(&views.page(), "Exposure");
    assert!(!tab.is_empty() && tab == page, "the tab reads {tab:?}, the Colour page {page:?}");
    assert!(page.iter().any(|t| t.contains(&format!("{set:.2}"))), "the page shows {set:.2}: {page:?}");
}

/// Auto Colour on the tab asks the app for the tool, and the tool's
/// request is the `auto_colour` op on the selected clip, alone in its
/// apply, as an assistant sends it: the same op, so the same grade.
#[test]
fn auto_colour_from_the_tab_is_the_auto_colour_op() {
    let mut views = Views::new();
    let (_, drawn) = views.tab(Vec::new());
    let (_, at) = drawn.iter().find(|(t, _)| t == "Auto Colour").cloned().unwrap();
    let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    let mut pressed = views.tab(vec![egui::Event::PointerMoved(at)]).0;
    pressed |= views.tab(vec![button(true)]).0;
    pressed |= views.tab(vec![button(false)]).0;
    assert!(pressed, "Auto Colour asked for");
    let request = super::colour::colour_tool_request(crate::panels::colour_check::ColourAction::AutoColour, Some(views.clip), &[], None).expect("a clip selected");
    assert_eq!(request["method"], "apply");
    let ops: Vec<klipp_document::ops::Op> = serde_json::from_value(request["ops"].clone()).expect("ops an apply reads");
    let want: klipp_document::ops::Op = serde_json::from_value(serde_json::json!({ "op": "auto_colour", "clips": [views.clip.0.to_string()] })).unwrap();
    assert_eq!(ops, vec![want]);
    assert!(super::colour::colour_tool_request(crate::panels::colour_check::ColourAction::AutoColour, None, &[], None).is_none(), "no clip, no request");
}


/// #2962: Contrast typed into after a double-click on the tab, with a
/// second clip in the timeline, while a status message drawn before the
/// inspector comes and goes: the field keeps the keys, Ctrl+A selects its
/// text, and Enter leaves the value typed on this clip's grade, the other
/// clip and the selection as they were.
#[test]
fn contrast_typed_after_a_double_click_is_this_clips() {
    let mut views = Views::new();
    let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 25);
    let other = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(2.0), tc(4.0), tc(0.0), tc(2.0));
    let other_id = other.id;
    let v1 = views.state.sequences[0].tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
    views.state.sequences[0].tracks[v1].clips.push(other);
    let grade_of = |state: &AppState, id: ClipId| serde_json::to_value(state.sequences[0].all_clips().find(|c| c.id == id).unwrap().grade()).unwrap();
    let other_before = grade_of(&views.state, other_id);

    let (_, drawn) = views.tab(Vec::new());
    let (_, label) = drawn.iter().find(|(t, _)| t == "Contrast").cloned().unwrap();
    let at = drawn.iter().find(|(t, p)| t == "0.00" && (p.y - label.y).abs() < 4.0).map(|(_, p)| *p).expect("Contrast's 0.00");
    let b = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    for ev in [vec![egui::Event::PointerMoved(at)], vec![b(true)], vec![b(false)], vec![b(true)], vec![b(false)], vec![]] {
        views.tab(ev);
    }
    let field = views.tab.memory(|m| m.focused()).expect("the double-click gave Contrast the keys");
    views.busy = true;
    views.tab(vec![egui::Event::PointerGone]);
    views.tab(Vec::new());
    assert_eq!(views.tab.memory(|m| m.focused()), Some(field), "the field keeps the keys through the shift");
    let ctrl = egui::Modifiers { ctrl: true, command: true, ..Default::default() };
    let key = |key, modifiers| egui::Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers };
    views.tab(vec![key(egui::Key::A, ctrl)]);
    let range = egui::TextEdit::load_state(&views.tab, field).and_then(|s| s.cursor.char_range()).expect("a selection");
    assert_eq!((range.primary.index.min(range.secondary.index), range.primary.index.max(range.secondary.index)), (0, "0.00".len()), "Ctrl+A selected the field's text");
    assert!(views.tab.wants_keyboard_input(), "and the field still has the keys, so Select All is not read");
    views.busy = false;
    views.tab(vec![egui::Event::Text("0.4".into())]);
    views.tab(vec![key(egui::Key::Enter, egui::Modifiers::NONE)]);
    views.tab(Vec::new());
    let clip = views.state.sequences[0].all_clips().find(|c| c.id == views.clip).unwrap();
    let contrast = clip.grade().nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Corrector).and_then(|n| n.params.contrast);
    assert_eq!(contrast, Some(0.4), "the value typed, on this clip");
    assert_eq!(grade_of(&views.state, other_id), other_before, "the other clip is untouched");
    assert_eq!((views.state.selected_clip, views.state.selected_clips.len()), (Some(views.clip), 0), "the selection is as it was");
}
