//! The composited skim (#2945) keeps the raw skim's pace on a 4K source:
//! a skim across a 4K clip with a title over it, drawn from the clip's
//! 1080p skim file by the GPU stack, makes its frames as fast as the raw
//! skim frames of that file were made, measured over the same positions
//! (55 frames/s each when written).
//!
//! Needs the ffmpeg CLI to write the sources; skips without it.

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

#[test]
fn a_composited_skim_of_a_4k_clip_keeps_the_raw_skims_pace() {
    let required = std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1");
    let (Some(original), Some(skim)) = (common::hevc_source("skim-pace-4k", 3840, 2160, common::SDR_TAGS), common::hevc_source("skim-pace-1080", 1920, 1080, common::SDR_TAGS)) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let fps = Fps::new(common::FPS, 1);
    let tc = |f: i64| common::frame_tc(f, fps);
    let id = MediaId::new();
    let mut pool = MediaPool::default();
    pool.add(MediaItem {
        id,
        path: original.clone(),
        kind: klipp_media::MediaItemKind::File,
        resolution: Resolution { width: 3840, height: 2160 },
        skim_file: Some(skim.clone()),
        ..MediaItem::new_synthetic_title(id, original.display().to_string(), tc(50), fps)
    });
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![Clip { kind: ClipKind::Media { media_id: id }, source_in: tc(0), source_out: tc(50), ..Clip::gap(tc(0), tc(50)) }];
    let td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "PACE".into(), font_size: 160.0, color: [255, 255, 255, 255], ..Default::default() });
    let mut t1 = Track::new("T1", TrackKind::Text);
    t1.clips = vec![Clip { kind: ClipKind::Title(td), ..Clip::gap(tc(0), tc(50)) }];
    let mut seq = Sequence::new_default();
    seq.fps = fps;
    seq.resolution = Resolution { width: 3840, height: 2160 };
    seq.tracks = vec![t1, v1];

    // Positions back and forth over the clip, as a drag goes.
    let back: Vec<i64> = (1..50).step_by(2).collect();
    let positions: Vec<i64> = (0..50).step_by(2).chain(back.into_iter().rev()).collect();

    // Today's raw skim: the skim file's frames, read back and halved to
    // 1080 lines on the CPU.
    let mut raw = klipp_engine::skim::SkimDecoders::new(4);
    let _ = raw.frame_at(&skim, 0, 0.5, 1080, Duration::from_millis(600));
    let t0 = Instant::now();
    for f in &positions {
        raw.frame_at(&skim, 0, *f as f64 / common::FPS as f64, 1080, Duration::from_millis(400)).expect("a raw skim frame");
    }
    let raw_fps = positions.len() as f64 / t0.elapsed().as_secs_f64();
    drop(raw);

    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, events) = crossbeam_channel::unbounded::<AppEvent>();
    let fb = Arc::new(Mutex::new(FrameBuffer::new(1920, 1080)));
    let engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(1920, 1080))),
        Arc::new(klipp_timeline::SharedSequence::new(seq)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
    );
    let thread = std::thread::spawn(move || engine.run());
    cmd.send(AppCommand::Pause).unwrap();
    let skim_at = |f: i64| -> bool {
        cmd.send(AppCommand::SkimTimeline { time: tc(f) }).unwrap();
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(10) {
            if let Ok(AppEvent::TimelineSkimShown { composited, .. }) = events.recv_timeout(Duration::from_millis(20)) {
                return composited;
            }
        }
        false
    };
    // The decoder opened and the stack warmed, as after a skim's first frame.
    skim_at(25);
    let t0 = Instant::now();
    let drawn = positions.iter().filter(|f| skim_at(**f)).count();
    let composited_fps = positions.len() as f64 / t0.elapsed().as_secs_f64();
    cmd.send(AppCommand::EndScrub).unwrap();
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();
    klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));

    eprintln!("4K source skimmed from its 1080p skim file: raw {raw_fps:.1} frames/s, composited with a title {composited_fps:.1} frames/s ({drawn} of {} drawn)", positions.len());
    if drawn == 0 && !required {
        eprintln!("UNVERIFIED: no skim was drawn (no GPU stack here)");
        return;
    }
    assert_eq!(drawn, positions.len(), "every position drawn");
    // Both are bound by the decode, the same frames of the same file: the
    // same pace within the run-to-run spread (about 5%).
    assert!(composited_fps >= raw_fps * 0.9, "composited skims at {composited_fps:.1} frames/s, the raw skim at {raw_fps:.1}");
}
