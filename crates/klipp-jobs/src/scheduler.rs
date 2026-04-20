//! Background-job scheduler. Watches [`ForegroundActivity`] and
//! pauses/resumes [`BackgroundJob`]s based on the active
//! [`SchedulerPolicy`].
//!
//! The scheduler does NOT run job work itself — each job owns its
//! own worker thread (via `ThreadJob`) or subprocess (via
//! `SubprocessJob`). The scheduler's job is the *control plane*:
//! decide when each job should be paused or resumed, and surface
//! the live job list for UI.
//!
//! A single monitor thread polls the foreground signal at
//! [`MONITOR_INTERVAL`]. When the user becomes busy and policy is
//! `Defer`, all background jobs get `pause()`. When idle, all get
//! `resume()`. The transitions are debounced so a momentary mouse
//! twitch doesn't churn pause/resume.
//!
//! Jobs are dropped from the scheduler's list once they report
//! `is_done`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::foreground::ForegroundActivity;
use crate::job::{BackgroundJob, JobPriority};

/// How aggressively background jobs yield to foreground activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerPolicy {
    /// Pause all background jobs as soon as the user is busy.
    /// Default for the "I want the editor to feel responsive" mode.
    Defer,
    /// Background jobs keep running but the scheduler may slot in
    /// short pauses every few hundred ms. Reserved for a future
    /// "balanced" mode — currently treated identical to `Defer`.
    Balanced,
    /// Never pause background jobs for foreground activity. The user
    /// chose throughput over responsiveness.
    MaxThroughput,
}

/// Polling interval for the monitor thread. 50 ms is small enough
/// that a play/pause click feels instant (well under the 100 ms
/// human-perception threshold) and large enough not to burn CPU on
/// scheduler bookkeeping.
const MONITOR_INTERVAL: Duration = Duration::from_millis(50);

/// How long the user has to be idle (no input AND not playing) before
/// background jobs may resume after being paused. Prevents thrash
/// during a quick click.
const IDLE_RESUME_GRACE: Duration = Duration::from_millis(500);

struct SchedulerInner {
    foreground: Arc<ForegroundActivity>,
    policy: Mutex<SchedulerPolicy>,
    jobs: Mutex<Vec<Arc<dyn BackgroundJob>>>,
    paused_currently: AtomicBool,
    shutdown: AtomicBool,
}

pub struct JobScheduler {
    inner: Arc<SchedulerInner>,
    monitor_handle: Mutex<Option<JoinHandle<()>>>,
}

impl JobScheduler {
    pub fn new(foreground: Arc<ForegroundActivity>, policy: SchedulerPolicy) -> Arc<Self> {
        let inner = Arc::new(SchedulerInner {
            foreground,
            policy: Mutex::new(policy),
            jobs: Mutex::new(Vec::new()),
            paused_currently: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
        });

        let inner_w = Arc::clone(&inner);
        let monitor_handle = std::thread::Builder::new()
            .name("klipp-job-scheduler".into())
            .spawn(move || monitor_loop(inner_w))
            .expect("scheduler monitor thread must spawn");

        Arc::new(Self {
            inner,
            monitor_handle: Mutex::new(Some(monitor_handle)),
        })
    }

    /// Add a job to the scheduler. The job starts in its own
    /// thread/subprocess immediately when constructed; submitting
    /// here just enrolls it for pause/resume control.
    pub fn submit(&self, job: Arc<dyn BackgroundJob>) {
        let mut jobs = self.inner.jobs.lock().unwrap_or_else(|e| e.into_inner());
        // If the scheduler is currently in "paused" state (because
        // user is busy), pause the freshly-added job too so it
        // doesn't race ahead before the monitor catches it.
        if self.inner.paused_currently.load(Ordering::Acquire) {
            job.pause();
        }
        jobs.push(job);
    }

    pub fn set_policy(&self, policy: SchedulerPolicy) {
        let mut p = self.inner.policy.lock().unwrap_or_else(|e| e.into_inner());
        *p = policy;
    }

    pub fn policy(&self) -> SchedulerPolicy {
        *self.inner.policy.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Snapshot the current job list. Useful for the Background
    /// Tasks UI panel to render progress + cancel buttons.
    pub fn jobs(&self) -> Vec<Arc<dyn BackgroundJob>> {
        let jobs = self.inner.jobs.lock().unwrap_or_else(|e| e.into_inner());
        jobs.clone()
    }

    /// Cancel every running job. Idempotent.
    pub fn cancel_all(&self) {
        let jobs = self.inner.jobs.lock().unwrap_or_else(|e| e.into_inner());
        for j in jobs.iter() {
            j.cancel();
        }
    }

    /// Stop the monitor thread. Existing jobs keep running on their
    /// own — call `cancel_all` first if you want them to stop.
    pub fn shutdown(&self) {
        self.inner.shutdown.store(true, Ordering::Release);
        let h = {
            let mut slot = self.monitor_handle.lock().unwrap_or_else(|e| e.into_inner());
            slot.take()
        };
        if let Some(handle) = h {
            let _ = handle.join();
        }
    }
}

impl Drop for JobScheduler {
    fn drop(&mut self) {
        // Best-effort: don't keep the monitor running after the
        // scheduler is dropped.
        self.inner.shutdown.store(true, Ordering::Release);
    }
}

fn monitor_loop(inner: Arc<SchedulerInner>) {
    // Window for "user has been idle this long, safe to resume".
    let busy_window = Duration::from_secs(2);

    loop {
        if inner.shutdown.load(Ordering::Acquire) {
            return;
        }

        let policy = *inner.policy.lock().unwrap_or_else(|e| e.into_inner());

        let should_pause = match policy {
            SchedulerPolicy::MaxThroughput => false,
            SchedulerPolicy::Defer | SchedulerPolicy::Balanced => {
                inner.foreground.is_user_busy(busy_window)
            }
        };

        // For resume we require both: not busy now, AND idle for the
        // grace window. (`is_user_busy` already includes the busy
        // window; the additional grace prevents flap on click-click
        // sequences.)
        let should_resume = !should_pause
            && !inner.foreground.is_user_busy(IDLE_RESUME_GRACE);

        let was_paused = inner.paused_currently.load(Ordering::Acquire);
        let mut next_state = was_paused;
        if !was_paused && should_pause {
            next_state = true;
        } else if was_paused && should_resume {
            next_state = false;
        }

        if next_state != was_paused {
            inner.paused_currently.store(next_state, Ordering::Release);
            apply_pause_state(&inner, next_state);
        }

        // Reap completed jobs from the list so the UI doesn't keep
        // showing them and we don't leak Arcs forever.
        {
            let mut jobs = inner.jobs.lock().unwrap_or_else(|e| e.into_inner());
            jobs.retain(|j| !j.is_done());
        }

        std::thread::sleep(MONITOR_INTERVAL);
    }
}

fn apply_pause_state(inner: &SchedulerInner, paused: bool) {
    let jobs = inner.jobs.lock().unwrap_or_else(|e| e.into_inner());
    for j in jobs.iter() {
        // Don't pause UserInteractive — those are explicit foreground
        // preflight jobs.
        if j.priority() == JobPriority::UserInteractive { continue; }
        if paused {
            j.pause();
        } else {
            j.resume();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::time::Instant;

    use crate::job::{BackgroundJob, JobId, ResourceClass};

    /// Minimal mock job — counts pause/resume calls and tracks "done".
    struct MockJob {
        id: JobId,
        priority: JobPriority,
        pause_count: Arc<AtomicU32>,
        resume_count: Arc<AtomicU32>,
        cancel_count: Arc<AtomicU32>,
        done: Arc<AtomicBool>,
    }

    impl MockJob {
        fn new(priority: JobPriority) -> (Arc<Self>, Arc<AtomicU32>, Arc<AtomicU32>, Arc<AtomicBool>) {
            let pause_count = Arc::new(AtomicU32::new(0));
            let resume_count = Arc::new(AtomicU32::new(0));
            let done = Arc::new(AtomicBool::new(false));
            let job = Arc::new(MockJob {
                id: JobId::new(),
                priority,
                pause_count: Arc::clone(&pause_count),
                resume_count: Arc::clone(&resume_count),
                cancel_count: Arc::new(AtomicU32::new(0)),
                done: Arc::clone(&done),
            });
            (job, pause_count, resume_count, done)
        }
    }

    impl BackgroundJob for MockJob {
        fn id(&self) -> JobId { self.id }
        fn name(&self) -> String { format!("mock-{}", self.id) }
        fn priority(&self) -> JobPriority { self.priority }
        fn resource_class(&self) -> ResourceClass { ResourceClass::Cpu }
        fn progress(&self) -> f32 { 0.0 }
        fn is_done(&self) -> bool { self.done.load(Ordering::Acquire) }
        fn pause(&self) { self.pause_count.fetch_add(1, Ordering::Relaxed); }
        fn resume(&self) { self.resume_count.fetch_add(1, Ordering::Relaxed); }
        fn cancel(&self) {
            self.cancel_count.fetch_add(1, Ordering::Relaxed);
            self.done.store(true, Ordering::Release);
        }
    }

    fn wait_until<F: FnMut() -> bool>(mut cond: F, timeout: Duration) -> bool {
        let start = Instant::now();
        while !cond() {
            if start.elapsed() > timeout { return false; }
            std::thread::sleep(Duration::from_millis(10));
        }
        true
    }

    #[test]
    fn submit_and_reap() {
        let fg = ForegroundActivity::new();
        let sched = JobScheduler::new(Arc::clone(&fg), SchedulerPolicy::Defer);
        let (job, _, _, done) = MockJob::new(JobPriority::Background);
        sched.submit(job.clone() as Arc<dyn BackgroundJob>);

        assert_eq!(sched.jobs().len(), 1);
        done.store(true, Ordering::Release);
        // Monitor should reap within a few intervals.
        assert!(wait_until(|| sched.jobs().is_empty(), Duration::from_millis(500)));
        sched.shutdown();
    }

    #[test]
    fn defer_policy_pauses_on_play() {
        let fg = ForegroundActivity::new();
        let sched = JobScheduler::new(Arc::clone(&fg), SchedulerPolicy::Defer);
        let (job, pause_count, resume_count, _) = MockJob::new(JobPriority::Background);
        sched.submit(job.clone() as Arc<dyn BackgroundJob>);

        // Initially nothing happening — no pause.
        std::thread::sleep(MONITOR_INTERVAL * 3);
        let initial_pauses = pause_count.load(Ordering::Relaxed);
        assert_eq!(initial_pauses, 0, "fresh job must not be paused before play");

        // Engine starts playing.
        fg.set_playing(true);
        assert!(wait_until(|| pause_count.load(Ordering::Relaxed) > 0, Duration::from_millis(500)),
            "scheduler must pause job within ~50ms of play");

        // Engine stops playing — after grace, job resumes.
        fg.set_playing(false);
        assert!(wait_until(|| resume_count.load(Ordering::Relaxed) > 0, Duration::from_secs(2)),
            "scheduler must resume job after play stops + grace");

        sched.shutdown();
    }

    #[test]
    fn max_throughput_policy_never_pauses() {
        let fg = ForegroundActivity::new();
        let sched = JobScheduler::new(Arc::clone(&fg), SchedulerPolicy::MaxThroughput);
        let (job, pause_count, _, _) = MockJob::new(JobPriority::Background);
        sched.submit(job.clone() as Arc<dyn BackgroundJob>);

        // Start playing — under MaxThroughput, scheduler must NOT
        // pause the job.
        fg.set_playing(true);
        std::thread::sleep(MONITOR_INTERVAL * 6);
        assert_eq!(pause_count.load(Ordering::Relaxed), 0,
            "MaxThroughput must not pause jobs even on play");

        sched.shutdown();
    }

    #[test]
    fn user_interactive_priority_is_not_paused() {
        let fg = ForegroundActivity::new();
        let sched = JobScheduler::new(Arc::clone(&fg), SchedulerPolicy::Defer);
        let (job, pause_count, _, _) = MockJob::new(JobPriority::UserInteractive);
        sched.submit(job.clone() as Arc<dyn BackgroundJob>);

        fg.set_playing(true);
        std::thread::sleep(MONITOR_INTERVAL * 6);
        assert_eq!(pause_count.load(Ordering::Relaxed), 0,
            "UserInteractive jobs must never be paused by scheduler");

        sched.shutdown();
    }

    #[test]
    fn newly_submitted_job_paused_if_scheduler_currently_paused() {
        let fg = ForegroundActivity::new();
        let sched = JobScheduler::new(Arc::clone(&fg), SchedulerPolicy::Defer);

        // Trigger pause before submitting.
        fg.set_playing(true);
        // Wait a couple of monitor ticks so the scheduler enters
        // paused state.
        std::thread::sleep(MONITOR_INTERVAL * 4);

        let (job, pause_count, _, _) = MockJob::new(JobPriority::Background);
        sched.submit(job.clone() as Arc<dyn BackgroundJob>);

        // Newly-submitted job should be paused immediately by submit,
        // not have to wait for the next monitor tick.
        assert!(pause_count.load(Ordering::Relaxed) >= 1,
            "submit should pause job when scheduler is in paused state");

        sched.shutdown();
    }

    #[test]
    fn cancel_all_marks_jobs_done() {
        let fg = ForegroundActivity::new();
        let sched = JobScheduler::new(Arc::clone(&fg), SchedulerPolicy::Defer);
        let (job1, _, _, done1) = MockJob::new(JobPriority::Background);
        let (job2, _, _, done2) = MockJob::new(JobPriority::Background);
        sched.submit(job1.clone() as Arc<dyn BackgroundJob>);
        sched.submit(job2.clone() as Arc<dyn BackgroundJob>);

        sched.cancel_all();
        assert!(done1.load(Ordering::Acquire));
        assert!(done2.load(Ordering::Acquire));
        sched.shutdown();
    }
}
