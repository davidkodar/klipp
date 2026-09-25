//! File pickers that do not stop the UI.
//!
//! `rfd::FileDialog` blocks the calling thread until the user picks or
//! cancels. Called from `update()`, that stops the event loop, so the
//! window stops answering the compositor's pings, and a few seconds into
//! choosing a file Hyprland (like GNOME and KDE) offers to terminate
//! Klipp as "not responding". Someone who takes that offer loses the
//! session.
//!
//! [`spawn`] runs the picker on a worker thread instead. The picker turns
//! its answer into commands; they wait here until the app drains them
//! with [`take_ready`] on its next frame, and the worker asks egui for
//! that frame when it finishes, so a result lands even while the app is
//! otherwise idle.
//!
//! Because the window stays live, the user can open another project or
//! switch sequence while a picker is up. A command built for the old one
//! must not land in the new one — Save As would write project B over the
//! A.klipp chosen for project A. So every picker remembers the scope it
//! was opened in (the app passes it to [`install`] each frame: which
//! project is open and which sequence is active), and a result that
//! closes in a different scope is discarded, not delivered.
//!
//! rfd's Linux backend talks to the desktop portal over D-Bus and is
//! safe off the main thread. The pickers are not parented to the window,
//! so nothing ties them to the UI thread either.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use klipp_core::AppCommand;

/// A picker is on screen. One at a time: the portal would stack a second
/// dialog over the first.
static OPEN: AtomicBool = AtomicBool::new(false);

/// The scope the app is in now, as last passed to [`install`].
static SCOPE: AtomicU64 = AtomicU64::new(0);

/// Pickers that have closed: the scope each was opened in, and its commands.
static READY: Mutex<Vec<(u64, Vec<AppCommand>)>> = Mutex::new(Vec::new());

/// Requests turned away because a picker was already open.
static REFUSED: AtomicUsize = AtomicUsize::new(0);

/// The context to wake when a picker closes.
static CTX: OnceLock<egui::Context> = OnceLock::new();

/// What [`take_ready`] found since the last frame.
#[derive(Default)]
pub struct Ready {
    /// Commands from pickers that closed in the scope they were opened in.
    pub commands: Vec<AppCommand>,
    /// Pickers whose result was discarded because the project or the
    /// sequence changed while they were open.
    pub stale: usize,
    /// Requests ignored because another picker was already open.
    pub refused: usize,
}

/// Call at the start of every frame: remembers the context to repaint
/// when a picker closes, and the scope new pickers belong to.
pub fn install(ctx: &egui::Context, scope: u64) {
    let _ = CTX.set(ctx.clone());
    SCOPE.store(scope, Ordering::Release);
}

/// Whether a picker is on screen.
pub fn is_open() -> bool {
    OPEN.load(Ordering::Acquire)
}

/// Clears [`OPEN`] however the picker thread ends, a panic included:
/// a flag left set would silently refuse every picker until restart.
struct ClearOpen;

impl Drop for ClearOpen {
    fn drop(&mut self) {
        OPEN.store(false, Ordering::Release);
    }
}

/// Run `pick` on a worker thread and queue the commands it returns.
///
/// Refused while another picker is open (counted in [`Ready::refused`]
/// so the app can say so). `pick` shows the dialog and maps the choice to
/// commands; an empty list means the user cancelled.
pub fn spawn<F>(pick: F)
where
    F: FnOnce() -> Vec<AppCommand> + Send + 'static,
{
    if OPEN.swap(true, Ordering::AcqRel) {
        REFUSED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let scope = SCOPE.load(Ordering::Acquire);
    let started = std::thread::Builder::new().name("file-dialog".into()).spawn(move || {
        {
            let _clear = ClearOpen;
            let cmds = pick();
            READY.lock().unwrap_or_else(|e| e.into_inner()).push((scope, cmds));
        }
        // After the flag is clear, so the frame this wakes can open the
        // next picker.
        if let Some(ctx) = CTX.get() {
            ctx.request_repaint();
        }
    });
    if let Err(e) = started {
        tracing::error!("file dialog: could not start the picker thread: {e}");
        OPEN.store(false, Ordering::Release);
    }
}

/// What pickers that closed since the last call chose, for the scope the
/// app is in now; results for another scope are counted and dropped.
pub fn take_ready() -> Ready {
    let scope = SCOPE.load(Ordering::Acquire);
    let batches = std::mem::take(&mut *READY.lock().unwrap_or_else(|e| e.into_inner()));
    let mut ready = Ready { refused: REFUSED.swap(0, Ordering::Relaxed), ..Ready::default() };
    for (opened_in, cmds) in batches {
        if opened_in == scope {
            ready.commands.extend(cmds);
        } else if !cmds.is_empty() {
            ready.stale += 1;
        }
    }
    ready
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// The module is global state; its tests must not interleave.
    static SERIAL: Mutex<()> = Mutex::new(());

    // No egui context is installed in these tests (`CTX` stays empty),
    // so the worker skips the repaint; the tests poll instead.

    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn next_ready() -> Ready {
        let mut out = Ready::default();
        wait_until("a picker result", || {
            let r = take_ready();
            let any = !r.commands.is_empty() || r.stale > 0;
            out.commands.extend(r.commands);
            out.stale += r.stale;
            out.refused += r.refused;
            any
        });
        out
    }

    fn reset(scope: u64) {
        wait_until("no picker open", || !is_open());
        let _ = take_ready();
        SCOPE.store(scope, Ordering::Release);
    }

    #[test]
    fn a_result_arrives_on_a_later_frame_and_one_picker_runs_at_a_time() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset(1);
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        spawn(move || {
            release_rx.recv().unwrap();
            vec![AppCommand::Undo]
        });
        // Still open: a second request is refused, not stacked.
        spawn(|| vec![AppCommand::Redo]);
        let early = take_ready();
        assert!(early.commands.is_empty(), "nothing is ready before the picker closes");
        assert_eq!(early.refused, 1);

        release_tx.send(()).unwrap();
        let got = next_ready();
        assert!(matches!(got.commands.as_slice(), [AppCommand::Undo]));

        // Closed, so the next request runs.
        wait_until("the picker to close", || !is_open());
        spawn(|| vec![AppCommand::Redo]);
        assert!(matches!(next_ready().commands.as_slice(), [AppCommand::Redo]));
    }

    #[test]
    fn a_result_for_a_project_that_is_no_longer_open_is_dropped() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset(7);
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        spawn(move || {
            release_rx.recv().unwrap();
            vec![AppCommand::Undo]
        });
        // The user opens another project while the picker is up.
        SCOPE.store(8, Ordering::Release);
        release_tx.send(()).unwrap();
        let got = next_ready();
        assert!(got.commands.is_empty(), "a command for the old project must not run in the new one");
        assert_eq!(got.stale, 1);
    }

    #[test]
    fn a_panicking_picker_does_not_block_the_next_one() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset(3);
        spawn(|| panic!("picker blew up"));
        wait_until("the panicking picker to release the flag", || !is_open());
        spawn(|| vec![AppCommand::Redo]);
        assert!(matches!(next_ready().commands.as_slice(), [AppCommand::Redo]));
    }
}
