//! The generated Effects tab (M3 step 12): the clip's picture as a stack,
//! read from its graph by the projection (`klipp_graph_lift::stack_view`,
//! M3 step 7), each node drawn by the generated rows (step 5) and its
//! adapter (step 6), every edit a path command.
//!
//! What a row shows: its eye, its name, its mix where the node is
//! mixable, a chip for each mask feeding it (which reveals the mask's
//! producer in the graph), the Placement row's resolved cut, and its
//! parameters. A grade group or any node the stack can't edit opens the
//! graph at it instead.
use crate::widgets::menu::SubmenuRows;

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
    // The node picked in the picture graph: its card outlined, and
    // scrolled into view once each time the pick changes (#1347).
    let selected = state.selected_node_addr.as_ref().filter(|a| a.root_clip() == Some(clip.id.0));
    let revealed_id = egui::Id::new("stack_view_revealed");
    let reveal = selected.filter(|a| ui.data(|d| d.get_temp::<NodePath>(revealed_id)).as_ref() != Some(*a));
    ui.data_mut(|d| match selected {
        Some(a) => d.insert_temp(revealed_id, a.clone()),
        None => d.remove::<NodePath>(revealed_id),
    });
    // A placed recipe's own controls first: what it was made to be set by,
    // above its nodes' raw fields.
    recipe_cards(ui, theme, state, clip, graph, cmds);
    for item in &view.items {
        match item {
            Item::Header(section) => header(ui, theme, *section),
            Item::Node(row) => {
                let card = ui.scope(|ui| {
                    row_card(ui, theme, state, clip, graph, row, cmds, clip_time_secs);
                    if let Some(i) = order.iter().position(|r| r.addr == row.addr) {
                        reorder_buttons(ui, view, &order, i, cmds);
                    }
                    if let Some(p) = &row.placement {
                        placement_controls(ui, clip, view, row, p, cmds);
                    }
                });
                if selected == Some(&row.addr) {
                    ui.painter().rect_stroke(card.response.rect.expand(2.0), 3.0, egui::Stroke::new(1.0_f32, theme.accent));
                    if reveal == Some(&row.addr) {
                        card.response.scroll_to_me(Some(egui::Align::Center));
                    }
                }
                ui.add_space(4.0);
            }
        }
    }
    selected_matte(ui, theme, clip, graph, view, selected, cmds, clip_time_secs);
    add_menu(ui, state, clip, view, cmds);
}

/// One placed recipe's controls as the inspector shows them.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RecipeCard {
    pub name: String,
    /// Each control: its name, label, value, range, and whether the card
    /// can set it (a number, and the first recipe on the clip to use the
    /// name, as the edit finds a control by name).
    pub controls: Vec<(String, String, f64, Option<(f64, f64)>, bool)>,
    /// Controls that aren't numbers, with their values, shown as they are.
    pub others: Vec<(String, String)>,
}

/// The recipes placed in `graph`, each with its named controls, from the
/// recipe's own description where it is known.
pub(crate) fn recipe_cards_of(graph: &Graph, known: &[klipp_timeline::composition_template::CompositionTemplate]) -> Vec<RecipeCard> {
    use klipp_core::TemplateValue;
    let frames: Vec<klipp_nodes::NodeFrame> = graph.ui.frames.iter().filter_map(|f| serde_json::from_value(f.clone()).ok()).collect();
    let mut seen: Vec<String> = Vec::new();
    let mut cards = Vec::new();
    for placed in frames.iter().filter_map(|f| f.recipe.as_ref()) {
        let Some(recipe) = known.iter().find(|r| r.name.eq_ignore_ascii_case(&placed.name)) else {
            cards.push(RecipeCard { name: placed.name.clone(), controls: Vec::new(), others: Vec::new() });
            continue;
        };
        let mut card = RecipeCard { name: placed.name.clone(), controls: Vec::new(), others: Vec::new() };
        for (name, p) in &recipe.parameters {
            let linked = placed.instance.links.iter().any(|l| &l.parameter == name);
            match &p.default {
                TemplateValue::Number(_) if linked => {
                    let range = p.range.as_ref().and_then(|r| Some((r.min?, r.max?)));
                    let first = !seen.iter().any(|s| s.eq_ignore_ascii_case(name));
                    card.controls.push((name.clone(), p.label.clone(), placed.instance.control(name), range, first));
                }
                other => {
                    let shown = match placed.instance.overrides.get(name).unwrap_or(other) {
                        TemplateValue::Number(n) => format!("{n:.2}"),
                        TemplateValue::Bool(b) => if *b { "on".into() } else { "off".into() },
                        TemplateValue::Text(t) | TemplateValue::Enum(t) => t.clone(),
                        TemplateValue::Color(c) => format!("#{:02x}{:02x}{:02x}", (c[0] * 255.0) as u8, (c[1] * 255.0) as u8, (c[2] * 255.0) as u8),
                    };
                    card.others.push((p.label.clone(), shown));
                }
            }
            seen.push(name.clone());
        }
        cards.push(card);
    }
    cards
}

/// The clip's placed recipes, each a card with its named controls: what
/// the recipe was made to be set by, instead of every node's own fields.
fn recipe_cards(ui: &mut egui::Ui, theme: &KlippTheme, state: &AppState, clip: &Clip, graph: &Graph, cmds: &mut Vec<AppCommand>) {
    let mut known: Vec<klipp_timeline::composition_template::CompositionTemplate> = state.composition_templates.recipes().cloned().collect();
    known.extend(klipp_timeline::composition_template::builtin_recipes());
    for (i, card) in recipe_cards_of(graph, &known).into_iter().enumerate() {
        egui::Frame::none().stroke(egui::Stroke::new(1.0_f32, theme.border)).rounding(3.0).inner_margin(6.0).show(ui, |ui| {
            ui.label(egui::RichText::new(format!("Recipe \u{00B7} {}", card.name)).size(12.0).color(theme.text_primary));
            if card.controls.is_empty() && card.others.is_empty() {
                ui.label(egui::RichText::new("Its controls aren't known here: its nodes are set on their own cards.").size(10.0).color(theme.text_dim));
            }
            for (name, label, value, range, settable) in &card.controls {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(label).size(11.0));
                    let draft_id = egui::Id::new(("recipe_control", clip.id, i, name.as_str()));
                    let mut v = ui.data(|d| d.get_temp::<f64>(draft_id)).unwrap_or(*value);
                    let mut field = crate::widgets::number_field(&mut v).speed(range.map_or(0.01, |(lo, hi)| (hi - lo) / 400.0)).max_decimals(3);
                    if let Some((lo, hi)) = range {
                        field = field.range(*lo..=*hi);
                    }
                    let r = ui.add_enabled(*settable, field);
                    let r = if *settable { r } else { r.on_disabled_hover_text("Another recipe on this clip has a control of this name; set this one on its nodes") };
                    if r.dragged() {
                        ui.data_mut(|d| d.insert_temp(draft_id, v));
                    } else {
                        ui.data_mut(|d| d.remove::<f64>(draft_id));
                        if (r.changed() || r.drag_stopped()) && (v - value).abs() > 1e-9 {
                            cmds.push(AppCommand::SetRecipeControl { clip_id: clip.id, control: name.clone(), value: v });
                        }
                    }
                });
            }
            for (label, shown) in &card.others {
                ui.label(egui::RichText::new(format!("{label}: {shown}")).size(11.0).color(theme.text_dim))
                    .on_hover_text("Set where the recipe is placed; change it on its nodes");
            }
        });
        ui.add_space(4.0);
    }
}

/// A matte beside the chain picked in the graph that no effect's card shows
/// under its "Masked by" chip: its own card, with what to do to use it. A
/// Roto Shape or window added from the menu starts unwired, and selecting it
/// showed nothing, so it had no controls anywhere.
#[allow(clippy::too_many_arguments)]
fn selected_matte(ui: &mut egui::Ui, theme: &KlippTheme, clip: &Clip, graph: &Graph, view: &StackView, selected: Option<&NodePath>, cmds: &mut Vec<AppCommand>, clip_time_secs: f64) {
    let Some(addr) = selected.filter(|a| a.nodes.len() == 1) else { return };
    if view.rows().any(|r| r.mask_chips.iter().any(|c| c.source == *addr)) {
        return;
    }
    let read = graph.edges.iter().any(|e| Some(&e.from.0) == addr.nodes.first());
    let fps = f64::from(clip.timeline_in.rate.max(1));
    let frame = crate::inspector::keyframe_selection::FrameSelection { time_secs: clip_time_secs, tolerance: 0.5 / fps, clip_start: clip.timeline_in };
    let source_secs = clip.source_secs_at(clip_time_secs, fps);
    let card = ui.scope(|ui| {
        let name = node_at(graph, addr).map_or(String::new(), |n| n.label.clone());
        ui.label(egui::RichText::new(if name.is_empty() { "Matte".to_owned() } else { name }).strong());
        let drawn = crate::inspector::side_matte::show(ui, theme, clip, graph, addr, frame, source_secs, cmds);
        if drawn && !read {
            ui.add(egui::Label::new(egui::RichText::new(UNWIRED_MATTE).size(10.0).color(theme.text_dim)).wrap());
        }
        drawn
    });
    if card.inner {
        ui.painter().rect_stroke(card.response.rect.expand(2.0), 3.0, egui::Stroke::new(1.0_f32, theme.accent));
        ui.add_space(4.0);
    }
}

/// What a matte nothing reads says.
const UNWIRED_MATTE: &str = "Connect to an effect's mask input to use it.";

/// The rows that can be moved and removed here: the clip's own effects
/// in its top level, not a placed kind, not read-only.
pub fn movable(view: &StackView) -> Vec<&Row> {
    view.rows().filter(|r| r.section == Section::Effects && r.addr.nodes.len() == 1 && !r.read_only && !r.placed && r.placement.is_none() && r.provenance.is_none()).collect()
}

/// The rows the Placement row steps past with its moves: the clip's own
/// top-level effects, the placed kinds' cards and the Placement row
/// itself, as the list holds them all.
pub fn placement_steps(view: &StackView) -> Vec<&Row> {
    view.rows().filter(|r| r.section == Section::Effects && r.addr.nodes.len() == 1 && !r.read_only && r.provenance.is_none()).collect()
}

/// The Placement row's mode and moves (M3 DEC-§2): Auto or Fixed, each
/// one path edit of its mode, and up and down, a move that makes it Fixed
/// where it goes. Auto is refused, and greyed with the reason, for a
/// placement that moves the clip, which automatic placement can't keep
/// yet.
fn placement_controls(ui: &mut egui::Ui, clip: &Clip, view: &StackView, row: &Row, placement: &Placement, cmds: &mut Vec<AppCommand>) {
    use klipp_graph_format::lifted::types as lt;
    if row.read_only || row.provenance.is_some() {
        return;
    }
    let auto = placement.mode != Some(lt::PLACEMENT_FIXED);
    let refusal = clip.placement_auto_refusal();
    let mode = |m: u32| AppCommand::SetParamAt { addr: row.addr.clone(), key: "mode".into(), value: klipp_graph_format::ParamValue::Enum(m) };
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt(("placement_mode", clip.id.0))
            .selected_text(if auto { "Auto" } else { "Fixed" })
            .show_ui(ui, |ui| {
                let r = ui.add_enabled(auto || refusal.is_none(), egui::SelectableLabel::new(auto, "Auto"));
                let r = match refusal.filter(|_| !auto) {
                    Some(why) => r.on_disabled_hover_text(why),
                    None => r.on_hover_text("Placed before the first effect drawn outside the picture"),
                };
                if r.clicked() && !auto {
                    cmds.push(mode(lt::PLACEMENT_AUTO));
                }
                if ui.selectable_label(!auto, "Fixed").on_hover_text("Placed where the row is").clicked() && auto {
                    cmds.push(mode(lt::PLACEMENT_FIXED));
                }
            });
        let steps = placement_steps(view);
        if let Some(i) = steps.iter().position(|r| r.addr == row.addr) {
            if ui.add_enabled(i > 0, egui::Button::new("↑").small()).on_hover_text("Move up (fixes it here)").clicked() {
                if let Some(after) = after_for_up(view, &steps, i) {
                    cmds.push(AppCommand::MoveNodeAt { addr: row.addr.clone(), after });
                }
            }
            if ui.add_enabled(i + 1 < steps.len(), egui::Button::new("↓").small()).on_hover_text("Move down (fixes it here)").clicked() {
                if let Some(after) = steps[i + 1].addr.node() {
                    cmds.push(AppCommand::MoveNodeAt { addr: row.addr.clone(), after });
                }
            }
        }
    });
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
pub(crate) fn effect_node(tag: &str, effect: &klipp_effects::BuiltinEffect) -> Option<Node> {
    let payload = serde_json::to_value(effect).ok()?;
    serde_json::from_value(serde_json::json!({
        "id": uuid::Uuid::new_v4(),
        "type_id": klipp_graph_lift::names::effect_node_type(tag),
        "legacy": { "payload": payload },
    }))
    .ok()
}

/// Whether a Transform card's Scale Y follows its Scale X: under keys on
/// the clip's scale, which key the width with the height at the clip's
/// aspect (#1406), so a static Scale Y there would disagree with the
/// picture. Not when the height has keys of its own (#1471).
pub(crate) fn height_follows_width(tag: &str, clip: &Clip) -> bool {
    tag == "transform" && !clip.transform_scale_keyframes().is_empty() && !height_keyed_apart(tag, clip)
}

/// Whether the clip's height is keyed apart from its width, on its own
/// lane (#1471): the Video tab's Scale Y row keys it, and a Transform
/// card's static Scale Y would disagree with the picture.
pub(crate) fn height_keyed_apart(tag: &str, clip: &Clip) -> bool {
    tag == "transform" && !crate::inspector::placement_by_path::lane_keys(clip, "scale_y").is_empty()
}

/// The four kinds the clip places itself (a transform, an opacity, a
/// speed ramp, a temporal denoise): a card of theirs is added through the
/// clip's effect list, which places it (the placed-kinds ruling).
pub fn is_placed_kind(tag: &str) -> bool {
    matches!(tag, "transform" | "opacity" | "speed_ramp" | "temporal_denoise")
}

/// Whether `addr` names a placed kind's node of `clip`: one whose edits
/// go through the clip's effect list ([`route_placed`]), not by path.
pub fn is_placed_node(clip: &Clip, addr: &NodePath) -> bool {
    let Ok(lifted) = clip.picture_v2() else { return false };
    addr.root_clip() == Some(clip.id.0)
        && addr.nodes.len() == 1
        && lifted.picture.graph.nodes.iter().find(|n| Some(n.id) == addr.node()).and_then(klipp_graph_lift::effect_params::tag).is_some_and(is_placed_kind)
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

/// What adding `effects`, in order and with their own values, to `clip`
/// sends behind the switch (M3 step 19): each a node after the one before,
/// the first after the stack's last effect (as its Add menu adds), a
/// placed kind through the clip's effect list, which places it (until the
/// placement producers move). Several are one undo step where they're
/// sent (`effect_targets::commands`). Or why the clip can't take them,
/// which the caller says: never the old index add instead.
pub fn add_effects_command(state: &AppState, clip: &Clip, effects: &[klipp_effects::BuiltinEffect]) -> Result<Vec<AppCommand>, String> {
    let lifted = clip.picture_v2().map_err(|e| format!("its picture can't be read ({e})"))?;
    let view = project(Root::Clip { clip: clip.id.0 }, &lifted.picture.graph, &klipp_timeline::RegistryCatalog, placement(state, clip).as_ref()).map_err(|e| format!("its stack can't be read ({e})"))?;
    let mut after = add_after(&view).ok_or("its stack has no place to add after")?;
    let mut adds = Vec::new();
    let mut placed = Vec::new();
    for effect in effects {
        let tag = serde_json::to_value(effect).ok().and_then(|v| v.get("type")?.as_str().map(str::to_owned)).ok_or("an effect without a type")?;
        if is_placed_kind(&tag) {
            placed.push(AppCommand::AddBuiltinEffect { clip_id: clip.id, effect: effect.clone() });
            continue;
        }
        let node = effect_node(&tag, effect).ok_or_else(|| format!("a {tag} can't be made a node"))?;
        let next = NodePath::new(Root::Clip { clip: clip.id.0 }, [node.id]);
        adds.push(AppCommand::AddNodeAt { after: std::mem::replace(&mut after, next), node: Box::new(node) });
    }
    adds.extend(placed);
    Ok(adds)
}

fn add_menu(ui: &mut egui::Ui, state: &AppState, clip: &Clip, view: &StackView, cmds: &mut Vec<AppCommand>) {
    let Some(after) = add_after(view) else { return };
    ui.add_space(6.0);
    ui.menu_rows("+ Add effect", |ui| {
        let mut types: Vec<_> = effect_types().collect();
        types.sort_by_key(|t| t.desc.name);
        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
            for t in types {
                if ui.button(t.desc.name).clicked() {
                    cmds.extend(add_to_selection(state, clip, &after, t));
                    ui.close_menu();
                }
            }
        });
    });
}

/// What the add menu's `ty` sends: on `clip` alone, or, with it one of
/// several selected clips, on each of them that can take a picture
/// effect, as a drop on the selection does: one undo step, or refused
/// whole with the reason in a toast.
fn add_to_selection(state: &AppState, clip: &Clip, after: &NodePath, ty: &klipp_effects::registry::EffectType) -> Vec<AppCommand> {
    let many = state.selected_clips.len() > 1 && state.selected_clips.contains(&clip.id);
    if !many {
        return add_command(clip, after, ty.tag).into_iter().collect();
    }
    let targets = crate::effect_targets::targets(state.sequence(), state.selected_clip, &state.selected_clips, Some(clip.id), crate::effect_targets::EffectKind::Picture);
    let effect = (ty.sample)();
    let adds = crate::effect_targets::all_or_none(&targets, |id| {
        let c = state.sequence().all_clips().find(|c| c.id == id).ok_or("the clip isn't in the sequence")?;
        add_effects_command(state, c, std::slice::from_ref(&effect))
    });
    match adds {
        Ok(mut adds) => {
            if targets.skipped > 0 {
                adds.push(AppCommand::ShowToast { text: crate::effect_targets::said(ty.desc.name, &targets) });
            }
            adds
        }
        Err(why) => vec![AppCommand::ShowToast { text: format!("\"{}\" wasn't added: {why}", ty.desc.name) }],
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

/// `cmds` as they reach `clip`: a path command naming a placed kind's node
/// (a Temporal Denoise's track in the curve editor, its key deleted from
/// the timeline) goes through the clip's effect list, as the kind's card
/// sends its own (`to_effect_list`), since the path handler refuses a
/// placed node while the clip's picture is stored as fields (#1419).
/// Every other command passes as it is.
pub fn route_placed(clip: &Clip, cmds: Vec<AppCommand>) -> Vec<AppCommand> {
    let Ok(lifted) = clip.picture_v2() else { return cmds };
    let graph = &lifted.picture.graph;
    let mut out = Vec::new();
    for cmd in cmds {
        let addr = match &cmd {
            AppCommand::AddKeyAt { addr, .. }
            | AppCommand::RemoveKeyAt { addr, .. }
            | AppCommand::MoveKeyAt { addr, .. }
            | AppCommand::SetInterpKeyAt { addr, .. }
            | AppCommand::BeginParamEditAt { addr, .. }
            | AppCommand::EndParamEditAt { addr, .. }
            | AppCommand::SetParamAt { addr, .. } => Some(addr),
            _ => None,
        };
        let placed = addr
            .filter(|a| a.root_clip() == Some(clip.id.0) && a.nodes.len() == 1)
            .and_then(|a| graph.nodes.iter().find(|n| Some(n.id) == a.node()))
            .and_then(|n| klipp_graph_lift::effect_params::tag(n).filter(|tag| is_placed_kind(tag)).and_then(|tag| effect_types().find(|t| t.tag == tag)).map(|ty| (n, ty)));
        match (placed, &cmd) {
            // A key dragged on its track (the curve editor's Begin / End
            // around its moves) is a keyframe drag: one step whose undo
            // puts the keys back. The card's value drag, an effect drag,
            // snapshots the effect's values and not its keys.
            (Some((n, _)), AppCommand::BeginParamEditAt { key, .. }) => out.push(AppCommand::BeginKeyframeDrag { key: format!("{:?}:{}:{}", clip.id, n.id, key.as_str()), clip_id: clip.id }),
            (Some((n, _)), AppCommand::EndParamEditAt { key, .. }) => out.push(AppCommand::EndKeyframeDrag { key: format!("{:?}:{}:{}", clip.id, n.id, key.as_str()), clip_id: clip.id }),
            (Some((n, ty)), _) => out.extend(to_effect_list(clip, ty, n.id, &klipp_graph_lift::effect_params::entry_of(n).unwrap_or(serde_json::Value::Null), vec![cmd])),
            (None, _) => out.push(cmd),
        }
    }
    out
}

/// A placed kind's row commands as the clip's effect-list commands, which
/// place it: a drag's bracket as the effect drag, a value as the effect
/// with it changed, a key as the effect's key, its eye as the effect's
/// switch. What has no list form (a seek) passes through.
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
            // Its eye: the effect switched as the old card's eye switches it.
            AppCommand::SetNodeEnabledAt { enabled, .. } => {
                if clip.builtin_effects_enabled().get(index).copied().unwrap_or(true) != enabled {
                    out.push(AppCommand::ToggleBuiltinEffectEnabled { clip_id, index });
                }
            }
            // A key dragged (the curve editor's), found by its time as an
            // interpolation is.
            AppCommand::MoveKeyAt { key: pk, time_secs, new_time_secs, value, .. } => {
                let lanes = clip.effect_keyframes();
                if let Some(keys) = lanes.get(&format!("{index}:{}", pk.as_str())) {
                    if let Some(keyframe_index) = crate::inspector::keyframe_selection::nearest(keys, time_secs, KEY_TOLERANCE) {
                        out.push(AppCommand::MoveEffectKeyframe { clip_id, effect_index: index, param_name: pk.as_str().to_owned(), keyframe_index, new_time_secs, new_value: value });
                    }
                }
            }
            AppCommand::SetInterpKeyAt { key: pk, time_secs, interpolation, .. } => {
                let lanes = clip.effect_keyframes();
                if let Some(keys) = lanes.get(&format!("{index}:{}", pk.as_str())) {
                    if let Some(keyframe_index) = crate::inspector::keyframe_selection::nearest(keys, time_secs, KEY_TOLERANCE) {
                        out.push(AppCommand::SetEffectKeyframeInterpolation { clip_id, effect_index: index, param_name: pk.as_str().to_owned(), keyframe_index, interpolation });
                    }
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// A node's Mix row: how much of its change is kept, dragged as one step,
/// typed, and reset to full from its label (IC15); full is stored as
/// none. An effect's card and a grade node's page draw it alike (#1348).
pub(crate) fn mix_row(ui: &mut egui::Ui, label: &str, addr: &NodePath, stored: Option<f32>, read_only: bool, cmds: &mut Vec<AppCommand>) {
    let mix = stored.unwrap_or(1.0);
    let mut v = mix;
    let r = ui
        .add_enabled_ui(!read_only, |ui| crate::widgets::scrubby_param::scrubby_param_row(ui, label, &mut v, 0.0..=1.0, 1.0, 0.005, 2, "", None))
        .inner;
    let key = klipp_graph_format::ParamKey::from(AppState::NODE_MIX_DRAG_KEY);
    if r.drag_started {
        cmds.push(AppCommand::BeginParamEditAt {
            addr: addr.clone(),
            key: key.clone(),
        });
    }
    // egui reports drag_stopped on Escape before the widget is drawn.
    if r.drag_stopped && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        cmds.push(AppCommand::AbandonParamEditAt {
            addr: addr.clone(),
            key,
        });
        // Escape also releases egui's capture so held-pointer movement
        // cannot begin writing a second edit before the button is released.
        ui.ctx().stop_dragging();
    } else {
        if r.changed && v != mix {
            cmds.push(AppCommand::SetMixAt {
                addr: addr.clone(),
                mix: (v < 1.0).then_some(v),
            });
        }
        if r.drag_stopped {
            cmds.push(AppCommand::EndParamEditAt {
                addr: addr.clone(),
                key,
            });
        }
    }
}

/// The channel a masked node reads its mask by, one of those its mask
/// port takes, or the port's default (#1348). Nothing for a node whose
/// mask takes no channel.
pub(crate) fn mask_channel_row(ui: &mut egui::Ui, node: &Node, addr: &NodePath, read_only: bool, cmds: &mut Vec<AppCommand>) {
    use klipp_graph_format::PortCatalog;
    let Some(choices) = klipp_timeline::RegistryCatalog.node_spec(node).and_then(|s| s.inputs.into_iter().find(|p| p.key == "mask")).and_then(|p| p.params.into_iter().find(|(k, _)| k == "channel")).map(|(_, v)| v) else { return };
    if choices.is_empty() {
        return;
    }
    let current = match node.params.get(&klipp_graph_format::ParamKey::from("mask.channel")) {
        Some(klipp_graph_format::ParamValue::Text(c)) => Some(c.clone()),
        _ => None,
    };
    let name = |c: Option<&str>| match c {
        None => "Default".to_owned(),
        Some(c) => {
            let mut chars = c.chars();
            chars.next().map(|f| f.to_uppercase().chain(chars).collect()).unwrap_or_default()
        }
    };
    let mut chosen = current.clone();
    ui.add_enabled_ui(!read_only, |ui| {
        ui.horizontal(|ui| {
            ui.label("Mask channel");
            egui::ComboBox::from_id_salt(("mask_channel", addr.clone())).selected_text(name(current.as_deref())).show_ui(ui, |ui| {
                ui.selectable_value(&mut chosen, None, name(None));
                for c in &choices {
                    ui.selectable_value(&mut chosen, Some(c.clone()), name(Some(c)));
                }
            });
        });
    });
    if chosen != current {
        cmds.push(AppCommand::SetMaskChannelAt { addr: addr.clone(), channel: chosen });
    }
}

/// What a node is called in the stack: its label, else its effect's
/// name, else a grade group's, else its type.
pub fn display_name(graph: &Graph, addr: &NodePath, label: &str, is_group: bool) -> String {
    if !label.is_empty() {
        return label.to_owned();
    }
    let node = node_at(graph, addr);
    // A frei0r plug-in by the plug-in it runs (Brightness), the last part
    // of its entry's `frei0r:{path}:{name}`, not the kind all share.
    let plugin = node.filter(|n| klipp_graph_lift::effect_params::tag(n) == Some("frei0r"))
        .and_then(|n| klipp_graph_lift::effect_params::entry_of(n).ok())
        .and_then(|e| e.get("type_id")?.as_str().and_then(|id| id.rsplit(':').next()).filter(|name| !name.is_empty()).map(str::to_owned));
    if let Some(name) = plugin {
        return name;
    }
    if let Some(ty) = node.and_then(klipp_graph_lift::effect_params::tag).and_then(|tag| effect_types().find(|t| t.tag == tag)) {
        return ty.desc.name.to_owned();
    }
    if is_group {
        return String::from("Grade");
    }
    node.map_or_else(String::new, |n| n.type_id.as_str().rsplit('.').next().unwrap_or_default().replace('_', " "))
}

/// What a Chroma Key's card says when its key colour is so near grey that
/// white and greys key out with it: the key measures colour alone (its
/// CbCr), and a neutral pixel sits at its centre, `tolerance + soft_edge`
/// from the key or nearer. A dark navy logo's white letters went with its
/// blue at the default 0.2. `None` for a key colour clear of grey.
pub(crate) fn greys_keyed_note(key_color: [u8; 3], tolerance: f32, soft_edge: f32) -> Option<&'static str> {
    let [r, g, b] = key_color.map(f32::from);
    let cb = -0.168736 * r - 0.331264 * g + 0.5 * b;
    let cr = 0.5 * r - 0.418688 * g - 0.081312 * b;
    let from_grey = (cb * cb + cr * cr).sqrt() / 255.0;
    (from_grey < tolerance + soft_edge).then_some("This key colour is close to grey: white, black and greys key out with it. Lower the Tolerance, or key a more saturated colour.")
}

/// What the Placement row says: the renderer's cut, or pending.
pub fn placement_text(p: &Placement) -> String {
    match &p.resolved {
        Some(r) => r.reason.clone(),
        None => String::from("Placement pending"),
    }
}

#[allow(clippy::too_many_arguments)]
/// A surface pin's surface: chosen among the clip's tracked ones, or why
/// there is nothing to choose. The node added from the graph starts with
/// none, and has no other way to be given one.
fn surface_choice(ui: &mut egui::Ui, theme: &KlippTheme, clip: &Clip, addr: &NodePath, entry: &serde_json::Value, cmds: &mut Vec<AppCommand>) {
    let chosen = entry.get("surface").and_then(serde_json::Value::as_str).unwrap_or_default();
    let tracked: Vec<_> = clip.surfaces.iter().filter(|s| !s.keys.is_empty()).collect();
    let current = tracked.iter().find(|s| s.id.to_string() == chosen || s.name == chosen);
    let note = |ui: &mut egui::Ui, text: &str| {
        ui.add(egui::Label::new(egui::RichText::new(text).size(10.0).color(theme.text_dim)).wrap());
    };
    if tracked.is_empty() {
        note(ui, "This clip has no tracked surface. Track one under Surface in the Video tab, or pin this clip to another clip's with Effects > Pin to Surface in its menu.");
    } else {
        egui::ComboBox::from_id_salt(("surface_pin_choice", addr.clone()))
            .selected_text(current.map_or("Choose a surface", |s| s.name.as_str()))
            .show_ui(ui, |ui| {
                for s in &tracked {
                    if ui.selectable_label(current.is_some_and(|c| c.id == s.id), &s.name).clicked() {
                        let mut payload = entry.clone();
                        payload["surface"] = serde_json::Value::String(s.id.to_string());
                        cmds.push(AppCommand::SetPayloadAt { addr: addr.clone(), payload });
                    }
                }
            });
    }
    if chosen.is_empty() {
        note(ui, "No surface chosen: the picture stays as it is.");
    } else if current.is_none() {
        note(ui, "The surface it names is not on this clip, so the pin draws nothing.");
    }
}

fn row_card(ui: &mut egui::Ui, theme: &KlippTheme, state: &AppState, clip: &Clip, graph: &Graph, row: &Row, cmds: &mut Vec<AppCommand>, clip_time_secs: f64) {
    let node = node_at(graph, &row.addr);
    ui.horizontal(|ui| {
        let mut on = row.enabled;
        if ui.add_enabled(!row.read_only, egui::Checkbox::without_text(&mut on)).on_hover_text(if on { "Turn off" } else { "Turn on" }).changed() {
            let toggle = AppCommand::SetNodeEnabledAt { addr: row.addr.clone(), enabled: on };
            // A placed kind's eye goes through the clip's effect list, as
            // the rest of its card does (`to_effect_list`).
            let placed = row.placed || node.is_some_and(|n| n.type_id.as_str() == klipp_graph_format::lifted::types::PLACEMENT);
            let effect = node.and_then(|n| klipp_graph_lift::effect_params::tag(n).and_then(|tag| effect_types().find(|t| t.tag == tag)).map(|ty| (n, ty)));
            match effect.filter(|_| placed) {
                Some((n, ty)) => cmds.extend(to_effect_list(clip, ty, n.id, &klipp_graph_lift::effect_params::entry_of(n).unwrap_or(serde_json::Value::Null), vec![toggle])),
                None => cmds.push(toggle),
            }
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
        mix_row(ui, "Mix", &row.addr, node.and_then(|n| n.mix), row.read_only, cmds);
    }
    for chip in &row.mask_chips {
        if ui.small_button(format!("Masked by: {}", chip.label)).on_hover_text("Show the mask in the graph").clicked() {
            cmds.push(AppCommand::RevealGraph { path: chip.source.clone(), select: true });
        }
        // A matte beside the chain (#1746): its settings here, under the
        // effect it masks.
        if !row.read_only {
            let frame = crate::inspector::keyframe_selection::FrameSelection {
                time_secs: clip_time_secs,
                tolerance: crate::inspector::keyframe_selection::frame_tolerance(state.sequence()),
                clip_start: clip.timeline_in,
            };
            let fps = f64::from(state.sequence().fps.num) / f64::from(state.sequence().fps.den.max(1));
            let source_secs = clip.source_secs_at(clip_time_secs, fps);
            crate::inspector::side_matte::show(ui, theme, clip, graph, &chip.source, frame, source_secs, cmds);
        }
    }
    if let Some(n) = node.filter(|_| !row.mask_chips.is_empty()) {
        mask_channel_row(ui, n, &row.addr, row.read_only, cmds);
    }
    for branch in &row.sub_stacks {
        // By its own name where it has one (a merge's "Gradient").
        let named = node_at(graph, &branch.source).map_or("", |n| n.label.as_str());
        let from = display_name(graph, &branch.source, named, false);
        if ui.small_button(format!("{} ← {from}", branch.input.as_str())).on_hover_text("Show the branch in the graph").clicked() {
            cmds.push(AppCommand::RevealGraph { path: branch.source.clone(), select: true });
        }
        // An effect beside the chain feeding it (a merge's generator,
        // #1746): its card here, under what it feeds.
        let beside = node_at(graph, &branch.source).filter(|n| branch.source.nodes.len() == 1 && klipp_graph_lift::effect_params::tag(n).is_some());
        if let Some(n) = beside.filter(|_| !row.read_only) {
            let side = Row {
                addr: branch.source.clone(),
                type_id: n.type_id.clone(),
                label: n.label.clone(),
                section: row.section,
                enabled: n.enabled,
                placed: false,
                mixable: false,
                mask_chips: Vec::new(),
                sub_stacks: Vec::new(),
                is_group: false,
                provenance: None,
                read_only: false,
                placement: None,
            };
            ui.indent(("side_effect", branch.source.clone()), |ui| row_card(ui, theme, state, clip, graph, &side, cmds, clip_time_secs));
        }
    }
    let effect = node.and_then(|n| klipp_graph_lift::effect_params::tag(n).map(|tag| (n, tag))).and_then(|(n, tag)| effect_types().find(|t| t.tag == tag).map(|t| (n, t)));
    match effect {
        Some((n, ty)) if !row.read_only => {
            if ty.tag == "chroma_key" {
                if let Ok(klipp_effects::BuiltinEffect::ChromaKey { key_color, tolerance, soft_edge, .. }) = serde_json::from_value(klipp_graph_lift::effect_params::entry_of(n).unwrap_or(serde_json::Value::Null)) {
                    if let Some(note) = greys_keyed_note(key_color, tolerance, soft_edge) {
                        ui.add(egui::Label::new(egui::RichText::new(note).size(10.0).color(theme.marker_warning)).wrap());
                    }
                }
            }
            if ty.tag == "surface_pin" {
                surface_choice(ui, theme, clip, &row.addr, &klipp_graph_lift::effect_params::entry_of(n).unwrap_or(serde_json::Value::Null), cmds);
            }
            let entry = klipp_graph_lift::effect_params::entry_of(n).unwrap_or(serde_json::Value::Null);
            // The catalog retains legacy animation names (Chroma Key's
            // "spill"), while the picture stores their canonical keys
            // ("soft_edge"). Bind every row operation to that picture key,
            // but look up its presentation by the original descriptor.
            let params: Vec<_> = ty.desc.params.iter().map(|p| {
                let key = klipp_graph_lift::names::KEYFRAME_ALIASES.iter()
                    .find(|(tag, from, _)| *tag == ty.tag && *from == p.key)
                    .map_or(p.key, |(_, _, to)| *to);
                ParamDesc { key, ..*p }
            })
            // Under keys on the clip's scale, its height follows the width
            // at the clip's aspect (#1406), or its own keys (#1471): no
            // static Scale Y to edit.
            .filter(|p| !((height_follows_width(ty.tag, clip) || height_keyed_apart(ty.tag, clip)) && p.key == "scale_y"))
            .collect();
            let presentation = |p: &ParamDesc| {
                let original = klipp_graph_lift::names::keyframe_name(ty.tag, p.key);
                ty.presentation(ty.desc.params.iter().find(|p| p.key == original).unwrap_or(p))
            };
            let stored = |p: &ParamDesc| effect_value(&entry, p);
            let keys = |p: &ParamDesc| {
                let base = crate::inspector::generated::row::number(&effect_value(&entry, p)).unwrap_or(p.default);
                // Non-anchor Opacity keeps its effect keys in the legacy
                // lane; the lift intentionally does not animate this node.
                if ty.tag == "opacity" {
                    if let Some((false, index)) = clip.effect_slot_of_node(n.id) {
                        let lanes = clip.effect_keyframes();
                        let keys = lanes.get(&format!("{index}:{}", p.key)).map_or(&[][..], Vec::as_slice);
                        return crate::inspector::generated::keys_from_lane(keys, base, clip_time_secs, KEY_TOLERANCE);
                    }
                }
                crate::inspector::generated::keys_at(n, p, base, clip_time_secs, KEY_TOLERANCE)
            };
            let rows = NodeRows { params: &params, presentation: &presentation, stored: &stored, keys: &keys };
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
            ui.indent(("stack_row", row.addr.clone()), |ui| {
                effect_card(ui, &mut rc, &rows, ty, clip, state);
                if height_follows_width(ty.tag, clip) {
                    ui.label(egui::RichText::new("Scale Y follows Scale X's keys, at the clip's aspect").small().color(theme.text_dim))
                        .on_hover_text("The clip's scale is keyed: its height keeps its aspect against the width.");
                } else if height_keyed_apart(ty.tag, clip) {
                    ui.label(egui::RichText::new("Scale Y has keys of its own").small().color(theme.text_dim))
                        .on_hover_text("The clip's height is keyed apart from its width, on the Video tab's Scale Y row.");
                }
            });
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

#[cfg(test)]
mod surface_choice_tests {

    /// A key colour near grey says that greys key out with it (the walk's
    /// navy 13/59/102 at the default 0.2 took the white letters); a green
    /// screen's, and the navy at 0.05, say nothing.
    #[test]
    fn a_key_colour_near_grey_says_greys_key_out() {
        assert!(greys_keyed_note([13, 59, 102], 0.2, 0.1).is_some());
        assert!(greys_keyed_note([13, 59, 102], 0.05, 0.1).is_none());
        assert!(greys_keyed_note([0, 255, 0], 0.2, 0.1).is_none());
        assert!(greys_keyed_note([0, 71, 187], 0.2, 0.1).is_none(), "a blue screen");
    }

    /// A frei0r plug-in's card is named after the plug-in it runs
    /// (Brightness), not the kind every plug-in shares ("frei0r").
    #[test]
    fn a_frei0r_card_is_named_after_its_plugin() {
        let t = |s| klipp_core::RationalTime::from_secs_f64(s, 24);
        let fx = klipp_effects::BuiltinEffect::Frei0r { type_id: "frei0r:/usr/lib/frei0r-1/brightness.so:Brightness".into(), params: Vec::new() };
        let clip = klipp_timeline::test_clip::TestClip::of(klipp_timeline::Clip::media(klipp_core::MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0))).effect(fx).build();
        let graph = clip.picture_v2().unwrap().picture.graph.clone();
        let node = graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("frei0r")).expect("the plug-in's node").id;
        let addr = NodePath::new(Root::Clip { clip: clip.id.0 }, [node]);
        let label = graph.nodes.iter().find(|n| n.id == node).unwrap().label.clone();
        assert_eq!(super::display_name(&graph, &addr, &label, false), "Brightness", "its label {label:?}");
    }

    use super::*;

    fn frame(ctx: &egui::Context, clip: &Clip, addr: &NodePath, entry: &serde_json::Value, clock: f64, events: Vec<egui::Event>) -> (Vec<AppCommand>, Vec<(String, egui::Pos2)>) {
        let mut cmds = Vec::new();
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(420.0, 600.0))), time: Some(clock), events, ..Default::default() };
        let out = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| surface_choice(ui, &KlippTheme::default(), clip, addr, entry, &mut cmds));
        });
        (cmds, out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Text(t) => Some((t.galley.text().to_owned(), t.pos + t.galley.size() / 2.0)), _ => None }).collect())
    }

    /// With none chosen it says the picture stays; with no surface on the
    /// clip it says so; with one, a click chooses it by its id.
    #[test]
    fn a_surface_pin_chooses_among_the_clips_surfaces_or_says_there_are_none() {
        let t = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
        let mut clip = klipp_timeline::Clip::media(klipp_core::MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
        let addr = NodePath::new(klipp_graph_format::path::Root::Clip { clip: clip.id.0 }, [uuid::Uuid::new_v4()]);
        let entry = serde_json::json!({ "surface": "", "inset": 0.0 });

        let ctx = egui::Context::default();
        let (_, texts) = frame(&ctx, &clip, &addr, &entry, 0.0, Vec::new());
        assert!(texts.iter().any(|(t, _)| t.starts_with("This clip has no tracked surface")), "{texts:?}");
        assert!(texts.iter().any(|(t, _)| t.starts_with("No surface chosen")), "{texts:?}");

        let screen = [[0.2, 0.15], [0.8, 0.2], [0.75, 0.85], [0.25, 0.8]];
        let key = klipp_timeline::SurfaceKey::at_corners(1.0, &screen, &screen).expect("a quad");
        let id = uuid::Uuid::new_v4();
        clip.surfaces.push(klipp_timeline::Surface { id, name: "Phone".into(), corners: screen, reference_secs: 1.0, keys: vec![key], source_size: [1920, 1080], media_fps: 24.0, track_area: None, exclude: Vec::new() });
        let ctx = egui::Context::default();
        let mut clock = 0.0;
        let mut step = |events| { clock += 0.05; frame(&ctx, &clip, &addr, &entry, clock, events) };
        step(Vec::new());
        let (_, texts) = step(Vec::new());
        let click = |at: egui::Pos2, pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        let open = texts.iter().find(|(t, _)| t == "Choose a surface").expect("the choice").1;
        step(vec![egui::Event::PointerMoved(open), click(open, true)]);
        step(vec![click(open, false)]);
        let (_, texts) = step(Vec::new());
        let item = texts.iter().find(|(t, _)| t == "Phone").expect("the surface offered").1;
        step(vec![egui::Event::PointerMoved(item)]);
        step(vec![click(item, true)]);
        let (cmds, _) = step(vec![click(item, false)]);
        let chose = cmds.iter().find_map(|c| match c { AppCommand::SetPayloadAt { payload, .. } => payload["surface"].as_str().map(str::to_owned), _ => None });
        assert_eq!(chose, Some(id.to_string()), "{cmds:?}");
    }
}

#[cfg(test)]
mod selected_matte_tests {
    use super::*;

    fn texts(state: &AppState, clip: &Clip) -> Vec<String> {
        let ctx = egui::Context::default();
        let mut texts = Vec::new();
        for _ in 0..2 {
            let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(420.0, 3000.0))), ..Default::default() }, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| show(ui, &KlippTheme::default(), state, clip, &mut Vec::new(), 0.5));
            });
            texts = out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Text(t) => Some(t.galley.text().to_owned()), _ => None }).collect();
        }
        texts
    }

    /// A Qualifier from the graph's Mattes menu starts off, so it keys
    /// nothing until set, and its card says how to set it.
    #[test]
    fn a_picture_qualifier_starts_off_and_says_how_to_set_it() {
        let t = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
        let mut clip = Clip::media(klipp_core::MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
        let graph = clip.picture_v2().unwrap().picture.graph.clone();
        let level = NodePath::new(Root::Clip { clip: clip.id.0 }, []);
        let key = crate::picture_mattes::matte("matte.qualifier").expect("the qualifier");
        assert!((key.params)().qualifier.is_some_and(|q| !q.enabled), "off until set");
        let id = uuid::Uuid::new_v4();
        clip.apply_picture_ops(&crate::picture_mattes::add_ops(&graph, &level, key, id, (100.0, 100.0), None).unwrap()).unwrap();
        let state = AppState { selected_node_addr: Some(NodePath::new(Root::Clip { clip: clip.id.0 }, [id])), ..Default::default() };
        assert!(texts(&state, &clip).iter().any(|t| t == crate::inspector::side_matte::QUALIFIER_UNSET), "{:?}", texts(&state, &clip));
    }

    /// A Roto Shape added from the graph and picked there has its card, with
    /// its shapes, and says it needs a mask input to do anything; picked
    /// nothing, it has none.
    #[test]
    fn a_picked_roto_shape_shows_its_controls_and_what_it_needs() {
        let t = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
        let mut clip = Clip::media(klipp_core::MediaId::new(), t(0.0), t(4.0), t(0.0), t(4.0));
        let graph = clip.picture_v2().unwrap().picture.graph.clone();
        let level = NodePath::new(Root::Clip { clip: clip.id.0 }, []);
        let roto = crate::picture_mattes::matte("matte.roto").expect("the roto shape");
        let id = uuid::Uuid::new_v4();
        let ops = crate::picture_mattes::add_ops(&graph, &level, roto, id, (100.0, 100.0), None).expect("its ops");
        clip.apply_picture_ops(&ops).expect("added");
        let mut state = AppState::default();
        let before = texts(&state, &clip);
        assert!(!before.iter().any(|t| t == UNWIRED_MATTE), "{before:?}");
        state.selected_node_addr = Some(NodePath::new(Root::Clip { clip: clip.id.0 }, [id]));
        let after = texts(&state, &clip);
        assert!(after.iter().any(|t| t == UNWIRED_MATTE), "{after:?}");
        assert!(after.len() > before.len() + 3, "its controls are drawn: {after:?}");
    }

}

#[cfg(test)]
mod recipe_card_tests {
    use super::recipe_cards_of;

    /// A frame holding a placed recipe, its controls linked as placing it
    /// links them.
    fn placed(name: &str, controls: &[(&str, f64)]) -> serde_json::Value {
        let links: Vec<serde_json::Value> = controls.iter().enumerate().map(|(i, (c, v))| serde_json::json!({
            "parameter": c, "property": format!("node:{i}.amount"), "from": [*v, 1.0], "to": [0.0, 1.0],
        })).collect();
        serde_json::json!({
            "id": uuid::Uuid::new_v4(), "label": name, "color_rgba": 0, "contained_node_ids": [],
            "recipe": { "name": name, "instance": { "template_id": uuid::Uuid::nil(), "links": links }, "nodes": {} },
        })
    }

    /// A placed recipe shows its named controls with their values, as its
    /// file labels them; a second recipe with a control of the same name
    /// shows it but can't set it, as the edit finds a control by name.
    #[test]
    fn a_placed_recipe_shows_its_named_controls() {
        let known = klipp_timeline::composition_template::builtin_recipes();
        let mut graph = klipp_graph_format::Graph::empty();
        graph.ui.frames.push(placed("Glitch", &[("amount", 0.4)]));
        graph.ui.frames.push(placed("Glitch", &[("amount", 0.7)]));
        let cards = recipe_cards_of(&graph, &known);
        assert_eq!(cards.len(), 2, "{cards:?}");
        let (name, label, value, _, settable) = &cards[0].controls.iter().find(|c| c.0 == "amount").expect("Amount").clone();
        assert_eq!((name.as_str(), label.as_str(), *value, *settable), ("amount", "Amount", 0.4, true));
        let second = cards[1].controls.iter().find(|c| c.0 == "amount").expect("Amount");
        assert_eq!((second.2, second.4), (0.7, false));
        // A recipe the app doesn't know is still named.
        let mut other = klipp_graph_format::Graph::empty();
        other.ui.frames.push(placed("Someone else's", &[]));
        assert_eq!(recipe_cards_of(&other, &known)[0].name, "Someone else's");
    }
}

