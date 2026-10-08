//! The step to revision 3 (effects keep their node ids): every
//! revision-2 project's `project.json` gets `format_revision: 3`, and not
//! one other byte changes (see [`klipp_graph_lift::revision3`]).
//!
//! It is its own run, not stage A's, because what it promises is stronger:
//! - **Preflight** freezes a manifest of every candidate, with a reason for
//!   each inclusion and exclusion and a hash of every file it covers
//!   (`project.json` and its saved backups, `media/manifest.json`, the side
//!   stores, `.recovery`). Anything it can't read or settle is a stop. The
//!   real run takes the hash of the manifest its dry run reported, and
//!   refuses if the manifest it makes then differs (drift).
//! - **Backups first:** every covered file of every project to convert is
//!   copied (its bytes, never a link) and read back against its hash
//!   before the first project file is written.
//! - **Write** changes the revision number only, and each written file is
//!   held to its backup by the raw-JSON oracle ([`revision3::verify`]) and
//!   read back by the app's loader (a copy, so the load can't touch the
//!   project).
//! - **Rollback is whole-run:** a durable journal names every file, with
//!   the hash of what the run writes there, before it is written, and
//!   marks it once written. Any error restores every file holding what the
//!   run wrote from the verified backups, and checks each against its
//!   hash; a file holding neither its original bytes nor the run's is
//!   someone else's, never overwritten, and left and named. A
//!   crash is rolled back by the next run, never finished forward. A
//!   restore that fails stops with the files it couldn't restore, and
//!   everything (backups, journal) is kept.
//!
//! The dry run reads only: it writes to a fresh, empty output folder,
//! reached through no link and outside every project and backup, loads its
//! copies there, and says "render NOT RUN" unless it was given a render
//! check.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::revision3;
use crate::run::{io_err, make_dirs, write_atomic, Lock, Settings};
use crate::{revision, revision2, scan};

/// The file a dry run leaves in its output folder, so a later scan never
/// takes the copies there for projects.
pub const DRY_RUN_MARK: &str = ".klipp-dry-run";

pub fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// One file the manifest covers, by absolute path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub path: PathBuf,
    pub sha256: String,
    pub len: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Decision {
    /// At revision 2, checked: to step to 3.
    Convert,
    /// At revision 3: left as it is.
    AlreadyConverted,
    /// Excluded by `--exclude`: left as it is, byte for byte.
    Excluded,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub dir: PathBuf,
    pub decision: Decision,
    pub reason: String,
    pub revision: Option<u64>,
    pub items: Vec<Item>,
}

/// What preflight found: every candidate, the exclusions that matched no
/// candidate, and every stop. A run starts only from one without stops.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub candidates: Vec<Candidate>,
    pub unmatched_exclusions: Vec<PathBuf>,
    pub stops: Vec<String>,
}

impl Manifest {
    /// The hash a real run is approved with.
    pub fn hash(&self) -> String {
        sha256(&serde_json::to_vec(self).unwrap_or_default())
    }

    fn to_convert(&self) -> impl Iterator<Item = &Candidate> {
        self.candidates.iter().filter(|c| c.decision == Decision::Convert)
    }
}

/// The files of `dir` the manifest covers, sorted. A link among them is a
/// stop: the run never follows one out of the project. An excluded
/// project's links (a linked side folder, or a linked file among those it
/// covers) aren't followed either, but aren't a stop: the run never writes
/// an excluded project, and its other files are still held to their
/// hashes.
fn inventory(dir: &Path, excluded: bool) -> Result<Vec<Item>, String> {
    // A linked folder would take every file under it out of the project,
    // whatever the files themselves are.
    let mut linked = Vec::new();
    for rel in ["media", "looks", ".recovery"] {
        let d = dir.join(rel);
        if std::fs::symlink_metadata(&d).is_ok_and(|m| !m.is_dir()) {
            if !excluded {
                return Err(format!("{} isn't a plain folder (a link?): left for a person to settle", d.display()));
            }
            linked.push(d);
        }
    }
    let mut paths = Vec::new();
    let named = |rel: &str| dir.join(rel);
    for rel in ["project.json", "media/manifest.json", "media/stills.json"] {
        paths.push(named(rel));
    }
    let under_link = |p: &Path| linked.iter().any(|l| p.starts_with(l));
    let listed = |d: &Path, keep: &dyn Fn(&str) -> bool| -> Result<Vec<PathBuf>, String> {
        if under_link(d) {
            return Ok(Vec::new());
        }
        match std::fs::read_dir(d) {
            Ok(es) => es.map(|e| e.map(|e| e.path()).map_err(|e| io_err("listing", d, e))).filter(|p| p.as_ref().map_or(true, |p| p.file_name().is_some_and(|n| keep(&n.to_string_lossy())))).collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(io_err("listing", d, e)),
        }
    };
    paths.extend(listed(dir, &|n| n.starts_with("project.json.bak"))?);
    paths.extend(listed(&dir.join("media"), &|n| n.starts_with("manifest.json.bak"))?);
    paths.extend(listed(&dir.join("looks"), &|n| n.ends_with(".json"))?);
    let mut stack = vec![dir.join(".recovery")];
    while let Some(d) = stack.pop() {
        for p in listed(&d, &|_| true)? {
            let m = std::fs::symlink_metadata(&p).map_err(|e| io_err("reading", &p, e))?;
            if m.is_dir() {
                stack.push(p);
            } else {
                paths.push(p);
            }
        }
    }
    let mut items = Vec::new();
    for p in paths.into_iter().filter(|p| !under_link(p)) {
        let m = match std::fs::symlink_metadata(&p) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(io_err("reading", &p, e)),
        };
        if !m.is_file() {
            // An excluded project is never written: a link in it is left
            // unfollowed, as its linked folders are.
            if excluded && m.file_type().is_symlink() {
                continue;
            }
            return Err(format!("{} isn't a plain file (a link?): left for a person to settle", p.display()));
        }
        let bytes = std::fs::read(&p).map_err(|e| io_err("reading", &p, e))?;
        items.push(Item { path: p, sha256: sha256(&bytes), len: bytes.len() as u64 });
    }
    items.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(items)
}

fn read_json(p: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(p).map_err(|e| io_err("reading", p, e))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: doesn't parse: {e}", p.display()))
}

/// Settle one candidate, or say why it stops.
fn settle(dir: &Path, excluded: bool) -> Result<Candidate, String> {
    let items = inventory(dir, excluded)?;
    let candidate = |decision, reason: String, revision| Candidate { dir: dir.to_path_buf(), decision, reason, revision, items: items.clone() };
    if excluded {
        return Ok(candidate(Decision::Excluded, "excluded by --exclude".into(), None));
    }
    let raw = read_json(&dir.join("project.json"))?;
    let r = revision::read_revision(&raw).map_err(|e| format!("project.json: {e}"))?;
    if r == revision2::PROJECT_REVISION {
        return Ok(candidate(Decision::AlreadyConverted, format!("at revision {r}"), Some(r)));
    }
    if r != revision2::REVISION {
        return Err(format!("at revision {r}: the stage-A run converts a project from before revision {} first", revision2::REVISION));
    }
    if scan::has_pending_recovery(dir) {
        return Err("a save that stopped part way: open it in Klipp first".into());
    }
    if scan::pair_mismatch(dir) {
        return Err("project.json and its manifest are from different saves: open it in Klipp and save first".into());
    }
    for side in [dir.join("media/stills.json")].into_iter().chain(items.iter().map(|i| i.path.clone()).filter(|p| p.parent() == Some(&dir.join("looks")))) {
        if side.is_file() {
            let at = read_json(&side)?.get("format_revision").and_then(Value::as_u64);
            if at != Some(revision2::REVISION) {
                return Err(format!("{} is at revision {at:?}, not {}", side.display(), revision2::REVISION));
            }
        }
    }
    let checked = revision3::check(&raw)?;
    let reason = format!(
        "at revision 2, effect ids checked: {} clip pictures ({} picture-truth, ids kept as stored), {} track effect lists",
        checked.clips, checked.truth_clips, checked.tracks_with_effects
    );
    Ok(candidate(Decision::Convert, reason, Some(r)))
}

/// Freeze the manifest of the run's projects (see the module's note).
pub fn preflight(s: &Settings) -> Manifest {
    let mut m = Manifest::default();
    let projects: BTreeSet<&PathBuf> = s.projects.iter().collect();
    m.unmatched_exclusions = s.excluded.iter().filter(|e| !projects.contains(e)).cloned().collect();
    for e in &m.unmatched_exclusions {
        m.stops.push(format!("--exclude {} names no project of the run", e.display()));
    }
    for dir in projects {
        match settle(dir, s.excluded.contains(dir)) {
            Ok(c) => m.candidates.push(c),
            Err(why) => m.stops.push(format!("{}: {why}", dir.display())),
        }
    }
    m
}

// ── The journal ─────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
    BackingUp,
    Writing,
    RollingBack,
    RolledBack,
    Committed,
}

/// A file the run may have written, recorded before it is: where it is,
/// where its verified backup is, the hash both had, the hash of what the
/// run writes there, and whether the write was done. A rollback restores
/// only a file holding what the run wrote: one someone else changed is
/// left as it is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Touched {
    pub path: PathBuf,
    pub backup: PathBuf,
    pub sha256: String,
    pub written_sha256: String,
    pub written: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Journal {
    pub manifest_hash: String,
    pub state: State,
    pub touched: Vec<Touched>,
}

pub fn journal_path(s: &Settings) -> PathBuf {
    s.backup_root.join("run3.json")
}

fn record(s: &Settings, j: &Journal) -> Result<(), String> {
    make_dirs(&s.backup_root)?;
    write_atomic(&journal_path(s), &serde_json::to_vec_pretty(j).map_err(|e| e.to_string())?)
}

fn in_backup(s: &Settings, path: &Path) -> PathBuf {
    s.backup_root.join(&s.date).join(path.strip_prefix("/").unwrap_or(path))
}

// ── The run ─────────────────────────────────────────────────────────────

/// Where a test can stop the run as if it died.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Point {
    /// Part way through the backups: after the n-th file.
    BackingUp(usize),
    BackedUp,
    /// The n-th project to convert, recorded, not yet written.
    BeforeWrite(usize),
    /// The n-th project written, its write not yet marked in the journal.
    Written(usize),
    Verified(usize),
    BeforeCommit,
    /// Rolling back: after restoring the n-th touched file.
    Restored(usize),
}

pub struct Hooks<'a> {
    /// Stop as if the process died here.
    pub crash: &'a dyn Fn(Point) -> bool,
    /// Make the n-th write fail as a full disk would.
    pub fail_write: &'a dyn Fn(usize) -> bool,
    /// Load the project copied to the folder given, as the app loads it.
    pub reload: &'a dyn Fn(&Path) -> Result<(), String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RunError {
    /// Refused before anything was written.
    Refused(String),
    /// Stopped after writing, and every touched file restored and checked.
    RolledBack(String),
    /// Stopped, and some files couldn't be restored: each with why.
    /// Backups and the journal are kept.
    RollbackIncomplete { why: String, unrestored: Vec<(PathBuf, String)> },
    /// A test's crash.
    Crashed(Point),
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub converted: Vec<PathBuf>,
    pub already: Vec<PathBuf>,
    pub excluded: Vec<PathBuf>,
    /// A run that found an unfinished one rolls it back first.
    pub rolled_back_earlier: bool,
}

/// Restore every touched file that holds what the run wrote from its
/// backup, and check it. A file at its original hash needs nothing; one
/// that holds neither hash isn't the run's to overwrite, and is left and
/// named.
fn roll_back(s: &Settings, j: &mut Journal, hooks: &Hooks) -> Result<(), RunError> {
    j.state = State::RollingBack;
    record(s, j).map_err(|e| RunError::RollbackIncomplete { why: format!("recording the rollback: {e}"), unrestored: j.touched.iter().map(|t| (t.path.clone(), "not tried".into())).collect() })?;
    let mut unrestored = Vec::new();
    for (i, t) in j.touched.iter().enumerate() {
        let now = std::fs::read(&t.path).map(|b| sha256(&b));
        let r = match now {
            Ok(h) if h == t.sha256 => Ok(()),
            Ok(h) if h == t.written_sha256 => restore(t),
            // Neither hash: someone else's bytes, whether they came before
            // the run's write or after it (a crash between the write and
            // its mark leaves `written` false either way), so never
            // overwritten, and named, the journal left unfinished.
            Ok(_) => Err(if t.written { "changed by someone else since the run wrote it: left as it is, its backup kept" } else { "changed by someone else during the run (before or after its write): left as it is, its backup kept" }.into()),
            Err(e) => Err(io_err("reading", &t.path, e)),
        };
        if let Err(why) = r {
            unrestored.push((t.path.clone(), why));
        }
        if (hooks.crash)(Point::Restored(i)) {
            return Err(RunError::Crashed(Point::Restored(i)));
        }
    }
    if !unrestored.is_empty() {
        return Err(RunError::RollbackIncomplete { why: "restoring from the backups".into(), unrestored });
    }
    j.state = State::RolledBack;
    record(s, j).map_err(|e| RunError::RollbackIncomplete { why: format!("every file restored, but recording it failed: {e}"), unrestored: Vec::new() })
}

/// Put a touched file's backup back, and read it back against its hash.
fn restore(t: &Touched) -> Result<(), String> {
    std::fs::read(&t.backup)
            .map_err(|e| io_err("reading the backup", &t.backup, e))
            .and_then(|b| if sha256(&b) == t.sha256 { Ok(b) } else { Err(format!("the backup {} no longer has its hash", t.backup.display())) })
            .and_then(|b| write_keeping_mode(&t.path, &b))
            .and_then(|()| std::fs::read(&t.path).map_err(|e| io_err("reading back", &t.path, e)))
            .and_then(|b| if sha256(&b) == t.sha256 { Ok(()) } else { Err("restored, but it reads back different".into()) })
}

/// Roll back a run the journal shows unfinished. `Ok(true)` when there was
/// one.
pub fn recover(s: &Settings, hooks: &Hooks) -> Result<bool, RunError> {
    let path = journal_path(s);
    if !path.exists() {
        return Ok(false);
    }
    let text = std::fs::read_to_string(&path).map_err(|e| RunError::Refused(io_err("reading", &path, e)))?;
    let mut j: Journal = serde_json::from_str(&text).map_err(|e| RunError::Refused(format!("{}: {e}", path.display())))?;
    match j.state {
        State::Committed | State::RolledBack => Ok(false),
        State::BackingUp | State::Writing | State::RollingBack => roll_back(s, &mut j, hooks).map(|()| true),
    }
}

fn backup_all(s: &Settings, m: &Manifest, hooks: &Hooks) -> Result<(), String> {
    let mut n = 0;
    for c in m.to_convert() {
        for item in &c.items {
            let to = in_backup(s, &item.path);
            if to.exists() {
                return Err(format!("{} is already in the backup: run with a fresh date folder (another day's run, or move that one aside)", to.display()));
            }
            let bytes = std::fs::read(&item.path).map_err(|e| io_err("reading", &item.path, e))?;
            if sha256(&bytes) != item.sha256 {
                return Err(format!("{} changed since the dry run", item.path.display()));
            }
            make_dirs(to.parent().ok_or("a backup path without a folder")?)?;
            // Written from its bytes, so the backup is a file of its own.
            write_atomic(&to, &bytes)?;
            let back = std::fs::read(&to).map_err(|e| io_err("reading back", &to, e))?;
            if sha256(&back) != item.sha256 {
                return Err(format!("the backup {} reads back different", to.display()));
            }
            if (hooks.crash)(Point::BackingUp(n)) {
                return Err(format!("crash:{n}"));
            }
            n += 1;
        }
    }
    Ok(())
}

/// [`write_atomic`], the file keeping the permission bits it had (a new
/// file from `write_atomic` would get the default ones).
fn write_keeping_mode(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mode = std::fs::metadata(path).map(|m| m.permissions()).ok();
    write_atomic(path, bytes)?;
    if let Some(mode) = mode {
        std::fs::set_permissions(path, mode).map_err(|e| io_err("setting the mode of", path, e))?;
    }
    Ok(())
}

/// Whether every file of `c` still has its manifest hash.
fn unchanged(c: &Candidate) -> Result<(), String> {
    for item in &c.items {
        let now = std::fs::read(&item.path).map_err(|e| io_err("reading", &item.path, e))?;
        if sha256(&now) != item.sha256 {
            return Err(format!("{} changed since the dry run", item.path.display()));
        }
    }
    Ok(())
}

/// The text the run writes for `c`'s project.json, from its backup, the
/// backup held to the manifest's hash first.
fn converted_text(s: &Settings, c: &Candidate, item: &Item) -> Result<String, String> {
    let backup = in_backup(s, &item.path);
    let bytes = std::fs::read(&backup).map_err(|e| io_err("reading the backup of", &item.path, e))?;
    if sha256(&bytes) != item.sha256 {
        return Err(format!("the backup {} no longer has its hash", backup.display()));
    }
    let original = String::from_utf8(bytes).map_err(|e| format!("{}: {e}", c.dir.display()))?;
    revision3::project_text(&original)
}

/// Write one project; the n-th to convert. The checks are made again just
/// before the write, so a change since the journal named the file is
/// never overwritten.
fn write_one(n: usize, c: &Candidate, path: &Path, text: &str, hooks: &Hooks, preconditions: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
    preconditions()?;
    unchanged(c)?;
    if (hooks.fail_write)(n) {
        return Err(format!("writing {}: no space left on device (a test's)", path.display()));
    }
    write_keeping_mode(path, text.as_bytes())?;
    Ok(())
}

/// Hold the written project to its backup and load a copy of it.
fn prove_one(s: &Settings, n: usize, c: &Candidate, hooks: &Hooks) -> Result<(), String> {
    let path = c.dir.join("project.json");
    let item = c.items.iter().find(|i| i.path == path).ok_or_else(|| format!("{}: no project.json in its manifest", c.dir.display()))?;
    let backup = std::fs::read(in_backup(s, &path)).map_err(|e| io_err("reading the backup of", &path, e))?;
    if sha256(&backup) != item.sha256 {
        return Err(format!("the backup of {} no longer has its hash", path.display()));
    }
    let original = String::from_utf8(backup).map_err(|e| format!("{}: {e}", path.display()))?;
    let written = std::fs::read_to_string(&path).map_err(|e| io_err("reading back", &path, e))?;
    revision3::verify(&original, &written).map_err(|e| format!("{}: {e}", path.display()))?;
    let copy = s.backup_root.join(&s.date).join(".reload").join(n.to_string());
    let _ = std::fs::remove_dir_all(&copy);
    make_dirs(&copy.join("media"))?;
    std::fs::write(copy.join("project.json"), &written).map_err(|e| io_err("copying", &path, e))?;
    let manifest = c.dir.join("media/manifest.json");
    std::fs::copy(&manifest, copy.join("media/manifest.json")).map_err(|e| io_err("copying", &manifest, e))?;
    let loaded = (hooks.reload)(&copy).map_err(|e| format!("{}: the app's loader refuses it: {e}", c.dir.display()));
    let _ = std::fs::remove_dir_all(&copy);
    loaded
}

/// The real run, from the manifest hash its dry run reported.
pub fn run(s: &Settings, approved: &str, preconditions: &dyn Fn() -> Result<(), String>, hooks: &Hooks) -> Result<Report, RunError> {
    let _lock = Lock::take(&s.backup_root).map_err(RunError::Refused)?;
    let mut report = Report { rolled_back_earlier: recover(s, hooks)?, ..Report::default() };
    if report.rolled_back_earlier {
        return Err(RunError::RolledBack("an earlier run stopped part way and is rolled back: run the dry run again".into()));
    }
    preconditions().map_err(RunError::Refused)?;
    let m = preflight(s);
    if !m.stops.is_empty() {
        return Err(RunError::Refused(format!("preflight stops: {}", m.stops.join(" | "))));
    }
    if m.hash() != approved {
        return Err(RunError::Refused(format!("the projects changed since the dry run (manifest {} now, {approved} approved)", m.hash())));
    }
    for c in &m.candidates {
        match c.decision {
            Decision::Convert => report.converted.push(c.dir.clone()),
            Decision::AlreadyConverted => report.already.push(c.dir.clone()),
            Decision::Excluded => report.excluded.push(c.dir.clone()),
        }
    }
    let mut j = Journal { manifest_hash: m.hash(), state: State::BackingUp, touched: Vec::new() };
    record(s, &j).map_err(RunError::Refused)?;
    match backup_all(s, &m, hooks) {
        Ok(()) => {}
        Err(why) if why.starts_with("crash:") => return Err(RunError::Crashed(Point::BackingUp(why[6..].parse().unwrap_or(0)))),
        Err(why) => {
            // Nothing written yet: the journal says so, the backups stay.
            j.state = State::RolledBack;
            let _ = record(s, &j);
            return Err(RunError::Refused(format!("backing up: {why}")));
        }
    }
    if (hooks.crash)(Point::BackedUp) {
        return Err(RunError::Crashed(Point::BackedUp));
    }
    j.state = State::Writing;
    record(s, &j).map_err(RunError::Refused)?;
    let mut failed = None;
    for (n, c) in m.to_convert().enumerate() {
        let path = c.dir.join("project.json");
        let Some(item) = c.items.iter().find(|i| i.path == path) else {
            failed = Some(format!("{}: no project.json in its manifest", c.dir.display()));
            break;
        };
        // Checked before the journal names it, so a file that changed is
        // never in the journal at all.
        let text = match unchanged(c).and_then(|()| converted_text(s, c, item)) {
            Ok(text) => text,
            Err(e) => {
                failed = Some(e);
                break;
            }
        };
        j.touched.push(Touched { path: path.clone(), backup: in_backup(s, &path), sha256: item.sha256.clone(), written_sha256: sha256(text.as_bytes()), written: false });
        if let Err(e) = record(s, &j) {
            failed = Some(e);
            break;
        }
        if (hooks.crash)(Point::BeforeWrite(n)) {
            return Err(RunError::Crashed(Point::BeforeWrite(n)));
        }
        if let Err(e) = write_one(n, c, &path, &text, hooks, preconditions) {
            failed = Some(e);
            break;
        }
        // Written, not yet marked: the window a rollback can't tell from
        // before the write.
        if (hooks.crash)(Point::Written(n)) {
            return Err(RunError::Crashed(Point::Written(n)));
        }
        if let Some(t) = j.touched.last_mut() {
            t.written = true;
        }
        if let Err(e) = record(s, &j) {
            failed = Some(e);
            break;
        }
        if let Err(e) = prove_one(s, n, c, hooks) {
            failed = Some(e);
            break;
        }
        if (hooks.crash)(Point::Verified(n)) {
            return Err(RunError::Crashed(Point::Verified(n)));
        }
    }
    if failed.is_none() {
        // What the run doesn't convert is as it was.
        for c in m.candidates.iter().filter(|c| c.decision != Decision::Convert) {
            for item in &c.items {
                if std::fs::read(&item.path).map(|b| sha256(&b)).ok().as_deref() != Some(item.sha256.as_str()) {
                    failed = Some(format!("{} changed during the run", item.path.display()));
                }
            }
        }
    }
    if (hooks.crash)(Point::BeforeCommit) {
        return Err(RunError::Crashed(Point::BeforeCommit));
    }
    if let Some(why) = failed {
        roll_back(s, &mut j, hooks)?;
        return Err(RunError::RolledBack(why));
    }
    j.state = State::Committed;
    if let Err(e) = record(s, &j) {
        roll_back(s, &mut j, hooks)?;
        return Err(RunError::RolledBack(format!("recording the commit: {e}")));
    }
    Ok(report)
}

// ── The dry run ─────────────────────────────────────────────────────────

/// One project's line in the dry run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DryProject {
    pub dir: PathBuf,
    pub decision: Decision,
    pub reason: String,
    /// The oracle and the loader on the converted copy.
    pub proven: Result<(), String>,
    /// The render check's word, or "render NOT RUN".
    pub render: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DryReport {
    pub manifest_hash: String,
    pub build: String,
    pub projects: Vec<DryProject>,
    pub unmatched_exclusions: Vec<PathBuf>,
    pub stops: Vec<String>,
    pub rendered: bool,
}

impl DryReport {
    /// Whether a real run may follow: no stops, every project proven, and
    /// rendered.
    pub fn go(&self) -> bool {
        self.stops.is_empty() && self.rendered && self.projects.iter().all(|p| p.proven.is_ok() && (p.decision != Decision::Convert || !p.render.starts_with("render ")))
    }

    pub fn text(&self) -> String {
        let mut t = format!("revision 3 dry run\nbuild: {}\nmanifest: {}\n", self.build, self.manifest_hash);
        let count = |d| self.projects.iter().filter(|p| p.decision == d).count();
        t += &format!(
            "to convert: {}, already at 3: {}, excluded: {}\n",
            count(Decision::Convert),
            count(Decision::AlreadyConverted),
            count(Decision::Excluded)
        );
        if !self.rendered {
            t += "render NOT RUN: a real run needs a rendered dry run\n";
        }
        for s in &self.stops {
            t += &format!("STOP {s}\n");
        }
        for p in &self.projects {
            let proven = match &p.proven {
                Ok(()) => "proven".to_owned(),
                Err(e) => format!("FAILED {e}"),
            };
            t += &format!("{} [{:?}] {}; {proven}; {}\n", p.dir.display(), p.decision, p.reason, p.render);
        }
        t += if self.go() { "verdict: ready for the go\n" } else { "verdict: NOT ready\n" };
        t
    }
}

/// `p`'s real path, though its end may not exist yet: its nearest folder
/// that does, resolved, and the rest of `p` after it.
fn real_path(p: &Path) -> PathBuf {
    let mut base = p;
    let mut rest = Vec::new();
    loop {
        if let Ok(real) = std::fs::canonicalize(base) {
            return rest.iter().rev().fold(real, |at, c| at.join(c));
        }
        match (base.parent(), base.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                base = parent;
            }
            _ => return p.to_path_buf(),
        }
    }
}

/// Make the dry run's output folder: a fresh, empty one, reached through
/// no link, and outside every project and backup (by their real paths, so
/// a link can't put it inside one). Returns its real path.
fn dry_run_output(s: &Settings, out: &Path) -> Result<PathBuf, String> {
    let out = if out.is_absolute() { out.to_path_buf() } else { std::env::current_dir().map_err(|e| e.to_string())?.join(out) };
    if out.components().any(|c| matches!(c, std::path::Component::ParentDir | std::path::Component::CurDir)) {
        return Err(format!("the output {} has a . or .. in it: name it plainly", out.display()));
    }
    let mut at = PathBuf::new();
    for c in out.components() {
        at.push(c);
        match std::fs::symlink_metadata(&at) {
            Ok(m) if m.file_type().is_symlink() => return Err(format!("the output's path goes through a link at {}: name a path without one", at.display())),
            Ok(m) if !m.is_dir() => return Err(format!("{} isn't a folder", at.display())),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => return Err(io_err("reading", &at, e)),
        }
    }
    // No component is a link, so the path is its own real path; the
    // projects and the backup are compared by theirs too.
    for p in s.projects.iter().chain(std::iter::once(&s.backup_root)) {
        let p_real = real_path(p);
        if out.starts_with(&p_real) || p_real.starts_with(&out) || out.starts_with(p) || p.starts_with(&out) {
            return Err(format!("the output {} is inside {} (or holds it): it must be outside every project and backup", out.display(), p.display()));
        }
    }
    if out.exists() {
        let mut entries = std::fs::read_dir(&out).map_err(|e| io_err("listing", &out, e))?;
        if entries.next().is_some() {
            return Err(format!("the output {} isn't empty (an earlier dry run's?): name a fresh folder", out.display()));
        }
    } else {
        if let Some(parent) = out.parent() {
            make_dirs(parent)?;
        }
        std::fs::create_dir(&out).map_err(|e| io_err("making", &out, e))?;
    }
    let real = std::fs::canonicalize(&out).map_err(|e| io_err("resolving", &out, e))?;
    if real != out {
        return Err(format!("the output {} resolves to {}: name its real path", out.display(), real.display()));
    }
    Ok(real)
}

/// The dry run: reads the projects, writes only under `out`. `render`, when
/// given, renders the project (read only) and its converted copy under
/// `out` and says how they compare, or why they don't.
pub fn dry_run(s: &Settings, out: &Path, build: &str, reload: &dyn Fn(&Path) -> Result<(), String>, render: Option<&dyn Fn(&Path, &Path) -> Result<String, String>>) -> Result<DryReport, String> {
    let out = dry_run_output(s, out)?;
    let m = preflight(s);
    std::fs::write(out.join(DRY_RUN_MARK), b"klipp-convert step3 dry run\n").map_err(|e| io_err("marking", &out, e))?;
    let mut projects = Vec::new();
    for (n, c) in m.candidates.iter().enumerate() {
        let mut p = DryProject { dir: c.dir.clone(), decision: c.decision, reason: c.reason.clone(), proven: Ok(()), render: "render NOT RUN".into() };
        if c.decision == Decision::Convert {
            let copy = out.join("projects").join(n.to_string());
            p.proven = (|| {
                let text = std::fs::read_to_string(c.dir.join("project.json")).map_err(|e| e.to_string())?;
                let written = revision3::project_text(&text)?;
                revision3::verify(&text, &written)?;
                std::fs::create_dir_all(copy.join("media")).map_err(|e| e.to_string())?;
                std::fs::write(copy.join("project.json"), &written).map_err(|e| e.to_string())?;
                std::fs::copy(c.dir.join("media/manifest.json"), copy.join("media/manifest.json")).map_err(|e| e.to_string())?;
                reload(&copy)
            })();
            if let (Some(render), Ok(())) = (render, &p.proven) {
                p.render = render(&c.dir, &copy).unwrap_or_else(|e| format!("render FAILED {e}"));
            }
        }
        projects.push(p);
    }
    let report = DryReport { manifest_hash: m.hash(), build: build.into(), projects, unmatched_exclusions: m.unmatched_exclusions.clone(), stops: m.stops.clone(), rendered: render.is_some() };
    std::fs::write(out.join("manifest.json"), serde_json::to_vec_pretty(&m).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::write(out.join("report.txt"), report.text()).map_err(|e| e.to_string())?;
    Ok(report)
}
