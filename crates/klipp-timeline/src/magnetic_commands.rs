//! Magnetic timeline edit commands — undoable operations produced by MagneticEditor.

use crate::magnetic::{CascadeRecord, RippleRecord};
use crate::sequence::{Clip, MagneticAnchor, TrackRole};
use klipp_core::{ClipId, RationalTime};

/// All undoable magnetic edit commands.
/// Each variant stores enough information to fully reverse the operation.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum MagneticEditCommand {
    InsertPrimary {
        inserted_clip: Clip,
        track_index: usize,
        rippled_clips: Vec<RippleRecord>,
        cascaded_clips: Vec<CascadeRecord>,
    },
    OverwritePrimary {
        inserted_clip: Clip,
        track_index: usize,
        overwritten_clips: Vec<Clip>,
        trimmed_clips: Vec<RippleRecord>,
        /// T-8: when the new clip straddles an existing clip, the
        /// straddled clip is split into fresh-id left + right remnants.
        /// Those remnants need to be re-inserted verbatim by
        /// apply_forward — before this field, apply_forward only had
        /// the trimmed_clips ripple records (positions only) to work
        /// with, and `set_clip_position(remnant_id, …)` silently
        /// no-oped because the remnant didn't exist in the sequence at
        /// replay time. Stored here as full Clip values so redo
        /// reconstructs the remnant exactly. The overlap-left/right
        /// trimmed clips are kept here too, under their own ids (#919).
        split_remnants: Vec<Clip>,
    },
    RippleDelete {
        removed_clip: Clip,
        track_index: usize,
        position: usize,
        rippled_clips: Vec<RippleRecord>,
        cascaded_clips: Vec<CascadeRecord>,
        /// Connected clips whose anchor was the deleted clip — now disconnected.
        disconnected_clips: Vec<(ClipId, usize, MagneticAnchor)>,
        /// Storylines whose anchor was the deleted clip — now demoted.
        disconnected_storylines: Vec<(usize, MagneticAnchor)>,
        /// Linked clip that was also removed (e.g. audio partner).
        removed_linked_clip: Option<(Clip, usize)>,
    },
    Lift {
        removed_clip: Clip,
        gap_clip: Clip,
        track_index: usize,
        position: usize,
        /// T-1: clips whose magnetic_anchor referenced the lifted clip
        /// and were disconnected as part of the lift. On undo the
        /// anchors are restored from these records so the connected
        /// clips re-attach to the re-inserted primary clip.
        disconnected_clips: Vec<(ClipId, usize, MagneticAnchor)>,
        /// T-1: storylines whose anchor was the lifted clip; demoted
        /// to Connected at lift time, restored on undo.
        disconnected_storylines: Vec<(usize, MagneticAnchor)>,
        /// T-3: partner clips (via `linked_clip_id`) whose link pointed
        /// at the lifted clip. Their `linked_clip_id` is cleared here
        /// and restored on undo. Lift doesn't delete the partner (V↔A
        /// pair is broken, not removed), so a dangling id would make
        /// the partner's visual pairing silently drop on the next
        /// clip-id lookup.
        unlinked_partners: Vec<(ClipId, usize)>,
    },
    AppendPrimary {
        appended_clip: Clip,
        track_index: usize,
    },
    MovePrimary {
        clip_id: ClipId,
        remove_command: Box<MagneticEditCommand>,
        insert_command: Box<MagneticEditCommand>,
    },
    InsertGap {
        gap_clip: Clip,
        track_index: usize,
        rippled_clips: Vec<RippleRecord>,
        cascaded_clips: Vec<CascadeRecord>,
    },
    Blade {
        original_clip: Clip,
        left_clip: Clip,
        right_clip: Clip,
        track_index: usize,
        position: usize,
    },
    TrimHead {
        clip_id: ClipId,
        track_index: usize,
        old_timeline_in: RationalTime,
        old_source_in: RationalTime,
        new_timeline_in: RationalTime,
        new_source_in: RationalTime,
        /// A multicam primary's cut list before and after: the cuts move
        /// with the picture under the start (`Clip::keep_angle_cuts_on_picture`).
        old_multicam_cuts: Vec<(f64, u8)>,
        new_multicam_cuts: Vec<(f64, u8)>,
        rippled_clips: Vec<RippleRecord>,
        cascaded_clips: Vec<CascadeRecord>,
    },
    TrimTail {
        clip_id: ClipId,
        track_index: usize,
        old_timeline_out: RationalTime,
        old_source_out: RationalTime,
        new_timeline_out: RationalTime,
        new_source_out: RationalTime,
        rippled_clips: Vec<RippleRecord>,
        cascaded_clips: Vec<CascadeRecord>,
    },
    ConnectClip {
        clip_id: ClipId,
        old_anchor: Option<MagneticAnchor>,
        new_anchor: MagneticAnchor,
        old_position: RationalTime,
        new_position: RationalTime,
    },
    DisconnectClip {
        clip_id: ClipId,
        old_anchor: MagneticAnchor,
        position: RationalTime,
    },
    PromoteToStoryline {
        track_index: usize,
        old_role: TrackRole,
        storyline_anchor: MagneticAnchor,
        old_clip_anchors: Vec<Option<MagneticAnchor>>,
        first_clip_absolute_pos: RationalTime,
    },
    BreakApartStoryline {
        track_index: usize,
        old_anchor: MagneticAnchor,
        storyline_start: RationalTime,
    },
    ResolveCollision {
        clip_id: ClipId,
        track_index: usize,
        old_in: RationalTime,
        old_out: RationalTime,
        new_in: RationalTime,
        new_out: RationalTime,
    },
}

impl MagneticEditCommand {
    /// Human-readable label for the undo/redo menu.
    pub fn label(&self) -> &'static str {
        match self {
            Self::InsertPrimary { .. } => "Insert Clip",
            Self::OverwritePrimary { .. } => "Overwrite",
            Self::RippleDelete { .. } => "Ripple Delete",
            Self::Lift { .. } => "Lift",
            Self::AppendPrimary { .. } => "Append Clip",
            Self::MovePrimary { .. } => "Move Clip",
            Self::InsertGap { .. } => "Insert Gap",
            Self::Blade { .. } => "Blade",
            Self::TrimHead { .. } => "Trim Head",
            Self::TrimTail { .. } => "Trim Tail",
            Self::ConnectClip { .. } => "Connect Clip",
            Self::DisconnectClip { .. } => "Disconnect Clip",
            Self::PromoteToStoryline { .. } => "Create Storyline",
            Self::BreakApartStoryline { .. } => "Break Apart Storyline",
            Self::ResolveCollision { .. } => "Resolve Collision",
        }
    }
}
