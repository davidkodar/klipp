//! Recording track automation (#2306): the recorder ticked each frame,
//! the engine told which controls to play at a moved value over their
//! keys, and the pass put into the tracks when playback stops.

use super::*;

impl KlippApp {
    /// One frame of the automation pass: while playing, the controls their
    /// tracks' modes write are sampled at the playhead; when playback
    /// stops, the pass is thinned into the tracks' keys, one undo step.
    /// Every frame, the engine plays the controls being moved as moved.
    pub(super) fn tick_automation(&mut self) {
        let playing = self.state.playback == PlaybackState::Playing;
        let secs = self.state.playhead.to_secs_f64();
        let seq = &self.state.sequences[self.state.active_sequence_idx];
        let changed = self.state.automation_pass.tick(seq, &self.state.automation_held, playing, secs);
        self.send_automation_overrides(changed);
        if playing || !self.state.automation_pass.has_samples() {
            return;
        }
        let seq = &self.state.sequences[self.state.active_sequence_idx];
        let edits: Vec<EditCommand> = self
            .state
            .automation_pass
            .finish(seq)
            .into_iter()
            .filter_map(|(track_index, new)| {
                let old = seq.tracks.get(track_index)?.clone();
                Some(EditCommand::SetTrack { description: "Automation Pass", track_index, old: Box::new(old), new: Box::new(new) })
            })
            .collect();
        self.apply_edits_as_one(&edits, "Automation Pass");
    }

    /// Another project opening: the pass is dropped, and the controls
    /// moved go back to their keys.
    pub(super) fn drop_automation_pass(&mut self) {
        self.state.automation_held.clear();
        let changed = self.state.automation_pass.drop_pass();
        self.send_automation_overrides(changed);
    }

    fn send_automation_overrides(&self, changed: Vec<(usize, klipp_core::automation::TrackParam, Option<f32>)>) {
        for (track_index, param, value) in changed {
            let _ = self.cmd_tx.send(AppCommand::OverrideTrackAutomation { track_index, param, value });
        }
    }
}
