//! Playing on the GPU stack with the UI sharing the engine's device, the
//! frames go to the viewer's texture and never come back as bytes. The
//! scopes used to break that: every due pass had the stack read the 4K
//! frame back, 33 MB and 24 ms, only to upload it again for the GPU
//! scopes (#333). Now they measure it where the encode left it, so they
//! keep refreshing while the display bytes do not move.
//!
//! Its own test binary: the shared device is process-wide, installed
//! before the engine makes its pipeline, as the UI does at start.

mod common;

use klipp_core::AppCommand;
use std::time::{Duration, Instant};

#[test]
fn the_scopes_refresh_while_playing_without_a_readback() {
    let Some(ctx) = klipp_effects::gpu_context::GpuContext::new() else {
        eprintln!("no GPU; skipping");
        return;
    };
    if klipp_effects::gpu_context::GpuContext::install_shared(ctx).is_err() {
        eprintln!("a shared device was already installed; skipping");
        return;
    }
    let Some(src) = common::hevc_ramp() else { return };
    let parked = common::Parked::start(src, common::RAMP_W, common::RAMP_H);
    parked.send(AppCommand::SetScopesVisible(true));
    parked.send(AppCommand::SetActiveScopeModes { modes: [0u8, 1, 2, 3].into_iter().collect() });
    parked.settle();

    let scope_generations = || -> Vec<u64> { (0..4).map(|m| parked.scope_texture(m).map_or(0, |e| e.generation)).collect() };
    parked.send(AppCommand::Play);
    // Let playback reach the GPU stack, then watch a stretch of it.
    let deadline = Instant::now() + Duration::from_secs(10);
    while parked.gpu_frames_and_generation().0 < 3 {
        assert!(Instant::now() < deadline, "no frame reached the viewer's texture: the GPU stack never presented");
        std::thread::sleep(Duration::from_millis(5));
    }
    let (gpu_before, bytes_before) = parked.gpu_frames_and_generation();
    let scopes_before = scope_generations();
    std::thread::sleep(Duration::from_millis(600));
    let (gpu_after, bytes_after) = parked.gpu_frames_and_generation();
    let scopes_after = scope_generations();
    parked.send(AppCommand::Pause);

    assert!(gpu_after > gpu_before + 3, "playback stalled: {gpu_before} → {gpu_after} GPU frames");
    assert_eq!(bytes_after, bytes_before, "display bytes were published while playing zero-copy: a readback");
    for m in 0..4 {
        assert!(
            scopes_after[m] >= scopes_before[m] + 2,
            "scope {m} did not refresh while playing: generation {} → {}",
            scopes_before[m],
            scopes_after[m],
        );
    }
}
