//! The one edit shown while a gesture is under way (a trim being dragged,
//! a slip, a slide): applied to the document so the timeline, the viewer
//! and the engine show it, but not yet a step of any history.
//!
//! Each frame the gesture works out the whole edit from where it began
//! ("trim this clip from 2.0 s to 2.7 s"), never a change since the last
//! frame, and shows it in place of the one shown before: that one taken
//! back by its inverse, this one applied. So nothing drifts however many
//! frames the drag lasts, cancelling is the last edit's inverse, and what
//! is recorded at the end is what was on screen.
//!
//! The live edit stays on the sequence it began on. It is ended or
//! cancelled before any other edit, undo or redo is made.

use klipp_core::SequenceId;

use crate::apply::{DocumentView, Effects};
use crate::history::EditCommand;

/// The edit shown now, if any, and the sequence it is shown on.
#[derive(Debug, Default)]
pub struct LiveEdit {
    shown: Option<(SequenceId, EditCommand)>,
}

impl LiveEdit {
    /// Whether an edit is shown.
    pub fn is_live(&self) -> bool {
        self.shown.is_some()
    }

    /// The sequence the edit is shown on.
    pub fn sequence(&self) -> Option<SequenceId> {
        self.shown.as_ref().map(|(id, _)| *id)
    }

    /// The edit shown, if any.
    pub fn edit(&self) -> Option<&EditCommand> {
        self.shown.as_ref().map(|(_, e)| e)
    }

    /// `edit`, made from the state before the first one shown, shown on the
    /// view's active sequence in place of the one shown before. The first
    /// call begins the live edit. What the host does comes back, for both.
    ///
    /// A live edit begun on another sequence is taken back there first.
    pub fn show(&mut self, view: &mut DocumentView<'_>, edit: EditCommand) -> Effects {
        let mut effects = self.cancel(view);
        effects.merge(view.apply_forward(&edit));
        let id = view.sequences[view.active].id;
        self.shown = Some((id, edit));
        effects
    }

    /// The edit shown taken back, on the sequence it was shown on; nothing
    /// when none is shown or that sequence is gone.
    pub fn cancel(&mut self, view: &mut DocumentView<'_>) -> Effects {
        let Some((id, edit)) = self.shown.take() else { return Effects::default() };
        let Some(idx) = view.sequences.iter().position(|s| s.id == id) else { return Effects::default() };
        let active = std::mem::replace(&mut view.active, idx);
        let effects = view.apply_inverse(&edit);
        view.active = active;
        effects
    }

    /// The edit shown, left applied, for the host to record as its step;
    /// the live edit ends. `None` when none is shown.
    pub fn end(&mut self) -> Option<(SequenceId, EditCommand)> {
        self.shown.take()
    }
}
