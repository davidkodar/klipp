//! A timeline skim shows the picture (#2945): the title over a clip, its
//! effect, the title alone where there is no clip, a title half way into
//! its entrance and the middle of a wipe, each as the parked frame at the
//! same time shows it, drawn by the GPU stack from the skim files with no
//! source read back. It showed the raw clip under the cursor, the title
//! and its effect missing until the cursor rested.
//!
//! Each clip's skim file is its own file here, so the skim and the parked
//! frame read the same pixels and differ only by the way they are drawn.
//! Needs the ffmpeg CLI to write the sources; skips without it.

mod common;

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 640;
const H: u32 = 360;

fn tc(frame: i64) -> Timecode {
    common::frame_tc(frame, Fps::new(common::FPS, 1))
}

/// A media clip of `media` from `from` to `to`, read from its start.
fn shot(media: MediaId, from: i64, to: i64) -> Clip {
    Clip { kind: ClipKind::Media { media_id: media }, source_in: tc(0), source_out: tc(to - from), ..Clip::gap(tc(from), tc(to)) }
}

/// A large white title from `from` to `to`, rising in over its first 20
/// frames when `entrance`.
fn title(from: i64, to: i64, entrance: bool) -> Clip {
    let mut td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "SKIM".into(), font_size: 120.0, color: [255, 255, 255, 255], ..Default::default() });
    if entrance {
        td.objects[0].fx_mut().motion.enter = Some(klipp_core::title_motion::Motion::new(klipp_core::title_motion::MotionKind::Rise).duration(0.8));
    }
    Clip { kind: ClipKind::Title(td), ..Clip::gap(tc(from), tc(to)) }
}

struct Viewer {
    cmd: crossbeam_channel::Sender<AppCommand>,
    events: crossbeam_channel::Receiver<AppEvent>,
    fb: Arc<Mutex<FrameBuffer>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Viewer {
    fn start(seq: Sequence, pool: MediaPool) -> Self {
        let (cmd, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, events) = crossbeam_channel::unbounded::<AppEvent>();
        let fb = Arc::new(Mutex::new(FrameBuffer::new(W, H)));
        let engine = PlaybackEngine::new(
            cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(W, H))),
            Arc::new(klipp_timeline::SharedSequence::new(seq)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
            Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
        );
        let thread = Some(std::thread::spawn(move || engine.run()));
        cmd.send(AppCommand::Pause).unwrap();
        Self { cmd, events, fb, thread }
    }

    /// The newest frame's bytes.
    fn bytes(&self) -> Vec<u8> {
        let mut fb = self.fb.lock().unwrap();
        fb.swap_pending();
        fb.latest_frame().0.to_vec()
    }

    /// The skim at `frame`: whether the engine drew it, and its bytes.
    fn skim(&self, frame: i64) -> (bool, Vec<u8>) {
        while self.events.try_recv().is_ok() {}
        self.cmd.send(AppCommand::SkimTimeline { time: tc(frame) }).unwrap();
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(20) {
            if let Ok(AppEvent::TimelineSkimShown { composited, .. }) = self.events.recv_timeout(Duration::from_millis(50)) {
                return (composited, self.bytes());
            }
        }
        panic!("no answer to the skim at frame {frame}");
    }

    /// The parked frame at `frame`.
    fn parked(&self, frame: i64) -> Vec<u8> {
        self.cmd.send(AppCommand::SeekTo(tc(frame))).unwrap();
        let t0 = Instant::now();
        loop {
            let done = {
                let fb = self.fb.lock().unwrap();
                fb.latest_presented() == Some(tc(frame)) && fb.latest_complete()
            };
            if done || t0.elapsed() > Duration::from_secs(20) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        common::settle(&self.fb);
        self.bytes()
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        let _ = self.cmd.send(AppCommand::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));
    }
}

/// The largest difference between two frames, any channel, over the
/// frame sampled every 4 pixels.
fn worst(a: &[u8], b: &[u8]) -> u8 {
    assert_eq!(a.len(), b.len(), "frames of one size");
    a.chunks(4).zip(b.chunks(4)).step_by(4).map(|(p, q)| (0..3).map(|c| p[c].abs_diff(q[c])).max().unwrap()).max().unwrap()
}

#[test]
fn a_skim_shows_titles_effects_and_transitions_as_parked() {
    let required = std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1");
    let (Some(a), Some(b)) = (common::hevc_source("skim-pic-a", W, H, common::SDR_TAGS), common::hevc_source("skim-pic-b", W, H, common::SDR_TAGS)) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let fps = Fps::new(common::FPS, 1);
    let mut pool = MediaPool::default();
    let mut add = |path: &std::path::Path| {
        let id = MediaId::new();
        pool.add(MediaItem {
            id,
            path: path.to_path_buf(),
            kind: klipp_media::MediaItemKind::File,
            resolution: Resolution { width: W, height: H },
            skim_file: Some(path.to_path_buf()),
            ..MediaItem::new_synthetic_title(id, path.display().to_string(), tc(50), fps)
        });
        id
    };
    let (left, right) = (add(&a), add(&b));
    // V1: a to 40, wiped into b over 30..40, b to 50; then nothing to 75.
    // T: a title with a reflection over 0..25, then one rising in alone
    // over 55..75.
    let mut v1 = Track::new("V1", TrackKind::Video);
    let (sa, sb) = (shot(left, 0, 40), Clip { source_in: tc(10), source_out: tc(30), ..shot(right, 30, 50) });
    v1.transitions.push(klipp_timeline::Transition {
        id: TransitionId::new(),
        kind: TransitionKind::WipeLeft,
        duration: tc(10),
        clip_a_id: sa.id,
        clip_b_id: sb.id,
        user_params: Default::default(),
        alignment: TransitionAlignment::Center,
    });
    v1.clips = vec![sa, sb];
    let mut reflected = title(0, 25, false);
    reflected.push_effect(klipp_effects::registry::effect_type("reflection").map(|t| (t.sample)()).expect("the Reflection"));
    let mut t1 = Track::new("T1", TrackKind::Text);
    t1.clips = vec![reflected, title(55, 75, true)];
    let mut seq = Sequence::new_default();
    seq.fps = fps;
    seq.resolution = Resolution { width: W, height: H };
    seq.tracks = vec![t1, v1];

    let viewer = Viewer::start(seq, pool);
    let off_gpu = klipp_engine::engine::SKIM_SOURCES_OFF_GPU.load(Ordering::Relaxed);
    for (frame, what) in [(12, "the title and its reflection over the clip"), (35, "the middle of the wipe"), (65, "the title alone"), (60, "the title half way into its entrance")] {
        let (composited, skim) = viewer.skim(frame);
        if !composited && !required {
            eprintln!("UNVERIFIED: the skim at {frame} was not drawn (no GPU stack here)");
            return;
        }
        assert!(composited, "{what}: the skim at {frame} was not drawn\n{}", viewer.fb.lock().unwrap().trail().join("\n"));
        let parked = viewer.parked(frame);
        let off = worst(&skim, &parked);
        eprintln!("{what} (frame {frame}): skim vs parked, worst {off} levels");
        assert!(off <= 4, "{what}: the skim differs from the parked frame by {off} levels");
    }
    // The title is there, and moving: white on black where it is alone,
    // and not where it rests while it rises in.
    let (_, alone) = viewer.skim(65);
    let white = alone.chunks(4).filter(|p| p[0] > 200 && p[1] > 200 && p[2] > 200).count();
    assert!(white > 500, "the title alone shows: {white} white pixels");
    let (_, rising) = viewer.skim(60);
    let (_, rested) = viewer.skim(74);
    assert!(worst(&rising, &rested) > 100, "half way into its entrance the title is not where it rests");
    assert_eq!(klipp_engine::engine::SKIM_SOURCES_OFF_GPU.load(Ordering::Relaxed) - off_gpu, 0, "a skim source was read off the GPU");
}
