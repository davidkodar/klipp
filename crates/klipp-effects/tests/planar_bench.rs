//! The planar tracker on real footage (#2754): each clip of
//! `planar_bench.txt` followed from its first frame, and the points
//! checked on it mapped by the track to each frame they were checked on.
//! The mean error over all those frames and the error on the last one
//! (drift) must stay under the clip's limits, on the CPU and, when there
//! is one, the GPU.
//!
//! The clips are fetched outside the repository: run
//! `scripts/planar-bench.sh`, which checks them and runs this. A `local`
//! clip, which has no address, runs when its file is where the list says
//! and matches its sha256, and is reported as skipped otherwise.

use klipp_effects::gpu::planar::PlanarGpu;
use klipp_effects::planar::{track, Options, State, Tracked};
use klipp_effects::window_track::GrayFrame;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};

/// The width frames are read at: the size the editor tracks at.
const SIDE: u32 = 1280;

type Points = Vec<(f64, f64)>;

struct Clip {
    name: String,
    /// A clip kept only on this computer, where it is and its sha256:
    /// one with no address to fetch it from.
    local: Option<(PathBuf, String)>,
    quad: [(f64, f64); 4],
    exclude: Vec<Points>,
    /// Each frame checked and where the points are on it; the first is
    /// the frame the quad is set on.
    at: Vec<(usize, Points)>,
    /// The most mean error, and the most on the last frame checked,
    /// pixels.
    limit: (f64, f64),
    /// What the tracker can't do on it yet, when it can't.
    known: Option<String>,
}

fn points(words: &[&str]) -> Points {
    words
        .iter()
        .map(|w| {
            let (x, y) = w.split_once(',').expect("x,y");
            (x.parse().expect("x"), y.parse().expect("y"))
        })
        .collect()
}

fn clips(text: &str) -> Vec<Clip> {
    let mut out: Vec<Clip> = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words[0] {
            "clip" => out.push(Clip { name: words[1].into(), local: None, quad: [(0.0, 0.0); 4], exclude: Vec::new(), at: Vec::new(), limit: (0.0, 0.0), known: None }),
            "local" => {
                let path = words[3..].join(" ");
                let path = match path.strip_prefix("~/") {
                    Some(rest) => PathBuf::from(std::env::var_os("HOME").expect("HOME")).join(rest),
                    None => PathBuf::from(path),
                };
                out.push(Clip { name: words[1].into(), local: Some((path, words[2].into())), quad: [(0.0, 0.0); 4], exclude: Vec::new(), at: Vec::new(), limit: (0.0, 0.0), known: None });
            }
            "quad" => out.last_mut().expect("a clip").quad = points(&words[1..]).try_into().expect("four corners"),
            "exclude" => out.last_mut().expect("a clip").exclude.push(points(&words[1..])),
            "at" => out.last_mut().expect("a clip").at.push((words[1].parse().expect("frame"), points(&words[2..]))),
            "limit" => out.last_mut().expect("a clip").limit = (words[1].parse().expect("mean"), words[2].parse().expect("drift")),
            "known" => out.last_mut().expect("a clip").known = Some(words[1..].join(" ")),
            other => panic!("unknown line {other}"),
        }
    }
    out
}

/// A clip's frames read grey at [`SIDE`] across, one after another.
struct Frames {
    child: Child,
    out: BufReader<ChildStdout>,
    next: usize,
}

impl Frames {
    fn open(path: &Path) -> Self {
        let mut child = Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-i"])
            .arg(path)
            .args(["-vf", &format!("scale={SIDE}:-2:flags=bicubic,format=gray"), "-fps_mode", "passthrough", "-f", "image2pipe", "-vcodec", "pgm", "-"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("ffmpeg on the PATH");
        let out = BufReader::new(child.stdout.take().expect("piped"));
        Self { child, out, next: 0 }
    }

    /// The frame after the one read last, if the clip has one.
    fn read(&mut self) -> Option<GrayFrame> {
        let mut words = Vec::new();
        while words.len() < 4 {
            let mut line = String::new();
            if self.out.read_line(&mut line).ok()? == 0 {
                return None;
            }
            words.extend(line.split_whitespace().map(String::from));
        }
        let (width, height): (usize, usize) = (words[1].parse().ok()?, words[2].parse().ok()?);
        let mut data = vec![0u8; width * height];
        self.out.read_exact(&mut data).ok()?;
        self.next += 1;
        Some(GrayFrame { data, width, height })
    }

    /// Frame `n`, reading past those before it.
    fn frame(&mut self, n: usize) -> Option<GrayFrame> {
        while self.next < n {
            self.read()?;
        }
        (self.next == n).then(|| self.read()).flatten()
    }
}

impl Drop for Frames {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// How far the track puts the checked points from where they are, each
/// frame checked after the first.
fn errors(clip: &Clip, tracked: &[Tracked]) -> Vec<(usize, f64, Option<State>)> {
    let (_, from) = &clip.at[0];
    clip.at[1..]
        .iter()
        .map(|(frame, to)| match tracked.iter().find(|t| t.frame == *frame) {
            Some(t) => {
                let e = from.iter().zip(to).map(|(p, q)| {
                    let m = t.h.transform(p.0, p.1);
                    (m.0 - q.0).hypot(m.1 - q.1)
                });
                (*frame, e.sum::<f64>() / from.len() as f64, Some(t.state))
            }
            None => (*frame, f64::INFINITY, None),
        })
        .collect()
}

fn run(clip: &Clip, path: &Path, on_gpu: Option<&mut PlanarGpu>) -> Vec<Tracked> {
    let first = clip.at[0].0;
    let last = clip.at.last().expect("frames checked").0;
    let mut frames = Frames::open(path);
    let reference = frames.frame(first).expect("the first frame");
    let wanted: Vec<usize> = (first + 1..=last).collect();
    let mut next = |f: usize| frames.frame(f);
    let opts = Options::default();
    match on_gpu {
        Some(g) => g.track(&reference, clip.quad, &clip.exclude, &wanted, &mut next, &opts, &|_| true),
        None => track(&reference, clip.quad, &clip.exclude, &wanted, &mut next, &opts, &|_| true),
    }
}

/// Whether the file at `path` has the sha256 `sha`.
fn matches(path: &Path, sha: &str) -> bool {
    Command::new("sha256sum").arg(path).output().is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).split_whitespace().next() == Some(sha))
}

fn clip_dir() -> PathBuf {
    std::env::var_os("KLIPP_PLANAR_BENCH").map(PathBuf::from).unwrap_or_else(|| {
        let cache = std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").expect("HOME")).join(".cache"));
        cache.join("klipp/planar-bench")
    })
}

#[test]
#[ignore = "real footage fetched outside the repository: run scripts/planar-bench.sh"]
fn the_tracker_holds_real_surfaces() {
    let clips = clips(include_str!("planar_bench.txt"));
    let dir = clip_dir();
    let mut gpu = PlanarGpu::new();
    if gpu.is_none() {
        assert_ne!(std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref(), Ok("1"), "GPU required");
        eprintln!("UNVERIFIED on the GPU: none here");
    }
    let mut over = Vec::new();
    for clip in &clips {
        let path = match &clip.local {
            Some((path, sha)) => {
                if !matches(path, sha) {
                    eprintln!("{}: skipped, {} is not here or is not the clip annotated", clip.name, path.display());
                    continue;
                }
                path.clone()
            }
            None => dir.join(format!("{}.mp4", clip.name)),
        };
        assert!(path.exists(), "{} is not fetched: run scripts/planar-bench.sh", path.display());
        let mut runs = vec![("cpu", run(clip, &path, None))];
        if let Some(g) = gpu.as_mut() {
            runs.push(("gpu", run(clip, &path, Some(g))));
        }
        for (on, tracked) in runs {
            let e = errors(clip, &tracked);
            let mean = e.iter().map(|e| e.1).sum::<f64>() / e.len() as f64;
            let drift = e.last().expect("frames checked").1;
            let lost = tracked.iter().filter(|t| t.state == State::Lost).count();
            let each: Vec<String> = e.iter().map(|(f, px, s)| format!("{f}:{px:.2}{}", if s.is_some_and(|s| s != State::Good) { "*" } else { "" })).collect();
            eprintln!("{} {on}: mean {mean:.2} px, drift {drift:.2} px, {lost} lost of {}; {}", clip.name, tracked.len(), each.join(" "));
            let held = mean <= clip.limit.0 && drift <= clip.limit.1;
            match (&clip.known, held) {
                (None, false) => over.push(format!("{} {on}: mean {mean:.2} (limit {}), drift {drift:.2} (limit {})", clip.name, clip.limit.0, clip.limit.1)),
                (Some(known), false) => eprintln!("{} {on}: KNOWN, {known}", clip.name),
                (Some(_), true) => over.push(format!("{} {on}: now held within its limit: take its `known` line off", clip.name)),
                (None, true) => {}
            }
        }
    }
    assert!(over.is_empty(), "past the limits: {over:#?}");
}
