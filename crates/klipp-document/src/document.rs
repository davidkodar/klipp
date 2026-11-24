//! The project as it is edited: its sequences and media pool, and the
//! undo history of each sequence and of the project. Which sequence is
//! open, and its frame rate, are the timeline view's: a stacked timeline
//! trades them with its own.
//!
//! The first step of one way to change it (see the architecture notes):
//! for now its fields are open, as AppState's were, and the app reaches
//! them through `state.document`. Committing an edit, undo and redo move
//! here next, then each edit is made through them and the fields close.

use klipp_core::SequenceId;
use klipp_media::MediaPool;
use klipp_timeline::Sequence;

use crate::history::CommandHistory;

pub struct Document {
    pub sequences: Vec<Sequence>,
    pub media_pool: MediaPool,
    /// Per-sequence undo stack. Keyed by `SequenceId` (stable across
    /// reorder/delete) so an entry never points at a sequence that
    /// shifted underneath it. Use `active_history` /
    /// `active_history_mut` / `push_history` to access the current
    /// sequence's stack.
    pub command_histories: std::collections::HashMap<SequenceId, CommandHistory>,
    /// The project's own undo history (#1986): the steps that belong to
    /// the whole project — its colour set-up, a file's camera encoding,
    /// the media pool's records — reachable by ctrl+z from any sequence.
    /// Use `push_history`, `take_undo` and `take_redo`, which choose
    /// between it and the active sequence's by when each step moved.
    pub project_history: CommandHistory,
    /// The clock every history stamps its steps from, so the project's
    /// and a sequence's compare.
    pub history_clock: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl Default for Document {
    /// A project of one empty sequence, with nothing to undo:
    /// what AppState started with.
    fn default() -> Self {
        Self {
            sequences: vec![Sequence::new_default()],
            media_pool: MediaPool::new(),
            command_histories: std::collections::HashMap::new(),
            project_history: CommandHistory::new(),
            history_clock: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }
}
