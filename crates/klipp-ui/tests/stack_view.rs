//! The generated Effects tab (M3 step 12), drawn headless: what it shows
//! and what its controls send.

use klipp_core::{AppCommand, ClipId, MediaId, RationalTime};
use klipp_effects::BuiltinEffect as E;
use klipp_timeline::Clip;
use klipp_ui::inspector::stack_view;
use klipp_ui::state::AppState;
use klipp_ui::theme::KlippTheme;

fn clip() -> Clip {
    let t = |s: f64| RationalTime::from_secs_f64(s, 24);
    let mut c = Clip::media(MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0));
    c.id = ClipId::new();
    c.push_effect(E::GaussianBlur { radius: 3.0 });
    c.push_effect(E::Vignette { amount: 0.5, size: 0.5, softness: 0.5, roundness: 0.5 });
    c
}

/// One frame of the tab with `events`: the text drawn, at its centre, and
/// the commands pushed.
fn frame(ctx: &egui::Context, clip: &Clip, state: &AppState, events: Vec<egui::Event>) -> (Vec<(String, egui::Pos2)>, Vec<AppCommand>) {
    let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(520.0, 3000.0))), time: Some(0.0), events, ..Default::default() };
    let mut cmds = Vec::new();
    let out = ctx.run(input, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| stack_view::show(ui, &KlippTheme::default(), state, clip, &mut cmds, 0.0));
    });
    let text = out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)), _ => None }).collect();
    (text, cmds)
}

fn texts(drawn: &[(String, egui::Pos2)]) -> Vec<&str> {
    drawn.iter().map(|(s, _)| s.as_str()).collect()
}

/// Opening the tab, frame after frame, sends nothing and draws the same.
#[test]
fn opening_the_tab_never_changes_mode_or_values() {
    let c = clip();
    let state = AppState::default();
    let ctx = egui::Context::default();
    let (first, cmds) = frame(&ctx, &c, &state, Vec::new());
    assert!(cmds.is_empty(), "{cmds:?}");
    for _ in 0..9 {
        let (again, cmds) = frame(&ctx, &c, &state, Vec::new());
        assert!(cmds.is_empty(), "{cmds:?}");
        assert_eq!(texts(&again), texts(&first));
    }
}

/// The stack shows the clip's effects in order under their header, each
/// with its rows, and the Placement row.
#[test]
fn the_stack_shows_each_effect_in_order() {
    let c = clip();
    let (drawn, _) = frame(&egui::Context::default(), &c, &AppState::default(), Vec::new());
    let t = texts(&drawn);
    let at = |s: &str| t.iter().position(|x| *x == s).unwrap_or_else(|| panic!("{s:?} not in {t:?}"));
    assert!(at("EFFECTS") < at("Gaussian Blur") && at("Gaussian Blur") < at("Vignette"), "{t:?}");
    assert!(at("Gaussian Blur") < at("Radius"), "the blur's own row under it: {t:?}");
    assert!(t.iter().any(|s| s.starts_with("Auto") || s.starts_with("Fixed") || *s == "Placement pending"), "the Placement row: {t:?}");
}

/// The eye turns the effect off by its path, and nothing else is sent.
#[test]
fn the_eye_turns_an_effect_off_by_path() {
    let mut c = clip();
    let state = AppState::default();
    let ctx = egui::Context::default();
    let (drawn, _) = frame(&ctx, &c, &state, Vec::new());
    // The eye sits left of the name, on its line.
    let name = drawn.iter().find(|(s, _)| s == "Vignette").unwrap().1;
    let eye = egui::pos2(14.0, name.y);
    let button = |pressed| egui::Event::PointerButton { pos: eye, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    frame(&ctx, &c, &state, vec![egui::Event::PointerMoved(eye), button(true)]);
    let (_, cmds) = frame(&ctx, &c, &state, vec![button(false)]);
    let vignette = c.effect_id_at(1).unwrap().as_uuid();
    assert!(
        matches!(cmds.as_slice(), [AppCommand::SetNodeEnabledAt { addr, enabled: false }] if addr.node() == Some(vignette)),
        "{cmds:?}"
    );
}
