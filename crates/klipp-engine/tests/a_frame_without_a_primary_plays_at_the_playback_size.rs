//! A frame with no primary picture is drawn at the playback size
//! (#3205). A reversed clip is never the primary decoder's (#617); with
//! it, or titles alone, the viewer drew a 4K timeline's frame at 4K: the
//! effects chain at 3840x2160 under pictures decoded at 1080p and fitted
//! up to it, four times the work and memory the viewer can show.
//! It is drawn at the sequence's size held to the playback resolution,
//! as a clip on V1 is, parked and playing; pixel-sized effects follow
//! the render size, so the frame is the full-size one shrunk.

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_effects::BuiltinEffect;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::test_clip::TestClip;
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 3840;
const H: u32 = 2160;
const FPS: u32 = 25;

/// Three seconds of 4K test pattern, every frame a keyframe so a seek
/// lands at once. `None` without ffmpeg.
fn uhd() -> Option<std::path::PathBuf> {
    let out = std::env::temp_dir().join("klipp-uhd-testsrc2-intra-3s.mp4");
    if out.exists() {
        return Some(out);
    }
    let partial = common::unshared(&out, "mp4");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", &format!("testsrc2=s={W}x{H}:r={FPS}:d=3")])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-g", "1", "-pix_fmt", "yuv420p"])
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

/// `full` (RGBA, `w` by `h`) shrunk by two each way, a box of four.
fn halve(full: &[u8], w: u32, h: u32) -> Vec<u8> {
    let (hw, hh) = (w / 2, h / 2);
    let mut out = Vec::with_capacity((hw * hh * 4) as usize);
    for y in 0..hh {
        for x in 0..hw {
            for c in 0..4 {
                let at = |dx: u32, dy: u32| u32::from(full[(((2 * y + dy) * w + 2 * x + dx) * 4 + c) as usize]);
                out.push(((at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1) + 2) / 4) as u8);
            }
        }
    }
    out
}

/// Mean difference over red, green and blue, in levels.
fn mean_diff(a: &[u8], b: &[u8]) -> f64 {
    let (sum, n) = a.chunks_exact(4).zip(b.chunks_exact(4)).fold((0u64, 0u64), |(s, n), (p, q)| (s + (0..3).map(|c| u64::from(p[c].abs_diff(q[c]))).sum::<u64>(), n + 3));
    sum as f64 / n.max(1) as f64
}

#[test]
fn a_frame_without_a_primary_is_drawn_at_the_playback_size() {
    let Some(path) = uhd() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let fps = Fps::new(FPS, 1);
    let tc = |s: f64| common::frame_tc((s * f64::from(FPS)).round() as i64, fps);
    let media = MediaId::new();
    let mut pool = MediaPool::default();
    pool.add(MediaItem {
        id: media,
        path: path.clone(),
        kind: klipp_media::MediaItemKind::File,
        resolution: Resolution { width: W, height: H },
        is_still: false,
        has_audio: false,
        fps,
        ..MediaItem::new_synthetic_title(media, path.display().to_string(), tc(3.0), fps)
    });
    // Reversed, so nothing is the primary decoder's. A blur, whose
    // radius is in the sequence's pixels.
    let mut back = Clip { kind: ClipKind::Media { media_id: media }, source_in: tc(0.0), source_out: tc(3.0), ..Clip::gap(tc(0.0), tc(3.0)) };
    back.speed = -1.0;
    let back = TestClip::of(back).effect(BuiltinEffect::GaussianBlur { radius: 24.0 }).build();
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![back];
    let mut seq = Sequence::new_default();
    seq.fps = fps;
    seq.resolution = Resolution { width: W, height: H };
    seq.tracks = vec![v1];
    let reference = {
        let snapshot = klipp_render::SequenceSnapshot::from_sequence_with_all(&seq, std::slice::from_ref(&seq), &pool, &ExportSettings::default(), None, false, WorkingColorspace::Rec709Linear).expect("snapshot");
        klipp_render::still::timeline_still(&snapshot, 1.0, W, H).expect("the full-size frame")
    };

    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
    std::thread::spawn(move || while ev_rx.recv().is_ok() {});
    let fb = Arc::new(Mutex::new(FrameBuffer::new(W, H)));
    let engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(W, H))),
        Arc::new(klipp_timeline::SharedSequence::new(seq)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
    );
    let thread = engine.spawn();

    // Parked: the frame frame_at returns without a max_width. Waited
    // for, not settled on alone: the buffer starts out empty at 4K.
    let g = common::generation(&fb);
    cmd.send(AppCommand::SetPlayhead(tc(1.0))).unwrap();
    assert!(common::wait_new(&fb, g, Duration::from_secs(30)).is_some(), "no frame was drawn at 1 s");
    common::settle(&fb);
    let (parked, parked_size) = {
        let fb = fb.lock().unwrap();
        let (data, w, h, _) = fb.latest_frame();
        (data.to_vec(), (w, h))
    };
    // Playing: every frame presented in a second of it.
    cmd.send(AppCommand::SetPlayhead(tc(0.2))).unwrap();
    common::settle(&fb);
    cmd.send(AppCommand::Play).unwrap();
    let mut sizes = std::collections::BTreeSet::new();
    let mut seen = common::generation(&fb);
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_millis(1200) {
        let fb = fb.lock().unwrap();
        if fb.latest_generation() != seen {
            seen = fb.latest_generation();
            let (_, w, h, _) = fb.latest_frame();
            sizes.insert((w, h));
        }
        drop(fb);
        std::thread::sleep(Duration::from_millis(2));
    }
    let _ = cmd.send(AppCommand::Pause);
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();
    klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));

    assert_eq!(parked_size, (1920, 1080), "parked, the frame is at the playback size, as with a clip on V1");
    assert!(!sizes.is_empty(), "nothing was presented while playing");
    assert_eq!(sizes.into_iter().collect::<Vec<_>>(), vec![(1920, 1080)], "every frame played is at the playback size");
    // The blur scaled with the frame: the 1080p frame is the 4K one
    // shrunk, within what decoding at 1080p rather than shrinking after
    // makes. Its radius in the 1080p frame's own pixels would be twice
    // as wide.
    let shrunk = halve(&reference.rgba, reference.width, reference.height);
    let off = mean_diff(&parked, &shrunk);
    eprintln!("the 1080p frame is {off:.2} levels from the 4K one shrunk");
    assert!(off < 1.5, "the 1080p frame is {off:.2} levels from the 4K one shrunk");
}
