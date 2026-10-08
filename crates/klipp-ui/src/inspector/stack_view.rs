//! The generated Effects tab (M3 step 12): the clip's picture as a stack,
//! read from its graph by the projection (`klipp_graph_lift::stack_view`,
//! M3 step 7), each node drawn by the generated rows (step 5) and its
//! adapter (step 6), every edit a path command. Drawn instead of the
//! hand-written effects tab while `AppState::use_generated_stack` is on.
//!
//! What a row shows: its eye, its name, its mix where the node is
//! mixable, a chip for each mask feeding it (which reveals the mask's
//! producer in the graph), the Placement row's resolved cut, and its
//! parameters. A grade group or any node the stack can't edit opens the
//! graph at it instead.

use crate::inspector::generated::adapters::effect_card;
use crate::inspector::generated::{effect_value, Entry, NodeRows, RowCtx};
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::{AppCommand, Keyframe};
use klipp_effects::registry::{effect_types, ParamDesc};
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_format::{Graph, Node};
use klipp_graph_lift::stack_view::{project, Item, Placement, Row, Section, StackView};
use klipp_timeline::Clip;

/// How close to the playhead a key counts as at it, in seconds.
const KEY_TOLERANCE: f64 = 0.5 / 24.0;

/// Draw the clip's stack and push the path commands its edits imply.
pub fn show(ui: &mut egui::Ui, theme: &KlippTheme, state: &AppState, clip: &Clip, cmds: &mut Vec<AppCommand>, clip_time_secs: f64) {
    let lifted = match clip.picture_v2() {
        Ok(l) => l,
        Err(e) => {
            ui.colored_label(theme.marker_error, format!("The clip's picture can't be read: {e}"));
            return;
        }
    };
    let resolved = placement(state, clip);
    let view = match project(Root::Clip { clip: clip.id.0 }, &lifted.picture.graph, &klipp_timeline::RegistryCatalog, resolved.as_ref()) {
        Ok(v) => v,
        Err(e) => {
            ui.colored_label(theme.marker_error, format!("The clip's stack can't be drawn: {e:?}"));
            return;
        }
    };
    draw_stack(ui, theme, state, clip, &lifted.picture.graph, &view, cmds, clip_time_secs);
    // A clip's plug-in effects keep their own cards under the stack (M4
    // owns them).
    if !clip.ofx_effects().is_empty() {
        ui.add_space(6.0);
        crate::inspector::effects_tab::show_ofx_cards(ui, theme, state, clip, cmds);
    }
}

/// Where the clip's transform is drawn, as the renderer resolves it at
/// the playhead; `None` while it can't be told (shown as pending).
fn placement(state: &AppState, clip: &Clip) -> Option<klipp_graph_lift::stack_view::ResolvedPlacement> {
    let seq = state.sequence();
    let fps = f64::from(seq.fps.num) / f64::from(seq.fps.den.max(1));
    klipp_compose::resolve::placement_in(seq, clip.id, state.playhead, fps, None).ok().flatten()
}

#[allow(clippy::too_many_arguments)]
fn draw_stack(ui: &mut egui::Ui, theme: &KlippTheme, state: &AppState, clip: &Clip, graph: &Graph, view: &StackView, cmds: &mut Vec<AppCommand>, clip_time_secs: f64) {
    let order = movable(view);
    for item in &view.items {
        match item {
            Item::Header(section) => header(ui, theme, *section),
            Item::Node(row) => {
                row_card(ui, theme, state, clip, graph, row, cmds, clip_time_secs);
                if let Some(i) = order.iter().position(|r| r.addr == row.addr) {
                    reorder_buttons(ui, view, &order, i, cmds);
                }
                ui.add_space(4.0);
            }
        }
    }
    add_menu(ui, clip, view, cmds);
}

/// The rows that can be moved and removed here: the clip's own effects
/// in its top level, not a placed kind, not read-only.
pub fn movable(view: &StackView) -> Vec<&Row> {
    view.rows().filter(|r| r.section == Section::Effects && r.addr.nodes.len() == 1 && !r.read_only && !r.placed && r.placement.is_none() && r.provenance.is_none()).collect()
}

/// The node a row's move up goes after: the row before the one above it,
/// or what precedes the effects (the last row before them) for the top.
fn after_for_up(view: &StackView, order: &[&Row], i: usize) -> Option<klipp_graph_format::NodeId> {
    if i >= 2 {
        return order[i - 2].addr.node();
    }
    let first = order.first()?;
    let rows: Vec<&Row> = view.rows().collect();
    let at = rows.iter().position(|r| r.addr == first.addr)?;
    at.checked_sub(1).and_then(|p| rows[p].addr.node())
}

fn reorder_buttons(ui: &mut egui::Ui, view: &StackView, order: &[&Row], i: usize, cmds: &mut Vec<AppCommand>) {
    let row = order[i];
    ui.horizontal(|ui| {
        if ui.add_enabled(i > 0, egui::Button::new("↑").small()).on_hover_text("Move up").clicked() {
            if let Some(after) = after_for_up(view, order, i) {
                cmds.push(AppCommand::MoveNodeAt { addr: row.addr.clone(), after });
            }
        }
        if ui.add_enabled(i + 1 < order.len(), egui::Button::new("↓").small()).on_hover_text("Move down").clicked() {
            if let Some(after) = order[i + 1].addr.node() {
                cmds.push(AppCommand::MoveNodeAt { addr: row.addr.clone(), after });
            }
        }
        // A removal is planned by the app (M3 step 9): a refusal says why,
        // and a mask's readers it ungates are named.
        if ui.small_button("✕").on_hover_text("Remove").clicked() {
            cmds.push(AppCommand::RemoveNodeAt { addr: row.addr.clone() });
        }
    });
}

/// Where an added effect goes: after the clip's last effect, or after the
/// last row before the effects when it has none.
pub fn add_after(view: &StackView) -> Option<NodePath> {
    if let Some(last) = movable(view).last() {
        return Some(last.addr.clone());
    }
    view.rows().filter(|r| r.section != Section::Effects && r.addr.nodes.len() == 1).last().map(|r| r.addr.clone())
}

/// A new effect node for the saved tag `tag`, at its reset values: what
/// an add puts in the clip's picture.
pub fn new_effect_node(tag: &str) -> Option<Node> {
    let ty = effect_types().find(|t| t.tag == tag)?;
    effect_node(tag, &(ty.sample)())
}

/// A new effect node for `effect`, with its values, saved as `tag`.
fn effect_node(tag: &str, effect: &klipp_effects::BuiltinEffect) -> Option<Node> {
    let payload = serde_json::to_value(effect).ok()?;
    serde_json::from_value(serde_json::json!({
        "id": uuid::Uuid::new_v4(),
        "type_id": klipp_graph_lift::to_v2::effect_node_type(tag),
        "legacy": { "payload": payload },
    }))
    .ok()
}

/// The four kinds the clip places itself (a transform, an opacity, a
/// speed ramp, a temporal denoise): a card of theirs is added through the
/// clip's effect list, which places it (the placed-kinds ruling).
pub fn is_placed_kind(tag: &str) -> bool {
    matches!(tag, "transform" | "opacity" | "speed_ramp" | "temporal_denoise")
}

/// What adding the effect saved as `tag` sends: a placed kind through the
/// clip's effect list, any other as a node after `after`.
pub fn add_command(clip: &Clip, after: &NodePath, tag: &str) -> Option<AppCommand> {
    if is_placed_kind(tag) {
        let ty = effect_types().find(|t| t.tag == tag)?;
        return Some(AppCommand::AddBuiltinEffect { clip_id: clip.id, effect: (ty.sample)() });
    }
    new_effect_node(tag).map(|node| AppCommand::AddNodeAt { after: after.clone(), node: Box::new(node) })
}

/// What adding `effect`, with its own values, to `clip` sends behind the
/// switch: a node after the stack's last effect (as its Add menu adds), or
/// a placed kind through the clip's effect list. `None` when the clip's
/// stack can't be read, so the caller adds it to the list as before.
pub fn add_effect_command(state: &AppState, clip: &Clip, effect: &klipp_effects::BuiltinEffect) -> Option<AppCommand> {
    let tag = serde_json::to_value(effect).ok()?.get("type")?.as_str()?.to_owned();
    if is_placed_kind(&tag) {
        return Some(AppCommand::AddBuiltinEffect { clip_id: clip.id, effect: effect.clone() });
    }
    let lifted = clip.picture_v2().ok()?;
    let view = project(Root::Clip { clip: clip.id.0 }, &lifted.picture.graph, &klipp_timeline::RegistryCatalog, placement(state, clip).as_ref()).ok()?;
    let after = add_after(&view)?;
    let node = effect_node(&tag, effect)?;
    Some(AppCommand::AddNodeAt { after, node: Box::new(node) })
}

fn add_menu(ui: &mut egui::Ui, clip: &Clip, view: &StackView, cmds: &mut Vec<AppCommand>) {
    let Some(after) = add_after(view) else { return };
    ui.add_space(6.0);
    ui.menu_button("+ Add effect", |ui| {
        let mut types: Vec<_> = effect_types().collect();
        types.sort_by_key(|t| t.desc.name);
        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
            for t in types {
                if ui.button(t.desc.name).clicked() {
                    cmds.extend(add_command(clip, &after, t.tag));
                    ui.close_menu();
                }
            }
        });
    });
}

fn header(ui: &mut egui::Ui, theme: &KlippTheme, section: Section) {
    let name = match section {
        Section::Source => "SOURCE",
        Section::Colour => "COLOUR",
        Section::Effects => "EFFECTS",
    };
    ui.add_space(6.0);
    ui.label(egui::RichText::new(name).size(10.0).strong().color(theme.text_dim));
}

/// The node a row names, inside groups too.
pub fn node_at<'a>(graph: &'a Graph, addr: &NodePath) -> Option<&'a Node> {
    let (last, through) = addr.nodes.split_last()?;
    let mut level = graph;
    for id in through {
        level = &level.nodes.iter().find(|n| n.id == *id)?.group.as_ref()?.graph;
    }
    level.nodes.iter().find(|n| n.id == *last)
}

/// A placed kind's row commands as the clip's effect-list commands, which
/// place it: a drag's bracket as the effect drag, a value as the effect
/// with it changed, a key as the effect's key. What has no list form (a
/// seek) passes through.
pub fn to_effect_list(clip: &Clip, ty: &klipp_effects::registry::EffectType, node: klipp_graph_format::NodeId, entry: &serde_json::Value, cmds: Vec<AppCommand>) -> Vec<AppCommand> {
    let Some((false, index)) = clip.effect_slot_of_node(node) else { return Vec::new() };
    let clip_id = clip.id;
    let key = format!("{clip_id:?}:builtin:{index}");
    let read = |e: &serde_json::Value| serde_json::from_value::<klipp_effects::BuiltinEffect>(e.clone()).ok();
    let Some(mut current) = read(entry) else { return Vec::new() };
    let mut current_json = entry.clone();
    let field = |pk: &str| ty.desc.params.iter().find(|p| p.key == pk);
    let mut out = Vec::new();
    for cmd in cmds {
        match cmd {
            AppCommand::BeginParamEditAt { .. } => out.push(AppCommand::BeginEffectDrag { key: key.clone(), clip_id, effect_index: index, old_effect: current.clone() }),
            AppCommand::EndParamEditAt { .. } => out.push(AppCommand::EndEffectDrag { key: key.clone(), clip_id, effect_index: index, new_effect: current.clone() }),
            AppCommand::AbandonParamEditAt { .. } => out.push(AppCommand::AbandonEffectDrag { clip_id, effect_index: index, added: false }),
            AppCommand::SetParamAt { key: pk, value, .. } => {
                let Some(p) = field(pk.as_str()) else { continue };
                let v = match value {
                    klipp_graph_format::ParamValue::Float(f) => serde_json::json!(f),
                    klipp_graph_format::ParamValue::Int(i) => serde_json::json!(i),
                    klipp_graph_format::ParamValue::Bool(b) => serde_json::json!(b),
                    klipp_graph_format::ParamValue::Enum(e) => serde_json::json!(e),
                    _ => continue,
                };
                let slot = match (current_json.get_mut(p.field), p.index) {
                    (Some(f), Some(i)) => f.get_mut(i),
                    (f, None) => f,
                    _ => None,
                };
                let Some(slot) = slot else { continue };
                *slot = v;
                let Some(e) = read(&current_json) else { continue };
                current = e;
                out.push(AppCommand::UpdateBuiltinEffect { clip_id, index, effect: current.clone() });
            }
            AppCommand::SetPayloadAt { payload, .. } => {
                let Some(e) = read(&payload) else { continue };
                current_json = payload;
                current = e;
                out.push(AppCommand::UpdateBuiltinEffect { clip_id, index, effect: current.clone() });
            }
            AppCommand::AddKeyAt { key: pk, keyframe, .. } => {
                if let Ok(k) = serde_json::from_value::<Keyframe>(keyframe) {
                    out.push(AppCommand::AddEffectKeyframe { clip_id, effect_index: index, param_name: pk.as_str().to_owned(), time_secs: k.time_secs, value: k.value });
                }
            }
            AppCommand::RemoveKeyAt { key: pk, time_secs, .. } => out.push(AppCommand::RemoveEffectKeyframe { clip_id, effect_index: index, param_name: pk.as_str().to_owned(), time_secs }),
            other => out.push(other),
        }
    }
    out
}

/// What a node is called in the stack: its label, else its effect's
/// name, else a grade group's, else its type.
pub fn display_name(graph: &Graph, addr: &NodePath, label: &str, is_group: bool) -> String {
    if !label.is_empty() {
        return label.to_owned();
    }
    let node = node_at(graph, addr);
    if let Some(ty) = node.and_then(klipp_graph_lift::effect_params::tag).and_then(|tag| effect_types().find(|t| t.tag == tag)) {
        return ty.desc.name.to_owned();
    }
    if is_group {
        return String::from("Grade");
    }
    node.map_or_else(String::new, |n| n.type_id.as_str().rsplit('.').next().unwrap_or_default().replace('_', " "))
}

/// What the Placement row says: the renderer's cut, or pending.
pub fn placement_text(p: &Placement) -> String {
    match &p.resolved {
        Some(r) => r.reason.clone(),
        None => String::from("Placement pending"),
    }
}

#[allow(clippy::too_many_arguments)]
fn row_card(ui: &mut egui::Ui, theme: &KlippTheme, state: &AppState, clip: &Clip, graph: &Graph, row: &Row, cmds: &mut Vec<AppCommand>, clip_time_secs: f64) {
    let node = node_at(graph, &row.addr);
    ui.horizontal(|ui| {
        let mut on = row.enabled;
        if ui.add_enabled(!row.read_only, egui::Checkbox::without_text(&mut on)).on_hover_text(if on { "Turn off" } else { "Turn on" }).changed() {
            cmds.push(AppCommand::SetNodeEnabledAt { addr: row.addr.clone(), enabled: on });
        }
        let name = egui::RichText::new(display_name(graph, &row.addr, &row.label, row.is_group)).strong();
        ui.label(if row.enabled { name } else { name.color(theme.text_dim) });
        if row.provenance.is_some() {
            ui.label(egui::RichText::new("track").size(10.0).color(theme.text_dim)).on_hover_text("From the clip's track");
        }
    });
    if let Some(p) = &row.placement {
        ui.label(egui::RichText::new(placement_text(p)).size(10.0).color(theme.text_dim));
    }
    if row.mixable {
        let mix = node.and_then(|n| n.mix).unwrap_or(1.0);
        let mut v = mix;
        let r = ui.add_enabled(!row.read_only, egui::Slider::new(&mut v, 0.0..=1.0).text("Mix"));
        let key = klipp_graph_format::ParamKey::from(AppState::NODE_MIX_DRAG_KEY);
        if r.drag_started() {
            cmds.push(AppCommand::BeginParamEditAt {
                addr: row.addr.clone(),
                key: key.clone(),
            });
        }
        // egui reports drag_stopped on Escape before the widget is drawn.
        if (r.dragged() || r.drag_stopped()) && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            cmds.push(AppCommand::AbandonParamEditAt {
                addr: row.addr.clone(),
                key,
            });
            // Escape also releases egui's capture so held-pointer movement
            // cannot begin writing a second edit before the button is released.
            ui.ctx().stop_dragging();
        } else {
            if r.changed() && v != mix {
                cmds.push(AppCommand::SetMixAt {
                    addr: row.addr.clone(),
                    mix: (v < 1.0).then_some(v),
                });
            }
            if r.drag_stopped() {
                cmds.push(AppCommand::EndParamEditAt {
                    addr: row.addr.clone(),
                    key,
                });
            }
        }
    }
    for chip in &row.mask_chips {
        if ui.small_button(format!("Masked by: {}", chip.label)).on_hover_text("Show the mask in the graph").clicked() {
            cmds.push(AppCommand::RevealGraph { path: chip.source.clone(), select: true });
        }
    }
    for branch in &row.sub_stacks {
        let from = display_name(graph, &branch.source, "", false);
        if ui.small_button(format!("{} ← {from}", branch.input.as_str())).on_hover_text("Show the branch in the graph").clicked() {
            cmds.push(AppCommand::RevealGraph { path: branch.source.clone(), select: true });
        }
    }
    let effect = node.and_then(|n| klipp_graph_lift::effect_params::tag(n).map(|tag| (n, tag))).and_then(|(n, tag)| effect_types().find(|t| t.tag == tag).map(|t| (n, t)));
    match effect {
        Some((n, ty)) if !row.read_only => {
            let entry = klipp_graph_lift::effect_params::entry_of(n).unwrap_or(serde_json::Value::Null);
            let presentation = |p: &ParamDesc| ty.presentation(p);
            let stored = |p: &ParamDesc| effect_value(&entry, p);
            let keys = |p: &ParamDesc| {
                let base = crate::inspector::generated::row::number(&effect_value(&entry, p)).unwrap_or(p.default);
                crate::inspector::generated::keys_at(n, p, base, clip_time_secs, KEY_TOLERANCE)
            };
            let rows = NodeRows { params: ty.desc.params, presentation: &presentation, stored: &stored, keys: &keys };
            let behaviours = |ui: &mut egui::Ui, param: &klipp_core::ParamId, cmds: &mut Vec<AppCommand>| {
                crate::inspector::modifier_ui::show_behaviors_content(ui, theme, clip, std::slice::from_ref(param), cmds);
            };
            // A placed kind's edits go through the clip's effect list, which
            // places it: drawn into a scratch list, then translated.
            // The clip's placement node, where a fixed Transform is drawn,
            // is placed as well: it holds the Transform's entry.
            let placed = row.placed || n.type_id.as_str() == klipp_graph_format::lifted::types::PLACEMENT;
            let mut scratch = Vec::new();
            let out: &mut Vec<AppCommand> = if placed { &mut scratch } else { cmds };
            let mut rc = RowCtx {
                addr: &row.addr,
                clip_time_secs,
                timeline_in_secs: clip.timeline_in.to_secs_f64(),
                rate: clip.timeline_in.rate,
                encoding: klipp_grade_model::Encoding::from_u8(klipp_core::grade_encoding().effective(klipp_engine::video_decoder::working_colorspace()).as_u8()),
                entry: Some(Entry { payload: &entry, fields_at: &[], choices_by_index: false }),
                cmds: out,
                behaviours: Some(&behaviours),
                theme,
                app: Some(state),
                custom: None,
            };
            ui.indent(("stack_row", row.addr.clone()), |ui| effect_card(ui, &mut rc, &rows, ty, clip, state));
            if placed {
                cmds.extend(to_effect_list(clip, ty, n.id, &entry, scratch));
            }
        }
        _ if row.is_group || row.section == Section::Colour => {
            let open = ui.small_button("Open in graph").clicked();
            if open {
                cmds.push(AppCommand::RevealGraph { path: row.addr.clone(), select: true });
            }
        }
        _ => {}
    }
}
