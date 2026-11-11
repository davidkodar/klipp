//! A media-pool thumbnail is one still frame, and its decoder holds no
//! more than that needs. Opening a project of 93 files started every
//! file's thumbnail at once, each on a software decoder with a frame
//! thread a core (16), each thread holding its own frames: 8–9 GB of
//! memory for a few seconds on a 1440p long-GOP project.

use std::time::Duration;

mod common;

fn rss_mb() -> f64 {
    let s = std::fs::read_to_string("/proc/self/statm").unwrap();
    let pages: f64 = s.split_whitespace().nth(1).unwrap().parse().unwrap();
    pages * 4096.0 / 1e6
}

/// 1440p H.264 at 62.5 fps, a keyframe every 8 s: the thumbnail at 10%
/// of a long clip is a long decode from its keyframe.
fn long_gop_1440p() -> Option<std::path::PathBuf> {
    let out = std::env::temp_dir().join("klipp-thumb-light-1440p.mp4");
    if out.exists() {
        return Some(out);
    }
    let partial = common::unshared(&out, "mp4");
    let ok = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=s=2560x1440:r=125/2:d=10"])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-g", "500", "-keyint_min", "500", "-sc_threshold", "0"])
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

/// The decoder threads alive while one thumbnail decodes: ffmpeg names
/// its frame threads `av:…`, and each holds frames of its own.
#[test]
fn a_thumbnail_decodes_on_two_threads() {
    let Some(src) = long_gop_1440p() else {
        eprintln!("skipping: no ffmpeg with libx264");
        return;
    };
    let av_threads = || {
        std::fs::read_dir("/proc/self/task")
            .map(|d| d.filter_map(|e| std::fs::read_to_string(e.ok()?.path().join("comm")).ok()).filter(|c| c.starts_with("av:")).count())
            .unwrap_or(0)
    };
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let d = std::sync::Arc::clone(&done);
    let sampler = std::thread::spawn(move || {
        let (mut most, mut peak_mb) = (0usize, 0.0_f64);
        while !d.load(std::sync::atomic::Ordering::Relaxed) {
            most = most.max(av_threads());
            peak_mb = peak_mb.max(rss_mb());
            std::thread::sleep(Duration::from_millis(1));
        }
        (most, peak_mb)
    });
    let base = rss_mb();
    let (rgba, w, h) = klipp_engine::generate_thumbnail_at_size(&src, 7.8, 480).expect("the thumbnail at 7.8 s");
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    let (most, peak) = sampler.join().unwrap();
    assert!(w > 0 && h > 0 && rgba.len() == (w * h * 4) as usize);
    eprintln!("thumbnail decode: {most} decoder threads at most, {:.0} MB added", peak - base);
    assert!(most <= 4, "one thumbnail decoded on {most} decoder threads, each holding frames of its own");
}
