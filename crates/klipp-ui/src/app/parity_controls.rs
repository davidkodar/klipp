//! M3 step 18 parity: the effects stack's switches and menus. Each case
//! drives the generated stack through its real control (a checkbox
//! clicked, a menu item picked) and compares the clip's whole typed
//! picture and its undo depth with a twin where the old route's command,
//! `UpdateBuiltinEffect` with the one field changed, made the same edit.
//! Undo must then give back the exact picture before and redo the exact
//! picture after, through the document as the app applies them.
//!
//! The old card's menus are labelled differently by design, so the oracle
//! for a discrete control is the old route's command, not a click in the
//! old view. A key is recorded only once its case's assertions pass.

use super::inspector_edit_host::{InspectorEditHost, InspectorSideEffects};
use super::parity_manifest::{CoverageKey, Interaction, Report, Surface};
use crate::inspector::generated::{effect_value, row};
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::{AppCommand, ClipId};
use klipp_effects::registry::presentation::{Cond, Presentation, Widget};
use klipp_effects::registry::{effect_types, EffectType, ParamDesc, ParamKind};
use klipp_effects::BuiltinEffect;
use serde_json::Value;

/// What the edit host would send on: dropped.
struct Sink;
impl InspectorSideEffects for Sink {
    fn sync_sequence(&self, _state: &AppState) {}
    fn send(&self, _command: AppCommand) {}
}

/// One clip holding one effect: a clone each for the stack and its twin,
/// so their pictures compare down to every id.
fn fixture(effect: &BuiltinEffect) -> klipp_timeline::Clip {
    let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
    let mut clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    clip.push_effect(effect.clone());
    clip
}

/// One state, the generated stack drawn over its clip and driven.
struct Stack {
    state: AppState,
    clip: ClipId,
    ctx: egui::Context,
    time: f64,
    unapplied: Vec<String>,
}

impl Stack {
    fn new(clip: klipp_timeline::Clip) -> Self {
        let id = clip.id;
        let mut state = AppState::default();
        state.sequences[0].tracks[0].clips.push(clip);
        state.use_generated_stack = true;
        Stack { state, clip: id, ctx: egui::Context::default(), time: 0.0, unapplied: Vec::new() }
    }

    fn clip(&self) -> &klipp_timeline::Clip {
        self.state.sequences[0].all_clips().find(|c| c.id == self.clip).unwrap()
    }

    fn picture(&self) -> klipp_timeline::LiftedPicture {
        (*self.clip().picture_v2().unwrap()).clone()
    }

    /// One frame with `events`: the text drawn, at its centre. The
    /// commands the stack sent are applied as the app applies them.
    fn frame(&mut self, events: Vec<egui::Event>) -> Vec<(String, egui::Pos2)> {
        self.time += 0.05;
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(520.0, 3000.0))), time: Some(self.time), events, ..Default::default() };
        let mut cmds = Vec::new();
        let (state, clip_id) = (&self.state, self.clip);
        let out = self.ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let clip = state.sequences[0].all_clips().find(|c| c.id == clip_id).unwrap();
                crate::inspector::stack_view::show(ui, &KlippTheme::default(), state, clip, &mut cmds, 0.0);
            });
        });
        for cmd in cmds {
            self.apply(&cmd);
        }
        out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)), _ => None }).collect()
    }

    /// `cmd` as the app applies it: a path command by the state, an index
    /// command by the inspector edit host, after the dispatch boundary.
    fn apply(&mut self, cmd: &AppCommand) {
        self.state.toast = Default::default();
        if self.state.path_command(cmd).is_some() {
            if let Some(why) = self.state.toast.message_for_test() {
                self.unapplied.push(format!("{cmd:?} refused: {why}"));
            }
            return;
        }
        self.state.before_dispatch(cmd);
        if !(InspectorEditHost { state: &mut self.state, side: &Sink }).apply(cmd) {
            self.unapplied.push(format!("{cmd:?}"));
        }
    }

    /// Click `text`: its last drawn occurrence (an open menu is drawn over
    /// the panel), after a frame to settle.
    fn click(&mut self, text: &str) -> Result<(), String> {
        self.frame(Vec::new());
        let at = self.frame(Vec::new()).into_iter().rev().find(|(t, _)| t == text).map(|(_, p)| p).ok_or_else(|| format!("{text:?} isn't drawn"))?;
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        self.frame(vec![egui::Event::PointerMoved(at), button(true)]);
        self.frame(vec![egui::Event::PointerMoved(at), button(false)]);
        self.frame(Vec::new());
        Ok(())
    }

    /// Undo (or redo) one step, through the document as the app does.
    fn step(&mut self, forward: bool) -> Result<(), String> {
        let history = self.state.active_history_mut();
        let entry = if forward { history.redo() } else { history.undo() }.ok_or("no step to take")?;
        let mut doc = klipp_document::apply::DocumentView { sequences: &mut self.state.sequences, active: self.state.active_sequence_idx, media_pool: &mut self.state.media_pool, fps: &mut self.state.fps };
        if forward { doc.apply_forward(&entry.command); } else { doc.apply_inverse(&entry.command); }
        Ok(())
    }
}

/// `effect` with `desc`'s stored field set to `value`.
fn with_field(effect: &BuiltinEffect, desc: &ParamDesc, value: Value) -> Result<BuiltinEffect, String> {
    let mut entry = serde_json::to_value(effect).map_err(|e| e.to_string())?;
    match desc.index {
        Some(i) => entry[desc.field][i] = value,
        None => entry[desc.field] = value,
    }
    serde_json::from_value(entry).map_err(|e| format!("{} set: {e}", desc.field))
}

fn stored(effect: &BuiltinEffect, desc: &ParamDesc) -> Value {
    effect_value(&serde_json::to_value(effect).unwrap_or(Value::Null), desc)
}

/// What the generated row writes for `desc` showing `index` (a switch's
/// off or on, a menu's item), in the form `was` is stored: the row's own
/// rule, restated here so a drift in the row fails the parity.
fn stored_choice(desc: &ParamDesc, was: &Value, index: usize) -> Value {
    match (desc.kind, was) {
        (ParamKind::Choice(names), Value::String(_)) => Value::from(names[index]),
        (_, Value::Bool(_)) => Value::Bool(index == 1),
        _ => Value::from(index as u32),
    }
}

/// The index a switch or menu shows for `value`.
fn shown_index(desc: &ParamDesc, value: &Value) -> Option<usize> {
    match desc.kind {
        ParamKind::Choice(_) => row::choice_index(desc, value),
        _ => row::number(value).map(|v| usize::from(v != 0.0)),
    }
}

/// The type's sample with every condition `pres` names met, so its row
/// shows and is editable, and the fixture's name.
fn shown_fixture(ty: &EffectType, pres: &Presentation) -> Result<(BuiltinEffect, &'static str), String> {
    let mut effect = (ty.sample)();
    let mut variant = "sample";
    for Cond::ParamIs { key, value } in [pres.visible_if, pres.enabled_if].into_iter().flatten() {
        let desc = ty.desc.params.iter().find(|p| p.key == key).ok_or_else(|| format!("its condition names {key:?}, not a parameter"))?;
        let want = value as usize;
        if shown_index(desc, &stored(&effect, desc)) != Some(want) {
            effect = with_field(&effect, desc, stored_choice(desc, &stored(&effect, desc), want))?;
            variant = "conditions met";
        }
    }
    Ok((effect, variant))
}

/// A menu's item labels, as the generated row draws them.
fn menu_labels(desc: &ParamDesc, pres: &Presentation) -> Vec<&'static str> {
    match (desc.kind, pres.labels) {
        (_, Some(labels)) => labels.to_vec(),
        (ParamKind::Choice(names), None) => names.to_vec(),
        _ => Vec::new(),
    }
}

/// One discrete edit through the generated stack, `drive` working its
/// control: the picture the old route's update leaves (the field set to
/// `want`), in one undo step, undone and redone exactly.
fn edit_case(effect: &BuiltinEffect, desc: &ParamDesc, want: Value, drive: impl FnOnce(&mut Stack) -> Result<(), String>) -> Result<(), String> {
    let clip = fixture(effect);
    let (mut new, mut old) = (Stack::new(clip.clone()), Stack::new(clip));
    let before = new.picture();
    drive(&mut new)?;
    old.apply(&AppCommand::UpdateBuiltinEffect { clip_id: old.clip, index: 0, effect: with_field(effect, desc, want)? });
    if !new.unapplied.is_empty() || !old.unapplied.is_empty() {
        return Err(format!("unapplied: new {:?}, old {:?}", new.unapplied, old.unapplied));
    }
    let after = new.picture();
    if after == before {
        return Err("the control changed nothing".into());
    }
    if after != old.picture() {
        return Err(format!("not the old route's picture: new {:?}, old {:?}", new.clip().builtin_effects(), old.clip().builtin_effects()));
    }
    let (un, uo) = (new.state.active_history().undo_count(), old.state.active_history().undo_count());
    if (un, uo) != (1, 1) {
        return Err(format!("undo depth new {un}, old {uo}, not one each"));
    }
    new.step(false)?;
    if new.picture() != before {
        return Err("undo doesn't give back the picture before".into());
    }
    new.step(true)?;
    if new.picture() != after {
        return Err("redo doesn't give back the picture after".into());
    }
    Ok(())
}

/// Every switch clicked both ways and every menu item picked, on every
/// effect the stack draws as a card, each against the old route.
pub(super) fn run(report: &mut Report) {
    for ty in effect_types() {
        // A placed kind is the clip's own property, edited in the Video tab.
        if crate::inspector::stack_view::is_placed_kind(ty.tag) {
            continue;
        }
        for desc in ty.desc.params {
            let pres = ty.presentation(desc);
            if !matches!(pres.widget, Widget::Toggle | Widget::Combo) {
                continue;
            }
            let id = ty.desc.id;
            let (effect, variant) = match shown_fixture(ty, &pres) {
                Ok(f) => f,
                Err(e) => {
                    report.case(&format!("{id} {}", desc.key), [], || Err(e));
                    continue;
                }
            };
            let key = |interaction| CoverageKey::new(id, desc.key, Surface::EffectsStack, variant, interaction);
            let label = pres.label.unwrap_or(desc.label);
            let was = stored(&effect, desc);
            match pres.widget {
                Widget::Toggle => {
                    for on in [false, true] {
                        let name = format!("{id} {} switched {}", desc.key, if on { "on" } else { "off" });
                        report.case(&name, [key(Interaction::Toggle(on)), key(Interaction::UndoRedo)], || {
                            // From the other way, so the click turns it `on`.
                            let start = with_field(&effect, desc, stored_choice(desc, &was, usize::from(!on)))?;
                            edit_case(&start, desc, stored_choice(desc, &was, usize::from(on)), |s| s.click(label))
                        });
                    }
                }
                Widget::Combo => {
                    let labels = menu_labels(desc, &pres);
                    for (i, item) in labels.iter().enumerate() {
                        let name = format!("{id} {} picks {item:?}", desc.key);
                        report.case(&name, [key(Interaction::ComboSelect(i)), key(Interaction::UndoRedo)], || {
                            // From another item, so the pick changes it.
                            let from = (i + 1) % labels.len();
                            let start = with_field(&effect, desc, stored_choice(desc, &was, from))?;
                            edit_case(&start, desc, stored_choice(desc, &was, i), |s| {
                                s.click(labels[from])?;
                                s.click(item)
                            })
                        });
                    }
                }
                _ => {}
            }
        }
    }
}

/// Every switch and menu row on a card yields its cases, so a row the
/// stack can't drive is a failure, not a missing count.
#[test]
fn effect_switches_and_menus_edit_as_the_old_route() {
    let mut report = Report::default();
    report.family("effect controls", run);
    report.family_result().unwrap();
    assert!(!report.passed.is_empty(), "no switch or menu row was driven");
}
