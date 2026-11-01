//! #3209 PR 3: a take of an audition previewed in its slot is handed to
//! the engine in place of the document (`SharedSequence::stand_in`). The
//! engine decodes the take's own file there, and frames it draws ahead
//! meanwhile are kept by what is in them, so once the document is back
//! the viewer shows the document's picture, parked and playing, never a
//! kept frame of the take.

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::{AppCommand as C, AppEvent, ClipId, Fps, MediaId, RationalTime, Resolution, TakeId};
use klipp_engine::background_render::{self as bg, State};
use klipp_engine::frame_cache::NAMESPACE;
use klipp_engine::perf::PerfRing;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, Sequence, SharedSequence, StandInFor, Track, TrackKind};

const W: u32 = 320;
const H: u32 = 180;
const SECS: u32 = 4;
const FPS: u32 = 24;

/// A 4 s source drawn by ffmpeg's `lavfi` input `what`, or `None` without
/// ffmpeg.
fn source(name: &str, what: &str) -> Option<std::path::PathBuf> {
    let out = std::env::temp_dir().join(format!("klipp-stand-in-{name}-{}.mkv", std::process::id()));
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", &format!("{what}{}s={W}x{H}:r={FPS}:d={SECS}", if what.contains('=') { ":" } else { "=" }), "-c:v", "ffv1", "-pix_fmt", "yuv444p"])
        .arg(&out)
        .status()
        .ok()?
        .success();
    ok.then_some(out)
}

fn tc(frame: i64) -> RationalTime {
    RationalTime { value: frame, rate: FPS }
}

/// Wait until `ok`, up to `limit`; whether it came.
fn wait_for(limit: Duration, mut ok: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while !ok() {
        if start.elapsed() > limit {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    true
}

/// The share of the frame's pixels that are pure red, as the take's
/// source is.
fn red_share(rgba: &[u8]) -> f64 {
    let px = rgba.len() / 4;
    let red = rgba.chunks_exact(4).filter(|p| p[0] > 200 && p[1] < 60 && p[2] < 60).count();
    red as f64 / px.max(1) as f64
}

#[test]
fn the_document_comes_back_whole_after_a_take_stood_in() {
    let (Some(pattern), Some(red)) = (source("pattern", "testsrc2"), source("red", "color=c=red")) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let fps = Fps::new(FPS, 1);
    let frames = (SECS * FPS) as i64;
    let (doc_media, take_media) = (MediaId::new(), MediaId::new());
    let mut pool = MediaPool::default();
    for (id, path) in [(doc_media, &pattern), (take_media, &red)] {
        pool.add(MediaItem {
            id,
            path: path.clone(),
            kind: klipp_media::MediaItemKind::File,
            resolution: Resolution { width: W, height: H },
            ..MediaItem::new_synthetic_title(id, path.display().to_string(), tc(frames), fps)
        });
    }
    let slot = Clip { kind: ClipKind::Media { media_id: doc_media }, source_in: tc(0), source_out: tc(frames), ..Clip::gap(tc(0), tc(frames)) };
    let clip_id: ClipId = slot.id;
    let mut document = Sequence::new_default();
    document.fps = fps;
    document.resolution = Resolution { width: W, height: H };
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![slot.clone()];
    document.tracks = vec![v1];
    // The take: the same slot, the other file in it.
    let mut take = document.clone();
    if let ClipKind::Media { media_id } = &mut take.tracks[0].clips[0].kind {
        *media_id = take_media;
    }

    let shared = Arc::new(SharedSequence::new(document.clone()));
    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
    std::thread::spawn(move || while ev_rx.recv().is_ok() {});
    let fb = Arc::new(Mutex::new(FrameBuffer::new(W, H)));
    let engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(W, H))),
        Arc::clone(&shared), Arc::new(RwLock::new(pool)), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::new(Mutex::new(PerfRing::default())),
    );
    let mut engine = engine;
    let foreground = klipp_jobs::ForegroundActivity::new();
    engine.set_foreground_activity(Arc::clone(&foreground));
    let thread = std::thread::spawn(move || engine.run());
    let generation = || fb.lock().unwrap().latest_generation();
    let frame = || fb.lock().unwrap().latest_frame().0.to_vec();
    // A frame newer than `since`, settled: the generation still for a
    // quarter of a second.
    let shown_after = |since: u64| {
        assert!(wait_for(Duration::from_secs(20), || generation() > since), "no new frame");
        let mut last = generation();
        let mut still = Instant::now();
        while still.elapsed() < Duration::from_millis(250) {
            std::thread::sleep(Duration::from_millis(20));
            let now = generation();
            if now != last {
                last = now;
                still = Instant::now();
            }
        }
        frame()
    };

    let g = generation();
    cmd.send(C::SetPlayhead(tc(10))).unwrap();
    let parked = shown_after(g);
    assert!(red_share(&parked) < 0.2, "the document's pattern parked: {:.2} red", red_share(&parked));

    // The take stands in: the engine decodes the take's own file there.
    let g = generation();
    shared.stand_in(take.clone(), StandInFor { clip: clip_id, take: TakeId::new() });
    cmd.send(C::SequenceUpdated).unwrap();
    let previewed = shown_after(g);
    assert!(red_share(&previewed) > 0.9, "the take's own file in the slot: {:.2} red", red_share(&previewed));

    // Frames drawn ahead while it stands in.
    let root = std::env::temp_dir().join(format!("klipp-stand-in-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    cmd.send(C::SetFreezeFolder(Some(root.clone()))).unwrap();
    let store = klipp_engine::cache_store::shared_at(&root, 1 << 30);
    bg::force_heavy(true);
    cmd.send(C::SetBackgroundRender { on: true, delay_secs: 0.3 }).unwrap();
    assert!(wait_for(Duration::from_secs(60), || bg::state() == State::Done), "the take's frames drawn ahead: {:?}", bg::state());
    let kept = store.lock().unwrap().frames(NAMESPACE).len();
    assert!(kept > 0, "frames kept while the take stood in");

    // The document back: its picture parked, and playing.
    foreground.touch_input();
    let g = generation();
    assert!(shared.end_stand_in());
    cmd.send(C::SequenceUpdated).unwrap();
    let back = shown_after(g);
    assert!(red_share(&back) < 0.2, "the document's picture back, not a kept frame of the take: {:.2} red", red_share(&back));
    let shown_kept = klipp_engine::frames_shown_kept();
    cmd.send(C::SeekTo(tc(0))).unwrap();
    cmd.send(C::Play).unwrap();
    let mut reds = 0;
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(50));
        if red_share(&frame()) > 0.5 {
            reds += 1;
        }
    }
    cmd.send(C::Pause).unwrap();
    assert_eq!(reds, 0, "no frame of the take while the document plays");
    eprintln!("MEASURE {kept} frames kept for the take; {} shown from the cache while the document played", klipp_engine::frames_shown_kept() - shown_kept);
    bg::force_heavy(false);
    cmd.send(C::SetBackgroundRender { on: false, delay_secs: 0.3 }).unwrap();
    let _ = std::fs::remove_dir_all(&root);
    // The engine stopped and its decode threads let go before the process
    // ends: one still in ffmpeg's teardown at exit crashes it.
    cmd.send(C::Quit).unwrap();
    let _ = thread.join();
    klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));
    let _ = std::fs::remove_file(&pattern);
    let _ = std::fs::remove_file(&red);
}
