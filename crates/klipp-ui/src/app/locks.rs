//! Locked tracks: an edit refused because of one says so (#908). Move,
//! trim, roll, slip and slide returned early without a word, so a drag
//! on a locked track looked like a broken drag.

use super::*;

impl KlippApp {
    /// Whether `clip_id` sits on a locked track; when it does, a toast
    /// names the track.
    pub(super) fn refused_by_lock(&mut self, clip_id: &ClipId) -> bool {
        let locked = self.state.sequences[self.state.active_sequence_idx]
            .tracks
            .iter()
            .find(|t| t.locked && t.clips.iter().any(|c| c.id == *clip_id))
            .map(|t| t.name.clone());
        match locked {
            Some(name) => {
                self.state.toast.show(format!("{name} is locked"));
                true
            }
            None => false,
        }
    }
}
