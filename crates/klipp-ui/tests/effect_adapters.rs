//! The effect adapters (M3 step 6): an effect's specialist behaviour, re-
//! hosted around its generated rows, sends what today's effects tab sends.

use klipp_core::{AppCommand, ClipId, RationalTime};
use klipp_effects::registry::{effect_types, EffectType, ParamDesc};
use klipp_graph_format::path::{NodePath, Root};
use klipp_timeline::Clip;
use klipp_ui::inspector::generated::adapters::{adapter, effect_card, AdapterCtx};
use klipp_ui::inspector::generated::{effect_value, KeyState, NodeRows, RowCtx, RowOutcome};
use klipp_ui::state::AppState;
use klipp_ui::theme::KlippTheme;

/// Commands compared by what they say (`AppCommand` has no `PartialEq`).
macro_rules! assert_cmds {
    ($got:expr, $want:expr) => {{
        let (got, want): (Vec<AppCommand>, Vec<AppCommand>) = ($got.into_iter().collect(), $want.into_iter().collect());
        assert_eq!(format!("{got:?}"), format!("{want:?}"));
    }};
}

fn ty(id: &str) -> &'static EffectType {
    effect_types().find(|t| t.desc.id == id).unwrap()
}

fn clip() -> Clip {
    let mut c = Clip::gap(RationalTime::from_secs_f64(0.0, 24), RationalTime::from_secs_f64(2.0, 24));
    c.id = ClipId::new();
    c
}

/// One frame of `t`'s card for `clip`, with `events`: the text drawn, at
/// its centre, and the commands pushed.
fn card(ctx: &egui::Context, t: &EffectType, clip: &Clip, state: &AppState, events: Vec<egui::Event>) -> (Vec<(String, egui::Pos2)>, Vec<AppCommand>) {
    let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(520.0, 2000.0))), time: Some(0.0), events, ..Default::default() };
    let json = serde_json::to_value((t.sample)()).unwrap();
    let theme = KlippTheme::default();
    let path = NodePath::new(Root::Clip { clip: clip.id.0 }, [uuid::Uuid::from_u128(1)]);
    let mut cmds = Vec::new();
    let presentation = |p: &ParamDesc| t.presentation(p);
    let stored = |p: &ParamDesc| effect_value(&json, p);
    let keys = |_: &ParamDesc| (None, KeyState::default());
    let rows = NodeRows { params: t.desc.params, presentation: &presentation, stored: &stored, keys: &keys };
    let out = ctx.run(input, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let mut rc = RowCtx {
                addr: &path,
                clip_time_secs: 0.0,
                timeline_in_secs: 0.0,
                rate: 24,
                encoding: klipp_grade_model::Encoding::Bt1886,
                entry: Some(klipp_ui::inspector::generated::Entry { payload: &json, fields_at: &[], choices_by_index: false }),
                cmds: &mut cmds,
                behaviours: None,
                theme: &theme,
                app: Some(state),
                custom: None,
            };
            effect_card(ui, &mut rc, &rows, t, clip, state);
        });
    });
    let text = out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)), _ => None }).collect();
    (text, cmds)
}

/// Click the text `label` on `t`'s card: the commands the click pushed.
fn click(t: &EffectType, clip: &Clip, state: &AppState, label: &str) -> Vec<AppCommand> {
    let ctx = egui::Context::default();
    let (drawn, cmds) = card(&ctx, t, clip, state, Vec::new());
    assert!(cmds.is_empty(), "drawing pushed {cmds:?}");
    let at = drawn.iter().find(|(s, _)| s == label).unwrap_or_else(|| panic!("no {label:?} in {:?}", drawn.iter().map(|(s, _)| s).collect::<Vec<_>>())).1;
    let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    card(&ctx, t, clip, state, vec![egui::Event::PointerMoved(at), button(true)]);
    card(&ctx, t, clip, state, vec![button(false)]).1
}

/// Analyse, then Cancel while it measures, then Re-analyse once it has:
/// both analysis-driven effects, through the generated card.
#[test]
fn analysis_buttons_click_through() {
    let c = clip();
    let mut state = AppState::default();
    let deflicker = ty("klipp.fx.deflicker");
    assert_cmds!(click(deflicker, &c, &state, "Analyse"), [AppCommand::AnalyzeDeflicker { clip_id: c.id }]);
    state.deflicker_in_progress.insert(c.id);
    assert_cmds!(click(deflicker, &c, &state, "Cancel"), [AppCommand::CancelDeflicker { clip_id: c.id }]);
    state.deflicker_in_progress.clear();
    let mut measured = c.clone();
    measured.deflicker_data = Some(klipp_effects::deflicker::DeflickerData { luma: vec![0.5; 48], fps: 24.0 });
    assert_cmds!(click(deflicker, &measured, &state, "Re-analyse"), [AppCommand::AnalyzeDeflicker { clip_id: c.id }]);

    let stab = ty("klipp.fx.video_stabilization");
    assert_cmds!(click(stab, &c, &state, "Analyze Clip"), [AppCommand::AnalyzeStabilization { clip_id: c.id }]);
    state.stabilization_in_progress.insert(c.id);
    assert_cmds!(click(stab, &c, &state, "Cancel"), [AppCommand::CancelStabilization { clip_id: c.id }]);
    state.stabilization_in_progress.clear();
    let mut analysed = c.clone();
    analysed.stabilization_data = Some(klipp_effects::stabilization::StabilizationData { frame_transforms: Vec::new(), accumulated: Vec::new(), trajectory: Vec::new(), fps: 24.0, frame_count: 0 });
    assert_cmds!(click(stab, &analysed, &state, "Re-analyze"), [AppCommand::AnalyzeStabilization { clip_id: c.id }]);
    assert_cmds!(click(stab, &analysed, &state, "Compare"), [AppCommand::ToggleStabilizationCompare]);
}

/// A row's edit brings today's side command: a moved window re-gains a
/// measured deflicker, a moved smoothing re-smooths an analysed
/// stabilization (and not without its analysis); other rows bring none.
#[test]
fn row_edits_bring_todays_side_commands() {
    let state = AppState::default();
    let theme = KlippTheme::default();
    let mut analysed = clip();
    let run = |id: &str, clip: &Clip, key: &str, outcome: RowOutcome| {
        let mut cmds = Vec::new();
        adapter(id).unwrap().after_row(&mut AdapterCtx { theme: &theme, clip, state: &state, cmds: &mut cmds }, key, outcome);
        cmds
    };
    let c = clip();
    assert_cmds!(run("deflicker", &c, "window_secs", RowOutcome::Edited), [AppCommand::RegainDeflicker { clip_id: c.id }]);
    assert_cmds!(run("deflicker", &c, "window_secs", RowOutcome::Keyed), [AppCommand::RegainDeflicker { clip_id: c.id }]);
    assert!(run("deflicker", &c, "strength", RowOutcome::Edited).is_empty());
    assert!(run("deflicker", &c, "window_secs", RowOutcome::Nothing).is_empty());
    assert!(run("video_stabilization", &c, "smoothing", RowOutcome::Edited).is_empty(), "no analysis to re-smooth");
    analysed.stabilization_data = Some(klipp_effects::stabilization::StabilizationData { frame_transforms: Vec::new(), accumulated: Vec::new(), trajectory: Vec::new(), fps: 24.0, frame_count: 0 });
    assert_cmds!(run("video_stabilization", &analysed, "smoothing", RowOutcome::Edited), [AppCommand::ResmoothStabilization { clip_id: analysed.id }]);
    assert!(run("video_stabilization", &analysed, "crop_ratio", RowOutcome::Edited).is_empty());
}

/// Focus Blur shows its notice and no editable row: a title's roll sets it.
#[test]
fn focus_blur_shows_information_only() {
    let ctx = egui::Context::default();
    let (drawn, cmds) = card(&ctx, ty("klipp.fx.focus_blur"), &clip(), &AppState::default(), Vec::new());
    assert!(cmds.is_empty());
    let texts: Vec<&str> = drawn.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(texts, ["Set by the roll's focus band in the Title tab"]);
}

/// Temporal Noise Reduction keeps today's three lines: where it runs (the
/// notice), what the threshold means (its help), where it renders (the
/// adapter's note).
#[test]
fn temporal_denoise_keeps_its_three_lines() {
    let ctx = egui::Context::default();
    let (drawn, _) = card(&ctx, ty("klipp.fx.temporal_denoise"), &clip(), &AppState::default(), Vec::new());
    let texts: Vec<&str> = drawn.iter().map(|(s, _)| s.as_str()).collect();
    for line in ["Runs on the source, before the grade", "How far a pixel may change between frames", "rendered on the CPU rather than the GPU"] {
        assert!(texts.iter().any(|t| t.contains(line)), "{line:?} in {texts:?}");
    }
}

/// A frei0r effect whose plug-in isn't installed says so and changes
/// nothing.
#[test]
fn a_missing_frei0r_plugin_is_named_and_kept() {
    let ctx = egui::Context::default();
    let (drawn, cmds) = card(&ctx, ty("klipp.fx.frei0r"), &clip(), &AppState::default(), Vec::new());
    assert!(cmds.is_empty(), "{cmds:?}");
    assert!(drawn.iter().any(|(s, _)| s.starts_with("⚠ Plugin not installed")), "{:?}", drawn.iter().map(|(s, _)| s).collect::<Vec<_>>());
}
