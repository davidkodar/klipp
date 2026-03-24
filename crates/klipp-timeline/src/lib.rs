mod clip_kind;
pub mod magnetic;
pub mod magnetic_commands;
mod marker;
mod sequence;

pub use clip_kind::{
    ClipKind, TextAlignment, TitleBackground, TitleData, TitlePosition, TitleShadow,
};
pub use marker::Marker;
pub use sequence::{
    AnchorAlignment, Clip, MagneticAnchor, Sequence, Track, TrackKind,
    TrackRole, Transition, TransitionKind,
};
