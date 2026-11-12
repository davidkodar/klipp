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
    /// When each of those was first seen, and when Play was sent.
    shown_when: Vec<Instant>,
    played_at: Instant,
    /// Where playing started.
    from: RationalTime,
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
    let from = RationalTime::from_frames(frame * i64::from(fps.den), fps.rate());
    cmd.send(AppCommand::SetPlayhead(from)).unwrap();
    common::settle(&fb);
    std::thread::sleep(Duration::from_millis(300));
    klipp_engine::engine::keep_frames_given();
    let played_at = Instant::now();
    cmd.send(AppCommand::Play).unwrap();
    let mut shown: Vec<RationalTime> = Vec::new();
    let mut shown_when: Vec<Instant> = Vec::new();
    let end = Instant::now() + Duration::from_secs_f64(secs);
    while Instant::now() < end {
        if let Some(at) = fb.lock().unwrap().latest_presented() {
            if shown.last() != Some(&at) {
                shown.push(at);
                shown_when.push(Instant::now());
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
    Played { rate, shown, shown_when, played_at, from, given, dropping: health.dropping, seconds: health.seconds, due: shared.due_fps }
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

/// Eight seconds of a tone, stereo, as a WAV.
fn tone() -> Option<std::path::PathBuf> {
    let out = std::env::temp_dir().join("klipp-pacing-tone-8s.wav");
    if out.exists() {
        return Some(out);
    }
    let partial = common::unshared(&out, "wav");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=8"])
        .args(["-ac", "2", "-c:a", "pcm_s16le"])
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

/// Mixed rates across cuts in a 30 fps sequence: on V1 (the primary) a 24
/// fps clip to 3 s, then a 25 fps one; over it, small, a 30 fps clip to
/// 3.2 s, a 24 fps one to 4 s, then a 10 fps one. At 10 fps, half of the
/// file's frame and half of the sequence's pick different frames (#696);
/// at 24 and 30 they agree. With a tone under it all when `sound`. The
/// layers' clips, and the pool with each file for export.
fn mixed_rates(sound: bool) -> Option<(Sequence, MediaPool, Vec<Clip>, std::collections::HashMap<MediaId, std::path::PathBuf>)> {
    let fps = Fps::new(30, 1);
    let (p10, p24, p25, p30) = (video(10, 1)?, video(24, 1)?, video(25, 1)?, video(30, 1)?);
    let mut pool = MediaPool::default();
    let layers = vec![
        clip(&mut pool, &p30, Fps::new(30, 1), false, fps, 0.0, 3.2, true),
        clip(&mut pool, &p24, Fps::new(24, 1), false, fps, 3.2, 4.0, true),
        clip(&mut pool, &p10, Fps::new(10, 1), false, fps, 4.0, 8.0, true),
    ];
    let primary = vec![
        clip(&mut pool, &p24, Fps::new(24, 1), false, fps, 0.0, 3.0, false),
        clip(&mut pool, &p25, Fps::new(25, 1), false, fps, 3.0, 8.0, false),
    ];
    let files = pool.all().map(|m| (m.id, m.path.clone())).collect();
    let mut seq = sequence(fps, vec![layers.clone(), primary]);
    if sound {
        let tone = tone()?;
        let id = MediaId::new();
        let tc = |s: f64| RationalTime::from_secs_f64(s, fps.rate());
        pool.add(MediaItem {
            id,
            path: tone.clone(),
            kind: klipp_media::MediaItemKind::File,
            has_audio: true,
            audio_codec: "pcm_s16le".into(),
            audio_channels: Some(2),
            ..MediaItem::new_synthetic_title(id, tone.display().to_string(), tc(8.0), fps)
        });
        let mut a1 = Track::new("A1", TrackKind::Audio);
        a1.clips = vec![Clip { kind: ClipKind::Media { media_id: id }, source_in: tc(0.0), source_out: tc(8.0), ..Clip::gap(tc(0.0), tc(8.0)) }];
        seq.tracks.push(a1);
    }
    Some((seq, pool, layers, files))
}

/// Playing across cuts between rates, each layer frame the viewer draws
/// is the one export draws at that timeline frame (its file frame's time),
/// and every timeline frame where export's layer frame moves on is drawn.
#[test]
fn played_across_mixed_rate_cuts_the_layers_show_exports_frames() {
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let Some((seq, pool, layers, files)) = mixed_rates(false) else { return eprintln!("skipping: no ffmpeg") };
    let fps = seq.fps;
    let played = play(seq, pool, 2.0, 3.0);
    let media: std::collections::HashMap<MediaId, klipp_render::MediaFile> =
        files.into_iter().map(|(id, path)| (id, klipp_render::MediaFile { path, ..Default::default() })).collect();
    let mut export = klipp_render::ExportFrameSource::new(&media, fps.as_f64(), WorkingColorspace::default());
    let frame_of = |c: &Clip, asked: f64| ((c.timeline_in.to_secs_f64() + asked - c.source_in.to_secs_f64()) * fps.as_f64()).round() as i64;
    let mut checked = 0;
    for c in &layers {
        // The viewer's frame for each timeline frame it drew the layer at.
        let mut drawn: std::collections::BTreeMap<i64, f64> = std::collections::BTreeMap::new();
        for (id, asked, pts) in &played.given {
            if *id == c.id {
                let n = frame_of(c, *asked);
                assert!(((c.timeline_in.to_secs_f64() + asked) * fps.as_f64() - n as f64).abs() < 1e-3, "{:?}: asked between timeline frames, at {asked}", c.id);
                drawn.insert(n, pts.expect("a decoded frame"));
            }
        }
        let (Some(&first), Some(&last)) = (drawn.keys().next(), drawn.keys().next_back()) else { continue };
        let mut before = None;
        for n in first..=last {
            let secs = klipp_compose::render::source_secs(c, RationalTime::from_frames(n * i64::from(fps.den), fps.rate()), fps.as_f64());
            let exported = klipp_compose::FrameSource::frame(&mut export, c, secs, W, H).and_then(|f| f.pts).expect("export decodes the layer");
            if let Some(&shown) = drawn.get(&n) {
                assert!((shown - exported).abs() < 1e-4, "timeline frame {n}: the viewer drew the layer's {shown:.4} s, export {exported:.4} s");
                checked += 1;
            } else if before.is_some_and(|b: f64| (b - exported).abs() > 1e-4) {
                panic!("timeline frame {n}: export's layer frame moves on to {exported:.4} s, and the viewer did not draw it");
            }
            before = Some(exported);
        }
    }
    eprintln!("MEASURE layer frames checked against export: {checked}");
    assert!(layers.iter().all(|c| played.given.iter().any(|(id, _, _)| *id == c.id)), "each layer was drawn");
    assert!(checked > 60, "only {checked} layer frames drawn in ~3 s");
}

/// Playing mixed rates across cuts, with sound: each picture is shown
/// within a frame of the clock both sound and picture follow (sync.rs's
/// master clock, wall time from Play at 1×), drawn again on the
/// sequence's clock or not.
#[test]
fn played_across_mixed_rate_cuts_the_picture_keeps_to_the_clock() {
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let Some((seq, pool, _, _)) = mixed_rates(true) else { return eprintln!("skipping: no ffmpeg") };
    let frame = seq.fps.frame_duration_secs();
    let played = play(seq, pool, 2.0, 3.0);
    // Past the start: the clock runs from when Play arrives.
    let offsets: Vec<f64> = played
        .shown
        .iter()
        .zip(&played.shown_when)
        .filter(|(_, w)| w.duration_since(played.played_at) > Duration::from_millis(300))
        .map(|(at, w)| played.from.to_secs_f64() + w.duration_since(played.played_at).as_secs_f64() - at.to_secs_f64())
        .collect();
    assert!(offsets.len() > 60, "only {} pictures in ~3 s", offsets.len());
    let mut sorted = offsets.clone();
    sorted.sort_by(f64::total_cmp);
    let median = sorted[sorted.len() / 2];
    let worst = offsets.iter().map(|o| (o - median).abs()).fold(0.0, f64::max);
    eprintln!("MEASURE picture vs clock over {} pictures: median {:.1} ms behind, worst {:.1} ms from it, range {:.1}..{:.1} ms", offsets.len(), median * 1e3, worst * 1e3, sorted[0] * 1e3, sorted[sorted.len() - 1] * 1e3);
    assert!(worst <= frame, "a picture {:.1} ms off the clock's steady lag, over a frame ({:.1} ms)", worst * 1e3, frame * 1e3);
}
