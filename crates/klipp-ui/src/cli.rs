//! What `klipp` does with its command line: `--version` and `--help`
//! print and exit, which is what the bug report form asks
//! `klipp --version` for, and paths open at startup — a project folder
//! opens, a media file is imported. That is what the desktop entry's
//! `Exec=klipp %F` passes for "Open With Klipp" and a double-click.
//!
//! Opening goes through the UI's own command dispatcher, like a click
//! would: `OpenProject` and `ImportMedia` are UI commands, and the
//! channel to the engine drops them (#395). [`StartupOpen`] is the
//! sequence — open the project, then import the media into it once it
//! is open — kept free of the app so it can be tested.
//!
//! Deliberately a few matches, not a framework: there is nothing else to
//! parse.

use std::path::PathBuf;

/// What the command line asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cli {
    /// Start, opening these paths (none: start as the preferences say).
    Run(Vec<PathBuf>),
    Version,
    Help,
    /// An option Klipp does not have, to say so and exit.
    Unknown(String),
}

/// Read the arguments after the program name.
pub fn parse(args: impl IntoIterator<Item = std::ffi::OsString>) -> Cli {
    let mut paths = Vec::new();
    let mut only_paths = false;
    for arg in args {
        if !only_paths {
            match arg.to_str() {
                Some("--version" | "-V") => return Cli::Version,
                Some("--help" | "-h") => return Cli::Help,
                // Everything after `--` is a path, even one starting "-".
                Some("--") => {
                    only_paths = true;
                    continue;
                }
                Some(s) if s.starts_with('-') && s.len() > 1 => return Cli::Unknown(s.to_string()),
                _ => {}
            }
        }
        // Absolute now, against the directory Klipp was started in: the
        // project's path is compared, stored in the recents and shown.
        let path = PathBuf::from(arg);
        paths.push(std::path::absolute(&path).unwrap_or(path));
    }
    Cli::Run(paths)
}

pub const USAGE: &str = "\
Usage: klipp [OPTIONS] [PATH]...

Open Klipp, and with it each PATH: a project folder (name.klipp) is
opened, a media file is imported into the project that is open.

Options:
  -V, --version   Print the version and build, and exit
  -h, --help      Print this help, and exit";

static STARTUP_PATHS: std::sync::OnceLock<Vec<PathBuf>> = std::sync::OnceLock::new();

/// When this process was started as a transcription worker (#640), run
/// the job and give the exit code; `None` for a normal start. Checked
/// first, before anything of the app's is set up.
pub fn transcription_worker() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new(klipp_analysis::transcript::WORKER_FLAG)) {
        return None;
    }
    let spec = args.next().map(|a| a.to_string_lossy().into_owned()).unwrap_or_default();
    Some(klipp_analysis::transcript::worker_main(&spec))
}

/// Run transcriptions in a worker of this executable, so a crash in
/// whisper.cpp ends the worker and not the editor (#640).
pub fn use_transcription_worker() {
    if let Ok(exe) = std::env::current_exe() {
        klipp_analysis::transcript::run_in_worker(exe);
    }
}

/// The paths to open at startup, given before the app is made.
pub fn set_startup_paths(paths: Vec<PathBuf>) {
    let _ = STARTUP_PATHS.set(paths);
}

/// The paths given at startup, if any.
pub fn startup_paths() -> &'static [PathBuf] {
    STARTUP_PATHS.get().map(Vec::as_slice).unwrap_or(&[])
}

/// Whether `path` is a Klipp project: a folder holding `project.json`,
/// or one named `*.klipp`.
pub fn is_project(path: &std::path::Path) -> bool {
    path.is_dir() && (path.join("project.json").is_file() || path.extension().is_some_and(|e| e == "klipp"))
}

/// What to do on a step of [`StartupOpen`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Open this project, through the UI's dispatcher.
    Open(PathBuf),
    /// Start an empty project to import into.
    NewProject,
    /// Import this media file into the project that is open.
    Import(PathBuf),
    /// Say this.
    Toast(String),
}

/// Opening what was given at startup (or the last project, or a dropped
/// folder): the project first, then, once it is open, the media — into
/// the project the user asked for, not into whatever was open before it.
#[derive(Debug, Clone, Default)]
pub struct StartupOpen {
    project: Option<PathBuf>,
    media: Vec<PathBuf>,
    problems: Vec<String>,
    asked: bool,
    frames_waited: u32,
}

/// Frames to wait for a project to open before giving up on the media
/// queued for it (a lock prompt cancelled, say): about half a minute.
const WAIT_FRAMES: u32 = 1800;

impl StartupOpen {
    /// Sort `paths`: the first project, the media files, and what cannot
    /// be opened, each named.
    pub fn plan(paths: &[PathBuf]) -> Self {
        let mut open = Self::default();
        let mut extra = Vec::new();
        let mut missing = Vec::new();
        let mut unknown = Vec::new();
        for p in paths {
            let name = p.display().to_string();
            if is_project(p) {
                if open.project.is_none() {
                    open.project = Some(p.clone());
                } else {
                    extra.push(name);
                }
            } else if !p.exists() {
                missing.push(name);
            } else if p.is_file() && klipp_core::media_extensions::path_is_media(p) {
                open.media.push(p.clone());
            } else {
                unknown.push(name);
            }
        }
        if !missing.is_empty() {
            open.problems.push(format!("Not found: {}", missing.join(", ")));
        }
        if !unknown.is_empty() {
            open.problems.push(format!("Not a project or a media file Klipp reads: {}", unknown.join(", ")));
        }
        if !extra.is_empty() {
            open.problems.push(format!("One project opens at a time, so these were not opened: {}", extra.join(", ")));
        }
        open
    }

    /// A single project, as the startup preference or a drop gives it.
    pub fn project(path: PathBuf) -> Self {
        Self::plan(&[path])
    }

    /// Whether there is anything left to do.
    pub fn done(&self) -> bool {
        self.asked && self.project.is_none() && self.media.is_empty() && self.problems.is_empty()
    }

    /// This frame's steps. `open_now` is the project open now; `failed` is
    /// the error of an open of `project` that has just failed.
    pub fn step(&mut self, open_now: Option<&std::path::Path>, failed: Option<&str>) -> Vec<Step> {
        let mut steps: Vec<Step> = std::mem::take(&mut self.problems).into_iter().map(Step::Toast).collect();
        if !self.asked {
            self.asked = true;
            match &self.project {
                Some(p) => steps.push(Step::Open(p.clone())),
                None if !self.media.is_empty() => {
                    if open_now.is_none() {
                        steps.push(Step::NewProject);
                    }
                    steps.extend(self.media.drain(..).map(Step::Import));
                }
                None => {}
            }
            return steps;
        }
        let Some(project) = self.project.clone() else { return steps };
        if open_now == Some(project.as_path()) {
            self.project = None;
            steps.extend(self.media.drain(..).map(Step::Import));
        } else if let Some(why) = failed {
            self.project = None;
            let dropped = std::mem::take(&mut self.media);
            steps.push(Step::Toast(if dropped.is_empty() {
                format!("Could not open {}: {why}", project.display())
            } else {
                format!("Could not open {}: {why}. The {} file(s) given with it were not imported", project.display(), dropped.len())
            }));
        } else {
            self.frames_waited += 1;
            if self.frames_waited >= WAIT_FRAMES {
                self.project = None;
                let dropped = std::mem::take(&mut self.media);
                if !dropped.is_empty() {
                    steps.push(Step::Toast(format!(
                        "{} did not open, so the {} file(s) given with it were not imported",
                        project.display(),
                        dropped.len()
                    )));
                }
            }
        }
        steps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<std::ffi::OsString> {
        a.iter().map(Into::into).collect()
    }

    #[test]
    fn version_and_help_win_wherever_they_are() {
        assert_eq!(parse(args(&["--version"])), Cli::Version);
        assert_eq!(parse(args(&["-V"])), Cli::Version);
        assert_eq!(parse(args(&["a.mov", "--help"])), Cli::Help);
    }

    #[test]
    fn paths_are_kept_in_order_and_dashes_after_double_dash_are_paths() {
        let here = std::env::current_dir().unwrap();
        assert_eq!(parse(args(&[])), Cli::Run(vec![]));
        assert_eq!(parse(args(&["x.klipp", "a.mov"])), Cli::Run(vec![here.join("x.klipp"), here.join("a.mov")]));
        assert_eq!(parse(args(&["--", "-odd.mov"])), Cli::Run(vec![here.join("-odd.mov")]));
        assert_eq!(parse(args(&["-"])), Cli::Run(vec![here.join("-")]), "a lone dash is a name, not an option");
    }

    #[test]
    fn an_option_klipp_does_not_have_is_named() {
        assert_eq!(parse(args(&["--frobnicate"])), Cli::Unknown("--frobnicate".into()));
    }

    #[test]
    fn paths_are_made_absolute_against_where_klipp_started() {
        let Cli::Run(paths) = parse(args(&["film.klipp"])) else { panic!() };
        assert!(paths[0].is_absolute());
        assert!(paths[0].ends_with("film.klipp"));
    }

    /// Each test its own folder: they run in parallel, and one's cleanup
    /// must not take another's files away.
    fn fixture(test: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("klipp-startup-{test}-{}", std::process::id()));
        let project = root.join("Film.klipp");
        std::fs::create_dir_all(&project).unwrap();
        let clip = root.join("take.mov");
        std::fs::write(&clip, b"x").unwrap();
        (root, project, clip)
    }

    #[test]
    fn a_startup_project_opens_first_and_its_media_waits_for_it() {
        let (root, project, clip) = fixture("opens-first");
        let mut open = StartupOpen::plan(&[clip.clone(), project.clone()]);
        // Frame one: open the project; nothing imported into whatever is open.
        assert_eq!(open.step(None, None), vec![Step::Open(project.clone())]);
        // Still opening (a lock prompt, autosave recovery): nothing yet.
        assert_eq!(open.step(Some(std::path::Path::new("/other.klipp")), None), vec![]);
        // Open: now the media, into it.
        assert_eq!(open.step(Some(&project), None), vec![Step::Import(clip.clone())]);
        assert!(open.done());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn media_alone_goes_into_an_empty_project_and_every_problem_is_said() {
        let (root, project, clip) = fixture("media-alone");
        let second = root.join("Other.klipp");
        std::fs::create_dir_all(&second).unwrap();
        let mut open = StartupOpen::plan(&[clip.clone(), root.join("gone.mov"), root.clone()]);
        let steps = open.step(None, None);
        assert!(matches!(&steps[0], Step::Toast(t) if t.starts_with("Not found")), "{steps:?}");
        assert!(matches!(&steps[1], Step::Toast(t) if t.starts_with("Not a project")), "{steps:?}");
        assert_eq!(&steps[2..], &[Step::NewProject, Step::Import(clip.clone())]);

        let mut two = StartupOpen::plan(&[project.clone(), second.clone()]);
        let steps = two.step(None, None);
        assert!(matches!(&steps[0], Step::Toast(t) if t.contains("One project opens at a time")), "{steps:?}");
        assert_eq!(steps[1], Step::Open(project.clone()));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_failed_open_says_so_and_what_was_left_out() {
        let (root, project, clip) = fixture("failed-open");
        let mut open = StartupOpen::plan(&[project.clone(), clip]);
        open.step(None, None);
        let steps = open.step(None, Some("project.json is not valid"));
        assert!(matches!(&steps[..], [Step::Toast(t)] if t.contains("project.json is not valid") && t.contains("1 file(s)")), "{steps:?}");
        assert!(open.done());
        std::fs::remove_dir_all(&root).ok();
    }
}
