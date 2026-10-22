//! One parameter's row, drawn from what the parameter is ([`ParamDesc`])
//! and how it's shown ([`Presentation`]), writing by path.
//!
//! A row reads the node and never writes it: it pushes path commands
//! (`SetParamAt`, the key commands, `SeekTo`) and the handler makes the
//! change, so drawing has no side effect. It generalises today's
//! `scrubby_param_row` plus the effects tab's post-row logic:
//! - a drag is `BeginParamEditAt`, a `SetParamAt` per move, then
//!   `EndParamEditAt`: one undo step; a typed value or a reset is one
//!   `SetParamAt`, one step too;
//! - on a parameter with keys, a changed value sets a key at the playhead
//!   (`AddKeyAt`) instead of the stored value, which isn't what's shown;
//! - the diamond, on a keyframable parameter only, adds or removes the
//!   key at the playhead, and previous/next seek to the neighbouring key;
//! - a reset writes the presentation's reset hint, or the default;
//! - a choice's labels name its stored index and never rename it;
//! - a value the row can't read as its kind is shown read-only and kept.

use super::custom;
use crate::widgets::scrubby_param::{scrubby_param_row, KeyframeState};
use klipp_core::{AppCommand, Interpolation, Keyframe};
use klipp_effects::registry::presentation::{Presentation, Suffix, Widget};
use klipp_effects::registry::{ParamDesc, ParamKind};
use klipp_graph_format::path::NodePath;
use klipp_graph_format::{ParamKey, ParamValue};
use klipp_grade_model::Encoding;
use serde_json::Value;

/// Where a row writes and what time it's drawn at.
pub struct RowCtx<'a> {
    /// The node the row's parameter belongs to.
    pub addr: &'a NodePath,
    /// The playhead, in the clip's own seconds (where keys are timed).
    pub clip_time_secs: f64,
    /// Where the clip starts on the timeline, and its rate, for a seek to
    /// a key.
    pub timeline_in_secs: f64,
    pub rate: u32,
    /// The grade encoding in force, for a value shown encoded.
    pub encoding: Encoding,
    /// The node's stored entry, where it has one (a built-in effect's
    /// settings, a grade node's v1 entry): a value that isn't a number or
    /// a switch (a choice stored by name, a colour) is written by editing
    /// it whole (`SetPayloadAt`).
    pub entry: Option<Entry<'a>>,
    /// The path commands the rows imply.
    pub cmds: &'a mut Vec<AppCommand>,
    /// Draws a parameter's Behaviours (its modifier stack, as
    /// `inspector::modifier_ui` does) for the row's ⋯ menu; `None` leaves
    /// the menu out (IC9: only the generated rows have it).
    pub behaviours: Option<&'a dyn Fn(&mut egui::Ui, &klipp_core::ParamId, &mut Vec<AppCommand>)>,
    /// The theme and the app, for a named editor that needs them (a
    /// plug-in's warnings read the project's size).
    pub theme: &'a crate::theme::KlippTheme,
    pub app: Option<&'a crate::state::AppState>,
    /// The view's own specialist editors, asked first for a named editor
    /// (`Widget::Custom`) or a wheel's part (as `"wheels"`): whether it
    /// drew the row. A view whose editors need its own state (the grade
    /// inspector's wheels and curves) answers here; `None` leaves the
    /// registered editors.
    pub custom: Option<&'a mut CustomHook<'a>>,
}

/// A view's specialist editors (`RowCtx::custom`): given the editor's id,
/// the parameter and the commands to push into, whether it drew it.
pub type CustomHook<'a> = dyn FnMut(&mut egui::Ui, &str, &ParamDesc, &mut Vec<AppCommand>) -> bool + 'a;

/// A node's stored entry, and where in it its parameters' fields sit.
#[derive(Clone, Copy)]
pub struct Entry<'a> {
    pub payload: &'a Value,
    /// The object holding the fields: `[]` for an effect's settings,
    /// `["params"]` for a grade node's entry.
    pub fields_at: &'static [&'static str],
    /// A choice is written as its index (`SetParamAt`), not its name: a
    /// grade node's are, as the old panel's `SetNodeParam` wrote them,
    /// since its stored names are its own types', not the catalog's.
    pub choices_by_index: bool,
}

/// A parameter's keys around the playhead, in the clip's seconds.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct KeyState {
    pub any: bool,
    pub at_playhead: bool,
    pub in_frame: usize,
    pub prev: Option<f64>,
    pub next: Option<f64>,
    pub interpolation: Option<Interpolation>,
}

/// One row's input.
pub struct RowInput<'a> {
    pub desc: &'a ParamDesc,
    pub pres: &'a Presentation,
    /// The value as stored on the node, before animation (a switch as a
    /// bool, a choice as its index or its name, a colour as its bytes).
    pub stored: &'a Value,
    /// The animated value at the playhead, when the parameter is keyed:
    /// what the row shows then.
    pub shown_at_t: Option<f32>,
    pub keys: KeyState,
}

/// What a row did this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowOutcome {
    Nothing,
    Edited,
    Keyed,
    Reset,
    Navigated,
}

/// The row's label: the presentation's, else the catalog's.
pub fn label(input: &RowInput) -> &'static str {
    input.pres.label.unwrap_or(input.desc.label)
}

/// What a reset writes: the presentation's hint, else the default.
pub fn reset_value(desc: &ParamDesc, pres: &Presentation) -> f32 {
    pres.reset.unwrap_or(desc.default)
}

/// Whether the row draws a diamond: a parameter keyframable, set by hand.
pub fn has_diamond(desc: &ParamDesc) -> bool {
    desc.keyframable && !desc.derived
}

/// `stored` as a number, for a numeric parameter.
pub fn number(stored: &Value) -> Option<f32> {
    match stored {
        Value::Number(n) => n.as_f64().map(|x| x as f32),
        Value::Bool(b) => Some(f32::from(u8::from(*b))),
        _ => None,
    }
}

/// A choice's name as stored names are matched against it: its letters
/// and digits, whatever the case ("Rec.709 linear" is stored as
/// "Rec709Linear", "Box Blur" as "box_blur").
pub fn plain_name(name: &str) -> String {
    name.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect()
}

/// `stored` as a choice's index: stored by name or by index.
pub fn choice_index(desc: &ParamDesc, stored: &Value) -> Option<usize> {
    let ParamKind::Choice(names) = desc.kind else { return None };
    match stored {
        Value::String(s) => names.iter().position(|n| plain_name(n) == plain_name(s)),
        Value::Number(n) => n.as_u64().map(|i| i as usize).filter(|i| *i < names.len()),
        Value::Bool(b) => Some(usize::from(*b)).filter(|i| *i < names.len()),
        _ => None,
    }
}

/// The value written for a number shown as `shown`.
fn number_value(desc: &ParamDesc, shown: f32) -> ParamValue {
    match desc.kind {
        ParamKind::Whole => ParamValue::Int(shown.round() as i32),
        _ => ParamValue::Float(shown),
    }
}

fn set(ctx: &mut RowCtx, desc: &ParamDesc, value: ParamValue) {
    ctx.cmds.push(AppCommand::SetParamAt { addr: ctx.addr.clone(), key: ParamKey::new(desc.key), value });
}

/// Write `new`, a value in the form the node stores it: a number or a
/// switch by `SetParamAt`; anything else (a choice's name, a colour's
/// bytes) by the node's entry with that one field changed. `false` when
/// it can't be written here (no entry to edit).
fn write_stored(ctx: &mut RowCtx, desc: &ParamDesc, new: Value) -> bool {
    match &new {
        Value::Bool(b) => set(ctx, desc, ParamValue::Bool(*b)),
        Value::Number(n) if n.is_i64() || n.is_u64() => set(ctx, desc, ParamValue::Int(n.as_i64().unwrap_or(0) as i32)),
        Value::Number(n) => set(ctx, desc, ParamValue::Float(n.as_f64().unwrap_or(0.0) as f32)),
        _ => {
            let Some(entry) = ctx.entry else { return false };
            let mut payload = entry.payload.clone();
            let mut at = &mut payload;
            for step in entry.fields_at {
                let Some(next) = at.get_mut(*step) else { return false };
                at = next;
            }
            // A grade field may sit in a struct: `grain.amount`.
            let mut steps = desc.field.split('.').peekable();
            let mut field = None;
            while let Some(step) = steps.next() {
                let Some(next) = at.get_mut(step) else { return false };
                if steps.peek().is_none() {
                    field = Some(next);
                    break;
                }
                at = next;
            }
            let Some(field) = field else { return false };
            match desc.index {
                Some(i) => match field.get_mut(i) {
                    Some(slot) => *slot = new,
                    None => return false,
                },
                None => *field = new,
            }
            ctx.cmds.push(AppCommand::SetPayloadAt { addr: ctx.addr.clone(), payload });
        }
    }
    true
}

fn add_key(ctx: &mut RowCtx, desc: &ParamDesc, value: f32) {
    let key = Keyframe { time_secs: ctx.clip_time_secs, value, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
    ctx.cmds.push(AppCommand::AddKeyAt {
        addr: ctx.addr.clone(),
        key: ParamKey::new(desc.key),
        keyframe: serde_json::to_value(key).unwrap_or(Value::Null),
    });
}

/// A switch's or a choice's key: the stored number (a switch's 0 or 1, a
/// choice's index), held until the next key rather than blended.
fn add_step_key(ctx: &mut RowCtx, desc: &ParamDesc, value: f32) {
    let key = Keyframe { time_secs: ctx.clip_time_secs, value, interpolation: Interpolation::Step, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
    ctx.cmds.push(AppCommand::AddKeyAt {
        addr: ctx.addr.clone(),
        key: ParamKey::new(desc.key),
        keyframe: serde_json::to_value(key).unwrap_or(Value::Null),
    });
}

/// A keyframable switch's or choice's diamond and key arrows (#1318): a
/// click keys `value` at the playhead as a step, by path, or takes the
/// key there away; the arrows seek to the previous or next key. `None`
/// when they did nothing, or the parameter can't be keyed.
fn stepped_keys(ui: &mut egui::Ui, ctx: &mut RowCtx, desc: &ParamDesc, keys: KeyState, value: f32) -> Option<RowOutcome> {
    if !has_diamond(desc) {
        return None;
    }
    let r = crate::widgets::scrubby_param::key_controls(ui, KeyframeState {
        has_keyframe_at_playhead: keys.at_playhead,
        has_any_keyframes: keys.any,
        keys_in_frame: keys.in_frame,
        prev_keyframe_time: keys.prev,
        next_keyframe_time: keys.next,
        current_interpolation: keys.interpolation,
    });
    if r.toggled {
        if keys.at_playhead {
            ctx.cmds.push(AppCommand::RemoveKeyAt { addr: ctx.addr.clone(), key: ParamKey::new(desc.key), time_secs: ctx.clip_time_secs });
        } else {
            add_step_key(ctx, desc, value);
        }
        return Some(RowOutcome::Keyed);
    }
    let to = if r.prev { keys.prev } else if r.next { keys.next } else { None };
    to.map(|t| {
        seek(ctx, t);
        RowOutcome::Navigated
    })
}

fn seek(ctx: &mut RowCtx, clip_secs: f64) {
    ctx.cmds.push(AppCommand::SeekTo(klipp_core::Timecode::from_secs_f64(ctx.timeline_in_secs + clip_secs, ctx.rate)));
}

/// The slider's range, shown: the catalog's soft range, a whole number's
/// declared one, else its hard range, else 0 to 1.
pub fn shown_range(desc: &ParamDesc, pres: &Presentation, encoding: Encoding) -> (f32, f32) {
    let raw = pres.soft_whole.map(|(lo, hi)| (lo as f32, hi as f32)).or(desc.soft).or(desc.range).unwrap_or((0.0, 1.0));
    match pres.display {
        Some(map) => ((map.to_shown)(raw.0, encoding), (map.to_shown)(raw.1, encoding)),
        None => raw,
    }
}

/// Draw one parameter's row and push the path commands it implies.
pub fn param_row(ui: &mut egui::Ui, ctx: &mut RowCtx, input: RowInput) -> RowOutcome {
    let RowInput { desc, pres, stored, shown_at_t, keys } = input;
    match pres.widget {
        Widget::Hidden => RowOutcome::Nothing,
        // Shown to the presentation's decimals, never edited here (#1421).
        Widget::Readout => {
            let shown = number(stored).map(|v| serde_json::Value::String(format!("{v:.*}", pres.decimals as usize))).unwrap_or_else(|| stored.clone());
            read_only(ui, desc.key, pres.label.unwrap_or(desc.label), &shown, "Measured by the node's tool")
        }
        Widget::Custom(id) if ctx.custom.as_mut().is_some_and(|hook| hook(ui, id, desc, ctx.cmds)) => RowOutcome::Nothing,
        Widget::WheelPart(_) if ctx.custom.as_mut().is_some_and(|hook| hook(ui, "wheels", desc, ctx.cmds)) => RowOutcome::Nothing,
        Widget::Custom(id) => match custom::editor(id) {
            Some(editor) => {
                editor.show(ui, ctx, desc, stored);
                RowOutcome::Nothing
            }
            None => read_only(ui, desc.key, pres.label.unwrap_or(desc.label), stored, id),
        },
        Widget::Toggle => toggle_row(ui, ctx, RowInput { desc, pres, stored, shown_at_t, keys }),
        Widget::Combo => combo_row(ui, ctx, RowInput { desc, pres, stored, shown_at_t, keys }),
        Widget::ColourRgb | Widget::ColourRgba => colour_row(ui, ctx, desc, pres, stored),
        Widget::Slider if !has_diamond(desc) => slider_row(ui, ctx, desc, pres, stored),
        Widget::Scrub | Widget::Slider | Widget::Gradient(_) | Widget::HueRainbow | Widget::WheelPart(_) | Widget::Vec2 => {
            numeric_row(ui, ctx, RowInput { desc, pres, stored, shown_at_t, keys })
        }
    }
}

/// A value the row doesn't draw an editor for: its label and its stored
/// text, never written.
fn read_only(ui: &mut egui::Ui, key: &str, label: &str, stored: &Value, why: &str) -> RowOutcome {
    // The key is only the recorder's.
    let _ = key;
    #[cfg(any(test, feature = "row-recorder"))]
    crate::widgets::row_recorder::control(ui.next_auto_id(), "read_only", key, label, stored.clone());
    ui.horizontal(|ui| {
        ui.label(label);
        let text = match stored {
            Value::Null => String::from("—"),
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        ui.weak(text).on_hover_text(if why.is_empty() { "Not editable here" } else { why });
    });
    RowOutcome::Nothing
}

fn numeric_row(ui: &mut egui::Ui, ctx: &mut RowCtx, input: RowInput) -> RowOutcome {
    let RowInput { desc, pres, stored, shown_at_t, keys } = input;
    let label = pres.label.unwrap_or(desc.label);
    let Some(stored_number) = number(stored) else {
        return read_only(ui, desc.key, label, stored, "");
    };
    let encoding = ctx.encoding;
    let to_shown = |v: f32| pres.display.map_or(v, |m| (m.to_shown)(v, encoding));
    let to_stored = |v: f32| pres.display.map_or(v, |m| (m.to_stored)(v, encoding));
    let mut shown = to_shown(shown_at_t.unwrap_or(stored_number));
    // Include both the stored and reset values (perspective's 0 means
    // "unset"). The DragValue must not clamp a label reset back into the
    // soft range later in the same frame; drawing never writes either.
    let (lo, hi) = shown_range(desc, pres, ctx.encoding);
    // A grade row drags as the old panel's (`grade_drag`): the value it
    // shows, over its shown span, unrounded, then stored by the display's
    // conversion, as the old rows convert theirs (M3-4).
    let grade_span = (hi - lo).abs();
    // What a grade drag may reach, as the old rows clamp it: the row's
    // range in shown units, then the stored value to its stored range.
    let shown_bounds = (lo.min(hi), lo.max(hi));
    let stored_bounds = (to_stored(lo).min(to_stored(hi)), to_stored(lo).max(to_stored(hi)));
    let reset = to_shown(reset_value(desc, pres));
    let (lo, hi) = (lo.min(shown).min(reset), hi.max(shown).max(reset));
    let before = shown;
    let suffix = match pres.suffix {
        Suffix::None => String::new(),
        s => {
            let t = s.text(desc.unit);
            if t.is_empty() || t == "×" || t.starts_with(':') { t.to_owned() } else { format!(" {t}") }
        }
    };
    let diamond = has_diamond(desc).then_some(KeyframeState {
        has_keyframe_at_playhead: keys.at_playhead,
        has_any_keyframes: keys.any,
        keys_in_frame: keys.in_frame,
        prev_keyframe_time: keys.prev,
        next_keyframe_time: keys.next,
        current_interpolation: keys.interpolation,
    });
    #[cfg(any(test, feature = "row-recorder"))]
    crate::widgets::row_recorder::next_parameter(desc.key);
    // The stored value a grade drag left, kept as is rather than read back
    // from the shown one, which a display map would round.
    let dragged: std::cell::Cell<Option<f32>> = std::cell::Cell::new(None);
    let r = if is_grade(ctx) {
        // From the stored value each time, shown as the old row shows it.
        let stored_now = shown_at_t.unwrap_or(stored_number);
        let stored_now = drag_carry(ui, desc, stored_now).unwrap_or(stored_now);
        let rule = |_shown: f32, dx: f32| {
            let moved = crate::widgets::grade_drag::grade_drag(to_shown(dragged.get().unwrap_or(stored_now)), dx, grade_span);
            let next = to_stored(moved.clamp(shown_bounds.0, shown_bounds.1)).clamp(stored_bounds.0, stored_bounds.1);
            dragged.set(Some(next));
            to_shown(next)
        };
        crate::widgets::scrubby_param::scrubby_param_row_dragged(ui, label, &mut shown, lo..=hi, reset, drag_speed(pres.step), usize::from(pres.decimals), &suffix, diamond, &rule)
    } else {
        scrubby_param_row(ui, label, &mut shown, lo..=hi, reset, drag_speed(pres.step), usize::from(pres.decimals), &suffix, diamond)
    };
    if is_grade(ctx) {
        keep_drag_carry(ui, desc, dragged.get().filter(|_| !r.drag_stopped));
    }
    if has_diamond(desc) {
        behaviours_menu(ui, ctx, desc);
    }
    let key = ParamKey::new(desc.key);
    // egui reports Escape as drag_stopped before drawing this row. It
    // abandons the gesture, rather than committing the value as release
    // does; the path drag's snapshot also restores any animated value.
    if r.drag_stopped && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        ctx.cmds.push(AppCommand::AbandonParamEditAt { addr: ctx.addr.clone(), key });
        ui.ctx().stop_dragging();
        return RowOutcome::Nothing;
    }
    let mut outcome = RowOutcome::Nothing;
    if r.drag_started {
        ctx.cmds.push(AppCommand::BeginParamEditAt { addr: ctx.addr.clone(), key: key.clone() });
    }
    if r.changed && shown != before {
        let value = dragged.get().filter(|_| !r.reset_clicked).unwrap_or_else(|| to_stored(shown));
        if keys.any && has_diamond(desc) {
            // Keyed: what's shown is the animation, so the edit is a key.
            add_key(ctx, desc, value);
            outcome = RowOutcome::Keyed;
        } else {
            set(ctx, desc, number_value(desc, value));
            outcome = if r.reset_clicked { RowOutcome::Reset } else { RowOutcome::Edited };
        }
    }
    if r.drag_stopped {
        ctx.cmds.push(AppCommand::EndParamEditAt { addr: ctx.addr.clone(), key: key.clone() });
    }
    if r.keyframe_toggled && has_diamond(desc) {
        if keys.at_playhead {
            ctx.cmds.push(AppCommand::RemoveKeyAt { addr: ctx.addr.clone(), key: key.clone(), time_secs: ctx.clip_time_secs });
        } else {
            add_key(ctx, desc, to_stored(shown));
        }
        outcome = RowOutcome::Keyed;
    }
    if let Some(interpolation) = r.keyframe_set_interpolation.filter(|_| keys.at_playhead) {
        ctx.cmds.push(AppCommand::SetInterpKeyAt { addr: ctx.addr.clone(), key: key.clone(), time_secs: ctx.clip_time_secs, interpolation });
        outcome = RowOutcome::Keyed;
    }
    if let Some(preset) = r.keyframe_ease_preset.filter(|_| keys.at_playhead) {
        ctx.cmds.push(AppCommand::EaseKeyAt { addr: ctx.addr.clone(), key: key.clone(), time_secs: ctx.clip_time_secs, ease: preset.key_ease() });
        outcome = RowOutcome::Keyed;
    }
    if r.keyframe_nav_prev {
        if let Some(t) = keys.prev {
            seek(ctx, t);
            outcome = RowOutcome::Navigated;
        }
    }
    if r.keyframe_nav_next {
        if let Some(t) = keys.next {
            seek(ctx, t);
            outcome = RowOutcome::Navigated;
        }
    }
    outcome
}

/// The row's ⋯ menu: the parameter's Behaviours, addressed by path
/// (`ParamId::Node`), on a parameter that can be animated.
fn behaviours_menu(ui: &mut egui::Ui, ctx: &mut RowCtx, desc: &ParamDesc) {
    let Some(show) = ctx.behaviours else { return };
    let param = klipp_core::ParamId::Node { addr: ctx.addr.clone(), key: ParamKey::new(desc.key) };
    let cmds = &mut *ctx.cmds;
    ui.menu_button("⋯", |ui| show(ui, &param, cmds)).response.on_hover_text("Behaviours");
}

/// Whether the row edits a grade node's settings (its entry's fields sit
/// under `params`), which drag by the grade rule.
fn is_grade(ctx: &RowCtx) -> bool {
    ctx.entry.as_ref().is_some_and(|e| e.fields_at == ["params"])
}

/// A grade drag's own value, kept across its frames: the unrounded value
/// it reached, while the stored one is still what it wrote. A whole-number
/// row stores its value rounded, and a slow drag restarting from that each
/// frame would never move; a float row stores what it reached, so this is
/// that value again.
fn drag_carry(ui: &egui::Ui, desc: &ParamDesc, stored: f32) -> Option<f32> {
    let id = ui.id().with(("grade_drag_carry", desc.key));
    ui.data(|d| d.get_temp::<(f32, f32)>(id)).filter(|(written, _)| *written == stored).map(|(_, reached)| reached)
}

fn keep_drag_carry(ui: &egui::Ui, desc: &ParamDesc, reached: Option<f32>) {
    let id = ui.id().with(("grade_drag_carry", desc.key));
    match reached {
        Some(v) => {
            let written = if desc.kind == ParamKind::Whole { v.round() } else { v };
            ui.data_mut(|d| d.insert_temp(id, (written, v)));
        }
        None => ui.data_mut(|d| d.remove::<(f32, f32)>(id)),
    }
}

/// The drag speed for a declared step, as the hand-written rows pass it:
/// the step's decimal value, so 0.01 is 0.01, not the `f32`'s widening
/// (0.009999999776), which a drag's rounding would show as one step lost
/// every few (the parity harness, M3 step 1c, found it).
pub fn drag_speed(step: f32) -> f64 {
    step.to_string().parse().unwrap_or(f64::from(step))
}

/// A plain slider over the shown range, for a value set by hand and never
/// keyed (a seed, a sample count): a drag is one undo step.
fn slider_row(ui: &mut egui::Ui, ctx: &mut RowCtx, desc: &ParamDesc, pres: &Presentation, stored: &Value) -> RowOutcome {
    let label = pres.label.unwrap_or(desc.label);
    let Some(mut v) = number(stored) else { return read_only(ui, desc.key, label, stored, "") };
    let (lo, hi) = shown_range(desc, pres, ctx.encoding);
    let reset = reset_value(desc, pres);
    let (lo, hi) = (lo.min(v).min(reset), hi.max(v).max(reset));
    #[cfg(any(test, feature = "row-recorder"))]
    {
        crate::widgets::row_recorder::numeric(ui.next_auto_id(), "slider", label, v, &(lo..=hi), None, None, None, "", None);
        crate::widgets::row_recorder::parameter(desc.key);
    }
    let before = v;
    // A grade slider's number drags as the old panel's (`grade_drag`, its
    // stored value, a span per 800 pixels, unrounded); its rail stays.
    let grade_span = is_grade(ctx).then(|| {
        let (slo, shi) = shown_range(desc, pres, ctx.encoding);
        (shi - slo).abs()
    });
    let (r, reset_clicked) = ui
        .horizontal(|ui| {
            let reset_clicked = ui.add(egui::Label::new(label).sense(egui::Sense::click())).clicked();
            if reset_clicked { v = reset; }
            match grade_span {
                Some(span) => {
                    let before_drag = drag_carry(ui, desc, v).unwrap_or(v);
                    let number = ui.add(egui::DragValue::new(&mut v).range(lo..=hi).speed(span / 800.0));
                    if number.dragged() {
                        let pace = ui.input(|i| if i.modifiers.shift_only() { 0.1 } else if i.modifiers.alt { 10.0 } else { 1.0 });
                        v = crate::widgets::grade_drag::grade_drag(before_drag, number.drag_delta().x * pace, span).clamp(lo, hi);
                    }
                    keep_drag_carry(ui, desc, (number.dragged() && !number.drag_stopped()).then_some(v));
                    let slider = egui::Slider::new(&mut v, lo..=hi).show_value(false);
                    let rail = ui.add(if desc.kind == ParamKind::Whole { slider.integer() } else { slider });
                    (number.union(rail), reset_clicked)
                }
                None => {
                    let slider = egui::Slider::new(&mut v, lo..=hi);
                    (ui.add(if desc.kind == ParamKind::Whole { slider.integer() } else { slider }), reset_clicked)
                }
            }
        })
        .inner;
    let key = ParamKey::new(desc.key);
    if r.drag_stopped() && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        ctx.cmds.push(AppCommand::AbandonParamEditAt { addr: ctx.addr.clone(), key });
        ui.ctx().stop_dragging();
        return RowOutcome::Nothing;
    }
    if r.drag_started() {
        ctx.cmds.push(AppCommand::BeginParamEditAt { addr: ctx.addr.clone(), key: key.clone() });
    }
    let edited = (r.changed() || reset_clicked) && v != before;
    if edited {
        set(ctx, desc, number_value(desc, v));
    }
    if r.drag_stopped() {
        ctx.cmds.push(AppCommand::EndParamEditAt { addr: ctx.addr.clone(), key });
    }
    if edited && reset_clicked { RowOutcome::Reset } else if edited { RowOutcome::Edited } else { RowOutcome::Nothing }
}

/// A switch, or a choice of two (off then on) shown as one.
fn toggle_row(ui: &mut egui::Ui, ctx: &mut RowCtx, input: RowInput) -> RowOutcome {
    let RowInput { desc, pres, stored, shown_at_t, keys } = input;
    let label = pres.label.unwrap_or(desc.label);
    let on = match desc.kind {
        ParamKind::Choice(_) => choice_index(desc, stored).map(|i| i == 1),
        _ => match stored {
            Value::Bool(b) => Some(*b),
            Value::Number(n) => n.as_f64().map(|x| x != 0.0),
            _ => None,
        },
    };
    let Some(mut on) = on else { return read_only(ui, desc.key, label, stored, "") };
    // Keyed: the switch shows the animation at the playhead, and a click
    // on it is a key there.
    let keyed = keys.any && has_diamond(desc);
    if let Some(v) = shown_at_t.filter(|_| keyed) {
        on = v > 0.5;
    }
    let shown = on;
    let (changed, stepped) = ui
        .horizontal(|ui| {
            #[cfg(any(test, feature = "row-recorder"))]
            crate::widgets::row_recorder::control(ui.next_auto_id(), "checkbox", desc.key, label, on);
            let changed = ui.checkbox(&mut on, label).changed();
            (changed, stepped_keys(ui, ctx, desc, keys, f32::from(u8::from(shown))))
        })
        .inner;
    if let Some(outcome) = stepped {
        return outcome;
    }
    if !changed {
        return RowOutcome::Nothing;
    }
    if keyed {
        add_step_key(ctx, desc, f32::from(u8::from(on)));
        return RowOutcome::Keyed;
    }
    // In the stored form: a switch's bool or number, a choice's index or
    // name.
    let by_index = ctx.entry.as_ref().is_some_and(|e| e.choices_by_index);
    let new = match (desc.kind, stored) {
        (ParamKind::Choice(_), _) if by_index => Value::from(u32::from(on)),
        (ParamKind::Choice(names), Value::String(_)) => Value::from(names[usize::from(on)]),
        (_, Value::Bool(_)) => Value::Bool(on),
        _ => Value::from(u32::from(on)),
    };
    if write_stored(ctx, desc, new) { RowOutcome::Edited } else { RowOutcome::Nothing }
}

/// A choice's drop-down: its labels name the stored index.
fn combo_row(ui: &mut egui::Ui, ctx: &mut RowCtx, input: RowInput) -> RowOutcome {
    let RowInput { desc, pres, stored, shown_at_t, keys } = input;
    let label = pres.label.unwrap_or(desc.label);
    let names: Vec<&str> = match (desc.kind, pres.labels) {
        (_, Some(labels)) => labels.to_vec(),
        (ParamKind::Choice(names), None) => names.to_vec(),
        _ => Vec::new(),
    };
    let index = match desc.kind {
        ParamKind::Bool => number(stored).map(|v| usize::from(v != 0.0)),
        _ => choice_index(desc, stored),
    };
    let Some(mut index) = index.filter(|i| *i < names.len()) else {
        return read_only(ui, desc.key, label, stored, "");
    };
    // Keyed: the choice shows the animation at the playhead, and picking
    // another item is a key there.
    let keyed = keys.any && has_diamond(desc);
    if let Some(v) = shown_at_t.filter(|v| keyed && v.is_finite() && *v >= 0.0 && (v.round() as usize) < names.len()) {
        index = v.round() as usize;
    }
    #[cfg(any(test, feature = "row-recorder"))]
    crate::widgets::row_recorder::control(ui.next_auto_id(), "choice", desc.key, label, stored.clone());
    let mut chosen = index;
    let stepped = ui
        .horizontal(|ui| {
            ui.label(label);
            // Every item shown at once, as far as the window allows: a long
            // list (a transform's camera curves) isn't cut to a scroll.
            egui::ComboBox::from_id_salt((ctx.addr.clone(), desc.key)).selected_text(names[index]).height(f32::INFINITY).show_ui(ui, |ui| {
                for (i, name) in names.iter().enumerate() {
                    ui.selectable_value(&mut chosen, i, *name);
                }
            });
            stepped_keys(ui, ctx, desc, keys, index as f32)
        })
        .inner;
    if let Some(outcome) = stepped {
        return outcome;
    }
    if chosen == index {
        return RowOutcome::Nothing;
    }
    if keyed {
        add_step_key(ctx, desc, chosen as f32);
        return RowOutcome::Keyed;
    }
    let by_index = ctx.entry.as_ref().is_some_and(|e| e.choices_by_index);
    let new = match (desc.kind, stored) {
        (ParamKind::Choice(_), _) if by_index => Value::from(chosen as u32),
        (ParamKind::Choice(names), Value::String(_)) => Value::from(names[chosen]),
        (_, Value::Bool(_)) => Value::Bool(chosen == 1),
        _ => Value::from(chosen as u32),
    };
    if write_stored(ctx, desc, new) { RowOutcome::Edited } else { RowOutcome::Nothing }
}

/// An authored colour, as its sRGB bytes.
fn colour_row(ui: &mut egui::Ui, ctx: &mut RowCtx, desc: &ParamDesc, pres: &Presentation, stored: &Value) -> RowOutcome {
    let label = pres.label.unwrap_or(desc.label);
    let bytes: Option<Vec<u8>> = stored.as_array().map(|a| a.iter().filter_map(|v| v.as_u64().and_then(|b| u8::try_from(b).ok())).collect());
    let Some(bytes) = bytes.filter(|b| b.len() == 3 || b.len() == 4) else {
        return read_only(ui, desc.key, label, stored, "");
    };
    #[cfg(any(test, feature = "row-recorder"))]
    crate::widgets::row_recorder::control(ui.next_auto_id(), "colour", desc.key, label, stored.clone());
    let alpha = pres.widget == Widget::ColourRgba && bytes.len() == 4;
    let mut changed: Option<Vec<u8>> = None;
    ui.horizontal(|ui| {
        ui.label(label);
        if alpha {
            // The bytes are straight colour and alpha: an alpha edit leaves
            // the colour as it is. Edited as premultiplied, lowering the
            // alpha darkened the stored colour with it.
            let mut c = [bytes[0], bytes[1], bytes[2], bytes[3]];
            if ui.color_edit_button_srgba_unmultiplied(&mut c).changed() {
                changed = Some(c.to_vec());
            }
        } else {
            let mut c = [bytes[0], bytes[1], bytes[2]];
            if ui.color_edit_button_srgb(&mut c).changed() {
                // Any alpha the bytes carry is kept.
                changed = Some(c.iter().copied().chain(bytes.get(3).copied()).collect());
            }
        }
    });
    // A drag in the popup (a channel's number dragged, the colour field
    // pressed and moved) is one gesture, one undo step: a drag over the
    // node's values (an empty key) begun with its first change while the
    // pointer is held, ended once it is let go.
    let drag = ui.id().with(("colour_drag", desc.key));
    let held = ui.input(|i| i.pointer.primary_down());
    let dragging = ui.data(|d| d.get_temp::<bool>(drag)).unwrap_or(false);
    let node_key = klipp_graph_format::ParamKey::from("");
    let mut outcome = RowOutcome::Nothing;
    if let Some(new) = changed {
        if held && !dragging {
            ctx.cmds.push(AppCommand::BeginParamEditAt { addr: ctx.addr.clone(), key: node_key.clone() });
            ui.data_mut(|d| d.insert_temp(drag, true));
        }
        if write_stored(ctx, desc, Value::from(new)) {
            outcome = RowOutcome::Edited;
        }
    }
    if !held && ui.data(|d| d.get_temp::<bool>(drag)).unwrap_or(false) {
        ctx.cmds.push(AppCommand::EndParamEditAt { addr: ctx.addr.clone(), key: node_key });
        ui.data_mut(|d| d.remove::<bool>(drag));
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_effects::registry::effect_types;
    use klipp_graph_format::path::Root;

    fn desc(type_id: &str, key: &str) -> &'static ParamDesc {
        effect_types().find(|t| t.desc.id == type_id).unwrap().desc.params.iter().find(|p| p.key == key).unwrap()
    }

    /// The commands writing `new` for `(type_id, key)` on `payload` push.
    fn written(type_id: &str, key: &str, payload: &Value, new: Value) -> Vec<AppCommand> {
        let addr = NodePath::new(Root::Clip { clip: uuid::Uuid::nil() }, [uuid::Uuid::from_u128(1)]);
        let mut cmds = Vec::new();
        let theme = crate::theme::KlippTheme::default();
        let mut ctx = RowCtx { addr: &addr, clip_time_secs: 0.0, timeline_in_secs: 0.0, rate: 24, encoding: Encoding::Bt1886, entry: Some(Entry { payload, fields_at: &[], choices_by_index: false }), cmds: &mut cmds, behaviours: None, theme: &theme, app: None, custom: None };
        assert!(write_stored(&mut ctx, desc(type_id, key), new));
        cmds
    }

    /// Every choice in the effect and grade catalogs keeps its names apart
    /// as stored names are matched (`plain_name`): two that read alike
    /// would make a stored name pick the wrong one.
    #[test]
    fn every_choice_name_is_its_own_by_letters_and_digits() {
        let params = effect_types().flat_map(|t| t.desc.params.iter().map(move |p| (t.desc.id, p)));
        let grade = klipp_nodes::registry::node_types().flat_map(|t| t.desc.params.iter().map(move |p| (t.desc.id, p)));
        let mut checked = 0;
        for (id, p) in params.chain(grade) {
            let ParamKind::Choice(names) = p.kind else { continue };
            let mut seen = std::collections::BTreeMap::new();
            for name in names {
                if let Some(other) = seen.insert(plain_name(name), name) {
                    panic!("{id} {}: {other:?} and {name:?} read alike", p.key);
                }
            }
            checked += 1;
        }
        assert!(checked > 10, "only {checked} choices");
    }

    /// A choice the effect stores by name is written by its catalog name,
    /// never by the label the row shows ("Hawk" stays "hawk").
    #[test]
    fn choice_labels_never_rename_stored_values() {
        let payload = serde_json::json!({ "type": "chroma_key", "spill_method": "channel_clamp", "tolerance": 0.2 });
        let d = desc("klipp.fx.chroma_key", "spill_method");
        let ParamKind::Choice(names) = d.kind else { panic!() };
        let cmds = written("klipp.fx.chroma_key", "spill_method", &payload, Value::from(names[2]));
        let [AppCommand::SetPayloadAt { payload: p, .. }] = cmds.as_slice() else { panic!("{cmds:?}") };
        assert_eq!(p["spill_method"], "hawk");
        assert_eq!(p["tolerance"], payload["tolerance"], "the other fields are kept");
    }

    /// A colour is written as its bytes in the entry, keeping its alpha and
    /// every other field, a hidden one included.
    #[test]
    fn a_colour_keeps_its_alpha_and_its_siblings() {
        let payload = serde_json::json!({ "type": "color_fade", "color": [0, 0, 0, 200], "duration_secs": 1.0, "mode": "in", "progress": 0.3 });
        let cmds = written("klipp.fx.color_fade", "color", &payload, serde_json::json!([10, 20, 30, 200]));
        let [AppCommand::SetPayloadAt { payload: p, .. }] = cmds.as_slice() else { panic!("{cmds:?}") };
        assert_eq!(p["color"], serde_json::json!([10, 20, 30, 200]));
        assert_eq!(p["progress"], payload["progress"], "the hidden progress is kept");
    }

    /// A row drags at its step's decimal value, exactly as a hand-written
    /// row's literal: 0.01 is 0.01.
    #[test]
    fn a_row_drags_at_its_declared_step_exactly() {
        for (step, want) in [(0.01f32, 0.01f64), (0.005, 0.005), (0.002, 0.002), (0.05, 0.05), (1.0, 1.0), (0.0075, 0.0075)] {
            assert_eq!(drag_speed(step), want);
        }
        assert_ne!(f64::from(0.01f32), 0.01, "why the widening isn't enough");
    }

    /// A number or a switch goes by `SetParamAt`, in its stored kind.
    #[test]
    fn numbers_and_switches_go_by_set_param() {
        let payload = serde_json::json!({});
        let cmds = written("klipp.fx.video_stabilization", "zoom", &payload, Value::Bool(true));
        assert!(matches!(cmds.as_slice(), [AppCommand::SetParamAt { value: ParamValue::Bool(true), .. }]), "{cmds:?}");
        let cmds = written("klipp.fx.radial_blur", "samples", &payload, Value::from(16u32));
        assert!(matches!(cmds.as_slice(), [AppCommand::SetParamAt { value: ParamValue::Int(16), .. }]), "{cmds:?}");
    }
}
