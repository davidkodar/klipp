use std::collections::HashMap;
use klipp_core::{BinId, ClipId, Keyframe, MediaId, RationalTime};
use klipp_timeline::Clip;

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
        clip.speed_keyframes = self.speed_keyframes.clone();
    }
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
            EditCommand::EditKeyframes { description, .. } => description,
            EditCommand::EditClipAudio { .. } => "Edit Audio",
            EditCommand::EditBuiltinEffect { .. } => "Edit Effect",
            EditCommand::SetClipSpeed { .. } => "Set Clip Speed",
            EditCommand::EditTitle { description, .. } => description,
            EditCommand::EditGenerator { .. } => "Edit Generator",
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

    /// Number of ops pushed since the last project save. Incremented by push(),
    /// reset to 0 by reset_ops_since_save() when the project is saved. Used by
    /// the op-count autosave trigger.
    pub ops_since_save: u64,
}

impl CommandHistory {
    pub fn new() -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
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
        self.undo_stack.push(entry);
        self.redo_stack.clear();
        if self.max_size > 0 && self.undo_stack.len() > self.max_size {
            self.undo_stack.remove(0);
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

    /// Get all undo stack entries (oldest first) for history panel display.
    pub fn undo_entries(&self) -> &[HistoryEntry] {
        &self.undo_stack
    }

    /// Get all redo stack entries (oldest first) for history panel display.
    pub fn redo_entries(&self) -> &[HistoryEntry] {
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
        // For any other type, just use the new entry as-is
        _ => new_entry.command.clone(),
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
    use klipp_media::{MediaItem, MediaPool};

    /// Helper: create a minimal MediaItem for testing bin operations.
    fn test_media_item(id: MediaId) -> MediaItem {
        MediaItem {
            id,
            path: std::path::PathBuf::from("/test/video.mp4"),
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
