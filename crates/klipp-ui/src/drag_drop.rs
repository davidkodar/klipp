use klipp_core::MediaId;
use klipp_timeline::TitleData;

/// Payload being dragged across panels.
#[derive(Debug, Clone)]
pub enum DragPayload {
    /// Dragging an effect from the effects browser tab.
    Effect {
        name: String,
        is_builtin: bool,
        /// OFX plugin identifier (if OFX), or empty for built-in.
        plugin_id: String,
    },
    /// Dragging an audio effect from the effects browser.
    AudioEffect {
        name: String,
        /// Audio effect type_id (e.g. "eq", "compressor", "reverb")
        type_id: String,
    },
    /// Dragging a transition from the transitions browser tab.
    Transition { name: String, kind_id: u8 },
    /// Dragging a media item from the media browser tab.
    MediaClip { media_id: MediaId },
    /// Dragging one or more files from the FILES browser tab. These files
    /// haven't been imported yet — drop targets must push `LoadMediaFile`
    /// per path before doing anything else with them.
    FilePaths { paths: Vec<std::path::PathBuf> },
    /// Dragging a title template from the titles browser tab.
    /// `list_key` is the template's key in the browser's Favourites and
    /// Recent, which tells a saved template from a built-in one.
    Title { template_name: String, list_key: String, title_data: TitleData },
    /// Dragging a generator preset from the generators browser tab.
    Generator { name: String, data: klipp_core::GeneratorData },
}

/// Global drag-and-drop state for cross-panel interactions.
#[derive(Default)]
pub struct DragDropState {
    /// Currently active drag payload.
    pub payload: Option<DragPayload>,
    /// Screen position of the drag cursor.
    pub cursor_pos: Option<egui::Pos2>,
    /// Whether the drag was started this frame.
    pub started: bool,
}

impl DragDropState {
    /// Begin a new drag operation.
    pub fn begin(&mut self, payload: DragPayload) {
        self.payload = Some(payload);
        self.started = true;
    }

    /// Update cursor position during drag.
    pub fn update_pos(&mut self, pos: egui::Pos2) {
        self.cursor_pos = Some(pos);
    }

    /// End the drag — returns the payload if there was one.
    pub fn end(&mut self) -> Option<DragPayload> {
        self.started = false;
        self.cursor_pos = None;
        self.payload.take()
    }

    /// Check if a drag is currently active.
    pub fn is_active(&self) -> bool {
        self.payload.is_some()
    }

    /// Reset started flag at beginning of each frame.
    pub fn frame_start(&mut self) {
        self.started = false;
    }
}

/// Which items travel with a drag from the media browser: the dragged one
/// alone, unless it is part of a multi-selection — then every selected
/// item, in the order the browser shows them.
///
/// Browser order, not selection order, because it is what Premiere,
/// Resolve, Avid and Final Cut all do, and what the user can see: sort
/// the browser, select, drag, and the timeline reads like the browser.
/// (The selection is a set; it has no order to honour anyway.) Dragging
/// an item that is not in the selection drags that item alone, which is
/// the rule everywhere.
pub fn drag_group(displayed: &[MediaId], selected: &std::collections::HashSet<MediaId>, dragged: MediaId) -> Vec<MediaId> {
    if selected.len() > 1 && selected.contains(&dragged) {
        displayed.iter().copied().filter(|id| selected.contains(id)).collect()
    } else {
        vec![dragged]
    }
}

/// Whether a media item can go on a track of this kind: audio-only media
/// on audio tracks, anything with a picture on video tracks, synthetic
/// titles on text and subtitle tracks only.
pub fn media_fits_track(item: &klipp_media::MediaItem, kind: klipp_timeline::TrackKind) -> bool {
    use klipp_timeline::TrackKind;
    let is_title = matches!(item.kind, klipp_media::MediaItemKind::SyntheticTitle);
    if is_title {
        return matches!(kind, TrackKind::Text | TrackKind::Subtitle);
    }
    let is_audio_only = item.video_codec.is_empty();
    match kind {
        TrackKind::Audio => is_audio_only,
        TrackKind::Text | TrackKind::Subtitle => false,
        _ => !is_audio_only,
    }
}

#[cfg(test)]
mod drag_group_tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn a_multi_selection_drags_in_browser_order_whatever_the_selection_order() {
        let ids: Vec<MediaId> = (0..5).map(|_| MediaId::new()).collect();
        // Selected 3, 1 and 4 — in that order, which a set forgets.
        let selected: HashSet<MediaId> = [ids[3], ids[1], ids[4]].into_iter().collect();
        assert_eq!(drag_group(&ids, &selected, ids[3]), vec![ids[1], ids[3], ids[4]]);
        // Re-sorting the browser re-orders the drag.
        let reversed: Vec<MediaId> = ids.iter().rev().copied().collect();
        assert_eq!(drag_group(&reversed, &selected, ids[3]), vec![ids[4], ids[3], ids[1]]);
    }

    #[test]
    fn dragging_an_item_outside_the_selection_drags_it_alone() {
        let ids: Vec<MediaId> = (0..4).map(|_| MediaId::new()).collect();
        let selected: HashSet<MediaId> = [ids[0], ids[1]].into_iter().collect();
        assert_eq!(drag_group(&ids, &selected, ids[3]), vec![ids[3]]);
    }

    #[test]
    fn a_single_selection_is_not_a_group() {
        let ids: Vec<MediaId> = (0..3).map(|_| MediaId::new()).collect();
        let selected: HashSet<MediaId> = [ids[1]].into_iter().collect();
        assert_eq!(drag_group(&ids, &selected, ids[1]), vec![ids[1]]);
        assert_eq!(drag_group(&ids, &HashSet::new(), ids[2]), vec![ids[2]]);
    }
}
