//! Hardware decoders share the GPU's one CUDA context. Each NVDEC decoder
//! made a CUDA context of its own, each with its own GPU channels and
//! memory: a session's decoders ran the GPU out of channels, and no
//! other process could make a device ("NV_ERR_STATE_IN_USE" from the
//! driver). Eight decoders open at once now hold about what one does,
//! and opening and closing them over and over leaves nothing behind.
//!
//! Read per process: the driver's handles this process has open, and its
//! GPU memory from `nvidia-smi`. Without hardware decode or nvidia-smi
//! there is nothing to measure, and it says so. Its own binary: what it
//! reads is the process's.

mod common;

use std::time::{Duration, Instant};

use klipp_engine::video_decoder::VideoDecoder;

/// Decoders held open at once.
const OPEN: usize = 8;
/// Open-and-close rounds of [`OPEN`] decoders.
const ROUNDS: usize = 3;
/// What seven more decoders may add to the driver's handles: a decoder's
/// own (three each measured), never a context's (seventeen each).
const MORE_HANDLES: usize = 4 * (OPEN - 1);
/// What seven more decoders may add to the GPU memory: their surfaces
/// (some 33 MB each measured at 640x360), not a context each (276 MB
/// each).
const MORE_MB: f64 = 7.0 * 40.0;

/// The driver's device handles this process has open.
fn nvidia_handles() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .map(|d| d.filter_map(Result::ok).filter(|e| std::fs::read_link(e.path()).is_ok_and(|t| t.to_string_lossy().starts_with("/dev/nvidia"))).count())
        .unwrap_or(0)
}

/// This process's GPU memory in MB, `None` without `nvidia-smi`.
fn own_vram_mb() -> Option<f64> {
    let o = std::process::Command::new("nvidia-smi").args(["--query-compute-apps=pid,used_memory", "--format=csv,noheader,nounits"]).output().ok()?;
    if !o.status.success() {
        return None;
    }
    let me = std::process::id().to_string();
    Some(String::from_utf8_lossy(&o.stdout).lines().filter_map(|l| {
        let mut f = l.split(',').map(str::trim);
        (f.next()? == me).then(|| f.next()?.parse::<f64>().ok()).flatten()
    }).sum())
}

/// A decoder of `src`, its first frame decoded, so its hardware is made.
fn opened(src: &std::path::Path) -> VideoDecoder {
    let dec = VideoDecoder::open(src, 0).expect("open");
    dec.resume();
    let by = Instant::now() + Duration::from_secs(20);
    while Instant::now() < by && dec.frame_queue().lock().unwrap().is_empty() {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!dec.frame_queue().lock().unwrap().is_empty(), "no first frame");
    dec
}

#[test]
fn decoders_share_one_cuda_context() {
    let Some(src) = common::hevc_source("shared-context", 640, 360, common::SDR_TAGS) else {
        eprintln!("skipping: no ffmpeg");
        return;
    };
    let first = opened(&src);
    if !first.hw_active.load(std::sync::atomic::Ordering::Relaxed) {
        eprintln!("skipping: no hardware decode here");
        return;
    }
    let Some(_) = own_vram_mb() else {
        eprintln!("skipping: no nvidia-smi reading of the GPU memory here");
        return;
    };
    // Settled with one open: what the process holds for one decoder.
    std::thread::sleep(Duration::from_millis(300));
    let (handles_one, mb_one) = (nvidia_handles(), own_vram_mb().unwrap());
    let mut more: Vec<VideoDecoder> = (1..OPEN).map(|_| opened(&src)).collect();
    assert!(more.iter().all(|d| d.hw_active.load(std::sync::atomic::Ordering::Relaxed)), "every decoder on the GPU");
    std::thread::sleep(Duration::from_millis(300));
    let (handles_all, mb_all) = (nvidia_handles(), own_vram_mb().unwrap());
    eprintln!("MEASURE one decoder: {handles_one} handles, {mb_one:.0} MB; {OPEN}: {handles_all} handles, {mb_all:.0} MB");
    assert!(handles_all <= handles_one + MORE_HANDLES, "{OPEN} decoders hold {handles_all} driver handles against one's {handles_one}: a CUDA context each");
    assert!(mb_all <= mb_one + MORE_MB, "{OPEN} decoders hold {mb_all:.0} MB against one's {mb_one:.0}: a CUDA context each");

    // Opened and closed over and over, nothing is left behind.
    more.clear();
    for round in 0..ROUNDS {
        let all: Vec<VideoDecoder> = (1..OPEN).map(|_| opened(&src)).collect();
        drop(all);
        std::thread::sleep(Duration::from_millis(300));
        let handles = nvidia_handles();
        eprintln!("MEASURE round {round}: {handles} handles, {:.0} MB with one decoder open", own_vram_mb().unwrap());
        assert!(handles <= handles_one + 2, "round {round}: {handles} driver handles where one decoder held {handles_one}");
    }
    drop(first);
}
