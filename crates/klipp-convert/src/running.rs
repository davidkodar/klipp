//! Whether Klipp is running, which the conversion must refuse to run
//! beside (#1054): a running app holds a project in memory and saves it
//! back in the old format over the converted one.
//!
//! Two signs, either enough: a process of any build (the installed
//! `klipp`, a development `klipp-app`), or a control socket that answers.
//! The socket covers an app under another name or in another user
//! namespace the process list cannot see.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// The executables that are the app.
pub const APP_NAMES: &[&str] = &["klipp", "klipp-app"];

/// What was found running.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Running {
    /// Process ids of app processes.
    pub processes: Vec<u32>,
    /// Control sockets that answered.
    pub sockets: Vec<PathBuf>,
}

impl Running {
    pub fn is_running(&self) -> bool {
        !self.processes.is_empty() || !self.sockets.is_empty()
    }
}

/// The app's processes among those in `proc_dir` (`/proc`): each whose
/// executable's name, or failing that its `comm`, is one of [`APP_NAMES`].
/// A `proc_dir` that can't be listed is an error, never "none running": the
/// conversion must refuse when it can't tell.
pub fn app_processes(proc_dir: &Path) -> Result<Vec<u32>, String> {
    let es = std::fs::read_dir(proc_dir).map_err(|e| format!("listing {}: {e}", proc_dir.display()))?;
    let mut out: Vec<u32> = es
        .flatten()
        .filter_map(|e| {
            let pid: u32 = e.file_name().to_str()?.parse().ok()?;
            let dir = e.path();
            let name = std::fs::read_link(dir.join("exe"))
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                // A deleted executable reads "name (deleted)".
                .map(|n| n.trim_end_matches(" (deleted)").to_string())
                .or_else(|| std::fs::read_to_string(dir.join("comm")).ok().map(|c| c.trim().to_string()))?;
            APP_NAMES.contains(&name.as_str()).then_some(pid)
        })
        .collect();
    out.sort_unstable();
    Ok(out)
}

/// The control sockets in `runtime_dir` that answer: `klipp*.sock`, the
/// app's own and the per-instance ones a second app binds, plus
/// `override_path` (`KLIPP_CONTROL_SOCKET`). A runtime folder that isn't
/// there holds no socket; one that can't be listed is an error.
pub fn answering_sockets(runtime_dir: Option<&Path>, override_path: Option<&Path>) -> Result<Vec<PathBuf>, String> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = runtime_dir {
        match std::fs::read_dir(dir) {
            Ok(es) => candidates.extend(es.flatten().map(|e| e.path()).filter(|p| {
                let n = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                n.starts_with("klipp") && n.ends_with(".sock")
            })),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("listing {}: {e}", dir.display())),
        }
    }
    candidates.extend(override_path.map(Path::to_path_buf));
    candidates.sort();
    candidates.dedup();
    Ok(candidates.into_iter().filter(|p| UnixStream::connect(p).is_ok()).collect())
}

/// Whether Klipp is running on this machine, for this user: an error when
/// it can't be told.
pub fn running() -> Result<Running, String> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    let over = std::env::var_os("KLIPP_CONTROL_SOCKET").map(PathBuf::from);
    Ok(Running { processes: app_processes(Path::new("/proc"))?, sockets: answering_sockets(runtime.as_deref(), over.as_deref())? })
}
