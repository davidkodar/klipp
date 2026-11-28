//! Analysis gives way to the person at the editor. While they play,
//! scrub or have just touched the controls, no analyser starts a step,
//! and an ffmpeg already decoding is stopped where it stands until they
//! are done: the viewer's own hardware decode and the analysers' frame
//! grabs share one decoder, and the picture on screen comes first. The
//! host says what "busy" means through [`install`]; with nothing
//! installed (tests, tools) nothing ever waits.

use std::process::{Command, Output};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

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

/// Wait until the editor is idle. Said in the log once per wait.
pub fn wait() {
    klipp_jobs::held_child::wait_while(&held);
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
    klipp_jobs::held_child::output_within(cmd, limit, &held)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    // One test: the hook is process-wide, so two would race each other.
    #[test]
    fn the_runner_waits_for_the_installed_hold() {
        assert!(!held(), "nothing installed: nothing waits");
        wait();
        let busy = Arc::new(AtomicBool::new(true));
        let b = Arc::clone(&busy);
        install(Arc::new(move || b.load(Ordering::Relaxed)));
        let t = std::thread::spawn(|| {
            let mut cmd = Command::new("sh");
            cmd.args(["-c", "echo out"]);
            output(&mut cmd)
        });
        std::thread::sleep(Duration::from_millis(250));
        assert!(!t.is_finished(), "held before the spawn");
        busy.store(false, Ordering::Relaxed);
        let out = t.join().unwrap().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "out");
        install(Arc::new(|| false));
    }
}
