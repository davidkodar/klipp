use std::collections::{HashMap, VecDeque};
use klipp_core::{AudioEffectInstance, BinId, ClipId, Keyframe, MediaId, RationalTime};
use klipp_timeline::Clip;

/// Snapshot of a clip's audio chain (effects + per-effect kfs). Used by
/// `EditCommand::EditAudioChain` so add/remove/edit/reorder operations
/// on the audio chain can be undone atomically.
#[derive(Debug, Clone)]
pub struct AudioChainSnapshot {
    pub audio_effects: Vec<AudioEffectInstance>,
    pub audio_effect_keyframes: HashMap<String, Vec<Keyframe>>,
}

impl AudioChainSnapshot {
    pub fn from_clip(clip: &Clip) -> Self {
        Self {
            audio_effects: clip.audio_effects.clone(),
            audio_effect_keyframes: clip.audio_effect_keyframes.clone(),
        }
    }

    pub fn apply_to_clip(&self, clip: &mut Clip) {
        clip.audio_effects = self.audio_effects.clone();
        clip.audio_effect_keyframes = self.audio_effect_keyframes.clone();
    }
}

/// Snapshot of all keyframe data for a clip (used for undo/redo).
#[derive(Debug, Clone)]
pub struct KeyframeSnapshot {
    pub volume_keyframes: Vec<Keyframe>,
    pub pan_keyframes: Vec<Keyframe>,
    pub gain_keyframes: Vec<Keyframe>,
    pub opacity_keyframes: Vec<Keyframe>,
    pub transform_pos_x_keyframes: Vec<Keyframe>,
    pub transform_pos_y_keyframes: Vec<Keyframe>,
    pub transform_scale_keyframes: Vec<Keyframe>,
    pub transform_rotation_keyframes: Vec<Keyframe>,
    pub effect_keyframes: HashMap<String, Vec<Keyframe>>,
    pub audio_effect_keyframes: HashMap<String, Vec<Keyframe>>,
    pub speed_keyframes: Vec<Keyframe>,
}

impl KeyframeSnapshot {
    /// Create an empty keyframe snapshot (no keyframes).
    pub fn empty() -> Self {
        Self {
            volume_keyframes: Vec::new(),
            pan_keyframes: Vec::new(),
            gain_keyframes: Vec::new(),
            opacity_keyframes: Vec::new(),
            transform_pos_x_keyframes: Vec::new(),
            transform_pos_y_keyframes: Vec::new(),
            transform_scale_keyframes: Vec::new(),
            transform_rotation_keyframes: Vec::new(),
            effect_keyframes: HashMap::new(),
            audio_effect_keyframes: HashMap::new(),
            speed_keyframes: Vec::new(),
        }
    }

    /// Capture a snapshot of all keyframe data from a clip.
    pub fn from_clip(clip: &Clip) -> Self {
        Self {
            volume_keyframes: clip.volume_keyframes.clone(),
            pan_keyframes: clip.pan_keyframes.clone(),
            gain_keyframes: clip.gain_keyframes.clone(),
            opacity_keyframes: clip.opacity_keyframes.clone(),
            transform_pos_x_keyframes: clip.transform_pos_x_keyframes.clone(),
            transform_pos_y_keyframes: clip.transform_pos_y_keyframes.clone(),
            transform_scale_keyframes: clip.transform_scale_keyframes.clone(),
            transform_rotation_keyframes: clip.transform_rotation_keyframes.clone(),
            effect_keyframes: clip.effect_keyframes.clone(),
            audio_effect_keyframes: clip.audio_effect_keyframes.clone(),
            speed_keyframes: clip.speed_keyframes.clone(),
        }
    }

    /// Apply this snapshot's keyframe data to a clip.
    pub fn apply_to_clip(&self, clip: &mut Clip) {
        clip.volume_keyframes = self.volume_keyframes.clone();
        clip.pan_keyframes = self.pan_keyframes.clone();
        clip.gain_keyframes = self.gain_keyframes.clone();
        clip.opacity_keyframes = self.opacity_keyframes.clone();
        clip.transform_pos_x_keyframes = self.transform_pos_x_keyframes.clone();
        clip.transform_pos_y_keyframes = self.transform_pos_y_keyframes.clone();
        clip.transform_scale_keyframes = self.transform_scale_keyframes.clone();
        clip.transform_rotation_keyframes = self.transform_rotation_keyframes.clone();
        clip.effect_keyframes = self.effect_keyframes.clone();
        clip.audio_effect_keyframes = self.audio_effect_keyframes.clone();
        clip.speed_keyframes = self.speed_keyframes.clone();
    }
}

/// Records how a linked partner clip moved alongside a primary move so
/// undo/redo can put both clips back atomically. The partner stays on
/// its own track (linked A/V pairs cross track kinds), only its
/// timeline bounds shift.
#[derive(Debug, Clone)]
pub struct LinkedMovePartner {
    pub partner_id: ClipId,
    pub old_timeline_in: RationalTime,
    pub old_timeline_out: RationalTime,
    pub new_timeline_in: RationalTime,
    pub new_timeline_out: RationalTime,
}

#[derive(Debug, Clone)]
pub enum EditCommand {
    // === LEGACY (Free tracks only — existing behaviour unchanged) ===
    LegacyAppendClip {
        clip: Clip,
        track_index: usize,
    },
    /// Title clip added (with optional track creation for undo)
    AddTitleClip {
        clip: Clip,
        track_index: usize,
        /// If true, undo also removes the track that was auto-created
        created_track: bool,
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
        /// Linked A/V partner that moved by the same delta. `None` when the
        /// clip had no linked partner or the partner was already carried
        /// by group propagation (avoids double-moving in undo/redo).
        linked_partner: Option<LinkedMovePartner>,
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
    LegacyRippleTrimClip {
        clip_id: ClipId,
        old_source_in: RationalTime,
        old_source_out: RationalTime,
        old_timeline_in: RationalTime,
        old_timeline_out: RationalTime,
        new_source_in: RationalTime,
        new_source_out: RationalTime,
        new_timeline_in: RationalTime,
        new_timeline_out: RationalTime,
        /// Downstream clips' original positions before the ripple shift
        shifted_clips: Vec<(ClipId, RationalTime, RationalTime)>,
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
    /// Atomic multi-way split at N scene markers. Replaces `original_clip`
    /// at `position` with `new_clips` (ordered left→right). Single undo step.
    LegacySplitAtSceneMarkers {
        track_index: usize,
        original_clip: Clip,
        position: usize,
        new_clips: Vec<Clip>,
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
        /// Linked A/V partner that moved by the same delta on its own
        /// track (the partner doesn't cross tracks — only timeline bounds
        /// shift). `None` when no linked partner exists.
        linked_partner: Option<LinkedMovePartner>,
    },

    // === MAGNETIC (Primary/Connected tracks) ===
    Magnetic(klipp_timeline::magnetic_commands::MagneticEditCommand),

    /// Source-viewer three-point edit that inserted a video clip on the
    /// primary track and a linked audio partner on a connected audio
    /// track, as a single atomic action. The video side rides on the
    /// magnetic command; the audio track is captured as full
    /// before/after clip-list snapshots because
    /// `overwrite_insert_clip_on_track` does not emit ripple records and
    /// snapshotting is simpler than synthesising per-clip records.
    SourceLinkedInsert {
        video_command: klipp_timeline::magnetic_commands::MagneticEditCommand,
        audio_track_index: usize,
        audio_clips_before: Vec<Clip>,
        audio_clips_after: Vec<Clip>,
    },

    LegacySlipClip {
        clip_id: ClipId,
        old_source_in: RationalTime,
        old_source_out: RationalTime,
        new_source_in: RationalTime,
        new_source_out: RationalTime,
    },
    LegacySlideClip {
        clip_id: ClipId,
        old_timeline_in: RationalTime,
        old_timeline_out: RationalTime,
        new_timeline_in: RationalTime,
        new_timeline_out: RationalTime,
        left_clip_id: Option<ClipId>,
        old_left_out: Option<RationalTime>,
        new_left_out: Option<RationalTime>,
        right_clip_id: Option<ClipId>,
        old_right_in: Option<RationalTime>,
        new_right_in: Option<RationalTime>,
    },

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
        from_bin_id: Option<BinId>,
        to_bin_id: Option<BinId>,
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
    /// Batch rename of media-item display names. `changes` is
    /// `(media_id, old_filename, new_filename)` so undo and redo are
    /// simple map-and-assign loops. Used for single-item rename too —
    /// the Vec just has one entry.
    BatchRenameMediaItems {
        changes: Vec<(MediaId, String, String)>,
    },
    /// Subclip created from a zone on `parent_id`. Full `subclip`
    /// MediaItem is carried so redo can re-insert it verbatim (same id,
    /// same zone, same name). Undo drops it from the pool.
    CreateSubclip {
        subclip: klipp_media::MediaItem,
        parent_id: MediaId,
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

    // === KEYFRAMES ===
    EditKeyframes {
        clip_id: ClipId,
        /// Optional linked partner clip (audio keyframes are mirrored to linked clip)
        linked_clip_id: Option<ClipId>,
        description: &'static str,
        old_state: KeyframeSnapshot,
        new_state: KeyframeSnapshot,
        /// Old state for linked partner clip (if any)
        old_linked_state: Option<KeyframeSnapshot>,
        /// New state for linked partner clip (if any)
        new_linked_state: Option<KeyframeSnapshot>,
    },

    // === BUILTIN EFFECTS ===
    EditBuiltinEffect {
        clip_id: ClipId,
        effect_index: usize,
        old_effect: klipp_effects::BuiltinEffect,
        new_effect: klipp_effects::BuiltinEffect,
    },
    /// Per-effect enable/disable for builtin effects. Both undo and redo
    /// simply flip the boolean at `index`, so only the index is stored.
    ToggleBuiltinEffectEnabled {
        clip_id: ClipId,
        index: usize,
    },
    /// OFX effect enable/disable. `AppliedEffect.enabled` is an absolute
    /// bool so we capture both old and new values for clean restoration.
    SetOFXEffectEnabled {
        clip_id: ClipId,
        effect_index: usize,
        old_enabled: bool,
        new_enabled: bool,
    },

    // === CLIP SPEED ===
    SetClipSpeed {
        clip_id: ClipId,
        old_speed: f64,
        new_speed: f64,
        old_timeline_out: RationalTime,
        new_timeline_out: RationalTime,
    },

    // === TITLE EDITING ===
    EditTitle {
        clip_id: ClipId,
        description: &'static str,
        old_data: klipp_timeline::TitleData,
        new_data: klipp_timeline::TitleData,
    },

    // === GENERATOR EDITING ===
    EditGenerator {
        clip_id: ClipId,
        old_data: klipp_core::GeneratorData,
        new_data: klipp_core::GeneratorData,
    },

    // === AUDIO CHAIN ===
    /// Atomic snapshot-based undo for any edit to a clip's audio
    /// effect chain — Add / Remove / Update param / Toggle / Wet-Dry /
    /// Reorder all push one of these. The whole `audio_effects` vec
    /// plus `audio_effect_keyframes` map are snapshotted on both sides
    /// so the apply path is a simple "restore the snapshot" without
    /// per-command branching.
    EditAudioChain {
        clip_id: ClipId,
        description: &'static str,
        old_state: AudioChainSnapshot,
        new_state: AudioChainSnapshot,
    },
}

impl EditCommand {
    pub fn label(&self) -> &'static str {
        match self {
            EditCommand::AddTitleClip { .. } => "Add Title",
            EditCommand::LegacyAppendClip { .. } => "Append Clip",
            EditCommand::LegacyRemoveClip { .. } | EditCommand::LegacyRippleRemove { .. } => "Remove Clip",
            EditCommand::LegacyMoveClip { .. } | EditCommand::LegacyMoveClipToTrack { .. } => "Move Clip",
            EditCommand::LegacyTrimClip { .. } | EditCommand::LegacyRippleTrimClip { .. } => "Trim Clip",
            EditCommand::LegacyRollTrimClip { .. } => "Roll Trim",
            EditCommand::LegacySlipClip { .. } => "Slip Clip",
            EditCommand::LegacySlideClip { .. } => "Slide Clip",
            EditCommand::LegacyBladeClip { .. } => "Blade Clip",
            EditCommand::LegacySplitAtSceneMarkers { .. } => "Split at Scene Markers",
            EditCommand::LegacyCutClip { .. } => "Cut Clip",
            EditCommand::LegacyPasteClip { .. } => "Paste Clip",
            EditCommand::Magnetic(cmd) => cmd.label(),
            EditCommand::SourceLinkedInsert { video_command, .. } => video_command.label(),
            EditCommand::AddMediaToPool { .. } => "Import Media",
            EditCommand::RemoveMediaFromPool { .. } => "Remove Media",
            EditCommand::MoveMediaToBin { .. } => "Move to Bin",
            EditCommand::SetMediaTags { .. } => "Set Tags",
            EditCommand::SetMediaRating { .. } => "Set Rating",
            EditCommand::SetMediaColorLabel { .. } => "Set Color Label",
            EditCommand::RenameBin { .. } => "Rename Bin",
            EditCommand::BatchRenameMediaItems { changes } => {
                if changes.len() == 1 { "Rename Media Item" } else { "Batch Rename Media Items" }
            }
            EditCommand::CreateSubclip { .. } => "Create Subclip",
            EditCommand::SetRange { .. } => "Set Range",
            EditCommand::SetNodeGraph { .. } => "Color Grade",
            EditCommand::EditKeyframes { description, .. } => description,
            EditCommand::EditClipAudio { .. } => "Edit Audio",
            EditCommand::EditBuiltinEffect { .. } => "Edit Effect",
            EditCommand::ToggleBuiltinEffectEnabled { .. } => "Toggle Effect",
            EditCommand::SetOFXEffectEnabled { .. } => "Toggle Effect",
            EditCommand::SetClipSpeed { .. } => "Set Clip Speed",
            EditCommand::EditTitle { description, .. } => description,
            EditCommand::EditGenerator { .. } => "Edit Generator",
            EditCommand::EditAudioChain { description, .. } => description,
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
    undo_stack: VecDeque<HistoryEntry>,
    redo_stack: VecDeque<HistoryEntry>,
    max_size: usize,

    // Coalescing state — for drag gesture support
    /// Identifies which param is being dragged (e.g. "clip_id:builtin:index")
    pending_coalesce_key: Option<String>,
    /// "Before" state captured at drag start
    pending_old_entry: Option<HistoryEntry>,

    /// Number of ops pushed since the last project save. Incremented by push(),
    /// reset to 0 by reset_ops_since_save() when the project is saved. Used by
    /// the op-count autosave trigger.
    pub ops_since_save: u64,
}

impl CommandHistory {
    pub fn new() -> Self {
        Self {
            undo_stack: VecDeque::new(),
            redo_stack: VecDeque::new(),
            max_size: 1000,
            pending_coalesce_key: None,
            pending_old_entry: None,
            ops_since_save: 0,
        }
    }

    /// Reset the ops-since-save counter. Call from the project-save path.
    pub fn reset_ops_since_save(&mut self) {
        self.ops_since_save = 0;
    }

    /// Set the maximum undo stack size. 0 = unlimited.
    pub fn set_max_size(&mut self, max: usize) {
        self.max_size = max;
    }

    pub fn push(&mut self, entry: HistoryEntry) {
        self.undo_stack.push_back(entry);
        self.redo_stack.clear();
        if self.max_size > 0 && self.undo_stack.len() > self.max_size {
            self.undo_stack.pop_front();
        }
        self.ops_since_save = self.ops_since_save.saturating_add(1);
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
            } else {
                // Defensive: the coalesce key was set but the "before" snapshot is
                // missing (e.g. a caller invoked end_drag without begin_drag first,
                // or an intervening code path stole the pending entry). Push the
                // final state on its own so the user's edit is not silently lost;
                // undo will revert this single entry even if it can't reach the
                // true pre-drag state.
                self.push(final_state);
            }
        }
    }

    /// Returns true if a drag is in progress for the given key.
    /// Command handlers check this to suppress individual undo pushes during drag.
    pub fn is_dragging(&self, key: &str) -> bool {
        self.pending_coalesce_key.as_deref() == Some(key)
    }

    /// Returns true if ANY drag-coalesce envelope is currently open (regardless
    /// of which key). The autosave timer reads this to skip while the user is
    /// mid-gesture — without it, a per-frame state mutation (e.g. dragging a
    /// keyframe diamond at 60 Hz) trips the time-or-op autosave trigger and
    /// floods the toast queue. Autosave catches up on the next tick after the
    /// gesture's `end_drag`.
    pub fn is_any_drag_in_progress(&self) -> bool {
        self.pending_coalesce_key.is_some()
    }

    /// Returns a reference to the pending old entry (for reading old state at end_drag).
    pub fn pending_old_entry_ref(&self) -> Option<&HistoryEntry> {
        self.pending_old_entry.as_ref()
    }

    pub fn undo(&mut self) -> Option<HistoryEntry> {
        let entry = self.undo_stack.pop_back()?;
        self.redo_stack.push_back(entry.clone());
        Some(entry)
    }

    pub fn redo(&mut self) -> Option<HistoryEntry> {
        let entry = self.redo_stack.pop_back()?;
        self.undo_stack.push_back(entry.clone());
        Some(entry)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    pub fn undo_label(&self) -> Option<&'static str> {
        self.undo_stack.back().map(|e| e.label())
    }

    pub fn redo_label(&self) -> Option<&'static str> {
        self.redo_stack.back().map(|e| e.label())
    }

    /// Get all undo stack entries (oldest first) for history panel display.
    pub fn undo_entries(&self) -> &VecDeque<HistoryEntry> {
        &self.undo_stack
    }

    /// Get all redo stack entries (oldest first) for history panel display.
    pub fn redo_entries(&self) -> &VecDeque<HistoryEntry> {
        &self.redo_stack
    }

    /// Undo N steps (for history panel click-to-jump).
    pub fn undo_count(&self) -> usize {
        self.undo_stack.len()
    }

    pub fn redo_count(&self) -> usize {
        self.redo_stack.len()
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
        (
            EditCommand::EditKeyframes { clip_id, linked_clip_id, description, old_state, old_linked_state, .. },
            EditCommand::EditKeyframes { new_state, new_linked_state, .. },
        ) => EditCommand::EditKeyframes {
            clip_id: *clip_id,
            linked_clip_id: *linked_clip_id,
            description,
            old_state: old_state.clone(),
            new_state: new_state.clone(),
            old_linked_state: old_linked_state.clone(),
            new_linked_state: new_linked_state.clone(),
        },
        (
            EditCommand::EditAudioChain { clip_id, description, old_state, .. },
            EditCommand::EditAudioChain { new_state, .. },
        ) => EditCommand::EditAudioChain {
            clip_id: *clip_id,
            description,
            old_state: old_state.clone(),
            new_state: new_state.clone(),
        },
        // Unmatched pair: the pre-drag and post-drag commands are different
        // variants, or this is a coalesce-eligible variant someone wired up
        // without extending combine_old_new. Loudly flag in debug, fall back
        // to the pre-drag command in release so undo at least rolls back to
        // the last known-good state (the pre-drag snapshot).
        (old_cmd, new_cmd) => {
            debug_assert!(
                false,
                "combine_old_new: unhandled variant pair {:?} / {:?} — extend this match when adding a coalesce-eligible variant",
                std::mem::discriminant(old_cmd),
                std::mem::discriminant(new_cmd),
            );
            tracing::warn!(
                "history: drag coalesce missing handler for {:?} / {:?} — falling back to pre-drag state",
                std::mem::discriminant(old_cmd),
                std::mem::discriminant(new_cmd),
            );
            old_entry.command.clone()
        }
    };
    HistoryEntry {
        sequence_idx: new_entry.sequence_idx,
        command,
    }
}

#[cfg(test)]
mod bin_undo_tests {
    use super::*;
    use klipp_core::{BinId, MediaId, RationalTime};
    use klipp_media::{MediaItem, MediaItemKind, MediaPool};

    /// Helper: create a minimal MediaItem for testing bin operations.
    fn test_media_item(id: MediaId) -> MediaItem {
        MediaItem {
            id,
            path: std::path::PathBuf::from("/test/video.mp4"),
            kind: MediaItemKind::File,
            filename: "video.mp4".to_string(),
            duration: RationalTime::from_frames(240, 24),
            fps: klipp_core::Fps::new(24, 1),
            resolution: klipp_core::Resolution { width: 1920, height: 1080 },
            video_codec: "h264".to_string(),
            audio_codec: "aac".to_string(),
            has_audio: true,
            file_size_bytes: 1024,
            sample_rate: None,
            audio_channels: None,
            offline: false,
            is_still: false,
            proxy_path: None,
            proxy_ready: false,
            default_source_in: None,
            default_source_out: None,
            rating: 0,
            tags: Vec::new(),
            user_notes: String::new(),
            bin: None,
            bin_id: None,
            parent_media_id: None,
            added_at: 0,
            fingerprint: None,
            file_modified_at: None,
            media_created_at: None,
            color_label: Default::default(),
            colour_space: Default::default(),
            log_format: Default::default(),
            source_rotation: 0,
            camera_make: None,
            camera_model: None,
            encoder: None,
            bit_depth: None,
            video_bitrate_kbps: None,
            audio_bitrate_kbps: None,
            color_primaries: None,
            color_transfer: None,
            pixel_format: None,
            detected_log_format: Default::default(),
            skim_file: None,
            needs_reprobe: false,
            waveform_data: None,
            audio_cache: None,
        }
    }

    /// Simulate MoveItemToBin forward operation (mirrors app.rs handler).
    fn move_item_to_bin(pool: &mut MediaPool, media_id: MediaId, new_bin: Option<String>) -> EditCommand {
        let new_bin_id = new_bin.as_ref().map(|name| pool.find_or_create_bin(name));
        let item = pool.get_mut(&media_id).unwrap();
        let from_bin = item.bin.clone();
        let from_bin_id = item.bin_id;
        item.bin = new_bin.clone();
        item.bin_id = new_bin_id;
        EditCommand::MoveMediaToBin {
            media_id,
            from_bin,
            to_bin: new_bin,
            from_bin_id,
            to_bin_id: new_bin_id,
        }
    }

    /// Simulate MoveItemToBin undo (mirrors app.rs undo handler).
    fn undo_move_item_to_bin(pool: &mut MediaPool, cmd: &EditCommand) {
        if let EditCommand::MoveMediaToBin { media_id, from_bin, from_bin_id, .. } = cmd {
            if let Some(item) = pool.get_mut(media_id) {
                item.bin = from_bin.clone();
                item.bin_id = *from_bin_id;
            }
        }
    }

    /// Simulate MoveItemToBin redo (mirrors app.rs redo handler).
    fn redo_move_item_to_bin(pool: &mut MediaPool, cmd: &EditCommand) {
        if let EditCommand::MoveMediaToBin { media_id, to_bin, to_bin_id, .. } = cmd {
            if let Some(item) = pool.get_mut(media_id) {
                item.bin = to_bin.clone();
                item.bin_id = *to_bin_id;
            }
        }
    }

    /// Simulate RenameBin forward (mirrors app.rs handler).
    fn rename_bin(pool: &mut MediaPool, old_name: &str, new_name: &str) -> EditCommand {
        // Rename the Bin record
        for bin in &mut pool.bins {
            if bin.name == old_name {
                bin.name = new_name.to_string();
                break;
            }
        }
        // Update legacy item.bin
        let ids: Vec<MediaId> = pool.all()
            .filter(|m| m.bin.as_deref() == Some(old_name))
            .map(|m| m.id)
            .collect();
        for id in ids {
            if let Some(item) = pool.get_mut(&id) {
                item.bin = Some(new_name.to_string());
            }
        }
        EditCommand::RenameBin {
            old_name: old_name.to_string(),
            new_name: new_name.to_string(),
        }
    }

    /// Simulate RenameBin undo (mirrors app.rs undo handler).
    fn undo_rename_bin(pool: &mut MediaPool, cmd: &EditCommand) {
        if let EditCommand::RenameBin { old_name, new_name } = cmd {
            for bin in &mut pool.bins {
                if bin.name == *new_name {
                    bin.name = old_name.clone();
                    break;
                }
            }
            let ids: Vec<MediaId> = pool.all()
                .filter(|m| m.bin.as_deref() == Some(new_name.as_str()))
                .map(|m| m.id)
                .collect();
            for id in ids {
                if let Some(item) = pool.get_mut(&id) {
                    item.bin = Some(old_name.clone());
                }
            }
        }
    }

    /// Simulate RenameBin redo (mirrors app.rs redo handler).
    fn redo_rename_bin(pool: &mut MediaPool, cmd: &EditCommand) {
        if let EditCommand::RenameBin { old_name, new_name } = cmd {
            for bin in &mut pool.bins {
                if bin.name == *old_name {
                    bin.name = new_name.clone();
                    break;
                }
            }
            let ids: Vec<MediaId> = pool.all()
                .filter(|m| m.bin.as_deref() == Some(old_name.as_str()))
                .map(|m| m.id)
                .collect();
            for id in ids {
                if let Some(item) = pool.get_mut(&id) {
                    item.bin = Some(new_name.clone());
                }
            }
        }
    }

    /// Helper: assert that bin and bin_id are consistent for an item.
    fn assert_bin_consistent(pool: &MediaPool, media_id: MediaId, expected_bin: Option<&str>, expected_bin_id: Option<BinId>) {
        let item = pool.get(&media_id).expect("item not found");
        assert_eq!(item.bin.as_deref(), expected_bin, "bin string mismatch for {media_id}");
        assert_eq!(item.bin_id, expected_bin_id, "bin_id mismatch for {media_id}");
    }

    // ── Test 1: MoveItemToBin full cycle (move → undo → redo → undo) ──

    #[test]
    fn move_item_to_bin_full_cycle() {
        let mut pool = MediaPool::new();
        let mid = MediaId::new();
        let bin_a_id = pool.find_or_create_bin("Bin A");

        let mut item = test_media_item(mid);
        item.bin = Some("Bin A".to_string());
        item.bin_id = Some(bin_a_id);
        pool.add(item);

        // Forward: move to Bin B
        let cmd = move_item_to_bin(&mut pool, mid, Some("Bin B".to_string()));
        let bin_b_id = pool.get(&mid).unwrap().bin_id.unwrap();
        assert_bin_consistent(&pool, mid, Some("Bin B"), Some(bin_b_id));

        // Undo: back to Bin A
        undo_move_item_to_bin(&mut pool, &cmd);
        assert_bin_consistent(&pool, mid, Some("Bin A"), Some(bin_a_id));

        // Redo: back to Bin B
        redo_move_item_to_bin(&mut pool, &cmd);
        assert_bin_consistent(&pool, mid, Some("Bin B"), Some(bin_b_id));

        // Undo again: back to Bin A
        undo_move_item_to_bin(&mut pool, &cmd);
        assert_bin_consistent(&pool, mid, Some("Bin A"), Some(bin_a_id));
    }

    // ── Test 2: MoveItemToBin with multiple items ──

    #[test]
    fn move_multiple_items_to_bin_undo() {
        let mut pool = MediaPool::new();
        let bin_a_id = pool.find_or_create_bin("Bin A");
        let mid1 = MediaId::new();
        let mid2 = MediaId::new();

        let mut item1 = test_media_item(mid1);
        item1.bin = Some("Bin A".to_string());
        item1.bin_id = Some(bin_a_id);
        pool.add(item1);

        let mut item2 = test_media_item(mid2);
        item2.path = std::path::PathBuf::from("/test/video2.mp4");
        item2.bin = Some("Bin A".to_string());
        item2.bin_id = Some(bin_a_id);
        pool.add(item2);

        // Move both items
        let cmd1 = move_item_to_bin(&mut pool, mid1, Some("Bin B".to_string()));
        let bin_b_id = pool.get(&mid1).unwrap().bin_id.unwrap();
        let cmd2 = move_item_to_bin(&mut pool, mid2, Some("Bin B".to_string()));

        assert_bin_consistent(&pool, mid1, Some("Bin B"), Some(bin_b_id));
        assert_bin_consistent(&pool, mid2, Some("Bin B"), Some(bin_b_id));

        // Undo both (reverse order)
        undo_move_item_to_bin(&mut pool, &cmd2);
        assert_bin_consistent(&pool, mid2, Some("Bin A"), Some(bin_a_id));
        undo_move_item_to_bin(&mut pool, &cmd1);
        assert_bin_consistent(&pool, mid1, Some("Bin A"), Some(bin_a_id));
    }

    // ── Test 3: MoveItemToBin from root (None) to bin and back ──

    #[test]
    fn move_item_from_root_to_bin_undo() {
        let mut pool = MediaPool::new();
        let mid = MediaId::new();
        pool.add(test_media_item(mid));

        // Initially at root (no bin)
        assert_bin_consistent(&pool, mid, None, None);

        // Move to a bin
        let cmd = move_item_to_bin(&mut pool, mid, Some("Dailies".to_string()));
        let dailies_id = pool.get(&mid).unwrap().bin_id.unwrap();
        assert_bin_consistent(&pool, mid, Some("Dailies"), Some(dailies_id));

        // Undo: back to root
        undo_move_item_to_bin(&mut pool, &cmd);
        assert_bin_consistent(&pool, mid, None, None);
    }

    // ── Test 4: RenameBin forward + undo ──

    #[test]
    fn rename_bin_forward_and_undo() {
        let mut pool = MediaPool::new();
        let bin_id = pool.find_or_create_bin("Old Name");
        let mid = MediaId::new();

        let mut item = test_media_item(mid);
        item.bin = Some("Old Name".to_string());
        item.bin_id = Some(bin_id);
        pool.add(item);

        // Forward: rename
        let cmd = rename_bin(&mut pool, "Old Name", "New Name");
        assert_eq!(pool.get(&mid).unwrap().bin.as_deref(), Some("New Name"));
        assert_eq!(pool.find_bin(&bin_id).unwrap().name, "New Name");

        // Undo: back to Old Name
        undo_rename_bin(&mut pool, &cmd);
        assert_eq!(pool.get(&mid).unwrap().bin.as_deref(), Some("Old Name"));
        assert_eq!(pool.find_bin(&bin_id).unwrap().name, "Old Name");
    }

    // ── Test 5: RenameBin does NOT change bin_id ──

    #[test]
    fn rename_bin_preserves_bin_id() {
        let mut pool = MediaPool::new();
        let bin_id = pool.find_or_create_bin("Original");
        let mid1 = MediaId::new();
        let mid2 = MediaId::new();

        let mut item1 = test_media_item(mid1);
        item1.bin = Some("Original".to_string());
        item1.bin_id = Some(bin_id);
        pool.add(item1);

        let mut item2 = test_media_item(mid2);
        item2.path = std::path::PathBuf::from("/test/video2.mp4");
        item2.bin = Some("Original".to_string());
        item2.bin_id = Some(bin_id);
        pool.add(item2);

        // Rename
        let cmd = rename_bin(&mut pool, "Original", "Renamed");

        // bin_id must be unchanged for both items
        assert_eq!(pool.get(&mid1).unwrap().bin_id, Some(bin_id));
        assert_eq!(pool.get(&mid2).unwrap().bin_id, Some(bin_id));

        // Undo
        undo_rename_bin(&mut pool, &cmd);
        assert_eq!(pool.get(&mid1).unwrap().bin_id, Some(bin_id));
        assert_eq!(pool.get(&mid2).unwrap().bin_id, Some(bin_id));

        // Redo
        redo_rename_bin(&mut pool, &cmd);
        assert_eq!(pool.get(&mid1).unwrap().bin_id, Some(bin_id));
        assert_eq!(pool.get(&mid2).unwrap().bin_id, Some(bin_id));
    }

    // ── Test 6: RenameBin redo restores new name ──

    #[test]
    fn rename_bin_redo() {
        let mut pool = MediaPool::new();
        let bin_id = pool.find_or_create_bin("Alpha");
        let mid = MediaId::new();

        let mut item = test_media_item(mid);
        item.bin = Some("Alpha".to_string());
        item.bin_id = Some(bin_id);
        pool.add(item);

        let cmd = rename_bin(&mut pool, "Alpha", "Beta");
        undo_rename_bin(&mut pool, &cmd);
        assert_eq!(pool.get(&mid).unwrap().bin.as_deref(), Some("Alpha"));

        redo_rename_bin(&mut pool, &cmd);
        assert_eq!(pool.get(&mid).unwrap().bin.as_deref(), Some("Beta"));
        assert_eq!(pool.find_bin(&bin_id).unwrap().name, "Beta");
    }

    // ── Test 7: Move then rename then undo both ──

    #[test]
    fn move_then_rename_undo_sequence() {
        let mut pool = MediaPool::new();
        let bin_a_id = pool.find_or_create_bin("Bin A");
        let mid = MediaId::new();

        let mut item = test_media_item(mid);
        item.bin = Some("Bin A".to_string());
        item.bin_id = Some(bin_a_id);
        pool.add(item);

        // Move to new bin
        let move_cmd = move_item_to_bin(&mut pool, mid, Some("Bin B".to_string()));
        let bin_b_id = pool.get(&mid).unwrap().bin_id.unwrap();

        // Rename that bin
        let rename_cmd = rename_bin(&mut pool, "Bin B", "Bin B Renamed");
        assert_bin_consistent(&pool, mid, Some("Bin B Renamed"), Some(bin_b_id));

        // Undo rename
        undo_rename_bin(&mut pool, &rename_cmd);
        assert_bin_consistent(&pool, mid, Some("Bin B"), Some(bin_b_id));

        // Undo move
        undo_move_item_to_bin(&mut pool, &move_cmd);
        assert_bin_consistent(&pool, mid, Some("Bin A"), Some(bin_a_id));
    }
}

// ═══════════════════════════════════════════════════════════════════
// Drag-coalesce defensive-path tests — regression-pin CODE_AUDIT U-3
// and U-4. The two production fixes this guards are:
//
//   * combine_old_new — wildcard arm logs + falls back to old command
//     (was: silently returned new.command.clone(), throwing away
//     old state, so undo was a no-op for any future variant).
//   * end_drag — when pending_coalesce_key is Some but
//     pending_old_entry is None (caller invoked end_drag without
//     begin_drag, or an intervening path stole the snapshot), the
//     final state is pushed on its own (was: silently dropped).
//
// Both look intentional from the inline comments, but neither was
// covered by tests. Without these regression tests a future cleanup
// could re-introduce the original bugs and only surface them the
// next time a user lost an edit.
// ═══════════════════════════════════════════════════════════════════
#[cfg(test)]
mod drag_coalesce_tests {
    use super::*;
    use klipp_core::ClipId;

    fn audio_entry(gain: f32) -> HistoryEntry {
        // Single-variant entry with `gain` propagated to old AND new
        // sides — callers fold separate "old" and "new" snapshots into
        // begin_drag / end_drag, so each individual entry just carries
        // the same value on both sides.
        HistoryEntry {
            sequence_idx: 0,
            command: EditCommand::EditClipAudio {
                clip_id: ClipId::new(),
                old_gain: gain, old_volume: gain, old_pan: 0.0,
                old_fade_in: 0.0, old_fade_out: 0.0,
                old_channel_config: klipp_core::ChannelConfig::default(),
                new_gain: gain, new_volume: gain, new_pan: 0.0,
                new_fade_in: 0.0, new_fade_out: 0.0,
                new_channel_config: klipp_core::ChannelConfig::default(),
            },
        }
    }

    fn slip_entry() -> HistoryEntry {
        // A non-coalesce-eligible variant — used to construct a
        // mismatched (old, new) pair for combine_old_new's wildcard
        // arm. LegacySlipClip has no matching arm in combine_old_new,
        // so pairing it with EditClipAudio reaches the fallback.
        HistoryEntry {
            sequence_idx: 0,
            command: EditCommand::LegacySlipClip {
                clip_id: ClipId::new(),
                old_source_in: klipp_core::RationalTime::ZERO,
                old_source_out: klipp_core::RationalTime::ZERO,
                new_source_in: klipp_core::RationalTime::ZERO,
                new_source_out: klipp_core::RationalTime::ZERO,
            },
        }
    }

    // ── U-3: combine_old_new ────────────────────────────────────────

    #[test]
    fn combine_old_new_clip_audio_takes_old_pre_and_new_post() {
        // Happy path: identical variants on both sides. The combined
        // entry must carry old_* from `old_entry` and new_* from
        // `new_entry` so undo rolls back to the pre-drag state and
        // redo restores the post-drag state.
        let old = HistoryEntry {
            sequence_idx: 7,
            command: EditCommand::EditClipAudio {
                clip_id: ClipId::new(),
                old_gain: 1.0, old_volume: 1.0, old_pan: 0.0,
                old_fade_in: 0.5, old_fade_out: 0.25,
                old_channel_config: klipp_core::ChannelConfig::default(),
                new_gain: 1.0, new_volume: 1.0, new_pan: 0.0,
                new_fade_in: 0.5, new_fade_out: 0.25,
                new_channel_config: klipp_core::ChannelConfig::default(),
            },
        };
        let new = HistoryEntry {
            sequence_idx: 7,
            command: EditCommand::EditClipAudio {
                clip_id: ClipId::new(),
                old_gain: 9.0, old_volume: 9.0, old_pan: 0.9,
                old_fade_in: 9.0, old_fade_out: 9.0,
                old_channel_config: klipp_core::ChannelConfig::default(),
                new_gain: 5.0, new_volume: 0.7, new_pan: -0.3,
                new_fade_in: 1.5, new_fade_out: 0.75,
                new_channel_config: klipp_core::ChannelConfig::default(),
            },
        };
        let combined = combine_old_new(old, new);
        assert_eq!(combined.sequence_idx, 7);
        match combined.command {
            EditCommand::EditClipAudio {
                old_gain, old_volume, old_pan, old_fade_in, old_fade_out,
                new_gain, new_volume, new_pan, new_fade_in, new_fade_out,
                ..
            } => {
                // OLD side comes from `old_entry` (1.0 / 0.5 / 0.25),
                // NOT from `new_entry`'s old_* (9.0).
                assert_eq!(old_gain, 1.0);
                assert_eq!(old_volume, 1.0);
                assert_eq!(old_pan, 0.0);
                assert_eq!(old_fade_in, 0.5);
                assert_eq!(old_fade_out, 0.25);
                // NEW side comes from `new_entry` (5.0 / 0.7 / -0.3 / 1.5 / 0.75).
                assert_eq!(new_gain, 5.0);
                assert_eq!(new_volume, 0.7);
                assert_eq!(new_pan, -0.3);
                assert_eq!(new_fade_in, 1.5);
                assert_eq!(new_fade_out, 0.75);
            }
            _ => panic!("expected EditClipAudio after coalesce"),
        }
    }

    // The wildcard arm in combine_old_new behaves differently in
    // debug vs release: debug_assert! fires loudly in debug builds,
    // and release returns old_entry.command.clone() so the user gets
    // an undo that at least rolls back to the pre-drag state. Pair
    // of cfg-gated tests pin both halves of that contract.

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "unhandled variant pair")]
    fn combine_old_new_unmatched_variants_trips_debug_assert_in_debug() {
        // Pairing EditClipAudio (coalesce-eligible) with LegacySlipClip
        // (no matching arm) hits the wildcard. CI in debug catches a
        // future "I added a new coalesce-eligible variant and forgot
        // to extend combine_old_new" before it ships.
        let _ = combine_old_new(audio_entry(1.0), slip_entry());
    }

    #[cfg(not(debug_assertions))]
    #[test]
    fn combine_old_new_unmatched_variants_falls_back_to_old_in_release() {
        // In release the debug_assert is a no-op and the wildcard
        // falls back to old_entry.command.clone(). The pre-drag
        // command MUST be carried through so undo rolls back to the
        // last known-good state — the original U-3 bug was that the
        // wildcard returned new.command, throwing away old state.
        let old = audio_entry(1.0);
        let new = slip_entry();
        let combined = combine_old_new(old, new);
        match combined.command {
            EditCommand::EditClipAudio { old_gain, .. } => {
                assert_eq!(old_gain, 1.0,
                    "wildcard fallback must carry the pre-drag command (old), not the post-drag command (new)");
            }
            other => panic!("expected EditClipAudio (from old_entry), got {other:?}"),
        }
    }

    // ── U-4: end_drag ───────────────────────────────────────────────

    #[test]
    fn end_drag_pushes_final_when_pending_old_entry_missing() {
        // Inconsistent state: pending_coalesce_key is Some but
        // pending_old_entry is None. The defensive else branch must
        // push final_state on its own so the user's edit survives —
        // before the fix this dropped the edit on the floor.
        let mut h = CommandHistory::new();
        h.pending_coalesce_key = Some("test_key".to_string());
        h.pending_old_entry = None;

        let final_state = audio_entry(2.5);
        h.end_drag(final_state);

        assert_eq!(h.undo_stack.len(), 1, "final_state must reach the undo stack");
        match &h.undo_stack[0].command {
            EditCommand::EditClipAudio { new_gain, .. } => {
                assert_eq!(*new_gain, 2.5, "the pushed entry IS the final_state we passed");
            }
            other => panic!("expected EditClipAudio, got {other:?}"),
        }
        // The stale coalesce state is cleared either way.
        assert!(h.pending_coalesce_key.is_none());
        assert!(h.pending_old_entry.is_none());
    }

    #[test]
    fn end_drag_no_op_when_no_pending_drag() {
        // The OTHER path: end_drag called without a prior begin_drag
        // (e.g. duplicate end_drag firing from two callers). Both
        // pending_* are None — the function should silently no-op,
        // NOT push final_state. This pin makes that intentional vs
        // accidental: changing this behaviour later would surface
        // here.
        let mut h = CommandHistory::new();
        let final_state = audio_entry(7.0);
        h.end_drag(final_state);

        assert_eq!(h.undo_stack.len(), 0,
            "no pending drag = no-op; stale duplicate end_drag should not produce a phantom undo entry");
    }
}

// ═══════════════════════════════════════════════════════════════════
// Audio chain snapshot roundtrip — covers AddAudioEffect /
// RemoveAudioEffect / UpdateAudioEffectParam / ToggleAudioEffect /
// SetAudioEffectWetDry / ReorderAudioEffect undo paths. Before this
// fix, none of these handlers pushed history at all — the user could
// add an effect and not undo it.
// ═══════════════════════════════════════════════════════════════════
#[cfg(test)]
mod audio_chain_snapshot_tests {
    use super::*;
    use klipp_core::{AudioEffectInstance, ClipId, Interpolation, MediaId, RationalTime};
    use std::collections::HashMap;

    fn make_clip() -> Clip {
        Clip::media(
            MediaId::new(),
            RationalTime::from_frames(0, 24),
            RationalTime::from_frames(48, 24),
            RationalTime::from_frames(0, 24),
            RationalTime::from_frames(48, 24),
        )
    }

    fn reverb() -> AudioEffectInstance {
        AudioEffectInstance {
            effect_type: "reverb".into(),
            enabled: true,
            wet_dry: 0.5,
            params: vec![("room_size".into(), 0.7), ("damping".into(), 0.3)],
        }
    }

    #[test]
    fn snapshot_captures_effects_and_kfs() {
        let mut clip = make_clip();
        clip.audio_effects = vec![reverb()];
        clip.audio_effect_keyframes.insert(
            "0:room_size".to_string(),
            vec![Keyframe {
                time_secs: 0.5,
                value: 0.9,
                interpolation: Interpolation::Linear,
                tangent_in: (0.0, 0.0),
                tangent_out: (0.0, 0.0),
            }],
        );

        let snap = AudioChainSnapshot::from_clip(&clip);

        assert_eq!(snap.audio_effects.len(), 1);
        assert_eq!(snap.audio_effects[0].effect_type, "reverb");
        assert_eq!(snap.audio_effects[0].wet_dry, 0.5);
        assert_eq!(snap.audio_effect_keyframes.len(), 1);
        let kfs = snap.audio_effect_keyframes.get("0:room_size").unwrap();
        assert_eq!(kfs.len(), 1);
        assert!((kfs[0].value - 0.9).abs() < 1e-6);
    }

    #[test]
    fn apply_overwrites_clip_chain_state() {
        // Build a "before" clip with no effects, snapshot it. Then add
        // an effect to the clip. Apply the snapshot — the effect must
        // be removed (clip state restored to "before").
        let mut clip = make_clip();
        let snap_empty = AudioChainSnapshot::from_clip(&clip);

        clip.audio_effects.push(reverb());
        clip.audio_effect_keyframes.insert(
            "0:room_size".to_string(),
            vec![Keyframe {
                time_secs: 0.0, value: 1.0,
                interpolation: Interpolation::Linear,
                tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
            }],
        );

        snap_empty.apply_to_clip(&mut clip);
        assert!(clip.audio_effects.is_empty(),
            "apply must restore the snapshot's empty effect chain");
        assert!(clip.audio_effect_keyframes.is_empty(),
            "apply must also restore kf state — leaving stale kfs would \
             reference a removed effect index");
    }

    #[test]
    fn edit_audio_chain_label_uses_description() {
        let entry = EditCommand::EditAudioChain {
            clip_id: ClipId::new(),
            description: "Add Audio Effect",
            old_state: AudioChainSnapshot {
                audio_effects: vec![],
                audio_effect_keyframes: HashMap::new(),
            },
            new_state: AudioChainSnapshot {
                audio_effects: vec![reverb()],
                audio_effect_keyframes: HashMap::new(),
            },
        };
        assert_eq!(entry.label(), "Add Audio Effect");
    }

    #[test]
    fn snapshot_roundtrip_preserves_per_param_kfs() {
        // Multiple kf tracks — make sure they all survive the round trip
        // verbatim (sorted insertion order isn't preserved by HashMap,
        // so we compare keys + values explicitly).
        let mut clip = make_clip();
        clip.audio_effects = vec![reverb(), AudioEffectInstance {
            effect_type: "compressor".into(),
            enabled: true,
            wet_dry: 1.0,
            params: vec![("threshold".into(), -12.0)],
        }];
        let kf = |t, v| Keyframe {
            time_secs: t, value: v,
            interpolation: Interpolation::Linear,
            tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
        };
        clip.audio_effect_keyframes.insert("0:room_size".into(), vec![kf(0.0, 0.5), kf(1.0, 0.9)]);
        clip.audio_effect_keyframes.insert("1:threshold".into(), vec![kf(0.5, -6.0)]);
        clip.audio_effect_keyframes.insert("0:__wetdry".into(), vec![kf(0.0, 1.0), kf(2.0, 0.0)]);

        let snap = AudioChainSnapshot::from_clip(&clip);

        // Round-trip: clear clip, then re-apply.
        clip.audio_effects.clear();
        clip.audio_effect_keyframes.clear();
        snap.apply_to_clip(&mut clip);

        assert_eq!(clip.audio_effects.len(), 2);
        assert_eq!(clip.audio_effect_keyframes.len(), 3);
        assert_eq!(clip.audio_effect_keyframes["0:room_size"].len(), 2);
        assert!((clip.audio_effect_keyframes["0:room_size"][1].value - 0.9).abs() < 1e-6);
        assert!((clip.audio_effect_keyframes["1:threshold"][0].value - (-6.0)).abs() < 1e-6);
        assert!((clip.audio_effect_keyframes["0:__wetdry"][1].value - 0.0).abs() < 1e-6);
    }
}

// ═══════════════════════════════════════════════════════════════════
// Audio-effect keyframe mutation semantics (Phase B follow-up).
//
// Each test mirrors the in-place mutation logic that the
// Reverse/Scale/Bump/Paste AppCommand handlers in app.rs run on
// `clip.audio_effect_keyframes["{idx}:{name}"]`. Testing the handlers
// directly would require standing up the full KlippApp; testing the
// LOGIC at the data-mutation level catches the same bugs (off-by-
// one in time math, retain-clamp edge cases, tangent-mirror
// direction) without that ceremony.
// ═══════════════════════════════════════════════════════════════════
#[cfg(test)]
mod audio_effect_kf_ops_tests {
    use super::*;
    use klipp_core::{Interpolation, Keyframe, MediaId, RationalTime};

    fn make_clip_with_kfs(track_key: &str, kfs: Vec<Keyframe>) -> Clip {
        let mut c = Clip::media(
            MediaId::new(),
            RationalTime::from_frames(0, 24),
            RationalTime::from_frames(48, 24), // 2.0s @ 24fps
            RationalTime::from_frames(0, 24),
            RationalTime::from_frames(48, 24),
        );
        c.audio_effect_keyframes.insert(track_key.to_string(), kfs);
        c
    }

    fn kf(t: f64, v: f32) -> Keyframe {
        Keyframe {
            time_secs: t, value: v,
            interpolation: Interpolation::Linear,
            tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
        }
    }

    fn kf_bezier(t: f64, v: f32, tin: (f64, f32), tout: (f64, f32)) -> Keyframe {
        Keyframe {
            time_secs: t, value: v,
            interpolation: Interpolation::Bezier,
            tangent_in: tin, tangent_out: tout,
        }
    }

    /// Mirror the Reverse handler's mutation: reflect kf times around
    /// the max time, swap tangent_in / tangent_out with x-negation.
    fn reverse_kfs_in_place(kfs: &mut Vec<Keyframe>) {
        if kfs.len() < 2 { return; }
        let max_time = kfs.last().map(|k| k.time_secs).unwrap_or(0.0);
        for k in kfs.iter_mut() {
            k.time_secs = max_time - k.time_secs;
            let old_in = k.tangent_in;
            let old_out = k.tangent_out;
            k.tangent_in = (-old_out.0, old_out.1);
            k.tangent_out = (-old_in.0, old_in.1);
        }
        kfs.reverse();
        kfs.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
    }

    #[test]
    fn reverse_two_kfs_swaps_their_times() {
        let mut clip = make_clip_with_kfs("0:gain", vec![kf(0.0, -6.0), kf(1.0, 0.0)]);
        let kfs = clip.audio_effect_keyframes.get_mut("0:gain").unwrap();
        reverse_kfs_in_place(kfs);
        // After reversal: original t=0,v=-6 becomes t=1,v=-6; original
        // t=1,v=0 becomes t=0,v=0. The values stay attached to their
        // original positions on the curve, the TIMES reflect.
        assert_eq!(kfs.len(), 2);
        assert!((kfs[0].time_secs - 0.0).abs() < 1e-9);
        assert!((kfs[0].value - 0.0).abs() < 1e-6);
        assert!((kfs[1].time_secs - 1.0).abs() < 1e-9);
        assert!((kfs[1].value - (-6.0)).abs() < 1e-6);
    }

    #[test]
    fn reverse_negates_bezier_tangent_x_components() {
        // Bezier handles must point the right way after a reverse:
        // an out-handle pointing forward (+x) becomes an in-handle
        // pointing backward (-x), and vice versa. Without this swap
        // the curve direction would invert visually.
        let mut clip = make_clip_with_kfs("0:gain", vec![
            kf_bezier(0.0, 0.0, (-0.1, 0.0), (0.2, 0.5)),
            kf_bezier(1.0, 1.0, (-0.3, 0.0), (0.0, 0.0)),
        ]);
        let kfs = clip.audio_effect_keyframes.get_mut("0:gain").unwrap();
        reverse_kfs_in_place(kfs);

        // Original first kf had tangent_out=(0.2, 0.5). After reversal
        // it sits at the END (time=1.0) and that out-handle becomes
        // tangent_in=(-0.2, 0.5). y-component preserved (mirror is
        // around X, not Y).
        let last = kfs.last().unwrap();
        assert!((last.time_secs - 1.0).abs() < 1e-9);
        assert!((last.tangent_in.0 - (-0.2)).abs() < 1e-9,
            "tangent_in.0 should be -tangent_out.0, got {}", last.tangent_in.0);
        assert!((last.tangent_in.1 - 0.5).abs() < 1e-9);
    }

    #[test]
    fn reverse_does_nothing_for_lt_two_kfs() {
        // Single kf reversal would be visually meaningless; the
        // handler short-circuits. Pin that.
        let mut clip = make_clip_with_kfs("0:gain", vec![kf(0.5, 1.0)]);
        let original = clip.audio_effect_keyframes["0:gain"].clone();
        let kfs = clip.audio_effect_keyframes.get_mut("0:gain").unwrap();
        reverse_kfs_in_place(kfs);
        assert_eq!(kfs[0].time_secs, original[0].time_secs);
        assert_eq!(kfs[0].value, original[0].value);
    }

    /// Mirror the ScaleTiming handler.
    fn scale_kfs_timing_in_place(kfs: &mut Vec<Keyframe>, factor: f64, clip_dur: f64) {
        for k in kfs.iter_mut() {
            k.time_secs *= factor;
            k.tangent_in.0 *= factor;
            k.tangent_out.0 *= factor;
        }
        kfs.retain(|k| k.time_secs >= 0.0 && k.time_secs <= clip_dur);
        kfs.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
    }

    #[test]
    fn scale_half_speed_compresses_timing() {
        // Half-speed-button uses factor=2.0 (envelope plays for 2× as
        // long). Three kfs at 0/1/2s become 0/2/4s; clipped at clip_dur=2s
        // so the third kf is dropped.
        let mut clip = make_clip_with_kfs("0:gain", vec![
            kf(0.0, 0.0), kf(1.0, 1.0), kf(2.0, 0.0),
        ]);
        let kfs = clip.audio_effect_keyframes.get_mut("0:gain").unwrap();
        scale_kfs_timing_in_place(kfs, 2.0, 2.0);
        assert_eq!(kfs.len(), 2, "kf at t=4s after scale must be retained-clamped: {kfs:?}");
        assert_eq!(kfs[0].time_secs, 0.0);
        assert_eq!(kfs[1].time_secs, 2.0);
    }

    #[test]
    fn scale_double_speed_stretches_tangent_x() {
        // Double-speed-button uses factor=0.5. tangent_in.0 / out.0
        // must scale with the time component or the bezier curve
        // shape distorts.
        let mut clip = make_clip_with_kfs("0:gain", vec![
            kf_bezier(0.0, 0.0, (0.0, 0.0), (0.4, 0.5)),
            kf_bezier(2.0, 1.0, (-0.4, 0.0), (0.0, 0.0)),
        ]);
        let kfs = clip.audio_effect_keyframes.get_mut("0:gain").unwrap();
        scale_kfs_timing_in_place(kfs, 0.5, 2.0);
        assert!((kfs[0].tangent_out.0 - 0.2).abs() < 1e-9,
            "out.0 must scale with time: 0.4 * 0.5 = 0.2, got {}", kfs[0].tangent_out.0);
        assert!((kfs[1].tangent_in.0 - (-0.2)).abs() < 1e-9,
            "in.0 must scale: -0.4 * 0.5 = -0.2, got {}", kfs[1].tangent_in.0);
    }

    /// Mirror the InsertBump handler.
    fn insert_bump(
        kfs: &mut Vec<Keyframe>,
        center_time: f64, bump_value: f32, base_value: f32,
        duration_secs: f64, ramp_secs: f64, clip_dur: f64,
    ) {
        let bump_kfs: Vec<(f64, f32)> = vec![
            (center_time - duration_secs / 2.0 - ramp_secs, base_value),
            (center_time - duration_secs / 2.0, bump_value),
            (center_time + duration_secs / 2.0, bump_value),
            (center_time + duration_secs / 2.0 + ramp_secs, base_value),
        ];
        for (t, v) in bump_kfs {
            if t >= 0.0 && t <= clip_dur {
                kfs.push(Keyframe {
                    time_secs: t, value: v,
                    interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                });
            }
        }
        kfs.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
    }

    #[test]
    fn bump_inserts_four_kfs_in_clip_range() {
        // Center 1.0s, duration 0.5s (bump from 0.75 to 1.25), ramp
        // 0.15s on each side. Four kfs at 0.6, 0.75, 1.25, 1.4s.
        let mut clip = make_clip_with_kfs("0:gain", vec![]);
        let kfs = clip.audio_effect_keyframes.get_mut("0:gain").unwrap();
        insert_bump(kfs, 1.0, 1.0, 0.0, 0.5, 0.15, 2.0);
        assert_eq!(kfs.len(), 4);
        let times: Vec<f64> = kfs.iter().map(|k| k.time_secs).collect();
        assert!((times[0] - 0.6).abs() < 1e-9, "first ramp-in start: {times:?}");
        assert!((times[1] - 0.75).abs() < 1e-9);
        assert!((times[2] - 1.25).abs() < 1e-9);
        assert!((times[3] - 1.4).abs() < 1e-9);
        // Bump plateau is at value=1.0; outside ramps are at base_value=0.
        assert!((kfs[1].value - 1.0).abs() < 1e-6);
        assert!((kfs[2].value - 1.0).abs() < 1e-6);
        assert!((kfs[0].value - 0.0).abs() < 1e-6);
        assert!((kfs[3].value - 0.0).abs() < 1e-6);
    }

    #[test]
    fn bump_drops_kfs_that_fall_outside_clip_range() {
        // Center 0.05s with ramp 0.15s — the leading ramp kf would
        // sit at -0.225s. Must be discarded, not clamped.
        let mut clip = make_clip_with_kfs("0:gain", vec![]);
        let kfs = clip.audio_effect_keyframes.get_mut("0:gain").unwrap();
        insert_bump(kfs, 0.05, 1.0, 0.0, 0.0, 0.15, 2.0);
        // center=0.05, duration=0 → bump kfs at 0.05-0-0.15=-0.10,
        // 0.05, 0.05, 0.20. Only three are >= 0.
        assert_eq!(kfs.len(), 3, "kf at t=-0.10 must be dropped: {kfs:?}");
    }

    /// Mirror the Paste handler's time-scale logic (when source dur
    /// differs from target dur).
    fn paste_kfs_with_scaling(source_kfs: &[Keyframe], source_dur: f64, target_dur: f64) -> Vec<Keyframe> {
        let scale = if source_dur > 0.0 { target_dur / source_dur } else { 1.0 };
        let mut new_kfs: Vec<Keyframe> = source_kfs.iter().map(|kf| {
            let mut k = kf.clone();
            k.time_secs *= scale;
            k
        }).collect();
        new_kfs.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
        new_kfs
    }

    #[test]
    fn paste_scales_kf_times_to_target_clip_duration() {
        // Source clip 2s, target clip 4s → kfs scale by 2x. A kf at
        // 1.0s (midpoint of source) should land at 2.0s (midpoint of
        // target).
        let source = vec![kf(0.0, 0.0), kf(1.0, 1.0), kf(2.0, 0.0)];
        let pasted = paste_kfs_with_scaling(&source, 2.0, 4.0);
        assert_eq!(pasted.len(), 3);
        assert!((pasted[0].time_secs - 0.0).abs() < 1e-9);
        assert!((pasted[1].time_secs - 2.0).abs() < 1e-9);
        assert!((pasted[2].time_secs - 4.0).abs() < 1e-9);
    }

    #[test]
    fn paste_into_zero_duration_source_uses_identity() {
        // Defensive: if the clipboard somehow has source_duration=0
        // (malformed save state, or the source clip was empty), the
        // scale falls back to 1.0 — a sane no-op rather than NaN'ing
        // every time_secs.
        let source = vec![kf(0.5, 1.0)];
        let pasted = paste_kfs_with_scaling(&source, 0.0, 5.0);
        assert_eq!(pasted.len(), 1);
        assert!((pasted[0].time_secs - 0.5).abs() < 1e-9);
    }
}
