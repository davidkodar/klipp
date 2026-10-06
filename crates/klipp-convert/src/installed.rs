//! The real conversion refuses to run while an installed Klipp predates the
//! format revision check.
//!
//! A build from before 6fd04616 doesn't read `format_revision`. It guards
//! against a converted project only through the effect sentinel, which
//! needs a clip to carry it, and it ignores track effects and grade groups
//! in every file. The installed package is the one such build that lingers,
//! so it is rebuilt first, and the conversion checks that the package's
//! commit (from its version in `pacman -Qi`) contains e610ac5e: the
//! revision refusal (6fd04616) and the presets that survive an old save
//! (#1050). The binary
//! itself is never run: an old build would take over the live control
//! socket.

use std::path::Path;
use std::process::Command;

/// The oldest commit an installed build may be. Builds since 6fd04616
/// refuse a newer `format_revision`, which keeps them off converted
/// projects and the side stores read only after a project loads; the
/// global colour presets sit behind no refusal, and only builds since
/// e610ac5e (#1050) leave a preset they can't read alone instead of
/// overwriting one of the same name.
pub const FIRST_REFUSING_COMMIT: &str = "e610ac5e";

/// What the installed package says about old builds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installed {
    /// No Klipp package is installed.
    None,
    /// Installed, built from a commit that has the revision check.
    Current { version: String, commit: String },
    /// Installed, built from before the check: rebuild and reinstall first.
    TooOld { version: String, commit: String },
    /// Installed, but its commit can't be read from the version or isn't in
    /// this repository; treated as too old.
    Unknown { version: String },
}

impl Installed {
    /// Whether the real run may start.
    pub fn allows_conversion(&self) -> bool {
        matches!(self, Self::None | Self::Current { .. })
    }
}

/// The commit in a `-git` package version, `r<count>.<hash>-<pkgrel>`.
pub fn commit_of_version(version: &str) -> Option<&str> {
    let rest = version.strip_prefix('r')?;
    let (_count, tail) = rest.split_once('.')?;
    let hash = tail.split('-').next()?;
    (hash.len() >= 7 && hash.chars().all(|c| c.is_ascii_hexdigit())).then_some(hash)
}

/// The `Version` field of `pacman -Qi` output.
pub fn version_of_query(output: &str) -> Option<&str> {
    output.lines().find_map(|l| {
        let (key, value) = l.split_once(':')?;
        (key.trim() == "Version").then(|| value.trim())
    })
}

/// Read the installed package and judge it against `repo`'s history.
///
/// Fails closed: only pacman's confirmed "was not found" means no package.
/// A query that can't run, any other failure, or output without a Version
/// line is [`Installed::Unknown`], which refuses. The locale is pinned so
/// the output is pacman's own words.
pub fn check(repo: &Path) -> Installed {
    let out = match Command::new("pacman").args(["-Qi", "klipp"]).env("LC_ALL", "C").output() {
        Ok(out) => out,
        Err(e) => return Installed::Unknown { version: format!("pacman couldn't run: {e}") },
    };
    let stderr = String::from_utf8_lossy(&out.stderr);
    judge_query(repo, out.status.success(), &String::from_utf8_lossy(&out.stdout), &stderr)
}

/// [`check`]'s decision on pacman's exit status and output.
pub fn judge_query(repo: &Path, success: bool, stdout: &str, stderr: &str) -> Installed {
    if !success {
        return if stderr.contains("was not found") {
            Installed::None
        } else {
            Installed::Unknown { version: format!("pacman failed: {}", stderr.trim()) }
        };
    }
    match version_of_query(stdout) {
        Some(version) => judge(repo, version.to_owned()),
        None => Installed::Unknown { version: "pacman gave no Version line".into() },
    }
}

/// Judge a package version against `repo`: its commit must contain
/// [`FIRST_REFUSING_COMMIT`].
pub fn judge(repo: &Path, version: String) -> Installed {
    let Some(commit) = commit_of_version(&version).map(str::to_owned) else {
        return Installed::Unknown { version };
    };
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["merge-base", "--is-ancestor", FIRST_REFUSING_COMMIT, &commit])
        .stderr(std::process::Stdio::null())
        .status();
    match status.ok().and_then(|s| s.code()) {
        Some(0) => Installed::Current { version, commit },
        Some(1) => Installed::TooOld { version, commit },
        _ => Installed::Unknown { version },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUERY: &str = "Name            : klipp-git\nVersion         : r1908.3f7ae79c-1\nBuild Date      : Mon 28 Sep 2026\n";

    #[test]
    fn the_commit_is_read_from_a_git_package_version() {
        assert_eq!(version_of_query(QUERY), Some("r1908.3f7ae79c-1"));
        assert_eq!(commit_of_version("r1908.3f7ae79c-1"), Some("3f7ae79c"));
        assert_eq!(commit_of_version("r2100.6fd04616abcd-2"), Some("6fd04616abcd"));
        for v in ["0.1.0-1", "r1908-1", "r1908.zzzzzzzz-1", "r1.abc-1", ""] {
            assert_eq!(commit_of_version(v), None, "{v}");
        }
    }

    fn repo() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    /// The installed package of 2026-09-28 is too old; the commit that
    /// added the check, and anything after it, is current.
    #[test]
    fn builds_are_judged_by_their_history() {
        let old = judge(&repo(), "r1908.3f7ae79c-1".into());
        assert_eq!(old, Installed::TooOld { version: "r1908.3f7ae79c-1".into(), commit: "3f7ae79c".into() });
        assert!(!old.allows_conversion());
        // The revision check alone isn't enough: the presets need #1050.
        assert!(matches!(judge(&repo(), "r2000.6fd04616-1".into()), Installed::TooOld { .. }));
        let first = judge(&repo(), format!("r2000.{FIRST_REFUSING_COMMIT}-1"));
        assert!(matches!(first, Installed::Current { .. }));
        assert!(first.allows_conversion());
        assert!(matches!(judge(&repo(), "0.1.0-1".into()), Installed::Unknown { .. }));
        assert!(matches!(judge(&repo(), "r1.0123456789abcdef-1".into()), Installed::Unknown { .. }), "a commit this repository doesn't have");
        assert!(Installed::None.allows_conversion());
    }

    /// Only a confirmed absence lets the run start; anything unclear refuses.
    #[test]
    fn the_query_fails_closed() {
        let r = repo();
        assert_eq!(judge_query(&r, false, "", "error: package 'klipp' was not found\n"), Installed::None);
        for (ok, out, err) in [(false, "", "error: could not open database"), (true, "Name : klipp-git\n", ""), (true, "Version : 0.1.0-1\n", "")] {
            let j = judge_query(&r, ok, out, err);
            assert!(!j.allows_conversion(), "{ok} {out:?} {err:?} -> {j:?}");
        }
        assert!(matches!(judge_query(&r, true, QUERY, ""), Installed::TooOld { .. }));
    }
}
