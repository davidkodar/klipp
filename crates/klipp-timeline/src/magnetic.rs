//! Magnetic Timeline — core editing operations.
//!
//! This module implements FCP-parity magnetic timeline behaviour.
//! All operations return a `MagneticResult` containing an `EditCommand`
//! that can be pushed to the undo stack.

use crate::sequence::AnchorAlignment;
use klipp_core::{ClipId, RationalTime};

// ═══════════════════════════════════════════════════════════════════
// Edit Mode
// ═══════════════════════════════════════════════════════════════════

/// Insert vs Overwrite mode for timeline edits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EditMode {
    /// Placing a clip ripples everything downstream (magnetic default).
    #[default]
    Insert,
    /// Placing a clip replaces whatever is underneath, no ripple.
    Overwrite,
}

// ═══════════════════════════════════════════════════════════════════
// Supporting Types
// ═══════════════════════════════════════════════════════════════════

/// Result of a magnetic edit operation — contains the undoable command.
pub struct MagneticResult {
    pub command: crate::magnetic_commands::MagneticEditCommand,
}

/// Record of a clip that was rippled (moved) as a side effect of an edit.
#[derive(Debug, Clone)]
pub struct RippleRecord {
    pub clip_id: ClipId,
    pub old_in: RationalTime,
    pub old_out: RationalTime,
    pub new_in: RationalTime,
    pub new_out: RationalTime,
}

/// Record of a connected clip that was cascaded (followed its anchor).
#[derive(Debug, Clone)]
pub struct CascadeRecord {
    pub clip_id: ClipId,
    pub track_index: usize,
    pub old_in: RationalTime,
    pub old_out: RationalTime,
    pub new_in: RationalTime,
    pub new_out: RationalTime,
    pub collision: Option<CollisionInfo>,
}

/// Information about a clip overlap/collision on a connected track.
#[derive(Debug, Clone)]
pub struct CollisionInfo {
    pub colliding_clip_id: ClipId,
    pub overlap_start: RationalTime,
    pub overlap_end: RationalTime,
    pub resolution: CollisionResolution,
}

/// How to handle a collision between overlapping clips.
#[derive(Debug, Clone, PartialEq)]
pub enum CollisionResolution {
    /// Show amber warning indicator, allow overlap temporarily.
    Warn,
    /// Push colliding clip forward automatically.
    Push,
}

/// A snap target during drag operations.
#[derive(Debug, Clone)]
pub struct SnapPoint {
    pub time: RationalTime,
    pub kind: SnapKind,
}

/// What the snap target represents.
#[derive(Debug, Clone)]
pub enum SnapKind {
    ClipBoundary,
    Marker,
    Playhead,
    /// Snap back to where clip started (original position).
    OriginalPosition,
}

/// Auto-connect/disconnect thresholds.
pub const AUTO_CONNECT_TRACK_DISTANCE: f32 = 1.5;
pub const AUTO_DISCONNECT_TRACK_DISTANCE: f32 = 3.0;
pub const AUTO_CONNECT_SNAP_FRAMES: i64 = 12;

// ═══════════════════════════════════════════════════════════════════
// MagneticOp — operations dispatched from the UI to MagneticEditor
// ═══════════════════════════════════════════════════════════════════

/// Operations for the magnetic timeline system.
/// Each variant maps to a MagneticEditor method.
#[derive(Debug, Clone)]
pub enum MagneticOp {
    InsertPrimary {
        clip: crate::Clip,
        at_time: RationalTime,
    },
    OverwritePrimary {
        clip: crate::Clip,
        at_time: RationalTime,
    },
    RippleDelete {
        clip_id: ClipId,
    },
    Lift {
        clip_id: ClipId,
    },
    AppendPrimary {
        clip: crate::Clip,
    },
    MovePrimary {
        clip_id: ClipId,
        to_time: RationalTime,
    },
    InsertGap {
        duration: RationalTime,
        at_time: RationalTime,
    },
    Blade {
        clip_id: ClipId,
        at_time: RationalTime,
    },
    TrimHead {
        clip_id: ClipId,
        new_in: RationalTime,
    },
    TrimTail {
        clip_id: ClipId,
        new_out: RationalTime,
    },
    ConnectClip {
        clip_id: ClipId,
        anchor_id: ClipId,
        alignment: AnchorAlignment,
    },
    DisconnectClip {
        clip_id: ClipId,
    },
    PromoteToStoryline {
        track_index: usize,
        anchor_clip_id: ClipId,
    },
    BreakApartStoryline {
        track_index: usize,
    },
    ResolveCollisionPush {
        clip_id: ClipId,
        colliding_id: ClipId,
    },
}
