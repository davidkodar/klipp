//! The node editor's own edits of its selection (#1745): duplicate,
//! copy, paste and replace, on the grade level or the picture level the
//! canvas shows, each one undo step. On the state, so the app and a test
//! drive the same code.

use super::*;
use klipp_graph_format::path::NodePath;
use klipp_graph_lift::ops::GraphOp;
use klipp_timeline::GradeAccess;

/// How far a duplicate lands from what it copies, in canvas units.
const DUPLICATE_OFFSET: (f32, f32) = (40.0, 40.0);

impl AppState {
    /// Applies a node edit; `None` for any other command, else whether
    /// the document changed. A refusal says why in a toast.
    pub(crate) fn node_edit(&mut self, cmd: &AppCommand) -> Option<bool> {
        let result = match cmd {
            AppCommand::DuplicateNodes { clip_id, node_ids } => self.duplicate_grade_nodes(*clip_id, node_ids),
            AppCommand::CopyNodes { clip_id, node_ids } => self.copy_grade_nodes(*clip_id, node_ids).map(|()| false),
            AppCommand::PasteNodes { clip_id, position } => self.paste_grade_nodes(*clip_id, *position),
            AppCommand::ReplaceNode { clip_id, node_id, kind } => self.replace_grade_node(*clip_id, *node_id, kind),
            AppCommand::CopyPictureNodes { nodes } => {
                self.picture_node_clipboard = nodes.clone();
                self.toast.show(format!("Copied {} node{}", nodes.len(), if nodes.len() == 1 { "" } else { "s" }));
                Ok(false)
            }
            AppCommand::PastePictureNodes { after, position } => self.paste_picture_nodes(after, *position),
            _ => return None,
        };
        Some(result.unwrap_or_else(|why| {
            self.toast.show(why);
            false
        }))
    }

    /// The level holding `nodes` in `clip_id`'s grade, by its path from
    /// the top: the level the canvas shows when they are not found.
    fn holding(&self, clip_id: ClipId, nodes: &[klipp_nodes::NodeId]) -> Result<Vec<klipp_nodes::NodeId>, String> {
        let clip = self.sequence().all_clips().find(|c| c.id == clip_id).ok_or("The clip is no longer on the timeline")?;
        super::colour::graph_path_for(clip.grade(), nodes, &self.graph_descent_path).ok_or_else(|| "The nodes are not at one level".to_owned())
    }

    fn level_graph(&self, clip_id: ClipId, path: &[klipp_nodes::NodeId]) -> Result<klipp_nodes::NodeGraph, String> {
        let clip = self.sequence().all_clips().find(|c| c.id == clip_id).ok_or("The clip is no longer on the timeline")?;
        super::shortcuts::grade_level(clip.grade(), path).cloned().ok_or_else(|| "That level of the grade is gone".to_owned())
    }

    /// The selection as the clipboard takes it: the level's nodes among
    /// `ids`, Source and Output left out.
    fn grade_selection(graph: &klipp_nodes::NodeGraph, ids: &[klipp_nodes::NodeId]) -> std::collections::HashSet<klipp_nodes::NodeId> {
        graph.nodes.iter()
            .filter(|n| ids.contains(&n.id) && !matches!(n.kind, klipp_nodes::NodeKind::Source | klipp_nodes::NodeKind::Output))
            .map(|n| n.id)
            .collect()
    }

    fn select_nodes(&mut self, ids: &[klipp_nodes::NodeId]) {
        self.selected_node_ids = ids.iter().copied().collect();
        self.selected_node_id = ids.last().copied();
    }

    fn duplicate_grade_nodes(&mut self, clip_id: ClipId, ids: &[klipp_nodes::NodeId]) -> Result<bool, String> {
        let path = self.holding(clip_id, ids)?;
        let level = self.level_graph(clip_id, &path)?;
        let selection = Self::grade_selection(&level, ids);
        if selection.is_empty() {
            return Err("Duplicate: no node to copy here".into());
        }
        let copy = crate::state::GradeNodeClipboard::from_selection(&level, &selection);
        let mut made = Vec::new();
        super::colour::edit_clip_graph(self, clip_id, &path, true, |graph| made = copy.merge_into_ids(graph, DUPLICATE_OFFSET))
            .ok_or("The clip is no longer on the timeline")?
            .map_err(|()| "That level of the grade is gone".to_owned())?;
        self.select_nodes(&made);
        self.dirty = true;
        Ok(true)
    }

    fn copy_grade_nodes(&mut self, clip_id: ClipId, ids: &[klipp_nodes::NodeId]) -> Result<(), String> {
        let path = self.holding(clip_id, ids)?;
        let level = self.level_graph(clip_id, &path)?;
        let selection = Self::grade_selection(&level, ids);
        if selection.is_empty() {
            return Err("Copy: no node selected".into());
        }
        let copy = crate::state::GradeNodeClipboard::from_selection(&level, &selection);
        self.toast.show(format!("Copied {} node{}", copy.nodes.len(), if copy.nodes.len() == 1 { "" } else { "s" }));
        self.grade_node_clipboard = Some(copy);
        Ok(())
    }

    fn paste_grade_nodes(&mut self, clip_id: ClipId, position: (f32, f32)) -> Result<bool, String> {
        let copy = self.grade_node_clipboard.clone().filter(|c| !c.nodes.is_empty()).ok_or("Paste: nothing copied")?;
        // The copy's top-left lands where the paste was asked for.
        let min = copy.node_positions.values().fold(None, |m: Option<(f32, f32)>, p| Some(m.map_or((p.x, p.y), |(x, y)| (x.min(p.x), y.min(p.y)))));
        let offset = min.map_or(position, |(x, y)| (position.0 - x, position.1 - y));
        let path = self.graph_descent_path.clone();
        let mut made = Vec::new();
        super::colour::edit_clip_graph(self, clip_id, &path, true, |graph| made = copy.merge_into_ids(graph, offset))
            .ok_or("The clip is no longer on the timeline")?
            .map_err(|()| "That level of the grade is gone".to_owned())?;
        self.select_nodes(&made);
        self.dirty = true;
        Ok(true)
    }

    fn replace_grade_node(&mut self, clip_id: ClipId, node_id: klipp_nodes::NodeId, kind: &str) -> Result<bool, String> {
        let new_kind = klipp_nodes::kinds::addable_node_kind(kind).ok_or_else(|| format!("{kind} is not a grade node"))?;
        let path = self.holding(clip_id, &[node_id])?;
        let level = self.level_graph(clip_id, &path)?;
        let old = level.nodes.iter().find(|n| n.id == node_id).ok_or("The node is gone")?;
        if matches!(old.kind, klipp_nodes::NodeKind::Source | klipp_nodes::NodeKind::Output | klipp_nodes::NodeKind::Compound) {
            return Err(format!("A {} can't be replaced", old.kind.display_name()));
        }
        if old.kind == new_kind {
            return Ok(false);
        }
        let ports = |k: klipp_nodes::NodeKind| klipp_nodes::registry::node_type(k).map_or((1, 1), |t| (t.desc.inputs.len(), t.desc.outputs.len().max(1)));
        let (inputs, outputs) = ports(new_kind);
        // A name given by hand stays; the kind's own name follows it.
        let label = if old.label.is_empty() || old.label == old.kind.display_name() { new_kind.display_name().to_owned() } else { old.label.clone() };
        let enabled = old.enabled;
        super::colour::edit_clip_graph(self, clip_id, &path, true, |graph| {
            if let Some(n) = graph.nodes.iter_mut().find(|n| n.id == node_id) {
                *n = klipp_nodes::Node {
                    id: node_id, kind: new_kind, label, enabled,
                    params: Default::default(), keyframes: Default::default(),
                    output_link: None, input_links: Vec::new(),
                };
            }
            // Wires into ports the new kind lacks go.
            graph.edges.retain(|e| !(e.to_node == node_id && e.to_port as usize >= inputs) && !(e.from_node == node_id && e.from_port as usize >= outputs));
        })
        .ok_or("The clip is no longer on the timeline")?
        .map_err(|()| "That level of the grade is gone".to_owned())?;
        self.select_nodes(&[node_id]);
        self.dirty = true;
        Ok(true)
    }

    /// The kept picture nodes, fresh ids, chained after the node `after`
    /// names: the first after it, each next after the one before; placed
    /// as they were copied, the first at `position`.
    fn paste_picture_nodes(&mut self, after: &NodePath, position: (f32, f32)) -> Result<bool, String> {
        if self.picture_node_clipboard.is_empty() {
            return Err("Paste: nothing copied".into());
        }
        let level = &after.nodes[..after.nodes.len().saturating_sub(1)];
        let at = |id| NodePath::new(after.root, level.iter().copied().chain([id]));
        let mut ops = Vec::new();
        let mut prev = after.clone();
        let mut made = Vec::new();
        for (i, node) in self.picture_node_clipboard.iter().enumerate() {
            let mut node = node.clone();
            node.id = klipp_graph_format::NodeId::new_v4();
            let place = serde_json::json!({"x": position.0 + i as f32 * 200.0, "y": position.1});
            ops.push(GraphOp::AddNode { after: prev.clone(), node: Box::new(node.clone()) });
            ops.push(GraphOp::SetNodeMeta { path: at(node.id), label: None, position: Some(place) });
            prev = at(node.id);
            made.push(node.id);
        }
        self.apply_path_ops(&ops, "Paste Nodes")?;
        self.select_nodes(&made);
        Ok(true)
    }
}
