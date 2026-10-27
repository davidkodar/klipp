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
mod clip_anim;
mod picture_fields;
mod picture_store;
mod picture_ops;
mod picture_split;
pub use picture_ops::{take_picture_refusal, takes_picture_ops, EffectEdit, OfxEdit, PictureEdit, PICTURE_WRITES_REFUSED};
pub use klipp_graph_lift::registry_catalog::RegistryCatalog;
#[cfg(test)]
mod picture_cache_tests;
pub use clip_anim::{EffectKeyframes, Keyframes, ParamModifiers};
pub use track_automation::{AutomationMode, TrackAutomation, TrackParam};
pub use picture_fields::ClipPictureParts;
pub use picture_store::{ClipPictureFields, LiftedPicture, COLD_LIFTS};
mod edit_plan;
mod review;
mod effect_slots;
mod other_effect_slots;
mod grade_access;
mod grade_roots;
mod node_labels;
pub use node_labels::{carry_node_labels, grade_group_id, grade_node_labels, remap_grade_node_labels, with_grade_node_labels, GradeNodeLabel, NodeLabel, NODE_LABEL_COLOURS};
pub use grade_roots::{grade_node_for_run, lowered_grade};
pub use grade_access::{Grade, GradeAccess, GradeSlot};
mod render_gates;
pub use effect_slots::{EffectId, EffectList, EffectsState};
pub use other_effect_slots::{OfxEffects, TrackEffectList};
mod picture_reads;
pub use picture_reads::{PictureEffect, PictureOfx};
mod clip_kind;
mod title_fields;
mod title_template_file;
pub mod title_library;
pub mod title_box;
mod credits;
pub mod caption_motion;
pub mod composition_template;
pub use caption_motion::{animated_caption, CaptionAnimation, CaptionMotion, CaptionPlace};
pub use klipp_core::text3d;
pub use klipp_core::text3d::{Bevel, BevelProfile, Camera3d, Light3d, Text3d, Text3dKeys};
pub use credits::{credit_rows, credits_title, parse_credits, CreditLine, CreditsLayout, CreditsStyle};
pub use title_template_file::{read_templates, save_template, template_dir, template_for_save, TemplateFile, TEMPLATE_EXTENSION, TEMPLATE_FORMAT};
pub use title_fields::{FieldKind, LayerAnimators, LayerSize, LayerTiming, DURATION_RANGE as TITLE_DURATION_RANGE, PublishedField, Timing, SIZE_RANGE as TITLE_SIZE_RANGE, SPEED_RANGE as TITLE_SPEED_RANGE};
pub mod crossfade;
pub mod flatten;
pub mod frame_glide;
pub mod insert_edit;
pub mod magnetic;
pub mod magnetic_commands;
mod marker;
pub mod multicam;
pub mod picture_v1;
mod sequence;
pub mod shared;
pub mod surface;
/// Clips for tests, built through the picture (M4 step 12-0).
#[cfg(any(test, feature = "test-clip"))]
pub mod test_clip;
pub mod track_automation;
pub use surface::{PinFit, PinLoss, Surface, SurfaceKey, SurfacePin, TrackState};
pub use shared::{SequenceRead, SequenceRevision, SequenceWrite, SharedSequence};

pub use klipp_core::title_motion::{Animator, AnimatorProps, RangeSelector, SelectOrder, SelectShape, SelectUnit, UnitPose, CrawlDirection, CubicBezier, EdgeFade, Easing, LoopGeometry, LoopPose, Motion, MotionKind, ScrollDirection, StaggerUnit, TitleLoop, TitleMotion};
pub use klipp_core::layer_mask::LayerMaskKind;
pub use clip_kind::{
    adjust_style_runs_for_text_change, apply_style_to_range, default_text_transform,
    CharAnimation, CharEntrance, ClipKind, ColourKeys, FontAxis, FontStyle, InnerShadow, LastLineAlign, MotionBlur, ObjectTransform,
    Shape, ShapeFit, ShapeKind, ShapeStroke, StyleRun, TextAlignment, TextBox, TextCase, TextDirection,
    PathTextAlign, TextExtrusion, TextGlow, TextGradient, TextOutline, TextPattern, ListMarker, ListStyle, NumberStyle, Paragraph, Script, TabAlign, TabStop, TextPath, TextFit, TextVerticalAlign, TextureFill,
    TexturePresetId, TextureSource, TextureTileMode, TitleAnimationType,
    TitleAsset,
    LayerFx, LayerMask, LegacyTextFx, FxMigration,
    ContrastProtect, TitleBackground, TitleData, TitleObject, TitleObjectContent, TitleObjectId, TitlePosition,
    TitleShadow, GeneratorData, ImageFit, ImageLayer};
pub use brief::{Brief, CastMember, Feature, MotionAnchorSpec, ShadowSpec, TitleMotionSpec, TypeStyle};
pub use review::{FindingReview, ReviewDecision};
pub use edit_plan::{EditPlan, LayerMotion, PlanBackground, PlanLayer, PlanMusic, PlanPlacement, PlanSegment, PlanSfx, PlanTitle, PlanZoom, LAYER_MOTIONS};
pub use marker::{Marker, ReviewNote};
pub use klipp_core::MulticamSound;
pub use klipp_core::fill_gradient::{GradientKind, GradientStop, MAX_GRADIENT_STOPS};
pub use sequence::{
    effective_timeline_duration_for_keyframes, one_frame, speed_at_keyframes, speed_integral_keyframes,
    AnchorAlignment, BakedOwn, Camera, CaptionStyle, ChannelConfig, Clip, ClipLabel, ClipMarker, GradeFlag, ClipRole, FadeCurve, GradeVersion, LinkMode,
    FirstClipAutoSet, GradeGroup, GradeStage, Look, MagneticAnchor, MatteBinding, Sequence, VersionOf, StencilSettings, Track, TrackEffects, TrackPaste, TrackSettings, OwnEffects, TrackKind, TrackRole,
    Transition, TransitionKind, TransformLink, TransformLinkTarget, SPEED_MAX, SPEED_MIN, DEFAULT_TITLE_NITS, TITLE_NITS_RANGE,
};

pub mod vector_path;
pub use vector_path::{StrokeGlow, PathTrim, TrimRange, TrimKeyframe, VectorPath, PathPose, PathContour, PathVertex, PathKeyframe, ShapePreset};
