//! #3212: heavy frames are drawn ahead once the person has been idle for
//! the delay, any input stops the drawing within a frame with every kept
//! frame whole, it starts again after the delay, and it waits while
//! playback runs. What is drawn is then shown from the cache as it plays.

use std::time::{Duration, Instant};

use klipp_core::AppCommand as C;
use klipp_engine::background_render::{self as bg, State};
use klipp_engine::frame_cache::NAMESPACE;

mod common;

const W: u32 = 320;
const H: u32 = 180;
const SECS: u32 = 4;

fn source() -> Option<std::path::PathBuf> {
    let out = std::env::temp_dir().join(format!("klipp-idle-{}.mkv", std::process::id()));
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", &format!("testsrc2=s={W}x{H}:r=24:d={SECS}"), "-c:v", "ffv1", "-pix_fmt", "yuv444p"])
        .arg(&out)
        .status()
        .ok()?
        .success();
    ok.then_some(out)
}

/// Wait until `ok`, up to `limit`; whether it came.
fn wait_for(limit: Duration, mut ok: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while !ok() {
        if start.elapsed() > limit {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    true
}

#[test]
fn heavy_frames_are_drawn_while_idle_and_input_stops_them_within_a_frame() {
    let Some(src) = source() else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let p = common::Parked::start_long(src, W, H, SECS);
    let root = std::env::temp_dir().join(format!("klipp-idle-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    p.send(C::SetFreezeFolder(Some(root.clone())));
    let store = klipp_engine::cache_store::shared_at(&root, 1 << 30);
    let stored = || store.lock().unwrap().frames(NAMESPACE).len();
    let total = (SECS as i64 * i64::from(p.fps.num) / i64::from(p.fps.den)) as usize;
    let delay = Duration::from_millis(500);
    bg::force_heavy(true);

    // Idle past the delay: drawn, every frame of the heavy track.
    let since = Instant::now();
    p.send(C::SetBackgroundRender { on: true, delay_secs: delay.as_secs_f32() });
    assert!(wait_for(Duration::from_secs(10), || stored() > 0), "drawing started once idle: {:?}", bg::state());
    let started = since.elapsed();
    assert!(wait_for(Duration::from_secs(60), || bg::state() == State::Done), "every heavy frame drawn: {:?}, {} of {total}", bg::state(), stored());
    let drawn = since.elapsed();
    eprintln!("MEASURE first frame {:.0} ms after turning on (idle already), all {} in {:.2} s", started.as_secs_f64() * 1000.0, stored(), drawn.as_secs_f64());
    assert_eq!(stored(), total, "one frame each, at the size playback draws");

    // Played: shown from the cache.
    let shown_before = klipp_engine::frames_shown_kept();
    p.send(C::SeekTo(common::frame_tc(0, p.fps)));
    p.send(C::Play);
    std::thread::sleep(Duration::from_secs(2));
    p.send(C::Pause);
    let shown = klipp_engine::frames_shown_kept() - shown_before;
    eprintln!("MEASURE played 2 s after drawing ahead: {shown} frames from the cache");
    assert!(shown >= 24, "playback shows what was drawn ahead: {shown}");

    // Input stops it within a frame: no more than the frame in hand lands.
    p.send(C::ClearRenderCacheAll);
    assert!(wait_for(Duration::from_secs(10), || stored() < 3), "cleared");
    assert!(wait_for(Duration::from_secs(30), || stored() >= 3), "drawing again after the clear");
    p.foreground.touch_input();
    let at_input = stored();
    let touched = Instant::now();
    assert!(wait_for(Duration::from_secs(5), || bg::state() == State::Stopped), "stopped: {:?}", bg::state());
    let stopped_in = touched.elapsed();
    std::thread::sleep(Duration::from_millis(200));
    let after = stored();
    eprintln!("MEASURE input: {} frame(s) landed after it, the state read Stopped in {:.0} ms", after - at_input, stopped_in.as_secs_f64() * 1000.0);
    assert!(after <= at_input + 1, "only the frame in hand lands after input: {at_input} then {after}");
    assert!(after < total, "the input came before the drawing was done");
    for at in store.lock().unwrap().frames(NAMESPACE) {
        assert!(klipp_engine::cache_store::read_stored(&root, NAMESPACE, at).is_some(), "every kept frame whole: {at:?}");
    }

    // Idle again past the delay: starts again and finishes.
    assert!(wait_for(Duration::from_secs(60), || bg::state() == State::Done), "started again after the delay: {:?}", bg::state());
    let resumed = touched.elapsed();
    eprintln!("MEASURE done again {:.2} s after the input", resumed.as_secs_f64());
    assert!(resumed >= delay, "not before the delay");
    assert_eq!(stored(), total);

    // Full: it stops, keeping what it drew, and lets go of nothing.
    let per_frame = store.lock().unwrap().bytes(NAMESPACE) / total as u64;
    p.send(C::ClearRenderCacheAll);
    assert!(wait_for(Duration::from_secs(10), || stored() == 0), "cleared");
    store.lock().unwrap().set_cap(per_frame * 10);
    assert!(wait_for(Duration::from_secs(30), || bg::state() == State::Full), "full: {:?}, {} kept", bg::state(), stored());
    let kept: Vec<_> = store.lock().unwrap().frames(NAMESPACE);
    std::thread::sleep(delay * 3);
    assert_eq!(store.lock().unwrap().frames(NAMESPACE), kept, "nothing drawn over, nothing let go");
    eprintln!("MEASURE full at {} of {total} frames", kept.len());
    assert!((5..=10).contains(&kept.len()), "about ten frames fit: {}", kept.len());
    store.lock().unwrap().set_cap(1 << 30);

    // Playing: nothing is drawn.
    p.send(C::ClearRenderCacheAll);
    p.send(C::SeekTo(common::frame_tc(0, p.fps)));
    p.send(C::Play);
    std::thread::sleep(delay * 3);
    let while_playing = stored();
    p.send(C::Pause);
    assert_eq!(while_playing, 0, "nothing drawn while playing: {:?}", bg::state());

    // Turned off: nothing is drawn, however idle.
    p.send(C::SetBackgroundRender { on: false, delay_secs: delay.as_secs_f32() });
    p.send(C::ClearRenderCacheAll);
    std::thread::sleep(delay * 3);
    assert_eq!((stored(), bg::state()), (0, State::Off), "off");
    bg::force_heavy(false);
    let _ = std::fs::remove_dir_all(&root);
}

/// Measured, not a check: a 4K FFV1 range, heavy to decode, played live
/// and then after it was drawn ahead. Frames shown in two seconds and
/// their time. Run alone: `--ignored measure_heavy_range`.
#[test]
#[ignore]
fn measure_heavy_range_live_and_drawn_ahead() {
    let (w, h, secs) = (3840u32, 2160u32, 3u32);
    let src = std::env::temp_dir().join(format!("klipp-idle-4k-{}.mkv", std::process::id()));
    let made = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", &format!("testsrc2=s={w}x{h}:r=24:d={secs}"), "-c:v", "ffv1", "-pix_fmt", "yuv444p"])
        .arg(&src)
        .status()
        .is_ok_and(|s| s.success());
    if !made {
        eprintln!("skipping: no ffmpeg");
        return;
    }
    let p = common::Parked::start_long(src.clone(), w, h, secs);
    let root = std::env::temp_dir().join(format!("klipp-idle-4k-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    p.send(C::SetFreezeFolder(Some(root.clone())));
    let play = |label: &str| {
        let kept = klipp_engine::frames_shown_kept();
        p.send(C::SeekTo(common::frame_tc(0, p.fps)));
        std::thread::sleep(Duration::from_millis(500));
        let start = Instant::now();
        p.send(C::Play);
        std::thread::sleep(Duration::from_secs(2));
        p.send(C::Pause);
        let samples: Vec<_> = p.perf().into_iter().filter(|s| s.timestamp > start).collect();
        let n = samples.len().max(1) as f32;
        let mean = samples.iter().map(|s| s.total_ms).sum::<f32>() / n;
        let render = samples.iter().map(|s| s.phases.render_ms + s.phases.sync_ms).sum::<f32>() / n;
        eprintln!(
            "MEASURE {label}: {} frames shown in 2 s ({} late), {} from the cache, frame {mean:.1} ms, render and wait {render:.1} ms",
            samples.len(),
            samples.iter().filter(|s| s.dropped).count(),
            klipp_engine::frames_shown_kept() - kept,
        );
    };
    play("live");
    bg::force_heavy(true);
    let since = Instant::now();
    p.send(C::SetBackgroundRender { on: true, delay_secs: 0.5 });
    assert!(wait_for(Duration::from_secs(300), || bg::state() == State::Done), "drawn: {:?}", bg::state());
    eprintln!("MEASURE drawn ahead in {:.1} s", since.elapsed().as_secs_f64());
    play("after drawing ahead");
    let load = klipp_engine::frame_cache::load_costs();
    eprintln!(
        "MEASURE loading a kept frame, on the reading thread over {} frames: read {:.1} ms, LZ4 {:.1} ms, unpack {:.1} ms, to display bytes {:.1} ms, total {:.1} ms",
        load.frames, load.read_ms, load.lz4_ms, load.unpack_ms, load.encode_ms, load.total_ms,
    );
    bg::force_heavy(false);
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_file(&src);
}
