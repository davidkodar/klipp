//! The headless engine parked on one graded clip, with a slider dragged
//! the way the UI drags one — for the tests about paused grading.

#![allow(dead_code)]

use klipp_timeline::GradeAccess;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use crossbeam_queue::ArrayQueue;
use klipp_core::*;
use klipp_engine::perf::{PerfRing, PerfSample};
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams, NodePos};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

pub const FPS: u32 = 25;

/// How long the frame buffer must stay unchanged for a paused frame to
/// count as settled. A stream that opens after this much silence would be
/// read as missing — the picture would then be wrong by a whole layer, so
/// a window that is too short fails loudly rather than passing quietly.
pub const QUIET: Duration = Duration::from_millis(250);

/// The newest frame `fb` holds.
pub fn generation(fb: &Mutex<FrameBuffer>) -> u64 {
    fb.lock().unwrap().latest_generation()
}

/// [`Parked::settle`] for any engine's frame buffer.
pub fn settle(fb: &Mutex<FrameBuffer>) {
    // Thirty seconds at the most: a loaded machine settles later, not
    // wrongly. Quiet counts only on a complete frame: one drawn before
    // every clip's stream had its picture is still to be redrawn.
    let cap = Instant::now() + Duration::from_secs(30);
    let mut g = generation(fb);
    while Instant::now() < cap {
        match wait_new(fb, g, QUIET) {
            Some(_) => g = generation(fb),
            None if fb.lock().unwrap().latest_complete() => return,
            None => {}
        }
    }
}

/// [`Parked::wait_new`] for any engine's frame buffer.
pub fn wait_new(fb: &Mutex<FrameBuffer>, since: u64, limit: Duration) -> Option<Duration> {
    let t0 = Instant::now();
    while t0.elapsed() < limit {
        if generation(fb) != since {
            return Some(t0.elapsed());
        }
        std::thread::sleep(Duration::from_micros(200));
    }
    None
}

/// A two-second ffmpeg test pattern, 10-bit, tagged with `setparams`
/// (`tags`, e.g. `color_trc=smpte2084:...`) — FFV1 in Matroska keeps
/// only tags set on the frames. `None` without ffmpeg.
pub fn source(name: &str, width: u32, height: u32, tags: &str) -> Option<PathBuf> {
    encoded(name, width, height, tags, Fps::new(FPS, 1), &["-c:v", "ffv1"], "mkv")
}

/// [`source`] as 10-bit HEVC with a keyframe every second — long-GOP, as
/// camera and delivery files are, and decoded by NVDEC where there is one.
pub fn hevc_source(name: &str, width: u32, height: u32, tags: &str) -> Option<PathBuf> {
    hevc_source_at(name, width, height, tags, Fps::new(FPS, 1))
}

/// [`hevc_source`] at another frame rate (29.97 is `Fps::new(30000, 1001)`).
pub fn hevc_source_at(name: &str, width: u32, height: u32, tags: &str, fps: Fps) -> Option<PathBuf> {
    let gop = fps.as_f64().round().to_string();
    encoded(name, width, height, tags, fps, &["-c:v", "libx265", "-preset", "ultrafast", "-x265-params", &format!("keyint={gop}:min-keyint={gop}:log-level=error")], "mp4")
}

fn encoded(name: &str, width: u32, height: u32, tags: &str, fps: Fps, codec: &[&str], ext: &str) -> Option<PathBuf> {
    let out = std::env::temp_dir().join(format!("klipp-parked-{name}-{width}x{height}.{ext}"));
    if out.exists() {
        return Some(out);
    }
    let rate = format!("{}/{}", fps.num, fps.den);
    // Written under a name of its own and moved into place: written to
    // `out` directly, a test running alongside found it existing and
    // decoded the half that was there.
    let partial = unshared(&out, ext);
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", &format!("testsrc2=s={width}x{height}:r={rate}:d=2")])
        .args(["-vf", &format!("format=yuv420p10le,setparams=range=tv:{tags}")])
        .args(codec)
        .arg(&partial)
        .status()
        .ok()?
        .success();
    if !ok {
        let _ = std::fs::remove_file(&partial);
        return None;
    }
    std::fs::rename(&partial, &out).ok()?;
    Some(out)
}

/// A scratch path beside the cached fixture `out`, with extension `ext`,
/// that no other writer of `out` shares. The tests in a binary run
/// concurrently, and on a cold temp dir several build the same fixture at
/// once: under one shared scratch name, one deleted or overwrote the file
/// another was still encoding, which panicked, handed out a half file, or
/// returned `None` and so skipped a test that then read as a pass. Each
/// writer renames a whole file into place, and they all write the same
/// bytes, so the last rename wins harmlessly.
pub fn unshared(out: &std::path::Path, ext: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    out.with_extension(format!("{}-{n}.{ext}", std::process::id()))
}

pub const PQ_TAGS: &str = "colorspace=bt2020nc:color_primaries=bt2020:color_trc=smpte2084";
pub const SDR_TAGS: &str = "colorspace=bt709:color_primaries=bt709:color_trc=bt709";

/// A grade of one Exposure node at `stops`.
pub fn exposure_graph(stops: f32) -> NodeGraph {
    let node = |kind, params| Node {
        id: NodeId::new_v4(), kind, label: format!("{kind:?}"), enabled: true, params,
        keyframes: Default::default(), output_link: None, input_links: vec![],
    };
    let p = NodeParams {
        exposure: Some(stops),
        ..Default::default()
    };
    let nodes = vec![node(NodeKind::Source, NodeParams::default()), node(NodeKind::Exposure, p), node(NodeKind::Output, NodeParams::default())];
    let edges = (0..2).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 }).collect();
    let node_positions = nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 100.0, y: 0.0 })).collect();
    NodeGraph { output_node_id: nodes[2].id, nodes, edges, node_positions, working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

/// Frame `f` as a timeline position at `fps`: `f * den` ticks of `num`
/// a second.
pub fn frame_tc(f: i64, fps: Fps) -> RationalTime {
    RationalTime { value: f * fps.den as i64, rate: fps.num }
}

/// The engine, running headless on one clip of `src` graded with an
/// Exposure node, parked on frame 10.
pub struct Parked {
    cmd: crossbeam_channel::Sender<AppCommand>,
    fb: Arc<Mutex<FrameBuffer>>,
    playhead: Arc<Mutex<Option<RationalTime>>>,
    pub fps: Fps,
    perf: Arc<Mutex<PerfRing>>,
    params: Arc<ArrayQueue<ParamChange>>,
    thread: Option<std::thread::JoinHandle<()>>,
    pub clip_id: ClipId,
    pub source_node: NodeId,
    pub exposure_node: NodeId,
    /// The person's input as the UI reports it: `touch_input` is a key
    /// pressed or a click.
    pub foreground: Arc<klipp_jobs::ForegroundActivity>,
    /// The sequence the engine plays: replaced, an edit as the UI makes it.
    pub sequence: Arc<klipp_timeline::SharedSequence>,
}

impl Parked {
    pub fn start(src: PathBuf, width: u32, height: u32) -> Self {
        Self::start_at(src, width, height, Fps::new(FPS, 1), true)
    }

    /// [`Self::start`] with the clip ungraded: what it shows is the
    /// source's own levels.
    pub fn start_ungraded(src: PathBuf, width: u32, height: u32) -> Self {
        Self::start_at(src, width, height, Fps::new(FPS, 1), false)
    }

    /// [`Self::start`] with a clip `secs` long, all of a source at least
    /// that long, for a test that has to keep playing: the usual clip is
    /// two seconds, parked 1.6 s from its end, and a loaded machine can
    /// spend that on one slow pass before a watch has begun (#1828).
    pub fn start_long(src: PathBuf, width: u32, height: u32, secs: u32) -> Self {
        Self::start_spanning(src, width, height, Fps::new(FPS, 1), true, (secs * FPS) as i64, |_| {}, true)
    }

    /// [`Self::start`] with the clip changed by `tweak` before the engine
    /// sees it: an effect added, a node put above the Exposure node. The
    /// slider still moves that Exposure node.
    pub fn start_with(src: PathBuf, width: u32, height: u32, tweak: impl FnOnce(&mut Clip)) -> Self {
        Self::start_spanning(src, width, height, Fps::new(FPS, 1), true, 50, tweak, true)
    }

    /// [`Self::start_with`] on an engine that keeps no temporal neighbours
    /// between renders (#1863): every render decodes them.
    pub fn start_with_unheld(src: PathBuf, width: u32, height: u32, tweak: impl FnOnce(&mut Clip)) -> Self {
        Self::start_spanning(src, width, height, Fps::new(FPS, 1), true, 50, tweak, false)
    }

    /// [`Self::start`] on a timeline at `fps`, for a source at that rate.
    pub fn start_at(src: PathBuf, width: u32, height: u32, fps: Fps, graded: bool) -> Self {
        Self::start_spanning(src, width, height, fps, graded, 50, |_| {}, true)
    }

    /// [`Self::start_with`] on a timeline at `fps`, whatever the source's
    /// rate.
    pub fn start_with_at(src: PathBuf, width: u32, height: u32, fps: Fps, tweak: impl FnOnce(&mut Clip)) -> Self {
        Self::start_spanning(src, width, height, fps, true, 50, tweak, true)
    }

    /// [`Self::start_with_at`] on an engine that keeps nothing between
    /// renders.
    pub fn start_with_at_unheld(src: PathBuf, width: u32, height: u32, fps: Fps, tweak: impl FnOnce(&mut Clip)) -> Self {
        Self::start_spanning(src, width, height, fps, true, 50, tweak, false)
    }

    /// [`Self::start_at`] with a clip `frames` long.
    fn start_spanning(src: PathBuf, width: u32, height: u32, fps: Fps, graded: bool, frames: i64, tweak: impl FnOnce(&mut Clip), hold: bool) -> Self {
        Self::start_spanning_under(src, width, height, fps, graded, frames, tweak, hold, None)
    }

    /// [`Self::start`] with `title` on a text track over the clip, for
    /// the parked drag with titles on screen (#2351).
    pub fn start_under_title(src: PathBuf, width: u32, height: u32, title: klipp_timeline::TitleData) -> Self {
        Self::start_spanning_under(src, width, height, Fps::new(FPS, 1), true, 50, |_| {}, true, Some(title))
    }

    #[allow(clippy::too_many_arguments)]
    fn start_spanning_under(src: PathBuf, width: u32, height: u32, fps: Fps, graded: bool, frames: i64, tweak: impl FnOnce(&mut Clip), hold: bool, title: Option<klipp_timeline::TitleData>) -> Self {
        let tc = |f: i64| frame_tc(f, fps);
        let id = MediaId::new();
        let mut clip = Clip { kind: ClipKind::Media { media_id: id }, ..Clip::gap(tc(0), tc(frames)) };
        clip.source_in = tc(0);
        clip.source_out = tc(frames);
        clip.set_grade(exposure_graph(if graded { 0.5 } else { 0.0 }));
        tweak(&mut clip);
        let of = |kind: NodeKind| clip.grade().nodes.iter().find(|n| n.kind == kind).map(|n| n.id).expect("the grade keeps its node");
        let (clip_id, source_node, exposure_node) = (clip.id, of(NodeKind::Source), of(NodeKind::Exposure));
        let mut seq = Sequence::new_default();
        seq.fps = fps;
        seq.resolution = Resolution { width, height };
        let mut t = Track::new("V1", TrackKind::Video);
        t.clips = vec![clip];
        seq.tracks = vec![t];
        if let Some(title) = title {
            let mut text = Track::new("T1", TrackKind::Text);
            text.clips = vec![Clip { kind: ClipKind::Title(title), ..Clip::gap(tc(0), tc(frames)) }];
            // Tracks run top first.
            seq.tracks.insert(0, text);
        }

        let mut pool = MediaPool::default();
        pool.add(MediaItem {
            id,
            path: src.clone(),
            kind: klipp_media::MediaItemKind::File,
            resolution: Resolution { width, height },
            ..MediaItem::new_synthetic_title(id, src.display().to_string(), tc(frames), fps)
        });

        let (cmd, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
        let playhead = Arc::new(Mutex::new(None));
        let reported = Arc::clone(&playhead);
        std::thread::spawn(move || {
            while let Ok(ev) = ev_rx.recv() {
                if let AppEvent::PlayheadMoved(tc) = ev {
                    *reported.lock().unwrap() = Some(tc);
                }
            }
        });
        let fb = Arc::new(Mutex::new(FrameBuffer::new(width, height)));
        let perf = Arc::new(Mutex::new(PerfRing::default()));
        let sequence = Arc::new(klipp_timeline::SharedSequence::new(seq));
        let mut engine = PlaybackEngine::new(
            cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(width, height))),
            Arc::clone(&sequence), Arc::new(RwLock::new(pool)), Box::new(|| {}),
            Arc::new(Mutex::new(None)), Arc::clone(&perf),
        );
        engine.set_neighbour_hold(hold);
        let foreground = klipp_jobs::ForegroundActivity::new();
        engine.set_foreground_activity(Arc::clone(&foreground));
        let params = engine.param_change_queue();
        let thread = Some(std::thread::spawn(move || engine.run()));
        let parked = Self { cmd, fb, playhead, fps, perf, params, thread, clip_id, source_node, exposure_node, foreground, sequence };

        parked.send(AppCommand::SetPlayhead(tc(10)));
        let g = parked.generation();
        parked.wait_new(g, Duration::from_secs(20)).expect("no first frame");
        parked.settle();
        parked
    }

    pub fn send(&self, c: AppCommand) {
        self.cmd.send(c).unwrap();
    }

    /// The strip over the ruler as the engine last worked it out (#3212).
    pub fn render_status(&self) -> Vec<klipp_engine::texture::RenderSpan> {
        self.fb.lock().unwrap().render_status.clone()
    }

    /// The playhead the engine last reported, as a frame number.
    pub fn frame_number(&self) -> Option<f64> {
        let tc = (*self.playhead.lock().unwrap())?;
        Some(tc.value as f64 * self.fps.num as f64 / (tc.rate as f64 * self.fps.den as f64))
    }

    pub fn generation(&self) -> u64 {
        generation(&self.fb)
    }

    /// Wait for a frame newer than `since`, then for the picture to stop
    /// changing; how long the first frame took. A paused frame is rendered
    /// again as what it was waiting on arrives — its streams, its ungraded
    /// picture — and the last of those is the one a test reads.
    pub fn settle_after(&self, since: u64, limit: Duration) -> Option<Duration> {
        let took = self.wait_new(since, limit)?;
        self.settle();
        Some(took)
    }

    /// Wait until no new frame has arrived for [`QUIET`] on a complete
    /// frame, thirty seconds at the most. The engine has no "done" signal:
    /// paused, it wakes every 16 ms to look for commands, so quiet on the
    /// frame buffer is the
    /// measure. It replaced fixed sleeps of 250–1500 ms, which were both
    /// slower and, under a loaded machine, no safer.
    ///
    /// Quiet is only meaningful once something has been drawn: before the
    /// first frame the buffer is quiet because it is empty. A command that
    /// must produce a frame goes through [`Self::settle_after`], which
    /// waits for that frame first.
    pub fn settle(&self) {
        settle(&self.fb)
    }

    /// Wait for a frame newer than `since`; how long it took.
    pub fn wait_new(&self, since: u64, limit: Duration) -> Option<Duration> {
        wait_new(&self.fb, since, limit)
    }

    pub fn begin_drag(&self) {
        self.send(AppCommand::BeginNodeParamEdit { clip_id: self.clip_id });
    }

    /// [`Self::begin_drag`], then steps of the slider from `from` until a
    /// step's frame is one the engine rendered inside the drag (#1799):
    /// the begin goes through the command channel and a step through the
    /// parameter queue, so on a loaded machine a step can be rendered
    /// before the engine has seen the begin. The perf sample of that
    /// frame, after which every step is a drag step.
    pub fn begin_drag_seen(&self, from: f32) -> klipp_engine::perf::PerfSample {
        self.begin_drag();
        for k in 0..20 {
            self.step(from + 0.05 * k as f32).expect("a drag step showed no frame");
            if let Some(s) = self.perf().last().filter(|s| s.in_param_edit) {
                return *s;
            }
        }
        panic!("the engine never rendered a frame inside the drag");
    }

    /// Move the exposure slider to `stops` and wait for the frame; the
    /// time from the change to the frame, ms.
    pub fn step(&self, stops: f32) -> Option<f64> {
        let g = self.generation();
        let t0 = Instant::now();
        let _ = self.params.push(ParamChange {
            clip_id: self.clip_id,
            node_id: self.exposure_node,
            param: "exposure".into(),
            value: stops,
        });
        self.wait_new(g, Duration::from_secs(60)).map(|_| t0.elapsed().as_secs_f64() * 1000.0)
    }

    /// Let go of the slider and wait for the regrade that follows, ms.
    pub fn end_drag(&self) -> Option<f64> {
        let g = self.generation();
        let t0 = Instant::now();
        self.send(AppCommand::EndNodeParamEdit { clip_id: self.clip_id });
        self.wait_new(g, Duration::from_secs(60)).map(|_| t0.elapsed().as_secs_f64() * 1000.0)
    }

    /// The frame on screen, RGBA.
    pub fn frame(&self) -> Vec<u8> {
        self.fb.lock().unwrap().latest_frame().0.to_vec()
    }

    /// Frames shown on the GPU without their bytes (zero-copy), and the
    /// display bytes' generation.
    pub fn gpu_frames_and_generation(&self) -> (u64, u64) {
        let fb = self.fb.lock().unwrap();
        (fb.gpu_output_frame, fb.latest_generation())
    }

    /// The engine's recent notes on the picture, oldest first: each
    /// `gen N <what was shown, how, in which state>`.
    pub fn trail(&self) -> Vec<String> {
        self.fb.lock().unwrap().trail()
    }

    /// The scope texture the engine last published for `mode`.
    pub fn scope_texture(&self, mode: u8) -> Option<klipp_engine::texture::ScopeTextureEntry> {
        self.fb.lock().unwrap().scope_textures.get(&mode).cloned()
    }

    /// The picture's light in cd/m² at `(nx, ny)`, while HDR monitoring
    /// is on.
    pub fn nits_at(&self, nx: f32, ny: f32) -> Option<f32> {
        self.fb.lock().unwrap().nits_at(nx, ny)
    }

    /// `nits_at`, waited for up to `limit`: the light map is published
    /// with the frame monitoring renders, and on a loaded machine the
    /// frame that settled can be one from before it.
    pub fn wait_nits_at(&self, nx: f32, ny: f32, limit: Duration) -> Option<f32> {
        let t0 = Instant::now();
        loop {
            if let Some(n) = self.nits_at(nx, ny) {
                return Some(n);
            }
            if t0.elapsed() >= limit {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn perf(&self) -> Vec<PerfSample> {
        self.perf.lock().unwrap().iter_chronological().cloned().collect()
    }
}

impl Drop for Parked {
    fn drop(&mut self) {
        let _ = self.cmd.send(AppCommand::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
            // A decode thread still in ffmpeg's teardown at exit crashes the
            // process (CUDA's context destroyed under it).
            klipp_engine::video_decoder::wait_for_decode_threads(std::time::Duration::from_secs(10));
        }
    }
}

// ─── The PQ band fixture ──────────────────────────────────────────────
//
// Shared because two tests measure it — HDR monitoring's readout and the
// HDR reference output — and a second copy of the band levels is exactly
// the mistake the cache-key comment below was written about.

/// The band fixture's frame size.
pub const BANDS_W: u32 = 640;
pub const BANDS_H: u32 = 360;
/// Four vertical bands of known luminance, in cd/m².
pub const BAND_NITS: [f64; 4] = [5.0, 26.0, 203.0, 1000.0];
/// A fifth band, saturated: BT.2020 red at this level, green and blue at
/// the bottom. Its brightest channel is the red one — the same thing the
/// encoder measures, once the picture is in BT.2020. In a BT.709 project
/// the working values of this colour are another basis entirely (and its
/// green and blue are negative), so measuring there reads a different
/// number.
pub const SATURATED_RED_NITS: f64 = 600.0;

pub fn pq_signal(nits: f64) -> f64 {
    let (m1, m2) = (2610.0 / 16384.0, 2523.0 / 4096.0 * 128.0);
    let (c1, c2, c3) = (3424.0 / 4096.0, 2413.0 / 4096.0 * 32.0, 2392.0 / 4096.0 * 32.0);
    let y = (nits / 10000.0f64).powf(m1);
    ((c1 + c2 * y) / (1.0 + c3 * y)).powf(m2)
}

pub fn pq_nits(signal: f64) -> f64 {
    let (m1, m2) = (2610.0 / 16384.0, 2523.0 / 4096.0 * 128.0);
    let (c1, c2, c3) = (3424.0 / 4096.0, 2413.0 / 4096.0 * 32.0, 2392.0 / 4096.0 * 32.0);
    let p = signal.powf(1.0 / m2);
    10000.0 * ((p - c1).max(0.0) / (c2 - c3 * p)).powf(1.0 / m1)
}

/// A lossless 10-bit PQ clip of four neutral bands at [`BAND_NITS`], plus a
/// saturated fifth at [`SATURATED_RED_NITS`].
///
/// Cached between runs under a name that carries the codes it holds: keyed
/// on the frame size alone, a file written for other band levels stayed in
/// /tmp and every later run measured it instead — bands 0 and 1 happened
/// to match, and band 2 read 1004 cd/m² where the test wanted 203.
pub fn pq_bands() -> Option<PathBuf> {
    if std::process::Command::new("ffmpeg").arg("-version").output().is_err() {
        return None;
    }
    let y10 = |signal: f64| (64.0 + signal * 876.0).round() as u16;
    let c10 = |v: f64| (512.0 + v * 896.0).round().clamp(4.0, 1019.0) as u16;
    // Four neutral bands, then BT.2020 R'G'B' → Y'CbCr (BT.2020 NCL,
    // limited range) for the saturated one.
    let (kr, kb) = (0.2627f64, 0.0593f64);
    let (r, g, b) = (pq_signal(SATURATED_RED_NITS), pq_signal(1.0), pq_signal(1.0));
    let luma = kr * r + (1.0 - kr - kb) * g + kb * b;
    let saturated = [y10(luma), c10((b - luma) / (2.0 * (1.0 - kb))), c10((r - luma) / (2.0 * (1.0 - kr)))];
    let codes: Vec<[u16; 3]> = BAND_NITS
        .iter()
        .map(|n| [y10(pq_signal(*n)), 512, 512])
        .chain(std::iter::once(saturated))
        .collect();
    // Every code in the name, chroma included: the saturated band differs
    // from a neutral one in chroma alone.
    let name: String = codes.iter().flatten().map(|c| format!("-{c}")).collect();
    let out = std::env::temp_dir().join(format!("klipp-hdr-monitor-bands-{BANDS_W}x{BANDS_H}{name}.mkv"));
    if out.exists() {
        return Some(out);
    }
    let raw = unshared(&out, "yuv");
    let mut bytes = Vec::new();
    // Two seconds: the harness parks ten frames in.
    for _ in 0..50 {
        for plane in 0..3 {
            let pw = if plane == 0 { BANDS_W } else { BANDS_W / 2 };
            let ph = if plane == 0 { BANDS_H } else { BANDS_H / 2 };
            for _ in 0..ph {
                for x in 0..pw {
                    let band = (x * BANDS_W / pw * 5 / BANDS_W) as usize;
                    bytes.extend_from_slice(&codes[band.min(4)][plane].to_le_bytes());
                }
            }
        }
    }
    std::fs::write(&raw, bytes).ok()?;
    let size = format!("{BANDS_W}x{BANDS_H}");
    // Written beside the cache name and moved into place, so a run that
    // dies mid-encode leaves no half file for the next one to measure.
    let partial = unshared(&out, "mkv");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "yuv420p10le", "-s", &size, "-r", "25"])
        .args(["-i", raw.to_str()?, "-vf", &format!("setparams=range=tv:{}", PQ_TAGS)])
        .args(["-c:v", "ffv1"])
        .arg(&partial)
        .status()
        .ok()?
        .success();
    let _ = std::fs::remove_file(&raw);
    if !ok {
        let _ = std::fs::remove_file(&partial);
        return None;
    }
    std::fs::rename(&partial, &out).ok()?;
    Some(out)
}

// ─── The grey ramp fixture ────────────────────────────────────────────
//
// `RAMP_N` solid grey frames whose luma climbs from black to white in
// equal steps, so frame `i` is the `i`-th brightest and a walk or a strip
// can be checked frame by frame. Shared by the sequential-decoder and
// filmstrip tests.

use std::path::Path;
use std::process::Command;

/// NVDEC declines anything under 144 pixels a side and falls back to
/// software, which would quietly leave the hardware path unwalked.
pub const RAMP_W: u32 = 160;
pub const RAMP_H: u32 = 160;
/// Enough frames for a B-frame pyramid to reorder across, and for the
/// decoder to hold several at EOF.
pub const RAMP_N: usize = 40;

/// The source: `N` solid grey frames whose luma climbs from black to white
/// in equal steps, so frame `i` is the `i`-th brightest. Written as raw
/// 10-bit 4:2:0 and encoded with `codec` into `ext`. `None` without ffmpeg;
/// an encode that fails with ffmpeg present is a broken test, and panics.
pub fn ramp(name: &str, codec: &[&str], ext: &str) -> Option<PathBuf> {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        return None;
    }
    let out = std::env::temp_dir().join(format!("klipp-seq-ramp-{name}-{RAMP_W}x{RAMP_H}x{RAMP_N}.{ext}"));
    if out.exists() {
        return Some(out);
    }
    let mut bytes = Vec::new();
    for i in 0..RAMP_N {
        // Limited range, 64..=940, evenly spaced.
        let y = 64 + (i as u32 * 876 / (RAMP_N as u32 - 1)) as u16;
        let (pw, ph) = (RAMP_W as usize / 2, RAMP_H as usize / 2);
        for _ in 0..(RAMP_W * RAMP_H) {
            bytes.extend_from_slice(&y.to_le_bytes());
        }
        for _ in 0..(2 * pw * ph) {
            bytes.extend_from_slice(&512u16.to_le_bytes());
        }
    }
    let raw = unshared(&out, "yuv");
    std::fs::write(&raw, bytes).ok()?;
    let partial = unshared(&out, ext);
    let ok = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "yuv420p10le"])
        .args(["-s", &format!("{RAMP_W}x{RAMP_H}"), "-r", "25", "-i"])
        .arg(&raw)
        .args(["-vf", "setparams=range=tv:colorspace=bt709:color_primaries=bt709:color_trc=bt709"])
        .args(codec)
        .arg(&partial)
        .status()
        .ok()?
        .success();
    let _ = std::fs::remove_file(&raw);
    if !ok {
        let _ = std::fs::remove_file(&partial);
        panic!("ffmpeg could not encode the {name} ramp");
    }
    std::fs::rename(&partial, &out).ok()?;
    Some(out)
}

/// HEVC with four B-frames and a long GOP, `moov` at the front so a
/// truncated copy still opens.
pub fn hevc_ramp() -> Option<PathBuf> {
    ramp("hevc-faststart", &["-c:v", "libx265", "-preset", "ultrafast", "-x265-params", "bframes=4:keyint=250:log-level=error", "-movflags", "+faststart"], "mp4")
}

/// `secs` seconds of flat grey HEVC at 25 fps, the ramp's size, for a test
/// that has to keep playing: 40 frames run out in 1.6 s, and on a loaded
/// machine before a watch that needs them is done (#1523).
pub fn hevc_long(secs: u32) -> Option<PathBuf> {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        return None;
    }
    let out = std::env::temp_dir().join(format!("klipp-hevc-long-{RAMP_W}x{RAMP_H}x{secs}s.mp4"));
    if out.exists() {
        return Some(out);
    }
    let partial = unshared(&out, "mp4");
    let ok = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("color=c=gray:s={RAMP_W}x{RAMP_H}:r=25:d={secs}"))
        .args(["-pix_fmt", "yuv420p10le", "-vf", "setparams=range=tv:colorspace=bt709:color_primaries=bt709:color_trc=bt709"])
        .args(["-c:v", "libx265", "-preset", "ultrafast", "-x265-params", "bframes=4:keyint=250:log-level=error", "-movflags", "+faststart"])
        .arg(&partial)
        .status()
        .ok()?
        .success();
    if !ok {
        let _ = std::fs::remove_file(&partial);
        panic!("ffmpeg could not encode {secs} s of HEVC");
    }
    std::fs::rename(&partial, &out).ok()?;
    Some(out)
}

/// FFV1 in AVI: every frame is a `00dc` chunk, so a frame's data can be
/// found and damaged without touching the container.
pub fn ffv1_avi_ramp() -> Option<PathBuf> {
    ramp("ffv1-avi", &["-c:v", "ffv1"], "avi")
}

/// `src` with `len` bytes of `fill` written from `at`, as a new file.
pub fn damaged(src: &Path, name: &str, at: usize, len: usize, fill: u8) -> PathBuf {
    let mut bytes = std::fs::read(src).expect("read source");
    let end = (at + len).min(bytes.len());
    for b in &mut bytes[at..end] {
        *b = fill;
    }
    let out = std::env::temp_dir().join(format!("klipp-seq-ramp-{name}-{}", src.file_name().unwrap().to_string_lossy()));
    std::fs::write(&out, bytes).expect("write damaged copy");
    out
}

/// `src` cut exactly where its `n`-th video packet ends (the demuxer says
/// where), so a demuxer meets a clean end of file rather than a torn
/// sample. Written beside the source under `name`.
pub fn cut_after_packet(src: &Path, n: usize, name: &str) -> PathBuf {
    let mut input = ffmpeg_next::format::input(src).expect("open the source with ffmpeg");
    let video = input.streams().best(ffmpeg_next::media::Type::Video).expect("a video stream").index();
    let mut ends = Vec::new();
    for (stream, packet) in input.packets() {
        if stream.index() == video {
            ends.push((packet.position() as usize) + packet.size());
        }
    }
    let bytes = std::fs::read(src).unwrap();
    let out = std::env::temp_dir().join(format!("klipp-seq-ramp-{name}.{}", src.extension().unwrap().to_string_lossy()));
    std::fs::write(&out, &bytes[..ends[n]]).unwrap();
    out
}

/// The data of every `00dc` chunk in an AVI's `movi` list overwritten, so
/// no frame in it can be decoded while the container reads fine.
pub fn every_frame_damaged(src: &Path, name: &str) -> PathBuf {
    let mut bytes = std::fs::read(src).unwrap();
    let movi = bytes.windows(4).position(|w| w == b"movi").expect("an AVI movi list");
    let chunks: Vec<usize> = bytes[movi..].windows(4).enumerate().filter(|(_, w)| *w == b"00dc").map(|(i, _)| movi + i).collect();
    for chunk in chunks {
        let size = u32::from_le_bytes(bytes[chunk + 4..chunk + 8].try_into().unwrap()) as usize;
        let end = (chunk + 8 + size).min(bytes.len());
        for b in &mut bytes[chunk + 8..end] {
            *b = 0xff;
        }
    }
    let out = std::env::temp_dir().join(format!("klipp-seq-ramp-{name}-{}", src.file_name().unwrap().to_string_lossy()));
    std::fs::write(&out, bytes).unwrap();
    out
}

/// The repository's C OFX fixture (`org.klipp.fixture.channels`, an 8-bit
/// channel swap) built into a bundle under a scratch folder; the folder,
/// for `OFX_PLUGIN_PATH`, or `None` without a C compiler.
pub fn ofx_fixture_bundle() -> Option<std::path::PathBuf> {
    let root = std::env::temp_dir().join(format!("klipp-probe-ofx-{}", std::process::id()));
    let binary = root.join("KlippFixture.ofx.bundle/Contents/Linux-x86-64/KlippFixture.ofx");
    std::fs::create_dir_all(binary.parent()?).ok()?;
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../klipp-plugin/tests/fixtures/ofx/fixture.c");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
    let ok = std::process::Command::new(cc)
        .args(["-std=c11", "-O2", "-shared", "-fPIC"])
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .status()
        .ok()?
        .success();
    ok.then_some(root)
}

/// What else is keeping the GPU busy, for a timing test to print before
/// it measures and to name when it falls short: the GPU's power state and
/// clock, and each other process with a share of its shader time (from
/// `nvidia-smi`). A test sharing the GPU with a compositor or another app
/// reads slower without any change of its own; said this way, a
/// contended run isn't mistaken for a slowdown.
#[allow(dead_code)]
pub fn gpu_others() -> String {
    let run = |args: &[&str]| std::process::Command::new("nvidia-smi").args(args).output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    let Some(state) = run(&["--query-gpu=pstate,clocks.sm", "--format=csv,noheader"]) else { return "GPU load unknown (no nvidia-smi)".into() };
    let busy = run(&["pmon", "-c", "2", "-s", "u"]).map(|t| busy_on_gpu(&t, std::process::id())).unwrap_or_default();
    let share = |sm: u32, dec: u32| match (sm, dec) {
        (_, 0) => format!("{sm}%"),
        (0, _) => format!("{dec}% decode"),
        _ => format!("{sm}%, {dec}% decode"),
    };
    let others = if busy.is_empty() { "nothing else busy".to_string() } else { busy.iter().map(|(name, sm, dec)| format!("{name} {}", share(*sm, *dec))).collect::<Vec<_>>().join(", ") };
    format!("GPU {}; others on it: {others}", state.trim())
}

/// The processes `nvidia-smi pmon -s u` shows using shader time or the
/// video decoder, but `me`, each at its highest shader and decode share
/// over the samples, busiest first. The decoder is apart from the
/// shaders: a reversed long-GOP clip is held up by another process's
/// decoding with the shaders idle.
#[allow(dead_code)]
pub fn busy_on_gpu(pmon: &str, me: u32) -> Vec<(String, u32, u32)> {
    let pct = |f: &[&str], i: usize| f.get(i).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
    let mut seen: Vec<(String, u32, u32)> = Vec::new();
    for line in pmon.lines().filter(|l| !l.trim_start().starts_with('#')) {
        let f: Vec<&str> = line.split_whitespace().collect();
        let (Some(pid), Some(name)) = (f.get(1).and_then(|p| p.parse::<u32>().ok()), f.last()) else { continue };
        // Columns: gpu pid type sm mem enc dec jpg ofa command.
        let (sm, dec) = (pct(&f, 3), pct(&f, 6));
        if pid == me || (sm == 0 && dec == 0) {
            continue;
        }
        match seen.iter_mut().find(|(n, _, _)| n == name) {
            Some((_, s, d)) => (*s, *d) = ((*s).max(sm), (*d).max(dec)),
            None => seen.push((name.to_string(), sm, dec)),
        }
    }
    seen.sort_by_key(|s| std::cmp::Reverse(s.1 + s.2));
    seen
}
