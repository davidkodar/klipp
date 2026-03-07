use klipp_core::{ClipId, RationalTime};
use klipp_timeline::Clip;

#[derive(Debug, Clone)]
pub enum EditCommand {
    AppendClip {
        clip: Clip,
        track_index: usize,
    },
    RemoveClip {
        clip: Clip,
        track_index: usize,
        position: usize,
    },
    RippleRemove {
        removed_clip: Clip,
        track_index: usize,
        position: usize,
        shifted_clips: Vec<(ClipId, RationalTime, RationalTime)>,
    },
    MoveClip {
        clip_id: ClipId,
        old_timeline_in: RationalTime,
        old_timeline_out: RationalTime,
        new_timeline_in: RationalTime,
        new_timeline_out: RationalTime,
    },
    TrimClip {
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
    BladeClip {
        track_index: usize,
        original_clip: Clip,
        clip_a: Clip,
        clip_b: Clip,
        position: usize,
    },
    CutClip {
        clip: Clip,
        media_item: klipp_media::MediaItem,
        track_index: usize,
        position: usize,
        shifted_clips: Vec<(ClipId, RationalTime, RationalTime)>,
    },
    PasteClip {
        clip: Clip,
        track_index: usize,
    },
}

impl EditCommand {
    pub fn label(&self) -> &'static str {
        match self {
            EditCommand::AppendClip { .. } => "Append Clip",
            EditCommand::RemoveClip { .. } | EditCommand::RippleRemove { .. } => "Remove Clip",
            EditCommand::MoveClip { .. } => "Move Clip",
            EditCommand::TrimClip { .. } => "Trim Clip",
            EditCommand::BladeClip { .. } => "Blade Clip",
            EditCommand::CutClip { .. } => "Cut Clip",
            EditCommand::PasteClip { .. } => "Paste Clip",
        }
    }
}

#[derive(Default)]
pub struct CommandHistory {
    undo_stack: Vec<EditCommand>,
    redo_stack: Vec<EditCommand>,
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

    pub fn push(&mut self, cmd: EditCommand) {
        self.undo_stack.push(cmd);
        self.redo_stack.clear();
        if self.undo_stack.len() > self.max_size {
            self.undo_stack.remove(0);
        }
    }

    pub fn undo(&mut self) -> Option<EditCommand> {
        let cmd = self.undo_stack.pop()?;
        self.redo_stack.push(cmd.clone());
        Some(cmd)
    }

    pub fn redo(&mut self) -> Option<EditCommand> {
        let cmd = self.redo_stack.pop()?;
        self.undo_stack.push(cmd.clone());
        Some(cmd)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    pub fn undo_label(&self) -> Option<&'static str> {
        self.undo_stack.last().map(|c| c.label())
    }

    pub fn redo_label(&self) -> Option<&'static str> {
        self.redo_stack.last().map(|c| c.label())
    }
}
