//! Job implementation for subprocesses (ffmpeg skim transcode, future
//! external tool jobs). Pause/resume use SIGSTOP/SIGCONT on Unix —
//! the kernel suspends the process instantly, releasing CPU and (for
//! NVENC subprocesses) freeing GPU compute slots between SIGSTOP and
//! the next SIGCONT.
//!
//! Cancellation kills the child and waits for it to terminate; safe to
//! call repeatedly.
//!
//! Progress is reported via a callback that the caller wires to
//! whatever stdout-parsing it already does (e.g. ffmpeg's
//! `-progress pipe:1` `out_time_us=...` lines). We don't bake a
//! particular protocol in here — the existing skim spawner already
//! parses ffmpeg progress, and we just need a way to feed that number
//! into [`SubprocessJob::set_progress`].

use std::process::Child;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::sync::Arc;

use crate::job::{BackgroundJob, JobId, JobPriority, ResourceClass};

pub struct SubprocessJob {
    id: JobId,
    name: String,
    priority: JobPriority,
    resource_class: ResourceClass,
    /// Child PID, or 0 if the child hasn't been registered yet (e.g.
    /// scheduled but the spawning thread is still warming up). 0 is
    /// sentinel-safe: `kill(0, SIGSTOP)` would target a process group
    /// rather than a real PID, which we explicitly guard against.
    pid: AtomicI32,
    progress: AtomicU32, // out of 1_000_000 — gives 6-digit precision in [0, 1]
    done: AtomicBool,
    paused: AtomicBool,
    cancelled: AtomicBool,
    /// Shared handle to the running child so cancellation can kill it.
    /// `None` until `register_child` is called by the spawn helper.
    child: std::sync::Mutex<Option<Child>>,
}

impl SubprocessJob {
    pub fn new(name: impl Into<String>, priority: JobPriority, resource_class: ResourceClass) -> Arc<Self> {
        Arc::new(Self {
            id: JobId::new(),
            name: name.into(),
            priority,
            resource_class,
            pid: AtomicI32::new(0),
            progress: AtomicU32::new(0),
            done: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            child: std::sync::Mutex::new(None),
        })
    }

    /// Register the spawned child process with the job. Stores the
    /// PID for SIGSTOP/SIGCONT and the `Child` handle for cancel-kill.
    /// Call this immediately after `Command::spawn()` succeeds.
    ///
    /// If the job was paused while we were between children (e.g. an
    /// NVENC attempt failed and we're now spawning the libx264
    /// fallback), the new child is SIGSTOPed immediately so the
    /// scheduler's pause stays honoured across the fallback. Same
    /// applies to cancel: a job cancelled between children gets the
    /// new child killed before this returns.
    pub fn register_child(&self, child: Child) {
        let pid = child.id() as i32;
        self.pid.store(pid, Ordering::Release);
        let mut slot = self.child.lock().unwrap_or_else(|e| e.into_inner());
        *slot = Some(child);
        // Honour any pause/cancel state that was set between children.
        if self.cancelled.load(Ordering::Acquire) {
            if let Some(mut c) = slot.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
            return;
        }
        if self.paused.load(Ordering::Acquire) {
            self.send_stop();
        }
    }

    /// Take ownership of the child back so the spawning thread can
    /// `wait()` on it without contending with cancel-kill. Returns
    /// `None` if the child was already taken or never registered.
    pub fn take_child(&self) -> Option<Child> {
        let mut slot = self.child.lock().unwrap_or_else(|e| e.into_inner());
        slot.take()
    }

    /// Update progress in [0.0, 1.0]. Out-of-range values clamp.
    pub fn set_progress(&self, p: f32) {
        let clamped = p.clamp(0.0, 1.0);
        let scaled = (clamped * 1_000_000.0) as u32;
        self.progress.store(scaled, Ordering::Relaxed);
    }

    /// Mark the job as completed (success or failure). Does NOT
    /// touch the child handle — caller is expected to have already
    /// `wait()`-ed.
    pub fn mark_done(&self) {
        self.done.store(true, Ordering::Release);
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// Send `SIGSTOP` to the child. No-op on non-Unix or if the PID
    /// hasn't been registered yet.
    #[cfg(unix)]
    fn send_stop(&self) {
        let pid = self.pid.load(Ordering::Acquire);
        if pid <= 0 { return; }
        // SAFETY: `kill` is FFI; passing a PID we read from Child::id
        // is well-defined for any value (kernel rejects non-existent
        // PIDs by returning -1, which we ignore).
        unsafe {
            let _ = libc::kill(pid, libc::SIGSTOP);
        }
    }

    #[cfg(unix)]
    fn send_cont(&self) {
        let pid = self.pid.load(Ordering::Acquire);
        if pid <= 0 { return; }
        unsafe {
            let _ = libc::kill(pid, libc::SIGCONT);
        }
    }

    #[cfg(not(unix))]
    fn send_stop(&self) {
        // No portable equivalent on Windows. We could implement via
        // Job objects + DebugActiveProcess, but for now log + no-op.
        tracing::debug!("SubprocessJob::pause: SIGSTOP unsupported on this platform");
    }

    #[cfg(not(unix))]
    fn send_cont(&self) {}
}

impl BackgroundJob for SubprocessJob {
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
        // Already paused: skip (idempotent).
        if self.paused.swap(true, Ordering::AcqRel) { return; }
        self.send_stop();
    }

    fn resume(&self) {
        if !self.paused.swap(false, Ordering::AcqRel) { return; }
        self.send_cont();
    }

    fn cancel(&self) {
        // Mark cancelled first so the spawn thread observes it and
        // doesn't restart anything mid-flight.
        if self.cancelled.swap(true, Ordering::AcqRel) { return; }
        // Resume before killing — SIGKILL on a stopped process leaks
        // until SIGCONT is sent, because the kernel can't reap a
        // process that's still stopped.
        self.send_cont();
        let mut slot = self.child.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(mut child) = slot.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.done.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::{Duration, Instant};

    /// Helper: build a `sleep` Command. Cross-platform: macOS, Linux,
    /// and BSD all ship `sleep`. Windows isn't covered — we no-op
    /// pause there anyway.
    fn sleep_cmd(secs: u64) -> Command {
        let mut c = Command::new("sleep");
        c.arg(secs.to_string());
        c
    }

    #[test]
    fn lifecycle_basic() {
        let job = SubprocessJob::new("test", JobPriority::Background, ResourceClass::Cpu);
        assert_eq!(job.progress(), 0.0);
        assert!(!job.is_done());
        assert!(!job.is_paused());
        assert!(!job.is_cancelled());

        job.set_progress(0.5);
        assert!((job.progress() - 0.5).abs() < 1e-3);

        job.set_progress(2.0); // out-of-range clamps
        assert_eq!(job.progress(), 1.0);

        job.set_progress(-1.0);
        assert_eq!(job.progress(), 0.0);
    }

    #[test]
    fn pause_resume_idempotent() {
        let job = SubprocessJob::new("test", JobPriority::Background, ResourceClass::Cpu);
        // No PID registered — pause/resume should be safe no-ops.
        job.pause();
        assert!(job.is_paused());
        job.pause(); // double-pause: no panic
        assert!(job.is_paused());
        job.resume();
        assert!(!job.is_paused());
        job.resume(); // double-resume: no panic
        assert!(!job.is_paused());
    }

    #[cfg(unix)]
    #[test]
    fn pause_actually_stops_child() {
        // Spawn `sleep 5`, then pause. Verify the kernel reports
        // stopped state via /proc/<pid>/stat. SIGSTOP delivery is
        // asynchronous — the kernel may take a few ticks to apply
        // it, especially right after exec() — so we retry the
        // /proc read for up to 500ms before giving up.
        let job = SubprocessJob::new("sleep-stop-test", JobPriority::Background, ResourceClass::Cpu);
        let mut cmd = sleep_cmd(2);
        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("skipping pause_actually_stops_child: spawn failed ({e})");
                return;
            }
        };
        job.register_child(child);

        job.pause();
        let pid = job.pid.load(Ordering::Acquire);
        assert!(pid > 0);

        // Wait for the kernel to mark the process stopped. /proc
        // state field is 'T' (stopped by signal) once SIGSTOP is
        // applied, 'R' or 'S' before. Retry every 10ms up to 500ms.
        let mut last_state = "?".to_string();
        let deadline = std::time::Instant::now() + Duration::from_millis(500);
        let stopped = loop {
            let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                Ok(s) => s,
                Err(_) => break false, // process gone — unexpected
            };
            // Format: "<pid> (<comm>) <state> <ppid> ..."
            // <comm> may contain spaces / parens; rsplit at ')' to
            // skip past it.
            let after_comm = stat.rsplit_once(')').map(|(_, rest)| rest).unwrap_or("");
            let state_char = after_comm.split_whitespace().next().unwrap_or("?").to_string();
            last_state = state_char.clone();
            if state_char == "T" { break true; }
            if std::time::Instant::now() > deadline { break false; }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(stopped,
            "expected pid {pid} to enter stopped state 'T' within 500ms, last saw {last_state:?}");

        job.cancel();
        assert!(job.is_done());
    }

    #[cfg(unix)]
    #[test]
    fn cancel_kills_running_child() {
        let job = SubprocessJob::new("sleep-cancel-test", JobPriority::Background, ResourceClass::Cpu);
        let mut cmd = sleep_cmd(30);
        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("skipping cancel_kills_running_child: spawn failed ({e})");
                return;
            }
        };
        let pid = child.id();
        job.register_child(child);

        let start = Instant::now();
        job.cancel();
        // Cancel must terminate fast — well under 1 sec.
        assert!(start.elapsed() < Duration::from_secs(1),
            "cancel took too long: {:?}", start.elapsed());
        assert!(job.is_done());

        // PID must no longer exist (or be a zombie that's already
        // been reaped). Sending signal 0 to a non-existent PID
        // returns -1 with errno ESRCH.
        unsafe {
            let r = libc::kill(pid as i32, 0);
            assert_eq!(r, -1,
                "expected child PID {pid} to be reaped after cancel");
        }
    }

    #[cfg(unix)]
    #[test]
    fn register_child_after_cancel_kills_immediately() {
        // NVENC-then-libx264 fallback pattern: cancel could land
        // between the two `register_child` calls. The new child
        // must die immediately so we don't leak a runaway ffmpeg.
        let job = SubprocessJob::new("fallback-cancel-test", JobPriority::Background, ResourceClass::Cpu);
        // Cancel before any child registered — sets the cancelled
        // flag without affecting any process.
        job.cancel();

        // Now spawn the fallback child.
        let mut cmd = sleep_cmd(30);
        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("skipping register_after_cancel: spawn failed ({e})");
                return;
            }
        };
        let pid = child.id();
        job.register_child(child);

        // The new child should have been killed by register_child.
        // Wait briefly for the kill to take effect.
        std::thread::sleep(Duration::from_millis(100));
        unsafe {
            let r = libc::kill(pid as i32, 0);
            assert_eq!(r, -1,
                "register_child after cancel must kill the new child immediately, pid {pid} still alive");
        }
    }

    #[cfg(unix)]
    #[test]
    fn register_child_after_pause_stops_immediately() {
        // Pause set while between children. The new child must come
        // up SIGSTOPed so the scheduler's pause stays honoured across
        // the NVENC→libx264 fallback.
        let job = SubprocessJob::new("fallback-pause-test", JobPriority::Background, ResourceClass::Cpu);
        job.pause();

        let mut cmd = sleep_cmd(5);
        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("skipping register_after_pause: spawn failed ({e})");
                return;
            }
        };
        let pid = child.id() as i32;
        job.register_child(child);

        // Wait for the kernel to apply SIGSTOP.
        let deadline = Instant::now() + Duration::from_millis(500);
        let stopped = loop {
            let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                Ok(s) => s,
                Err(_) => break false,
            };
            let after_comm = stat.rsplit_once(')').map(|(_, rest)| rest).unwrap_or("");
            let state = after_comm.split_whitespace().next().unwrap_or("?");
            if state == "T" { break true; }
            if Instant::now() > deadline { break false; }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(stopped, "register_child after pause must SIGSTOP the new child");

        job.cancel();
    }

    #[cfg(unix)]
    #[test]
    fn cancel_resumes_paused_child_before_kill() {
        // SIGKILL on a SIGSTOPped process won't let the kernel reap
        // it until SIGCONT is sent. Cancel must SIGCONT first; this
        // test would hang if it didn't.
        let job = SubprocessJob::new("sleep-pause-cancel", JobPriority::Background, ResourceClass::Cpu);
        let mut cmd = sleep_cmd(30);
        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("skipping cancel_resumes_paused: spawn failed ({e})");
                return;
            }
        };
        job.register_child(child);

        job.pause();
        let start = Instant::now();
        job.cancel();
        assert!(start.elapsed() < Duration::from_secs(1),
            "cancel of paused child took too long: {:?}", start.elapsed());
    }
}
