//! Three still images stacked over a moving background are each read and
//! sent up to the GPU once (and once more if playing renders at another
//! size), then drawn from there while they play: no further uploads,
//! every one a hit, as the perf samples report (#2102).

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_effects::BuiltinEffect as E;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::test_clip::TestClip;
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 3840;
const H: u32 = 2160;

/// A one-frame PNG at the sequence's size. `None` without ffmpeg.
fn still_png(n: u32) -> Option<std::path::PathBuf> {
    let out = std::env::temp_dir().join(format!("klipp-stacked-still-{n}-{W}x{H}.png"));
    if out.exists() {
        return Some(out);
    }
    let partial = common::unshared(&out, "png");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", &format!("testsrc2=s={W}x{H}:d=1"), "-vf", &format!("hue=h={}", n * 90), "-frames:v", "1"])
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

#[test]
fn three_stacked_stills_are_uploaded_once() {
    let fps = Fps::new(30, 1);
    let tc = |f| common::frame_tc(f, fps);
    let mut pool = MediaPool::default();
    let mut tracks = Vec::new();
    for n in 0..3 {
        let Some(png) = still_png(n) else {
            eprintln!("skipping: no ffmpeg");
            return;
        };
        let id = MediaId::new();
        pool.add(MediaItem {
            id,
            path: png.clone(),
            kind: klipp_media::MediaItemKind::File,
            resolution: Resolution { width: W, height: H },
            is_still: true,
            ..MediaItem::new_synthetic_title(id, png.display().to_string(), tc(1), fps)
        });
        // Cards as the film has them: rounded, with a glow, faded in,
        // rising, growing and tilting back as they come.
        let card = Clip { kind: ClipKind::Media { media_id: id }, source_in: tc(0), source_out: tc(150), ..Clip::gap(tc(0), tc(150)) };
        let card = TestClip::of(card)
            .effect(E::RoundedCorners { radius: 0.055, feather: 0.002 })
            .effect(E::OuterGlow { radius: 0.0144, intensity: 0.24, color: [140, 150, 255] })
            .lane("opacity", 0.0, 0.0)
            .lane("opacity", 0.65, 1.0)
            .lane("pos_x", 0.0, (n as f32 - 1.0) * 1152.0)
            .lane("pos_y", 0.0, 1404.0)
            .lane("pos_y", 0.65, 172.8)
            .lane("pos_y", 5.0, 64.8)
            .lane("scale", 0.0, 0.29)
            .lane("scale", 5.0, 0.31)
            .lane("tilt_y", 0.0, 15.0)
            .lane("tilt_y", 5.0, 5.0)
            .build();
        let mut t = Track::new(&format!("Card {n}"), TrackKind::Video);
        t.clips = vec![card];
        tracks.push(t);
    }
    let gradient = Clip {
        kind: ClipKind::Generator(GeneratorData::LivingGradient { a: [5, 4, 12, 255], b: [28, 18, 61, 255], angle_deg: 25.0, drift_deg_per_sec: 1.5, glow: 0.18, grain: 0.0 }),
        source_in: tc(0),
        source_out: tc(150),
        ..Clip::gap(tc(0), tc(150))
    };
    let mut under = Track::new("V1", TrackKind::Video);
    under.clips = vec![gradient];
    tracks.push(under);
    let mut seq = Sequence::new_default();
    seq.fps = fps;
    seq.resolution = Resolution { width: W, height: H };
    seq.tracks = tracks;

    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
    std::thread::spawn(move || while ev_rx.recv().is_ok() {});
    let fb = Arc::new(Mutex::new(FrameBuffer::new(W, H)));
    let ring = Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default()));
    let engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(W, H))),
        Arc::new(klipp_timeline::SharedSequence::new(seq)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::clone(&ring),
    );
    let thread = std::thread::spawn(move || engine.run());
    cmd.send(AppCommand::SetPlayhead(tc(3))).unwrap();
    common::settle(&fb);
    let parked = ring.lock().unwrap().iter_chronological().count();
    cmd.send(AppCommand::Play).unwrap();
    // Waited on, not timed: 20 frames shown while playing.
    let t0 = Instant::now();
    while ring.lock().unwrap().iter_chronological().count() < parked + 20 && t0.elapsed() < Duration::from_secs(30) {
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = cmd.send(AppCommand::Pause);
    let samples: Vec<_> = ring.lock().unwrap().iter_chronological().skip(parked).copied().collect();
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();
    klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));

    let mut ms: Vec<f32> = samples.iter().map(|s| s.total_ms).collect();
    ms.sort_by(f32::total_cmp);
    let (kept, uploaded): (u32, u32) = samples.iter().fold((0, 0), |(k, u), s| (k + s.stills_kept, u + s.stills_uploaded));
    eprintln!("{} frames playing: median {:.1} ms, stills kept {kept}, uploaded {uploaded}", ms.len(), ms.get(ms.len() / 2).copied().unwrap_or(0.0));
    assert!(samples.len() >= 20, "{} frames shown while playing", samples.len());
    // The first frame playing may fit them again at the playing size,
    // once each; after it, all three are drawn from the GPU every frame.
    let after = &samples[1..];
    let (kept_after, uploaded_after): (u32, u32) = after.iter().fold((0, 0), |(k, u), s| (k + s.stills_kept, u + s.stills_uploaded));
    assert!(samples[0].stills_uploaded <= 3, "the first frame playing sent up {}", samples[0].stills_uploaded);
    assert_eq!(uploaded_after, 0, "stills sent up again after the first frame playing ({kept_after} kept)");
    assert_eq!(kept_after, 3 * after.len() as u32, "not every still was drawn from the GPU");
}
