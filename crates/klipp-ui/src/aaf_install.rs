//! Opening an AAF file with no converter to read it (#2086): the person
//! is told what is missing and asked whether Klipp may install it into a
//! Python environment of its own, shown the exact commands; with their
//! consent the commands run, their output shown as it comes, and when
//! they finish the file opens. A failure is told in a sentence.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use klipp_core::AppCommand;
use klipp_project::otio_tool;

/// Where the install is.
pub enum AafInstall {
    /// Asking, the commands shown; `python` is `None` when there is no
    /// Python to make the environment with.
    Asking { path: PathBuf, python: Option<PathBuf>, steps: Vec<Vec<String>> },
    /// Running: the lines so far, and the outcome once it is in.
    Running { path: PathBuf, lines: Arc<Mutex<Vec<String>>>, done: Arc<Mutex<Option<Result<(), String>>>> },
    /// Stopped short: why, and what the commands said.
    Failed { path: PathBuf, why: String, lines: Vec<String> },
}

impl AafInstall {
    /// The question for `path`, with this system's Python and Klipp's
    /// environment.
    pub fn ask(path: PathBuf) -> Self {
        let python = otio_tool::find_python(std::env::var_os("PATH").as_deref());
        let steps = python.as_ref().map(|p| otio_tool::install_steps(p, &otio_tool::venv_dir())).unwrap_or_default();
        AafInstall::Asking { path, python, steps }
    }
}

/// The install's window, if there is one, for this frame. Its answer
/// moves it on; an install that finished opens the file through `cmds`.
pub fn show(ctx: &egui::Context, slot: &mut Option<AafInstall>, cmds: &mut Vec<AppCommand>) {
    let Some(current) = slot.take() else { return };
    let window = egui::Window::new("Open AAF").collapsible(false).resizable(false).order(egui::Order::Foreground).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]);
    let next = match current {
        AafInstall::Asking { path, python, steps } => {
            let mut answer = None;
            window.show(ctx, |ui| {
                ui.set_max_width(520.0);
                ui.label("Klipp opens AAF with OpenTimelineIO's AAF converter, which isn't installed.");
                ui.add_space(6.0);
                match &python {
                    None => {
                        ui.label(egui::RichText::new(otio_tool::InstallError::NoPython.to_string()).strong());
                    }
                    Some(_) => {
                        ui.label(format!("Klipp can install it in a folder of its own ({}), leaving your system's Python untouched, by running:", otio_tool::venv_dir().display()));
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
                    if python.is_some() && ui.button("Install and Open").clicked() {
                        answer = Some(true);
                    }
                });
            });
            match answer {
                Some(false) => None,
                Some(true) => Some(start(ctx, path, steps)),
                None => Some(AafInstall::Asking { path, python, steps }),
            }
        }
        AafInstall::Running { path, lines, done } => {
            let finished = done.lock().map(|d| d.clone()).unwrap_or(None);
            window.show(ctx, |ui| {
                ui.set_max_width(520.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Installing OpenTimelineIO's AAF converter…");
                });
                output(ui, &lines.lock().map(|l| l.clone()).unwrap_or_default());
            });
            match finished {
                Some(Ok(())) => {
                    cmds.push(AppCommand::OpenFromAnotherEditor { path, request_id: uuid::Uuid::nil() });
                    None
                }
                Some(Err(why)) => Some(AafInstall::Failed { path, why, lines: lines.lock().map(|l| l.clone()).unwrap_or_default() }),
                None => Some(AafInstall::Running { path, lines, done }),
            }
        }
        AafInstall::Failed { path, why, lines } => {
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
                Some(true) => Some(AafInstall::ask(path)),
                None => Some(AafInstall::Failed { path, why, lines }),
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

/// `steps` run on a thread of their own, each line kept and a frame asked
/// for, the outcome kept when they are through.
fn start(ctx: &egui::Context, path: PathBuf, steps: Vec<Vec<String>>) -> AafInstall {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let done = Arc::new(Mutex::new(None));
    let (l, d, ctx) = (lines.clone(), done.clone(), ctx.clone());
    std::thread::spawn(move || {
        let outcome = otio_tool::run_steps(&steps, |line| {
            if let Ok(mut l) = l.lock() {
                l.push(line);
            }
            ctx.request_repaint();
        });
        if let Ok(mut d) = d.lock() {
            *d = Some(outcome.map_err(|e| e.to_string()));
        }
        ctx.request_repaint();
    });
    AafInstall::Running { path, lines, done }
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
        let mut slot = Some(AafInstall::Asking { path: "/tmp/a.aaf".into(), python: Some("python3".into()), steps });
        // A window is laid out on its first frame and drawn from its second.
        frame(&ctx, &mut slot, Vec::new());
        let (texts, _) = frame(&ctx, &mut slot, Vec::new());
        assert!(texts.iter().any(|(t, _)| t == "python3 -m venv /tmp/v") && texts.iter().any(|(t, _)| t == "Install and Open"), "{texts:?}");
        let mut none = Some(AafInstall::Asking { path: "/tmp/a.aaf".into(), python: None, steps: Vec::new() });
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

    /// A finished install opens the file it was for.
    #[test]
    fn a_finished_install_opens_the_file() {
        let ctx = egui::Context::default();
        let mut slot = Some(AafInstall::Running { path: "/tmp/a.aaf".into(), lines: Arc::new(Mutex::new(vec!["Successfully installed".into()])), done: Arc::new(Mutex::new(Some(Ok(())))) });
        let (_, cmds) = frame(&ctx, &mut slot, Vec::new());
        assert!(slot.is_none());
        assert!(matches!(&cmds[..], [AppCommand::OpenFromAnotherEditor { path, request_id }] if path == std::path::Path::new("/tmp/a.aaf") && request_id.is_nil()));
    }
}
