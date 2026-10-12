//! A long-running helper process (an ffmpeg encode) that ends with the
//! app. Killed or crashed, the app can't stop its children itself, and
//! they ran on to the end for a project no app had open (#1361).

use std::process::Command;

/// `program` as a command whose process gets SIGTERM when the thread that
/// started it ends: through util-linux's `setpriv --pdeathsig` where it is
/// installed, which sets the signal and then becomes `program`; else
/// `program` as it is. Start it from a thread that waits for it, so the
/// signal comes only when the app itself goes away.
pub fn ends_with_app(program: &str) -> Command {
    match setpriv() {
        Some(setpriv) => {
            let mut c = Command::new(setpriv);
            c.args(["--pdeathsig", "TERM", "--", program]);
            c
        }
        None => Command::new(program),
    }
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

    /// A child whose starting thread ends is signalled and stops, where a
    /// plain child would sleep on for a minute.
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
        assert_eq!(status.signal(), Some(15), "ended by SIGTERM: {status:?}");
    }
}
