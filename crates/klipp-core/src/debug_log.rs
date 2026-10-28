//! The debug log's file (#2988): written for as long as the app runs,
//! in pieces of a bounded size, the oldest let go.
//!
//! The log was one file opened at launch and written through to the end.
//! A write that failed was dropped without a word, and a file moved or
//! removed under the app took every line after it with it: a persona run's
//! log stopped two minutes in while the app ran on, and nothing said why.
//! This writer starts a new piece past [`RotatingLog::max_bytes`], keeping
//! [`RotatingLog::keep`] earlier ones (`name.1` the newest), opens the
//! file again when a write fails or the file has gone, and says so once on
//! stderr and in the log itself.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// How often, in bytes written, the writer checks its file is still there.
const CHECK_EVERY: u64 = 256 * 1024;

struct Open {
    file: Option<File>,
    /// Bytes in the current piece.
    written: u64,
    /// Bytes since the file was last checked for.
    since_check: u64,
    /// Writes that failed since the last one that worked.
    failed: u64,
}

/// The debug log, rotated by size.
pub struct RotatingLog {
    path: PathBuf,
    /// The most one piece holds before the next is started.
    pub max_bytes: u64,
    /// Earlier pieces kept beside the current one.
    pub keep: usize,
    open: Mutex<Open>,
}

impl RotatingLog {
    /// The log at `path`, its previous run's moved aside as the first
    /// earlier piece and the oldest beyond `keep` let go: at most
    /// `(keep + 1) * max_bytes` on disk.
    pub fn start(path: impl Into<PathBuf>, max_bytes: u64, keep: usize) -> io::Result<Self> {
        let path = path.into();
        shift(&path, keep);
        let file = File::create(&path)?;
        Ok(Self { path, max_bytes: max_bytes.max(4096), keep, open: Mutex::new(Open { file: Some(file), written: 0, since_check: 0, failed: 0 }) })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn write_line(&self, buf: &[u8]) {
        let mut o = self.open.lock().unwrap_or_else(|e| e.into_inner());
        if o.written > 0 && o.written + buf.len() as u64 > self.max_bytes {
            shift(&self.path, self.keep);
            o.file = None;
        }
        // Moved or removed under the app: a new file where it was.
        if o.since_check >= CHECK_EVERY {
            o.since_check = 0;
            if !self.path.exists() {
                o.file = None;
            }
        }
        if o.file.is_none() {
            match OpenOptions::new().create(true).append(true).open(&self.path) {
                Ok(f) => {
                    o.written = f.metadata().map(|m| m.len()).unwrap_or(0);
                    o.file = Some(f);
                }
                Err(e) => {
                    o.failed += 1;
                    if o.failed == 1 {
                        eprintln!("klipp: the debug log {} cannot be opened: {e}", self.path.display());
                    }
                    return;
                }
            }
        }
        let failed_before = o.failed;
        let result = o.file.as_mut().expect("opened above").write_all(buf);
        match result {
            Ok(()) => {
                o.written += buf.len() as u64;
                o.since_check += buf.len() as u64;
                if failed_before > 0 {
                    o.failed = 0;
                    let note = format!("klipp: the debug log could not be written for {failed_before} lines; it is written again\n");
                    let _ = o.file.as_mut().expect("open").write_all(note.as_bytes());
                }
            }
            Err(e) => {
                o.failed += 1;
                o.file = None;
                if o.failed == 1 {
                    eprintln!("klipp: writing the debug log {} failed: {e}; trying again", self.path.display());
                }
            }
        }
    }
}

/// `path`'s pieces moved one along, `path` itself becoming `path.1`, and
/// the one past `keep` removed.
fn shift(path: &Path, keep: usize) {
    let piece = |n: usize| PathBuf::from(format!("{}.{n}", path.display()));
    let _ = std::fs::remove_file(piece(keep.max(1)));
    for n in (1..keep.max(1)).rev() {
        let _ = std::fs::rename(piece(n), piece(n + 1));
    }
    let _ = std::fs::rename(path, piece(1));
}

/// Each event is one write of a whole line (the tracing formatter writes
/// one buffer per event); a failure is the writer's to handle, never the
/// caller's.
impl Write for &RotatingLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_line(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("klipp-debug-log-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn line(n: usize) -> String {
        format!("2026-10-28T14:00:32.000000Z  INFO klipp_test: line {n:06} {}\n", "x".repeat(60))
    }

    /// #2988: written far past where the persona run's log stopped (1.76
    /// MB), in pieces: every line is in the log, the newest in the file,
    /// and the whole stays within its bound.
    #[test]
    fn the_log_is_written_past_the_old_stop_in_bounded_pieces() {
        let d = dir("rotate");
        let path = d.join("klipp-debug.log");
        std::fs::write(&path, "the previous run\n").unwrap();
        let log = RotatingLog::start(&path, 1 << 20, 3).unwrap();
        let mut w = &log;
        let n = 60_000; // some 6 MB
        for i in 0..n {
            w.write_all(line(i).as_bytes()).unwrap();
        }
        let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        let pieces: Vec<PathBuf> = std::iter::once(path.clone()).chain((1..=3).map(|k| PathBuf::from(format!("{}.{k}", path.display())))).collect();
        let total: u64 = pieces.iter().map(|p| size(p)).sum();
        assert!(total <= 4 << 20, "within (keep + 1) pieces: {total}");
        assert!(!PathBuf::from(format!("{}.4", path.display())).exists(), "nothing past the kept pieces");
        let newest = std::fs::read_to_string(&path).unwrap();
        assert!(newest.ends_with(&line(n - 1)), "the last line is the last written");
        assert!(size(&path) > 0 && size(&path) <= 1 << 20);
        // The kept pieces run on without a gap into the current one.
        let joined: String = pieces.iter().rev().filter_map(|p| std::fs::read_to_string(p).ok()).collect();
        let first_kept: usize = joined.lines().next().unwrap()["2026-10-28T14:00:32.000000Z  INFO klipp_test: line ".len()..][..6].parse().unwrap();
        for (k, l) in joined.lines().enumerate() {
            assert_eq!(l, line(first_kept + k).trim_end(), "line {k} of the kept log");
        }
        std::fs::remove_dir_all(&d).ok();
    }

    /// A log removed under the app is made again, and the lines after go
    /// on into it.
    #[test]
    fn a_removed_log_is_made_again() {
        let d = dir("removed");
        let path = d.join("klipp-debug.log");
        let log = RotatingLog::start(&path, 64 << 20, 2).unwrap();
        let mut w = &log;
        w.write_all(b"before\n").unwrap();
        std::fs::remove_file(&path).unwrap();
        // Past one check's worth of bytes.
        for i in 0..5_000 {
            w.write_all(line(i).as_bytes()).unwrap();
        }
        let text = std::fs::read_to_string(&path).expect("made again");
        assert!(text.ends_with(&line(4_999)), "the newest lines are there");
        std::fs::remove_dir_all(&d).ok();
    }
}
