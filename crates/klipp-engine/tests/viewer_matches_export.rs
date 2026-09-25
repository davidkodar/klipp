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
//!
//! # Reading a failure
//!
//! Each source varies a different channel, so *which* channel is off says
//! which half of the pipeline is at fault. Worth a minute before reaching
//! for a debugger:
//!
//! * `b` — the dissolve's incoming side — varies **green** and is flat in
//!   red and blue. Green off with red and blue exact means the mix ratio and
//!   the outgoing side are right, and only b's source position is wrong: a
//!   transition side at the wrong point of its handle. That is what an 88 -
//!   level green-only failure was, and it was a stale frame accepted as the
//!   landing of a seek (fixed in c5a7467f — see `tests/seek_epoch.rs`).
//! * `c` — the V2 overlay — varies **red**. Red off at "c only" or "centre"
//!   but the corner clean means c's stream is on the wrong frame, with V1
//!   fine.
//! * `a` and `d` are flat in every channel. All three channels off together
//!   points at the mix, the grade, or the display transform rather than at
//!   any stream's position — a stale stream cannot move a channel its source
//!   holds constant.
//!
//! The three [`POINTS`] narrow it further: the corner sees V1 alone, so a
//! difference there is the dissolve or the grade; the centre sees d over c
//! over V1, so a difference only there is compositing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
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

/// Generated fixtures live in a directory named for this test process,
/// because these binaries run in parallel and a shared filename means
/// one run reads a file another is still writing — a black frame that
/// looks like a pipeline bug. `scratch_file` also removes the
/// directories that finished runs left behind; before it did, a day of
/// full-suite runs left 313 of them holding 5.4 GB on the tmpfs.
fn tmp(name: &str) -> PathBuf {
    klipp_testing::scratch::scratch_file("klipp-viewer-export", name)
}

fn ffmpeg_ok() -> bool {
    std::process::Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

/// A flat or per-frame-changing RGB source: `r`, `g`, `b` are lavfi `geq`
/// expressions in N, the frame number.
///
/// The file name carries a uuid because cargo runs these tests in
/// parallel and each one builds its own timeline: on a shared name, one
/// test reads the file while another's ffmpeg is still writing it, and
/// gets a truncated source — which shows up as a black export and looks
/// like a pipeline bug rather than a test one.
fn rgb_source(name: &str, r: &str, g: &str, b: &str) -> PathBuf {
    rgb_source_sized(name, (W, H), r, g, b)
}

/// [`rgb_source`], coded at another size.
fn rgb_source_sized(name: &str, (w, h): (u32, u32), r: &str, g: &str, b: &str) -> PathBuf {
    let out = tmp(&format!("{}-{}", uuid::Uuid::new_v4(), name));
    let filter = format!(
        "color=c=black:s={w}x{h}:r={FPS}:d=10,format=rgb24,geq=r='{r}':g='{g}':b='{b}',\
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
    let p = NodeParams {
        exposure: Some(stops),
        ..Default::default()
    };
    let nodes = vec![node(NodeKind::Source, NodeParams::default()), node(NodeKind::Exposure, p), node(NodeKind::Output, NodeParams::default())];
    let edges = (0..2).map(|i| Edge { from_node: nodes[i].id, from_port: 0, to_node: nodes[i + 1].id, to_port: 0 }).collect();
    let node_positions = nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 100.0, y: 0.0 })).collect();
    NodeGraph { output_node_id: nodes[2].id, nodes, edges, node_positions, working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

#[derive(Clone)]
struct Timeline {
    seq: Sequence,
    files: HashMap<MediaId, PathBuf>,
    /// Media not coded at the sequence's size, or turned (a phone's
    /// rotation matrix): the coded size and the turn. The rest are coded
    /// at the sequence's size, upright.
    coded: HashMap<MediaId, (u32, u32, i32)>,
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
    Timeline { seq, files, coded: HashMap::new() }
}

/// The timeline exported through the real `ExportJob`, H.264 near-lossless.
fn export(t: &Timeline, out: &Path) {
    let video = VideoTimeline {
        sequence: t.seq.clone(),
        media: t
            .files
            .iter()
            .map(|(id, p)| {
                let source_rotation = t.coded.get(id).map_or(0, |c| c.2);
                (*id, MediaFile { path: p.clone(), log_format: None, source_rotation })
            })
            .collect(),
        ..Default::default()
    };
    let snapshot = SequenceSnapshot {
        fps: FPS as f64, fps_num: FPS as u32, fps_den: 1, width: W, height: H, sample_rate: 48000,
        duration_frames: LEN as u64, export_range: None, video,
        audio_tracks: vec![], global_lut_path: None, subtitle_tracks: vec![], chapters: vec![],
        pan_law: Default::default(),
        working_colorspace: WorkingColorspace::Rec709Linear,
    };
    let settings = ExportSettings {
        pan_law: Default::default(),
        output_path: out.to_path_buf(), format: ExportFormat::Mp4, video_codec: ExportVideoCodec::H264,
        resolution: None, fps: None, crf: 1, preset: EncoderPreset::Fast, audio_codec: ExportAudioCodec::Copy,
        audio_bitrate_kbps: None, video_bitrate_kbps: None, rate_control: RateControl::Crf,
        burn_in_subtitles: false, embed_subtitles: false, target_lufs: None, true_peak_dbtp: None, embed_chapters: false,
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
            // As an import records it: the upright size, and the turn.
            let (cw, ch, source_rotation) = t.coded.get(id).copied().unwrap_or((W, H, 0));
            let (width, height) = if source_rotation.rem_euclid(180) == 90 { (ch, cw) } else { (cw, ch) };
            pool.add(MediaItem {
                id: *id,
                path: path.clone(),
                kind: klipp_media::MediaItemKind::File,
                resolution: Resolution { width, height },
                source_rotation,
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

    /// Wait until no new frame has arrived for 300 ms, then return the
    /// latest as RGB24. A stream that opened later than that would leave a
    /// layer out of the picture, which the comparison catches by a mile.
    fn settled_frame(&self) -> Vec<u8> {
        let generation = || self.fb.lock().unwrap().latest_generation();
        let start = Instant::now();
        let mut last = generation();
        let mut since = Instant::now();
        while since.elapsed() < Duration::from_millis(300) && start.elapsed() < Duration::from_secs(10) {
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
    ///
    /// Two waits, in order. First for frame `n` itself to be the one
    /// presented — the engine names it — because quiet on the frame buffer
    /// before the first frame is not a settled picture, it is no picture:
    /// waiting for quiet alone read the black start-up buffer as frame 10
    /// whenever decoders took longer than the window to open. Then for the
    /// re-renders that follow as the frame's streams arrive to stop.
    fn frame_at(&self, n: i64) -> Vec<u8> {
        self.cmd.send(AppCommand::SetPlayhead(tc(n))).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let presented = self.fb.lock().unwrap().latest_presented().map(|at| at.value * FPS / at.rate as i64);
            if presented == Some(n) {
                break;
            }
            assert!(Instant::now() < deadline, "the viewer never presented frame {n} (last: {presented:?})");
            std::thread::sleep(Duration::from_millis(5));
        }
        self.settled_frame()
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        let _ = self.cmd.send(AppCommand::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        // Joining the engine thread is not enough: its decoders detach
        // their own threads rather than block shutdown, and a detached
        // one still inside ffmpeg's teardown when the test binary calls
        // `exit()` crashes in libc. That took this file down about one
        // full-suite run in five.
        klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));
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

/// The timeline exported to a fresh file.
fn exported(t: &Timeline) -> PathBuf {
    let out = tmp(&format!("export-{}.mp4", uuid::Uuid::new_v4()));
    export(t, &out);
    out
}

/// [`timeline`] and its export, made once for the tests that compare
/// against them. The export is 200 frames through the real `ExportJob`;
/// two tests each made their own, at the same time, on the same GPU.
fn main_fixture() -> &'static (Timeline, PathBuf) {
    static FIXTURE: OnceLock<(Timeline, PathBuf)> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let t = timeline();
        let out = exported(&t);
        (t, out)
    })
}

/// [`secondary_timeline`] and its export, likewise.
fn secondary_fixture() -> &'static (Timeline, PathBuf) {
    static FIXTURE: OnceLock<(Timeline, PathBuf)> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let t = secondary_timeline();
        let out = exported(&t);
        (t, out)
    })
}

#[test]
fn the_viewer_shows_the_exported_frame() {
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let (t, out) = main_fixture();
    let viewer = Viewer::start(t);

    // Frames: a alone, a under c, a under c and d, the dissolve before and
    // after its cut, b under both, b under c; then back, to catch a stream
    // left on a later frame.
    //
    // 185 is b alone past the end of every overlay, and b carries no
    // grade — the one frame here that exercises the export's identity
    // pass-through, where the source frame is seeded into the GPU chain
    // and converted without a graph running. Nothing else in this file
    // reaches it: every other frame is graded, composited, or both.
    let frames = [10, 35, 60, 94, 106, 120, 160, 185, 60, 35];
    let mut worst = 0;
    for n in frames {
        let (v, e) = (viewer.frame_at(n), exported_frame(out, n));
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
    let (t, out) = main_fixture();
    let viewer = Viewer::start(t);
    let mut worst = 0;
    for (from, play_ms) in [(80, 800), (90, 700), (70, 1500)] {
        viewer.frame_at(from);
        viewer.cmd.send(AppCommand::Play).unwrap();
        std::thread::sleep(Duration::from_millis(play_ms));
        viewer.cmd.send(AppCommand::Pause).unwrap();
        let (v, n) = viewer.settled_frame_at();
        assert!(n > from, "playback did not move: from {from}, at {n}");
        worst = worst.max(compare(&format!("played {from}->{n:3}"), &v, &exported_frame(out, n)));
    }
    eprintln!("viewer vs export, after playing: worst {worst} levels");
    assert!(worst <= 3, "the viewer differs from the export by {worst} levels");
}

/// A qualifier-driven secondary: the primary grade, a key over it, a
/// second grade on the branch, and an ApplyMatte recombining the two.
///
/// This is the graph the viewer now carries on the GPU end to end, and
/// the one an export renders by whatever route the render path takes.
/// The two have to agree, and nothing checked that until this test:
/// `ApplyMatte` changed twice — the blend moved from display space into
/// linear light, and the matte stopped being sRGB-decoded on the way in
/// — so a feathered edge produces different numbers than it used to.
/// Fully selected and fully excluded pixels would survive either
/// mistake; the half-selected ones are the point.
fn secondary_graph() -> klipp_nodes::NodeGraph {
    use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams, NodePos};
    let node = |kind, params| Node {
        id: NodeId::new_v4(), kind, label: format!("{kind:?}"), enabled: true, params,
        keyframes: Default::default(), output_link: None, input_links: vec![],
    };
    let primary = NodeParams {
        temperature: Some(-15.0),
        contrast: Some(0.04),
        ..Default::default()
    };
    let keyer = NodeParams {
        qualifier: Some(klipp_core::QualifierSettings {
            enabled: true,
            // The warm end of the gradient below, and nothing else.
            hue_center: 27.0, hue_width: 20.0, hue_soft: 10.0,
            sat_low: 0.20, sat_high: 0.50, sat_soft: 0.05,
            lum_low: 0.10, lum_high: 0.85, lum_soft: 0.04,
            ..Default::default()
        }),
        ..Default::default()
    };
    let branch = NodeParams {
        temperature: Some(-45.0),
        ..Default::default()
    };

    let src = node(NodeKind::Source, NodeParams::default());
    let prim = node(NodeKind::Corrector, primary);
    let qual = node(NodeKind::Qualifier, keyer);
    let cool = node(NodeKind::Corrector, branch);
    let apply = node(NodeKind::ApplyMatte, NodeParams::default());
    let out = node(NodeKind::Output, NodeParams::default());
    let edges = vec![
        Edge { from_node: src.id, from_port: 0, to_node: prim.id, to_port: 0 },
        Edge { from_node: prim.id, from_port: 0, to_node: qual.id, to_port: 0 },
        Edge { from_node: prim.id, from_port: 0, to_node: cool.id, to_port: 0 },
        Edge { from_node: cool.id, from_port: 0, to_node: apply.id, to_port: 0 },
        Edge { from_node: qual.id, from_port: 0, to_node: apply.id, to_port: 1 },
        Edge { from_node: prim.id, from_port: 0, to_node: apply.id, to_port: 2 },
        Edge { from_node: apply.id, from_port: 0, to_node: out.id, to_port: 0 },
    ];
    let output_node_id = out.id;
    let nodes = vec![src, prim, qual, cool, apply, out];
    let node_positions = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id, NodePos { x: i as f32 * 150.0, y: 0.0 }))
        .collect();
    NodeGraph { output_node_id, nodes, edges, node_positions, working_space_override: None, frames: vec![], sticky_notes: vec![] }
}

/// One clip, graded with a secondary, over a source that runs warm on
/// the left and cool on the right: the key takes the left, leaves the
/// right, and feathers across the middle, so the three [`POINTS`] sample
/// selected, feathered and excluded pixels of the same frame.
fn secondary_timeline() -> Timeline {
    let g = MediaId::new();
    let files = HashMap::from([(
        g,
        rgb_source("grad.mkv", "170-40*X/W", "130+20*X/W", "95+110*X/W"),
    )]);
    let mut clip = media_clip(g, 0, 100, 0);
    clip.node_graph = secondary_graph();
    let mut seq = Sequence::new_default();
    seq.fps = Fps::new(FPS as u32, 1);
    seq.resolution = Resolution { width: W, height: H };
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![clip];
    seq.tracks = vec![v1];
    Timeline { seq, files, coded: HashMap::new() }
}

#[test]
fn a_secondary_grade_exports_as_the_viewer_shows_it() {
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let (t, out) = secondary_fixture();
    let viewer = Viewer::start(t);
    let mut worst = 0;
    for n in [10, 40, 80] {
        let (v, e) = (viewer.frame_at(n), exported_frame(out, n));
        worst = worst.max(compare(&format!("secondary frame {n:3}"), &v, &e));
    }
    eprintln!("secondary, viewer vs export: worst {worst} levels");
    assert!(worst <= 3, "the export differs from the viewer by {worst} levels");
}

/// And the secondary is actually in both pictures. A key that selected
/// nothing, or an ApplyMatte that passed the graded frame straight
/// through, would make the test above pass on two identical no-ops.
#[test]
fn the_secondary_changes_both_the_viewer_and_the_export() {
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let (with, a) = secondary_fixture();
    let mut without = with.clone();
    // Same graph with the key's own node bypassed: ApplyMatte then has no
    // matte and passes the graded branch through, so every pixel gets the
    // secondary instead of the keyed ones.
    for clip in &mut without.seq.tracks[0].clips {
        for node in &mut clip.node_graph.nodes {
            if node.kind == klipp_nodes::NodeKind::Qualifier {
                node.enabled = false;
            }
        }
    }
    let b = exported(&without);
    let (fa, fb) = (exported_frame(a, 40), exported_frame(&b, 40));
    let d = POINTS
        .iter()
        .map(|(_, x, y)| {
            let (p, q) = (px(&fa, *x, *y), px(&fb, *x, *y));
            (0..3).map(|c| (p[c] - q[c]).abs()).max().unwrap()
        })
        .max()
        .unwrap();
    eprintln!("keyed vs unkeyed export: worst {d} levels apart");
    assert!(d > 5, "the key made no difference to the export ({d} levels)");
}

/// One clip, coded `coded` and turned by `rotation`, over this landscape
/// sequence's first two seconds. The source is red on its coded left half
/// and blue on its right.
fn one_clip_timeline(name: &str, coded: (u32, u32), rotation: i32) -> Timeline {
    let p = MediaId::new();
    let file = rgb_source_sized(name, coded, "if(lt(X,W/2),200,40)", "40", "if(lt(X,W/2),40,200)");
    let mut seq = Sequence::new_default();
    seq.fps = Fps::new(FPS as u32, 1);
    seq.resolution = Resolution { width: W, height: H };
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![media_clip(p, 0, 50, 0)];
    seq.tracks = vec![v1];
    Timeline { seq, files: HashMap::from([(p, file)]), coded: HashMap::from([(p, (coded.0, coded.1, rotation))]) }
}

/// What a frame should hold at a point.
#[derive(Clone, Copy, Debug)]
enum Expect {
    /// A bar: black.
    Bar,
    /// Some of the picture, of either colour.
    Picture,
    /// The source's red half.
    Red,
    /// The source's blue half.
    Blue,
}

fn holds(p: [i32; 3], e: Expect) -> bool {
    match e {
        Expect::Bar => p.iter().all(|c| *c <= 20),
        Expect::Picture => holds(p, Expect::Red) || holds(p, Expect::Blue),
        Expect::Red => p[0] > 150 && p[2] < 90,
        Expect::Blue => p[2] > 150 && p[0] < 90,
    }
}

/// Export `t` and check what its frames hold at `points`, then check the
/// viewer shows the same; the exported frames, for more checks. The
/// export is checked first and on its own, so a failure says which half
/// is wrong: the viewer's frame size is asserted only after it.
fn fits_alike(t: &Timeline, points: &[(&str, u32, u32, Expect)]) -> Vec<Vec<u8>> {
    let out = exported(t);
    let frames = [10, 30];
    let exported: Vec<Vec<u8>> = frames.iter().map(|n| exported_frame(&out, *n)).collect();
    for (n, e) in frames.iter().zip(&exported) {
        for (name, x, y, want) in points {
            let q = px(e, *x, *y);
            eprintln!("export frame {n} {name:10}: {q:?}, want {want:?}");
            assert!(holds(q, *want), "export frame {n}: the {name} is {q:?}, not {want:?}");
        }
    }
    // `frame_at` asserts the viewer's frame is the sequence's size.
    let viewer = Viewer::start(t);
    for (n, e) in frames.iter().zip(&exported) {
        let v = viewer.frame_at(*n);
        for (name, x, y, _) in points {
            let (p, q) = (px(&v, *x, *y), px(e, *x, *y));
            let d = (0..3).map(|c| (p[c] - q[c]).abs()).max().unwrap();
            eprintln!("frame {n} {name:10}: viewer {p:?} export {q:?} diff {d}");
            assert!(d <= 3, "frame {n}: the {name} differs by {d} levels (viewer {p:?}, export {q:?})");
        }
    }
    exported
}

/// A phone clip shot upright — coded landscape with a quarter-turn matrix
/// — in a landscape sequence is pillarboxed by both. The export used to
/// decode it straight to the frame's size, stretching it three times too
/// wide, while the viewer rendered in a frame the clip's own shape.
#[test]
fn a_portrait_clip_is_pillarboxed_in_the_viewer_and_the_export_alike() {
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let t = one_clip_timeline("portrait.mkv", (W, H), 90);
    // Upright, the picture is H * H / W = 202 pixels wide, centred.
    let picture_w = H * H / W;
    let (left, right) = ((W - picture_w) / 2, (W + picture_w) / 2);
    let (upper, lower) = ((W / 2, H / 4), (W / 2, H * 3 / 4));
    let exported = fits_alike(&t, &[
        ("left bar", left / 2, H / 2, Expect::Bar),
        ("right bar", (right + W) / 2, H / 2, Expect::Bar),
        ("upper", upper.0, upper.1, Expect::Picture),
        ("lower", lower.0, lower.1, Expect::Picture),
    ]);
    // Upright: the halves one above the other, not side by side.
    for e in &exported {
        let (u, l) = (px(e, upper.0, upper.1), px(e, lower.0, lower.1));
        assert!((u[0] - l[0]).abs() > 100, "the halves are not above each other ({u:?}, {l:?})");
    }
}

/// A picture wider than the sequence is letterboxed by both.
#[test]
fn a_wide_clip_is_letterboxed_in_the_viewer_and_the_export_alike() {
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    // 3.2:1: at the frame's width, 200 rows of picture, centred.
    let t = one_clip_timeline("wide.mkv", (W, 200), 0);
    let (top, bottom) = ((H - 200) / 2, (H + 200) / 2);
    fits_alike(&t, &[
        ("top bar", W / 2, top / 2, Expect::Bar),
        ("bottom bar", W / 2, (bottom + H) / 2, Expect::Bar),
        ("left", W / 4, H / 2, Expect::Red),
        ("right", W * 3 / 4, H / 2, Expect::Blue),
    ]);
}

/// A half turn keeps the frame's shape and fills it, turned over in both:
/// the coded left half ends up on the right.
#[test]
fn a_half_turned_clip_is_turned_over_in_the_viewer_and_the_export_alike() {
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let t = one_clip_timeline("half-turn.mkv", (W, H), 180);
    fits_alike(&t, &[
        ("left", W / 4, H / 2, Expect::Blue),
        ("right", W * 3 / 4, H / 2, Expect::Red),
    ]);
}

/// An odd-sized picture keeps its odd side in both, so the bars end on
/// the same row: 201 rows centred in 360 are rows 79 to 279. Read as the
/// light of single pixels, since the export's 4:2:0 chroma is shared by
/// row pairs and bleeds colour one row across an odd edge.
#[test]
fn an_odd_sized_clip_is_barred_on_the_same_rows_in_the_viewer_and_the_export() {
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let t = one_clip_timeline("odd.mkv", (W, 201), 0);
    let (first, last) = ((H - 201) / 2, (H - 201) / 2 + 200);
    let out = exported(&t);
    let e = exported_frame(&out, 10);
    let luma = |rgb: &[u8], y: u32| {
        let i = ((y * W + W / 4) * 3) as usize;
        0.2126 * rgb[i] as f32 + 0.7152 * rgb[i + 1] as f32 + 0.0722 * rgb[i + 2] as f32
    };
    let rows = [(first - 1, false), (first, true), (last, true), (last + 1, false)];
    for (y, lit) in rows {
        eprintln!("export row {y}: luma {:.0}", luma(&e, y));
        assert_eq!(luma(&e, y) > 35.0, lit, "export row {y} (luma {:.0})", luma(&e, y));
    }
    let viewer = Viewer::start(&t);
    let v = viewer.frame_at(10);
    for (y, lit) in rows {
        eprintln!("viewer row {y}: luma {:.0}", luma(&v, y));
        assert_eq!(luma(&v, y) > 35.0, lit, "viewer row {y} (luma {:.0})", luma(&v, y));
    }
}
