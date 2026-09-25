use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};
use klipp_core::{AudioEffectInstance, BinId, ClipId, Fps, Keyframe, MediaId, RationalTime, Resolution};
use klipp_timeline::Clip;

/// Window during which consecutive "Edit Title Text" pushes for the
/// same clip collapse into a single undo entry. 150 ms matches the
/// typical inter-keystroke interval of a typing burst — anything longer
/// is treated as a deliberate pause and starts a new undo entry.
const TITLE_TEXT_COALESCE_WINDOW: Duration = Duration::from_millis(150);

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
    pub transform_tilt_x_keyframes: Vec<Keyframe>,
    pub transform_tilt_y_keyframes: Vec<Keyframe>,
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
            transform_tilt_x_keyframes: Vec::new(),
            transform_tilt_y_keyframes: Vec::new(),
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
            transform_tilt_x_keyframes: clip.transform_tilt_x_keyframes.clone(),
            transform_tilt_y_keyframes: clip.transform_tilt_y_keyframes.clone(),
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
        clip.transform_tilt_x_keyframes = self.transform_tilt_x_keyframes.clone();
        clip.transform_tilt_y_keyframes = self.transform_tilt_y_keyframes.clone();
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
    /// Atomic undo for "double-click bin item to timeline" / dragged-from-bin
    /// linked-import. Captures every side-effect of one user gesture so a
    /// single ctrl+z reverts all of: appended video clip, appended audio
    /// clip, an audio track auto-created to host it, and the
    /// resolution/fps autoset that fires when the sequence was empty.
    LinkedImport {
        video: Option<(Clip, usize)>,
        audio: Option<(Clip, usize)>,
        /// Set when a new video track was auto-created above the stack
        /// to host this import (Shift+double-click escape hatch). Undo
        /// removes it. Index is captured *after* creation, before any
        /// audio track was pushed.
        created_video_track_at: Option<usize>,
        /// Set when the audio track was auto-created at the end of the
        /// track list to host the imported audio. Undo removes it.
        created_audio_track_at: Option<usize>,
        /// (prev, new) resolution + fps when the import triggered the
        /// empty-sequence autoset. Undo restores prev; redo restores new.
        autoset: Option<((Resolution, Fps), (Resolution, Fps))>,
    },
    /// A track added at `index` (the plan's add_track). Undo removes it,
    /// which is safe because a batch undoes its clips first.
    AddTrack { index: usize, track: klipp_timeline::Track },
    /// The sequence's brief, replaced whole.
    SetBrief { old: Option<Box<klipp_timeline::Brief>>, new: Option<Box<klipp_timeline::Brief>> },
    /// The sequence's edit plan, replaced whole.
    SetEditPlan { old: Option<Box<klipp_timeline::EditPlan>>, new: Option<Box<klipp_timeline::EditPlan>> },
    /// A clip's role: dialogue, music, effects… what the mix and the
    /// checks treat it as.
    SetClipRole { clip_id: ClipId, old: klipp_core::ClipRole, new: klipp_core::ClipRole },
    /// The sequence's picture size and rate, as (resolution, fps).
    SetSequenceFormat { old: (Resolution, Fps), new: (Resolution, Fps) },
    /// The move over the whole sequence; see `klipp_timeline::Camera`.
    SetSequenceCamera { old: Box<klipp_timeline::Camera>, new: Box<klipp_timeline::Camera> },
    /// How far back a clip sits for the camera; see `Clip::depth`.
    SetClipDepth { clip_id: ClipId, old: f32, new: f32 },
    /// Replace the persisted values of an inserted composition template.
    SetTemplateInstance { clip_id: ClipId, parameter: String, old: Box<klipp_core::TemplateInstance>, new: Box<klipp_core::TemplateInstance> },
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
    /// Swap a clip's backing media (FCPX-style "Replace From Bin").
    /// Captures the old `media_id`, plus `source_out` + `timeline_out`
    /// in case the swap auto-clamped them — undo restores all three
    /// even if the new media's duration was shorter than the old
    /// trim. Other clip props (effects, keyframes, transitions, link)
    /// are preserved by the forward path so they don't need capture.
    LegacyReplaceClipMedia {
        clip_id: ClipId,
        old_media_id: klipp_core::MediaId,
        new_media_id: klipp_core::MediaId,
        old_source_out: RationalTime,
        new_source_out: RationalTime,
        old_timeline_out: RationalTime,
        new_timeline_out: RationalTime,
    },
    /// Exchange two adjacent clips on the same track. Both clips' old
    /// `timeline_in`/`timeline_out` are captured so undo restores
    /// exact positions even after subsequent rounding. When the
    /// clips' linked A/V partners also swapped (matched-pair only),
    /// their old positions are captured separately.
    LegacySwapClips {
        clip_a_id: ClipId,
        a_old_in: RationalTime,
        a_old_out: RationalTime,
        clip_b_id: ClipId,
        b_old_in: RationalTime,
        b_old_out: RationalTime,
        /// Linked partner of `clip_a`, with its old `timeline_in`,
        /// when both clips had a linked partner and the swap
        /// propagated to those partners on their track.
        linked_a_partner: Option<(ClipId, RationalTime, RationalTime)>,
        linked_b_partner: Option<(ClipId, RationalTime, RationalTime)>,
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
    /// An insert edit on free tracks: the clip, every split it made,
    /// every clip it moved, and the transitions and links it re-pointed
    /// (the timeline crate owns that operation and its inverse); plus the
    /// linked audio clip laid under it, and the audio track created to
    /// hold that if there was none. One ctrl+z reverts all of it.
    LegacyInsertEdit {
        edit: klipp_timeline::insert_edit::InsertEdit,
        audio: Option<(Clip, usize)>,
        created_audio_track_at: Option<usize>,
    },
    /// Several edits made by one gesture — a multi-selection dropped on
    /// a track — undone and redone as one. Undo applies the inverses in
    /// reverse order; redo the forwards in order.
    Batch(Vec<EditCommand>),
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

    // === TRANSITIONS ===
    /// A transition added between two adjacent clips on a track. On a
    /// picture track it also fades the clips' own sound out and in (the
    /// dip under a dissolve, overridden where the linked sound has a
    /// crossfade of its own); on an audio track it is the crossfade, and
    /// the clips' fades are left alone. Undo removes it, restores the
    /// fades, and puts back any transition it replaced.
    AddTransition {
        track_index: usize,
        transition: klipp_timeline::Transition,
        clip_a_id: ClipId,
        clip_b_id: ClipId,
        old_fade_out_a: f32,
        old_fade_in_b: f32,
        replaced: Option<klipp_timeline::Transition>,
    },
    /// A transition taken away. On a picture track the fades it set on
    /// its clips go with it; undo puts both back.
    RemoveTransition {
        track_index: usize,
        transition: klipp_timeline::Transition,
        old_fade_out_a: f32,
        old_fade_in_b: f32,
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
    /// The whole builtin-effects list of a clip, replaced: adding a
    /// vignette, a grain, a stabiliser. The enabled-flags vec goes with
    /// it. Snapshot both sides, restore on undo.
    SetBuiltinEffects {
        clip_id: ClipId,
        old_effects: Vec<klipp_effects::BuiltinEffect>,
        old_enabled: Vec<bool>,
        new_effects: Vec<klipp_effects::BuiltinEffect>,
        new_enabled: Vec<bool>,
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

/// Undo the side-effects of `EditCommand::LinkedImport` on a Sequence.
/// Returns the Fps the caller should propagate to its own UI state
/// when the autoset is being reverted.
pub fn apply_linked_import_inverse(
    seq: &mut klipp_timeline::Sequence,
    video: &Option<(Clip, usize)>,
    audio: &Option<(Clip, usize)>,
    created_video_track_at: Option<usize>,
    created_audio_track_at: Option<usize>,
    autoset: &Option<((Resolution, Fps), (Resolution, Fps))>,
) -> Option<Fps> {
    if let Some((clip, ti)) = video {
        if let Some(track) = seq.tracks.get_mut(*ti) {
            track.remove_clip(clip.id);
        }
    }
    if let Some((clip, ti)) = audio {
        if let Some(track) = seq.tracks.get_mut(*ti) {
            track.remove_clip(clip.id);
        }
    }
    // Drop auto-created tracks in descending index order so the lower
    // index stays valid through the higher-index removal.
    let mut to_drop: Vec<usize> = [created_video_track_at, created_audio_track_at]
        .into_iter().flatten().collect();
    to_drop.sort_unstable();
    while let Some(at) = to_drop.pop() {
        if at < seq.tracks.len() {
            seq.tracks.remove(at);
        }
    }
    if let Some((prev, _new)) = autoset {
        seq.resolution = prev.0;
        seq.fps = prev.1;
        Some(prev.1)
    } else {
        None
    }
}

/// Re-apply the side-effects of `EditCommand::LinkedImport` on a Sequence.
/// Returns the Fps the caller should propagate when the autoset is being
/// re-applied.
pub fn apply_linked_import_forward(
    seq: &mut klipp_timeline::Sequence,
    video: &Option<(Clip, usize)>,
    audio: &Option<(Clip, usize)>,
    created_video_track_at: Option<usize>,
    created_audio_track_at: Option<usize>,
    autoset: &Option<((Resolution, Fps), (Resolution, Fps))>,
) -> Option<Fps> {
    let propagated_fps = if let Some((_prev, new)) = autoset {
        seq.resolution = new.0;
        seq.fps = new.1;
        Some(new.1)
    } else {
        None
    };
    // Re-create video track first (inserts at index 0, shifts everyone),
    // then audio (push to end). Order matches the original gesture.
    if created_video_track_at.is_some() {
        let video_count = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video).count();
        seq.add_video_track(format!("Video {}", video_count + 1));
    }
    if created_audio_track_at.is_some() {
        let audio_count = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Audio).count();
        seq.add_audio_track(format!("Audio {}", audio_count + 1));
    }
    if let Some((clip, ti)) = video {
        if let Some(track) = seq.tracks.get_mut(*ti) {
            track.clips.push(clip.clone());
            track.sort_clips();
        }
    }
    if let Some((clip, ti)) = audio {
        if let Some(track) = seq.tracks.get_mut(*ti) {
            track.clips.push(clip.clone());
            track.sort_clips();
        }
    }
    propagated_fps
}

/// Undo `EditCommand::LegacyInsertEdit`: the audio companion and its
/// track go first, then the insert itself is unwound. The track, if one
/// was created, holds only the companion — it did not exist when the
/// insert rippled the others — so removing it loses nothing else.
pub fn apply_insert_edit_inverse(
    seq: &mut klipp_timeline::Sequence,
    edit: &klipp_timeline::insert_edit::InsertEdit,
    audio: &Option<(Clip, usize)>,
    created_audio_track_at: Option<usize>,
) {
    apply_linked_import_inverse(seq, &None, audio, None, created_audio_track_at, &None);
    seq.undo_insert_edit(edit);
}

/// Redo `EditCommand::LegacyInsertEdit`: the insert, then the companion
/// (and its track) on top, as the original gesture did it. `edit.clip`
/// already carries its link to the companion, so the re-inserted video
/// clip comes back linked.
pub fn apply_insert_edit_forward(
    seq: &mut klipp_timeline::Sequence,
    edit: &klipp_timeline::insert_edit::InsertEdit,
    audio: &Option<(Clip, usize)>,
    created_audio_track_at: Option<usize>,
) {
    seq.redo_insert_edit(edit);
    apply_linked_import_forward(seq, &None, audio, None, created_audio_track_at, &None);
}

impl EditCommand {
    /// A cut of one clip into two, on a legacy or a magnetic track.
    fn is_blade(&self) -> bool {
        matches!(self, EditCommand::LegacyBladeClip { .. })
            || matches!(self, EditCommand::Magnetic(cmd) if cmd.label() == "Blade")
    }

    pub fn label(&self) -> &'static str {
        match self {
            EditCommand::AddTitleClip { .. } => "Add Title",
            EditCommand::AddTrack { .. } => "Add Track",
            EditCommand::SetBrief { .. } => "Set Brief",
            EditCommand::SetEditPlan { .. } => "Set Edit Plan",
            EditCommand::SetClipRole { .. } => "Set Clip Role",
            EditCommand::SetSequenceFormat { .. } => "Sequence Format",
            EditCommand::SetSequenceCamera { .. } => "Camera",
            EditCommand::SetClipDepth { .. } => "Depth",
            EditCommand::SetTemplateInstance { .. } => "Template Parameter",
            EditCommand::LegacyAppendClip { .. } => "Append Clip",
            EditCommand::LinkedImport { .. } => "Add to Timeline",
            EditCommand::LegacyRemoveClip { .. } | EditCommand::LegacyRippleRemove { .. } => "Remove Clip",
            EditCommand::LegacyMoveClip { .. } | EditCommand::LegacyMoveClipToTrack { .. } => "Move Clip",
            EditCommand::LegacySwapClips { .. } => "Swap Clips",
            EditCommand::LegacyReplaceClipMedia { .. } => "Replace Clip Media",
            EditCommand::LegacyTrimClip { .. } | EditCommand::LegacyRippleTrimClip { .. } => "Trim Clip",
            EditCommand::LegacyRollTrimClip { .. } => "Roll Trim",
            EditCommand::LegacySlipClip { .. } => "Slip Clip",
            EditCommand::LegacySlideClip { .. } => "Slide Clip",
            EditCommand::LegacyBladeClip { .. } => "Blade Clip",
            EditCommand::LegacySplitAtSceneMarkers { .. } => "Split at Scene Markers",
            EditCommand::LegacyCutClip { .. } => "Cut Clip",
            EditCommand::LegacyPasteClip { .. } => "Paste Clip",
            EditCommand::LegacyInsertEdit { .. } => "Insert Edit",
            // A blade through a clip and its linked audio is one cut, and
            // undoes under the blade's name. Anything else batched is the
            // multi-drop this was first made for.
            EditCommand::Batch(cmds) if !cmds.is_empty() && cmds.iter().all(EditCommand::is_blade) => cmds[0].label(),
            EditCommand::Batch(..) => "Add Clips",
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
            EditCommand::AddTransition { .. } => "Add Transition",
            EditCommand::RemoveTransition { .. } => "Remove Transition",
            EditCommand::SetNodeGraph { .. } => "Color Grade",
            EditCommand::EditKeyframes { description, .. } => description,
            EditCommand::EditClipAudio { .. } => "Edit Audio",
            EditCommand::EditBuiltinEffect { .. } => "Edit Effect",
            EditCommand::SetBuiltinEffects { .. } => "Effects",
            EditCommand::ToggleBuiltinEffectEnabled { .. } => "Toggle Effect",
            EditCommand::SetOFXEffectEnabled { .. } => "Toggle Effect",
            EditCommand::SetClipSpeed { .. } => "Set Clip Speed",
            EditCommand::EditTitle { description, .. } => description,
            EditCommand::EditGenerator { .. } => "Edit Generator",
            EditCommand::EditAudioChain { description, .. } => description,
        }
    }
}

/// One entry on a sequence's undo/redo stack. The sequence is implicit
/// in *which* `CommandHistory` the entry is stored on
/// (`AppState::command_histories[seq_id]`) — there's no need to tag
/// entries with their sequence id.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
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

    /// (clip_id, instant) of the most recent "Edit Title Text" push, used
    /// by `try_coalesce_title_text` to merge adjacent keystrokes into one
    /// undo entry. Cleared whenever a non-text-edit push happens (so
    /// intervening edits cleanly break the burst).
    last_title_text_edit: Option<(ClipId, Instant)>,

    /// The document revision to advance on every push, undo and redo:
    /// what a client asks to learn whether anything changed.
    revision: Option<std::sync::Arc<klipp_core::ControlShared>>,

    /// While a batch is open, pushes collect here instead of landing on
    /// the stack; `end_batch` pushes them as one `Batch` entry.
    batch: Option<Vec<EditCommand>>,
}

impl CommandHistory {
    pub fn new() -> Self {
        Self {
            undo_stack: VecDeque::new(),
            redo_stack: VecDeque::new(),
            max_size: 1000,
            pending_coalesce_key: None,
            pending_old_entry: None,
            last_title_text_edit: None,
            revision: None,
            batch: None,
        }
    }

    /// Collect every push until `end_batch` into one undo entry. A
    /// gesture that makes several edits — a multi-selection dropped on a
    /// track — should take one ctrl+z, not one per clip.
    pub fn begin_batch(&mut self) {
        self.batch = Some(Vec::new());
    }

    /// Whether a batch is open. A gesture that batches its own edits
    /// opens one only when none is: `begin_batch` starts afresh, and
    /// opening a second inside the first would drop what the first had
    /// collected.
    pub fn is_batching(&self) -> bool {
        self.batch.is_some()
    }

    /// Close the batch: nothing collected pushes nothing, one edit pushes
    /// itself, more push a `Batch`.
    pub fn end_batch(&mut self) {
        let Some(mut commands) = self.batch.take() else { return };
        match commands.len() {
            0 => {}
            1 => self.push(HistoryEntry { command: commands.pop().unwrap() }),
            _ => self.push(HistoryEntry { command: EditCommand::Batch(commands) }),
        }
    }

    /// Set the maximum undo stack size. 0 = unlimited.
    pub fn set_max_size(&mut self, max: usize) {
        self.max_size = max;
    }

    /// Let a continuing gesture — a value being dragged — update the edit
    /// on top of the stack instead of stacking one edit per step. `f`
    /// says whether it took the new value; when it did, redo is cleared
    /// as a push would. Returns whether it merged; if not, push.
    pub fn merge_into_last(&mut self, f: impl FnOnce(&mut EditCommand) -> bool) -> bool {
        if self.batch.is_some() {
            return false;
        }
        let Some(last) = self.undo_stack.back_mut() else { return false };
        if !f(&mut last.command) {
            return false;
        }
        self.redo_stack.clear();
        self.bump_revision();
        true
    }

    pub fn push(&mut self, entry: HistoryEntry) {
        if let Some(batch) = self.batch.as_mut() {
            batch.push(entry.command);
            return;
        }
        // Any non-text-edit push terminates a typing burst — the next
        // keystroke after this becomes a fresh undo entry.
        let breaks_burst = !matches!(
            &entry.command,
            EditCommand::EditTitle { description, .. } if *description == "Edit Title Text"
        );
        if breaks_burst {
            self.last_title_text_edit = None;
        }
        self.undo_stack.push_back(entry);
        self.redo_stack.clear();
        self.bump_revision();
        if self.max_size > 0 && self.undo_stack.len() > self.max_size {
            self.undo_stack.pop_front();
        }
    }

    /// Title-text-edit coalescing: if `old → new` is a text-only delta
    /// AND the most recent undo entry is an "Edit Title Text" entry for
    /// `clip_id` pushed within `TITLE_TEXT_COALESCE_WINDOW`, mutate it
    /// in place by replacing its `new_data` and refresh the burst
    /// timestamp. Returns true on success, in which case the caller
    /// must skip pushing a new entry.
    ///
    /// Gating on text-only ensures a font / size / colour change in the
    /// middle of typing breaks the burst — only consecutive keystrokes
    /// merge.
    pub fn try_coalesce_title_text(
        &mut self,
        clip_id: ClipId,
        old: &klipp_timeline::TitleData,
        new: &klipp_timeline::TitleData,
    ) -> bool {
        if !title_diff_is_text_only(old, new) {
            return false;
        }
        let now = Instant::now();
        let within_window = self
            .last_title_text_edit
            .map(|(c, t)| c == clip_id && now.duration_since(t) < TITLE_TEXT_COALESCE_WINDOW)
            .unwrap_or(false);
        if !within_window {
            return false;
        }
        let Some(entry) = self.undo_stack.back_mut() else {
            return false;
        };
        let EditCommand::EditTitle {
            clip_id: ec,
            description,
            new_data: nd,
            ..
        } = &mut entry.command
        else {
            return false;
        };
        if *ec != clip_id || *description != "Edit Title Text" {
            return false;
        }
        *nd = new.clone();
        self.last_title_text_edit = Some((clip_id, now));
        true
    }

    /// Mark the moment of an "Edit Title Text" push so the next
    /// keystroke within the coalesce window can merge into it.
    pub fn note_title_text_edit(&mut self, clip_id: ClipId) {
        self.last_title_text_edit = Some((clip_id, Instant::now()));
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

    /// Discard any pending drag-coalesce envelope without pushing.
    /// Called when the user navigates away (e.g. switches sequences)
    /// before releasing a drag — the gesture can never close in this
    /// stack, so we stop tracking it. Intentionally does NOT push the
    /// pre-drag snapshot; doing so would synthesise an undo entry for
    /// a non-edit, which surprises users.
    pub fn abandon_pending_drag(&mut self) {
        self.pending_coalesce_key = None;
        self.pending_old_entry = None;
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
        self.bump_revision();
        Some(entry)
    }

    pub fn redo(&mut self) -> Option<HistoryEntry> {
        let entry = self.redo_stack.pop_back()?;
        self.undo_stack.push_back(entry.clone());
        self.bump_revision();
        Some(entry)
    }

    /// Advance the shared document revision on every push, undo and
    /// redo of this history.
    pub fn set_revision_shared(&mut self, shared: std::sync::Arc<klipp_core::ControlShared>) {
        self.revision = Some(shared);
    }

    pub fn has_revision_shared(&self) -> bool {
        self.revision.is_some()
    }

    fn bump_revision(&self) {
        if let Some(r) = &self.revision {
            r.bump_revision();
        }
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
/// Returns true iff the only difference between `old` and `new` is the
/// `text` field (and the auto-derived `style_runs`) on one or more
/// `TextBox` layers — i.e. the user is typing into the textarea and
/// nothing else changed. Used by `try_coalesce_title_text` so a
/// font / size / colour change in the middle of a typing burst breaks
/// the burst cleanly.
fn title_diff_is_text_only(
    old: &klipp_timeline::TitleData,
    new: &klipp_timeline::TitleData,
) -> bool {
    if old.objects.len() != new.objects.len() {
        return false;
    }
    let mut clone = old.clone();
    let mut had_text_change = false;
    for (oc, nc) in clone.objects.iter_mut().zip(new.objects.iter()) {
        if let (
            klipp_timeline::TitleObjectContent::TextBox(otb),
            klipp_timeline::TitleObjectContent::TextBox(ntb),
        ) = (&mut oc.content, &nc.content)
        {
            if otb.text != ntb.text {
                had_text_change = true;
                otb.text = ntb.text.clone();
                // style_runs auto-adjust on text-length change in the
                // handler; don't let that look like an unrelated edit.
                otb.style_runs = ntb.style_runs.clone();
            }
        }
    }
    had_text_change && clone == *new
}

/// For EditBuiltinEffect: takes old_effect from old_entry, new_effect from new_entry.
/// For EditClipAudio: takes old_* from old_entry, new_* from new_entry.
fn combine_old_new(old_entry: HistoryEntry, new_entry: HistoryEntry) -> HistoryEntry {
    let command = match (&old_entry.command, &new_entry.command) {
        (EditCommand::EditTitle {clip_id, description, old_data, ..}, EditCommand::EditTitle {new_data, ..}) => EditCommand::EditTitle {clip_id:*clip_id, description, old_data:old_data.clone(), new_data:new_data.clone()},

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
        // A drag on the sequence camera's keys: the whole camera as it was
        // when the drag began and as it is at its end.
        (
            EditCommand::SetSequenceCamera { old, .. },
            EditCommand::SetSequenceCamera { new, .. },
        ) => EditCommand::SetSequenceCamera { old: old.clone(), new: new.clone() },
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
            log_format_user_set: false,
            skim_file: None,
            needs_reprobe: false,
            waveform_data: None,
            audio_cache: None,
            shot: Default::default(),
            metadata_read: false,
            truncated_from_secs: None,
            log: Default::default(),
            synced_audio: None,
            markers: Vec::new(),
            screen_text: String::new(),
            span: None,
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
// Drag-coalesce defensive-path tests
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
    fn reverse_kfs_in_place(kfs: &mut [Keyframe]) {
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

// ═══════════════════════════════════════════════════════════════════
// Title-text-edit coalescing tests
// ═══════════════════════════════════════════════════════════════════
#[cfg(test)]
mod title_text_coalesce_tests {
    use super::*;
    use klipp_timeline::TitleData;

    fn entry(clip_id: ClipId, old: TitleData, new: TitleData) -> HistoryEntry {
        HistoryEntry {
            command: EditCommand::EditTitle {
                clip_id,
                description: "Edit Title Text",
                old_data: old,
                new_data: new,
            },
        }
    }

    fn td_with_text(s: &str) -> TitleData {
        let tb = klipp_timeline::TextBox {
            text: s.to_string(),
            ..Default::default()
        };
        TitleData::single_text(tb)
    }

    fn td_with_text_and_size(s: &str, size: f32) -> TitleData {
        let tb = klipp_timeline::TextBox {
            text: s.to_string(),
            font_size: size,
            ..Default::default()
        };
        TitleData::single_text(tb)
    }

    #[test]
    fn keystroke_burst_collapses_to_single_entry() {
        let mut h = CommandHistory::new();
        let cid = ClipId::new();

        let a = td_with_text("");
        let b = td_with_text("h");
        h.push(entry(cid, a.clone(), b.clone()));
        h.note_title_text_edit(cid);
        assert_eq!(h.undo_stack.len(), 1);

        // Each subsequent keystroke within the window coalesces into the
        // same undo entry — undo_stack.len() stays at 1, but new_data
        // updates to reflect the latest state.
        let c = td_with_text("hi");
        assert!(h.try_coalesce_title_text(cid, &b, &c));
        assert_eq!(h.undo_stack.len(), 1);

        let d = td_with_text("hi!");
        assert!(h.try_coalesce_title_text(cid, &c, &d));
        assert_eq!(h.undo_stack.len(), 1);

        // Undo restores the original "before-burst" state, not the
        // intermediate "h" or "hi" snapshots.
        if let EditCommand::EditTitle { old_data, new_data, .. } =
            &h.undo_stack.back().unwrap().command
        {
            assert_eq!(old_data.text(), "");
            assert_eq!(new_data.text(), "hi!");
        } else {
            panic!("expected EditTitle entry");
        }
    }

    #[test]
    fn font_size_change_breaks_burst() {
        let mut h = CommandHistory::new();
        let cid = ClipId::new();

        let a = td_with_text_and_size("", 32.0);
        let b = td_with_text_and_size("h", 32.0);
        h.push(entry(cid, a, b.clone()));
        h.note_title_text_edit(cid);

        // A non-text-only delta (font size change) must not coalesce
        // into the open burst.
        let c = td_with_text_and_size("h", 48.0);
        assert!(!h.try_coalesce_title_text(cid, &b, &c));
    }

    #[test]
    fn intervening_non_text_push_breaks_burst() {
        let mut h = CommandHistory::new();
        let cid = ClipId::new();

        let a = td_with_text("");
        let b = td_with_text("h");
        h.push(entry(cid, a, b.clone()));
        h.note_title_text_edit(cid);

        // A non-text-edit push (different description) must clear the
        // burst marker so the next text edit pushes a fresh entry.
        let other = HistoryEntry {
            command: EditCommand::EditTitle {
                clip_id: cid,
                description: "Set Font",
                old_data: b.clone(),
                new_data: b.clone(),
            },
        };
        h.push(other);

        let c = td_with_text("hi");
        assert!(!h.try_coalesce_title_text(cid, &b, &c));
    }

    #[test]
    fn idle_gap_breaks_burst() {
        let mut h = CommandHistory::new();
        let cid = ClipId::new();

        let a = td_with_text("");
        let b = td_with_text("h");
        h.push(entry(cid, a, b.clone()));
        // Pretend the burst marker is older than the coalesce window.
        h.last_title_text_edit = Some((
            cid,
            Instant::now() - TITLE_TEXT_COALESCE_WINDOW - Duration::from_millis(50),
        ));

        let c = td_with_text("hi");
        assert!(!h.try_coalesce_title_text(cid, &b, &c));
    }

    #[test]
    fn different_clip_breaks_burst() {
        let mut h = CommandHistory::new();
        let cid_a = ClipId::new();
        let cid_b = ClipId::new();

        let s1 = td_with_text("");
        let s2 = td_with_text("h");
        h.push(entry(cid_a, s1, s2.clone()));
        h.note_title_text_edit(cid_a);

        // Same window, but a different clip — must not coalesce.
        let s3 = td_with_text("hi");
        assert!(!h.try_coalesce_title_text(cid_b, &s2, &s3));
    }

    #[test]
    fn diff_is_text_only_predicate() {
        let mut a = td_with_text_and_size("hello", 32.0);
        let mut b = td_with_text_and_size("hello!", 32.0);
        assert!(title_diff_is_text_only(&a, &b));

        // Same text, different font size → not text-only.
        b = td_with_text_and_size("hello", 48.0);
        assert!(!title_diff_is_text_only(&a, &b));

        // Same text, same everything → no text change, predicate fails.
        b = td_with_text_and_size("hello", 32.0);
        assert!(!title_diff_is_text_only(&a, &b));

        // Different layer count → not text-only.
        a = td_with_text("hello");
        b = td_with_text("hello");
        let tb_extra = klipp_timeline::TextBox {
            text: "extra".into(),
            ..Default::default()
        };
        b.objects.push(klipp_timeline::TitleObject {
            id: klipp_timeline::TitleObjectId(99),
            name: String::new(),
            transform: klipp_timeline::default_text_transform(),
            visible: true,
            locked: false,
            opacity: 1.0,
            opacity_keyframes: Vec::new(),
            content: klipp_timeline::TitleObjectContent::TextBox(tb_extra),
            fx: Some(klipp_timeline::LayerFx::default()),
        });
        assert!(!title_diff_is_text_only(&a, &b));
    }
}

#[cfg(test)]
mod linked_import_tests {
    use super::*;
    use klipp_core::{Fps, MediaId, Resolution, Timecode};
    use klipp_timeline::{Sequence, TrackKind};

    fn dur(secs: i64, rate: u32) -> Timecode {
        Timecode { value: secs * rate as i64, rate }
    }

    /// Simulates the import side of the gesture: append linked V+A clips,
    /// no autoset (sequence already has tracks at default fps), no
    /// auto-created audio track, returns the LinkedImport variant fields
    /// the inverse helper would receive.
    fn run_import_va(seq: &mut Sequence) -> (
        Option<(Clip, usize)>,
        Option<(Clip, usize)>,
        Option<usize>,
        Option<((Resolution, Fps), (Resolution, Fps))>,
    ) {
        let mid = MediaId::new();
        let video_clip_id = seq.tracks[0].append_clip(mid, dur(2, 24));
        let audio_clip_id = seq.tracks[1].append_clip(mid, dur(2, 24));
        let v_snap = seq.tracks[0].find_clip(video_clip_id).unwrap().clone();
        let a_snap = seq.tracks[1].find_clip(audio_clip_id).unwrap().clone();
        (Some((v_snap, 0)), Some((a_snap, 1)), None, None)
    }

    #[test]
    fn inverse_removes_appended_va_clips() {
        let mut seq = Sequence::new_default();
        assert_eq!(seq.tracks[0].clips.len(), 0);
        assert_eq!(seq.tracks[1].clips.len(), 0);
        let (video, audio, created, autoset) = run_import_va(&mut seq);
        assert_eq!(seq.tracks[0].clips.len(), 1);
        assert_eq!(seq.tracks[1].clips.len(), 1);

        let fps_change = apply_linked_import_inverse(&mut seq, &video, &audio, None, created, &autoset);
        assert!(fps_change.is_none());
        assert_eq!(seq.tracks[0].clips.len(), 0);
        assert_eq!(seq.tracks[1].clips.len(), 0);
    }

    #[test]
    fn forward_replays_appended_va_clips() {
        let mut seq = Sequence::new_default();
        let (video, audio, created, autoset) = run_import_va(&mut seq);
        // Roll back, then redo.
        apply_linked_import_inverse(&mut seq, &video, &audio, None, created, &autoset);
        let fps_change = apply_linked_import_forward(&mut seq, &video, &audio, None, created, &autoset);
        assert!(fps_change.is_none());
        assert_eq!(seq.tracks[0].clips.len(), 1);
        assert_eq!(seq.tracks[1].clips.len(), 1);
    }

    #[test]
    fn inverse_drops_auto_created_audio_track() {
        let mut seq = Sequence::new_default();
        // Drop the default audio track to simulate "no audio track exists".
        seq.tracks.retain(|t| t.kind != TrackKind::Audio);
        assert_eq!(seq.tracks.len(), 1);
        // Simulate import side: append a video clip on V1, auto-create
        // an audio track at the end, append the audio clip there.
        let mid = MediaId::new();
        let video_clip_id = seq.tracks[0].append_clip(mid, dur(2, 24));
        seq.add_audio_track("Audio 1".to_string());
        let audio_track_idx = seq.tracks.len() - 1;
        let audio_clip_id = seq.tracks[audio_track_idx].append_clip(mid, dur(2, 24));
        let v = Some((seq.tracks[0].find_clip(video_clip_id).unwrap().clone(), 0usize));
        let a = Some((seq.tracks[audio_track_idx].find_clip(audio_clip_id).unwrap().clone(), audio_track_idx));

        apply_linked_import_inverse(&mut seq, &v, &a, None, Some(audio_track_idx), &None);

        // Audio track and both clips are gone; only V1 remains.
        assert_eq!(seq.tracks.len(), 1);
        assert_eq!(seq.tracks[0].kind, TrackKind::Video);
        assert!(seq.tracks[0].clips.is_empty());
    }

    #[test]
    fn inverse_restores_prev_resolution_and_fps_on_autoset() {
        let mut seq = Sequence::new_default();
        let prev = (seq.resolution, seq.fps);
        let new_res = Resolution { width: 3840, height: 2160 };
        let new_fps = Fps::new(60, 1);
        seq.resolution = new_res;
        seq.fps = new_fps;
        let autoset = Some((prev, (new_res, new_fps)));
        let (video, audio, created, _) = run_import_va(&mut seq);

        let fps_change = apply_linked_import_inverse(&mut seq, &video, &audio, None, created, &autoset);
        assert_eq!(fps_change, Some(prev.1));
        assert_eq!(seq.resolution, prev.0);
        assert_eq!(seq.fps, prev.1);
    }

    #[test]
    fn forward_reapplies_autoset_resolution_and_fps() {
        let mut seq = Sequence::new_default();
        let prev = (seq.resolution, seq.fps);
        let new_res = Resolution { width: 3840, height: 2160 };
        let new_fps = Fps::new(60, 1);
        let autoset = Some((prev, (new_res, new_fps)));

        let fps_change = apply_linked_import_forward(&mut seq, &None, &None, None, None, &autoset);
        assert_eq!(fps_change, Some(new_fps));
        assert_eq!(seq.resolution, new_res);
        assert_eq!(seq.fps, new_fps);
    }

    /// #324: a blade through a clip and its linked audio is one gesture,
    /// batched, and it undoes under the blade's own name.
    #[test]
    fn a_batch_of_one_kind_of_edit_takes_that_kinds_name() {
        let t = |f: i64| klipp_core::RationalTime { value: f, rate: 30 };
        let clip = || Clip::media(klipp_core::MediaId::new(), t(0), t(30), t(0), t(30));
        let blade = |i: usize| EditCommand::LegacyBladeClip {
            track_index: i,
            original_clip: clip(),
            clip_a: clip(),
            clip_b: clip(),
            position: 0,
        };
        assert_eq!(EditCommand::Batch(vec![blade(0), blade(1)]).label(), "Blade Clip");
        let mixed = EditCommand::Batch(vec![blade(0), EditCommand::LinkedImport {
            video: None, audio: None, created_video_track_at: None, created_audio_track_at: None, autoset: None,
        }]);
        assert_eq!(mixed.label(), "Add Clips");
    }

    #[test]
    fn a_batch_says_when_it_is_open() {
        let mut h = CommandHistory::new();
        assert!(!h.is_batching());
        h.begin_batch();
        assert!(h.is_batching());
        h.end_batch();
        assert!(!h.is_batching());
    }

    #[test]
    fn label_is_user_visible_string() {
        let cmd = EditCommand::LinkedImport {
            video: None,
            audio: None,
            created_video_track_at: None,
            created_audio_track_at: None,
            autoset: None,
        };
        assert_eq!(cmd.label(), "Add to Timeline");
    }

    #[test]
    fn inverse_drops_both_auto_created_tracks_in_shift_mode() {
        // Shift-mode V+A import: a fresh video track is added on top
        // (index 0) and a fresh audio track is pushed at the end. Undo
        // must drop both without leaving orphan clips behind.
        let mut seq = Sequence::new_default();
        let video_track_count_before = seq.tracks.iter().filter(|t| t.kind == TrackKind::Video).count();
        let audio_track_count_before = seq.tracks.iter().filter(|t| t.kind == TrackKind::Audio).count();

        // Mirror the shift-mode handler: create new V at top, new A at end.
        seq.add_video_track("Video 2".to_string());
        let new_video_idx = 0usize;
        seq.add_audio_track("Audio 2".to_string());
        let new_audio_idx = seq.tracks.len() - 1;

        let mid = MediaId::new();
        let video_clip_id = seq.tracks[new_video_idx].append_clip(mid, dur(2, 24));
        let audio_clip_id = seq.tracks[new_audio_idx].append_clip(mid, dur(2, 24));
        let v = Some((seq.tracks[new_video_idx].find_clip(video_clip_id).unwrap().clone(), new_video_idx));
        let a = Some((seq.tracks[new_audio_idx].find_clip(audio_clip_id).unwrap().clone(), new_audio_idx));

        apply_linked_import_inverse(
            &mut seq, &v, &a,
            Some(new_video_idx),
            Some(new_audio_idx),
            &None,
        );

        let video_track_count_after = seq.tracks.iter().filter(|t| t.kind == TrackKind::Video).count();
        let audio_track_count_after = seq.tracks.iter().filter(|t| t.kind == TrackKind::Audio).count();
        assert_eq!(video_track_count_after, video_track_count_before);
        assert_eq!(audio_track_count_after, audio_track_count_before);
        // No track left holding clips from this import.
        assert!(seq.tracks.iter().all(|t| t.clips.is_empty()));
    }

    #[test]
    fn forward_recreates_both_tracks_in_shift_mode() {
        let mut seq = Sequence::new_default();
        let starting_video = seq.tracks.iter().filter(|t| t.kind == TrackKind::Video).count();
        let starting_audio = seq.tracks.iter().filter(|t| t.kind == TrackKind::Audio).count();
        // Capture clip snapshots from a faux import on hypothetical new tracks.
        let mid = MediaId::new();
        seq.add_video_track("Video 2".to_string());
        seq.add_audio_track("Audio 2".to_string());
        let v_idx = 0usize;
        let a_idx = seq.tracks.len() - 1;
        let v_id = seq.tracks[v_idx].append_clip(mid, dur(2, 24));
        let a_id = seq.tracks[a_idx].append_clip(mid, dur(2, 24));
        let v_snap = seq.tracks[v_idx].find_clip(v_id).unwrap().clone();
        let a_snap = seq.tracks[a_idx].find_clip(a_id).unwrap().clone();

        // Roll back by inverse, then redo via forward.
        apply_linked_import_inverse(
            &mut seq,
            &Some((v_snap.clone(), v_idx)),
            &Some((a_snap.clone(), a_idx)),
            Some(v_idx),
            Some(a_idx),
            &None,
        );
        assert_eq!(seq.tracks.iter().filter(|t| t.kind == TrackKind::Video).count(), starting_video);
        assert_eq!(seq.tracks.iter().filter(|t| t.kind == TrackKind::Audio).count(), starting_audio);

        apply_linked_import_forward(
            &mut seq,
            &Some((v_snap, v_idx)),
            &Some((a_snap, a_idx)),
            Some(v_idx),
            Some(a_idx),
            &None,
        );

        assert_eq!(seq.tracks.iter().filter(|t| t.kind == TrackKind::Video).count(), starting_video + 1);
        assert_eq!(seq.tracks.iter().filter(|t| t.kind == TrackKind::Audio).count(), starting_audio + 1);
        // Clips landed on the recreated tracks.
        assert_eq!(seq.tracks[0].clips.len(), 1);
        assert_eq!(seq.tracks[seq.tracks.len() - 1].clips.len(), 1);
    }
}

#[cfg(test)]
mod insert_edit_with_audio_tests {
    //! The handler's whole gesture — an insert on V1 with a linked audio
    //! clip laid under it, on a track made for it — undone by one entry
    //! and redone by one, through the helpers the app's undo calls.
    use super::*;
    use klipp_core::{ClipId, MediaId, RationalTime};
    use klipp_timeline::{Sequence, Track, TrackKind};

    fn tc(f: i64) -> RationalTime {
        RationalTime::from_frames(f, 25)
    }
    fn clip(from: i64, to: i64) -> Clip {
        Clip::media(MediaId::new(), tc(from), tc(to), tc(0), tc(to - from))
    }
    type Shape = Vec<(TrackKind, Vec<(ClipId, i64, i64, Option<ClipId>)>)>;
    fn shape(s: &Sequence) -> Shape {
        let f = |t: RationalTime| t.value * 25 / t.rate as i64;
        s.tracks
            .iter()
            .map(|t| (t.kind, t.clips.iter().map(|c| (c.id, f(c.timeline_in), f(c.timeline_out), c.linked_clip_id)).collect()))
            .collect()
    }

    #[test]
    fn one_entry_reverts_the_insert_its_audio_and_the_track_made_for_it() {
        // A video-only sequence: the audio track has to be made.
        let mut seq = Sequence::new_default();
        seq.fps = klipp_core::Fps::new(25, 1);
        let mut v1 = Track::new("V1", TrackKind::Video);
        v1.clips = vec![clip(0, 100)];
        seq.tracks = vec![v1];
        let before = shape(&seq);

        // The handler: insert, then the companion the way
        // attach_linked_audio_to_clip lays it — same span, linked both
        // ways, on an audio track made at the end.
        let inserted = clip(0, 10);
        let video_id = inserted.id;
        let mut edit = seq.insert_edit(0, inserted, tc(40)).expect("insert");
        seq.add_audio_track("Audio 1".into());
        let ai = seq.tracks.len() - 1;
        let mut audio = clip(40, 50);
        audio.linked_clip_id = Some(video_id);
        let audio_id = audio.id;
        seq.tracks[ai].clips.push(audio.clone());
        seq.tracks[0].find_clip_mut(video_id).unwrap().linked_clip_id = Some(audio_id);
        edit.clip.linked_clip_id = Some(audio_id);
        let after = shape(&seq);
        assert_eq!(after[0].1.len(), 3, "left half, insert, right half");
        assert_eq!(after.len(), 2, "an audio track was made");
        assert_eq!(after[1].1, vec![(audio_id, 40, 50, Some(video_id))]);

        let audio_rec = Some((audio, ai));
        apply_insert_edit_inverse(&mut seq, &edit, &audio_rec, Some(ai));
        assert_eq!(shape(&seq), before, "undo did not put the sequence back — track included");

        apply_insert_edit_forward(&mut seq, &edit, &audio_rec, Some(ai));
        assert_eq!(shape(&seq), after, "redo did not reproduce the gesture, links included");

        apply_insert_edit_inverse(&mut seq, &edit, &audio_rec, Some(ai));
        assert_eq!(shape(&seq), before);
    }
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    use klipp_core::MediaId;
    use klipp_timeline::Clip;

    fn append(i: usize) -> EditCommand {
        let t = klipp_core::RationalTime::from_frames(i as i64 * 10, 25);
        EditCommand::LegacyAppendClip { clip: Clip::media(MediaId::new(), t, t, t, t), track_index: 0 }
    }

    #[test]
    fn a_batch_of_several_pushes_is_one_entry_in_order() {
        let mut h = CommandHistory::new();
        h.begin_batch();
        for i in 0..3 {
            h.push(HistoryEntry { command: append(i) });
        }
        assert!(h.undo().is_none(), "nothing lands on the stack while the batch is open");
        h.end_batch();
        let entry = h.undo().expect("one entry");
        assert_eq!(entry.label(), "Add Clips");
        let EditCommand::Batch(commands) = &entry.command else { panic!("not a batch: {:?}", entry.command) };
        assert_eq!(commands.len(), 3);
        for (i, c) in commands.iter().enumerate() {
            let EditCommand::LegacyAppendClip { clip, .. } = c else { panic!() };
            assert_eq!(clip.timeline_in.value, i as i64 * 10, "the batch keeps the order of the pushes");
        }
        assert!(h.undo().is_none(), "the three were one entry");
    }

    #[test]
    fn a_batch_of_one_is_that_entry_and_of_none_is_nothing() {
        let mut h = CommandHistory::new();
        h.begin_batch();
        h.push(HistoryEntry { command: append(0) });
        h.end_batch();
        assert!(matches!(h.undo().map(|e| e.command), Some(EditCommand::LegacyAppendClip { .. })));
        h.begin_batch();
        h.end_batch();
        assert!(h.undo().is_none());
    }
}

#[cfg(test)]
mod merge_tests {
    use super::*;

    #[test]
    fn a_dragged_value_is_one_undo_step_and_a_different_edit_is_its_own() {
        let mut h = CommandHistory::new();
        let clip_id = klipp_core::ClipId::new();
        let depth = |old: f32, new: f32| HistoryEntry { command: EditCommand::SetClipDepth { clip_id, old, new } };
        h.push(depth(1.0, 1.5));
        // The drag goes on: the same edit takes the new value.
        let took = h.merge_into_last(|last| match last {
            EditCommand::SetClipDepth { new, .. } => {
                *new = 3.0;
                true
            }
            _ => false,
        });
        assert!(took);
        assert_eq!(h.undo_stack.len(), 1);
        assert!(matches!(h.undo_stack.back().unwrap().command, EditCommand::SetClipDepth { old, new, .. } if old == 1.0 && new == 3.0));
        // Something else on top is not merged into.
        h.push(HistoryEntry { command: EditCommand::SetSequenceCamera { old: Box::default(), new: Box::default() } });
        assert!(!h.merge_into_last(|last| matches!(last, EditCommand::SetClipDepth { .. })));
    }
}

#[cfg(test)]
mod camera_drag_tests {
    use super::*;

    fn camera_with_x_at(time_secs: f64) -> Box<klipp_timeline::Camera> {
        let mut cam = klipp_timeline::Camera::default();
        klipp_core::keyframe::upsert_keyframe(&mut cam.x_keyframes, time_secs, 0.5);
        Box::new(cam)
    }

    /// Every frame of a camera-key drag moves the key again; the history
    /// keeps the camera from before the first move and from after the
    /// last, as one entry, so undo puts the key back where the drag
    /// found it.
    #[test]
    fn a_camera_drag_is_one_entry_from_its_start_to_its_end() {
        let mut h = CommandHistory::new();
        let before = camera_with_x_at(1.0);
        let after = camera_with_x_at(2.5);
        h.begin_drag("camera:0".into(), HistoryEntry { command: EditCommand::SetSequenceCamera { old: before.clone(), new: before.clone() } });
        assert!(h.is_dragging("camera:0"));
        h.end_drag(HistoryEntry { command: EditCommand::SetSequenceCamera { old: after.clone(), new: after.clone() } });
        assert_eq!(h.undo_stack.len(), 1);
        match &h.undo_stack.back().unwrap().command {
            EditCommand::SetSequenceCamera { old, new } => {
                assert_eq!(old, &before, "undo returns to the camera from before the drag");
                assert_eq!(new, &after, "redo returns to the camera from its end");
            }
            other => panic!("not a camera entry: {other:?}"),
        }
        assert!(!h.is_dragging("camera:0"));
    }
}
