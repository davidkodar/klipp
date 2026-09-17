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
}
