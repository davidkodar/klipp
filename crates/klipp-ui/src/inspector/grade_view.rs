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

/// Draw the clip's grade node `node` and push the commands its edits
/// imply: `false`, drawing nothing, when the node isn't in the clip's own
/// grade or its kind has no catalog type.
pub(crate) fn show(ui: &mut egui::Ui, theme: &KlippTheme, clip: &Clip, node: NodeId, cmds: &mut Vec<AppCommand>, frame: FrameSelection) -> bool {
    let Some(v1) = clip.grade().nodes.iter().find(|n| n.id == node) else { return false };
    let Some(ty) = klipp_nodes::registry::node_type(v1.kind) else { return false };
    let Some((addr, lifted)) = grade_node(clip, node) else { return false };
    let start = cmds.len();
    crate::panels::node_inspector::node_header(ui, &v1.kind, v1.enabled, &v1.keyframes, node, clip.id, cmds, theme, frame);
    let entry = lifted.legacy.as_ref().map(|l| l.payload.clone()).unwrap_or(serde_json::Value::Null);
    let presentation = |p: &ParamDesc| ty.presentation(p);
    let stored = |p: &ParamDesc| grade_value(&entry, p);
    let keys = |p: &ParamDesc| {
        let base = row::number(&grade_value(&entry, p)).unwrap_or(p.default);
        keys_at(&lifted, p, base, frame.time_secs, frame.tolerance)
    };
    let rows = NodeRows { params: ty.desc.params, presentation: &presentation, stored: &stored, keys: &keys };
    let mut ctx = RowCtx {
        addr: &addr,
        clip_time_secs: frame.time_secs,
        timeline_in_secs: clip.timeline_in.to_secs_f64(),
        rate: clip.timeline_in.rate,
        encoding: klipp_grade_model::Encoding::from_u8(klipp_core::grade_encoding().effective(klipp_engine::video_decoder::working_colorspace()).as_u8()),
        entry: Some(Entry { payload: &entry, fields_at: &["params"] }),
        cmds: &mut *cmds,
        // A grade parameter's Behaviours aren't kept yet.
        behaviours: None,
        theme,
        app: None,
    };
    let drawn = ui.indent(("grade_node", node), |ui| node_rows(ui, &mut ctx, &rows)).inner;
    if drawn.is_empty() {
        ui.label(egui::RichText::new("No parameters").color(theme.text_dim));
    }
    // A key the rows name at the playhead is the stored key in its frame.
    for cmd in &mut cmds[start..] {
        crate::inspector::keyframe_selection::resolve_for_clip(clip, frame.tolerance, cmd);
    }
    true
}
