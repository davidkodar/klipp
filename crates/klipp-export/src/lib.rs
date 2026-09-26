//! Export-queue model: background job tracking for render/export.
//!
//! Where [`klipp_render::ExportJob`] runs one export end-to-end on its
//! own thread, this crate sits one layer above: it owns the list of
//! jobs the user has added, their statuses ([`RenderJobStatus`]), and
//! dispatches them to a bounded pool of worker threads. The UI binds
//! directly to [`RenderQueue`] to show the render-queue window and to
//! cancel / reprioritise jobs.
//!
//! # Events
//!
//! [`RenderQueueEvent`] fires on status transitions (queued →
//! running → completed / failed / cancelled). The UI listens on the
//! event channel to update its panel without polling.
//!
//! # Concurrency
//!
//! `max_parallel` defaults to 2. Real-world limit on consumer NVIDIA
//! cards is typically 2–3 concurrent NVENC sessions. A job that finds no
//! session free falls back to the software encoder for its codec (#381)
//! and says so in its hardware line, so running more jobs than the card
//! has sessions is slower, not a failure.

pub mod queue;

pub use queue::{RenderJob, RenderJobStatus, RenderQueue, RenderQueueEvent, RenderSnapshot};
