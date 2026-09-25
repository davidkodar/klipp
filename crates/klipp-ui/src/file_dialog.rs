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
//! Some pickers answer into UI state rather than into a command — the
//! export page's output path, a preferences folder, a file the widget
//! reads itself. Those use [`pick_path`] with an `egui::Id`, and the
//! widget collects the answer with [`take_path`] when it next draws. A
//! worker that has something to say afterwards (a file written, a read
//! that failed) says it with [`notify()`], and the app shows it as a toast.
//!
//! rfd's Linux backend talks to the desktop portal over D-Bus and is
//! safe off the main thread. The pickers are not parented to the window,
//! so nothing ties them to the UI thread either.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use klipp_core::AppCommand;

/// A picker is on screen. One at a time: the portal would stack a second
/// dialog over the first.
static OPEN: AtomicBool = AtomicBool::new(false);

/// The scope the app is in now, as last passed to [`install`].
static SCOPE: AtomicU64 = AtomicU64::new(0);

/// Pickers that have closed: the scope each was opened in, and its answer.
static READY: Mutex<Vec<(u64, Answer)>> = Mutex::new(Vec::new());

/// Paths chosen by [`pick_path`] and not yet collected: the scope, the
/// widget's id, the choice, and how many frames it has waited.
static PATHS: Mutex<Vec<(u64, egui::Id, Picked, u8)>> = Mutex::new(Vec::new());

/// The id of the [`pick_path`] picker on screen, if that is the kind open.
static PENDING: Mutex<Option<egui::Id>> = Mutex::new(None);

/// Messages from picker workers, shown by the app as toasts.
static NOTICES: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Frames a chosen path waits for its widget before it is dropped. The
/// widget draws every frame it is visible, so one that has not collected
/// its answer in this many has closed, and a later reopening must not
/// find an old choice waiting in it.
const PATH_FRAMES: u8 = 3;

enum Answer {
    Commands(Vec<AppCommand>),
    Path(egui::Id, Picked),
}

/// What a picker opened with [`pick_path`] came back with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Picked {
    Path(PathBuf),
    /// The user dismissed the picker.
    Cancelled,
}

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
    /// Paths chosen for a widget that closed before collecting them.
    pub expired: usize,
    /// What picker workers had to say, in order.
    pub notices: Vec<String>,
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
        *lock(&PENDING) = None;
        OPEN.store(false, Ordering::Release);
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
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
    run(None, move || Answer::Commands(pick()));
}

/// Run `pick` on a worker thread for the widget `id`, which collects the
/// answer with [`take_path`]. For a picker whose result the widget
/// applies itself rather than through a command.
///
/// `false` when the picker was refused because another is open: nothing
/// will answer, and a caller waiting on the answer must not wait.
pub fn pick_path<F>(id: egui::Id, pick: F) -> bool
where
    F: FnOnce() -> Option<PathBuf> + Send + 'static,
{
    run(Some(id), move || Answer::Path(id, pick().map_or(Picked::Cancelled, Picked::Path)))
}

/// One frame of a widget that must act on the path it asks for — the
/// unsaved-changes dialog's Save, whose close or quit follows the save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Awaited {
    /// Not asking: never started, or the picker was refused.
    Idle,
    /// The picker is open, or its answer has not reached this widget yet.
    Waiting,
    Answered(Picked),
}

/// Drive [`Awaited`] for `id`. `waiting` is what the last frame returned
/// ([`Awaited::Waiting`]); `start` is the click that asks. While waiting
/// this only collects: it never opens a second picker, however the
/// worker's finishing falls between frames.
pub fn await_path<F>(id: egui::Id, waiting: bool, start: bool, pick: F) -> Awaited
where
    F: FnOnce() -> Option<PathBuf> + Send + 'static,
{
    if waiting {
        return take_path(id).map_or(Awaited::Waiting, Awaited::Answered);
    }
    if start && pick_path(id, pick) {
        return Awaited::Waiting;
    }
    Awaited::Idle
}

/// The answer to the [`pick_path`] picker `id` opened, once it has closed.
/// A widget calls this every frame it draws.
pub fn take_path(id: egui::Id) -> Option<Picked> {
    let scope = SCOPE.load(Ordering::Acquire);
    let mut paths = lock(&PATHS);
    let at = paths.iter().position(|(s, i, _, _)| *s == scope && *i == id)?;
    Some(paths.swap_remove(at).2)
}

/// Whether the picker `id` opened with [`pick_path`] is on screen.
pub fn is_pending(id: egui::Id) -> bool {
    *lock(&PENDING) == Some(id)
}

/// Queue `message` to be shown as a toast on the next frame. For picker
/// workers reporting what they did with the file.
pub fn notify(message: impl Into<String>) {
    lock(&NOTICES).push(message.into());
    if let Some(ctx) = CTX.get() {
        ctx.request_repaint();
    }
}

fn run<F>(id: Option<egui::Id>, pick: F) -> bool
where
    F: FnOnce() -> Answer + Send + 'static,
{
    if OPEN.swap(true, Ordering::AcqRel) {
        REFUSED.fetch_add(1, Ordering::Relaxed);
        return false;
    }
    *lock(&PENDING) = id;
    let scope = SCOPE.load(Ordering::Acquire);
    let started = std::thread::Builder::new().name("file-dialog".into()).spawn(move || {
        {
            let _clear = ClearOpen;
            let answer = pick();
            lock(&READY).push((scope, answer));
        }
        // After the flag is clear, so the frame this wakes can open the
        // next picker.
        if let Some(ctx) = CTX.get() {
            ctx.request_repaint();
        }
    });
    if let Err(e) = started {
        tracing::error!("file dialog: could not start the picker thread: {e}");
        *lock(&PENDING) = None;
        OPEN.store(false, Ordering::Release);
        return false;
    }
    true
}

/// What pickers that closed since the last call chose, for the scope the
/// app is in now; results for another scope are counted and dropped.
pub fn take_ready() -> Ready {
    let scope = SCOPE.load(Ordering::Acquire);
    let batches = std::mem::take(&mut *lock(&READY));
    let mut ready = Ready {
        refused: REFUSED.swap(0, Ordering::Relaxed),
        notices: std::mem::take(&mut *lock(&NOTICES)),
        ..Ready::default()
    };
    let mut paths = lock(&PATHS);
    // Age the paths already waiting; one its widget has not collected
    // in time belongs to a widget that has closed.
    let mut expired = 0;
    paths.retain_mut(|(_, id, picked, age)| {
        *age += 1;
        let keep = *age <= PATH_FRAMES;
        if !keep {
            tracing::info!("file dialog: dropped an uncollected answer for {id:?}");
            expired += usize::from(*picked != Picked::Cancelled);
        }
        keep
    });
    ready.expired = expired;
    for (opened_in, answer) in batches {
        match answer {
            Answer::Commands(cmds) if opened_in == scope => ready.commands.extend(cmds),
            Answer::Commands(cmds) => ready.stale += usize::from(!cmds.is_empty()),
            Answer::Path(id, picked) if opened_in == scope => paths.push((scope, id, picked, 0)),
            Answer::Path(_, picked) => ready.stale += usize::from(picked != Picked::Cancelled),
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
        lock(&PATHS).clear();
        SCOPE.store(scope, Ordering::Release);
    }

    fn wait_for_path(id: egui::Id) -> Picked {
        let mut got = None;
        wait_until("a chosen path", || {
            let _ = take_ready();
            got = take_path(id);
            got.is_some()
        });
        got.unwrap()
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

    #[test]
    fn a_chosen_path_waits_for_its_own_widget() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset(11);
        let id = egui::Id::new("export-output");
        pick_path(id, || Some(PathBuf::from("/tmp/out.mov")));
        assert!(take_path(egui::Id::new("someone-else")).is_none());
        assert_eq!(wait_for_path(id), Picked::Path(PathBuf::from("/tmp/out.mov")));
        assert!(take_path(id).is_none(), "an answer is collected once");

        wait_until("the picker to close", || !is_open());
        pick_path(id, || None);
        assert_eq!(wait_for_path(id), Picked::Cancelled);
    }

    #[test]
    fn a_path_nobody_collects_is_dropped_rather_than_found_on_reopening() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset(12);
        let id = egui::Id::new("prefs-folder");
        pick_path(id, || Some(PathBuf::from("/media")));
        wait_until("the picker to close", || !is_open());
        // The widget has closed, so nothing collects it for a while.
        for _ in 0..=PATH_FRAMES + 1 {
            let _ = take_ready();
        }
        assert!(take_path(id).is_none());
    }

    #[test]
    fn a_path_chosen_for_another_project_is_counted_stale() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset(13);
        let id = egui::Id::new("consolidate-dest");
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        pick_path(id, move || {
            release_rx.recv().unwrap();
            Some(PathBuf::from("/elsewhere"))
        });
        assert!(is_pending(id));
        SCOPE.store(14, Ordering::Release);
        release_tx.send(()).unwrap();
        let got = next_ready();
        assert_eq!(got.stale, 1);
        assert!(take_path(id).is_none());
        assert!(!is_pending(id));
    }

    #[test]
    fn a_worker_notice_reaches_the_next_frame() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset(15);
        spawn(|| {
            notify("Frame saved");
            Vec::new()
        });
        wait_until("the picker to close", || !is_open());
        assert_eq!(take_ready().notices, vec!["Frame saved".to_string()]);
    }

    #[test]
    fn a_path_nobody_collects_is_counted_so_the_app_can_say_so() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset(16);
        let id = egui::Id::new("closed-panel");
        pick_path(id, || Some(PathBuf::from("/media")));
        wait_until("the picker to close", || !is_open());
        let mut expired = 0;
        for _ in 0..=PATH_FRAMES + 1 {
            expired += take_ready().expired;
        }
        assert_eq!(expired, 1);
    }

    #[test]
    fn awaiting_a_path_never_opens_a_second_picker() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset(17);
        let id = egui::Id::new("unsaved-save");
        let opened = std::sync::Arc::new(AtomicUsize::new(0));
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let o = opened.clone();
        assert_eq!(
            await_path(id, false, true, move || {
                o.fetch_add(1, Ordering::SeqCst);
                release_rx.recv().unwrap();
                Some(PathBuf::from("/p/A.klipp"))
            }),
            Awaited::Waiting
        );
        release_tx.send(()).unwrap();
        // The worker finishes, and its answer sits in READY until the
        // next frame's take_ready: the widget must keep waiting, not ask
        // again — even when the click is repeated.
        wait_until("the picker to close", || !is_open());
        let o = opened.clone();
        assert_eq!(await_path(id, true, true, move || { o.fetch_add(1, Ordering::SeqCst); None }), Awaited::Waiting);
        let _ = take_ready();
        assert_eq!(await_path(id, true, false, || None), Awaited::Answered(Picked::Path("/p/A.klipp".into())));
        assert_eq!(opened.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_refused_picker_is_not_waited_on() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        reset(18);
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        spawn(move || {
            release_rx.recv().unwrap();
            Vec::new()
        });
        let id = egui::Id::new("unsaved-save");
        assert_eq!(await_path(id, false, true, || None), Awaited::Idle, "refused, so nothing will answer");
        // And it does not open by itself once the other picker closes.
        release_tx.send(()).unwrap();
        wait_until("the other picker to close", || !is_open());
        assert_eq!(await_path(id, false, false, || None), Awaited::Idle);
        assert!(!is_open());
        assert_eq!(take_ready().refused, 1);
    }
}
