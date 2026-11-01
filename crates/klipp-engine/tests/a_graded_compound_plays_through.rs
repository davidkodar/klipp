//! Playback carries on into a compound whose
//! sequence has a timeline grade, drawn whole as a precomposition (#370).

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = common::RAMP_W;
const H: u32 = common::RAMP_H;

fn item(id: MediaId, src: &std::path::Path, fps: Fps) -> MediaItem {
    MediaItem {
        id,
        path: src.to_path_buf(),
        kind: klipp_media::MediaItemKind::File,
        resolution: Resolution { width: W, height: H },
        ..MediaItem::new_synthetic_title(id, src.display().to_string(), common::frame_tc(common::RAMP_N as i64, fps), fps)
    }
}

fn shot(id: MediaId, from: i64, to: i64, source_in: i64, fps: Fps) -> Clip {
    let tc = |f| common::frame_tc(f, fps);
    Clip { kind: ClipKind::Media { media_id: id }, source_in: tc(source_in), source_out: tc(source_in + to - from), ..Clip::gap(tc(from), tc(to)) }
}

/// The furthest frame shown playing from frame 2, played until one past
/// `goal` seconds is shown (or a minute).
fn furthest_played(graded: bool, goal: f64) -> f64 {
    let (Some(a), Some(b)) = (common::ramp("p584-a", &["-c:v", "ffv1"], "avi"), common::ramp("p584-b", &["-c:v", "ffv1"], "avi")) else {
        return f64::NAN;
    };
    let fps = Fps::new(common::FPS, 1);
    let (ia, ib) = (MediaId::new(), MediaId::new());
    let mut pool = MediaPool::default();
    pool.add(item(ia, &a, fps));
    pool.add(item(ib, &b, fps));

    let mut inner = Sequence::new_default();
    (inner.fps, inner.resolution) = (fps, Resolution { width: W, height: H });
    let mut iv = Track::new("V1", TrackKind::Video);
    iv.clips = vec![shot(ib, 0, 20, 0, fps)];
    inner.tracks = vec![iv];
    if graded {
        let mut g = klipp_nodes::NodeGraph::default();
        g.nodes.iter_mut().for_each(|n| {
            if n.kind == klipp_nodes::NodeKind::Corrector {
                n.params.exposure = Some(-1.0);
            }
        });
        inner.timeline_grade_slot().set(Some(g));
    }

    let mut outer = Sequence::new_default();
    (outer.fps, outer.resolution) = (fps, Resolution { width: W, height: H });
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![shot(ia, 0, 20, 15, fps), Clip::sequence(inner.id, common::frame_tc(20, fps), common::frame_tc(40, fps))];
    outer.tracks = vec![v1];
    let flat = klipp_timeline::flatten::flatten_sequence(&outer, &[outer.clone(), inner.clone()]);
    eprintln!("graded={graded}: V1 kinds after flatten: {:?}", flat.tracks[0].clips.iter().map(|c| std::mem::discriminant(&c.kind)).collect::<Vec<_>>());

    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
    std::thread::spawn(move || while ev_rx.recv().is_ok() {});
    let fb = Arc::new(Mutex::new(FrameBuffer::new(W, H)));
    let engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(W, H))),
        Arc::new(klipp_timeline::SharedSequence::new(flat)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
    );
    let thread = engine.spawn();
    // A look inside the compound first: an engine's first composite
    // builds its GPU pipelines, near a second here, which would have the
    // clock past the end before the cut's first frame showed. The app has
    // them long before any cut.
    // Each parked frame waited for, not timed: a loaded machine is
    // slower, not wrong.
    let shown_at = |f: i64| {
        let want = common::frame_tc(f, fps).to_secs_f64();
        let by = Instant::now() + Duration::from_secs(30);
        let at = |fb: &FrameBuffer| fb.latest_presented().is_some_and(|t| (t.to_secs_f64() - want).abs() < 0.5 / fps.as_f64());
        while Instant::now() < by && !at(&fb.lock().unwrap()) {
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    cmd.send(AppCommand::SetPlayhead(common::frame_tc(30, fps))).unwrap();
    shown_at(30);
    cmd.send(AppCommand::SetPlayhead(common::frame_tc(2, fps))).unwrap();
    shown_at(2);
    cmd.send(AppCommand::Play).unwrap();
    let mut furthest = 0.0f64;
    let mut last_gen = 0;
    let t0 = Instant::now();
    // Until a frame past `goal` is shown, or a minute.
    while t0.elapsed() < Duration::from_secs(60) && furthest <= goal {
        {
            let fb = fb.lock().unwrap();
            let (bytes, _, _, generation) = fb.latest_frame();
            if generation != last_gen && !bytes.is_empty() {
                last_gen = generation;
                if let Some(at) = fb.latest_presented() {
                    furthest = furthest.max(at.to_secs_f64());
                }
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = cmd.send(AppCommand::Pause);
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();
    // A decode thread still in ffmpeg's teardown at exit crashes the
    // process (CUDA's context destroyed under it).
    klipp_engine::video_decoder::wait_for_decode_threads(std::time::Duration::from_secs(10));
    furthest
}

#[test]
fn playback_carries_on_into_a_graded_compound() {
    let cut = 20.0 / common::FPS as f64;
    let plain = furthest_played(false, cut + 0.3);
    let graded = furthest_played(true, cut + 0.3);
    eprintln!("cut at {cut:.2}s; furthest frame shown: ungraded compound {plain:.2}s, graded compound {graded:.2}s");
    assert!(plain > cut + 0.3, "control: an ungraded compound plays on: {plain}");
    assert!(graded > cut + 0.3, "a graded compound plays on too: {graded}");
}
