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
use crate::inspector::generated::{effect_value, Entry, KeyState, NodeRows, RowCtx};
use crate::inspector::keyframe_selection;
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::{AppCommand, Keyframe};
use klipp_effects::registry::{effect_types, ParamDesc};
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_format::{Graph, Node, ParamKey};
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
    for item in &view.items {
        match item {
            Item::Header(section) => header(ui, theme, *section),
            Item::Node(row) => {
                row_card(ui, theme, state, clip, graph, row, cmds, clip_time_secs);
                ui.add_space(4.0);
            }
        }
    }
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

/// A node's keys for `key`, as stored on its animation.
fn keys_of(node: &Node, key: &str) -> Vec<Keyframe> {
    node.anim.get(&ParamKey::new(key)).map(|a| a.keys.iter().filter_map(|k| serde_json::from_value(k.clone()).ok()).collect()).unwrap_or_default()
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
        if r.changed() && v != mix {
            cmds.push(AppCommand::SetMixAt { addr: row.addr.clone(), mix: (v < 1.0).then_some(v) });
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
        Some((n, ty)) if !row.read_only && !row.placed => {
            let entry = klipp_graph_lift::effect_params::entry_of(n).unwrap_or(serde_json::Value::Null);
            let presentation = |p: &ParamDesc| ty.presentation(p);
            let stored = |p: &ParamDesc| effect_value(&entry, p);
            let keys = |p: &ParamDesc| {
                let kfs = keys_of(n, p.key);
                if kfs.is_empty() {
                    return (None, KeyState::default());
                }
                let s = keyframe_selection::state(&kfs, clip_time_secs, KEY_TOLERANCE);
                let base = crate::inspector::generated::row::number(&effect_value(&entry, p)).unwrap_or(p.default);
                let shown = Some(keyframe_selection::editable_value(&kfs, clip_time_secs, base, KEY_TOLERANCE));
                (shown, KeyState { any: s.has_any_keyframes, at_playhead: s.has_keyframe_at_playhead, in_frame: s.keys_in_frame, prev: s.prev_keyframe_time, next: s.next_keyframe_time, interpolation: s.current_interpolation })
            };
            let rows = NodeRows { params: ty.desc.params, presentation: &presentation, stored: &stored, keys: &keys };
            let behaviours = |ui: &mut egui::Ui, param: &klipp_core::ParamId, cmds: &mut Vec<AppCommand>| {
                crate::inspector::modifier_ui::show_behaviors_content(ui, theme, clip, std::slice::from_ref(param), cmds);
            };
            let mut rc = RowCtx {
                addr: &row.addr,
                clip_time_secs,
                timeline_in_secs: clip.timeline_in.to_secs_f64(),
                rate: clip.timeline_in.rate,
                encoding: klipp_grade_model::Encoding::from_u8(klipp_core::grade_encoding().effective(klipp_engine::video_decoder::working_colorspace()).as_u8()),
                entry: Some(Entry { payload: &entry, fields_at: &[] }),
                cmds,
                behaviours: Some(&behaviours),
                theme,
                app: Some(state),
            };
            ui.indent(("stack_row", row.addr.clone()), |ui| effect_card(ui, &mut rc, &rows, ty, clip, state));
        }
        Some(_) if row.placed => {
            // A placed kind's card (a Transform) is the clip's placement,
            // edited in the Video tab.
            ui.label(egui::RichText::new("Edited in the Video tab").size(10.0).color(theme.text_dim));
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
