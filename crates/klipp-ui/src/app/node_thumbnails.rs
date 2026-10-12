//! Thumbnail requests follow the graph view, and accept only its current render snapshot.

use super::*;
use klipp_compose::InspectAt;
use klipp_engine::node_thumbnails::PreviewQuality;
use klipp_timeline::SequenceRevision;

#[derive(Default)]
pub(crate) struct ThumbnailState {
    request: u64,
    clip: Option<ClipId>,
    wanted: Vec<InspectAt>,
    revision: Option<SequenceRevision>,
    frame: Option<i64>,
    publication: Option<u64>,
    source_sent: Option<(u64, usize)>,
    pub quality: Option<PreviewQuality>,
}

#[derive(Debug, PartialEq, Eq)]
enum RequestUpdate {
    Parked,
    Current,
    Send(u64),
}

impl ThumbnailState {
    fn update_request(&mut self, revision: SequenceRevision, frame: i64, parked: bool) -> RequestUpdate {
        // A drained slider tick can advance the sequence with an empty
        // queue while its drag remains open. Request just once on release.
        if parked { return RequestUpdate::Parked; }
        if self.revision == Some(revision) && self.frame == Some(frame) {
            return RequestUpdate::Current;
        }
        self.request = self.request.wrapping_add(1);
        self.revision = Some(revision);
        self.frame = Some(frame);
        RequestUpdate::Send(self.request)
    }
}

impl AppState {
    pub(crate) fn invalidate_node_thumbnails(&mut self) {
        self.node_thumbnail_state.request = self.node_thumbnail_state.request.wrapping_add(1);
        self.node_thumbnail_state.revision = None;
        self.clear_node_thumbnails();
    }

    fn clear_node_thumbnails(&mut self) {
        if !self.node_thumbs.is_empty() {
            self.node_thumbs.clear();
            self.node_thumbs_gen = self.node_thumbs_gen.wrapping_add(1);
        }
        self.node_thumbs_unavailable.clear();
        self.node_thumbnail_state.publication = None;
        self.node_thumbnail_state.quality = None;
    }
}

impl AppState {
    /// How each node of `clip`'s graph fared on the frame shown (#1747),
    /// for the node editor's badges: the render's decline for it, else
    /// why its asked-for preview couldn't be made.
    pub(crate) fn node_feedback(&self, clip: ClipId) -> std::collections::HashMap<klipp_nodes::NodeId, crate::panels::node_graph_v2::NodeFeedback> {
        use crate::panels::node_graph_v2::{FeedbackKind, NodeFeedback};
        let mut out = std::collections::HashMap::new();
        for (at, why) in &self.node_thumbs_unavailable {
            if at.path.root != (klipp_graph_format::path::Root::Clip { clip: clip.0 }) { continue; }
            if let Some(node) = at.path.node() {
                out.insert(node, NodeFeedback { kind: FeedbackKind::NoPreview, reason: why.clone() });
            }
        }
        for d in self.frame_declines.iter().filter(|d| d.shown() && d.clip == Some(clip)) {
            let kind = if d.outcome == klipp_nodes::DeclineOutcome::RenderFailed { FeedbackKind::Failed } else { FeedbackKind::Declined };
            out.insert(d.node, NodeFeedback { kind, reason: d.reason.clone() });
        }
        out
    }
}

impl KlippApp {
    pub(super) fn handle_node_thumbnail_request(&mut self, cmd: &AppCommand) -> bool {
        let AppCommand::SetVisibleNodeThumbs { clip_id, at, .. } = cmd else { return false };
        let wanted: Vec<_> = at.iter().take(klipp_engine::node_thumbnails::MAX_THUMBS).cloned().collect();
        if self.state.node_thumbnail_state.clip != Some(*clip_id) || self.state.node_thumbnail_state.wanted != wanted {
            self.state.invalidate_node_thumbnails();
            self.state.node_thumbnail_state.clip = Some(*clip_id);
            self.state.node_thumbnail_state.wanted = wanted;
        }
        self.refresh_node_thumbnails();
        true
    }

    /// Revisions reject old answers without retaining a sequence. Requests
    /// wait until both the drag and its queued parameter writes are done.
    pub(super) fn refresh_node_thumbnails(&mut self) {
        self.node_graph_v2_panel.thumbnail_quality = None;
        let visible = (self.state.graph_dock_open || (self.state.show_color_panel && self.state.show_node_strip))
            && self.state.playback != klipp_engine::PlaybackState::Playing;
        // The clip whose graph is drawn: the dock's, which may be the
        // picture under the playhead with nothing selected (its nodes
        // stayed blank), else the selected clip's.
        let selected = match (self.state.graph_dock_open, self.state.graph_dock_root) {
            (true, Some(klipp_graph_format::path::Root::Clip { clip })) => Some(ClipId(clip)),
            (true, _) => None,
            (false, _) => self.state.selected_clip,
        };
        if !visible || self.state.node_thumbnail_state.clip != selected {
            if let Some(clip_id) = self.state.node_thumbnail_state.clip.take() {
                self.state.invalidate_node_thumbnails();
                self.state.node_thumbnail_state.wanted.clear();
                let _ = self.cmd_tx.send(AppCommand::SetVisibleNodeThumbs {
                    request: self.state.node_thumbnail_state.request, clip_id, at: Vec::new(),
                });
                self.node_graph_v2_panel.invalidate_thumbnail_requests();
            }
            self.state.clear_node_thumbnails();
            return;
        }
        let Some(clip_id) = selected else { return };
        let revision = self.sequence.revision();
        let frame = klipp_engine::node_thumbnails::frame_index(self.state.playhead, self.state.sequence().fps);
        let parked = self.state.any_drag_in_progress() || !self.param_change_queue.is_empty();
        match self.state.node_thumbnail_state.update_request(revision, frame, parked) {
            RequestUpdate::Parked => {
                self.state.clear_node_thumbnails();
                return;
            }
            RequestUpdate::Send(request) => {
                self.state.clear_node_thumbnails();
                let _ = self.cmd_tx.send(AppCommand::SetVisibleNodeThumbs {
                    request, clip_id, at: self.state.node_thumbnail_state.wanted.clone(),
                });
            }
            RequestUpdate::Current => {}
        }
        {
            let fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            let batch = fb.node_thumbnail_batch.as_ref().filter(|b|
                b.matches(self.state.node_thumbnail_state.request, frame, revision));
            if let Some(batch) = batch {
                if self.state.node_thumbnail_state.publication != Some(fb.node_thumbs_gen) {
                    self.state.node_thumbs = batch.images.clone();
                    self.state.node_thumbs_unavailable = batch.unavailable.clone();
                    self.state.node_thumbs_gen = self.state.node_thumbs_gen.wrapping_add(1);
                    self.state.node_thumbnail_state.publication = Some(fb.node_thumbs_gen);
                    self.state.node_thumbnail_state.quality = Some(batch.quality);
                }
            } else {
                self.state.clear_node_thumbnails();
            }
        }
        self.node_graph_v2_panel.thumbnail_quality = self.state.node_thumbnail_state.quality;
        self.send_filmstrip_node_thumbnails(clip_id);
    }

    fn send_filmstrip_node_thumbnails(&mut self, clip_id: ClipId) {
        if self.state.node_thumbnail_state.wanted.is_empty() { return; }
        let Some(clip) = self.state.sequence().all_clips().find(|c| c.id == clip_id) else { return };
        let Some(media) = clip.media_id() else { return };
        let source_time = clip.source_in.to_secs_f64()
            + (self.state.playhead - clip.timeline_in).to_secs_f64() * clip.speed.max(0.001);
        let Some(frames) = self.state.thumbnail_cache.filmstrip(&media) else { return };
        let Some((index, tile)) = frames.iter().enumerate().min_by(|(_, a), (_, b)|
            (a.time_secs - source_time).abs().total_cmp(&(b.time_secs - source_time).abs())) else { return };
        let request = self.state.node_thumbnail_state.request;
        if self.state.node_thumbnail_state.source_sent == Some((request, index)) { return; }
        let _ = self.cmd_tx.send(AppCommand::RenderNodeThumbsFromSource {
            request, clip_id, playhead: self.state.playhead,
            src_rgba: tile.rgba.clone(), src_w: tile.width, src_h: tile.height,
        });
        self.state.node_thumbnail_state.source_sent = Some((request, index));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clearing_for_an_unchanged_drag_allows_the_same_publication_to_return() {
        let mut state = AppState::default();
        state.node_thumbnail_state.publication = Some(7);
        state.node_thumbnail_state.quality = Some(PreviewQuality::Viewer);
        state.clear_node_thumbnails();
        assert_ne!(state.node_thumbnail_state.publication, Some(7), "the unchanged viewer answer must be copied back after release");
        assert_eq!(state.node_thumbnail_state.quality, None);
    }

    #[test]
    fn thumbnail_requests_wait_for_all_drag_steps_and_send_once_on_release() {
        let shared = klipp_timeline::SharedSequence::new(klipp_timeline::Sequence::new_default());
        let mut state = ThumbnailState::default();
        assert!(matches!(state.update_request(shared.revision(), 12, false), RequestUpdate::Send(_)));
        let request = state.request;
        for step in 0..20 {
            shared.write().unwrap().name = format!("drained step {step}");
            // The engine has drained its queue, but the UI drag is open.
            assert_eq!(state.update_request(shared.revision(), 12, true), RequestUpdate::Parked);
            assert_eq!(state.request, request, "no extra regrade or filmstrip request while dragging");
        }
        assert!(matches!(state.update_request(shared.revision(), 12, false), RequestUpdate::Send(_)));
        let released = state.request;
        for _ in 0..4 {
            assert_eq!(state.update_request(shared.revision(), 12, false), RequestUpdate::Current);
        }
        assert_eq!(released, request + 1);
        shared.write().unwrap().name = "pending final write".into();
        assert_eq!(state.update_request(shared.revision(), 12, true), RequestUpdate::Parked);
        assert!(matches!(state.update_request(shared.revision(), 12, false), RequestUpdate::Send(_)));
    }

    #[test]
    fn a_live_writer_invalidates_thumbnails_before_an_undo_step_exists() {
        let mut state = AppState::default();
        let clip = ClipId::new();
        let node = uuid::Uuid::new_v4();
        let at = klipp_compose::inspect::grade_node(clip, node);
        state.node_thumbs.insert(at.clone(), vec![0; klipp_engine::node_thumbnails::BYTES_PER_THUMB]);
        let before = state.node_thumbnail_state.request;
        state.before_dispatch(&AppCommand::SetParamAt {
            addr: at.path, key: "exposure".into(), value: klipp_graph_format::ParamValue::Float(0.5),
        });
        assert!(state.node_thumbs.is_empty());
        assert!(state.node_thumbnail_state.request > before);
        assert_eq!(state.active_history().undo_count(), 0);
    }
}
