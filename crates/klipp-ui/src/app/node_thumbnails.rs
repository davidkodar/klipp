//! Thumbnail requests follow the graph view, and accept only its current render snapshot.

use super::*;
use klipp_compose::InspectAt;
use klipp_engine::node_thumbnails::PreviewQuality;
use std::sync::Arc;

#[derive(Default)]
pub(crate) struct ThumbnailState {
    request: u64,
    clip: Option<ClipId>,
    wanted: Vec<InspectAt>,
    snapshot: Option<Arc<klipp_timeline::Sequence>>,
    frame: Option<klipp_core::RationalTime>,
    publication: u64,
    source_sent: Option<(u64, usize)>,
    pub quality: Option<PreviewQuality>,
}

impl AppState {
    pub(crate) fn invalidate_node_thumbnails(&mut self) {
        self.node_thumbnail_state.request = self.node_thumbnail_state.request.wrapping_add(1);
        self.node_thumbnail_state.snapshot = None;
        self.clear_node_thumbnails();
    }

    fn clear_node_thumbnails(&mut self) {
        if !self.node_thumbs.is_empty() {
            self.node_thumbs.clear();
            self.node_thumbs_gen = self.node_thumbs_gen.wrapping_add(1);
        }
        self.node_thumbnail_state.quality = None;
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

    /// Snapshot identity changes on every shared-sequence write while a
    /// request retains the old snapshot. Also reject while a UI parameter
    /// change has not reached that shared sequence yet.
    pub(super) fn refresh_node_thumbnails(&mut self) {
        self.node_graph_v2_panel.thumbnail_quality = None;
        let visible = (self.state.graph_dock_open || (self.state.show_color_panel && self.state.show_node_strip))
            && self.state.playback != klipp_engine::PlaybackState::Playing;
        let selected = self.state.selected_clip;
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
        if !self.param_change_queue.is_empty() {
            self.state.clear_node_thumbnails();
            return;
        }
        let snapshot = self.sequence.snapshot();
        let frame = self.state.playhead;
        let fresh_context = self.state.node_thumbnail_state.frame != Some(frame)
            || !self.state.node_thumbnail_state.snapshot.as_ref().is_some_and(|old| Arc::ptr_eq(old, &snapshot));
        if fresh_context {
            self.state.invalidate_node_thumbnails();
            self.state.node_thumbnail_state.snapshot = Some(snapshot.clone());
            self.state.node_thumbnail_state.frame = Some(frame);
            let _ = self.cmd_tx.send(AppCommand::SetVisibleNodeThumbs {
                request: self.state.node_thumbnail_state.request, clip_id,
                at: self.state.node_thumbnail_state.wanted.clone(),
            });
        }
        {
            let fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            let batch = fb.node_thumbnail_batch.as_ref().filter(|b|
                b.matches(self.state.node_thumbnail_state.request, frame, &snapshot));
            if let Some(batch) = batch {
                if self.state.node_thumbnail_state.publication != fb.node_thumbs_gen {
                    self.state.node_thumbs = batch.images.clone();
                    self.state.node_thumbs_gen = self.state.node_thumbs_gen.wrapping_add(1);
                    self.state.node_thumbnail_state.publication = fb.node_thumbs_gen;
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
