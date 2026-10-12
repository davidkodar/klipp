//! Background job scheduling primitives — built so foreground playback
//! always wins for CPU/GPU/disk while skim transcodes, filmstrip
//! generation, audio conform, and future heavy tasks (auto-stabilize,
//! scene detect, ML upscale) cooperatively yield.
//!
//! Three layers:
//!   * [`ForegroundActivity`] — single shared atomic state describing
//!     whether the user is busy. Anyone can read; engine + UI write.
//!   * [`PauseToken`] — condvar-backed gate threads check between
//!     work chunks. Wakes instantly on resume, blocks cheaply when
//!     paused (no busy-spin).
//!   * [`BackgroundJob`] trait + [`JobScheduler`] — orchestrates a
//!     pool of jobs, decides who runs vs pauses based on policy +
//!     foreground signal.
//!
//! The goal is one place to express "if the user is doing anything,
//! the background gets out of the way" — every future background task
//! gets that for free by implementing the trait.

#![forbid(unsafe_op_in_unsafe_fn)]

mod child;
mod foreground;
mod job;
mod pause;
mod scheduler;
mod slots;
mod subprocess;
mod thread_job;

pub use child::ends_with_app;
pub use foreground::ForegroundActivity;
pub use job::{BackgroundJob, ChunkResult, JobId, JobPriority, ResourceClass};
pub use pause::PauseToken;
pub use scheduler::{JobScheduler, SchedulerPolicy};
pub use slots::{SlotGuard, Slots};
pub use subprocess::SubprocessJob;
pub use thread_job::{lower_current_thread_priority, ThreadJob};
