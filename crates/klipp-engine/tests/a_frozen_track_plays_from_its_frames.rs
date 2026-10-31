//! A frozen track plays from its stored frames (#3207): its media isn't
//! decoded, its effects aren't run, and the viewer's frame costs less,
//! while the other track plays live. Measured on a heavy UHD track (a
//! graded clip with a wide blur and a glow) over a plain one, played live
//! and then frozen at the size the viewer draws while playing.

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use klipp_core::*;
use klipp_engine::{FrameBuffer, PlaybackEngine};
use klipp_media::{MediaItem, MediaPool};
use klipp_timeline::{Clip, ClipKind, GradeAccess, Sequence, Track, TrackKind};

const W: u32 = 3840;
const H: u32 = 2160;
const FPS: u32 = 30;
const SECS: i64 = 4;

/// Four seconds of a moving test pattern in H.264 at UHD.
fn video(name: &str, hue: u32) -> Option<std::path::PathBuf> {
    let out = std::env::temp_dir().join(format!("klipp-frozen-{name}-{W}x{H}.mp4"));
    if out.exists() {
        return Some(out);
    }
    let partial = common::unshared(&out, "mp4");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", &format!("testsrc2=s={W}x{H}:r={FPS}:d={SECS},hue=h={hue}")])
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

/// The render copy of a heavy track (V2) over a plain one (V1), and the pool.
fn project() -> Option<(Sequence, MediaPool)> {
    let fps = Fps::new(FPS, 1);
    let tc = |f| common::frame_tc(f, fps);
    let frames = SECS * FPS as i64;
    let mut pool = MediaPool::default();
    let mut clip_of = |name: &str, hue: u32| -> Option<Clip> {
        let path = video(name, hue)?;
        let id = MediaId::new();
        pool.add(MediaItem {
            id,
            path: path.clone(),
            kind: klipp_media::MediaItemKind::File,
            resolution: Resolution { width: W, height: H },
            is_still: false,
            has_audio: false,
            ..MediaItem::new_synthetic_title(id, path.display().to_string(), tc(frames), fps)
        });
        Some(Clip { kind: ClipKind::Media { media_id: id }, source_in: tc(0), source_out: tc(frames), ..Clip::gap(tc(0), tc(frames)) })
    };
    let heavy = clip_of("heavy", 200)?;
    let plain = clip_of("plain", 0)?;
    let mut heavy = klipp_timeline::test_clip::TestClip::of(heavy)
        .effect(klipp_effects::BuiltinEffect::GaussianBlur { radius: 40.0 })
        .effect(klipp_effects::BuiltinEffect::OuterGlow { radius: 0.05, intensity: 0.6, color: [140, 150, 255] })
        .effect(klipp_effects::BuiltinEffect::RoundedCorners { radius: 0.06, feather: 0.004 })
        .lane("scale", 0.0, 0.7)
        .build();
    let mut g = klipp_nodes::NodeGraph::default();
    g.ensure_corrector();
    if let Some(n) = g.nodes.iter_mut().find(|n| n.kind == klipp_nodes::NodeKind::Corrector) {
        n.params.exposure = Some(0.4);
        n.params.saturation = Some(1.3);
    }
    heavy.set_grade(g);
    let mut v2 = Track::new("V2", TrackKind::Video);
    v2.clips = vec![heavy];
    let mut v1 = Track::new("V1", TrackKind::Video);
    v1.clips = vec![plain];
    let mut seq = Sequence::new_default();
    seq.fps = fps;
    seq.resolution = Resolution { width: W, height: H };
    seq.tracks = vec![v2, v1];
    let flat = klipp_timeline::flatten::flatten_sequence(&seq, std::slice::from_ref(&seq));
    Some((flat, pool))
}

/// What Klipp holds on the GPU, MB, on any adapter: its caches' total,
/// and wgpu's buffer and texture totals where the build counts them
/// (`gpu-memory-report`).
#[derive(Clone, Copy)]
struct GpuMemory {
    held_mb: f64,
    driver_mb: Option<f64>,
}

impl std::fmt::Debug for GpuMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:.0} MB held", self.held_mb)?;
        match self.driver_mb {
            Some(d) => write!(f, ", {d:.0} MB in wgpu's buffers and textures"),
            None => Ok(()),
        }
    }
}

fn gpu_memory_now() -> GpuMemory {
    let s = klipp_effects::gpu_memory::snapshot();
    let mb = |b: u64| b as f64 / 1e6;
    GpuMemory { held_mb: mb(s.held_total), driver_mb: s.driver.map(|d| mb(d.buffer_bytes + d.texture_bytes)) }
}

/// What a play showed.
#[derive(Debug)]
struct Played {
    /// The viewer frame's render (the work handed to the GPU) and its
    /// whole frame, medians, ms.
    render: f32,
    total: f32,
    /// Frames drawn while playing.
    frames: usize,
    /// Decoders open, GPU and software, median over the play: what stays
    /// open, not the first moments before a frozen frame is laid.
    decoders: u32,
    /// The viewer's frame size while playing.
    size: (u32, u32),
    memory: Option<GpuMemory>,
}

/// Play `seq` from half a second for `secs`, `frozen` the freeze folder and
/// the frozen track when freezing.
fn play(seq: &Sequence, pool: &MediaPool, frozen: Option<(&std::path::Path, TrackId)>, secs: f64) -> Played {
    let fps = seq.fps;
    let (cmd, cmd_rx) = crossbeam_channel::unbounded();
    let (ev_tx, _ev_rx) = crossbeam_channel::unbounded::<AppEvent>();
    let fb = Arc::new(Mutex::new(FrameBuffer::new(W, H)));
    let ring = Arc::new(Mutex::new(klipp_engine::perf::PerfRing::default()));
    let mut engine = PlaybackEngine::new(
        cmd_rx, ev_tx, Arc::clone(&fb), Arc::new(Mutex::new(FrameBuffer::new(W, H))),
        Arc::new(klipp_timeline::SharedSequence::new(seq.clone())), Arc::new(RwLock::new(pool.clone())), Box::new(|| {}),
        Arc::new(Mutex::new(None)), Arc::clone(&ring),
    );
    let _ = klipp_effects::gpu_context::GpuContext::shared();
    engine.prewarm_gpu();
    let thread = std::thread::spawn(move || engine.run());
    if let Some((folder, track)) = frozen {
        cmd.send(AppCommand::SetFreezeFolder(Some(folder.to_path_buf()))).unwrap();
        cmd.send(AppCommand::SetFrozenTracks(vec![track])).unwrap();
    }
    cmd.send(AppCommand::SetPlayhead(RationalTime::from_secs_f64(0.5, fps.rate()))).unwrap();
    common::settle(&fb);
    cmd.send(AppCommand::Play).unwrap();
    let since = Instant::now();
    let mut memory = None;
    while since.elapsed() < Duration::from_secs_f64(secs) {
        std::thread::sleep(Duration::from_millis(100));
        if since.elapsed() > Duration::from_secs_f64(secs * 0.6) && memory.is_none() {
            memory = Some(gpu_memory_now());
        }
    }
    let size = {
        let fb = fb.lock().unwrap();
        let (_, w, h, _) = fb.latest_frame();
        (w, h)
    };
    let _ = cmd.send(AppCommand::Pause);
    let samples: Vec<_> = ring.lock().unwrap().iter_chronological().filter(|s| s.timestamp >= since).cloned().collect();
    let _ = cmd.send(AppCommand::Quit);
    let _ = thread.join();
    klipp_engine::video_decoder::wait_for_decode_threads(Duration::from_secs(10));
    let drawn: Vec<_> = samples.iter().filter(|s| s.phases.render_ms > 0.0).collect();
    let median = |mut v: Vec<f32>| {
        v.sort_by(f32::total_cmp);
        v.get(v.len() / 2).copied().unwrap_or(0.0)
    };
    if std::env::var_os("KLIPP_TRACE").is_some() {
        eprintln!("TRACE decoders over the play: {:?}", samples.iter().map(|s| (s.streams_gpu, s.streams_software)).collect::<Vec<_>>());
    }
    let mut open: Vec<u32> = samples.iter().map(|s| s.streams_gpu + s.streams_software).collect();
    open.sort();
    Played {
        render: median(drawn.iter().map(|s| s.phases.render_ms).collect()),
        total: median(drawn.iter().map(|s| s.total_ms).collect()),
        frames: drawn.len(),
        decoders: open.get(open.len() / 2).copied().unwrap_or(0),
        size,
        memory,
    }
}

/// The frame's cost on the GPU stack at `size`, the GPU waited for, median
/// over a second of frames: drawn live from the files (decoded as export
/// decodes them), or with the frozen track laid from its stored frames.
fn stack_cost(seq: &Sequence, files: &'static HashMap<MediaId, klipp_render::MediaFile>, size: (u32, u32), frozen: Option<(&std::path::Path, TrackId, &klipp_engine::freeze::FreezeRequest)>) -> f64 {
    let working = klipp_engine::video_decoder::working_colorspace();
    let mut r = klipp_compose::Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_gpu_enabled(true);
    r.set_working_colorspace(working);
    let mut src = klipp_render::ExportFrameSource::new(files, seq.fps.as_f64(), working);
    let ctx = klipp_effects::gpu_context::GpuContext::shared();
    let mut costs = Vec::new();
    for f in 0..(FPS as i64 + 6) {
        let at = common::frame_tc(15 + f, seq.fps);
        if let Some((folder, track, req)) = frozen {
            let media = |id: &str| req.media.get(id).copied();
            let env = klipp_compose::freeze::FreezeEnv { sequences: &[], media: &media, plugin: &klipp_engine::freeze::plugin_identity, environment: req.environment };
            let key = klipp_compose::freeze::slot_key(seq, 0, at, &env).expect("a key");
            let packed = klipp_engine::cache_store::read_stored_packed(folder, &klipp_engine::freeze::namespace(track), klipp_engine::cache_store::FrameAt { tick: at.value, key, size }).expect("stored");
            r.set_frozen_layers(HashMap::from([(0, Arc::new(klipp_compose::freeze::FrozenLayer::from_packed(packed)))]));
        }
        let t = Instant::now();
        let _ = r.render_display_on_gpu(seq, at, size.0, size.1, &mut src, klipp_compose::RenderOptions::default(), klipp_compose::DisplayDelivery { texture: None, bytes: false });
        if let Some(ctx) = &ctx {
            ctx.device().poll(wgpu::Maintain::Wait);
        }
        if f >= 6 {
            costs.push(t.elapsed().as_secs_f64() * 1000.0);
        }
    }
    costs.sort_by(f64::total_cmp);
    costs[costs.len() / 2]
}

/// Live, then frozen at the size the viewer drew: the frozen track's frames
/// are stored, its decoder isn't opened while they serve, and the frame
/// costs less to render.
/// The tests here set process-wide switches and time playback: one at
/// a time.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

#[test]
fn a_frozen_track_plays_from_its_frames_and_isnt_decoded() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let Some((seq, pool)) = project() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    // Played once to warm the process (shaders, files), then measured: a
    // cold first play draws half the frames a warm one does.
    let _ = play(&seq, &pool, None, 2.0);
    let live = play(&seq, &pool, None, 3.0);
    eprintln!("MEASURE live: {live:?}");
    // What the heavy track cost to play, as the engine last said it.
    let heavy = seq.tracks[0].id;
    let live_cost = klipp_engine::track_cost::cost(heavy).unwrap_or_default();
    eprintln!("MEASURE the heavy track's cost live: {}", live_cost.describe());
    assert!(live_cost.decoders >= 1 && live_cost.gpu_bytes + live_cost.cpu_bytes > 0, "the heavy track's decoder and its frames counted: {live_cost:?}");
    assert!(live.size.0 > 0 && live.frames > 10, "live played: {live:?}");

    // Frozen at the size the app freezes at: the size the viewer drew.
    let size = klipp_engine::freeze::playback_size((W, H));
    assert_eq!(size, live.size, "the app's freeze size is the size the viewer plays at");
    let folder = std::env::temp_dir().join(format!("klipp-frozen-folder-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    let store = Arc::new(Mutex::new(klipp_engine::cache_store::CacheStore::open(&folder, 64 << 30)));
    let track_id = seq.tracks[0].id;
    store.lock().unwrap().pin(&klipp_engine::freeze::namespace(track_id));
    let files: &'static HashMap<MediaId, klipp_render::MediaFile> = Box::leak(Box::new(pool.all().map(|i| (i.id, klipp_render::MediaFile::of(i))).collect()));
    let working = klipp_engine::video_decoder::working_colorspace();
    let fps = seq.fps.as_f64();
    let frames: Vec<Timecode> = (0..SECS * FPS as i64).map(|f| common::frame_tc(f, seq.fps)).collect();
    let req = klipp_engine::freeze::FreezeRequest {
        seq: Arc::new(seq.clone()),
        sequences: Arc::new(vec![]),
        track: 0,
        track_id,
        frames: frames.clone(),
        size,
        working,
        // As the engine plays: proxies on, its default.
        environment: klipp_engine::freeze::environment(working, true),
        media: Arc::new(klipp_engine::freeze::media_identities(&pool)),
    };
    let started = Instant::now();
    let (tx, rx) = std::sync::mpsc::channel();
    let job = klipp_engine::freeze::spawn_freeze(req, Arc::clone(&store), move || klipp_render::ExportFrameSource::new(files, fps, working), move |t| tx.send(t).unwrap());
    job.join();
    let tally = rx.recv().unwrap();
    let mb = store.lock().unwrap().bytes(&klipp_engine::freeze::namespace(track_id)) as f64 / 1e6;
    eprintln!("MEASURE freeze: {tally:?} in {:.1} s, {mb:.0} MB on disk", started.elapsed().as_secs_f64());
    assert_eq!(tally.stored, frames.len(), "every frame frozen: {tally:?}");

    let counted = klipp_engine::frozen_counts();
    let frozen = play(&seq, &pool, Some((&folder, track_id)), 3.0);
    let (laid, skipped) = klipp_engine::frozen_counts();
    let (laid, skipped) = (laid - counted.0, skipped - counted.1);
    eprintln!("MEASURE frozen layers laid {laid}, clip decodes skipped {skipped}");
    assert!(laid as usize >= frozen.frames / 2, "frozen layers laid while playing: {laid} for {} frames", frozen.frames);
    eprintln!("MEASURE frozen: {frozen:?}");
    let frozen_cost = klipp_engine::track_cost::cost(heavy).unwrap_or_default();
    eprintln!("MEASURE the heavy track's cost frozen: {}", frozen_cost.describe());
    assert!(frozen_cost.decoders < live_cost.decoders, "frozen, its decoder isn't counted: {frozen_cost:?} against {live_cost:?}");
    assert!(frozen.frames > 10, "frozen played: {frozen:?}");
    assert!(frozen.decoders < live.decoders, "the frozen track's media isn't decoded: {} decoders frozen, {} live", frozen.decoders, live.decoders);
    assert!(frozen.frames + 3 >= live.frames, "no fewer frames drawn: {} frozen, {} live", frozen.frames, live.frames);
    eprintln!("MEASURE GPU memory: {:?} live, {:?} frozen", live.memory, frozen.memory);
    eprintln!("MEASURE render thread per frame, median: {:.1} ms live, {:.1} ms frozen; whole frame {:.1} ms live, {:.1} ms frozen", live.render, frozen.render, live.total, frozen.total);
    // The frame on the GPU stack, the GPU waited for.
    let req_again = klipp_engine::freeze::FreezeRequest { seq: Arc::new(seq.clone()), sequences: Arc::new(vec![]), track: 0, track_id, frames: vec![], size: live.size, working, environment: klipp_engine::freeze::environment(working, true), media: Arc::new(klipp_engine::freeze::media_identities(&pool)) };
    let gpu_live = stack_cost(&seq, files, live.size, None);
    let gpu_frozen = stack_cost(&seq, files, live.size, Some((&folder, track_id, &req_again)));
    eprintln!("MEASURE frame on the GPU stack, GPU waited for, median: {gpu_live:.1} ms live (its decode included), {gpu_frozen:.1} ms frozen");
    assert!(gpu_frozen < gpu_live, "the frozen frame costs less on the GPU stack: {gpu_frozen:.1} ms frozen, {gpu_live:.1} ms live");
    let _ = std::fs::remove_dir_all(&folder);
}

/// #3234: on a device without timestamps (timing switched off here),
/// trouble has each visible track drawn alone once, a track a frame, as
/// its cost "measured once"; with timestamps, nothing is drawn alone.
#[test]
fn without_timestamps_trouble_measures_each_track_once() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let Some((seq, pool)) = project() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    klipp_engine::playback_health::force_trouble(true);
    klipp_effects::gpu::timing::set_off(true);
    klipp_engine::playback_health::set_shared(|s| s.measured_once.clear());
    let _ = play(&seq, &pool, None, 2.0);
    let once = klipp_engine::playback_health::shared().measured_once;
    eprintln!("MEASURE measured once, timing off: {:?}", seq.tracks.iter().map(|t| (t.name.clone(), once.get(&t.id).copied())).collect::<Vec<_>>());
    let timed_after_off = klipp_engine::track_cost::gpu_timed();
    klipp_effects::gpu::timing::set_off(false);
    klipp_engine::playback_health::set_shared(|s| s.measured_once.clear());
    let _ = play(&seq, &pool, None, 2.0);
    let once_timed = klipp_engine::playback_health::shared().measured_once;
    klipp_engine::playback_health::force_trouble(false);
    assert!(!timed_after_off, "timing switched off reads as not measured");
    for t in seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video) {
        assert!(once.get(&t.id).is_some_and(|ms| *ms > 0.0), "{} measured once: {once:?}", t.name);
    }
    assert!(once_timed.is_empty(), "with timestamps, nothing drawn alone: {once_timed:?}");
}
