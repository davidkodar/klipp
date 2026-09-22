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
//!   See the [`time`] module for the [`RationalTime::ZERO`] gotcha
//!   (rate = 1; never use as the LHS of arithmetic against a higher-rate
//!   time, the result rescales down to whole seconds).
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

pub mod build_info;
pub mod color_grade;
pub mod color_ids;
pub mod colorspace;
pub mod config;
pub mod events;
pub mod generator;
pub mod gpu_vendor;
pub mod keyframe;
pub mod title_effect_keys;
pub mod title_motion;
pub mod export_preset;
pub mod id;
pub mod media_extensions;
pub mod modifier;
pub mod modifier_math;
pub mod ofx_value;
pub mod snap;
pub mod shot;
pub mod logging;
pub mod field;
pub mod smart_bin;
pub mod sync;
pub mod media_marker;
pub mod span;
pub mod startup_timer;
pub mod time;
pub mod transition_registry;
pub mod types;
pub mod working_colorspace;

pub use color_grade::{ColorMatchSettings, ColorWarperParams, CurveSet, DifferenceKeyMatchSpace, DifferenceKeySettings, FilmGrainSettings, HueSatCurves, LayerMixerInput, LumaKeySettings, Mask, MaskKeyframes, MaskShape, MaskStack, QualifierSettings, SoftClipSettings};
pub use colorspace::{ColorMatrix, ColorPrimaries, ColorRange, ColorTransfer, SourceColorspace};
pub use keyframe::{Interpolation, Keyframe, TitlePositionAxis};
pub use events::{AppCommand, AppEvent, AudioParam, ChapterSnapshot, ControlShared, ExportStatus, ProjectStatus, DEFAULT_MASTERING_PEAK_NITS, EncoderPreset, ExportAudioCodec, ExportFormat, ExportSettings, ExportVideoCodec, HdrMode, HdrMonitor, MASTERING_PEAK_CHOICES, MASTERING_PEAK_RANGE, MarkerColor, MarkerKind, MediaLoadedPayload, OdtChoice, ParamChange, RateControl, SdrTransfer, ToneMapOp, Tool, TrackTarget, VideoParam, ViewerMode};
pub use export_preset::{ExportPreset, ExportPresetSettings, PresetId, ProxyResolution, builtin_presets};
pub use id::{BinId, ClipId, GroupId, MarkerId, MediaId, SequenceId, TrackId, TransitionId};
pub use time::{Fps, RationalTime, TimeRange, Timecode, TimecodeDisplay};
pub use modifier::{EvalContext, Modifier, ModifierBlend, ModifierStack, ParamId, Waveform, evaluate_param};
pub use snap::{GridInterval, SnapConfig, SnapTargetKind};
pub use ofx_value::OfxValue;
pub use types::{AudioEffectInstance, BlendMode, ChannelConfig, ClipRole, ColorSpace, FadeCurve, FocalRef, FourPointGradient, GeneratorData, LogFormat, PixelFormat, Resolution, TimerFormat, TimerMode, TrackPoint, TransitionAlignment, TransitionUserParams};
pub use transition_registry::{TransitionKind, TransitionSpec, TRANSITION_SPECS, spec_for_kind, spec_for_kind_id, spec_for_string_key};
pub use config::{
    ChannelMask, GpuPreference, HistogramDisplayMode, HistogramLevels, HistogramSettings,
    LoudnessStandard, MeteringBallistics, PanLaw, PlaybackQuality, ScopeSettings,
    ScopeQuality, VectorscopeSettings, WaveformParadeSettings,
};
pub use gpu_vendor::GpuVendor;
pub use working_colorspace::WorkingColorspace;
pub use shot::ShotMetadata;
pub use logging::ClipLog;
pub use field::Field;
pub use smart_bin::{RuleOp, SmartBin, SmartRule};
pub use sync::{timecode_to_secs, SyncMethod, SyncedAudio};
pub use media_marker::{MediaMarker, MediaMarkerKind};
pub use span::ClipSpan;
