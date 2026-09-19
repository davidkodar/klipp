//! Sequence / track / clip data model and the magnetic-timeline editor.
//!
//! This crate owns the editor's document model — everything that lives in
//! a `.klipp` project's in-memory shape — and the operations that mutate
//! it. It depends only on `klipp-core` (for IDs, time, and keyframes) and
//! is itself consumed by the engine, render, project, and UI crates.
//!
//! # Structure
//!
//! * [`Sequence`] — a top-level document containing an ordered set of
//!   tracks.
//! * [`Track`] — a lane of clips. [`TrackKind`] distinguishes Video /
//!   Audio / Text / Subtitle. [`TrackRole`] layers a magnetic-timeline
//!   role (Free / Primary / Connected / ConnectedStoryline) on top.
//! * [`Clip`] — a media reference with timing ([`ClipKind`] for media /
//!   title / generator / gap / nested sequence), fade/speed/effects state,
//!   and keyframe tracks. `Clip` is intentionally broad; field relevance
//!   varies by `ClipKind`.
//! * [`Transition`] — links two clips on the same track.
//! * [`Marker`] / [`ClipMarker`] — timeline-level and per-clip markers.
//!
//! # Magnetic timeline
//!
//! The [`magnetic`] module implements FCP-parity editing ops —
//! `MagneticEditor::insert_primary`, `overwrite_primary`, `lift`, `blade`,
//! `ripple_delete`, `trim_head`/`tail`, `promote_to_storyline`, …. Every
//! mutation returns a [`magnetic_commands::MagneticEditCommand`] carrying
//! the inverse, so the UI's undo stack can round-trip edits without
//! needing to snapshot the full sequence.
//!
//! The [`flatten`] module resolves nested-sequence clips
//! ([`ClipKind::Sequence`]) into a flat clip list for playback / render.
//!
//! # Invariants
//!
//! * Primary tracks must be gapless (enforced under
//!   `cfg(debug_assertions)` by `Sequence::assert_primary_gapless`).
//! * `Clip::timeline_in` and `Clip::timeline_out` must share the same
//!   rate.
//! * `MagneticAnchor::anchor_clip_id` must resolve to a live clip.
//! * There is a latent parallel edit path
//!   (`Track::blade_at` / `ripple_trim_left/right` / `ripple_remove`) that
//!   duplicates `MagneticEditor` logic and bypasses undo — prefer the
//!   magnetic entry points for any new call.

mod brief;
mod edit_plan;
mod clip_kind;
pub mod flatten;
pub mod insert_edit;
pub mod magnetic;
pub mod magnetic_commands;
mod marker;
mod sequence;

pub use klipp_core::title_motion::{CrawlDirection, Easing, LoopGeometry, LoopPose, Motion, MotionKind, ScrollDirection, StaggerUnit, TitleLoop, TitleMotion};
pub use clip_kind::{
    adjust_style_runs_for_text_change, apply_style_to_range, default_text_transform,
    CharAnimation, CharEntrance, ClipKind, FontStyle, InnerShadow, MotionBlur, ObjectTransform,
    Shape, ShapeKind, ShapeStroke, StyleRun, TextAlignment, TextBox, TextCase, TextDirection,
    TextExtrusion, TextGlow, TextGradient, TextOutline, TextPattern, TextPath, TextureFill,
    TexturePresetId, TextureSource, TextureTileMode, TitleAnimationType,
    TitleAsset,
    TitleBackground, TitleData, TitleObject, TitleObjectContent, TitleObjectId, TitlePosition,
    TitleShadow, GeneratorData, ImageFit, ImageLayer};
pub use brief::{Brief, CastMember, Feature, TypeStyle};
pub use edit_plan::{EditPlan, PlanMusic, PlanSegment, PlanSfx, PlanTitle, PlanZoom};
pub use marker::Marker;
pub use sequence::{
    effective_timeline_duration_for_keyframes, speed_at_keyframes, speed_integral_keyframes,
    AnchorAlignment, ChannelConfig, Clip, ClipMarker, ClipRole, FadeCurve, GradeVersion, LinkMode,
    Look, MagneticAnchor, MatteBinding, Sequence, StencilSettings, Track, TrackKind, TrackRole,
    Transition, TransitionKind, TransformLink, TransformLinkTarget, SPEED_MAX, SPEED_MIN,
};
