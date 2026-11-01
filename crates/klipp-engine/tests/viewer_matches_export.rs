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

use klipp_timeline::GradeAccess;
use klipp_timeline::test_clip::{self, TestClip};
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
/// The decode delay is one setting for the whole process, and the tests
/// in this file run in parallel: while the slow-decoder test had it set,
/// every other engine here decoded 60 ms a frame too, and one of them
/// now and then read a frame before its stream caught up (#411, about
/// one full run in sixteen). The slow test holds this for writing; the
/// rest hold it for reading, so they still run alongside each other,
/// but never alongside it.
static DECODER_SPEED: RwLock<()> = RwLock::new(());

fn decoders_at_speed() -> std::sync::RwLockReadGuard<'static, ()> {
    DECODER_SPEED.read().unwrap_or_else(|e| e.into_inner())
}

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
fn rgb_source_sized(name: &str, size: (u32, u32), r: &str, g: &str, b: &str) -> PathBuf {
    rgb_source_at(name, size, FPS as u32, r, g, b)
}

/// [`rgb_source_sized`], at its own frame rate.
fn rgb_source_at(name: &str, (w, h): (u32, u32), rate: u32, r: &str, g: &str, b: &str) -> PathBuf {
    let out = tmp(&format!("{}-{}", uuid::Uuid::new_v4(), name));
    let filter = format!(
        "color=c=black:s={w}x{h}:r={rate}:d=10,format=rgb24,geq=r='{r}':g='{g}':b='{b}',\
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

fn scaled(c: Clip, scale: f32, x: f32, y: f32) -> Clip {
    TestClip::of(c).effect(BuiltinEffect::Transform {
        position_x: x, position_y: y, scale_x: scale, scale_y: scale, rotation: 0.0,
        anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None,
    }).build()
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
    clip_a.set_grade(exposure(1.0));
    let clip_b = media_clip(b, 100, LEN, 25);
    let clip_c = TestClip::of(scaled(media_clip(c, 25, 175, 10), 0.6, 0.0, 0.0)).effect(BuiltinEffect::Opacity { value: 0.5 }).build();
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
                (*id, MediaFile { path: p.clone(), log_format: None, source_rotation, ..Default::default() })
            })
            .collect(),
        ..Default::default()
    };
    let snapshot = SequenceSnapshot {
        audio_buses: Vec::new(),
        fps: FPS as f64, fps_num: FPS as u32, fps_den: 1, width: W, height: H, sample_rate: 48000,
        duration_frames: LEN as u64, export_range: None, video,
        audio_tracks: vec![], global_lut_path: None, subtitle_tracks: vec![], chapters: vec![],
        pan_law: Default::default(),
        working_colorspace: WorkingColorspace::Rec709Linear, grade_encoding: klipp_core::GradeEncoding::Bt1886,
    };
    let settings = ExportSettings {
        pan_law: Default::default(),
        output_path: out.to_path_buf(), format: ExportFormat::Mp4, video_codec: ExportVideoCodec::H264,
        resolution: None, fps: None, crf: 1, preset: EncoderPreset::Fast, audio_codec: ExportAudioCodec::Copy,
        audio_bitrate_kbps: None, video_bitrate_kbps: None, rate_control: RateControl::Crf,
        burn_in_subtitles: false, embed_subtitles: false, target_lufs: None, true_peak_dbtp: None, embed_chapters: false, captions_608: false, hdr10plus: false,
        bit_depth: 0, hdr_mode: HdrMode::Sdr, tone_map_op: ToneMapOp::Hable, odt: OdtChoice::MatchViewer,
        alpha: false,
        mastering_peak_nits: klipp_core::DEFAULT_MASTERING_PEAK_NITS,
    };
    ExportJob {
        id: uuid::Uuid::new_v4(), output_path: out.to_path_buf(), settings, snapshot,
        cancel: Arc::new(AtomicBool::new(false)), progress: Arc::new(AtomicU64::new(0)),
        hw_info: Default::default(), render_stage: Default::default(), loudness_result: Default::default(), timings: Default::default(),
        analyser: None,
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
    thread: Option<klipp_engine::RunningEngine>,
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
            Arc::new(klipp_timeline::SharedSequence::new(t.seq.clone())),
            Arc::new(RwLock::new(pool)),
            Box::new(|| {}),
            Arc::new(Mutex::new(None)),
            Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
        );
        let thread = engine.spawn();
        // Drain events: the engine must not block on a full channel. Which
        // frame is on screen comes from the frame buffer, with the frame.
        std::thread::spawn(move || while ev_rx.recv().is_ok() {});
        Self { cmd, fb, thread: Some(thread) }
    }

    /// Wait until no new frame has arrived for 300 ms, then return the
    /// latest as RGB24. A stream that opened later than that would leave a
    /// layer out of the picture, which the comparison catches by a mile.
    fn settled_frame(&self) -> Vec<u8> {
        self.settled_labelled().0
    }

    /// [`Viewer::settled_frame`], with the frame the bytes are of and
    /// whether it was complete, read under the same lock as the bytes: a
    /// frame published between reading the label and reading the bytes
    /// would pair one frame's label with another's picture (#1092).
    fn settled_labelled(&self) -> (Vec<u8>, Option<i64>, bool) {
        let generation = || self.fb.lock().unwrap().latest_generation();
        let start = Instant::now();
        let mut last = generation();
        let mut since = Instant::now();
        while since.elapsed() < Duration::from_millis(300) && start.elapsed() < Duration::from_secs(30) {
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
        let presented = fb.latest_presented().map(|at| at.value * FPS / at.rate as i64);
        (rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect(), presented, fb.latest_complete())
    }

    /// Play from where it is parked (`from`) for at least `at_least`, and
    /// on until a frame past `from` is shown (or a minute), then pause.
    /// Returns how long it played. A loaded machine plays later and slower,
    /// never wrong: nothing here asks the wall clock for a frame count.
    fn play(&self, from: i64, at_least: Duration) -> Duration {
        let t0 = Instant::now();
        self.cmd.send(AppCommand::Play).unwrap();
        std::thread::sleep(at_least);
        let moved = |fb: &FrameBuffer| fb.shown_presented().is_some_and(|t| (t.to_secs_f64() * FPS as f64).round() as i64 > from);
        while t0.elapsed() < Duration::from_secs(60) && !moved(&self.fb.lock().unwrap()) {
            std::thread::sleep(Duration::from_millis(5));
        }
        self.cmd.send(AppCommand::Pause).unwrap();
        t0.elapsed()
    }

    /// The frame playback stopped on, once it has settled: the one the
    /// viewer shows, which while playing is a zero-copy frame with no bytes
    /// to read, so its bytes are asked for by parking on it. Reading the
    /// bytes that happened to be there and trusting the frame the screen
    /// names paired an older picture with a newer frame — under load, 165
    /// levels out.
    fn settled_frame_at(&self) -> (Vec<u8>, i64) {
        let _ = self.settled_frame();
        let at = self.fb.lock().unwrap().shown_presented().expect("the engine names the frame it presents");
        let n = at.value * FPS / at.rate as i64;
        (self.frame_at(n), n)
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
    ///
    /// Only a frame published after the park was asked for counts (#1092).
    /// Pause parks at the playback clock, which under load runs ahead of
    /// the frame on screen: the screen still named frame `n` from playing,
    /// so the wait passed on that old frame before the engine had read the
    /// command, and the park ahead of it published during the quiet window
    /// and was read as frame `n` (96 levels out). And the settled bytes are
    /// checked to still be frame `n`, complete, before they're returned.
    fn frame_at(&self, n: i64) -> Vec<u8> {
        let asked_after = self.fb.lock().unwrap().latest_generation();
        self.cmd.send(AppCommand::SetPlayhead(tc(n))).unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            self.wait_for_complete(n, asked_after, deadline);
            let (bytes, presented, complete) = self.settled_labelled();
            if presented == Some(n) && complete {
                return bytes;
            }
            assert!(Instant::now() < deadline, "the viewer settled on frame {presented:?} (complete {complete}), not a complete frame {n}\n{}", self.trail());
        }
    }

    /// The engine's recent notes on the viewer's picture, one per line,
    /// for a failed wait to say what the engine did (#1092).
    fn trail(&self) -> String {
        let trail = self.fb.lock().unwrap().trail();
        format!("engine trail, oldest first:\n  {}", trail.join("\n  "))
    }

    /// Wait for a complete frame `n` published after generation `after`.
    fn wait_for_complete(&self, n: i64, after: u64, deadline: Instant) {
        // The frame buffer names its bytes, never a zero-copy frame newer
        // than them, so this waits for frame `n`'s own picture — and for a
        // complete one: a frame rendered before every clip's stream had its
        // picture says so, and under load the overlays' streams can take
        // longer than the quiet window below to arrive. Frame 60 was read
        // with only its bottom clip, 95 levels out where c and d belonged.
        loop {
            let (generation, presented, complete) = {
                let fb = self.fb.lock().unwrap();
                (fb.latest_generation(), fb.latest_presented().map(|at| at.value * FPS / at.rate as i64), fb.latest_complete())
            };
            if generation > after && presented == Some(n) && complete {
                return;
            }
            assert!(Instant::now() < deadline, "the viewer never presented a complete frame {n} (last: {presented:?}, complete {complete}, generation {generation}, asked after {after})\n{}", self.trail());
            std::thread::sleep(Duration::from_millis(5));
        }
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
    let _decoders = decoders_at_speed();
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
    let _decoders = decoders_at_speed();
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let (t, out) = main_fixture();
    let viewer = Viewer::start(t);
    let mut worst = 0;
    for (from, play_ms) in [(80, 800), (90, 700), (70, 1500)] {
        viewer.frame_at(from);
        viewer.play(from, Duration::from_millis(play_ms));
        let (v, n) = viewer.settled_frame_at();
        assert!(n > from, "playback did not move: from {from}, at {n}");
        worst = worst.max(compare(&format!("played {from}->{n:3}"), &v, &exported_frame(out, n)));
        // And back into what was just played: the frames cached and the
        // streams left where playback took them must not stand in for it.
        let back = n - 4;
        worst = worst.max(compare(&format!("back to {back:3}"), &viewer.frame_at(back), &exported_frame(out, back)));
    }
    eprintln!("viewer vs export, after playing: worst {worst} levels");
    assert!(worst <= 3, "the viewer differs from the export by {worst} levels");
}

/// #1092: a park asked for after another one is still being made is the
/// one read. Pause parks at the playback clock, which under load runs ahead
/// of the frame on screen; the test then parked on the frame the screen
/// named. The screen still named it, so the wait passed before the engine
/// had read the second park, and the first park, published during the
/// quiet window, was read as the second (96 levels out). This makes that
/// happen without load: a slow decoder holds the second park back while
/// the first one, already under way, publishes.
#[test]
fn a_park_asked_for_while_another_is_made_is_the_one_read() {
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            klipp_engine::video_decoder::set_decode_delay_for_tests(0);
        }
    }
    let _decoders = DECODER_SPEED.write().unwrap_or_else(|e| e.into_inner());
    let (t, out) = main_fixture();
    let viewer = Viewer::start(t);
    let _restore = Restore;
    // Both frames rendered once, so the screen can name 95 while 107 is made.
    viewer.frame_at(107);
    viewer.frame_at(95);
    klipp_engine::video_decoder::set_decode_delay_for_tests(500);
    viewer.cmd.send(AppCommand::SetPlayhead(tc(107))).unwrap();
    // Let the engine take the first park up before the second is asked
    // for, or it reads both commands in one tick and makes only the last.
    std::thread::sleep(Duration::from_millis(40));
    let worst = compare("asked for 95 while 107 was made", &viewer.frame_at(95), &exported_frame(out, 95));
    assert!(worst <= 3, "the viewer differs from the export by {worst} levels");
}

/// Playing through the dissolve with decoders slower than the timeline,
/// as a loaded machine makes them. The dissolve's other side then lags
/// its stream while playing, and a frame drawn from the one before was
/// published as complete; after the pause it stayed on screen until the
/// parked refresh replaced it, so a read in between took it (#411, 75
/// to 165 levels out under load). Slowing the decoders makes that
/// happen every time instead of one gate run in ten.
#[test]
fn a_slow_decoder_after_playing_still_shows_the_exported_frames() {
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            klipp_engine::video_decoder::set_decode_delay_for_tests(0);
        }
    }
    let _decoders = DECODER_SPEED.write().unwrap_or_else(|e| e.into_inner());
    let (t, out) = main_fixture();
    let viewer = Viewer::start(t);
    let _restore = Restore;
    klipp_engine::video_decoder::set_decode_delay_for_tests(60);
    let mut worst = 0;
    for (from, play_ms) in [(80, 800), (90, 700), (70, 1500)] {
        viewer.frame_at(from);
        viewer.play(from, Duration::from_millis(play_ms));
        let (v, n) = viewer.settled_frame_at();
        worst = worst.max(compare(&format!("slow, played {from}->{n:3}"), &v, &exported_frame(out, n)));
        let back = n - 4;
        worst = worst.max(compare(&format!("slow, back to {back:3}"), &viewer.frame_at(back), &exported_frame(out, back)));
    }
    eprintln!("viewer vs export with a slow decoder: worst {worst} levels");
    assert!(worst <= 3, "the viewer differs from the export by {worst} levels");
}

/// Paused on the dissolve's cut frame just as playback crosses it: the
/// stream that takes the outgoing clip's decoder over has a's frame at the
/// cut, and the viewer settles on a complete frame 100, the export's.
///
/// The decoder's frame at the cut was taken by playback and dropped, and
/// the stream held nothing, its queue starting a frame later: parked there,
/// no frame for it ever came, and the viewer stayed incomplete until any
/// wait gave up (#1494). The pause is sent the moment the engine notes the
/// cut, with the decoders slowed so the next frame is 60 ms off; a pause
/// that still lands past the cut is tried again.
#[test]
fn pausing_on_a_dissolves_cut_settles_on_the_exported_frame() {
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            klipp_engine::video_decoder::set_decode_delay_for_tests(0);
        }
    }
    let _decoders = DECODER_SPEED.write().unwrap_or_else(|e| e.into_inner());
    let (t, out) = main_fixture();
    let viewer = Viewer::start(t);
    let _restore = Restore;
    klipp_engine::video_decoder::set_decode_delay_for_tests(60);
    let cuts = |v: &Viewer| v.fb.lock().unwrap().trail().iter().filter(|l| l.contains("cut at 100:")).count();
    let paused_at = |v: &Viewer| {
        let trail = v.fb.lock().unwrap().trail();
        trail.iter().rev().find_map(|l| l.split("paused at ").nth(1).and_then(|n| n.trim().parse::<i64>().ok()))
    };
    for attempt in 0..5 {
        viewer.frame_at(97);
        let before = cuts(&viewer);
        viewer.cmd.send(AppCommand::Play).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while cuts(&viewer) == before {
            assert!(Instant::now() < deadline, "playback never crossed the cut
{}", viewer.trail());
            std::thread::sleep(Duration::from_millis(1));
        }
        let asked_after = viewer.fb.lock().unwrap().latest_generation();
        viewer.cmd.send(AppCommand::Pause).unwrap();
        let wait = Instant::now() + Duration::from_secs(10);
        while paused_at(&viewer).is_none_or(|n| n < 100) {
            assert!(Instant::now() < wait, "the pause was never noted
{}", viewer.trail());
            std::thread::sleep(Duration::from_millis(2));
        }
        if paused_at(&viewer) != Some(100) {
            eprintln!("attempt {attempt}: paused past the cut, at {:?}; again", paused_at(&viewer));
            continue;
        }
        // The cut's own frame, of the outgoing clip at its time, went
        // with the decoder: the tag matched.
        let handed = viewer.fb.lock().unwrap().trail().iter().any(|l| l.contains("cut at 100:") && l.contains("handed on"));
        assert!(handed, "the frame at the cut wasn't handed on\n{}", viewer.trail());
        viewer.wait_for_complete(100, asked_after, Instant::now() + Duration::from_secs(20));
        let (v, presented, complete) = viewer.settled_labelled();
        assert_eq!((presented, complete), (Some(100), true), "settled on the cut\n{}", viewer.trail());
        let worst = compare("paused on the cut", &v, &exported_frame(out, 100));
        assert!(worst <= 3, "the viewer differs from the export on the cut by {worst} levels");
        return;
    }
    panic!("no pause landed on the cut in five tries\n{}", viewer.trail());
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
    clip.set_grade(secondary_graph());
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
    let _decoders = decoders_at_speed();
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
    let _decoders = decoders_at_speed();
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
        clip.edit_grade(|g| {
            for node in &mut g.nodes {
                if node.kind == klipp_nodes::NodeKind::Qualifier {
                    node.enabled = false;
                }
            }
        });
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

/// A freeform mask keyed to move across the frame (#74), cutting the
/// clip's picture out: the viewer and the export draw it where its keys
/// put it on each frame, both evaluating the keys the same way.
fn moving_mask_timeline() -> Timeline {
    use klipp_core::mask_path::{BezierPoint, PathEase, PathKey};
    use klipp_nodes::{Edge, Node, NodeGraph, NodeId, NodeKind, NodeParams, NodePos};
    let node = |kind, params| Node { id: NodeId::new_v4(), kind, label: format!("{kind:?}"), enabled: true, params, keyframes: Default::default(), output_link: None, input_links: vec![] };
    let shape = |off: f32| -> Vec<BezierPoint> {
        [(0.1, 0.3), (0.3, 0.3), (0.3, 0.7), (0.1, 0.7)]
            .into_iter()
            .map(|(x, y)| BezierPoint { x: x + off, y, out_dx: 0.04, out_dy: -0.03, in_dx: -0.04, in_dy: 0.03, smooth: true })
            .collect()
    };
    let mut mask = klipp_core::Mask { shape: klipp_core::MaskShape::Bezier { points: shape(0.0) }, feather: 0.02, ..klipp_core::Mask::default() };
    mask.keyframes.path = vec![
        PathKey { time_secs: 0.0, points: shape(0.0), ease: PathEase::Ease },
        PathKey { time_secs: 90.0 / FPS as f64, points: shape(0.55), ease: PathEase::Linear },
    ];
    let src = node(NodeKind::Source, NodeParams::default());
    let m = node(NodeKind::Mask, NodeParams { masks: Some(klipp_core::MaskStack { masks: vec![mask] }), ..Default::default() });
    let cut = node(NodeKind::MaskCutout, NodeParams::default());
    let out = node(NodeKind::Output, NodeParams::default());
    let edges = vec![
        Edge { from_node: src.id, from_port: 0, to_node: cut.id, to_port: 0 },
        Edge { from_node: m.id, from_port: 0, to_node: cut.id, to_port: 1 },
        Edge { from_node: cut.id, from_port: 0, to_node: out.id, to_port: 0 },
    ];
    let output_node_id = out.id;
    let nodes = vec![src, m, cut, out];
    let node_positions = nodes.iter().enumerate().map(|(i, n)| (n.id, NodePos { x: i as f32 * 150.0, y: 0.0 })).collect();
    let graph = NodeGraph { output_node_id, nodes, edges, node_positions, working_space_override: None, frames: vec![], sticky_notes: vec![] };
    let g = MediaId::new();
    let files = HashMap::from([(g, rgb_source("flat.mkv", "200", "150", "90"))]);
    let mut clip = media_clip(g, 0, 100, 0);
    clip.set_grade(graph);
    let mut seq = Sequence::new_default();
    seq.fps = Fps::new(FPS as u32, 1);
    seq.resolution = Resolution { width: W, height: H };
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![clip];
    seq.tracks = vec![v1];
    Timeline { seq, files, coded: HashMap::new() }
}

#[test]
fn a_moving_freeform_mask_exports_as_the_viewer_shows_it() {
    let _decoders = decoders_at_speed();
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let t = moving_mask_timeline();
    let out = exported(&t);
    let viewer = Viewer::start(&t);
    let mut worst = 0;
    let mut frames = Vec::new();
    for n in [5, 30, 60, 85] {
        let (v, e) = (viewer.frame_at(n), exported_frame(&out, n));
        worst = worst.max(compare(&format!("moving mask frame {n:3}"), &v, &e));
        frames.push(e);
    }
    eprintln!("moving mask, viewer vs export: worst {worst} levels");
    assert!(worst <= 3, "the export differs from the viewer by {worst} levels");
    // And it moved: lit on the left early, dark there late, and the
    // other way round on the right.
    let lit = |f: &[u8], x: f32| px(f, (x * W as f32) as u32, H / 2)[0] > 100;
    assert!(lit(&frames[0], 0.2) && !lit(&frames[0], 0.75), "frame 5");
    assert!(!lit(&frames[3], 0.2) && lit(&frames[3], 0.75), "frame 85");
}

/// The main timeline — graded clip, dissolve, scaled and faded layers —
/// under a timeline grade: the viewer and the export both run it over
/// the composite, and it changes the picture.
#[test]
fn a_timeline_grade_exports_as_the_viewer_shows_it() {
    let _decoders = decoders_at_speed();
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let mut t = timeline();
    t.seq.timeline_grade_slot().set(Some(exposure(-0.8)));
    let out = exported(&t);
    let viewer = Viewer::start(&t);
    let mut worst = 0;
    for n in [10, 60, 110] {
        let (v, e) = (viewer.frame_at(n), exported_frame(&out, n));
        worst = worst.max(compare(&format!("timeline-graded frame {n:3}"), &v, &e));
    }
    eprintln!("timeline grade, viewer vs export: worst {worst} levels");
    assert!(worst <= 3, "the export differs from the viewer by {worst} levels");
    let (_, plain) = main_fixture();
    let (g, p) = (exported_frame(&out, 60), exported_frame(plain, 60));
    let d = g.iter().zip(&p).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap_or(0);
    assert!(d > 20, "only {d} levels: the timeline grade is not in the export");
}

/// The main timeline with two of its clips in a grade group (#237),
/// darkened by the group's post-clip grade on top of clip a's own. Both
/// run the flattened sequence with the group baked in, as the UI hands
/// the engine and as the export's snapshot flattens it (the snapshot's
/// own test checks that it does). They agree, and the group's grade is
/// in the picture.
#[test]
fn a_grade_group_exports_as_the_viewer_shows_it() {
    let _decoders = decoders_at_speed();
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let mut t = timeline();
    let mut group = klipp_timeline::GradeGroup::new("Scene");
    group.stage_slot(klipp_timeline::GradeStage::Post).set(Some(exposure(-0.8)));
    let gid = group.id;
    t.seq.grade_groups = vec![group];
    // The two full-frame clips: a (with its own grade) and b.
    for c in t.seq.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).filter(|c| test_clip::effects(c).is_empty() && c.transform_scale_keyframes().is_empty()) {
        c.grade_group = Some(gid);
    }
    let mut shown = t.clone();
    shown.seq = klipp_timeline::flatten::flatten_sequence(&t.seq, std::slice::from_ref(&t.seq));
    let out = exported(&shown);
    let viewer = Viewer::start(&shown);
    let mut worst = 0;
    for n in [10, 60, 110] {
        let (v, e) = (viewer.frame_at(n), exported_frame(&out, n));
        worst = worst.max(compare(&format!("group-graded frame {n:3}"), &v, &e));
    }
    eprintln!("grade group, viewer vs export: worst {worst} levels");
    assert!(worst <= 3, "the export differs from the viewer by {worst} levels");
    let (_, plain) = main_fixture();
    let (g, p) = (exported_frame(&out, 60), exported_frame(plain, 60));
    let d = g.iter().zip(&p).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap_or(0);
    assert!(d > 20, "only {d} levels: the group's grade is not in the export");
}

/// The main timeline with a vignette and a darkening grade on V1 itself
/// (#846), over clips a (with its own grade) and b, which carry none of
/// it. Both run the flattened sequence with the track's effects baked onto
/// its clips, as the UI hands the engine and as the export's snapshot
/// makes it (the snapshot's own test checks that it does). They agree, and
/// the track's effects are in the picture: at the centre, which only the
/// grade reaches, and at the corner, which the vignette darkens more.
#[test]
fn a_tracks_effects_and_grade_export_as_the_viewer_shows_them() {
    let _decoders = decoders_at_speed();
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let mut t = timeline();
    let v1 = t.seq.tracks.iter_mut().find(|t| t.name == "V1").unwrap();
    v1.effects.set_video_effect_list(vec![BuiltinEffect::Vignette { amount: -0.8, size: 0.5, softness: 0.4, roundness: 0.7 }], vec![]);
    v1.effects.grade_slot().set(Some(exposure(-0.8)));
    let mut shown = t.clone();
    shown.seq = klipp_timeline::flatten::flatten_sequence(&t.seq, std::slice::from_ref(&t.seq));
    let out = exported(&shown);
    let viewer = Viewer::start(&shown);
    let mut worst = 0;
    for n in [10, 60, 110] {
        let (v, e) = (viewer.frame_at(n), exported_frame(&out, n));
        worst = worst.max(compare(&format!("track-effect frame {n:3}"), &v, &e));
    }
    eprintln!("track effects, viewer vs export: worst {worst} levels");
    assert!(worst <= 3, "the export differs from the viewer by {worst} levels");
    let (_, plain) = main_fixture();
    let (g, p) = (exported_frame(&out, 10), exported_frame(plain, 10));
    let d = g.iter().zip(&p).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap_or(0);
    assert!(d > 20, "only {d} levels: the track's effects are not in the export");
    // The corner, where the vignette bites, darker than the plain export
    // by more than the grade alone makes the middle.
    let darker = |x: u32, y: u32| {
        let (a, b) = (px(&p, x, y), px(&g, x, y));
        (0..3).map(|c| b[c] - a[c]).sum::<i32>()
    };
    assert!(darker(4, 4) < darker(W / 2, H / 2), "the vignette is not in the export: corner {} vs centre {}", darker(4, 4), darker(W / 2, H / 2));
}

/// The main timeline with a colour-space transform and a 3D LUT on V1's
/// clips (#1746), added to each clip's picture as the Picture view adds
/// them: the transform reads the picture's shown values as linear
/// Rec.709 into Rec.2020, darker and paler; the LUT, a `.cube` on disk,
/// warms and crushes it. Both run in the
/// viewer and the export alike, and both are in the picture.
#[test]
fn a_lut_and_a_colour_space_transform_export_as_the_viewer_shows_them() {
    let _decoders = decoders_at_speed();
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let cube = tmp(&format!("warm-{}.cube", uuid::Uuid::new_v4()));
    let n = 9;
    let rows: Vec<[f32; 3]> = (0..n * n * n)
        .map(|i| {
            let [r, g, b] = [i % n, (i / n) % n, i / (n * n)].map(|v| v as f32 / (n - 1) as f32);
            [(r * 1.1 + 0.03).min(1.0), g.powf(1.2), b * 0.8]
        })
        .collect();
    klipp_color::write_cube(&cube, &rows, n as u32, "warm").unwrap();
    let mut t = timeline();
    let fx = [
        BuiltinEffect::ColourSpaceTransform { input_gamut: 1, input_curve: 1, output_gamut: 2, output_curve: 0, prepared: None },
        BuiltinEffect::Lut3D { path: cube.to_string_lossy().into_owned(), opacity: 0.9, tetrahedral: true, prepared: None },
    ];
    for c in t.seq.tracks.iter_mut().find(|t| t.name == "V1").unwrap().clips.iter_mut() {
        add_after_display(c, &fx);
    }
    let mut shown = t.clone();
    shown.seq = klipp_timeline::flatten::flatten_sequence(&t.seq, std::slice::from_ref(&t.seq));
    let out = exported(&shown);
    let viewer = Viewer::start(&shown);
    let mut worst = 0;
    for n in [10, 60, 110] {
        let (v, e) = (viewer.frame_at(n), exported_frame(&out, n));
        worst = worst.max(compare(&format!("LUT and transform frame {n:3}"), &v, &e));
    }
    eprintln!("LUT and transform, viewer vs export: worst {worst} levels");
    assert!(worst <= 3, "the export differs from the viewer by {worst} levels");
    let (_, plain) = main_fixture();
    let (g, p) = (exported_frame(&out, 60), exported_frame(plain, 60));
    let d = g.iter().zip(&p).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap_or(0);
    assert!(d > 20, "only {d} levels: the LUT and the transform are not in the export");
}

/// The main timeline with a gamma on V1's clips masked by a luma key beside
/// each one's picture chain (#1746), as the Picture view's Mattes menu
/// makes it: the key reads the clip's source and feeds the gamma's mask.
/// Both run it alike, and the keyed gamma is in the picture (the sources
/// are flat, so the key takes the whole of each from its low end).
#[test]
fn a_luma_keyed_gamma_exports_as_the_viewer_shows_it() {
    let _decoders = decoders_at_speed();
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    use klipp_graph_format::path::{NodePath, Root};
    use klipp_graph_format::ports::{input_key, output_key};
    let mut t = timeline();
    for c in t.seq.tracks.iter_mut().find(|t| t.name == "V1").unwrap().clips.iter_mut() {
        add_after_display(c, &[BuiltinEffect::Gamma { gamma: 1.8 }]);
        let graph = c.picture_v2().unwrap().picture.graph.clone();
        let at = |id| NodePath::new(Root::Clip { clip: c.id.0 }, [id]);
        let source = graph.nodes.iter().find(|n| n.type_id.as_str() == klipp_graph_format::lifted::types::SOURCE).unwrap().id;
        let blur = graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("gamma")).unwrap().id;
        let key = klipp_nodes::NodeParams { luma_key: Some(klipp_core::LumaKeySettings { enabled: true, low: 0.0, high: 1.0, soft: 0.05, invert: false }), ..Default::default() };
        let node: klipp_graph_format::Node = serde_json::from_value(serde_json::json!({
            "id": klipp_graph_format::NodeId::new_v4(),
            "type_id": "klipp.legacy.node",
            "legacy": { "payload": { "kind": "LumaKey", "label": "Luma Key", "params": key } },
        }))
        .unwrap();
        let id = node.id;
        c.apply_picture_ops(&[
            klipp_graph_lift::ops::GraphOp::AddNodeBeside { at: at(source), node: Box::new(node) },
            klipp_graph_lift::ops::GraphOp::Connect { from: at(source), from_port: "out".into(), to: at(id), to_port: input_key("LumaKey", 0) },
            klipp_graph_lift::ops::GraphOp::Connect { from: at(id), from_port: output_key("LumaKey", 0), to: at(blur), to_port: "mask".into() },
        ])
        .unwrap();
    }
    let mut shown = t.clone();
    shown.seq = klipp_timeline::flatten::flatten_sequence(&t.seq, std::slice::from_ref(&t.seq));
    let out = exported(&shown);
    let viewer = Viewer::start(&shown);
    let mut worst = 0;
    for n in [10, 60, 110] {
        let (v, e) = (viewer.frame_at(n), exported_frame(&out, n));
        worst = worst.max(compare(&format!("luma-keyed gamma frame {n:3}"), &v, &e));
    }
    eprintln!("luma-keyed gamma, viewer vs export: worst {worst} levels");
    assert!(worst <= 3, "the export differs from the viewer by {worst} levels");
    let (_, plain) = main_fixture();
    let (g, p) = (exported_frame(&out, 10), exported_frame(plain, 10));
    let (a, b) = (px(&g, 4, 4), px(&p, 4, 4));
    let d = (0..3).map(|c| (a[c] - b[c]).abs()).max().unwrap_or(0);
    assert!(d > 20, "only {d} levels at the corner: the keyed gamma is not in the export");
}

/// The main timeline with a Picture Transform node on V1's clips (#1746),
/// shrinking and turning each picture inside its frame: both run it
/// alike, and it is in the picture (the corner it uncovers is black).
#[test]
fn a_picture_transform_exports_as_the_viewer_shows_it() {
    let _decoders = decoders_at_speed();
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let mut t = timeline();
    let fx = [BuiltinEffect::PictureTransform { position_x: 30.0, position_y: 10.0, scale_x: 0.6, scale_y: 0.6, rotation: 8.0, anchor_x: 0.5, anchor_y: 0.5 }];
    for c in t.seq.tracks.iter_mut().find(|t| t.name == "V1").unwrap().clips.iter_mut() {
        add_after_display(c, &fx);
    }
    let mut shown = t.clone();
    shown.seq = klipp_timeline::flatten::flatten_sequence(&t.seq, std::slice::from_ref(&t.seq));
    let out = exported(&shown);
    let viewer = Viewer::start(&shown);
    let mut worst = 0;
    for n in [10, 60, 110] {
        let (v, e) = (viewer.frame_at(n), exported_frame(&out, n));
        worst = worst.max(compare(&format!("picture transform frame {n:3}"), &v, &e));
    }
    eprintln!("picture transform, viewer vs export: worst {worst} levels");
    assert!(worst <= 3, "the export differs from the viewer by {worst} levels");
    let (_, plain) = main_fixture();
    let (g, p) = (exported_frame(&out, 10), exported_frame(plain, 10));
    let (a, b) = (px(&g, 4, 4), px(&p, 4, 4));
    let d = (0..3).map(|c| (a[c] - b[c]).abs()).max().unwrap_or(0);
    assert!(d > 20, "only {d} levels at the corner: the picture transform is not in the export");
}

/// The main timeline with a Merge on V1's clips (#1746) laying a gradient
/// Generator over each picture: the generator beside the chain reading the
/// clip's source, wired into the merge's `fg`, as the Picture view's
/// Sources menu makes it. Both run it alike, and it is in the picture.
#[test]
fn a_merged_gradient_exports_as_the_viewer_shows_it() {
    let _decoders = decoders_at_speed();
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    use klipp_graph_format::path::{NodePath, Root};
    let mut t = timeline();
    for c in t.seq.tracks.iter_mut().find(|t| t.name == "V1").unwrap().clips.iter_mut() {
        add_after_display(c, &[BuiltinEffect::Merge { blend_mode: klipp_effects::BlendMode::Multiply, opacity: 0.8 }]);
        let graph = c.picture_v2().unwrap().picture.graph.clone();
        let at = |id| NodePath::new(Root::Clip { clip: c.id.0 }, [id]);
        let source = graph.nodes.iter().find(|n| n.type_id.as_str() == klipp_graph_format::lifted::types::SOURCE).unwrap().id;
        let merge = graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("merge")).unwrap().id;
        let gradient = BuiltinEffect::Generator { shape: 1, color_a: [30, 60, 160, 255], color_b: [250, 220, 120, 255], angle: 25.0, center_x: 0.5, center_y: 0.5, radius: 0.5 };
        let payload = serde_json::to_value(&gradient).unwrap();
        let node: klipp_graph_format::Node = serde_json::from_value(serde_json::json!({
            "id": klipp_graph_format::NodeId::new_v4(),
            "type_id": klipp_graph_lift::names::effect_node_type("generator"),
            "legacy": { "payload": payload },
        }))
        .unwrap();
        let id = node.id;
        c.apply_picture_ops(&[
            klipp_graph_lift::ops::GraphOp::AddNodeBeside { at: at(source), node: Box::new(node) },
            klipp_graph_lift::ops::GraphOp::Connect { from: at(source), from_port: "out".into(), to: at(id), to_port: "in".into() },
            klipp_graph_lift::ops::GraphOp::Connect { from: at(id), from_port: "out".into(), to: at(merge), to_port: "fg".into() },
        ])
        .unwrap();
    }
    let mut shown = t.clone();
    shown.seq = klipp_timeline::flatten::flatten_sequence(&t.seq, std::slice::from_ref(&t.seq));
    let out = exported(&shown);
    let viewer = Viewer::start(&shown);
    let mut worst = 0;
    for n in [10, 60, 110] {
        let (v, e) = (viewer.frame_at(n), exported_frame(&out, n));
        worst = worst.max(compare(&format!("merged gradient frame {n:3}"), &v, &e));
    }
    eprintln!("merged gradient, viewer vs export: worst {worst} levels");
    assert!(worst <= 3, "the export differs from the viewer by {worst} levels");
    let (_, plain) = main_fixture();
    let (g, p) = (exported_frame(&out, 10), exported_frame(plain, 10));
    let (a, b) = (px(&g, 4, 4), px(&p, 4, 4));
    let d = (0..3).map(|c| (a[c] - b[c]).abs()).max().unwrap_or(0);
    assert!(d > 20, "only {d} levels at the corner: the merge is not in the export");
}

/// The main timeline with a Merge on V1's clips (#1746) laying another
/// media item over each picture, twelve frames on: a Media Source beside
/// the chain, wired into the merge's `fg`, as the Picture view's Sources
/// menu makes it. The item is no clip's, and moves: a bar along its top
/// says which of its frames each route drew, the same, the one the time
/// names, and it is in the picture.
#[test]
fn a_merged_media_item_exports_as_the_viewer_shows_it() {
    let _decoders = decoders_at_speed();
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    use klipp_graph_format::path::{NodePath, Root};
    let mut t = timeline();
    let item = MediaId::new();
    // A smooth ramp across, so the export's 4:2:0 chroma has no edge to
    // blur at a sample point, and red that moves frame to frame.
    // Its frame number as a place: a bright bar across a band at the top,
    // 8 pixels on a frame, which no colour transform moves. Below the
    // band a smooth ramp, red that moves.
    let bar = "if(lt(Y,24),if(between(X,N*8,N*8+7),235,30)";
    let item_file = rgb_source("item.mkv", &format!("{bar},mod(N*9,200)+20)"), &format!("{bar},X/3+20)"), &format!("{bar},90)"));
    t.files.insert(item, item_file);
    for c in t.seq.tracks.iter_mut().find(|t| t.name == "V1").unwrap().clips.iter_mut() {
        add_after_display(c, &[BuiltinEffect::Merge { blend_mode: klipp_effects::BlendMode::Normal, opacity: 1.0 }]);
        let graph = c.picture_v2().unwrap().picture.graph.clone();
        let at = |id| NodePath::new(Root::Clip { clip: c.id.0 }, [id]);
        let source = graph.nodes.iter().find(|n| n.type_id.as_str() == klipp_graph_format::lifted::types::SOURCE).unwrap().id;
        let merge = graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("merge")).unwrap().id;
        // A whole number of frames, so no time falls between two.
        let media = BuiltinEffect::MediaSource { media: item.0.to_string(), offset: 12.0 / FPS as f32 };
        let payload = serde_json::to_value(&media).unwrap();
        let node: klipp_graph_format::Node = serde_json::from_value(serde_json::json!({
            "id": klipp_graph_format::NodeId::new_v4(),
            "type_id": klipp_graph_lift::names::effect_node_type("media_source"),
            "legacy": { "payload": payload },
        }))
        .unwrap();
        let id = node.id;
        c.apply_picture_ops(&[
            klipp_graph_lift::ops::GraphOp::AddNodeBeside { at: at(source), node: Box::new(node) },
            klipp_graph_lift::ops::GraphOp::Connect { from: at(source), from_port: "out".into(), to: at(id), to_port: "in".into() },
            klipp_graph_lift::ops::GraphOp::Connect { from: at(id), from_port: "out".into(), to: at(merge), to_port: "fg".into() },
        ])
        .unwrap();
    }
    let mut shown = t.clone();
    shown.seq = klipp_timeline::flatten::flatten_sequence(&t.seq, std::slice::from_ref(&t.seq));
    let out = exported(&shown);
    let viewer = Viewer::start(&shown);
    let mut worst = 0;
    let mut frames = Vec::new();
    // The item's frame its bar names: where along the band it is brightest.
    let item_frame = |rgb: &[u8]| (2..W - 2).max_by_key(|x| px(rgb, *x, 12).iter().sum::<i32>()).map_or(-1, |x| i64::from(x / 8));
    // V1's clips start at 0 and at 100, from their sources' 0 and 25.
    for (n, clip_frame) in [(10, 10), (60, 60), (110, 10)] {
        let (v, e) = (viewer.frame_at(n), exported_frame(&out, n));
        worst = worst.max(compare(&format!("merged media item frame {n:3}"), &v, &e));
        frames.push((n, clip_frame + 12, item_frame(&v), item_frame(&e)));
    }
    eprintln!("merged media item, viewer vs export: worst {worst} levels; (frame, item frame wanted, viewer's, export's): {frames:?}");
    assert!(frames.iter().all(|(_, want, v, e)| v == want && e == want), "the item's frame drawn, (frame, wanted, viewer's, export's): {frames:?}");
    assert!(worst <= 3, "the export differs from the viewer by {worst} levels; (frame, item frame wanted, viewer's, export's): {frames:?}");
    let (_, plain) = main_fixture();
    let (g, p) = (exported_frame(&out, 10), exported_frame(plain, 10));
    let (a, b) = (px(&g, 4, 40), px(&p, 4, 40));
    let d = (0..3).map(|c| (a[c] - b[c]).abs()).max().unwrap_or(0);
    assert!(d > 20, "only {d} levels below the band: the item is not in the export");
}

/// The main timeline with V2's scaled clip merged by Difference with
/// what is below it (#1746): a Clip Below beside its chain wired into the
/// merge's `fg`, as the Picture view's Sources menu makes it. The clip is
/// drawn over the composite below on both routes, and it is in the export.
#[test]
fn a_clip_merged_with_what_is_below_exports_as_the_viewer_shows_it() {
    let _decoders = decoders_at_speed();
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    use klipp_graph_format::path::{NodePath, Root};
    let mut t = timeline();
    for c in t.seq.tracks.iter_mut().find(|t| t.name == "V2").unwrap().clips.iter_mut() {
        add_after_display(c, &[BuiltinEffect::Merge { blend_mode: klipp_effects::BlendMode::Difference, opacity: 1.0 }]);
        let graph = c.picture_v2().unwrap().picture.graph.clone();
        let at = |id| NodePath::new(Root::Clip { clip: c.id.0 }, [id]);
        let source = graph.nodes.iter().find(|n| n.type_id.as_str() == klipp_graph_format::lifted::types::SOURCE).unwrap().id;
        let merge = graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("merge")).unwrap().id;
        let payload = serde_json::to_value(BuiltinEffect::ClipBelow {}).unwrap();
        let node: klipp_graph_format::Node = serde_json::from_value(serde_json::json!({
            "id": klipp_graph_format::NodeId::new_v4(),
            "type_id": klipp_graph_lift::names::effect_node_type("clip_below"),
            "legacy": { "payload": payload },
        }))
        .unwrap();
        let id = node.id;
        c.apply_picture_ops(&[
            klipp_graph_lift::ops::GraphOp::AddNodeBeside { at: at(source), node: Box::new(node) },
            klipp_graph_lift::ops::GraphOp::Connect { from: at(source), from_port: "out".into(), to: at(id), to_port: "in".into() },
            klipp_graph_lift::ops::GraphOp::Connect { from: at(id), from_port: "out".into(), to: at(merge), to_port: "fg".into() },
        ])
        .unwrap();
    }
    let mut shown = t.clone();
    shown.seq = klipp_timeline::flatten::flatten_sequence(&t.seq, std::slice::from_ref(&t.seq));
    let out = exported(&shown);
    let viewer = Viewer::start(&shown);
    let mut worst = 0;
    for n in [40, 60, 110] {
        let (v, e) = (viewer.frame_at(n), exported_frame(&out, n));
        worst = worst.max(compare(&format!("clip below frame {n:3}"), &v, &e));
    }
    eprintln!("clip below, viewer vs export: worst {worst} levels");
    assert!(worst <= 3, "the export differs from the viewer by {worst} levels");
    let (_, plain) = main_fixture();
    let (g, p) = (exported_frame(&out, 60), exported_frame(plain, 60));
    // Inside V2's clip, outside V3's smaller one over the centre.
    let (a, b) = (px(&g, W / 4, H / 2), px(&p, W / 4, H / 2));
    let d = (0..3).map(|c| (a[c] - b[c]).abs()).max().unwrap_or(0);
    assert!(d > 20, "only {d} levels inside V2's clip: the clip below is not in the export");
}

/// The main timeline with a grade in V1's clips' chains (#1746), after a
/// glow: a compound holding a Corrector that lifts the exposure and takes
/// saturation down. Both routes run it alike, and it is in the export.
#[test]
fn a_grade_compound_exports_as_the_viewer_shows_it() {
    let _decoders = decoders_at_speed();
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    use klipp_graph_format::path::{NodePath, Root};
    let mut t = timeline();
    let mut grade = klipp_nodes::NodeGraph::default();
    let corrector = grade.ensure_corrector();
    let n = grade.nodes.iter_mut().find(|n| n.id == corrector).unwrap();
    n.params.exposure = Some(0.6);
    n.params.saturation = Some(0.3);
    for c in t.seq.tracks.iter_mut().find(|t| t.name == "V1").unwrap().clips.iter_mut() {
        add_after_display(c, &[BuiltinEffect::Glow { radius: 12.0, intensity: 0.4, threshold: 0.0 }]);
        let graph = c.picture_v2().unwrap().picture.graph.clone();
        let glow = graph.nodes.iter().find(|n| klipp_graph_lift::effect_params::tag(n) == Some("glow")).unwrap().id;
        let compound = klipp_graph_lift::grade_compound::compound_of(&grade, klipp_graph_format::NodeId::new_v4()).unwrap();
        c.apply_picture_ops(&[klipp_graph_lift::ops::GraphOp::AddNode { after: NodePath::new(Root::Clip { clip: c.id.0 }, [glow]), node: Box::new(compound) }]).unwrap();
    }
    let mut shown = t.clone();
    shown.seq = klipp_timeline::flatten::flatten_sequence(&t.seq, std::slice::from_ref(&t.seq));
    let out = exported(&shown);
    let viewer = Viewer::start(&shown);
    let mut worst = 0;
    for n in [10, 60, 110] {
        let (v, e) = (viewer.frame_at(n), exported_frame(&out, n));
        worst = worst.max(compare(&format!("grade compound frame {n:3}"), &v, &e));
    }
    eprintln!("grade compound, viewer vs export: worst {worst} levels");
    assert!(worst <= 3, "the export differs from the viewer by {worst} levels");
    let (_, plain) = main_fixture();
    let (g, p) = (exported_frame(&out, 10), exported_frame(plain, 10));
    let (a, b) = (px(&g, 30, 30), px(&p, 30, 30));
    let d = (0..3).map(|c| (a[c] - b[c]).abs()).max().unwrap_or(0);
    assert!(d > 20, "only {d} levels at the corner: the compound is not in the export");
}

/// `fx` added to `c`'s picture as the Picture view adds them: each after
/// the last, from the display step.
fn add_after_display(c: &mut Clip, fx: &[BuiltinEffect]) {
    use klipp_graph_format::path::{NodePath, Root};
    let picture = c.picture_v2().unwrap().picture.clone();
    let mut after = picture.graph.nodes.iter().find(|n| n.type_id.as_str() == klipp_graph_format::lifted::types::SCENE_TO_DISPLAY).unwrap().id;
    let mut ops = Vec::new();
    for e in fx {
        let payload = serde_json::to_value(e).unwrap();
        let node: klipp_graph_format::Node = serde_json::from_value(serde_json::json!({
            "id": klipp_graph_format::NodeId::new_v4(),
            "type_id": klipp_graph_lift::names::effect_node_type(payload["type"].as_str().unwrap()),
            "legacy": { "payload": payload },
        }))
        .unwrap();
        let id = node.id;
        ops.push(klipp_graph_lift::ops::GraphOp::AddNode { after: NodePath::new(Root::Clip { clip: c.id.0 }, [after]), node: Box::new(node) });
        after = id;
    }
    c.apply_picture_ops(&ops).unwrap();
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
    let _decoders = decoders_at_speed();
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

/// A clip whose height is keyed apart from its width (#1471): its width
/// keyed 0.8 to 1.2 and its height 0.3 to 0.5 over its two seconds. At
/// the frames checked the width nearly fills the frame and the height
/// leaves bars above and below, in the viewer and the export alike; a
/// height that followed the width would leave none.
#[test]
fn a_height_keyed_apart_is_drawn_alike_in_the_viewer_and_the_export() {
    let _decoders = decoders_at_speed();
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let mut t = one_clip_timeline("height.mkv", (W, H), 0);
    let c = &mut t.seq.tracks[0].clips[0];
    *c = TestClip::of(c.clone()).lane("scale", 0.0, 0.8).lane("scale", 2.0, 1.2).lane("scale_y", 0.0, 0.3).lane("scale_y", 2.0, 0.5).build();
    fits_alike(&t, &[
        ("top bar", W / 2, H / 5, Expect::Bar),
        ("bottom bar", W / 2, H - H / 5, Expect::Bar),
        ("left", W / 8, H / 2, Expect::Red),
        ("right", W - W / 8, H / 2, Expect::Blue),
    ]);
}

/// A picture wider than the sequence is letterboxed by both.
#[test]
fn a_wide_clip_is_letterboxed_in_the_viewer_and_the_export_alike() {
    let _decoders = decoders_at_speed();
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
    let _decoders = decoders_at_speed();
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
    let _decoders = decoders_at_speed();
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

/// #617: the main timeline with b reversed — it walks its range back
/// from its last frame, entering the dissolve from past its end. The
/// export shows it backward, and the viewer shows the export's frames:
/// parked on it, in the dissolve, and after playing through it.
#[test]
fn a_reversed_clip_shows_as_it_exports() {
    let _decoders = decoders_at_speed();
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let mut t = timeline();
    let b = t.seq.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).find(|c| c.timeline_in == tc(100)).expect("clip b");
    b.speed = -1.0;
    let out = exported(&t);

    // b's green rises five levels a source frame: reversed, it falls a
    // frame of timeline at a time.
    let green = |n: i64| px(&exported_frame(&out, n), 30, 30)[1];
    assert_eq!(green(120) - green(121), 5, "the export plays b backward: {} then {}", green(120), green(121));

    let viewer = Viewer::start(&t);
    let mut worst = 0;
    for n in [120, 160, LEN - 1, 100, 95, 121] {
        worst = worst.max(compare(&format!("reversed frame {n:3}"), &viewer.frame_at(n), &exported_frame(&out, n)));
    }
    // Played within it, and into it from a through the dissolve: the
    // playhead runs at the timeline's rate, not off to the end.
    for from in [115, 70] {
        viewer.frame_at(from);
        let played = viewer.play(from, Duration::from_millis(1600));
        let (v, n) = viewer.settled_frame_at();
        // Correctness: it moved, and never ran far past the time it played
        // (the playhead off to the end). How fast it went is performance:
        // held only on a quiet machine.
        let at_rate = played.as_secs_f64() * FPS as f64;
        assert!(n > from && (n as f64) < from as f64 + at_rate + 10.0, "playback from {from} is at {n} after {played:?} at 25 fps");
        if quiet_machine() {
            assert!(n as f64 > from as f64 + at_rate * 0.5, "playback from {from} reached only {n} in {played:?}: under half the timeline's rate");
        } else {
            eprintln!("skipped the rate check (the machine is loaded): playback from {from} at {n} after {played:?}");
        }
        worst = worst.max(compare(&format!("played {from}->{n:3}"), &v, &exported_frame(&out, n)));
    }
    eprintln!("reversed clip, viewer vs export: worst {worst} levels");
    assert!(worst <= 3, "the viewer differs from the export by {worst} levels");
}

/// Whether the machine is quiet enough for a rate to mean anything: the
/// last minute's load under half its cores.
fn quiet_machine() -> bool {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as f64;
    std::fs::read_to_string("/proc/loadavg").ok().and_then(|l| l.split_whitespace().next()?.parse::<f64>().ok()).is_some_and(|load| load < cores * 0.5)
}

/// #696: footage at another rate than the sequence's shows the same frame
/// of the file in the viewer as in the export. Two 10 fps sources in the
/// 25 fps sequence, a new colour each frame of the file: one across V1
/// (the primary decoder) and one over its middle on V2 (a clip stream).
/// Export shows a file's frame from half of *its* frame before it; the
/// viewer took half a sequence frame, and at frame 4 (0.16 s) the viewer
/// still showed the file's 0.1 s while export showed 0.2 s. Parked, and
/// stopped after playing.
#[test]
fn footage_at_another_rate_shows_the_same_file_frame_as_the_export() {
    let _decoders = decoders_at_speed();
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let (under, over) = (MediaId::new(), MediaId::new());
    let files = HashMap::from([
        (under, rgb_source_at("ten-under.mkv", (W, H), 10, "mod(N*20,200)+30", "90", "90")),
        (over, rgb_source_at("ten-over.mkv", (W, H), 10, "90", "90", "mod(N*20,200)+30")),
    ]);
    let mut seq = Sequence::new_default();
    seq.fps = Fps::new(FPS as u32, 1);
    seq.resolution = Resolution { width: W, height: H };
    let track = |name: &str, clips: Vec<Clip>| {
        let mut t = Track::new(name, TrackKind::Video);
        t.clips = clips;
        t
    };
    seq.tracks = vec![
        track("V2", vec![scaled(media_clip(over, 0, LEN, 0), 0.6, 0.0, 0.0)]),
        track("V1", vec![media_clip(under, 0, LEN, 0)]),
    ];
    let t = Timeline { seq, files, coded: HashMap::new() };
    let out = exported(&t);
    let viewer = Viewer::start(&t);

    // Frames where half a sequence frame and half a file frame pick
    // different frames of the file (4, 9, 14 …), and some where they agree.
    let mut worst = 0;
    for n in [4, 9, 14, 19, 24, 2, 7] {
        worst = worst.max(compare(&format!("10 fps, frame {n:3}"), &viewer.frame_at(n), &exported_frame(&out, n)));
    }
    // Played, then stopped: the frame it stops on is the export's.
    viewer.cmd.send(AppCommand::SetPlayhead(tc(30))).unwrap();
    let _ = viewer.frame_at(30);
    viewer.play(30, Duration::from_millis(700));
    let (v, n) = viewer.settled_frame_at();
    worst = worst.max(compare(&format!("10 fps, played to {n:3}"), &v, &exported_frame(&out, n)));
    eprintln!("footage at another rate, viewer vs export: worst {worst} levels");
    assert!(worst <= 3, "the viewer shows another frame of the file than the export: {worst} levels");
}

/// #2556: footage at a higher rate than the sequence's shows, playing and
/// parked, the file's frame the export shows for each timeline frame. A
/// 30 fps source in the 25 fps sequence, a new colour each frame of the
/// file: playback showed every one of the file's frames as it came, at
/// its own time, not the one export renders for the timeline's frame.
#[test]
fn faster_footage_plays_the_file_frame_the_export_shows() {
    let _decoders = decoders_at_speed();
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let id = MediaId::new();
    let files = HashMap::from([(id, rgb_source_at("thirty.mkv", (W, H), 30, "mod(N*7,200)+30", "90", "90"))]);
    let mut seq = Sequence::new_default();
    seq.fps = Fps::new(FPS as u32, 1);
    seq.resolution = Resolution { width: W, height: H };
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![media_clip(id, 0, LEN, 0)];
    seq.tracks = vec![v1];
    let t = Timeline { seq, files, coded: HashMap::new() };
    let out = exported(&t);
    let viewer = Viewer::start(&t);
    let mut worst = 0;
    for n in [3, 5, 8, 13, 21] {
        worst = worst.max(compare(&format!("30 fps, frame {n:3}"), &viewer.frame_at(n), &exported_frame(&out, n)));
    }
    viewer.cmd.send(AppCommand::SetPlayhead(tc(10))).unwrap();
    let _ = viewer.frame_at(10);
    viewer.play(10, Duration::from_millis(700));
    let (v, n) = viewer.settled_frame_at();
    worst = worst.max(compare(&format!("30 fps, played to {n:3}"), &v, &exported_frame(&out, n)));
    eprintln!("faster footage, viewer vs export: worst {worst} levels");
    assert!(worst <= 3, "the viewer shows another frame of the file than the export: {worst} levels");
}

/// #3289: parked one frame on, onto a timeline frame whose file frame
/// began before it, the viewer shows the export's frame of the file. A
/// 30 fps source in the 25 fps sequence: timeline frame 21 is 0.84 s, and
/// the file's frame on show then began at 0.833 s. Parked first at 20,
/// the decoder has decoded on past it, so the move to 21 is answered from
/// its queue, which kept only frames from 0.84 s on and showed 0.867 s,
/// a frame of the file late (7 levels on this source's red).
#[test]
fn a_park_one_frame_on_shows_the_file_frame_begun_before_it() {
    let _decoders = decoders_at_speed();
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let id = MediaId::new();
    let files = HashMap::from([(id, rgb_source_at("thirty-step.mkv", (W, H), 30, "mod(N*7,200)+30", "90", "90"))]);
    let mut seq = Sequence::new_default();
    seq.fps = Fps::new(FPS as u32, 1);
    seq.resolution = Resolution { width: W, height: H };
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![media_clip(id, 0, LEN, 0)];
    seq.tracks = vec![v1];
    let t = Timeline { seq, files, coded: HashMap::new() };
    let out = exported(&t);
    let viewer = Viewer::start(&t);
    let mut worst = 0;
    for (from, to) in [(20, 21), (16, 17), (6, 7)] {
        let _ = viewer.frame_at(from);
        // The decoder runs on past the parked frame before it pauses.
        std::thread::sleep(Duration::from_millis(300));
        worst = worst.max(compare(&format!("parked {from} then {to:3}"), &viewer.frame_at(to), &exported_frame(&out, to)));
    }
    assert!(worst <= 3, "parked one frame on, the viewer shows another frame of the file than the export: {worst} levels");
}

/// A lossless 10-bit PQ clip (BT.2020, ST 2084) at the sequence's size:
/// luma ramping across the frame from 1 to about 2000 nits, a bright
/// square in the middle for a glow to spread from, neutral chroma. Two
/// seconds at the sequence's rate.
fn pq_source(name: &str) -> PathBuf {
    let pq = |nits: f64| {
        let (m1, m2) = (2610.0 / 16384.0, 2523.0 / 4096.0 * 128.0);
        let (c1, c2, c3) = (3424.0 / 4096.0, 2413.0 / 4096.0 * 32.0, 2392.0 / 4096.0 * 32.0);
        let y = (nits / 10000.0f64).powf(m1);
        (64.0 + ((c1 + c2 * y) / (1.0 + c3 * y)).powf(m2) * 876.0).round() as u16
    };
    let raw = tmp(&format!("{}-{name}.yuv", uuid::Uuid::new_v4()));
    let out = tmp(&format!("{}-{name}.mkv", uuid::Uuid::new_v4()));
    let mut frame = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let centre = x.abs_diff(W / 2) < 40 && y.abs_diff(H / 2) < 40;
            let nits = if centre { 3000.0 } else { 10f64.powf(3.3 * x as f64 / W as f64) };
            frame.extend_from_slice(&pq(nits).to_le_bytes());
        }
    }
    for _ in 0..(W / 2) * H * 2 {
        frame.extend_from_slice(&512u16.to_le_bytes());
    }
    std::fs::write(&raw, frame.repeat(2 * FPS as usize)).unwrap();
    let tags = ["-color_range", "tv", "-colorspace", "bt2020nc", "-color_primaries", "bt2020", "-color_trc", "smpte2084"];
    let size = format!("{W}x{H}");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-y", "-v", "error", "-f", "rawvideo", "-pix_fmt", "yuv422p10le", "-s", &size, "-r", &FPS.to_string()])
        .args(tags)
        .args(["-i", raw.to_str().unwrap(), "-c:v", "ffv1", "-pix_fmt", "yuv422p10le"])
        .args(tags)
        .arg(&out)
        .status()
        .unwrap()
        .success();
    let _ = std::fs::remove_file(&raw);
    assert!(ok, "ffmpeg could not make {name}");
    out
}

/// The worst difference between two frames, in levels, over every pixel.
fn worst_levels(a: &[u8], b: &[u8]) -> i32 {
    a.iter().zip(b).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap_or(0)
}

/// A graded PQ clip with effects shows the same picture in the viewer and
/// the export. Both render it through the clip renderer, which since phase
/// 2b M2 step 11c runs its grade, display step and effects as one plan
/// (the route itself is pinned by klipp-compose's `effects_follow_grade`).
/// The effects are in it: a glow spreads from the bright square and a
/// vignette darkens the corners, against the same clip without them.
#[test]
fn a_graded_hdr_clip_with_effects_exports_as_the_viewer_shows_it() {
    let _decoders = decoders_at_speed();
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let p = MediaId::new();
    let file = pq_source("pq-effects");
    let make = |effects: Vec<BuiltinEffect>| {
        let mut seq = Sequence::new_default();
        seq.fps = Fps::new(FPS as u32, 1);
        seq.resolution = Resolution { width: W, height: H };
        let mut clip = media_clip(p, 0, 40, 0);
        clip.set_grade(exposure(-0.5));
        let clip = TestClip::of(clip).effects(effects).build();
        let mut v1 = Track::new("V1", TrackKind::Video);
        v1.clips = vec![clip];
        seq.tracks = vec![v1];
        Timeline { seq, files: HashMap::from([(p, file.clone())]), coded: HashMap::new() }
    };
    let t = make(vec![
        BuiltinEffect::Glow { radius: 12.0, intensity: 0.8, threshold: 0.0 },
        BuiltinEffect::Vignette { amount: -0.8, size: 0.5, softness: 0.4, roundness: 0.7 },
    ]);
    let out = exported(&t);
    let viewer = Viewer::start(&t);
    let mut worst = 0;
    for n in [5, 20, 35] {
        let (v, e) = (viewer.frame_at(n), exported_frame(&out, n));
        let w = worst_levels(&v, &e);
        eprintln!("HDR with effects, frame {n}: worst {w} levels");
        worst = worst.max(w);
    }
    assert!(worst <= 3, "the export differs from the viewer by {worst} levels");
    let plain = exported(&make(vec![]));
    let (g, p) = (exported_frame(&out, 20), exported_frame(&plain, 20));
    assert!(worst_levels(&g, &p) > 20, "the effects are in the export");
    let corner = |f: &[u8]| px(f, 4, 4).iter().sum::<i32>();
    assert!(corner(&g) < corner(&p), "the vignette darkens the corner: {} vs {}", corner(&g), corner(&p));
}

/// A real file's neighbours are native-sized even when the viewer is smaller.
/// Both the first media frame and an interior one must actually be denoised.
#[test]
fn temporal_denoise_uses_native_neighbours_in_a_smaller_viewer() {
    let _decoders = decoders_at_speed();
    assert!(ffmpeg_ok(), "FFmpeg is required for the temporal size regression");
    let id = MediaId::new();
    // A larger 4:3 source is fitted into the 16:9 canvas. A same-aspect
    // parked source intentionally keeps its decoded resolution in the viewer.
    let coded = (W * 2, W * 3 / 2);
    let level = "90+16*eq(mod(N,3),1)";
    let path = rgb_source_sized("temporal-sized.mkv", coded, level, level, level);
    let mut seq = Sequence::new_default();
    seq.fps = Fps::new(FPS as u32, 1);
    seq.resolution = Resolution { width: W, height: H };
    let mut track = Track::new("V", TrackKind::Video);
    track.clips.push(media_clip(id, 0, LEN, 0));
    seq.tracks = vec![track];
    let plain = Timeline { seq, files: HashMap::from([(id, path)]), coded: HashMap::from([(id, (coded.0, coded.1, 0))]) };
    let mut denoised = plain.clone();
    denoised.seq.tracks[0].clips[0] = TestClip::of(denoised.seq.tracks[0].clips[0].clone())
        .effect(BuiltinEffect::TemporalDenoise { strength: 1.0, threshold: 0.15 }).build();
    let baseline = Viewer::start(&plain);
    let subject = Viewer::start(&denoised);
    for (frame, brighter) in [(0, true), (1, false), (6, true)] {
        let before = px(&baseline.frame_at(frame), W / 2, H / 2)[0];
        let after = px(&subject.frame_at(frame), W / 2, H / 2)[0];
        let change = if brighter { after - before } else { before - after };
        assert!(change > 1, "frame {frame}: viewer did not use its neighbours ({before} -> {after})");
    }
}

/// Y'CbCr code values of the patches, left to right: black and white at
/// both ranges' ends, mid grey, and a colour that BT.601 and BT.709 read
/// 7 levels apart in red. As in klipp-render's export parity tests.
const PATCHES: [[u8; 3]; 6] = [[0, 128, 128], [16, 128, 128], [128, 128, 128], [235, 128, 128], [255, 128, 128], [120, 100, 170]];

/// The coded size of [`patch_clip`]'s clips, pillarboxed in the sequence
/// to 480×360 between x = 80 and 560.
const PATCH_SIZE: (u32, u32) = (960, 720);

/// The sRGB-coded colour a correct decode gives `code`.
fn patch_rgb([y, u, v]: [u8; 3], full: bool, bt601: bool) -> [i32; 3] {
    let (y, cb, cr) = if full {
        (y as f64 / 255.0, (u as f64 - 128.0) / 255.0, (v as f64 - 128.0) / 255.0)
    } else {
        ((y as f64 - 16.0) / 219.0, (u as f64 - 128.0) / 224.0, (v as f64 - 128.0) / 224.0)
    };
    let (kr, kb) = if bt601 { (0.299, 0.114) } else { (0.2126, 0.0722) };
    let r = y + 2.0 * (1.0 - kr) * cr;
    let b = y + 2.0 * (1.0 - kb) * cb;
    let g = (y - kr * r - kb * b) / (1.0 - kr - kb);
    [r, g, b].map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as i32)
}

/// Two seconds of lossless H.264 with [`PATCHES`] as vertical bands,
/// 4:2:0, tagged with `tags`: the decode route a screen recording takes.
/// Coded 720 lines high, as an untagged file under 720 lines is taken to
/// have SD primaries and a recording of a screen is not; and 4:3, as a
/// parked viewer shows a same-aspect clip at its own size.
fn patch_clip(name: &str, tags: &[&str]) -> PathBuf {
    let (w, h) = PATCH_SIZE;
    let raw = tmp(&format!("{}-{name}.yuv", uuid::Uuid::new_v4()));
    let out = tmp(&format!("{}-{name}.mkv", uuid::Uuid::new_v4()));
    let band = |x: u32| PATCHES[(x * PATCHES.len() as u32 / w) as usize];
    let mut frame = Vec::with_capacity((w * h * 3 / 2) as usize);
    for _ in 0..h { frame.extend((0..w).map(|x| band(x)[0])); }
    for c in 1..3 {
        for _ in 0..h / 2 { frame.extend((0..w / 2).map(|x| band(x * 2)[c])); }
    }
    std::fs::write(&raw, frame.repeat(2 * FPS as usize)).unwrap();
    let size = format!("{w}x{h}");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-y", "-v", "error", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-s", &size, "-r", &FPS.to_string()])
        .args(tags)
        .args(["-i", raw.to_str().unwrap(), "-c:v", "libx264", "-qp", "0", "-pix_fmt", "yuv420p"])
        .args(tags)
        .arg(&out)
        .status()
        .unwrap()
        .success();
    let _ = std::fs::remove_file(&raw);
    assert!(ok, "ffmpeg could not make {name}");
    out
}

/// A full-range source shows at its own levels, in the viewer as in the
/// export: black stays black and white white, nothing lifted or clipped,
/// its colour read through the matrix it is tagged with. Screen recorders
/// write full range with BT.601 and no transfer tag, phones full range
/// BT.709. A limited-range clip is the control.
#[test]
fn a_full_range_source_shows_and_exports_at_its_code_values() {
    let _decoders = decoders_at_speed();
    if !ffmpeg_ok() {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let cases: [(&str, &[&str], bool, bool); 3] = [
        ("full-601-untagged", &["-color_range", "pc", "-colorspace", "bt470bg"], true, true),
        ("full-709", &["-color_range", "pc", "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"], true, false),
        ("limited-709", &["-color_range", "tv", "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"], false, false),
    ];
    for (name, tags, full, bt601) in cases {
        let id = MediaId::new();
        let mut seq = Sequence::new_default();
        seq.fps = Fps::new(FPS as u32, 1);
        seq.resolution = Resolution { width: W, height: H };
        let mut v1 = Track::new("V1", TrackKind::Video);
        v1.clips = vec![media_clip(id, 0, 40, 0)];
        seq.tracks = vec![v1];
        let coded = HashMap::from([(id, (PATCH_SIZE.0, PATCH_SIZE.1, 0))]);
        let t = Timeline { seq, files: HashMap::from([(id, patch_clip(name, tags))]), coded };
        let out = exported(&t);
        let viewer = Viewer::start(&t);
        let (v, e) = (viewer.frame_at(20), exported_frame(&out, 20));
        let n = PATCHES.len() as u32;
        let mut worst = 0;
        for (i, code) in PATCHES.into_iter().enumerate() {
            let want = patch_rgb(code, full, bt601);
            let x = 80 + (2 * i as u32 + 1) * 480 / (2 * n);
            let (got_v, got_e) = (px(&v, x, H / 2), px(&e, x, H / 2));
            eprintln!("{name}: code {code:?} want {want:?} viewer {got_v:?} export {got_e:?}");
            for got in [got_v, got_e] {
                worst = worst.max((0..3).map(|c| (got[c] - want[c]).abs()).max().unwrap());
            }
        }
        assert!(worst <= 3, "{name}: the viewer or the export is {worst} levels off its source's code values");
    }
}
