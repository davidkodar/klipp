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

use crate::history::{EditCommand, HistoryEntry, Scope};

/// What a history is set up with: how many steps it keeps, and the
/// revision counter every push, undo and redo advances (the control
/// socket's, which the app may replace once the socket starts).
pub struct HistorySetup<'a> {
    pub max_size: usize,
    pub revision: &'a std::sync::Arc<klipp_core::ControlShared>,
}

/// Which of a sequence's step and the project's moved last.
fn later(sequence: Option<u64>, project: Option<u64>) -> Option<Scope> {
    match (sequence, project) {
        (None, None) => None,
        (Some(_), None) => Some(Scope::Sequence),
        (None, Some(_)) => Some(Scope::Project),
        (Some(s), Some(p)) => Some(if p > s { Scope::Project } else { Scope::Sequence }),
    }
}

/// The undo history: each sequence's and the project's (#1986), ordered
/// by one clock so undo takes whichever step is latest. `active` is the
/// sequence the timeline shows.
impl Document {
    /// Sequence `id`'s history; an empty one for a sequence with none yet
    /// (a project from before the histories were per sequence).
    pub fn history_for(&self, id: SequenceId) -> &CommandHistory {
        static EMPTY: std::sync::OnceLock<CommandHistory> = std::sync::OnceLock::new();
        self.command_histories.get(&id).unwrap_or_else(|| EMPTY.get_or_init(CommandHistory::new))
    }

    /// Sequence `id`'s history to change, made with `setup` if it has none,
    /// and given the revision counter and the clock if it lacks them.
    pub fn history_for_mut(&mut self, id: SequenceId, setup: &HistorySetup<'_>) -> &mut CommandHistory {
        let clock = &self.history_clock;
        let h = self.command_histories.entry(id).or_insert_with(|| {
            let mut h = CommandHistory::new();
            h.set_max_size(setup.max_size);
            h
        });
        if !h.has_revision_shared() {
            h.set_revision_shared(std::sync::Arc::clone(setup.revision));
        }
        if !h.has_clock() {
            h.set_clock(std::sync::Arc::clone(clock));
        }
        h
    }

    /// The project's history, set up as the sequences' are.
    pub fn project_history_mut(&mut self, setup: &HistorySetup<'_>) -> &mut CommandHistory {
        let h = &mut self.project_history;
        h.set_max_size(setup.max_size);
        if !h.has_revision_shared() {
            h.set_revision_shared(std::sync::Arc::clone(setup.revision));
        }
        if !h.has_clock() {
            h.set_clock(std::sync::Arc::clone(&self.history_clock));
        }
        h
    }

    /// Which history ctrl+z takes the next step back from.
    pub fn next_undo(&self, active: SequenceId) -> Option<Scope> {
        later(self.history_for(active).undo_stamp(), self.project_history.undo_stamp())
    }

    /// Which history redo puts the next step back from: the step taken
    /// back last, unless an edit since has made it stale. A sequence's step
    /// goes stale with a newer project step; a project step with a newer
    /// edit in any sequence.
    pub fn next_redo(&self, active: SequenceId) -> Option<Scope> {
        let (seq, project) = (self.redo_fresh_sequence(active), self.redo_fresh_project());
        later(seq.then(|| self.history_for(active).redo_stamp()).flatten(), project.then(|| self.project_history.redo_stamp()).flatten())
    }

    fn redo_fresh_sequence(&self, active: SequenceId) -> bool {
        self.history_for(active).redo_stamp().is_some_and(|s| s > self.project_history.last_edit_stamp())
    }

    fn redo_fresh_project(&self) -> bool {
        let last = self.command_histories.values().map(|h| h.last_edit_stamp()).max().unwrap_or(0);
        self.project_history.redo_stamp().is_some_and(|s| s > last)
    }

    fn history_of(&self, scope: Scope, active: SequenceId) -> &CommandHistory {
        match scope {
            Scope::Project => &self.project_history,
            Scope::Sequence => self.history_for(active),
        }
    }

    /// The step an undo would take next.
    pub fn next_undo_command(&self, active: SequenceId) -> Option<&EditCommand> {
        self.history_of(self.next_undo(active)?, active).undo_entries().last().map(|e| &e.command)
    }

    /// The step a redo would put back next.
    pub fn next_redo_command(&self, active: SequenceId) -> Option<&EditCommand> {
        self.history_of(self.next_redo(active)?, active).redo_entries().last().map(|e| &e.command)
    }

    /// What ctrl+z would take back; a project step says it is the project's.
    pub fn undo_label(&self, active: SequenceId) -> Option<String> {
        match self.next_undo(active)? {
            Scope::Project => self.project_history.undo_label().map(|l| format!("{l} (project)")),
            Scope::Sequence => self.history_for(active).undo_label().map(str::to_owned),
        }
    }

    /// What redo would put back, named as [`Self::undo_label`] names it.
    pub fn redo_label(&self, active: SequenceId) -> Option<String> {
        match self.next_redo(active)? {
            Scope::Project => self.project_history.redo_label().map(|l| format!("{l} (project)")),
            Scope::Sequence => self.history_for(active).redo_label().map(str::to_owned),
        }
    }

    /// How many steps ctrl+z can take back: the sequence's and the project's.
    pub fn undo_depth(&self, active: SequenceId) -> usize {
        self.history_for(active).undo_count() + self.project_history.undo_count()
    }

    /// How many steps redo can put back.
    pub fn redo_depth(&self, active: SequenceId) -> usize {
        let last_sequence_edit = self.command_histories.values().map(|h| h.last_edit_stamp()).max().unwrap_or(0);
        let project_edit = self.project_history.last_edit_stamp();
        self.history_for(active).redo_stamped().filter(|(s, _)| *s > project_edit).count()
            + self.project_history.redo_stamped().filter(|(s, _)| *s > last_sequence_edit).count()
    }

    /// Drop the redo steps an edit has made stale: they would never be
    /// offered again. A stack goes stale from the top down.
    pub fn prune_stale_redo(&mut self, active: SequenceId, setup: &HistorySetup<'_>) {
        if self.project_history.redo_stamp().is_some() && !self.redo_fresh_project() {
            self.project_history.clear_redo();
        }
        if self.history_for(active).redo_stamp().is_some() && !self.redo_fresh_sequence(active) {
            self.history_for_mut(active, setup).clear_redo();
        }
    }

    /// The steps in reach, as the History panel lists them: undo's, the
    /// next last, and redo's, the next last; a project step says so.
    pub fn history_view(&self, active: SequenceId) -> (Vec<String>, Vec<String>) {
        let (seq, project) = (self.history_for(active), &self.project_history);
        let label = |e: &HistoryEntry, is_project: bool| if is_project { format!("{} (project)", e.label()) } else { e.label().to_owned() };
        let mut undo: Vec<(u64, String)> = seq.undo_stamped().map(|(s, e)| (s, label(e, false))).chain(project.undo_stamped().map(|(s, e)| (s, label(e, true)))).collect();
        undo.sort_by_key(|(s, _)| *s);
        let last_sequence_edit = self.command_histories.values().map(|h| h.last_edit_stamp()).max().unwrap_or(0);
        let mut redo: Vec<(u64, String)> = seq.redo_stamped().filter(|(s, _)| *s > project.last_edit_stamp()).map(|(s, e)| (s, label(e, false)))
            .chain(project.redo_stamped().filter(|(s, _)| *s > last_sequence_edit).map(|(s, e)| (s, label(e, true))))
            .collect();
        redo.sort_by_key(|(s, _)| *s);
        (undo.into_iter().map(|(_, l)| l).collect(), redo.into_iter().map(|(_, l)| l).collect())
    }

    /// Take the next step back, from whichever history holds it.
    pub fn take_undo(&mut self, active: SequenceId, setup: &HistorySetup<'_>) -> Option<HistoryEntry> {
        self.prune_stale_redo(active, setup);
        match self.next_undo(active)? {
            Scope::Project => self.project_history_mut(setup).undo(),
            Scope::Sequence => self.history_for_mut(active, setup).undo(),
        }
    }

    /// Put the next step back, from whichever history holds it.
    pub fn take_redo(&mut self, active: SequenceId, setup: &HistorySetup<'_>) -> Option<HistoryEntry> {
        self.prune_stale_redo(active, setup);
        match self.next_redo(active)? {
            Scope::Project => self.project_history_mut(setup).redo(),
            Scope::Sequence => self.history_for_mut(active, setup).redo(),
        }
    }

    /// Record `command`, already applied: a project step into the project's
    /// history (reachable from every sequence), else into the active
    /// sequence's; then the redo steps it made stale go. Inside a batch the
    /// sequence has open, a project step is one more edit of it.
    pub fn record(&mut self, active: SequenceId, setup: &HistorySetup<'_>, command: EditCommand) -> Scope {
        let scope = if command.scope() == Scope::Project && !self.history_for(active).is_batching() { Scope::Project } else { Scope::Sequence };
        match scope {
            Scope::Project => {
                // A gesture of the sequence's under way ends: what follows
                // is a new step, not folded into one from before this.
                self.history_for_mut(active, setup).end_burst();
                self.project_history_mut(setup).push(HistoryEntry { command });
            }
            Scope::Sequence => self.history_for_mut(active, setup).push(HistoryEntry { command }),
        }
        self.prune_stale_redo(active, setup);
        scope
    }

    /// Every history kept to `max` steps.
    pub fn set_history_max_size(&mut self, max: usize) {
        for h in self.command_histories.values_mut() {
            h.set_max_size(max);
        }
        self.project_history.set_max_size(max);
    }

    /// Whether any history has a drag under way (autosave waits).
    pub fn any_drag_in_progress(&self) -> bool {
        self.command_histories.values().any(|h| h.is_any_drag_in_progress()) || self.project_history.is_any_drag_in_progress()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sequence_step(label: &str) -> EditCommand {
        EditCommand::Named { label: label.into(), edit: Box::new(EditCommand::Batch(Vec::new())) }
    }

    fn project_step(label: &str) -> EditCommand {
        EditCommand::SetComposition { id: klipp_timeline::CompositionId::new(), before: None, after: None, label: label.into() }
    }

    /// Undo takes whichever step moved last, the sequence's or the
    /// project's; redo puts them back in turn; an edit after an undo makes
    /// the redo it would have offered stale. All on a Document alone.
    #[test]
    fn undo_takes_the_latest_step_of_the_sequence_and_the_project() {
        let mut d = Document::default();
        let shared = std::sync::Arc::new(klipp_core::ControlShared::default());
        let setup = HistorySetup { max_size: 100, revision: &shared };
        let active = d.sequences[0].id;

        assert_eq!(d.record(active, &setup, sequence_step("Cut")), Scope::Sequence);
        assert_eq!(d.record(active, &setup, project_step("Colour")), Scope::Project);
        assert_eq!(d.record(active, &setup, sequence_step("Trim")), Scope::Sequence);
        assert_eq!((d.undo_depth(active), d.undo_label(active).as_deref()), (3, Some("Trim")));
        assert_eq!(d.history_view(active).0, ["Cut", "Colour (project)", "Trim"]);

        assert_eq!(d.take_undo(active, &setup).map(|e| e.label().to_owned()).as_deref(), Some("Trim"));
        assert_eq!(d.undo_label(active).as_deref(), Some("Colour (project)"), "the project's is next");
        assert_eq!(d.take_undo(active, &setup).map(|e| e.label().to_owned()).as_deref(), Some("Colour"));
        assert_eq!((d.redo_depth(active), d.redo_label(active).as_deref()), (2, Some("Colour (project)")));
        assert_eq!(d.take_redo(active, &setup).map(|e| e.label().to_owned()).as_deref(), Some("Colour"));
        assert_eq!(d.redo_label(active).as_deref(), Some("Trim"));

        // A new edit: the Trim waiting to be put back is gone.
        d.record(active, &setup, sequence_step("Ripple"));
        assert_eq!(d.redo_depth(active), 0);
        assert!(d.take_redo(active, &setup).is_none());
        assert!(!d.any_drag_in_progress());
    }
}
