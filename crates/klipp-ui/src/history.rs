use klipp_core::{ClipId, RationalTime};
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
}

impl EditCommand {
    pub fn label(&self) -> &'static str {
        match self {
            EditCommand::LegacyAppendClip { .. } => "Append Clip",
            EditCommand::LegacyRemoveClip { .. } | EditCommand::LegacyRippleRemove { .. } => "Remove Clip",
            EditCommand::LegacyMoveClip { .. } | EditCommand::LegacyMoveClipToTrack { .. } => "Move Clip",
            EditCommand::LegacyTrimClip { .. } => "Trim Clip",
            EditCommand::LegacyBladeClip { .. } => "Blade Clip",
            EditCommand::LegacyCutClip { .. } => "Cut Clip",
            EditCommand::LegacyPasteClip { .. } => "Paste Clip",
            EditCommand::Magnetic(cmd) => cmd.label(),
            EditCommand::RenameBin { .. } => "Rename Bin",
            EditCommand::SetRange { .. } => "Set Range",
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
}

impl CommandHistory {
    pub fn new() -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            max_size: 100,
        }
    }

    pub fn push(&mut self, entry: HistoryEntry) {
        self.undo_stack.push(entry);
        self.redo_stack.clear();
        if self.undo_stack.len() > self.max_size {
            self.undo_stack.remove(0);
        }
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
