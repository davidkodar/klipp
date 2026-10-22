//! Preflight on a copy, then one guarded app-thread batch.

use serde_json::{Value, json};
use crate::{ControlContext, AppCommand, project_status, graph_model, graph_prepare, graph_wire::{GraphPath, GraphWireOp}, envelope::{send_with_reply, EnvelopeReply}};

pub(crate) fn write(ctx: &ControlContext, ops: &[GraphWireOp], expected_revision: Option<u64>, expected_project_identity: Option<u64>, label: Option<String>) -> Result<Value, String> {
    let refuse = |index: Option<usize>, why: String| json!({"accepted":false,"applied":false,"op_index":index,"why":why});
    let label = label.unwrap_or_else(|| "Edit graph".into());
    if label.len() > 256 { return Ok(refuse(None, "the undo label is at most 256 bytes".into())); }
    let Some(first) = ops.first().filter(|_| ops.len() <= 64) else {
        return Ok(refuse(None, "a graph batch needs 1 to 64 operations".into()));
    };
    // Any picture the graph reads: a clip's, the timeline grade, a grade
    // group's stage, a track's grade or effects (#1911). Each commits as
    // one step of its own kind on the app thread.
    let root = first.path().root.root();
    let identity = project_status(ctx)?.identity;
    if expected_project_identity.is_some_and(|want| want != identity) {
        return Ok(refuse(None, "the project changed; read the graph again".into()));
    }
    let sequence = ctx.sequence.read().map_err(|_| "sequence lock poisoned")?;
    // This engine mirror may lag an app revision. Preflight is advisory;
    // the app revalidates against its live document at the guarded commit.
    let revision = ctx.shared.revision();
    if expected_revision.is_some_and(|want| want != revision) {
        return Ok(refuse(None, "the document revision changed; read the graph again".into()));
    }
    let docs = match graph_model::documents(&sequence, root) {
        Ok(docs) => docs,
        Err(why) => return Ok(refuse(Some(0), why)),
    };
    drop(sequence);
    let prepared = match graph_prepare::prepare(ops, docs) {
        Ok(batch) => batch,
        Err((at, why)) => return Ok(refuse(at, why)),
    };
    let mut result = send_with_reply(ctx, vec![AppCommand::GraphOps { ops: prepared.ops, label: label.clone() }], Vec::new(), false,
        EnvelopeReply { expected_revision: Some(revision), expected_project_identity: Some(identity), label: Some(label), source_indices: Some(prepared.source_indices), expected_sources: Vec::new() })?;
    // Queued ids are useful for recovery, but do not claim they exist until
    // the app acknowledges the write. A late outcome is in operation_status.
    if result["accepted"] == true {
        let created: Vec<_> = prepared.created.into_iter().map(|(op_index,path)| json!({"op_index":op_index,"path":GraphPath::from(renamed_path(path, &result["renamed"]))})).collect();
        let key = if result["applied"] == true { "created" } else { "pending_created" };
        result[key] = json!(created);
    }
    Ok(result)
}

/// `path` with each node the app's picture holds under another id once
/// written (`renamed`, its reply's pairs of old and new ids) named by that
/// id, so the path leads to it (#1256): an effect added to a clip whose
/// fields keep no effect ids is lifted again under its list slot's id.
fn renamed_path(mut path: klipp_graph_format::path::NodePath, renamed: &Value) -> klipp_graph_format::path::NodePath {
    let pairs: Vec<(String, String)> = renamed.as_array().into_iter().flatten()
        .filter_map(|pair| Some((pair.get(0)?.as_str()?.to_string(), pair.get(1)?.as_str()?.to_string())))
        .collect();
    for node in &mut path.nodes {
        if let Some(now) = pairs.iter().find(|(was, _)| *was == node.to_string()).and_then(|(_, now)| now.parse().ok()) {
            *node = now;
        }
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_graph_format::path::{NodePath, Root};

    /// The created path names the id the app's reply renames a node to,
    /// at any depth of the path, and a path the reply doesn't rename stays.
    #[test]
    fn a_created_path_names_the_id_the_app_renamed_it_to() {
        let id = |n: u128| uuid::Uuid::from_u128(n);
        let root = Root::Clip { clip: id(1) };
        let renamed = json!([[id(0xa).to_string(), id(0xb).to_string()], [id(0xc).to_string(), id(0xd).to_string()]]);
        assert_eq!(renamed_path(NodePath::new(root, [id(0xa)]), &renamed).nodes, vec![id(0xb)]);
        assert_eq!(renamed_path(NodePath::new(root, [id(0xe), id(0xc)]), &renamed).nodes, vec![id(0xe), id(0xd)]);
        assert_eq!(renamed_path(NodePath::new(root, [id(0xf)]), &renamed).nodes, vec![id(0xf)]);
        assert_eq!(renamed_path(NodePath::new(root, [id(0xa)]), &Value::Null).nodes, vec![id(0xa)], "no renames, no change");
    }
}
