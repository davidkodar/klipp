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
    let root = first.path().root.root();
    if !matches!(root, klipp_graph_format::path::Root::Clip { .. }) {
        return Ok(refuse(Some(0), "graph writes currently support clip roots only".into()));
    }
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
        Err((at, why)) => return Ok(refuse(Some(at), why)),
    };
    let mut result = send_with_reply(ctx, vec![AppCommand::GraphOps { ops: prepared.ops, label: label.clone() }], Vec::new(), false,
        EnvelopeReply { expected_revision: Some(revision), expected_project_identity: Some(identity), label: Some(label), source_indices: Some(prepared.source_indices) })?;
    // Queued ids are useful for recovery, but do not claim they exist until
    // the app acknowledges the write. A late outcome is in operation_status.
    if result["accepted"] == true {
        let created: Vec<_> = prepared.created.into_iter().map(|(op_index,path)| json!({"op_index":op_index,"path":GraphPath::from(path)})).collect();
        let key = if result["applied"] == true { "created" } else { "pending_created" };
        result[key] = json!(created);
    }
    Ok(result)
}
