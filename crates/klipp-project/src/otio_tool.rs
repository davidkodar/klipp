//! OpenTimelineIO's converter, for the formats Klipp reaches through it:
//! AAF above all (#2086). A converter already on the system is used when
//! it has the AAF adapter; else one Klipp installed for itself, in a
//! Python environment of its own under its data folder, since most
//! systems refuse `pip install --user` into theirs. Installing is never
//! done unasked: [`install_steps`] are the exact commands, shown to the
//! person, and [`run_steps`] runs them with their output passed on line
//! by line, a failure told in a sentence rather than a traceback.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The packages the AAF converter is: OpenTimelineIO and its AAF adapter,
/// at the releases that read AAF as Klipp expects.
pub const PACKAGES: [&str; 2] = ["opentimelineio>=0.17", "otio-aaf-adapter>=1.0"];

/// The adapter's name, as OpenTimelineIO lists it and as it is asked for.
pub const ADAPTER: &str = "AAF";

/// How long a converter may take to say what it has, and to convert.
const INFO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
pub const CONVERT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// The file a finished install leaves in the environment: one without it
/// was stopped part way and is not used.
const COMPLETE: &str = ".klipp-complete";

/// A converter that can read AAF: the program, and whether it is the one
/// Klipp installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Converter {
    pub program: PathBuf,
    pub owned: bool,
}

/// Where Klipp keeps its own Python environment for the converter, under
/// its data folder; `None` on a system with no data folder, where nothing
/// is installed.
pub fn venv_dir() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("klipp/tools/otio-venv"))
}

/// `name` in the folders of `search_path` (a `PATH`), the first that is a
/// file.
fn on_path(name: &str, search_path: Option<&OsStr>) -> Option<PathBuf> {
    std::env::split_paths(search_path?).map(|dir| dir.join(name)).find(|p| p.is_file())
}

/// `program` with `args`, its output, given `timeout` to finish (killed
/// past it); `None` when it can't start or runs over.
fn run_for(program: &Path, args: &[&OsStr], timeout: std::time::Duration) -> Option<std::process::Output> {
    let mut child = std::process::Command::new(program).args(args).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().ok()?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Whether OpenTimelineIO's plug-in listing names the AAF adapter: the
/// name as a word of its own, not inside another's.
fn lists_aaf(info: &str) -> bool {
    info.lines().any(|l| l.split(|c: char| !c.is_ascii_alphanumeric() && c != '_').any(|w| w == ADAPTER))
}

/// Whether the OpenTimelineIO next to `converter` lists the AAF adapter:
/// its `otiopluginfo`, beside it, names it.
fn has_aaf_adapter(converter: &Path) -> bool {
    let info = converter.with_file_name("otiopluginfo");
    run_for(&info, &[], INFO_TIMEOUT).is_some_and(|ran| ran.status.success() && lists_aaf(&String::from_utf8_lossy(&ran.stdout)))
}

/// The converter to read AAF with: one on `search_path` that has the AAF
/// adapter first, else the one in Klipp's own environment `venv`; `None`
/// when neither is there.
pub fn find_in(search_path: Option<&OsStr>, venv: &Path) -> Option<Converter> {
    if let Some(program) = on_path("otioconvert", search_path).filter(|p| has_aaf_adapter(p)) {
        return Some(Converter { program, owned: false });
    }
    let program = venv.join("bin/otioconvert");
    (venv.join(COMPLETE).is_file() && program.is_file() && has_aaf_adapter(&program)).then_some(Converter { program, owned: true })
}

/// What [`find`] found this session, asked once.
static FOUND: std::sync::Mutex<Option<Option<Converter>>> = std::sync::Mutex::new(None);

/// [`find_in`] on this system's `PATH` and Klipp's environment, looked for
/// once a session (each look runs `otiopluginfo`); [`forget`] looks again.
pub fn find() -> Option<Converter> {
    let mut found = FOUND.lock().unwrap_or_else(|e| e.into_inner());
    found.get_or_insert_with(|| venv_dir().and_then(|venv| find_in(std::env::var_os("PATH").as_deref(), &venv))).clone()
}

/// The converter looked for again at the next [`find`]: after an install.
pub fn forget() {
    *FOUND.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// `venv` marked as a finished install, or as not one.
pub fn mark_complete(venv: &Path, complete: bool) {
    let marker = venv.join(COMPLETE);
    if complete {
        let _ = std::fs::write(marker, b"");
    } else {
        let _ = std::fs::remove_file(marker);
    }
}

/// The Python to make Klipp's environment with: `python3`, else `python`,
/// on `search_path`.
pub fn find_python(search_path: Option<&OsStr>) -> Option<PathBuf> {
    on_path("python3", search_path).or_else(|| on_path("python", search_path))
}

/// The commands that install the converter into `venv` with `python`: the
/// environment made, then the packages installed into it with its own pip.
/// Each is shown as it will run.
pub fn install_steps(python: &Path, venv: &Path) -> Vec<Vec<String>> {
    let venv_python = venv.join("bin/python");
    let mut pip = vec![venv_python.display().to_string(), "-m".into(), "pip".into(), "install".into()];
    pip.extend(PACKAGES.iter().map(|p| p.to_string()));
    // `--clear`: an environment left broken (a Python upgrade, a stopped
    // install) is made afresh.
    vec![vec![python.display().to_string(), "-m".into(), "venv".into(), "--clear".into(), venv.display().to_string()], pip]
}

/// One step as the person reads it, the way a shell would take it.
pub fn shown(step: &[String]) -> String {
    step.iter().map(|a| if a.contains(' ') { format!("'{a}'") } else { a.clone() }).collect::<Vec<_>>().join(" ")
}

/// Why an install did not finish, in words.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InstallError {
    #[error("Python 3 isn't installed. Install it from your system's packages, then try again.")]
    NoPython,
    #[error("Python can't make the converter's own folder: its venv module is missing. Install your system's python3-venv package (or python with venv), then try again.")]
    NoVenv,
    #[error("pip couldn't download OpenTimelineIO: check the internet connection, then try again.")]
    Offline,
    #[error("The install was stopped.")]
    Cancelled,
    #[error("This system has no data folder for Klipp to install the converter in.")]
    NoDataFolder,
    #[error("{0}")]
    Failed(String),
}

/// What a failed step said worth repeating: its last line that is not part
/// of a Python traceback.
fn last_said(output: &[String]) -> String {
    output
        .iter()
        .rev()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.starts_with("Traceback") && !l.starts_with("File \"") && !l.starts_with('^') && !l.starts_with("~"))
        .unwrap_or("no message")
        .to_string()
}

/// A failed step's output told as an [`InstallError`]: the cause where it
/// is a known one, else the step and what it last said.
fn explain(step_index: usize, step: &[String], output: &[String]) -> InstallError {
    let all = output.join("\n").to_lowercase();
    if step_index == 0 && (all.contains("ensurepip") || all.contains("no module named venv")) {
        return InstallError::NoVenv;
    }
    let offline = ["temporary failure in name resolution", "network is unreachable", "failed to establish a new connection", "connection timed out", "could not fetch url", "no matching distribution found"];
    if offline.iter().any(|w| all.contains(w)) {
        return InstallError::Offline;
    }
    InstallError::Failed(format!("`{}` stopped: {}", shown(step), last_said(output)))
}

/// `steps` run in turn, each line they write passed to `line` as it comes,
/// stopping at the first that fails, told as an [`InstallError`]. Once
/// `cancel` is set the running step is killed and it stops, `Cancelled`.
pub fn run_steps(steps: &[Vec<String>], cancel: &std::sync::atomic::AtomicBool, mut line: impl FnMut(String)) -> Result<(), InstallError> {
    use std::io::{BufRead, BufReader};
    for (i, step) in steps.iter().enumerate() {
        line(format!("$ {}", shown(step)));
        let child = std::process::Command::new(&step[0])
            .args(&step[1..])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn();
        let mut child = match child {
            Ok(child) => child,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && i == 0 => return Err(InstallError::NoPython),
            Err(e) => return Err(InstallError::Failed(format!("`{}` could not start: {e}", shown(step)))),
        };
        // Both streams, as they come, through one channel.
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let readers: Vec<_> = [child.stdout.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>), child.stderr.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>)]
            .into_iter()
            .flatten()
            .map(|stream| {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    for l in BufReader::new(stream).lines().map_while(Result::ok) {
                        if tx.send(l).is_err() {
                            break;
                        }
                    }
                })
            })
            .collect();
        drop(tx);
        let mut output = Vec::new();
        // Lines as they come, the step watched for its end and for a stop.
        let status = loop {
            while let Ok(l) = rx.recv_timeout(std::time::Duration::from_millis(50)) {
                line(l.clone());
                output.push(l);
            }
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(InstallError::Cancelled);
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(e) => return Err(InstallError::Failed(e.to_string())),
            }
        };
        for l in rx.try_iter() {
            line(l.clone());
            output.push(l);
        }
        for r in readers {
            let _ = r.join();
        }
        if !status.success() {
            return Err(explain(i, step, &output));
        }
    }
    Ok(())
}

/// Why a conversion failed, in words.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConvertError {
    #[error("OpenTimelineIO's converter could not start: {0}")]
    NotRun(String),
    #[error("OpenTimelineIO could not read the AAF: {0}")]
    Failed(String),
}

/// `aaf` converted to the OpenTimelineIO file `out` with `converter`, read
/// with the AAF adapter by name whatever the file is called, given
/// [`CONVERT_TIMEOUT`]. A failed conversion leaves no `out` behind.
pub fn convert(converter: &Converter, aaf: &Path, out: &Path) -> Result<(), ConvertError> {
    let args: [&OsStr; 6] = ["-i".as_ref(), aaf.as_os_str(), "-o".as_ref(), out.as_os_str(), "--input-adapter".as_ref(), ADAPTER.as_ref()];
    let outcome = match run_for(&converter.program, &args, CONVERT_TIMEOUT) {
        None => Err(ConvertError::NotRun(format!("it didn't finish within {} minutes, or couldn't start", CONVERT_TIMEOUT.as_secs() / 60))),
        Some(ran) if ran.status.success() => Ok(()),
        Some(ran) => {
            let said: Vec<String> = String::from_utf8_lossy(&ran.stderr).lines().map(str::to_string).collect();
            Err(ConvertError::Failed(last_said(&said)))
        }
    };
    if outcome.is_err() {
        let _ = std::fs::remove_file(out);
    }
    outcome
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A folder of its own for one test.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("klipp-otio-tool-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A shell script `name` in `dir` that runs `body`.
    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// With nothing installed there is no converter; a system one without
    /// the AAF adapter is passed over; one with it is used.
    #[test]
    fn a_system_converter_with_the_aaf_adapter_is_found_first() {
        let bin = scratch("system");
        let venv = bin.join("venv");
        assert_eq!(find_in(Some(bin.as_os_str()), &venv), None);
        script(&bin, "otioconvert", "exit 0");
        script(&bin, "otiopluginfo", "echo 'adapters: otio_json, fcp_xml, aaf_like_thing'");
        assert_eq!(find_in(Some(bin.as_os_str()), &venv), None, "no AAF adapter, only a name with aaf in it");
        script(&bin, "otiopluginfo", "echo 'adapters: otio_json, AAF'");
        assert_eq!(find_in(Some(bin.as_os_str()), &venv), Some(Converter { program: bin.join("otioconvert"), owned: false }));
    }

    /// The install, with a stand-in Python on the path: the environment
    /// made, the packages installed with its own pip, each line passed on,
    /// and then the converter in it found.
    #[test]
    fn the_converter_installs_into_klipps_own_environment() {
        let bin = scratch("install");
        let venv = bin.join("tools/otio-venv");
        // `python3 -m venv DIR` makes DIR/bin/python, whose `-m pip install`
        // puts otioconvert and otiopluginfo beside it.
        let venv_python = "#!/bin/sh\necho Collecting opentimelineio\nd=$(dirname \"$0\")\nprintf '#!/bin/sh\\nexit 0\\n' > \"$d/otioconvert\"\nprintf '#!/bin/sh\\necho AAF\\n' > \"$d/otiopluginfo\"\nchmod +x \"$d/otioconvert\" \"$d/otiopluginfo\"\necho Successfully installed\n";
        let python = script(&bin, "python3", &format!("mkdir -p \"$4/bin\"\ncat > \"$4/bin/python\" <<'PY'\n{venv_python}PY\nchmod +x \"$4/bin/python\"\necho made the environment"));
        assert_eq!(find_python(Some(bin.as_os_str())), Some(python.clone()));
        let steps = install_steps(&python, &venv);
        assert_eq!(steps[0][1..3], ["-m".to_string(), "venv".to_string()]);
        assert!(steps[0].contains(&"--clear".to_string()));
        assert!(steps[1].ends_with(&["opentimelineio>=0.17".to_string(), "otio-aaf-adapter>=1.0".to_string()]) && steps[1][0].ends_with("otio-venv/bin/python"));
        let mut said = Vec::new();
        run_steps(&steps, &std::sync::atomic::AtomicBool::new(false), |l| said.push(l)).expect("installed");
        assert!(said.iter().any(|l| l == "Successfully installed") && said[0].starts_with("$ "), "{said:?}");
        assert_eq!(find_in(None, &venv), None, "not used until marked finished");
        mark_complete(&venv, true);
        assert_eq!(find_in(None, &venv), Some(Converter { program: venv.join("bin/otioconvert"), owned: true }));
    }

    /// A failure is told in a sentence, never as a traceback: no Python, no
    /// venv module, no network, or the step and what it last said.
    #[test]
    fn a_failed_install_explains_itself() {
        let bin = scratch("fail");
        let venv = bin.join("venv");
        assert_eq!(run_steps(&install_steps(&bin.join("no-python3"), &venv), &std::sync::atomic::AtomicBool::new(false), |_| {}), Err(InstallError::NoPython));
        let python = script(&bin, "python3", "echo 'Error: Command returned non-zero exit status 1. ensurepip is not available' >&2\nexit 1");
        assert_eq!(run_steps(&install_steps(&python, &venv), &std::sync::atomic::AtomicBool::new(false), |_| {}), Err(InstallError::NoVenv));
        let pip = script(&bin, "pip-offline", "echo 'Traceback (most recent call last):' >&2\necho '  File \"x.py\", line 1' >&2\necho 'ERROR: No matching distribution found for opentimelineio' >&2\nexit 1");
        assert_eq!(run_steps(&[vec![pip.display().to_string()]], &std::sync::atomic::AtomicBool::new(false), |_| {}), Err(InstallError::Offline));
        let other = script(&bin, "pip-broken", "echo 'Traceback (most recent call last):' >&2\necho '  File \"x.py\", line 1' >&2\necho 'PermissionError: denied' >&2\nexit 1");
        match run_steps(&[vec![other.display().to_string()]], &std::sync::atomic::AtomicBool::new(false), |_| {}) {
            Err(InstallError::Failed(text)) => assert!(text.contains("PermissionError: denied") && !text.contains("Traceback"), "{text}"),
            other => panic!("{other:?}"),
        }
    }

    /// A stopped install kills the step running, and says so.
    #[test]
    fn a_stopped_install_kills_its_step() {
        let bin = scratch("cancel");
        let slow = script(&bin, "slow", "echo started\nsleep 30");
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let c = cancel.clone();
        let started = std::time::Instant::now();
        let outcome = run_steps(&[vec![slow.display().to_string()]], &cancel, |l| {
            if l == "started" {
                c.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        });
        assert_eq!(outcome, Err(InstallError::Cancelled));
        assert!(started.elapsed() < std::time::Duration::from_secs(10), "killed, not waited for");
    }

    /// A failed conversion leaves no partial file, and the converter is
    /// asked for the AAF adapter by name.
    #[test]
    fn a_conversion_names_the_adapter_and_cleans_up() {
        let bin = scratch("convert");
        let out = bin.join("out.otio");
        let args = bin.join("args");
        let conv = script(&bin, "otioconvert", &format!("echo \"$@\" > '{}'\necho partial > '{}'\necho 'Traceback' >&2\necho 'ValueError: bad AAF' >&2\nexit 1", args.display(), out.display()));
        let got = convert(&Converter { program: conv, owned: false }, &bin.join("cut.bin"), &out);
        assert_eq!(got, Err(ConvertError::Failed("ValueError: bad AAF".into())));
        assert!(!out.exists(), "the partial file removed");
        assert!(std::fs::read_to_string(args).unwrap().contains("--input-adapter AAF"));
    }
}
