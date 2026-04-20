//! Job implementation for thread-based work (filmstrip generation,
//! audio conform, future heavy CPU/GPU jobs that we own the loop of).
//!
//! The work closure receives an `Arc<PauseToken>`. It must call
//! `wait_while_paused()` between chunks so the scheduler can pause it
//! cooperatively; otherwise pause becomes a no-op until the chunk
//! finishes naturally.
//!
//! On Unix we lower the OS thread priority via `setpriority(PRIO_PROCESS, gettid, +10)`
//! at thread start. The kernel scheduler then prefers any
//! default-priority thread (including the engine playback thread)
//! whenever both are runnable.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::job::{BackgroundJob, JobId, JobPriority, ResourceClass};
use crate::pause::PauseToken;

pub struct ThreadJob {
    id: JobId,
    name: String,
    priority: JobPriority,
    resource_class: ResourceClass,
    progress: Arc<AtomicU32>,         // out of 1_000_000
    done: Arc<AtomicBool>,
    pause_token: Arc<PauseToken>,
    handle: std::sync::Mutex<Option<JoinHandle<()>>>,
}

impl ThreadJob {
    /// Spawn a new thread job. The closure is given:
    ///   * `pause_token`: call `wait_while_paused()` between chunks.
    ///   * `set_progress`: a closure to update progress in [0.0, 1.0].
    ///
    /// On success the thread is alive and running. The thread
    /// terminating (whether by completion, cancellation observation,
    /// or panic) marks the job done.
    pub fn spawn<F>(
        name: impl Into<String>,
        priority: JobPriority,
        resource_class: ResourceClass,
        work: F,
    ) -> Arc<Self>
    where
        F: FnOnce(Arc<PauseToken>, ProgressReporter) + Send + 'static,
    {
        let name_str = name.into();
        let pause_token = PauseToken::new();
        let progress = Arc::new(AtomicU32::new(0));
        let done = Arc::new(AtomicBool::new(false));

        let token_clone = Arc::clone(&pause_token);
        let progress_clone = Arc::clone(&progress);
        let done_clone = Arc::clone(&done);

        let thread_name = format!("klipp-job:{name_str}");
        let nice_offset = nice_offset_for_priority(priority);

        let handle = std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                set_thread_nice(nice_offset);
                let reporter = ProgressReporter { inner: progress_clone };
                // Catch panics so the job is marked done even on
                // panic — otherwise the scheduler would retain a
                // ghost job forever.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    work(token_clone, reporter);
                }));
                done_clone.store(true, Ordering::Release);
                if let Err(e) = result {
                    tracing::warn!("ThreadJob worker panicked: {e:?}");
                }
            })
            .expect("spawning a thread job must not fail at OS level");

        Arc::new(Self {
            id: JobId::new(),
            name: name_str,
            priority,
            resource_class,
            progress,
            done,
            pause_token,
            handle: std::sync::Mutex::new(Some(handle)),
        })
    }

    /// Block until the worker thread finishes. Returns immediately if
    /// already done. Useful for tests + graceful shutdown.
    pub fn join(&self) {
        let h = {
            let mut slot = self.handle.lock().unwrap_or_else(|e| e.into_inner());
            slot.take()
        };
        if let Some(handle) = h {
            let _ = handle.join();
        }
    }
}

impl BackgroundJob for ThreadJob {
    fn id(&self) -> JobId { self.id }
    fn name(&self) -> String { self.name.clone() }
    fn priority(&self) -> JobPriority { self.priority }
    fn resource_class(&self) -> ResourceClass { self.resource_class }

    fn progress(&self) -> f32 {
        self.progress.load(Ordering::Relaxed) as f32 / 1_000_000.0
    }

    fn is_done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    fn pause(&self) {
        self.pause_token.pause();
    }

    fn resume(&self) {
        self.pause_token.resume();
    }

    fn cancel(&self) {
        self.pause_token.cancel();
        // Worker observes is_cancelled() and exits; done flag is set
        // inside the spawn closure when work() returns.
    }
}

/// Update closure handed to job workers — wraps the atomic counter.
pub struct ProgressReporter {
    inner: Arc<AtomicU32>,
}

impl ProgressReporter {
    pub fn set(&self, p: f32) {
        let clamped = p.clamp(0.0, 1.0);
        let scaled = (clamped * 1_000_000.0) as u32;
        self.inner.store(scaled, Ordering::Relaxed);
    }
}

/// Map our app-level priority to a Linux nice offset. Background
/// jobs end up at nice +10 (well below default 0), Idle at +19 (the
/// floor — only runs when CPU is bored). Default + UserInteractive
/// stay at 0.
fn nice_offset_for_priority(priority: JobPriority) -> i32 {
    match priority {
        JobPriority::Idle => 19,
        JobPriority::Background => 10,
        JobPriority::Default => 0,
        JobPriority::UserInteractive => 0,
    }
}

#[cfg(unix)]
fn set_thread_nice(offset: i32) {
    if offset == 0 { return; }
    // setpriority on PRIO_PROCESS with `who = 0` targets the calling
    // thread's TID on Linux (Linux quirk — POSIX says "process"). On
    // macOS this targets the calling process, which is wrong for our
    // purposes — we'd lower the priority of the whole app. Guard
    // accordingly.
    #[cfg(target_os = "linux")]
    {
        unsafe {
            // Reset errno before calling.
            *libc::__errno_location() = 0;
            let r = libc::setpriority(libc::PRIO_PROCESS, 0, offset);
            if r == -1 && *libc::__errno_location() != 0 {
                tracing::debug!("setpriority(+{offset}) failed; running at default nice");
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        // macOS / BSD: we'd need pthread_setschedparam or QoS classes
        // to do this correctly per-thread. For now, no-op. App-level
        // pause via PauseToken still works.
        let _ = offset;
    }
}

#[cfg(not(unix))]
fn set_thread_nice(_offset: i32) {
    // Windows: SetThreadPriority. Not implemented in this phase.
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;
    use std::time::{Duration, Instant};

    #[test]
    fn spawn_and_complete() {
        let counter = Arc::new(AtomicU32::new(0));
        let counter_w = Arc::clone(&counter);
        let job = ThreadJob::spawn(
            "test-complete",
            JobPriority::Background,
            ResourceClass::Cpu,
            move |_pause, progress| {
                for i in 0..10 {
                    counter_w.fetch_add(1, Ordering::Relaxed);
                    progress.set((i + 1) as f32 / 10.0);
                }
            },
        );
        job.join();
        assert!(job.is_done());
        assert_eq!(counter.load(Ordering::Relaxed), 10);
        assert!((job.progress() - 1.0).abs() < 1e-3);
    }

    #[test]
    fn pause_resume_via_token() {
        let counter = Arc::new(AtomicU32::new(0));
        let counter_w = Arc::clone(&counter);
        let job = ThreadJob::spawn(
            "test-pause",
            JobPriority::Background,
            ResourceClass::Cpu,
            move |pause, _progress| {
                for _ in 0..50 {
                    pause.wait_while_paused();
                    if pause.is_cancelled() { break; }
                    counter_w.fetch_add(1, Ordering::Relaxed);
                    std::thread::sleep(Duration::from_millis(2));
                }
            },
        );

        // Let it run a beat then pause.
        std::thread::sleep(Duration::from_millis(20));
        job.pause();
        let mid = counter.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(60));
        let after_pause = counter.load(Ordering::Relaxed);
        assert!(after_pause - mid <= 1,
            "paused job advanced too far: {mid} -> {after_pause}");
        job.resume();
        job.join();
        assert!(job.is_done());
        assert_eq!(counter.load(Ordering::Relaxed), 50);
    }

    #[test]
    fn cancel_releases_paused_worker() {
        let job = ThreadJob::spawn(
            "test-cancel",
            JobPriority::Background,
            ResourceClass::Cpu,
            move |pause, _progress| {
                for _ in 0..100 {
                    pause.wait_while_paused();
                    if pause.is_cancelled() { return; }
                    std::thread::sleep(Duration::from_millis(20));
                }
            },
        );
        job.pause();
        std::thread::sleep(Duration::from_millis(30));
        let start = Instant::now();
        job.cancel();
        job.join();
        assert!(start.elapsed() < Duration::from_secs(1),
            "cancel of paused worker took too long: {:?}", start.elapsed());
        assert!(job.is_done());
    }

    #[test]
    fn panic_in_worker_marks_done() {
        let job = ThreadJob::spawn(
            "test-panic",
            JobPriority::Background,
            ResourceClass::Cpu,
            move |_pause, _progress| {
                panic!("synthetic panic for test");
            },
        );
        job.join();
        assert!(job.is_done(),
            "ThreadJob must mark done even when worker panics");
    }
}
