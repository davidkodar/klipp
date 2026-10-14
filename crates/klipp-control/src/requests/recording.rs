//! Screen recordings over the socket (#1903, #1904): started, watched and
//! stopped by id, or recorded whole in one call, with the capture's
//! codec and quality chosen and what was captured reported.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

/// The capture codecs gpu-screen-recorder takes (`-k`).
pub(crate) const CODECS: [&str; 6] = ["h264", "hevc", "av1", "vp9", "hevc_10bit", "av1_10bit"];
/// Its quality presets (`-q`).
pub(crate) const QUALITIES: [&str; 4] = ["medium", "high", "very_high", "ultra"];

/// The recorder on the machine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Recorder {
    GpuScreenRecorder,
    WfRecorder,
}

impl Recorder {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Recorder::GpuScreenRecorder => "gpu-screen-recorder",
            Recorder::WfRecorder => "wf-recorder",
        }
    }
}

/// What a recording is asked to capture.
#[derive(Debug, Clone)]
pub(crate) struct Capture {
    pub(crate) target: String,
    pub(crate) fps: u32,
    pub(crate) codec: String,
    pub(crate) quality: Option<String>,
}

/// The recorder's arguments for `capture` into `out`, or why it can't take
/// them: an unknown codec, or a quality wf-recorder has no setting for.
pub(crate) fn recorder_args(recorder: Recorder, capture: &Capture, out: &Path) -> Result<Vec<String>, String> {
    if !CODECS.contains(&capture.codec.as_str()) {
        return Err(format!("no capture codec {:?}; give {}", capture.codec, CODECS.join(", ")));
    }
    let target = &capture.target;
    let region = target.contains('+') && target.contains('x');
    let mut args: Vec<String> = Vec::new();
    match recorder {
        Recorder::GpuScreenRecorder => {
            let quality = capture.quality.as_deref().unwrap_or("high");
            if !QUALITIES.contains(&quality) {
                return Err(format!("no capture quality {quality:?}; give {}", QUALITIES.join(", ")));
            }
            if region {
                args.extend(["-w".into(), "region".into(), "-region".into(), target.clone()]);
            } else {
                args.extend(["-w".into(), target.clone()]);
            }
            args.extend(["-f".into(), capture.fps.to_string(), "-k".into(), capture.codec.clone(), "-q".into(), quality.into(), "-cursor".into(), "yes".into(), "-o".into()]);
        }
        Recorder::WfRecorder => {
            if let Some(q) = &capture.quality {
                return Err(format!("wf-recorder has no quality preset to set {q:?} with; leave quality out, or install gpu-screen-recorder"));
            }
            // Its encoders write 8 bits whatever the codec's name says.
            if capture.codec.ends_with("_10bit") {
                return Err(format!("wf-recorder records 8 bits a sample, so {} would come out 8-bit; give hevc or av1, or install gpu-screen-recorder for 10-bit", capture.codec));
            }
            if region {
                // wf-recorder wants "X,Y WxH".
                let (size, pos) = target.split_once('+').unwrap_or((target, "0+0"));
                args.extend(["-g".into(), format!("{} {}", pos.replace('+', ","), size)]);
            }
            let encoder = match capture.codec.as_str() {
                "h264" => "libx264",
                "hevc" | "hevc_10bit" => "libx265",
                "av1" | "av1_10bit" => "libsvtav1",
                _ => "libvpx-vp9",
            };
            args.extend(["-r".into(), capture.fps.to_string(), "-c".into(), encoder.into(), "-f".into()]);
        }
    }
    args.push(out.to_string_lossy().into_owned());
    Ok(args)
}

/// Where a recording is in its life.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum State {
    /// Started; nothing written yet.
    Starting,
    /// Writing.
    Recording,
    /// The recorder ended on its own, at its length: `stop` finishes it.
    Ended,
    /// Being stopped.
    Stopping,
    /// Stopped and the file finished.
    Finished,
    /// It stopped without a file.
    Failed,
}

impl State {
    pub(crate) fn label(self) -> &'static str {
        match self {
            State::Starting => "starting",
            State::Recording => "recording",
            State::Ended => "ended",
            State::Stopping => "stopping",
            State::Finished => "finished",
            State::Failed => "failed",
        }
    }
}

/// One recording: its recorder while it runs, and what it ended as.
pub(crate) struct Recording {
    pub(crate) id: String,
    pub(crate) path: PathBuf,
    pub(crate) started: Instant,
    pub(crate) max_secs: f64,
    pub(crate) capture: Capture,
    pub(crate) recorder: Recorder,
    child: Mutex<Option<Child>>,
    /// The recorder's last lines on stderr, read as it writes them so a
    /// long take never fills the pipe and stalls it.
    stderr_tail: Arc<Mutex<std::collections::VecDeque<String>>>,
    stopping: std::sync::atomic::AtomicBool,
    /// How it ended: `Ok` how it was stopped, `Err` why it failed.
    ended: Mutex<Option<Result<String, String>>>,
}

fn registry() -> &'static Mutex<HashMap<String, Arc<Recording>>> {
    static RECORDINGS: OnceLock<Mutex<HashMap<String, Arc<Recording>>>> = OnceLock::new();
    RECORDINGS.get_or_init(Default::default)
}

/// Send `signal` ("INT", "TERM", "KILL") to `pid` with the `kill`
/// command; whether it was sent.
fn signal(pid: u32, signal: &str) -> bool {
    Command::new("kill").args([&format!("-{signal}"), &pid.to_string()]).status().is_ok_and(|s| s.success())
}

/// How a stop asks the recorder to finish, and how long it waits each
/// time: SIGINT, on which a recorder writes its file's index; then TERM;
/// then KILL, which leaves the file without one.
const STOP_STEPS: [(&str, f64); 3] = [("INT", 10.0), ("TERM", 5.0), ("KILL", 3.0)];
/// How many of the recorder's stderr lines are kept.
const STDERR_LINES: usize = 20;

impl Recording {
    /// Start `program` with `args` as recording `id` into `path`; it is
    /// stopped after `max_secs` if nobody stops it first.
    pub(crate) fn start(id: String, program: &Path, args: &[String], path: PathBuf, max_secs: f64, capture: Capture, recorder: Recorder) -> Result<Arc<Self>, String> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("could not start the recorder: {e}"))?;
        let stderr_tail: Arc<Mutex<std::collections::VecDeque<String>>> = Default::default();
        if let Some(err) = child.stderr.take() {
            let tail = Arc::clone(&stderr_tail);
            std::thread::spawn(move || {
                use std::io::BufRead;
                for line in std::io::BufReader::new(err).lines().map_while(Result::ok) {
                    let mut t = tail.lock().unwrap_or_else(|e| e.into_inner());
                    if t.len() == STDERR_LINES {
                        t.pop_front();
                    }
                    t.push_back(line);
                }
            });
        }
        let rec = Arc::new(Self {
            id: id.clone(), path, started: Instant::now(), max_secs, capture, recorder,
            child: Mutex::new(Some(child)), stderr_tail, stopping: Default::default(), ended: Mutex::new(None),
        });
        registry().lock().unwrap_or_else(|e| e.into_inner()).insert(id, Arc::clone(&rec));
        // Stopped at its length if still running then.
        let watched = Arc::downgrade(&rec);
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs_f64(max_secs));
            if let Some(rec) = watched.upgrade() {
                rec.signal_if_running("INT");
            }
        });
        Ok(rec)
    }

    /// The recording called `id`.
    pub(crate) fn find(id: &str) -> Option<Arc<Self>> {
        registry().lock().unwrap_or_else(|e| e.into_inner()).get(id.trim()).cloned()
    }

    /// Drop a recording from the registry once its outcome is given.
    pub(crate) fn forget(id: &str) {
        registry().lock().unwrap_or_else(|e| e.into_inner()).remove(id);
    }

    /// Whether its recorder is still running.
    pub(crate) fn running(&self) -> bool {
        let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
        match child.as_mut() {
            Some(c) => matches!(c.try_wait(), Ok(None)),
            None => false,
        }
    }

    /// Send the recorder `sig`, if it still runs. Checked and sent under
    /// the lock, on a child not yet waited for, so the pid is still its
    /// own and never a process that took the number after it.
    fn signal_if_running(&self, sig: &str) -> bool {
        let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
        match child.as_mut() {
            Some(c) => matches!(c.try_wait(), Ok(None)) && signal(c.id(), sig),
            None => false,
        }
    }

    /// Bytes written so far.
    pub(crate) fn bytes(&self) -> u64 {
        std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0)
    }

    /// Where it is; never waits on a stop in progress.
    pub(crate) fn state(&self) -> State {
        if let Some(ended) = self.ended.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            return if ended.is_ok() { State::Finished } else { State::Failed };
        }
        if self.stopping.load(std::sync::atomic::Ordering::Acquire) {
            return State::Stopping;
        }
        match (self.running(), self.bytes() > 0) {
            (true, false) => State::Starting,
            (true, true) => State::Recording,
            (false, _) => State::Ended,
        }
    }

    /// The recorder's last stderr line, for a failure.
    fn last_words(&self) -> String {
        self.stderr_tail.lock().unwrap_or_else(|e| e.into_inner()).back().cloned().unwrap_or_default()
    }

    /// Stop it, if it still runs, and wait for its file, a bounded time
    /// for each step of [`STOP_STEPS`]; a status asked meanwhile answers
    /// `stopping` at once. Once ended, again gives the same outcome; one
    /// stop at a time.
    pub(crate) fn stop(&self) -> Result<String, String> {
        if let Some(e) = self.ended.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            return e.clone();
        }
        if self.stopping.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return Err(format!("{} is already being stopped; ask its status", self.id));
        }
        let mut how = "stopped".to_string();
        for (step, (sig, secs)) in STOP_STEPS.iter().enumerate() {
            if !self.running() {
                break;
            }
            if self.signal_if_running(sig) && step > 0 {
                how = format!("stopped with {sig}, the recorder not having finished on INT; its file may lack its index");
            }
            let until = Instant::now() + std::time::Duration::from_secs_f64(*secs);
            while self.running() && Instant::now() < until {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        let outcome = if self.running() {
            let pid = self.child.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|c| c.id()).unwrap_or(0);
            Err(format!("the recorder (pid {pid}) did not stop on INT, TERM or KILL"))
        } else if !self.path.is_file() || self.bytes() == 0 {
            Err(format!("the recorder wrote nothing: {}", self.last_words()))
        } else {
            Ok(how)
        };
        // The child is reaped by `running`; nothing is left to wait on.
        self.child.lock().unwrap_or_else(|e| e.into_inner()).take();
        *self.ended.lock().unwrap_or_else(|e| e.into_inner()) = Some(outcome.clone());
        self.stopping.store(false, std::sync::atomic::Ordering::Release);
        outcome
    }
}

/// A pixel format's bits a sample: `yuv420p10le` 10, `p010le` 10,
/// `yuv420p` 8.
pub(crate) fn bit_depth_of(pix_fmt: &str) -> u32 {
    if pix_fmt.contains("12") {
        12
    } else if pix_fmt.contains("10") || pix_fmt.starts_with("p01") {
        10
    } else {
        8
    }
}

/// What a file was captured as, read off its picture stream: size, frame
/// rate and codec (#1904). `None` without ffprobe or a picture.
pub(crate) fn captured(path: &Path) -> Option<serde_json::Value> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=codec_name,width,height,avg_frame_rate,pix_fmt", "-of", "default=nw=1"])
        .arg(path)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let field = |k: &str| text.lines().find_map(|l| l.strip_prefix(&format!("{k}=")).map(str::to_owned));
    let fps = field("avg_frame_rate").and_then(|r| {
        let (n, d) = r.split_once('/')?;
        let (n, d): (f64, f64) = (n.parse().ok()?, d.parse().ok()?);
        (d > 0.0).then(|| (n / d * 1000.0).round() / 1000.0)
    });
    Some(serde_json::json!({
        "width": field("width")?.parse::<u32>().ok()?,
        "height": field("height")?.parse::<u32>().ok()?,
        "fps": fps,
        "codec": field("codec_name")?,
        "pixel_format": field("pix_fmt"),
        "bit_depth": field("pix_fmt").map(|p| bit_depth_of(&p)),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture(codec: &str, quality: Option<&str>) -> Capture {
        Capture { target: "focused".into(), fps: 60, codec: codec.into(), quality: quality.map(Into::into) }
    }

    /// #1904: the codec and quality asked for are the recorder's, the
    /// defaults as they were; what a recorder can't take is refused.
    #[test]
    fn a_capture_asks_the_recorder_for_its_codec_and_quality() {
        let out = Path::new("/tmp/take.mp4");
        let gsr = recorder_args(Recorder::GpuScreenRecorder, &capture("hevc", Some("very_high")), out).unwrap();
        let at = |k: &str| gsr.iter().position(|a| a == k).map(|i| gsr[i + 1].as_str());
        assert_eq!((at("-k"), at("-q"), at("-f"), at("-w")), (Some("hevc"), Some("very_high"), Some("60"), Some("focused")));
        assert_eq!(gsr.last().map(String::as_str), Some("/tmp/take.mp4"));
        let plain = recorder_args(Recorder::GpuScreenRecorder, &capture("h264", None), out).unwrap();
        assert!(plain.windows(2).any(|w| w == ["-q", "high"]), "the default quality: {plain:?}");
        let err = recorder_args(Recorder::GpuScreenRecorder, &capture("h264", Some("max")), out).unwrap_err();
        assert!(QUALITIES.iter().all(|q| err.contains(q)), "{err}");
        let err = recorder_args(Recorder::GpuScreenRecorder, &capture("prores", None), out).unwrap_err();
        assert!(CODECS.iter().all(|c| err.contains(c)), "{err}");
        let mut region = capture("h264", None);
        region.target = "1920x1080+0+0".into();
        let wf = recorder_args(Recorder::WfRecorder, &region, out).unwrap();
        assert!(wf.windows(2).any(|w| w == ["-g", "0,0 1920x1080"]) && wf.windows(2).any(|w| w == ["-c", "libx264"]), "{wf:?}");
        assert!(recorder_args(Recorder::WfRecorder, &capture("h264", Some("ultra")), out).is_err(), "wf-recorder has no preset");
    }

    fn wait_for(rec: &Recording, want: State) -> State {
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while rec.state() != want && Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        rec.state()
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("klipp-recording-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// #1903: a recording started is found by its id, starts, records once
    /// it has written, and stops to finished with its file; stopping
    /// again gives the same, and a finished one can be dropped. One that
    /// writes nothing fails, saying so. `sh` stands in for the recorder:
    /// it writes, then waits to be interrupted.
    #[test]
    fn a_recording_is_started_watched_and_stopped_by_its_id() {
        let dir = scratch("stop");
        let path = dir.join("take.mp4");
        let script = format!("printf data > '{}'; exec sleep 30", path.display());
        let rec = Recording::start("rec-test-1".into(), Path::new("sh"), &["-c".into(), script], path.clone(), 30.0, capture("h264", None), Recorder::GpuScreenRecorder).unwrap();
        assert!(Recording::find("rec-test-1").is_some_and(|r| Arc::ptr_eq(&r, &rec)));
        assert_eq!(wait_for(&rec, State::Recording), State::Recording, "written: ready");
        assert_eq!(rec.stop(), Ok("stopped".into()));
        assert_eq!(rec.state(), State::Finished);
        assert_eq!(rec.stop(), Ok("stopped".into()), "stopped again: the same");
        Recording::forget("rec-test-1");
        assert!(Recording::find("rec-test-1").is_none(), "dropped once given");

        let silent = Recording::start("rec-test-2".into(), Path::new("sh"), &["-c".into(), "echo 'no output device' >&2; exec sleep 30".into()], dir.join("nothing.mp4"), 30.0, capture("h264", None), Recorder::GpuScreenRecorder).unwrap();
        assert_eq!(silent.state(), State::Starting, "nothing written yet");
        std::thread::sleep(std::time::Duration::from_millis(200));
        let err = silent.stop().unwrap_err();
        assert!(err.contains("wrote nothing") && err.contains("no output device"), "the recorder's own last words: {err}");
        assert_eq!(silent.state(), State::Failed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #1903: a recording not stopped is stopped at its length; its state
    /// then reads ended, and reading it changes nothing until a stop.
    #[test]
    fn a_recording_stops_at_its_length_and_waits_for_stop() {
        let dir = scratch("len");
        let path = dir.join("take.mp4");
        let script = format!("printf data > '{}'; exec sleep 30", path.display());
        let rec = Recording::start("rec-test-3".into(), Path::new("sh"), &["-c".into(), script], path, 0.3, capture("h264", None), Recorder::GpuScreenRecorder).unwrap();
        assert_eq!(wait_for(&rec, State::Ended), State::Ended, "stopped at 0.3 s");
        assert_eq!(rec.state(), State::Ended, "a status reads, it doesn't finish");
        assert_eq!(rec.stop(), Ok("stopped".into()));
        assert_eq!(rec.state(), State::Finished);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #1903: a recorder deaf to INT is stopped with TERM after its wait,
    /// and the stop says so; a status meanwhile answers at once.
    #[test]
    fn a_recorder_that_ignores_int_is_stopped_harder() {
        let dir = scratch("deaf");
        let path = dir.join("take.mp4");
        let script = format!("trap '' INT; printf data > '{}'; while :; do sleep 0.1; done", path.display());
        let rec = Recording::start("rec-test-4".into(), Path::new("sh"), &["-c".into(), script], path, 60.0, capture("h264", None), Recorder::GpuScreenRecorder).unwrap();
        assert_eq!(wait_for(&rec, State::Recording), State::Recording);
        let stopping = Arc::clone(&rec);
        let stopper = std::thread::spawn(move || stopping.stop());
        assert_eq!(wait_for(&rec, State::Stopping), State::Stopping, "a status during the stop answers");
        let how = stopper.join().unwrap().unwrap();
        assert!(how.contains("TERM"), "{how}");
        assert_eq!(rec.state(), State::Finished);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #1903: a recorder that writes to stderr all the while doesn't stall
    /// on a full pipe: its lines are read as they come.
    #[test]
    fn a_chatty_recorder_never_blocks() {
        let dir = scratch("chatty");
        let path = dir.join("take.mp4");
        // 200 KB of stderr, three pipes' worth, then the file.
        let script = format!("i=0; while [ $i -lt 2000 ]; do echo 'frame 0123456789 0123456789 0123456789 0123456789 0123456789 0123456789 0123456789 012345' >&2; i=$((i+1)); done; printf data > '{}'; exec sleep 30", path.display());
        let rec = Recording::start("rec-test-5".into(), Path::new("sh"), &["-c".into(), script], path, 30.0, capture("h264", None), Recorder::GpuScreenRecorder).unwrap();
        assert_eq!(wait_for(&rec, State::Recording), State::Recording, "it got past its chatter to the file");
        assert_eq!(rec.stop(), Ok("stopped".into()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pixel_format_gives_its_depth() {
        assert_eq!(bit_depth_of("yuv420p"), 8);
        assert_eq!(bit_depth_of("yuv420p10le"), 10);
        assert_eq!(bit_depth_of("p010le"), 10);
        assert_eq!(bit_depth_of("yuv444p12le"), 12);
        assert!(recorder_args(Recorder::WfRecorder, &capture("hevc_10bit", None), Path::new("/tmp/t.mp4")).unwrap_err().contains("8-bit"));
    }
}
