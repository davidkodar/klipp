//! M3: the switches and menus of the effects stack and the grade pages,
//! held to what they must store. Each case drives the generated view
//! through its real control (a checkbox clicked, a menu item picked) and
//! compares the whole typed picture with the expected one: the start with
//! the one field set to the value the switch or item names (the catalog's
//! stored form), written straight into the document. One undo step, and
//! undo gives back the exact picture before and redo the exact picture
//! after, through the document as the app applies them.
//!
//! No old view is drawn: the expectation comes from the catalog, never
//! from running a widget again. A key is recorded only once its case's
//! assertions pass.

use super::inspector_edit_host::{InspectorEditHost, InspectorSideEffects};
use super::parity_harness::GradeRoot;
use super::parity_manifest::{catalog, CoverageKey, Interaction, Manifest, Report, Surface};
use crate::inspector::generated::{effect_value, row};
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::{AppCommand, ClipId};
use klipp_effects::registry::presentation::{Presentation, Widget};
use klipp_effects::registry::{effect_types, EffectType, ParamDesc, ParamKind};
use klipp_effects::BuiltinEffect;
use klipp_timeline::{test_clip, GradeAccess};
use serde_json::Value;

/// What the edit host would send on: dropped.
struct Sink;
impl InspectorSideEffects for Sink {
    fn sync_sequence(&self, _state: &AppState) {}
    fn send(&self, _command: AppCommand) {}
}

/// One clip holding one effect: a clone each for the stack and its
/// oracle, so their pictures compare down to every id.
pub(super) fn fixture(effect: &BuiltinEffect) -> klipp_timeline::Clip {
    let tc = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
    let mut clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
    clip.push_effect(effect.clone());
    clip
}

/// One state, the generated stack drawn over its clip and driven.
pub(super) struct Stack {
    pub(super) state: AppState,
    pub(super) clip: ClipId,
    pub(super) ctx: egui::Context,
    time: f64,
    pub(super) unapplied: Vec<String>,
}

impl Stack {
    pub(super) fn new(clip: klipp_timeline::Clip) -> Self {
        let id = clip.id;
        let mut state = AppState::default();
        state.sequences[0].tracks[0].clips.push(clip);
        Stack { state, clip: id, ctx: egui::Context::default(), time: 0.0, unapplied: Vec::new() }
    }

    pub(super) fn clip(&self) -> &klipp_timeline::Clip {
        self.state.sequences[0].all_clips().find(|c| c.id == self.clip).unwrap()
    }

    pub(super) fn picture(&self) -> klipp_timeline::LiftedPicture {
        (*self.clip().picture_v2().unwrap()).clone()
    }

    /// One frame with `events`: the text drawn, at its centre. The
    /// commands the stack sent are applied as the app applies them.
    pub(super) fn frame(&mut self, events: Vec<egui::Event>) -> Vec<(String, egui::Pos2)> {
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
    pub(super) fn apply(&mut self, cmd: &AppCommand) {
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
    pub(super) fn click(&mut self, text: &str) -> Result<(), String> {
        self.frame(Vec::new());
        let at = self.frame(Vec::new()).into_iter().rev().find(|(t, _)| t == text).map(|(_, p)| p).ok_or_else(|| format!("{text:?} isn't drawn"))?;
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        self.frame(vec![egui::Event::PointerMoved(at), button(true)]);
        self.frame(vec![egui::Event::PointerMoved(at), button(false)]);
        self.frame(Vec::new());
        Ok(())
    }

    /// Click `text` on the row labelled `row`: a menu's shown item where
    /// another menu on the card shows the same (a transform's input and
    /// output gamuts, #1746).
    pub(super) fn click_in_row(&mut self, row: &str, text: &str) -> Result<(), String> {
        self.frame(Vec::new());
        let drawn = self.frame(Vec::new());
        let y = drawn.iter().find(|(t, _)| t == row).map(|(_, p)| p.y).ok_or_else(|| format!("{row:?} isn't drawn"))?;
        let at = drawn.iter().filter(|(t, _)| t == text).min_by(|a, b| (a.1.y - y).abs().total_cmp(&(b.1.y - y).abs())).map(|(_, p)| *p).ok_or_else(|| format!("{text:?} isn't drawn"))?;
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        self.frame(vec![egui::Event::PointerMoved(at), button(true)]);
        self.frame(vec![egui::Event::PointerMoved(at), button(false)]);
        self.frame(Vec::new());
        Ok(())
    }

    /// Undo (or redo) one step, through the document as the app does.
    pub(super) fn step(&mut self, forward: bool) -> Result<(), String> {
        // The undo or redo passes the dispatch boundary first, as the app's
        // does, which closes any drag still open.
        self.state.before_dispatch(&if forward { AppCommand::Redo } else { AppCommand::Undo });
        // The step the app takes: the sequence's or the project's (#1986).
        let entry = if forward { self.state.take_redo() } else { self.state.take_undo() }.ok_or("no step to take")?;
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
    let mut effect = super::parity_numeric::acting_sample(ty);
    let mut variant = "sample";
    for (key, value) in [pres.visible_if, pres.enabled_if].into_iter().flatten().map(|c| (c.key(), c.a_value_meeting_it())) {
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
/// control: the expected picture (the start's clip with the field set to
/// `want`, the value the control names, by the clip's effect list), in
/// one undo step, undone and redone exactly.
fn edit_case(effect: &BuiltinEffect, desc: &ParamDesc, want: Value, drive: impl FnOnce(&mut Stack) -> Result<(), String>) -> Result<(), String> {
    let clip = fixture(effect);
    let (mut new, mut oracle) = (Stack::new(clip.clone()), Stack::new(clip));
    let before = new.picture();
    drive(&mut new)?;
    oracle.apply(&AppCommand::UpdateBuiltinEffect { clip_id: oracle.clip, index: 0, effect: with_field(effect, desc, want)? });
    if !new.unapplied.is_empty() {
        return Err(format!("unapplied: {:?}", new.unapplied));
    }
    if !oracle.unapplied.is_empty() {
        return Err(format!("the expected value can't be written: {:?}", oracle.unapplied));
    }
    let after = new.picture();
    if after == before {
        return Err("the control changed nothing".into());
    }
    if after != oracle.picture() {
        return Err(format!("not the expected picture: stored {:?}, expected {:?}", test_clip::effects(new.clip()), test_clip::effects(oracle.clip())));
    }
    let steps = new.state.active_history().undo_count();
    if steps != 1 {
        return Err(format!("{steps} undo steps, not one"));
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

/// Why a stack row's drawn edit (its Pixels) is excluded, if it is: Speed
/// Ramp's retiming is the clip's speed curve, and the lift drops the
/// ramp's own settings, so no switch or menu of it moves the frame.
const SPEED_RAMP_PIXELS: &str = "known defect #1409: the lift drops the ramp, so Speed Ramp's switches and menus move nothing";

/// Switches that say what a measurement may set, not how the picture is
/// drawn: a reflection's tint and darkness follow the water only when it
/// is measured (#2795), so flipping one moves no pixel by design.
const POLICY_SWITCHES: &[(&str, &str)] = &[("reflection", "tint_auto"), ("reflection", "darken_auto")];
const POLICY_PIXELS: &str = "by design (#2795): whether the water's measurement sets the tint and darkness, which it does when measured, not when switched";

/// A stack row's switch flipped or another item picked, on the effect's
/// pixel fixture (`parity_numeric::pixel_fixture`) through the card: of
/// the row's other values, the first that moves the frame, with the CPU's
/// frame the GPU's (`clip_edit_drawn_on`, at the type's route and size).
/// An error beginning `BLOCKED` when the GPU route isn't kept.
fn stack_drawn(ty: &EffectType, desc: &ParamDesc, pres: &Presentation, effect: &BuiltinEffect) -> Result<(), String> {
    let (routes, _) = super::parity_numeric::routes_for(ty.desc.id);
    let size = super::parity_numeric::size_for(ty.tag);
    let (clip, index) = super::parity_numeric::pixel_fixture(ty, effect, Surface::EffectsStack)?;
    let label = pres.label.unwrap_or(desc.label);
    let labels = menu_labels(desc, pres);
    let current = stored(&test_clip::effects(&clip)[index], desc);
    let others: Vec<Option<usize>> = match pres.widget {
        Widget::Toggle => vec![None],
        _ => {
            let at = shown_index(desc, &current).unwrap_or(0);
            (0..labels.len()).filter(|i| *i != at).map(Some).collect()
        }
    };
    let mut last = String::from("the row has no other value");
    for other in others {
        let mut s = Stack::new(clip.clone());
        match other {
            None => s.click(label)?,
            Some(i) => {
                let at = shown_index(desc, &current).unwrap_or(0);
                s.click(labels[at])?;
                s.click(labels[i])?;
            }
        }
        if !s.unapplied.is_empty() {
            return Err(format!("unapplied: {:?}", s.unapplied));
        }
        let after = s.clip().clone();
        match super::parity_pixels::clip_edit_drawn_on(routes, size, &clip, &after) {
            Ok(()) => return Ok(()),
            Err(e) if e.starts_with("BLOCKED") => return Err(e),
            Err(e) => last = format!("{e} ({})", other.map_or_else(|| "switched".to_owned(), |i| format!("picking {:?}", labels[i]))),
        }
    }
    Err(last)
}

/// The obligations of every row these cases drive, on the surface and
/// fixture variant each is driven on: an effect card's switches and menus
/// on the stack (`run`), a per-node grade's (`run_grade`), and the
/// Corrector's on its tabbed page and the bottom tools (`run_grade_pages`).
/// Taken from the same selection the runs make, not from the central
/// rules, so completeness can't be circular.
pub(super) fn require(manifest: &mut Manifest) {
    let entries = catalog();
    let mut want = |type_id: &str, parameter: &str, surface: Surface, variant: &str| {
        if let Some(entry) = entries.iter().find(|e| e.type_id == type_id && e.parameter == parameter) {
            manifest.require_fixture(entry, surface, variant);
            // A grade family runs on the sequence's grades too (`on_roots`),
            // which earns its keys there under the root's variant.
            if entry.grade {
                for root in GradeRoot::SEQUENCE {
                    manifest.require_fixture(entry, Surface::NonClipGrade, &format!("{}: {surface:?} {variant}", root.variant()));
                }
            }
        }
    };
    let discrete = |w: Widget| matches!(w, Widget::Toggle | Widget::Combo);
    // Applied once `want` is done with the manifest.
    let mut pinned = Vec::new();
    let mut policies = Vec::new();
    // A placed kind's card too (Speed Ramp's switches and menus): its rows
    // are drawn on the stack and sent through the clip's effect list.
    for ty in effect_types() {
        for desc in ty.desc.params {
            let pres = ty.presentation(desc);
            if discrete(pres.widget) {
                if let Ok((_, variant)) = shown_fixture(ty, &pres) {
                    want(ty.desc.id, desc.key, Surface::EffectsStack, variant);
                    if ty.tag == "speed_ramp" {
                        pinned.push(CoverageKey::new(ty.desc.id, desc.key, Surface::EffectsStack, variant, Interaction::Pixels));
                    }
                    if POLICY_SWITCHES.contains(&(ty.tag, desc.key)) {
                        policies.push(CoverageKey::new(ty.desc.id, desc.key, Surface::EffectsStack, variant, Interaction::Pixels));
                    }
                }
            }
        }
    }
    for ty in klipp_nodes::registry::node_types() {
        if !super::main_layout::per_node_kind(ty.kind) {
            continue;
        }
        for desc in ty.desc.params {
            if discrete(ty.presentation(desc).widget) {
                want(ty.desc.id, desc.key, Surface::GradePerNode, grade_variant(ty, desc));
            }
        }
    }
    for kind in PAGE_KINDS {
        let ty = klipp_nodes::registry::node_type(kind).expect("a page type");
        for desc in ty.desc.params {
            if discrete(ty.presentation(desc).widget) && CORRECTOR_TABS.iter().any(|(k, _)| *k == desc.key) {
                for surface in [Surface::GradeTabbedRight, Surface::GradeBottomTools] {
                    want(ty.desc.id, desc.key, surface, grade_variant(ty, desc));
                }
            }
        }
    }
    for key in pinned {
        manifest.exclude_key(key, SPEED_RAMP_PIXELS);
    }
    for key in policies {
        manifest.exclude_key(key, POLICY_PIXELS);
    }
}

/// The fixture variant `discrete_cases` names a grade row by: "conditions
/// met" when a condition on it isn't met at its type's defaults, which the
/// case switches on first, else "sample".
pub(super) fn grade_variant(ty: &klipp_nodes::registry::NodeType, desc: &ParamDesc) -> &'static str {
    let pres = ty.presentation(desc);
    let unmet = [pres.visible_if, pres.enabled_if].into_iter().flatten().any(|c| {
        ty.desc.params.iter().find(|p| p.key == c.key()).is_none_or(|cond| !c.holds_for(cond.default.round()))
    });
    if unmet { "conditions met" } else { "sample" }
}

/// Every switch clicked both ways and every menu item picked, on every
/// effect the stack draws as a card, each held to the value it names.
pub(super) fn run(report: &mut Report) {
    // A placed kind's card included: its rows are drawn on the stack, their
    // commands sent through the clip's effect list (`to_effect_list`).
    for ty in effect_types() {
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
            // Opened: the card drawn with the row on it writes nothing.
            report.case(&format!("{id} {} opened", desc.key), [key(Interaction::OpenWithoutWrite)], || {
                let mut s = Stack::new(fixture(&effect));
                let before = s.picture();
                s.frame(Vec::new());
                if !s.frame(Vec::new()).iter().any(|(t, _)| t == label) {
                    return Err(format!("{label:?} isn't drawn"));
                }
                if !s.unapplied.is_empty() || s.picture() != before || s.state.active_history().undo_count() != 0 {
                    return Err(format!("opening wrote: {:?}", s.unapplied));
                }
                Ok(())
            });
            // Drawn: the switch flipped or another item picked changes the
            // frame, the CPU's the GPU's (Speed Ramp's pinned, #1409).
            if ty.tag != "speed_ramp" && !POLICY_SWITCHES.contains(&(ty.tag, desc.key)) {
                let name = format!("{id} {} drawn", desc.key);
                let drawn = || stack_drawn(ty, desc, &pres, &effect);
                match super::parity_numeric::routes_for(id) {
                    (_, Some(reason)) => report.case_cpu_by_design(&name, [key(Interaction::Pixels)], reason, drawn),
                    (_, None) => report.case_or_blocked(&name, [key(Interaction::Pixels)], drawn),
                }
            }
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
                                s.click_in_row(label, labels[from])?;
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
fn effect_switches_and_menus_store_what_they_name() {
    let mut report = Report::default();
    report.family("effect controls", run);
    report.family_result().unwrap();
    assert!(!report.passed.is_empty(), "no switch or menu row was driven");
}

/// One grade node drawn by the generated grade inspector (`grade_view`),
/// or by a Colour page's tabs or bottom tools, and driven.
pub(super) struct Grade {
    pub(super) state: AppState,
    pub(super) clip: ClipId,
    pub(super) node: klipp_nodes::NodeId,
    ctx: egui::Context,
    clock: f64,
    panel: crate::panels::node_inspector::NodeInspectorPanel,
    pub(super) unapplied: Vec<String>,
    /// Requests the app runs off the panel, recorded here, in order, not
    /// applied: the sampling tools' (Auto White Balance, an eyedropper,
    /// Colour Match's capture and apply), which read the viewer's frame,
    /// and a mask region's tracking, or its tracker taken off.
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
    /// The path writes the view sent (`SetParamAt`, `SetPayloadAt`), in
    /// order: where an oracle finds the node's address in this grade,
    /// whatever its root.
    pub(super) path_writes: Vec<AppCommand>,
    /// Which grade the node is in (M3 step 19): the clip's own (`clip` is
    /// that clip), or one of the sequence's own (`clip` is the id of the
    /// stand-in it is drawn and edited through, there only inside
    /// [`Grade::with_stand_in`]).
    pub(super) root: GradeRoot,
    /// The fixture's grade group, for a stage root.
    group: Option<klipp_core::GradeGroupId>,
    /// The clip the fixture was made on, whatever `root` is.
    home: ClipId,
    /// The graph a stand-in is put on with while its root has no grade,
    /// kept as the app keeps it.
    fresh: Option<(ClipId, klipp_nodes::NodeGraph)>,
    /// The last frame's icons (its textured rectangles), where drawn.
    pub(super) icons: Vec<egui::Rect>,
    /// The last frame's keyframe diamonds (small closed four-point paths).
    pub(super) diamonds: Vec<egui::Rect>,
    /// The last frame's small square frames (a checkbox's box).
    pub(super) boxes: Vec<egui::Rect>,
    /// The last frame's large filled panels, at least 100 points wide and
    /// 60 tall (a curve editor's canvas).
    pub(super) canvases: Vec<egui::Rect>,
    /// The playhead moves the view asked for (a row's key carets), in
    /// order, as the app's seek would take them.
    pub(super) seeks: Vec<klipp_core::Timecode>,
}

/// A grade's picture, as the families compare it: a clip's lifted picture,
/// or a sequence grade as stored, as a JSON value (its canvas positions are
/// a hash map, whose written order two equal grades needn't share; a
/// value's maps compare whatever their order).
#[derive(Clone, Debug, PartialEq)]
pub(super) enum GradePicture {
    Clip(Box<klipp_timeline::LiftedPicture>),
    Root(serde_json::Value),
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

thread_local! {
    /// The grade [`Grade::new`] puts its node in while [`on_roots`] runs a
    /// family: the clip's own, but for that run.
    static ROOT: std::cell::Cell<GradeRoot> = const { std::cell::Cell::new(GradeRoot::Clip) };
}

/// `family` run again with its grade on each of the sequence's own grades
/// (M3 step 19): the timeline grade and a group's pre-clip and post-clip
/// stages, drawn and edited through their stand-ins as the app does, every
/// case it earns counted on `Surface::NonClipGrade` (`Report::merge_on_root`).
pub(super) fn on_roots(report: &mut Report, family: impl Fn(&mut Report)) {
    for root in GradeRoot::SEQUENCE {
        let mut inner = Report::default();
        {
            let _run = RootRun::start(root);
            inner.family(root.variant(), &family);
        }
        report.merge_on_root(root.variant(), inner);
    }
}

/// Every sequence grade earned keys on `Surface::NonClipGrade`: a family
/// that ran on none of them counts for nothing there.
pub(super) fn assert_on_every_root(report: &Report) {
    for root in GradeRoot::SEQUENCE {
        let prefix = format!("{}: ", root.variant());
        assert!(
            report.passed.iter().any(|k| k.surface == Surface::NonClipGrade && k.variant.starts_with(&prefix)),
            "no case passed on the {} grade",
            root.variant()
        );
    }
}

/// A family's run on `root`, while it lives: [`Grade::new`] puts its node
/// there, and every case must touch it. Dropped (a panic included), both
/// are put back, so no later test runs on it.
struct RootRun(super::parity_manifest::OnRoot);

impl RootRun {
    fn start(root: GradeRoot) -> Self {
        ROOT.with(|r| r.set(root));
        RootRun(super::parity_manifest::OnRoot::start())
    }
}

impl Drop for RootRun {
    fn drop(&mut self) {
        ROOT.with(|r| r.set(GradeRoot::Clip));
    }
}

impl Grade {
    /// A grade's node driven: in the clip's own grade, or, while
    /// [`on_roots`] runs a family, in the sequence grade it names.
    pub(super) fn new(state: AppState, clip: ClipId, node: klipp_nodes::NodeId) -> Self {
        Self::plain(state, clip, node).on_root(ROOT.with(std::cell::Cell::get))
    }

    /// [`Grade::new`] in the clip's own grade, whatever family runs.
    fn plain(state: AppState, clip: ClipId, node: klipp_nodes::NodeId) -> Self {
        // The icons load as in the app: a failed one is drawn as a warning
        // filling the panel, which pushes the rows off it.
        let ctx = egui::Context::default();
        crate::icons::install(&ctx);
        Grade { state, clip, node, ctx, clock: 0.0, panel: Default::default(), unapplied: Vec::new(), side: Vec::new(), page: Page::PerNode, before_input: None, gesture: None, root: GradeRoot::Clip, group: None, home: clip, fresh: None, icons: Vec::new(), diamonds: Vec::new(), boxes: Vec::new(), canvases: Vec::new(), seeks: Vec::new(), path_writes: Vec::new() }
    }

    /// The same node in `root` instead (M3 step 19): for a sequence grade,
    /// the clip's grade moved there (the timeline grade, or a stage of a
    /// fixture group), drawn and edited through its stand-in.
    pub(super) fn on_root(mut self, root: GradeRoot) -> Self {
        use klipp_timeline::GradeStage;
        if root == GradeRoot::Clip || self.root != GradeRoot::Clip {
            return self;
        }
        let grade = self.clip().grade().clone();
        let seq = &mut self.state.sequences[0];
        self.clip = match root {
            GradeRoot::Timeline => {
                seq.timeline_grade_slot().set(Some(grade));
                seq.timeline_grade_clip_id()
            }
            GradeRoot::Pre | GradeRoot::Post => {
                let stage = if root == GradeRoot::Pre { GradeStage::Pre } else { GradeStage::Post };
                let mut group = klipp_timeline::GradeGroup::new("Fixture");
                group.stage_slot(stage).set(Some(grade));
                let id = group.stage_clip_id(stage);
                self.group = Some(group.id);
                seq.grade_groups.push(group);
                id
            }
            GradeRoot::Clip => unreachable!(),
        };
        self.root = root;
        self
    }

    /// What the Colour workspace grades `root` through, for a sequence grade.
    fn stand_in(&self) -> Option<super::timeline_grade::GradeStandIn> {
        use super::timeline_grade::GradeStandIn;
        use klipp_timeline::GradeStage;
        match self.root {
            GradeRoot::Clip => None,
            GradeRoot::Timeline => Some(GradeStandIn::Timeline),
            GradeRoot::Pre => self.group.map(|g| GradeStandIn::Group(g, GradeStage::Pre)),
            GradeRoot::Post => self.group.map(|g| GradeStandIn::Group(g, GradeStage::Post)),
        }
    }

    /// `f` with a sequence grade's stand-in on, as the app has it while it
    /// draws the Colour panels and runs a colour command, and taken down
    /// after, settling what `f` wrote on it into the grade. A clip's grade
    /// has none.
    pub(super) fn with_stand_in<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let Some(which) = self.stand_in() else { return f(self) };
        super::timeline_grade::stand_in_grade_on(&mut self.state.sequences[0], which, &mut self.fresh);
        let out = f(self);
        super::timeline_grade::stand_down_timeline_grade_on(&mut self.state.sequences[0], &mut self.fresh);
        out
    }

    /// The grade the node is in, as stored.
    pub(super) fn grade(&self) -> &klipp_nodes::NodeGraph {
        match self.stand_in() {
            None => klipp_timeline::GradeAccess::grade(self.clip()),
            Some(which) => {
                let seq = &self.state.sequences[0];
                seq.root_grade(which.root(seq)).ok().flatten().expect("the fixture's grade")
            }
        }
    }

    /// A fresh state, no history, holding a clone of this one's clip and files.
    pub(super) fn twin(&self) -> Self {
        let mut state = AppState::default();
        let home = self.state.sequences[0].all_clips().find(|c| c.id == self.home).unwrap();
        let track = self.state.sequences[0].tracks.iter().position(|t| t.clips.iter().any(|c| c.id == self.home)).unwrap();
        state.sequences[0].tracks[track].clips.push(home.clone());
        // Its files too: a clip's file settings (its camera encoding) are
        // the file's.
        state.media_pool = self.state.media_pool.clone();
        // A sequence grade's: the sequence's grades and groups too, under
        // the same sequence id, which the timeline grade's stand-in id is
        // made from.
        state.sequences[0].id = self.state.sequences[0].id;
        state.sequences[0].timeline_grade_slot().set(self.state.sequences[0].timeline_grade().cloned());
        state.sequences[0].grade_groups = self.state.sequences[0].grade_groups.clone();
        let mut twin = Grade { page: self.page, root: self.root, group: self.group, clip: self.clip, ..Grade::plain(state, self.home, self.node) };
        // The zone an HDR Zones page shows, as picked on this one (#240).
        twin.panel.hdr_zone_pick = self.panel.hdr_zone_pick.clone();
        twin
    }

    /// The same, an HDR Zones page (#240) showing the zone `key` names
    /// (`hdr_<zone>_...`), as if picked from its zone menu. Any other key
    /// leaves it as it is.
    pub(super) fn picking(mut self, key: &str) -> Self {
        let zone = key.strip_prefix("hdr_").and_then(|rest| klipp_nodes::params::HDR_ZONE_KEYS.iter().position(|z| rest.strip_prefix(z).is_some_and(|r| r.starts_with('_'))));
        if let Some(zone) = zone {
            self.panel.hdr_zone_pick.insert(self.node, zone);
        }
        self
    }

    /// The same, on `page`.
    pub(super) fn on(self, page: Page) -> Self {
        Grade { page, ..self }
    }

    /// The same, drawing `node` of the same grade.
    pub(super) fn on_node(self, node: klipp_nodes::NodeId) -> Self {
        Grade { node, ..self }
    }

    pub(super) fn clip(&self) -> &klipp_timeline::Clip {
        self.state.sequences[0].all_clips().find(|c| c.id == self.clip).unwrap()
    }

    /// What the pixel oracle draws for the node's grade: the clip it was
    /// made on, alone, with its own grade, or on a sequence grade's root:
    /// the sequence's timeline grade, or a group's stage with the clip in
    /// the group.
    pub(super) fn frame_sequence(&self) -> klipp_timeline::Sequence {
        let mut seq = self.state.sequences[0].clone();
        let mut home = seq.all_clips().find(|c| c.id == self.home).unwrap().clone();
        home.grade_group = self.group;
        let mut track = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        track.clips = vec![home];
        seq.tracks = vec![track];
        seq
    }

    pub(super) fn picture(&self) -> GradePicture {
        match self.root {
            GradeRoot::Clip => GradePicture::Clip(Box::new((*self.clip().picture_v2().unwrap()).clone())),
            _ => GradePicture::Root(serde_json::to_value(self.grade()).unwrap()),
        }
    }

    /// One frame with `events`: the text drawn, at its centre. The
    /// generated view's commands are applied as the app dispatches them;
    /// the old panel edits the clip's grade in place, as the app lets it.
    pub(super) fn frame(&mut self, events: Vec<egui::Event>) -> Vec<(String, egui::Pos2)> {
        self.clock += 0.05;
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(420.0, 2400.0))), time: Some(self.clock), events, ..Default::default() };
        let theme = KlippTheme::default();
        self.before_input = Some(self.grade().clone());
        if self.root != GradeRoot::Clip {
            super::parity_manifest::touched_the_root();
        }
        let (cmds, out) = self.with_stand_in(|g| {
            // The clip drawn (a sequence grade's stand-in, stood in) starts
            // where the carets seek from.
            let clip_start = g.state.sequences[0].all_clips().find(|c| c.id == g.clip).expect("the grade's clip").timeline_in;
            let frame = crate::inspector::keyframe_selection::FrameSelection { time_secs: 0.5, tolerance: 0.02, clip_start };
            g.draw(input, frame, &theme)
        });
        self.icons = out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Rect(r) if r.fill_texture_id != egui::TextureId::default() => Some(r.rect), _ => None }).collect();
        self.boxes = out.shapes.iter().filter_map(|s| match &s.shape {
            egui::Shape::Rect(r) if r.fill_texture_id == egui::TextureId::default() && (10.0..=20.0).contains(&r.rect.width()) && (r.rect.width() - r.rect.height()).abs() < 1.0 => Some(r.rect),
            _ => None,
        }).collect();
        self.canvases = out.shapes.iter().filter_map(|s| match &s.shape {
            egui::Shape::Rect(r) if r.fill_texture_id == egui::TextureId::default() && r.fill != egui::Color32::TRANSPARENT && r.rect.width() >= 100.0 && r.rect.height() >= 60.0 => Some(r.rect),
            _ => None,
        }).collect();
        self.diamonds = out.shapes.iter().filter_map(|s| match &s.shape {
            egui::Shape::Path(path) if path.closed && path.points.len() == 4 => {
                let r = egui::Rect::from_points(&path.points);
                ((6.0..=12.0).contains(&r.width()) && (r.width() - r.height()).abs() < 0.5).then_some(r)
            }
            _ => None,
        }).collect();
        for cmd in cmds {
            self.apply(cmd);
        }
        out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)), _ => None }).collect()
    }

    /// One frame drawn as the app draws the Colour panels, the stand-in on
    /// for a sequence grade: its commands, not yet applied, and its shapes.
    fn draw(&mut self, input: egui::RawInput, frame: crate::inspector::keyframe_selection::FrameSelection, theme: &KlippTheme) -> (Vec<AppCommand>, egui::FullOutput) {
        let mut cmds = Vec::new();
        let root = self.stand_in().map(|w| w.root(&self.state.sequences[0]));
        let Grade { state, clip, node, ctx, panel, page, .. } = self;
        let (clip_id, node, page) = (*clip, *node, *page);
        let out = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| match page {
                Page::PerNode => match root {
                    None => {
                        let clip = state.sequences[0].all_clips().find(|c| c.id == clip_id).unwrap().clone();
                        crate::inspector::grade_view::show(ui, theme, &clip, &[], node, &mut cmds, frame, Some(panel));
                    }
                    Some(root) => {
                        let snapshot = crate::inspector::grade_view::snapshot_for_root(&state.sequences[0], clip_id, root, &[], &mut state.pending_corrector, &mut state.root_grade_start);
                        let mut panel_cmds = Vec::new();
                        // A node the grade doesn't hold draws nothing, as in the app.
                        let _ = snapshot.as_ref().is_some_and(|s| s.page(ui, theme, node, &mut panel_cmds, frame, Some(panel)));
                        let grade = state.sequences[0].root_grade(root).ok().flatten().cloned().unwrap_or_default();
                        for mut cmd in panel_cmds {
                            crate::inspector::keyframe_selection::resolve_for_grade(&grade, frame.tolerance, &mut cmd);
                            cmds.push(cmd);
                        }
                    }
                },
                // The tabbed page and the bottom tools as the app draws them
                // (`main_layout`): the panel over the clip's grade, its
                // shared rows the generated ones.
                Page::Tabbed(tab) | Page::Bottom(tab) => {
                    // A sequence grade's snapshot is taken at its root, before
                    // the stand-in is borrowed to draw, as the app takes it.
                    let at_root = root.and_then(|root| crate::inspector::grade_view::snapshot_for_root(&state.sequences[0], clip_id, root, &[], &mut state.pending_corrector, &mut state.root_grade_start));
                    let clip = state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip_id)).unwrap();
                    panel.generated = if root.is_some() { at_root } else { crate::inspector::grade_view::snapshot_for(clip, &[], false, &mut state.pending_corrector) };
                    let (mut selected, mut tab) = (Some(node), tab);
                    let wide = matches!(page, Page::Bottom(_));
                    cmds.extend(clip.edit_grade(|g| if wide {
                        panel.show_simple_horizontal(ui, g, clip_id, &mut selected, &mut tab, theme, frame)
                    } else {
                        panel.show_simple(ui, g, clip_id, &mut selected, &mut tab, theme, frame)
                    }));
                    panel.generated = None;
                }
            });
        });
        (cmds, out)
    }

    /// `cmd` as the app applies it: its key resolved, past the dispatch
    /// boundary, then by the path ops, the colour dispatch's writes the
    /// harness shares (colour.rs's `write_node_settings`, `edit_clip_graph`,
    /// `write_node_param` and the gesture's close and abandon), or the
    /// inspector edit host. A sampling tool's request is recorded in
    /// `side`; what nothing applies, in `unapplied`.
    pub(super) fn apply(&mut self, mut cmd: AppCommand) {
        if self.root != GradeRoot::Clip {
            super::parity_manifest::touched_the_root();
        }
        crate::inspector::keyframe_selection::resolve_command(&self.state.sequences[0], &mut cmd);
        // A seek moves the playhead, which the grade's frame doesn't follow:
        // recorded, as the app's seek would take it.
        if let AppCommand::SeekTo(at) = cmd {
            self.seeks.push(at);
            return;
        }
        // The dispatch boundary first, for every command, as the app's
        // dispatch passes it.
        self.state.before_dispatch(&cmd);
        self.state.toast = Default::default();
        if matches!(cmd, AppCommand::SetParamAt { .. } | AppCommand::SetPayloadAt { .. }) {
            self.path_writes.push(cmd.clone());
        }
        match self.state.path_command(&cmd) {
            Some(_) => {
                if let Some(why) = self.state.toast.message_for_test() {
                    self.unapplied.push(format!("{cmd:?} refused: {why}"));
                }
            }
            None => {
                if matches!(cmd, AppCommand::AutoWhiteBalance { .. } | AppCommand::PickWhiteBalance { .. } | AppCommand::PickQualifierColour { .. } | AppCommand::PickQualifierStroke { .. } | AppCommand::PickHueCurve { .. } | AppCommand::CaptureColorMatchReference | AppCommand::ApplyColorMatch { .. } | AppCommand::TrackMaskRegion { .. } | AppCommand::RemoveMaskTracking { .. }) {
                    self.side.push(format!("{cmd:?}"));
                    return;
                }
                // The colour dispatch runs with a sequence grade's stand-in
                // on (`handle_color_commands`), as here.
                self.with_stand_in(|g| g.apply_colour(cmd));
            }
        }
    }

    /// A colour command as the colour dispatch lands it: an old-panel
    /// gesture recorded as one graph step, a node's number written in
    /// place, an editor's settings written as a step, or the inspector
    /// edit host's.
    fn apply_colour(&mut self, cmd: AppCommand) {
        // An old-panel gesture, recorded as the colour dispatch
        // records it: one graph step, from the grade as it was
        // before the input that began it.
        match &cmd {
            AppCommand::BeginNodeParamEdit { .. } => {
                if self.gesture.is_none() {
                    self.gesture = self.before_input.clone();
                }
                return;
            }
            AppCommand::AbandonNodeParamEdit { .. } => {
                if let Some(old_graph) = self.gesture.take() {
                    super::colour::abandon_gesture(&mut self.state, self.clip, old_graph);
                }
                return;
            }
            AppCommand::EndNodeParamEdit { .. } => {
                if let Some(old_graph) = self.gesture.take() {
                    super::colour::close_gesture(&mut self.state, self.clip, old_graph);
                }
                return;
            }
            _ => {}
        }
        // A file's camera encoding, as the colour dispatch applies
        // it (#1396): the file and every clip of it, one step.
        if let AppCommand::SetClipLogFormat { clip_id, log_format } = &cmd {
            self.state.set_clip_log_format(*clip_id, *log_format);
            return;
        }
        let path = self.state.graph_descent_path.clone();
        // The old panel's number, as the colour dispatch writes
        // it (the history is its gesture's).
        if let AppCommand::SetNodeParam { node_id, param, value, .. } = &cmd {
            let target = self.state.reserved_target(self.clip, *node_id);
            super::colour::write_node_param(&mut self.state, self.clip, &path, target, param, *value);
            return;
        }
        // A per-node editor's settings, as the colour dispatch
        // lands them: written, one graph step unless a gesture
        // is open (its close records the step). Whether `cmd`
        // is one is asked of a copy, so nothing else is written.
        if super::colour::write_node_settings(&mut self.grade().clone(), &cmd) {
            super::colour::edit_clip_graph(&mut self.state, self.clip, &path, self.gesture.is_none(), |g| {
                super::colour::write_node_settings(g, &cmd);
            });
            return;
        }
        if !(InspectorEditHost { state: &mut self.state, side: &Sink }).apply(&cmd) {
            self.unapplied.push(format!("{cmd:?}"));
        }
    }


    pub(super) fn click_at(&mut self, at: egui::Pos2) {
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        self.frame(vec![egui::Event::PointerMoved(at), button(true)]);
        self.frame(vec![egui::Event::PointerMoved(at), button(false)]);
        self.frame(vec![egui::Event::PointerGone]);
    }

    /// A secondary click at `at`, as a context menu opens.
    pub(super) fn right_click_at(&mut self, at: egui::Pos2) {
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Secondary, pressed, modifiers: egui::Modifiers::NONE };
        self.frame(vec![egui::Event::PointerMoved(at), button(true)]);
        self.frame(vec![egui::Event::PointerMoved(at), button(false)]);
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
        // The step the app takes: the sequence's or the project's (#1986).
        let entry = if forward { self.state.take_redo() } else { self.state.take_undo() }.ok_or("no step to take")?;
        let mut doc = klipp_document::apply::DocumentView { sequences: &mut self.state.sequences, active: self.state.active_sequence_idx, media_pool: &mut self.state.media_pool, fps: &mut self.state.fps };
        if forward { doc.apply_forward(&entry.command); } else { doc.apply_inverse(&entry.command); }
        Ok(())
    }

    /// The node's stored value for `desc`, as its rows read it.
    pub(super) fn value(&self, desc: &ParamDesc) -> Value {
        let node = self.grade().nodes.iter().find(|n| n.id == self.node).cloned().unwrap();
        let entry = serde_json::json!({ "params": node.params });
        crate::inspector::generated::grade_value(&entry, desc)
    }
}

/// Whether `after` is `before` but for `node`'s value `key` (a dotted key
/// names a field of a settings struct): every other node as it was, and
/// this node's other values and switch.
fn nothing_else_changed(before: &klipp_nodes::NodeGraph, after: &klipp_nodes::NodeGraph, node: klipp_nodes::NodeId, key: &str) -> Result<(), String> {
    let without = |g: &klipp_nodes::NodeGraph| {
        let mut v = serde_json::to_value(g).unwrap();
        if let Some(nodes) = v["nodes"].as_array_mut() {
            for n in nodes.iter_mut().filter(|n| n["id"] == serde_json::to_value(node).unwrap()) {
                // A warper sector's drag writes its hue and saturation
                // together, both kept in the warper's arrays: that sector's
                // pair set aside, every other sector compared.
                let sector = key.strip_prefix("warper_hue_").or_else(|| key.strip_prefix("warper_sat_")).and_then(|i| i.parse::<usize>().ok());
                if let Some(i) = sector {
                    // A warper never set is every sector at zero.
                    if n["params"]["warper_params"].is_null() {
                        n["params"]["warper_params"] = serde_json::json!({ "hue_shifts": vec![0.0; 12], "sat_shifts": vec![0.0; 12] });
                    }
                    for shifts in ["hue_shifts", "sat_shifts"] {
                        if let Some(slot) = n["params"]["warper_params"][shifts].get_mut(i) {
                            *slot = serde_json::Value::Null;
                        }
                    }
                    continue;
                }
                // An HDR zone's value (#240), kept in the zones' arrays: that
                // zone's element set aside, every other compared. A wheel's
                // edit writes its puck and Y together: those of the zone too.
                let zone = key.strip_prefix("hdr_").and_then(|rest| {
                    klipp_nodes::params::HDR_ZONE_KEYS.iter().enumerate().find_map(|(i, z)| rest.strip_prefix(z)?.strip_prefix('_').map(|c| (i, c)))
                });
                if let Some((i, control)) = zone {
                    if n["params"]["hdr_zones"].is_null() {
                        n["params"]["hdr_zones"] = serde_json::to_value(klipp_core::HdrZonesSettings::default()).unwrap();
                    }
                    let wheel = matches!(control, "colour_x" | "colour_y" | "exposure");
                    for array in ["exposure", "colour_x", "colour_y", "saturation", "boundary", "falloff"].into_iter().filter(|a| *a == control || (wheel && matches!(*a, "colour_x" | "colour_y" | "exposure"))) {
                        if let Some(slot) = n["params"]["hdr_zones"][array].get_mut(i) {
                            *slot = serde_json::Value::Null;
                        }
                    }
                    continue;
                }
                let mut at = &mut n["params"];
                let mut parts = key.split('.').peekable();
                while let Some(part) = parts.next() {
                    if parts.peek().is_none() {
                        if let Some(o) = at.as_object_mut() {
                            o.remove(part);
                        }
                    } else {
                        at = &mut at[part];
                    }
                }
            }
        }
        v
    };
    let (b, a) = (without(before), without(after));
    if b != a {
        let differ = |b: &serde_json::Value, a: &serde_json::Value| -> String {
            let (bn, an) = (b["nodes"].as_array().cloned().unwrap_or_default(), a["nodes"].as_array().cloned().unwrap_or_default());
            for (x, y) in bn.iter().zip(&an) {
                if x != y {
                    let (xp, yp) = (x["params"].as_object().cloned().unwrap_or_default(), y["params"].as_object().cloned().unwrap_or_default());
                    for k in xp.keys().chain(yp.keys()) {
                        if xp.get(k) != yp.get(k) {
                            return format!("{} {k}: {:?} to {:?}", x["kind"], xp.get(k), yp.get(k));
                        }
                    }
                    return format!("{} (not its params)", x["kind"]);
                }
            }
            "the graph's shape".into()
        };
        return Err(format!("the edit of {key} changed more of the grade than {key}: {}", differ(&b, &a)));
    }
    Ok(())
}

/// The expected-result oracle for a grade edit (M3, the old view no longer
/// compared): what an edit of the generated view must leave, checked
/// against the same value written by path. Shared by the grade families.
/// A stored number as the row writes it: whole for a whole-number row.
pub(super) fn param_value(desc: &ParamDesc, v: f32) -> klipp_graph_format::ParamValue {
    match desc.kind {
        klipp_effects::registry::ParamKind::Whole => klipp_graph_format::ParamValue::Int(v.round() as i32),
        _ => klipp_graph_format::ParamValue::Float(v),
    }
}

/// Whether `stored` is `want` as the row writes it, exactly.
pub(super) fn stores(desc: &ParamDesc, stored: &serde_json::Value, want: f32) -> bool {
    match param_value(desc, want) {
        klipp_graph_format::ParamValue::Int(i) => stored.as_i64() == Some(i64::from(i)),
        klipp_graph_format::ParamValue::Float(f) => stored.as_f64().is_some_and(|v| v as f32 == f),
        _ => false,
    }
}

/// One edit of the generated view (`drive`) from `start`, held to what
/// it must store: `want`, the row's stored value, written by path to a
/// clone of `start` (`SetParamAt`, at the address the view wrote it to),
/// is the picture the view must leave, exactly. A shared editor writing
/// its node's settings whole, with no path write, is held to `want` as
/// its stored value. Either way: the row's own node changed, one undo
/// step, and undo and redo exact.
pub(super) fn oracle_case(start: &Grade, desc: &ParamDesc, want: f32, drive: impl Fn(&mut Grade) -> Result<(), String>) -> Result<(), String> {
    let mut new = start.twin();
    new.drawn();
    let before = new.picture();
    let value_before = new.value(desc);
    drive(&mut new)?;
    if !new.unapplied.is_empty() {
        return Err(format!("unapplied: {:?}", new.unapplied));
    }
    let after = new.picture();
    let stored = new.value(desc);
    if stored == value_before {
        return Err(format!("the edit left this node's {} at {value_before}", desc.key));
    }
    if !stores(desc, &stored, want) {
        return Err(format!("stored {stored}, where {want} is expected"));
    }
    let addr = new.path_writes.iter().rev().find_map(|c| match c {
        AppCommand::SetParamAt { addr, key, .. } if key.as_str() == desc.key => Some(addr.clone()),
        _ => None,
    });
    match &addr {
        Some(addr) => {
            // The write names this node, wherever its root puts it.
            if addr.node().map(|n| serde_json::to_value(n).ok()) != Some(serde_json::to_value(start.node).ok()) {
                return Err(format!("the view wrote {} to another node ({addr:?})", desc.key));
            }
        }
        // A shared editor writing its node's settings whole: nothing else
        // of the grade changed, this node's other values included.
        None => nothing_else_changed(start.grade(), new.grade(), start.node, desc.key)?,
    }
    if let Some(addr) = addr {
        let mut oracle = start.twin();
        oracle.apply(AppCommand::SetParamAt { addr, key: klipp_graph_format::ParamKey::new(desc.key), value: param_value(desc, want) });
        if !oracle.unapplied.is_empty() {
            return Err(format!("the oracle's write: unapplied {:?}", oracle.unapplied));
        }
        if oracle.picture() != after {
            return Err(format!("the edit stored {want} but left another picture than writing it by path does"));
        }
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

/// `start`'s grade with its node changed by `edit`, written straight into
/// the document (through a sequence grade's stand-in, as the app edits
/// one): the picture an edit naming that change must leave.
pub(super) fn expected_with(start: &Grade, edit: impl FnOnce(&mut klipp_nodes::Node) -> Result<(), String>) -> Result<GradePicture, String> {
    Ok(with_node(start, edit)?.picture())
}

/// A twin of `start` with its node changed by `edit`, written straight
/// into the document (through a sequence grade's stand-in, as the app
/// edits one), no widget involved.
pub(super) fn with_node(start: &Grade, edit: impl FnOnce(&mut klipp_nodes::Node) -> Result<(), String>) -> Result<Grade, String> {
    let mut changed = start.twin();
    let node = changed.node;
    let written = changed.with_stand_in(|g| -> Result<(), String> {
        let clip_id = g.clip;
        let clip = g.state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip_id)).ok_or("no clip for the grade")?;
        clip.edit_grade(|graph| edit(graph.nodes.iter_mut().find(|n| n.id == node).ok_or("no node")?))
    });
    written?;
    Ok(changed)
}

/// `params` with the switch at `field` (a dotted path into a struct, as
/// `soft_clip.enabled`) set to `on`.
fn set_switch(params: &mut klipp_nodes::NodeParams, field: &str, on: bool) -> Result<(), String> {
    let mut json = serde_json::to_value(&*params).map_err(|e| e.to_string())?;
    let mut at = &mut json;
    for step in field.split('.') {
        at = at.as_object_mut().ok_or_else(|| format!("{field} isn't in a struct"))?.entry(step).or_insert(Value::Null);
    }
    *at = Value::Bool(on);
    *params = serde_json::from_value(json).map_err(|e| format!("{field} set: {e}"))?;
    Ok(())
}

/// A row shown under a condition (`visible_if`) comes and goes with it
/// (ConditionalVisibility): on `start`, where the family draws the row
/// with its condition met, `name` (the row's label, or a wheel's name for
/// a wheel's part) is drawn; with the condition's switch the other way,
/// written to the node straight, it isn't, or fewer times; and drawing
/// either writes nothing.
pub(super) fn visibility_case(start: &Grade, ty: &klipp_nodes::registry::NodeType, desc: &ParamDesc, name: &str) -> Result<(), String> {
    let Some((key, value)) = ty.presentation(desc).visible_if.map(|c| (c.key(), c.a_value_meeting_it())) else { return Err(format!("{} has no condition", desc.key)) };
    let cond = ty.desc.params.iter().find(|p| p.key == key).ok_or_else(|| format!("its condition names {key:?}, not a parameter"))?;
    let drawn = |g: &mut Grade| -> Result<usize, String> {
        let before = (g.picture(), g.state.active_history().undo_count());
        let n = g.drawn().iter().filter(|(t, _)| t == name).count();
        if (g.picture(), g.state.active_history().undo_count()) != before || !g.unapplied.is_empty() {
            return Err(format!("drawing wrote: {:?}", g.unapplied));
        }
        Ok(n)
    };
    let shown = drawn(&mut start.twin())?;
    if shown == 0 {
        return Err(format!("{name:?} isn't drawn with {key} met"));
    }
    let mut unmet = with_node(start, |n| match cond.kind {
        // A menu's: another of its items (#895).
        ParamKind::Choice(_) => n.params.apply_override(key, if value == 0.0 { 1.0 } else { 0.0 }).then_some(()).ok_or_else(|| format!("{key} can't be set")),
        _ => set_switch(&mut n.params, cond.field, value == 0.0),
    })?;
    let hidden = drawn(&mut unmet)?;
    if hidden >= shown {
        return Err(format!("{name:?} is drawn {hidden} times with {key} unmet, {shown} with it met"));
    }
    Ok(())
}

/// What a switch showing `index` (off 0, on 1) or a menu's item `index`
/// names, as a path write gives it: the document stores it in the node's
/// own form (a menu's enum by name, a struct's switch inside its struct,
/// made at its default when unset).
fn named_value(desc: &ParamDesc, index: usize) -> klipp_graph_format::ParamValue {
    match desc.kind {
        ParamKind::Bool => klipp_graph_format::ParamValue::Bool(index == 1),
        _ => klipp_graph_format::ParamValue::Int(index as i32),
    }
}

/// A discrete grade edit, `drive` working the control from `start`: the
/// node's `desc` shown at `index` after, and the picture exactly the one
/// the value `index` names, written by path to a clone of `start` through
/// the document (at the address the view wrote to, which must name this
/// node), in one undo step, undone and redone exactly. A shared editor
/// writing its node's settings by command is held to the item shown and
/// to changing no other node.
pub(super) fn grade_case(start: &Grade, desc: &ParamDesc, index: usize, drive: impl Fn(&mut Grade) -> Result<(), String>) -> Result<(), String> {
    let mut new = start.twin();
    // The picture as the page shows it, drawn once (a page may write on
    // its first draw, as the tabbed page's curves do).
    new.drawn();
    let before = new.picture();
    drive(&mut new)?;
    if !new.unapplied.is_empty() {
        return Err(format!("unapplied: {:?}", new.unapplied));
    }
    let after = new.picture();
    if after == before {
        return Err("the control changed nothing".into());
    }
    if shown_index(desc, &new.value(desc)) != Some(index) {
        return Err(format!("stored {}, which doesn't show item {index}", new.value(desc)));
    }
    let addr = new.path_writes.iter().rev().find_map(|c| match c {
        AppCommand::SetParamAt { addr, .. } | AppCommand::SetPayloadAt { addr, .. } => Some(addr.clone()),
        _ => None,
    });
    match addr {
        Some(addr) => {
            if addr.node() != Some(new.node) {
                return Err(format!("the view wrote to {addr:?}, not this node"));
            }
            let mut oracle = start.twin();
            oracle.drawn();
            oracle.apply(AppCommand::SetParamAt { addr, key: klipp_graph_format::ParamKey::new(desc.key), value: named_value(desc, index) });
            if !oracle.unapplied.is_empty() {
                return Err(format!("the expected value can't be written: {:?}", oracle.unapplied));
            }
            if after != oracle.picture() {
                let node = |g: &Grade| g.grade().nodes.iter().find(|n| n.id == g.node).map(|n| format!("{:?}", n.params)).unwrap_or_default();
                return Err(format!("not the expected picture ({} at item {index}): stored {}, expected {}", desc.key, node(&new), node(&oracle)));
            }
        }
        // A shared editor (a whole-node widget, the bottom tools' Linear /
        // Log) writes its node's settings by command, not by path: the
        // item shown (above), and nothing but this node changed.
        None => {
            let mut was = start.twin();
            was.drawn();
            let others = |g: &Grade| g.grade().nodes.iter().filter(|n| n.id != g.node).map(|n| (n.id, format!("{:?}", n.params), n.enabled)).collect::<Vec<_>>();
            if others(&new) != others(&was) {
                return Err("the editor changed another node".into());
            }
        }
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
    if matches!(kind, NodeKind::Blend | NodeKind::ApplyMatte) {
        return wired(kind);
    }
    if !matte_out {
        return super::grade_inspector_tests::graded(kind);
    }
    let mut g = NodeGraph::default();
    let (qualifier, mask, _) = g.ensure_windowed_secondary(NodeKind::Corrector);
    let node = match kind {
        NodeKind::Qualifier => qualifier,
        NodeKind::Mask => mask,
        NodeKind::CombineMattes => g.nodes.iter().find(|n| n.kind == kind)?.id,
        // A Key Mixer stands where the key and window are combined, both
        // wired into it.
        NodeKind::KeyMixer => {
            let n = g.nodes.iter_mut().find(|n| n.kind == NodeKind::CombineMattes)?;
            n.kind = kind;
            n.label = kind.display_name().to_string();
            n.id
        }
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

/// A Blend or an Apply Matte after the Corrector with its other inputs
/// wired, so its mix has something to mix: the Blend's second input the
/// source, the Apply Matte's matte a Qualifier's of the source and its
/// ungraded input the source.
fn wired(kind: klipp_nodes::NodeKind) -> Option<(AppState, ClipId, klipp_nodes::NodeId)> {
    use klipp_nodes::{Edge, Node, NodeKind};
    let (mut state, clip, node) = super::grade_inspector_tests::graded(kind)?;
    let c = state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip))?;
    klipp_timeline::GradeAccess::edit_grade(c, |g| {
        let source = g.nodes.iter().find(|n| n.kind == NodeKind::Source).map(|n| n.id);
        let Some(source) = source else { return };
        let edge = |from_node, to_node, to_port| Edge { from_node, from_port: 0, to_node, to_port };
        if kind == NodeKind::Blend {
            g.edges.push(edge(source, node, 1));
        } else {
            let qualifier = Node { id: klipp_nodes::NodeId::new_v4(), kind: NodeKind::Qualifier, label: NodeKind::Qualifier.display_name().to_string(), enabled: true, params: Default::default(), keyframes: Default::default(), output_link: None, input_links: Vec::new() };
            let q = qualifier.id;
            g.nodes.push(qualifier);
            g.edges.push(edge(source, q, 0));
            g.edges.push(edge(q, node, 1));
            g.edges.push(edge(source, node, 2));
        }
    });
    c.picture_v2().ok()?;
    Some((state, clip, node))
}

/// Every switch clicked both ways and every menu item picked on every
/// grade node the per-node layout draws, each held to the value it names.
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
        let base = Grade::new(state, clip, node);
        for desc in ty.desc.params {
            discrete_cases(report, ty, desc, &base, Surface::GradePerNode);
        }
    }
}

/// `base` with `desc`'s row shown: its conditions met by clicking their
/// switches, and the fixture's variant, "conditions met" when any had to
/// be, else "sample".
pub(super) fn meet_conditions(base: &Grade, ty: &klipp_nodes::registry::NodeType, desc: &ParamDesc) -> Result<(Grade, &'static str), String> {
    let pres = ty.presentation(desc);
    let mut shown = base.twin();
    let mut variant = "sample";
    for (key, value) in [pres.visible_if, pres.enabled_if].into_iter().flatten().map(|c| (c.key(), c.a_value_meeting_it())) {
        let cond = ty.desc.params.iter().find(|p| p.key == key).ok_or_else(|| format!("its condition names {key:?}, not a parameter"))?;
        let at = shown_index(cond, &shown.value(cond));
        if at != Some(value as usize) {
            let cond_pres = ty.presentation(cond);
            let cond_label = cond_pres.label.unwrap_or(cond.label);
            match cond.kind {
                // A menu's condition (a qualifier's mode, #895): its item
                // picked from the menu, as a user picks it.
                ParamKind::Choice(_) => {
                    let labels = menu_labels(cond, &cond_pres);
                    let shows = at.and_then(|i| labels.get(i)).ok_or_else(|| format!("{key} shows no item"))?;
                    let item = labels.get(value as usize).ok_or_else(|| format!("{key} has no item {value}"))?;
                    shown.pick(cond_label, shows, item)?;
                }
                _ => shown.switch(cond_label)?,
            }
            variant = "conditions met";
        }
    }
    if !shown.unapplied.is_empty() {
        return Err(format!("meeting its conditions: unapplied {:?}", shown.unapplied));
    }
    Ok((shown, variant))
}

/// A grade node's switch or menu row (`desc`), drawn on `base`'s page and
/// counted on `surface`: clicked both ways or every item picked, each
/// from the other value, held to the value it names; nothing for a row of
/// another widget.
fn discrete_cases(report: &mut Report, ty: &klipp_nodes::registry::NodeType, desc: &'static ParamDesc, base: &Grade, surface: Surface) {
    let id = ty.desc.id;
    let pres = ty.presentation(desc);
    if !matches!(pres.widget, Widget::Toggle | Widget::Combo) {
        return;
    }
    let label = pres.label.unwrap_or(desc.label);
    let (shown, variant) = match meet_conditions(base, ty, desc) {
        Ok(shown) => shown,
        Err(e) => {
            report.case(&format!("{id} {}", desc.key), [], || Err(e));
            return;
        }
    };
    let key = |interaction| CoverageKey::new(id, desc.key, surface, variant, interaction);
    // Opened: the page drawn with the row on it writes nothing.
    report.case(&format!("{id} {} on {surface:?} opened", desc.key), [key(Interaction::OpenWithoutWrite)], || {
        let mut g = shown.twin();
        let before = (g.picture(), g.state.active_history().undo_count());
        g.drawn();
        g.drawn();
        if (g.picture(), g.state.active_history().undo_count()) != before || !g.unapplied.is_empty() {
            return Err(format!("opening wrote: {:?}", g.unapplied));
        }
        Ok(())
    });
    // Shown under a condition: it comes and goes with it.
    if pres.visible_if.is_some() {
        report.case(&format!("{id} {} on {surface:?} shown with its condition", desc.key), [key(Interaction::ConditionalVisibility)], || visibility_case(&shown, ty, desc, label));
    }
    // Drawn: the switch flipped or another item picked changes the frame,
    // the CPU's the GPU's, through the stage the grade is on. From what
    // the row needs to show (`shown_in_frame`), the first of its other
    // values that moves the frame.
    report.case_or_blocked(&format!("{id} {} on {surface:?} drawn", desc.key), [key(Interaction::Pixels)], || {
        let base = super::parity_grade_numeric::shown_in_frame(&shown, ty, desc);
        // By design (#2758): the timeline's grade grades no clip's media, so
        // there is no tracked subject for it to read.
        if desc.key == "matte_subject" && base.root == GradeRoot::Timeline {
            return Ok(());
        }
        let labels = menu_labels(desc, &pres);
        let at = shown_index(desc, &base.value(desc)).unwrap_or(0);
        let others: Vec<Option<usize>> = match pres.widget {
            Widget::Toggle => vec![None],
            _ => (0..labels.len()).filter(|i| *i != at).map(Some).collect(),
        };
        let mut last = String::from("the row has no other value");
        for other in others {
            let mut g = base.twin();
            g.drawn();
            match other {
                None => g.switch(label)?,
                Some(i) => g.pick(label, labels[at], labels[i])?,
            }
            if !g.unapplied.is_empty() {
                return Err(format!("unapplied: {:?}", g.unapplied));
            }
            match super::parity_pixels::edit_drawn(&base.frame_sequence(), &g.frame_sequence()) {
                Ok(()) => return Ok(()),
                Err(e) if e.starts_with("BLOCKED") => return Err(e),
                Err(e) => last = format!("{e} ({})", other.map_or_else(|| "switched".to_owned(), |i| format!("picking {:?}", labels[i]))),
            }
        }
        Err(last)
    });
    match pres.widget {
        Widget::Toggle => {
            let is_on = shown_index(desc, &shown.value(desc)) == Some(1);
            for on in [false, true] {
                let name = format!("{id} {} on {surface:?} switched {}", desc.key, if on { "on" } else { "off" });
                report.case(&name, [key(Interaction::Toggle(on)), key(Interaction::UndoRedo)], || {
                    // From the other way, so the click turns it `on`.
                    let mut start = shown.twin();
                    if is_on == on {
                        start.switch(label)?;
                    }
                    if !start.unapplied.is_empty() {
                        return Err(format!("from the other way: unapplied {:?}", start.unapplied));
                    }
                    grade_case(&start, desc, usize::from(on), |g| g.switch(label))
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
                    let mut start = shown.twin();
                    let from = if i == current { (i + 1) % labels.len() } else { current };
                    if from != current {
                        start.pick(label, labels[current], labels[from])?;
                    }
                    if !start.unapplied.is_empty() {
                        return Err(format!("from another item: unapplied {:?}", start.unapplied));
                    }
                    grade_case(&start, desc, i, |g| g.pick(label, labels[from], item))
                });
            }
        }
        _ => {}
    }
}

/// The Corrector's tab for each of its switches and menus, as the tabbed
/// page lays them out.
const CORRECTOR_TABS: &[(&str, crate::state::ColorTab)] = &[("wheels_log_mode", crate::state::ColorTab::Wheels), ("curves_preserve_luma", crate::state::ColorTab::Curves)];

/// The types whose switches and menus the tabbed page and the bottom tools
/// draw on a tab: the Corrector's, and a Color Wheels' and an RGB Curves'
/// (the log wheels and preserve-luma switch on their own tabs).
const PAGE_KINDS: [klipp_nodes::NodeKind; 3] = [klipp_nodes::NodeKind::Corrector, klipp_nodes::NodeKind::ColorWheels, klipp_nodes::NodeKind::RGBCurves];

/// Those switches and menus on the pages that share their rows: the tabbed
/// page in the right inspector, and the Colour page's bottom tools. A real
/// node, on its tab, through the generated rows; another type than the
/// Corrector with the Corrector off, since the pages edit an enabled
/// Corrector whatever node is selected.
pub(super) fn run_grade_pages(report: &mut Report) {
    for kind in PAGE_KINDS {
        run_grade_pages_of(report, kind);
    }
}

fn run_grade_pages_of(report: &mut Report, kind: klipp_nodes::NodeKind) {
    let ty = klipp_nodes::registry::node_type(kind).unwrap();
    let Some((mut state, clip, node)) = super::grade_inspector_tests::graded(kind) else {
        report.case(ty.desc.id, [], || Err("the grade won't take one".into()));
        return;
    };
    if kind != klipp_nodes::NodeKind::Corrector {
        if let Some(c) = state.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(clip)) {
            klipp_timeline::GradeAccess::edit_grade(c, |g| g.nodes.iter_mut().filter(|n| n.kind == klipp_nodes::NodeKind::Corrector).for_each(|n| n.enabled = false));
        }
    }
    let base = Grade::new(state, clip, node);
    for desc in ty.desc.params {
        if !matches!(ty.presentation(desc).widget, Widget::Toggle | Widget::Combo) {
            continue;
        }
        let Some(&(_, tab)) = CORRECTOR_TABS.iter().find(|(k, _)| *k == desc.key) else {
            // Another type's other switches are its per-node page's.
            if kind == klipp_nodes::NodeKind::Corrector {
                report.case(&format!("{} {}", ty.desc.id, desc.key), [], || Err("no tab known for it".into()));
            }
            continue;
        };
        for (page, surface) in [(Page::Tabbed(tab), Surface::GradeTabbedRight), (Page::Bottom(tab), Surface::GradeBottomTools)] {
            let on_page = Grade { page, ..base.twin() };
            discrete_cases(report, ty, desc, &on_page, surface);
        }
    }
}

/// The Corrector's switches and menus on its tabbed page and the bottom
/// tools store what they name.
#[test]
fn corrector_page_switches_and_menus_store_what_they_name() {
    let mut report = Report::default();
    report.family("grade pages", run_grade_pages);
    // The same cases with the grade on the timeline grade and a group's
    // stages, drawn through their stand-ins (M3 step 19).
    on_roots(&mut report, run_grade_pages);
    report.family_result().unwrap();
    assert_on_every_root(&report);
    assert!(!report.passed.is_empty(), "no switch or menu row was driven");
}

/// Every switch and menu row of a per-node grade yields its cases, so a
/// row the view can't drive is a failure, not a missing count.
#[test]
fn grade_switches_and_menus_store_what_they_name() {
    let mut report = Report::default();
    report.family("grade controls", run_grade);
    // The same cases with the grade on the timeline grade and a group's
    // stages, drawn through their stand-ins (M3 step 19).
    on_roots(&mut report, run_grade);
    report.family_result().unwrap();
    assert_on_every_root(&report);
    assert!(!report.passed.is_empty(), "no switch or menu row was driven");
}

/// The tabbed page's tabs, each drawn on both pages.
const TABS: [crate::state::ColorTab; 5] = [crate::state::ColorTab::Adjustments, crate::state::ColorTab::Wheels, crate::state::ColorTab::Curves, crate::state::ColorTab::HueSat, crate::state::ColorTab::Warper];

/// Opening any tab of the Corrector's pages writes nothing: the picture
/// as it was before the first draw, no step, nothing sent that wasn't
/// applied. The Curves, Hue/Sat and Warper editors wrote
/// their defaults on their first draw (#1392).
#[test]
fn opening_a_corrector_page_tab_writes_nothing() {
    let (state, clip, node) = super::grade_inspector_tests::graded(klipp_nodes::NodeKind::Corrector).unwrap();
    let base = Grade::new(state, clip, node);
    let mut wrote = Vec::new();
    for tab in TABS {
        for page in [Page::Tabbed(tab), Page::Bottom(tab)] {
            let mut g = Grade { page, ..base.twin() };
            let before = g.picture();
            g.drawn();
            g.drawn();
            if g.picture() != before || g.state.active_history().undo_count() != 0 || !g.unapplied.is_empty() {
                wrote.push(format!("{page:?}: steps {}, unapplied {:?}", g.state.active_history().undo_count(), g.unapplied));
            }
        }
    }
    assert!(wrote.is_empty(), "opening wrote:\n{}", wrote.join("\n"));
}

/// A curve edited after its tab is opened (a point dragged in over
/// several ticks) is one undo step (IC13), and undoing it leaves the curves unset, as they were, not their
/// stored defaults (#1392).
#[test]
fn a_curve_edit_after_opening_is_one_step_back_to_unset() {
    let (state, clip, node) = super::grade_inspector_tests::graded(klipp_nodes::NodeKind::Corrector).unwrap();
    let base = Grade::new(state, clip, node);
    let curves = |g: &Grade| g.grade().nodes.iter().find(|n| n.id == g.node).map(|n| n.params.curves.clone()).unwrap();
    for page in [Page::Tabbed(crate::state::ColorTab::Curves), Page::Bottom(crate::state::ColorTab::Curves)] {
        let mut g = Grade { page, ..base.twin() };
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
        assert!(curves(&g).is_some(), "{page:?}: no click edited the curve");
        assert!(g.unapplied.is_empty(), "{:?}", g.unapplied);
        assert_eq!(g.state.active_history().undo_count(), 1, "{page:?}: one step");
        g.step(false).unwrap();
        assert!(curves(&g).is_none(), "{page:?}: undo leaves the curves unset");
    }
}


/// A Colour Warper sector moved on the Corrector's pages is one step,
/// and undoing it leaves the warper unset, as it was: its
/// sector commands (`SetNodeParam`, inside the drag's gesture) carry the
/// edit, and the gesture starts from the grade before the press (#1392).
#[test]
fn a_warper_move_after_opening_is_one_step_back_to_unset() {
    let (state, clip, node) = super::grade_inspector_tests::graded(klipp_nodes::NodeKind::Corrector).unwrap();
    let base = Grade::new(state, clip, node);
    let warper = |g: &Grade| g.grade().nodes.iter().find(|n| n.id == g.node).map(|n| n.params.warper_params.clone()).unwrap();
    const SECTORS: [&str; 12] = ["R", "O", "Y", "YG", "G", "CG", "C", "CB", "B", "BM", "M", "MR"];
    for page in [Page::Tabbed(crate::state::ColorTab::Warper), Page::Bottom(crate::state::ColorTab::Warper)] {
        let mut g = Grade { page, ..base.twin() };
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
        assert!(warper(&g).is_some(), "{page:?}: no drag moved a sector");
        assert!(g.unapplied.is_empty(), "{:?}", g.unapplied);
        assert_eq!(g.state.active_history().undo_count(), 1, "{page:?}: one step");
        g.step(false).unwrap();
        assert!(warper(&g).is_none(), "{page:?}: undo leaves the warper unset");
    }
}

/// Escape during a curve drag takes the drag back: the curves as they
/// were (unset), and no undo step.
#[test]
fn escape_takes_back_a_curve_drag() {
    let (state, clip, node) = super::grade_inspector_tests::graded(klipp_nodes::NodeKind::Corrector).unwrap();
    let base = Grade::new(state, clip, node);
    let curves = |g: &Grade| g.grade().nodes.iter().find(|n| n.id == g.node).map(|n| n.params.curves.clone()).unwrap();
    for page in [Page::Tabbed(crate::state::ColorTab::Curves), Page::Bottom(crate::state::ColorTab::Curves)] {
        let mut g = Grade { page, ..base.twin() };
        let before = { g.drawn(); g.picture() };
        let drawn = g.drawn();
        let strip = drawn.iter().filter(|(t, _)| ["M", "R", "G", "B"].contains(&t.as_str())).map(|(_, p)| p.y).fold(f32::MIN, f32::max);
        let at = egui::pos2(150.0, strip + 140.0);
        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        g.frame(vec![egui::Event::PointerMoved(at), button(at, true)]);
        for dy in [8.0, 16.0, 24.0] {
            g.frame(vec![egui::Event::PointerMoved(at - egui::vec2(0.0, dy))]);
        }
        assert!(curves(&g).is_some(), "{page:?}: the drag moved the curve");
        g.frame(vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
        g.frame(vec![button(at - egui::vec2(0.0, 24.0), false)]);
        g.frame(Vec::new());
        assert!(g.unapplied.is_empty(), "{:?}", g.unapplied);
        assert_eq!(g.state.active_history().undo_count(), 0, "{page:?}: no step");
        assert!(curves(&g).is_none(), "{page:?}: the curves as they were");
        assert!(g.picture() == before, "{page:?}: the picture as it was");
    }
}

/// Opening any grade node the per-node layout draws writes nothing: the
/// picture as it was before the first draw, no step, nothing sent. A
/// default stored on draw (a mask stack, a difference key, a layer mixer)
/// would change the project unrecorded (the class of #1392).
#[test]
fn opening_a_per_node_grade_writes_nothing() {
    let mut wrote = Vec::new();
    let mut opened = 0;
    for ty in klipp_nodes::registry::node_types() {
        if !super::main_layout::per_node_kind(ty.kind) {
            continue;
        }
        let Some((state, clip, node)) = placed(ty.kind) else { continue };
        let base = Grade::new(state, clip, node);
        let mut g = base.twin();
        let before = g.picture();
        g.drawn();
        g.drawn();
        if g.picture() != before || g.state.active_history().undo_count() != 0 || !g.unapplied.is_empty() {
            let params = |p: &GradePicture| format!("{p:?}").len();
            wrote.push(format!("{}: steps {}, unapplied {:?} ({} → {} bytes)", ty.desc.id, g.state.active_history().undo_count(), g.unapplied, params(&before), params(&g.picture())));
        }
        opened += 1;
    }
    assert!(wrote.is_empty(), "opening wrote:\n{}", wrote.join("\n"));
    // Each per-node kind once (the old panel's run is gone).
    assert!(opened > 10, "only {opened} opened");
}

/// A colour wheel's drag on the per-node layout (`wheel_block`) is one
/// step, and Escape during it takes it back: the picture as
/// it was, and no step.
#[test]
fn a_wheel_drag_is_one_step_and_escape_takes_it_back() {
    let Some((state, clip, node)) = super::grade_inspector_tests::graded(klipp_nodes::NodeKind::ColorWheels) else { panic!("a Color Wheels node") };
    let base = Grade::new(state, clip, node);
    let lift = |g: &Grade| g.grade().nodes.iter().find(|n| n.id == g.node).map(|n| (n.params.lift_x, n.params.lift_y)).unwrap();
    for escape in [false, true] {
        let mut g = base.twin();
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
        assert_ne!(lift(&g), (None, None), "the drag moved the wheel");
        if escape {
            g.frame(vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
        }
        g.frame(vec![button(centre + egui::vec2(18.0, -18.0), false)]);
        g.frame(Vec::new());
        assert!(g.unapplied.is_empty(), "{:?}", g.unapplied);
        if escape {
            assert_eq!(g.state.active_history().undo_count(), 0, "Escape leaves no step");
            assert!(g.picture() == before, "Escape gives back the picture");
        } else {
            assert_eq!(g.state.active_history().undo_count(), 1, "one step");
            g.step(false).unwrap();
            assert!(g.picture() == before, "its undo gives back the picture");
        }
    }
}
