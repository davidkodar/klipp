//! Path-addressed edits (M3 step 2): a node named by its root and the ids
//! down to it (`NodePath`), a parameter by its key. Each edit is a batch of
//! graph ops on that node's picture, applied as `GraphOps` is: one undo step
//! or nothing. They sit beside the index and bare-id commands, which stay
//! until the inspector switches over (step 19).
//!
//! A drag (`BeginParamEditAt` .. `EndParamEditAt`) of one `(addr, key)` is
//! one undo step. Any other edit while it is open closes it first, as one
//! step, and then records its own: an open drag never merges another edit.

use super::*;
use klipp_graph_format::path::{NodePath, Root};
use klipp_graph_format::{Node, ParamKey};
use klipp_graph_lift::ops::GraphOp;
use klipp_timeline::GradeAccess;

/// What every path drag's history key starts with.
const PATH_DRAG: &str = "at:";

fn layout_drag(root: Root) -> String { format!("{PATH_DRAG}{root:?}:layout") }

/// The history key a path drag is held under.
fn drag_key(addr: &NodePath, key: &ParamKey) -> String {
    format!("{}{key}", node_drag(addr))
}

/// The key of a drag over the node at `addr`'s parameters together, begun
/// with an empty key (a corner pin's handle moves two of them at once, M3
/// step 13); every other drag is on its one parameter.
fn node_drag(addr: &NodePath) -> String {
    let ids: Vec<String> = addr.nodes.iter().map(|n| n.to_string()).collect();
    format!("{PATH_DRAG}{:?}:{}:", addr.root, ids.join("/"))
}

/// The graph ops a path-addressed edit is, and its undo label; `None` for
/// the commands that aren't one batch of ops (the drags, the dock and the
/// split view's state, and every other command).
pub(crate) fn path_ops(cmd: &AppCommand) -> Option<(Vec<GraphOp>, &'static str)> {
    Some(match cmd {
        AppCommand::SetNodePositionAt { addr, position } => (vec![GraphOp::SetNodeMeta { path: addr.clone(), label: None, position: Some(serde_json::json!({"x":position.0,"y":position.1})) }], "Move Nodes"),
        AppCommand::AddNodeAt { after, node } => (vec![GraphOp::AddNode { after: after.clone(), node: node.clone() }], "Add Node"),
        AppCommand::RemoveNodeAt { addr } => (vec![GraphOp::RemoveNode { path: addr.clone() }], "Remove Node"),
        AppCommand::MoveNodeAt { addr, after } => (vec![GraphOp::MoveAfter { path: addr.clone(), after: *after }], "Move Node"),
        AppCommand::SetNodeEnabledAt { addr, enabled } => (vec![GraphOp::SetEnabled { path: addr.clone(), enabled: *enabled }], "Toggle Node"),
        AppCommand::SetParamAt { addr, key, value } => (vec![GraphOp::SetParam { path: addr.clone(), key: key.clone(), value: value.clone() }], "Edit Parameter"),
        AppCommand::SetPayloadAt { addr, payload } => (vec![GraphOp::SetPayload { path: addr.clone(), payload: payload.clone() }], "Edit Effect"),
        AppCommand::SetMixAt { addr, mix } => (vec![GraphOp::SetMix { path: addr.clone(), mix: *mix }], "Set Mix"),
        AppCommand::ConnectMaskAt { addr, from, from_port } => (
            vec![GraphOp::Connect { from: from.clone(), from_port: from_port.clone(), to: addr.clone(), to_port: "mask".into() }],
            "Connect Mask",
        ),
        AppCommand::SetMaskChannelAt { addr, channel } => (vec![GraphOp::SetMaskChannel { path: addr.clone(), channel: channel.clone() }], "Set Mask Channel"),
        AppCommand::AddKeyAt { addr, key, keyframe } => (vec![GraphOp::AddKeyAt { path: addr.clone(), key: key.clone(), keyframe: keyframe.clone() }], "Add Keyframe"),
        AppCommand::RemoveKeyAt { addr, key, time_secs } => (vec![GraphOp::RemoveKeyAt { path: addr.clone(), key: key.clone(), time_secs: *time_secs }], "Remove Keyframe"),
        AppCommand::SetAnnotationsAt { at, frames, sticky_notes, label } => (vec![GraphOp::SetAnnotations { path: at.clone(), frames: frames.clone(), sticky_notes: sticky_notes.clone() }], label),
        _ => return None,
    })
}

/// The node `path` names in `nodes`: each id but the last a group whose
/// body holds the next. A path that skips a group isn't found: nothing is
/// searched for.
fn walk<'a>(mut nodes: &'a [Node], ids: &[klipp_graph_format::NodeId]) -> Option<&'a Node> {
    let (last, through) = ids.split_last()?;
    for step in through {
        nodes = &nodes.iter().find(|n| n.id == *step)?.group.as_ref()?.graph.nodes;
    }
    nodes.iter().find(|n| n.id == *last)
}

/// The node an edit of one node's values names: its parameter, payload,
/// mix, keys, switch or mask channel. `None` for the ops that make,
/// remove, move or wire nodes.
fn edited_path(op: &GraphOp) -> Option<&NodePath> {
    match op {
        GraphOp::SetParam { path, .. } | GraphOp::SetPayload { path, .. } | GraphOp::SetMix { path, .. } | GraphOp::AddKeyAt { path, .. } | GraphOp::RemoveKeyAt { path, .. } | GraphOp::SetAnim { path, .. } | GraphOp::SetEnabled { path, .. } | GraphOp::SetMaskChannel { path, .. } => Some(path),
        _ => None,
    }
}

/// Whether `root` is one of a sequence's own grades, edited by path at
/// its root rather than in a clip's picture (M3 step 19).
fn is_sequence_grade(root: Root) -> bool {
    matches!(root, Root::TimelineGrade { .. } | Root::GroupStage { .. } | Root::Track { part: klipp_graph_format::path::TrackPart::Grade, .. })
}

fn grade_stage(stage: klipp_graph_format::lifted::Stage) -> klipp_timeline::GradeStage {
    match stage {
        klipp_graph_format::lifted::Stage::Pre => klipp_timeline::GradeStage::Pre,
        klipp_graph_format::lifted::Stage::Post => klipp_timeline::GradeStage::Post,
    }
}

impl KlippApp {
    pub(super) fn handle_path_commands(&mut self, cmd: &AppCommand) -> bool {
        match self.state.path_command(cmd) {
            None => false,
            Some(changed) => {
                if changed {
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                }
                if matches!(cmd, AppCommand::SetSplitCompareNode { .. }) {
                    self.split_compare_to_engine();
                }
                true
            }
        }
    }

    /// The split compare as the state holds it, to the engine (M3 step 4),
    /// which renders the compared node bypassed: a refused command leaves
    /// it as it was. Comparing an effect ends the other compare modes.
    fn split_compare_to_engine(&mut self) {
        let compared = self.state.split_compare_node.clone();
        if compared.is_some() {
            if self.state.before_after_active {
                self.state.before_after_active = false;
                let _ = self.cmd_tx.send(AppCommand::ToggleBeforeAfter);
            }
            if self.state.stabilization_compare_active {
                self.state.stabilization_compare_active = false;
                let _ = self.cmd_tx.send(AppCommand::ToggleStabilizationCompare);
            }
            self.state.grade_still_compare = false;
        }
        let (clip_id, path) = compared.map_or((None, None), |(c, p)| (Some(c), Some(p)));
        let _ = self.cmd_tx.send(AppCommand::SetSplitCompareNode { clip_id, path });
    }
}

/// `key`'s animation on `node`: its `anim`, or for a grade node (or a
/// merge lifted from the grade) its keyframe track on its record, where a
/// grade node keeps its keys. `None` when it has no keys there.
fn animation_of(node: &Node, key: &ParamKey) -> Option<klipp_graph_format::Animation> {
    if !matches!(node.type_id.as_str(), klipp_graph_format::types::LEGACY_NODE | klipp_graph_format::lifted::types::MERGE) {
        // By either of an alias's names (a ChromaKey's `spill` is its `soft_edge`).
        let stored = klipp_graph_lift::names::anim_key(klipp_graph_lift::effect_params::tag(node).unwrap_or(""), key.as_str());
        return node.anim.get(&ParamKey::from(stored)).cloned();
    }
    let tracks = node.legacy.as_ref()?.keyframes.as_ref()?.get("tracks")?.as_array()?;
    let track = tracks.iter().find(|t| t.get("param_name").and_then(serde_json::Value::as_str) == Some(key.as_str()))?;
    let keys = track.get("keyframes")?.as_array()?.clone();
    (!keys.is_empty()).then_some(klipp_graph_format::Animation { keys, modifiers: None })
}

/// `anim`'s keys as keyframes.
fn decoded_keys(anim: &klipp_graph_format::Animation, key: &ParamKey) -> Result<Vec<klipp_core::Keyframe>, String> {
    anim.keys.iter().map(|k| serde_json::from_value(k.clone()).map_err(|e| format!("a key of {key}: {e}"))).collect()
}

/// `keys` mirrored in time over their own span, each key's handles
/// swapped and turned: Reverse Keyframes on any track. Fewer than two
/// keys stay as they are.
pub(crate) fn reversed_keys(mut keys: Vec<klipp_core::Keyframe>) -> Vec<klipp_core::Keyframe> {
    if keys.len() < 2 {
        return keys;
    }
    let max_time = keys.iter().map(|k| k.time_secs).fold(f64::MIN, f64::max);
    for kf in keys.iter_mut() {
        kf.time_secs = max_time - kf.time_secs;
        let (old_in, old_out) = (kf.tangent_in, kf.tangent_out);
        kf.tangent_in = (-old_out.0, old_out.1);
        kf.tangent_out = (-old_in.0, old_in.1);
    }
    keys.sort_by(|a, b| a.time_secs.total_cmp(&b.time_secs));
    keys
}

/// The clipboard's keys scaled from the clip they were copied from to one
/// `clip_dur` long: Paste Keyframes on any track.
pub(crate) fn pasted_keys(cb: &crate::state::KeyframeClipboard, clip_dur: f64) -> Vec<klipp_core::Keyframe> {
    let scale = if cb.source_duration_secs > 0.0 && clip_dur > 0.0 { clip_dur / cb.source_duration_secs } else { 1.0 };
    let mut keys: Vec<klipp_core::Keyframe> = cb.keyframes.iter().map(|k| klipp_core::Keyframe { time_secs: k.time_secs * scale, ..k.clone() }).collect();
    keys.sort_by(|a, b| a.time_secs.total_cmp(&b.time_secs));
    keys
}

impl crate::state::AppState {
    /// The node's implicit blend, distinct from an effect parameter named `mix`.
    pub(crate) const NODE_MIX_DRAG_KEY: &'static str = "@mix";

    /// Where the nodes of `clip_id`'s grade at `descent` (the compounds it
    /// is inside, outermost first) are by path (#1476): a clip's under its
    /// grade group, a sequence grade's, edited through its stand-in, at its
    /// own root.
    pub(super) fn grade_level(&self, clip_id: ClipId, descent: &[klipp_nodes::NodeId]) -> Result<NodePath, String> {
        let seq = &self.sequences[self.active_sequence_idx];
        if let Some(which) = super::timeline_grade::GradeStandIn::of(seq, clip_id) {
            return Ok(NodePath::new(which.root(seq), descent.iter().copied()));
        }
        let clip = seq.all_clips().find(|c| c.id == clip_id).ok_or("no such clip")?;
        let picture = clip.picture_v2()?;
        let group = picture.picture.graph.nodes.iter().find(|n| n.type_id.as_str() == klipp_graph_format::lifted::types::GROUP).ok_or("the clip has no grade")?.id;
        Ok(NodePath::new(Root::Clip { clip: clip_id.0 }, std::iter::once(group).chain(descent.iter().copied())))
    }

    /// The lifted body at `level` ([`Self::grade_level`]).
    fn level_body(&self, level: &NodePath) -> Result<klipp_graph_format::Graph, String> {
        if level.nodes.is_empty() {
            return self.root_grade_doc(level.root);
        }
        self.node_at(level)?.group.map(|g| g.graph).ok_or_else(|| "that isn't a compound".to_string())
    }

    /// The ops of a structure edit on `clip_id`'s grade at `descent`, as
    /// the grade canvas makes it (#1476), and the step's name: each one
    /// path-op batch, so one undo step at any depth, on a clip's grade and
    /// on a sequence grade's (through its stand-in).
    pub(super) fn grade_structure_ops(&self, clip_id: ClipId, descent: &[klipp_nodes::NodeId], edit: GradeStructure) -> Result<(Vec<GraphOp>, &'static str), String> {
        let level = self.grade_level(clip_id, descent)?;
        let at = |node| NodePath::new(level.root, level.nodes.iter().copied().chain([node]));
        Ok(match edit {
            // Unwired, as the canvas adds one: beside the level's output,
            // reading what an unwired node reads (the frame into its
            // primary, and the like), placed where it was dropped.
            GradeStructure::Add { kind, id, position } => {
                let body = self.level_body(&level)?;
                let node = crate::inspector::grade_view::new_grade_node(kind, id);
                let mut ops = vec![GraphOp::AddNodeBeside { at: at(body.output), node: Box::new(node.clone()) }];
                for e in klipp_graph_lift::structure::inputs_of_unwired(&body, &node) {
                    ops.push(GraphOp::Connect { from: at(e.from.0), from_port: e.from.1, to: at(id), to_port: e.to.1 });
                }
                let place = serde_json::to_value(klipp_nodes::NodePos { x: position.0, y: position.1 }).map_err(|e| e.to_string())?;
                ops.push(GraphOp::SetNodeMeta { path: at(id), label: None, position: Some(place) });
                (ops, "Add Node")
            }
            GradeStructure::Remove { node } => (vec![GraphOp::RemoveNode { path: at(node) }], "Remove Node"),
            #[cfg(test)]
            GradeStructure::Reorder { node, after } => (vec![GraphOp::MoveAfter { path: at(node), after }], "Move Node"),
            GradeStructure::Group { nodes, group } => {
                let first = *nodes.first().ok_or("Group needs two nodes or more")?;
                (vec![GraphOp::Group { path: at(first), nodes, group }], "Group into Compound")
            }
            GradeStructure::Ungroup { node } => (vec![GraphOp::Ungroup { path: at(node) }], "Ungroup Compound"),
        })
    }

    /// The structure edit a grade canvas's command makes (#1476), and where:
    /// the clip it names, the level (the canvas's descent; a node the
    /// sender names goes to the grade's top, a removal to the level holding
    /// it) and the edit. `None` for any other command.
    pub(super) fn canvas_structure(&self, cmd: &AppCommand) -> Option<(ClipId, Vec<klipp_nodes::NodeId>, GradeStructure)> {
        match cmd {
            AppCommand::AddNode { clip_id, kind, node_id, position } => {
                let kind = klipp_nodes::kinds::addable_node_kind(kind)?;
                let id = node_id.unwrap_or_else(klipp_nodes::NodeId::new_v4);
                Some((*clip_id, super::colour::add_node_path(*node_id, &self.graph_descent_path), GradeStructure::Add { kind, id, position: *position }))
            }
            AppCommand::RemoveNode { clip_id, node_id } => {
                let seq = &self.sequences[self.active_sequence_idx];
                let grade = seq.all_clips().find(|c| c.id == *clip_id)?.grade();
                let descent = super::colour::graph_path_for(grade, &[*node_id], &self.graph_descent_path)?;
                Some((*clip_id, descent, GradeStructure::Remove { node: *node_id }))
            }
            AppCommand::GroupIntoCompound { clip_id, node_ids } => {
                let mut nodes: Vec<klipp_nodes::NodeId> = Vec::new();
                for id in node_ids {
                    if !nodes.contains(id) {
                        nodes.push(*id);
                    }
                }
                (nodes.len() >= 2).then(|| (*clip_id, self.graph_descent_path.clone(), GradeStructure::Group { nodes, group: klipp_nodes::NodeId::new_v4() }))
            }
            AppCommand::UngroupCompound { clip_id, node_id } => Some((*clip_id, self.graph_descent_path.clone(), GradeStructure::Ungroup { node: *node_id })),
            _ => None,
        }
    }

    /// A grade canvas's node drag (`MoveNode`, a frame's movement) placed
    /// by path at the level the canvas shows (#1528): `None` for any other
    /// command, else whether the grade changed. Inside the canvas's layout
    /// gesture it joins that gesture's one step.
    pub(super) fn canvas_move(&mut self, cmd: &AppCommand) -> Option<bool> {
        let AppCommand::MoveNode { clip_id, node_id, delta_x, delta_y } = cmd else { return None };
        let descent = self.graph_descent_path.clone();
        let level = match self.grade_level(*clip_id, &descent) {
            Ok(level) => level,
            Err(why) => {
                self.toast.show(why);
                return Some(false);
            }
        };
        let at = self.level_body(&level).ok().and_then(|b| b.ui.positions.get(node_id).cloned());
        let coord = |k: &str| at.as_ref().and_then(|p| p.get(k)).and_then(serde_json::Value::as_f64).unwrap_or(0.0) as f32;
        let position = (coord("x") + delta_x, coord("y") + delta_y);
        let addr = NodePath::new(level.root, level.nodes.iter().copied().chain([*node_id]));
        self.path_command(&AppCommand::SetNodePositionAt { addr, position })
    }

    /// A grade canvas's Auto Layout (`AutoLayoutNodeGraph`) of the level it
    /// shows, placed by path as one step (#1528): `None` for any other
    /// command, else whether the grade changed.
    pub(super) fn canvas_auto_layout(&mut self, cmd: &AppCommand) -> Option<bool> {
        let AppCommand::AutoLayoutNodeGraph { clip_id } = cmd else { return None };
        let descent = self.graph_descent_path.clone();
        let laid = || -> Result<Vec<GraphOp>, String> {
            let level = self.grade_level(*clip_id, &descent)?;
            let seq = &self.sequences[self.active_sequence_idx];
            let grade = match level.root {
                Root::Clip { .. } => seq.all_clips().find(|c| c.id == *clip_id).ok_or("no such clip")?.grade(),
                root => match seq.root_grade(root)? {
                    Some(g) => g,
                    None => &self.root_grade_start.as_ref().filter(|(r, _)| *r == root).ok_or("this grade doesn't exist yet")?.1,
                },
            };
            let shown = crate::inspector::grade_view::v1_level(grade, &descent).ok_or("Auto-layout: invalid descent path")?;
            let mut placed: Vec<_> = klipp_nodes::auto_layout(shown).into_iter().collect();
            placed.sort_by_key(|(id, _)| shown.nodes.iter().position(|n| n.id == *id));
            placed
                .into_iter()
                .map(|(id, p)| {
                    let position = serde_json::to_value(p).map_err(|e| e.to_string())?;
                    Ok(GraphOp::SetNodeMeta { path: NodePath::new(level.root, level.nodes.iter().copied().chain([id])), label: None, position: Some(position) })
                })
                .collect()
        };
        let ops = match laid() {
            Ok(ops) if ops.is_empty() => return Some(false),
            Ok(ops) => ops,
            Err(why) => {
                self.toast.show(why);
                return Some(false);
            }
        };
        match self.apply_path_ops(&ops, "Auto Layout") {
            Ok(()) => {
                self.toast.show("Auto-layout applied (Ctrl+Z to revert)");
                Some(true)
            }
            Err(why) => {
                tracing::warn!(%why, "auto layout refused");
                self.toast.show(why);
                Some(false)
            }
        }
    }

    /// A grade canvas's structure command (#1476) applied as its path ops,
    /// one step: `None` when `cmd` isn't one, else whether the grade
    /// changed. The selection follows a Group (the new compound) and an
    /// Ungroup (the nodes it held).
    pub(super) fn canvas_structure_edit(&mut self, cmd: &AppCommand) -> Option<bool> {
        let (clip_id, descent, edit) = self.canvas_structure(cmd)?;
        let unpacked: Vec<klipp_nodes::NodeId> = match &edit {
            GradeStructure::Ungroup { node } => self.sequences[self.active_sequence_idx]
                .all_clips()
                .find(|c| c.id == clip_id)
                .and_then(|c| crate::inspector::grade_view::v1_level(c.grade(), &descent).and_then(|g| g.nodes.iter().find(|n| n.id == *node)).and_then(|n| n.params.compound_subgraph.clone()))
                .map(|sub| sub.nodes.iter().filter(|n| !matches!(n.kind, klipp_nodes::NodeKind::Source | klipp_nodes::NodeKind::Output)).map(|n| n.id).collect())
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        let select = match &edit {
            GradeStructure::Group { group, .. } => Some(vec![*group]),
            GradeStructure::Ungroup { .. } => Some(unpacked),
            _ => None,
        };
        if !self.grade_structure_edit(clip_id, &descent, edit) {
            return Some(false);
        }
        if let Some(select) = select {
            self.selected_node_ids.clear();
            self.selected_node_ids.extend(select.iter().copied());
            self.selected_node_id = select.first().copied();
        }
        Some(true)
    }

    /// [`Self::grade_structure_ops`] applied: whether the grade changed. A
    /// refusal is said, and changes nothing.
    pub(super) fn grade_structure_edit(&mut self, clip_id: ClipId, descent: &[klipp_nodes::NodeId], edit: GradeStructure) -> bool {
        match self.grade_structure_ops(clip_id, descent, edit) {
            Ok((ops, label)) => match self.apply_path_ops(&ops, label) {
                Ok(()) => true,
                Err(why) => {
                    self.toast.show(why);
                    false
                }
            },
            Err(why) => {
                self.toast.show(why);
                false
            }
        }
    }

    /// Run a path-addressed command on the state: `None` when `cmd` isn't
    /// one, else whether a picture changed (the engine wants the
    /// sequence). A refusal is said in a toast and changes nothing.
    pub(crate) fn path_command(&mut self, cmd: &AppCommand) -> Option<bool> {
        // A tick of a row whose drag an edit closed under it: the rest of
        // the gesture is one drag again.
        if let AppCommand::SetParamAt { addr, key, .. } = cmd {
            if self.resumes(addr, key) && !self.active_history().is_any_drag_in_progress() {
                let _ = self.begin_param_edit_at(addr, key);
            }
        }
        // A clip's keyframe command on its placement or opacity lane, as
        // the path edits it is (M4 step 12c).
        if let Some(result) = self.lane_command(cmd) {
            return match result {
                Ok(changed) => Some(changed),
                Err(why) => {
                    tracing::warn!(%why, "lane edit refused");
                    self.toast.show(why);
                    Some(false)
                }
            };
        }
        let result = match cmd {
            AppCommand::SetNodePositionAt { position, .. } if !position.0.is_finite() || !position.1.is_finite() => Err("Node positions must be finite".into()),
            AppCommand::BeginNodeLayoutEdit { root } => self.begin_node_layout_edit(self.layout_root(*root)).map(|()| false),
            AppCommand::EndNodeLayoutEdit { root } => {
                self.end_node_layout_edit(self.layout_root(*root));
                Ok(false)
            }
            AppCommand::AbandonNodeLayoutEdit { root } => Ok(self.abandon_node_layout_edit(self.layout_root(*root))),
            AppCommand::BeginParamEditAt { addr, key } => self.begin_param_edit_at(addr, key).map(|()| false),
            AppCommand::EndParamEditAt { addr, key } => {
                self.end_param_edit_at(addr, key);
                Ok(false)
            }
            AppCommand::AbandonParamEditAt { addr, key } => Ok(self.abandon_param_edit_at(addr, key)),
            AppCommand::ToggleGraphDock => { self.toggle_graph_dock(); Ok(false) },
            AppCommand::SetNodeColourLabel { node, colour } => Ok(self.set_node_colour_label(node, *colour)),
            AppCommand::RevealGraph { path, select } => self.reveal_graph(path, *select).map(|()| false),
            AppCommand::SetGraphDockRoot { root } => self.set_graph_dock_root(*root).map(|()| false),
            AppCommand::SetSplitCompareNode { clip_id, path } => self.set_split_compare_node(*clip_id, path.as_ref()).map(|()| false),
            AppCommand::MoveKeyAt { addr, key, time_secs, new_time_secs, value } => self
                .key_edit(addr, key, *time_secs, |k| {
                    k["time_secs"] = serde_json::json!(new_time_secs);
                    k["value"] = serde_json::json!(value);
                })
                .and_then(|ops| self.apply_path_ops(&ops, "Move Keyframe"))
                .map(|()| true),
            AppCommand::SetInterpKeyAt { addr, key, time_secs, interpolation } => self
                .interpolation_edit(addr, key, *time_secs, *interpolation)
                .and_then(|ops| self.apply_path_ops(&ops, "Keyframe Interpolation"))
                .map(|()| true),
            AppCommand::EaseKeyAt { addr, key, time_secs, ease } => self
                .ease_edit(addr, key, *time_secs, *ease)
                .and_then(|ops| self.apply_path_ops(&ops, "Ease Keyframe"))
                .map(|()| true),
            AppCommand::CopyKeysAt { addr, key } => self.node_at(addr).and_then(|node| {
                let keys = animation_of(&node, key).map_or(Ok(Vec::new()), |a| decoded_keys(&a, key))?;
                self.keyframe_clipboard = Some(crate::state::KeyframeClipboard { keyframes: keys, source_duration_secs: self.clip_secs_at(addr) });
                Ok(false)
            }),
            AppCommand::PasteKeysAt { addr, key } => match self.keyframe_clipboard.clone() {
                None => Ok(false),
                Some(cb) => {
                    let clip_dur = self.clip_secs_at(addr);
                    self.lane_edit(addr, key, |_| pasted_keys(&cb, clip_dur)).and_then(|ops| self.apply_path_ops(&ops, "Paste Keyframes")).map(|()| true)
                }
            },
            AppCommand::ReverseKeysAt { addr, key } => self.lane_edit(addr, key, reversed_keys).and_then(|ops| self.apply_path_ops(&ops, "Reverse Keyframes")).map(|()| true),
            _ => {
                let (ops, label) = path_ops(cmd)?;
                self.with_pending_corrector(ops).and_then(|ops| self.apply_path_ops(&ops, label)).map(|()| true)
            }
        };
        match result {
            Ok(changed) => Some(changed),
            Err(why) => {
                tracing::warn!(%why, "path edit refused");
                self.toast.show(why);
                Some(false)
            }
        }
    }

    /// A clip's keyframe command on its placement or opacity lane (M4 step
    /// 12c): the timeline's, the Video tab's, the curve editor's and the
    /// shortcuts' `*VideoKeyframe` commands, and a key drag over such a
    /// lane, applied by path on the clip's placement node, each one step
    /// (a drag one step over its ticks). The lane's new keys are what the
    /// old lane edit made of the keys the picture holds, in its order, so
    /// a key moved or re-shaped keeps its place in the selection. `None`
    /// for any other command, a matte's lane, or a clip without a
    /// placement node: those keep the old route.
    fn lane_command(&mut self, cmd: &AppCommand) -> Option<Result<bool, String>> {
        use klipp_core::Keyframe;
        use AppCommand as C;
        let clip_id = match cmd {
            C::AddVideoKeyframe { clip_id, .. }
            | C::RemoveVideoKeyframe { clip_id, .. }
            | C::MoveVideoKeyframe { clip_id, .. }
            | C::SetVideoKeyframeInterpolation { clip_id, .. }
            | C::SetVideoKeyframeTangent { clip_id, .. }
            | C::ScaleVideoKeyframeTiming { clip_id, .. }
            | C::InsertVideoKeyframeBump { clip_id, .. }
            | C::ClearVideoParamKeyframes { clip_id, .. }
            | C::CopyVideoKeyframes { clip_id, .. }
            | C::PasteVideoKeyframes { clip_id, .. }
            | C::ReverseVideoKeyframes { clip_id, .. }
            | C::ApplyKenBurns { clip_id, .. }
            | C::ClearTransformKeyframes { clip_id }
            | C::BeginKeyframeDrag { clip_id, .. }
            | C::EndKeyframeDrag { clip_id, .. } => *clip_id,
            _ => return None,
        };
        let clip = self.sequences[self.active_sequence_idx].all_clips().find(|c| c.id == clip_id)?.clone();
        let addr = clip.placement_path()?;
        let lane = |param: &klipp_core::VideoParam| crate::inspector::placement_by_path::lane_key(*param);
        // The whole-lane ops: one batch, so one step.
        let whole = |state: &mut Self, ops: Vec<GraphOp>, label: &str| Some(state.apply_path_ops(&ops, label).map(|()| true));
        match cmd {
            C::BeginKeyframeDrag { key, .. } => {
                let k = drag_lane(key).and_then(|p| lane(&p))?;
                return Some(self.begin_param_edit_at(&addr, &ParamKey::from(k)).map(|()| false));
            }
            C::EndKeyframeDrag { key, .. } => {
                let k = drag_lane(key).and_then(|p| lane(&p))?;
                self.end_param_edit_at(&addr, &ParamKey::from(k));
                return Some(Ok(false));
            }
            C::ApplyKenBurns { start_scale, end_scale, start_pos_x, start_pos_y, end_pos_x, end_pos_y, .. } => {
                let duration = clip.source_out.to_secs_f64() - clip.source_in.to_secs_f64();
                let ease = 0.42 * duration;
                let pair = |a: f32, b: f32| {
                    vec![
                        Keyframe { time_secs: 0.0, value: a, interpolation: klipp_core::Interpolation::Bezier, tangent_out: (ease, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() },
                        Keyframe { time_secs: duration, value: b, interpolation: klipp_core::Interpolation::Bezier, tangent_out: (0.0, 0.0), tangent_in: (-ease, 0.0), handles: Default::default() },
                    ]
                };
                let ops = vec![set_lane(&clip, &addr, "scale", pair(*start_scale, *end_scale)), set_lane(&clip, &addr, "pos_x", pair(*start_pos_x, *end_pos_x)), set_lane(&clip, &addr, "pos_y", pair(*start_pos_y, *end_pos_y))];
                return whole(self, ops, "Apply Ken Burns");
            }
            C::ClearTransformKeyframes { .. } => {
                let ops = ["scale", "pos_x", "pos_y", "rotation"].into_iter().map(|k| set_lane(&clip, &addr, k, Vec::new())).collect();
                return whole(self, ops, "Clear Transform Keyframes");
            }
            _ => {}
        }
        let param = match cmd {
            C::AddVideoKeyframe { param, .. }
            | C::RemoveVideoKeyframe { param, .. }
            | C::MoveVideoKeyframe { param, .. }
            | C::SetVideoKeyframeInterpolation { param, .. }
            | C::SetVideoKeyframeTangent { param, .. }
            | C::ScaleVideoKeyframeTiming { param, .. }
            | C::InsertVideoKeyframeBump { param, .. }
            | C::ClearVideoParamKeyframes { param, .. }
            | C::CopyVideoKeyframes { param, .. }
            | C::PasteVideoKeyframes { param, .. }
            | C::ReverseVideoKeyframes { param, .. } => *param,
            _ => return None,
        };
        let key = lane(&param)?;
        let mut keys = crate::inspector::placement_by_path::lane_keys(&clip, key);
        let sort = |keys: &mut Vec<Keyframe>| keys.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
        let clip_dur = clip.duration().to_secs_f64();
        let (keys, label) = match cmd {
            // A key put on or taken off: the path's own key edits.
            C::AddVideoKeyframe { time_secs, value, .. } => {
                let k = klipp_core::keyframe::upserted(&keys, *time_secs, *value);
                return Some(match serde_json::to_value(k) {
                    Ok(keyframe) => self.path_command_result(&C::AddKeyAt { addr: addr.clone(), key: key.into(), keyframe }),
                    Err(e) => Err(e.to_string()),
                });
            }
            C::RemoveVideoKeyframe { time_secs, .. } => return Some(self.path_command_result(&C::RemoveKeyAt { addr: addr.clone(), key: key.into(), time_secs: *time_secs })),
            C::CopyVideoKeyframes { .. } => {
                self.keyframe_clipboard = Some(crate::state::KeyframeClipboard { keyframes: keys, source_duration_secs: clip_dur });
                return Some(Ok(false));
            }
            C::MoveVideoKeyframe { index, new_time_secs, new_value, .. } => {
                if let Some(kf) = keys.get_mut(*index) {
                    kf.time_secs = *new_time_secs;
                    kf.value = *new_value;
                }
                sort(&mut keys);
                (keys, "Move Keyframe")
            }
            C::SetVideoKeyframeInterpolation { keyframe_index, interpolation, .. } => {
                klipp_core::keyframe::set_interpolation_at(&mut keys, *keyframe_index, *interpolation);
                (keys, "Set Keyframe Interpolation")
            }
            C::SetVideoKeyframeTangent { keyframe_index, tangent_in, tangent_out, .. } => {
                if let Some(kf) = keys.get_mut(*keyframe_index) {
                    if let Some(ti) = tangent_in {
                        kf.tangent_in = *ti;
                    }
                    if let Some(to) = tangent_out {
                        kf.tangent_out = *to;
                    }
                }
                (keys, "Set Keyframe Tangent")
            }
            C::ScaleVideoKeyframeTiming { factor, .. } => {
                for kf in keys.iter_mut() {
                    kf.time_secs *= factor;
                    kf.tangent_in.0 *= *factor;
                    kf.tangent_out.0 *= *factor;
                }
                keys.retain(|kf| kf.time_secs >= 0.0 && kf.time_secs <= clip_dur);
                sort(&mut keys);
                (keys, "Scale Video Keyframe Timing")
            }
            C::InsertVideoKeyframeBump { center_time, bump_value, base_value, duration_secs, ramp_secs, .. } => {
                let half = duration_secs / 2.0;
                for (t, v) in [(center_time - half - ramp_secs, *base_value), (center_time - half, *bump_value), (center_time + half, *bump_value), (center_time + half + ramp_secs, *base_value)] {
                    if t >= 0.0 && t <= clip_dur {
                        keys.push(Keyframe { time_secs: t, value: v, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() });
                    }
                }
                sort(&mut keys);
                (keys, "Insert Video Keyframe Bump")
            }
            C::ClearVideoParamKeyframes { .. } => (Vec::new(), "Clear Keyframes"),
            C::PasteVideoKeyframes { .. } => {
                let Some(cb) = self.keyframe_clipboard.clone() else { return Some(Ok(false)) };
                (pasted_keys(&cb, clip_dur), "Paste Video Keyframes")
            }
            C::ReverseVideoKeyframes { .. } => (reversed_keys(keys), "Reverse Video Keyframes"),
            _ => return None,
        };
        let op = set_lane(&clip, &addr, key, keys);
        // Inside an open drag of this lane (a key or a handle dragged), the
        // tick is the drag's, written live.
        if self.active_history().is_dragging(&drag_key(&addr, &ParamKey::from(key))) {
            return Some(self.apply_live(op));
        }
        whole(self, vec![op], label)
    }

    /// The placement or opacity lane a keyframe command on one lane edits
    /// (or a key drag over one names), by its node path and key.
    fn lane_target(&self, cmd: &AppCommand) -> Option<(NodePath, ParamKey)> {
        use AppCommand as C;
        let (clip_id, param) = match cmd {
            C::AddVideoKeyframe { clip_id, param, .. }
            | C::RemoveVideoKeyframe { clip_id, param, .. }
            | C::MoveVideoKeyframe { clip_id, param, .. }
            | C::SetVideoKeyframeInterpolation { clip_id, param, .. }
            | C::SetVideoKeyframeTangent { clip_id, param, .. } => (*clip_id, *param),
            C::BeginKeyframeDrag { clip_id, key } | C::EndKeyframeDrag { clip_id, key } => (*clip_id, drag_lane(key)?),
            _ => return None,
        };
        let key = crate::inspector::placement_by_path::lane_key(param)?;
        let clip = self.sequences[self.active_sequence_idx].all_clips().find(|c| c.id == clip_id)?;
        Some((clip.placement_path()?, ParamKey::from(key)))
    }

    /// Whole placement or opacity lanes of `clip_id` set as one step (M4
    /// step 12c): for a writer that lays a lane's keys down itself (a
    /// motion preset, a tracker's positions). Each `(key, keys)` replaces
    /// that lane, its modifier stack kept.
    pub(super) fn set_lanes(&mut self, clip_id: ClipId, lanes: &[(&str, Vec<klipp_core::Keyframe>)], label: &str) -> Result<(), String> {
        let clip = self.sequences[self.active_sequence_idx].all_clips().find(|c| c.id == clip_id).ok_or("no such clip")?;
        let addr = clip.placement_path().ok_or("this clip has no placement to key")?;
        let ops: Vec<GraphOp> = lanes.iter().map(|(key, keys)| set_lane(clip, &addr, key, keys.clone())).collect();
        self.apply_path_ops(&ops, label)
    }

    /// A path command applied as `path_command` applies it, its refusal
    /// handed back rather than said, for `lane_command` to say once.
    fn path_command_result(&mut self, cmd: &AppCommand) -> Result<bool, String> {
        let (ops, label) = path_ops(cmd).ok_or("not a path edit")?;
        self.apply_path_ops(&ops, label).map(|()| true)
    }

    /// One op written now, inside the open drag that will record it.
    fn apply_live(&mut self, op: GraphOp) -> Result<bool, String> {
        let graph_ops::GraphOpsOutcome { edit, .. } = graph_ops::graph_ops_edit(&self.sequences[self.active_sequence_idx], &[op]).map_err(|(_, why)| why)?;
        if let EditCommand::PictureEdit { clip_id, edit, .. } = edit {
            self.set_picture(clip_id, &edit.after);
        }
        Ok(true)
    }

    /// Whether `cmd`, arriving while a path drag is open, closes it first.
    /// Only a command that can write the picture, the saved project or the
    /// history closes it, so that edit is its own step and never folded
    /// into the drag's; a command that writes none of them (transport, a
    /// selection, the viewer and its scopes, a panel, a dialog, a job whose
    /// result arrives later) leaves the drag open.
    ///
    /// Which commands write is [`super::command_writes::writes`], where
    /// every command is named.
    pub(crate) fn closes_path_drag(&self, cmd: &AppCommand) -> bool {
        let Some(open) = self.active_history().pending_drag_key().filter(|k| k.starts_with(PATH_DRAG)) else { return false };
        // A key moved or re-shaped on a placement or opacity lane, or that
        // lane's drag opening or closing, is that lane's drag (M4 step 12c).
        if let Some((addr, key)) = self.lane_target(cmd) {
            return drag_key(&addr, &key) != open;
        }
        match cmd {
            AppCommand::BeginNodeLayoutEdit { root } | AppCommand::EndNodeLayoutEdit { root } | AppCommand::AbandonNodeLayoutEdit { root } => layout_drag(self.layout_root(*root)) != open,
            AppCommand::SetNodePositionAt { addr, .. } => layout_drag(addr.root) != open,
            // A node or a note dragged on the grade canvas is part of its
            // layout gesture (#1745): the dispatch closed the gesture on
            // each move, so a drag was a step a frame.
            AppCommand::MoveNode { clip_id, .. } | AppCommand::MoveStickyNote { clip_id, .. } => layout_drag(self.layout_root(Root::Clip { clip: clip_id.0 })) != open,
            // A node dropped on a wire: the drag that brought it there and
            // the drop are one step (#1744), on either canvas.
            AppCommand::SpliceNodeIntoWire { clip_id, .. } => layout_drag(self.layout_root(Root::Clip { clip: clip_id.0 })) != open,
            AppCommand::MoveNodeAt { addr, .. } => layout_drag(addr.root) != open,
            // A note moved on the picture canvas is part of its layout
            // gesture (#1743).
            AppCommand::SetAnnotationsAt { at, .. } => layout_drag(at.root) != open,
            AppCommand::BeginParamEditAt { addr, key } | AppCommand::EndParamEditAt { addr, key } | AppCommand::AbandonParamEditAt { addr, key } => drag_key(addr, key) != open,
            // An edit of the dragged parameter, its value or its keys, is
            // part of the drag; of any of the node's, in a drag over them all.
            AppCommand::SetParamAt { addr, key, .. } | AppCommand::MoveKeyAt { addr, key, .. } | AppCommand::AddKeyAt { addr, key, .. } | AppCommand::RemoveKeyAt { addr, key, .. } => {
                drag_key(addr, key) != open && node_drag(addr) != open
            }
            AppCommand::SetMixAt { addr, .. } => drag_key(addr, &Self::NODE_MIX_DRAG_KEY.into()) != open && node_drag(addr) != open,
            // A whole entry written (a colour dragged in its popup) is part
            // of a drag over the node's values.
            AppCommand::SetPayloadAt { addr, .. } => node_drag(addr) != open,
            _ => super::command_writes::writes(cmd),
        }
    }

    /// Close an open path drag before an edit applied outside the dispatch
    /// boundary (`apply_edits_as_one`: the control socket's edits and graph
    /// ops, edit plans, and the app's own whole edits). Such an edit is
    /// never part of a path drag, whose steps are written live, so any
    /// open one is closed, as its own step, before the edit is applied.
    ///
    /// The row may still be held: its next tick reopens the drag
    /// (`resumable_path_drag`), so the gesture is two steps, the part
    /// before the edit and the part after, never one per tick.
    pub(crate) fn close_path_drag_before_edit(&mut self) {
        let Some(open) = self.active_history().pending_drag_key().filter(|k| k.starts_with(PATH_DRAG)).map(str::to_owned) else { return };
        self.close_open_drag();
        self.resumable_path_drag = Some((open, self.selected_clip));
    }

    /// Forget a drag a result closed (`resumable_path_drag`) once its
    /// gesture can't be going on, so a later edit of the row (a typed
    /// value, a reset) is its own step and reopens nothing: each frame,
    /// before the panels draw, when no button is held (a release the row
    /// missed, being hidden or collapsed, counts) or another clip is
    /// selected.
    pub(crate) fn forget_stale_path_drag(&mut self, pointer_down: bool) {
        if self.resumable_path_drag.as_ref().is_some_and(|(_, selected)| !pointer_down || *selected != self.selected_clip) {
            self.resumable_path_drag = None;
        }
    }

    fn resumes(&self, addr: &NodePath, key: &ParamKey) -> bool {
        self.resumable_path_drag.as_ref().is_some_and(|(open, _)| *open == drag_key(addr, key))
    }

    /// The dispatch boundary every command passes (M3 step 2): an open path
    /// drag that `cmd` doesn't belong to is closed as its own step before
    /// any handler runs.
    pub(crate) fn before_dispatch(&mut self, cmd: &AppCommand) {
        if super::command_writes::writes(cmd) { self.invalidate_node_thumbnails(); }
        // Any other write ends a drag a result closed: only its own row's
        // next tick resumes it.
        let resuming = matches!(cmd, AppCommand::SetParamAt { addr, key, .. } if self.resumes(addr, key));
        if super::command_writes::writes(cmd) && !resuming {
            self.resumable_path_drag = None;
        }
        if self.closes_path_drag(cmd) {
            // A path drag closed by a write that isn't its own (a sampled
            // result, a command over the socket) is its own step, as
            // `close_path_drag_before_edit` closes one; its row may still be
            // held, and its next tick resumes it, so the rest of the drag is
            // one more step, never one per tick. Any other drag just closes.
            self.close_path_drag_before_edit();
            self.close_open_drag();
        }
    }

    /// `ops` as one undo step named `label`, or live inside the open drag
    /// of the one parameter they set (or of its node's, in a drag over them
    /// all): a value, or (a key dragged on a curve, M3 step 3) its
    /// animation, or a key added or taken off. Any other edit closes an
    /// open drag first, as its own step.
    pub(crate) fn apply_path_ops(&mut self, ops: &[GraphOp], label: &str) -> Result<(), String> {
        if ops.first().is_some_and(|op| is_sequence_grade(op.path().root)) {
            return self.apply_root_path_ops(ops, label);
        }
        let refused = |(at, why): (Option<usize>, String)| match at {
            Some(i) if ops.len() > 1 => format!("{label}: op {i}: {why}"),
            _ => format!("{label}: {why}"),
        };
        // A node, or a note (#1743), placed inside an open layout gesture,
        // or a node moved in its chain by a drop on a wire (#1744).
        if let [GraphOp::SetNodeMeta { path, label: None, position: Some(_) } | GraphOp::SetAnnotations { path, .. } | GraphOp::MoveAfter { path, .. }] = ops {
            if self.active_history().is_dragging(&layout_drag(path.root)) {
                let graph_ops::GraphOpsOutcome { edit, .. } = graph_ops::graph_ops_edit(&self.sequences[self.active_sequence_idx], ops).map_err(refused)?;
                if let EditCommand::PictureEdit { clip_id, edit, .. } = edit { self.set_picture(clip_id, &edit.after); }
                return Ok(());
            }
        }
        let mix_key = Self::NODE_MIX_DRAG_KEY.into();
        let node_key = ParamKey::from("");
        // A first edit of the pending Corrector makes it first: the edit is
        // still the one it names, so a drag's first tick stays the drag's.
        let made = self.pending_corrector_ops(ops);
        let dragged = match &ops[made..] {
            [GraphOp::SetParam { path, key, .. } | GraphOp::SetAnim { path, key, .. } | GraphOp::AddKeyAt { path, key, .. } | GraphOp::RemoveKeyAt { path, key, .. }] => Some((path, key)),
            [GraphOp::SetMix { path, .. }] => Some((path, &mix_key)),
            // A whole entry, inside a drag over the node's values.
            [GraphOp::SetPayload { path, .. }] => Some((path, &node_key)),
            _ => None,
        };
        if let Some((path, key)) = dragged {
            if self.active_history().pending_drag_key().is_some_and(|open| open == drag_key(path, key) || open == node_drag(path)) {
                let graph_ops::GraphOpsOutcome { edit, .. } = graph_ops::graph_ops_edit(&self.sequences[self.active_sequence_idx], ops).map_err(refused)?;
                if let EditCommand::PictureEdit { clip_id, edit, .. } = edit {
                    self.set_picture(clip_id, &edit.after);
                }
                return Ok(());
            }
        }
        if self.active_history().is_any_drag_in_progress() {
            self.close_open_drag();
        }
        let graph_ops::GraphOpsOutcome { edit, ungated, .. } = graph_ops::graph_ops_edit(&self.sequences[self.active_sequence_idx], ops).map_err(refused)?;
        if let EditCommand::PictureEdit { clip_id, edit, .. } = &edit {
            self.set_picture(*clip_id, &edit.after);
        }
        // One step named `label`, or one edit of a batch already open (a
        // look's chain, an agent's action), which then names the step:
        // opening a batch of its own here would drop what that one held.
        self.push_history(EditCommand::Named { label: label.to_string(), edit: Box::new(edit) });
        self.show_ungated(&ungated);
        Ok(())
    }

    /// [`AppState::apply_path_ops`] on one of the sequence's own grades
    /// (M3 step 19): applied to the stored grade (or, while it has none,
    /// to the graph its stand-in drew), live inside the open drag of the
    /// one parameter it sets, else as one step that undoes to the grade as
    /// it was, `None` included.
    fn apply_root_path_ops(&mut self, ops: &[GraphOp], label: &str) -> Result<(), String> {
        let root = ops[0].path().root;
        let start = self.root_grade_start.as_ref().filter(|(r, _)| *r == root).map(|(_, g)| g.clone());
        // A node placed inside an open layout gesture: the gesture's step
        // holds it (#1528).
        if matches!(ops, [GraphOp::SetNodeMeta { label: None, position: Some(_), .. }]) && self.active_history().is_dragging(&layout_drag(root)) {
            self.sequences[self.active_sequence_idx].apply_root_grade_ops(ops, start.as_ref())?;
            self.dirty = true;
            return Ok(());
        }
        let mix_key: ParamKey = Self::NODE_MIX_DRAG_KEY.into();
        let node_key = ParamKey::from("");
        let made = self.pending_corrector_ops(ops);
        let dragged = match &ops[made..] {
            [GraphOp::SetParam { path, key, .. } | GraphOp::SetAnim { path, key, .. } | GraphOp::AddKeyAt { path, key, .. } | GraphOp::RemoveKeyAt { path, key, .. }] => Some((path, key)),
            [GraphOp::SetMix { path, .. }] => Some((path, &mix_key)),
            // A whole entry, inside a drag over the node's values.
            [GraphOp::SetPayload { path, .. }] => Some((path, &node_key)),
            _ => None,
        };
        let live = dragged.is_some_and(|(path, key)| self.active_history().pending_drag_key().is_some_and(|open| open == drag_key(path, key) || open == node_drag(path)));
        if !live && self.active_history().is_any_drag_in_progress() {
            self.close_open_drag();
        }
        let (old, new) = self.sequences[self.active_sequence_idx].apply_root_grade_ops(ops, start.as_ref())?;
        self.dirty = true;
        if live {
            return Ok(());
        }
        // Named as `apply_path_ops` names its step, inside any open batch.
        let edit = EditCommand::SetRootGrade { description: "Edit Grade", root, old: old.map(Box::new), new: Some(Box::new(new)) };
        self.push_history(EditCommand::Named { label: label.to_string(), edit: Box::new(edit) });
        Ok(())
    }

    /// Write `picture` to the clip, with no history entry of its own.
    pub(super) fn set_picture(&mut self, clip_id: ClipId, picture: &klipp_timeline::ClipPictureFields) {
        let seq = &mut self.sequences[self.active_sequence_idx];
        if let Some(clip) = seq.tracks.iter_mut().find_map(|t| t.find_clip_mut(clip_id)) {
            clip.restore_picture(picture);
            self.dirty = true;
            self.media_usage_cache_dirty = true;
        }
    }

    /// The root a layout gesture names: a sequence grade's stand-in (the
    /// clip the grade canvas edits it through) stands for its own root
    /// (#1528).
    fn layout_root(&self, root: Root) -> Root {
        let seq = &self.sequences[self.active_sequence_idx];
        match root {
            Root::Clip { clip } => super::timeline_grade::GradeStandIn::of(seq, ClipId(clip)).map_or(root, |which| which.root(seq)),
            _ => root,
        }
    }

    /// Whether a layout gesture is open on the canvas `clip_id`'s grade
    /// is edited through (#1745): an edit inside it is the gesture's step.
    pub(super) fn in_layout_gesture(&self, clip_id: ClipId) -> bool {
        self.active_history().is_dragging(&layout_drag(self.layout_root(Root::Clip { clip: clip_id.0 })))
    }

    fn begin_node_layout_edit(&mut self, root: Root) -> Result<(), String> {
        // A sequence grade's canvas: one step over its grade (#1528).
        if is_sequence_grade(root) {
            let before = self.sequences[self.active_sequence_idx].root_grade(root)?.cloned().map(Box::new);
            if self.active_history().is_any_drag_in_progress() { self.close_open_drag(); }
            self.active_history_mut().begin_drag(layout_drag(root), HistoryEntry {
                command: EditCommand::SetRootGrade { description: "Move Nodes", root, old: before.clone(), new: before },
            });
            return Ok(());
        }
        let Root::Clip { clip } = root else { return Err("Only a clip's canvas can be arranged".into()) };
        let clip_id = ClipId(clip);
        let before = self.picture_of(clip_id).ok_or_else(|| format!("No clip {clip} on this sequence"))?;
        if self.active_history().is_any_drag_in_progress() { self.close_open_drag(); }
        self.active_history_mut().begin_drag(layout_drag(root), HistoryEntry {
            command: EditCommand::PictureEdit { clip_id, description: "Move Nodes", edit: Box::new(klipp_timeline::PictureEdit { before: before.clone(), after: before }) },
        });
        Ok(())
    }

    fn end_node_layout_edit(&mut self, root: Root) {
        if !self.active_history().is_dragging(&layout_drag(root)) { return; }
        let unchanged = match self.active_history().pending_old_entry_ref().map(|e| &e.command) {
            Some(EditCommand::PictureEdit { clip_id, edit, .. }) => self.sequence().all_clips().find(|c| c.id == *clip_id).is_some_and(|clip| {
                let Ok(now) = clip.picture_v2() else { return false };
                let mut before = clip.clone();
                before.restore_picture(&edit.before);
                before.picture_v2().is_ok_and(|picture| picture == now)
            }),
            Some(EditCommand::SetRootGrade { root, old, .. }) => {
                let now = self.sequence().root_grade(*root).ok().flatten().map(|g| serde_json::to_value(g).ok());
                now == old.as_deref().map(|g| serde_json::to_value(g).ok())
            }
            _ => false,
        };
        if unchanged { self.active_history_mut().abandon_pending_drag(); } else { self.close_open_drag(); }
    }

    fn abandon_node_layout_edit(&mut self, root: Root) -> bool {
        if !self.active_history().is_dragging(&layout_drag(root)) { return false; }
        if let Some(EditCommand::SetRootGrade { root, old, .. }) = self.active_history().pending_old_entry_ref().map(|e| e.command.clone()) {
            self.active_history_mut().abandon_pending_drag();
            self.dirty = true;
            return self.sequences[self.active_sequence_idx].set_root_grade(root, old.map(|g| *g)).is_ok();
        }
        let before = match self.active_history().pending_old_entry_ref().map(|e| &e.command) {
            Some(EditCommand::PictureEdit { clip_id, edit, .. }) => Some((*clip_id, edit.before.clone())),
            _ => None,
        };
        self.active_history_mut().abandon_pending_drag();
        let Some((clip_id, before)) = before else { return false };
        self.set_picture(clip_id, &before);
        true
    }

    fn begin_param_edit_at(&mut self, addr: &NodePath, key: &ParamKey) -> Result<(), String> {
        self.resumable_path_drag = None;
        if self.active_history().is_any_drag_in_progress() {
            self.close_open_drag();
        }
        if !self.names_pending_corrector(addr) {
            self.node_at(addr)?;
        }
        if is_sequence_grade(addr.root) {
            let before = self.sequences[self.active_sequence_idx].root_grade(addr.root)?.cloned().map(Box::new);
            self.active_history_mut().begin_drag(
                drag_key(addr, key),
                HistoryEntry { command: EditCommand::SetRootGrade { description: "Edit Parameter", root: addr.root, old: before.clone(), new: before } },
            );
            return Ok(());
        }
        let Root::Clip { clip } = addr.root else { return Err("only a clip's graph takes edits by path so far".into()) };
        let clip_id = ClipId(clip);
        let before = self.picture_of(clip_id).ok_or_else(|| format!("no clip {clip} on this sequence"))?;
        self.active_history_mut().begin_drag(
            drag_key(addr, key),
            HistoryEntry {
                command: EditCommand::PictureEdit { clip_id, description: "Edit Parameter", edit: Box::new(klipp_timeline::PictureEdit { before: before.clone(), after: before }) },
            },
        );
        Ok(())
    }

    fn end_param_edit_at(&mut self, addr: &NodePath, key: &ParamKey) {
        if self.resumes(addr, key) {
            self.resumable_path_drag = None;
        }
        if !self.active_history().is_dragging(&drag_key(addr, key)) {
            return;
        }
        // The entry keeps the drag's own "before": closing it is closing
        // any open drag, from its start to the picture now.
        self.close_open_drag();
    }

    /// Take the open drag of `(addr, key)` back: the clip's picture as the
    /// drag found it, and no history entry. Whether a picture changed.
    fn abandon_param_edit_at(&mut self, addr: &NodePath, key: &ParamKey) -> bool {
        if self.resumes(addr, key) {
            self.resumable_path_drag = None;
        }
        if !self.active_history().is_dragging(&drag_key(addr, key)) {
            return false;
        }
        if let Some(EditCommand::SetRootGrade { root, old, .. }) = self.active_history().pending_old_entry_ref().map(|e| e.command.clone()) {
            self.active_history_mut().abandon_pending_drag();
            let _ = self.sequences[self.active_sequence_idx].set_root_grade(root, old.map(|g| *g));
            self.dirty = true;
            return true;
        }
        let before = match self.active_history().pending_old_entry_ref().map(|e| &e.command) {
            Some(EditCommand::PictureEdit { clip_id, edit, .. }) => Some((*clip_id, edit.before.clone())),
            _ => None,
        };
        self.active_history_mut().abandon_pending_drag();
        let Some((clip_id, before)) = before else { return false };
        self.set_picture(clip_id, &before);
        true
    }

    /// The one op that changes the key of `(addr, key)` at `time_secs` by
    /// `edit`: the parameter's whole animation, written back in time order.
    fn key_edit(&self, addr: &NodePath, key: &ParamKey, time_secs: f64, edit: impl FnOnce(&mut serde_json::Value)) -> Result<Vec<GraphOp>, String> {
        let node = self.node_at(addr)?;
        let mut anim = animation_of(&node, key).ok_or_else(|| format!("{key} isn't animated"))?;
        let time = |k: &serde_json::Value| k.get("time_secs").and_then(serde_json::Value::as_f64);
        let at = anim.keys.iter().position(|k| time(k).is_some_and(|t| (t - time_secs).abs() < 1e-9)).ok_or_else(|| format!("{key} has no key at {time_secs} s"))?;
        edit(&mut anim.keys[at]);
        // Keys are named by their time, so two may never share one: a key
        // moved onto another's time would make the next edit of either
        // ambiguous (M3 step 3).
        let moved = time(&anim.keys[at]).unwrap_or(time_secs);
        if anim.keys.iter().enumerate().any(|(i, k)| i != at && time(k).is_some_and(|t| (t - moved).abs() < 1e-9)) {
            return Err(format!("{key} has another key at {moved} s"));
        }
        anim.keys.sort_by(|a, b| time(a).unwrap_or(0.0).total_cmp(&time(b).unwrap_or(0.0)));
        Ok(vec![GraphOp::SetAnim { path: addr.clone(), key: key.clone(), anim: Some(anim) }])
    }

    /// The one op that sets the interpolation at the key of `(addr, key)` at
    /// `time_secs` as the keyframe editor sets it
    /// ([`klipp_core::keyframe::set_interpolation_at`]): that key and the
    /// one before it, with Bezier tangents where they have none.
    fn interpolation_edit(&self, addr: &NodePath, key: &ParamKey, time_secs: f64, interpolation: klipp_core::Interpolation) -> Result<Vec<GraphOp>, String> {
        let node = self.node_at(addr)?;
        let mut anim = animation_of(&node, key).ok_or_else(|| format!("{key} isn't animated"))?;
        let mut keys: Vec<klipp_core::Keyframe> = anim.keys.iter().map(|k| serde_json::from_value(k.clone()).map_err(|e| format!("a key of {key}: {e}"))).collect::<Result<_, _>>()?;
        let at = keys.iter().position(|k| (k.time_secs - time_secs).abs() < 1e-9).ok_or_else(|| format!("{key} has no key at {time_secs} s"))?;
        klipp_core::keyframe::set_interpolation_at(&mut keys, at, interpolation);
        anim.keys = keys.iter().map(|k| serde_json::to_value(k).map_err(|e| e.to_string())).collect::<Result<_, _>>()?;
        Ok(vec![GraphOp::SetAnim { path: addr.clone(), key: key.clone(), anim: Some(anim) }])
    }

    /// The one op that eases the key of `(addr, key)` at `time_secs` on
    /// `side` ([`klipp_core::keyframe::ease_key`]).
    fn ease_edit(&self, addr: &NodePath, key: &ParamKey, time_secs: f64, side: klipp_core::KeyEase) -> Result<Vec<GraphOp>, String> {
        let node = self.node_at(addr)?;
        let mut anim = animation_of(&node, key).ok_or_else(|| format!("{key} isn't animated"))?;
        let mut keys = decoded_keys(&anim, key)?;
        let at = keys.iter().position(|k| (k.time_secs - time_secs).abs() < 1e-9).ok_or_else(|| format!("{key} has no key at {time_secs} s"))?;
        klipp_core::keyframe::ease_key(&mut keys, at, side);
        anim.keys = keys.iter().map(|k| serde_json::to_value(k).map_err(|e| e.to_string())).collect::<Result<_, _>>()?;
        Ok(vec![GraphOp::SetAnim { path: addr.clone(), key: key.clone(), anim: Some(anim) }])
    }

    /// The one op that puts `edit`'s keys on `(addr, key)` in place of its
    /// own, its modifier stack kept: a whole lane pasted or reversed.
    fn lane_edit(&self, addr: &NodePath, key: &ParamKey, edit: impl FnOnce(Vec<klipp_core::Keyframe>) -> Vec<klipp_core::Keyframe>) -> Result<Vec<GraphOp>, String> {
        let node = self.node_at(addr)?;
        let anim = animation_of(&node, key);
        let keys = edit(anim.as_ref().map_or(Ok(Vec::new()), |a| decoded_keys(a, key))?);
        let modifiers = anim.and_then(|a| a.modifiers);
        let keys: Vec<serde_json::Value> = keys.iter().map(|k| serde_json::to_value(k).map_err(|e| e.to_string())).collect::<Result<_, _>>()?;
        let anim = (!keys.is_empty() || modifiers.is_some()).then_some(klipp_graph_format::Animation { keys, modifiers });
        Ok(vec![GraphOp::SetAnim { path: addr.clone(), key: key.clone(), anim }])
    }

    /// How long the clip whose graph `addr` names is, for a copied lane's
    /// keys to scale by when pasted; 0 for a graph that isn't a clip's.
    fn clip_secs_at(&self, addr: &NodePath) -> f64 {
        addr.root_clip()
            .and_then(|id| self.sequences[self.active_sequence_idx].all_clips().find(|c| c.id.0 == id))
            .map_or(0.0, |c| c.duration().to_secs_f64())
    }

    /// Whether `addr` names the Corrector the tabbed page reserved for the
    /// clip (`pending_corrector`) and the clip doesn't hold it yet: at the
    /// grade's top, or inside a compound in it (#1483), where the first
    /// edit makes it in the compound's body.
    fn names_pending_corrector(&self, addr: &NodePath) -> bool {
        let Some((clip, id)) = self.pending_corrector else { return false };
        // A clip's grade nodes are under its grade group; a sequence
        // grade's at its root's top (M3 step 19).
        let top = if matches!(addr.root, Root::Clip { .. }) { 2 } else { 1 };
        self.editing_clip(addr.root) == Some(clip) && addr.nodes.len() >= top && addr.nodes.last() == Some(&id) && self.node_at(addr).is_err()
    }

    /// The clip a root's grade commands name: the clip, or the stand-in a
    /// sequence's own grade is edited through (`GradeStandIn`).
    fn editing_clip(&self, root: Root) -> Option<ClipId> {
        let seq = &self.sequences[self.active_sequence_idx];
        match root {
            Root::Clip { clip } => Some(ClipId(clip)),
            Root::TimelineGrade { sequence } if sequence == seq.id.0 => Some(seq.timeline_grade_clip_id()),
            Root::GroupStage { group, stage } => seq.grade_group(klipp_core::GradeGroupId(group)).map(|g| g.stage_clip_id(grade_stage(stage))),
            Root::Track { track, part: klipp_graph_format::path::TrackPart::Grade } => Some(klipp_timeline::Sequence::track_grade_clip_id(klipp_core::TrackId(track))),
            _ => None,
        }
    }

    /// The grade of one of the sequence's own roots as its paths address
    /// it: the one stored, or while it has none, the graph its stand-in
    /// was put on with (`root_grade_start`).
    fn root_grade_doc(&self, root: Root) -> Result<klipp_graph_format::Graph, String> {
        let seq = &self.sequences[self.active_sequence_idx];
        let start = self.root_grade_start.as_ref().filter(|(r, _)| *r == root).map(|(_, g)| g);
        let grade = seq.root_grade(root)?.or(start).ok_or("this grade doesn't exist yet")?;
        seq.lift_root_grade(root, grade)
    }

    /// `ops` applied as one step named `label`, the reserved Corrector
    /// made first when the first of them edits it (#1527): what a sender
    /// that sets several values at once (printer lights) sends.
    pub(crate) fn apply_path_ops_making_pending(&mut self, ops: Vec<GraphOp>, label: &str) -> Result<(), String> {
        let ops = self.with_pending_corrector(ops)?;
        self.apply_path_ops(&ops, label)
    }

    /// `ops` with the pending Corrector made first when the first of them
    /// edits it (M3 step 19): added in the grade where
    /// `NodeGraph::ensure_corrector` puts one, between the grade's output
    /// and what fed it, and placed as it places one, so the edit and the node it makes are one step,
    /// and undoing it leaves the grade as it was.
    fn with_pending_corrector(&self, ops: Vec<GraphOp>) -> Result<Vec<GraphOp>, String> {
        let Some(addr) = ops.first().and_then(edited_path).filter(|a| self.names_pending_corrector(a)).cloned() else { return Ok(ops) };
        let (prefix, id) = (&addr.nodes[..addr.nodes.len() - 1], *addr.nodes.last().expect("a node"));
        let at = |node| NodePath::new(addr.root, prefix.iter().copied().chain([node]));
        let body = match prefix {
            [] => self.root_grade_doc(addr.root)?,
            _ => self.node_at(&NodePath::new(addr.root, prefix.iter().copied()))?.group.ok_or("the grade isn't a group")?.graph,
        };
        let body = &body;
        let output = body.nodes.iter().find(|n| n.type_id.as_str() == klipp_graph_format::lifted::types::GROUP_OUTPUT).ok_or("the grade has no output")?.id;
        let input = klipp_graph_format::ports::input_key("Output", 0);
        let fed = body.edges.iter().find(|e| e.to == (output, input.clone())).ok_or("nothing feeds the grade's output")?;
        // Just before the output on the canvas, as `ensure_corrector` puts it.
        let xy = |v: &serde_json::Value, k: &str| v.get(k).and_then(serde_json::Value::as_f64);
        let place = match body.ui.positions.get(&output) {
            Some(p) => serde_json::json!({"x": xy(p, "x").unwrap_or(440.0) - 220.0, "y": xy(p, "y").unwrap_or(0.0)}),
            None => serde_json::json!({"x": 220.0, "y": 0.0}),
        };
        let mut made = vec![
            GraphOp::AddNodeBeside { at: at(fed.from.0), node: Box::new(crate::inspector::grade_view::pending_corrector(id)) },
            GraphOp::Connect { from: at(fed.from.0), from_port: fed.from.1.clone(), to: at(id), to_port: klipp_graph_format::ports::input_key("Corrector", 0) },
            GraphOp::Connect { from: at(id), from_port: klipp_graph_format::ports::output_key("Corrector", 0), to: at(output), to_port: input },
            GraphOp::SetNodeMeta { path: at(id), label: None, position: Some(place) },
        ];
        made.extend(ops);
        Ok(made)
    }

    /// How many of `ops`, at their start, make the pending Corrector (see
    /// [`AppState::with_pending_corrector`]): none, or the four.
    fn pending_corrector_ops(&self, ops: &[GraphOp]) -> usize {
        let pending = self.pending_corrector.map(|(_, id)| id);
        match ops {
            [GraphOp::AddNodeBeside { node, .. }, _, _, _, _, ..] if Some(node.id) == pending => 4,
            _ => 0,
        }
    }

    /// The node `path` names, on this sequence: only a clip's graph so far,
    /// as `GraphOps` takes.
    fn node_at(&self, path: &NodePath) -> Result<Node, String> {
        if is_sequence_grade(path.root) {
            return walk(&self.root_grade_doc(path.root)?.nodes, &path.nodes).cloned().ok_or_else(|| format!("{:?} names no node of the grade", path.nodes));
        }
        let Root::Clip { clip } = path.root else { return Err("only a clip's graph is shown by path so far".into()) };
        let c = self.sequences[self.active_sequence_idx].all_clips().find(|c| c.id.0 == clip).ok_or_else(|| format!("no clip {clip} on this sequence"))?;
        let lifted = c.picture_v2().map_err(|e| e.to_string())?;
        // The placement by its stable address (M3 step 13).
        if path.nodes == [klipp_graph_lift::clip_properties::placement_id(clip)] {
            if let Some(n) = klipp_graph_lift::clip_properties::placement_node(&lifted.picture) {
                return Ok(n.clone());
            }
        }
        walk(&lifted.picture.graph.nodes, &path.nodes).cloned().ok_or_else(|| format!("{:?} names no node of the clip's graph", path.nodes))
    }

    fn reveal_graph(&mut self, path: &NodePath, select: bool) -> Result<(), String> {
        self.node_at(path)?;
        let parent = &path.nodes[..path.nodes.len() - 1];
        if self.graph_dock_root != Some(path.root) || self.graph_dock_path != parent {
            self.graph_descent_path.clear();
            self.selected_node_addr = None;
            self.selected_node_id = None;
            self.selected_node_ids.clear();
        }
        self.selected_clip = path.root_clip().map(ClipId);
        self.graph_dock_open = true;
        self.graph_dock_root = Some(path.root);
        self.graph_dock_path = path.nodes[..path.nodes.len() - 1].to_vec();
        if select {
            self.selected_node_addr = Some(path.clone());
            self.selected_node_id = path.node();
            self.selected_node_ids.clear();
            self.selected_node_ids.extend(path.node());
        }
        self.focus_graph_dock();
        Ok(())
    }

    fn set_graph_dock_root(&mut self, root: Root) -> Result<(), String> {
        let Root::Clip { clip } = root else { return Err("only a clip's graph is shown by path so far".into()) };
        if !self.sequences[self.active_sequence_idx].all_clips().any(|c| c.id.0 == clip) {
            return Err(format!("no clip {clip} on this sequence"));
        }
        self.selected_clip = Some(ClipId(clip));
        self.graph_dock_open = true;
        self.graph_dock_root = Some(root);
        self.graph_dock_path.clear();
        self.graph_descent_path.clear();
        self.selected_node_addr = None;
        self.selected_node_id = None;
        self.selected_node_ids.clear();
        self.focus_graph_dock();
        Ok(())
    }

    /// Whether the split compare's node has gone (removed, or its clip):
    /// then the compare is ended here, and the caller tells the engine.
    pub(crate) fn split_compare_gone(&mut self) -> bool {
        let Some((_, path)) = self.split_compare_node.clone() else { return false };
        if self.node_at(&path).is_ok() {
            return false;
        }
        self.split_compare_node = None;
        true
    }

    fn set_split_compare_node(&mut self, clip_id: Option<ClipId>, path: Option<&NodePath>) -> Result<(), String> {
        let (Some(clip_id), Some(path)) = (clip_id, path) else {
            self.split_compare_node = None;
            return Ok(());
        };
        if path.root != (Root::Clip { clip: clip_id.0 }) {
            return Err("the compared node is in another clip's graph".into());
        }
        self.node_at(path)?;
        self.split_compare_node = Some((clip_id, path.clone()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! M3 step 2's proofs for the effect commands (m3-step2-test-spec.md
    //! §2): each index command and its path twin give the same picture and
    //! history, a drag by path is one undo step, an edit lands only in the
    //! root and node it names, and ids are kept.

    use super::*;
    use crate::state::AppState;
    use klipp_effects::BuiltinEffect as E;
    use klipp_graph_format::{types, ParamValue};
    use klipp_timeline::{EffectEdit, GradeAccess};
    use klipp_timeline::test_clip::{self, TestClip};

    /// A clip with `effects`, ready to put in states of its own.
    fn fixture(effects: &[E]) -> klipp_timeline::Clip {
        let tc = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 25);
        TestClip::of(klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0))).effects(effects.iter().cloned()).build()
    }

    /// A fresh state holding `clips` on its first video track.
    fn with(clips: &[klipp_timeline::Clip]) -> AppState {
        let mut state = AppState::default();
        let v1 = state.sequences[0].tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        state.sequences[0].tracks[v1].clips.extend(clips.iter().cloned());
        state
    }

    fn state(effects: &[E]) -> (AppState, ClipId) {
        let c = fixture(effects);
        let id = c.id;
        (with(&[c]), id)
    }

    fn clip(state: &AppState, id: ClipId) -> &klipp_timeline::Clip {
        state.sequences[0].all_clips().find(|c| c.id == id).unwrap()
    }

    /// The whole picture, as typed: the graph and the versions.
    fn picture(state: &AppState, id: ClipId) -> klipp_timeline::LiftedPicture {
        (*clip(state, id).picture_v2().unwrap()).clone()
    }

    /// The effect nodes, in list order.
    fn effects(state: &AppState, id: ClipId) -> Vec<klipp_graph_format::NodeId> {
        clip(state, id).picture_v2().unwrap().picture.graph.nodes.iter().filter(|n| n.type_id.as_str() == types::LEGACY_EFFECT).map(|n| n.id).collect()
    }

    fn at(id: ClipId, nodes: &[klipp_graph_format::NodeId]) -> NodePath {
        NodePath::new(Root::Clip { clip: id.0 }, nodes.iter().copied())
    }

    /// What the app's old handler does with an index command
    /// (`effect_edit_by_ops`): the clip's own ops for the edit, one picture
    /// edit on the history.
    fn old(state: &mut AppState, id: ClipId, edit: EffectEdit) {
        let seq = &mut state.sequences[0];
        let c = seq.tracks.iter_mut().find_map(|t| t.find_clip_mut(id)).unwrap();
        let ops = c.effect_edit_ops(edit).expect("a plain effect edit");
        let done = c.apply_picture_ops(&ops).unwrap();
        state.push_history(EditCommand::PictureEdit { description: "Edit Effect", clip_id: id, edit: Box::new(done) });
    }

    /// Undo once, as the app does for a picture edit.
    fn undo(state: &mut AppState) {
        let entry = state.active_history_mut().undo().expect("a step to undo");
        let mut command = entry.command;
        // A path edit is recorded under its label (`Named`).
        if let EditCommand::Named { edit, .. } = command {
            command = *edit;
        }
        let EditCommand::PictureEdit { clip_id, edit, .. } = command else { panic!("a picture edit") };
        state.set_picture(clip_id, &edit.before);
    }

    fn run(state: &mut AppState, cmd: AppCommand) {
        assert_eq!(state.path_command(&cmd), Some(true), "{cmd:?} applied");
    }

    /// The placement or opacity lane `key`'s keys, as the picture holds them.
    fn lane(state: &AppState, id: ClipId, key: &str) -> Vec<(f64, f32)> {
        crate::inspector::placement_by_path::lane_keys(clip(state, id), key).iter().map(|k| (k.time_secs, k.value)).collect()
    }

    /// A clip with keys on its x position (0.5 s: 10, 1 s: 20, 1.5 s: 30)
    /// and its opacity (0.5 s: 1, 1 s: 0.5), put there by path.
    fn lane_keyed() -> (AppState, ClipId) {
        use klipp_core::VideoParam as P;
        let (mut s, id) = state(&[]);
        for (param, t, v) in [(P::PositionX, 0.5, 10.0), (P::PositionX, 1.0, 20.0), (P::PositionX, 1.5, 30.0), (P::Opacity, 0.5, 1.0), (P::Opacity, 1.0, 0.5)] {
            run(&mut s, AppCommand::AddVideoKeyframe { clip_id: id, param, time_secs: t, value: v });
        }
        (s, id)
    }

    /// Every keyframe command on a placement or opacity lane, as the
    /// timeline, the Video tab, the curve editor and the shortcuts send it,
    /// is applied by path on the placement node as one step, which undo
    /// takes back exactly; the multi-lane ones (Ken Burns, clearing the
    /// transform) too (M4 step 12c).
    #[test]
    fn a_lane_keyframe_command_is_one_path_step() {
        use klipp_core::{Interpolation, VideoParam as P};
        let (s0, id0) = lane_keyed();
        assert_eq!(lane(&s0, id0, "pos_x"), vec![(0.5, 10.0), (1.0, 20.0), (1.5, 30.0)], "keyed by path");
        type Cmd = fn(ClipId) -> AppCommand;
        type Check = fn(&AppState, ClipId);
        let families: [(&str, Cmd, Check); 11] = [
            ("add", |id| AppCommand::AddVideoKeyframe { clip_id: id, param: P::PositionX, time_secs: 0.75, value: 15.0 }, |s, id| assert_eq!(lane(s, id, "pos_x").len(), 4)),
            ("remove", |id| AppCommand::RemoveVideoKeyframe { clip_id: id, param: P::PositionX, time_secs: 1.0 }, |s, id| assert_eq!(lane(s, id, "pos_x"), vec![(0.5, 10.0), (1.5, 30.0)])),
            ("move", |id| AppCommand::MoveVideoKeyframe { clip_id: id, param: P::PositionX, index: 0, new_time_secs: 1.2, new_value: 12.0 }, |s, id| assert_eq!(lane(s, id, "pos_x"), vec![(1.0, 20.0), (1.2, 12.0), (1.5, 30.0)])),
            ("interpolation", |id| AppCommand::SetVideoKeyframeInterpolation { clip_id: id, param: P::PositionX, keyframe_index: 1, interpolation: Interpolation::Bezier }, |s, id| {
                let keys = crate::inspector::placement_by_path::lane_keys(clip(s, id), "pos_x");
                assert!(keys[0].interpolation == Interpolation::Bezier && keys[1].interpolation == Interpolation::Bezier && keys[1].tangent_out.0 > 0.0, "{keys:?}");
            }),
            ("tangent", |id| AppCommand::SetVideoKeyframeTangent { clip_id: id, param: P::PositionX, keyframe_index: 1, tangent_in: Some((-0.1, 2.0)), tangent_out: None }, |s, id| assert_eq!(crate::inspector::placement_by_path::lane_keys(clip(s, id), "pos_x")[1].tangent_in, (-0.1, 2.0))),
            ("scale timing", |id| AppCommand::ScaleVideoKeyframeTiming { clip_id: id, param: P::PositionX, factor: 0.5 }, |s, id| assert_eq!(lane(s, id, "pos_x"), vec![(0.25, 10.0), (0.5, 20.0), (0.75, 30.0)])),
            ("reverse", |id| AppCommand::ReverseVideoKeyframes { clip_id: id, param: P::PositionX }, |s, id| assert_eq!(lane(s, id, "pos_x"), vec![(0.0, 30.0), (0.5, 20.0), (1.0, 10.0)])),
            ("bump", |id| AppCommand::InsertVideoKeyframeBump { clip_id: id, param: P::Opacity, center_time: 1.5, bump_value: 0.2, base_value: 1.0, duration_secs: 0.4, ramp_secs: 0.1 }, |s, id| assert_eq!(lane(s, id, "opacity").len(), 6)),
            ("clear a lane", |id| AppCommand::ClearVideoParamKeyframes { clip_id: id, param: P::PositionX }, |s, id| assert!(lane(s, id, "pos_x").is_empty())),
            ("Ken Burns", |id| AppCommand::ApplyKenBurns { clip_id: id, start_scale: 1.0, end_scale: 1.3, start_pos_x: 0.0, start_pos_y: 0.0, end_pos_x: 40.0, end_pos_y: 20.0 }, |s, id| {
                assert_eq!(lane(s, id, "scale").iter().map(|k| k.1).collect::<Vec<_>>(), vec![1.0, 1.3]);
                assert_eq!(lane(s, id, "pos_x").iter().map(|k| k.1).collect::<Vec<_>>(), vec![0.0, 40.0]);
                assert_eq!(lane(s, id, "pos_y").iter().map(|k| k.1).collect::<Vec<_>>(), vec![0.0, 20.0]);
            }),
            ("clear the transform", |id| AppCommand::ClearTransformKeyframes { clip_id: id }, |s, id| {
                assert!(lane(s, id, "pos_x").is_empty());
                assert_eq!(lane(s, id, "opacity").len(), 2, "the opacity isn't the transform's");
            }),
        ];
        for (name, cmd, check) in families {
            let (mut s, id) = lane_keyed();
            let (before, steps) = (picture(&s, id), s.active_history().undo_count());
            assert_eq!(s.path_command(&cmd(id)), Some(true), "{name}: {:?}", s.toast.message_for_test());
            check(&s, id);
            assert_eq!(s.active_history().undo_count(), steps + 1, "{name}: one step");
            undo(&mut s);
            assert_eq!(picture(&s, id), before, "{name}: undo, as it was");
        }
    }

    /// Copy then paste onto another lane: the keys, scaled to the clip's
    /// length, in one step.
    #[test]
    fn copied_keys_paste_by_path_in_one_step() {
        use klipp_core::VideoParam as P;
        let (mut s, id) = lane_keyed();
        assert_eq!(s.path_command(&AppCommand::CopyVideoKeyframes { clip_id: id, param: P::PositionX }), Some(false), "a copy writes nothing");
        let (before, steps) = (picture(&s, id), s.active_history().undo_count());
        run(&mut s, AppCommand::PasteVideoKeyframes { clip_id: id, param: P::PositionY });
        assert_eq!(lane(&s, id, "pos_y"), lane(&s, id, "pos_x"));
        assert_eq!(s.active_history().undo_count(), steps + 1);
        undo(&mut s);
        assert_eq!(picture(&s, id), before);
    }

    /// A key dragged on a placement lane, as the timeline brackets it: the
    /// drag's ticks are one step, the key keeps its place in the lane's
    /// order as the old lane edit kept it (so the selection follows it),
    /// and undo takes the whole drag back.
    #[test]
    fn a_lane_key_drag_is_one_step_and_the_key_keeps_its_place() {
        use klipp_core::VideoParam as P;
        let (mut s, id) = lane_keyed();
        let (before, steps) = (picture(&s, id), s.active_history().undo_count());
        let key = format!("kf:video:{}:{:?}:{}", id.0, P::PositionX, 1);
        assert_eq!(s.path_command(&AppCommand::BeginKeyframeDrag { key: key.clone(), clip_id: id }), Some(false));
        for (t, v) in [(1.05, 21.0), (1.1, 22.0), (1.2, 24.0)] {
            assert!(!s.closes_path_drag(&AppCommand::MoveVideoKeyframe { clip_id: id, param: P::PositionX, index: 1, new_time_secs: t, new_value: v }), "a tick of the lane's drag doesn't close it");
            run(&mut s, AppCommand::MoveVideoKeyframe { clip_id: id, param: P::PositionX, index: 1, new_time_secs: t, new_value: v });
            assert_eq!(lane(&s, id, "pos_x")[1], (t, v), "the dragged key stays the second");
        }
        assert_eq!(s.path_command(&AppCommand::EndKeyframeDrag { key, clip_id: id }), Some(false));
        assert_eq!(s.active_history().undo_count(), steps + 1, "one step for the drag");
        undo(&mut s);
        assert_eq!(picture(&s, id), before);
    }

    /// A socket batch (the control bridge's `expand_control_commands`,
    /// batched) of two path edits and an index edit, dispatched as the app
    /// dispatches it, is one undo step that undoes all three exactly. A
    /// path edit used to open a batch of its own, which threw away what
    /// the socket's batch had collected and made each edit its own step.
    #[test]
    fn a_socket_batch_of_path_and_index_edits_is_one_step() {
        let (mut s, id) = state(&[E::GaussianBlur { radius: 3.0 }, E::Sharpen { amount: 0.5 }]);
        let ids = effects(&s, id);
        let (before, steps) = (picture(&s, id), s.active_history().undo_count());
        let invert = E::Invert { mix: 1.0 };
        let cmds = super::super::control_bridge::expand_control_commands(
            vec![
                AppCommand::SetParamAt { addr: at(id, &[ids[0]]), key: "radius".into(), value: ParamValue::Float(7.0) },
                AppCommand::SetParamAt { addr: at(id, &[ids[1]]), key: "amount".into(), value: ParamValue::Float(0.9) },
                AppCommand::AddBuiltinEffect { clip_id: id, effect: invert.clone() },
            ],
            true,
        );
        for cmd in cmds {
            s.before_dispatch(&cmd);
            match cmd {
                AppCommand::BeginHistoryBatch => s.active_history_mut().begin_batch(),
                AppCommand::EndHistoryBatch => s.active_history_mut().end_batch(),
                // The index add, as the app's old handler lands it.
                AppCommand::AddBuiltinEffect { .. } => old(&mut s, id, EffectEdit::Add(&invert)),
                cmd => run(&mut s, cmd),
            }
        }
        let list = |s: &AppState| serde_json::to_value(test_clip::effects(clip(s, id))).unwrap();
        assert_eq!(list(&s), serde_json::to_value([E::GaussianBlur { radius: 7.0 }, E::Sharpen { amount: 0.9 }, invert.clone()]).unwrap(), "all three applied");
        assert_eq!(s.active_history().undo_count(), steps + 1, "one step");
        let entry = s.active_history_mut().undo().expect("the batch's step");
        let mut doc = klipp_document::apply::DocumentView { sequences: &mut s.sequences, active: s.active_sequence_idx, media_pool: &mut s.media_pool, fps: &mut s.fps };
        doc.apply_inverse(&entry.command);
        assert_eq!(picture(&s, id), before, "undone, all three, exactly");
    }

    fn layout_command(s: &mut AppState, cmd: AppCommand) {
        s.before_dispatch(&cmd);
        assert!(s.path_command(&cmd).is_some(), "{cmd:?}");
    }

    #[test]
    fn a_node_layout_gesture_moves_several_nodes_and_undoes_once() {
        let (mut s, id) = state(&[E::GaussianBlur { radius: 3.0 }, E::Sharpen { amount: 0.5 }]);
        let ids = effects(&s, id);
        let root = Root::Clip { clip: id.0 };
        let before = picture(&s, id);
        layout_command(&mut s, AppCommand::BeginNodeLayoutEdit { root });
        for x in [10.0, 20.0, 30.0] {
            for (i, node) in ids.iter().enumerate() {
                layout_command(&mut s, AppCommand::SetNodePositionAt { addr: at(id, &[*node]), position: (x, i as f32 * 100.0) });
            }
            s.before_dispatch(&AppCommand::SetScopesVisible(true));
            assert_eq!(s.active_history().undo_count(), 0);
        }
        layout_command(&mut s, AppCommand::EndNodeLayoutEdit { root });
        assert_eq!(s.active_history().undo_count(), 1);
        for (i, node) in ids.iter().enumerate() {
            assert_eq!(picture(&s, id).picture.graph.ui.positions[node], serde_json::json!({"x":30.0,"y":i as f32 * 100.0}));
        }
        undo(&mut s);
        assert_eq!(picture(&s, id), before);
    }

    #[test]
    fn an_unrelated_edit_closes_a_node_layout_gesture() {
        let (mut s, id) = state(&[E::GaussianBlur { radius: 3.0 }]);
        let node = effects(&s, id)[0];
        let root = Root::Clip { clip: id.0 };
        let before = picture(&s, id);
        layout_command(&mut s, AppCommand::BeginNodeLayoutEdit { root });
        layout_command(&mut s, AppCommand::SetNodePositionAt { addr: at(id, &[node]), position: (10.0, 20.0) });
        let moved = picture(&s, id);
        layout_command(&mut s, AppCommand::SetNodeEnabledAt { addr: at(id, &[node]), enabled: false });
        assert!(!s.active_history().is_any_drag_in_progress());
        assert_eq!(s.active_history().undo_count(), 2);
        layout_command(&mut s, AppCommand::EndNodeLayoutEdit { root });
        undo(&mut s);
        assert_eq!(picture(&s, id), moved);
        undo(&mut s);
        assert_eq!(picture(&s, id), before);
    }

    #[test]
    fn cancelled_and_empty_node_layout_gestures_record_nothing() {
        let (mut s, id) = state(&[E::GaussianBlur { radius: 3.0 }]);
        let node = effects(&s, id)[0];
        let root = Root::Clip { clip: id.0 };
        let before = picture(&s, id);
        layout_command(&mut s, AppCommand::BeginNodeLayoutEdit { root });
        layout_command(&mut s, AppCommand::EndNodeLayoutEdit { root });
        assert_eq!(s.active_history().undo_count(), 0);
        layout_command(&mut s, AppCommand::BeginNodeLayoutEdit { root });
        layout_command(&mut s, AppCommand::SetNodePositionAt { addr: at(id, &[node]), position: (10.0, 20.0) });
        layout_command(&mut s, AppCommand::AbandonNodeLayoutEdit { root });
        assert_eq!(s.active_history().undo_count(), 0);
        assert_eq!(picture(&s, id), before);
    }

    /// Every index command on a clip's effect list has a path twin: a new
    /// command naming an effect by index fails to compile here until it is
    /// listed. OFX plug-ins' twins are out of step 2 (the clip's writer
    /// refuses OFX nodes; they get a step of their own), as are track,
    /// audio and title-object effects.
    fn has_a_twin(cmd: &AppCommand) -> bool {
        match cmd {
            AppCommand::AddBuiltinEffect { .. }
            | AppCommand::RemoveBuiltinEffect { .. }
            | AppCommand::ToggleBuiltinEffectEnabled { .. }
            | AppCommand::UpdateBuiltinEffect { .. }
            | AppCommand::ReorderBuiltinEffect { .. }
            | AppCommand::AddEffectKeyframe { .. }
            | AppCommand::RemoveEffectKeyframe { .. }
            | AppCommand::MoveEffectKeyframe { .. }
            | AppCommand::SetEffectKeyframeInterpolation { .. }
            | AppCommand::BeginEffectDrag { .. }
            | AppCommand::EndEffectDrag { .. } => true,
            AppCommand::AddEffect { .. }
            | AppCommand::RemoveEffect { .. }
            | AppCommand::SetEffectEnabled { .. }
            | AppCommand::ReorderEffect { .. }
            | AppCommand::SetEffectParam { .. } => false,
            _ => unreachable!("not an index command"),
        }
    }

    /// Each built-in effect command by index, and its path twin on a copy:
    /// the same picture, one history step each, and one undo back to the
    /// picture before (proof 1, the effect list's half).
    #[test]
    fn every_index_command_has_a_path_twin() {
        let fx = [E::GaussianBlur { radius: 3.0 }, E::Sharpen { amount: 0.5 }, E::Gamma { gamma: 1.2 }];
        let c = fixture(&fx);
        let id = c.id;
        let base = with(std::slice::from_ref(&c));
        let ids = effects(&base, id);
        let (i0, i1, i2) = (ids[0], ids[1], ids[2]);
        let gamma = E::Gamma { gamma: 1.6 };
        let added = E::Vignette { amount: 0.3, size: 0.8, softness: 0.5, roundness: 0.7 };
        type Twin = Box<dyn Fn(&AppState) -> AppCommand>;
        let cases: Vec<(AppCommand, EffectEdit, Twin)> = vec![
            (
                AppCommand::RemoveBuiltinEffect { clip_id: id, index: 1 },
                EffectEdit::Remove(1),
                Box::new(move |_| AppCommand::RemoveNodeAt { addr: at(id, &[i1]) }),
            ),
            (
                AppCommand::ToggleBuiltinEffectEnabled { clip_id: id, index: 0 },
                EffectEdit::SetEnabled(0, false),
                Box::new(move |_| AppCommand::SetNodeEnabledAt { addr: at(id, &[i0]), enabled: false }),
            ),
            (
                AppCommand::UpdateBuiltinEffect { clip_id: id, index: 2, effect: gamma.clone() },
                EffectEdit::Update(2, &gamma),
                Box::new(move |_| AppCommand::SetPayloadAt { addr: at(id, &[i2]), payload: serde_json::to_value(E::Gamma { gamma: 1.6 }).unwrap() }),
            ),
            (
                AppCommand::ReorderBuiltinEffect { clip_id: id, from_index: 2, to_index: 1 },
                EffectEdit::Move { from: 2, to: 1 },
                Box::new(move |_| AppCommand::MoveNodeAt { addr: at(id, &[i2]), after: i0 }),
            ),
        ];
        for (index_cmd, edit, twin) in cases {
            assert!(has_a_twin(&index_cmd));
            let (mut a, mut b) = (with(std::slice::from_ref(&c)), with(std::slice::from_ref(&c)));
            old(&mut a, id, edit);
            let cmd = twin(&b);
            run(&mut b, cmd);
            assert_eq!(picture(&a, id), picture(&b, id), "{index_cmd:?}: the twin draws the same picture");
            assert_eq!(a.active_history().undo_count(), b.active_history().undo_count(), "{index_cmd:?}: one step each");
            undo(&mut b);
            assert_eq!(picture(&b, id), picture(&base, id), "{index_cmd:?}: undone");
        }
        // The add: the twin names the node the old add made, so the two
        // pictures are the same down to its id.
        let index_cmd = AppCommand::AddBuiltinEffect { clip_id: id, effect: added.clone() };
        assert!(has_a_twin(&index_cmd));
        let (mut a, mut b) = (with(std::slice::from_ref(&c)), with(std::slice::from_ref(&c)));
        old(&mut a, id, EffectEdit::Add(&added));
        let made = clip(&a, id).picture_v2().unwrap().picture.graph.nodes.iter().find(|n| !ids.contains(&n.id) && n.type_id.as_str() == types::LEGACY_EFFECT).cloned().expect("the added node");
        run(&mut b, AppCommand::AddNodeAt { after: at(id, &[ids[2]]), node: Box::new(made) });
        assert_eq!(picture(&a, id), picture(&b, id), "the add's twin draws the same picture");
        undo(&mut b);
        assert_eq!(picture(&b, id), picture(&base, id), "the add undone");
    }

    #[test]
    fn mix_drags_keep_other_nodes_and_parameters_in_separate_undo_steps() {
        for other_node in [false, true] {
            let (mut s, id) = state(&[E::GaussianBlur { radius: 3.0 }, E::Sharpen { amount: 0.5 }]);
            let ids = effects(&s, id);
            let addr = at(id, &[ids[0]]);
            let before = picture(&s, id);
            let key = AppState::NODE_MIX_DRAG_KEY.into();
            s.path_command(&AppCommand::BeginParamEditAt {
                addr: addr.clone(),
                key,
            });
            for mix in [0.8, 0.6, 0.4] {
                let cmd = AppCommand::SetMixAt {
                    addr: addr.clone(),
                    mix: Some(mix),
                };
                assert!(!s.closes_path_drag(&cmd));
                s.before_dispatch(&cmd);
                run(&mut s, cmd);
            }
            let dragged = picture(&s, id);
            let cmd = if other_node {
                AppCommand::SetMixAt {
                    addr: at(id, &[ids[1]]),
                    mix: Some(0.2),
                }
            } else {
                AppCommand::SetParamAt {
                    addr,
                    key: "radius".into(),
                    value: ParamValue::Float(9.0),
                }
            };
            assert!(s.closes_path_drag(&cmd));
            s.before_dispatch(&cmd);
            run(&mut s, cmd);
            assert!(!s.any_drag_in_progress());
            assert_eq!(s.active_history().undo_count(), 2);
            undo(&mut s);
            assert_eq!(picture(&s, id), dragged);
            undo(&mut s);
            assert_eq!(picture(&s, id), before);
        }
    }

    /// A drag by path is one undo step, whatever its steps: undo returns
    /// the value from before it (proof 2).
    #[test]
    fn a_drag_by_path_is_one_undo_step() {
        let (mut s, id) = state(&[E::GaussianBlur { radius: 3.0 }]);
        let before = picture(&s, id);
        let blur = at(id, &effects(&s, id));
        let key: ParamKey = "radius".into();
        let steps = s.active_history().undo_count();
        assert_eq!(s.path_command(&AppCommand::BeginParamEditAt { addr: blur.clone(), key: key.clone() }), Some(false));
        for i in 0..30 {
            run(&mut s, AppCommand::SetParamAt { addr: blur.clone(), key: key.clone(), value: ParamValue::Float(3.0 + i as f32) });
        }
        assert_eq!(s.path_command(&AppCommand::EndParamEditAt { addr: blur.clone(), key: key.clone() }), Some(false));
        assert_eq!(s.active_history().undo_count(), steps + 1, "thirty steps, one entry");
        assert!(matches!(test_clip::effects(clip(&s, id))[0], E::GaussianBlur { radius } if radius == 32.0));
        undo(&mut s);
        assert_eq!(picture(&s, id), before, "undo returns the value from before the drag");
    }

    /// An edit of another `(addr, key)` while a drag is open closes the
    /// drag as its own step and records its own: it never merges into the
    /// drag's entry. A drag left open is closed by the next edit the same way.
    #[test]
    fn an_edit_inside_a_drag_closes_it_first() {
        let (mut s, id) = state(&[E::GaussianBlur { radius: 3.0 }, E::Sharpen { amount: 0.5 }]);
        let ids = effects(&s, id);
        let (blur, sharpen) = (at(id, &[ids[0]]), at(id, &[ids[1]]));
        let steps = s.active_history().undo_count();
        s.path_command(&AppCommand::BeginParamEditAt { addr: blur.clone(), key: "radius".into() });
        run(&mut s, AppCommand::SetParamAt { addr: blur.clone(), key: "radius".into(), value: ParamValue::Float(9.0) });
        run(&mut s, AppCommand::SetParamAt { addr: sharpen.clone(), key: "amount".into(), value: ParamValue::Float(0.9) });
        assert!(!s.active_history().is_any_drag_in_progress(), "the other edit closed the drag");
        assert_eq!(s.active_history().undo_count(), steps + 2, "the drag, then the edit");
        // The drag's End after it was closed changes nothing.
        s.path_command(&AppCommand::EndParamEditAt { addr: blur, key: "radius".into() });
        assert_eq!(s.active_history().undo_count(), steps + 2);
        undo(&mut s);
        assert!(matches!(test_clip::effects(clip(&s, id))[1], E::Sharpen { amount } if amount == 0.5), "the edit undoes alone");
        assert!(matches!(test_clip::effects(clip(&s, id))[0], E::GaussianBlur { radius } if radius == 9.0), "the drag's step stays");
    }

    /// A write that isn't the drag's own, passing the dispatch boundary
    /// while a row's drag is held (a sampled result, a command over the
    /// socket), closes the drag as its own step; the held row's next ticks
    /// resume it as one more step, never one step per tick.
    #[test]
    fn a_drag_held_past_a_write_at_the_boundary_is_three_steps() {
        let (mut s, id) = state(&[E::GaussianBlur { radius: 3.0 }, E::Sharpen { amount: 0.5 }]);
        let ids = effects(&s, id);
        let (blur, sharpen) = (at(id, &[ids[0]]), at(id, &[ids[1]]));
        let dispatch = |s: &mut AppState, cmd: AppCommand| {
            s.before_dispatch(&cmd);
            assert!(s.path_command(&cmd).is_some(), "{cmd:?}");
        };
        let radius = |v: f32| AppCommand::SetParamAt { addr: blur.clone(), key: "radius".into(), value: ParamValue::Float(v) };
        let steps = s.active_history().undo_count();
        dispatch(&mut s, AppCommand::BeginParamEditAt { addr: blur.clone(), key: "radius".into() });
        dispatch(&mut s, radius(5.0));
        // The write from elsewhere, mid-drag.
        dispatch(&mut s, AppCommand::SetParamAt { addr: sharpen.clone(), key: "amount".into(), value: ParamValue::Float(0.9) });
        // The row still held: ten more ticks, then its release.
        for i in 0..10 {
            dispatch(&mut s, radius(6.0 + i as f32));
        }
        dispatch(&mut s, AppCommand::EndParamEditAt { addr: blur.clone(), key: "radius".into() });
        assert_eq!(s.active_history().undo_count(), steps + 3, "the drag before, the write, the drag after");
        let values = |s: &AppState| match test_clip::effects(clip(s, id)).as_slice() {
            [E::GaussianBlur { radius }, E::Sharpen { amount }] => (*radius, *amount),
            other => panic!("{other:?}"),
        };
        assert_eq!(values(&s), (15.0, 0.9));
        undo(&mut s);
        assert_eq!(values(&s), (5.0, 0.9), "the drag's rest undoes alone");
        undo(&mut s);
        assert_eq!(values(&s), (5.0, 0.5), "then the write");
        undo(&mut s);
        assert_eq!(values(&s), (3.0, 0.5), "then the drag's start");
    }

    /// An edit by path changes only the clip and node it names; a path
    /// that skips a group isn't searched for, it's refused, and nothing
    /// changes (proofs 3 and 4).
    #[test]
    fn an_edit_lands_only_in_the_addressed_root() {
        let (mut s, a) = state(&[E::GaussianBlur { radius: 3.0 }]);
        let v1 = s.sequences[0].tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        let mut copy = clip(&s, a).clone();
        copy.id = ClipId::new();
        let b = copy.id;
        s.sequences[0].tracks[v1].clips.push(copy);
        let other = picture(&s, b);
        let fx_a = effects(&s, a);
        run(&mut s, AppCommand::SetParamAt { addr: at(a, &fx_a), key: "radius".into(), value: ParamValue::Float(7.0) });
        assert!(matches!(test_clip::effects(clip(&s, a))[0], E::GaussianBlur { radius } if radius == 7.0));
        assert_eq!(picture(&s, b), other, "the other clip is untouched");
        // A node inside the grade group, named without the group.
        let grade = clip(&s, a).picture_v2().unwrap().picture.graph.nodes.iter().find_map(|n| n.group.as_ref().and_then(|g| g.graph.nodes.first()).map(|inner| inner.id));
        if let Some(inner) = grade {
            let steps = s.active_history().undo_count();
            let before = picture(&s, a);
            assert_eq!(s.path_command(&AppCommand::SetNodeEnabledAt { addr: at(a, &[inner]), enabled: false }), Some(false), "refused");
            assert_eq!(picture(&s, a), before);
            assert_eq!(s.active_history().undo_count(), steps);
        }
    }

    /// M3 step 4: the split compare names a node, so moving the effects
    /// around it, or taking away one before it, leaves it on the same
    /// effect (the index it held was patched by hand on a removal, and a
    /// reorder moved it onto another effect); taking the compared effect
    /// away ends it. The engine toggles the same node.
    #[test]
    fn split_compare_survives_a_reorder() {
        let (mut s, a) = state(&[E::GaussianBlur { radius: 3.0 }, E::Sharpen { amount: 0.5 }, E::Vibrance { amount: 0.2 }]);
        let fx = effects(&s, a);
        let compared = at(a, &[fx[2]]);
        assert_eq!(s.path_command(&AppCommand::SetSplitCompareNode { clip_id: Some(a), path: Some(compared.clone()) }), Some(false));
        let is_vibrance = |s: &AppState| {
            let (_, path) = s.split_compare_node.clone().expect("still compared");
            let c = clip(s, a);
            let (ofx, i) = c.effect_slot_of_node(path.node().unwrap()).expect("the clip holds it");
            !ofx && matches!(test_clip::effects(c)[i], E::Vibrance { .. })
        };
        // Moved to the front, then the effect before it taken away.
        run(&mut s, AppCommand::MoveNodeAt { addr: compared.clone(), after: fx[0] });
        assert!(!s.split_compare_gone());
        assert!(is_vibrance(&s), "the compare moved with its effect");
        run(&mut s, AppCommand::RemoveNodeAt { addr: at(a, &[fx[0]]) });
        assert!(!s.split_compare_gone());
        assert!(is_vibrance(&s), "a removal before it doesn't move it");
        let mut toggled = clip(&s, a).clone();
        assert!(toggled.toggle_effect_node(compared.node().unwrap()));
        let i = toggled.effect_slot_of_node(compared.node().unwrap()).unwrap().1;
        assert!(!test_clip::enabled(&toggled)[i], "the engine's bypass is the compared effect's");
        // The compared effect taken away: the compare ends.
        run(&mut s, AppCommand::RemoveNodeAt { addr: compared });
        assert!(s.split_compare_gone());
        assert!(s.split_compare_node.is_none());
    }

    /// A clip with a keyed Gamma and a keyed Vibrance (M3 step 3).
    fn keyed() -> (AppState, ClipId) {
        let c = TestClip::of(fixture(&[E::Gamma { gamma: 1.0 }, E::Vibrance { amount: 0.1 }]))
            .key(0, "gamma", 0.2, 0.8)
            .key(0, "gamma", 1.2, 1.4)
            .key(1, "amount", 0.5, 0.0)
            .key(1, "amount", 1.5, 0.6)
            .build();
        let id = c.id;
        (with(&[c]), id)
    }

    /// M3 step 3, H4 at the UI's level: the curve editor's tracks are the
    /// nodes' parameters, listed in the order the picture runs them, so
    /// a reorder moves each track (its keys, its diamonds) with its node.
    #[test]
    fn curve_tracks_follow_the_node_through_a_reorder() {
        let (mut s, id) = keyed();
        let tracks = |s: &AppState| clip(s, id).animated_effect_keys();
        let before = tracks(&s);
        assert_eq!(before.iter().map(|(_, k, t)| (t.as_str(), k.as_str())).collect::<Vec<_>>(), [("gamma", "gamma"), ("vibrance", "amount")]);
        let keys_of = |s: &AppState, (path, key, _): &(NodePath, ParamKey, String)| clip(s, id).node_key_state(path, key).unwrap().keys;
        let gamma_keys = keys_of(&s, &before[0]);
        let fx = effects(&s, id);
        run(&mut s, AppCommand::MoveNodeAt { addr: at(id, &[fx[0]]), after: fx[1] });
        let after = tracks(&s);
        assert_eq!(after.iter().map(|(_, _, t)| t.as_str()).collect::<Vec<_>>(), ["vibrance", "gamma"], "listed as the picture runs them");
        let gamma = after.iter().find(|(_, _, t)| t == "gamma").unwrap();
        assert_eq!(gamma.0, before[0].0, "the track is the same node's");
        assert_eq!(keys_of(&s, gamma), gamma_keys, "its keys moved with it");
    }

    /// IC6: a track's label and range come from the catalog and its
    /// presentation, so a pixel radius ranges over its slider's span.
    #[test]
    fn curve_ranges_come_from_the_catalog() {
        let (s, id) = state(&[E::GaussianBlur { radius: 3.0 }]);
        let node = effects(&s, id)[0];
        let p = crate::panels::curve_editor::CurveParam::Node { path: at(id, &[node]), key: "radius".into(), tag: "gaussian_blur".into() };
        let desc = klipp_effects::registry::effect_type("gaussian_blur").unwrap().desc.params.iter().find(|d| d.field == "radius").unwrap();
        let (lo, hi) = desc.soft.or(desc.range).unwrap();
        assert_eq!((p.curve_range().0, p.curve_range().1), (lo, hi));
        assert_ne!((lo, hi), (0.0, 1.0), "not the old 0..1");
        assert!(p.label().starts_with("Gaussian Blur: "), "{}", p.label());
    }

    /// E: on a plain stack, the key a click resolves to by path is the
    /// key it resolved to by index.
    #[test]
    fn keyframe_selection_by_path_matches_by_index_on_a_linear_stack() {
        let (s, id) = keyed();
        let fx = effects(&s, id);
        let seq = &s.sequences[0];
        for t in [0.19, 0.21, 1.19, 1.25, 0.7] {
            let mut by_index = AppCommand::RemoveEffectKeyframe { clip_id: id, effect_index: 0, param_name: "gamma".into(), time_secs: t };
            let mut by_path = AppCommand::RemoveKeyAt { addr: at(id, &[fx[0]]), key: "gamma".into(), time_secs: t };
            crate::inspector::keyframe_selection::resolve_command(seq, &mut by_index);
            crate::inspector::keyframe_selection::resolve_command(seq, &mut by_path);
            let (AppCommand::RemoveEffectKeyframe { time_secs: a, .. }, AppCommand::RemoveKeyAt { time_secs: b, .. }) = (&by_index, &by_path) else { unreachable!() };
            assert_eq!(a, b, "at {t}");
        }
    }

    /// A key dragged on the curve writes the node's animation, live, and
    /// the drag is one undo step back to where it started.
    #[test]
    fn a_curve_edit_writes_the_node_anim() {
        let (mut s, id) = keyed();
        let fx = effects(&s, id);
        let (addr, key) = (at(id, &[fx[0]]), ParamKey::from("gamma"));
        let keys = |s: &AppState| clip(s, id).node_key_state(&addr, &key).unwrap().keys;
        let before = keys(&s);
        let steps = s.active_history().undo_count();
        assert_eq!(s.path_command(&AppCommand::BeginParamEditAt { addr: addr.clone(), key: key.clone() }), Some(false));
        let mut time = 0.2;
        for (t, v) in [(0.3, 0.9), (0.4, 1.0), (0.5, 1.1)] {
            run(&mut s, AppCommand::MoveKeyAt { addr: addr.clone(), key: key.clone(), time_secs: time, new_time_secs: t, value: v });
            time = t;
            assert_eq!(s.active_history().undo_count(), steps, "a drag's steps are no undo steps of their own");
        }
        assert_eq!(keys(&s)[0].time_secs, 0.5);
        assert_eq!(keys(&s)[0].value, 1.1);
        assert_eq!(s.path_command(&AppCommand::EndParamEditAt { addr: addr.clone(), key: key.clone() }), Some(false));
        assert_eq!(s.active_history().undo_count(), steps + 1, "one undo step for the drag");
        undo(&mut s);
        assert_eq!(keys(&s), before, "undone, the keys as they were");
    }

    /// A key dragged onto another's time is refused there (two keys never
    /// share a time, which names them), so the drag goes on moving the key
    /// it took, never the other; and undo brings back both.
    #[test]
    fn a_key_dragged_across_another_keeps_its_identity() {
        let (mut s, id) = keyed();
        let fx = effects(&s, id);
        let (addr, key) = (at(id, &[fx[0]]), ParamKey::from("gamma"));
        let keys = |s: &AppState| clip(s, id).node_key_state(&addr, &key).unwrap().keys.iter().map(|k| (k.time_secs, k.value)).collect::<Vec<_>>();
        let before = keys(&s);
        assert_eq!(before, [(0.2, 0.8), (1.2, 1.4)]);
        assert_eq!(s.path_command(&AppCommand::BeginParamEditAt { addr: addr.clone(), key: key.clone() }), Some(false));
        // B onto A's time: refused, B stays where it was.
        let onto = s.path_command(&AppCommand::MoveKeyAt { addr: addr.clone(), key: key.clone(), time_secs: 1.2, new_time_secs: 0.2, value: 1.0 });
        assert_eq!(onto, Some(false), "refused");
        assert_eq!(keys(&s), before);
        // B past A: B moves, A untouched, and the next frame moves B again.
        run(&mut s, AppCommand::MoveKeyAt { addr: addr.clone(), key: key.clone(), time_secs: 1.2, new_time_secs: 0.1, value: 1.0 });
        run(&mut s, AppCommand::MoveKeyAt { addr: addr.clone(), key: key.clone(), time_secs: 0.1, new_time_secs: 0.05, value: 0.9 });
        assert_eq!(keys(&s), [(0.05, 0.9), (0.2, 0.8)]);
        assert_eq!(s.path_command(&AppCommand::EndParamEditAt { addr: addr.clone(), key: key.clone() }), Some(false));
        undo(&mut s);
        assert_eq!(keys(&s), before, "undone, both keys as they were");
    }

    /// M3 step 13: the corner pin's handle, by path (the commands
    /// `panels/viewer.rs` sends behind the switch): a drag sets both of a
    /// corner's element keys and keys the keyed one, all one undo step on
    /// the pin's node, and a reorder between two drags leaves them on the
    /// same node.
    #[test]
    fn a_cornerpin_handle_drag_keys_the_same_node() {
        let pin = (klipp_effects::registry::effect_type("corner_pin").unwrap().sample)();
        let (mut s, id) = state(&[pin, E::Gamma { gamma: 1.2 }]);
        let pin_node = clip(&s, id).effect_node_at(false, 0).unwrap();
        let addr = at(id, &[pin_node]);
        let key = |t: f64, v: f32| serde_json::to_value(klipp_core::Keyframe { time_secs: t, value: v, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }).unwrap();
        // tl_x keyed before the drags.
        run(&mut s, AppCommand::AddKeyAt { addr: addr.clone(), key: "tl_x".into(), keyframe: key(0.0, 0.0) });
        let drag = |s: &mut AppState, t: f64, x: f32, y: f32| {
            let steps = s.active_history().undo_count();
            // A drag over the pin's parameters together (an empty key).
            assert_eq!(s.path_command(&AppCommand::BeginParamEditAt { addr: addr.clone(), key: "".into() }), Some(false));
            for (k, v) in [("tl_x", x), ("tl_y", y)] {
                run(s, AppCommand::SetParamAt { addr: addr.clone(), key: k.into(), value: ParamValue::Float(v) });
            }
            run(s, AppCommand::AddKeyAt { addr: addr.clone(), key: "tl_x".into(), keyframe: key(t, x) });
            assert_eq!(s.path_command(&AppCommand::EndParamEditAt { addr: addr.clone(), key: "".into() }), Some(false));
            assert_eq!(s.active_history().undo_count(), steps + 1, "one undo step a drag");
        };
        drag(&mut s, 1.0, 0.1, 0.05);
        let fx = effects(&s, id);
        run(&mut s, AppCommand::MoveNodeAt { addr: addr.clone(), after: fx[1] });
        drag(&mut s, 1.5, 0.2, 0.1);
        let c = clip(&s, id);
        assert_eq!(c.effect_node_at(false, 1), Some(pin_node), "the pin moved after the gamma");
        let keys = c.node_key_state(&addr, &"tl_x".into()).unwrap().keys;
        assert_eq!(keys.iter().map(|k| (k.time_secs, k.value)).collect::<Vec<_>>(), [(0.0, 0.0), (1.0, 0.1), (1.5, 0.2)], "every key on the pin's node");
        assert!(matches!(test_clip::effects(c)[1], E::CornerPin { tl, .. } if tl[1] == 0.1), "{:?}", test_clip::effects(c)[1]);
    }

    /// An effect moved by path (the stack's ↑ and ↓ send `MoveNodeAt`,
    /// `reorder_moves_by_path`) stays the selected one, keeps its keys, and
    /// the move is one step whose undo puts the order back with the
    /// selection and the keys as they were.
    #[test]
    fn a_reorder_keeps_the_selection_and_the_keys() {
        let (mut s, id) = state(&[E::GaussianBlur { radius: 3.0 }, E::Vignette { amount: 0.5, size: 0.5, softness: 0.5, roundness: 0.5 }]);
        let fx = effects(&s, id);
        let blur = at(id, &[fx[0]]);
        let key = |t: f64, v: f32| serde_json::to_value(klipp_core::Keyframe { time_secs: t, value: v, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }).unwrap();
        run(&mut s, AppCommand::AddKeyAt { addr: blur.clone(), key: "radius".into(), keyframe: key(0.0, 2.0) });
        run(&mut s, AppCommand::AddKeyAt { addr: blur.clone(), key: "radius".into(), keyframe: key(1.0, 6.0) });
        s.selected_node_addr = Some(blur.clone());
        let keys = |s: &AppState| clip(s, id).node_key_state(&blur, &"radius".into()).map(|k| k.keys.iter().map(|k| (k.time_secs, k.value)).collect::<Vec<_>>());
        let had = keys(&s);
        assert_eq!(had.as_deref(), Some(&[(0.0, 2.0), (1.0, 6.0)][..]), "the blur's keys");
        let steps = s.active_history().undo_count();
        run(&mut s, AppCommand::MoveNodeAt { addr: blur.clone(), after: fx[1] });
        assert_eq!(effects(&s, id), [fx[1], fx[0]], "the blur moved after the vignette");
        assert_eq!(s.selected_node_addr, Some(blur.clone()), "the moved effect is still the selected one");
        assert_eq!(keys(&s), had, "its keys moved with it");
        assert_eq!(s.active_history().undo_count(), steps + 1, "one step");
        undo(&mut s);
        assert_eq!(effects(&s, id), fx, "undo puts the order back");
        assert_eq!(s.selected_node_addr, Some(blur.clone()), "and keeps the selection");
        assert_eq!(keys(&s), had, "and the keys");
    }

    /// A pin drag taken back (Escape) leaves the picture exactly as the
    /// drag found it, the static axis's value and the keyed axis's new
    /// key alike, and records no step.
    #[test]
    fn an_abandoned_cornerpin_drag_leaves_the_picture_as_it_was() {
        let pin = (klipp_effects::registry::effect_type("corner_pin").unwrap().sample)();
        let (mut s, id) = state(&[pin]);
        let addr = at(id, &[clip(&s, id).effect_node_at(false, 0).unwrap()]);
        let key = |t: f64, v: f32| serde_json::to_value(klipp_core::Keyframe { time_secs: t, value: v, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }).unwrap();
        run(&mut s, AppCommand::AddKeyAt { addr: addr.clone(), key: "tl_x".into(), keyframe: key(0.0, 0.0) });
        let picture = |s: &AppState| serde_json::to_string(&clip(s, id).picture_v2().unwrap().picture).unwrap();
        let (before, steps) = (picture(&s), s.active_history().undo_count());
        assert_eq!(s.path_command(&AppCommand::BeginParamEditAt { addr: addr.clone(), key: "".into() }), Some(false));
        for frame in 1..=3 {
            let v = 0.1 * frame as f32;
            run(&mut s, AppCommand::SetParamAt { addr: addr.clone(), key: "tl_x".into(), value: ParamValue::Float(v) });
            run(&mut s, AppCommand::SetParamAt { addr: addr.clone(), key: "tl_y".into(), value: ParamValue::Float(v) });
            run(&mut s, AppCommand::AddKeyAt { addr: addr.clone(), key: "tl_x".into(), keyframe: key(1.0, v) });
        }
        assert_ne!(picture(&s), before, "the drag moved the pin");
        assert_eq!(s.path_command(&AppCommand::AbandonParamEditAt { addr: addr.clone(), key: "".into() }), Some(true));
        assert_eq!(picture(&s), before, "values and keys as the drag found them");
        assert_eq!(s.active_history().undo_count(), steps, "no step recorded");
        assert!(!s.active_history().is_any_drag_in_progress());
    }

    /// The gizmo's abandon by path, after it moved the clip, leaves the
    /// placement as the drag found it and records no step.
    #[test]
    fn an_abandoned_gizmo_drag_leaves_the_placement_as_it_was() {
        let (mut s, id) = state(&[E::Gamma { gamma: 1.1 }, transform(5.0, 1.0)]);
        let c = clip(&s, id).clone();
        let picture = |s: &AppState| serde_json::to_string(&clip(s, id).picture_v2().unwrap().picture).unwrap();
        let (before, steps) = (picture(&s), s.active_history().undo_count());
        let mut cmds = gizmo_drag(&c, transform(5.0, 1.0), transform(40.0, 0.6));
        let end = cmds.pop();
        assert!(matches!(end, Some(AppCommand::EndEffectDrag { .. })));
        cmds.push(AppCommand::AbandonEffectDrag { clip_id: id, effect_index: 1, added: false });
        for cmd in crate::inspector::placement_by_path::by_path(cmds, &c) {
            s.before_dispatch(&cmd);
            assert!(s.path_command(&cmd).is_some(), "{cmd:?} is a path command");
        }
        assert_eq!(picture(&s), before);
        assert_eq!(s.active_history().undo_count(), steps, "no step recorded");
    }

    /// The card by path reads what the clip draws: the first enabled
    /// Transform past a switched-off one, the last enabled of several
    /// Opacity entries. A scale edit built from what it shows sets the
    /// scale alone; the enabled Transform's position and rotation stay.
    #[test]
    fn the_card_shows_and_keeps_the_entries_the_clip_reads() {
        let tf = |x: f32, rotation: f32| E::Transform { position_x: x, position_y: 2.0, scale_x: 1.0, scale_y: 1.0, rotation, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None };
        let c = TestClip::of(fixture(&[])).effect(tf(20.0, 45.0)).off().effect(tf(8.0, 10.0)).effects([E::Opacity { value: 0.3 }, E::Opacity { value: 0.6 }]).effect(E::Opacity { value: 0.9 }).off().build();
        let id = c.id;
        let mut s = with(&[c]);
        let c = clip(&s, id).clone();
        let shown = crate::inspector::placement_by_path::placement(&c);
        assert_eq!((shown.position_x, shown.rotation), (8.0, 10.0), "the enabled Transform's");
        assert_eq!(crate::inspector::placement_by_path::opacity(&c), 0.6, "the last enabled Opacity's");
        let mut t = shown;
        (t.scale_x, t.scale_y) = (0.5, 0.5);
        let cmds = crate::inspector::placement_by_path::by_path(vec![AppCommand::UpdateBuiltinEffect { clip_id: id, index: 1, effect: t.to_builtin_effect() }], &c);
        assert!(matches!(&cmds[..], [AppCommand::SetParamAt { key, .. }] if key.as_str() == "scale"), "{cmds:?}");
        for cmd in cmds {
            run(&mut s, cmd);
        }
        let p = clip(&s, id).picture_v2().unwrap().picture.placement.clone();
        assert_eq!((p.pos_x, p.rotation, p.scale), (Some(8.0), Some(10.0), Some(0.5)));
    }

    /// A width edit by path keeps the clip's aspect, and a height edit
    /// sets the aspect alone (#1406).
    #[test]
    fn a_scale_edit_by_path_keeps_the_aspect() {
        let c = fixture(&[E::Transform { position_x: 0.0, position_y: 0.0, scale_x: 1.0, scale_y: 0.5, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None }]);
        let id = c.id;
        let mut s = with(&[c]);
        let c = clip(&s, id).clone();
        let mut t = crate::inspector::placement_by_path::placement(&c);
        assert_eq!((t.scale_x, t.scale_y), (1.0, 0.5));
        (t.scale_x, t.scale_y) = (0.8, crate::inspector::video_tab::keep_aspect(&t, 0.8));
        let cmds = crate::inspector::placement_by_path::by_path(vec![AppCommand::UpdateBuiltinEffect { clip_id: id, index: 0, effect: t.to_builtin_effect() }], &c);
        assert!(matches!(&cmds[..], [AppCommand::SetParamAt { key, .. }] if key.as_str() == "scale"), "{cmds:?}");
        for cmd in cmds {
            run(&mut s, cmd);
        }
        let c = clip(&s, id).clone();
        let p = c.picture_v2().unwrap().picture.placement.clone();
        assert_eq!((p.scale, p.aspect), (Some(0.8), Some(0.5)));
        assert!(matches!(test_clip::effects(&c)[0], E::Transform { scale_x, scale_y, perspective, .. } if (scale_x, scale_y, perspective) == (0.8, 0.4, 0.0)));
        let mut t = crate::inspector::placement_by_path::placement(&c);
        t.scale_y = 0.8;
        let cmds = crate::inspector::placement_by_path::by_path(vec![AppCommand::UpdateBuiltinEffect { clip_id: id, index: 0, effect: t.to_builtin_effect() }], &c);
        assert!(matches!(&cmds[..], [AppCommand::SetParamAt { key, .. }] if key.as_str() == "scale_y"), "{cmds:?}");
        for cmd in cmds {
            run(&mut s, cmd);
        }
        let p = clip(&s, id).picture_v2().unwrap().picture.placement.clone();
        assert_eq!((p.scale, p.aspect), (Some(0.8), None));
    }

    /// The card's Scale Y keys go to the clip's height lane by path, apart
    /// from the width's (#1471): the renderer draws the height from them,
    /// one undo takes a key back, and with none left the height follows
    /// the width at the aspect again.
    #[test]
    fn a_height_key_by_path_lands_on_its_own_lane() {
        let c = TestClip::of(fixture(&[])).effect(E::Transform { position_x: 0.0, position_y: 0.0, scale_x: 1.0, scale_y: 0.5, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None }).build();
        let id = c.id;
        let mut s = with(&[c]);
        let height = |s: &AppState, t: f64| {
            let c = clip(s, id);
            let x = klipp_compose::eval::evaluate_clip_own_transform(c, t, &klipp_compose::eval::eval_context(c, c.timeline_in, 24.0));
            (x.2, x.3)
        };
        for (t, v) in [(0.0, 0.25), (1.0, 0.75)] {
            let c = clip(&s, id).clone();
            let cmds = crate::inspector::placement_by_path::by_path(vec![AppCommand::AddVideoKeyframe { clip_id: id, param: klipp_core::VideoParam::ScaleY, time_secs: t, value: v }], &c);
            assert!(matches!(&cmds[..], [AppCommand::AddKeyAt { key, .. }] if key.as_str() == "scale_y"), "{cmds:?}");
            for cmd in cmds {
                run(&mut s, cmd);
            }
        }
        let c = clip(&s, id);
        assert_eq!(crate::inspector::placement_by_path::lane_keys(c, "scale_y").len(), 2);
        assert!(crate::inspector::placement_by_path::lane_keys(c, "scale").is_empty(), "the width's lane is its own");
        assert_eq!(height(&s, 0.5), (1.0, 0.5));
        assert_eq!(height(&s, 0.25), (1.0, 0.375), "between the keys, the height's");
        undo(&mut s);
        assert_eq!(crate::inspector::placement_by_path::lane_keys(clip(&s, id), "scale_y").len(), 1, "one undo, one key");
        let c = clip(&s, id).clone();
        let cmds = crate::inspector::placement_by_path::by_path(vec![AppCommand::RemoveVideoKeyframe { clip_id: id, param: klipp_core::VideoParam::ScaleY, time_secs: 0.0 }], &c);
        for cmd in cmds {
            run(&mut s, cmd);
        }
        assert!(crate::inspector::placement_by_path::lane_keys(clip(&s, id), "scale_y").is_empty());
        assert_eq!(height(&s, 0.25), (1.0, 0.5), "the width at the aspect again");
    }

    /// A key put by path where one already is keeps that key's time and
    /// curve shape, as the lanes' own upsert does: the card's placement
    /// and opacity keys, field-backed and on a picture-truth clip, and the
    /// pin's keys as the viewer builds them.
    #[test]
    fn a_key_set_by_path_keeps_its_curve_shape() {
        let bezier = |t: f64, v: f32| klipp_core::Keyframe { time_secs: t, value: v, interpolation: klipp_core::Interpolation::Bezier, tangent_out: (0.1, 0.2), tangent_in: (-0.3, 0.4), handles: Default::default() };
        for truth in [false, true] {
            let mut c = fixture(&[E::GaussianBlur { radius: 2.0 }]);
            c.edit_video_param_keyframes(klipp_core::VideoParam::PositionX, |k| k.push(bezier(0.5, 1.0)));
            c.edit_video_param_keyframes(klipp_core::VideoParam::Opacity, |k| k.push(bezier(0.5, 0.4)));
            let id = c.id;
            let mut s = with(&[c]);
            if truth {
                let blur = effects(&s, id)[0];
                run(&mut s, AppCommand::SetMixAt { addr: at(id, &[blur]), mix: Some(0.5) });
                assert!(clip(&s, id).is_picture_truth());
            }
            let c = clip(&s, id).clone();
            let card = vec![
                AppCommand::AddVideoKeyframe { clip_id: id, param: klipp_core::VideoParam::PositionX, time_secs: 0.5 + 1e-8, value: 7.0 },
                AppCommand::AddVideoKeyframe { clip_id: id, param: klipp_core::VideoParam::Opacity, time_secs: 0.5 + 1e-8, value: 0.8 },
            ];
            for cmd in crate::inspector::placement_by_path::by_path(card, &c) {
                run(&mut s, cmd);
            }
            let picture = clip(&s, id).picture_v2().unwrap();
            let p = &picture.picture;
            for (anim, value) in [(&p.placement_anim[&ParamKey::from("pos_x")], 7.0), (p.opacity_anim.as_ref().unwrap(), 0.8)] {
                let keys: Vec<klipp_core::Keyframe> = anim.keys.iter().map(|k| serde_json::from_value(k.clone()).unwrap()).collect();
                assert_eq!(keys, [klipp_core::Keyframe { value, ..bezier(0.5, 0.0) }], "truth {truth}");
            }
        }
        // The pin's: its keys as the viewer reads them (by its node), the
        // key built from them.
        let pin = (klipp_effects::registry::effect_type("corner_pin").unwrap().sample)();
        let (mut s, id) = state(&[pin]);
        let addr = at(id, &[clip(&s, id).effect_node_at(false, 0).unwrap()]);
        run(&mut s, AppCommand::AddKeyAt { addr: addr.clone(), key: "tl_x".into(), keyframe: serde_json::to_value(bezier(0.5, 0.1)).unwrap() });
        let keys = clip(&s, id).node_key_state(&addr, &"tl_x".into()).unwrap().keys;
        let keyframe = serde_json::to_value(klipp_core::keyframe::upserted(&keys, 0.5 + 1e-8, 0.3)).unwrap();
        run(&mut s, AppCommand::AddKeyAt { addr: addr.clone(), key: "tl_x".into(), keyframe });
        assert_eq!(clip(&s, id).node_key_state(&addr, &"tl_x".into()).unwrap().keys, [klipp_core::Keyframe { value: 0.3, ..bezier(0.5, 0.0) }]);
    }

    /// The engine's side of an edit, left out: these tests read the state.
    struct NoSide;
    impl super::super::inspector_edit_host::InspectorSideEffects for NoSide {
        fn sync_sequence(&self, _: &AppState) {}
        fn send(&self, _: AppCommand) {}
    }

    /// The commands the viewer's gizmo sends for one drag that moves and
    /// scales the clip (`panels/viewer.rs`), from `initial` to `moved`.
    fn gizmo_drag(c: &klipp_timeline::Clip, initial: E, moved: E) -> Vec<AppCommand> {
        let list = test_clip::effects(c);
        let index = list.iter().position(|e| matches!(e, E::Transform { .. }));
        let key = format!("viewer_transform_{}", c.id.0);
        let idx = index.unwrap_or(list.len());
        let mut cmds = Vec::new();
        if index.is_none() {
            cmds.push(AppCommand::AddBuiltinEffect { clip_id: c.id, effect: initial.clone() });
        }
        cmds.push(AppCommand::BeginEffectDrag { key: key.clone(), clip_id: c.id, effect_index: idx, old_effect: initial });
        cmds.push(AppCommand::UpdateBuiltinEffect { clip_id: c.id, index: idx, effect: moved.clone() });
        cmds.push(AppCommand::UpdateBuiltinEffect { clip_id: c.id, index: idx, effect: moved.clone() });
        cmds.push(AppCommand::EndEffectDrag { key, clip_id: c.id, effect_index: idx, new_effect: moved });
        cmds
    }

    fn transform(x: f32, scale: f32) -> E {
        E::Transform { position_x: x, position_y: -4.0, scale_x: scale, scale_y: scale, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None }
    }

    /// M3 step 13: the gizmo's drag by path (behind the switch) gives the
    /// picture the old drag gave, though the old one added a Transform
    /// entry and the new one sets the clip's placement: the typed pictures
    /// equal (each added entry's id read as the other's), one undo step
    /// each, from a clip with a Transform and from one without.
    #[test]
    fn a_gizmo_drag_gives_the_same_picture() {
        for start in [vec![E::Gamma { gamma: 1.1 }], vec![E::Gamma { gamma: 1.1 }, transform(5.0, 1.0)]] {
            let c0 = fixture(&start);
            let id = c0.id;
            let (mut old, mut new) = (with(std::slice::from_ref(&c0)), with(std::slice::from_ref(&c0)));
            let c = clip(&old, id).clone();
            let identity = crate::inspector::video_tab::TransformParams::default().to_builtin_effect();
            let initial = test_clip::effects(&c).into_iter().find(|e| matches!(e, E::Transform { .. })).unwrap_or(identity);
            let cmds = gizmo_drag(&c, initial, transform(40.0, 0.6));
            let steps = old.active_history().undo_count();
            for cmd in &cmds {
                old.before_dispatch(cmd);
                assert!(super::super::inspector_edit_host::InspectorEditHost { state: &mut old, side: &NoSide }.apply(cmd), "{cmd:?}");
            }
            for cmd in crate::inspector::placement_by_path::by_path(cmds, &c) {
                new.before_dispatch(&cmd);
                assert!(new.path_command(&cmd).is_some(), "{cmd:?} is a path command");
            }
            assert_eq!(old.active_history().undo_count(), steps + 1);
            assert_eq!(new.active_history().undo_count(), steps + 1, "one undo step");
            let (a, b) = (clip(&old, id), clip(&new, id));
            let mut pb = serde_json::to_string(&b.picture_v2().unwrap().picture).unwrap();
            for i in 0..test_clip::effects(a).len() {
                if let (Some(x), Some(y)) = (b.effect_node_at(false, i), a.effect_node_at(false, i)) {
                    pb = pb.replace(&x.to_string(), &y.to_string());
                }
            }
            assert_eq!(pb, serde_json::to_string(&a.picture_v2().unwrap().picture).unwrap(), "from {start:?}");
            assert_eq!(b.picture_v2().unwrap().picture.placement.pos_x, Some(40.0));
        }
    }

    /// The card's keys by path land on the placement node: the picture's
    /// placement animation, from the clip's lane.
    #[test]
    fn video_tab_keys_land_on_the_placement_node() {
        let (mut s, id) = state(&[E::Gamma { gamma: 1.1 }]);
        let c = clip(&s, id).clone();
        let card = vec![AppCommand::AddVideoKeyframe { clip_id: id, param: klipp_core::VideoParam::PositionX, time_secs: 0.5, value: 12.0 }];
        let cmds = crate::inspector::placement_by_path::by_path(card, &c);
        assert!(matches!(&cmds[..], [AppCommand::AddKeyAt { key, .. }] if key.as_str() == "pos_x"), "{cmds:?}");
        for cmd in &cmds {
            run(&mut s, cmd.clone());
        }
        let picture = clip(&s, id).picture_v2().unwrap();
        let keys = &picture.picture.placement_anim[&ParamKey::from("pos_x")].keys;
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0]["value"], serde_json::json!(12.0));
    }

    /// IC3: the card's reset and a generated row's reset of the
    /// perspective set the same value, the catalog's default (0, read as
    /// 1), so the two never disagree.
    #[test]
    fn perspective_row_and_card_reset_agree() {
        let mut tilted = transform(0.0, 1.0);
        if let E::Transform { perspective, .. } = &mut tilted {
            *perspective = 2.0;
        }
        let c0 = fixture(&[tilted]);
        let id = c0.id;
        let (mut card, mut row) = (with(std::slice::from_ref(&c0)), with(std::slice::from_ref(&c0)));
        let c = clip(&card, id).clone();
        let reset = vec![AppCommand::UpdateBuiltinEffect { clip_id: id, index: 0, effect: crate::inspector::video_tab::TransformParams::default().to_builtin_effect() }];
        for cmd in crate::inspector::placement_by_path::by_path(reset, &c) {
            run(&mut card, cmd);
        }
        let default = klipp_effects::registry::effect_type("transform").unwrap().desc.params.iter().find(|p| p.field == "perspective").unwrap().default;
        let addr = c.placement_path().unwrap();
        run(&mut row, AppCommand::SetParamAt { addr, key: "perspective".into(), value: ParamValue::Float(default) });
        let read = |s: &AppState| clip(s, id).picture_v2().unwrap().picture.placement.perspective;
        assert_eq!(read(&card), Some(0.0));
        assert_eq!(read(&card), read(&row));
    }

    /// State held by address belongs to the clip it names: a selection,
    /// a compared node and a dock root on one clip aren't another's, even
    /// where the local node ids agree.
    #[test]
    fn two_clips_with_the_same_local_node_id_do_not_share_state() {
        let (mut s, a) = state(&[E::GaussianBlur { radius: 3.0 }]);
        let v1 = s.sequences[0].tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        let mut copy = clip(&s, a).clone();
        copy.id = ClipId::new();
        let b = copy.id;
        s.sequences[0].tracks[v1].clips.push(copy);
        let node = effects(&s, a)[0];
        assert_eq!(s.path_command(&AppCommand::RevealGraph { path: at(a, &[node]), select: true }), Some(false));
        assert_eq!(s.path_command(&AppCommand::SetSplitCompareNode { clip_id: Some(a), path: Some(at(a, &[node])) }), Some(false));
        assert_eq!(s.selected_node_addr.as_ref().map(|p| p.root), Some(Root::Clip { clip: a.0 }));
        assert_ne!(s.selected_node_addr, Some(at(b, &[node])), "B's node of the same id isn't selected");
        assert_eq!(s.split_compare_node.as_ref().map(|(c, _)| *c), Some(a));
        // A path naming A's node in B's graph is refused.
        assert_eq!(s.path_command(&AppCommand::SetSplitCompareNode { clip_id: Some(b), path: Some(at(a, &[node])) }), Some(false));
        assert_eq!(s.split_compare_node.as_ref().map(|(c, _)| *c), Some(a), "unchanged");
    }

    /// Mix, mask channel and mask feed by path: each one step, the clip
    /// picture-truth after, and undo brings back the clip as it was; a
    /// type that takes no mix is refused with nothing changed.
    #[test]
    fn mix_and_masks_by_path_are_one_step_each() {
        let c = fixture(&[E::GaussianBlur { radius: 3.0 }, E::Invert { mix: 1.0 }]);
        let id = c.id;
        let base = with(std::slice::from_ref(&c));
        let ids = effects(&base, id);
        for cmd in [
            AppCommand::SetMixAt { addr: at(id, &[ids[0]]), mix: Some(0.5) },
            AppCommand::SetMaskChannelAt { addr: at(id, &[ids[0]]), channel: Some("alpha".into()) },
        ] {
            let mut s = with(std::slice::from_ref(&c));
            run(&mut s, cmd.clone());
            assert_eq!(s.active_history().undo_count(), base.active_history().undo_count() + 1, "{cmd:?}");
            assert!(clip(&s, id).is_picture_truth(), "{cmd:?}: the picture holds it");
            undo(&mut s);
            assert!(!clip(&s, id).is_picture_truth(), "{cmd:?}: undone");
            assert_eq!(picture(&s, id), picture(&base, id));
        }
        let mut s = with(std::slice::from_ref(&c));
        let before = picture(&s, id);
        assert_eq!(s.path_command(&AppCommand::SetMixAt { addr: at(id, &[ids.get(1).copied().unwrap_or(ids[0])]), mix: Some(0.5) }), Some(false), "Invert takes no mix");
        assert_eq!(picture(&s, id), before);
    }

    /// `RevealGraph` is idempotent and never closes the dock; it selects
    /// on request; an address that names nothing is refused with no change.
    /// `SetGraphDockRoot` sets the root and goes back to its top level.
    #[test]
    fn reveal_graph_and_the_dock_root() {
        let (mut s, id) = state(&[E::GaussianBlur { radius: 3.0 }]);
        let node = effects(&s, id)[0];
        s.graph_dock_path = vec![uuid::Uuid::from_u128(7)];
        for _ in 0..2 {
            assert_eq!(s.path_command(&AppCommand::RevealGraph { path: at(id, &[node]), select: false }), Some(false));
            assert!(s.graph_dock_open);
            assert_eq!(s.graph_dock_root, Some(Root::Clip { clip: id.0 }));
            assert!(s.graph_dock_path.is_empty());
            assert_eq!(s.selected_node_addr, None);
        }
        s.path_command(&AppCommand::RevealGraph { path: at(id, &[node]), select: true });
        assert_eq!(s.selected_node_addr, Some(at(id, &[node])));
        let held = (s.graph_dock_open, s.graph_dock_root, s.graph_dock_path.clone(), s.selected_node_addr.clone());
        s.path_command(&AppCommand::RevealGraph { path: at(id, &[uuid::Uuid::from_u128(9)]), select: true });
        assert_eq!((s.graph_dock_open, s.graph_dock_root, s.graph_dock_path.clone(), s.selected_node_addr.clone()), held, "refused, unchanged");
        s.graph_dock_path = vec![node];
        s.path_command(&AppCommand::SetGraphDockRoot { root: Root::Clip { clip: id.0 } });
        assert!(s.graph_dock_path.is_empty());
        s.path_command(&AppCommand::SetGraphDockRoot { root: Root::Clip { clip: uuid::Uuid::from_u128(5) } });
        assert_eq!(s.graph_dock_root, Some(Root::Clip { clip: id.0 }), "a clip not here is refused");
    }

    /// Ids are kept (M3 proposal 6 is in): the node an add names is the
    /// node the clip holds, and a move renames nothing, so a selection made
    /// before the move still names the moved effect.
    #[test]
    fn ids_are_kept_by_add_and_move() {
        let (mut s, id) = state(&[E::GaussianBlur { radius: 3.0 }, E::Sharpen { amount: 0.5 }]);
        let ids = effects(&s, id);
        let mut node = clip(&s, id).picture_v2().unwrap().picture.graph.nodes.iter().find(|n| n.id == ids[1]).cloned().unwrap();
        node.id = uuid::Uuid::from_u128(0xadd);
        node.legacy.as_mut().unwrap().payload = serde_json::to_value(E::Gamma { gamma: 1.1 }).unwrap();
        run(&mut s, AppCommand::AddNodeAt { after: at(id, &[ids[1]]), node: Box::new(node) });
        assert_eq!(effects(&s, id), vec![ids[0], ids[1], uuid::Uuid::from_u128(0xadd)], "the clip keeps the add's id");
        s.path_command(&AppCommand::RevealGraph { path: at(id, &[ids[1]]), select: true });
        let order_before = effects(&s, id);
        run(&mut s, AppCommand::MoveNodeAt { addr: at(id, &[ids[1]]), after: uuid::Uuid::from_u128(0xadd) });
        let mut moved = effects(&s, id);
        assert_eq!(moved, vec![ids[0], uuid::Uuid::from_u128(0xadd), ids[1]], "moved, every id the same");
        moved.sort();
        let mut was = order_before;
        was.sort();
        assert_eq!(moved, was);
        assert_eq!(s.selected_node_addr, Some(at(id, &[ids[1]])), "the selection still names it");
    }

    #[test]
    fn removing_a_mask_by_path_reports_its_reader_and_undo_restores_it() {
        let mut c = fixture(&[]);
        let mut grade = klipp_nodes::NodeGraph::default();
        let (mask, reader) = grade.ensure_masked_secondary(klipp_nodes::NodeKind::Corrector);
        grade.nodes.iter_mut().find(|n| n.id == reader).unwrap().label = "Skin correction".into();
        c.set_grade(grade);
        let p = c.picture_v2().unwrap();
        let group = p.picture.graph.nodes.iter().find(|n| n.group.as_ref().is_some_and(|b| b.graph.nodes.iter().any(|n| n.id == mask))).unwrap().id;
        let addr = at(c.id, &[group, mask]);
        let mut s = with(std::slice::from_ref(&c));
        let before = picture(&s, c.id);
        let outcome = graph_ops::graph_ops_edit(&s.sequences[0], &[GraphOp::RemoveNode { path: addr.clone() }]).unwrap();
        assert_eq!(outcome.ungated, vec![klipp_graph_lift::edit_planner::Ungated { path: at(c.id, &[group, reader]), label: "Skin correction".into() }]);
        let count = s.active_history().undo_count();
        run(&mut s, AppCommand::RemoveNodeAt { addr });
        assert_eq!(s.active_history().undo_count(), count + 1);
        assert_eq!(s.toast.message_for_test(), Some("1 node lost their mask: Skin correction"));
        undo(&mut s);
        assert_eq!(picture(&s, c.id), before);
    }

    /// A clip with a Corrector in its grade, and the Corrector's address:
    /// inside the clip's grade group.
    fn graded() -> (klipp_timeline::Clip, NodePath) {
        let mut c = fixture(&[E::GaussianBlur { radius: 3.0 }]);
        let mut g = klipp_nodes::NodeGraph::default();
        g.ensure_corrector();
        let corrector = g.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Corrector).unwrap().id;
        c.set_grade(g);
        let p = c.picture_v2().unwrap().picture.clone();
        let group = p.graph.nodes.iter().find(|n| n.group.as_ref().is_some_and(|b| b.graph.nodes.iter().any(|i| i.id == corrector))).expect("the grade group holds the Corrector").id;
        let addr = at(c.id, &[group, corrector]);
        (c, addr)
    }

    /// A grade node's scalar parameter set by its bare id (`SetNodeParam`,
    /// `apply_override` on the graph) and by path (`SetParamAt` on
    /// `[group, node]`): the same picture, and one undo back. The Corrector
    /// named without its group is refused.
    #[test]
    fn a_grade_param_by_path_is_the_param_by_id() {
        let (c, addr) = graded();
        let node = addr.node().unwrap();
        for (param, value) in [("exposure", 0.7f32), ("contrast", 0.2), ("saturation", 1.4), ("exposure", 0.0), ("qual_hue_width", 40.0), ("luma_key_low", 0.2), ("wheels_log_mode", 1.0)] {
            let mut a = with(std::slice::from_ref(&c));
            let seq = &mut a.sequences[0];
            let clip_a = seq.tracks.iter_mut().find_map(|t| t.find_clip_mut(c.id)).unwrap();
            clip_a.edit_grade(|root| root.nodes.iter_mut().find(|n| n.id == node).unwrap().params.apply_override(param, value));
            let mut b = with(std::slice::from_ref(&c));
            run(&mut b, AppCommand::SetParamAt { addr: addr.clone(), key: param.into(), value: ParamValue::Float(value) });
            assert_eq!(picture(&a, c.id), picture(&b, c.id), "{param}: the same picture");
            undo(&mut b);
            assert_eq!(picture(&b, c.id), picture(&with(std::slice::from_ref(&c)), c.id), "{param}: undone");
        }
        let mut a = with(std::slice::from_ref(&c));
        let seq = &mut a.sequences[0];
        let clip_a = seq.tracks.iter_mut().find_map(|t| t.find_clip_mut(c.id)).unwrap();
        clip_a.edit_grade(|root| root.nodes.iter_mut().find(|n| n.id == node).unwrap().enabled = false);
        let mut b = with(std::slice::from_ref(&c));
        run(&mut b, AppCommand::SetNodeEnabledAt { addr: addr.clone(), enabled: false });
        assert_eq!(picture(&a, c.id), picture(&b, c.id), "toggled by path as by id");
        let mut s = with(std::slice::from_ref(&c));
        let before = picture(&s, c.id);
        assert_eq!(s.path_command(&AppCommand::SetParamAt { addr: at(c.id, &[node]), key: "exposure".into(), value: ParamValue::Float(0.5) }), Some(false), "the group skipped: refused");
        assert_eq!(picture(&s, c.id), before);
    }


    /// A grade node's struct parameter set whole by its bare id
    /// (`SetNodeQualifier`) and by path (`SetPayloadAt`, the node's stored
    /// entry with that one struct changed): the same picture.
    #[test]
    fn a_grade_struct_by_path_is_the_struct_by_id() {
        let (c, addr) = graded();
        let node = addr.node().unwrap();
        let q = klipp_core::QualifierSettings { enabled: true, hue_center: 30.0, hue_width: 40.0, ..Default::default() };
        let mut a = with(std::slice::from_ref(&c));
        let clip_a = a.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(c.id)).unwrap();
        clip_a.edit_grade(|root| root.nodes.iter_mut().find(|n| n.id == node).unwrap().params.qualifier = Some(q.clone()));
        let entry = |s: &AppState| {
            let p = clip(s, c.id).picture_v2().unwrap();
            let group = p.picture.graph.nodes.iter().find(|n| n.id == addr.nodes[0]).unwrap().clone();
            group.group.unwrap().graph.nodes.iter().find(|n| n.id == node).unwrap().legacy.clone().unwrap().payload
        };
        let mut b = with(std::slice::from_ref(&c));
        let mut payload = entry(&b);
        payload["params"]["qualifier"] = serde_json::to_value(&q).unwrap();
        run(&mut b, AppCommand::SetPayloadAt { addr: addr.clone(), payload });
        assert_eq!(picture(&a, c.id), picture(&b, c.id), "the qualifier by path as by id");
        undo(&mut b);
        assert_eq!(picture(&b, c.id), picture(&with(std::slice::from_ref(&c)), c.id), "undone");
    }


    /// A grade node's struct field set by path (`SetParamAt` with
    /// `struct.field`) is that struct set whole by id, made at its default
    /// when the node had none.
    #[test]
    fn a_grade_struct_field_by_path_is_the_struct_by_id() {
        let (c, addr) = graded();
        let node = addr.node().unwrap();
        let mut a = with(std::slice::from_ref(&c));
        let clip_a = a.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(c.id)).unwrap();
        clip_a.edit_grade(|root| {
            let params = &mut root.nodes.iter_mut().find(|n| n.id == node).unwrap().params;
            params.grain = Some(klipp_core::FilmGrainSettings { amount: 0.4, ..Default::default() });
        });
        let mut b = with(std::slice::from_ref(&c));
        run(&mut b, AppCommand::SetParamAt { addr: addr.clone(), key: "grain.amount".into(), value: ParamValue::Float(0.4) });
        assert_eq!(picture(&a, c.id), picture(&b, c.id), "the grain's amount by path as the grain by id");
    }

    /// A grade node's key by path (`AddKeyAt`, `RemoveKeyAt` on
    /// `[group, node]`) is its key by id (`AddKeyframe`, `RemoveKeyframe`):
    /// the same picture, the key on the node's own track.
    #[test]
    fn a_grade_key_by_path_is_the_key_by_id() {
        let (c, addr) = graded();
        let node = addr.node().unwrap();
        let key = klipp_core::Keyframe { time_secs: 0.5, value: 0.7, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
        let mut a = with(std::slice::from_ref(&c));
        let clip_a = a.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(c.id)).unwrap();
        clip_a.edit_grade(|root| root.nodes.iter_mut().find(|n| n.id == node).unwrap().keyframes.track_mut("exposure").set_keyframe(0.5, 0.7));
        let mut b = with(std::slice::from_ref(&c));
        run(&mut b, AppCommand::AddKeyAt { addr: addr.clone(), key: "exposure".into(), keyframe: serde_json::to_value(key).unwrap() });
        assert_eq!(picture(&a, c.id), picture(&b, c.id), "keyed by path as by id");
        let tracks = &klipp_timeline::GradeAccess::grade(clip(&b, c.id)).nodes.iter().find(|n| n.id == node).unwrap().keyframes;
        assert_eq!(tracks.evaluate_all(0.5), vec![("exposure".to_string(), 0.7)], "the key is on the node's track");
        let clip_a = a.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(c.id)).unwrap();
        clip_a.edit_grade(|root| root.nodes.iter_mut().find(|n| n.id == node).unwrap().keyframes.track_mut("exposure").remove_keyframe(0.5));
        run(&mut b, AppCommand::RemoveKeyAt { addr: addr.clone(), key: "exposure".into(), time_secs: 0.5 });
        assert_eq!(picture(&a, c.id), picture(&b, c.id), "the key taken out by path as by id");
    }

    /// A grade key set by path is kept: it comes back with the project
    /// after a save and a load, and the grade renders it, the keyed
    /// frame the same as one graded to that value outright.
    #[test]
    fn a_grade_key_by_path_survives_a_save_and_renders() {
        let (c, addr) = graded();
        let node = addr.node().unwrap();
        let mut s = with(std::slice::from_ref(&c));
        for (t, v) in [(0.0, 0.0f32), (1.0, 1.0)] {
            let key = klipp_core::Keyframe { time_secs: t, value: v, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
            run(&mut s, AppCommand::AddKeyAt { addr: addr.clone(), key: "exposure".into(), keyframe: serde_json::to_value(key).unwrap() });
        }
        let dir = std::env::temp_dir().join(format!("klipp_grade_key_by_path_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        klipp_project::save_project(&dir, &s.sequences, &s.media_pool).unwrap();
        let (sequences, ..) = klipp_project::load_project(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let loaded = sequences[0].all_clips().find(|x| x.id == c.id).unwrap();
        let graph = klipp_timeline::GradeAccess::grade(loaded).clone();
        let keys = &graph.nodes.iter().find(|n| n.id == node).unwrap().keyframes;
        assert_eq!(keys.evaluate_all(0.5), vec![("exposure".to_string(), 0.5)], "the key came back with the project");
        // Rendered at 0.5 s, the keyed grade is the grade at +0.5 stops.
        let mut outright = graph.clone();
        let n = outright.nodes.iter_mut().find(|n| n.id == node).unwrap();
        n.keyframes = Default::default();
        n.params.apply_override("exposure", 0.5);
        let frame: Vec<u8> = (0..16 * 16).flat_map(|i| [(i % 251) as u8, 90, 160, 255]).collect();
        let mut ex = klipp_nodes::executor::GraphExecutor::new_cpu_only();
        let (keyed, _) = ex.execute(&frame, &graph, 16, 16, 0.5, 12, None, None).unwrap();
        let (set, _) = ex.execute(&frame, &outright, 16, 16, 0.5, 12, None, None).unwrap();
        assert_ne!(keyed, frame, "the key grades the frame");
        assert_eq!(keyed, set, "the keyed frame is the frame graded to the key's value");
    }

    /// A grade key is moved and given its interpolation by path, as an
    /// effect's is: `MoveKeyAt` puts it at its new time and value, and
    /// `SetInterpKeyAt` sets it as the keyframe editor does
    /// (`set_interpolation_at`), on the node's own track. The same on a
    /// merge lifted from the grade (a Blend), which keeps its keys there too.
    #[test]
    fn a_grade_key_moves_and_takes_its_interpolation_by_path() {
        for (kind, key) in [(klipp_nodes::NodeKind::Corrector, "exposure"), (klipp_nodes::NodeKind::Blend, "mix")] {
            let (mut c, corrector_addr) = graded();
            let node = if kind == klipp_nodes::NodeKind::Corrector {
                corrector_addr.node().unwrap()
            } else {
                let n = klipp_nodes::Node { id: klipp_nodes::NodeId::new_v4(), kind, label: String::new(), enabled: true, params: Default::default(), keyframes: Default::default(), output_link: None, input_links: Vec::new() };
                let id = n.id;
                assert!(klipp_timeline::GradeAccess::insert_grade_node(&mut c, corrector_addr.node(), n), "{kind:?} in the grade");
                id
            };
            let p = c.picture_v2().unwrap().picture.clone();
            let lifted = p.graph.nodes.iter().find(|n| n.id == corrector_addr.nodes[0]).unwrap().group.as_ref().unwrap().graph.nodes.iter().find(|n| n.id == node).unwrap().type_id.clone();
            let addr = at(c.id, &[corrector_addr.nodes[0], node]);
            let mut s = with(std::slice::from_ref(&c));
            for (t, v) in [(0.0, 0.0f32), (1.0, 1.0)] {
                let k = klipp_core::Keyframe { time_secs: t, value: v, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
                run(&mut s, AppCommand::AddKeyAt { addr: addr.clone(), key: key.into(), keyframe: serde_json::to_value(k).unwrap() });
            }
            run(&mut s, AppCommand::MoveKeyAt { addr: addr.clone(), key: key.into(), time_secs: 1.0, new_time_secs: 1.5, value: 0.8 });
            run(&mut s, AppCommand::SetInterpKeyAt { addr: addr.clone(), key: key.into(), time_secs: 1.5, interpolation: klipp_core::Interpolation::Bezier });
            let track = klipp_timeline::GradeAccess::grade(clip(&s, c.id)).nodes.iter().find(|n| n.id == node).unwrap().keyframes.tracks.iter().find(|t| t.param_name == key).unwrap().keyframes.clone();
            let mut want = vec![
                klipp_core::Keyframe { time_secs: 0.0, value: 0.0, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() },
                klipp_core::Keyframe { time_secs: 1.5, value: 0.8, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() },
            ];
            klipp_core::keyframe::set_interpolation_at(&mut want, 1, klipp_core::Interpolation::Bezier);
            assert_eq!(track, want, "{kind:?} ({lifted}) {key}");
        }
    }

    /// An opacity preset picked on the Video tab of a clip with no
    /// Opacity effect and another effect (a Blur) first in its list: the
    /// tab sends the opacity entry's update at index 0 (its fallback behind
    /// the switch), and its by-path translation sets the clip's opacity
    /// anchor, never that index's effect. The Blur is unchanged, the opacity
    /// takes the preset's value, and it is one undo step (M3 step 14).
    #[test]
    fn an_opacity_preset_without_an_opacity_effect_leaves_the_first_effect_alone() {
        let c = fixture(&[E::GaussianBlur { radius: 3.0 }]);
        let mut s = with(std::slice::from_ref(&c));
        let blur = test_clip::effects(clip(&s, c.id))[0].clone();
        let sent = vec![AppCommand::UpdateBuiltinEffect { clip_id: c.id, index: 0, effect: E::Opacity { value: 0.4 } }];
        let translated = crate::inspector::placement_by_path::by_path(sent, clip(&s, c.id));
        assert!(translated.iter().all(|cmd| !matches!(cmd, AppCommand::UpdateBuiltinEffect { .. })), "by path, never the list's index: {translated:?}");
        for cmd in &translated {
            run(&mut s, cmd.clone());
        }
        assert_eq!(test_clip::effects(clip(&s, c.id))[0], blur, "the Blur at index 0 is unchanged");
        assert!((crate::inspector::placement_by_path::opacity(clip(&s, c.id)) - 0.4).abs() < 1e-6, "the opacity takes the preset");
        assert_eq!(s.active_history().undo_count(), 1, "one undo step");
    }

    /// #1327: a background result (a learnt noise profile, set on the
    /// clip's audio effect) arriving while a path drag is open is a step of
    /// its own: the drag closes first, the drag's later end adds nothing,
    /// and undo takes back the result alone, then the drag; redo puts
    /// both back. Undone and redone as the app does, by the document.
    #[test]
    fn a_result_during_a_path_drag_is_its_own_step() {
        let mut c = fixture(&[E::GaussianBlur { radius: 3.0 }]);
        c.audio_effects.push(klipp_core::AudioEffectInstance { effect_type: "denoise".into(), enabled: true, wet_dry: 1.0, params: vec![("strength".into(), 0.2)] });
        let ids = effects(&with(std::slice::from_ref(&c)), c.id);
        let blur = at(c.id, &[ids[0]]);
        let mut s = with(std::slice::from_ref(&c));
        let audio = |s: &AppState| clip(s, c.id).audio_effects[0].params.clone();
        let (picture0, audio0) = (picture(&s, c.id), audio(&s));
        s.path_command(&AppCommand::BeginParamEditAt { addr: blur.clone(), key: "radius".into() });
        run(&mut s, AppCommand::SetParamAt { addr: blur.clone(), key: "radius".into(), value: ParamValue::Float(9.0) });
        let picture1 = picture(&s, c.id);
        assert!(s.set_audio_effect_params(c.id, 0, &[("strength".into(), 0.8)], "Learn Noise Profile"), "the result lands");
        let audio1 = audio(&s);
        s.path_command(&AppCommand::EndParamEditAt { addr: blur.clone(), key: "radius".into() });
        assert_eq!(s.active_history().undo_count(), 2, "the drag and the result, two steps");
        let step = |s: &mut AppState, back: bool| {
            let entry = if back { s.active_history_mut().undo() } else { s.active_history_mut().redo() }.expect("a step");
            let seq = s.active_sequence_idx;
            let mut view = klipp_document::apply::DocumentView { sequences: &mut s.sequences, active: seq, media_pool: &mut s.media_pool, fps: &mut s.fps };
            if back { view.apply_inverse(&entry.command); } else { view.apply_forward(&entry.command); }
        };
        step(&mut s, true);
        assert_eq!((picture(&s, c.id), audio(&s)), (picture1.clone(), audio0.clone()), "undo takes back the result alone");
        step(&mut s, true);
        assert_eq!((picture(&s, c.id), audio(&s)), (picture0, audio0), "then the drag");
        step(&mut s, false);
        step(&mut s, false);
        assert_eq!((picture(&s, c.id), audio(&s)), (picture1, audio1), "redo puts both back");
    }

    /// A drag a result closes while its row is still held (#1338) goes on
    /// as one more drag: the rest of its ticks are one step, not one each,
    /// and each half undoes exactly.
    #[test]
    fn a_drag_held_past_a_result_is_two_steps() {
        let c = fixture(&[E::GaussianBlur { radius: 3.0 }]);
        let ids = effects(&with(std::slice::from_ref(&c)), c.id);
        let blur = at(c.id, &[ids[0]]);
        let mut s = with(std::slice::from_ref(&c));
        let tick = |s: &mut AppState, r: f32| run(s, AppCommand::SetParamAt { addr: blur.clone(), key: "radius".into(), value: ParamValue::Float(r) });
        let picture0 = picture(&s, c.id);
        s.path_command(&AppCommand::BeginParamEditAt { addr: blur.clone(), key: "radius".into() });
        tick(&mut s, 5.0);
        s.close_path_drag_before_edit();
        let picture1 = picture(&s, c.id);
        for r in [6.0, 7.0, 8.0] {
            tick(&mut s, r);
        }
        let picture2 = picture(&s, c.id);
        s.path_command(&AppCommand::EndParamEditAt { addr: blur.clone(), key: "radius".into() });
        assert_eq!(picture(&s, c.id), picture2, "the release keeps the last tick");
        assert_eq!(s.active_history().undo_count(), 2, "before the result and after it, two steps");
        assert!(s.resumable_path_drag.is_none(), "the release forgets the drag");
        undo(&mut s);
        assert_eq!(picture(&s, c.id), picture1, "undo takes back the part after the result");
        undo(&mut s);
        assert_eq!(picture(&s, c.id), picture0, "then the part before");
        // Released, or cancelled, with no tick after the result: one step,
        // and the drag forgotten, so a later typed edit of the row is its
        // own step and reopens no gesture.
        for close in [AppCommand::EndParamEditAt { addr: blur.clone(), key: "radius".into() }, AppCommand::AbandonParamEditAt { addr: blur.clone(), key: "radius".into() }] {
            let mut s = with(std::slice::from_ref(&c));
            s.path_command(&AppCommand::BeginParamEditAt { addr: blur.clone(), key: "radius".into() });
            tick(&mut s, 5.0);
            s.close_path_drag_before_edit();
            s.path_command(&close);
            assert_eq!(s.active_history().undo_count(), 1, "{close:?}");
            assert!(s.resumable_path_drag.is_none(), "{close:?} forgets the drag");
            tick(&mut s, 7.0);
            assert_eq!(s.active_history().undo_count(), 2, "a later edit is its own step");
            assert!(!s.active_history().is_any_drag_in_progress(), "and opens no drag");
        }
        // Escape after moving on past the result: the picture as the result
        // left it; the part before it, and its step, stay.
        let mut s = with(std::slice::from_ref(&c));
        s.path_command(&AppCommand::BeginParamEditAt { addr: blur.clone(), key: "radius".into() });
        tick(&mut s, 5.0);
        s.close_path_drag_before_edit();
        let after_result = picture(&s, c.id);
        tick(&mut s, 8.0);
        s.path_command(&AppCommand::AbandonParamEditAt { addr: blur.clone(), key: "radius".into() });
        assert_eq!(picture(&s, c.id), after_result, "Escape takes back the part after the result alone");
        assert_eq!(s.active_history().undo_count(), 1, "the part before keeps its step");
        assert!(s.resumable_path_drag.is_none());
        undo(&mut s);
        assert_eq!(picture(&s, c.id), picture0, "which undoes to the start");
    }

    /// A drag a result closed whose gesture then ends without a release
    /// reaching its row (the row hidden or collapsed, or another clip
    /// selected) is forgotten, so a later typed edit of the same row is its
    /// own single step and leaves no drag open (#1379).
    #[test]
    fn a_result_closed_drag_is_forgotten_once_its_gesture_ends() {
        let c = fixture(&[E::GaussianBlur { radius: 3.0 }]);
        let ids = effects(&with(std::slice::from_ref(&c)), c.id);
        let blur = at(c.id, &[ids[0]]);
        let typed = |s: &mut AppState, r: f32| {
            let cmd = AppCommand::SetParamAt { addr: blur.clone(), key: "radius".into(), value: ParamValue::Float(r) };
            s.before_dispatch(&cmd);
            run(s, cmd);
        };
        let closed = |selected: Option<ClipId>| {
            let mut s = with(std::slice::from_ref(&c));
            s.selected_clip = selected;
            s.path_command(&AppCommand::BeginParamEditAt { addr: blur.clone(), key: "radius".into() });
            run(&mut s, AppCommand::SetParamAt { addr: blur.clone(), key: "radius".into(), value: ParamValue::Float(5.0) });
            s.close_path_drag_before_edit();
            s
        };
        // The row hidden: its release never comes, and the next frame has
        // no button held.
        let mut s = closed(Some(c.id));
        s.forget_stale_path_drag(false);
        assert!(s.resumable_path_drag.is_none(), "no button held: forgotten");
        typed(&mut s, 7.0);
        assert_eq!(s.active_history().undo_count(), 2, "the typed edit is its own step");
        assert!(!s.active_history().is_any_drag_in_progress(), "and leaves no drag open");
        // Another clip selected while the button is still down.
        let mut s = closed(Some(c.id));
        s.selected_clip = Some(ClipId::new());
        s.forget_stale_path_drag(true);
        assert!(s.resumable_path_drag.is_none(), "another clip: forgotten");
        // Still held, same selection: kept, and the row's tick resumes it.
        let mut s = closed(Some(c.id));
        s.forget_stale_path_drag(true);
        assert!(s.resumable_path_drag.is_some(), "still held: kept");
        // Any other write ends it.
        s.before_dispatch(&AppCommand::ToggleBuiltinEffectEnabled { clip_id: c.id, index: 0 });
        assert!(s.resumable_path_drag.is_none(), "another write: forgotten");
    }

    /// An effect's keys by path and by index: adding one (`AddKeyAt`,
    /// `AddEffectKeyframe`), moving it and setting its interpolation, and
    /// removing it give the same picture each time.
    #[test]
    fn effect_keys_by_path_are_the_keys_by_index() {
        let c = fixture(&[E::GaussianBlur { radius: 3.0 }]);
        let node = effects(&with(std::slice::from_ref(&c)), c.id)[0];
        let addr = at(c.id, &[node]);
        let key = |t: f64, v: f32, i: klipp_core::Interpolation| klipp_core::Keyframe { time_secs: t, value: v, interpolation: i, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
        let lin = klipp_core::Interpolation::Linear;
        // By index: the clip's own keyframe list for the effect.
        let by_index = |keys: Vec<klipp_core::Keyframe>| {
            let mut s = with(std::slice::from_ref(&c));
            let clip = s.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(c.id)).unwrap();
            let id = test_clip::effect_id(clip, 0);
            clip.edit_effect_keyframes_of(id, "radius", |v| *v = keys);
            s
        };
        let mut s = with(std::slice::from_ref(&c));
        run(&mut s, AppCommand::AddKeyAt { addr: addr.clone(), key: "radius".into(), keyframe: serde_json::to_value(key(0.5, 6.0, lin)).unwrap() });
        assert_eq!(picture(&s, c.id), picture(&by_index(vec![key(0.5, 6.0, lin)]), c.id), "added");
        run(&mut s, AppCommand::MoveKeyAt { addr: addr.clone(), key: "radius".into(), time_secs: 0.5, new_time_secs: 1.0, value: 8.0 });
        assert_eq!(picture(&s, c.id), picture(&by_index(vec![key(1.0, 8.0, lin)]), c.id), "moved");
        let hold = klipp_core::Interpolation::Step;
        run(&mut s, AppCommand::SetInterpKeyAt { addr: addr.clone(), key: "radius".into(), time_secs: 1.0, interpolation: hold });
        assert_eq!(picture(&s, c.id), picture(&by_index(vec![key(1.0, 8.0, hold)]), c.id), "interpolation");
        run(&mut s, AppCommand::RemoveKeyAt { addr: addr.clone(), key: "radius".into(), time_secs: 1.0 });
        assert_eq!(picture(&s, c.id), picture(&with(std::slice::from_ref(&c)), c.id), "removed");
        assert_eq!(s.active_history().undo_count(), 4, "one step each");
        assert_eq!(s.path_command(&AppCommand::MoveKeyAt { addr, key: "radius".into(), time_secs: 3.0, new_time_secs: 1.0, value: 1.0 }), Some(false), "no key there: refused");
    }


    /// An effect track's whole-lane keyframe menu items, by path (#1400):
    /// Reverse mirrors its keys in time over their span, and Copy then
    /// Paste puts one effect's keys on another's, each one undo step (a
    /// copy none) that undo takes back. A placed kind's node, whose keys go
    /// through the effect list, is told apart for the menu to leave them out.
    #[test]
    fn an_effect_lane_copies_pastes_and_reverses_by_path() {
        let blur = || E::GaussianBlur { radius: 3.0 };
        let c = fixture(&[blur(), blur()]);
        let key = |t: f64, v: f32| klipp_core::Keyframe { time_secs: t, value: v, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
        let nodes = effects(&with(std::slice::from_ref(&c)), c.id);
        // Each blur's lane laid down by path, as the picture holds it.
        let seeded = |lanes: [Vec<klipp_core::Keyframe>; 2]| {
            let mut s = with(std::slice::from_ref(&c));
            let clip = s.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(c.id)).unwrap();
            let ops: Vec<GraphOp> = lanes
                .iter()
                .zip(&nodes)
                .filter(|(keys, _)| !keys.is_empty())
                .map(|(keys, node)| GraphOp::SetAnim {
                    path: at(c.id, &[*node]),
                    key: "radius".into(),
                    anim: Some(klipp_graph_format::Animation { keys: keys.iter().map(|k| serde_json::to_value(k).unwrap()).collect(), modifiers: None }),
                })
                .collect();
            if !ops.is_empty() {
                clip.apply_picture_ops(&ops).unwrap();
            }
            s
        };
        let mut s = seeded([vec![key(0.0, 2.0), key(1.0, 9.0), key(1.5, 4.0)], Vec::new()]);
        let (first, second) = (at(c.id, &[nodes[0]]), at(c.id, &[nodes[1]]));
        let reversed = vec![key(0.0, 4.0), key(0.5, 9.0), key(1.5, 2.0)];

        run(&mut s, AppCommand::ReverseKeysAt { addr: first.clone(), key: "radius".into() });
        assert_eq!(picture(&s, c.id), picture(&seeded([reversed.clone(), Vec::new()]), c.id), "reversed");
        assert_eq!(s.active_history().undo_count(), 1, "one step");

        assert_eq!(s.path_command(&AppCommand::CopyKeysAt { addr: first.clone(), key: "radius".into() }), Some(false), "a copy writes nothing");
        assert_eq!(s.active_history().undo_count(), 1, "nor takes a step");
        run(&mut s, AppCommand::PasteKeysAt { addr: second, key: "radius".into() });
        assert_eq!(picture(&s, c.id), picture(&seeded([reversed.clone(), reversed.clone()]), c.id), "pasted");
        assert_eq!(s.active_history().undo_count(), 2, "one step");
        undo(&mut s);
        assert_eq!(picture(&s, c.id), picture(&seeded([reversed, Vec::new()]), c.id), "undo takes the paste back");

        assert!(!crate::inspector::stack_view::is_placed_node(clip(&s, c.id), &first), "a blur's keys go by path");
        let d = fixture(&[E::TemporalDenoise { strength: 0.5, threshold: 0.02 }]);
        let sd = with(std::slice::from_ref(&d));
        let denoise = clip(&sd, d.id).picture_v2().unwrap().picture.graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("temporal_denoise")).map(|n| n.id).unwrap();
        assert!(crate::inspector::stack_view::is_placed_node(clip(&sd, d.id), &at(d.id, &[denoise])), "a denoise's go through its effect list");
    }

    /// Setting a key's interpolation by path is the keyframe editor's rule,
    /// as by index: the key and the one before it change, and Bezier gives
    /// both tangents. Two and three keys, Step and Bezier, on the middle and
    /// the last key.
    #[test]
    fn interpolation_by_path_is_the_editors_rule() {
        let c = fixture(&[E::GaussianBlur { radius: 3.0 }]);
        let node = effects(&with(std::slice::from_ref(&c)), c.id)[0];
        let addr = at(c.id, &[node]);
        let key = |t: f64, v: f32| klipp_core::Keyframe { time_secs: t, value: v, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
        for keys in [vec![key(0.0, 2.0), key(1.0, 9.0)], vec![key(0.0, 2.0), key(1.0, 9.0), key(1.5, 4.0)]] {
            for interpolation in [klipp_core::Interpolation::Step, klipp_core::Interpolation::Bezier] {
                for index in 1..keys.len() {
                    let seeded = |keys: Vec<klipp_core::Keyframe>| {
                        let mut s = with(std::slice::from_ref(&c));
                        let clip = s.sequences[0].tracks.iter_mut().find_map(|t| t.find_clip_mut(c.id)).unwrap();
                        let id = test_clip::effect_id(clip, 0);
                        clip.edit_effect_keyframes_of(id, "radius", |v| *v = keys);
                        s
                    };
                    let mut want = keys.clone();
                    klipp_core::keyframe::set_interpolation_at(&mut want, index, interpolation);
                    let mut s = seeded(keys.clone());
                    run(&mut s, AppCommand::SetInterpKeyAt { addr: addr.clone(), key: "radius".into(), time_secs: keys[index].time_secs, interpolation });
                    assert_eq!(picture(&s, c.id), picture(&seeded(want.clone()), c.id), "{} keys, {interpolation:?} at {index}", keys.len());
                    // The rule itself: the key before changes too, and Bezier
                    // gives both tangents.
                    assert_eq!(want[index - 1].interpolation, interpolation);
                    if interpolation == klipp_core::Interpolation::Bezier {
                        assert_ne!(want[index].tangent_in, (0.0, 0.0));
                        assert_ne!(want[index - 1].tangent_out, (0.0, 0.0));
                    }
                }
            }
        }
    }


    /// A path drag left open while a command of any other route arrives:
    /// the dispatch boundary closes the drag first, so the edit is its own
    /// step, neither taken as one of the drag's live steps nor folded into
    /// its snapshot. Every old effect command, a keyframe edit and a
    /// `GraphOps` batch close it; the drag's own commands, transport and
    /// what only the viewer, the timeline and the panels show don't. Then, end to end: a drag, an old toggle, the drag's end are
    /// two steps, each undoing its own change.
    /// `show_panel` (a harness's way to open a clip's inspector, M3 step
    /// 18) sets the layout only: classified as writing nothing, it leaves
    /// an open path drag open, and the history, the document and its
    /// unsaved state as they were.
    #[test]
    fn showing_a_panel_writes_nothing_and_keeps_a_drag_open() {
        let (mut s, id) = state(&[E::GaussianBlur { radius: 3.0 }]);
        let blur = effects(&s, id)[0];
        let show = AppCommand::ShowPanel { workspace: Some("colour".into()), inspector_tab: Some("effects".into()) };
        assert!(!super::super::command_writes::writes(&show));
        assert_eq!(s.path_command(&AppCommand::BeginParamEditAt { addr: at(id, &[blur]), key: "radius".into() }), Some(false));
        let (before, steps, dirty) = (picture(&s, id), s.active_history().undo_count(), s.dirty);
        assert!(!s.closes_path_drag(&show), "the drag stays open");
        s.before_dispatch(&show);
        assert_eq!(s.path_command(&show), None, "not a path command: the app's layout handles it");
        assert!(s.active_history().is_any_drag_in_progress(), "the drag stays open");
        assert_eq!((picture(&s, id), s.active_history().undo_count(), s.dirty), (before, steps, dirty));
    }

    #[test]
    fn any_other_command_closes_an_open_path_drag() {
        let c = fixture(&[E::GaussianBlur { radius: 3.0 }, E::Sharpen { amount: 0.5 }]);
        let ids = effects(&with(std::slice::from_ref(&c)), c.id);
        let blur = at(c.id, &[ids[0]]);
        let mut s = with(std::slice::from_ref(&c));
        assert!(!s.closes_path_drag(&AppCommand::ToggleBuiltinEffectEnabled { clip_id: c.id, index: 1 }), "no drag open, nothing to close");
        s.path_command(&AppCommand::BeginParamEditAt { addr: blur.clone(), key: "radius".into() });
        let others = [
            AppCommand::ToggleBuiltinEffectEnabled { clip_id: c.id, index: 1 },
            AppCommand::RemoveBuiltinEffect { clip_id: c.id, index: 1 },
            AppCommand::ReorderBuiltinEffect { clip_id: c.id, from_index: 1, to_index: 0 },
            AppCommand::UpdateBuiltinEffect { clip_id: c.id, index: 1, effect: E::Sharpen { amount: 0.9 } },
            AppCommand::AddEffectKeyframe { clip_id: c.id, effect_index: 1, param_name: "amount".into(), time_secs: 0.5, value: 0.7 },
            AppCommand::GraphOps { ops: vec![GraphOp::SetEnabled { path: at(c.id, &[ids[1]]), enabled: false }], label: "x".into() },
            AppCommand::SetParamAt { addr: at(c.id, &[ids[1]]), key: "amount".into(), value: ParamValue::Float(0.7) },
            AppCommand::SetParamAt { addr: blur.clone(), key: "other".into(), value: ParamValue::Float(0.7) },
        ];
        for cmd in &others {
            assert!(s.closes_path_drag(cmd), "{cmd:?} closes the drag");
        }
        for cmd in [
            AppCommand::SetParamAt { addr: blur.clone(), key: "radius".into(), value: ParamValue::Float(5.0) },
            AppCommand::EndParamEditAt { addr: blur.clone(), key: "radius".into() },
            AppCommand::SeekTo(klipp_core::RationalTime::from_secs_f64(0.0, 25)),
            AppCommand::Play,
            // What the viewer, the timeline and the panels show: the Video
            // tab sends the first two as it draws and as a drag starts.
            AppCommand::SetInspectorMattePreviewTarget(None),
            AppCommand::SetVideoRubberBandParam(klipp_core::VideoParam::Opacity),
            AppCommand::SelectClip(Some(c.id)),
            AppCommand::SetTimelineZoom { zoom: 2.0, scroll_x: None },
            AppCommand::SetScopesVisible(true),
            AppCommand::ToggleColorPanel,
        ] {
            assert!(!s.closes_path_drag(&cmd), "{cmd:?} leaves the drag open");
        }
        // A writer the old list didn't name, beyond the effect commands:
        // a clip's depth, which merges into the last step if left open.
        assert!(s.closes_path_drag(&AppCommand::SetClipDepth { clip_id: c.id, depth: 2.0 }), "a clip's depth closes the drag");
        // End to end: the drag's step, then an old toggle at the boundary.
        let original = picture(&s, c.id);
        run(&mut s, AppCommand::SetParamAt { addr: blur.clone(), key: "radius".into(), value: ParamValue::Float(9.0) });
        let dragged = picture(&s, c.id);
        let toggle = AppCommand::ToggleBuiltinEffectEnabled { clip_id: c.id, index: 1 };
        s.before_dispatch(&toggle);
        assert!(!s.active_history().is_any_drag_in_progress(), "closed before the toggle's handler runs");
        old(&mut s, c.id, EffectEdit::SetEnabled(1, false));
        s.path_command(&AppCommand::EndParamEditAt { addr: blur.clone(), key: "radius".into() });
        assert_eq!(s.active_history().undo_count(), 2, "the drag and the toggle, two steps");
        undo(&mut s);
        assert_eq!(picture(&s, c.id), dragged, "undo takes back the toggle alone");
        undo(&mut s);
        assert_eq!(picture(&s, c.id), original, "then the drag");
        // The socket's route: a whole edit applied without passing the
        // dispatch boundary (`apply_edits_as_one` closes the drag first).
        let mut s = with(std::slice::from_ref(&c));
        let original = picture(&s, c.id);
        s.path_command(&AppCommand::BeginParamEditAt { addr: blur.clone(), key: "radius".into() });
        run(&mut s, AppCommand::SetParamAt { addr: blur.clone(), key: "radius".into(), value: ParamValue::Float(9.0) });
        let dragged = picture(&s, c.id);
        s.close_path_drag_before_edit();
        let graph_ops::GraphOpsOutcome { edit, .. } = graph_ops::graph_ops_edit(&s.sequences[0], &[GraphOp::SetEnabled { path: at(c.id, &[ids[1]]), enabled: false }]).unwrap();
        if let EditCommand::PictureEdit { clip_id, edit: e, .. } = &edit {
            s.set_picture(*clip_id, &e.after);
        }
        s.push_history(edit);
        s.path_command(&AppCommand::EndParamEditAt { addr: blur.clone(), key: "radius".into() });
        assert_eq!(s.active_history().undo_count(), 2, "socket route: two steps");
        undo(&mut s);
        assert_eq!(picture(&s, c.id), dragged, "socket route: the edit undoes alone");
        undo(&mut s);
        assert_eq!(picture(&s, c.id), original, "socket route: then the drag");
    }

}

/// A structure edit of a grade on the canvas (#1476), at the level it
/// shows.
#[derive(Clone, Debug)]
pub(super) enum GradeStructure {
    /// A node of `kind` added unwired, under `id`, at `position`.
    Add { kind: klipp_nodes::NodeKind, id: klipp_nodes::NodeId, position: (f32, f32) },
    Remove { node: klipp_nodes::NodeId },
    /// `node` moved to just after `after`: no canvas gesture makes it
    /// (the grade canvas has no reorder); its tests and the parity family
    /// drive it as `MoveAfter` by path.
    #[cfg(test)]
    Reorder { node: klipp_nodes::NodeId, after: klipp_nodes::NodeId },
    /// `nodes` packed into a new compound `group`.
    Group { nodes: Vec<klipp_nodes::NodeId>, group: klipp_nodes::NodeId },
    /// The compound `node` dissolved.
    Ungroup { node: klipp_nodes::NodeId },
}

/// The placement or opacity lane a key drag's key names, as the timeline
/// and the curve editor key their video lanes' drags
/// (`kf:video:<clip>:<param>:…`, `kf:vtangent:<clip>:<param>:…`).
fn drag_lane(key: &str) -> Option<klipp_core::VideoParam> {
    use klipp_core::VideoParam as P;
    let mut parts = key.split(':');
    if parts.next() != Some("kf") || !matches!(parts.next(), Some("video" | "vtangent")) {
        return None;
    }
    let _clip = parts.next()?;
    let named = parts.next()?;
    [P::Opacity, P::PositionX, P::PositionY, P::Scale, P::ScaleY, P::Rotation, P::TiltX, P::TiltY, P::MatteFeather, P::MatteChoke].into_iter().find(|p| format!("{p:?}") == named)
}

/// The op that sets the placement or opacity lane `key` of `clip` to
/// `keys` whole, the lane's modifier stack kept: none left takes the lane
/// off.
fn set_lane(clip: &klipp_timeline::Clip, addr: &NodePath, key: &str, keys: Vec<klipp_core::Keyframe>) -> GraphOp {
    let modifiers = crate::inspector::placement_by_path::lane_anim(clip, key).and_then(|a| a.modifiers);
    let keys: Vec<serde_json::Value> = keys.iter().filter_map(|k| serde_json::to_value(k).ok()).collect();
    let anim = (!keys.is_empty() || modifiers.is_some()).then_some(klipp_graph_format::Animation { keys, modifiers });
    GraphOp::SetAnim { path: addr.clone(), key: key.into(), anim }
}
