//! Who has a project open: a lock file inside the project folder, so a
//! second editor opening the same project from shared storage is told,
//! rather than both saving and the second save silently replacing the
//! first editor's afternoon.
//!
//! `project.lock` records the user, host, process and a heartbeat the
//! holder refreshes while the project is open. A crash leaves the file
//! behind, so a lock is judged by that heartbeat: one refreshed recently
//! is somebody working, one gone quiet for [`STALE_AFTER_SECS`] is a
//! session that ended without closing, and one held by a process on this
//! machine that no longer exists is certainly dead.
//!
//! Taking a lock from someone else is allowed — a machine died holding
//! it — and is written to `lock_history.log` in the project, so what
//! happened can be explained afterwards. The holder that lost it finds
//! out on its next heartbeat and stops saving.
//!
//! On NFS and SMB a lock file is advisory: two opens in the same instant
//! can both believe they won. That is acceptable for what this is for. It
//! turns silent loss into a warning nearly always, and does not claim to
//! be a guarantee.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// The lock file's name inside a project folder.
pub const LOCK_FILE: &str = "project.lock";

/// The record of who took the lock from whom, inside a project folder.
pub const HISTORY_FILE: &str = "lock_history.log";

/// How often the holder refreshes the heartbeat.
pub const HEARTBEAT_SECS: u64 = 30;

/// A heartbeat older than this is a session that ended without closing:
/// four missed beats, so a busy moment or a slow share is not mistaken
/// for a crash.
pub const STALE_AFTER_SECS: u64 = HEARTBEAT_SECS * 4;

/// What a lock file says. Every field defaults, so a lock written by an
/// older or newer build still reads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LockInfo {
    pub user: String,
    pub host: String,
    /// `/etc/machine-id`: what makes "this machine" certain. Two machines
    /// can share a hostname (every fresh install is `archlinux`); they
    /// do not share this.
    pub machine: String,
    pub pid: u32,
    /// Unique per app session, so a process can tell its own lock from
    /// one a restarted process with the same pid left behind.
    pub session: String,
    /// Unix seconds.
    pub opened_at: u64,
    /// Unix seconds; refreshed every [`HEARTBEAT_SECS`].
    pub heartbeat_at: u64,
}

impl LockInfo {
    /// `user@host`, for messages.
    pub fn who(&self) -> String {
        format!("{}@{}", self.user, self.host)
    }

    /// Seconds since the holder last showed signs of life.
    pub fn quiet_for(&self, now: u64) -> u64 {
        now.saturating_sub(self.heartbeat_at)
    }
}

/// How a lock held by someone else looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// The heartbeat is recent: someone is working in it now.
    Active,
    /// The heartbeat stopped long ago: most likely a crash or a machine
    /// put to sleep.
    Stale,
    /// Held by a process on this machine that no longer exists.
    Dead,
}

/// The state of a project's lock, as seen by this session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockState {
    /// Nobody holds it.
    Free,
    /// This session holds it.
    Ours,
    /// Someone else holds it.
    Held(LockInfo, Liveness),
}

/// This app session's identity, fixed for the life of the process.
#[derive(Debug, Clone)]
pub struct Holder {
    pub user: String,
    pub host: String,
    pub machine: String,
    pub pid: u32,
    pub session: String,
}

impl Holder {
    /// The current user, host and process, with a fresh session id.
    pub fn current() -> Self {
        let user = ["USER", "USERNAME", "LOGNAME"]
            .iter()
            .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
            .unwrap_or_else(|| "unknown".into());
        let host = std::fs::read_to_string("/proc/sys/kernel/hostname")
            .or_else(|_| std::fs::read_to_string("/etc/hostname"))
            .ok()
            .map(|h| h.trim().to_string())
            .filter(|h| !h.is_empty())
            .or_else(|| std::env::var("COMPUTERNAME").ok())
            .unwrap_or_else(|| "unknown-host".into());
        let machine = ["/etc/machine-id", "/var/lib/dbus/machine-id"]
            .iter()
            .find_map(|p| std::fs::read_to_string(p).ok())
            .map(|m| m.trim().to_string())
            .unwrap_or_default();
        let pid = std::process::id();
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        Self { user, host, machine, pid, session: format!("{pid:x}-{nanos:x}") }
    }

    fn info(&self, opened_at: u64, now: u64) -> LockInfo {
        LockInfo {
            user: self.user.clone(),
            host: self.host.clone(),
            machine: self.machine.clone(),
            pid: self.pid,
            session: self.session.clone(),
            opened_at,
            heartbeat_at: now,
        }
    }
}

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn lock_path(project: &Path) -> PathBuf {
    project.join(LOCK_FILE)
}

/// Read a project's lock file. `None` when there is none, or when it
/// cannot be read as one — a lock nobody can interpret protects nothing,
/// and treating it as held would leave the project unopenable.
pub fn read(project: &Path) -> Option<LockInfo> {
    // A lock file is a few hundred bytes: anything past 64 KiB is not one.
    let text = crate::path_util::read_to_string_capped(&lock_path(project), 64 * 1024).ok()?;
    serde_json::from_str(&text).ok()
}

/// How the lock `info` looks from this machine at `now`.
pub fn liveness(info: &LockInfo, me: &Holder, now: u64) -> Liveness {
    // Only a lock from this very machine can be judged by its pid: a
    // matching hostname is not enough, and without a machine id on both
    // sides the heartbeat decides.
    let this_machine = !info.machine.is_empty() && info.machine == me.machine;
    if this_machine && !process_exists(info.pid) {
        return Liveness::Dead;
    }
    if info.quiet_for(now) > STALE_AFTER_SECS {
        Liveness::Stale
    } else {
        Liveness::Active
    }
}

fn process_exists(pid: u32) -> bool {
    if cfg!(target_os = "linux") {
        Path::new(&format!("/proc/{pid}")).exists()
    } else {
        // No cheap portable check; assume alive and let the heartbeat
        // decide.
        true
    }
}

/// The lock's state for this session.
pub fn inspect(project: &Path, me: &Holder) -> LockState {
    match read(project) {
        None => LockState::Free,
        Some(info) if info.session == me.session => LockState::Ours,
        Some(info) => {
            let l = liveness(&info, me, now_secs());
            LockState::Held(info, l)
        }
    }
}

/// Take the lock if nobody else holds it. `Err` names the holder.
///
/// The lock appears complete or not at all: it is written to a file of
/// this session's own and hard-linked into place, which fails if a lock
/// exists. So a second opener never reads a half-written lock, takes it
/// for garbage and replaces it — both would think they had won. Where
/// hard links are not supported (some SMB shares) it falls back to an
/// exclusive create.
///
/// A lock file that cannot be read is replaced only once it is older
/// than [`STALE_AFTER_SECS`]: a fresh one is somebody's lock being
/// written, and counts as held.
pub fn acquire(project: &Path, me: &Holder) -> Result<(), Box<(LockInfo, Liveness)>> {
    let now = now_secs();
    match inspect(project, me) {
        LockState::Ours => return heartbeat(project, me).map_err(|lost| Box::new((lost, Liveness::Active))),
        LockState::Held(info, l) => return Err(Box::new((info, l))),
        LockState::Free => {}
    }
    let body = serde_json::to_vec_pretty(&me.info(now, now)).unwrap_or_default();
    let path = lock_path(project);
    match place_exclusively(project, &path, &body, &me.session) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => match read(project) {
            // Someone else holds it, or took it between the look and now.
            Some(info) if info.session != me.session => {
                let l = liveness(&info, me, now);
                Err(Box::new((info, l)))
            }
            Some(_) => Ok(()),
            None if file_age_secs(&path).is_some_and(|age| age > STALE_AFTER_SECS) => match write_atomic(&path, &body) {
                Ok(()) => Ok(()),
                Err(e) => {
                    tracing::warn!("project lock: could not replace an unreadable {}: {e}", path.display());
                    Ok(())
                }
            },
            // Fresh and unreadable: being written by someone right now.
            None => Err(Box::new((
                LockInfo { user: "someone".into(), host: "another session".into(), heartbeat_at: now, ..LockInfo::default() },
                Liveness::Active,
            ))),
        },
        Err(e) => {
            // A read-only share, say. Not being able to lock must not
            // stop anyone opening their project; it only stops the
            // warning reaching the next person.
            tracing::warn!("project lock: could not create {}: {e}", path.display());
            Ok(())
        }
    }
}

/// Put `body` at `path` only if nothing is there, all at once.
fn place_exclusively(project: &Path, path: &Path, body: &[u8], session: &str) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = project.join(format!(".{LOCK_FILE}.{session}"));
    let write_tmp = || -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(body)?;
        f.sync_all()
    };
    let linked = write_tmp().and_then(|()| std::fs::hard_link(&tmp, path));
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(e),
        Err(e) if e.kind() == std::io::ErrorKind::Unsupported || e.raw_os_error() == Some(1) || e.raw_os_error() == Some(95) => {
            // EPERM / EOPNOTSUPP: no hard links here. An exclusive create
            // is the next best; a reader may briefly see it empty, which
            // `acquire` treats as held rather than as garbage.
            let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
            f.write_all(body)?;
            f.sync_all()
        }
        other => other,
    }
}

fn file_age_secs(path: &Path) -> Option<u64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    SystemTime::now().duration_since(modified).ok().map(|d| d.as_secs())
}

/// Take the lock whoever holds it, and record that it was taken. `Err`
/// when the lock could not be written — then nothing was taken, and the
/// caller must not act as if it holds the project.
pub fn take_over(project: &Path, me: &Holder, from: Option<&LockInfo>) -> std::io::Result<()> {
    let now = now_secs();
    let body = serde_json::to_vec_pretty(&me.info(now, now)).unwrap_or_default();
    write_atomic(&lock_path(project), &body)?;
    if read(project).is_none_or(|info| info.session != me.session) {
        return Err(std::io::Error::other("the lock did not read back as ours"));
    }
    if let Some(prev) = from {
        let line = format!(
            "{} {} took the lock from {} (pid {}, opened {}, last seen {} ago)\n",
            crate::iso8601_now(),
            format_args!("{}@{}", me.user, me.host),
            prev.who(),
            prev.pid,
            unix_to_iso(prev.opened_at),
            describe_secs(prev.quiet_for(now)),
        );
        append(&project.join(HISTORY_FILE), &line);
    }
    Ok(())
}

/// Refresh this session's heartbeat. `Err` with the new holder when the
/// lock has been taken over, which means this session must stop saving.
/// A lock file that has vanished is written again: the session still
/// has the project open.
///
/// There is no compare-and-swap on a plain file, so a takeover landing
/// between the check and the write would be overwritten. The check is
/// made again immediately before writing, which leaves a window of
/// microseconds. If it is ever lost, the taker's next heartbeat finds
/// this session's lock and the taker goes read-only while this session
/// keeps saving: the takeover fails, but there is still one writer, and
/// nobody's work is replaced.
pub fn heartbeat(project: &Path, me: &Holder) -> Result<(), LockInfo> {
    let now = now_secs();
    let current = || match read(project) {
        Some(info) if info.session != me.session => Err(info),
        Some(info) => Ok(info.opened_at),
        None => Ok(now),
    };
    current()?;
    let body = serde_json::to_vec_pretty(&me.info(current()?, now)).unwrap_or_default();
    if let Err(e) = write_atomic(&lock_path(project), &body) {
        tracing::warn!("project lock: could not refresh {}: {e}", project.display());
    }
    Ok(())
}

/// Give the lock up, if this session holds it.
pub fn release(project: &Path, me: &Holder) {
    if read(project).is_some_and(|info| info.session == me.session) {
        let _ = std::fs::remove_file(lock_path(project));
    }
}

/// Keeps a held lock alive: refreshes the heartbeat on its own thread,
/// so a slow or hung share never stalls the UI, and notices when someone
/// else takes the lock over. Dropping it gives the lock up, also from
/// that thread.
pub struct Keeper {
    project: PathBuf,
    holder: Holder,
    shared: std::sync::Arc<KeeperShared>,
}

struct KeeperShared {
    stop: std::sync::Mutex<bool>,
    wake: std::sync::Condvar,
    lost: std::sync::Mutex<Option<LockInfo>>,
}

impl Keeper {
    /// Keep `project`'s lock, which `holder` has just acquired or taken.
    pub fn start(project: PathBuf, holder: Holder) -> Self {
        let shared = std::sync::Arc::new(KeeperShared {
            stop: std::sync::Mutex::new(false),
            wake: std::sync::Condvar::new(),
            lost: std::sync::Mutex::new(None),
        });
        let (p, h, s) = (project.clone(), holder.clone(), shared.clone());
        let spawned = std::thread::Builder::new().name("project-lock".into()).spawn(move || {
            let interval = std::time::Duration::from_secs(HEARTBEAT_SECS);
            loop {
                let stop = s.stop.lock().unwrap_or_else(|e| e.into_inner());
                // Checks the flag before waiting too: a drop that came
                // before this thread first waited would otherwise sleep
                // a whole interval unheard.
                let (stop, _) = s.wake.wait_timeout_while(stop, interval, |stop| !*stop).unwrap_or_else(|e| e.into_inner());
                if *stop {
                    drop(stop);
                    release(&p, &h);
                    return;
                }
                drop(stop);
                if let Err(taker) = heartbeat(&p, &h) {
                    tracing::warn!("project lock: {} took over {}", taker.who(), p.display());
                    *s.lost.lock().unwrap_or_else(|e| e.into_inner()) = Some(taker);
                    return;
                }
            }
        });
        if let Err(e) = spawned {
            tracing::warn!("project lock: could not start the heartbeat: {e}");
        }
        Self { project, holder, shared }
    }

    pub fn project(&self) -> &Path {
        &self.project
    }

    /// Who took the lock, once the heartbeat has found it taken.
    pub fn lost(&self) -> Option<LockInfo> {
        self.shared.lost.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Check now rather than at the next beat: `Err` with the new holder
    /// if the lock is no longer this session's. For just before a save.
    pub fn check_now(&self) -> Result<(), LockInfo> {
        if let Some(taker) = self.lost() {
            return Err(taker);
        }
        match read(&self.project) {
            Some(info) if info.session != self.holder.session => Err(info),
            _ => Ok(()),
        }
    }

    /// Give the lock up on this thread, and stop the heartbeat. For the
    /// exit path, where the process ends before a thread would get to it.
    pub fn release_now(self) {
        release(&self.project, &self.holder);
    }
}

impl Drop for Keeper {
    fn drop(&mut self) {
        *self.shared.stop.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.shared.wake.notify_all();
    }
}

/// What a project just opened asks of its lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ask {
    /// Take it if it is free; a dead holder's is taken over.
    Acquire,
    /// Take it whoever holds it (the user chose to), recording whom from.
    TakeOver(Option<LockInfo>),
}

/// What came of an [`Ask`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The lock is ours: keep it alive with a [`Keeper`].
    Held,
    /// Someone has it open: the project is read-only here.
    HeldElsewhere(LockInfo),
    /// It was to be taken over and could not be written: read-only.
    TakeOverFailed(String),
}

impl Outcome {
    fn is_ours(&self) -> bool {
        matches!(self, Outcome::Held)
    }
}

/// `ask` of `project`'s lock, done here: the files' writes and their sync.
pub fn resolve(project: &Path, me: &Holder, ask: Ask) -> Outcome {
    let take = |from: Option<LockInfo>| match take_over(project, me, from.as_ref()) {
        Ok(()) => Outcome::Held,
        Err(e) => {
            tracing::warn!("project lock: could not take over {}: {e}", project.display());
            Outcome::TakeOverFailed(e.to_string())
        }
    };
    match ask {
        Ask::TakeOver(from) => take(from),
        Ask::Acquire => match acquire(project, me).map_err(|held| *held) {
            Ok(()) => Outcome::Held,
            Err((info, Liveness::Dead)) => take(Some(info)),
            Err((info, _)) => Outcome::HeldElsewhere(info),
        },
    }
}

/// A lock being taken on a thread of its own. Taking it writes and syncs
/// a file in the project folder, and a sync waits for the filesystem: with
/// other files being written it took seconds, and done by the window the
/// window froze on opening a project. [`Pending::poll`] says what came of
/// it once it is known; dropped before then, a lock it took is given back.
pub struct Pending {
    project: PathBuf,
    holder: Holder,
    answer: std::sync::mpsc::Receiver<Outcome>,
    given_up: std::sync::Arc<std::sync::atomic::AtomicBool>,
    known: bool,
    /// This take's place among the takes of its project in this process.
    take: u64,
}

/// The latest take of each project started in this process. A take given
/// up gives back the lock it took only while it is still its project's
/// latest: the project opened again since, the newer take holds the same
/// lock, this session's, and giving it back would leave the project open
/// with no lock until the next heartbeat.
static LATEST_TAKE: std::sync::Mutex<Vec<(PathBuf, u64)>> = std::sync::Mutex::new(Vec::new());

fn latest_take(project: &Path) -> u64 {
    LATEST_TAKE.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(p, _)| p == project).map_or(0, |(_, n)| *n)
}

fn next_take(project: &Path) -> u64 {
    let mut takes = LATEST_TAKE.lock().unwrap_or_else(|e| e.into_inner());
    match takes.iter_mut().find(|(p, _)| p == project) {
        Some((_, n)) => {
            *n += 1;
            *n
        }
        None => {
            takes.push((project.to_path_buf(), 1));
            1
        }
    }
}

/// Give back a lock a take given up took, unless a newer take of the
/// project has started since.
fn give_back(project: &Path, holder: &Holder, take: u64) {
    if latest_take(project) == take {
        release(project, holder);
    }
}

impl Pending {
    pub fn start(project: PathBuf, holder: Holder, ask: Ask) -> Self {
        let (p, h) = (project.clone(), holder.clone());
        Self::start_with(project, holder, move || resolve(&p, &h, ask))
    }

    /// [`Self::start`] with the work given, for tests.
    #[doc(hidden)]
    pub fn start_with(project: PathBuf, holder: Holder, work: impl FnOnce() -> Outcome + Send + 'static) -> Self {
        let (tx, answer) = std::sync::mpsc::channel();
        let given_up = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let take = next_take(&project);
        let (p, h, gone) = (project.clone(), holder.clone(), given_up.clone());
        let run = move || {
            let outcome = work();
            let ours = outcome.is_ours();
            // Nobody waits for it any more: what it took goes back.
            let unwanted = gone.load(std::sync::atomic::Ordering::SeqCst) || tx.send(outcome).is_err();
            if unwanted && ours {
                give_back(&p, &h, take);
            }
        };
        if let Err(e) = std::thread::Builder::new().name("project-lock-take".into()).spawn(run) {
            // No thread: the lock is not taken, as when it cannot be
            // written, and the project still opens.
            tracing::warn!("project lock: could not start taking it: {e}");
        }
        Self { project, holder, answer, given_up, known: false, take }
    }

    pub fn project(&self) -> &Path {
        &self.project
    }

    /// What came of it, once: `None` while it is still being taken, and
    /// after it was said. A thread that ended without saying counts as a
    /// lock that could not be written: held by nobody.
    pub fn poll(&mut self) -> Option<Option<Outcome>> {
        if self.known {
            return None;
        }
        match self.answer.try_recv() {
            Ok(outcome) => {
                self.known = true;
                Some(Some(outcome))
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.known = true;
                Some(None)
            }
        }
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if self.known {
            return;
        }
        self.given_up.store(true, std::sync::atomic::Ordering::SeqCst);
        // Said just before it was given up: given back here.
        if let Ok(outcome) = self.answer.try_recv() {
            if outcome.is_ours() {
                give_back(&self.project, &self.holder, self.take);
            }
        }
    }
}

fn write_atomic(path: &Path, body: &[u8]) -> std::io::Result<()> {
    crate::path_util::atomic_write(path, body)
}

fn append(path: &Path, line: &str) {
    use std::io::Write;
    let r = std::fs::OpenOptions::new().create(true).append(true).open(path).and_then(|mut f| f.write_all(line.as_bytes()));
    if let Err(e) = r {
        tracing::warn!("project lock: could not record the takeover in {}: {e}", path.display());
    }
}

fn unix_to_iso(secs: u64) -> String {
    let days = secs / 86_400;
    let rem = secs % 86_400;
    // Civil from days, Howard Hinnant's algorithm.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// "12 seconds", "40 minutes", "3 hours", "2 days".
pub fn describe_secs(secs: u64) -> String {
    let (n, unit) = match secs {
        0..=89 => (secs, "second"),
        90..=5399 => ((secs + 30) / 60, "minute"),
        5400..=172_799 => ((secs + 1800) / 3600, "hour"),
        _ => ((secs + 43_200) / 86_400, "day"),
    };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {

    /// A lock file past 64 KiB is not read: no lock, not a huge read.
    #[test]
    fn an_outsized_lock_file_is_not_read() {
        let dir = std::env::temp_dir().join(format!("klipp-lock-big-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(lock_path(&dir), vec![b' '; 100 * 1024]).unwrap();
        assert!(read(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    use super::*;

    fn project(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("klipp-lock-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn other(host: &str, pid: u32) -> Holder {
        Holder { user: "anna".into(), host: host.into(), machine: format!("{host}-machine-id"), pid, session: format!("anna-{pid}") }
    }

    /// Taking the lock waits for the filesystem, and the one asking does
    /// not: it goes on at once, and hears what came of it after.
    #[test]
    fn a_slow_lock_is_taken_without_waiting_for_it() {
        let dir = project("slow");
        let me = Holder::current();
        let (p, h) = (dir.clone(), me.clone());
        let asked = std::time::Instant::now();
        let mut pending = Pending::start_with(dir.clone(), me.clone(), move || {
            std::thread::sleep(std::time::Duration::from_millis(600));
            resolve(&p, &h, Ask::Acquire)
        });
        assert!(asked.elapsed() < std::time::Duration::from_millis(200), "starting it waited {:?}", asked.elapsed());
        assert_eq!(pending.poll(), None, "not known yet");
        let t0 = std::time::Instant::now();
        let outcome = loop {
            if let Some(o) = pending.poll() {
                break o;
            }
            assert!(t0.elapsed() < std::time::Duration::from_secs(10), "never resolved");
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert_eq!(outcome, Some(Outcome::Held));
        assert_eq!(pending.poll(), None, "said once");
        assert!(matches!(inspect(&dir, &me), LockState::Ours));
    }

    /// A lock still being taken when the project is closed, or another
    /// opened, is given back once taken: nobody would keep it alive, and
    /// it would read as held for minutes.
    #[test]
    fn a_lock_given_up_while_being_taken_is_given_back() {
        let dir = project("given-up");
        let me = Holder::current();
        let (p, h) = (dir.clone(), me.clone());
        let pending = Pending::start_with(dir.clone(), me.clone(), move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            resolve(&p, &h, Ask::Acquire)
        });
        drop(pending);
        let t0 = std::time::Instant::now();
        while t0.elapsed() < std::time::Duration::from_secs(5) && !matches!(inspect(&dir, &me), LockState::Ours) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // Taken, then given back.
        let t1 = std::time::Instant::now();
        while t1.elapsed() < std::time::Duration::from_secs(5) && !matches!(inspect(&dir, &me), LockState::Free) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(matches!(inspect(&dir, &me), LockState::Free), "the lock was left behind");
    }

    /// The project opened, closed and opened again before the first take
    /// answered: the first, given up, does not give back the lock the
    /// second holds, which is the same session's.
    #[test]
    fn a_take_given_up_leaves_a_newer_takes_lock() {
        let dir = project("retaken");
        let me = Holder::current();
        let (p, h) = (dir.clone(), me.clone());
        let slow = Pending::start_with(dir.clone(), me.clone(), move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            resolve(&p, &h, Ask::Acquire)
        });
        drop(slow);
        let mut again = Pending::start(dir.clone(), me.clone(), Ask::Acquire);
        let t0 = std::time::Instant::now();
        while again.poll().is_none() && t0.elapsed() < std::time::Duration::from_secs(5) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // Past the first take's answer, and its giving back.
        std::thread::sleep(std::time::Duration::from_millis(600));
        assert!(matches!(inspect(&dir, &me), LockState::Ours), "the newer take's lock was given back");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Off its thread the lock is judged as before: someone else's live
    /// one makes this session read-only and says who.
    /// A dead holder's lock on this machine is taken over, as on the
    /// window's thread before.
    #[test]
    fn a_dead_holders_lock_is_taken_over() {
        if !cfg!(target_os = "linux") {
            return;
        }
        let dir = project("dead-taken");
        let me = Holder::current();
        let ghost = Holder { host: me.host.clone(), machine: me.machine.clone(), ..other("", u32::MAX - 1) };
        acquire(&dir, &ghost).unwrap();
        assert_eq!(resolve(&dir, &me, Ask::Acquire), Outcome::Held);
        assert!(matches!(inspect(&dir, &me), LockState::Ours));
        assert!(std::fs::read_to_string(dir.join(HISTORY_FILE)).unwrap_or_default().contains("took the lock from"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn someone_elses_lock_is_still_said() {
        let dir = project("elsewhere");
        let anna = other("studio", 4242);
        acquire(&dir, &anna).expect("anna takes it");
        assert!(matches!(resolve(&dir, &Holder::current(), Ask::Acquire), Outcome::HeldElsewhere(info) if info.user == "anna"));
    }

    #[test]
    fn a_second_editor_is_told_who_has_it() {
        let p = project("second");
        let anna = other("laptop", 4242);
        let me = Holder::current();
        acquire(&p, &anna).unwrap();
        let (info, l) = *acquire(&p, &me).unwrap_err();
        assert_eq!(info.who(), "anna@laptop");
        assert_eq!(l, Liveness::Active);
        assert_eq!(inspect(&p, &anna), LockState::Ours);
        std::fs::remove_dir_all(&p).ok();
    }

    #[test]
    fn a_crashed_session_goes_stale_and_can_be_taken_with_a_trace() {
        let p = project("stale");
        let anna = other("laptop", 4242);
        acquire(&p, &anna).unwrap();
        // Forty minutes without a heartbeat.
        let mut info = read(&p).unwrap();
        info.heartbeat_at -= 40 * 60;
        std::fs::write(p.join(LOCK_FILE), serde_json::to_vec(&info).unwrap()).unwrap();

        let me = Holder::current();
        let (held, l) = *acquire(&p, &me).unwrap_err();
        assert_eq!(l, Liveness::Stale);
        take_over(&p, &me, Some(&held)).unwrap();
        assert_eq!(inspect(&p, &me), LockState::Ours);
        let log = std::fs::read_to_string(p.join(HISTORY_FILE)).unwrap();
        assert!(log.contains("took the lock from anna@laptop"), "{log}");
        assert!(log.contains("40 minutes ago"), "{log}");

        // The old holder finds out on its next beat, and stops.
        assert_eq!(heartbeat(&p, &anna).unwrap_err().session, me.session);
        std::fs::remove_dir_all(&p).ok();
    }

    #[test]
    fn a_dead_process_on_this_machine_is_recognised() {
        let p = project("dead");
        let me = Holder::current();
        // No process has pid u32::MAX - 1.
        let ghost = Holder { host: me.host.clone(), machine: me.machine.clone(), ..other("", u32::MAX - 1) };
        acquire(&p, &ghost).unwrap();
        if cfg!(target_os = "linux") {
            assert_eq!(acquire(&p, &me).unwrap_err().1, Liveness::Dead);
        }
        std::fs::remove_dir_all(&p).ok();
    }

    #[test]
    fn release_leaves_someone_elses_lock_alone() {
        let p = project("release");
        let anna = other("laptop", 4242);
        let me = Holder::current();
        acquire(&p, &anna).unwrap();
        release(&p, &me);
        assert!(read(&p).is_some(), "not ours to remove");
        release(&p, &anna);
        assert!(read(&p).is_none());
        acquire(&p, &me).unwrap();
        std::fs::remove_dir_all(&p).ok();
    }

    #[test]
    fn an_unreadable_lock_counts_as_held_while_fresh_and_is_replaced_once_old() {
        let p = project("garbage");
        std::fs::write(p.join(LOCK_FILE), b"").unwrap();
        let me = Holder::current();
        // Fresh: somebody's lock mid-write, not garbage to replace.
        assert!(acquire(&p, &me).is_err());
        let old = SystemTime::now() - std::time::Duration::from_secs(STALE_AFTER_SECS + 60);
        std::fs::File::options().write(true).open(p.join(LOCK_FILE)).unwrap().set_modified(old).unwrap();
        acquire(&p, &me).unwrap();
        assert_eq!(inspect(&p, &me), LockState::Ours);
        std::fs::remove_dir_all(&p).ok();
    }

    #[test]
    fn of_many_simultaneous_openers_exactly_one_wins() {
        let p = project("race");
        // All sixteen released at once, so the attempts really overlap.
        let start = std::sync::Arc::new(std::sync::Barrier::new(16));
        let winners: usize = (0..16)
            .map(|i| {
                let (p, start) = (p.clone(), start.clone());
                std::thread::spawn(move || {
                    start.wait();
                    usize::from(acquire(&p, &other("racer", 1000 + i)).is_ok())
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|t| t.join().unwrap())
            .sum();
        assert_eq!(winners, 1);
        let leftovers: Vec<_> = std::fs::read_dir(&p).unwrap().flatten().filter(|e| e.file_name() != LOCK_FILE).collect();
        assert!(leftovers.is_empty(), "temporary files left behind: {leftovers:?}");
        std::fs::remove_dir_all(&p).ok();
    }

    #[test]
    fn a_namesake_machine_is_not_judged_by_this_ones_pids() {
        let p = project("namesake");
        let me = Holder::current();
        // Another machine with the same hostname, and a pid not running here.
        let twin = Holder { host: me.host.clone(), machine: "some-other-machine".into(), ..other("", u32::MAX - 1) };
        acquire(&p, &twin).unwrap();
        assert_eq!(acquire(&p, &me).unwrap_err().1, Liveness::Active);
        std::fs::remove_dir_all(&p).ok();
    }

    #[cfg(unix)]
    #[test]
    fn a_takeover_that_cannot_write_says_so() {
        use std::os::unix::fs::PermissionsExt;
        let p = project("unwritable");
        let anna = other("laptop", 4242);
        acquire(&p, &anna).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o555)).unwrap();
        let taken = take_over(&p, &Holder::current(), read(&p).as_ref());
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Root writes anywhere; the claim is about an ordinary user.
        let root = std::env::var("USER").is_ok_and(|u| u == "root");
        if root {
            eprintln!("skipped: running with permission to write the folder anyway");
        } else {
            assert!(taken.is_err());
            assert_eq!(read(&p).unwrap().who(), "anna@laptop");
        }
        std::fs::remove_dir_all(&p).ok();
    }

    #[test]
    fn a_lock_from_another_build_still_reads() {
        let p = project("compat");
        std::fs::write(p.join(LOCK_FILE), br#"{"user":"anna","host":"laptop","pid":7,"session":"s","heartbeat_at":1}"#).unwrap();
        assert_eq!(read(&p).unwrap().who(), "anna@laptop");
        std::fs::remove_dir_all(&p).ok();
    }

    #[test]
    fn durations_read_as_a_person_would_say_them() {
        assert_eq!(describe_secs(12), "12 seconds");
        assert_eq!(describe_secs(60), "60 seconds");
        assert_eq!(describe_secs(40 * 60), "40 minutes");
        assert_eq!(describe_secs(3 * 3600), "3 hours");
        assert_eq!(describe_secs(86_400 * 2), "2 days");
        assert_eq!(unix_to_iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(unix_to_iso(1_790_000_000), "2026-09-21T14:13:20Z");
    }

    #[test]
    fn a_keeper_notices_a_takeover_and_releases_on_drop() {
        let p = project("keeper");
        let me = Holder::current();
        acquire(&p, &me).unwrap();
        let keeper = Keeper::start(p.clone(), me.clone());
        assert!(keeper.check_now().is_ok());
        let anna = other("laptop", 4242);
        take_over(&p, &anna, read(&p).as_ref()).unwrap();
        assert_eq!(keeper.check_now().unwrap_err().who(), "anna@laptop");
        drop(keeper);
        // Dropping must not remove a lock that is no longer ours.
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(read(&p).unwrap().who(), "anna@laptop");

        release(&p, &anna);
        acquire(&p, &me).unwrap();
        drop(Keeper::start(p.clone(), me.clone()));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while read(&p).is_some() {
            assert!(std::time::Instant::now() < deadline, "the keeper did not release on drop");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        std::fs::remove_dir_all(&p).ok();
    }
}
