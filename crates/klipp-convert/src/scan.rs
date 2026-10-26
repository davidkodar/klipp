//! The conversion's dry run: every Klipp project on this machine, and
//! what about each would stop it converting safely (#1054). It converts
//! nothing and writes nothing inside a project; the report goes where it
//! is told.
//!
//! A project is found two ways: by walking a home directory for a
//! `project.json` whose sibling `media/manifest.json` both read as
//! Klipp's, and from the app's recent-projects list. The walk leaves out
//! what is never converted in place: git working trees (the repository,
//! its worktrees and the gate's checkout hold test fixtures, which are
//! converted in a PR), build folders, the Trash, the conversion's own
//! backups and staging folders.
//!
//! What it reports of each is read the way the app reads it, copied here
//! rather than linked, so the converter depends on no app crate: an
//! auto-save is `<stem>_autosave.klipp` beside the project and is newer
//! when its folder's modification time is
//! (`klipp-ui` `has_newer_autosave`), and a save that stopped part way
//! leaves `.recovery/pending` with a pair whose generations differ
//! (`klipp-project` `pair_to_open`).

use std::collections::BTreeSet;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;

/// Where the conversion keeps a backup of every project it converts.
pub fn backup_root(home: &Path) -> PathBuf {
    home.join(".local/share/klipp/pre-v2")
}

/// Where the step to revision 3 keeps its backups and journal.
pub fn backup_root_v3(home: &Path) -> PathBuf {
    home.join(".local/share/klipp/pre-v3")
}

/// Where the step to revision 4 keeps its backups and journal.
pub fn backup_root_v4(home: &Path) -> PathBuf {
    home.join(".local/share/klipp/pre-v4")
}

/// Where step 5's run (#2754) keeps its backups.
pub fn backup_root_v5(home: &Path) -> PathBuf {
    home.join(".local/share/klipp/pre-v5")
}

/// Folder names whose contents are never a project to convert in place.
const SKIPPED_DIRS: &[&str] = &["target", "node_modules", ".cache", ".klipp-cache"];

/// The file that marks a folder as a harness's or a test run's scratch: a
/// script that copies projects to work on (an A/B run, a bisect, an MCP
/// run) writes it at the top of its output, and every project under it is
/// counted as scratch, never listed, so the conversion never takes those
/// copies for a user's projects.
pub const SCRATCH_MARK: &str = ".klipp-scratch";

/// Whether `path`, found scanning `home`, is in a test run's scratch: in
/// a folder named `tmp`, or starting `tmp-` or `tmp_`, below `home` (or
/// below the system's temporary folder), or anywhere in the system's
/// temporary folder when `home` is not itself inside it. Its projects are
/// made by tests and sessions, never a user's, and the conversion must
/// never rewrite them; they are counted, not listed (#1054).
pub fn is_scratch(path: &Path, home: &Path) -> bool {
    let tmp = std::env::temp_dir();
    if path.starts_with(&tmp) && !home.starts_with(&tmp) {
        return true;
    }
    let below = path.strip_prefix(home).or_else(|_| path.strip_prefix(&tmp)).unwrap_or(path);
    below.components().any(|c| {
        let n = c.as_os_str().to_string_lossy();
        n == "tmp" || n.starts_with("tmp-") || n.starts_with("tmp_")
    })
}

/// Whether `dir` is a folder the walk does not enter.
fn skipped(dir: &Path, home: &Path) -> bool {
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // A git working tree: a `.git` directory, or the `.git` file a
    // worktree has.
    dir.join(".git").exists()
        || SKIPPED_DIRS.contains(&name.as_str())
        || name.ends_with(".klipp-convert")
        || name.ends_with(".klipp-v1")
        || dir == home.join(".local/share/Trash")
        || dir == backup_root(home)
        || dir == backup_root_v3(home)
        || dir == backup_root_v4(home)
        || dir == backup_root_v5(home)
        // A dry run's output: its copies are converted projects' twins.
        || dir.join(crate::step3::DRY_RUN_MARK).exists()
}

/// Whether `dir` holds a Klipp project: a `project.json` and a
/// `media/manifest.json` that both have Klipp's shape. Another
/// application's `project.json` is not one.
pub fn is_klipp_project(dir: &Path) -> bool {
    let read = |p: PathBuf| std::fs::read_to_string(p).ok().and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
    let (Some(project), Some(manifest)) = (read(dir.join("project.json")), read(dir.join("media").join("manifest.json"))) else {
        return false;
    };
    let project_shape = project.get("version").is_some_and(|v| v.is_string())
        && project.get("klipp_version_min").is_some()
        && (project.get("sequences").is_some_and(|s| s.is_array()) || project.get("sequence").is_some());
    let manifest_shape = manifest.get("version").is_some_and(|v| v.is_string()) && manifest.get("items").is_some_and(|i| i.is_array());
    project_shape && manifest_shape
}

/// What a walk of the home folder found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Discovered {
    /// Every Klipp project folder, sorted.
    pub projects: Vec<PathBuf>,
    /// Projects in test runs' scratch ([`is_scratch`]), counted only.
    pub scratch: usize,
    /// Folders that couldn't be listed, which may hold projects the walk
    /// never saw.
    pub unreadable: Vec<PathBuf>,
}

/// Every Klipp project folder under `home`, not following symbolic links,
/// and the folders it couldn't list.
pub fn discover(home: &Path) -> Discovered {
    let mut found = BTreeSet::new();
    let mut scratch = 0;
    let mut unreadable = Vec::new();
    let mut stack = vec![(home.to_path_buf(), false)];
    while let Some((dir, in_scratch)) = stack.pop() {
        if dir != home && skipped(&dir, home) {
            continue;
        }
        let in_scratch = in_scratch || is_scratch(&dir, home) || dir.join(SCRATCH_MARK).exists();
        if is_klipp_project(&dir) {
            if in_scratch {
                scratch += 1;
            } else {
                found.insert(dir.clone());
            }
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(es) => es,
            // Gone since it was listed: nothing in it to miss.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                unreadable.push(dir);
                continue;
            }
        };
        for e in entries.flatten() {
            // Not through links: a link into another tree would list its
            // projects twice, or a whole other disk.
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push((e.path(), in_scratch));
            }
        }
    }
    unreadable.sort();
    Discovered { projects: found.into_iter().collect(), scratch, unreadable }
}

/// The app's preferences file, which holds the recent-projects list.
pub fn prefs_path(config_dir: &Path) -> PathBuf {
    config_dir.join("klipp").join("preferences.json")
}

/// The projects in the app's recent list, as the preferences file holds
/// them: `general.recent_projects`, entries with a `path` (or, in older
/// files, bare paths).
pub fn recent_projects(prefs: &Path) -> Vec<PathBuf> {
    let Some(v) = std::fs::read_to_string(prefs).ok().and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()) else {
        return Vec::new();
    };
    let Some(list) = v.get("general").and_then(|g| g.get("recent_projects")).and_then(|l| l.as_array()) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|e| e.get("path").and_then(|p| p.as_str()).or_else(|| e.as_str()))
        .map(PathBuf::from)
        .collect()
}

/// The auto-save folder the app keeps beside a project.
pub fn autosave_path(project: &Path) -> PathBuf {
    let stem = project.file_stem().unwrap_or_default().to_string_lossy();
    project.with_file_name(format!("{stem}_autosave.klipp"))
}

/// Whether `dir` is itself an auto-save folder.
pub fn is_autosave(dir: &Path) -> bool {
    dir.file_stem().is_some_and(|s| s.to_string_lossy().ends_with("_autosave"))
}

/// Whether the project's auto-save is newer than the project, as the app
/// decides it when it offers to restore one: by the folders' times.
pub fn has_newer_autosave(project: &Path) -> bool {
    let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    match (modified(project), modified(&autosave_path(project))) {
        (Some(saved), Some(auto)) => auto > saved,
        (None, Some(_)) => true,
        _ => false,
    }
}

fn generation_of(text: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(text).ok()?.get("generation")?.as_str().map(str::to_string)
}

/// Whether a save stopped part way and the app would open from its
/// recovery copy: a `pending` mark, and a pair whose two files are from
/// different saves.
pub fn has_pending_recovery(dir: &Path) -> bool {
    if !dir.join(".recovery").join("pending").exists() {
        return false;
    }
    let read = |p: PathBuf| std::fs::read_to_string(p).unwrap_or_default();
    let (project, manifest) = (read(dir.join("project.json")), read(dir.join("media").join("manifest.json")));
    generation_of(&project) != generation_of(&manifest)
}

/// Whether the pair is from different saves with no `pending` mark: the
/// app then mends it from the backups beside the pair, which the converted
/// copy doesn't carry, so the conversion stops on it.
pub fn pair_mismatch(dir: &Path) -> bool {
    if dir.join(".recovery").join("pending").exists() {
        return false;
    }
    let read = |p: PathBuf| std::fs::read_to_string(p).unwrap_or_default();
    let (project, manifest) = (read(dir.join("project.json")), read(dir.join("media").join("manifest.json")));
    generation_of(&project) != generation_of(&manifest)
}

/// The project's side stores.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SideStores {
    /// Looks in `looks/`.
    pub looks: usize,
    pub stills_index: bool,
    pub stills_dir: bool,
}

fn json_files_in(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|es| es.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "json")).count())
        .unwrap_or(0)
}

/// The folders under `presets/color` that hold tool presets, one per tool
/// (`klipp-ui` `tool_presets_dir`).
pub const TOOL_PRESET_DIRS: [&str; 6] = ["wheels", "curves", "huesat", "warper", "color_basic", "full"];

/// The presets the app keeps in its config folder. The run converts the
/// colour presets; tool and single-effect presets stay unchanged at
/// revision 2 (they hold no picture), and move at a later revision.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Presets {
    /// `presets/color/*.json`.
    pub colour: usize,
    /// One per tool, in `presets/color/<tool>/`.
    pub tool: usize,
    /// One per effect, in `effect_presets/<effect key>/`.
    pub effect: usize,
}

/// The presets under `config_dir/klipp`.
pub fn presets(config_dir: &Path) -> Presets {
    presets_in(&config_dir.join("klipp"))
}

/// The presets in the app's config folder `klipp` (`~/.config/klipp`).
pub fn presets_in(klipp: &Path) -> Presets {
    let colour = klipp.join("presets").join("color");
    let tool = TOOL_PRESET_DIRS.iter().map(|t| json_files_in(&colour.join(t))).sum();
    // Each effect's presets are in a folder named by its key.
    let effect = std::fs::read_dir(klipp.join("effect_presets"))
        .map(|es| es.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| json_files_in(&e.path())).sum())
        .unwrap_or(0);
    Presets { colour: json_files_in(&colour), tool, effect }
}

/// Everything the dry run says of one project.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectScan {
    pub path: PathBuf,
    /// Found in the recent list, not by the walk (outside the home
    /// directory, or inside a tree the walk leaves out).
    pub from_recent_list: bool,
    /// Whether it is itself an auto-save folder whose project is gone.
    pub orphan_autosave: bool,
    pub autosave: bool,
    pub newer_autosave: bool,
    pub pending_recovery: bool,
    /// The pair is from different saves with no `pending` mark.
    pub pair_mismatch: bool,
    /// Folders inside the project that couldn't be listed.
    pub unreadable_dirs: usize,
    pub side_stores: SideStores,
    /// Symbolic links inside the project, relative to it.
    pub symlinks: Vec<PathBuf>,
    /// Whether the project folder is itself a link.
    pub is_symlink: bool,
    /// Whether it is on another filesystem than the backup root, so a
    /// backup is a copy, never a rename.
    pub other_filesystem: bool,
    /// Bytes of the files the conversion rewrites.
    pub rewritten_bytes: u64,
    /// Bytes of the whole folder without its media files, proxies and
    /// caches: what a backup copies.
    pub backup_bytes: u64,
}

/// The files the conversion rewrites in a project.
fn rewritten_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = vec![dir.join("project.json"), dir.join("media").join("manifest.json"), dir.join("media").join("stills.json")];
    if let Ok(es) = std::fs::read_dir(dir.join("looks")) {
        files.extend(es.flatten().map(|e| e.path()));
    }
    files
}

/// The media files the manifest lists that live inside the project (a
/// collected project's copies), which a backup leaves where they are.
fn media_files(dir: &Path) -> BTreeSet<PathBuf> {
    let Some(v) = std::fs::read_to_string(dir.join("media").join("manifest.json")).ok().and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()) else {
        return BTreeSet::new();
    };
    let items = v.get("items").and_then(|i| i.as_array()).cloned().unwrap_or_default();
    let mut out = BTreeSet::new();
    for item in &items {
        for key in ["path", "proxy_path", "skim_file"] {
            if let Some(p) = item.get(key).and_then(|p| p.as_str()) {
                let p = Path::new(p);
                out.insert(if p.is_relative() { dir.join(p) } else { p.to_path_buf() });
            }
        }
    }
    out
}

fn size(p: &Path) -> u64 {
    std::fs::symlink_metadata(p).map(|m| m.len()).unwrap_or(0)
}

/// Read one project. `backup_device` is the backup root's filesystem.
pub fn scan_project(dir: &Path, from_recent_list: bool, backup_device: Option<u64>) -> ProjectScan {
    let media = media_files(dir);
    let mut symlinks = Vec::new();
    let mut backup_bytes = 0;
    let mut unreadable_dirs = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let es = match std::fs::read_dir(&d) {
            Ok(es) => es,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                unreadable_dirs += 1;
                continue;
            }
        };
        for e in es.flatten() {
            let p = e.path();
            let Ok(t) = e.file_type() else { continue };
            if t.is_symlink() {
                symlinks.push(p.strip_prefix(dir).unwrap_or(&p).to_path_buf());
            } else if t.is_dir() {
                let name = e.file_name();
                if name != ".klipp-cache" && name != "proxies" && name != "skims" {
                    stack.push(p);
                }
            } else if !media.contains(&p) {
                backup_bytes += size(&p);
            }
        }
    }
    symlinks.sort();
    let device = |p: &Path| std::fs::metadata(p).map(|m| m.dev()).ok();
    let looks = json_files_in(&dir.join("looks"));
    ProjectScan {
        path: dir.to_path_buf(),
        from_recent_list,
        orphan_autosave: is_autosave(dir),
        autosave: autosave_path(dir).exists(),
        newer_autosave: has_newer_autosave(dir),
        pending_recovery: has_pending_recovery(dir),
        pair_mismatch: pair_mismatch(dir),
        unreadable_dirs,
        side_stores: SideStores { looks, stills_index: dir.join("media").join("stills.json").exists(), stills_dir: dir.join("media").join("stills").is_dir() },
        symlinks,
        is_symlink: std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink()),
        other_filesystem: match (device(dir), backup_device) {
            (Some(a), Some(b)) => a != b,
            _ => false,
        },
        rewritten_bytes: rewritten_files(dir).iter().map(|p| size(p)).sum(),
        backup_bytes,
    }
}

/// The dry run's whole report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    pub home: PathBuf,
    pub prefs: PathBuf,
    pub backup_root: PathBuf,
    pub projects: Vec<ProjectScan>,
    /// Paths in the recent list that are not projects (moved, deleted).
    pub recent_missing: Vec<PathBuf>,
    /// Projects in test runs' scratch, under the home directory or in the
    /// recent list, left out: counted, never listed.
    pub scratch_skipped: usize,
    /// Folders the walk couldn't list, which may hold projects it never saw.
    pub unreadable_dirs: Vec<PathBuf>,
    pub presets: Presets,
    /// What runs, or why it couldn't be told.
    pub running: Result<crate::running::Running, String>,
    pub scanned_at: u64,
}

/// The totals the report closes with: counts and bytes, and nothing that
/// names a project, so they can be posted.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Counts {
    pub projects: usize,
    pub from_recent_list_only: usize,
    pub autosaves: usize,
    pub newer_autosaves: usize,
    pub orphan_autosaves: usize,
    pub pending_recoveries: usize,
    pub pair_mismatches: usize,
    /// Folders that couldn't be listed: in the walk, and inside projects.
    pub unreadable_dirs: usize,
    pub looks: usize,
    pub stills_indexes: usize,
    pub presets: usize,
    pub with_symlinks: usize,
    pub symlinked_projects: usize,
    pub on_other_filesystem: usize,
    pub scratch_skipped: usize,
    pub rewritten_bytes: u64,
    pub backup_bytes: u64,
}

impl Report {
    pub fn counts(&self) -> Counts {
        let p = &self.projects;
        let n = |f: fn(&ProjectScan) -> bool| p.iter().filter(|s| f(s)).count();
        Counts {
            projects: p.len(),
            from_recent_list_only: n(|s| s.from_recent_list),
            autosaves: n(|s| s.autosave),
            newer_autosaves: n(|s| s.newer_autosave),
            orphan_autosaves: n(|s| s.orphan_autosave),
            pending_recoveries: n(|s| s.pending_recovery),
            pair_mismatches: n(|s| s.pair_mismatch),
            unreadable_dirs: self.unreadable_dirs.len() + p.iter().map(|s| s.unreadable_dirs).sum::<usize>(),
            looks: p.iter().map(|s| s.side_stores.looks).sum(),
            stills_indexes: n(|s| s.side_stores.stills_index),
            presets: self.presets.colour + self.presets.tool + self.presets.effect,
            with_symlinks: n(|s| !s.symlinks.is_empty()),
            symlinked_projects: n(|s| s.is_symlink),
            on_other_filesystem: n(|s| s.other_filesystem),
            scratch_skipped: self.scratch_skipped,
            rewritten_bytes: p.iter().map(|s| s.rewritten_bytes).sum(),
            backup_bytes: p.iter().map(|s| s.backup_bytes).sum(),
        }
    }

    /// The report as text: a line per project that needs a look, then the
    /// counts.
    pub fn text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("Klipp conversion dry run\nhome: {}\nrecent list: {}\nbackups go to: {}\n\n", self.home.display(), self.prefs.display(), self.backup_root.display()));
        match &self.running {
            Ok(r) if r.is_running() => out.push_str(&format!("KLIPP IS RUNNING: {r:?}\n\n")),
            Ok(_) => {}
            Err(e) => out.push_str(&format!("COULD NOT TELL WHETHER KLIPP IS RUNNING: {e}\n\n")),
        }
        for s in &self.projects {
            let mut notes = Vec::new();
            if s.newer_autosave { notes.push("newer auto-save".to_string()) }
            if s.orphan_autosave { notes.push("auto-save whose project is gone".to_string()) }
            if s.pending_recovery { notes.push("a save stopped part way".to_string()) }
            if s.pair_mismatch { notes.push("the pair is from different saves, with no recovery mark".to_string()) }
            if s.unreadable_dirs > 0 { notes.push(format!("{} folders inside it couldn't be read", s.unreadable_dirs)) }
            if s.is_symlink { notes.push("the folder is a link".to_string()) }
            if !s.symlinks.is_empty() { notes.push(format!("{} links inside", s.symlinks.len())) }
            if s.other_filesystem { notes.push("another filesystem".to_string()) }
            if s.from_recent_list { notes.push("found only in the recent list".to_string()) }
            let tag = if notes.is_empty() { "ok".to_string() } else { notes.join("; ") };
            out.push_str(&format!("{}  [{}]\n", s.path.display(), tag));
        }
        for d in &self.unreadable_dirs {
            out.push_str(&format!("{}  [couldn't be read: projects in it would be missed]\n", d.display()));
        }
        for m in &self.recent_missing {
            out.push_str(&format!("{}  [in the recent list, not a project]\n", m.display()));
        }
        let p = &self.presets;
        out.push_str(&format!(
            "\npresets: {} colour (converted to revision 2); {} tool and {} effect (unchanged at revision 2)\n",
            p.colour, p.tool, p.effect
        ));
        let c = self.counts();
        out.push_str(&format!("\n{}\n", serde_json::to_string_pretty(&c).unwrap_or_default()));
        out
    }
}

/// The dry run: the projects under `home` and in the recent list in
/// `config_dir`, read without writing.
pub fn scan(home: &Path, config_dir: &Path) -> Report {
    let backup = backup_root(home);
    let device = std::fs::metadata(&backup).or_else(|_| std::fs::metadata(home)).map(|m| m.dev()).ok();
    let Discovered { projects: walked, scratch: mut scratch_skipped, unreadable: unreadable_dirs } = discover(home);
    let prefs = prefs_path(config_dir);
    let mut projects: Vec<ProjectScan> = walked.iter().map(|d| scan_project(d, false, device)).collect();
    let mut recent_missing = Vec::new();
    for r in recent_projects(&prefs) {
        if walked.contains(&r) {
            continue;
        }
        if is_scratch(&r, home) {
            scratch_skipped += usize::from(is_klipp_project(&r));
            continue;
        }
        if is_klipp_project(&r) {
            projects.push(scan_project(&r, true, device));
        } else {
            recent_missing.push(r);
        }
    }
    // An auto-save beside its project is reported with that project.
    let listed: BTreeSet<PathBuf> = projects.iter().map(|p| p.path.clone()).collect();
    projects.retain(|p| !(p.orphan_autosave && project_of_autosave(&p.path).is_some_and(|main| listed.contains(&main))));
    let scanned_at = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    Report {
        home: home.to_path_buf(),
        prefs,
        backup_root: backup,
        projects,
        recent_missing,
        scratch_skipped,
        unreadable_dirs,
        presets: presets(config_dir),
        running: crate::running::running(),
        scanned_at,
    }
}

/// The project an auto-save folder belongs to: `X_autosave.klipp` → `X.klipp`.
pub fn project_of_autosave(dir: &Path) -> Option<PathBuf> {
    let stem = dir.file_stem()?.to_string_lossy();
    let main = stem.strip_suffix("_autosave")?;
    let ext = dir.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    Some(dir.with_file_name(format!("{main}{ext}")))
}
