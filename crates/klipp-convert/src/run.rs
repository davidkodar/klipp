//! The one-time conversion on disk (#1087): every project and the config
//! folder's colour presets, from revision 1 to revision 2, with a backup of
//! each and a journal that makes a crash at any point end either fully
//! rolled back or fully converted.
//!
//! The work in memory is the dry run's (#1066) and `revision2`'s (#1070);
//! this module is what touches disk. Per project, journalled in the backup
//! root:
//!
//! 1. the project folder's time is recorded;
//! 2. it is copied into `<dir>.klipp-convert`: media hard-linked (copied
//!    where a link can't be made), everything else copied with its mode,
//!    symlinks kept, and the caches, partial files, backups and recovery
//!    copy left out;
//! 3. the revision-2 files are written into the copy and read back;
//! 4. a render check may stop it (a hook; the check itself comes later);
//! 5. the swap: the original goes aside to `<dir>.klipp-v1`, the copy takes
//!    its place, and the original moves into the backup root;
//! 6. its auto-save folder follows it into the backup;
//! 7. the folder's time is put back.
//!
//! Until the original goes aside, a failure rolls back (the copy goes);
//! after it, the run only goes forward. Recovery reads the disk as well as
//! the journal, so a crash between a rename and its journal entry is found
//! either way.
//!
//! The media a converted project shares with its backup is hard-linked:
//! that is safe because every writer under `media/` renames a new file into
//! place (the manifest's and the stills' atomic writes), never writes one
//! in place. A writer that did would edit the backup too.
//!
//! The run is never pointed at real data by its tests: every path comes in
//! through [`Settings`], and the command line refuses to start without
//! `--confirm-real-run`.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::dry_run::{self, Parity};
use crate::{revision, revision2, scan};

/// Where a run reads and writes. The real run takes these from the
/// machine; the tests point every one into a temporary folder.
#[derive(Debug, Clone)]
pub struct Settings {
    /// The projects to convert.
    pub projects: Vec<PathBuf>,
    /// Projects left as they are, on purpose: a stop the user settled by
    /// excluding the project.
    pub excluded: BTreeSet<PathBuf>,
    /// The app's config folder (`~/.config/klipp`), for its colour presets.
    pub config_dir: Option<PathBuf>,
    /// Where backups and the journal go (`~/.local/share/klipp/pre-v2`).
    pub backup_root: PathBuf,
    /// The day of the run, naming its backup folder.
    pub date: String,
}

/// The run's hooks: a test's injected crash, and the render check that may
/// stop a project.
pub struct Hooks<'a> {
    /// Called after each step's work, before (`recorded == false`) and
    /// after (`true`) the journal records it; `true` stops the run there as
    /// a crash would, leaving the disk as it is.
    pub crash: &'a dyn Fn(Step, bool) -> bool,
    /// Called inside a move across file systems (to the folder it moves
    /// to); `true` stops the run there as a crash would.
    pub crash_in_move: &'a dyn Fn(MovePoint, &Path) -> bool,
    /// Checks the converted copy before the swap; an error stops the
    /// project, rolled back.
    pub render_check: &'a dyn Fn(&Path) -> Result<(), String>,
    /// Asked again just before each swap (the real run: that no Klipp has
    /// started since the run began); an error ends the run there, that
    /// project rolled back.
    pub swap_allowed: &'a dyn Fn() -> Result<(), String>,
}

impl Hooks<'_> {
    /// No crash, no render check, every swap allowed.
    pub const NONE: Hooks<'static> = Hooks { crash: &|_, _| false, crash_in_move: &|_, _| false, render_check: &|_| Ok(()), swap_allowed: &|| Ok(()) };
}

/// The points inside a move across file systems where a crash leaves the
/// most to recover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovePoint {
    /// The copy is whole and in place; the source not yet touched.
    InPlace,
    /// Part of the source removed.
    PartRemoved,
}

/// A project's step in the journal, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Step {
    Started,
    Copied,
    Written,
    Verified,
    /// The original renamed aside: from here the run only goes forward.
    OriginalAside,
    Swapped,
    BackedUp,
    AutosaveMoved,
    Done,
}

impl Step {
    pub const ALL: [Step; 9] = [
        Step::Started,
        Step::Copied,
        Step::Written,
        Step::Verified,
        Step::OriginalAside,
        Step::Swapped,
        Step::BackedUp,
        Step::AutosaveMoved,
        Step::Done,
    ];
}

/// One project's journal entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    dir: PathBuf,
    copy: PathBuf,
    aside: PathBuf,
    backup: PathBuf,
    autosave: PathBuf,
    autosave_backup: PathBuf,
    /// The folder's time when the run started on it, as seconds and
    /// nanoseconds since the epoch.
    mtime: (u64, u32),
    step: Step,
}

/// What a run did.
#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub converted: Vec<PathBuf>,
    /// Already at revision 2, with every store.
    pub skipped: Vec<PathBuf>,
    pub stopped: Vec<(PathBuf, String)>,
    pub excluded: Vec<PathBuf>,
    /// Journal entries finished from an earlier run that stopped.
    pub recovered: Vec<(PathBuf, String)>,
    /// Journal entries that were empty or wouldn't parse, moved into
    /// `journal/damaged`. The project each belonged to can't be known from
    /// it; the run's own checks stop any project a step left part way
    /// (a copy, an original aside or a backup already there).
    pub damaged_journals: Vec<PathBuf>,
    /// Media items whose file isn't there, per converted project.
    pub media_offline: Vec<(PathBuf, usize)>,
    /// Colour presets converted, and those that stopped.
    pub presets_converted: usize,
    pub presets_stopped: Vec<(PathBuf, String)>,
}

/// Why a run didn't happen or ended early.
#[derive(Debug, PartialEq, Eq)]
pub enum RunError {
    /// A precondition failed; nothing was touched.
    Refused(String),
    /// The crash hook stopped the run (tests only).
    Crashed(Step),
    /// The crash hook stopped the run inside a move (tests only).
    CrashedInMove(MovePoint),
    /// The disk refused something the run must not leave half done.
    Io(String),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Refused(why) => write!(f, "refused: {why}"),
            RunError::Crashed(step) => write!(f, "stopped as a crash would, at {step:?}"),
            RunError::CrashedInMove(point) => write!(f, "stopped as a crash would, inside a move at {point:?}"),
            RunError::Io(e) => write!(f, "{e}"),
        }
    }
}

fn io_err(what: &str, path: &Path, e: io::Error) -> String {
    format!("{what} {}: {e}", path.display())
}

/// The real run's preconditions: no Klipp running, and an installed
/// package that refuses converted files (#1066).
pub fn real_preconditions(repo: &Path) -> Result<(), String> {
    no_klipp_running()?;
    let installed = crate::installed::check(repo);
    if !installed.allows_conversion() {
        return Err(format!("the installed package doesn't refuse converted files: {installed:?}"));
    }
    Ok(())
}

/// That no Klipp is running: checked at the start and again before each
/// swap.
pub fn no_klipp_running() -> Result<(), String> {
    let r = crate::running::running().map_err(|e| format!("can't tell whether a Klipp is running: {e}"))?;
    if r.is_running() {
        return Err(format!("a Klipp is running: {r:?}"));
    }
    Ok(())
}

/// The run's lock: a file holding the process id, removed when the run
/// ends. A lock whose process is gone is stale and taken over.
struct Lock(PathBuf);

impl Lock {
    fn take(root: &Path) -> Result<Lock, String> {
        std::fs::create_dir_all(root).map_err(|e| io_err("making", root, e))?;
        let path = root.join("run.lock");
        if let Ok(text) = std::fs::read_to_string(&path) {
            let alive = text.trim().parse::<u32>().is_ok_and(|pid| Path::new(&format!("/proc/{pid}")).exists());
            if alive {
                return Err(format!("another run holds {} (process {})", path.display(), text.trim()));
            }
            // Stale: its process is gone.
            std::fs::remove_file(&path).map_err(|e| io_err("clearing the stale", &path, e))?;
        }
        // Made only if it isn't there: two runs starting together can't
        // both take it.
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e| format!("another run took {} first: {e}", path.display()))?;
        f.write_all(std::process::id().to_string().as_bytes()).map_err(|e| io_err("writing", &path, e))?;
        Ok(Lock(path))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn journal_dir(s: &Settings) -> PathBuf {
    s.backup_root.join("journal")
}

fn journal_path(s: &Settings, dir: &Path) -> PathBuf {
    let id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, dir.as_os_str().as_encoded_bytes());
    journal_dir(s).join(format!("{id}.json"))
}

// ── On disk, durably ────────────────────────────────────────────────────
//
// Every step the journal records must be on disk before the record is, and
// a source is removed only once its copy is: a file's data is synced before
// it is renamed into place, a folder after a name in it changes, and a
// whole copied tree before its source goes. Otherwise a power cut could
// keep the renames and the removal but lose the data they point at.

/// Sync a file's data, or a folder's list of names, to the disk.
fn sync(path: &Path) -> Result<(), String> {
    std::fs::File::open(path).and_then(|f| f.sync_all()).map_err(|e| io_err("syncing", path, e))
}

/// Sync the folder a path is in, after the path was made, renamed or removed.
fn sync_parent(path: &Path) -> Result<(), String> {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => sync(p),
        _ => Ok(()),
    }
}

/// Rename, then sync the folders of both names.
fn rename_synced(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::rename(from, to)?;
    let sync_dir = |p: &Path| p.parent().filter(|d| !d.as_os_str().is_empty()).map_or(Ok(()), |d| std::fs::File::open(d).and_then(|f| f.sync_all()));
    sync_dir(to)?;
    if from.parent() != to.parent() {
        sync_dir(from)?;
    }
    Ok(())
}

/// Write `bytes` to `path` whole or not at all: beside it, synced, then
/// renamed.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.tmp.{}", std::process::id()));
    let mut f = std::fs::File::create(&tmp).map_err(|e| io_err("writing", &tmp, e))?;
    f.write_all(bytes).and_then(|()| f.sync_all()).map_err(|e| io_err("writing", &tmp, e))?;
    drop(f);
    rename_synced(&tmp, path).map_err(|e| io_err("renaming into", path, e))
}

fn record(s: &Settings, e: &Entry) -> Result<(), String> {
    let dir = journal_dir(s);
    if !dir.is_dir() {
        std::fs::create_dir_all(&dir).map_err(|err| io_err("making", &dir, err))?;
        sync_parent(&dir)?;
    }
    let text = serde_json::to_vec_pretty(e).map_err(|err| err.to_string())?;
    write_atomic(&journal_path(s, &e.dir), &text)
}

fn modified(path: &Path) -> Option<(u64, u32)> {
    let t = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
    let d = t.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    Some((d.as_secs(), d.subsec_nanos()))
}

fn set_modified(path: &Path, (secs, nanos): (u64, u32)) -> Result<(), String> {
    let t = SystemTime::UNIX_EPOCH + std::time::Duration::new(secs, nanos);
    std::fs::File::open(path).and_then(|f| f.set_modified(t).and_then(|()| f.sync_all())).map_err(|e| io_err("setting the time of", path, e))
}

/// A path's place in the backup: the backup root, the run's day, then the
/// path itself from the root of the file system.
fn in_backup(s: &Settings, path: &Path) -> PathBuf {
    let rel = path.strip_prefix("/").unwrap_or(path);
    s.backup_root.join(&s.date).join(rel)
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

// ── The copy ────────────────────────────────────────────────────────────

/// Whether the copy leaves `rel` (a path inside the project) out: caches,
/// partial files, the backups beside the pair, the recovery copy.
fn left_out(rel: &Path) -> bool {
    let name = rel.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let top = rel.components().next().map(|c| c.as_os_str().to_string_lossy().into_owned()).unwrap_or_default();
    top == ".klipp-cache"
        || top == ".recovery"
        || name.ends_with(".part")
        || (name.starts_with('.') && name.contains(".tmp."))
        || (rel.parent() == Some(Path::new("")) && name.starts_with("project.json.bak."))
        || (rel.parent() == Some(Path::new("media")) && name.starts_with("manifest.json.bak."))
}

/// The JSON files the conversion rewrites: never hard-linked, so writing
/// the new one can't reach the original.
fn rewritten(rel: &Path) -> bool {
    rel == Path::new("project.json") || rel == Path::new("media/stills.json") || rel.starts_with("looks")
}

/// The folders of a project the conversion writes files into.
fn writes_into(rel: &Path) -> bool {
    rel == Path::new("media") || rel == Path::new("looks")
}

/// Copy `src` into the new folder `dst` as step 2 describes.
fn copy_project(src: &Path, dst: &Path) -> Result<(), String> {
    copy_tree(src, dst, Path::new(""), true)
}

/// Copy a folder. `project` applies the project's rules: what is left out,
/// and media hard-linked; without it every file is copied (a backup across
/// file systems). Every copied file and every folder is synced, so the
/// whole tree is on disk when this returns.
fn copy_tree(src: &Path, dst: &Path, rel: &Path, project: bool) -> Result<(), String> {
    std::fs::create_dir(dst).map_err(|e| io_err("making", dst, e))?;
    let perms = std::fs::metadata(src).map_err(|e| io_err("reading", src, e))?.permissions();
    for entry in std::fs::read_dir(src).map_err(|e| io_err("listing", src, e))? {
        let entry = entry.map_err(|e| io_err("listing", src, e))?;
        let (from, to) = (entry.path(), dst.join(entry.file_name()));
        let rel = rel.join(entry.file_name());
        if project && left_out(&rel) {
            continue;
        }
        let kind = entry.file_type().map_err(|e| io_err("reading", &from, e))?;
        if project && kind.is_symlink() && writes_into(&rel) {
            // Kept as a link, the converted files written into it would
            // land in the folder it names: the original's, or one it shares.
            let target = std::fs::read_link(&from).map_err(|e| io_err("reading the link", &from, e))?;
            return Err(format!("{} is a link to {}, and the conversion writes into it: make it a folder of the project first", from.display(), target.display()));
        }
        if kind.is_symlink() {
            let target = std::fs::read_link(&from).map_err(|e| io_err("reading the link", &from, e))?;
            std::os::unix::fs::symlink(&target, &to).map_err(|e| io_err("linking", &to, e))?;
        } else if kind.is_dir() {
            copy_tree(&from, &to, &rel, project)?;
        } else if project && rel.starts_with("media") && !rewritten(&rel) {
            // Shared with the original, which goes into the backup: the
            // media is never written, so one copy of it serves both.
            if std::fs::hard_link(&from, &to).is_err() {
                std::fs::copy(&from, &to).map_err(|e| io_err("copying", &from, e))?;
                sync(&to)?;
            }
        } else {
            std::fs::copy(&from, &to).map_err(|e| io_err("copying", &from, e))?;
            sync(&to)?;
        }
    }
    std::fs::set_permissions(dst, perms).map_err(|e| io_err("setting the mode of", dst, e))?;
    sync(dst)?;
    sync_parent(dst)
}

/// Whether two trees hold the same files, byte for byte, and the same links.
fn same_tree(a: &Path, b: &Path) -> Result<(), String> {
    let list = |d: &Path| -> Result<Vec<(std::ffi::OsString, std::fs::FileType)>, String> {
        let mut v = Vec::new();
        for e in std::fs::read_dir(d).map_err(|e| io_err("listing", d, e))? {
            let e = e.map_err(|e| io_err("listing", d, e))?;
            v.push((e.file_name(), e.file_type().map_err(|err| io_err("reading", &e.path(), err))?));
        }
        v.sort_by(|x, y| x.0.cmp(&y.0));
        Ok(v)
    };
    let (la, lb) = (list(a)?, list(b)?);
    if la.iter().map(|x| &x.0).ne(lb.iter().map(|x| &x.0)) {
        return Err(format!("{} and {} list different files", a.display(), b.display()));
    }
    for (name, kind) in la {
        let (x, y) = (a.join(&name), b.join(&name));
        if kind.is_symlink() {
            if std::fs::read_link(&x).ok() != std::fs::read_link(&y).ok() {
                return Err(format!("{} differs", y.display()));
            }
        } else if kind.is_dir() {
            same_tree(&x, &y)?;
        } else if std::fs::read(&x).map_err(|e| io_err("reading", &x, e))? != std::fs::read(&y).map_err(|e| io_err("reading", &y, e))? {
            return Err(format!("{} differs", y.display()));
        }
    }
    Ok(())
}

/// Move a folder to `to`: a rename, or on another file system a copy made
/// beside it, checked byte for byte, renamed into place, and only then the
/// source removed. So a `to` that is there is whole, whatever stopped the
/// move: a rerun only finishes removing what is left of the source, and
/// never touches `to`.
fn move_tree(from: &Path, to: &Path, crash: &dyn Fn(MovePoint, &Path) -> bool) -> Result<(), RunError> {
    let io = RunError::Io;
    if to.exists() {
        if from.exists() {
            std::fs::remove_dir_all(from).map_err(|e| io(io_err("removing what is left of", from, e)))?;
        }
        return Ok(());
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io(io_err("making", parent, e)))?;
    }
    match rename_synced(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
            let partial = with_suffix(to, ".partial");
            if partial.exists() {
                std::fs::remove_dir_all(&partial).map_err(|e| io(io_err("clearing", &partial, e)))?;
            }
            // Synced whole by `copy_tree`, before the source is touched.
            copy_tree(from, &partial, Path::new(""), false).map_err(io)?;
            same_tree(from, &partial).map_err(io)?;
            rename_synced(&partial, to).map_err(|e| io(io_err("moving into place", to, e)))?;
            if crash(MovePoint::InPlace, to) {
                return Err(RunError::CrashedInMove(MovePoint::InPlace));
            }
            // One entry first, so a test can stop it part way.
            if let Some(first) = std::fs::read_dir(from).ok().and_then(|mut es| es.next()).and_then(Result::ok) {
                let p = first.path();
                let r = if first.file_type().is_ok_and(|t| t.is_dir()) { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
                r.map_err(|e| io(io_err("removing", &p, e)))?;
            }
            if crash(MovePoint::PartRemoved, to) {
                return Err(RunError::CrashedInMove(MovePoint::PartRemoved));
            }
            std::fs::remove_dir_all(from).map_err(|e| io(io_err("removing", from, e)))
        }
        Err(e) => Err(io(io_err("moving", from, e))),
    }
}

// ── One project ─────────────────────────────────────────────────────────

/// The revision-2 files of the project in `dir`: (path inside the
/// project, text).
fn converted_files(dir: &Path) -> Result<Vec<(PathBuf, String)>, String> {
    let text = std::fs::read_to_string(dir.join("project.json")).map_err(|e| io_err("reading", &dir.join("project.json"), e))?;
    let raw: Value = serde_json::from_str(&text).map_err(|e| format!("project.json: {e}"))?;
    let (out, _) = revision2::project(&raw)?;
    let mut files = vec![(PathBuf::from("project.json"), serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?)];
    let stills = dir.join("media/stills.json");
    if stills.is_file() {
        let raw: Value = serde_json::from_str(&std::fs::read_to_string(&stills).map_err(|e| io_err("reading", &stills, e))?).map_err(|e| format!("media/stills.json: {e}"))?;
        files.push((PathBuf::from("media/stills.json"), serde_json::to_string_pretty(&revision2::stills(&raw)?).map_err(|e| e.to_string())?));
    }
    for look in looks(dir)? {
        let raw: Value = serde_json::from_str(&std::fs::read_to_string(&look).map_err(|e| io_err("reading", &look, e))?).map_err(|e| format!("{}: {e}", look.display()))?;
        let rel = look.strip_prefix(dir).unwrap_or(&look).to_path_buf();
        files.push((rel, serde_json::to_string_pretty(&revision2::look(&raw)?).map_err(|e| e.to_string())?));
    }
    Ok(files)
}

fn looks(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let d = dir.join("looks");
    let Ok(entries) = std::fs::read_dir(&d) else { return Ok(Vec::new()) };
    let mut v: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
    v.sort();
    Ok(v)
}

/// The written copy read back and held to what it was made from.
fn verify_copy(original: &Path, copy: &Path) -> Result<(), String> {
    let raw: Value = serde_json::from_str(&std::fs::read_to_string(original.join("project.json")).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let written = std::fs::read_to_string(copy.join("project.json")).map_err(|e| io_err("reading back", &copy.join("project.json"), e))?;
    revision2::verify_project(&raw, &written)?;
    let stills = original.join("media/stills.json");
    if stills.is_file() {
        let raw: Value = serde_json::from_str(&std::fs::read_to_string(&stills).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let back = std::fs::read_to_string(copy.join("media/stills.json")).map_err(|e| e.to_string())?;
        revision2::verify_side(&revision2::stills(&raw)?, &back)?;
    }
    for look in looks(original)? {
        let raw: Value = serde_json::from_str(&std::fs::read_to_string(&look).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let rel = look.strip_prefix(original).unwrap_or(&look);
        let back = std::fs::read_to_string(copy.join(rel)).map_err(|e| e.to_string())?;
        revision2::verify_side(&revision2::look(&raw)?, &back)?;
    }
    Ok(())
}

/// Where a project's journal entry stands, as the disk shows it: a rename
/// done but not yet recorded is counted done.
fn settle_from_disk(e: &mut Entry) {
    // The aside appears only by renaming the original, so with it there
    // the original went aside, whether or not the copy has since taken its
    // place (both renames done, neither recorded).
    if e.step == Step::Verified && e.aside.exists() && (!e.dir.exists() || !e.copy.exists()) {
        e.step = Step::OriginalAside;
    }
    if e.step == Step::OriginalAside && e.dir.exists() && !e.copy.exists() {
        e.step = Step::Swapped;
    }
    // A backup that is there is whole (see `move_tree`), even with what is
    // left of the original beside it: the move only has its removal to
    // finish, which the next step's `move_tree` does first.
    if e.step == Step::Swapped && e.backup.exists() && !e.aside.exists() {
        e.step = Step::BackedUp;
    }
    if e.step == Step::BackedUp && !e.autosave.exists() {
        e.step = Step::AutosaveMoved;
    }
}

/// Take one entry from its step to the end; `Err` is a stop with its
/// reason, already rolled back where it came before the swap.
fn advance(s: &Settings, e: &mut Entry, hooks: &Hooks) -> Result<(), RunError> {
    let crash_point = |step: Step, recorded: bool| -> Result<(), RunError> {
        if (hooks.crash)(step, recorded) {
            Err(RunError::Crashed(step))
        } else {
            Ok(())
        }
    };
    loop {
        let next = match e.step {
            Step::Started => {
                if e.copy.exists() {
                    std::fs::remove_dir_all(&e.copy).map_err(|err| RunError::Io(io_err("clearing", &e.copy, err)))?;
                }
                copy_project(&e.dir, &e.copy).map_err(RunError::Io)?;
                Step::Copied
            }
            Step::Copied => {
                for (rel, text) in converted_files(&e.dir).map_err(RunError::Io)? {
                    write_atomic(&e.copy.join(&rel), text.as_bytes()).map_err(RunError::Io)?;
                }
                Step::Written
            }
            Step::Written => {
                verify_copy(&e.dir, &e.copy).map_err(RunError::Io)?;
                (hooks.render_check)(&e.copy).map_err(|why| RunError::Io(format!("the render check stopped it: {why}")))?;
                Step::Verified
            }
            Step::Verified => {
                (hooks.swap_allowed)().map_err(RunError::Refused)?;
                rename_synced(&e.dir, &e.aside).map_err(|err| RunError::Io(io_err("moving aside", &e.dir, err)))?;
                Step::OriginalAside
            }
            Step::OriginalAside => {
                rename_synced(&e.copy, &e.dir).map_err(|err| RunError::Io(io_err("moving into place", &e.copy, err)))?;
                Step::Swapped
            }
            Step::Swapped => {
                move_tree(&e.aside, &e.backup, hooks.crash_in_move)?;
                Step::BackedUp
            }
            Step::BackedUp => {
                if e.autosave.exists() {
                    move_tree(&e.autosave, &e.autosave_backup, hooks.crash_in_move)?;
                }
                Step::AutosaveMoved
            }
            Step::AutosaveMoved => {
                set_modified(&e.dir, e.mtime).map_err(RunError::Io)?;
                Step::Done
            }
            Step::Done => return Ok(()),
        };
        crash_point(next, false)?;
        e.step = next;
        record(s, e).map_err(RunError::Io)?;
        crash_point(next, true)?;
    }
}

/// Undo an entry that never got as far as the swap: its copy goes.
fn roll_back(s: &Settings, e: &Entry) -> Result<(), String> {
    if e.copy.exists() {
        std::fs::remove_dir_all(&e.copy).map_err(|err| io_err("removing", &e.copy, err))?;
    }
    let j = journal_path(s, &e.dir);
    std::fs::remove_file(&j).map_err(|err| io_err("removing", &j, err))
}

/// Finish every entry an earlier run left: back before the swap, forward
/// after it.
pub fn recover(s: &Settings, report: &mut Report) -> Result<(), RunError> {
    let Ok(entries) = std::fs::read_dir(journal_dir(s)) else { return Ok(()) };
    for path in entries.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|x| x == "json")) {
        let text = std::fs::read_to_string(&path).map_err(|e| RunError::Io(io_err("reading", &path, e)))?;
        let Ok(mut e) = serde_json::from_str::<Entry>(&text) else {
            // Empty or torn: set aside and named, so it stops neither this
            // run nor every run after it.
            let damaged = journal_dir(s).join("damaged");
            std::fs::create_dir_all(&damaged).map_err(|err| RunError::Io(io_err("making", &damaged, err)))?;
            let to = damaged.join(path.file_name().unwrap_or_default());
            rename_synced(&path, &to).map_err(|err| RunError::Io(io_err("setting aside", &path, err)))?;
            report.damaged_journals.push(to);
            continue;
        };
        settle_from_disk(&mut e);
        if e.step == Step::Done {
            std::fs::remove_file(&path).map_err(|err| RunError::Io(io_err("removing", &path, err)))?;
            continue;
        }
        if e.step < Step::OriginalAside {
            roll_back(s, &e).map_err(RunError::Io)?;
            report.recovered.push((e.dir.clone(), "rolled back".into()));
        } else {
            advance(s, &mut e, &Hooks::NONE)?;
            std::fs::remove_file(journal_path(s, &e.dir)).map_err(|err| RunError::Io(io_err("removing", &path, err)))?;
            report.recovered.push((e.dir.clone(), "finished".into()));
        }
    }
    Ok(())
}

/// Where a project stands before the run touches it.
enum Standing {
    /// Revision 1 throughout: to convert.
    Old,
    /// Revision 2 throughout: already converted.
    Converted,
    /// Some stores at one revision and some at the other.
    Mixed(String),
}

fn standing(dir: &Path) -> Result<Standing, String> {
    let read = |p: &Path| -> Result<Value, String> { serde_json::from_str(&std::fs::read_to_string(p).map_err(|e| io_err("reading", p, e))?).map_err(|e| format!("{}: {e}", p.display())) };
    let project = revision::read_revision(&read(&dir.join("project.json"))?).map_err(|e| format!("project.json: {e}"))?;
    let side = |v: &Value| v.get("format_revision").and_then(Value::as_u64).unwrap_or(0);
    let mut others = Vec::new();
    let stills = dir.join("media/stills.json");
    if stills.is_file() {
        others.push(("media/stills.json".to_owned(), side(&read(&stills)?)));
    }
    for look in looks(dir)? {
        others.push((look.strip_prefix(dir).unwrap_or(&look).display().to_string(), side(&read(&look)?)));
    }
    let new = project >= revision2::REVISION;
    match others.iter().find(|(_, r)| (*r >= revision2::REVISION) != new) {
        Some((what, r)) => Ok(Standing::Mixed(format!("project.json is at revision {project} but {what} at {r}"))),
        None if new => Ok(Standing::Converted),
        None => Ok(Standing::Old),
    }
}

/// Count the media items of a project whose file isn't there.
fn offline_media(dir: &Path) -> usize {
    let Ok(text) = std::fs::read_to_string(dir.join("media/manifest.json")) else { return 0 };
    let Ok(m) = serde_json::from_str::<Value>(&text) else { return 0 };
    m.get("items").and_then(Value::as_array).into_iter().flatten().filter_map(|i| i.get("path").and_then(Value::as_str)).filter(|p| !Path::new(p).exists()).count()
}

fn convert_one(s: &Settings, dir: &Path, hooks: &Hooks, report: &mut Report) -> Result<(), RunError> {
    let copy = with_suffix(dir, ".klipp-convert");
    let aside = with_suffix(dir, ".klipp-v1");
    for there in [&copy, &aside] {
        if there.exists() {
            report.stopped.push((dir.to_path_buf(), format!("{} is already there, and no journal names it", there.display())));
            return Ok(());
        }
    }
    // A backup that is there is taken as whole by recovery, so the run
    // never starts one where another already stands.
    let autosave = scan::autosave_path(dir);
    for held in [in_backup(s, dir), in_backup(s, &autosave)] {
        if held.exists() {
            report.stopped.push((dir.to_path_buf(), format!("{} is already in the backup", held.display())));
            return Ok(());
        }
    }
    let mut e = Entry {
        dir: dir.to_path_buf(),
        aside,
        backup: in_backup(s, dir),
        autosave_backup: in_backup(s, &autosave),
        autosave,
        copy,
        mtime: modified(dir).ok_or_else(|| RunError::Io(format!("reading the time of {}", dir.display())))?,
        step: Step::Started,
    };
    record(s, &e).map_err(RunError::Io)?;
    if (hooks.crash)(Step::Started, true) {
        return Err(RunError::Crashed(Step::Started));
    }
    let advanced = advance(s, &mut e, hooks);
    // A step can fail after its work is on disk (a rename done, the sync
    // after it refused): what to do next goes by the disk, not the step
    // last recorded.
    if advanced.is_err() {
        settle_from_disk(&mut e);
    }
    match advanced {
        Ok(()) => {
            std::fs::remove_file(journal_path(s, dir)).map_err(|err| RunError::Io(io_err("removing the journal of", dir, err)))?;
            report.media_offline.push((dir.to_path_buf(), offline_media(dir)));
            report.converted.push(dir.to_path_buf());
            Ok(())
        }
        Err(RunError::Io(why)) if e.step < Step::OriginalAside => {
            roll_back(s, &e).map_err(RunError::Io)?;
            report.stopped.push((dir.to_path_buf(), why));
            Ok(())
        }
        Err(RunError::Refused(why)) if e.step < Step::OriginalAside => {
            roll_back(s, &e).map_err(RunError::Io)?;
            Err(RunError::Refused(format!("before swapping {}: {why}", dir.display())))
        }
        Err(other) => Err(other),
    }
}

// ── Config-folder stores ────────────────────────────────────────────────

/// The colour presets of the config folder, converted in place after the
/// whole folder is backed up and checked. A rerun converts what is left.
fn convert_presets(s: &Settings, config: &Path, report: &mut Report) -> Result<(), RunError> {
    let presets = config.join("presets/color");
    let Ok(entries) = std::fs::read_dir(&presets) else { return Ok(()) };
    let backup = s.backup_root.join(&s.date).join("config");
    if !backup.exists() {
        // Made aside, checked, then renamed: a backup that is there is
        // whole, even after a crash part way through making it.
        if let Some(parent) = backup.parent() {
            std::fs::create_dir_all(parent).map_err(|e| RunError::Io(io_err("making", parent, e)))?;
        }
        let partial = with_suffix(&backup, ".partial");
        if partial.exists() {
            std::fs::remove_dir_all(&partial).map_err(|e| RunError::Io(io_err("clearing", &partial, e)))?;
        }
        copy_tree(config, &partial, Path::new(""), false).map_err(RunError::Io)?;
        same_tree(config, &partial).map_err(RunError::Io)?;
        rename_synced(&partial, &backup).map_err(|e| RunError::Io(io_err("moving into place", &backup, e)))?;
    }
    let mut files: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "json")).collect();
    files.sort();
    for path in files {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let held = backup.join("presets/color").join(&name);
        if !held.is_file() {
            report.presets_stopped.push((path.clone(), "not in the backup".into()));
            continue;
        }
        let text = std::fs::read_to_string(&path).map_err(|e| RunError::Io(io_err("reading", &path, e)))?;
        let Ok(raw) = serde_json::from_str::<Value>(&text) else {
            // Today's reader skips a file it can't parse, and so will the
            // next: kept as it is, and named.
            report.presets_stopped.push((path, "doesn't parse as JSON; left as it is".into()));
            continue;
        };
        if raw.get("format_revision").and_then(Value::as_u64) == Some(revision2::REVISION) {
            continue;
        }
        let converted = revision2::colour_preset(&name, &raw).and_then(|c| {
            let text = serde_json::to_string_pretty(&c).map_err(|e| e.to_string())?;
            revision2::verify_side(&c, &text)?;
            Ok(text)
        });
        match converted {
            Ok(text) => {
                write_atomic(&path, text.as_bytes()).map_err(RunError::Io)?;
                report.presets_converted += 1;
            }
            Err(why) => report.presets_stopped.push((path, why)),
        }
    }
    Ok(())
}

// ── The run ─────────────────────────────────────────────────────────────

/// The conversion: preconditions, recovery of an earlier run, then every
/// project and the config folder.
pub fn run(s: &Settings, preconditions: &dyn Fn() -> Result<(), String>, parity: Parity, hooks: &Hooks) -> Result<Report, RunError> {
    preconditions().map_err(RunError::Refused)?;
    let _lock = Lock::take(&s.backup_root).map_err(RunError::Refused)?;
    let mut report = Report::default();
    recover(s, &mut report)?;

    // Every project as it stands, and the dry run's verdict on those to
    // convert: any stop not settled by excluding the project refuses the
    // run before anything is touched.
    let mut todo = Vec::new();
    let mut refusals = Vec::new();
    // Matched as the folders they name: a trailing slash or a link to the
    // project still excludes it.
    let excluded: BTreeSet<PathBuf> = s.excluded.iter().map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone())).collect();
    let listed: BTreeSet<PathBuf> = s.projects.iter().filter_map(|p| std::fs::canonicalize(p).ok()).collect();
    // An exclusion that names no project in the run is most likely a typo
    // for one the user meant to protect.
    let unmatched: Vec<String> = s
        .excluded
        .iter()
        .filter(|raw| !s.projects.contains(raw) && !listed.contains(&std::fs::canonicalize(raw).unwrap_or_else(|_| (*raw).clone())))
        .map(|raw| raw.display().to_string())
        .collect();
    if !unmatched.is_empty() {
        return Err(RunError::Refused(format!("--exclude names no project in the run: {}", unmatched.join(", "))));
    }
    let mut seen = BTreeSet::new();
    for p in &s.projects {
        let dir = match std::fs::canonicalize(p) {
            Ok(d) => d,
            Err(e) => {
                refusals.push(format!("{}: {e}", p.display()));
                continue;
            }
        };
        // The same folder named twice (a link, the recent list) is one
        // project.
        if !seen.insert(dir.clone()) {
            continue;
        }
        if excluded.contains(&dir) || excluded.contains(p) {
            report.excluded.push(p.clone());
            continue;
        }
        // An auto-save is never converted on its own. Beside a project in
        // the run, it goes with that project: into the backup when the
        // project is converted, left as it is when the project is stopped
        // or excluded. Any other is a stop.
        if scan::is_autosave(&dir) {
            match scan::project_of_autosave(&dir) {
                Some(main) if listed.contains(&main) => {
                    if excluded.contains(&main) {
                        report.excluded.push(p.clone());
                    }
                }
                _ => report.stopped.push((dir, "an auto-save whose project isn't in the run: open it in Klipp and save it as a project, or remove it; left as it is".into())),
            }
            continue;
        }
        match standing(&dir) {
            Ok(Standing::Converted) => report.skipped.push(dir),
            Ok(Standing::Mixed(why)) => report.stopped.push((dir, format!("mixed: {why}; left as it is"))),
            Err(why) => refusals.push(format!("{}: {why}", dir.display())),
            Ok(Standing::Old) => {
                let checked = dry_run::check_project(&dir, parity);
                if scan::has_newer_autosave(&dir) {
                    report.stopped.push((dir, "an auto-save newer than the project: open it in Klipp and keep or discard it first".into()));
                } else if scan::has_pending_recovery(&dir) {
                    report.stopped.push((dir, "a save that stopped part way: open it in Klipp first".into()));
                } else if scan::pair_mismatch(&dir) {
                    report.stopped.push((dir, "project.json and its manifest are from different saves: open it in Klipp and save first".into()));
                } else if !checked.passed() {
                    refusals.push(format!("{}: {}", dir.display(), checked.stops.join("; ")));
                } else {
                    todo.push(dir);
                }
            }
        }
    }
    if !refusals.is_empty() {
        return Err(RunError::Refused(format!("the dry run stops, unsettled: {}", refusals.join(" | "))));
    }
    for dir in todo {
        convert_one(s, &dir, hooks, &mut report)?;
    }
    if let Some(config) = &s.config_dir {
        convert_presets(s, config, &mut report)?;
    }
    Ok(report)
}

// ── The command line ────────────────────────────────────────────────────

/// What `run` takes on its command line.
#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    /// Projects left as they are (`--exclude <dir>`).
    pub excluded: BTreeSet<PathBuf>,
}

/// The run's arguments. Without `--confirm-real-run` it refuses: the run
/// rewrites every project on the machine, once, at the switch.
pub fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut confirmed = false;
    let mut excluded = BTreeSet::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--confirm-real-run" => confirmed = true,
            "--exclude" => {
                excluded.insert(PathBuf::from(it.next().ok_or("--exclude needs a project folder")?));
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    if !confirmed {
        return Err("this converts every Klipp project on this machine, once; it runs only with --confirm-real-run".into());
    }
    Ok(Args { excluded })
}

/// The day `secs` after the epoch falls on, as `YYYY-MM-DD` (UTC).
pub fn date_of(secs: u64) -> String {
    // Civil from days (Howard Hinnant's algorithm).
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// The real run's settings: the projects the scan lists, those under
/// `home` and those of the app's recent list (test runs' scratch projects
/// never among them), the config folder `config_dir` (`~/.config/klipp`),
/// and the backup root, dated today. A folder the scan couldn't read may
/// hide projects, so it refuses. Auto-saves stay in the list: the run
/// decides each with its project.
pub fn real_settings(home: &Path, config_dir: &Path, excluded: BTreeSet<PathBuf>) -> Result<Settings, String> {
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let found = scan::discover(home);
    if !found.unreadable.is_empty() {
        return Err(format!("folders the scan couldn't read, which may hold projects: {:?}", found.unreadable));
    }
    let mut projects: BTreeSet<PathBuf> = found.projects.into_iter().collect();
    // As `scan::scan` reads it: the preferences under the config folder's
    // parent.
    if let Some(config_root) = config_dir.parent() {
        for r in scan::recent_projects(&scan::prefs_path(config_root)) {
            if !scan::is_scratch(&r, home) && scan::is_klipp_project(&r) {
                projects.insert(r);
            }
        }
    }
    Ok(Settings {
        projects: projects.into_iter().collect(),
        excluded,
        config_dir: Some(config_dir.to_path_buf()),
        backup_root: scan::backup_root(home),
        date: date_of(now),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_run_refuses_without_its_confirmation() {
        let a = |v: &[&str]| parse_args(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert!(a(&[]).is_err());
        assert!(a(&["--exclude", "/x"]).is_err());
        assert!(a(&["--confirm"]).is_err());
        assert_eq!(a(&["--confirm-real-run", "--exclude", "/x"]).unwrap().excluded, BTreeSet::from([PathBuf::from("/x")]));
    }

    #[test]
    fn dates_are_civil_days() {
        assert_eq!(date_of(0), "1970-01-01");
        assert_eq!(date_of(951_782_400), "2000-02-29");
        assert_eq!(date_of(1_791_244_800), "2026-10-06");
    }
}
