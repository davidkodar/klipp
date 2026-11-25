//! Connected clips and storylines: a clip connected to the primary clip
//! nearest it or disconnected, a connected track promoted to a storyline
//! or broken apart. Moved out of the G / Shift+G / Shift+S key handlers so
//! the clip's right-click menu can do the same (#911): its Connect,
//! Disconnect, Promote and Break Apart items only closed the menu.

use super::*;

impl KlippApp {
    /// The menu's storyline commands: `false` for any other.
    pub(super) fn handle_storyline_commands(&mut self, cmd: &AppCommand) -> bool {
        match cmd {
            AppCommand::ConnectToPrimary { clip_id } => {
                self.connect_to_primary(*clip_id);
                true
            }
            AppCommand::DisconnectFromPrimary { clip_id } => {
                self.disconnect_from_primary(*clip_id);
                true
            }
            AppCommand::ToggleStoryline { clip_id } => {
                self.toggle_storyline(*clip_id);
                true
            }
            _ => false,
        }
    }

    /// Connect `clip_id` to the primary clip nearest its start. False when
    /// it is on the primary track, or there is no primary clip.
    pub(super) fn connect_to_primary(&mut self, clip_id: ClipId) -> bool {
        let seq = &self.state.document.sequences[self.state.active_sequence_idx];
        let Some(ti) = seq.tracks.iter().position(|t| t.clips.iter().any(|c| c.id == clip_id)) else { return false };
        if seq.tracks[ti].magnetic_role == klipp_timeline::TrackRole::Primary {
            return false;
        }
        let Some(clip_in) = seq.tracks[ti].clips.iter().find(|c| c.id == clip_id).map(|c| c.timeline_in) else { return false };
        let Some(primary_idx) = seq.primary_track_index() else { return false };
        let Some(anchor_id) = seq.tracks[primary_idx]
            .clips
            .iter()
            .filter(|c| !c.kind.is_gap())
            .min_by_key(|c| (c.timeline_in - clip_in).abs().value.unsigned_abs())
            .map(|c| c.id)
        else {
            return false;
        };
        self.magnetic_step(|seq| klipp_timeline::magnetic::MagneticEditor::connect_clip(seq, clip_id, anchor_id, klipp_timeline::AnchorAlignment::StartAligned));
        true
    }

    /// Disconnect `clip_id` from the primary clip it is connected to.
    /// False when it is not connected.
    pub(super) fn disconnect_from_primary(&mut self, clip_id: ClipId) -> bool {
        let connected = self.state.document.sequences[self.state.active_sequence_idx]
            .all_clips()
            .any(|c| c.id == clip_id && c.magnetic_anchor.is_some());
        if !connected {
            return false;
        }
        self.magnetic_step(|seq| klipp_timeline::magnetic::MagneticEditor::disconnect_clip(seq, clip_id));
        true
    }

    /// The track `clip_id` is on promoted to a connected storyline, or a
    /// storyline broken apart again. False when the track is neither.
    pub(super) fn toggle_storyline(&mut self, clip_id: ClipId) -> bool {
        let seq = &self.state.document.sequences[self.state.active_sequence_idx];
        let Some(ti) = seq.tracks.iter().position(|t| t.clips.iter().any(|c| c.id == clip_id)) else { return false };
        let anchor = match seq.tracks[ti].magnetic_role {
            klipp_timeline::TrackRole::Connected => {
                let Some(primary_idx) = seq.primary_track_index() else { return false };
                let Some(clip_in) = seq.tracks[ti].clips.first().map(|c| c.timeline_in) else { return false };
                let Some(anchor_id) = seq.tracks[primary_idx]
                    .clips
                    .iter()
                    .filter(|c| !c.kind.is_gap())
                    .min_by_key(|c| (c.timeline_in - clip_in).abs().value.unsigned_abs())
                    .map(|c| c.id)
                else {
                    return false;
                };
                Some(anchor_id)
            }
            klipp_timeline::TrackRole::ConnectedStoryline => None,
            _ => return false,
        };
        self.magnetic_step(|seq| match anchor {
            Some(anchor_id) => klipp_timeline::magnetic::MagneticEditor::promote_to_storyline(seq, ti, anchor_id),
            None => klipp_timeline::magnetic::MagneticEditor::break_apart_storyline(seq, ti),
        });
        true
    }

    /// A magnetic edit of the active sequence made as one step: worked out
    /// by `edit`, made through one commit, the engine told.
    fn magnetic_step(&mut self, edit: impl FnOnce(&mut klipp_timeline::Sequence) -> klipp_timeline::magnetic::MagneticResult) {
        self.commit_worked_out(|app| Some(EditCommand::Magnetic(edit(app.state.sequence_mut()).command)));
        self.sync_sequence_to_engine();
    }
}
