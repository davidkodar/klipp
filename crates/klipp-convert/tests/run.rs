//! `klipp-convert run` on disk (#1087), in temporary folders only: every
//! project, backup, journal and config folder here is made by the test.

use std::collections::BTreeSet;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use klipp_convert::run::{real_settings, recover, run, Hooks, MovePoint, Report, RunError, Settings, Step};
use klipp_convert::StoreKind;
use serde_json::{json, Value};
use klipp_timeline::GradeAccess;

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

/// A v1 project as the build before the switch saved it (through the v1
/// writer kept as the oracle, since the app now writes revision 2), with
/// a Look, a still, media, and the files the copy must leave out or keep
/// as they are.
fn project(parent: &Path, name: &str) -> PathBuf {
    use klipp_core::{MediaId, RationalTime};
    let dir = parent.join(format!("{name}.klipp"));
    let t = |s: f64| RationalTime::from_secs_f64(s, 24);
    let mut seq = klipp_timeline::Sequence::new_default();
    let mut clip = klipp_timeline::Clip::media(MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0));
    clip.set_grade(graph());
    clip.set_effect_list(vec![klipp_effects::BuiltinEffect::GaussianBlur { radius: 2.0 }], clip.builtin_effects_enabled().to_vec());
    seq.tracks[0].clips.push(clip);
    klipp_project::save_project_v1(&dir, &[seq], &klipp_media::MediaPool::new()).unwrap();
    klipp_project::save_looks_v1(&dir, &[klipp_timeline::Look { id: uuid::Uuid::new_v4(), name: "Warm".into(), node_graph: graph() }]).unwrap();
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
    assert_eq!(revision_of(&dir.join("project.json")), klipp_convert::revision2::PROJECT_REVISION, "{}", dir.display());
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
    assert!(std::fs::File::open(s.backup_root.join("run.lock")).unwrap().try_lock().is_ok(), "the lock released");
    assert!(std::fs::read_dir(s.backup_root.join("journal")).unwrap().next().is_none());
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
            let hooks = Hooks { crash: &crash, crash_in_move: &|_, _| false, render_check: &|_| Ok(()), ..Hooks::NONE };
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
    let hooks = Hooks { crash: &|_, _| false, crash_in_move: &|_, _| false, render_check: &|_| Err("a frame differs".into()), ..Hooks::NONE };
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
    // The lock is the system's, not the file's words: held, even with the
    // file still empty (a run that has only just taken it), it refuses;
    // a file naming a live process but not held is no lock.
    std::fs::create_dir_all(&s.backup_root).unwrap();
    let held = std::fs::File::create(s.backup_root.join("run.lock")).unwrap();
    held.try_lock().unwrap();
    assert!(matches!(run(&s, &ok, &ok_parity, &Hooks::NONE), Err(RunError::Refused(_))));
    assert_eq!(snapshot(&dir), before);
    drop(held);
    std::fs::write(s.backup_root.join("run.lock"), std::process::id().to_string()).unwrap();
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
    // Tool and single-effect presets, as the app keeps them: left as they
    // are at revision 2, and counted (#1126).
    std::fs::create_dir_all(config.join("presets/color/wheels")).unwrap();
    std::fs::write(config.join("presets/color/wheels/Lift.json"), json!({ "name": "Lift", "tool": "Wheels", "params": { "exposure": 0.3 } }).to_string()).unwrap();
    std::fs::create_dir_all(config.join("effect_presets/gaussian_blur")).unwrap();
    std::fs::write(config.join("effect_presets/gaussian_blur/Soft.json"), json!({ "name": "Soft", "description": "", "effect_key": "gaussian_blur" }).to_string()).unwrap();
    let kept = |c: &Path| (std::fs::read(c.join("presets/color/wheels/Lift.json")).unwrap(), std::fs::read(c.join("effect_presets/gaussian_blur/Soft.json")).unwrap());
    let kept_before = kept(&config);
    let found = klipp_convert::scan::presets_in(&config);
    assert_eq!((found.colour, found.tool, found.effect), (2, 1, 1), "an effect's presets are counted in its folder");
    let before = snapshot(&config);
    let mut s = settings(&r, Vec::new());
    s.config_dir = Some(config.clone());
    let report = run(&s, &ok, &ok_parity, &Hooks::NONE).unwrap();
    assert_eq!(report.presets_converted, 1, "{report:?}");
    assert_eq!((report.tool_presets_unchanged, report.effect_presets_unchanged), (1, 1));
    assert_eq!(kept(&config), kept_before, "tool and effect presets unchanged");
    assert_eq!(report.presets_stopped, vec![(config.join("presets/color/broken.json"), "doesn't parse as JSON; left as it is".to_string())]);
    assert_eq!(revision_of(&config.join("presets/color/Warm.json")), 2);
    assert_eq!(std::fs::read(config.join("presets/color/broken.json")).unwrap(), b"{ not json", "a file that won't parse is kept and named");
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
            let hooks = Hooks { crash: &|_, _| false, crash_in_move: &crash, render_check: &|_| Ok(()), ..Hooks::NONE };
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

/// Make `<name>_autosave.klipp` beside `parent`'s project a whole project,
/// as the build before the switch saved it, so discovery finds it; its
/// time `secs`.
fn autosave(parent: &Path, name: &str, secs: u64) -> PathBuf {
    let auto = parent.join(format!("{name}_autosave.klipp"));
    let _ = std::fs::remove_dir_all(&auto);
    klipp_project::save_project_v1(&auto, &[klipp_timeline::Sequence::new_default()], &klipp_media::MediaPool::new()).unwrap();
    assert!(klipp_convert::scan::is_klipp_project(&auto));
    set_time(&auto, secs);
    auto
}

/// An auto-save in the run's list goes with its project: into the backup
/// when the project is converted, left as it is when the project is
/// stopped or excluded, and never converted on its own (#1087 review).
#[test]
fn an_autosave_goes_with_its_project() {
    for case in ["converted", "stopped", "excluded"] {
        let r = root(&format!("autosave-with-{case}"));
        let dir = project(&r, "Film");
        let newer = case == "stopped";
        let auto = autosave(&r, "Film", if newer { 1_200_000_000 } else { 1_000_000_000 });
        let (before, auto_before) = (snapshot(&dir), snapshot(&auto));
        let mut s = settings(&r, vec![auto.clone(), dir.clone()]);
        if case == "excluded" {
            s.excluded.insert(dir.clone());
        }
        let report = run(&s, &ok, &ok_parity, &Hooks::NONE).unwrap();
        assert!(!report.converted.contains(&auto) && !report.stopped.iter().any(|(d, _)| d == &auto), "{case}: {report:?}");
        match case {
            "converted" => {
                assert_eq!(report.converted, vec![dir.clone()]);
                assert_converted(&s, &dir, &before);
                assert!(!auto.exists());
                assert_eq!(snapshot(&in_backup(&s, &auto)), auto_before, "the auto-save backed up as it was");
            }
            _ => {
                assert!(report.converted.is_empty(), "{case}: {report:?}");
                if case == "excluded" {
                    assert_eq!(report.excluded, vec![auto.clone(), dir.clone()]);
                }
                assert_eq!((snapshot(&dir), snapshot(&auto)), (before, auto_before), "{case}: both left as they are");
                assert_eq!(revision_of(&auto.join("project.json")), 0);
            }
        }
        let _ = std::fs::remove_dir_all(&r);
    }
}

/// An auto-save whose project isn't in the run is a stop, left as it is.
#[test]
fn an_autosave_without_its_project_is_a_stop() {
    let r = root("orphan-autosave");
    let auto = autosave(&r, "Gone", 1_000_000_000);
    let before = snapshot(&auto);
    let s = settings(&r, vec![auto.clone()]);
    let report = run(&s, &ok, &ok_parity, &Hooks::NONE).unwrap();
    assert!(report.stopped.iter().any(|(d, why)| d == &auto && why.contains("auto-save")), "{report:?}");
    assert_eq!(snapshot(&auto), before);
    let _ = std::fs::remove_dir_all(&r);
}

/// The real run's list, end to end on a made-up home: the projects under
/// it with their auto-saves, and a recent project outside it, each once.
#[test]
fn the_real_settings_find_every_project_once() {
    let r = root("real-settings");
    let home = r.join("home");
    let videos = home.join("Videos");
    std::fs::create_dir_all(&videos).unwrap();
    let film = project(&videos, "Film");
    let auto = autosave(&videos, "Film", 1_000_000_000);
    let out = project(&r.join("outside"), "Out");
    std::os::unix::fs::symlink(&film, home.join("Film-link.klipp")).unwrap();
    let config = home.join(".config/klipp");
    std::fs::create_dir_all(&config).unwrap();
    let recent = json!({ "general": { "recent_projects": [{ "path": out }, { "path": film }, { "path": home.join("Film-link.klipp") }] } });
    std::fs::write(config.join("preferences.json"), recent.to_string()).unwrap();
    let (film_before, out_before) = (snapshot(&film), snapshot(&out));

    let s = real_settings(&home, &config, BTreeSet::new()).unwrap();
    assert!(s.projects.contains(&film) && s.projects.contains(&auto) && s.projects.contains(&out), "{:?}", s.projects);
    assert_eq!(s.backup_root, home.join(".local/share/klipp/pre-v2"));
    let report = run(&s, &ok, &ok_parity, &Hooks::NONE).unwrap();
    let mut converted = report.converted.clone();
    converted.sort();
    assert_eq!(converted, vec![film.clone(), out.clone()], "{report:?}");
    assert!(report.stopped.is_empty(), "{report:?}");
    assert_converted(&s, &film, &film_before);
    assert_converted(&s, &out, &out_before);
    assert!(!auto.exists() && in_backup(&s, &auto).join("project.json").is_file());
    let _ = std::fs::remove_dir_all(&r);
}

/// A journal entry that is empty or torn is set aside and named; it stops
/// neither this run nor the next (#1087 review).
#[test]
fn a_damaged_journal_entry_is_set_aside_and_named() {
    let r = root("damaged-journal");
    let dir = project(&r, "Film");
    let before = snapshot(&dir);
    let s = settings(&r, vec![dir.clone()]);
    let journal = s.backup_root.join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    std::fs::write(journal.join("empty.json"), b"").unwrap();
    std::fs::write(journal.join("torn.json"), b"{ \"dir\": \"/x").unwrap();
    let report = run(&s, &ok, &ok_parity, &Hooks::NONE).unwrap();
    let mut damaged = report.damaged_journals.clone();
    damaged.sort();
    assert_eq!(damaged, vec![journal.join("damaged/empty.json"), journal.join("damaged/torn.json")]);
    assert_eq!(std::fs::read(journal.join("damaged/torn.json")).unwrap(), b"{ \"dir\": \"/x", "kept as it was");
    assert_eq!(report.converted, vec![dir.clone()]);
    assert_converted(&s, &dir, &before);
    assert!(run(&s, &ok, &ok_parity, &Hooks::NONE).unwrap().damaged_journals.is_empty(), "the next run isn't stopped by them");
    let _ = std::fs::remove_dir_all(&r);
}

/// An exclusion that names no project in the run refuses it: a typo would
/// otherwise convert the project the user meant to protect.
#[test]
fn an_exclusion_that_names_no_project_refuses() {
    let r = root("exclude-typo");
    let dir = project(&r, "Film");
    let before = snapshot(&dir);
    let mut s = settings(&r, vec![dir.clone()]);
    s.excluded.insert(r.join("Flim.klipp"));
    match run(&s, &ok, &ok_parity, &Hooks::NONE) {
        Err(RunError::Refused(why)) => assert!(why.contains("Flim.klipp"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(snapshot(&dir), before);
    let _ = std::fs::remove_dir_all(&r);
}

/// Both renames of the swap done while the journal still says Verified:
/// recovery goes by the disk and finishes the project, rather than
/// calling it rolled back with the converted folder in place.
#[test]
fn recovery_goes_by_the_disk_when_both_renames_are_unrecorded() {
    let r = root("both-renames");
    let dir = project(&r, "Film");
    let before = snapshot(&dir);
    let s = settings(&r, vec![dir.clone()]);
    let crash = |at: Step, recorded: bool| at == Step::OriginalAside && !recorded;
    let hooks = Hooks { crash: &crash, ..Hooks::NONE };
    assert_eq!(run(&s, &ok, &ok_parity, &hooks).unwrap_err(), RunError::Crashed(Step::OriginalAside));
    // The second rename, as a crash after it and before its record leaves it.
    std::fs::rename(format!("{}.klipp-convert", dir.display()), &dir).unwrap();
    let mut report = Report::default();
    recover(&s, &mut report).unwrap();
    assert_eq!(report.recovered, vec![(dir.clone(), "finished".to_string())]);
    assert_converted(&s, &dir, &before);
    let _ = std::fs::remove_dir_all(&r);
}

/// A Klipp started after the run began ends the run before the next swap,
/// that project rolled back.
#[test]
fn a_klipp_started_mid_run_ends_it_before_the_swap() {
    let r = root("swap-refused");
    let dir = project(&r, "Film");
    let before = snapshot(&dir);
    let s = settings(&r, vec![dir.clone()]);
    let hooks = Hooks { swap_allowed: &|| Err("a Klipp is running".into()), ..Hooks::NONE };
    match run(&s, &ok, &ok_parity, &hooks) {
        Err(RunError::Refused(why)) => assert!(why.contains("a Klipp is running"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(snapshot(&dir), before);
    assert!(!PathBuf::from(format!("{}.klipp-convert", dir.display())).exists());
    assert!(std::fs::read_dir(s.backup_root.join("journal")).unwrap().next().is_none(), "the journal entry is gone");
    let _ = std::fs::remove_dir_all(&r);
}

/// A project whose looks folder is a link to one outside it is a stop: the
/// converted Looks would otherwise be written through the link into the
/// folder it names before any swap or backup (#1121 review).
#[test]
fn a_linked_folder_the_conversion_writes_into_is_a_stop() {
    for linked in ["looks", "media"] {
        let r = root(&format!("linked-{linked}"));
        let dir = project(&r, "Film");
        let outside = r.join(format!("shared-{linked}"));
        std::fs::rename(dir.join(linked), &outside).unwrap();
        std::os::unix::fs::symlink(&outside, dir.join(linked)).unwrap();
        let (before, outside_before) = (snapshot(&dir), snapshot(&outside));
        let s = settings(&r, vec![dir.clone()]);
        let report = run(&s, &ok, &ok_parity, &Hooks::NONE).unwrap();
        assert!(report.stopped.iter().any(|(d, why)| d == &dir && why.contains("is a link")), "{linked}: {report:?}");
        assert_eq!((snapshot(&dir), snapshot(&outside)), (before, outside_before), "{linked}: nothing written through the link");
        assert!(!PathBuf::from(format!("{}.klipp-convert", dir.display())).exists());
        let _ = std::fs::remove_dir_all(&r);
    }
}

/// The original renamed aside, then the sync after the rename refused:
/// the run doesn't roll back as if the rename never happened, it keeps the
/// journal, and recovery finishes the project (#1121 review).
#[test]
fn a_sync_refused_after_a_rename_goes_forward() {
    let r = root("sync-after-rename");
    let dir = project(&r, "Film");
    let before = snapshot(&dir);
    let s = settings(&r, vec![dir.clone()]);
    // Writable and enterable, not readable: the rename works, opening the
    // folder to sync it doesn't.
    let swap_allowed = || {
        std::fs::set_permissions(&r, std::fs::Permissions::from_mode(0o300)).unwrap();
        Ok(())
    };
    let hooks = Hooks { swap_allowed: &swap_allowed, ..Hooks::NONE };
    let result = run(&s, &ok, &ok_parity, &hooks);
    std::fs::set_permissions(&r, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(result, Err(RunError::Io(_))), "{result:?}");
    assert!(!dir.exists() && PathBuf::from(format!("{}.klipp-v1", dir.display())).is_dir(), "the original went aside");
    assert_eq!(std::fs::read_dir(s.backup_root.join("journal")).unwrap().count(), 1, "the journal kept");
    let mut report = Report::default();
    recover(&s, &mut report).unwrap();
    assert_eq!(report.recovered, vec![(dir.clone(), "finished".to_string())]);
    assert_converted(&s, &dir, &before);
    let _ = std::fs::remove_dir_all(&r);
}

/// Two runs at once: while one holds the lock part way through a project,
/// the other is refused before touching anything, and the first finishes
/// (#1121 review).
#[test]
fn a_second_run_is_refused_while_the_first_holds_the_lock() {
    let r = root("contention");
    let (dir, other) = (project(&r, "Film"), project(&r.join("b"), "Other"));
    let before = snapshot(&dir);
    let other_before = snapshot(&other);
    let s = settings(&r, vec![dir.clone()]);
    let (inside, release) = (std::sync::mpsc::channel::<()>(), std::sync::mpsc::channel::<()>());
    let first = {
        let s = s.clone();
        let (tell, wait) = (inside.0, release.1);
        std::thread::spawn(move || {
            // Holds the lock in its render check until told to go on.
            let check = move |_: &Path| -> Result<(), String> {
                tell.send(()).unwrap();
                wait.recv().unwrap();
                Ok(())
            };
            run(&s, &ok, &ok_parity, &Hooks { render_check: &check, ..Hooks::NONE })
        })
    };
    inside.1.recv().unwrap();
    let second = settings(&r, vec![other.clone()]);
    match run(&second, &ok, &ok_parity, &Hooks::NONE) {
        Err(RunError::Refused(why)) => assert!(why.contains("another run holds"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(snapshot(&other), other_before, "the second run touched nothing");
    release.0.send(()).unwrap();
    assert_eq!(first.join().unwrap().unwrap().converted, vec![dir.clone()]);
    assert_converted(&s, &dir, &before);
    let _ = std::fs::remove_dir_all(&r);
}

/// Every folder the run makes is synced into the folder above it, so one
/// above that can't be synced stops the run before anything is moved
/// (#1121 review).
#[test]
fn a_new_folder_that_cannot_be_synced_stops_the_run() {
    let r = root("ancestor-sync");
    let dir = project(&r, "Film");
    let before = snapshot(&dir);
    let locked = r.join("locked");
    std::fs::create_dir_all(&locked).unwrap();
    // Writable and enterable, not readable: a folder can be made in it,
    // but it can't be opened to sync the new entry.
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o300)).unwrap();
    let mut s = settings(&r, vec![dir.clone()]);
    s.backup_root = locked.join("backup/pre-v2");
    let result = run(&s, &ok, &ok_parity, &Hooks::NONE);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(&result, Err(RunError::Refused(why)) if why.contains("syncing")), "{result:?}");
    assert_eq!(snapshot(&dir), before);
    assert!(!PathBuf::from(format!("{}.klipp-convert", dir.display())).exists());
    // A rerun finds the folders the first one made, and syncs them all the
    // same: still refused (#1121 review).
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o300)).unwrap();
    let again = run(&s, &ok, &ok_parity, &Hooks::NONE);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(&again, Err(RunError::Refused(why)) if why.contains("syncing")), "{again:?}");
    assert_eq!(snapshot(&dir), before);
    let _ = std::fs::remove_dir_all(&r);
}
