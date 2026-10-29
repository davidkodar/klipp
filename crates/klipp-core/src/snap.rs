//! Snap target classification and grid-snap interval selection.
//!
//! Two enums shared across the workspace so the UI, range tool, and
//! magnetic-edit layers can describe and configure snap targets in
//! consistent terms.
//!
//! `GridInterval` describes the user's choice of grid spacing
//! (persisted in preferences). Resolving the choice to a concrete
//! seconds value is left to the UI layer because `Adaptive` needs the
//! current ruler pixels-per-second.
//!
//! `SnapTargetKind` classifies what a snap candidate represents so the
//! UI can render a target-aware legend.

use serde::{Deserialize, Serialize};

/// Per-target snap configuration. `master` gates everything (matches
/// the pre-feature `snap_enabled: bool`); the per-target booleans add
/// granular control. Defaults preserve byte-identical behaviour for
/// existing users — every category that snapped before still does, and
/// `grid` defaults off so users who never opt in see no change.
///
/// Lives in klipp-core (rather than the UI layer) so that the
/// magnetic-edit layer in `klipp-timeline` can consult it without
/// pulling a UI dependency. UI persists the struct in `layout.json`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SnapConfig {
    pub master: bool,
    pub clips: bool,
    pub playhead: bool,
    pub markers: bool,
    pub in_out: bool,
    pub grid: bool,
}

impl Default for SnapConfig {
    fn default() -> Self {
        Self {
            master: true,
            clips: true,
            playhead: true,
            markers: true,
            in_out: true,
            grid: false,
        }
    }
}

impl SnapConfig {
    /// Per-target gate. Caller checks `master` separately — a
    /// master-off short-circuit happens before any kind-by-kind
    /// filtering in the snap-resolution functions.
    pub fn is_kind_enabled(&self, kind: SnapTargetKind) -> bool {
        match kind {
            SnapTargetKind::ClipEdge => self.clips,
            SnapTargetKind::Playhead => self.playhead,
            SnapTargetKind::Marker => self.markers,
            SnapTargetKind::SequenceStart | SnapTargetKind::InPoint | SnapTargetKind::OutPoint => self.in_out,
            SnapTargetKind::Grid => self.grid,
        }
    }
}

/// User-facing grid-snap interval choice. Stored in preferences.
///
/// Resolving to seconds is the UI layer's job: `Adaptive` consults the
/// ruler's tick density at the current zoom, `Frame` derives from the
/// active sequence fps. Other variants resolve to fixed seconds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[derive(Default)]
pub enum GridInterval {
    /// Grid snap disabled. Default — preserves byte-identical behaviour
    /// for users who never toggle grid snap on.
    #[default]
    Off,
    /// Snap to whatever interval the ruler is currently displaying as
    /// major ticks at the current zoom level. "Snap to what I see."
    Adaptive,
    /// Snap to `1.0 / fps_rate` — the smallest meaningful timeline
    /// increment for frame-accurate edits.
    Frame,
    /// Snap every 0.5 s.
    HalfSecond,
    /// Snap every 1 s.
    Second,
    /// Snap every 2 s.
    TwoSeconds,
    /// Snap every 5 s.
    FiveSeconds,
    /// Snap every `secs` seconds. Caller responsible for clamping to a
    /// sensible range (e.g. > 0).
    Custom(f64),
}


impl GridInterval {
    /// Short label for menu / dropdown rendering.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Adaptive => "Ruler ticks",
            Self::Frame => "Frames",
            Self::HalfSecond => "0.5 s",
            Self::Second => "1 s",
            Self::TwoSeconds => "2 s",
            Self::FiveSeconds => "5 s",
            Self::Custom(_) => "Custom",
        }
    }

    /// Fixed-seconds variants resolve here. `Off`, `Adaptive`, `Frame`,
    /// and `Custom` all return `None` — those need additional context
    /// (fps, pps, or user value) supplied by the UI layer's resolver.
    pub fn fixed_secs(&self) -> Option<f64> {
        match self {
            Self::HalfSecond => Some(0.5),
            Self::Second => Some(1.0),
            Self::TwoSeconds => Some(2.0),
            Self::FiveSeconds => Some(5.0),
            _ => None,
        }
    }
}

/// What a snap candidate represents. Surfaced in the on-snap legend
/// (commit 3) so the user sees *what* they snapped to, not just *that*
/// they snapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapTargetKind {
    /// A clip's `timeline_in` or `timeline_out` on any track.
    ClipEdge,
    /// The current playhead.
    Playhead,
    /// Sequence start (t = 0).
    SequenceStart,
    /// The sequence's in-point marker.
    InPoint,
    /// The sequence's out-point marker.
    OutPoint,
    /// A user-placed marker (start or end of a range marker).
    Marker,
    /// A grid tick at the configured interval.
    Grid,
}

impl SnapTargetKind {
    /// Short user-facing label for the on-snap legend.
    pub fn label(&self) -> &'static str {
        match self {
            Self::ClipEdge => "Clip edge",
            Self::Playhead => "Playhead",
            Self::SequenceStart => "Start",
            Self::InPoint => "In point",
            Self::OutPoint => "Out point",
            Self::Marker => "Marker",
            Self::Grid => "Grid",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_interval_default_is_off() {
        assert_eq!(GridInterval::default(), GridInterval::Off);
    }

    #[test]
    fn fixed_secs_returns_none_for_context_dependent_variants() {
        assert_eq!(GridInterval::Off.fixed_secs(), None);
        assert_eq!(GridInterval::Adaptive.fixed_secs(), None);
        assert_eq!(GridInterval::Frame.fixed_secs(), None);
        assert_eq!(GridInterval::Custom(0.25).fixed_secs(), None);
    }

    #[test]
    fn fixed_secs_returns_concrete_value_for_fixed_variants() {
        assert_eq!(GridInterval::HalfSecond.fixed_secs(), Some(0.5));
        assert_eq!(GridInterval::Second.fixed_secs(), Some(1.0));
        assert_eq!(GridInterval::TwoSeconds.fixed_secs(), Some(2.0));
        assert_eq!(GridInterval::FiveSeconds.fixed_secs(), Some(5.0));
    }

    // ── SnapConfig ──

    // Pinned: defaults match the pre-feature behaviour exactly. Master
    // on, every per-target except Grid on. If a future refactor flips
    // any of these, the back-compat invariant breaks.
    #[test]
    fn snap_config_defaults_preserve_prior_behaviour() {
        let cfg = SnapConfig::default();
        assert!(cfg.master);
        assert!(cfg.clips);
        assert!(cfg.playhead);
        assert!(cfg.markers);
        assert!(cfg.in_out);
        assert!(!cfg.grid, "grid must default off — existing users opt in");
    }

    #[test]
    fn snap_config_is_kind_enabled_routes_each_variant() {
        let cfg = SnapConfig {
            master: true,
            clips: false,
            playhead: true,
            markers: false,
            in_out: true,
            grid: false,
        };
        assert!(!cfg.is_kind_enabled(SnapTargetKind::ClipEdge));
        assert!(cfg.is_kind_enabled(SnapTargetKind::Playhead));
        assert!(!cfg.is_kind_enabled(SnapTargetKind::Marker));
        // SequenceStart / InPoint / OutPoint share the in_out flag.
        assert!(cfg.is_kind_enabled(SnapTargetKind::SequenceStart));
        assert!(cfg.is_kind_enabled(SnapTargetKind::InPoint));
        assert!(cfg.is_kind_enabled(SnapTargetKind::OutPoint));
        assert!(!cfg.is_kind_enabled(SnapTargetKind::Grid));
    }

    #[test]
    fn snap_target_kind_label_distinguishes_all_variants() {
        let kinds = [
            SnapTargetKind::ClipEdge,
            SnapTargetKind::Playhead,
            SnapTargetKind::SequenceStart,
            SnapTargetKind::InPoint,
            SnapTargetKind::OutPoint,
            SnapTargetKind::Marker,
            SnapTargetKind::Grid,
        ];
        let labels: std::collections::HashSet<_> = kinds.iter().map(|k| k.label()).collect();
        assert_eq!(labels.len(), kinds.len(), "labels must be unique");
    }
}
