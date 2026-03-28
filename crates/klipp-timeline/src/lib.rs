mod clip_kind;
pub mod magnetic;
pub mod magnetic_commands;
mod marker;
mod sequence;

pub use clip_kind::{
    ClipKind, FontStyle, InnerShadow, TextAlignment, TextGlow, TextGradient, TextOutline,
    TextPattern, TitleBackground, TitleData, TitlePosition, TitleShadow,
};
pub use marker::Marker;
pub use sequence::{
    AnchorAlignment, ChannelConfig, Clip, ClipMarker, ClipRole, FadeCurve, MagneticAnchor,
    Sequence, Track, TrackKind, TrackRole, Transition, TransitionKind,
};
