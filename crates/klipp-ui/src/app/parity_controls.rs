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
use klipp_timeline::GradeAccess;
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

    /// `cmd` as the app applies it, past the dispatch boundary: a path
    /// command by the state, an index command by the inspector edit host.
    fn apply(&mut self, cmd: &AppCommand) {
        self.state.before_dispatch(cmd);
        self.state.toast = Default::default();
        if self.state.path_command(cmd).is_some() {
            if let Some(why) = self.state.toast.message_for_test() {
                self.unapplied.push(format!("{cmd:?} refused: {why}"));
            }
            return;
        }
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
        // The undo or redo passes the dispatch boundary first, as the app's
        // does, which closes any drag still open.
        self.state.before_dispatch(&if forward { AppCommand::Redo } else { AppCommand::Undo });
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

/// One grade node drawn by the per-node layout, the generated view's
/// (`grade_view`) or the old panel's, and driven.
pub(super) struct Grade {
    pub(super) state: AppState,
    pub(super) clip: ClipId,
    pub(super) node: klipp_nodes::NodeId,
    ctx: egui::Context,
    clock: f64,
    panel: crate::panels::node_inspector::NodeInspectorPanel,
    generated: bool,
    pub(super) unapplied: Vec<String>,
    /// Requests the app runs off the panel (a mask region tracked, its
    /// tracker taken off), recorded here, in order, not applied.
    pub(super) side: Vec<String>,
    /// Which of the grade's pages draws the node.
    pub(super) page: Page,
    /// The grade as it was before this frame's input: what an old-panel
    /// gesture's step starts from, as the app keeps it
    /// (`keep_graph_before_input`), since the panel writes its value into
    /// the live graph before the gesture's Begin arrives.
    before_input: Option<klipp_nodes::NodeGraph>,
    /// An old-panel gesture's start (`BeginNodeParamEdit`).
    gesture: Option<klipp_nodes::NodeGraph>,
}

/// The grade pages a node's controls are drawn on.
#[derive(Clone, Copy, Debug)]
pub(super) enum Page {
    /// The per-node layout (`grade_view` and the old panel's `show`).
    PerNode,
    /// The Corrector's tabbed page in the right inspector, on a tab.
    Tabbed(crate::state::ColorTab),
    /// The Colour page's bottom tools, the same rows laid out wide.
    Bottom(crate::state::ColorTab),
}

impl Grade {
    pub(super) fn new(state: AppState, clip: ClipId, node: klipp_nodes::NodeId, generated: bool) -> Self {
        // The icons load as in the app: a failed one is drawn as a warning
        // filling the panel, which pushes the rows off it.
        let ctx = egui::Context::default();
        crate::icons::install(&ctx);
        Grade { state, clip, node, ctx, clock: 0.0, panel: Default::default(), generated, unapplied: Vec::new(), side: Vec::new(), page: Page::PerNode, before_input: None, gesture: None }
    }

    /// A fresh state, no history, holding a clone of this one's clip and files.
    pub(super) fn twin(&self, generated: bool) -> Self {
        let mut state = AppState::default();
        let track = self.state.sequences[0].tracks.iter().position(|t| t.clips.iter().any(|c| c.id == self.clip)).unwrap();
        state.sequences[0].tracks[track].clips.push(self.clip().clone());
        // Its files too: a clip's file settings (its camera encoding) are
        // the file's.
        state.media_pool = self.state.media_pool.clone();
        Grade { page: self.page, ..Grade::new(state, self.clip, self.node, generated) }
    }

    /// The same, on `page`.
    pub(super) fn on(self, page: Page) -> Self {
        Grade { page, ..self }
    }

    pub(super) fn clip(&self) -> &klipp_timeline::Clip {
        self.state.sequences[0].all_clips().find(|c| c.id == self.clip).unwrap()
    }

    pub(super) fn picture(&self) -> klipp_timeline::LiftedPicture {
        (*self.clip().picture_v2().unwrap()).clone()
    }

    /// One frame with `events`: the text drawn, at its centre. The
    /// generated view's commands are applied as the app dispatches them;
    /// the old panel edits the clip's grade in place, as the app lets it.
    pub(super) fn frame(&mut self, events: Vec<egui::Event>) -> Vec<(String, egui::Pos2)> {
        self.clock += 0.05;
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(420.0, 2400.0))), time: Some(self.clock), events, ..Default::default() };
        let frame = crate::inspector::keyframe_selection::FrameSelection { time_secs: 0.5, tolerance: 0.02 };
        let theme = KlippTheme::default();
        let mut cmds = Vec::new();
        self.before_input = Some(self.clip().grade().clone());
        let Grade { state, clip, node, ctx, panel, generated, page, .. } = self;
        let (clip_id, node, page) = (*clip, *node, *page);
        let out = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| match page {
                Page::PerNode if *generated => {
                    let clip = state.sequences[0].all_clips().find(|c| c.id == clip_id).unwrap().clone();
                    crate::inspector::grade_view::show(ui, &theme, &clip, node, &mut cmds, frame, Some(panel));
                }
                Page::PerNode => {
                    let clip = state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip_id)).unwrap();
                    cmds.extend(clip.edit_grade(|g| panel.show(ui, &Some(node), g, clip_id, &theme, frame)));
                }
                // The tabbed page and the bottom tools as the app draws them
                // (`main_layout`): the panel over the clip's grade, its
                // shared rows the generated ones when `generated`.
                Page::Tabbed(tab) | Page::Bottom(tab) => {
                    let clip = state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip_id)).unwrap();
                    panel.generated = crate::inspector::grade_view::snapshot_for(clip, *generated, false, false);
                    let (mut selected, mut tab) = (Some(node), tab);
                    let wide = matches!(page, Page::Bottom(_));
                    cmds.extend(clip.edit_grade(|g| if wide {
                        panel.show_simple_horizontal(ui, g, clip_id, &mut selected, &mut tab, &theme, frame)
                    } else {
                        panel.show_simple(ui, g, clip_id, &mut selected, &mut tab, &theme, frame)
                    }));
                    panel.generated = None;
                }
            });
        });
        for mut cmd in cmds {
            crate::inspector::keyframe_selection::resolve_command(&self.state.sequences[0], &mut cmd);
            // The dispatch boundary first, for every command, as the app's
            // dispatch passes it.
            self.state.before_dispatch(&cmd);
            self.state.toast = Default::default();
            match self.state.path_command(&cmd) {
                Some(_) => {
                    if let Some(why) = self.state.toast.message_for_test() {
                        self.unapplied.push(format!("{cmd:?} refused: {why}"));
                    }
                }
                None => {
                    if matches!(cmd, AppCommand::TrackMaskRegion { .. } | AppCommand::RemoveMaskTracking { .. }) {
                        self.side.push(format!("{cmd:?}"));
                        continue;
                    }
                    // An old-panel gesture, recorded as the colour dispatch
                    // records it: one graph step, from the grade as it was
                    // before the input that began it.
                    match &cmd {
                        AppCommand::BeginNodeParamEdit { .. } => {
                            if self.gesture.is_none() {
                                self.gesture = self.before_input.clone();
                            }
                            continue;
                        }
                        AppCommand::AbandonNodeParamEdit { .. } => {
                            if let Some(old_graph) = self.gesture.take() {
                                let clip = self.state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(self.clip)).unwrap();
                                clip.set_grade(old_graph);
                            }
                            continue;
                        }
                        AppCommand::EndNodeParamEdit { .. } => {
                            if let Some(old_graph) = self.gesture.take() {
                                let new_graph = self.clip().grade().clone();
                                self.state.active_history_mut().push(crate::history::HistoryEntry { command: crate::history::EditCommand::SetNodeGraph { clip_id: self.clip, old_graph, new_graph } });
                            }
                            continue;
                        }
                        _ => {}
                    }
                    // The old panel's number, as the colour dispatch writes
                    // it (`apply_override`; the history is its gesture's).
                    if let AppCommand::SetNodeParam { node_id, param, value, .. } = &cmd {
                        let clip = self.state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(self.clip)).unwrap();
                        clip.edit_grade(|g| g.nodes.iter_mut().filter(|n| n.id == *node_id).for_each(|n| { n.params.apply_override(param, *value); }));
                        continue;
                    }
                    // A curves editor's edit, as the colour dispatch writes
                    // it: the curves set, the node on, one graph step.
                    if let AppCommand::SetNodeCurves { node_id, curves, .. } = &cmd {
                        let clip = self.state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(self.clip)).unwrap();
                        let old_graph = clip.grade().clone();
                        clip.edit_grade(|g| g.nodes.iter_mut().filter(|n| n.id == *node_id).for_each(|n| { n.params.curves = Some(curves.clone()); n.enabled = true; }));
                        let new_graph = clip.grade().clone();
                        // Inside a gesture its End records the step.
                        if self.gesture.is_none() {
                            self.state.active_history_mut().push(crate::history::HistoryEntry { command: crate::history::EditCommand::SetNodeGraph { clip_id: self.clip, old_graph, new_graph } });
                        }
                        continue;
                    }
                    // A per-node editor's settings, as the app's colour
                    // dispatch writes them, recorded as one graph step.
                    let clip = self.state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(self.clip)).unwrap();
                    let old_graph = clip.grade().clone();
                    if clip.edit_grade(|g| super::colour::write_node_settings(g, &cmd)) {
                        let new_graph = clip.grade().clone();
                        self.state.active_history_mut().push(crate::history::HistoryEntry { command: crate::history::EditCommand::SetNodeGraph { clip_id: self.clip, old_graph, new_graph } });
                        continue;
                    }
                    if !(InspectorEditHost { state: &mut self.state, side: &Sink }).apply(&cmd) {
                        self.unapplied.push(format!("{cmd:?}"));
                    }
                }
            }
        }
        out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)), _ => None }).collect()
    }

    pub(super) fn click_at(&mut self, at: egui::Pos2) {
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        self.frame(vec![egui::Event::PointerMoved(at), button(true)]);
        self.frame(vec![egui::Event::PointerMoved(at), button(false)]);
        self.frame(vec![egui::Event::PointerGone]);
    }

    /// The settled frame's texts.
    pub(super) fn drawn(&mut self) -> Vec<(String, egui::Pos2)> {
        self.frame(Vec::new());
        self.frame(Vec::new())
    }

    /// Click the switch labelled `label`: its last drawn occurrence, the
    /// node's header coming first.
    fn switch(&mut self, label: &str) -> Result<(), String> {
        let at = self.drawn().into_iter().rev().find(|(t, _)| t == label).map(|(_, p)| p).ok_or_else(|| format!("no switch {label:?}"))?;
        self.click_at(at);
        Ok(())
    }

    /// Open the menu on the row labelled `label` (by the text it shows
    /// right of the label, so two menus showing one name aren't confused)
    /// and pick `item` from it.
    fn pick(&mut self, label: &str, shows: &str, item: &str) -> Result<(), String> {
        let (label, shows, item) = if self.generated { (label.to_owned(), shows, item) } else { old_menu(label, shows, item) };
        let label = label.as_str();
        let drawn = self.drawn();
        let Some(row) = drawn.iter().rev().find(|(t, _)| t == label).map(|(_, p)| *p) else {
            // A choice drawn as buttons side by side, with no row label (the
            // tabbed page's Linear / Log): the item is clicked where it is.
            let item_at = drawn.iter().rev().find(|(t, _)| t == item).map(|(_, p)| *p);
            let beside = item_at.is_some_and(|i| drawn.iter().any(|(t, p)| t == shows && (p.y - i.y).abs() < 6.0));
            let Some(item_at) = item_at.filter(|_| beside) else { return Err(format!("no row {label:?}")) };
            self.click_at(item_at);
            return Ok(());
        };
        let at = drawn.iter().filter(|(t, p)| t == shows && (p.y - row.y).abs() < 6.0 && p.x > row.x).map(|(_, p)| *p).next().ok_or_else(|| format!("row {label:?} doesn't show {shows:?}"))?;
        self.click_at(at);
        let item_at = self.drawn().into_iter().rev().find(|(t, _)| t == item).map(|(_, p)| p).ok_or_else(|| format!("{item:?} isn't in the menu"))?;
        self.click_at(item_at);
        Ok(())
    }

    pub(super) fn step(&mut self, forward: bool) -> Result<(), String> {
        // The undo or redo passes the dispatch boundary first, as the app's
        // does, which closes any drag still open.
        self.state.before_dispatch(&if forward { AppCommand::Redo } else { AppCommand::Undo });
        let history = self.state.active_history_mut();
        let entry = if forward { history.redo() } else { history.undo() }.ok_or("no step to take")?;
        let mut doc = klipp_document::apply::DocumentView { sequences: &mut self.state.sequences, active: self.state.active_sequence_idx, media_pool: &mut self.state.media_pool, fps: &mut self.state.fps };
        if forward { doc.apply_forward(&entry.command); } else { doc.apply_inverse(&entry.command); }
        Ok(())
    }

    /// The node's stored value for `desc`, as its rows read it.
    fn value(&self, desc: &ParamDesc) -> Value {
        let node = self.clip().grade().nodes.iter().find(|n| n.id == self.node).cloned().unwrap();
        let entry = serde_json::json!({ "params": node.params });
        crate::inspector::generated::grade_value(&entry, desc)
    }
}

/// The old panel's own words for a menu the generated row labels from the
/// catalog: its row label and its item names, where they differ. The
/// items are the same choices in the same order.
/// - Combine names each operator with its sets ("Intersect (A and B)").
/// - A LUT's spaces read "Rec.709 / sRGB" and "BT.2020", and the row
///   label ends in a colon. Both menus offer "Use LUT default" (unset),
///   the old one first and the generated row last, as the catalog's
///   fourth choice.
fn old_menu<'a>(label: &str, shows: &'a str, item: &'a str) -> (String, &'a str, &'a str) {
    let combine = |s: &'a str| match s {
        "Intersect" => "Intersect (A and B)",
        "Union" => "Union (A or B)",
        "Subtract" => "Subtract (A minus B)",
        other => other,
    };
    let space = |s: &'a str| match s {
        "Rec.709 linear" => "Rec.709 / sRGB",
        "BT.2020 linear" => "BT.2020",
        other => other,
    };
    match label {
        "Combine" => (label.to_owned(), combine(shows), combine(item)),
        "Input space" | "Output space" => (format!("{label}:"), space(shows), space(item)),
        _ => (label.to_owned(), shows, item),
    }
}

/// A discrete grade edit, `drive` working the control in both views from
/// one `start` clip: the generated view's picture must be the old panel's,
/// in one undo step there, undone and redone exactly.
fn grade_case(start: &Grade, drive: impl Fn(&mut Grade) -> Result<(), String>) -> Result<(), String> {
    let (mut new, mut old) = (start.twin(true), start.twin(false));
    // The picture as the page shows it, drawn once (a page may write on
    // its first draw, as the tabbed page's curves do).
    new.drawn();
    old.drawn();
    let before = new.picture();
    drive(&mut new)?;
    drive(&mut old).map_err(|e| format!("the old panel: {e}"))?;
    if !new.unapplied.is_empty() || !old.unapplied.is_empty() {
        return Err(format!("unapplied: new {:?}, old {:?}", new.unapplied, old.unapplied));
    }
    let after = new.picture();
    if after == before {
        return Err("the control changed nothing".into());
    }
    if after != old.picture() {
        let node = |g: &Grade| g.clip().grade().nodes.iter().find(|n| n.id == g.node).map(|n| format!("{:?}", n.params)).unwrap_or_default();
        return Err(format!("not the old panel's picture: new {}, old {}", node(&new), node(&old)));
    }
    if new.state.active_history().undo_count() != 1 {
        return Err(format!("{} undo steps, not one", new.state.active_history().undo_count()));
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

/// A clip graded with one `kind` node, placed as the app places it: a
/// matte source (a Qualifier, a key, a Combine, an External Matte) where
/// a windowed secondary takes its matte (`ensure_windowed_secondary`;
/// alone in the chain a matte source writes its matte where the picture
/// should be, which the graph refuses), anything else after the
/// Corrector (`grade_inspector_tests::graded`).
pub(super) fn placed(kind: klipp_nodes::NodeKind) -> Option<(AppState, ClipId, klipp_nodes::NodeId)> {
    use klipp_nodes::{NodeGraph, NodeKind};
    let ty = klipp_nodes::registry::node_type(kind)?;
    let matte_out = ty.desc.outputs.iter().any(|p| p.ty == klipp_effects::registry::PortType::Matte);
    if !matte_out {
        return super::grade_inspector_tests::graded(kind);
    }
    let mut g = NodeGraph::default();
    let (qualifier, mask, _) = g.ensure_windowed_secondary(NodeKind::Corrector);
    let node = match kind {
        NodeKind::Qualifier => qualifier,
        NodeKind::Mask => mask,
        NodeKind::CombineMattes => g.nodes.iter().find(|n| n.kind == kind)?.id,
        // A key of one picture input stands where the Qualifier stood; a
        // matte of none (read from a file) too, without the picture.
        _ if ty.desc.inputs.is_empty() || (ty.desc.inputs.len() == 1 && ty.desc.inputs[0].ty == klipp_effects::registry::PortType::Image) => {
            let n = g.nodes.iter_mut().find(|n| n.id == qualifier)?;
            n.kind = kind;
            n.label = kind.display_name().to_string();
            if ty.desc.inputs.is_empty() {
                g.edges.retain(|e| e.to_node != qualifier);
            }
            qualifier
        }
        _ => return None,
    };
    let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 25);
    let mut clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    clip.set_grade(g);
    clip.picture_v2().ok()?;
    let mut state = AppState::default();
    let v1 = state.sequences[0].tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
    let id = clip.id;
    state.sequences[0].tracks[v1].clips.push(clip);
    Some((state, id, node))
}

/// Every switch clicked both ways and every menu item picked on every
/// grade node the per-node layout draws, the generated view against the
/// old panel.
pub(super) fn run_grade(report: &mut Report) {
    for ty in klipp_nodes::registry::node_types() {
        if !super::main_layout::per_node_kind(ty.kind) {
            continue;
        }
        let id = ty.desc.id;
        // Only a type with a switch or a menu (not the Source and Output).
        if !ty.desc.params.iter().any(|p| matches!(ty.presentation(p).widget, Widget::Toggle | Widget::Combo)) {
            continue;
        }
        let Some((state, clip, node)) = placed(ty.kind) else {
            report.case(id, [], || Err("the grade won't take one".into()));
            continue;
        };
        let base = Grade::new(state, clip, node, true);
        for desc in ty.desc.params {
            discrete_cases(report, ty, desc, &base, Surface::GradePerNode);
        }
    }
}

/// A grade node's switch or menu row (`desc`), drawn on `base`'s page and
/// counted on `surface`: clicked both ways or every item picked, each
/// from the other value, through the generated view and the old panel;
/// nothing for a row of another widget.
fn discrete_cases(report: &mut Report, ty: &klipp_nodes::registry::NodeType, desc: &'static ParamDesc, base: &Grade, surface: Surface) {
    let id = ty.desc.id;
    let pres = ty.presentation(desc);
    if !matches!(pres.widget, Widget::Toggle | Widget::Combo) {
        return;
    }
    let label = pres.label.unwrap_or(desc.label);
    // Meet the row's conditions by clicking their switches.
    let mut shown = base.twin(true);
    let mut variant = "sample";
    let mut setup = Ok(());
    for Cond::ParamIs { key, value } in [pres.visible_if, pres.enabled_if].into_iter().flatten() {
        let Some(cond) = ty.desc.params.iter().find(|p| p.key == key) else {
            setup = Err(format!("its condition names {key:?}, not a parameter"));
            break;
        };
        if shown_index(cond, &shown.value(cond)) != Some(value as usize) {
            let cond_label = ty.presentation(cond).label.unwrap_or(cond.label);
            setup = shown.switch(cond_label);
            variant = "conditions met";
        }
    }
    if setup.is_ok() && !shown.unapplied.is_empty() {
        setup = Err(format!("meeting its conditions: unapplied {:?}", shown.unapplied));
    }
    if let Err(e) = setup {
        report.case(&format!("{id} {}", desc.key), [], || Err(e));
        return;
    }
    let key = |interaction| CoverageKey::new(id, desc.key, surface, variant, interaction);
    match pres.widget {
        Widget::Toggle => {
            let is_on = shown_index(desc, &shown.value(desc)) == Some(1);
            for on in [false, true] {
                let name = format!("{id} {} on {surface:?} switched {}", desc.key, if on { "on" } else { "off" });
                report.case(&name, [key(Interaction::Toggle(on)), key(Interaction::UndoRedo)], || {
                    // From the other way, so the click turns it `on`.
                    let mut start = shown.twin(true);
                    if is_on == on {
                        start.switch(label)?;
                    }
                    if !start.unapplied.is_empty() {
                        return Err(format!("from the other way: unapplied {:?}", start.unapplied));
                    }
                    grade_case(&start.twin(true), |g| g.switch(label))
                });
            }
        }
        Widget::Combo => {
            let labels = menu_labels(desc, &pres);
            let current = shown_index(desc, &shown.value(desc)).unwrap_or(0);
            for (i, item) in labels.iter().enumerate() {
                let name = format!("{id} {} on {surface:?} picks {item:?}", desc.key);
                report.case(&name, [key(Interaction::ComboSelect(i)), key(Interaction::UndoRedo)], || {
                    // From another item, so the pick changes it.
                    let mut start = shown.twin(true);
                    let from = if i == current { (i + 1) % labels.len() } else { current };
                    if from != current {
                        start.pick(label, labels[current], labels[from])?;
                    }
                    if !start.unapplied.is_empty() {
                        return Err(format!("from another item: unapplied {:?}", start.unapplied));
                    }
                    grade_case(&start.twin(true), |g| g.pick(label, labels[from], item))
                });
            }
        }
        _ => {}
    }
}

/// The Corrector's tab for each of its switches and menus, as the tabbed
/// page lays them out.
const CORRECTOR_TABS: &[(&str, crate::state::ColorTab)] = &[("wheels_log_mode", crate::state::ColorTab::Wheels), ("curves_preserve_luma", crate::state::ColorTab::Curves)];

/// The Corrector's switches and menus on the pages that share its rows:
/// the tabbed page in the right inspector, and the Colour page's bottom
/// tools. A real Corrector, on its tab, through the generated rows and
/// the old page alike.
pub(super) fn run_grade_pages(report: &mut Report) {
    let ty = klipp_nodes::registry::node_type(klipp_nodes::NodeKind::Corrector).unwrap();
    let Some((state, clip, node)) = super::grade_inspector_tests::graded(klipp_nodes::NodeKind::Corrector) else {
        report.case(ty.desc.id, [], || Err("the grade won't take a Corrector".into()));
        return;
    };
    let base = Grade::new(state, clip, node, true);
    for desc in ty.desc.params {
        if !matches!(ty.presentation(desc).widget, Widget::Toggle | Widget::Combo) {
            continue;
        }
        let Some(&(_, tab)) = CORRECTOR_TABS.iter().find(|(k, _)| *k == desc.key) else {
            report.case(&format!("{} {}", ty.desc.id, desc.key), [], || Err("no tab known for it".into()));
            continue;
        };
        for (page, surface) in [(Page::Tabbed(tab), Surface::GradeTabbedRight), (Page::Bottom(tab), Surface::GradeBottomTools)] {
            let on_page = Grade { page, ..base.twin(true) };
            discrete_cases(report, ty, desc, &on_page, surface);
        }
    }
}

/// The Corrector's switches and menus on its tabbed page and the bottom
/// tools edit as the old page does.
#[test]
fn corrector_page_switches_and_menus_edit_as_the_old_page() {
    let mut report = Report::default();
    report.family("grade pages", run_grade_pages);
    report.family_result().unwrap();
    assert!(!report.passed.is_empty(), "no switch or menu row was driven");
}

/// Every switch and menu row of a per-node grade yields its cases, so a
/// row neither view can drive is a failure, not a missing count.
#[test]
fn grade_switches_and_menus_edit_as_the_old_panel() {
    let mut report = Report::default();
    report.family("grade controls", run_grade);
    report.family_result().unwrap();
    assert!(!report.passed.is_empty(), "no switch or menu row was driven");
}

/// The tabbed page's tabs, each drawn on both pages.
const TABS: [crate::state::ColorTab; 5] = [crate::state::ColorTab::Adjustments, crate::state::ColorTab::Wheels, crate::state::ColorTab::Curves, crate::state::ColorTab::HueSat, crate::state::ColorTab::Warper];

/// Opening any tab of the Corrector's pages writes nothing, in either
/// view: the picture as it was before the first draw, no step, nothing
/// sent that wasn't applied. The Curves, Hue/Sat and Warper editors wrote
/// their defaults on their first draw (#1392).
#[test]
fn opening_a_corrector_page_tab_writes_nothing() {
    let (state, clip, node) = super::grade_inspector_tests::graded(klipp_nodes::NodeKind::Corrector).unwrap();
    let base = Grade::new(state, clip, node, true);
    let mut wrote = Vec::new();
    for tab in TABS {
        for page in [Page::Tabbed(tab), Page::Bottom(tab)] {
            for generated in [false, true] {
                let mut g = Grade { page, ..base.twin(generated) };
                let before = g.picture();
                g.drawn();
                g.drawn();
                if g.picture() != before || g.state.active_history().undo_count() != 0 || !g.unapplied.is_empty() {
                    wrote.push(format!("{page:?} generated {generated}: steps {}, unapplied {:?}", g.state.active_history().undo_count(), g.unapplied));
                }
            }
        }
    }
    assert!(wrote.is_empty(), "opening wrote:\n{}", wrote.join("\n"));
}

/// A curve edited after its tab is opened (a point dragged in over
/// several ticks) is one undo step in each view (IC13),
/// and undoing it leaves the curves unset, as they were, not their
/// stored defaults (#1392).
#[test]
fn a_curve_edit_after_opening_is_one_step_back_to_unset() {
    let (state, clip, node) = super::grade_inspector_tests::graded(klipp_nodes::NodeKind::Corrector).unwrap();
    let base = Grade::new(state, clip, node, true);
    let curves = |g: &Grade| g.clip().grade().nodes.iter().find(|n| n.id == g.node).map(|n| n.params.curves.clone()).unwrap();
    for page in [Page::Tabbed(crate::state::ColorTab::Curves), Page::Bottom(crate::state::ColorTab::Curves)] {
        for generated in [false, true] {
            let mut g = Grade { page, ..base.twin(generated) };
            assert!(curves(&g).is_none(), "the fixture's curves are unset");
            let drawn = g.drawn();
            // A point dragged up from an empty place in the editor's square,
            // below its channel strip: a drag there adds a point and moves it.
            let strip = drawn.iter().filter(|(t, _)| ["M", "R", "G", "B"].contains(&t.as_str())).map(|(_, p)| p.y).fold(f32::MIN, f32::max);
            assert!(strip > f32::MIN, "{page:?}: no channel strip");
            let mut y = strip + 60.0;
            while curves(&g).is_none() && y < strip + 400.0 {
                let at = egui::pos2(150.0, y);
                let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
                // A drag of several ticks: one gesture, so one step (IC13).
                g.frame(vec![egui::Event::PointerMoved(at), button(at, true)]);
                for dy in [8.0, 16.0, 24.0] {
                    g.frame(vec![egui::Event::PointerMoved(at - egui::vec2(0.0, dy))]);
                }
                g.frame(vec![button(at - egui::vec2(0.0, 24.0), false)]);
                g.frame(vec![egui::Event::PointerGone]);
                y += 40.0;
            }
            assert!(curves(&g).is_some(), "{page:?} generated {generated}: no click edited the curve");
            assert!(g.unapplied.is_empty(), "{:?}", g.unapplied);
            assert_eq!(g.state.active_history().undo_count(), 1, "{page:?} generated {generated}: one step");
            g.step(false).unwrap();
            assert!(curves(&g).is_none(), "{page:?} generated {generated}: undo leaves the curves unset");
        }
    }
}


/// A Colour Warper sector moved on the Corrector's pages is one step in
/// each view, and undoing it leaves the warper unset, as it was: its
/// sector commands (`SetNodeParam`, inside the drag's gesture) carry the
/// edit, and the gesture starts from the grade before the press (#1392).
#[test]
fn a_warper_move_after_opening_is_one_step_back_to_unset() {
    let (state, clip, node) = super::grade_inspector_tests::graded(klipp_nodes::NodeKind::Corrector).unwrap();
    let base = Grade::new(state, clip, node, true);
    let warper = |g: &Grade| g.clip().grade().nodes.iter().find(|n| n.id == g.node).map(|n| n.params.warper_params.clone()).unwrap();
    const SECTORS: [&str; 12] = ["R", "O", "Y", "YG", "G", "CG", "C", "CB", "B", "BM", "M", "MR"];
    for page in [Page::Tabbed(crate::state::ColorTab::Warper), Page::Bottom(crate::state::ColorTab::Warper)] {
        for generated in [false, true] {
            let mut g = Grade { page, ..base.twin(generated) };
            assert!(warper(&g).is_none(), "the fixture's warper is unset");
            let drawn = g.drawn();
            // The wheel's centre, among its sector labels; its points lie
            // between the centre and each label.
            let labels: Vec<egui::Pos2> = drawn.iter().filter(|(t, _)| SECTORS.contains(&t.as_str())).map(|(_, p)| *p).collect();
            assert!(labels.len() >= 12, "{page:?}: the sector labels aren't drawn");
            let centre = labels.iter().fold(egui::Vec2::ZERO, |s, p| s + p.to_vec2()) / labels.len() as f32;
            let centre = centre.to_pos2();
            let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            'found: for label in &labels {
                for k in [0.2, 0.35, 0.5, 0.65, 0.8] {
                    let at = centre + (*label - centre) * k;
                    let dir = (*label - centre).normalized() * 6.0;
                    g.frame(vec![egui::Event::PointerMoved(at), button(at, true)]);
                    for i in 1..=3 {
                        g.frame(vec![egui::Event::PointerMoved(at + dir * i as f32)]);
                    }
                    g.frame(vec![button(at + dir * 3.0, false)]);
                    g.frame(vec![egui::Event::PointerGone]);
                    if warper(&g).is_some() {
                        break 'found;
                    }
                }
            }
            assert!(warper(&g).is_some(), "{page:?} generated {generated}: no drag moved a sector");
            assert!(g.unapplied.is_empty(), "{:?}", g.unapplied);
            assert_eq!(g.state.active_history().undo_count(), 1, "{page:?} generated {generated}: one step");
            g.step(false).unwrap();
            assert!(warper(&g).is_none(), "{page:?} generated {generated}: undo leaves the warper unset");
        }
    }
}

/// Escape during a curve drag takes the drag back: the curves as they
/// were (unset), and no undo step, in each view.
#[test]
fn escape_takes_back_a_curve_drag() {
    let (state, clip, node) = super::grade_inspector_tests::graded(klipp_nodes::NodeKind::Corrector).unwrap();
    let base = Grade::new(state, clip, node, true);
    let curves = |g: &Grade| g.clip().grade().nodes.iter().find(|n| n.id == g.node).map(|n| n.params.curves.clone()).unwrap();
    for page in [Page::Tabbed(crate::state::ColorTab::Curves), Page::Bottom(crate::state::ColorTab::Curves)] {
        for generated in [false, true] {
            let mut g = Grade { page, ..base.twin(generated) };
            let before = { g.drawn(); g.picture() };
            let drawn = g.drawn();
            let strip = drawn.iter().filter(|(t, _)| ["M", "R", "G", "B"].contains(&t.as_str())).map(|(_, p)| p.y).fold(f32::MIN, f32::max);
            let at = egui::pos2(150.0, strip + 140.0);
            let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            g.frame(vec![egui::Event::PointerMoved(at), button(at, true)]);
            for dy in [8.0, 16.0, 24.0] {
                g.frame(vec![egui::Event::PointerMoved(at - egui::vec2(0.0, dy))]);
            }
            assert!(curves(&g).is_some(), "{page:?} generated {generated}: the drag moved the curve");
            g.frame(vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
            g.frame(vec![button(at - egui::vec2(0.0, 24.0), false)]);
            g.frame(Vec::new());
            assert!(g.unapplied.is_empty(), "{:?}", g.unapplied);
            assert_eq!(g.state.active_history().undo_count(), 0, "{page:?} generated {generated}: no step");
            assert!(curves(&g).is_none(), "{page:?} generated {generated}: the curves as they were");
            assert!(g.picture() == before, "{page:?} generated {generated}: the picture as it was");
        }
    }
}

/// Opening any grade node the per-node layout draws writes nothing, in
/// either view: the picture as it was before the first draw, no step,
/// nothing sent. The old panel draws in place over the clip's grade, so a
/// default it stores on draw (a mask stack, a difference key, a layer
/// mixer) would change the project unrecorded (the class of #1392).
#[test]
fn opening_a_per_node_grade_writes_nothing_in_either_view() {
    let mut wrote = Vec::new();
    let mut opened = 0;
    for ty in klipp_nodes::registry::node_types() {
        if !super::main_layout::per_node_kind(ty.kind) {
            continue;
        }
        let Some((state, clip, node)) = placed(ty.kind) else { continue };
        let base = Grade::new(state, clip, node, true);
        for generated in [false, true] {
            let mut g = base.twin(generated);
            let before = g.picture();
            g.drawn();
            g.drawn();
            if g.picture() != before || g.state.active_history().undo_count() != 0 || !g.unapplied.is_empty() {
                let params = |p: &klipp_timeline::LiftedPicture| format!("{p:?}").len();
                wrote.push(format!("{} generated {generated}: steps {}, unapplied {:?} ({} → {} bytes)", ty.desc.id, g.state.active_history().undo_count(), g.unapplied, params(&before), params(&g.picture())));
            }
            opened += 1;
        }
    }
    assert!(wrote.is_empty(), "opening wrote:\n{}", wrote.join("\n"));
    assert!(opened > 20, "only {opened} opened");
}

/// A colour wheel's drag on the per-node layout (`wheel_block`) is one
/// step in each view, and Escape during it takes it back: the picture as
/// it was, and no step.
#[test]
fn a_wheel_drag_is_one_step_and_escape_takes_it_back() {
    let Some((state, clip, node)) = super::grade_inspector_tests::graded(klipp_nodes::NodeKind::ColorWheels) else { panic!("a Color Wheels node") };
    let base = Grade::new(state, clip, node, true);
    let lift = |g: &Grade| g.clip().grade().nodes.iter().find(|n| n.id == g.node).map(|n| (n.params.lift_x, n.params.lift_y)).unwrap();
    for escape in [false, true] {
        for generated in [false, true] {
            let mut g = base.twin(generated);
            let drawn = g.drawn();
            let before = g.picture();
            // The Lift wheel: its centre between its label and the readouts
            // under it, across those readouts.
            let label = drawn.iter().find(|(t, _)| t == "Lift").map(|(_, p)| *p).expect("a Lift wheel");
            let readouts: Vec<egui::Pos2> = drawn.iter().filter(|(t, p)| t.starts_with('+') && p.y > label.y).map(|(_, p)| *p).collect();
            let below = readouts.iter().map(|p| p.y).fold(f32::MAX, f32::min);
            let row: Vec<&egui::Pos2> = readouts.iter().filter(|p| (p.y - below).abs() < 4.0 && (p.x - label.x).abs() < 120.0).collect();
            let centre = egui::pos2(row.iter().map(|p| p.x).sum::<f32>() / row.len() as f32, (label.y + below) / 2.0);
            let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            g.frame(vec![egui::Event::PointerMoved(centre), button(centre, true)]);
            for dx in [6.0, 12.0, 18.0] {
                g.frame(vec![egui::Event::PointerMoved(centre + egui::vec2(dx, -dx))]);
            }
            assert_ne!(lift(&g), (None, None), "generated {generated}: the drag moved the wheel");
            if escape {
                g.frame(vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
            }
            g.frame(vec![button(centre + egui::vec2(18.0, -18.0), false)]);
            g.frame(Vec::new());
            assert!(g.unapplied.is_empty(), "{:?}", g.unapplied);
            if escape {
                assert_eq!(g.state.active_history().undo_count(), 0, "generated {generated}: Escape leaves no step");
                assert!(g.picture() == before, "generated {generated}: Escape gives back the picture");
            } else {
                assert_eq!(g.state.active_history().undo_count(), 1, "generated {generated}: one step");
                g.step(false).unwrap();
                assert!(g.picture() == before, "generated {generated}: its undo gives back the picture");
            }
        }
    }
}
