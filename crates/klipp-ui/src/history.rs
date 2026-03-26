use klipp_core::{ClipId, MediaId, RationalTime};
use klipp_timeline::Clip;

#[derive(Debug, Clone)]
pub enum EditCommand {
    // === LEGACY (Free tracks only — existing behaviour unchanged) ===
    LegacyAppendClip {
        clip: Clip,
        track_index: usize,
    },
    LegacyRemoveClip {
        clip: Clip,
        track_index: usize,
        position: usize,
    },
    LegacyRippleRemove {
        removed_clip: Clip,
        track_index: usize,
        position: usize,
        shifted_clips: Vec<(ClipId, RationalTime, RationalTime)>,
    },
    LegacyMoveClip {
        clip_id: ClipId,
        old_timeline_in: RationalTime,
        old_timeline_out: RationalTime,
        new_timeline_in: RationalTime,
        new_timeline_out: RationalTime,
    },
    LegacyTrimClip {
        clip_id: ClipId,
        old_source_in: RationalTime,
        old_source_out: RationalTime,
        old_timeline_in: RationalTime,
        old_timeline_out: RationalTime,
        new_source_in: RationalTime,
        new_source_out: RationalTime,
        new_timeline_in: RationalTime,
        new_timeline_out: RationalTime,
    },
    LegacyRollTrimClip {
        clip_a_id: ClipId,
        clip_b_id: ClipId,
        old_a_source_out: RationalTime,
        old_a_timeline_out: RationalTime,
        old_b_source_in: RationalTime,
        old_b_timeline_in: RationalTime,
        new_a_source_out: RationalTime,
        new_a_timeline_out: RationalTime,
        new_b_source_in: RationalTime,
        new_b_timeline_in: RationalTime,
    },
    LegacyBladeClip {
        track_index: usize,
        original_clip: Clip,
        clip_a: Clip,
        clip_b: Clip,
        position: usize,
    },
    LegacyCutClip {
        clip: Clip,
        media_item: klipp_media::MediaItem,
        track_index: usize,
        position: usize,
        shifted_clips: Vec<(ClipId, RationalTime, RationalTime)>,
    },
    LegacyPasteClip {
        clip: Clip,
        track_index: usize,
    },
    LegacyMoveClipToTrack {
        clip: Clip,
        old_timeline_in: RationalTime,
        old_timeline_out: RationalTime,
        new_timeline_in: RationalTime,
        from_track: usize,
        to_track: usize,
    },

    // === MAGNETIC (Primary/Connected tracks) ===
    Magnetic(klipp_timeline::magnetic_commands::MagneticEditCommand),

    // === MEDIA POOL ===
    AddMediaToPool {
        item: klipp_media::MediaItem,
    },
    RemoveMediaFromPool {
        item: klipp_media::MediaItem,
        clip_count: usize,
    },
    MoveMediaToBin {
        media_id: MediaId,
        from_bin: Option<String>,
        to_bin: Option<String>,
    },
    SetMediaTags {
        media_id: MediaId,
        old_tags: Vec<String>,
        new_tags: Vec<String>,
    },
    SetMediaRating {
        media_id: MediaId,
        old_rating: u8,
        new_rating: u8,
    },
    SetMediaColorLabel {
        media_id: MediaId,
        old_label: u8,
        new_label: u8,
    },
    RenameBin {
        old_name: String,
        new_name: String,
    },

    // === RANGE SELECTION ===
    SetRange {
        old_in: Option<RationalTime>,
        old_out: Option<RationalTime>,
        new_in: Option<RationalTime>,
        new_out: Option<RationalTime>,
    },

    // === NODE GRAPH (color grading) ===
    SetNodeGraph {
        clip_id: ClipId,
        old_graph: klipp_nodes::NodeGraph,
        new_graph: klipp_nodes::NodeGraph,
    },

    // === CLIP AUDIO ===
    EditClipAudio {
        clip_id: ClipId,
        old_gain: f32,
        old_volume: f32,
        old_pan: f32,
        old_fade_in: f32,
        old_fade_out: f32,
        old_channel_config: klipp_timeline::ChannelConfig,
        new_gain: f32,
        new_volume: f32,
        new_pan: f32,
        new_fade_in: f32,
        new_fade_out: f32,
        new_channel_config: klipp_timeline::ChannelConfig,
    },

    // === BUILTIN EFFECTS ===
    EditBuiltinEffect {
        clip_id: ClipId,
        effect_index: usize,
        old_effect: klipp_effects::BuiltinEffect,
        new_effect: klipp_effects::BuiltinEffect,
    },

}

impl EditCommand {
    pub fn label(&self) -> &'static str {
        match self {
            EditCommand::LegacyAppendClip { .. } => "Append Clip",
            EditCommand::LegacyRemoveClip { .. } | EditCommand::LegacyRippleRemove { .. } => "Remove Clip",
            EditCommand::LegacyMoveClip { .. } | EditCommand::LegacyMoveClipToTrack { .. } => "Move Clip",
            EditCommand::LegacyTrimClip { .. } => "Trim Clip",
            EditCommand::LegacyRollTrimClip { .. } => "Roll Trim",
            EditCommand::LegacyBladeClip { .. } => "Blade Clip",
            EditCommand::LegacyCutClip { .. } => "Cut Clip",
            EditCommand::LegacyPasteClip { .. } => "Paste Clip",
            EditCommand::Magnetic(cmd) => cmd.label(),
            EditCommand::AddMediaToPool { .. } => "Import Media",
            EditCommand::RemoveMediaFromPool { .. } => "Remove Media",
            EditCommand::MoveMediaToBin { .. } => "Move to Bin",
            EditCommand::SetMediaTags { .. } => "Set Tags",
            EditCommand::SetMediaRating { .. } => "Set Rating",
            EditCommand::SetMediaColorLabel { .. } => "Set Color Label",
            EditCommand::RenameBin { .. } => "Rename Bin",
            EditCommand::SetRange { .. } => "Set Range",
            EditCommand::SetNodeGraph { .. } => "Color Grade",
            EditCommand::EditClipAudio { .. } => "Edit Audio",
            EditCommand::EditBuiltinEffect { .. } => "Edit Effect",
        }
    }
}

/// Wraps an EditCommand with the sequence index it targets.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub sequence_idx: usize,
    pub command: EditCommand,
}

impl HistoryEntry {
    pub fn label(&self) -> &'static str {
        self.command.label()
    }
}

#[derive(Default)]
pub struct CommandHistory {
    undo_stack: Vec<HistoryEntry>,
    redo_stack: Vec<HistoryEntry>,
    max_size: usize,

    // Coalescing state — for drag gesture support
    /// Identifies which param is being dragged (e.g. "clip_id:builtin:index")
    pending_coalesce_key: Option<String>,
    /// "Before" state captured at drag start
    pending_old_entry: Option<HistoryEntry>,
}

impl CommandHistory {
    pub fn new() -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            max_size: 100,
            pending_coalesce_key: None,
            pending_old_entry: None,
        }
    }

    /// Set the maximum undo stack size. 0 = unlimited.
    pub fn set_max_size(&mut self, max: usize) {
        self.max_size = max;
    }

    pub fn push(&mut self, entry: HistoryEntry) {
        self.undo_stack.push(entry);
        self.redo_stack.clear();
        if self.max_size > 0 && self.undo_stack.len() > self.max_size {
            self.undo_stack.remove(0);
        }
    }

    /// Call when the user starts dragging a parameter (mouse down on slider/DragValue).
    /// Captures the current state as the "before" snapshot.
    /// While a drag is active, calls to push() with the same key are suppressed
    /// by the command handler checking is_dragging().
    pub fn begin_drag(&mut self, key: String, old_state: HistoryEntry) {
        self.pending_coalesce_key = Some(key);
        self.pending_old_entry = Some(old_state);
    }

    /// Call when the user releases the drag (mouse up).
    /// Pushes exactly ONE undo entry using the "before" snapshot from begin_drag
    /// and the "after" state passed here.
    pub fn end_drag(&mut self, final_state: HistoryEntry) {
        if self.pending_coalesce_key.take().is_some() {
            if let Some(old_entry) = self.pending_old_entry.take() {
                // Construct a combined entry: old state from begin_drag, new state from final_state
                let combined = combine_old_new(old_entry, final_state);
                self.push(combined);
            }
        }
    }

    /// Returns true if a drag is in progress for the given key.
    /// Command handlers check this to suppress individual undo pushes during drag.
    pub fn is_dragging(&self, key: &str) -> bool {
        self.pending_coalesce_key.as_deref() == Some(key)
    }

    /// Returns a reference to the pending old entry (for reading old state at end_drag).
    pub fn pending_old_entry_ref(&self) -> Option<&HistoryEntry> {
        self.pending_old_entry.as_ref()
    }

    pub fn undo(&mut self) -> Option<HistoryEntry> {
        let entry = self.undo_stack.pop()?;
        self.redo_stack.push(entry.clone());
        Some(entry)
    }

    pub fn redo(&mut self) -> Option<HistoryEntry> {
        let entry = self.redo_stack.pop()?;
        self.undo_stack.push(entry.clone());
        Some(entry)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    pub fn undo_label(&self) -> Option<&'static str> {
        self.undo_stack.last().map(|e| e.label())
    }

    pub fn redo_label(&self) -> Option<&'static str> {
        self.redo_stack.last().map(|e| e.label())
    }
}

/// Combines the old state from begin_drag with the new state from end_drag.
/// For EditBuiltinEffect: takes old_effect from old_entry, new_effect from new_entry.
/// For EditClipAudio: takes old_* from old_entry, new_* from new_entry.
fn combine_old_new(old_entry: HistoryEntry, new_entry: HistoryEntry) -> HistoryEntry {
    let command = match (&old_entry.command, &new_entry.command) {
        (
            EditCommand::EditBuiltinEffect { clip_id, effect_index, old_effect, .. },
            EditCommand::EditBuiltinEffect { new_effect, .. },
        ) => EditCommand::EditBuiltinEffect {
            clip_id: *clip_id,
            effect_index: *effect_index,
            old_effect: old_effect.clone(),
            new_effect: new_effect.clone(),
        },
        (
            EditCommand::EditClipAudio { clip_id, old_gain, old_volume, old_pan, old_fade_in, old_fade_out, old_channel_config, .. },
            EditCommand::EditClipAudio { new_gain, new_volume, new_pan, new_fade_in, new_fade_out, new_channel_config, .. },
        ) => EditCommand::EditClipAudio {
            clip_id: *clip_id,
            old_gain: *old_gain,
            old_volume: *old_volume,
            old_pan: *old_pan,
            old_fade_in: *old_fade_in,
            old_fade_out: *old_fade_out,
            old_channel_config: *old_channel_config,
            new_gain: *new_gain,
            new_volume: *new_volume,
            new_pan: *new_pan,
            new_fade_in: *new_fade_in,
            new_fade_out: *new_fade_out,
            new_channel_config: *new_channel_config,
        },
        // For any other type, just use the new entry as-is
        _ => new_entry.command.clone(),
    };
    HistoryEntry {
        sequence_idx: new_entry.sequence_idx,
        command,
    }
}
