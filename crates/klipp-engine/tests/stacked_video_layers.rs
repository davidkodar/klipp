//! Stacked 4K video layers while playing (#2101): what each added layer
//! costs a viewer frame. Run with `--ignored --nocapture`; needs ffmpeg.

mod common;

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, GradeAccess, Sequence, Track, TrackKind};

const W: u32 = 3840;
const H: u32 = 2160;
const FPS: u32 = 30;

/// Six seconds of a moving test pattern in H.264, `w`×`h`. `None` without
/// ffmpeg.
fn video(k: usize, w: u32, h: u32) -> Option<std::path::PathBuf> {
    let out = std::env::temp_dir().join(format!("klipp-stacked-{w}x{h}-{k}.mp4"));
    if out.exists() {
        return Some(out);
    }
    let partial = common::unshared(&out, "mp4");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", &format!("testsrc2=s={w}x{h}:r={FPS}:d=6,hue=h={}", k * 90)])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-g", "30"])
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

/// What each layer is: its source size, a grade, effects.
#[derive(Clone, Copy, Debug)]
struct Kind {
    source: (u32, u32),
    graded: bool,
    effects: bool,
    /// An animated 3D move: scale, position, opacity and a tilt.
    tilted: bool,
}

/// Play `seq` from `from` for `secs`; the medians of the viewer frames'
/// render and whole frame, and how many.
fn play_sequence(seq: Sequence, pool: MediaPool, from: f64, secs: f64) -> Option<(f32, f32, usize)> {
    let fps = seq.fps;
    let (w, h) = (seq.resolution.width, seq.resolution.height);
    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
    // Every playhead the engine reports, and when: what a viewer sees move.
    let moves: Arc<Mutex<Vec<(Instant, i64)>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&moves);
    std::thread::spawn(move || {
        while let Ok(ev) = ev_rx.recv() {
            if let AppEvent::PlayheadMoved(tc) = ev {
                seen.lock().unwrap().push((Instant::now(), tc.value));
            }
        }
    });
    let fb = Arc::new(Mutex::new(FrameBuffer::new(w, h)));
    let ring = Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default()));
    let mut engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(w, h))),
        Arc::new(klipp_timeline::SharedSequence::new(seq)), Arc::new(RwLock::new(pool)), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::clone(&ring),
    );
    // As the app starts it (#2108), the shared device decided as the UI
    // decides it by installing its own.
    let _ = klipp_effects::gpu_context::GpuContext::shared();
    engine.prewarm_gpu();
    let thread = engine.spawn();
    // `KLIPP_WARM`: play it once first, so what is measured is a second
    // Play in a process that has drawn the section before.
    if std::env::var_os("KLIPP_WARM").is_some() {
        cmd.send(AppCommand::SetPlayhead(RationalTime::from_secs_f64(from, fps.rate()))).unwrap();
        common::settle(&fb);
        cmd.send(AppCommand::Play).unwrap();
        std::thread::sleep(Duration::from_secs_f64(secs));
        cmd.send(AppCommand::Pause).unwrap();
        std::thread::sleep(Duration::from_millis(300));
    }
    cmd.send(AppCommand::SetPlayhead(RationalTime::from_secs_f64(from, fps.rate()))).unwrap();
    common::settle(&fb);
    // `KLIPP_PARKED`: seconds to stay parked before Play, as a person
    // looks at the frame before pressing it.
    if let Some(p) = std::env::var("KLIPP_PARKED").ok().and_then(|v| v.parse::<f64>().ok()) {
        std::thread::sleep(Duration::from_secs_f64(p));
    }
    cmd.send(AppCommand::Play).unwrap();
    let since = Instant::now();
    // Every frame presented, and when: frames a second and the longest
    // wait between two.
    let mut shown: Vec<Instant> = Vec::new();
    let mut g = common::generation(&fb);
    while since.elapsed() < Duration::from_secs_f64(secs) {
        let now = common::generation(&fb);
        if now != g {
            shown.push(Instant::now());
            g = now;
        }
        std::thread::sleep(Duration::from_micros(500));
    }
    let _ = cmd.send(AppCommand::Pause);
    {
        // The playhead's distinct frames each second, and its first move.
        let moves = moves.lock().unwrap();
        let start = moves.iter().rev().find(|(t, _)| *t <= since).map(|m| m.1);
        let mut distinct: Vec<(Instant, i64)> = Vec::new();
        for &(t, v) in moves.iter().filter(|(t, _)| *t > since) {
            if distinct.last().map_or(start != Some(v), |l| l.1 != v) {
                distinct.push((t, v));
            }
        }
        let first = distinct.first().map_or(secs, |m| m.0.duration_since(since).as_secs_f64());
        let per: Vec<usize> = (0..secs.ceil() as usize).map(|k| distinct.iter().filter(|(t, _)| t.duration_since(since).as_secs_f64() as usize == k).count()).collect();
        let span = match (distinct.first(), distinct.last()) {
            (Some(a), Some(b)) => format!("{:.3} → {:.3} s on the timeline in {:.2} s", a.1 as f64 / f64::from(fps.rate()), b.1 as f64 / f64::from(fps.rate()), b.0.duration_since(a.0).as_secs_f64()),
            _ => "none".into(),
        };
        eprintln!("PLAYHEAD first move after {first:.2} s, distinct frames per second {per:?}, {span}");
        if std::env::var_os("KLIPP_TRACE").is_some() {
            let early: Vec<String> = moves.iter().filter(|(t, _)| t.duration_since(since).as_secs_f64() < 1.0 && *t > since).map(|(t, v)| format!("+{:.3}:{:.3}", t.duration_since(since).as_secs_f64(), *v as f64 / f64::from(fps.rate()))).collect();
            eprintln!("PLAYHEAD early moves (start {:?}): {}", start.map(|v| v as f64 / f64::from(fps.rate())), early.join(" "));
            let early_shown: Vec<String> = shown.iter().filter(|t| t.duration_since(since).as_secs_f64() < 1.0).map(|t| format!("+{:.3}", t.duration_since(since).as_secs_f64())).collect();
            eprintln!("SHOWN early: {}", early_shown.join(" "));
        }
    }
    let first_gap = shown.first().map_or(secs, |t| t.duration_since(since).as_secs_f64());
    let worst_gap = shown.windows(2).map(|w| w[1].duration_since(w[0]).as_secs_f64()).fold(first_gap, f64::max);
    let per_sec: Vec<usize> = (0..secs.ceil() as usize).map(|k| shown.iter().filter(|t| t.duration_since(since).as_secs_f64() as usize == k).count()).collect();
    eprintln!("SHOWN {} frames in {secs} s, per second {per_sec:?}, first after {:.2} s, longest wait {:.2} s", shown.len(), first_gap, worst_gap);
    let samples: Vec<(f32, f32)> = ring.lock().unwrap().iter_chronological().filter(|s| s.timestamp >= since && s.phases.render_ms > 0.0).map(|s| (s.phases.render_ms, s.total_ms)).collect();
    {
        let ring = ring.lock().unwrap();
        let all: Vec<_> = ring.iter_chronological().filter(|s| s.timestamp >= since).collect();
        let med = |mut v: Vec<f32>| { if v.is_empty() { return 0.0 } v.sort_by(f32::total_cmp); v[v.len() / 2] };
        eprintln!("DETAIL samples {} (with render {}), median sync {:.1} decode {:.1} present {:.1} update {:.1}, dropped {}",
            all.len(), samples.len(),
            med(all.iter().map(|s| s.phases.sync_ms).collect()), med(all.iter().map(|s| s.decode_ms).collect()),
            med(all.iter().map(|s| s.phases.present_ms).collect()), med(all.iter().map(|s| s.update_playback_ms).collect()),
            all.iter().filter(|s| s.dropped).count());
        // Decoders moved off a full GPU decoder (#2116).
        eprintln!("DECODERS most on the GPU {}, most in software {}",
            all.iter().map(|s| s.streams_gpu).max().unwrap_or(0), all.iter().map(|s| s.streams_software).max().unwrap_or(0));
        if std::env::var_os("KLIPP_TRACE").is_some() {
            let mut prev = since;
            for x in &all {
                let gap = x.timestamp.duration_since(prev).as_secs_f64() * 1000.0;
                if gap > 60.0 {
                    eprintln!("GAP {:.0} ms before a sample at +{:.2} s: sync {:.1} render {:.1} present {:.1} decode {:.1} update {:.1}", gap, x.timestamp.duration_since(since).as_secs_f64(), x.phases.sync_ms, x.phases.render_ms, x.phases.present_ms, x.decode_ms, x.update_playback_ms);
                }
                prev = x.timestamp;
            }
        }
    }
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();
    klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));
    if samples.is_empty() {
        return None;
    }
    let median = |mut v: Vec<f32>| { v.sort_by(f32::total_cmp); v[v.len() / 2] };
    Some((median(samples.iter().map(|s| s.0).collect()), median(samples.iter().map(|s| s.1).collect()), samples.len()))
}

/// A saved project's 4K sequence played from `KLIPP_FROM` for
/// `KLIPP_SECS`: `KLIPP_PROJECT=<dir.klipp> cargo test ... -- --ignored`.
#[test]
#[ignore = "playback measurement of a saved project"]
fn measure_project_window() {
    let Ok(dir) = std::env::var("KLIPP_PROJECT") else { return };
    if std::env::var_os("RUST_LOG").is_some() {
        let _ = tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).with_writer(std::io::stderr).try_init();
    }
    let num = |k: &str, d: f64| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
    let (seqs, pool, ..) = klipp_project::load_project(std::path::Path::new(&dir)).expect("loads");
    let seq = seqs.into_iter().max_by_key(|s| s.resolution.width).expect("a sequence");
    for item in pool.all() {
        if !item.path.exists() {
            eprintln!("MISSING {}", item.path.display());
        }
    }
    let (from, secs) = (num("KLIPP_FROM", 48.0), num("KLIPP_SECS", 3.0));
    // `KLIPP_HW_DECODE=0`: every stream decodes in software, as with the
    // preference off.
    if std::env::var("KLIPP_HW_DECODE").as_deref() == Ok("0") {
        klipp_engine::video_decoder::set_hardware_decode(false);
    }
    match play_sequence(seq, pool, from, secs) {
        Some((render, total, frames)) => eprintln!("MEASURE project {from}+{secs}s: render {render:.1} ms, frame {total:.1} ms, median of {frames}"),
        None => eprintln!("MEASURE project: no frames"),
    }
}

/// The median viewer frame's render, and its whole frame, playing `n`
/// stacked layers of `kind` for three seconds.
fn play(n: usize, kind: Kind) -> Option<(f32, f32, usize)> {
    let fps = Fps::new(FPS, 1);
    let tc = |f| common::frame_tc(f, fps);
    let mut pool = MediaPool::default();
    let mut tracks = Vec::new();
    for k in 0..n {
        let path = video(k, kind.source.0, kind.source.1)?;
        let id = MediaId::new();
        pool.add(MediaItem {
            id,
            path: path.clone(),
            kind: klipp_media::MediaItemKind::File,
            resolution: Resolution { width: kind.source.0, height: kind.source.1 },
            is_still: false,
            has_audio: false,
            ..MediaItem::new_synthetic_title(id, path.display().to_string(), tc(180), fps)
        });
        // Each a little smaller, so every layer shows.
        let mut b = klipp_timeline::test_clip::TestClip::of(Clip { kind: ClipKind::Media { media_id: id }, source_in: tc(0), source_out: tc(180), ..Clip::gap(tc(0), tc(180)) });
        b = if kind.tilted {
            b.lane("scale", 0.0, 0.25).lane("scale", 6.0, 0.5).lane("pos_x", 0.0, -1152.0 + 600.0 * k as f32).lane("pos_x", 6.0, 1152.0)
                .lane("opacity", 0.0, 0.2).lane("opacity", 6.0, 1.0).lane("tilt_y", 0.0, 15.0).lane("tilt_y", 6.0, 0.0)
        } else {
            b.lane("scale", 0.0, 1.0 - 0.2 * k as f32)
        };
        if kind.effects {
            b = b.effect(klipp_effects::BuiltinEffect::RoundedCorners { radius: 0.05, feather: 0.002 })
                .effect(klipp_effects::BuiltinEffect::OuterGlow { radius: 0.02, intensity: 0.3, color: [140, 150, 255] });
        }
        let mut clip = b.build();
        if kind.graded {
            let mut g = klipp_nodes::NodeGraph::default();
            g.ensure_corrector();
            if let Some(n) = g.nodes.iter_mut().find(|n| n.kind == klipp_nodes::NodeKind::Corrector) {
                n.params.exposure = Some(0.3);
            }
            clip.set_grade(g);
        }
        let mut t = Track::new(&format!("V{}", n - k), TrackKind::Video);
        t.clips = vec![clip];
        tracks.push(t);
    }
    tracks.reverse();
    let mut seq = Sequence::new_default();
    seq.fps = fps;
    seq.resolution = Resolution { width: W, height: H };
    seq.tracks = tracks;
    play_sequence(seq, pool, 0.5, 3.0)
}

#[test]
#[ignore = "4K playback measurement"]
fn measure_stacked_4k_video_layers() {
    let only = std::env::var("KLIPP_KIND").ok();
    let kinds = [
        ("plain", Kind { source: (W, H), graded: false, effects: false, tilted: false }),
        ("effects", Kind { source: (W, H), graded: false, effects: true, tilted: false }),
        ("tilted", Kind { source: (W, H), graded: false, effects: false, tilted: true }),
        ("effects+tilted", Kind { source: (W, H), graded: false, effects: true, tilted: true }),
    ];
    let kinds: Vec<_> = kinds.into_iter().filter(|(n, _)| only.as_deref().is_none_or(|o| o == *n)).map(|(_, k)| k).collect();
    for kind in kinds {
        for n in 1..=3 {
            match play(n, kind) {
                Some((render, total, frames)) => eprintln!("MEASURE {kind:?} {n} layer(s): render {render:.1} ms, frame {total:.1} ms, median of {frames}"),
                None => eprintln!("MEASURE {kind:?} {n}: no frames (no ffmpeg?)"),
            }
        }
    }
}

/// How long opening a decoder takes on the engine's thread, for the files
/// in `KLIPP_FILES` (`:`-separated).
#[test]
#[ignore = "decoder open timing"]
fn measure_decoder_open() {
    let Ok(files) = std::env::var("KLIPP_FILES") else { return };
    for f in files.split(':') {
        let t = Instant::now();
        let dec = klipp_engine::video_decoder::VideoDecoder::open(std::path::Path::new(f), 0);
        let opened = t.elapsed();
        let ok = dec.is_ok();
        drop(dec);
        eprintln!("OPEN {:.0} ms ok={ok} {f}", opened.as_secs_f64() * 1000.0);
    }
    klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));
}
