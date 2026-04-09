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
    /// Dragging a title template from the titles browser tab.
    Title { template_name: String, title_data: TitleData, animation: Option<String> },
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
