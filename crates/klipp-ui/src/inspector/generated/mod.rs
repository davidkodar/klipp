//! The generated parameter rows (M3 step 5): one function draws any
//! node's parameters from the catalog's description of them
//! ([`ParamDesc`]) and their presentation ([`Presentation`], M3 step 0),
//! and writes them by path, never by a place in a list. The effect stack,
//! the grade inspector and the canvas inspector all draw through it.

pub mod adapters;
pub mod custom;
pub mod presets;
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
/// ones, so a keyed switch doesn't make rows come and go). An unset value
/// reads as its default, as the node does.
pub fn holds(rows: &NodeRows, cond: &Cond) -> bool {
    let Some(p) = rows.params.iter().find(|p| p.key == cond.key()) else { return false };
    let stored = (rows.stored)(p);
    if stored.is_null() {
        return cond.holds_for(p.default);
    }
    row::number(&stored).or_else(|| row::choice_index(p, &stored).map(|i| i as f32)).is_some_and(|v| cond.holds_for(v))
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
        // Each row in its own scope, which counts as one widget however
        // much it draws: a row that grows or shrinks mid-drag leaves the
        // rows after it their ids and a held drag.
        let addr = ctx.addr.clone();
        let outcome = ui.push_id((addr, desc.key), |ui| ui.add_enabled_ui(enabled, |ui| param_row(ui, ctx, RowInput { desc, pres: &pres, stored: &stored, shown_at_t, keys })).inner).inner;
        if let Some(help) = pres.help {
            ui.label(egui::RichText::new(help).size(10.0).color(ctx.theme.text_dim));
        }
        outcomes.push((desc.key, outcome));
    }
    outcomes
}

/// A built-in effect's stored value for `desc`: its field in the saved
/// effect, or the element of it a keyed array names. A field the effect
/// leaves out at its default (a Crop's cover shape and focus, #2757) is
/// that default, as it reads back, and its row is drawn; a field stored
/// as null stays null.
pub fn effect_value(effect_json: &Value, desc: &ParamDesc) -> Value {
    let Some(v) = effect_json.get(desc.field) else { return default_value(desc) };
    match desc.index {
        Some(i) => v.get(i).cloned().unwrap_or(Value::Null),
        None => v.clone(),
    }
}

/// A grade node's stored value for `desc`: the field of its v1 entry's
/// parameters a dotted `field` names (`grain.amount` is the grain's
/// amount), or the element of it a keyed array names. An unset number,
/// switch or choice is its default, as the node reads it; anything else
/// unset is `Null`.
pub fn grade_value(entry: &Value, desc: &ParamDesc) -> Value {
    let mut at = entry.get("params");
    for step in desc.field.split('.') {
        at = at.and_then(|v| v.get(step));
    }
    let v = at.cloned().unwrap_or(Value::Null);
    let v = match desc.index {
        Some(i) => v.get(i).cloned().unwrap_or(Value::Null),
        None => v,
    };
    if v.is_null() { default_value(desc) } else { v }
}

/// `desc`'s default as a stored value: an unset number, switch or choice
/// is it; anything else unset is `Null`.
fn default_value(desc: &ParamDesc) -> Value {
    use klipp_effects::registry::ParamKind;
    if !desc.default.is_finite() {
        return Value::Null;
    }
    match desc.kind {
        ParamKind::Float => serde_json::json!(desc.default),
        ParamKind::Whole | ParamKind::Choice(_) => serde_json::json!(desc.default as i64),
        ParamKind::Bool => Value::Bool(desc.default != 0.0),
        _ => Value::Null,
    }
}

/// A node's keys for `desc` around `t` (the clip's seconds): the value
/// they give there, with `base` (its stored value) where they don't, and
/// where they are. `(None, default)` when it has none.
pub fn keys_at(node: &klipp_graph_format::Node, desc: &ParamDesc, base: f32, t: f64, tolerance: f64) -> (Option<f32>, KeyState) {
    let kfs = node_keys(node, desc.key);
    keys_from_lane(&kfs, base, t, tolerance)
}

/// The same row state for a placed effect's retained legacy lane.
pub(super) fn keys_from_lane(kfs: &[klipp_core::Keyframe], base: f32, t: f64, tolerance: f64) -> (Option<f32>, KeyState) {
    use crate::inspector::keyframe_selection;
    if kfs.is_empty() {
        return (None, KeyState::default());
    }
    let s = keyframe_selection::state(kfs, t, tolerance);
    let shown = Some(keyframe_selection::editable_value(kfs, t, base, tolerance));
    (shown, KeyState { any: s.has_any_keyframes, at_playhead: s.has_keyframe_at_playhead, in_frame: s.keys_in_frame, prev: s.prev_keyframe_time, next: s.next_keyframe_time, interpolation: s.current_interpolation })
}

/// A node's keys for `key`: its animation's, or for a grade node its v1
/// keyframe track on its record (a grade node, or a merge lifted from
/// one), where a grade node keeps its keys.
pub fn node_keys(node: &klipp_graph_format::Node, key: &str) -> Vec<klipp_core::Keyframe> {
    if matches!(node.type_id.as_str(), klipp_graph_format::types::LEGACY_NODE | klipp_graph_format::lifted::types::MERGE) {
        let tracks: klipp_nodes::NodeKeyframeTracks = node.legacy.as_ref().and_then(|l| l.keyframes.clone()).and_then(|k| serde_json::from_value(k).ok()).unwrap_or_default();
        return tracks.tracks.into_iter().find(|t| t.param_name == key).map(|t| t.keyframes).unwrap_or_default();
    }
    node.anim
        .get(&klipp_graph_format::ParamKey::new(key))
        .map(|a| a.keys.iter().filter_map(|k| serde_json::from_value(k.clone()).ok()).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod effect_value_tests {
    use super::*;

    /// A Crop saved at its default cover shape and focus leaves those
    /// fields out; read back, each is its default, so its row is drawn
    /// (#2757). A field stored as null stays null.
    #[test]
    fn a_field_left_out_at_its_default_reads_as_the_default() {
        let crop = klipp_effects::registry::effect_type("crop").expect("the Crop");
        let saved = serde_json::to_value((crop.sample)()).unwrap();
        let param = |key: &str| crop.desc.params.iter().find(|p| p.key == key).unwrap();
        for (key, want) in [("aspect", 0.0), ("focus_x", 0.5), ("focus_y", 0.5)] {
            assert!(saved.get(param(key).field).is_none(), "{key} is left out at its default: {saved}");
            assert_eq!(effect_value(&saved, param(key)), serde_json::json!(want), "{key}");
        }
        assert_eq!(effect_value(&saved, param("left")), saved["left"], "a field saved is read as saved");
        let mut nulled = saved.clone();
        nulled["aspect"] = Value::Null;
        assert_eq!(effect_value(&nulled, param("aspect")), Value::Null, "a null stays null");
    }
}
