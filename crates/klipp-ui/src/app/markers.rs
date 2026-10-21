//! Markers: added, changed, moved and taken off, on the timeline (#826).

use super::*;

impl KlippApp {
    pub(super) fn handle_marker_commands(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        match cmd {
            AppCommand::AddMarkerWith { marker_id, position, duration, label, comment, color, kind } => {
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                seq.markers.push(Marker { review: None,
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
                self.state.sequences[self.state.active_sequence_idx].markers.push(Marker { review: None,
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
            AppCommand::SetReviewStatus { marker_id, status } => {
                if let Some(r) = self.state.sequences[self.state.active_sequence_idx].markers.iter_mut().find(|m| m.id == *marker_id).and_then(|m| m.review.as_mut()) {
                    r.status = *status;
                }
                self.state.dirty = true;
                self.sync_sequence_to_engine();
                true
            }
            AppCommand::ImportReviewComments { path } => {
                let text = match std::fs::read_to_string(path) {
                    Ok(t) => t,
                    Err(e) => {
                        self.state.toast.show(format!("Could not read the comments: {e}"));
                        return true;
                    }
                };
                let seq = self.state.sequence();
                let fps = seq.fps.as_f64();
                // The round the file names, else the latest a copy was made for.
                let round = klipp_document::review::file_round(&text).unwrap_or_else(|| klipp_document::review::next_round(seq).saturating_sub(1).max(1));
                match klipp_document::review::parse_comments(&text, fps) {
                    Ok(comments) => {
                        let added = klipp_document::review::markers(seq, &comments, round);
                        let skipped = comments.len() - added.len();
                        if added.is_empty() {
                            self.state.toast.show(format!("All {} comments are already here", comments.len()));
                            return true;
                        }
                        let old = seq.markers.clone();
                        let mut new = old.clone();
                        new.extend(added.iter().cloned());
                        new.sort_by_key(|m| m.position);
                        let idx = self.state.active_sequence_idx;
                        self.state.sequences[idx].markers = new.clone();
                        self.state.push_history(klipp_document::history::EditCommand::SetMarkers { old, new });
                        self.state.dirty = true;
                        self.sync_sequence_to_engine();
                        self.state.show_markers_panel = true;
                        self.state.toast.show(format!("{} review comment{} added for round {round}{}", added.len(), if added.len() == 1 { "" } else { "s" },
                            if skipped > 0 { format!(", {skipped} already here") } else { String::new() }));
                    }
                    Err(why) => self.state.toast.show(format!("No comments read: {why}")),
                }
                true
            }
            AppCommand::ExportReviewList { path } => {
                let csv = klipp_document::review::review_list_csv(self.state.sequence());
                match std::fs::write(path, csv) {
                    Ok(()) => self.state.toast.show(format!("Review list written to {}", path.display())),
                    Err(e) => self.state.toast.show(format!("Could not write the review list: {e}")),
                }
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
            AppCommand::MoveMarker { marker_id, position } => {
                if let Some(m) = self.state.sequences[self.state.active_sequence_idx].markers.iter_mut().find(|m| m.id == *marker_id) {
                    m.position = *position;
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
