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

/// The debug log's file name.
pub const FILE_NAME: &str = "klipp-debug.log";

/// Where the debug log is written. Outside a Flatpak, the working directory
/// as always. In a Flatpak, the app's own state folder
/// (`~/.var/app/<id>/.local/state/klipp/`): there the working directory is
/// wherever Klipp happened to be started from, so a person asked for the
/// log couldn't be told where to find it.
pub fn path() -> PathBuf {
    path_for(
        crate::flatpak::app_id().is_some(),
        std::env::var_os("XDG_STATE_HOME").map(PathBuf::from).as_deref(),
        std::env::var_os("HOME").map(PathBuf::from).as_deref(),
    )
}

/// [`path`] from whether Klipp is a Flatpak, `XDG_STATE_HOME` and `HOME`.
/// A relative or empty `XDG_STATE_HOME` is ignored, as the XDG spec says.
pub fn path_for(flatpak: bool, state_home: Option<&Path>, home: Option<&Path>) -> PathBuf {
    if !flatpak {
        return PathBuf::from(FILE_NAME);
    }
    let state = state_home
        .filter(|p| p.is_absolute())
        .map(Path::to_path_buf)
        .or_else(|| home.filter(|h| h.is_absolute()).map(|h| h.join(".local/state")));
    match state {
        Some(state) => state.join("klipp").join(FILE_NAME),
        None => PathBuf::from(FILE_NAME),
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

/// Lines queued for the log at most: past them a line is dropped and
/// counted, not waited for.
const QUEUE_LINES: usize = 32 * 1024;

/// The debug log written on a thread of its own. A write to a file waits
/// when the disk is busy: other files being flushed held the window's
/// thread for half a second on one log line. A line is handed over here
/// and written there, in order; past [`QUEUE_LINES`] waiting, a line is
/// dropped, and how many is said in the log once it has caught up.
/// [`QueuedLog::flush`] waits for what is queued: the panic hook and the
/// exit call it. Killed outright, the lines still queued are lost.
pub struct QueuedLog {
    lines: std::sync::mpsc::SyncSender<Vec<u8>>,
    queued: std::sync::Arc<std::sync::atomic::AtomicU64>,
    written: std::sync::Arc<std::sync::atomic::AtomicU64>,
    dropped: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// When the first and the last line not yet said dropped were, in µs
    /// from `epoch`.
    drop_span: std::sync::Arc<std::sync::Mutex<Option<(u64, u64)>>>,
    epoch: std::time::Instant,
}

impl QueuedLog {
    /// `log`, written on its own thread.
    pub fn start(log: std::sync::Arc<RotatingLog>) -> Self {
        Self::start_with(move |line| log.write_line(line))
    }

    /// [`Self::start`] with the line's writer given, for tests.
    #[doc(hidden)]
    pub fn start_with(mut write: impl FnMut(&[u8]) + Send + 'static) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::Arc;
        let (lines, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(QUEUE_LINES);
        let (queued, written, dropped) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let drop_span = Arc::new(std::sync::Mutex::new(None::<(u64, u64)>));
        let epoch = std::time::Instant::now();
        let (w, d, span) = (written.clone(), dropped.clone(), drop_span.clone());
        let spawned = std::thread::Builder::new().name("debug-log".into()).spawn(move || {
            let mut said_dropped = 0u64;
            while let Ok(line) = rx.recv() {
                write(&line);
                w.fetch_add(1, Ordering::SeqCst);
                let now_dropped = d.load(Ordering::SeqCst);
                if now_dropped > said_dropped {
                    let now_us = epoch.elapsed().as_micros() as u64;
                    let when = span.lock().unwrap_or_else(|e| e.into_inner()).take().map(|(first, last)| {
                        let ago = |t: u64| now_us.saturating_sub(t) as f64 / 1e6;
                        format!(", from {:.2} s to {:.2} s before this line", ago(first), ago(last))
                    });
                    write(format!("debug log: {} line(s) dropped while the disk was busy{}\n", now_dropped - said_dropped, when.unwrap_or_default()).as_bytes());
                    said_dropped = now_dropped;
                }
            }
        });
        if let Err(e) = spawned {
            // No thread: nothing would be written. Said where it can be.
            eprintln!("debug log: could not start its writer: {e}");
        }
        Self { lines, queued, written, dropped, drop_span, epoch }
    }

    fn queue(&self, line: &[u8]) {
        use std::sync::atomic::Ordering;
        match self.lines.try_send(line.to_vec()) {
            Ok(()) => {
                self.queued.fetch_add(1, Ordering::SeqCst);
            }
            Err(_) => {
                let at = self.epoch.elapsed().as_micros() as u64;
                let mut span = self.drop_span.lock().unwrap_or_else(|e| e.into_inner());
                *span = Some(span.map_or((at, at), |(first, _)| (first, at)));
                // Counted under the span's lock, so the note's count and
                // its span are of the same lines.
                self.dropped.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    /// Wait, at most `within`, for every line queued so far to be
    /// written. `false` when they were not all written in time.
    pub fn flush(&self, within: std::time::Duration) -> bool {
        use std::sync::atomic::Ordering;
        let want = self.queued.load(Ordering::SeqCst);
        let t0 = std::time::Instant::now();
        while self.written.load(Ordering::SeqCst) < want {
            if t0.elapsed() >= within {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        true
    }
}

/// The process's debug log, once installed, for those that end the
/// process to flush.
static GLOBAL: std::sync::OnceLock<std::sync::Arc<QueuedLog>> = std::sync::OnceLock::new();

/// Make `log` the one [`flush_global`] waits for.
pub fn install_global(log: std::sync::Arc<QueuedLog>) {
    let _ = GLOBAL.set(log);
}

/// Wait, at most `within`, for the process's queued log lines to be
/// written: before an exit or after a panic is logged.
pub fn flush_global(within: std::time::Duration) {
    if let Some(log) = GLOBAL.get() {
        log.flush(within);
    }
}

impl Write for &QueuedLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.queue(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A line is handed over without waiting for a slow disk, and every
    /// line arrives, in order, once flushed.
    #[test]
    fn a_log_line_does_not_wait_for_a_slow_disk() {
        let got = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
        let g = got.clone();
        let log = QueuedLog::start_with(move |line| {
            std::thread::sleep(std::time::Duration::from_millis(20));
            g.lock().unwrap().push(String::from_utf8_lossy(line).into_owned());
        });
        let t0 = std::time::Instant::now();
        let mut w = &log;
        for i in 0..50 {
            w.write_all(format!("line {i}\n").as_bytes()).unwrap();
        }
        assert!(t0.elapsed() < std::time::Duration::from_millis(100), "the writes waited {:?} for the disk", t0.elapsed());
        assert!(log.flush(std::time::Duration::from_secs(10)), "flushed in time");
        let got = got.lock().unwrap().clone();
        assert_eq!(got, (0..50).map(|i| format!("line {i}\n")).collect::<Vec<_>>());
    }

    /// Past the queue's bound a line is dropped, not waited for, and the
    /// log says how many once it catches up.
    #[test]
    fn a_full_queue_drops_and_says_so() {
        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
        let got = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
        let g = got.clone();
        let mut first = true;
        let log = QueuedLog::start_with(move |line| {
            if first {
                // Held until the queue has filled behind it.
                let _ = go_rx.recv();
                first = false;
            }
            g.lock().unwrap().push(String::from_utf8_lossy(line).into_owned());
        });
        let mut w = &log;
        let t0 = std::time::Instant::now();
        for i in 0..QUEUE_LINES + 100 {
            w.write_all(format!("{i}\n").as_bytes()).unwrap();
        }
        assert!(t0.elapsed() < std::time::Duration::from_secs(2), "a full queue was waited for");
        go_tx.send(()).unwrap();
        assert!(log.flush(std::time::Duration::from_secs(10)));
        // The note follows the line written after the drops were counted.
        let t1 = std::time::Instant::now();
        while t1.elapsed() < std::time::Duration::from_secs(5) && !got.lock().unwrap().iter().any(|l| l.contains("dropped")) {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let got = got.lock().unwrap().clone();
        let note = got.iter().find(|l| l.contains("dropped")).expect("the drops are said");
        assert!(note.contains("line(s) dropped") && note.contains("s before this line"), "{note}");
    }

    /// A Flatpak's debug log is in its own state folder, whatever folder
    /// Klipp was started from; elsewhere it stays in the working directory.
    #[test]
    fn a_flatpaks_debug_log_is_in_its_state_folder() {
        let state = Path::new("/home/u/.var/app/io.github.davidkodar.Klipp/.local/state");
        let home = Path::new("/home/u");
        assert_eq!(path_for(true, Some(state), Some(home)), state.join("klipp/klipp-debug.log"));
        assert_eq!(path_for(true, None, Some(home)), PathBuf::from("/home/u/.local/state/klipp/klipp-debug.log"), "no XDG_STATE_HOME: its default under HOME");
        assert_eq!(path_for(true, Some(Path::new("relative/state")), Some(home)), PathBuf::from("/home/u/.local/state/klipp/klipp-debug.log"), "a relative XDG_STATE_HOME is ignored");
        assert_eq!(path_for(true, None, None), PathBuf::from(FILE_NAME), "nothing to go by: the working directory");
        assert_eq!(path_for(false, Some(state), Some(home)), PathBuf::from(FILE_NAME), "outside a Flatpak, as before");
    }

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
