//! Temp directories for generated test fixtures, that clean up after
//! the runs before them.
//!
//! Tests that need real media generate it with the ffmpeg CLI into a
//! directory named after the test process. That part is deliberate —
//! cargo runs test binaries in parallel, and a shared filename means one
//! run reads a file while another is still writing it, which shows up as
//! a black frame and looks like a pipeline bug.
//!
//! What was missing is the other end. Nothing removed those directories,
//! so every `cargo test` left one behind with its generated video
//! inside. Over a day of repeated full-suite runs that reached **313
//! directories holding 5.4 GB** on a 16 GB tmpfs — and the thing that
//! finally broke was not a test but the linker: `rust-lld` mmaps its
//! output, and when the filesystem fills underneath that mapping the
//! write lands on a page it cannot back, which arrives as SIGBUS. It
//! died six times before anyone connected a failing build to a test
//! directory.

use std::path::{Path, PathBuf};

/// This process's fixture directory under the test scratch root, created,
/// with the directories of finished runs swept away first.
///
/// `prefix` names the family — `"klipp-viewer-export"` gives
/// `target/test-scratch/klipp-viewer-export-<pid>` by default.
/// An explicit `TMPDIR` wins; otherwise `CARGO_TARGET_DIR` (or the
/// workspace target directory) owns these large generated media files.
///
/// The sweep is exact rather than a guess about age: a directory named
/// after a process id that is no longer running is definitively finished
/// with. Where that cannot be established (no `/proc`), it falls back to
/// leaving anything touched in the last hour alone, so a run starting
/// alongside this one is never pulled out from under it.
pub fn scratch_dir(prefix: &str) -> PathBuf {
    let base = scratch_root();
    let me = std::process::id();
    sweep(&base, prefix, me);
    let dir = base.join(format!("{prefix}-{me}"));
    std::fs::create_dir_all(&dir)
        .unwrap_or_else(|e| panic!("cannot create fixture dir {}: {e}", dir.display()));
    dir
}

/// Parent for generated media. Do not default to the system's often-small
/// RAM-backed `/tmp`: export fixtures can consume gigabytes.
pub fn scratch_root() -> PathBuf {
    choose_root(
        std::env::var_os("TMPDIR").map(PathBuf::from),
        std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").as_path(),
    )
}

fn choose_root(tmpdir: Option<PathBuf>, target: Option<PathBuf>, workspace: &Path) -> PathBuf {
    let root = tmpdir.filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| {
        target.filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| workspace.join("target")).join("test-scratch")
    });
    // Cargo runs tests from the package directory, not the workspace
    // where a relative target directory was selected.
    if root.is_absolute() { root } else { workspace.join(root) }
}

/// [`scratch_dir`] and a file inside it.
pub fn scratch_file(prefix: &str, name: &str) -> PathBuf {
    scratch_dir(prefix).join(name)
}

fn sweep(base: &Path, prefix: &str, me: u32) {
    let Ok(entries) = std::fs::read_dir(base) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(pid) = name.strip_prefix(prefix).and_then(|r| r.strip_prefix('-')) else {
            continue;
        };
        let Ok(pid) = pid.parse::<u32>() else { continue };
        if pid == me || !finished(pid, &entry.path()) {
            continue;
        }
        // Best effort: another run may be sweeping the same directory, or
        // it may not be ours to remove. Either way there is nothing to
        // report — the next run tries again.
        let _ = std::fs::remove_dir_all(entry.path());
    }
}

/// Whether the run that owned `dir` is over.
fn finished(pid: u32, dir: &Path) -> bool {
    if Path::new("/proc/self").is_dir() {
        return !Path::new(&format!("/proc/{pid}")).exists();
    }
    // No /proc to ask, so fall back on age. An hour is far longer than
    // any of these tests takes and short enough that a day's runs do not
    // pile up.
    dir.metadata()
        .and_then(|m| m.modified())
        .and_then(|t| t.elapsed().map_err(std::io::Error::other))
        .is_ok_and(|age| age.as_secs() > 3600)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_media_uses_target_unless_tmpdir_is_explicit() {
        let workspace = Path::new("/workspace");
        assert_eq!(choose_root(None, None, workspace), workspace.join("target/test-scratch"));
        assert_eq!(choose_root(None, Some("/build".into()), workspace), Path::new("/build/test-scratch"));
        assert_eq!(choose_root(None, Some("shared-build".into()), workspace), workspace.join("shared-build/test-scratch"));
        assert_eq!(choose_root(Some("/scratch".into()), Some("/build".into()), workspace), Path::new("/scratch"));
        assert_eq!(choose_root(Some("".into()), Some("".into()), workspace), workspace.join("target/test-scratch"));
    }

    /// The directory is made, and named for this process.
    #[test]
    fn it_makes_a_directory_for_this_run() {
        let d = scratch_dir("klipp-scratch-selftest");
        assert!(d.is_dir(), "{} was not created", d.display());
        assert!(
            d.file_name().unwrap().to_str().unwrap().ends_with(&std::process::id().to_string()),
            "the directory is not named for this process: {}",
            d.display(),
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A directory left by a run that is over gets removed — the whole
    /// point, since not removing them is what filled the tmpfs.
    #[test]
    fn it_sweeps_a_finished_runs_directory() {
        let prefix = "klipp-scratch-sweeptest";
        // pid 1 is init, which is certainly running, and u32::MAX is
        // above every pid on Linux, so it certainly is not.
        let dead = scratch_root().join(format!("{prefix}-{}", u32::MAX));
        let alive = scratch_root().join(format!("{prefix}-1"));
        std::fs::create_dir_all(&dead).unwrap();
        std::fs::create_dir_all(&alive).unwrap();
        std::fs::write(dead.join("leftover.bin"), b"x").unwrap();

        let mine = scratch_dir(prefix);

        assert!(!dead.exists(), "a finished run's directory was left behind");
        if Path::new("/proc/self").is_dir() {
            assert!(alive.exists(), "a live process's directory must not be removed");
        }
        let _ = std::fs::remove_dir_all(&alive);
        let _ = std::fs::remove_dir_all(&mine);
    }

    /// Directories of other families are none of its business.
    #[test]
    fn it_leaves_other_prefixes_alone() {
        let other = scratch_root().join(format!("klipp-scratch-other-{}", u32::MAX));
        std::fs::create_dir_all(&other).unwrap();
        let mine = scratch_dir("klipp-scratch-mine");
        assert!(other.exists(), "a different prefix was swept");
        let _ = std::fs::remove_dir_all(&other);
        let _ = std::fs::remove_dir_all(&mine);
    }
}
