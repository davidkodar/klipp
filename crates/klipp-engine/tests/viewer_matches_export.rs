//! The viewer shows what export writes, for a timeline with every kind of
//! layer the two used to disagree on.
//!
//! The real `PlaybackEngine` runs headless on a small timeline — a graded
//! clip dissolving into another on V1, two overlays above it at different
//! scales and opacities — and is parked at frames across it. Each viewer
//! frame is compared, in numbers, with the same frame of the timeline
//! exported through the real `ExportJob` and decoded the way a player does.
//!
//! Two of the sources change colour every frame, so a stream showing the
//! wrong frame of its media (a stale overlay after stepping back, a
//! transition side at the wrong point of its handle) shows up as a colour
//! difference, not just a position one. Sources come from the ffmpeg CLI;
//! the test skips without it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_effects::BuiltinEffect;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_render::*;
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind, Transition};

const W: u32 = 640;
const H: u32 = 360;
const FPS: i64 = 25;
/// Timeline length, frames.
const LEN: i64 = 200;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("klipp-viewer-export-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name)
}

fn ffmpeg_ok() -> bool {
    std::process::Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

/// A flat or per-frame-changing RGB source: `r`, `g`, `b` are lavfi `geq`
/// expressions in N, the frame number.
fn rgb_source(name: &str, r: &str, g: &str, b: &str) -> PathBuf {
    let out = tmp(name);
    let filter = format!(
        "color=c=black:s={W}x{H}:r={FPS}:d=10,format=rgb24,geq=r='{r}':g='{g}':b='{b}',\
         setparams=range=tv:colorspace=bt709:color_primaries=bt709:color_trc=bt709"
    );
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", &filter])
        .args(["-vf", "scale=out_color_matrix=bt709:out_range=tv", "-c:v", "ffv1", "-pix_fmt", "yuv444p"])
        .args(["-color_range", "tv", "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"])
        .arg(&out)
        .status()
        .unwrap()
        .success();
    assert!(ok, "ffmpeg could not make {name}");
    out
}

fn tc(frame: i64) -> RationalTime {
    RationalTime::from_frames(frame, FPS as u32)
}

fn media_clip(id: MediaId, from: i64, to: i64, source_in: i64) -> Clip {
    let mut c = Clip { kind: ClipKind::Media { media_id: id }, ..Clip::gap(tc(from), tc(to)) };
    c.source_in = tc(source_in);
    c.source_out = tc(source_in + to - from);
    c
}

fn scaled(mut c: Clip, scale: f32, x: f32, y: f32) -> Clip {
    c.builtin_effects.push(BuiltinEffect::Transform {
        position_x: x, position_y: y, scale_x: scale, scale_y: scale, rotation: 0.0,
        anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None,
    });
    c
}

fn exposure(stops: f32) -> klipp_nodes::NodeGraph {
    use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams, NodePos};
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

struct Timeline {
    seq: Sequence,
    files: HashMap<MediaId, PathBuf>,
}

/// V1: a (flat, one stop up) dissolving over one second into b (changing
/// every frame, entering from its handle). V2: c (changing every frame)
/// at 60% scale and half opacity. V3: d (flat) at 30%, over c's centre.
fn timeline() -> Timeline {
    let a = MediaId::new();
    let b = MediaId::new();
    let c = MediaId::new();
    let d = MediaId::new();
    let files = HashMap::from([
        (a, rgb_source("a.mkv", "176", "64", "64")),
        (b, rgb_source("b.mkv", "40", "mod(N*5,200)+30", "180")),
        (c, rgb_source("c.mkv", "mod(N*7,200)+30", "160", "60")),
        (d, rgb_source("d.mkv", "210", "210", "210")),
    ]);
    let mut clip_a = media_clip(a, 0, 100, 0);
    clip_a.node_graph = exposure(1.0);
    let clip_b = media_clip(b, 100, LEN, 25);
    let mut clip_c = scaled(media_clip(c, 25, 175, 10), 0.6, 0.0, 0.0);
    clip_c.builtin_effects.push(BuiltinEffect::Opacity { value: 0.5 });
    let clip_d = scaled(media_clip(d, 50, 150, 0), 0.3, 0.0, 0.0);
    let dissolve = Transition {
        id: klipp_core::TransitionId::new(),
        kind: klipp_core::TransitionKind::CrossDissolve,
        duration: tc(25),
        clip_a_id: clip_a.id,
        clip_b_id: clip_b.id,
        user_params: Default::default(),
        alignment: klipp_core::TransitionAlignment::Center,
    };
    let mut seq = Sequence::new_default();
    seq.fps = Fps::new(FPS as u32, 1);
    seq.resolution = Resolution { width: W, height: H };
    let track = |name: &str, clips: Vec<Clip>| {
        let mut t = Track::new(name, TrackKind::Video);
        t.clips = clips;
        t
    };
    let mut v1 = track("V1", vec![clip_a, clip_b]);
    v1.transitions.push(dissolve);
    seq.tracks = vec![track("V3", vec![clip_d]), track("V2", vec![clip_c]), v1];
    Timeline { seq, files }
}

/// The timeline exported through the real `ExportJob`, H.264 near-lossless.
fn export(t: &Timeline, out: &Path) {
    let mut video = VideoTimeline::default();
    video.sequence = t.seq.clone();
    video.media = t.files.iter().map(|(id, p)| (*id, MediaFile { path: p.clone(), log_format: None })).collect();
    let snapshot = SequenceSnapshot {
        fps: FPS as f64, fps_num: FPS as u32, fps_den: 1, width: W, height: H, sample_rate: 48000,
        duration_frames: LEN as u64, export_range: None, video,
        audio_tracks: vec![], global_lut_path: None, subtitle_tracks: vec![], chapters: vec![],
        working_colorspace: WorkingColorspace::Rec709Linear,
    };
    let settings = ExportSettings {
        output_path: out.to_path_buf(), format: ExportFormat::Mp4, video_codec: ExportVideoCodec::H264,
        resolution: None, fps: None, crf: 1, preset: EncoderPreset::Fast, audio_codec: ExportAudioCodec::Copy,
        audio_bitrate_kbps: None, video_bitrate_kbps: None, rate_control: RateControl::Crf,
        burn_in_subtitles: false, embed_subtitles: false, target_lufs: None, embed_chapters: false,
        bit_depth: 0, hdr_mode: HdrMode::Sdr, tone_map_op: ToneMapOp::Hable, odt: OdtChoice::MatchViewer,
        mastering_peak_nits: klipp_core::DEFAULT_MASTERING_PEAK_NITS,
    };
    ExportJob {
        id: uuid::Uuid::new_v4(), output_path: out.to_path_buf(), settings, snapshot,
        cancel: Arc::new(AtomicBool::new(false)), progress: Arc::new(AtomicU64::new(0)),
        hw_info: Default::default(), render_stage: Default::default(), loudness_result: Default::default(),
        total_frames: LEN as u64,
    }
    .run()
    .unwrap();
}

/// Frame `n` of an exported file as RGB24, decoded as BT.709 limited.
fn exported_frame(path: &Path, n: i64) -> Vec<u8> {
    let vf = format!("select=eq(n\\,{n}),scale=in_color_matrix=bt709:in_range=tv:flags=accurate_rnd+full_chroma_int,format=rgb24");
    let out = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"]).arg(path)
        .args(["-vf", &vf, "-frames:v", "1", "-f", "rawvideo", "-"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.stdout.len(), (W * H * 3) as usize, "frame {n} missing from the export");
    out.stdout
}

/// The engine, headless, on the timeline.
struct Viewer {
    cmd: crossbeam_channel::Sender<AppCommand>,
    fb: Arc<Mutex<FrameBuffer>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Viewer {
    fn start(t: &Timeline) -> Self {
        let (cmd, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<klipp_core::AppEvent>();
        let fb = Arc::new(Mutex::new(FrameBuffer::new(W, H)));
        let mut pool = MediaPool::default();
        for (id, path) in &t.files {
            pool.add(MediaItem {
                id: *id,
                path: path.clone(),
                kind: klipp_media::MediaItemKind::File,
                resolution: Resolution { width: W, height: H },
                ..MediaItem::new_synthetic_title(*id, path.display().to_string(), tc(250), Fps::new(FPS as u32, 1))
            });
        }
        let engine = PlaybackEngine::new(
            cmd_rx,
            ev_tx,
            Arc::clone(&fb),
            Arc::new(Mutex::new(FrameBuffer::new(W, H))),
            Arc::new(RwLock::new(t.seq.clone())),
            Arc::new(RwLock::new(pool)),
            Box::new(|| {}),
            Arc::new(Mutex::new(None)),
            Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
        );
        let thread = std::thread::spawn(move || engine.run());
        // Drain events: the engine must not block on a full channel. Which
        // frame is on screen comes from the frame buffer, with the frame.
        std::thread::spawn(move || while ev_rx.recv().is_ok() {});
        Self { cmd, fb, thread: Some(thread) }
    }

    /// Wait until no new frame has arrived for 600 ms, then return the
    /// latest as RGB24.
    fn settled_frame(&self) -> Vec<u8> {
        let generation = || self.fb.lock().unwrap().latest_generation();
        let start = Instant::now();
        let mut last = generation();
        let mut since = Instant::now();
        while since.elapsed() < Duration::from_millis(600) && start.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
            let g = generation();
            if g != last {
                last = g;
                since = Instant::now();
            }
        }
        let fb = self.fb.lock().unwrap();
        let (rgba, w, h, _) = fb.latest_frame();
        assert_eq!((w, h), (W, H), "viewer frame size");
        rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect()
    }

    /// [`Self::settled_frame`], with the frame the viewer says it is
    /// showing. Taken under one lock: during playback the playhead moves
    /// on while a frame is still on screen, so asking afterwards can name
    /// a different frame — under load, a whole frame out.
    fn settled_frame_at(&self) -> (Vec<u8>, i64) {
        let rgb = self.settled_frame();
        let fb = self.fb.lock().unwrap();
        let at = fb.latest_presented().expect("the engine names the frame it presents");
        (rgb, at.value * FPS / at.rate as i64)
    }

    /// Park the playhead at frame `n` and return the frame the viewer
    /// shows once it has settled, as RGB24.
    /// (A paused frame waiting on its streams re-renders when they arrive,
    /// hence waiting for the frames to stop, not for the first.)
    fn frame_at(&self, n: i64) -> Vec<u8> {
        self.cmd.send(AppCommand::SetPlayhead(tc(n))).unwrap();
        self.settled_frame()
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        let _ = self.cmd.send(AppCommand::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Median of a 5×5 block, per channel.
fn px(rgb: &[u8], x: u32, y: u32) -> [i32; 3] {
    let mut out = [0; 3];
    for (c, o) in out.iter_mut().enumerate() {
        let mut v: Vec<i32> = (0..25)
            .map(|k| {
                let (xx, yy) = (x + k % 5 - 2, y + k / 5 - 2);
                rgb[((yy * W + xx) * 3) as usize + c] as i32
            })
            .collect();
        v.sort();
        *o = v[12];
    }
    out
}

/// Points: the corner (V1 alone), inside c but outside d, the centre (d
/// over c over V1).
const POINTS: [(&str, u32, u32); 3] = [("corner", 30, 30), ("c only", W / 2 - 150, H / 2), ("centre", W / 2, H / 2)];

/// The worst channel difference between two frames over `POINTS`.
fn compare(label: &str, v: &[u8], e: &[u8]) -> i32 {
    let mut worst = 0;
    for (name, x, y) in POINTS {
        let (p, q) = (px(v, x, y), px(e, x, y));
        let d = (0..3).map(|c| (p[c] - q[c]).abs()).max().unwrap();
        eprintln!("{label} {name:7}: viewer {p:?} export {q:?} diff {d}");
        worst = worst.max(d);
    }
    worst
}

/// The export, made once for both tests.
fn exported(t: &Timeline) -> PathBuf {
    let out = tmp(&format!("export-{}.mp4", uuid::Uuid::new_v4()));
    export(t, &out);
    out
}

#[test]
fn the_viewer_shows_the_exported_frame() {
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let t = timeline();
    let out = exported(&t);
    let viewer = Viewer::start(&t);

    // Frames: a alone, a under c, a under c and d, the dissolve before and
    // after its cut, b under both, b under c; then back, to catch a stream
    // left on a later frame.
    let frames = [10, 35, 60, 94, 106, 120, 160, 60, 35];
    let mut worst = 0;
    for n in frames {
        let (v, e) = (viewer.frame_at(n), exported_frame(&out, n));
        worst = worst.max(compare(&format!("frame {n:3}"), &v, &e));
    }
    eprintln!("viewer vs export, parked: worst {worst} levels");
    assert!(worst <= 3, "the viewer differs from the export by {worst} levels");
}

/// Playing through the cut and the dissolve — the primary decoder moving
/// on to b while a's side keeps running into its handle on a stream — the
/// frame on screen when playback stops is the export's frame at that
/// playhead.
#[test]
fn playing_through_a_dissolve_shows_the_exported_frames() {
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let t = timeline();
    let out = exported(&t);
    let viewer = Viewer::start(&t);
    let mut worst = 0;
    for (from, play_ms) in [(80, 800), (90, 700), (70, 1500)] {
        viewer.frame_at(from);
        viewer.cmd.send(AppCommand::Play).unwrap();
        std::thread::sleep(Duration::from_millis(play_ms));
        viewer.cmd.send(AppCommand::Pause).unwrap();
        let (v, n) = viewer.settled_frame_at();
        assert!(n > from, "playback did not move: from {from}, at {n}");
        worst = worst.max(compare(&format!("played {from}->{n:3}"), &v, &exported_frame(&out, n)));
    }
    eprintln!("viewer vs export, after playing: worst {worst} levels");
    assert!(worst <= 3, "the viewer differs from the export by {worst} levels");
}
