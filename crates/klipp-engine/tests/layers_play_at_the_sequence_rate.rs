//! The picture is drawn on the sequence's frame clock whenever something
//! on screen can change, not only when the primary track's file gives a
//! new frame. A 30 fps layer over a 24 fps primary updated at 24 a second,
//! the primary's rate; and a lone 24 fps clip in a 30 fps sequence was
//! reported as dropping a fifth of its frames, which it never had.

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 320;
const H: u32 = 180;

/// One test at a time: the frames given and playback's health are the
/// process's.
static ONE: Mutex<()> = Mutex::new(());

/// Eight seconds of a moving test picture at `rate` (num/den), a keyframe
/// a second. `None` without ffmpeg.
fn video(num: u32, den: u32) -> Option<std::path::PathBuf> {
    let out = std::env::temp_dir().join(format!("klipp-pacing-{W}x{H}-{num}-{den}.mp4"));
    if out.exists() {
        return Some(out);
    }
    let partial = common::unshared(&out, "mp4");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", &format!("testsrc2=s={W}x{H}:r={num}/{den}:d=8")])
        .args(["-c:v", "libx264", "-g", &((num / den).max(1)).to_string(), "-pix_fmt", "yuv420p"])
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

/// A one-frame PNG.
fn still() -> Option<std::path::PathBuf> {
    let out = std::env::temp_dir().join(format!("klipp-pacing-still-{W}x{H}.png"));
    if out.exists() {
        return Some(out);
    }
    let partial = common::unshared(&out, "png");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", &format!("color=c=0x804020:s={W}x{H}:d=1"), "-frames:v", "1"])
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

/// A clip of `path` (`fps`, a still or not) over `[from, to)` seconds of a
/// sequence at `seq_fps`, its item added to `pool`; half size in the
/// frame's corner when `small`, so the one under it shows too.
fn clip(pool: &mut MediaPool, path: &std::path::Path, fps: Fps, is_still: bool, seq_fps: Fps, from: f64, to: f64, small: bool) -> Clip {
    let tc = |s: f64| RationalTime::from_secs_f64(s, seq_fps.rate());
    let id = MediaId::new();
    pool.add(MediaItem {
        id,
        path: path.to_path_buf(),
        kind: klipp_media::MediaItemKind::File,
        resolution: Resolution { width: W, height: H },
        is_still,
        has_audio: false,
        fps,
        ..MediaItem::new_synthetic_title(id, path.display().to_string(), tc(8.0), fps)
    });
    let mut c = Clip { kind: ClipKind::Media { media_id: id }, source_in: tc(0.0), source_out: tc(to - from), ..Clip::gap(tc(from), tc(to)) };
    if small {
        // One key: a constant half size, not an animation.
        c.edit_transform_scale_keyframes(|v| v.push(Keyframe { time_secs: 0.0, value: 0.5, interpolation: Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }));
    }
    c
}

/// What playing `seq` from `from_secs` for `secs` showed.
struct Played {
    /// Pictures presented a second over the last 1.5 s.
    rate: f64,
    /// Each picture's timeline time, in order, without repeats.
    shown: Vec<RationalTime>,
    /// Every layer frame the renders were given: clip, source time asked,
    /// the file frame's time.
    given: Vec<(ClipId, f64, Option<f64>)>,
    /// Playback's health at the end, and what it last counted as due a
    /// second.
    dropping: f32,
    seconds: usize,
    due: f64,
}

fn play(seq: Sequence, pool: MediaPool, from_secs: f64, secs: f64) -> Played {
    let fps = seq.fps;
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
    // On a timeline frame: 1 s is between two of 29.97's.
    let frame = (from_secs * fps.as_f64()).round() as i64;
    cmd.send(AppCommand::SetPlayhead(RationalTime::from_frames(frame * i64::from(fps.den), fps.rate()))).unwrap();
    common::settle(&fb);
    std::thread::sleep(Duration::from_millis(300));
    klipp_engine::engine::keep_frames_given();
    cmd.send(AppCommand::Play).unwrap();
    let mut shown: Vec<RationalTime> = Vec::new();
    let end = Instant::now() + Duration::from_secs_f64(secs);
    while Instant::now() < end {
        if let Some(at) = fb.lock().unwrap().latest_presented() {
            if shown.last() != Some(&at) {
                shown.push(at);
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let rate = fb.lock().unwrap().presented_fps(Instant::now(), Duration::from_millis(1500)).unwrap_or(0.0);
    let shared = klipp_engine::playback_health::shared();
    let health = shared.health;
    let given = klipp_engine::engine::frames_given();
    let _ = cmd.send(AppCommand::Pause);
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();
    klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));
    Played { rate, shown, given, dropping: health.dropping, seconds: health.seconds, due: shared.due_fps }
}

fn sequence(fps: Fps, tracks: Vec<Vec<Clip>>) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.fps = fps;
    seq.resolution = Resolution { width: W, height: H };
    seq.tracks = tracks
        .into_iter()
        .enumerate()
        .map(|(i, clips)| {
            let mut t = Track::new(&format!("V{i}"), TrackKind::Video);
            t.clips = clips;
            t
        })
        .collect();
    seq
}

/// The distinct file frames `clip` was shown at, in order.
fn frames_of(given: &[(ClipId, f64, Option<f64>)], clip: ClipId) -> Vec<f64> {
    let mut out: Vec<f64> = Vec::new();
    for (c, _, pts) in given {
        if *c == clip {
            if let Some(p) = pts {
                if out.last().is_none_or(|l| (l - p).abs() > 1e-6) {
                    out.push(*p);
                }
            }
        }
    }
    out
}

/// A lone 24 fps clip in a 30 fps sequence shows its 24 pictures a second
/// and drops nothing: there is nothing else to show.
#[test]
fn a_lone_slower_clip_plays_its_own_rate_and_drops_nothing() {
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(p24) = video(24, 1) else { return eprintln!("skipping: no ffmpeg") };
    for (seq_fps, file) in [(Fps::new(30, 1), Fps::new(24, 1)), (Fps::new(60, 1), Fps::new(30, 1)), (Fps::new(30000, 1001), Fps::new(24000, 1001))] {
        let path = if file.num == 24 { p24.clone() } else { video(file.num, file.den).expect("ffmpeg") };
        let mut pool = MediaPool::default();
        let c = clip(&mut pool, &path, file, false, seq_fps, 0.0, 8.0, false);
        let played = play(sequence(seq_fps, vec![vec![c]]), pool, 1.0, 4.0);
        assert!(played.rate <= file.as_f64() + 0.6, "{seq_fps:?} with a {file:?} clip: {:.1} pictures a second", played.rate);
        assert!(played.rate > file.as_f64() * 0.6, "{seq_fps:?} with a {file:?} clip: hardly moved, {:.1}", played.rate);
        assert!(played.seconds >= 1 && played.dropping < 0.05, "{seq_fps:?} with a {file:?} clip: reported {:.0}% dropped over {} s", played.dropping * 100.0, played.seconds);
        assert!((played.due - file.as_f64()).abs() < 0.1, "{seq_fps:?} with a {file:?} clip: due {:.2} a second", played.due);
    }
}

/// A 30 fps layer over a 24 fps primary shows each of its own frames: the
/// picture is drawn on the sequence's 30, and the layer's file frame moves
/// on each time, as export draws it. NTSC rates too.
#[test]
fn a_faster_layer_over_a_slower_primary_shows_every_frame() {
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    for (seq_fps, under, over) in [
        (Fps::new(30, 1), Fps::new(24, 1), Fps::new(30, 1)),
        (Fps::new(30000, 1001), Fps::new(24000, 1001), Fps::new(30000, 1001)),
        (Fps::new(60, 1), Fps::new(24, 1), Fps::new(60, 1)),
    ] {
        let (Some(pu), Some(po)) = (video(under.num, under.den), video(over.num, over.den)) else { return eprintln!("skipping: no ffmpeg") };
        let mut pool = MediaPool::default();
        let top = clip(&mut pool, &po, over, false, seq_fps, 0.0, 8.0, true);
        let bottom = clip(&mut pool, &pu, under, false, seq_fps, 0.0, 8.0, false);
        let top_id = top.id;
        let played = play(sequence(seq_fps, vec![vec![top], vec![bottom]]), pool, 1.0, 4.0);
        // Waited on, not timed: a loaded machine shows fewer. The layer's
        // own rate, capped by the sequence's, is what is due.
        let due = over.as_f64().min(seq_fps.as_f64());
        assert!(played.rate > due * 0.85, "{seq_fps:?}: {under:?} under {over:?}: {:.1} pictures a second, {due:.1} due", played.rate);
        assert!(played.dropping < 0.05, "{seq_fps:?}: reported {:.0}% dropped", played.dropping * 100.0);
        assert!((played.due - seq_fps.as_f64()).abs() < 0.1, "{seq_fps:?}: due {:.2} a second", played.due);
        // The layer's frames, in the pictures: each new picture a new file
        // frame of the layer, none skipped where the sequence had a frame.
        let frames = frames_of(&played.given, top_id);
        let step = 1.0 / over.as_f64();
        let skips = frames.windows(2).filter(|w| w[1] - w[0] > step * 1.5).count();
        assert!(frames.len() as f64 > 2.0 * due * 0.85, "{seq_fps:?}: the layer showed {} frames in ~3 s", frames.len());
        assert!(skips * 20 <= frames.len(), "{seq_fps:?}: the layer skipped frames {skips} times in {}", frames.len());
        // Each picture a timeline frame's.
        let off = played.shown.iter().filter(|t| { let f = t.to_secs_f64() * seq_fps.as_f64(); (f - f.round()).abs() > 1e-3 }).count();
        assert_eq!(off, 0, "{seq_fps:?}: pictures between timeline frames");
    }
}

/// A static still over a 24 fps primary changes nothing between the
/// primary's frames: the picture is drawn at 24, and nothing is dropped.
#[test]
fn a_static_layer_over_a_slower_primary_draws_nothing_more() {
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let (Some(p24), Some(png)) = (video(24, 1), still()) else { return eprintln!("skipping: no ffmpeg") };
    let fps = Fps::new(30, 1);
    let mut pool = MediaPool::default();
    let top = clip(&mut pool, &png, Fps::new(25, 1), true, fps, 0.0, 8.0, true);
    let bottom = clip(&mut pool, &p24, Fps::new(24, 1), false, fps, 0.0, 8.0, false);
    let played = play(sequence(fps, vec![vec![top], vec![bottom]]), pool, 1.0, 4.0);
    assert!(played.rate <= 24.6, "{:.1} pictures a second under a still", played.rate);
    assert!((played.due - 24.0).abs() < 0.1, "due {:.2} a second: the still changes nothing", played.due);
    assert!(played.dropping < 0.05, "reported {:.0}% dropped", played.dropping * 100.0);
}

/// A still on the primary track with a 30 fps layer over it: the still
/// changes nothing, the layer every frame, so the picture is drawn and due
/// at 30 and nothing is reported dropped.
#[test]
fn a_still_primary_under_a_moving_layer_is_drawn_at_the_layers_rate() {
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let (Some(p30), Some(png)) = (video(30, 1), still()) else { return eprintln!("skipping: no ffmpeg") };
    let fps = Fps::new(30, 1);
    let mut pool = MediaPool::default();
    let top = clip(&mut pool, &p30, Fps::new(30, 1), false, fps, 0.0, 8.0, true);
    let bottom = clip(&mut pool, &png, Fps::new(25, 1), true, fps, 0.0, 8.0, false);
    let top_id = top.id;
    let played = play(sequence(fps, vec![vec![top], vec![bottom]]), pool, 1.0, 4.0);
    assert!(played.rate > 30.0 * 0.85, "{:.1} pictures a second over a still", played.rate);
    assert!((played.due - 30.0).abs() < 0.1, "due {:.2} a second: the layer changes every frame", played.due);
    assert!(played.dropping < 0.05, "reported {:.0}% dropped", played.dropping * 100.0);
    let frames = frames_of(&played.given, top_id);
    assert!(frames.len() > 60, "the layer showed {} frames in ~3 s", frames.len());
}

/// A cut from a 24 fps clip to a 30 fps one, alone in a 30 fps sequence:
/// nothing reported dropped across it, and each side at its own rate.
#[test]
fn a_cut_between_rates_reports_nothing_dropped() {
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let (Some(p24), Some(p30)) = (video(24, 1), video(30, 1)) else { return eprintln!("skipping: no ffmpeg") };
    let fps = Fps::new(30, 1);
    let mut pool = MediaPool::default();
    let a = clip(&mut pool, &p24, Fps::new(24, 1), false, fps, 0.0, 3.0, false);
    let b = clip(&mut pool, &p30, Fps::new(30, 1), false, fps, 3.0, 8.0, false);
    let played = play(sequence(fps, vec![vec![a, b]]), pool, 0.5, 5.0);
    assert!(played.dropping < 0.05, "reported {:.0}% dropped across a cut from 24 to 30", played.dropping * 100.0);
}

/// A real overload is still reported: frames that take longer to decode
/// than the sequence gives them are dropped, and said to be.
#[test]
fn a_real_overload_is_still_reported() {
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(p30) = video(30, 1) else { return eprintln!("skipping: no ffmpeg") };
    let fps = Fps::new(30, 1);
    let mut pool = MediaPool::default();
    let c = clip(&mut pool, &p30, Fps::new(30, 1), false, fps, 0.0, 8.0, false);
    klipp_engine::video_decoder::set_decode_delay_for_tests(70);
    let played = play(sequence(fps, vec![vec![c]]), pool, 0.5, 5.0);
    klipp_engine::video_decoder::set_decode_delay_for_tests(0);
    assert!(played.rate < 20.0, "the delay held decoding to under 15 a second, yet {:.1} shown", played.rate);
    assert!(played.dropping > 0.2, "an overload reported as {:.0}% dropped", played.dropping * 100.0);
}
