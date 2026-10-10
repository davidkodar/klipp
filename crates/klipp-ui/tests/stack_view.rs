//! The generated Effects tab (M3 step 12), drawn headless: what it shows
//! and what its controls send.

use klipp_core::{AppCommand, ClipId, MediaId, RationalTime};
use klipp_effects::BuiltinEffect as E;
use klipp_timeline::Clip;
use klipp_timeline::test_clip::{self, TestClip};
use klipp_ui::inspector::stack_view;
use klipp_ui::state::AppState;
use klipp_ui::theme::KlippTheme;

fn clip() -> Clip {
    let t = |s: f64| RationalTime::from_secs_f64(s, 24);
    let mut c = Clip::media(MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0));
    c.id = ClipId::new();
    TestClip::of(c).effect(E::GaussianBlur { radius: 3.0 }).effect(E::Vignette { amount: 0.5, size: 0.5, softness: 0.5, roundness: 0.5 }).build()
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
    let c = clip();
    let state = AppState::default();
    let ctx = egui::Context::default();
    let (drawn, _) = frame(&ctx, &c, &state, Vec::new());
    // The eye sits left of the name, on its line.
    let name = drawn.iter().find(|(s, _)| s == "Vignette").unwrap().1;
    let eye = egui::pos2(14.0, name.y);
    let button = |pressed| egui::Event::PointerButton { pos: eye, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    frame(&ctx, &c, &state, vec![egui::Event::PointerMoved(eye), button(true)]);
    let (_, cmds) = frame(&ctx, &c, &state, vec![button(false)]);
    let vignette = test_clip::effect_id(&c, 1).as_uuid();
    assert!(
        matches!(cmds.as_slice(), [AppCommand::SetNodeEnabledAt { addr, enabled: false }] if addr.node() == Some(vignette)),
        "{cmds:?}"
    );
}

/// Click the first `label` drawn below the row named `row`: the commands
/// the click sent.
fn click_below(c: &Clip, row: &str, label: &str) -> Vec<AppCommand> {
    let state = AppState::default();
    let ctx = egui::Context::default();
    let (drawn, _) = frame(&ctx, c, &state, Vec::new());
    let y = drawn.iter().find(|(s, _)| s == row).unwrap().1.y;
    let at = drawn.iter().filter(|(s, p)| s == label && p.y > y).min_by(|a, b| a.1.y.total_cmp(&b.1.y)).unwrap_or_else(|| panic!("no {label} below {row}")).1;
    let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    frame(&ctx, c, &state, vec![egui::Event::PointerMoved(at), button(true)]);
    frame(&ctx, c, &state, vec![button(false)]).1
}

/// Remove asks the app to remove that effect's node by path; the app
/// plans it (M3 step 9) and says why when it refuses.
#[test]
fn remove_sends_the_effects_path() {
    let c = clip();
    let vignette = test_clip::effect_id(&c, 1).as_uuid();
    let cmds = click_below(&c, "Vignette", "✕");
    assert!(matches!(cmds.as_slice(), [AppCommand::RemoveNodeAt { addr }] if addr.node() == Some(vignette)), "{cmds:?}");
}

/// Down moves an effect after the next; up moves it after the one before
/// the one above, or after the row before the effects at the top.
#[test]
fn reorder_moves_by_path() {
    let c = clip();
    let (blur, vignette) = (test_clip::effect_id(&c, 0).as_uuid(), test_clip::effect_id(&c, 1).as_uuid());
    let down = click_below(&c, "Gaussian Blur", "↓");
    assert!(matches!(down.as_slice(), [AppCommand::MoveNodeAt { addr, after }] if addr.node() == Some(blur) && *after == vignette), "{down:?}");
    let up = click_below(&c, "Vignette", "↑");
    let lifted = c.picture_v2().unwrap();
    let view = klipp_graph_lift::stack_view::project(klipp_graph_format::path::Root::Clip { clip: c.id.0 }, &lifted.picture.graph, &klipp_timeline::RegistryCatalog, None).unwrap();
    let rows: Vec<_> = view.rows().collect();
    let before_effects = rows[rows.iter().position(|r| r.addr.node() == Some(blur)).unwrap() - 1].addr.node().unwrap();
    assert!(matches!(up.as_slice(), [AppCommand::MoveNodeAt { addr, after }] if addr.node() == Some(vignette) && *after == before_effects), "{up:?}");
}

/// An added effect is a new node holding the effect at its reset values,
/// put after the clip's last effect.
#[test]
fn an_add_goes_after_the_last_effect() {
    let c = clip();
    let node = stack_view::new_effect_node("sharpen").expect("a node");
    assert_eq!(node.type_id.as_str(), "klipp.legacy.effect");
    assert_eq!(klipp_graph_lift::effect_params::tag(&node), Some("sharpen"));
    let lifted = c.picture_v2().unwrap();
    let view = klipp_graph_lift::stack_view::project(klipp_graph_format::path::Root::Clip { clip: c.id.0 }, &lifted.picture.graph, &klipp_timeline::RegistryCatalog, None).unwrap();
    assert_eq!(stack_view::add_after(&view).and_then(|p| p.node()), Some(test_clip::effect_id(&c, 1).as_uuid()));
}

/// A second Transform, an Opacity, a Speed Ramp or a Temporal Denoise is a
/// card of its own, added through the clip's effect list, which places it;
/// every other effect is added as a node.
#[test]
fn placed_kinds_are_added_through_the_effect_list() {
    let c = clip();
    let after = klipp_graph_format::path::NodePath::new(klipp_graph_format::path::Root::Clip { clip: c.id.0 }, [test_clip::effect_id(&c, 1).as_uuid()]);
    for tag in ["transform", "opacity", "speed_ramp", "temporal_denoise"] {
        let cmd = stack_view::add_command(&c, &after, tag).unwrap();
        assert!(matches!(&cmd, AppCommand::AddBuiltinEffect { clip_id, effect } if *clip_id == c.id && klipp_ui::effect_presets::builtin_effect_key(effect) == klipp_ui::effect_presets::builtin_effect_key(&(klipp_effects::registry::effect_type(tag).unwrap().sample)())), "{tag}: {cmd:?}");
    }
    assert!(matches!(stack_view::add_command(&c, &after, "sharpen"), Some(AppCommand::AddNodeAt { .. })));
}
