//! A clip with a speed ramp is stepped and redrawn through its ramp.
//!
//! A frame step turned the decoded frame's source time into a playhead by
//! dividing by the clip's base speed, while the renderer integrates the
//! speed keyframes: in a ramped clip one step put the playhead, and the
//! frame cached under it, somewhere else entirely. And the fingerprints
//! that decide whether a parked frame is still current hashed only how
//! many speed keyframes there were, so dragging one kept the old frame
//! on screen until the next seek (#423). A multicam cut is the same kind
//! of edit: the cut list alone changes which file is under the playhead.

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = common::RAMP_W;
const H: u32 = common::RAMP_H;

/// A ramp of `speed` from the clip's start to well past its end: every
/// playhead in the clip plays at `speed`, whatever the base speed says.
fn held_at(speed: f32) -> Vec<Keyframe> {
    [0.0, 10.0]
        .map(|t| Keyframe { time_secs: t, value: speed, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() })
        .to_vec()
}

struct Engine {
    cmd: crossbeam_channel::Sender<AppCommand>,
    fb: Arc<Mutex<FrameBuffer>>,
    seq: Arc<klipp_timeline::SharedSequence>,
    playhead: Arc<Mutex<Option<RationalTime>>>,
    thread: Option<klipp_engine::RunningEngine>,
}

impl Engine {
    fn generation(&self) -> u64 {
        self.fb.lock().unwrap().latest_generation()
    }

    /// Wait for a frame newer than `since`, then for the picture to stop
    /// changing.
    fn settle_after(&self, since: u64) -> bool {
        let t0 = std::time::Instant::now();
        while self.generation() == since {
            if t0.elapsed() > Duration::from_secs(10) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let mut g = self.generation();
        loop {
            std::thread::sleep(common::QUIET);
            let now = self.generation();
            if now == g {
                return true;
            }
            g = now;
        }
    }

    fn frame(&self) -> Vec<u8> {
        self.fb.lock().unwrap().latest_frame().0.to_vec()
    }

    /// The playhead the engine last reported, in frames.
    fn frame_number(&self) -> f64 {
        let tc = self.playhead.lock().unwrap().expect("no playhead reported");
        tc.to_secs_f64() * common::FPS as f64
    }
}

impl Drop for Engine {
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

/// The engine parked on timeline frame 5 of the luma ramp, played at 2x
/// by its speed keyframes over a base speed of 1: source frame 10.
fn parked_in_a_ramp() -> Option<Engine> {
    let src = common::ffv1_avi_ramp()?;
    let fps = Fps::new(common::FPS, 1);
    let tc = |f| common::frame_tc(f, fps);
    let id = MediaId::new();
    let mut clip = Clip { kind: ClipKind::Media { media_id: id }, ..Clip::gap(tc(0), tc(20)) };
    clip.source_in = tc(0);
    clip.source_out = tc(common::RAMP_N as i64);
    clip.speed_keyframes = held_at(2.0);
    let mut seq = Sequence::new_default();
    seq.fps = fps;
    seq.resolution = Resolution { width: W, height: H };
    let mut t = Track::new("V1", TrackKind::Video);
    t.clips = vec![clip];
    seq.tracks = vec![t];
    Some(parked_at_frame_5(seq, pool_of(id, &src, fps)))
}

/// The luma ramp as media `id`.
fn pool_of(id: MediaId, src: &std::path::Path, fps: Fps) -> MediaPool {
    let mut pool = MediaPool::default();
    pool.add(MediaItem {
        id,
        path: src.to_path_buf(),
        kind: klipp_media::MediaItemKind::File,
        resolution: Resolution { width: W, height: H },
        ..MediaItem::new_synthetic_title(id, src.display().to_string(), common::frame_tc(common::RAMP_N as i64, fps), fps)
    });
    pool
}

/// An engine on `seq`, parked on timeline frame 5.
fn parked_at_frame_5(seq: Sequence, pool: MediaPool) -> Engine {
    let fps = seq.fps;
    let tc = |f| common::frame_tc(f, fps);
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
    let fb = Arc::new(Mutex::new(FrameBuffer::new(W, H)));
    let seq = Arc::new(klipp_timeline::SharedSequence::new(seq));
    let engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(W, H))),
        Arc::clone(&seq), Arc::new(RwLock::new(pool)), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
    );
    let thread = Some(engine.spawn());
    let e = Engine { cmd, fb, seq, playhead, thread };
    let g = e.generation();
    e.cmd.send(AppCommand::SetPlayhead(tc(5))).unwrap();
    assert!(e.settle_after(g), "no first frame");
    e
}

#[test]
fn a_frame_step_in_a_ramp_lands_where_the_ramp_puts_it() {
    let Some(e) = parked_in_a_ramp() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let g = e.generation();
    e.cmd.send(AppCommand::SeekRelative(1)).unwrap();
    assert!(e.settle_after(g), "the step drew nothing");
    // The next decoded frame is source frame 10 or 11, which the ramp
    // shows at timeline frame 5 or 5.5. Divided by the base speed it
    // was put at 10 or 11.
    let at = e.frame_number();
    assert!((5.0..=6.0).contains(&at), "one step in a 2x ramp from frame 5 moved the playhead to frame {at:.2}");
}

#[test]
fn dragging_a_speed_keyframe_redraws_the_parked_frame() {
    let Some(e) = parked_in_a_ramp() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let before = e.frame();
    let g = e.generation();
    // The same two keyframes, now at 1x: frame 5 shows source frame 5,
    // not 10. The count is unchanged, and so is the base speed.
    e.seq.write().unwrap().tracks[0].clips[0].speed_keyframes = held_at(1.0);
    e.cmd.send(AppCommand::SequenceUpdated).unwrap();
    assert!(e.settle_after(g), "the keyframe drag rendered nothing");
    assert_ne!(e.frame(), before, "the parked frame is still the one decoded under the old ramp");
}

/// A multicam switch changes the primary's cut list and nothing else,
/// and the fingerprints left the cut list out: paused, the viewer kept
/// the old camera until something else made the engine look (#479).
/// Two angles of the ramp, twenty frames apart in the source.
#[test]
fn a_multicam_cut_redraws_the_parked_frame() {
    let Some(src) = common::ffv1_avi_ramp() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let fps = Fps::new(common::FPS, 1);
    let tc = |f| common::frame_tc(f, fps);
    let id = MediaId::new();
    let group = GroupId::new();
    let angle = |n: u8, source_in: i64| {
        let mut clip = Clip { kind: ClipKind::Media { media_id: id }, ..Clip::gap(tc(0), tc(20)) };
        clip.source_in = tc(source_in);
        clip.source_out = tc(source_in + 20);
        clip.multicam_group = Some(group);
        clip.multicam_angle = n;
        clip
    };
    let mut seq = Sequence::new_default();
    seq.fps = fps;
    seq.resolution = Resolution { width: W, height: H };
    seq.tracks = [angle(0, 0), angle(1, 20)]
        .into_iter()
        .enumerate()
        .map(|(i, c)| {
            let mut t = Track::new(&format!("V{}", i + 1), TrackKind::Video);
            t.clips = vec![c];
            t
        })
        .collect();
    let e = parked_at_frame_5(seq, pool_of(id, &src, fps));
    let before = e.frame();
    let g = e.generation();
    // Written as the UI writes most edits: to the shared sequence, with
    // nothing sent after. The engine's idle look at the layout is all
    // that can notice.
    e.seq.write().unwrap().tracks[0].clips[0].multicam_cuts = vec![(0.0, 1)];
    assert!(e.settle_after(g), "the cut rendered nothing");
    assert_ne!(e.frame(), before, "the parked frame is still the first camera's");
}
