//! Markers: added, changed, moved and taken off, on the timeline (#826).

use super::*;

impl KlippApp {
    pub(super) fn handle_marker_commands(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        match cmd {
            AppCommand::AddMarkerWith { marker_id, position, duration, label, comment, color, kind } => {
                let marker = Marker { review: None, id: *marker_id, position: *position, label: label.clone(), color: *color, duration: *duration, comment: comment.clone(), kind: *kind };
                self.edit_markers(|m| {
                    m.push(marker);
                    sort_by_position(m);
                });
                true
            }
            AppCommand::AddMarker { position, label, color } => {
                let marker = Marker { review: None, id: MarkerId::new(), position: *position, label: label.clone(), color: *color, duration: None, comment: String::new(), kind: klipp_core::MarkerKind::None };
                self.edit_markers(|m| {
                    m.push(marker);
                    sort_by_position(m);
                });
                true
            }
            AppCommand::RemoveMarker { marker_id } => {
                self.edit_markers(|m| m.retain(|m| m.id != *marker_id));
                true
            }
            AppCommand::UpdateMarkerLabel { marker_id, label } => {
                self.edit_markers(|m| m.iter_mut().filter(|m| m.id == *marker_id).for_each(|m| m.label = label.clone()));
                true
            }
            AppCommand::UpdateMarkerColor { marker_id, color } => {
                self.edit_markers(|m| m.iter_mut().filter(|m| m.id == *marker_id).for_each(|m| m.color = *color));
                true
            }
            AppCommand::UpdateMarkerComment { marker_id, comment } => {
                self.edit_markers(|m| m.iter_mut().filter(|m| m.id == *marker_id).for_each(|m| m.comment = comment.clone()));
                true
            }
            AppCommand::SetReviewStatus { marker_id, status } => {
                self.edit_markers(|m| m.iter_mut().filter(|m| m.id == *marker_id).filter_map(|m| m.review.as_mut()).for_each(|r| r.status = *status));
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
                        self.edit_markers(|m| {
                            m.extend(added.iter().cloned());
                            m.sort_by_key(|m| m.position);
                        });
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
                self.edit_markers(|m| m.iter_mut().filter(|m| m.id == *marker_id).for_each(|m| m.kind = *kind));
                true
            }
            AppCommand::SetMarkerDuration { marker_id, duration } => {
                self.edit_markers(|m| m.iter_mut().filter(|m| m.id == *marker_id).for_each(|m| m.duration = *duration));
                true
            }
            AppCommand::MoveMarker { marker_id, position } => {
                self.edit_markers(|m| m.iter_mut().filter(|m| m.id == *marker_id).for_each(|m| m.position = *position));
                true
            }
            AppCommand::JumpToMarker { marker_id } => {
                if let Some(m) = self.state.document.sequences[self.state.active_sequence_idx].markers.iter().find(|m| m.id == *marker_id) {
                    let pos = m.position;
                    self.state.playhead = pos;
                    let _ = self.cmd_tx.send(AppCommand::SeekTo(pos));
                }
                true
            }
            AppCommand::AddClipMarker { clip_id, time_secs, name, color } => {
                for track in &mut self.state.document.sequences[self.state.active_sequence_idx].tracks {
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
                for track in &mut self.state.document.sequences[self.state.active_sequence_idx].tracks {
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

    /// The active sequence's markers changed by `f`, as one step through
    /// [`Self::commit`]; nothing when `f` changes nothing.
    fn edit_markers(&mut self, f: impl FnOnce(&mut Vec<Marker>)) {
        let old = self.state.sequence().markers.clone();
        let mut new = old.clone();
        f(&mut new);
        if new != old {
            self.commit(EditCommand::SetMarkers { old, new });
        }
    }
}

/// Markers in time order, as the timeline keeps them.
fn sort_by_position(markers: &mut [Marker]) {
    markers.sort_by(|a, b| a.position.to_secs_f64().partial_cmp(&b.position.to_secs_f64()).unwrap_or(std::cmp::Ordering::Equal));
}
