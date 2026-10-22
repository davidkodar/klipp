//! Native code runs in a worker process, not in the editor (#640, #689).
//!
//! whisper.cpp and ONNX Runtime stop the whole process on an assertion,
//! an abort or an out-of-memory of their own, which nothing in Rust can
//! catch, and analysis runs on import without anyone asking: a crash
//! there took the editor, and the unsaved work in it, down. In a worker
//! only the worker goes, and that one analysis of that one file fails
//! with the reason.
//!
//! A worker is the app's own executable, started with a flag and a JSON
//! job: it does the job, writes a `Reply` on its standard output and
//! exits. This module is the one way to start one (`run`), hold it to
//! a deadline, and read its reply or why it died; the worker's side is
//! [`main`]. Speech recognition sends its sound on the worker's
//! standard input; an analyser run whole (`run_analyser`) is told on
//! it instead when the editor is busy, so its frame grabs give way to
//! playback as they do in the editor ([`crate::hold`]).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// The program workers run in: set by the app to its own executable.
/// Unset — a test, a tool — everything runs in this process.
static EXE: OnceLock<PathBuf> = OnceLock::new();

/// Run native code in workers of `exe` from now on.
pub fn use_workers(exe: PathBuf) {
    let _ = EXE.set(exe);
}

pub(crate) fn exe() -> Option<&'static Path> {
    EXE.get().map(PathBuf::as_path)
}

/// The argument that makes the executable an analysis worker: it runs
/// one analyser on one file.
pub const ANALYSIS_FLAG: &str = "--analysis-worker";

/// A worker's answer, on its standard output.
#[derive(Serialize, Deserialize)]
pub(crate) enum Reply<T> {
    Done(T),
    Failed(String),
}

/// What a worker's standard input carries.
pub(crate) enum Input {
    /// These bytes, then the end.
    Data(Vec<u8>),
    /// Whether the editor is busy, a byte each time that changes: `1`
    /// busy, `0` idle. Kept open while the worker runs.
    Hold,
}

/// One job for a worker.
pub(crate) struct Job<'a> {
    pub flag: &'a str,
    pub spec: String,
    pub input: Input,
    /// How long it may run, not counting the time the editor held it.
    pub deadline: Duration,
    /// What it does, for the messages: "transcription".
    pub what: &'a str,
}

/// The executable to start: `exe`, or, when the app was upgraded while
/// running and its own file was replaced, the new one at the same path
/// (Linux names the old one "<path> (deleted)").
pub(crate) fn worker_exe(exe: &Path) -> PathBuf {
    if exe.exists() {
        return exe.to_path_buf();
    }
    let s = exe.to_string_lossy();
    match s.strip_suffix(" (deleted)") {
        Some(live) => PathBuf::from(live),
        None => exe.to_path_buf(),
    }
}

/// Run `job` in a worker of `exe`: its reply — a reported failure, a
/// worker that hung past its deadline and an answer that could not be
/// read among them — or, as the error, why the worker died.
pub(crate) fn run<T: DeserializeOwned>(exe: &Path, job: Job) -> Result<Result<T, String>, String> {
    let exe = worker_exe(exe);
    let started = Instant::now();
    let spawn = || {
        std::process::Command::new(&exe)
            .arg(job.flag)
            .arg(&job.spec)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
    };
    // An executable still open for writing somewhere cannot be started
    // (ETXTBSY): one just written, while another thread's fork holds the
    // writer's descriptor until its exec closes it, or a binary being
    // replaced by a build. Either lets go within moments.
    let mut child = spawn();
    for _ in 0..50 {
        match &child {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(Duration::from_millis(20));
                child = spawn();
            }
            _ => break,
        }
    }
    let mut child = child.map_err(|e| format!("could not start {}: {e}", exe.display()))?;
    // The input written, and the answers read, each on a thread of its
    // own: a worker that fills one pipe while we wait on another would
    // otherwise wait on us as we wait on it.
    let mut input = child.stdin.take().ok_or("no input to the worker")?;
    let (writer, mut hold_pipe) = match job.input {
        Input::Data(bytes) => (Some(std::thread::spawn(move || { let _ = input.write_all(&bytes); })), None),
        Input::Hold => (None, Some(input)),
    };
    let reader = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        })
    };
    let stdout = reader(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let stderr = reader(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let mut held = false;
    let mut held_since = Instant::now();
    let mut held_for = Duration::ZERO;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("the worker: {e}"));
            }
        }
        if let Some(pipe) = hold_pipe.as_mut() {
            let busy = crate::hold::held();
            if busy != held {
                if busy {
                    held_since = Instant::now();
                } else {
                    held_for += held_since.elapsed();
                }
                held = busy;
                // A worker that has stopped reading is about to exit.
                let _ = pipe.write_all(if busy { b"1" } else { b"0" }).and_then(|_| pipe.flush());
            }
        }
        let ran = started.elapsed().saturating_sub(held_for + if held { held_since.elapsed() } else { Duration::ZERO });
        if ran > job.deadline {
            let _ = child.kill();
            let _ = child.wait();
            if let Some(w) = writer {
                let _ = w.join();
            }
            return Ok(Err(format!("{} did not finish in {} s and was stopped", job.what, job.deadline.as_secs())));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    drop(hold_pipe);
    if let Some(w) = writer {
        let _ = w.join();
    }
    let (out, err) = (stdout.join().unwrap_or_default(), stderr.join().unwrap_or_default());
    if !status.success() {
        let said = String::from_utf8_lossy(&err);
        let last = said.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
        return Err(format!("{status}{}", if last.is_empty() { String::new() } else { format!(": {last}") }));
    }
    match serde_json::from_slice::<Reply<T>>(&out) {
        Ok(Reply::Done(t)) => Ok(Ok(t)),
        Ok(Reply::Failed(why)) => Ok(Err(why)),
        // A worker of another version — the app upgraded while it ran —
        // may answer in a shape this one does not read; running it again
        // would not change that.
        Err(e) => Ok(Err(format!("the {} worker's answer could not be read ({e}); if Klipp was just updated, restart it", job.what))),
    }
}

/// Write `reply` as the worker's answer; the exit code.
pub(crate) fn answer<T: Serialize>(reply: Result<T, String>) -> i32 {
    let reply = match reply {
        Ok(t) => Reply::Done(t),
        Err(e) => Reply::Failed(e),
    };
    let text = serde_json::to_vec(&reply).unwrap_or_default();
    let mut out = std::io::stdout().lock();
    if out.write_all(&text).and_then(|_| out.flush()).is_err() {
        return 1;
    }
    0
}

/// When this process was started as a worker, do its one job and give
/// the exit code; `None` for any other start. The app asks first, before
/// anything of its own is set up.
pub fn main() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    let flag = args.next()?;
    let spec = args.next().map(|a| a.to_string_lossy().into_owned()).unwrap_or_default();
    #[cfg(feature = "transcript")]
    if flag == crate::transcript::WORKER_FLAG {
        return Some(crate::transcript::worker_main(&spec));
    }
    if flag == ANALYSIS_FLAG {
        return Some(analysis_main(&spec));
    }
    None
}

/// One analyser on one file, in a worker.
#[derive(Serialize, Deserialize)]
struct AnalysisJob {
    analyser: String,
    item: klipp_media::MediaItem,
    ffmpeg: PathBuf,
}

/// How long an analyser may take on a file of `duration_secs` before it
/// is taken for hung. The models load in seconds, and the analysers that
/// run in a worker read at most `FRAME_BUDGET` frames whatever the file's
/// length, so their work is bounded by frames as much as by time: on a
/// machine reading text off 4K frames on the CPU, a few seconds a frame,
/// a short screen recording took longer than twice its length (#710
/// review). Generous on both counts; a hang is still caught.
pub(crate) fn analysis_deadline(duration_secs: f64) -> Duration {
    const FRAME_BUDGET: f64 = 360.0;
    const SLOW_FRAME_SECS: f64 = 6.0;
    let work = (duration_secs.max(0.0) * 2.0).max(FRAME_BUDGET * SLOW_FRAME_SECS);
    Duration::from_secs_f64(work + 300.0)
}

/// `a`'s result for `item`: in a worker when `a` runs native code and the
/// app has set one up, else here.
pub(crate) fn run_analyser(a: &dyn crate::Analyser, item: &klipp_media::MediaItem, ctx: &crate::AnalysisCtx) -> Result<serde_json::Value, String> {
    match exe() {
        Some(exe) if a.native() => run_analyser_in(exe, a, item, ctx),
        _ => a.run(item, ctx),
    }
}

/// [`run_analyser`] in the worker `exe`.
pub(crate) fn run_analyser_in(exe: &Path, a: &dyn crate::Analyser, item: &klipp_media::MediaItem, ctx: &crate::AnalysisCtx) -> Result<serde_json::Value, String> {
    let job = AnalysisJob { analyser: a.id().into(), item: item.clone(), ffmpeg: ctx.ffmpeg.clone() };
    let spec = serde_json::to_string(&job).map_err(|e| e.to_string())?;
    let started = Instant::now();
    let what = format!("the {} analysis", a.id());
    let job = Job { flag: ANALYSIS_FLAG, spec, input: Input::Hold, deadline: analysis_deadline(item.duration.to_secs_f64()), what: &what };
    let mut reply = run::<serde_json::Value>(exe, job).unwrap_or_else(|died| Err(format!("{what} stopped in its worker ({died})")));
    tracing::debug!("{what} of {} in a worker: {:.2} s", item.filename, started.elapsed().as_secs_f64());
    if let Some(used) = reply.as_mut().ok().and_then(|v| v.as_object_mut()).and_then(|o| o.remove(PROVIDERS_KEY)) {
        crate::providers::record_json(&used);
    }
    reply
}

/// The editor's busy flag as the worker's parent tells it, on standard
/// input.
static PARENT_BUSY: AtomicBool = AtomicBool::new(false);

/// Follow the parent's busy flag, so this worker's frame grabs give way
/// to playback as the editor's own do; and end when the parent does,
/// its end of the pipe closing with it.
fn follow_parent() {
    crate::hold::install(std::sync::Arc::new(|| PARENT_BUSY.load(Ordering::Relaxed)));
    std::thread::spawn(|| {
        let mut stdin = std::io::stdin().lock();
        let mut byte = [0u8; 1];
        loop {
            match stdin.read(&mut byte) {
                Ok(1) => PARENT_BUSY.store(byte[0] == b'1', Ordering::Relaxed),
                Ok(_) | Err(_) => std::process::exit(1),
            }
        }
    });
}

/// An analysis worker's whole life (#689): the analyser named in `spec`
/// on its file, the result on standard output.
fn analysis_main(spec: &str) -> i32 {
    follow_parent();
    let reply = (|| -> Result<serde_json::Value, String> {
        let job: AnalysisJob = serde_json::from_str(spec).map_err(|e| format!("the job: {e}"))?;
        let a = crate::native_analysers().into_iter().find(|a| a.id() == job.analyser).ok_or_else(|| format!("no analyser {} in this build", job.analyser))?;
        a.run(&job.item, &crate::AnalysisCtx { ffmpeg: job.ffmpeg })
    })();
    // What the worker's models ran on goes back with the result, for the
    // editor to show (#2250).
    let reply = reply.map(|mut v| {
        let used = crate::providers::to_json();
        if let (Some(o), false) = (v.as_object_mut(), used.is_empty()) {
            o.insert(PROVIDERS_KEY.into(), serde_json::Value::Array(used));
        }
        v
    });
    answer(reply)
}

/// The key a worker's reply carries its models' providers under, taken
/// out before the result is kept.
const PROVIDERS_KEY: &str = "_execution_providers";

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("klipp-analysis-worker-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Native code that would answer if it ran.
    struct Native;
    impl crate::Analyser for Native {
        fn id(&self) -> &'static str {
            "native"
        }
        fn schema_version(&self) -> u32 {
            1
        }
        fn wants(&self, _: &klipp_media::MediaItem) -> bool {
            true
        }
        fn run(&self, _: &klipp_media::MediaItem, _: &crate::AnalysisCtx) -> Result<serde_json::Value, String> {
            Ok(serde_json::json!({ "ran": "here" }))
        }
        fn native(&self) -> bool {
            true
        }
    }

    fn item() -> klipp_media::MediaItem {
        klipp_media::MediaItem::synthetic_for_test(PathBuf::from("/media/a.mp4"), None)
    }

    /// #689: a worker that aborts, as ONNX Runtime does on a bad model,
    /// fails that one analysis with the reason, for every analyser that
    /// runs in one; this process, the editor, carries on, and an analyser
    /// that runs here still runs. The stand-in exits with abort's status
    /// rather than raising the signal, so the tests leave no core dumps.
    #[test]
    fn an_aborting_worker_fails_only_its_own_analysis() {
        let dir = scratch("abort");
        let dead = script(&dir, "dead.sh", "echo 'onnxruntime: terminate called after throwing an instance' >&2\nexit 134");
        let mut native = crate::native_analysers();
        native.push(Box::new(Native));
        assert!(native.iter().all(|a| a.native()));
        for a in &native {
            let err = run_analyser_in(&dead, a.as_ref(), &item(), &crate::AnalysisCtx::default()).expect_err("a dead worker is an error");
            assert!(err.contains(&format!("the {} analysis stopped in its worker", a.id())) && err.contains("134") && err.contains("terminate called"), "{err}");
        }
        // The same analyser with no worker set up runs here, as a test
        // or a tool has it.
        assert_eq!(run_analyser(&Native, &item(), &crate::AnalysisCtx::default()).unwrap()["ran"], "here");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A worker that hangs is stopped at its deadline, and says so.
    #[test]
    fn a_hung_worker_is_stopped_at_its_deadline() {
        let dir = scratch("hang");
        let hung = script(&dir, "hung.sh", "exec sleep 60");
        let started = Instant::now();
        let job = Job { flag: ANALYSIS_FLAG, spec: "{}".into(), input: Input::Hold, deadline: Duration::from_secs(1), what: "the native analysis" };
        let reply = run::<serde_json::Value>(&hung, job).expect("a hung worker is a reply, not a death");
        assert_eq!(reply.unwrap_err(), "the native analysis did not finish in 1 s and was stopped");
        assert!(started.elapsed() < Duration::from_secs(10), "stopped at once: {:?}", started.elapsed());
        assert!(analysis_deadline(3600.0) > Duration::from_secs(7200));
        // A short clip still gets time for the whole frame budget on a slow machine.
        assert!(analysis_deadline(60.0) >= Duration::from_secs(360 * 6));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The job reaches the worker whole, its answer comes back, a failure
    /// it reports is the analysis's error, and an answer that cannot be
    /// read is one too rather than a result.
    #[test]
    fn the_job_goes_out_and_the_answer_comes_back() {
        let dir = scratch("answer");
        let echo = script(&dir, "echo.sh", "printf '{\"Done\":%s}' \"$2\"");
        let got = run_analyser_in(&echo, &Native, &item(), &crate::AnalysisCtx { ffmpeg: "/opt/ffmpeg".into() }).unwrap();
        let job: AnalysisJob = serde_json::from_value(got).unwrap();
        assert_eq!((job.analyser.as_str(), job.item.path.as_path(), job.ffmpeg.as_path()), ("native", Path::new("/media/a.mp4"), Path::new("/opt/ffmpeg")));

        let failed = script(&dir, "failed.sh", "printf '{\"Failed\":\"no detection model downloaded\"}'");
        assert_eq!(run_analyser_in(&failed, &Native, &item(), &crate::AnalysisCtx::default()).unwrap_err(), "no detection model downloaded");

        let garbage = script(&dir, "garbage.sh", "printf 'not json'");
        let err = run_analyser_in(&garbage, &Native, &item(), &crate::AnalysisCtx::default()).unwrap_err();
        assert!(err.contains("answer could not be read"), "{err}");

        // An app upgraded while it ran starts the new file at its path.
        let deleted = PathBuf::from(format!("{} (deleted)", echo.display()));
        assert_eq!(worker_exe(&deleted), echo);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A worker is told while the editor is busy, and the time it was
    /// held for does not count against its deadline.
    #[test]
    fn a_worker_hears_the_editor_is_busy_and_is_not_timed_for_it() {
        let dir = scratch("hold");
        let out = dir.join("heard");
        // Reads the busy bytes until its parent closes the pipe; answers
        // with what it heard once it has heard both.
        let listener = script(&dir, "listen.sh", &format!("head -c 2 > {}\nprintf '{{\"Done\":\"%s\"}}' \"$(cat {})\"", out.display(), out.display()));
        let busy = std::sync::Arc::new(AtomicBool::new(true));
        let flag = busy.clone();
        crate::hold::install(std::sync::Arc::new(move || flag.load(Ordering::Relaxed)));
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1500));
            busy.store(false, Ordering::Relaxed);
        });
        let job = Job { flag: ANALYSIS_FLAG, spec: "{}".into(), input: Input::Hold, deadline: Duration::from_secs(1), what: "the native analysis" };
        let heard = run::<String>(&listener, job).unwrap();
        crate::hold::install(std::sync::Arc::new(|| false));
        releaser.join().unwrap();
        assert_eq!(heard.as_deref(), Ok("10"), "busy, then idle; held 1.5 s past a 1 s deadline without being stopped");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// What a worker costs over running here (#689), on a real clip:
    /// `KLIPP_WORKER_EXE` (the app) and `KLIPP_MEASURE_CLIP`, with the
    /// models downloaded. Each native analyser runs here twice, the
    /// second warm, then in a worker.
    #[test]
    #[ignore = "measures; needs the app, a clip and the models"]
    fn measure_a_worker_against_running_here() {
        let (Some(exe), Some(clip)) = (std::env::var_os("KLIPP_WORKER_EXE"), std::env::var_os("KLIPP_MEASURE_CLIP")) else {
            eprintln!("set KLIPP_WORKER_EXE and KLIPP_MEASURE_CLIP");
            return;
        };
        let clip = PathBuf::from(clip);
        let mut item = klipp_media::MediaItem::synthetic_for_test(clip.clone(), None);
        let out = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
            .arg(&clip)
            .output()
            .unwrap();
        let secs: f64 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
        item.duration = klipp_core::RationalTime::from_secs_f64(secs, 30);
        let ctx = crate::AnalysisCtx::default();
        let time = |f: &dyn Fn() -> Result<serde_json::Value, String>| {
            let t = Instant::now();
            let r = f();
            (t.elapsed().as_secs_f64(), r.map(|_| ()))
        };
        for a in crate::native_analysers() {
            let cold = time(&|| a.run(&item, &ctx));
            let warm = time(&|| a.run(&item, &ctx));
            let worker = time(&|| run_analyser_in(Path::new(&exe), a.as_ref(), &item, &ctx));
            eprintln!("{}: here cold {:.2} s {:?}, here warm {:.2} s, in a worker {:.2} s {:?}", a.id(), cold.0, cold.1, warm.0, worker.0, worker.1);
        }
    }
}
