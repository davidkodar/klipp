//! Foundation types shared across the workspace.
//!
//! `klipp-core` is the bottom layer: every other crate depends on it and it
//! depends on nothing Klipp-specific. It owns the vocabulary of the editor
//! — IDs, timing primitives, event/command enums, export settings, and the
//! transition registry — without knowing anything about playback, render,
//! UI, or plugins.
//!
//! # Key types
//!
//! * **Identity** — [`ClipId`], [`MediaId`], [`BinId`], [`TrackId`],
//!   [`SequenceId`], [`MarkerId`], [`TransitionId`], [`GroupId`]. All are
//!   UUID-backed newtypes; copy-cheap, hash-cheap, compare-cheap.
//! * **Timing** — [`RationalTime`], [`Timecode`], [`Fps`], [`TimeRange`].
//!   [`RationalTime::ZERO`] has rate 1 and takes the other side's rate
//!   in arithmetic, so a sum or difference with it is exact.
//! * **Events** — [`AppCommand`] (UI → engine), [`AppEvent`] (engine → UI),
//!   [`ExportSettings`] and its sibling enums for the render pipeline.
//! * **Transitions** — [`TransitionKind`] + the compile-time
//!   [`TRANSITION_SPECS`] registry. [`spec_for_kind`] falls back to
//!   `CrossDissolve` if a variant goes unregistered, logged loudly.
//!
//! # Conventions
//!
//! * No heap allocation in hot primitives. `RationalTime` is 16 bytes;
//!   `Fps`, `Resolution`, `Timecode` stay on the stack.
//! * No I/O, no threads, no async. The crate is pure data.
//! * Serde-serialised types round-trip through the `.klipp` project format
//!   via `klipp-project`; treat any breaking rename as a migration.

pub mod frame_glide;
pub mod assets;
pub mod assistants_seen;
pub mod automation;
pub mod build_info;
pub mod color_grade;
pub mod delivery_spec;
pub mod color_ids;
pub mod colorspace;
pub mod config;
pub mod events;
pub mod generator;
pub mod gpu_vendor;
pub mod keyframe;
pub mod mask_path;
pub mod grade_flag;
pub use grade_flag::GradeFlag;
pub mod layer_mask;
pub mod title_effect_keys;
pub mod title_motion;
pub mod text3d;
pub mod title_material;
pub mod export_preset;
pub mod id;
pub mod media_extensions;
pub mod modifier;
pub mod modifier_math;
pub mod ofx_value;
pub mod snap;
pub mod shot;
pub mod logging;
pub mod normalize;
pub mod field;
pub mod fill_gradient;
pub mod smart_bin;
pub mod sync;
pub mod media_marker;
pub mod span;
pub mod startup_timer;
pub mod piece_transition;
pub mod time;
pub mod transition_registry;
pub mod types;
pub mod working_colorspace;

pub use color_grade::{ColorMatchSettings, ColorWarperParams, CurveSet, DifferenceKeyMatchSpace, DifferenceKeySettings, FilmGrainSettings, HdrZonesSettings, HueSatCurves, LayerMixerInput, LumaKeySettings, Mask, MaskCombine, MaskKeyframes, MaskShape, MaskStack, soft_edge, CornerKey, PerspectiveTrack, PlaneMap, QualifierMode, QualifierSettings, SoftClipSettings, WindowMotion};
pub use colorspace::{ColorMatrix, ColorPrimaries, ColorRange, ColorTransfer, SourceColorspace};
pub use keyframe::{Handles, Interpolation, KeyEase, Keyframe, TitlePositionAxis, TitleTransformProperty};
pub use events::{AppCommand, AppEvent, SubjectMatteUse, AudioParam, TitleFieldValue, CameraParam, ChapterSnapshot, ControlShared, DEFAULT_TRUE_PEAK_DBTP, CompositeFallback, ExportStatus, ExportTimings, ProjectStatus, ProxyStatus, RecentProjectSummary, SequenceSummary, VersionSummary, DeliveryOutput, TransportStatus, launch_identity_base, DEFAULT_MASTERING_PEAK_NITS, EncoderPreset, ExportAudioCodec, ExportFormat, ExportSettings, ExportVideoCodec, HdrMode, HdrMonitor, MASTERING_PEAK_CHOICES, MASTERING_PEAK_RANGE, MarkerColor, MarkerKind, ReviewStatus, MediaLoadedPayload, NodeSampleRefusal, OdtChoice, ParamChange, ProjectColour, RateControl, ScopeLut, SdrTransfer, SelectRange, ToneMapOp, Tool, TrackTarget, VideoParam, ViewerMode, ViewerOverlay, WipeClip};
pub use export_preset::{ExportPreset, ExportPresetSettings, PresetId, ProxyResolution, builtin_presets, DELIVERY_TEMPLATES};
pub use id::{BinId, ClipId, GradeGroupId, GroupId, MarkerId, MediaId, SequenceId, TrackId, TransitionId};
pub use time::{Fps, RationalTime, TimeRange, Timecode, TimecodeDisplay};
pub use modifier::{EvalContext, Extrapolation, Modifier, ModifierBlend, ModifierStack, ParamId, Waveform, evaluate_param};
pub use snap::{GridInterval, SnapConfig, SnapTargetKind};
pub use ofx_value::OfxValue;
pub use types::{AudioEffectInstance, BlendMode, ChannelConfig, ClipRole, ColorSpace, DEFAULT_GENERATOR_GRAIN, RoleProposal, RoleSource, FadeCurve, FocalRef, FourPointGradient, GeneratorData, LogFormat, PixelFormat, Resolution, TimerFormat, TimerMode, TrackPoint, TransitionAlignment, TransitionUserParams, ViewChannel};
pub use transition_registry::{TransitionKind, TransitionSpec, TRANSITION_SPECS, spec_for_kind, spec_for_kind_id, spec_for_string_key};
pub use config::{
    ChannelMask, GpuPreference, HistogramDisplayMode, HistogramLevels, HistogramSettings,
    LoudnessStandard, MeteringBallistics, PanLaw, PlaybackQuality, ScopeSettings,
    ScopeQuality, VectorscopeSettings, WaveformParadeSettings,
};
pub use gpu_vendor::GpuVendor;
pub use working_colorspace::{grade_encoding, set_grade_encoding, GradeEncoding, WorkingColorspace};
pub use shot::ShotMetadata;
pub use logging::ClipLog;
pub use field::Field;
pub use smart_bin::{RuleOp, SmartBin, SmartRule};
pub use sync::{plan_multicam, timecode_to_secs, AnglePlacement, AudioMatch, MulticamAngle, MulticamPlan, MulticamSound, MulticamSync, SyncMethod, SyncedAudio};
pub use media_marker::{MediaMarker, MediaMarkerKind};
pub use span::ClipSpan;

pub mod vector_path;
pub mod template;
pub use template::{LinkCurve, TemplateInstance, TemplateLink, TemplateValue};
