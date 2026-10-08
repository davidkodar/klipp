//! The generated parameter rows (M3 step 5): one function draws any
//! node's parameters from the catalog's description of them
//! ([`ParamDesc`]) and their presentation ([`Presentation`], M3 step 0),
//! and writes them by path, never by a place in a list. The effect stack,
//! the grade inspector and the canvas inspector all draw through it.
//!
//! Nothing calls it while `AppState::use_generated_stack` is off.

pub mod adapters;
pub mod custom;
pub mod row;

pub use row::{param_row, Entry, KeyState, RowCtx, RowInput, RowOutcome};

use klipp_effects::registry::presentation::{Cond, Presentation, Widget};
use klipp_effects::registry::ParamDesc;
use serde_json::Value;

/// A node's parameters, as the rows read them.
pub struct NodeRows<'a> {
    pub params: &'a [ParamDesc],
    /// Each parameter's presentation.
    pub presentation: &'a dyn Fn(&ParamDesc) -> Presentation,
    /// Each parameter's stored value (`Null` when the node has none).
    pub stored: &'a dyn Fn(&ParamDesc) -> Value,
    /// Each parameter's animated value at the playhead and its keys.
    pub keys: &'a dyn Fn(&ParamDesc) -> (Option<f32>, KeyState),
}

/// Whether `cond` holds on the node's stored values (never its animated
/// ones, so a keyed switch doesn't make rows come and go).
pub fn holds(rows: &NodeRows, cond: &Cond) -> bool {
    match cond {
        Cond::ParamIs { key, value } => {
            let Some(p) = rows.params.iter().find(|p| p.key == *key) else { return false };
            let stored = (rows.stored)(p);
            row::number(&stored).or_else(|| row::choice_index(p, &stored).map(|i| i as f32)) == Some(*value)
        }
    }
}

/// The parameters shown, in the order they're drawn: by presentation
/// order, then the catalog's; hidden ones and ones whose condition doesn't
/// hold left out. A hidden value is kept as it is.
pub fn shown<'a>(rows: &NodeRows<'a>) -> Vec<(&'a ParamDesc, Presentation)> {
    let mut out: Vec<(usize, &ParamDesc, Presentation)> = rows
        .params
        .iter()
        .enumerate()
        .map(|(i, p)| (i, p, (rows.presentation)(p)))
        .filter(|(_, _, pres)| pres.widget != Widget::Hidden && pres.visible_if.as_ref().is_none_or(|c| holds(rows, c)))
        .collect();
    out.sort_by_key(|(i, _, pres)| (pres.order, *i));
    out.into_iter().map(|(_, p, pres)| (p, pres)).collect()
}

/// Draw a node's rows under their sub-headers, each with its help under
/// it, and push the path commands they imply; each drawn row's key and
/// what it did. A named editor drawing several parameters (a wheel's
/// parts, a group sharing one editor) is drawn once, at its first.
pub fn node_rows(ui: &mut egui::Ui, ctx: &mut RowCtx, rows: &NodeRows) -> Vec<(&'static str, RowOutcome)> {
    let mut outcomes = Vec::new();
    let mut header: Option<&str> = None;
    let mut drawn_editors: Vec<(Widget, Option<&str>)> = Vec::new();
    for (desc, pres) in shown(rows) {
        if pres.group != header {
            header = pres.group;
            if let Some(g) = header {
                ui.add_space(2.0);
                ui.label(egui::RichText::new(g).size(10.0).strong());
            }
        }
        let shared = matches!(pres.widget, Widget::Custom(_) | Widget::WheelPart(_)) && pres.group.is_some();
        if shared {
            if drawn_editors.contains(&(pres.widget, pres.group)) {
                continue;
            }
            drawn_editors.push((pres.widget, pres.group));
        }
        let enabled = pres.enabled_if.as_ref().is_none_or(|c| holds(rows, c));
        let stored = (rows.stored)(desc);
        let (shown_at_t, keys) = (rows.keys)(desc);
        let outcome = ui.add_enabled_ui(enabled, |ui| param_row(ui, ctx, RowInput { desc, pres: &pres, stored: &stored, shown_at_t, keys })).inner;
        if let Some(help) = pres.help {
            ui.label(egui::RichText::new(help).size(10.0).color(ctx.theme.text_dim));
        }
        outcomes.push((desc.key, outcome));
    }
    outcomes
}

/// A built-in effect's stored value for `desc`: its field in the saved
/// effect, or the element of it a keyed array names.
pub fn effect_value(effect_json: &Value, desc: &ParamDesc) -> Value {
    let v = effect_json.get(desc.field).cloned().unwrap_or(Value::Null);
    match desc.index {
        Some(i) => v.get(i).cloned().unwrap_or(Value::Null),
        None => v,
    }
}
