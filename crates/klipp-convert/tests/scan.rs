//! The dry run finds what it should, reports each condition, and writes
//! nothing (#1054).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use klipp_convert::running::{answering_sockets, app_processes};
use klipp_convert::scan::{discover, scan, scan_project};

/// A fresh temporary folder.
fn temp(label: &str) -> PathBuf {
    let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos();
    let p = std::env::temp_dir().join(format!("klipp_convert_{label}_{}_{nanos}", std::process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

const PROJECT: &str = r#"{"version":"1.0.0","klipp_version_min":"0.1.0","generation":"g1","sequences":[]}"#;
const MANIFEST: &str = r#"{"version":"1.0.0","generation":"g1","items":[]}"#;

/// A Klipp project at `dir`, as a save leaves it.
fn project(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir.join("media")).unwrap();
    std::fs::write(dir.join("project.json"), PROJECT).unwrap();
    std::fs::write(dir.join("media/manifest.json"), MANIFEST).unwrap();
    dir.to_path_buf()
}

fn set_mtime(p: &Path, t: SystemTime) {
    std::fs::File::open(p).unwrap().set_modified(t).unwrap();
}

/// Every path under `dir` with its size and modification time.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, (u64, SystemTime)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let m = std::fs::symlink_metadata(e.path()).unwrap();
            out.insert(e.path(), (m.len(), m.modified().unwrap()));
            if m.is_dir() {
                stack.push(e.path());
            }
        }
    }
    out
}

#[test]
fn only_klipp_projects_outside_git_trees_and_build_folders_are_found() {
    let home = temp("discover");
    let kept = project(&home.join("Videos/Trip.klipp"));
    // Another application's project.json, with no Klipp manifest beside it.
    std::fs::create_dir_all(home.join("code/web/media")).unwrap();
    std::fs::write(home.join("code/web/project.json"), r#"{"name":"web","version":"2"}"#).unwrap();
    std::fs::write(home.join("code/web/media/manifest.json"), r#"{"files":[]}"#).unwrap();
    // A fixture in a git tree, a worktree, a build folder, the backups,
    // the staging folders and the Trash.
    std::fs::create_dir_all(home.join("repo/.git")).unwrap();
    project(&home.join("repo/fixtures/Test.klipp"));
    std::fs::create_dir_all(home.join("worktree")).unwrap();
    std::fs::write(home.join("worktree/.git"), "gitdir: elsewhere").unwrap();
    project(&home.join("worktree/fixtures/Test.klipp"));
    project(&home.join("build/target/debug/Test.klipp"));
    project(&home.join(".local/share/klipp/pre-v2/Trip.klipp"));
    project(&home.join("Videos/Trip.klipp.klipp-convert"));
    project(&home.join("Videos/Trip.klipp.klipp-v1"));
    project(&home.join(".local/share/Trash/files/Old.klipp"));

    assert_eq!(discover(&home).projects, vec![kept]);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_collected_project_is_found() {
    let home = temp("collected");
    let source = project(&home.join("Work/Source.klipp"));
    let dest = home.join("Delivery/Collected.klipp");
    let seq = vec![klipp_timeline::Sequence::new_default()];
    let options = klipp_project::consolidate::ConsolidateOptions { dest_dir: dest.clone(), include_proxies: false, include_skims: false, mode: Default::default(), looks: Vec::new(), fonts: Vec::new() };
    klipp_project::consolidate::consolidate_project(&source, &klipp_media::MediaPool::new(), &seq, &Default::default(), options, None).unwrap();
    assert!(discover(&home).projects.contains(&dest), "the Collect Project destination is found: {:?}", discover(&home));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_newer_autosave_is_judged_by_the_folders_times() {
    let home = temp("autosave");
    let dir = project(&home.join("Trip.klipp"));
    let auto = project(&home.join("Trip_autosave.klipp"));
    let now = SystemTime::now();
    set_mtime(&dir, now - Duration::from_secs(600));
    set_mtime(&auto, now);
    let s = scan_project(&dir, false, None);
    assert!(s.autosave && s.newer_autosave, "{s:?}");
    set_mtime(&auto, now - Duration::from_secs(1200));
    assert!(!scan_project(&dir, false, None).newer_autosave, "an older auto-save is not newer");
    // The auto-save is reported with its project, not as a project.
    let report = scan(&home, &home.join(".config"));
    assert_eq!(report.projects.iter().map(|p| p.path.clone()).collect::<Vec<_>>(), vec![dir.clone()]);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_pending_recovery_counts_only_when_the_pair_is_not_whole() {
    let home = temp("recovery");
    let dir = project(&home.join("Trip.klipp"));
    std::fs::create_dir_all(dir.join(".recovery")).unwrap();
    std::fs::write(dir.join(".recovery/pending"), "g2").unwrap();
    assert!(!scan_project(&dir, false, None).pending_recovery, "a mark with a whole pair: the save finished");
    std::fs::write(dir.join("project.json"), PROJECT.replace("g1", "g2")).unwrap();
    assert!(scan_project(&dir, false, None).pending_recovery, "a mark and a pair from two saves");
    let _ = std::fs::remove_dir_all(&home);
}

/// A pair from two saves with no recovery mark is reported apart: the app
/// mends it from the backups beside the pair, which a converted copy
/// doesn't carry (#1054 review).
#[test]
fn a_pair_from_two_saves_without_a_mark_is_reported() {
    let home = temp("pair");
    let dir = project(&home.join("Trip.klipp"));
    assert!(!scan_project(&dir, false, None).pair_mismatch, "a whole pair");
    std::fs::write(dir.join("project.json"), PROJECT.replace("g1", "g2")).unwrap();
    let s = scan_project(&dir, false, None);
    assert!(s.pair_mismatch && !s.pending_recovery, "two saves, no mark");
    std::fs::create_dir_all(dir.join(".recovery")).unwrap();
    std::fs::write(dir.join(".recovery/pending"), "g2").unwrap();
    let s = scan_project(&dir, false, None);
    assert!(!s.pair_mismatch && s.pending_recovery, "with the mark it is a stopped save");
    let config = home.join(".config");
    std::fs::remove_dir_all(dir.join(".recovery")).unwrap();
    assert_eq!(scan(&home, &config).counts().pair_mismatches, 1);
    let _ = std::fs::remove_dir_all(&home);
}

/// A folder the walk can't list is counted and named, never passed over
/// in silence: it may hold projects.
#[test]
fn a_folder_that_cannot_be_read_is_counted() {
    use std::os::unix::fs::PermissionsExt;
    let home = temp("unreadable");
    project(&home.join("Videos/Trip.klipp"));
    let locked = home.join("Locked");
    project(&locked.join("Hidden.klipp"));
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let d = discover(&home);
    let report = scan(&home, &home.join(".config"));
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(d.unreadable, vec![locked.clone()]);
    assert_eq!(report.counts().unreadable_dirs, 1);
    assert!(report.text().contains("couldn't be read"), "{}", report.text());
    let _ = std::fs::remove_dir_all(&home);
}

/// Whether Klipp runs can't be told from a process folder that can't be
/// listed, or a runtime folder that can't: an error, never "nothing runs".
#[test]
fn not_being_able_to_tell_is_an_error() {
    use std::os::unix::fs::PermissionsExt;
    assert!(app_processes(Path::new("/nonexistent/klipp-proc")).is_err());
    let run = temp("runtime");
    assert_eq!(answering_sockets(Some(&run.join("gone")), None), Ok(Vec::new()), "a runtime folder that isn't there holds none");
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o000)).unwrap();
    let r = answering_sockets(Some(&run), None);
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(r.is_err());
    let _ = std::fs::remove_dir_all(&run);
}

#[test]
fn links_are_reported_and_not_followed() {
    let home = temp("links");
    let real = project(&home.join("Elsewhere/Real.klipp"));
    std::os::unix::fs::symlink(&real, home.join("Linked.klipp")).unwrap();
    std::fs::create_dir_all(home.join("Shared")).unwrap();
    std::os::unix::fs::symlink(home.join("Shared"), real.join("media/shared")).unwrap();
    let found = discover(&home).projects;
    assert_eq!(found, vec![real.clone()], "the linked folder is not walked into, so the project is listed once");
    let s = scan_project(&real, false, None);
    assert_eq!(s.symlinks, vec![PathBuf::from("media/shared")]);
    assert!(scan_project(&home.join("Linked.klipp"), true, None).is_symlink);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_recent_list_adds_projects_the_walk_does_not_reach() {
    let home = temp("recent");
    let away = temp("recent_away");
    let outside = project(&away.join("Outside.klipp"));
    let config = home.join(".config");
    std::fs::create_dir_all(config.join("klipp")).unwrap();
    let prefs = serde_json::json!({ "general": { "recent_projects": [ { "path": outside, "name": "Outside" }, { "path": away.join("Gone.klipp"), "name": "Gone" } ] } });
    std::fs::write(config.join("klipp/preferences.json"), prefs.to_string()).unwrap();
    let report = scan(&home, &config);
    assert_eq!(report.projects.len(), 1);
    assert!(report.projects[0].from_recent_list && report.projects[0].path == outside);
    assert_eq!(report.recent_missing, vec![away.join("Gone.klipp")]);
    for d in [&home, &away] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[test]
fn side_stores_and_sizes_are_counted_without_media() {
    let home = temp("stores");
    let dir = project(&home.join("Trip.klipp"));
    std::fs::create_dir_all(dir.join("looks")).unwrap();
    std::fs::write(dir.join("looks/a.json"), "{}").unwrap();
    std::fs::write(dir.join("looks/b.json"), "{}").unwrap();
    std::fs::create_dir_all(dir.join("media/stills")).unwrap();
    std::fs::write(dir.join("media/stills.json"), "[]").unwrap();
    // A collected media file, listed by the manifest: not backed up.
    std::fs::write(dir.join("media/clip.mov"), vec![0u8; 5000]).unwrap();
    std::fs::write(dir.join("media/manifest.json"), r#"{"version":"1.0.0","generation":"g1","items":[{"path":"media/clip.mov"}]}"#).unwrap();
    std::fs::create_dir_all(dir.join(".klipp-cache")).unwrap();
    std::fs::write(dir.join(".klipp-cache/thumb"), vec![0u8; 7000]).unwrap();
    let s = scan_project(&dir, false, None);
    assert_eq!(s.side_stores.looks, 2);
    assert!(s.side_stores.stills_index && s.side_stores.stills_dir);
    assert!(s.backup_bytes < 1000, "media and cache left out: {}", s.backup_bytes);
    assert!(s.rewritten_bytes > 0 && s.rewritten_bytes <= s.backup_bytes);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_scan_writes_nothing_inside_what_it_reads() {
    let home = temp("readonly");
    let dir = project(&home.join("Trip.klipp"));
    project(&home.join("Trip_autosave.klipp"));
    std::fs::create_dir_all(dir.join(".recovery")).unwrap();
    std::fs::write(dir.join(".recovery/pending"), "g2").unwrap();
    let before = snapshot(&home);
    let report = scan(&home, &home.join(".config"));
    assert_eq!(report.projects.len(), 1);
    assert_eq!(snapshot(&home), before, "nothing under the scanned home changed");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_klipp_process_is_found_by_its_executable_or_its_name() {
    let proc = temp("proc");
    for (pid, name, exe) in [(100, "klipp-app", None), (200, "bash", Some("/usr/bin/bash")), (300, "x", Some("/usr/bin/klipp")), (400, "klipp-convert", None)] {
        let d = proc.join(pid.to_string());
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("comm"), format!("{name}\n")).unwrap();
        if let Some(exe) = exe {
            std::os::unix::fs::symlink(exe, d.join("exe")).unwrap();
        }
    }
    std::fs::create_dir_all(proc.join("self")).unwrap();
    assert_eq!(app_processes(&proc), Ok(vec![100, 300]));
    let _ = std::fs::remove_dir_all(&proc);
}

#[test]
fn only_a_control_socket_that_answers_counts() {
    // The gate's TMPDIR can exceed Unix socket path limits. Keep just
    // this socket fixture under a short, unique path, as the MCP tests do.
    let run = PathBuf::from("/tmp").join(format!("klipp-scan-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&run).unwrap();
    let live = run.join("klipp-control.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&live).unwrap();
    // A socket file left by an app that has quit: nothing answers.
    let stale = run.join("klipp-control-42.sock");
    drop(std::os::unix::net::UnixListener::bind(&stale).unwrap());
    let other = run.join("pipewire-0");
    let _p = std::os::unix::net::UnixListener::bind(&other).unwrap();
    assert_eq!(answering_sockets(Some(&run), None), Ok(vec![live.clone()]));
    let custom = run.join("custom.sock");
    let _c = std::os::unix::net::UnixListener::bind(&custom).unwrap();
    assert_eq!(answering_sockets(Some(&run), Some(&custom)), Ok(vec![custom.clone(), live]));
    let _ = std::fs::remove_dir_all(&run);
}

#[test]
fn projects_in_a_test_runs_scratch_are_counted_not_listed() {
    let home = temp("scratch");
    let kept = project(&home.join("Videos/Trip.klipp"));
    project(&home.join("Work/tmp-build/run1/a.klipp"));
    project(&home.join("Work/tmp-build/run2/b.klipp"));
    project(&home.join(".claude/jobs/abc/tmp/c.klipp"));
    project(&home.join("Work/tmp_de/d.klipp"));
    let d = discover(&home);
    assert_eq!(d.projects, vec![kept]);
    assert_eq!(d.scratch, 4, "every scratch project is counted");
    // In the recent list too: a project in a session's tmp folder.
    let away = std::env::temp_dir().join(format!("klipp_scratch_recent_{}", std::process::id()));
    let outside = project(&away.join("tmp/Session.klipp"));
    let config = home.join(".config");
    std::fs::create_dir_all(config.join("klipp")).unwrap();
    std::fs::write(config.join("klipp/preferences.json"), serde_json::json!({ "general": { "recent_projects": [ { "path": outside } ] } }).to_string()).unwrap();
    let report = scan(&home, &config);
    assert_eq!(report.projects.len(), 1);
    assert_eq!(report.scratch_skipped, 5);
    assert_eq!(report.counts().scratch_skipped, 5);
    for d in [&home, &away] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// A folder holding the scratch mark is a harness's output: every project
/// under it, however deep, is counted as scratch and never listed.
#[test]
fn projects_under_a_scratch_mark_are_counted_not_listed() {
    let home = temp("scratch-mark");
    let kept = project(&home.join("Videos/Trip.klipp"));
    let run = home.join("Notes/ab-run");
    project(&run.join("a1/project-copy.klipp"));
    project(&run.join("deeper/b2/projects/fx.klipp"));
    std::fs::write(run.join(klipp_convert::scan::SCRATCH_MARK), b"").unwrap();
    // A project beside the marked folder, not under it, is still found.
    let beside = project(&home.join("Notes/Real.klipp"));
    let d = discover(&home);
    assert_eq!(d.projects, vec![beside, kept]);
    assert_eq!(d.scratch, 2);
    let _ = std::fs::remove_dir_all(&home);
}
