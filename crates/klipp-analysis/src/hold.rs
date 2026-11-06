//! Analysis gives way to the person at the editor. While they play,
//! scrub or have just touched the controls, no analyser starts a step,
//! and an ffmpeg already decoding is stopped where it stands until they
//! are done: the viewer's own hardware decode and the analysers' frame
//! grabs share one decoder, and the picture on screen comes first. The
//! host says what "busy" means through [`install`]; with nothing
//! installed (tests, tools) nothing ever waits.

use std::io::Read;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

type Check = Arc<dyn Fn() -> bool + Send + Sync>;

fn check() -> &'static Mutex<Option<Check>> {
    static C: OnceLock<Mutex<Option<Check>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}

/// Say what "the editor is busy" is: `true` while analysis should wait.
pub fn install(f: Check) {
    *check().lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
}

/// Whether analysis should wait right now.
pub fn held() -> bool {
    // Called outside the lock: a check that panics leaves it usable.
    let f = check().lock().unwrap_or_else(|e| e.into_inner()).clone();
    f.is_some_and(|f| f())
}

const POLL: Duration = Duration::from_millis(100);

/// Wait until the editor is idle. Said in the log once per wait.
pub fn wait() {
    if !held() {
        return;
    }
    let t = Instant::now();
    tracing::info!("analysis waits: the editor is busy");
    while held() {
        std::thread::sleep(POLL);
    }
    tracing::info!("analysis resumes after {:.1} s", t.elapsed().as_secs_f64());
}

/// Run `cmd` to the end with its output captured, as `Command::output`
/// does, except that the child is stopped (SIGSTOP) while the editor is
/// busy and continued when it is idle again. The child dies with the
/// thread that started it, so a stopped one is never left behind.
pub fn output(cmd: &mut Command) -> std::io::Result<Output> {
    output_within(cmd, None)?.ok_or_else(|| std::io::Error::other("cut short with no deadline"))
}

/// [`output`], killed when it has run `limit` (not counting the time it
/// was stopped for the editor): `None` then. Hardware decode in ffmpeg
/// can deadlock on a seek into some files and wait forever (#519).
pub fn output_within(cmd: &mut Command, limit: Option<Duration>) -> std::io::Result<Option<Output>> {
    wait();
    let (mut child, guarded) = spawn_guarded(cmd)?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let mut stopped = false;
    let mut stopped_at = Instant::now();
    let mut stopped_for = Duration::ZERO;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        let ran = started.elapsed().saturating_sub(stopped_for + if stopped { stopped_at.elapsed() } else { Duration::ZERO });
        if limit.is_some_and(|l| ran > l) {
            let _ = child.kill();
            let _ = child.wait();
            tracing::warn!("{:?} killed after {:.1} s", cmd.get_program(), ran.as_secs_f64());
            return Ok(None);
        }
        // A child that would outlive this process is never stopped: one
        // left stopped would stay so for good.
        let busy = guarded && held();
        if busy != stopped {
            if busy {
                stopped_at = Instant::now();
            } else {
                stopped_for += stopped_at.elapsed();
            }
            if signal(child.id(), busy) {
                stopped = busy;
            }
        }
        std::thread::sleep(POLL);
    };
    if stopped {
        stopped_for += stopped_at.elapsed();
    }
    if stopped_for > Duration::ZERO {
        tracing::info!(
            "analysis decode stopped {:.1} s of {:.1} s while the editor was busy",
            stopped_for.as_secs_f64(),
            started.elapsed().as_secs_f64()
        );
    }
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    Ok(Some(Output { status, stdout, stderr }))
}

/// `cmd`'s program, arguments, environment changes and directory in a
/// new command, behind `prefix` (a program and its arguments) when given.
fn copy_of(cmd: &Command, prefix: &[&std::ffi::OsStr]) -> Command {
    let mut c = match prefix.split_first() {
        Some((program, args)) => {
            let mut c = Command::new(program);
            c.args(args).arg(cmd.get_program());
            c
        }
        None => Command::new(cmd.get_program()),
    };
    c.args(cmd.get_args());
    for (key, value) in cmd.get_envs() {
        match value {
            Some(v) => c.env(key, v),
            None => c.env_remove(key),
        };
    }
    if let Some(dir) = cmd.get_current_dir() {
        c.current_dir(dir);
    }
    c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    c
}

/// util-linux's `setpriv`, when it can give a child a death signal:
/// looked for once. Started through it, the child gets SIGKILL when this
/// process dies, the same as `PR_SET_PDEATHSIG` set in a `pre_exec`,
/// without this process forking.
#[cfg(target_os = "linux")]
fn setpriv() -> bool {
    static FOUND: OnceLock<bool> = OnceLock::new();
    *FOUND.get_or_init(|| {
        Command::new("setpriv")
            .args(["--pdeathsig", "KILL", "--", "true"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

/// Start `cmd` so that it dies with this process, and say whether it
/// will (`true`). A `pre_exec` makes the standard library fork, and a
/// fork copies the account of every writable mapping: one region past
/// RAM and swap together (the GPU driver reserves such regions in a long
/// session, near empty) and the kernel refuses it, "Cannot allocate
/// memory", while a spawn without one goes through. So through
/// `setpriv` where it is, the `pre_exec` where it isn't, and a plain
/// spawn, said in the log, where that is refused.
fn spawn_guarded(cmd: &Command) -> std::io::Result<(std::process::Child, bool)> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        if setpriv() {
            let prefix = ["setpriv", "--pdeathsig", "KILL", "--"].map(std::ffi::OsStr::new);
            return copy_of(cmd, &prefix).spawn().map(|c| (c, true));
        }
        let mut forked = copy_of(cmd, &[]);
        // SAFETY: prctl is async-signal-safe and touches nothing shared.
        unsafe {
            forked.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
        match forked.spawn() {
            Ok(c) => Ok((c, true)),
            Err(e) if e.raw_os_error() == Some(libc::ENOMEM) => {
                tracing::warn!("{:?}: the fork was refused ({e}; {}); started without a death signal, and not stopped while the editor is busy", cmd.get_program(), address_space());
                copy_of(cmd, &[]).spawn().map(|c| (c, false))
            }
            Err(e) => Err(e),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        copy_of(cmd, &[]).spawn().map(|c| (c, true))
    }
}

/// This process's address space, its largest accounted region, and the
/// memory free: why a fork was refused.
#[cfg(target_os = "linux")]
fn address_space() -> String {
    let size = std::fs::read_to_string("/proc/self/status").ok().and_then(|s| s.lines().find(|l| l.starts_with("VmSize")).map(|l| l.split_whitespace().nth(1).unwrap_or("?").to_string())).unwrap_or_default();
    let mut largest = 0u64;
    let mut region = 0u64;
    if let Ok(maps) = std::fs::read_to_string("/proc/self/smaps") {
        for line in maps.lines() {
            if let Some((range, _)) = line.split_once(' ').filter(|(r, _)| r.contains('-') && !r.ends_with(':')) {
                let mut ends = range.split('-').map(|h| u64::from_str_radix(h, 16).unwrap_or(0));
                region = match (ends.next(), ends.next()) {
                    (Some(a), Some(b)) => b.saturating_sub(a),
                    _ => 0,
                };
            } else if line.starts_with("VmFlags:") && line.split_whitespace().any(|f| f == "ac") {
                largest = largest.max(region);
            }
        }
    }
    let available = std::fs::read_to_string("/proc/meminfo").ok().and_then(|s| s.lines().find(|l| l.starts_with("MemAvailable")).map(|l| l.split_whitespace().nth(1).unwrap_or("?").to_string())).unwrap_or_default();
    format!("address space {size} kB, largest accounted region {} kB, {available} kB available", largest / 1024)
}

fn drain<R: Read + Send + 'static>(r: Option<R>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut v = Vec::new();
        if let Some(mut r) = r {
            let _ = r.read_to_end(&mut v);
        }
        v
    })
}

/// Stop (`true`) or continue the child. `false` where that cannot be
/// done, and the child simply runs on.
#[cfg(unix)]
fn signal(pid: u32, stop: bool) -> bool {
    let sig = if stop { libc::SIGSTOP } else { libc::SIGCONT };
    // SAFETY: kill with a pid this process spawned and still waits on.
    unsafe { libc::kill(pid as libc::pid_t, sig) == 0 }
}

#[cfg(not(unix))]
fn signal(_pid: u32, _stop: bool) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    // One test: the hook is process-wide, so two would race each other.
    #[test]
    fn a_stopped_child_finishes_once_released() {
        assert!(!held(), "nothing installed: nothing waits");
        wait();
        let busy = Arc::new(AtomicBool::new(true));
        let b = Arc::clone(&busy);
        install(Arc::new(move || b.load(Ordering::Relaxed)));
        let t = std::thread::spawn(|| {
            let mut cmd = Command::new("sh");
            cmd.args(["-c", "echo out; echo err >&2"]);
            output(&mut cmd)
        });
        std::thread::sleep(Duration::from_millis(250));
        assert!(!t.is_finished(), "held before the spawn");
        busy.store(false, Ordering::Relaxed);
        let out = t.join().unwrap().unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "out");
        assert_eq!(String::from_utf8_lossy(&out.stderr).trim(), "err");
        install(Arc::new(|| false));
    }

    /// This test binary again, running only `test`, with `role` in the
    /// environment: a process of its own for what would disturb the
    /// other tests (a region too big to fork, a SIGKILL).
    #[cfg(target_os = "linux")]
    fn again(test: &str, role: &str) -> Command {
        let mut c = Command::new(std::env::current_exe().unwrap());
        c.args(["--exact", test, "--nocapture", "--test-threads=1"]).env("KLIPP_HOLD_ROLE", role);
        c
    }

    /// One private writable region bigger than RAM and swap together,
    /// made of mappings the kernel accepts one at a time and merges, as
    /// the GPU driver's reservations grow in a long session. `None`
    /// where overcommit is not the kernel's default heuristic.
    #[cfg(target_os = "linux")]
    fn too_big_to_fork() -> Option<usize> {
        if std::fs::read_to_string("/proc/sys/vm/overcommit_memory").ok()?.trim() != "0" {
            return None;
        }
        // SAFETY: sysinfo fills the struct it is given.
        let total = unsafe {
            let mut s: libc::sysinfo = std::mem::zeroed();
            (libc::sysinfo(&mut s) == 0).then(|| (s.totalram + s.totalswap) as usize * s.mem_unit as usize)?
        };
        let each = (total / 2) & !0xfff;
        // SAFETY: a fresh reservation, then fixed mappings inside it;
        // nothing is touched, so no memory is used.
        unsafe {
            let base = libc::mmap(std::ptr::null_mut(), each * 3, libc::PROT_NONE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE, -1, 0);
            assert_ne!(base, libc::MAP_FAILED);
            for i in 0..3 {
                let p = libc::mmap((base as usize + i * each) as *mut _, each, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED, -1, 0);
                assert_ne!(p, libc::MAP_FAILED, "mapping {i} of {each} bytes");
            }
        }
        Some(each * 3)
    }

    /// In a process with one region past RAM and swap, as Klipp reaches
    /// after a long session, analysis's ffmpeg still starts. A `pre_exec`
    /// spawn forked, and the kernel refused it: shots, motion and the
    /// beat sheet failed with "Cannot allocate memory" until a restart.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_child_starts_in_a_process_too_big_to_fork() {
        const NAME: &str = "hold::tests::a_child_starts_in_a_process_too_big_to_fork";
        if std::env::var("KLIPP_HOLD_ROLE").as_deref() == Ok("big") {
            let Some(size) = too_big_to_fork() else {
                println!("ROLE skipped: overcommit is not 0");
                return;
            };
            use std::os::unix::process::CommandExt;
            let mut forked = Command::new("sh");
            forked.args(["-c", "true"]);
            // SAFETY: an empty pre_exec, only to make the spawn fork.
            unsafe { forked.pre_exec(|| Ok(())); }
            let refused = forked.status().is_err_and(|e| e.raw_os_error() == Some(libc::ENOMEM));
            assert!(refused, "the setup: a fork with a {} GB region is refused", size >> 30);
            let mut cmd = Command::new("sh");
            cmd.args(["-c", "echo started"]);
            let out = output(&mut cmd).expect("the child starts");
            assert!(out.status.success());
            assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "started");
            println!("ROLE ok with a {} GB region", size >> 30);
            return;
        }
        // With setpriv, and on a system without it: a PATH holding only
        // `sh`, where the refused fork falls back to a plain spawn.
        let bare = std::env::temp_dir().join(format!("klipp-hold-path-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&bare);
        std::fs::create_dir_all(&bare).unwrap();
        let sh = ["/bin/sh", "/usr/bin/sh"].into_iter().find(|p| std::path::Path::new(p).exists()).unwrap();
        std::os::unix::fs::symlink(sh, bare.join("sh")).unwrap();
        for (how, path) in [("with setpriv", None), ("without setpriv", Some(&bare))] {
            let mut run = again(NAME, "big");
            if let Some(p) = path {
                run.env("PATH", p);
            }
            let out = run.output().unwrap();
            let text = String::from_utf8_lossy(&out.stdout);
            assert!(out.status.success() && text.contains("ROLE "), "{how}: {text}\n{}", String::from_utf8_lossy(&out.stderr));
            eprintln!("{how}: {}", text.lines().find(|l| l.contains("ROLE ")).unwrap_or(""));
        }
        let _ = std::fs::remove_dir_all(&bare);
    }

    /// A child analysis started dies with this process, stopped for the
    /// editor or not: a stopped ffmpeg left behind would stay stopped.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_child_dies_with_its_parent() {
        const NAME: &str = "hold::tests::a_child_dies_with_its_parent";
        if let Ok(role) = std::env::var("KLIPP_HOLD_ROLE") {
            let pid_file = std::path::PathBuf::from(role);
            let busy = Arc::new(AtomicBool::new(false));
            let b = Arc::clone(&busy);
            install(Arc::new(move || b.load(Ordering::Relaxed)));
            std::thread::spawn(move || {
                let mut cmd = Command::new("sh");
                cmd.arg("-c").arg(format!("echo $$ > {}; exec sleep 30", pid_file.display()));
                let _ = output(&mut cmd);
            });
            // Stopped for the editor, then this process waits to be killed.
            std::thread::sleep(Duration::from_millis(500));
            busy.store(true, Ordering::Relaxed);
            std::thread::sleep(Duration::from_secs(60));
            return;
        }
        let pid_file = std::env::temp_dir().join(format!("klipp-hold-death-{}", std::process::id()));
        let _ = std::fs::remove_file(&pid_file);
        let mut parent = again(NAME, pid_file.to_str().unwrap()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        let start = Instant::now();
        let pid = loop {
            if let Some(pid) = std::fs::read_to_string(&pid_file).ok().and_then(|s| s.trim().parse::<i32>().ok()) {
                break pid;
            }
            assert!(start.elapsed() < Duration::from_secs(20), "the child wrote no pid");
            std::thread::sleep(Duration::from_millis(50));
        };
        std::thread::sleep(Duration::from_secs(1));
        let state = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        assert!(state.contains(") T "), "the child is stopped for the editor: {state}");
        parent.kill().unwrap();
        parent.wait().unwrap();
        let start = Instant::now();
        // SAFETY: signal 0 only asks whether the pid is there.
        while unsafe { libc::kill(pid, 0) } == 0 {
            let zombie = std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| s.contains(") Z "));
            if zombie {
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(5), "the stopped child {pid} outlived its parent");
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = std::fs::remove_file(&pid_file);
    }
}
