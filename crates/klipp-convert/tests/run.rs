//! `klipp-convert run` on disk (#1087), in temporary folders only: every
//! project, backup, journal and config folder here is made by the test.

use std::collections::BTreeSet;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use klipp_convert::run::{recover, run, Hooks, MovePoint, Report, RunError, Settings, Step};
use klipp_convert::StoreKind;
use serde_json::{json, Value};

fn ok_parity(_: &Value, _: StoreKind) -> Result<(), String> {
    Ok(())
}

fn ok() -> Result<(), String> {
    Ok(())
}

fn root(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("klipp-run-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::canonicalize(d).unwrap()
}

fn graph() -> klipp_nodes::NodeGraph {
    let mut g = klipp_nodes::NodeGraph::default();
    let c = g.ensure_corrector();
    g.nodes.iter_mut().find(|n| n.id == c).unwrap().params.exposure = Some(0.4);
    g
}

/// A v1 project as today's code saves it, with a Look, a still, media,
/// and the files the copy must leave out or keep as they are.
fn project(parent: &Path, name: &str) -> PathBuf {
    use klipp_core::{MediaId, RationalTime};
    let dir = parent.join(format!("{name}.klipp"));
    let t = |s: f64| RationalTime::from_secs_f64(s, 24);
    let mut seq = klipp_timeline::Sequence::new_default();
    let mut clip = klipp_timeline::Clip::media(MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0));
    clip.node_graph = graph();
    clip.set_effect_list(vec![klipp_effects::BuiltinEffect::GaussianBlur { radius: 2.0 }], clip.builtin_effects_enabled().to_vec());
    seq.tracks[0].clips.push(clip);
    klipp_project::save_project(&dir, &[seq], &klipp_media::MediaPool::new()).unwrap();
    klipp_project::save_looks(&dir, &[klipp_timeline::Look { id: uuid::Uuid::new_v4(), name: "Warm".into(), node_graph: graph() }]).unwrap();
    std::fs::write(dir.join("media/stills.json"), json!([{ "name": "s", "filename": "s.png", "grade": graph() }]).to_string()).unwrap();
    std::fs::create_dir_all(dir.join("media/stills")).unwrap();
    std::fs::write(dir.join("media/stills/s.png"), b"png bytes").unwrap();
    std::fs::write(dir.join("media/footage.mov"), vec![7u8; 4096]).unwrap();
    std::fs::write(dir.join("notes.txt"), b"notes").unwrap();
    std::fs::set_permissions(dir.join("notes.txt"), std::fs::Permissions::from_mode(0o640)).unwrap();
    std::os::unix::fs::symlink("notes.txt", dir.join("notes-link")).unwrap();
    std::fs::copy(dir.join("project.json"), dir.join("project.json.bak.1")).unwrap();
    std::fs::copy(dir.join("media/manifest.json"), dir.join("media/manifest.json.bak.1")).unwrap();
    std::fs::create_dir_all(dir.join(".recovery")).unwrap();
    std::fs::write(dir.join(".recovery/project.json"), b"{}").unwrap();
    std::fs::create_dir_all(dir.join(".klipp-cache")).unwrap();
    std::fs::write(dir.join(".klipp-cache/thumb"), b"cache").unwrap();
    std::fs::write(dir.join("media/render.part"), b"partial").unwrap();
    // An older auto-save: kept on its way into the backup, and the
    // project's time keeps it older.
    let auto = parent.join(format!("{name}_autosave.klipp"));
    std::fs::create_dir_all(&auto).unwrap();
    std::fs::write(auto.join("project.json"), b"{}").unwrap();
    set_time(&auto, 1_000_000_000);
    set_time(&dir, 1_100_000_000);
    dir
}

fn set_time(path: &Path, secs: u64) {
    let t = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs);
    std::fs::File::open(path).unwrap().set_modified(t).unwrap();
}

fn settings(root: &Path, projects: Vec<PathBuf>) -> Settings {
    Settings { projects, excluded: BTreeSet::new(), config_dir: None, backup_root: root.join("backup/pre-v2"), date: "2026-10-06".into() }
}

/// Every file of a tree with its bytes (links as their target), for a
/// before-and-after comparison.
fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    fn walk(base: &Path, d: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        let mut entries: Vec<_> = std::fs::read_dir(d).unwrap().map(|e| e.unwrap()).collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path();
            let rel = p.strip_prefix(base).unwrap().to_path_buf();
            let kind = e.file_type().unwrap();
            if kind.is_symlink() {
                out.push((rel, std::fs::read_link(&p).unwrap().into_os_string().into_encoded_bytes()));
            } else if kind.is_dir() {
                out.push((rel.clone(), b"<dir>".to_vec()));
                walk(base, &p, out);
            } else {
                out.push((rel, std::fs::read(&p).unwrap()));
            }
        }
    }
    walk(dir, dir, &mut out);
    out
}

fn revision_of(path: &Path) -> u64 {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    v.get("format_revision").and_then(Value::as_u64).unwrap_or(0)
}

fn in_backup(s: &Settings, path: &Path) -> PathBuf {
    s.backup_root.join(&s.date).join(path.strip_prefix("/").unwrap())
}

/// The project in `dir` is converted, and its backup is the original.
fn assert_converted(s: &Settings, dir: &Path, before: &[(PathBuf, Vec<u8>)]) {
    assert_eq!(revision_of(&dir.join("project.json")), 2, "{}", dir.display());
    assert_eq!(revision_of(&dir.join("media/stills.json")), 2);
    for look in std::fs::read_dir(dir.join("looks")).unwrap() {
        assert_eq!(revision_of(&look.unwrap().path()), 2);
    }
    assert_eq!(snapshot(&in_backup(s, dir)), before, "the backup is the original, byte for byte");
    for left in ["project.json.bak.1", "media/manifest.json.bak.1", ".recovery", ".klipp-cache", "media/render.part"] {
        assert!(!dir.join(left).exists(), "{left} left out of the converted project");
    }
    assert!(!PathBuf::from(format!("{}.klipp-convert", dir.display())).exists());
    assert!(!PathBuf::from(format!("{}.klipp-v1", dir.display())).exists());
}

#[test]
fn a_full_run_converts_and_backs_up() {
    let r = root("full");
    let dir = project(&r, "Film");
    let before = snapshot(&dir);
    let time_before = std::fs::metadata(&dir).unwrap().modified().unwrap();
    let auto_newer_before = klipp_convert::scan::has_newer_autosave(&dir);
    let s = settings(&r, vec![dir.clone()]);
    let report = run(&s, &ok, &ok_parity, &Hooks::NONE).unwrap();
    assert_eq!(report.converted, vec![dir.clone()]);
    assert_converted(&s, &dir, &before);
    // Media hard-linked: the converted project and the backup share it.
    let ino = |p: PathBuf| std::fs::metadata(p).unwrap().ino();
    assert_eq!(ino(dir.join("media/footage.mov")), ino(in_backup(&s, &dir).join("media/footage.mov")));
    // A rewritten file is its own, never the backup's.
    assert_ne!(ino(dir.join("media/stills.json")), ino(in_backup(&s, &dir).join("media/stills.json")));
    // Modes and links kept.
    assert_eq!(std::fs::metadata(dir.join("notes.txt")).unwrap().permissions().mode() & 0o777, 0o640);
    assert_eq!(std::fs::read_link(dir.join("notes-link")).unwrap(), PathBuf::from("notes.txt"));
    // The auto-save went into the backup, and the folder's time is back.
    let auto = r.join("Film_autosave.klipp");
    assert!(!auto.exists() && in_backup(&s, &auto).join("project.json").is_file());
    assert_eq!(std::fs::metadata(&dir).unwrap().modified().unwrap(), time_before);
    assert_eq!(klipp_convert::scan::has_newer_autosave(&dir), auto_newer_before);
    assert!(!s.backup_root.join("run.lock").exists() && std::fs::read_dir(s.backup_root.join("journal")).unwrap().next().is_none());
    let _ = std::fs::remove_dir_all(&r);
}

/// A crash at every journal step, before and after the step is recorded:
/// recovery leaves the project either untouched or wholly converted, and a
/// rerun then converts it.
#[test]
fn a_crash_at_any_step_ends_rolled_back_or_converted() {
    for step in Step::ALL {
        for recorded in [false, true] {
            if step == Step::Started && !recorded {
                continue; // Nothing has happened before the first record.
            }
            let r = root(&format!("crash-{step:?}-{recorded}"));
            let dir = project(&r, "Film");
            let before = snapshot(&dir);
            let s = settings(&r, vec![dir.clone()]);
            let crash = move |at: Step, rec: bool| at == step && rec == recorded;
            let hooks = Hooks { crash: &crash, crash_in_move: &|_, _| false, render_check: &|_| Ok(()) };
            assert_eq!(run(&s, &ok, &ok_parity, &hooks).unwrap_err(), RunError::Crashed(step));
            let mut report = Report::default();
            recover(&s, &mut report).unwrap();
            if step < Step::OriginalAside {
                assert_eq!(snapshot(&dir), before, "{step:?}/{recorded}: rolled back, the original untouched");
                assert!(!PathBuf::from(format!("{}.klipp-convert", dir.display())).exists(), "{step:?}/{recorded}");
            } else {
                assert_converted(&s, &dir, &before);
            }
            let report = run(&s, &ok, &ok_parity, &Hooks::NONE).unwrap();
            assert_eq!(report.converted.len() + report.skipped.len(), 1, "{step:?}/{recorded}: {report:?}");
            assert_converted(&s, &dir, &before);
            let _ = std::fs::remove_dir_all(&r);
        }
    }
}

#[test]
fn a_second_run_changes_nothing() {
    let r = root("twice");
    let dir = project(&r, "Film");
    let s = settings(&r, vec![dir.clone()]);
    run(&s, &ok, &ok_parity, &Hooks::NONE).unwrap();
    let (once, backup) = (snapshot(&dir), snapshot(&s.backup_root));
    let report = run(&s, &ok, &ok_parity, &Hooks::NONE).unwrap();
    assert_eq!((report.converted.len(), report.skipped.clone()), (0, vec![dir.clone()]));
    assert_eq!(snapshot(&dir), once);
    assert_eq!(snapshot(&s.backup_root), backup);
    let _ = std::fs::remove_dir_all(&r);
}

#[test]
fn a_mixed_project_is_stopped_and_left_alone() {
    let r = root("mixed");
    let dir = project(&r, "Film");
    let look = std::fs::read_dir(dir.join("looks")).unwrap().next().unwrap().unwrap().path();
    std::fs::write(&look, json!({ "format_revision": 2, "id": uuid::Uuid::new_v4(), "name": "x", "graph_v2": {} }).to_string()).unwrap();
    let before = snapshot(&dir);
    let s = settings(&r, vec![dir.clone()]);
    let report = run(&s, &ok, &ok_parity, &Hooks::NONE).unwrap();
    assert!(report.stopped.iter().any(|(d, why)| d == &dir && why.starts_with("mixed")), "{report:?}");
    assert_eq!(snapshot(&dir), before);
    assert!(!in_backup(&s, &dir).exists(), "not touched, not restored");
    let _ = std::fs::remove_dir_all(&r);
}

#[test]
fn a_symlinked_project_is_converted_where_it_lives() {
    let r = root("symlink");
    let dir = project(&r, "Film");
    let before = snapshot(&dir);
    let link = r.join("Shortcut.klipp");
    std::os::unix::fs::symlink(&dir, &link).unwrap();
    let s = settings(&r, vec![link.clone()]);
    let report = run(&s, &ok, &ok_parity, &Hooks::NONE).unwrap();
    assert_eq!(report.converted, vec![dir.clone()]);
    assert_converted(&s, &dir, &before);
    assert_eq!(std::fs::read_link(&link).unwrap(), dir, "the link still names the project");
    let _ = std::fs::remove_dir_all(&r);
}

#[test]
fn a_folder_the_run_cannot_write_is_a_stop() {
    let r = root("readonly");
    let parent = r.join("locked");
    std::fs::create_dir_all(&parent).unwrap();
    let dir = project(&parent, "Film");
    let before = snapshot(&dir);
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o555)).unwrap();
    let s = settings(&r, vec![dir.clone()]);
    let report = run(&s, &ok, &ok_parity, &Hooks::NONE);
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
    let report = report.unwrap();
    assert!(report.stopped.iter().any(|(d, _)| d == &dir), "{report:?}");
    assert_eq!(snapshot(&dir), before);
    let _ = std::fs::remove_dir_all(&r);
}

#[test]
fn a_newer_autosave_or_a_stopped_save_is_a_stop() {
    let r = root("autosave");
    let dir = project(&r, "Film");
    set_time(&r.join("Film_autosave.klipp"), 1_200_000_000);
    let before = snapshot(&dir);
    let s = settings(&r, vec![dir.clone()]);
    let report = run(&s, &ok, &ok_parity, &Hooks::NONE).unwrap();
    assert!(report.stopped.iter().any(|(d, why)| d == &dir && why.contains("auto-save")), "{report:?}");
    assert_eq!(snapshot(&dir), before);
    let _ = std::fs::remove_dir_all(&r);
}

/// A pair from two saves, mark or no mark, is a stop: the app would mend it
/// from backups the converted copy doesn't carry.
#[test]
fn a_pair_from_two_saves_is_a_stop() {
    let r = root("pair");
    let dir = project(&r, "Film");
    let path = dir.join("media/manifest.json");
    let mut m: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    m["generation"] = json!("another-save");
    std::fs::write(&path, m.to_string()).unwrap();
    let before = snapshot(&dir);
    let s = settings(&r, vec![dir.clone()]);
    let report = run(&s, &ok, &ok_parity, &Hooks::NONE).unwrap();
    assert!(report.stopped.iter().any(|(d, why)| d == &dir && why.contains("different saves")), "{report:?}");
    assert_eq!(snapshot(&dir), before);
    let _ = std::fs::remove_dir_all(&r);
}

#[test]
fn the_render_check_stops_a_project_rolled_back() {
    let r = root("render");
    let dir = project(&r, "Film");
    let before = snapshot(&dir);
    let s = settings(&r, vec![dir.clone()]);
    let hooks = Hooks { crash: &|_, _| false, crash_in_move: &|_, _| false, render_check: &|_| Err("a frame differs".into()) };
    let report = run(&s, &ok, &ok_parity, &hooks).unwrap();
    assert!(report.stopped.iter().any(|(d, why)| d == &dir && why.contains("a frame differs")), "{report:?}");
    assert_eq!(snapshot(&dir), before);
    assert!(std::fs::read_dir(s.backup_root.join("journal")).unwrap().next().is_none(), "the journal entry is gone");
    let _ = std::fs::remove_dir_all(&r);
}

#[test]
fn preconditions_and_the_lock_refuse_before_anything_is_touched() {
    let r = root("refuse");
    let dir = project(&r, "Film");
    let before = snapshot(&dir);
    let s = settings(&r, vec![dir.clone()]);
    let running = || Err("a Klipp is running".to_string());
    assert!(matches!(run(&s, &running, &ok_parity, &Hooks::NONE), Err(RunError::Refused(_))));
    // A lock held by a process that is alive refuses; a stale one is taken.
    std::fs::create_dir_all(&s.backup_root).unwrap();
    std::fs::write(s.backup_root.join("run.lock"), std::process::id().to_string()).unwrap();
    assert!(matches!(run(&s, &ok, &ok_parity, &Hooks::NONE), Err(RunError::Refused(_))));
    assert_eq!(snapshot(&dir), before);
    std::fs::write(s.backup_root.join("run.lock"), "4294967294").unwrap();
    assert_eq!(run(&s, &ok, &ok_parity, &Hooks::NONE).unwrap().converted, vec![dir]);
    let _ = std::fs::remove_dir_all(&r);
}

#[test]
fn a_dry_run_stop_refuses_unless_the_project_is_excluded() {
    let r = root("dryrun");
    let good = project(&r, "Good");
    let bad = project(&r, "Bad");
    // An effect today's reader doesn't know: the frozen reader refuses it.
    let path = bad.join("project.json");
    let mut v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    v["sequences"][0]["tracks"][0]["clips"][0]["builtin_effects"] = json!([{ "type": "no_such_effect" }]);
    std::fs::write(&path, v.to_string()).unwrap();
    let (good_before, bad_before) = (snapshot(&good), snapshot(&bad));
    let mut s = settings(&r, vec![good.clone(), bad.clone()]);
    assert!(matches!(run(&s, &ok, &ok_parity, &Hooks::NONE), Err(RunError::Refused(_))));
    assert_eq!((snapshot(&good), snapshot(&bad)), (good_before.clone(), bad_before.clone()), "nothing touched");
    s.excluded.insert(bad.clone());
    let report = run(&s, &ok, &ok_parity, &Hooks::NONE).unwrap();
    assert_eq!((report.converted, report.excluded), (vec![good.clone()], vec![bad.clone()]));
    assert_converted(&s, &good, &good_before);
    assert_eq!(snapshot(&bad), bad_before);
    let _ = std::fs::remove_dir_all(&r);
}

#[test]
fn colour_presets_are_backed_up_then_converted() {
    let r = root("presets");
    let config = r.join("config/klipp");
    std::fs::create_dir_all(config.join("presets/color")).unwrap();
    std::fs::write(config.join("presets/color/Warm.json"), serde_json::to_string(&graph()).unwrap()).unwrap();
    std::fs::write(config.join("presets/color/broken.json"), b"{ not json").unwrap();
    std::fs::write(config.join("prefs.json"), b"{}").unwrap();
    let before = snapshot(&config);
    let mut s = settings(&r, Vec::new());
    s.config_dir = Some(config.clone());
    let report = run(&s, &ok, &ok_parity, &Hooks::NONE).unwrap();
    assert_eq!((report.presets_converted, report.presets_stopped.len()), (1, 0), "{report:?}");
    assert_eq!(revision_of(&config.join("presets/color/Warm.json")), 2);
    assert!(!config.join("presets/color/broken.json").exists(), "a file today's reader skips goes, once backed up");
    assert_eq!(snapshot(&s.backup_root.join(&s.date).join("config")), before, "the whole folder backed up first");
    // A rerun converts nothing again.
    assert_eq!(run(&s, &ok, &ok_parity, &Hooks::NONE).unwrap().presets_converted, 0);
    let _ = std::fs::remove_dir_all(&r);
}

/// A backup on another file system is copied, checked, then the original
/// removed. Needs a writable folder on a second file system (`/dev/shm`).
#[test]
fn a_backup_on_another_file_system_is_copied_and_checked() {
    let r = root("otherfs");
    let shm = Path::new("/dev/shm");
    let dev = |p: &Path| std::fs::metadata(p).map(|m| m.dev()).ok();
    if dev(shm).is_none() || dev(shm) == dev(&r) || std::fs::create_dir_all(shm.join("klipp-run-test")).is_err() {
        eprintln!("skipped: no second writable file system (/dev/shm) beside {}", r.display());
        return;
    }
    let backup = shm.join(format!("klipp-run-test/{}", std::process::id()));
    let dir = project(&r, "Film");
    let before = snapshot(&dir);
    let mut s = settings(&r, vec![dir.clone()]);
    s.backup_root = backup.clone();
    let report = run(&s, &ok, &ok_parity, &Hooks::NONE).unwrap();
    assert_eq!(report.converted, vec![dir.clone()]);
    assert_converted(&s, &dir, &before);
    let _ = std::fs::remove_dir_all(&backup);
    let _ = std::fs::remove_dir_all(&r);
}

/// A crash inside a move across file systems, after the copy is in place
/// and part way through removing the source, of the project and of its
/// auto-save: recovery keeps the whole backup and finishes the removal
/// (#1087 review).
#[test]
fn a_crash_inside_a_move_across_file_systems_keeps_the_whole_backup() {
    let shm = Path::new("/dev/shm");
    let dev = |p: &Path| std::fs::metadata(p).map(|m| m.dev()).ok();
    let probe = root("otherfs-probe");
    if dev(shm).is_none() || dev(shm) == dev(&probe) || std::fs::create_dir_all(shm.join("klipp-run-test")).is_err() {
        eprintln!("skipped: no second writable file system (/dev/shm) beside {}", probe.display());
        return;
    }
    for autosave in [false, true] {
        for point in [MovePoint::InPlace, MovePoint::PartRemoved] {
            let r = root(&format!("movecrash-{autosave}-{point:?}"));
            let dir = project(&r, "Film");
            let before = snapshot(&dir);
            let auto = r.join("Film_autosave.klipp");
            let auto_before = snapshot(&auto);
            let mut s = settings(&r, vec![dir.clone()]);
            s.backup_root = shm.join(format!("klipp-run-test/{}-{autosave}-{point:?}", std::process::id()));
            let _ = std::fs::remove_dir_all(&s.backup_root);
            let crash = move |at: MovePoint, to: &Path| at == point && to.to_string_lossy().contains("_autosave") == autosave;
            let hooks = Hooks { crash: &|_, _| false, crash_in_move: &crash, render_check: &|_| Ok(()) };
            assert_eq!(run(&s, &ok, &ok_parity, &hooks).unwrap_err(), RunError::CrashedInMove(point));
            let mut report = Report::default();
            recover(&s, &mut report).unwrap();
            assert_converted(&s, &dir, &before);
            assert_eq!(snapshot(&in_backup(&s, &auto)), auto_before, "{autosave}/{point:?}: the auto-save's backup is whole");
            assert!(!auto.exists(), "and what was left of it is gone");
            let _ = std::fs::remove_dir_all(&s.backup_root);
            let _ = std::fs::remove_dir_all(&r);
        }
    }
    let _ = std::fs::remove_dir_all(&probe);
}

/// An exclusion names the folder however it is written: a trailing slash
/// or a link to it.
#[test]
fn an_exclusion_matches_the_folder_it_names() {
    let r = root("exclude");
    let dir = project(&r, "Film");
    let link = r.join("Link.klipp");
    std::os::unix::fs::symlink(&dir, &link).unwrap();
    let before = snapshot(&dir);
    let mut s = settings(&r, vec![dir.clone()]);
    s.excluded.insert(PathBuf::from(format!("{}/", link.display())));
    let report = run(&s, &ok, &ok_parity, &Hooks::NONE).unwrap();
    assert_eq!(report.excluded, vec![dir.clone()]);
    assert_eq!(snapshot(&dir), before);
    let _ = std::fs::remove_dir_all(&r);
}
