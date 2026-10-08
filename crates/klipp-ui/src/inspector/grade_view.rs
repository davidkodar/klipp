//! The generated grade inspector (M3 step 11): a grade node's own layout,
//! drawn by the generated rows from what its catalog type describes
//! (`klipp_nodes::registry`) and how that is shown (its presentation),
//! writing by the node's path. It stands in for the per-node layout
//! (`panels::node_inspector`) while `AppState::use_generated_stack` is on.
//!
//! It reads the clip's picture and never writes it: a clip's grade node is
//! at `[grade group, node]` under the clip, its stored values are its v1
//! entry's parameters, and its keys are its animation. A parameter the
//! catalog doesn't describe isn't drawn, so a control that changes nothing
//! isn't there (#1295). A row a specialist editor draws is read-only until
//! that editor is registered.

use crate::inspector::generated::{grade_value, keys_at, node_rows, row, Entry, NodeRows, RowCtx};
use crate::inspector::keyframe_selection::FrameSelection;
use crate::panels::node_inspector::NodeInspectorPanel;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;
use klipp_effects::registry::ParamDesc;
use klipp_graph_format::lifted::types as lt;
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_format::Node;
use klipp_nodes::NodeId;
use klipp_timeline::{Clip, GradeAccess};

/// Where a clip's grade node is in its picture: its path, and the node.
pub fn grade_node(clip: &Clip, node: NodeId) -> Option<(NodePath, Node)> {
    let picture = &clip.picture_v2().ok()?.picture;
    let group = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP)?;
    let inner = group.group.as_ref()?.graph.nodes.iter().find(|n| n.id == node)?;
    Some((NodePath::new(Root::Clip { clip: clip.id.0 }, [group.id, node]), inner.clone()))
}

/// A grade node's stored entry (its kind, its v1 params): what its rows
/// read their values from.
fn entry_of(node: &Node) -> serde_json::Value {
    node.legacy.as_ref().map(|l| l.payload.clone()).unwrap_or(serde_json::Value::Null)
}

/// Draw the clip's grade node `node` and push the commands its edits
/// imply: `false`, drawing nothing, when the node isn't in the clip's own
/// grade or its kind has no catalog type.
///
/// The specialist editors (the wheels, the curves, the warper, the masks,
/// a LUT's file, and the nodes whose own widget draws all their
/// parameters) are the per-node layout's, drawn through `panel`, which
/// keeps their state; without it those rows are read-only.
#[allow(clippy::too_many_arguments)]
pub(crate) fn show(ui: &mut egui::Ui, theme: &KlippTheme, clip: &Clip, node: NodeId, cmds: &mut Vec<AppCommand>, frame: FrameSelection, mut panel: Option<&mut NodeInspectorPanel>) -> bool {
    let Some(v1) = clip.grade().nodes.iter().find(|n| n.id == node) else { return false };
    let Some(ty) = klipp_nodes::registry::node_type(v1.kind) else { return false };
    let Some((addr, lifted)) = grade_node(clip, node) else { return false };
    let start = cmds.len();
    crate::panels::node_inspector::node_header(ui, &v1.kind, v1.enabled, &v1.keyframes, node, clip.id, cmds, theme, frame);
    // The widgets edit a copy and push the grade's own commands.
    let mut params = v1.params.clone();
    if let Some(p) = panel.as_deref_mut() {
        if ui.indent(("grade_node", node), |ui| p.grade_whole_node(ui, v1.kind, &mut params, node, clip.id, cmds, theme)).inner {
            return true;
        }
    }
    if v1.kind == klipp_nodes::NodeKind::Qualifier {
        let pick = ui.add(egui::Button::image(crate::icons::Icon::Eyedropper.image(12.0).tint(theme.overlay(160))).min_size(egui::vec2(22.0, 18.0)))
            .on_hover_text("Pick a colour: click it in the viewer to key it");
        if pick.clicked() {
            cmds.push(AppCommand::PickQualifierColour { clip_id: clip.id, node_id: node });
        }
    }
    let entry = entry_of(&lifted);
    let presentation = |p: &ParamDesc| ty.presentation(p);
    let stored = |p: &ParamDesc| grade_value(&entry, p);
    let keys = |p: &ParamDesc| {
        let base = row::number(&grade_value(&entry, p)).unwrap_or(p.default);
        keys_at(&lifted, p, base, frame.time_secs, frame.tolerance)
    };
    let rows = NodeRows { params: ty.desc.params, presentation: &presentation, stored: &stored, keys: &keys };
    // Each editor once: a wheel's parts are one block.
    let mut done: Vec<String> = Vec::new();
    let mut hook = |ui: &mut egui::Ui, id: &str, _: &ParamDesc, cmds: &mut Vec<AppCommand>| -> bool {
        let Some(p) = panel.as_deref_mut() else { return false };
        if done.iter().any(|d| d == id) {
            return true;
        }
        // The node's masks a tracker drives: Re-track and Untrack for them.
        p.tracked_masks = clip.track_points.iter().filter_map(|tp| tp.linked_mask.filter(|(n, _)| *n == node).map(|(_, i)| i)).collect();
        let drawn = p.grade_editor(ui, id, ty, &mut params, node, clip.id, cmds, theme, frame);
        p.tracked_masks.clear();
        if drawn {
            done.push(id.to_owned());
        }
        drawn
    };
    let mut ctx = RowCtx {
        addr: &addr,
        clip_time_secs: frame.time_secs,
        timeline_in_secs: clip.timeline_in.to_secs_f64(),
        rate: clip.timeline_in.rate,
        encoding: klipp_grade_model::Encoding::from_u8(klipp_core::grade_encoding().effective(klipp_engine::video_decoder::working_colorspace()).as_u8()),
        entry: Some(Entry { payload: &entry, fields_at: &["params"], choices_by_index: true }),
        cmds: &mut *cmds,
        // A grade parameter's Behaviours aren't kept yet.
        behaviours: None,
        theme,
        app: None,
        custom: Some(&mut hook),
    };
    let drawn = ui.indent(("grade_node", node), |ui| node_rows(ui, &mut ctx, &rows)).inner;
    if drawn.is_empty() {
        ui.label(egui::RichText::new("No parameters").color(theme.text_dim));
    }
    if v1.kind == klipp_nodes::NodeKind::Corrector {
        crate::panels::node_inspector::cdl_buttons(ui, &v1.params, node, clip.id);
    }
    // A key the rows name at the playhead is the stored key in its frame.
    for cmd in &mut cmds[start..] {
        crate::inspector::keyframe_selection::resolve_for_clip(clip, frame.tolerance, cmd);
    }
    true
}

/// The clip's own grade nodes as their generated rows read them, taken
/// before the tabbed page draws (it draws inside the grade's edit, with
/// no clip to read): what lets that page draw a parameter it shares with
/// the per-node layout as the same generated row (M3 step 11c).
pub(crate) struct GradeSnapshot {
    clip: klipp_core::ClipId,
    group: klipp_graph_format::NodeId,
    nodes: Vec<(klipp_nodes::NodeKind, Node)>,
    timeline_in_secs: f64,
    rate: u32,
}

/// The snapshot a tabbed page draws its shared rows from this frame: the
/// clip's own grade, with the generated inspector on (`on`); `None` for a
/// nested grade (descended into a compound) or a stand-in's (the timeline
/// grade, a group's stage), which keep the old rows. Every caller of the
/// tabbed page takes it the same way, each frame, and drops it after.
pub(crate) fn snapshot_for(clip: &Clip, on: bool, nested: bool, stand_in: bool) -> Option<GradeSnapshot> {
    (on && !nested && !stand_in).then(|| GradeSnapshot::of(clip)).flatten()
}

impl GradeSnapshot {
    /// The clip's grade nodes, or `None` when its picture has no grade
    /// group.
    pub(crate) fn of(clip: &Clip) -> Option<Self> {
        let picture = &clip.picture_v2().ok()?.picture;
        let group = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP)?;
        let body = &group.group.as_ref()?.graph;
        let nodes = clip.grade().nodes.iter().filter_map(|v1| body.nodes.iter().find(|n| n.id == v1.id).map(|n| (v1.kind, n.clone()))).collect();
        Some(Self { clip: clip.id, group: group.id, nodes, timeline_in_secs: clip.timeline_in.to_secs_f64(), rate: clip.timeline_in.rate })
    }

    /// Draw `node`'s parameter `key` as its generated row, as the per-node
    /// layout draws it, and push what it implies: whether it drew one (not
    /// for a node or key the snapshot doesn't hold, a phantom Corrector's).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn row(&self, ui: &mut egui::Ui, theme: &KlippTheme, node: NodeId, key: &str, cmds: &mut Vec<AppCommand>, frame: FrameSelection) -> bool {
        let Some((kind, lifted)) = self.nodes.iter().find(|(_, n)| n.id == node) else { return false };
        let Some(ty) = klipp_nodes::registry::node_type(*kind) else { return false };
        let Some(desc) = ty.desc.params.iter().find(|p| p.key == key) else { return false };
        let pres = ty.presentation(desc);
        let addr = NodePath::new(Root::Clip { clip: self.clip.0 }, [self.group, node]);
        let entry = entry_of(lifted);
        let stored = grade_value(&entry, desc);
        let base = row::number(&stored).unwrap_or(desc.default);
        let (shown_at_t, keys) = keys_at(lifted, desc, base, frame.time_secs, frame.tolerance);
        let mut ctx = RowCtx {
            addr: &addr,
            clip_time_secs: frame.time_secs,
            timeline_in_secs: self.timeline_in_secs,
            rate: self.rate,
            encoding: klipp_grade_model::Encoding::from_u8(klipp_core::grade_encoding().effective(klipp_engine::video_decoder::working_colorspace()).as_u8()),
            entry: Some(Entry { payload: &entry, fields_at: &["params"], choices_by_index: true }),
            cmds,
            behaviours: None,
            theme,
            app: None,
            custom: None,
        };
        row::param_row(ui, &mut ctx, row::RowInput { desc, pres: &pres, stored: &stored, shown_at_t, keys });
        true
    }
}
