//! Analysis gives way to the person at the editor. While they play,
//! scrub or have just touched the controls, no analyser starts a step,
//! and an ffmpeg already decoding is stopped where it stands until they
//! are done: the viewer's own hardware decode and the analysers' frame
//! grabs share one decoder, and the picture on screen comes first. The
//! host says what "busy" means through [`install`]; with nothing
//! installed (tests, tools) nothing ever waits.

use std::io::Read;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

type Check = Arc<dyn Fn() -> bool + Send + Sync>;

fn check() -> &'static Mutex<Option<Check>> {
    static C: OnceLock<Mutex<Option<Check>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}

/// Say what "the editor is busy" is: `true` while analysis should wait.
pub fn install(f: Check) {
    *check().lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
}

/// Whether analysis should wait right now.
pub fn held() -> bool {
    // Called outside the lock: a check that panics leaves it usable.
    let f = check().lock().unwrap_or_else(|e| e.into_inner()).clone();
    f.is_some_and(|f| f())
}

const POLL: Duration = Duration::from_millis(100);

/// Wait until the editor is idle. Said in the log once per wait.
pub fn wait() {
    if !held() {
        return;
    }
    let t = Instant::now();
    tracing::info!("analysis waits: the editor is busy");
    while held() {
        std::thread::sleep(POLL);
    }
    tracing::info!("analysis resumes after {:.1} s", t.elapsed().as_secs_f64());
}

/// Run `cmd` to the end with its output captured, as `Command::output`
/// does, except that the child is stopped (SIGSTOP) while the editor is
/// busy and continued when it is idle again. The child dies with the
/// thread that started it, so a stopped one is never left behind.
pub fn output(cmd: &mut Command) -> std::io::Result<Output> {
    output_within(cmd, None)?.ok_or_else(|| std::io::Error::other("cut short with no deadline"))
}

/// [`output`], killed when it has run `limit` (not counting the time it
/// was stopped for the editor): `None` then. Hardware decode in ffmpeg
/// can deadlock on a seek into some files and wait forever (#519).
pub fn output_within(cmd: &mut Command, limit: Option<Duration>) -> std::io::Result<Option<Output>> {
    wait();
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: prctl is async-signal-safe and touches nothing shared.
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
    }
    let mut child = cmd.spawn()?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let mut stopped = false;
    let mut stopped_at = Instant::now();
    let mut stopped_for = Duration::ZERO;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        let ran = started.elapsed().saturating_sub(stopped_for + if stopped { stopped_at.elapsed() } else { Duration::ZERO });
        if limit.is_some_and(|l| ran > l) {
            let _ = child.kill();
            let _ = child.wait();
            tracing::warn!("{:?} killed after {:.1} s", cmd.get_program(), ran.as_secs_f64());
            return Ok(None);
        }
        let busy = held();
        if busy != stopped {
            if busy {
                stopped_at = Instant::now();
            } else {
                stopped_for += stopped_at.elapsed();
            }
            if signal(child.id(), busy) {
                stopped = busy;
            }
        }
        std::thread::sleep(POLL);
    };
    if stopped {
        stopped_for += stopped_at.elapsed();
    }
    if stopped_for > Duration::ZERO {
        tracing::info!(
            "analysis decode stopped {:.1} s of {:.1} s while the editor was busy",
            stopped_for.as_secs_f64(),
            started.elapsed().as_secs_f64()
        );
    }
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    Ok(Some(Output { status, stdout, stderr }))
}

fn drain<R: Read + Send + 'static>(r: Option<R>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut v = Vec::new();
        if let Some(mut r) = r {
            let _ = r.read_to_end(&mut v);
        }
        v
    })
}

/// Stop (`true`) or continue the child. `false` where that cannot be
/// done, and the child simply runs on.
#[cfg(unix)]
fn signal(pid: u32, stop: bool) -> bool {
    let sig = if stop { libc::SIGSTOP } else { libc::SIGCONT };
    // SAFETY: kill with a pid this process spawned and still waits on.
    unsafe { libc::kill(pid as libc::pid_t, sig) == 0 }
}

#[cfg(not(unix))]
fn signal(_pid: u32, _stop: bool) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    // One test: the hook is process-wide, so two would race each other.
    #[test]
    fn a_stopped_child_finishes_once_released() {
        assert!(!held(), "nothing installed: nothing waits");
        wait();
        let busy = Arc::new(AtomicBool::new(true));
        let b = Arc::clone(&busy);
        install(Arc::new(move || b.load(Ordering::Relaxed)));
        let t = std::thread::spawn(|| {
            let mut cmd = Command::new("sh");
            cmd.args(["-c", "echo out; echo err >&2"]);
            output(&mut cmd)
        });
        std::thread::sleep(Duration::from_millis(250));
        assert!(!t.is_finished(), "held before the spawn");
        busy.store(false, Ordering::Relaxed);
        let out = t.join().unwrap().unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "out");
        assert_eq!(String::from_utf8_lossy(&out.stderr).trim(), "err");
        install(Arc::new(|| false));
    }
}
