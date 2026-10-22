//! Opening an AAF file with no converter to read it (#2086): the person
//! is told what is missing and asked whether Klipp may install it into a
//! Python environment of its own, shown the exact commands; with their
//! consent the commands run, their output shown as it comes, and when
//! they finish the file opens. A failure is told in a sentence.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use klipp_core::AppCommand;
use klipp_project::otio_tool;

/// Where the install is.
pub enum AafInstall {
    /// Asking, the commands shown; `python` is `None` when there is no
    /// Python to make the environment with, `venv` when there is no data
    /// folder to make it in.
    Asking { path: PathBuf, new_project: bool, python: Option<PathBuf>, venv: Option<PathBuf>, steps: Vec<Vec<String>> },
    /// Running: the lines so far, the outcome once it is in, and the flag
    /// that stops it (Cancel, or the app quitting).
    Running { path: PathBuf, new_project: bool, lines: Arc<Mutex<Vec<String>>>, done: Arc<Mutex<Option<Result<(), String>>>>, cancel: Arc<AtomicBool> },
    /// Stopped short: why, and what the commands said.
    Failed { path: PathBuf, new_project: bool, why: String, lines: Vec<String> },
}

impl AafInstall {
    /// The question for `path`, opened as a new project or into this one
    /// once the converter is there, with this system's Python and Klipp's
    /// environment.
    pub fn ask(path: PathBuf, new_project: bool) -> Self {
        let python = otio_tool::find_python(std::env::var_os("PATH").as_deref());
        let venv = otio_tool::venv_dir();
        let steps = python.as_ref().zip(venv.as_ref()).map(|(p, v)| otio_tool::install_steps(p, v)).unwrap_or_default();
        AafInstall::Asking { path, new_project, python, venv, steps }
    }

    /// Whether an install is running.
    pub fn is_running(&self) -> bool {
        matches!(self, AafInstall::Running { .. })
    }

    /// A running install stopped, its step killed: at Cancel, and when the
    /// app quits, waiting up to `wait` for the step to be gone.
    pub fn stop(&self, wait: std::time::Duration) {
        if let AafInstall::Running { done, cancel, .. } = self {
            cancel.store(true, Ordering::Relaxed);
            let until = std::time::Instant::now() + wait;
            while done.lock().map(|d| d.is_none()).unwrap_or(false) && std::time::Instant::now() < until {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    }
}

/// The install's window, if there is one, for this frame. Its answer
/// moves it on; an install that finished opens the file through `cmds`.
pub fn show(ctx: &egui::Context, slot: &mut Option<AafInstall>, cmds: &mut Vec<AppCommand>) {
    let Some(current) = slot.take() else { return };
    let window = egui::Window::new("Open AAF").collapsible(false).resizable(false).order(egui::Order::Foreground).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]);
    let next = match current {
        AafInstall::Asking { path, new_project, python, venv, steps } => {
            let mut answer = None;
            window.show(ctx, |ui| {
                ui.set_max_width(520.0);
                ui.label("Klipp opens AAF with OpenTimelineIO's AAF converter, which isn't installed.");
                ui.add_space(6.0);
                match (&python, &venv) {
                    (_, None) => {
                        ui.label(egui::RichText::new(otio_tool::InstallError::NoDataFolder.to_string()).strong());
                    }
                    (None, _) => {
                        ui.label(egui::RichText::new(otio_tool::InstallError::NoPython.to_string()).strong());
                    }
                    (Some(_), Some(venv)) => {
                        ui.label(format!("Klipp can install it in a folder of its own ({}), leaving your system's Python untouched, by running:", venv.display()));
                        ui.add_space(4.0);
                        for step in &steps {
                            ui.label(egui::RichText::new(otio_tool::shown(step)).monospace().small());
                        }
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new("This downloads the two packages from the Python Package Index.").small());
                    }
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        answer = Some(false);
                    }
                    if python.is_some() && venv.is_some() && ui.button("Install and Open").clicked() {
                        answer = Some(true);
                    }
                });
            });
            match answer {
                Some(false) => None,
                Some(true) => venv.map(|venv| start(ctx, path, new_project, steps, venv)),
                None => Some(AafInstall::Asking { path, new_project, python, venv, steps }),
            }
        }
        AafInstall::Running { path, new_project, lines, done, cancel } => {
            let finished = done.lock().map(|d| d.clone()).unwrap_or(None);
            window.show(ctx, |ui| {
                ui.set_max_width(520.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Installing OpenTimelineIO's AAF converter…");
                });
                output(ui, &lines.lock().map(|l| l.clone()).unwrap_or_default());
                if ui.add_enabled(!cancel.load(Ordering::Relaxed), egui::Button::new("Cancel")).clicked() {
                    cancel.store(true, Ordering::Relaxed);
                }
            });
            match finished {
                // On with what the file was being opened for.
                Some(Ok(())) => {
                    let request_id = uuid::Uuid::nil();
                    cmds.push(if new_project { AppCommand::OpenFromAnotherEditor { path, request_id } } else { AppCommand::ImportTimeline { path, request_id } });
                    None
                }
                Some(Err(why)) => Some(AafInstall::Failed { path, new_project, why, lines: lines.lock().map(|l| l.clone()).unwrap_or_default() }),
                None => Some(AafInstall::Running { path, new_project, lines, done, cancel }),
            }
        }
        AafInstall::Failed { path, new_project, why, lines } => {
            let mut answer = None;
            window.show(ctx, |ui| {
                ui.set_max_width(520.0);
                ui.label(egui::RichText::new("The converter wasn't installed").strong());
                ui.label(&why);
                egui::CollapsingHeader::new("What the commands said").default_open(false).show(ui, |ui| output(ui, &lines));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Close").clicked() {
                        answer = Some(false);
                    }
                    if ui.button("Try Again").clicked() {
                        answer = Some(true);
                    }
                });
            });
            match answer {
                Some(false) => None,
                Some(true) => Some(AafInstall::ask(path, new_project)),
                None => Some(AafInstall::Failed { path, new_project, why, lines }),
            }
        }
    };
    *slot = next;
}

/// The commands' output so far, newest at the bottom.
fn output(ui: &mut egui::Ui, lines: &[String]) {
    egui::ScrollArea::vertical().max_height(180.0).stick_to_bottom(true).show(ui, |ui| {
        for l in lines {
            ui.label(egui::RichText::new(l).monospace().small());
        }
    });
}

/// `steps` run on a thread of their own into `venv`, each line kept and a
/// frame asked for, the outcome kept when they are through. The
/// environment counts as installed only once they all finish and the
/// converter in it is then found with its AAF adapter.
fn start(ctx: &egui::Context, path: PathBuf, new_project: bool, steps: Vec<Vec<String>>, venv: PathBuf) -> AafInstall {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let done = Arc::new(Mutex::new(None));
    let cancel = Arc::new(AtomicBool::new(false));
    let (l, d, c, ctx) = (lines.clone(), done.clone(), cancel.clone(), ctx.clone());
    std::thread::spawn(move || {
        otio_tool::mark_complete(&venv, false);
        let outcome = otio_tool::run_steps(&steps, &c, |line| {
            if let Ok(mut l) = l.lock() {
                l.push(line);
            }
            ctx.request_repaint();
        })
        .map_err(|e| e.to_string())
        .and_then(|()| {
            otio_tool::mark_complete(&venv, true);
            otio_tool::forget();
            otio_tool::find().map(|_| ()).ok_or_else(|| "OpenTimelineIO installed, but it doesn't list its AAF adapter.".to_string())
        });
        if let Ok(mut d) = d.lock() {
            *d = Some(outcome);
        }
        ctx.request_repaint();
    });
    AafInstall::Running { path, new_project, lines, done, cancel }
}

/// An AAF being read through the converter, off the UI thread: the file,
/// who asked, whether it opens as a new project, and the outcome once in.
pub struct AafReading {
    pub path: PathBuf,
    pub request_id: uuid::Uuid,
    pub new_project: bool,
    pub outcome: Arc<Mutex<Option<Result<klipp_project::mlt_import::ImportResult, String>>>>,
    /// Set to stop it: Cancel, or the app quitting.
    pub cancel: Arc<AtomicBool>,
}

impl AafReading {
    /// `path` read on a thread of its own, the converter looked for there
    /// too (it asks OpenTimelineIO, which takes a while); its window keeps
    /// frames coming until it is in.
    pub fn start(path: PathBuf, request_id: uuid::Uuid, new_project: bool) -> Self {
        let outcome = Arc::new(Mutex::new(None));
        let cancel = Arc::new(AtomicBool::new(false));
        let (o, p, c) = (outcome.clone(), path.clone(), cancel.clone());
        std::thread::spawn(move || {
            let read = klipp_project::import_report::load_aaf(&p, &c);
            if let Ok(mut o) = o.lock() {
                *o = Some(read);
            }
        });
        AafReading { path, request_id, new_project, outcome, cancel }
    }

    /// Stopped, the converter killed, waiting up to `wait` for it.
    pub fn stop(&self, wait: std::time::Duration) {
        self.cancel.store(true, Ordering::Relaxed);
        let until = std::time::Instant::now() + wait;
        while self.outcome.lock().map(|o| o.is_none()).unwrap_or(false) && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// The outcome, once it is in, taken.
    pub fn take(&self) -> Option<Result<klipp_project::mlt_import::ImportResult, String>> {
        self.outcome.lock().ok()?.take()
    }
}

/// The window shown while an AAF is read, looking again for its outcome
/// every tenth of a second; its Cancel stops the reading.
pub fn show_reading(ctx: &egui::Context, reading: &AafReading) {
    ctx.request_repaint_after(std::time::Duration::from_millis(100));
    egui::Window::new("Open AAF").collapsible(false).resizable(false).order(egui::Order::Foreground).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(format!("Reading {} with OpenTimelineIO…", reading.path.file_name().unwrap_or_default().to_string_lossy()));
        });
        if ui.add_enabled(!reading.cancel.load(Ordering::Relaxed), egui::Button::new("Cancel")).clicked() {
            reading.cancel.store(true, Ordering::Relaxed);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One frame of the window: the text it drew, with where.
    fn frame(ctx: &egui::Context, slot: &mut Option<AafInstall>, events: Vec<egui::Event>) -> (Vec<(String, egui::Pos2)>, Vec<AppCommand>) {
        let mut cmds = Vec::new();
        let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 700.0))), events, ..Default::default() }, |ctx| show(ctx, slot, &mut cmds));
        (out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)), _ => None }).collect(), cmds)
    }

    /// The question names the commands it would run; with no Python it
    /// says so and offers no install; Cancel closes it, opening nothing.
    #[test]
    fn the_question_shows_its_commands_and_cancel_closes_it() {
        let ctx = egui::Context::default();
        let steps = vec![vec!["python3".to_string(), "-m".into(), "venv".into(), "/tmp/v".into()]];
        let mut slot = Some(AafInstall::Asking { path: "/tmp/a.aaf".into(), new_project: true, python: Some("python3".into()), venv: Some("/tmp/v".into()), steps });
        // A window is laid out on its first frame and drawn from its second.
        frame(&ctx, &mut slot, Vec::new());
        let (texts, _) = frame(&ctx, &mut slot, Vec::new());
        assert!(texts.iter().any(|(t, _)| t == "python3 -m venv /tmp/v") && texts.iter().any(|(t, _)| t == "Install and Open"), "{texts:?}");
        let mut none = Some(AafInstall::Asking { path: "/tmp/a.aaf".into(), new_project: true, python: None, venv: Some("/tmp/v".into()), steps: Vec::new() });
        let other = egui::Context::default();
        frame(&other, &mut none, Vec::new());
        let (texts, _) = frame(&other, &mut none, Vec::new());
        assert!(texts.iter().any(|(t, _)| t.starts_with("Python 3 isn't installed")) && !texts.iter().any(|(t, _)| t == "Install and Open"), "{texts:?}");
        let (texts, _) = frame(&ctx, &mut slot, Vec::new());
        let cancel = texts.iter().find(|(t, _)| t == "Cancel").unwrap().1;
        frame(&ctx, &mut slot, vec![egui::Event::PointerMoved(cancel)]);
        frame(&ctx, &mut slot, vec![egui::Event::PointerButton { pos: cancel, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE }]);
        let (_, cmds) = frame(&ctx, &mut slot, vec![egui::Event::PointerButton { pos: cancel, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }]);
        assert!(slot.is_none() && cmds.is_empty());
    }

    /// A finished install opens the file it was for, as it was being
    /// opened: as a new project, or into this one.
    #[test]
    fn a_finished_install_opens_the_file() {
        let ctx = egui::Context::default();
        let mut slot = Some(AafInstall::Running { path: "/tmp/a.aaf".into(), new_project: true, lines: Arc::new(Mutex::new(vec!["Successfully installed".into()])), done: Arc::new(Mutex::new(Some(Ok(())))), cancel: Arc::new(AtomicBool::new(false)) });
        let (_, cmds) = frame(&ctx, &mut slot, Vec::new());
        assert!(slot.is_none());
        assert!(matches!(&cmds[..], [AppCommand::OpenFromAnotherEditor { path, request_id }] if path == std::path::Path::new("/tmp/a.aaf") && request_id.is_nil()));
        let mut into = Some(AafInstall::Running { path: "/tmp/a.aaf".into(), new_project: false, lines: Arc::new(Mutex::new(Vec::new())), done: Arc::new(Mutex::new(Some(Ok(())))), cancel: Arc::new(AtomicBool::new(false)) });
        let (_, cmds) = frame(&ctx, &mut into, Vec::new());
        assert!(matches!(&cmds[..], [AppCommand::ImportTimeline { .. }]), "{cmds:?}");
    }

    /// Cancel stops a running install; with no data folder there is
    /// nothing to install into, and it says so.
    #[test]
    fn cancel_stops_an_install_and_no_data_folder_is_said() {
        let ctx = egui::Context::default();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut slot = Some(AafInstall::Running { path: "/tmp/a.aaf".into(), new_project: true, lines: Arc::new(Mutex::new(Vec::new())), done: Arc::new(Mutex::new(None)), cancel: cancel.clone() });
        assert!(slot.as_ref().unwrap().is_running());
        frame(&ctx, &mut slot, Vec::new());
        let (texts, _) = frame(&ctx, &mut slot, Vec::new());
        let at = texts.iter().find(|(t, _)| t == "Cancel").expect("Cancel").1;
        frame(&ctx, &mut slot, vec![egui::Event::PointerMoved(at)]);
        frame(&ctx, &mut slot, vec![egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE }]);
        frame(&ctx, &mut slot, vec![egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }]);
        assert!(cancel.load(Ordering::Relaxed));
        let other = egui::Context::default();
        let mut nowhere = Some(AafInstall::Asking { path: "/tmp/a.aaf".into(), new_project: true, python: Some("python3".into()), venv: None, steps: Vec::new() });
        frame(&other, &mut nowhere, Vec::new());
        let (texts, _) = frame(&other, &mut nowhere, Vec::new());
        assert!(texts.iter().any(|(t, _)| t.starts_with("This system has no data folder")) && !texts.iter().any(|(t, _)| t == "Install and Open"), "{texts:?}");
    }
}
