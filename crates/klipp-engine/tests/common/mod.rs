//! The headless engine parked on one graded clip, with a slider dragged
//! the way the UI drags one — for the tests about paused grading.

#![allow(dead_code)]

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
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", &format!("testsrc2=s={width}x{height}:r={rate}:d=2")])
        .args(["-vf", &format!("format=yuv420p10le,setparams=range=tv:{tags}")])
        .args(codec)
        .arg(&out)
        .status()
        .ok()?
        .success();
    ok.then_some(out)
}

pub const PQ_TAGS: &str = "colorspace=bt2020nc:color_primaries=bt2020:color_trc=smpte2084";
pub const SDR_TAGS: &str = "colorspace=bt709:color_primaries=bt709:color_trc=bt709";

fn exposure_graph(stops: f32) -> NodeGraph {
    let node = |kind, params| Node {
        id: NodeId::new_v4(), kind, label: format!("{kind:?}"), enabled: true, params,
        keyframes: Default::default(), output_link: None, input_links: vec![],
    };
    let mut p = NodeParams::default();
    p.exposure = Some(stops);
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

    /// [`Self::start`] on a timeline at `fps`, for a source at that rate.
    pub fn start_at(src: PathBuf, width: u32, height: u32, fps: Fps, graded: bool) -> Self {
        let tc = |f: i64| frame_tc(f, fps);
        let id = MediaId::new();
        let mut clip = Clip { kind: ClipKind::Media { media_id: id }, ..Clip::gap(tc(0), tc(50)) };
        clip.source_in = tc(0);
        clip.source_out = tc(50);
        clip.node_graph = exposure_graph(if graded { 0.5 } else { 0.0 });
        let (clip_id, source_node, exposure_node) = (clip.id, clip.node_graph.nodes[0].id, clip.node_graph.nodes[1].id);
        let mut seq = Sequence::new_default();
        seq.fps = fps;
        seq.resolution = Resolution { width, height };
        let mut t = Track::new("V1", TrackKind::Video);
        t.clips = vec![clip];
        seq.tracks = vec![t];

        let mut pool = MediaPool::default();
        pool.add(MediaItem {
            id,
            path: src.clone(),
            kind: klipp_media::MediaItemKind::File,
            resolution: Resolution { width, height },
            ..MediaItem::new_synthetic_title(id, src.display().to_string(), tc(50), fps)
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
        let engine = PlaybackEngine::new(
            cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(width, height))),
            Arc::new(RwLock::new(seq)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
            Arc::new(Mutex::new(None)), Arc::clone(&perf),
        );
        let params = engine.param_change_queue();
        let thread = Some(std::thread::spawn(move || engine.run()));
        let parked = Self { cmd, fb, playhead, fps, perf, params, thread, clip_id, source_node, exposure_node };

        parked.send(AppCommand::SetPlayhead(tc(10)));
        let g = parked.generation();
        parked.wait_new(g, Duration::from_secs(20)).expect("no first frame");
        std::thread::sleep(Duration::from_millis(1500));
        parked
    }

    pub fn send(&self, c: AppCommand) {
        self.cmd.send(c).unwrap();
    }

    /// The playhead the engine last reported, as a frame number.
    pub fn frame_number(&self) -> Option<f64> {
        let tc = (*self.playhead.lock().unwrap())?;
        Some(tc.value as f64 * self.fps.num as f64 / (tc.rate as f64 * self.fps.den as f64))
    }

    pub fn generation(&self) -> u64 {
        self.fb.lock().unwrap().latest_generation()
    }

    /// Wait for a frame newer than `since`; how long it took.
    pub fn wait_new(&self, since: u64, limit: Duration) -> Option<Duration> {
        let t0 = Instant::now();
        while t0.elapsed() < limit {
            if self.generation() != since {
                return Some(t0.elapsed());
            }
            std::thread::sleep(Duration::from_micros(200));
        }
        None
    }

    pub fn begin_drag(&self) {
        self.send(AppCommand::BeginNodeParamEdit { clip_id: self.clip_id });
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
        self.wait_new(g, Duration::from_secs(5)).map(|_| t0.elapsed().as_secs_f64() * 1000.0)
    }

    /// Let go of the slider and wait for the regrade that follows, ms.
    pub fn end_drag(&self) -> Option<f64> {
        let g = self.generation();
        let t0 = Instant::now();
        self.send(AppCommand::EndNodeParamEdit { clip_id: self.clip_id });
        self.wait_new(g, Duration::from_secs(5)).map(|_| t0.elapsed().as_secs_f64() * 1000.0)
    }

    /// The frame on screen, RGBA.
    pub fn frame(&self) -> Vec<u8> {
        self.fb.lock().unwrap().latest_frame().0.to_vec()
    }

    /// The picture's light in cd/m² at `(nx, ny)`, while HDR monitoring
    /// is on.
    pub fn nits_at(&self, nx: f32, ny: f32) -> Option<f32> {
        self.fb.lock().unwrap().nits_at(nx, ny)
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
        }
    }
}
