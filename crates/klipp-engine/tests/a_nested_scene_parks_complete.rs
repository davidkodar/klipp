//! A timeline of scenes inside scenes parks complete: the frame at a time
//! is drawn with every level's shots in it within seconds, at any depth,
//! and is the frame export writes there. A template saved from a placed
//! template, placed again, nests that way, every track a scene and no
//! shot of the timeline's own; the engine stayed Idle on such a frame,
//! and the clip streams it had waited for and missed were never looked at
//! again, so frame_at waited out its whole timeout on the second level.

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_render::snapshot::SequenceSnapshot;
use klipp_render::ExportJob;
use std::sync::atomic::{AtomicBool, AtomicU64};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

const W: u32 = 160;
const H: u32 = 90;
const FPS: u32 = 25;

/// Seven seconds of one colour. `None` without ffmpeg.
fn solid(colour: &str) -> Option<std::path::PathBuf> {
    let out = std::env::temp_dir().join(format!("klipp-solid-{colour}-{W}x{H}.mkv"));
    if out.exists() {
        return Some(out);
    }
    let partial = common::unshared(&out, "mkv");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", &format!("color=c={colour}:s={W}x{H}:r={FPS}:d=7"), "-c:v", "ffv1"])
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

/// The timeline for `depth` levels, as a template saved from a placed
/// template nests: a scene of two shots, red on the left and green on
/// the right; each level a scene holding the one below, placed a little
/// later and trimmed at its head; and the top holding every level at
/// once, one a track, with no shot of its own. Every level shows the same
/// two files at the same time.
fn nested(depth: usize, red: MediaId, green: MediaId, fps: Fps) -> (Sequence, Vec<Sequence>) {
    let tc = |s: f64| common::frame_tc((s * f64::from(FPS)).round() as i64, fps);
    let shot = |m: MediaId, from: f64, to: f64| Clip { kind: ClipKind::Media { media_id: m }, source_in: tc(0.0), source_out: tc(to - from), ..Clip::gap(tc(from), tc(to)) };
    let half = |c: Clip, left: bool| {
        klipp_timeline::test_clip::TestClip::of(c).effect(klipp_effects::BuiltinEffect::Crop { left: if left { 0.0 } else { 0.5 }, right: if left { 0.5 } else { 0.0 }, top: 0.0, bottom: 0.0, feather: 0.0, aspect: 0.0, focus_x: 0.5, focus_y: 0.5 }).build()
    };
    let scene = |name: &str, tracks: Vec<Track>| {
        let mut s = Sequence::new_default();
        s.name = name.into();
        s.fps = fps;
        s.resolution = Resolution { width: W, height: H };
        s.tracks = tracks;
        s
    };
    let track = |clips: Vec<Clip>| {
        let mut t = Track::new("V", TrackKind::Video);
        t.clips = clips;
        t
    };
    let mut levels = vec![scene("level 0", vec![track(vec![half(shot(red, 0.0, 6.0), true)]), track(vec![half(shot(green, 0.0, 6.0), false)])])];
    for level in 1..depth {
        // Placed 0.1 s in, from 0.1 s into the scene below.
        let mut placed = Clip::precomposition(levels[level - 1].id, tc(0.1), tc(5.5));
        placed.source_in = tc(0.1);
        placed.source_out = tc(5.5);
        levels.push(scene(&format!("level {level}"), vec![track(vec![placed])]));
    }
    let tracks: Vec<Track> = levels
        .iter()
        .rev()
        .map(|s| {
            let mut c = Clip::precomposition(s.id, tc(0.0), tc(5.0));
            c.source_out = tc(5.0);
            track(vec![c])
        })
        .collect();
    let top = scene("top", tracks);
    let mut all = levels;
    all.push(top.clone());
    (top, all)
}

/// Frame `n` of the timeline as the real export writes it, as RGB24.
fn exported(top: &Sequence, all: &[Sequence], pool: &MediaPool, n: u64, out: &std::path::Path) -> Vec<u8> {
    let settings = ExportSettings {
        pan_law: Default::default(),
        output_path: out.to_path_buf(), format: ExportFormat::Mp4, video_codec: ExportVideoCodec::H264,
        resolution: None, fps: None, crf: 1, preset: EncoderPreset::Fast, audio_codec: ExportAudioCodec::Copy,
        audio_bitrate_kbps: None, video_bitrate_kbps: None, rate_control: RateControl::Crf,
        burn_in_subtitles: false, embed_subtitles: false, target_lufs: None, true_peak_dbtp: None, embed_chapters: false, captions_608: false, hdr10plus: false,
        bit_depth: 0, hdr_mode: HdrMode::Sdr, tone_map_op: ToneMapOp::Hable, odt: OdtChoice::MatchViewer,
        alpha: false,
        mastering_peak_nits: klipp_core::DEFAULT_MASTERING_PEAK_NITS,
    };
    let mut snapshot = SequenceSnapshot::from_sequence_with_all(top, all, pool, &settings, None, false, WorkingColorspace::Rec709Linear).unwrap();
    snapshot.export_range = Some((n, n + 1));
    ExportJob {
        id: uuid::Uuid::new_v4(), output_path: out.to_path_buf(), settings, snapshot,
        cancel: Arc::new(AtomicBool::new(false)), progress: Arc::new(AtomicU64::new(0)),
        hw_info: Default::default(), render_stage: Default::default(), loudness_result: Default::default(), timings: Default::default(),
        analyser: None,
        total_frames: 1,
    }
    .run()
    .unwrap();
    let vf = "scale=in_color_matrix=bt709:in_range=tv:flags=accurate_rnd+full_chroma_int,format=rgb24";
    let o = std::process::Command::new("ffmpeg").args(["-nostdin", "-v", "error", "-i"]).arg(out).args(["-vf", vf, "-frames:v", "1", "-f", "rawvideo", "-"]).output().unwrap();
    assert_eq!(o.stdout.len(), (W * H * 3) as usize, "the exported frame");
    o.stdout
}

#[test]
fn scenes_deep_park_complete_in_seconds_as_export_draws_them() {
    let fps = Fps::new(FPS, 1);
    let tc = |s: f64| common::frame_tc((s * f64::from(FPS)).round() as i64, fps);
    let mut pool = MediaPool::default();
    let mut media = |colour: &str| -> Option<MediaId> {
        let path = solid(colour)?;
        let id = MediaId::new();
        pool.add(MediaItem {
            id,
            path: path.clone(),
            kind: klipp_media::MediaItemKind::File,
            resolution: Resolution { width: W, height: H },
            ..MediaItem::new_synthetic_title(id, path.display().to_string(), tc(7.0), fps)
        });
        Some(id)
    };
    let (Some(red), Some(green)) = (media("red"), media("green")) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    // Frame 62, 2.48 s: inside every level.
    let n = 62;
    let target = common::frame_tc(n, fps);
    for depth in 1..=4 {
        let (top, all) = nested(depth, red, green, fps);
        let flat = klipp_timeline::flatten::flatten_sequence(&top, &all);
        let (cmd, cmd_rx) = crossbeam_channel::unbounded();
        let (ev_tx, _ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
        let fb = Arc::new(Mutex::new(FrameBuffer::new(W, H)));
        let engine = PlaybackEngine::new(
            cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(W, H))),
            Arc::new(klipp_timeline::SharedSequence::new(flat)), Arc::new(RwLock::new(pool.clone())), Box::new(|| {}),
            Arc::new(Mutex::new(None)), Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default())),
        );
        let thread = std::thread::spawn(move || engine.run());
        // As frame_at asks: paused, then the time, then the frame at it
        // marked complete.
        cmd.send(AppCommand::Pause).unwrap();
        cmd.send(AppCommand::SeekTo(target)).unwrap();
        let t0 = Instant::now();
        let mut state = (None, false);
        while t0.elapsed() < Duration::from_secs(10) {
            state = {
                let fb = fb.lock().unwrap();
                (fb.latest_presented(), fb.latest_complete())
            };
            if state.0 == Some(target) && state.1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let took = t0.elapsed();
        let (viewer, vw, vh) = {
            let fb = fb.lock().unwrap();
            let (data, w, h, _) = fb.latest_frame();
            (data.to_vec(), w, h)
        };
        let trail = fb.lock().unwrap().trail();
        let _ = cmd.send(AppCommand::Quit);
        let _ = thread.join();
        assert!(state.0 == Some(target) && state.1, "{depth} levels: no complete frame at frame {n} in {took:?}: presented {:?}, complete {}\n{}", state.0, state.1, trail.join("\n"));
        assert_eq!((vw, vh), (W, H));
        let out = std::env::temp_dir().join(format!("klipp-nested-{depth}-{}.mp4", std::process::id()));
        let export = exported(&top, &all, &pool, n as u64, &out);
        let _ = std::fs::remove_file(&out);
        for (x, what) in [(W / 4, "red, left"), (3 * W / 4, "green, right")] {
            let v = &viewer[((H / 2 * W + x) * 4) as usize..][..3];
            let e = &export[((H / 2 * W + x) * 3) as usize..][..3];
            let off = v.iter().zip(e).map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs()).max().unwrap();
            assert!(off <= 6, "{depth} levels, {what}: the viewer shows {v:?}, export {e:?}");
        }
        eprintln!("{depth} levels: complete in {took:?}, as export draws it");
    }
    klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));
}
