mod clip_kind;
pub mod magnetic;
pub mod magnetic_commands;
mod marker;
mod sequence;

pub use clip_kind::{
    CharAnimation, CharEntrance, ClipKind, FontStyle, InnerShadow, MotionBlur, TextAlignment,
    TextExtrusion, TextGlow, TextGradient, TextOutline, TextPattern, TextPath, TitleBackground,
    TitleData, TitlePosition, TitleShadow,
};
pub use marker::Marker;
pub use sequence::{
    AnchorAlignment, ChannelConfig, Clip, ClipMarker, ClipRole, FadeCurve, MagneticAnchor,
    Sequence, Track, TrackKind, TrackRole, Transition, TransitionKind,
};
