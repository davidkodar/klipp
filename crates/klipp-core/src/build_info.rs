//! Which build of Klipp is running.
//!
//! The binary records the commit it was built from; the running process
//! reports that, with the time its executable was written, in the log, in
//! About and over the control socket. A process started before a rebuild
//! says so: the executable on disk is then newer than the one it runs.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::UNIX_EPOCH;

/// What the running binary was built from, and when.
#[derive(Debug, Clone)]
pub struct BuildInfo {
    /// The package version.
    pub version: &'static str,
    /// The short commit hash, or `unknown` when built outside a git checkout.
    pub commit: &'static str,
    /// Whether tracked files differed from the commit when it was built.
    pub dirty: bool,
    /// The cargo profile: `release` or `debug`.
    pub profile: &'static str,
    /// The executable, as this process started it.
    pub exe: Option<PathBuf>,
    /// When that executable was written, in seconds since the Unix epoch.
    pub built_at: Option<u64>,
}

static BUILD: OnceLock<BuildInfo> = OnceLock::new();

impl BuildInfo {
    /// The build of this process's executable, with what its build script
    /// recorded.
    pub fn of_this_process(
        version: &'static str,
        commit: &'static str,
        dirty: bool,
        profile: &'static str,
    ) -> Self {
        let exe = std::env::current_exe().ok();
        let built_at = exe.as_deref().and_then(modified_secs);
        Self {
            version,
            commit,
            dirty,
            profile,
            exe,
            built_at,
        }
    }

    /// `0.1.0 36466b08 release`, with `-dirty` after the commit when it was.
    pub fn label(&self) -> String {
        format!(
            "{} {}{} {}",
            self.version,
            self.commit,
            if self.dirty { "-dirty" } else { "" },
            self.profile
        )
    }

    /// Whether the executable on disk has been rebuilt since this process
    /// started it, so what runs is not what a restart would run.
    pub fn newer_build_on_disk(&self) -> bool {
        match (&self.exe, self.built_at) {
            (Some(exe), Some(at)) => modified_secs(exe).is_some_and(|now| now > at),
            _ => false,
        }
    }
}

fn modified_secs(path: &Path) -> Option<u64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    modified
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// Record this process's build. The first call wins.
pub fn set(info: BuildInfo) {
    let _ = BUILD.set(info);
}

/// This process's build: `unknown` where the binary recorded none, as in
/// tests.
pub fn get() -> &'static BuildInfo {
    BUILD.get_or_init(|| BuildInfo {
        version: env!("CARGO_PKG_VERSION"),
        commit: "unknown",
        dirty: false,
        profile: "unknown",
        exe: None,
        built_at: None,
    })
}

/// `secs` since the Unix epoch as `YYYY-MM-DD HH:MM:SS UTC`.
pub fn format_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Days since the epoch to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        rem / 3_600,
        rem / 60 % 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_time_reads_as_its_utc_date() {
        assert_eq!(format_utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(format_utc(951_868_800), "2000-03-01 00:00:00 UTC");
        assert_eq!(format_utc(1_709_210_096), "2024-02-29 12:34:56 UTC");
    }

    #[test]
    fn a_build_labels_its_commit() {
        let build = BuildInfo {
            version: "0.1.0",
            commit: "36466b08",
            dirty: true,
            profile: "release",
            exe: None,
            built_at: None,
        };
        assert_eq!(build.label(), "0.1.0 36466b08-dirty release");
        assert!(!build.newer_build_on_disk(), "no executable, nothing newer");
    }
}
