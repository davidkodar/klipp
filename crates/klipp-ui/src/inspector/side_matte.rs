//! A matte beside a clip's picture chain (#1746): a window, a roto shape,
//! a luma or difference key whose matte masks an effect, drawn under that
//! effect's "Masked by" chip. Its numbers and switches are its generated
//! rows, written by path; its shapes (the mask stack) and a difference
//! key's settings with its clean plate are written whole, by path, since a
//! matte beside the chain isn't in the clip's grade, where the grade's own
//! editors write.
//!
//! A shape's keyframes and tracking work on the clip's grade only: here
//! they stay as they are, and the shapes are edited as they stand.

use crate::inspector::generated::{grade_value, keys_at, node_rows, row, Entry, NodeRows, RowCtx};
use crate::inspector::keyframe_selection::FrameSelection;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;
use klipp_effects::registry::ParamDesc;
use klipp_graph_format::path::NodePath;
use klipp_graph_format::{Graph, Node};
use klipp_nodes::{NodeKind, NodeParams};

/// What a Qualifier beside the chain says until its colour is set.
pub(crate) const QUALIFIER_UNSET: &str = "Set Hue Center to the colour to key, or key it on the Colour page with + Qualifier";

/// What a drawn shape's card says of its points.
pub(crate) const ROTO_POINTS: &str = "Its points are moved in the viewer: pick this shape, then the Mask Tool in the timeline's toolbar";

/// The node `addr` names when it is a matte beside the chain: a grade
/// node at the picture's top, outside its grade group, with its kind and
/// settings as a grade run reads them.
fn matte_at<'a>(graph: &'a Graph, addr: &NodePath) -> Option<(&'a Node, NodeKind, NodeParams)> {
    (addr.nodes.len() == 1).then_some(())?;
    let node = crate::inspector::stack_view::node_at(graph, addr)?;
    klipp_graph_lift::effect_params::tag(node).is_none().then_some(())?;
    let run = klipp_timeline::grade_node_for_run(node).ok()?;
    Some((node, run.kind, run.params))
}

/// `value`, a structure of the matte's settings, written whole by path.
fn whole(addr: &NodePath, key: &str, value: &impl serde::Serialize) -> Option<AppCommand> {
    Some(AppCommand::SetParamAt { addr: addr.clone(), key: key.into(), value: klipp_graph_format::ParamValue::Text(serde_json::to_string(value).ok()?) })
}

/// Draw the matte `addr` names in `graph`, `clip`'s picture, and push the
/// commands its edits imply: `false`, drawing nothing, when `addr` isn't a
/// matte beside the chain. `source_secs` is the clip's source time at the
/// playhead, the frame a difference key's plate can be picked at.
#[allow(clippy::too_many_arguments)]
pub(crate) fn show(ui: &mut egui::Ui, theme: &KlippTheme, clip: &klipp_timeline::Clip, graph: &Graph, addr: &NodePath, frame: FrameSelection, source_secs: f64, cmds: &mut Vec<AppCommand>) -> bool {
    let Some((node, kind, params)) = matte_at(graph, addr) else { return false };
    let Some(ty) = klipp_nodes::registry::node_type(kind) else { return false };
    // A Qualifier here starts off, waiting for its colour: the eyedropper
    // reaches only a grade's own keys, so say what sets this one.
    if kind == NodeKind::Qualifier && params.qualifier.as_ref().is_none_or(|q| !q.enabled) {
        ui.add(egui::Label::new(egui::RichText::new(QUALIFIER_UNSET).size(10.0).color(theme.text_dim)).wrap());
    }
    // A drawn shape has no numbers for its points: they are moved in the
    // viewer, which a walk didn't find.
    if kind == NodeKind::Mask && params.masks.as_ref().is_some_and(|m| m.masks.iter().any(|m| matches!(m.shape, klipp_core::MaskShape::Bezier { .. }))) {
        ui.add(egui::Label::new(egui::RichText::new(ROTO_POINTS).size(10.0).color(theme.text_dim)).wrap());
    }
    // The rows read the settings as a grade node's entry holds them.
    let entry = serde_json::json!({ "kind": kind, "params": params });
    let presentation = |p: &ParamDesc| ty.presentation(p);
    let stored = |p: &ParamDesc| grade_value(&entry, p);
    let keys = |p: &ParamDesc| {
        let base = row::number(&grade_value(&entry, p)).unwrap_or(p.default);
        keys_at(node, p, base, frame.time_secs, frame.tolerance)
    };
    let rows = NodeRows { params: ty.desc.params, presentation: &presentation, stored: &stored, keys: &keys };
    let mut hook = |ui: &mut egui::Ui, id: &str, _: &ParamDesc, cmds: &mut Vec<AppCommand>| -> bool {
        match id {
            "mask_stack" => {
                let mut edited = params.clone();
                let mut grade_cmds = Vec::new();
                let masks = edited.masks.get_or_insert_with(Default::default);
                // Its masks a tracker is bound to, shown as tracked
                // (Re-track, Untrack, the stretch followed), as a grade's are.
                let tracked: Vec<usize> = clip.track_points.iter().filter_map(|tp| tp.linked_mask.filter(|(n, _)| *n == node.id).map(|(_, i)| i)).collect();
                crate::panels::node_inspector::NodeInspectorPanel::render_masks_body(ui, masks, node.id, clip.id, &mut grade_cmds, theme, frame, &tracked);
                cmds.extend(grade_cmds.into_iter().filter_map(|c| by_path(c, addr)));
                true
            }
            "plate_file" => {
                plate_picker(ui, theme, addr, &params, source_secs, cmds);
                true
            }
            _ => false,
        }
    };
    let mut ctx = RowCtx {
        addr,
        clip_time_secs: frame.time_secs,
        timeline_in_secs: clip.timeline_in.to_secs_f64(),
        rate: clip.timeline_in.rate,
        encoding: klipp_grade_model::Encoding::from_u8(klipp_core::grade_encoding().effective(klipp_engine::video_decoder::working_colorspace()).as_u8()),
        entry: Some(Entry { payload: &entry, fields_at: &["params"], choices_by_index: true }),
        cmds,
        behaviours: None,
        theme,
        app: None,
        custom: Some(&mut hook),
    };
    ui.indent(("side_matte", addr.clone()), |ui| node_rows(ui, &mut ctx, &rows));
    true
}

/// A grade editor's command for the matte, as its path's: its masks
/// written whole, and its drag gesture as the path's. What the grade
/// alone answers (a shape's keys, its tracking) is left out.
pub(crate) fn by_path(cmd: AppCommand, addr: &NodePath) -> Option<AppCommand> {
    let key = || klipp_graph_format::ParamKey::from("masks");
    Some(match cmd {
        AppCommand::SetNodeMasks { masks, .. } => whole(addr, "masks", &masks)?,
        AppCommand::BeginNodeParamEdit { .. } => AppCommand::BeginParamEditAt { addr: addr.clone(), key: key() },
        AppCommand::EndNodeParamEdit { .. } => AppCommand::EndParamEditAt { addr: addr.clone(), key: key() },
        AppCommand::AbandonNodeParamEdit { .. } => AppCommand::AbandonParamEditAt { addr: addr.clone(), key: key() },
        // A mask's keys go as they are: the app writes them into the
        // matte's masks by path. Dropped here, a diamond set no key.
        AppCommand::AddMaskKeyframe { .. } | AppCommand::RemoveMaskKeyframe { .. } | AppCommand::ClearMaskKeyframes { .. } | AppCommand::MoveMaskKeyframe { .. } | AppCommand::SetMaskKeyframeInterpolation { .. } => cmd,
        // Its tracking as well: the app reads and keys the window where it is.
        AppCommand::TrackMaskRegion { .. } | AppCommand::RemoveMaskTracking { .. } => cmd,
        _ => return None,
    })
}


/// A difference key's clean plate: a frame of the clip's own picked at
/// the playhead (#2758), or a file; its name, Use This Frame, Browse and
/// Clear.
fn plate_picker(ui: &mut egui::Ui, theme: &KlippTheme, addr: &NodePath, params: &NodeParams, source_secs: f64, cmds: &mut Vec<AppCommand>) {
    let plate = params.difference_key.as_ref().and_then(|d| d.plate_path.clone());
    let picked = params.difference_key.as_ref().and_then(|d| d.plate_frame);
    ui.horizontal(|ui| {
        ui.label("Plate");
        match (&plate, picked) {
            (_, Some(secs)) => {
                ui.label(format!("The clip's frame at {secs:.2} s")).on_hover_text("The clip's own frame, in its file's seconds: a trim leaves it where it is");
            }
            (Some(p), None) => {
                ui.label(p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned())).on_hover_text(p.display().to_string());
            }
            (None, None) => {
                ui.label(egui::RichText::new("None chosen").color(theme.text_dim));
            }
        }
    });
    let key = params.difference_key.clone().unwrap_or_default();
    let with_frame = {
        let key = key.clone();
        move |addr: &NodePath, secs: Option<f64>| {
            let mut key = key.clone();
            key.plate_frame = secs;
            whole(addr, "difference_key", &key)
        }
    };
    let with_plate = move |addr: &NodePath, plate: Option<std::path::PathBuf>| {
        let mut key = key.clone();
        key.plate_path = plate;
        key.plate_frame = None;
        whole(addr, "difference_key", &key)
    };
    ui.horizontal(|ui| {
        if ui.button("Use This Frame").on_hover_text("The clip's frame at the playhead as the clean plate: park on a frame without the subject").clicked() {
            cmds.extend(with_frame(addr, Some(source_secs)));
        }
        if ui.button("Browse\u{2026}").on_hover_text("Choose the clean plate: the shot without its subject").clicked() {
            let (addr, with_plate) = (addr.clone(), with_plate.clone());
            crate::file_dialog::spawn_pick(
                || rfd::FileDialog::new().set_title("Clean plate").add_filter("Images", &["png", "jpg", "jpeg", "tif", "tiff", "exr"]).pick_file(),
                move |chosen| with_plate(&addr, Some(chosen)).into_iter().collect(),
            );
        }
        if (plate.is_some() || picked.is_some()) && ui.button("Clear").clicked() {
            cmds.extend(with_plate(addr, None));
        }
    });
}

#[cfg(test)]
mod key_tests {
    /// The diamond by a matte's mask field sends its key: it was dropped
    /// here, so the Feather's diamond set no key.
    #[test]
    fn a_mask_key_from_the_matte_panel_is_sent() {
        let addr = klipp_graph_format::path::NodePath::new(klipp_graph_format::path::Root::Clip { clip: uuid::Uuid::new_v4() }, [uuid::Uuid::new_v4()]);
        let add = klipp_core::AppCommand::AddMaskKeyframe { clip_id: klipp_core::ClipId::new(), node_id: uuid::Uuid::new_v4(), mask_index: 0, param: "feather".into(), time_secs: 0.0, value: 0.05 };
        assert!(matches!(super::by_path(add, &addr), Some(klipp_core::AppCommand::AddMaskKeyframe { .. })));
        // And its Track button's commands: they were dropped too.
        let track = klipp_core::AppCommand::TrackMaskRegion { clip_id: klipp_core::ClipId::new(), node_id: uuid::Uuid::new_v4(), mask_index: 0, perspective: false };
        assert!(matches!(super::by_path(track, &addr), Some(klipp_core::AppCommand::TrackMaskRegion { .. })));
    }
}
