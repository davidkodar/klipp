//! [`BackgroundJob`] trait + supporting enums.
//!
//! A job represents a unit of background work the scheduler can
//! pause / resume / cancel. Implementations own their own thread or
//! subprocess; the trait surface is purely about control + observation
//! (`pause`, `resume`, `cancel`, `progress`, `is_done`).

use std::fmt;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct JobId(pub Uuid);

impl JobId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for JobId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Relative scheduling priority. The scheduler runs higher-priority
/// jobs ahead of lower ones and pauses lower ones first under load.
///
/// `UserInteractive` is reserved for foreground work that masquerades
/// as a job (e.g. preflight before a play that has to finish before
/// playback can start). Background tasks should use `Default` or
/// lower.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum JobPriority {
    /// Lowest — only runs when CPU/GPU is otherwise idle.
    Idle,
    /// Background — competes with other Background jobs FIFO,
    /// pauses on user activity. Default for filmstrip / skim
    /// transcode / audio conform.
    Background,
    /// Default — runs alongside Background but never pauses for
    /// user activity. For things the user explicitly asked for and
    /// is waiting on (export, manual proxy gen).
    Default,
    /// Highest — used by the scheduler to mark "must finish before
    /// foreground continues" preflight jobs. Engine code should
    /// almost never reach for this.
    UserInteractive,
}

/// What kind of system resource the job is contending for. The
/// scheduler can use this to avoid running, e.g. two NVDEC-heavy
/// jobs concurrently. Used for dispatch hints; not enforcement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceClass {
    /// NVDEC video decode — h264_cuvid, hevc_cuvid, av1_cuvid.
    Nvdec,
    /// NVENC video encode — h264_nvenc, hevc_nvenc.
    Nvenc,
    /// Mixed GPU (decode + encode + scaler chain).
    GpuMixed,
    /// CPU-only work — audio decode, image scaling fallback.
    Cpu,
    /// Disk I/O dominant — proxy reads, cache writes.
    Disk,
}

/// Outcome of running a single chunk of work via a job's internal
/// worker. Most jobs own their own thread / subprocess and don't
/// surface chunks directly; the enum exists for mock + test jobs and
/// for future cooperative-scheduler use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkResult {
    /// Chunk done; more pending.
    Yielded,
    /// Job finished successfully.
    Completed,
    /// Job aborted with an error.
    Failed(String),
    /// Wants resources currently in use elsewhere; retry later.
    NeedsResources,
}

/// The control surface a job exposes to the scheduler.
///
/// Implementations must be `Send + Sync` because the scheduler reads
/// state from a monitor thread while the job's own worker runs the
/// actual work.
pub trait BackgroundJob: Send + Sync {
    fn id(&self) -> JobId;
    fn name(&self) -> String;
    fn priority(&self) -> JobPriority;
    fn resource_class(&self) -> ResourceClass;

    /// 0.0 = not started, 1.0 = complete. Reported best-effort —
    /// some jobs may only know "running" / "done".
    fn progress(&self) -> f32;

    /// `true` once the job has terminated (either success or failure
    /// or cancellation). Scheduler removes done jobs from its list.
    fn is_done(&self) -> bool;

    /// Pause execution. For thread workers: signal the PauseToken.
    /// For subprocess: send SIGSTOP. Idempotent.
    fn pause(&self);

    /// Resume from pause. Idempotent.
    fn resume(&self);

    /// Permanently abort. Implementations should drop child processes,
    /// signal cancellation tokens, and ensure `is_done` becomes true.
    fn cancel(&self);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::Arc;

    /// Mock job for testing the trait surface.
    struct MockJob {
        id: JobId,
        name: String,
        progress: Arc<AtomicU32>, // out of 100
        done: Arc<AtomicBool>,
        paused: Arc<AtomicBool>,
        cancelled: Arc<AtomicBool>,
    }

    impl MockJob {
        fn new(name: &str) -> Self {
            Self {
                id: JobId::new(),
                name: name.into(),
                progress: Arc::new(AtomicU32::new(0)),
                done: Arc::new(AtomicBool::new(false)),
                paused: Arc::new(AtomicBool::new(false)),
                cancelled: Arc::new(AtomicBool::new(false)),
            }
        }
    }

    impl BackgroundJob for MockJob {
        fn id(&self) -> JobId { self.id }
        fn name(&self) -> String { self.name.clone() }
        fn priority(&self) -> JobPriority { JobPriority::Background }
        fn resource_class(&self) -> ResourceClass { ResourceClass::Cpu }
        fn progress(&self) -> f32 { self.progress.load(Ordering::Relaxed) as f32 / 100.0 }
        fn is_done(&self) -> bool { self.done.load(Ordering::Relaxed) }
        fn pause(&self) { self.paused.store(true, Ordering::Relaxed); }
        fn resume(&self) { self.paused.store(false, Ordering::Relaxed); }
        fn cancel(&self) {
            self.cancelled.store(true, Ordering::Relaxed);
            self.done.store(true, Ordering::Relaxed);
        }
    }

    #[test]
    fn job_id_unique_per_construction() {
        let a = JobId::new();
        let b = JobId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn priority_ordering_matches_intent() {
        // Idle < Background < Default < UserInteractive
        assert!(JobPriority::Idle < JobPriority::Background);
        assert!(JobPriority::Background < JobPriority::Default);
        assert!(JobPriority::Default < JobPriority::UserInteractive);
    }

    #[test]
    fn mock_job_lifecycle() {
        let job = MockJob::new("test");
        assert!(!job.is_done());
        assert_eq!(job.progress(), 0.0);
        assert_eq!(job.priority(), JobPriority::Background);
        assert_eq!(job.resource_class(), ResourceClass::Cpu);

        job.pause();
        assert!(job.paused.load(Ordering::Relaxed));
        job.resume();
        assert!(!job.paused.load(Ordering::Relaxed));

        job.cancel();
        assert!(job.is_done());
        assert!(job.cancelled.load(Ordering::Relaxed));
    }
}
