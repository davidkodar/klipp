//! The generated grade inspector (M3 step 11): a grade node's own layout,
//! drawn by the generated rows from what its catalog type describes
//! (`klipp_nodes::registry`) and how that is shown (its presentation),
//! writing by the node's path. A grade nested in a compound is drawn the
//! same way: since picture revision 4 a compound is a group of the picture,
//! so its nodes have paths through it (#1391).
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

/// Whether an HDR Zones page (#240) showing zone `zone` draws the
/// parameter `key`: one of that zone's (`hdr_<zone>_...`).
pub(crate) fn hdr_zone_shows(key: &str, zone: usize) -> bool {
    key.strip_prefix("hdr_").and_then(|r| r.strip_prefix(klipp_nodes::params::HDR_ZONE_KEYS[zone])).is_some_and(|r| r.starts_with('_'))
}

/// Where a clip's grade node is in its picture: its path, and the node.
pub fn grade_node(clip: &Clip, node: NodeId) -> Option<(NodePath, Node)> {
    let picture = &clip.picture_v2().ok()?.picture;
    let group = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP)?;
    let inner = group.group.as_ref()?.graph.nodes.iter().find(|n| n.id == node)?;
    Some((NodePath::new(Root::Clip { clip: clip.id.0 }, [group.id, node]), inner.clone()))
}

/// The lifted body `descent` (compound ids, outermost first) names in
/// `body`: each compound a group whose body is its subgraph (#1391).
fn lifted_level<'a>(body: &'a klipp_graph_format::Graph, descent: &[NodeId]) -> Option<&'a klipp_graph_format::Graph> {
    descent.iter().try_fold(body, |g, id| Some(&g.nodes.iter().find(|n| n.id == *id && n.type_id.as_str() == lt::GROUP)?.group.as_ref()?.graph))
}

/// The v1 subgraph `descent` names in `grade`, as the canvas descends.
pub(crate) fn v1_level<'a>(grade: &'a klipp_nodes::NodeGraph, descent: &[NodeId]) -> Option<&'a klipp_nodes::NodeGraph> {
    descent.iter().try_fold(grade, |g, id| g.nodes.iter().find(|n| n.id == *id && n.kind == klipp_nodes::NodeKind::Compound)?.params.compound_subgraph.as_ref())
}

/// The nodes of `level` something is wired into the mask port of.
fn masked(level: &klipp_graph_format::Graph) -> Vec<NodeId> {
    level.edges.iter().filter(|e| e.to.1.as_str() == "mask").map(|e| e.to.0).collect()
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
pub(crate) fn show(ui: &mut egui::Ui, theme: &KlippTheme, clip: &Clip, descent: &[NodeId], node: NodeId, cmds: &mut Vec<AppCommand>, frame: FrameSelection, panel: Option<&mut NodeInspectorPanel>) -> bool {
    let start = cmds.len();
    if !GradeSnapshot::of(clip, descent, None).is_some_and(|s| s.page(ui, theme, node, cmds, frame, panel)) {
        return false;
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
    /// The clip the grade's own commands name: the clip, or the stand-in
    /// a sequence's grade is edited through (`GradeStandIn`).
    clip: klipp_core::ClipId,
    /// Where the grade's nodes are by path: a clip's under its grade group
    /// (`prefix` that group), the timeline grade's and a group stage's at
    /// their root's top (M3 step 19).
    root: Root,
    prefix: Vec<klipp_graph_format::NodeId>,
    nodes: Vec<(klipp_nodes::NodeKind, Node)>,
    /// The grade's nodes as stored, for their headers and editors.
    v1: Vec<klipp_nodes::Node>,
    /// The node masks a tracker drives, as `(node, mask)`.
    tracked: Vec<(NodeId, usize)>,
    /// The nodes something is wired into the mask port of.
    masked: Vec<NodeId>,
    /// The Corrector the clip's first edit on the tabbed page makes, when
    /// its grade has no enabled one: drawn from its defaults, under the id
    /// it will take, and written nowhere until that edit.
    pending: Option<NodeId>,
    timeline_in_secs: f64,
    rate: u32,
}

/// The snapshot a tabbed page draws its shared rows from this frame: the
/// clip's own grade at `descent` (the compounds the canvas is inside,
/// outermost first); `None` for a stand-in's (the timeline grade, a
/// group's stage), which [`snapshot_for_root`] takes. Every caller of the
/// tabbed page takes it the same way, each frame, and drops it after.
///
/// `pending` is the app's reservation for a Corrector the clip doesn't
/// have yet (`AppState::pending_corrector`): kept while the clip's grade
/// has no enabled Corrector, so every frame and the edit that makes it
/// name one node, and a fresh id for each clip that needs one.
pub(crate) fn snapshot_for(clip: &Clip, descent: &[NodeId], stand_in: bool, pending: &mut Option<(klipp_core::ClipId, NodeId)>) -> Option<GradeSnapshot> {
    if stand_in {
        return None;
    }
    // The level drawn, the grade's top or a compound inside it (#1483):
    // its first edit makes its Corrector there.
    let level = v1_level(clip.grade(), descent)?;
    // Once the level has its Corrector the reservation is spent: a later
    // one (after that node is removed) is a new id, never one a node had.
    let reserved = reserve(level, clip.id, pending);
    *pending = reserved.map(|id| (clip.id, id));
    GradeSnapshot::of(clip, descent, reserved)
}

/// [`snapshot_for`] for a stand-in (M3 step 19): `stand_in`, the clip the
/// timeline grade or a group stage is edited through, as the grade at
/// `root`, at `descent` within it. Its nodes are addressed at `root`, and
/// its pending Corrector is reserved as a clip's is, under
/// the stand-in's id. While the root has no grade yet, the stand-in's (the graph it was
/// put on with) is the one drawn, and `start` keeps it for the first
/// edit to begin from, so that edit's ids are the ones drawn.
pub(crate) fn snapshot_for_root(
    seq: &klipp_timeline::Sequence,
    stand_in: klipp_core::ClipId,
    root: Root,
    descent: &[NodeId],
    pending: &mut Option<(klipp_core::ClipId, NodeId)>,
    start: &mut Option<(Root, klipp_nodes::NodeGraph)>,
) -> Option<GradeSnapshot> {
    let clip = seq.all_clips().find(|c| c.id == stand_in)?;
    let stored = seq.root_grade(root).ok()?;
    *start = stored.is_none().then(|| (root, clip.grade().clone()));
    let level = v1_level(stored.unwrap_or(clip.grade()), descent)?;
    let reserved = reserve(level, stand_in, pending);
    *pending = reserved.map(|id| (stand_in, id));
    GradeSnapshot::of_root(seq, clip, root, descent, reserved)
}

/// The Corrector reserved for `level` (a grade's top, or a compound's
/// subgraph in it) drawn for `clip`: none when the level has an enabled
/// one; else the reservation already held for that clip, while no node of
/// the level has its id; else a fresh id.
fn reserve(level: &klipp_nodes::NodeGraph, clip: klipp_core::ClipId, pending: &Option<(klipp_core::ClipId, NodeId)>) -> Option<NodeId> {
    let has = level.nodes.iter().any(|n| n.kind == klipp_nodes::NodeKind::Corrector && n.enabled);
    match *pending {
        _ if has => None,
        Some((c, id)) if c == clip && !level.nodes.iter().any(|n| n.id == id) => Some(id),
        _ => Some(NodeId::new_v4()),
    }
}

/// The Corrector a tabbed page's first edit adds, as its picture holds it
/// (M3 step 19): a grade node of its catalog type with every parameter
/// unset, as `NodeGraph::ensure_corrector` makes one, under `id`.
pub(crate) fn pending_corrector(id: NodeId) -> Node {
    let payload = serde_json::json!({"kind": klipp_nodes::NodeKind::Corrector, "label": "Corrector", "params": klipp_nodes::NodeParams::default()});
    serde_json::from_value(serde_json::json!({"id": id, "type_id": klipp_graph_format::types::LEGACY_NODE, "label": "", "legacy": {"payload": payload}}))
        .expect("a grade node's form")
}

/// A new grade node of `kind` as its picture holds it (#1476): every
/// parameter unset, as the canvas adds one, under `id`; a merge kind (Layer,
/// Blend, Layer Mixer) as the lift makes it.
pub(crate) fn new_grade_node(kind: klipp_nodes::NodeKind, id: NodeId) -> Node {
    let payload = serde_json::json!({"kind": kind, "label": kind.display_name(), "params": klipp_nodes::NodeParams::default()});
    let mut node: Node = serde_json::from_value(serde_json::json!({"id": id, "type_id": klipp_graph_format::types::LEGACY_NODE, "label": "", "legacy": {"payload": payload}}))
        .expect("a grade node's form");
    let name = serde_json::to_value(kind).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default();
    if let Some((_, op)) = lt::MERGE_OPS.iter().find(|(k, _)| *k == name) {
        node.type_id = klipp_graph_format::NodeTypeId::new(lt::MERGE);
        node.params.insert("op".into(), klipp_graph_format::ParamValue::Text((*op).to_owned()));
    }
    node
}

impl GradeSnapshot {
    /// The clip's grade nodes at `descent`, and the Corrector `pending`
    /// names when its grade has no enabled one; or `None` when its picture
    /// has no grade group, or no compound at `descent`.
    pub(crate) fn of(clip: &Clip, descent: &[NodeId], pending: Option<NodeId>) -> Option<Self> {
        let picture = &clip.picture_v2().ok()?.picture;
        let group = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == lt::GROUP)?;
        let body = lifted_level(&group.group.as_ref()?.graph, descent)?;
        let grade = v1_level(clip.grade(), descent)?;
        let mut nodes: Vec<_> = grade.nodes.iter().filter_map(|v1| body.nodes.iter().find(|n| n.id == v1.id).map(|n| (v1.kind, n.clone()))).collect();
        let pending = pending.filter(|id| !nodes.iter().any(|(_, n)| n.id == *id));
        if let Some(id) = pending {
            nodes.push((klipp_nodes::NodeKind::Corrector, pending_corrector(id)));
        }
        Some(Self {
            clip: clip.id,
            root: Root::Clip { clip: clip.id.0 },
            prefix: std::iter::once(group.id).chain(descent.iter().copied()).collect(),
            nodes,
            v1: grade.nodes.clone(),
            tracked: clip.track_points.iter().filter_map(|tp| tp.linked_mask).collect(),
            masked: masked(body),
            pending,
            timeline_in_secs: clip.timeline_in.to_secs_f64(),
            rate: clip.timeline_in.rate,
        })
    }

    /// The grade of `root`, one of the sequence's own (the timeline grade,
    /// a group stage), as drawn through its stand-in clip: its nodes by
    /// their paths at the root, read from the grade stored there, or from
    /// the stand-in's while the root has none yet (`stored` is `None`).
    /// The Corrector `pending` names is added as in [`GradeSnapshot::of`].
    pub(crate) fn of_root(seq: &klipp_timeline::Sequence, stand_in: &Clip, root: Root, descent: &[NodeId], pending: Option<NodeId>) -> Option<Self> {
        let top = seq.root_grade(root).ok()?.unwrap_or(stand_in.grade());
        let whole = seq.lift_root_grade(root, top).ok()?;
        let lifted = lifted_level(&whole, descent)?;
        let grade = v1_level(top, descent)?;
        let mut nodes: Vec<_> = grade.nodes.iter().filter_map(|v1| lifted.nodes.iter().find(|n| n.id == v1.id).map(|n| (v1.kind, n.clone()))).collect();
        let pending = pending.filter(|id| !nodes.iter().any(|(_, n)| n.id == *id));
        if let Some(id) = pending {
            nodes.push((klipp_nodes::NodeKind::Corrector, pending_corrector(id)));
        }
        Some(Self {
            clip: stand_in.id,
            root,
            prefix: descent.to_vec(),
            nodes,
            v1: grade.nodes.clone(),
            tracked: Vec::new(),
            masked: masked(lifted),
            pending,
            timeline_in_secs: stand_in.timeline_in.to_secs_f64(),
            rate: stand_in.timeline_in.rate,
        })
    }

    /// The path of `node` in this grade.
    fn addr(&self, node: NodeId) -> NodePath {
        NodePath::new(self.root, self.prefix.iter().copied().chain([node]))
    }

    /// Draw `node`'s per-node page and push the commands its edits imply:
    /// `false`, drawing nothing, when the grade doesn't hold it or its
    /// kind has no catalog type. See [`show`].
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn page(&self, ui: &mut egui::Ui, theme: &KlippTheme, node: NodeId, cmds: &mut Vec<AppCommand>, frame: FrameSelection, mut panel: Option<&mut NodeInspectorPanel>) -> bool {
        let Some(v1) = self.v1.iter().find(|n| n.id == node) else { return false };
        let Some(ty) = klipp_nodes::registry::node_type(v1.kind) else { return false };
        let Some((_, lifted)) = self.nodes.iter().find(|(_, n)| n.id == node) else { return false };
        let addr = self.addr(node);
        crate::panels::node_inspector::node_header(ui, &v1.kind, v1.enabled, &v1.keyframes, node, self.clip, cmds, theme, frame);
        // How much of its change the node keeps and, when something masks
        // it, the channel it reads the mask by (#1348).
        {
            use klipp_graph_format::PortCatalog;
            // A track's grade has no stored form for them yet (#1408).
            let keeps_blends = !matches!(self.root, Root::Track { .. });
            if keeps_blends && klipp_timeline::RegistryCatalog.node_spec(lifted).is_some_and(|s| s.mixable) {
                // "Node mix": a page may have a Mix of its own (the curves').
                crate::inspector::stack_view::mix_row(ui, "Node mix", &addr, lifted.mix, false, cmds);
            }
            if keeps_blends && self.masked.contains(&node) {
                crate::inspector::stack_view::mask_channel_row(ui, lifted, &addr, false, cmds);
            }
        }
        // The widgets edit a copy and push the grade's own commands.
        let mut params = v1.params.clone();
        if let Some(p) = panel.as_deref_mut() {
            if ui.indent(("grade_node", node), |ui| p.grade_whole_node(ui, v1.kind, &mut params, node, self.clip, cmds, theme)).inner {
                return true;
            }
        }
        if v1.kind == klipp_nodes::NodeKind::Qualifier {
            let q = v1.params.qualifier.clone().unwrap_or_default();
            if q.mode == klipp_core::QualifierMode::ThreeD {
                // The 3D key's strokes (#895): drawn across the colours to
                // keep and to take out, counted, and cleared.
                ui.horizontal(|ui| {
                    if ui.button("Keep").on_hover_text("Drag across the colours to keep, in the viewer").clicked() {
                        cmds.push(AppCommand::PickQualifierStroke { clip_id: self.clip, node_id: node, remove: false });
                    }
                    if ui.button("Remove").on_hover_text("Drag across the colours to take out of the key, in the viewer").clicked() {
                        cmds.push(AppCommand::PickQualifierStroke { clip_id: self.clip, node_id: node, remove: true });
                    }
                    let strokes = !q.keep_samples.is_empty() || !q.remove_samples.is_empty();
                    if ui.add_enabled(strokes, egui::Button::new("Clear")).on_hover_text("Take every stroke away").clicked() {
                        let qualifier = klipp_core::QualifierSettings { keep_samples: Vec::new(), remove_samples: Vec::new(), ..q.clone() };
                        cmds.push(AppCommand::SetNodeQualifier { clip_id: self.clip, node_id: node, qualifier });
                    }
                    ui.label(egui::RichText::new(format!("{} kept, {} out", q.keep_samples.len(), q.remove_samples.len())).size(10.0).color(theme.text_dim));
                });
            } else {
                let pick = ui.add(egui::Button::image(crate::icons::Icon::Eyedropper.image(12.0).tint(theme.overlay(160))).min_size(egui::vec2(22.0, 18.0)))
                    .on_hover_text("Pick a colour: click it in the viewer to key it");
                if pick.clicked() {
                    cmds.push(AppCommand::PickQualifierColour { clip_id: self.clip, node_id: node });
                }
            }
        }
        let entry = entry_of(lifted);
        // An HDR Zones node shows its picked zone's controls alone (#240).
        let picked_zone = (v1.kind == klipp_nodes::NodeKind::HdrZones).then(|| panel.as_deref().map_or(0, |p| p.hdr_zone_picked(node)));
        let presentation = |p: &ParamDesc| {
            let mut shown = ty.presentation(p);
            if picked_zone.is_some_and(|zone| !hdr_zone_shows(p.key, zone)) {
                shown.widget = klipp_effects::registry::presentation::Widget::Hidden;
            }
            shown
        };
        let stored = |p: &ParamDesc| grade_value(&entry, p);
        let keys = |p: &ParamDesc| {
            let base = row::number(&grade_value(&entry, p)).unwrap_or(p.default);
            keys_at(lifted, p, base, frame.time_secs, frame.tolerance)
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
            p.tracked_masks = self.tracked.iter().filter(|(n, _)| *n == node).map(|(_, i)| *i).collect();
            let drawn = p.grade_editor(ui, id, ty, &mut params, node, self.clip, cmds, theme, frame, Some(&v1.keyframes));
            p.tracked_masks.clear();
            if drawn {
                done.push(id.to_owned());
            }
            drawn
        };
        let mut ctx = RowCtx {
            addr: &addr,
            clip_time_secs: frame.time_secs,
            timeline_in_secs: self.timeline_in_secs,
            rate: self.rate,
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
        if let Some(zone) = picked_zone {
            ui.add_space(8.0);
            crate::panels::node_inspector::hdr_zone_map(ui, &params.hdr_zones.clone().unwrap_or_default(), zone, theme);
        }
        if drawn.is_empty() {
            ui.label(egui::RichText::new("No parameters").color(theme.text_dim));
        }
        if v1.kind == klipp_nodes::NodeKind::Corrector {
            crate::panels::node_inspector::cdl_buttons(ui, &v1.params, node, self.clip);
        }
        true
    }

    /// The Corrector this snapshot draws before it exists (see
    /// [`GradeSnapshot::of`]).
    pub(crate) fn pending(&self) -> Option<NodeId> {
        self.pending
    }

    /// The Corrector the Colour page's Adjustments tab edits: the level's
    /// first, else the one [`GradeSnapshot::pending`] draws.
    pub(crate) fn corrector(&self) -> Option<NodeId> {
        self.v1.iter().find(|n| n.kind == klipp_nodes::NodeKind::Corrector).map(|n| n.id).or(self.pending)
    }

    /// The node printer lights grade at this level (#1527): its enabled
    /// Corrector, else its enabled Colour Wheels, else the Corrector it
    /// reserves, which their first edit makes. Its path, and the offset it
    /// holds now.
    pub(crate) fn printer_light_target(&self) -> Option<(NodePath, (f32, f32, f32))> {
        let enabled = |kind| self.v1.iter().find(|n| n.kind == kind && n.enabled);
        if let Some(n) = enabled(klipp_nodes::NodeKind::Corrector).or_else(|| enabled(klipp_nodes::NodeKind::ColorWheels)) {
            let p = &n.params;
            return Some((self.addr(n.id), (p.offset_x.unwrap_or(0.0), p.offset_y.unwrap_or(0.0), p.offset_master.unwrap_or(0.0))));
        }
        self.pending.map(|id| (self.addr(id), (0.0, 0.0, 0.0)))
    }

    /// Draw one tab of the tabbed page (M3 step 19) for `node`: its
    /// generated rows whose presentation order is in `band` (a Corrector
    /// holds every tab, each its own band; any other kind is all one tab,
    /// `None`), the specialist editors among them drawn by `panel`, which
    /// edits `params`, the node's own values. Whether it drew: not for a
    /// node the snapshot doesn't hold.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn tab(&self, ui: &mut egui::Ui, theme: &KlippTheme, node: NodeId, band: Option<std::ops::RangeInclusive<u16>>, params: &mut klipp_nodes::NodeParams, panel: &mut NodeInspectorPanel, cmds: &mut Vec<AppCommand>, frame: FrameSelection) -> bool {
        let Some((kind, lifted)) = self.nodes.iter().find(|(_, n)| n.id == node) else { return false };
        let Some(ty) = klipp_nodes::registry::node_type(*kind) else { return false };
        let addr = self.addr(node);
        let entry = entry_of(lifted);
        let in_band: Vec<ParamDesc> = ty.desc.params.iter().filter(|p| band.as_ref().is_none_or(|b| b.contains(&ty.presentation(p).order))).copied().collect();
        let presentation = |p: &ParamDesc| ty.presentation(p);
        let stored = |p: &ParamDesc| grade_value(&entry, p);
        let keys = |p: &ParamDesc| {
            let base = row::number(&grade_value(&entry, p)).unwrap_or(p.default);
            keys_at(lifted, p, base, frame.time_secs, frame.tolerance)
        };
        let rows = NodeRows { params: &in_band, presentation: &presentation, stored: &stored, keys: &keys };
        let mut done: Vec<String> = Vec::new();
        let clip = self.clip;
        // The node's keys, for its editors' key strips: none for a pending
        // Corrector, which a key couldn't land on.
        let keyframes = self.v1.iter().find(|n| n.id == node).map(|n| &n.keyframes);
        let mut hook = |ui: &mut egui::Ui, id: &str, _: &ParamDesc, cmds: &mut Vec<AppCommand>| -> bool {
            if done.iter().any(|d| d == id) {
                return true;
            }
            let drawn = panel.grade_editor(ui, id, ty, params, node, clip, cmds, theme, frame, keyframes);
            if drawn {
                done.push(id.to_owned());
            }
            drawn
        };
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
            custom: Some(&mut hook),
        };
        ui.indent(("grade_tab", node), |ui| node_rows(ui, &mut ctx, &rows));
        true
    }

    /// Draw `node`'s parameter `key` as its generated row, as the per-node
    /// layout draws it, and push what it implies: whether it drew one (not
    /// for a node or key the snapshot doesn't hold).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn row(&self, ui: &mut egui::Ui, theme: &KlippTheme, node: NodeId, key: &str, cmds: &mut Vec<AppCommand>, frame: FrameSelection) -> bool {
        let Some((kind, lifted)) = self.nodes.iter().find(|(_, n)| n.id == node) else { return false };
        let Some(ty) = klipp_nodes::registry::node_type(*kind) else { return false };
        let Some(desc) = ty.desc.params.iter().find(|p| p.key == key) else { return false };
        let pres = ty.presentation(desc);
        let addr = self.addr(node);
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
