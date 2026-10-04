//! Markers: added, changed, moved and taken off, on the timeline (#826).

use super::*;

impl KlippApp {
    pub(super) fn handle_marker_commands(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        match cmd {
            AppCommand::AddMarkerWith { marker_id, position, duration, label, comment, color, kind } => {
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                seq.markers.push(Marker {
                    id: *marker_id,
                    position: *position,
                    label: label.clone(),
                    color: *color,
                    duration: *duration,
                    comment: comment.clone(),
                    kind: *kind,
                });
                seq.markers.sort_by(|a, b| a.position.to_secs_f64().partial_cmp(&b.position.to_secs_f64()).unwrap_or(std::cmp::Ordering::Equal));
                self.state.dirty = true;
                self.sync_sequence_to_engine();
                true
            }
            AppCommand::AddMarker { position, label, color } => {
                self.state.sequences[self.state.active_sequence_idx].markers.push(Marker {
                    id: MarkerId::new(),
                    position: *position,
                    label: label.clone(),
                    color: *color,
                    duration: None,
                    comment: String::new(),
                    kind: klipp_core::MarkerKind::None,
                });
                self.state.sequences[self.state.active_sequence_idx].markers.sort_by(|a, b| {
                    a.position.to_secs_f64().partial_cmp(&b.position.to_secs_f64()).unwrap_or(std::cmp::Ordering::Equal)
                });
                self.state.dirty = true;
                // The shared sequence too: the control socket reads its
                // markers from there, and missed any made here.
                self.sync_sequence_to_engine();
                true
            }
            AppCommand::RemoveMarker { marker_id } => {
                self.state.sequences[self.state.active_sequence_idx].markers.retain(|m| m.id != *marker_id);
                self.state.dirty = true;
                self.sync_sequence_to_engine();
                true
            }
            AppCommand::UpdateMarkerLabel { marker_id, label } => {
                if let Some(m) = self.state.sequences[self.state.active_sequence_idx].markers.iter_mut().find(|m| m.id == *marker_id) {
                    m.label = label.clone();
                }
                self.state.dirty = true;
                self.sync_sequence_to_engine();
                true
            }
            AppCommand::UpdateMarkerColor { marker_id, color } => {
                if let Some(m) = self.state.sequences[self.state.active_sequence_idx].markers.iter_mut().find(|m| m.id == *marker_id) {
                    m.color = *color;
                }
                self.state.dirty = true;
                self.sync_sequence_to_engine();
                true
            }
            AppCommand::UpdateMarkerComment { marker_id, comment } => {
                if let Some(m) = self.state.sequences[self.state.active_sequence_idx].markers.iter_mut().find(|m| m.id == *marker_id) {
                    m.comment = comment.clone();
                }
                self.state.dirty = true;
                self.sync_sequence_to_engine();
                true
            }
            AppCommand::UpdateMarkerKind { marker_id, kind } => {
                if let Some(m) = self.state.sequences[self.state.active_sequence_idx].markers.iter_mut().find(|m| m.id == *marker_id) {
                    m.kind = *kind;
                }
                self.state.dirty = true;
                self.sync_sequence_to_engine();
                true
            }
            AppCommand::SetMarkerDuration { marker_id, duration } => {
                if let Some(m) = self.state.sequences[self.state.active_sequence_idx].markers.iter_mut().find(|m| m.id == *marker_id) {
                    m.duration = *duration;
                }
                self.state.dirty = true;
                self.sync_sequence_to_engine();
                true
            }
            AppCommand::JumpToMarker { marker_id } => {
                if let Some(m) = self.state.sequences[self.state.active_sequence_idx].markers.iter().find(|m| m.id == *marker_id) {
                    let pos = m.position;
                    self.state.playhead = pos;
                    let _ = self.cmd_tx.send(AppCommand::SeekTo(pos));
                }
                true
            }
            AppCommand::AddClipMarker { clip_id, time_secs, name, color } => {
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.clip_markers.push(klipp_timeline::ClipMarker {
                            time_secs: *time_secs,
                            name: name.clone(),
                            color: *color,
                            comment: String::new(),
                        });
                        clip.clip_markers.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                        break;
                    }
                }
                self.sync_sequence_to_engine();
                self.state.dirty = true;
                true
            }
            AppCommand::RemoveClipMarker { clip_id, index } => {
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        if *index < clip.clip_markers.len() {
                            clip.clip_markers.remove(*index);
                        }
                        break;
                    }
                }
                self.sync_sequence_to_engine();
                self.state.dirty = true;
                true
            }
            _ => false,
        }
    }
}
