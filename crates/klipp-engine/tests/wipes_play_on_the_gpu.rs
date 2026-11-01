//! Shape wipes play both their sides (#2863): during a Star Wipe or a
//! Clock Wipe the incoming clip held one frame, or none, inside the shape
//! until about the cut, where it began to move, and the frame left the
//! GPU stack. `frame_at` drew it moving.
//!
//! Every shape wipe, at each alignment (centred on the cut, starting at
//! it, ending at it), cuts between 4K clips with handles, and the whole
//! timeline plays on the viewer path with a GPU (`KLIPP_TEST_REQUIRE_GPU=1`
//! fails without one). Through each transition each side is handed the
//! source frame of the time asked for, within a frame and a half, frame
//! after frame, from the transition's first frame; nothing falls back;
//! and the frames presented a second are read.
//!
//! Its own binary: the counters and the frames given it reads are the
//! process's, and the device is installed as the UI's, as the app does.

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind, Transition};

const W: u32 = 3840;
const H: u32 = 2160;
const FPS: Fps = Fps { num: 30000, den: 1001 };
/// Each clip's length on the timeline, and the transitions'.
const CLIP: f64 = 3.0;
const TRANSITION: f64 = 1.5;
/// Where each clip starts in the file, in turn: two seconds of head
/// handle and two of tail at least, for a transition at either end.
const SOURCE_INS: [f64; 3] = [2.0, 4.5, 7.0];

/// Every shape wipe.
const WIPES: [TransitionKind; 19] = [
    TransitionKind::WipeLeft,
    TransitionKind::WipeRight,
    TransitionKind::WipeUp,
    TransitionKind::WipeDown,
    TransitionKind::CircleWipeIn,
    TransitionKind::CircleWipeOut,
    TransitionKind::ClockWipe,
    TransitionKind::BarnDoorOpen,
    TransitionKind::BarnDoorClose,
    TransitionKind::StarWipe,
    TransitionKind::SplitWipeHorizontal,
    TransitionKind::SplitWipeVertical,
    TransitionKind::VenetianBlindHorizontal,
    TransitionKind::VenetianBlindVertical,
    TransitionKind::CheckerboardWipe,
    TransitionKind::DiagonalWipeTLBR,
    TransitionKind::DiagonalWipeTRBL,
    TransitionKind::BoxWipeIn,
    TransitionKind::BoxWipeOut,
];

const ALIGNMENTS: [TransitionAlignment; 3] = [TransitionAlignment::Center, TransitionAlignment::StartAtCut, TransitionAlignment::EndAtCut];

fn tc(secs: f64) -> RationalTime {
    RationalTime::from_secs_f64(secs, FPS.num)
}

/// Twelve seconds of 4K H.264 test pattern, moving every frame, a
/// keyframe a second as a camera writes them. `None` without ffmpeg.
fn source() -> Option<std::path::PathBuf> {
    let out = std::env::temp_dir().join("klipp-wipes-4k-12s-gop30.mp4");
    if out.exists() {
        return Some(out);
    }
    let partial = common::unshared(&out, "mp4");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", &format!("testsrc2=s={W}x{H}:r=30000/1001:d=12")])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-g", "30", "-pix_fmt", "yuv420p"])
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

fn ready() -> Option<std::path::PathBuf> {
    if let Some(ctx) = klipp_effects::gpu_context::GpuContext::new() {
        let _ = klipp_effects::gpu_context::GpuContext::install_shared(ctx);
    }
    let required = std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_some_and(|v| v == "1");
    if required {
        assert!(klipp_effects::gpu_context::GpuContext::shared_is_ui_device(), "the device is the viewer's");
        assert!(klipp_effects::gpu_context::GpuContext::shared().is_some(), "GPU required");
    } else if klipp_effects::gpu_context::GpuContext::shared().is_none() {
        eprintln!("skipping: no GPU");
        return None;
    }
    let src = source();
    if src.is_none() {
        assert!(!required, "ffmpeg is needed for the 4K source");
        eprintln!("skipping: no ffmpeg");
    }
    src
}

/// The timeline: a clip a wipe, each wipe at the next alignment in turn.
fn timeline(media: MediaId) -> Track {
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = (0..=WIPES.len())
        .map(|i| {
            let from = i as f64 * CLIP;
            let s = SOURCE_INS[i % SOURCE_INS.len()];
            Clip { kind: ClipKind::Media { media_id: media }, source_in: tc(s), source_out: tc(s + CLIP), ..Clip::gap(tc(from), tc(from + CLIP)) }
        })
        .collect();
    v1.transitions = WIPES
        .iter()
        .enumerate()
        .map(|(i, &kind)| Transition {
            id: TransitionId::new(),
            kind,
            duration: tc(TRANSITION),
            clip_a_id: v1.clips[i].id,
            clip_b_id: v1.clips[i + 1].id,
            user_params: spec_for_kind(kind).default_params,
            alignment: ALIGNMENTS[i % ALIGNMENTS.len()],
        })
        .collect();
    v1
}

struct Running {
    cmd: crossbeam_channel::Sender<AppCommand>,
    fb: Arc<Mutex<FrameBuffer>>,
    thread: klipp_engine::RunningEngine,
}

fn start(video: &std::path::Path, track: Track) -> Running {
    let media = track.clips[0].media_id().expect("media clips");
    let mut pool = MediaPool::default();
    pool.add(MediaItem {
        id: media,
        path: video.to_path_buf(),
        kind: klipp_media::MediaItemKind::File,
        resolution: Resolution { width: W, height: H },
        is_still: false,
        has_audio: false,
        fps: FPS,
        ..MediaItem::new_synthetic_title(media, video.display().to_string(), tc(12.0), FPS)
    });
    let mut seq = Sequence::new_default();
    seq.fps = FPS;
    seq.resolution = Resolution { width: W, height: H };
    seq.tracks = vec![track];
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
    Running { cmd, fb, thread }
}

fn presents(r: &Running) -> u64 {
    let fb = r.fb.lock().unwrap();
    fb.gpu_output_frame + fb.latest_generation()
}

fn picture_fallbacks() -> std::collections::BTreeMap<String, u64> {
    klipp_effects::residency::snapshot().by_feature.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}

#[test]
fn every_shape_wipe_plays_both_sides_on_the_gpu() {
    let Some(video) = ready() else { return };
    let media = MediaId::new();
    let track = timeline(media);
    let r = start(&video, track.clone());
    let g = common::generation(&r.fb);
    r.cmd.send(AppCommand::SetPlayhead(tc(0.5))).unwrap();
    common::wait_new(&r.fb, g, Duration::from_secs(20));
    common::settle(&r.fb);
    let secs = (WIPES.len() + 1) as f64 * CLIP - 1.0;
    r.cmd.send(AppCommand::Play).unwrap();
    // From the first frames played: the clip on screen when play began
    // is the primary's, decoded already.
    std::thread::sleep(Duration::from_millis(300));
    let before = picture_fallbacks();
    klipp_engine::engine::keep_frames_given();
    let t0 = Instant::now();
    let mut shown = 0u64;
    let mut last = presents(&r);
    while t0.elapsed() < Duration::from_secs_f64(secs - 0.5) {
        let p = presents(&r);
        if p != last {
            shown += 1;
            last = p;
        }
        std::thread::sleep(Duration::from_micros(500));
    }
    let played = t0.elapsed().as_secs_f64();
    let given = klipp_engine::engine::frames_given();
    let after = picture_fallbacks();
    let _ = r.cmd.send(AppCommand::Pause);
    let _ = r.cmd.send(AppCommand::Quit);
    let _ = r.thread.join();
    klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));

    let fps = shown as f64 / played;
    let fell: Vec<(String, u64)> = after.iter().map(|(k, v)| (k.clone(), v - before.get(k).copied().unwrap_or(0))).filter(|(_, n)| *n > 0).collect();
    eprintln!("4K shape wipes played: {fps:.1} frames presented a second; picture fallbacks {fell:?}");

    // Each transition, each side: the frames it was handed while the
    // transition ran, against the source time each was asked for.
    let fps_media = FPS.as_f64();
    let mut wrong = Vec::new();
    for tr in &track.transitions {
        let (start, end) = klipp_timeline::crossfade::window(&track, tr).expect("both clips");
        for (side, id) in [("outgoing", tr.clip_a_id), ("incoming", tr.clip_b_id)] {
            let c = track.find_clip(id).expect("on the track");
            let range = (klipp_compose::render::source_secs(c, start, fps_media), klipp_compose::render::source_secs(c, end, fps_media));
            let during: Vec<&(ClipId, f64, Option<f64>)> = given.iter().filter(|(clip, asked, _)| *clip == id && *asked >= range.0 - 1e-6 && *asked < range.1).collect();
            let off: Vec<(f64, Option<f64>)> = during.iter().filter(|(_, asked, pts)| pts.is_none_or(|p| (p - asked).abs() > 1.5 / fps_media)).map(|(_, a, p)| (*a, *p)).collect();
            let distinct = {
                let mut p: Vec<i64> = during.iter().filter_map(|(_, _, pts)| pts.map(|p| (p * 1000.0).round() as i64)).collect();
                p.dedup();
                p.len()
            };
            let frames = (TRANSITION * fps_media).round() as usize;
            if during.is_empty() || !off.is_empty() || distinct * 2 < frames {
                wrong.push(format!(
                    "{:?} {:?} {side}: {} frames asked, {distinct} distinct, {} not the frame asked (first {:?})",
                    tr.kind,
                    tr.alignment,
                    during.len(),
                    off.len(),
                    off.first()
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "sides that held a frame or had none:\n{}", wrong.join("\n"));
    assert!(fell.is_empty(), "picture fallbacks while it played: {fell:?}");
}
