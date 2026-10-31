//! #3210: a preview up on the shared sequence is drawn on the parked
//! frame as applying it would draw it, and taken down shows the frame as
//! it was, never the preview kept in a cache. The document's sequence is
//! not changed by it.

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use klipp_core::*;
use klipp_effects::BuiltinEffect;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, EffectEdit, Sequence, SharedSequence, Track, TrackKind};

const W: u32 = 1920;
const H: u32 = 1080;
const FPS: u32 = 25;

/// Three seconds of 1080p test pattern, every frame a keyframe. `None`
/// without ffmpeg.
fn hd() -> Option<std::path::PathBuf> {
    let out = std::env::temp_dir().join("klipp-hd-testsrc2-intra-3s.mp4");
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

/// Mean difference over red, green and blue, in levels.
fn mean_diff(a: &[u8], b: &[u8]) -> f64 {
    let (sum, n) = a.chunks_exact(4).zip(b.chunks_exact(4)).fold((0u64, 0u64), |(s, n), (p, q)| (s + (0..3).map(|c| u64::from(p[c].abs_diff(q[c]))).sum::<u64>(), n + 3));
    sum as f64 / n.max(1) as f64
}

#[test]
fn a_preview_is_drawn_as_applying_it_would_and_leaves_nothing_behind() {
    let Some(path) = hd() else {
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
    let clip = Clip { kind: ClipKind::Media { media_id: media }, source_in: tc(0.0), source_out: tc(3.0), ..Clip::gap(tc(0.0), tc(3.0)) };
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![clip];
    let mut seq = Sequence::new_default();
    seq.fps = fps;
    seq.resolution = Resolution { width: W, height: H };
    seq.tracks = vec![v1];
    // The clip as a blur applied leaves it.
    let mut applied = seq.clone();
    let blur = BuiltinEffect::GaussianBlur { radius: 16.0 };
    assert!(matches!(applied.tracks[0].clips[0].apply_effect_edits(&[EffectEdit::Add(&blur)]), Some(Ok(_))), "the blur applies");
    let applied = Arc::new(applied);

    let shared = Arc::new(SharedSequence::new(seq.clone()));
    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
    std::thread::spawn(move || while ev_rx.recv().is_ok() {});
    let fb = Arc::new(Mutex::new(FrameBuffer::new(W, H)));
    let engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(W, H))),
        Arc::clone(&shared), Arc::new(RwLock::new(pool)), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
    );
    let thread = std::thread::spawn(move || engine.run());
    // The frame drawn after `send`, once settled.
    let frame = |send: &dyn Fn()| -> Vec<u8> {
        let g = common::generation(&fb);
        send();
        assert!(common::wait_new(&fb, g, Duration::from_secs(30)).is_some(), "no frame was drawn");
        common::settle(&fb);
        fb.lock().unwrap().latest_frame().0.to_vec()
    };
    let original = frame(&|| cmd.send(AppCommand::SetPlayhead(tc(1.0))).unwrap());
    let doc = serde_json::to_string(&*shared.snapshot()).unwrap();
    let base = shared.revision();
    let previewed = frame(&|| {
        assert!(shared.set_preview(base, Arc::clone(&applied)), "the preview is taken");
        cmd.send(AppCommand::RefreshPreview).unwrap();
    });
    let skipped = fb.lock().unwrap().preview_skipped;
    let down = frame(&|| {
        assert!(shared.clear_preview());
        cmd.send(AppCommand::RefreshPreview).unwrap();
    });
    assert_eq!(serde_json::to_string(&*shared.snapshot()).unwrap(), doc, "the document's sequence is as it was");
    assert_eq!(shared.revision(), base, "and no edit was made");
    // Applied for real, then undone.
    let real = frame(&|| {
        shared.replace((*applied).clone());
        cmd.send(AppCommand::SequenceUpdated).unwrap();
    });
    let undone = frame(&|| {
        shared.replace(seq.clone());
        cmd.send(AppCommand::SequenceUpdated).unwrap();
    });
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();
    klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));

    let (blurred, versus_real, after_down, after_undo) = (mean_diff(&previewed, &original), mean_diff(&previewed, &real), mean_diff(&down, &original), mean_diff(&undone, &original));
    eprintln!("preview {blurred:.2} levels from the frame, {versus_real:.3} from the blur applied; taken down {after_down:.3}, undone {after_undo:.3}; skipped {skipped}");
    assert!(!skipped, "the GPU stack drew the preview");
    assert!(blurred > 1.0, "the preview shows the blur: {blurred:.2} levels from the frame");
    assert!(versus_real < 0.25, "the preview is the blur applied: {versus_real:.3} levels apart");
    assert!(after_down < 0.25, "taken down, the frame is the one before: {after_down:.3} levels apart");
    assert!(after_undo < 0.25, "and the same after the blur applied and undone: {after_undo:.3}");
}

/// How long a preview takes to show (#3210), for every effect the
/// browser lists, as it adds it: from the preview put up on the parked
/// 1080p frame to that frame drawn. A perf probe: run it explicitly,
/// alone on the GPU.
#[test]
#[ignore = "perf probe: run explicitly"]
fn a_preview_shows_within_a_frame_for_each_effect() {
    let Some(path) = hd() else {
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
    let clip = Clip { kind: ClipKind::Media { media_id: media }, source_in: tc(0.0), source_out: tc(3.0), ..Clip::gap(tc(0.0), tc(3.0)) };
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![clip];
    let mut seq = Sequence::new_default();
    seq.fps = fps;
    seq.resolution = Resolution { width: W, height: H };
    seq.tracks = vec![v1];
    let shared = Arc::new(SharedSequence::new(seq.clone()));
    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
    std::thread::spawn(move || while ev_rx.recv().is_ok() {});
    let fb = Arc::new(Mutex::new(FrameBuffer::new(W, H)));
    let engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(W, H))),
        Arc::clone(&shared), Arc::new(RwLock::new(pool)), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
    );
    let thread = std::thread::spawn(move || engine.run());
    let g = common::generation(&fb);
    cmd.send(AppCommand::SetPlayhead(tc(1.0))).unwrap();
    assert!(common::wait_new(&fb, g, Duration::from_secs(30)).is_some(), "no frame was drawn");
    common::settle(&fb);
    let mut times: Vec<(f64, &str)> = Vec::new();
    let mut skipped = Vec::new();
    for t in klipp_effects::registry::effect_types() {
        let klipp_effects::registry::Browser::Listed { start, .. } = &t.browser else { continue };
        let effect = start.map_or_else(|| (t.sample)(), |s| s());
        if matches!(effect, BuiltinEffect::SpeedRamp { .. }) {
            continue;
        }
        let mut with = seq.clone();
        if !matches!(with.tracks[0].clips[0].apply_effect_edits(&[EffectEdit::Add(&effect)]), Some(Ok(_))) {
            continue;
        }
        // Twice: the first time an effect is drawn its shaders are made.
        for warm in [true, false] {
            let g = common::generation(&fb);
            let t0 = std::time::Instant::now();
            assert!(shared.set_preview(shared.revision(), Arc::new(with.clone())));
            cmd.send(AppCommand::RefreshPreview).unwrap();
            let shown = common::wait_new(&fb, g, Duration::from_secs(10)).is_some();
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            if !warm && shown {
                if fb.lock().unwrap().preview_skipped {
                    skipped.push(t.tag);
                } else {
                    times.push((ms, t.tag));
                }
            }
            let g = common::generation(&fb);
            shared.clear_preview();
            cmd.send(AppCommand::RefreshPreview).unwrap();
            let _ = common::wait_new(&fb, g, Duration::from_secs(10));
        }
    }
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();
    klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));
    times.sort_by(|a, b| a.0.total_cmp(&b.0));
    let at = |q: f64| times.get(((times.len() as f64 - 1.0) * q).round() as usize).map_or(0.0, |t| t.0);
    eprintln!("{} effects previewed: p50 {:.1} ms, p90 {:.1} ms, slowest {:?}", times.len(), at(0.5), at(0.9), times.iter().rev().take(5).collect::<Vec<_>>());
    eprintln!("not drawn on the GPU stack, so not previewed: {skipped:?}");
    let over: Vec<_> = times.iter().filter(|t| t.0 > 1000.0 / 60.0).collect();
    eprintln!("over a 60 fps frame (16.7 ms): {over:?}");
}
