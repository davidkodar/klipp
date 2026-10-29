//! Resolve a wire batch against a disposable copy, retaining caller op indices.

use std::collections::BTreeMap;
use klipp_graph_format::path::NodePath;
use klipp_graph_lift::{ops::{GraphOp, apply_in_batch, check_batch_end}, paths::{Documents, resolve}};
use klipp_timeline::RegistryCatalog;
use crate::{graph_model, graph_wire::{GraphPath, GraphWireOp}};

pub(crate) struct Prepared {
    pub ops: Vec<GraphOp>,
    pub source_indices: Vec<usize>,
    pub created: BTreeMap<usize, NodePath>,
}

pub(crate) fn prepare(wire: &[GraphWireOp], mut docs: Documents) -> Result<Prepared, (Option<usize>, String)> {
    if wire.is_empty() || wire.len() > 64 { return Err((None, "a graph batch needs 1 to 64 operations".into())); }
    let root = wire[0].path().root.root();
    let mut result = Prepared { ops: Vec::new(), source_indices: Vec::new(), created: BTreeMap::new() };
    let mut pending = Vec::new();
    // The documents the batch starts from, for its end's check.
    let start = docs.clone();
    for (index, item) in wire.iter().enumerate() {
        let step = (|| -> Result<(), String> {
            if item.path().root.root() != root { return Err("a batch edits one root".into()); }
            let path = item.path().resolve(&result.created)?;
            if path.nodes.is_empty() { return Err("an operation must name a node".into()); }
            let mut initial_params = None;
            let mut initial_entry: Option<(klipp_graph_format::path::NodePath, serde_json::Value)> = None;
            let mut created = None;
            let mut then_label = None;
            let op = match item {
                GraphWireOp::Add { r#type, params, label, entry, .. } | GraphWireOp::AddBeside { r#type, params, label, entry, .. } => {
                    if params.len() > 64 { return Err("an add takes at most 64 initial parameters".into()); }
                    let id = uuid::Uuid::new_v4();
                    let mut node = graph_model::new_node(r#type, id, label.as_deref())?;
                    let beside = matches!(item, GraphWireOp::AddBeside { .. });
                    let mut target = path.clone();
                    *target.nodes.last_mut().unwrap() = id;
                    // An effect is added as the list adds one, unnamed, so it
                    // has its entry (its keys, its inspector row), then named
                    // in the same batch by relabelling's own route, which the
                    // picture keeps (#1912): an effect's entry holds no name.
                    if node.type_id.as_str() == klipp_graph_format::types::LEGACY_EFFECT {
                        if let Some(name) = label.as_ref().filter(|s| !s.is_empty()) {
                            node.label = String::new();
                            then_label = Some((target.clone(), name.clone()));
                        }
                    }
                    initial_entry = entry.clone().map(|e| (target.clone(), e));
                    initial_params = Some((target.clone(), params));
                    created = Some(target);
                    if beside { GraphOp::AddNodeBeside { at: path, node: Box::new(node) } }
                    else { GraphOp::AddNode { after: path, node: Box::new(node) } }
                }
                GraphWireOp::Remove { .. } => GraphOp::RemoveNode { path },
                GraphWireOp::Connect { from, from_port, to_port, .. } => GraphOp::Connect {
                    from: from.resolve(&result.created)?, from_port: from_port.as_str().into(), to: path, to_port: to_port.as_str().into(),
                },
                GraphWireOp::Disconnect { port, .. } => GraphOp::Disconnect { to: path, port: port.as_str().into() },
                GraphWireOp::Publish { from, from_port, key, label, .. } => {
                    let source = from.resolve(&result.created)?;
                    let (child, parent) = source.nodes.split_last().ok_or("publish needs a child node")?;
                    if source.root != path.root || parent != path.nodes {
                        return Err("publish source must be a direct child of the target group".into());
                    }
                    GraphOp::PublishOutput {
                        group: path,
                        endpoint: klipp_graph_format::PortBinding { node: *child, port: from_port.as_str().into() },
                        key: key.as_str().into(), label: label.clone().unwrap_or_default(),
                    }
                }
                GraphWireOp::Unpublish { key, .. } => GraphOp::Unpublish { group: path, key: key.as_str().into() },
                GraphWireOp::Set { key, value, .. } => {
                    let node = resolve(&docs, &path).map_err(|e| e.to_string())?;
                    graph_model::parameter_op(node, path, key, value)?
                }
                GraphWireOp::Modify { key, modifiers, .. } => {
                    let node = resolve(&docs, &path).map_err(|e| e.to_string())?;
                    graph_model::modifiers_op(node, path, key, modifiers)?
                }
                GraphWireOp::Keys { key, keys, .. } => {
                    let node = resolve(&docs, &path).map_err(|e| e.to_string())?;
                    graph_model::keys_op(node, path, key, keys)?
                }
                GraphWireOp::Enable { enabled, .. } => GraphOp::SetEnabled { path, enabled: *enabled },
                GraphWireOp::Mix { mix, .. } => {
                    if mix.is_some_and(|m| !m.is_finite() || !(0.0..=1.0).contains(&m)) { return Err("mix must be finite and between 0 and 1, or null".into()); }
                    GraphOp::SetMix { path, mix: *mix }
                }
                GraphWireOp::MaskChannel { channel, .. } => GraphOp::SetMaskChannel { path, channel: channel.clone() },
                GraphWireOp::MoveAfter { after, .. } => {
                    let mut sibling = item.path().clone();
                    *sibling.nodes.last_mut().unwrap() = after.clone();
                    let sibling = sibling.resolve(&result.created)?;
                    GraphOp::MoveAfter { path, after: sibling.node().ok_or("move_after needs a sibling")? }
                }
            };
            if let GraphOp::AddNodeBeside { node, .. } = &op { pending.push(node.id); }
            apply_in_batch(&mut docs, &op, &RegistryCatalog, &pending)?;
            result.ops.push(op);
            result.source_indices.push(index);
            // A saved node's whole entry, as SetPayload checks one.
            if let Some((target, payload)) = initial_entry {
                let op = GraphOp::SetPayload { path: target, payload };
                apply_in_batch(&mut docs, &op, &RegistryCatalog, &pending)?;
                result.ops.push(op);
                result.source_indices.push(index);
            }
            if let Some((target, params)) = initial_params {
                for (key, value) in params {
                    let node = resolve(&docs, &target).map_err(|e| e.to_string())?;
                    let op = graph_model::parameter_op(node, target.clone(), key, value)?;
                    apply_in_batch(&mut docs, &op, &RegistryCatalog, &pending)?;
                    result.ops.push(op);
                    result.source_indices.push(index);
                }
            }
            if let Some((target, name)) = then_label {
                let op = GraphOp::SetNodeMeta { path: target, label: Some(name), position: None };
                apply_in_batch(&mut docs, &op, &RegistryCatalog, &pending)?;
                result.ops.push(op);
                result.source_indices.push(index);
            }
            if let Some(path) = created { result.created.insert(index, path); }
            Ok(())
        })();
        step.map_err(|why| (Some(index), why))?;
    }
    check_batch_end(&docs, &start, root, &RegistryCatalog).map_err(|error| {
        // Attribute an unwired beside node to its creation, without parsing prose.
        // Other whole-batch failures have no single offending operation.
        let index = error.unwired.and_then(|(id, _)| {
            if !pending.contains(&id) { return None; }
            result.created.iter().find_map(|(index, path)| (path.node() == Some(id)).then_some(*index))
        });
        (index, error.why)
    })?;
    Ok(result)
}

pub(crate) fn existing_path(path: &GraphPath) -> Result<NodePath, String> {
    path.resolve(&BTreeMap::new())
}
