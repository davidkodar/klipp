mod clip_kind;
pub mod flatten;
pub mod magnetic;
pub mod magnetic_commands;
mod marker;
mod sequence;

pub use clip_kind::{
    adjust_style_runs_for_text_change, apply_style_to_range, CharAnimation, CharEntrance,
    ClipKind, FontStyle, InnerShadow, MotionBlur, StyleRun, TextAlignment, TextExtrusion,
    TextGlow, TextGradient, TextOutline, TextPattern, TextPath, TitleBackground, TitleData,
    TitlePosition, TitleShadow, GeneratorData,
};
pub use marker::Marker;
pub use sequence::{
    AnchorAlignment, ChannelConfig, Clip, ClipMarker, ClipRole, FadeCurve, GradeVersion,
    MagneticAnchor, Sequence, Track, TrackKind, TrackRole, Transition, TransitionKind,
};
