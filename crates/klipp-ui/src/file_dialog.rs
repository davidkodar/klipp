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
//! rfd's Linux backend talks to the desktop portal over D-Bus and is
//! safe off the main thread. The pickers are not parented to the window,
//! so nothing ties them to the UI thread either.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use klipp_core::AppCommand;

/// A picker is on screen. One at a time: the portal would stack a second
/// dialog over the first, and a click that opens the same picker twice
/// is nearly always a double click.
static OPEN: AtomicBool = AtomicBool::new(false);

/// Commands from pickers that have closed, waiting for the next frame.
static READY: Mutex<Vec<AppCommand>> = Mutex::new(Vec::new());

/// The context to wake when a picker closes.
static CTX: OnceLock<egui::Context> = OnceLock::new();

/// Remember the context to repaint when a picker closes. Cheap to call
/// every frame; only the first call stores anything.
pub fn install(ctx: &egui::Context) {
    let _ = CTX.set(ctx.clone());
}

/// Run `pick` on a worker thread and queue the commands it returns.
///
/// Ignored while another picker is open. `pick` shows the dialog and
/// maps the choice to commands; an empty list means the user cancelled.
pub fn spawn<F>(pick: F)
where
    F: FnOnce() -> Vec<AppCommand> + Send + 'static,
{
    if OPEN.swap(true, Ordering::AcqRel) {
        return;
    }
    let started = std::thread::Builder::new().name("file-dialog".into()).spawn(move || {
        let cmds = pick();
        READY.lock().unwrap_or_else(|e| e.into_inner()).extend(cmds);
        OPEN.store(false, Ordering::Release);
        if let Some(ctx) = CTX.get() {
            ctx.request_repaint();
        }
    });
    if let Err(e) = started {
        tracing::error!("file dialog: could not start the picker thread: {e}");
        OPEN.store(false, Ordering::Release);
    }
}

/// Commands from pickers that closed since the last call.
pub fn take_ready() -> Vec<AppCommand> {
    std::mem::take(&mut *READY.lock().unwrap_or_else(|e| e.into_inner()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn a_picker_result_arrives_on_a_later_frame_and_one_picker_runs_at_a_time() {
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        spawn(move || {
            release_rx.recv().unwrap();
            vec![AppCommand::Undo]
        });
        // Still open: a second request is dropped, not stacked.
        spawn(|| vec![AppCommand::Redo]);
        assert!(take_ready().is_empty(), "nothing is ready before the picker closes");

        release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut got = Vec::new();
        while got.is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
            got = take_ready();
        }
        assert_eq!(got.len(), 1);
        assert!(matches!(got[0], AppCommand::Undo));

        // Closed, so the next request runs.
        while OPEN.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(1));
        }
        spawn(|| vec![AppCommand::Redo]);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut got = Vec::new();
        while got.is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
            got = take_ready();
        }
        assert!(matches!(got.as_slice(), [AppCommand::Redo]));
    }
}
