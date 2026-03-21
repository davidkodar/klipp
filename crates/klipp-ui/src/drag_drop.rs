use klipp_core::MediaId;

/// Payload being dragged across panels.
#[derive(Debug, Clone)]
pub enum DragPayload {
    /// Dragging an effect from the effects browser tab.
    Effect { name: String },
    /// Dragging a transition from the transitions browser tab.
    Transition { name: String },
    /// Dragging a media item from the media browser tab.
    MediaClip { media_id: MediaId },
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
