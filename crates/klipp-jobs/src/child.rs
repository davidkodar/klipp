//! A long-running helper process (an ffmpeg encode) that ends with the
//! app. Killed or crashed, the app can't stop its children itself, and
//! they ran on to the end for a project no app had open (#1361).

use std::process::{Command, Stdio};

/// `program` as a command whose process is killed when the thread that
/// started it ends: through util-linux's `setpriv --pdeathsig` where it is
/// installed, which sets the signal and then becomes `program`; else
/// `program` as it is. Start it from a thread that waits for it, so the
/// signal comes only when the app itself goes away. The signal is
/// SIGKILL: an encode the scheduler has paused (SIGSTOP) would hold a
/// SIGTERM unanswered and stay stopped for good (#1271); the encodes
/// write to partial files, which a kill leaves to be made again.
///
/// Its stdin is null, not the terminal the app was started from: ffmpeg
/// with a terminal on stdin switches echo off to read its `q` key, and a
/// killed one never switches it back (#1767).
pub fn ends_with_app(program: &str) -> Command {
    let mut c = match setpriv() {
        Some(setpriv) => {
            let mut c = Command::new(setpriv);
            c.args(["--pdeathsig", "KILL", "--", program]);
            c
        }
        None => Command::new(program),
    };
    c.stdin(Stdio::null());
    c
}

fn setpriv() -> Option<&'static str> {
    static FOUND: std::sync::OnceLock<Option<&'static str>> = std::sync::OnceLock::new();
    *FOUND.get_or_init(|| {
        if !cfg!(target_os = "linux") {
            return None;
        }
        ["/usr/bin/setpriv", "/bin/setpriv"].into_iter().find(|p| std::path::Path::new(p).exists())
    })
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    /// A child whose starting thread ends is killed, where a plain child
    /// would sleep on for a minute; a paused (stopped) one too (#1271).
    #[test]
    fn the_child_ends_when_its_parent_does() {
        if setpriv().is_none() {
            eprintln!("no setpriv here: unverified");
            return;
        }
        // The thread lives a moment, as the app's do while their child runs:
        // a parent gone before setpriv sets the signal is never signalled.
        let mut child = std::thread::spawn(|| {
            let child = ends_with_app("sleep").arg("60").spawn().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(500));
            // Paused, as the scheduler pauses an encode during playback.
            let stopped = std::process::Command::new("kill").args(["-STOP", &child.id().to_string()]).status().unwrap();
            assert!(stopped.success());
            child
        })
        .join()
        .unwrap();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let status = loop {
            if let Some(s) = child.try_wait().unwrap() {
                break s;
            }
            if std::time::Instant::now() > until {
                let _ = child.kill();
                panic!("the child outlived the thread that started it");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        assert_eq!(status.signal(), Some(9), "killed: {status:?}");
    }

    /// The child reads from /dev/null, never the app's own stdin, which
    /// is the terminal when the app is started from one (#1767). The
    /// test runs itself again with a pipe as stdin, so a child that
    /// inherited stdin would show the pipe.
    #[test]
    fn the_child_does_not_share_the_terminal() {
        const PROBE: &str = "KLIPP_CHILD_STDIN_PROBE";
        if std::env::var_os(PROBE).is_some() {
            // spawn, as the encodes do: `output()` would null stdin itself.
            let child = ends_with_app("readlink").arg("/proc/self/fd/0").stdout(Stdio::piped()).spawn().unwrap();
            let out = child.wait_with_output().unwrap();
            println!("child stdin: {}", String::from_utf8_lossy(&out.stdout).trim());
            return;
        }
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "child::tests::the_child_does_not_share_the_terminal", "--nocapture"])
            .env(PROBE, "1")
            .stdin(Stdio::piped())
            .output()
            .unwrap();
        let said = String::from_utf8_lossy(&out.stdout);
        assert!(said.contains("child stdin: /dev/null"), "{said}");
    }
}
