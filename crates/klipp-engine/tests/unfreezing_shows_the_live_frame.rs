//! A track unfrozen while the viewer is parked on it shows its live frame
//! at once, and a seek after shows live frames only (#3207). The frozen
//! frame is drawn the export's way and differs from the live one by a
//! level or so, so the test first finds it on screen while frozen; a
//! frame parked from it and only regraded on the unfreeze would keep it.

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 1920;
const H: u32 = 1080;
/// The source, smaller than the timeline as in the report.
const SW: u32 = 1280;
const SH: u32 = 720;

/// Ten seconds of a moving test pattern in H.264: a stale frame shows
/// the wrong picture, not a shade off.
fn blue() -> Option<std::path::PathBuf> {
    let out = std::env::temp_dir().join(format!("klipp-unfreeze-pattern-{SW}x{SH}.mp4"));
    if out.exists() {
        return Some(out);
    }
    let partial = common::unshared(&out, "mp4");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", &format!("testsrc2=s={SW}x{SH}:r={}:d=10", common::FPS)])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"])
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

/// A ten-second 1080p timeline of the pattern, and its pool.
fn project() -> Option<(Sequence, MediaPool)> {
    let fps = Fps::new(common::FPS, 1);
    let tc = |f| common::frame_tc(f, fps);
    let frames = 10 * common::FPS as i64;
    let path = blue()?;
    let id = MediaId::new();
    let mut pool = MediaPool::default();
    pool.add(MediaItem {
        id,
        path: path.clone(),
        kind: klipp_media::MediaItemKind::File,
        resolution: Resolution { width: SW, height: SH },
        is_still: false,
        has_audio: false,
        fps,
        ..MediaItem::new_synthetic_title(id, path.display().to_string(), tc(frames), fps)
    });
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![Clip { kind: ClipKind::Media { media_id: id }, source_in: tc(0), source_out: tc(frames), ..Clip::gap(tc(0), tc(frames)) }];
    let mut seq = Sequence::new_default();
    seq.fps = fps;
    seq.resolution = Resolution { width: W, height: H };
    seq.tracks = vec![v1];
    Some((seq, pool))
}

/// The newest frame's bytes.
fn frame(fb: &Mutex<FrameBuffer>) -> (Vec<u8>, u32, u32) {
    let fb = fb.lock().unwrap();
    let (data, w, h, _) = fb.latest_frame();
    (data.to_vec(), w, h)
}

/// Mean difference over red, green and blue between two frames of one
/// size, in levels; `None` when their sizes differ.
fn mean_diff(a: &(Vec<u8>, u32, u32), b: &(Vec<u8>, u32, u32)) -> Option<f64> {
    if (a.1, a.2) != (b.1, b.2) || a.0.len() != b.0.len() || a.0.is_empty() {
        return None;
    }
    let (sum, n) = a.0.chunks_exact(4).zip(b.0.chunks_exact(4)).fold((0u64, 0u64), |(s, n), (p, q)| (s + (0..3).map(|c| u64::from(p[c].abs_diff(q[c]))).sum::<u64>(), n + 3));
    Some(sum as f64 / n.max(1) as f64)
}

/// Parked again at `secs`, settled.
fn seek(cmd: &crossbeam_channel::Sender<AppCommand>, fb: &Mutex<FrameBuffer>, secs: f64) {
    let g = common::generation(fb);
    cmd.send(AppCommand::SetPlayhead(common::frame_tc((secs * f64::from(common::FPS)).round() as i64, Fps::new(common::FPS, 1)))).unwrap();
    let _ = common::wait_new(fb, g, std::time::Duration::from_secs(10));
    common::settle(fb);
}

/// A parked engine on `seq` at frame 10, its track frozen from `folder`
/// when given; its command sender, frame buffer and thread.
fn parked(seq: &Sequence, pool: &MediaPool, frozen: Option<(&std::path::Path, TrackId)>) -> (crossbeam_channel::Sender<AppCommand>, Arc<Mutex<FrameBuffer>>, klipp_engine::RunningEngine) {
    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
    std::thread::spawn(move || while ev_rx.recv().is_ok() {});
    let fb = Arc::new(Mutex::new(FrameBuffer::new(W, H)));
    let engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(W, H))),
        Arc::new(klipp_timeline::SharedSequence::new(seq.clone())), Arc::new(RwLock::new(pool.clone())), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
    );
    let thread = engine.spawn();
    if let Some((folder, track)) = frozen {
        cmd.send(AppCommand::SetFreezeFolder(Some(folder.to_path_buf()))).unwrap();
        cmd.send(AppCommand::SetFrozenTracks(vec![track])).unwrap();
    }
    // Waited for, not settled on alone: the buffer starts out empty.
    let g = common::generation(&fb);
    cmd.send(AppCommand::SetPlayhead(common::frame_tc(10, seq.fps))).unwrap();
    assert!(common::wait_new(&fb, g, std::time::Duration::from_secs(30)).is_some(), "no frame was drawn");
    common::settle(&fb);
    (cmd, fb, thread)
}

#[test]
fn unfreezing_a_parked_track_shows_its_live_frame() {
    let Some((seq, pool)) = project() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let track = seq.tracks[0].id;
    // The live frame at 3 s, from an engine that never froze it.
    let (cmd, fb, thread) = parked(&seq, &pool, None);
    seek(&cmd, &fb, 2.0);
    let live_parked = frame(&fb);
    seek(&cmd, &fb, 3.0);
    let live = frame(&fb);
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();

    // The track's frames stored at both sizes the viewer draws, as the
    // app freezes it, into the project's folder.
    let folder = std::env::temp_dir().join(format!("klipp-unfreeze-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    let store = Arc::new(Mutex::new(klipp_engine::cache_store::CacheStore::open(&folder, 1 << 30)));
    store.lock().unwrap().pin(&klipp_engine::freeze::namespace(track));
    let working = klipp_engine::video_decoder::working_colorspace();
    let files: &'static HashMap<MediaId, klipp_render::MediaFile> = Box::leak(Box::new(pool.all().map(|i| (i.id, klipp_render::MediaFile::of(i))).collect()));
    let fps = seq.fps.as_f64();
    let frames: Vec<Timecode> = (0..10 * common::FPS as i64).map(|f| common::frame_tc(f, seq.fps)).collect();
    for size in [klipp_engine::freeze::playback_size((W, H)), (W, H)] {
        let req = klipp_engine::freeze::FreezeRequest {
            seq: Arc::new(seq.clone()),
            sequences: Arc::new(vec![]),
            track: 0,
            track_id: track,
            frames: frames.clone(),
            size,
            working,
            draw: klipp_engine::freeze::DrawSettings::current(true),
            environment: klipp_engine::freeze::environment(working, true, klipp_engine::freeze::DrawSettings::current(true)),
            media: Arc::new(klipp_engine::freeze::media_identities(&pool)),
        };
        klipp_engine::freeze::spawn_freeze(req, Arc::clone(&store), move || klipp_render::ExportFrameSource::new(files, fps, working), |_| {}).join();
    }
    let stored = store.lock().unwrap().bytes(&klipp_engine::freeze::namespace(track));

    let (cmd, fb, thread) = parked(&seq, &pool, Some((folder.as_path(), track)));
    seek(&cmd, &fb, 2.0);
    let frozen_laid = klipp_engine::frozen_counts();
    let frozen_parked = frame(&fb);
    cmd.send(AppCommand::SetFrozenTracks(Vec::new())).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let unfrozen_parked = frame(&fb);
    let before_seek = klipp_engine::frozen_counts();
    seek(&cmd, &fb, 3.0);
    let after = klipp_engine::frozen_counts();
    let unfrozen = frame(&fb);
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();
    let _ = std::fs::remove_dir_all(&folder);
    let diff = mean_diff(&unfrozen, &live);
    let (frozen_off, unfrozen_off) = (mean_diff(&frozen_parked, &live_parked), mean_diff(&unfrozen_parked, &live_parked));
    eprintln!("MEASURE at 2 s, parked: frozen vs live {frozen_off:?}, unfrozen (before any seek) vs live {unfrozen_off:?}");
    eprintln!("MEASURE stored {stored} bytes; frozen (laid, skipped) parked {frozen_laid:?}, unfrozen {before_seek:?}, after the seek {after:?}; unfrozen vs live at 3 s: {diff:?} levels, sizes {:?} {:?}", (unfrozen.1, unfrozen.2), (live.1, live.2));
    assert!(stored > 0, "the freeze stored frames");
    assert!(frozen_laid.0 > 0, "frozen, the stored frames were laid: {frozen_laid:?}");
    match (frozen_off, unfrozen_off) {
        (Some(f), Some(u)) if f > 0.1 => assert!(u < 0.1, "unfrozen, the parked frame is the live one at once: {u:.2} levels off it, the frozen one {f:.2}"),
        // Drawn alike both ways: nothing tells the frozen frame apart.
        _ => eprintln!("UNVERIFIED: the frozen frame reads as the live one at 2 s"),
    }
    assert_eq!(after.0, before_seek.0, "no frozen layer laid once unfrozen");
    let diff = diff.expect("frames of one size");
    assert!(diff < 1.0, "unfrozen, the live frame at 3 s: {diff:.2} levels off it");
}
