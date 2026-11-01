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

/// What each card carries, as the film's do: all of it by default.
#[derive(Clone, Copy, Debug)]
struct Chain {
    corners: bool,
    glow: bool,
    shadow: bool,
    /// Rising, growing and tilting back as it comes.
    moves: bool,
    fade: bool,
}

const FILM: Chain = Chain { corners: true, glow: true, shadow: false, moves: true, fade: true };

/// `layers` 4K still cards carrying `chain` over a living gradient, parked
/// and then played for `frames` frames: the perf samples of the frames
/// shown playing.
fn play(layers: u32, chain: Chain, frames: usize) -> Option<Vec<klipp_engine::perf::PerfSample>> {
    let fps = Fps::new(30, 1);
    let tc = |f| common::frame_tc(f, fps);
    let mut pool = MediaPool::default();
    let mut tracks = Vec::new();
    for n in 0..layers {
        let png = still_png(n)?;
        let id = MediaId::new();
        pool.add(MediaItem {
            id,
            path: png.clone(),
            kind: klipp_media::MediaItemKind::File,
            resolution: Resolution { width: W, height: H },
            is_still: true,
            ..MediaItem::new_synthetic_title(id, png.display().to_string(), tc(1), fps)
        });
        let card = Clip { kind: ClipKind::Media { media_id: id }, source_in: tc(0), source_out: tc(300), ..Clip::gap(tc(0), tc(300)) };
        let mut card = TestClip::of(card);
        if chain.corners {
            card = card.effect(E::RoundedCorners { radius: 0.055, feather: 0.002 });
        }
        if chain.glow {
            card = card.effect(E::OuterGlow { radius: 0.0144, intensity: 0.24, color: [140, 150, 255] });
        }
        if chain.shadow {
            card = card.effect(E::DropShadow { offset_x: 0.0, offset_y: 0.01, blur: 0.02, color: [0, 0, 0], opacity: 0.4 });
        }
        if chain.fade {
            card = card.lane("opacity", 0.0, 0.0).lane("opacity", 0.65, 1.0).lane("opacity", 10.0, 0.9);
        }
        if chain.moves {
            card = card
                .lane("pos_x", 0.0, (n as f32 - 1.0) * 1152.0)
                .lane("pos_y", 0.0, 1404.0)
                .lane("pos_y", 0.65, 172.8)
                .lane("pos_y", 10.0, 64.8)
                .lane("scale", 0.0, 0.29)
                .lane("scale", 10.0, 0.31)
                .lane("tilt_y", 0.0, 15.0)
                .lane("tilt_y", 10.0, 5.0);
        } else {
            card = card.lane("pos_x", 0.0, (n as f32 - 1.0) * 1152.0).lane("scale", 0.0, 0.3);
        }
        let mut t = Track::new(&format!("Card {n}"), TrackKind::Video);
        t.clips = vec![card.build()];
        tracks.push(t);
    }
    let gradient = Clip {
        kind: ClipKind::Generator(GeneratorData::LivingGradient { a: [5, 4, 12, 255], b: [28, 18, 61, 255], angle_deg: 25.0, drift_deg_per_sec: 1.5, glow: 0.18, grain: 0.0 }),
        source_in: tc(0),
        source_out: tc(300),
        ..Clip::gap(tc(0), tc(300))
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
    let ring = Arc::new(Mutex::new(klipp_engine::perf::PerfRing::with_capacity(frames + 64)));
    let engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(W, H))),
        Arc::new(klipp_timeline::SharedSequence::new(seq)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::clone(&ring),
    );
    let thread = engine.spawn();
    cmd.send(AppCommand::SetPlayhead(tc(30))).unwrap();
    common::settle(&fb);
    let parked = ring.lock().unwrap().iter_chronological().count();
    cmd.send(AppCommand::Play).unwrap();
    // Waited on, not timed.
    let t0 = Instant::now();
    while ring.lock().unwrap().iter_chronological().count() < parked + frames && t0.elapsed() < Duration::from_secs(60) {
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = cmd.send(AppCommand::Pause);
    let samples: Vec<_> = ring.lock().unwrap().iter_chronological().skip(parked).copied().collect();
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();
    klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));
    Some(samples)
}

fn median_ms(samples: &[klipp_engine::perf::PerfSample]) -> f32 {
    let mut ms: Vec<f32> = samples.iter().skip(1).map(|s| s.total_ms).collect();
    ms.sort_by(f32::total_cmp);
    ms.get(ms.len() / 2).copied().unwrap_or(0.0)
}

#[test]
fn three_stacked_stills_are_uploaded_once() {
    let Some(samples) = play(3, FILM, 20) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let (kept, uploaded): (u32, u32) = samples.iter().fold((0, 0), |(k, u), s| (k + s.stills_kept, u + s.stills_uploaded));
    eprintln!("{} frames playing: median {:.1} ms, stills kept {kept}, uploaded {uploaded}", samples.len(), median_ms(&samples));
    assert!(samples.len() >= 20, "{} frames shown while playing", samples.len());
    // The first frame playing may fit them again at the playing size,
    // once each; after it, all three are drawn from the GPU every frame.
    let after = &samples[1..];
    let (kept_after, uploaded_after): (u32, u32) = after.iter().fold((0, 0), |(k, u), s| (k + s.stills_kept, u + s.stills_uploaded));
    assert!(samples[0].stills_uploaded <= 3, "the first frame playing sent up {}", samples[0].stills_uploaded);
    assert_eq!(uploaded_after, 0, "stills sent up again after the first frame playing ({kept_after} kept)");
    assert_eq!(kept_after, 3 * after.len() as u32, "not every still was drawn from the GPU");
}

/// Each part of a card's chain, timed at 4K playing at Full quality: the frame's cost
/// with one and with three cards, for the chain as a whole, without each
/// part, and with a drop shadow added (#2101, #2102).
/// `cargo test -p klipp-engine --test stacked_stills_are_kept measure -- --ignored --nocapture`
#[test]
#[ignore = "4K performance measurement"]
fn measure_the_card_chain() {
    // Full size while playing, as the viewer plays at Full quality.
    klipp_engine::video_decoder::set_playback_quality_divisor(1);
    let none = Chain { corners: false, glow: false, shadow: false, moves: false, fade: false };
    let variants: [(&str, Chain); 8] = [
        ("plain", none),
        ("film chain", FILM),
        ("without corners", Chain { corners: false, ..FILM }),
        ("without glow", Chain { glow: false, ..FILM }),
        ("without moves", Chain { moves: false, ..FILM }),
        ("without fade", Chain { fade: false, ..FILM }),
        ("corners only", Chain { corners: true, ..none }),
        ("glow only", Chain { glow: true, ..none }),
    ];
    let extra = [("moves only", Chain { moves: true, ..none }), ("shadow only", Chain { shadow: true, ..none }), ("film + shadow", Chain { shadow: true, ..FILM })];
    for (name, chain) in variants.iter().chain(extra.iter()) {
        let one = play(1, *chain, 40).map(|s| median_ms(&s)).unwrap_or(0.0);
        let three = play(3, *chain, 40).map(|s| median_ms(&s)).unwrap_or(0.0);
        eprintln!("MEASURE {name:>16}: 1 card {one:5.1} ms, 3 cards {three:5.1} ms");
    }
}
