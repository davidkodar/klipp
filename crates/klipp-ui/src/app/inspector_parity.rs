//! The inspector parity harness (M3 step 1c): one scripted interaction,
//! run through the old effects tab (switch off) and the generated stack
//! (switch on), each view's commands applied as the app applies them (the
//! path commands by the state, the index ones by the inspector edit host),
//! must leave the same clip picture (the full typed `picture_v2`), the
//! same undo depth, and the same analysis side commands.
//!
//! Each view is drawn headless and driven by real pointer and key events,
//! found from the frame's text as the other headless tests do. Both views
//! draw a number with `scrubby_param_row`, so one script drives both.

use super::inspector_edit_host::{InspectorEditHost, InspectorSideEffects};
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::{AppCommand, ClipId};
use klipp_effects::BuiltinEffect;

/// What the host would send on: recorded, never sent.
#[derive(Default)]
struct Sink;
impl InspectorSideEffects for Sink {
    fn sync_sequence(&self, _state: &AppState) {}
    fn send(&self, _command: AppCommand) {}
}

/// The commands a view sends that neither the state nor the host applies,
/// and that the parity compares: the analysis side commands.
fn is_side(cmd: &AppCommand) -> bool {
    matches!(cmd, AppCommand::ResmoothStabilization { .. } | AppCommand::RegainDeflicker { .. } | AppCommand::AnalyzeStabilization { .. } | AppCommand::AnalyzeDeflicker { .. })
}

/// One view of one clip, drawn and driven.
struct Run {
    state: AppState,
    clip: ClipId,
    ctx: egui::Context,
    time: f64,
    side: Vec<String>,
    unapplied: Vec<String>,
}

/// The one clip both views start from: a clone each, so their pictures
/// can be compared down to every id.
fn fixture(effect: &BuiltinEffect) -> klipp_timeline::Clip {
    let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
    let mut clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    clip.push_effect(effect.clone());
    clip
}

impl Run {
    fn new(clip: klipp_timeline::Clip, generated: bool) -> Self {
        let id = clip.id;
        let mut state = AppState::default();
        state.sequences[0].tracks[0].clips.push(clip);
        state.use_generated_stack = generated;
        Run { state, clip: id, ctx: egui::Context::default(), time: 0.0, side: Vec::new(), unapplied: Vec::new() }
    }

    fn clip(&self) -> &klipp_timeline::Clip {
        self.state.sequences[0].all_clips().find(|c| c.id == self.clip).unwrap()
    }

    /// One frame with `events`: the text drawn, at its centre. The commands
    /// the view sent are applied as the app applies them.
    fn frame(&mut self, events: Vec<egui::Event>, modifiers: egui::Modifiers) -> Vec<(String, egui::Pos2)> {
        self.time += 0.05;
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(520.0, 3000.0))), time: Some(self.time), events, modifiers, ..Default::default() };
        let mut cmds = Vec::new();
        let (state, clip_id) = (&self.state, self.clip);
        let out = self.ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let clip = state.sequences[0].all_clips().find(|c| c.id == clip_id).unwrap();
                let theme = KlippTheme::default();
                if state.use_generated_stack {
                    crate::inspector::stack_view::show(ui, &theme, state, clip, &mut cmds, 0.0);
                } else {
                    crate::inspector::effects_tab::show(ui, &theme, state, clip, &mut cmds, &mut false, &mut false, 0.0);
                }
            });
        });
        for cmd in cmds {
            self.apply(cmd);
        }
        out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)), _ => None }).collect()
    }

    fn apply(&mut self, cmd: AppCommand) {
        if is_side(&cmd) {
            self.side.push(format!("{cmd:?}"));
            return;
        }
        if self.state.path_command(&cmd).is_some() {
            return;
        }
        self.state.before_dispatch(&cmd);
        if !(InspectorEditHost { state: &mut self.state, side: &Sink }).apply(&cmd) {
            self.unapplied.push(format!("{cmd:?}"));
        }
    }

    /// The number drawn on the row labelled `label`: the first text right
    /// of it on its line that reads as a number.
    fn number(&mut self, label: &str) -> Option<egui::Pos2> {
        let drawn = self.frame(Vec::new(), egui::Modifiers::NONE);
        // The label may be drawn twice (an effect named as its row): the
        // row's is the one with a number on its line.
        drawn.iter().filter(|(s, _)| s == label).find_map(|(_, at)| {
            drawn
                .iter()
                .filter(|(s, p)| (p.y - at.y).abs() < 6.0 && p.x > at.x && s.trim_start_matches('-').starts_with(|c: char| c.is_ascii_digit()))
                .min_by(|a, b| a.1.x.total_cmp(&b.1.x))
                .map(|(_, p)| *p)
        })
    }

    fn button(&mut self, at: egui::Pos2, button: egui::PointerButton, pressed: bool) {
        self.frame(vec![egui::Event::PointerMoved(at), egui::Event::PointerButton { pos: at, button, pressed, modifiers: egui::Modifiers::NONE }], egui::Modifiers::NONE);
    }

    fn key(&mut self, key: egui::Key, modifiers: egui::Modifiers) {
        self.frame(vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }], modifiers);
    }
}

/// An interaction with a row's number.
#[derive(Clone, Copy, Debug)]
enum Script {
    /// S1: type a value inside the slider range and press Enter.
    Type,
    /// S2: press on the number, three moves, release.
    Drag,
    /// S3: right-click the number.
    Reset,
}

fn play(run: &mut Run, label: &str, script: Script, typed: &str) -> bool {
    let Some(at) = run.number(label) else { return false };
    match script {
        Script::Type => {
            run.button(at, egui::PointerButton::Primary, true);
            run.button(at, egui::PointerButton::Primary, false);
            run.button(at, egui::PointerButton::Primary, true);
            run.button(at, egui::PointerButton::Primary, false);
            run.frame(Vec::new(), egui::Modifiers::NONE);
            run.key(egui::Key::A, egui::Modifiers::COMMAND);
            run.frame(vec![egui::Event::Text(typed.into())], egui::Modifiers::NONE);
            run.key(egui::Key::Enter, egui::Modifiers::NONE);
        }
        Script::Drag => {
            run.button(at, egui::PointerButton::Primary, true);
            for dx in [6.0, 12.0, 18.0] {
                run.frame(vec![egui::Event::PointerMoved(at + egui::vec2(dx, 0.0))], egui::Modifiers::NONE);
            }
            run.frame(vec![egui::Event::PointerButton { pos: at + egui::vec2(18.0, 0.0), button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }], egui::Modifiers::NONE);
        }
        Script::Reset => {
            run.button(at, egui::PointerButton::Secondary, true);
            run.button(at, egui::PointerButton::Secondary, false);
        }
    }
    run.frame(Vec::new(), egui::Modifiers::NONE);
    true
}

/// Each effect type's first number row, with today's label and a value
/// inside its slider range to type.
fn first_number_row(t: &klipp_effects::registry::EffectType) -> Option<(&'static str, String)> {
    use klipp_effects::registry::presentation::Widget;
    t.desc.params.iter().find_map(|p| {
        let pres = t.presentation(p);
        let numeric = matches!(p.kind, klipp_effects::registry::ParamKind::Float) && pres.widget == Widget::Scrub && pres.visible_if.is_none();
        let (lo, hi) = p.soft?;
        numeric.then(|| (pres.label.unwrap_or(p.label), format!("{:.*}", usize::from(pres.decimals), lo + (hi - lo) * 0.3)))
    })
}

/// The types whose rows differ from today by design (the meant
/// differences of the generated rows), where a reset can't agree.
const RESET_DIFFERS: &[&str] = &["klipp.fx.sharpen", "klipp.fx.vibrance"];

/// IC13: a drag of any row is one undo step. Colour Fade's duration was a
/// bare drag value, two steps for one drag; its picture still agrees.
const UNDO_DIFFERS: &[(&str, &str)] = &[("klipp.fx.color_fade", "Drag")];

/// S1–S3 on every effect's first number row: the same picture, the same
/// undo depth and the same side commands, old view against new.
#[test]
#[ignore = "M3 step 12: run with --ignored until the generated stack is the default"]
fn number_rows_edit_the_same_through_both_views() {
    let mut differ = Vec::new();
    let mut compared = 0;
    for t in klipp_effects::registry::effect_types() {
        let Some((label, typed)) = first_number_row(t) else { continue };
        for script in [Script::Type, Script::Drag, Script::Reset] {
            if matches!(script, Script::Reset) && RESET_DIFFERS.contains(&t.desc.id) {
                continue;
            }
            let clip = fixture(&(t.sample)());
            let (mut old, mut new) = (Run::new(clip.clone(), false), Run::new(clip, true));
            let (o, n) = (play(&mut old, label, script, &typed), play(&mut new, label, script, &typed));
            if !(o && n) {
                differ.push(format!("{} {script:?}: row {label:?} found old {o}, new {n}", t.desc.id));
                continue;
            }
            let (po, pn) = (old.clip().picture_v2().unwrap(), new.clip().picture_v2().unwrap());
            if po != pn {
                differ.push(format!("{} {script:?}: the pictures differ: old {:?}, new {:?}", t.desc.id, old.clip().builtin_effects(), new.clip().builtin_effects()));
            }
            let (uo, un) = (old.state.active_history().undo_count(), new.state.active_history().undo_count());
            if uo != un && !UNDO_DIFFERS.contains(&(t.desc.id, &format!("{script:?}"))) {
                differ.push(format!("{} {script:?}: undo depth old {uo}, new {un}", t.desc.id));
            }
            if old.side != new.side {
                differ.push(format!("{} {script:?}: side commands old {:?}, new {:?}", t.desc.id, old.side, new.side));
            }
            if !old.unapplied.is_empty() || !new.unapplied.is_empty() {
                differ.push(format!("{} {script:?}: unapplied old {:?}, new {:?}", t.desc.id, old.unapplied, new.unapplied));
            }
            compared += 1;
        }
    }
    assert!(differ.is_empty(), "{} differences in {compared} runs:\n{}", differ.len(), differ.join("\n"));
    assert!(compared > 100, "{compared} runs");
}


impl Run {
    /// Click the first text `label` drawn below the row named `row` (or
    /// anywhere when `row` is empty).
    fn click_text(&mut self, row: &str, label: &str) -> bool {
        let drawn = self.frame(Vec::new(), egui::Modifiers::NONE);
        let y = if row.is_empty() { f32::MIN } else {
            match drawn.iter().find(|(s, _)| s == row) {
                Some((_, p)) => p.y,
                None => return false,
            }
        };
        let Some(at) = drawn.iter().filter(|(s, p)| s == label && p.y > y).min_by(|a, b| a.1.y.total_cmp(&b.1.y)).map(|(_, p)| *p) else { return false };
        self.button(at, egui::PointerButton::Primary, true);
        self.button(at, egui::PointerButton::Primary, false);
        self.frame(Vec::new(), egui::Modifiers::NONE);
        true
    }

    /// Click `label` in an open menu: its last drawn occurrence (a menu is
    /// drawn over the panel), scrolling the menu down until it shows.
    fn click_menu_item(&mut self, label: &str) -> bool {
        let mut scroll_at = None;
        for _ in 0..40 {
            let drawn = self.frame(Vec::new(), egui::Modifiers::NONE);
            if drawn.iter().rev().any(|(s, _)| s == label) {
                // Let a scroll settle, then find it again, so the click lands
                // where it is drawn.
                for _ in 0..10 {
                    self.frame(Vec::new(), egui::Modifiers::NONE);
                }
                let drawn = self.frame(Vec::new(), egui::Modifiers::NONE);
                let Some(at) = drawn.iter().rev().find(|(s, _)| s == label).map(|(_, p)| *p) else { continue };
                self.button(at, egui::PointerButton::Primary, true);
                self.button(at, egui::PointerButton::Primary, false);
                self.frame(Vec::new(), egui::Modifiers::NONE);
                return true;
            }
            // Over the menu's own items: the first of its sorted names.
            let first = klipp_effects::registry::effect_types().map(|t| t.desc.name).min().unwrap_or_default();
            let Some(at) = scroll_at.or_else(|| drawn.iter().rev().find(|(s, _)| s == first).map(|(_, p)| *p)) else { return false };
            scroll_at = Some(at);
            self.frame(vec![egui::Event::PointerMoved(at), egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, -60.0), modifiers: egui::Modifiers::NONE }], egui::Modifiers::NONE);
        }
        false
    }

    /// The new view's eye on the row named `row`: left of its name.
    fn click_eye(&mut self, row: &str) -> bool {
        let drawn = self.frame(Vec::new(), egui::Modifiers::NONE);
        let Some(at) = drawn.iter().find(|(s, _)| s == row).map(|(_, p)| egui::pos2(14.0, p.y)) else { return false };
        self.button(at, egui::PointerButton::Primary, true);
        self.button(at, egui::PointerButton::Primary, false);
        self.frame(Vec::new(), egui::Modifiers::NONE);
        true
    }

    /// An index command, as the old card's own control sends it.
    fn send(&mut self, cmd: AppCommand) {
        self.apply(cmd);
    }
}

/// The name the new view shows for an effect's card.
fn card_name(t: &klipp_effects::registry::EffectType) -> &'static str {
    t.desc.name
}

/// S5 and S6 on every effect: turning it off, removing it, moving it below
/// another, and adding one, through each view's own control (the old
/// card's index commands, the new card's eye, ✕, ↓ and Add menu): the same
/// picture, or for an add, the same effects in the same order.
#[test]
#[ignore = "M3 step 12: run with --ignored until the generated stack is the default"]
fn card_edits_are_the_same_through_both_views() {
    let mut differ = Vec::new();
    let mut compared = 0;
    let other = BuiltinEffect::Sharpen { amount: 0.5 };
    for t in klipp_effects::registry::effect_types() {
        let effect = (t.sample)();
        let name = card_name(t);
        // A placed kind's card is the clip's own control: its eye, remove
        // and move are the effect list's, outside these.
        if crate::inspector::stack_view::is_placed_kind(t.tag) {
            continue;
        }
        let check = |what: &str, old: &Run, new: &Run, ids_equal: bool, differ: &mut Vec<String>| {
            let same = if ids_equal {
                old.clip().picture_v2().unwrap() == new.clip().picture_v2().unwrap()
            } else {
                old.clip().builtin_effects() == new.clip().builtin_effects() && old.clip().builtin_effects_enabled() == new.clip().builtin_effects_enabled()
            };
            if !same {
                differ.push(format!("{} {what}: old {:?}, new {:?}", t.desc.id, old.clip().builtin_effects(), new.clip().builtin_effects()));
            }
            if old.state.active_history().undo_count() != new.state.active_history().undo_count() {
                differ.push(format!("{} {what}: undo old {}, new {}", t.desc.id, old.state.active_history().undo_count(), new.state.active_history().undo_count()));
            }
            if !new.unapplied.is_empty() || !old.unapplied.is_empty() {
                differ.push(format!("{} {what}: unapplied old {:?}, new {:?}", t.desc.id, old.unapplied, new.unapplied));
            }
        };
        // S5: off.
        let clip = fixture(&effect);
        let (mut old, mut new) = (Run::new(clip.clone(), false), Run::new(clip.clone(), true));
        old.send(AppCommand::ToggleBuiltinEffectEnabled { clip_id: clip.id, index: 0 });
        if new.click_eye(name) { check("off", &old, &new, true, &mut differ) } else { differ.push(format!("{} off: no eye", t.desc.id)) }
        // S6: remove.
        let (mut old, mut new) = (Run::new(clip.clone(), false), Run::new(clip.clone(), true));
        old.send(AppCommand::RemoveBuiltinEffect { clip_id: clip.id, index: 0 });
        if new.click_text(name, "✕") { check("remove", &old, &new, true, &mut differ) } else { differ.push(format!("{} remove: no ✕", t.desc.id)) }
        // S6: move below another.
        let mut two = clip.clone();
        two.push_effect(other.clone());
        let (mut old, mut new) = (Run::new(two.clone(), false), Run::new(two, true));
        old.send(AppCommand::ReorderBuiltinEffect { clip_id: clip.id, from_index: 0, to_index: 1 });
        if new.click_text(name, "↓") { check("move", &old, &new, true, &mut differ) } else { differ.push(format!("{} move: no ↓", t.desc.id)) }
        // S6: add, onto a clip whose effect has another name, so the menu
        // item is the only one.
        let base = fixture(if t.tag == "sharpen" { &BuiltinEffect::Invert { mix: 1.0 } } else { &other });
        let (mut old, mut new) = (Run::new(base.clone(), false), Run::new(base.clone(), true));
        old.send(AppCommand::AddBuiltinEffect { clip_id: base.id, effect: effect.clone() });
        if new.click_text("", "+ Add effect") && new.click_menu_item(name) { check("add", &old, &new, false, &mut differ) } else { differ.push(format!("{} add: not in the menu", t.desc.id)) }
        compared += 4;
    }
    assert!(differ.is_empty(), "{} differences in {compared} runs:\n{}", differ.len(), differ.join("\n"));
    assert!(compared > 150, "{compared} runs");
}
