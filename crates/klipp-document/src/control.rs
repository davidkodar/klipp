//! What a client outside the UI sends the document's host, and what it
//! gets back. The socket builds an envelope; the UI applies it in one
//! frame and acknowledges with the revision after it.

use crate::history::EditCommand;
use klipp_core::AppCommand;

pub struct ControlEnvelope {
    /// The app's own commands, applied through its router as a click is.
    pub commands: Vec<AppCommand>,
    /// A resolved plan's commands, applied forward in order and recorded
    /// as one history entry.
    pub edits: Vec<EditCommand>,
    /// Record everything in this envelope as one undo step.
    pub batch: bool,
    /// The document revision the edits were resolved against. If the
    /// document has moved since, they are refused unapplied: the plan
    /// was made for a document that no longer exists.
    pub expected_revision: Option<u64>,
    /// The project identity the edits were planned against. Checked by
    /// the UI together with the revision before applying any of them.
    pub expected_project_identity: Option<u64>,
    /// The other sequences the edits copied from, each with the content
    /// revision it was read at (#2385): if one has changed since, they
    /// are refused unapplied, as for the revision.
    pub expected_sources: Vec<(klipp_core::SequenceId, u64)>,
    /// A name for the history entry, when a plan is applied.
    pub label: Option<String>,
    pub ack: Option<std::sync::mpsc::Sender<ControlAck>>,
}

/// What the host says once an envelope is handled.
#[derive(Debug, Clone)]
pub struct ControlAck {
    /// The document revision after the envelope.
    pub revision: u64,
    /// Why nothing was applied, if nothing was.
    pub refused: Option<String>,
    /// The graph op a refused `GraphOps` batch stopped at, by its index in
    /// the batch (node-graph M2 step 15d).
    pub refused_op: Option<usize>,
    /// Node ids an applied `GraphOps` batch named that the picture holds
    /// under other ids once written (#1256): an effect added to, or moved
    /// in, a clip whose fields keep no effect ids. The reply's paths are
    /// renamed through it.
    pub renamed: Vec<(klipp_graph_format::NodeId, klipp_graph_format::NodeId)>,
    /// Surviving nodes whose mask was removed by this applied batch.
    pub ungated: Vec<klipp_graph_lift::edit_planner::Ungated>,
    /// Why a write through a picture-truth clip's fields was refused while
    /// the envelope was applied (#2458): that write left its clip as it
    /// was, the rest of the envelope applied.
    pub picture_refused: Option<String>,
}
