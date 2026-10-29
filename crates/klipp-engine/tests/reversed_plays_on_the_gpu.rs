//! A reversed clip plays on the GPU decode path at speed (#2798): 4K
//! long-GOP H.264 reversed played at 12 fps, each frame decoded back from
//! its keyframe and made into a 4K picture on the CPU, and at a tenth
//! speed the same frame was made again for every frame shown.
//!
//! Measured on the viewer path with a GPU (`KLIPP_TEST_REQUIRE_GPU=1`
//! fails without one): reversed plays at 29 fps or more; at −0.1 only the
//! source frames due are handed out, about 3 a second; a cut from the
//! clip forward to it reversed shows no stall; and none of it falls back
//! or reads back.
//!
//! Its own binary: the counters it reads are the process's, and the
//! device is installed as the UI's, before the engine makes its
//! pipeline, as the app does at start: frames go to the viewer's texture
//! and nothing comes back as bytes while playing.

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 3840;
const H: u32 = 2160;
/// 29.97, a keyframe every 90 frames, as the issue's camera file.
const FPS: Fps = Fps { num: 30000, den: 1001 };

/// One test plays at a time: two 4K engines decoding at once each fall
/// behind, and the counters read are the process's.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn gpu_required() -> bool {
    std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1")
}

/// Eight seconds of 4K H.264, a keyframe every 90 frames. `None` without
/// ffmpeg.
fn source() -> Option<std::path::PathBuf> {
    let out = std::env::temp_dir().join("klipp-reversed-4k-gop90.mp4");
    if out.exists() {
        return Some(out);
    }
    let partial = common::unshared(&out, "mp4");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", &format!("testsrc2=s={W}x{H}:r=30000/1001:d=8")])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-profile:v", "high", "-g", "90", "-keyint_min", "90", "-sc_threshold", "0", "-pix_fmt", "yuv420p"])
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

/// Eight seconds of 4K H.264 panning across a still, blurred noise, a
/// keyframe every 90 frames: motion optical flow follows everywhere,
/// where the test pattern's flat areas and changing shapes are cross-faded
/// as motion it can't. `None` without ffmpeg.
fn pan_source() -> Option<std::path::PathBuf> {
    let out = std::env::temp_dir().join("klipp-reversed-4k-noise-pan-gop90.mp4");
    if out.exists() {
        return Some(out);
    }
    let partial = common::unshared(&out, "mp4");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "nullsrc=s=4800x2700:d=1,geq=lum='random(1)*255':cb=128:cr=128,gblur=sigma=3"])
        .args(["-vf", &format!("loop=-1:1:0,fps=30000/1001,crop={W}:{H}:x='40+t*90':y='30+t*40',trim=duration=8")])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-profile:v", "high", "-g", "90", "-keyint_min", "90", "-sc_threshold", "0", "-pix_fmt", "yuv420p"])
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

fn tc(secs: f64) -> RationalTime {
    RationalTime::from_secs_f64(secs, FPS.num)
}

struct Running {
    cmd: crossbeam_channel::Sender<AppCommand>,
    fb: Arc<Mutex<FrameBuffer>>,
    thread: std::thread::JoinHandle<()>,
}

/// An engine on `clips` of the 4K source, one track.
fn start(video: &std::path::Path, clips: Vec<Clip>) -> Running {
    let media = clips[0].media_id().expect("media clips");
    let mut pool = MediaPool::default();
    pool.add(MediaItem {
        id: media,
        path: video.to_path_buf(),
        kind: klipp_media::MediaItemKind::File,
        resolution: Resolution { width: W, height: H },
        is_still: false,
        has_audio: false,
        fps: FPS,
        ..MediaItem::new_synthetic_title(media, video.display().to_string(), tc(8.0), FPS)
    });
    let mut seq = Sequence::new_default();
    seq.fps = FPS;
    seq.resolution = Resolution { width: W, height: H };
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = clips;
    seq.tracks = vec![v1];
    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
    std::thread::spawn(move || while ev_rx.recv().is_ok() {});
    let fb = Arc::new(Mutex::new(FrameBuffer::new(W, H)));
    let engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(W, H))),
        Arc::new(klipp_timeline::SharedSequence::new(seq)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
    );
    let thread = std::thread::spawn(move || engine.run());
    Running { cmd, fb, thread }
}

fn stop(r: Running) {
    let _ = r.cmd.send(AppCommand::Pause);
    let _ = r.cmd.send(AppCommand::Quit);
    let _ = r.thread.join();
    klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));
}

/// A clip of the whole source from timeline 0, at `speed`.
fn clip_at(media: MediaId, speed: f64, from: f64, secs: f64) -> Clip {
    let mut c = Clip { kind: ClipKind::Media { media_id: media }, source_in: tc(0.0), source_out: tc(8.0), ..Clip::gap(tc(from), tc(from + secs)) };
    c.speed = speed;
    c
}

/// Frames presented so far: to the viewer's texture without bytes, and
/// as bytes.
fn presents(r: &Running) -> u64 {
    let fb = r.fb.lock().unwrap();
    fb.gpu_output_frame + fb.latest_generation()
}

/// What playing `secs` from `from` showed: the times of the frames
/// presented (wall seconds since play), the reversed source frames handed
/// out from a second in to the end, and the fallbacks and readbacks while
/// it played (before the pause, whose frame is shown as bytes).
fn play(r: &Running, from: f64, secs: f64) -> (Vec<f64>, u64, (u64, u64, u64)) {
    let g = common::generation(&r.fb);
    r.cmd.send(AppCommand::SetPlayhead(tc(from))).unwrap();
    common::wait_new(&r.fb, g, Duration::from_secs(20));
    common::settle(&r.fb);
    let mut before = fallbacks();
    let t0 = Instant::now();
    r.cmd.send(AppCommand::Play).unwrap();
    let mut shown = Vec::new();
    let mut last = presents(r);
    let mut given_from = None;
    while t0.elapsed() < Duration::from_secs_f64(secs) {
        // From a second in: playing, past the frame the play started on.
        if given_from.is_none() && t0.elapsed() >= Duration::from_secs(1) {
            given_from = Some(klipp_engine::engine::reversed_frames_given());
            before = fallbacks();
        }
        let g = presents(r);
        if g != last {
            last = g;
            shown.push(t0.elapsed().as_secs_f64());
        }
        std::thread::sleep(Duration::from_micros(500));
    }
    let given = klipp_engine::engine::reversed_frames_given() - given_from.unwrap_or(0);
    let after = fallbacks();
    r.cmd.send(AppCommand::Pause).unwrap();
    (shown, given, (after.0 - before.0, after.1 - before.1, after.2 - before.2))
}

/// The process's fallbacks and readbacks so far: picture fallbacks by
/// feature, engine GPU-frame readbacks, blocking effect downloads.
fn fallbacks() -> (u64, u64, u64) {
    let picture: u64 = klipp_effects::residency::snapshot().by_feature.values().sum();
    let engine = klipp_engine::CPU_READBACK_COUNT.load(std::sync::atomic::Ordering::Relaxed);
    let effects = klipp_effects::gpu::readback_stats().count;
    (picture, engine, effects)
}

fn presented_fps(shown: &[f64], after: f64) -> f64 {
    let steady: Vec<f64> = shown.iter().copied().filter(|t| *t >= after).collect();
    match (steady.first(), steady.last()) {
        (Some(a), Some(b)) if b > a => (steady.len() - 1) as f64 / (b - a),
        _ => 0.0,
    }
}

fn ready() -> Option<std::path::PathBuf> {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        if let Some(ctx) = klipp_effects::gpu_context::GpuContext::new() {
            let _ = klipp_effects::gpu_context::GpuContext::install_shared(ctx);
        }
    });
    if gpu_required() {
        assert!(klipp_effects::gpu_context::GpuContext::shared_is_ui_device(), "the device is the viewer's");
        assert!(klipp_effects::gpu_context::GpuContext::shared().is_some(), "GPU required");
    } else if klipp_effects::gpu_context::GpuContext::shared().is_none() {
        eprintln!("skipping: no GPU");
        return None;
    }
    let src = source();
    if src.is_none() {
        assert!(!gpu_required(), "ffmpeg is needed for the 4K source");
        eprintln!("skipping: no ffmpeg");
    }
    src
}

#[test]
fn a_reversed_4k_long_gop_clip_plays_at_speed_on_the_gpu() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let Some(video) = ready() else { return };
    // Timed: what else holds the GPU, said before measuring.
    let load = common::gpu_others();
    eprintln!("reversed 4K: {load}");
    let r = start(&video, vec![clip_at(MediaId::new(), -1.0, 0.0, 8.0)]);
    let (shown, given, fell) = play(&r, 0.5, 4.0);
    stop(r);
    // Distinct source frames a second over the last three seconds: the
    // presented count repeats a frame the display asks for again.
    let fps = given as f64 / 3.0;
    eprintln!("reversed 4K: {fps:.1} source frames a second ({} presents); fallbacks and readbacks {fell:?}", shown.len());
    assert!(fps >= 29.0, "reversed 4K plays at {fps:.1} fps ({load})");
    assert_eq!(fell, (0, 0, 0), "no picture fallbacks, engine readbacks or effect downloads while it played (picture, engine, effects)");
}

#[test]
fn at_a_tenth_reversed_only_the_frames_due_are_made() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let Some(video) = ready() else { return };
    // Eight seconds of source at a tenth: eighty of timeline.
    let r = start(&video, vec![clip_at(MediaId::new(), -0.1, 0.0, 80.0)]);
    let (shown, made, fell) = play(&r, 2.0, 4.0);
    stop(r);
    let presented = presented_fps(&shown, 1.0);
    eprintln!("−0.1 reversed: {made} source frames in the last 3 s, {presented:.1} presents a second");
    // 29.97 × 0.1 = 3 a second: at most 4 a second over 3 s.
    assert!(made <= 3 * 4, "{made} source frames made in 3 s at a tenth");
    assert!(made >= 6, "it plays on: {made} frames in 3 s");
    assert_eq!(fell, (0, 0, 0), "no fallbacks or readbacks at a tenth either");
}

/// Retimed by optical flow at a tenth reversed (#2854), the in-between
/// frames are drawn on the GPU stack: it presents at the timeline's rate,
/// and nothing falls back to the CPU composite or reads back. It left the
/// stack for the CPU, 146 to 208 ms a frame at 4K.
#[test]
fn at_a_tenth_reversed_by_flow_the_frames_stay_on_the_gpu() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    if ready().is_none() {
        return;
    }
    let video = pan_source().expect("ffmpeg made the panning source");
    let flow = klipp_timeline::test_clip::TestClip::of(clip_at(MediaId::new(), -0.1, 0.0, 80.0)).effect(klipp_effects::BuiltinEffect::SpeedRamp { speed: 1.0, blend_mode: 2 }).build();
    let r = start(&video, vec![flow]);
    let (shown, made, fell) = play(&r, 2.0, 5.0);
    stop(r);
    let presented = presented_fps(&shown, 2.0);
    let by_feature = klipp_effects::residency::snapshot().by_feature;
    eprintln!("−0.1 reversed by flow: {presented:.1} presents a second, {made} source frames in the last 4 s; fallbacks and readbacks {fell:?}; by feature {by_feature:?}");
    assert!(presented >= 29.0, "flow at a tenth presents {presented:.1} frames a second");
    assert_eq!(fell, (0, 0, 0), "no picture fallbacks, engine readbacks or effect downloads while it played (picture, engine, effects)");
}

#[test]
fn a_cut_from_the_clip_to_it_reversed_does_not_stall() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let Some(video) = ready() else { return };
    let media = MediaId::new();
    let forward = clip_at(media, 1.0, 0.0, 2.0);
    let mut forward = forward;
    forward.source_out = tc(2.0);
    let mut back = clip_at(media, -1.0, 2.0, 2.0);
    back.source_in = tc(0.0);
    back.source_out = tc(2.0);
    let r = start(&video, vec![forward, back]);
    // From the start, so the cut at 2 s comes well after the first frame
    // presented (this harness shows nothing for its first second and a
    // half of playing, forward as well).
    let (shown, _, _) = play(&r, 0.0, 4.0);
    stop(r);
    // The gaps between presents from the first, the last one running to
    // the end of the watch: a picture that stops showing stalls too.
    let longest = shown.windows(2).map(|w| w[1] - w[0]).chain(shown.last().map(|t| 4.0 - t)).fold(0.0, f64::max);
    eprintln!("presents at {:?}", shown.iter().map(|t| (t * 1000.0).round()).collect::<Vec<_>>());
    eprintln!("forward→reversed cut: longest gap {:.0} ms over {} frames", longest * 1000.0, shown.len());
    assert!(shown.len() > 60, "it played across the cut: {} frames presented", shown.len());
    assert!(longest < 2.5 / FPS.as_f64(), "the picture stalled {:.0} ms at the cut", longest * 1000.0);
}

/// Stepping back a frame at a time while paused, mid-GOP, takes each
/// frame from the window held on the GPU (#2798): after the first step,
/// every step shows its frame within a few milliseconds, for 40 steps at
/// a held key's pace. The old way decoded 30 frames back into a CPU
/// cache, reading each one back, and decoded again from a keyframe on the
/// 31st step; now a frame is converted only for the paused frame shown.
#[test]
fn stepping_back_while_paused_takes_frames_from_the_held_window() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let Some(video) = ready() else { return };
    let r = start(&video, vec![clip_at(MediaId::new(), 1.0, 0.0, 8.0)]);
    // Mid-GOP: keyframes at 0, 3.003 and 6.006 s.
    let g = common::generation(&r.fb);
    r.cmd.send(AppCommand::SetPlayhead(tc(5.0))).unwrap();
    common::wait_new(&r.fb, g, Duration::from_secs(20));
    common::settle(&r.fb);
    let converted = klipp_engine::CPU_READBACK_COUNT.load(std::sync::atomic::Ordering::Relaxed);
    let mut steps = Vec::new();
    for _ in 0..40 {
        let g = common::generation(&r.fb);
        let t = Instant::now();
        r.cmd.send(AppCommand::SeekRelative(-1)).unwrap();
        assert!(common::wait_new(&r.fb, g, Duration::from_secs(5)).is_some(), "a step back shows a frame");
        steps.push(t.elapsed().as_secs_f64() * 1000.0);
        // A held key repeats about 25 times a second.
        std::thread::sleep(Duration::from_millis(40).saturating_sub(t.elapsed()));
    }
    let converted = klipp_engine::CPU_READBACK_COUNT.load(std::sync::atomic::Ordering::Relaxed) - converted;
    stop(r);
    let later = steps[1..].iter().copied().fold(0.0, f64::max);
    eprintln!("stepping back: first {:.0} ms, then at most {later:.0} ms; {converted} frames converted to the CPU", steps[0]);
    assert!(later < 50.0, "a step back from the held window took {later:.0} ms: {steps:?}");
    assert!(converted <= steps.len() as u64, "{converted} frames converted for {} steps: only the paused frame shown", steps.len());
}


/// The GPU's other users are read from `nvidia-smi pmon`: each process
/// with shader time but the test's own, at its highest share, busiest
/// first; idle ones and the header left out.
#[test]
fn the_gpus_other_users_are_named() {
    let pmon = "# gpu  pid  type  sm  mem  enc  dec  jpg  ofa  command\n# Idx  #  C/G  %  %  %  %  %  %  name\n    0  1614  G  14  9  -  -  -  -  Hyprland\n    0  4242  C+G  50  10  -  -  -  -  me\n    0  1153  G  -  -  -  -  -  -  idle\n    0  1614  G  23  15  -  -  -  -  Hyprland\n    0  7777  G  10  7  -  -  -  -  localsend\n";
    assert_eq!(common::busy_on_gpu(pmon, 4242), vec![("Hyprland".to_string(), 23), ("localsend".to_string(), 10)]);
}
