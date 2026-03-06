/// klipp-core
///
/// The foundation crate. Every other klipp crate depends on this.
/// Contains only types, errors, and events — no logic, no I/O, no UI.
///
/// Dependency rule: this crate must NEVER depend on any other klipp-* crate.

pub mod error;
pub mod id;
pub mod time;
pub mod types;
pub mod events;

pub use error::{KlippError, Result};
pub use id::{ClipId, TrackId, SequenceId, NodeId, MediaId, MarkerId};
pub use time::{Timecode, TimeRange, Fps};
