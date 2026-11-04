//! J with the UI sharing the engine's device, as the app does: the frames
//! go to the viewer's texture, and what is read there is the picture.
//!
//! Its own test binary: the shared device is process-wide, installed
//! before the engine makes its pipeline, as the UI does at start.

mod common;

use klipp_core::{AppCommand, RationalTime};
use std::time::{Duration, Instant};

/// The picture on screen, 8 by 8 cells of mean luma, and where it is.
fn picture(parked: &common::Parked) -> Option<(Vec<f32>, f64, bool)> {
    let fb = parked.frame_buffer().lock().unwrap();
    let at = fb.shown_presented()?.to_secs_f64();
    let live = fb.gpu_output_live;
    let (bytes, w, h) = if live {
        let snap = fb.gpu_output_snapshot()?;
        drop(fb);
        (snap.read().ok()?, snap.width as usize, snap.height as usize)
    } else {
        let (bytes, w, h, _) = fb.latest_frame();
        (bytes.to_vec(), w as usize, h as usize)
    };
    if w == 0 || h == 0 || bytes.len() < w * h * 4 {
        return None;
    }
    let mut cells = vec![(0.0f32, 0u32); 64];
    for y in (0..h).step_by(7) {
        for x in (0..w).step_by(7) {
            let i = (y * 8 / h) * 8 + x * 8 / w;
            let p = (y * w + x) * 4;
            cells[i].0 += 0.3 * f32::from(bytes[p]) + 0.6 * f32::from(bytes[p + 1]) + 0.1 * f32::from(bytes[p + 2]);
            cells[i].1 += 1;
        }
    }
    Some((cells.iter().map(|c| c.0 / c.1.max(1) as f32).collect(), at, live))
}

#[test]
fn j_shows_the_frame_on_the_viewers_texture() {
    let Some(ctx) = klipp_effects::gpu_context::GpuContext::new() else {
        eprintln!("no GPU; skipping");
        return;
    };
    if klipp_effects::gpu_context::GpuContext::install_shared(ctx).is_err() {
        eprintln!("a shared device was already installed; skipping");
        return;
    }
    let Some(src) = common::hevc_long(30) else { return };
    let parked = common::Parked::start_long(src, common::RAMP_W, common::RAMP_H, 30);
    let tc = |s: f64| RationalTime::from_secs_f64(s, 30_000);
    let g = parked.generation();
    parked.send(AppCommand::SetPlayhead(tc(20.0)));
    parked.wait_new(g, Duration::from_secs(20));
    parked.settle();
    parked.send(AppCommand::SetSpeed(-1.0));
    parked.send(AppCommand::Play);

    let t0 = Instant::now();
    let mut seen: Vec<(f64, f32, bool)> = Vec::new();
    let mut last = None;
    while t0.elapsed() < Duration::from_secs(3) {
        if let Some((cells, at, live)) = picture(&parked) {
            seen.push((at, cells.iter().sum::<f32>() / cells.len() as f32, live));
            last = Some((cells, at));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let (during, at) = last.expect("a picture during J");
    assert!(seen.iter().all(|s| s.1 > 30.0), "never black while its window fills: {seen:?}");
    assert!(seen.last().is_some_and(|s| s.0 < 19.0), "ran back: {seen:?}");
    let luma = during.iter().sum::<f32>() / during.len() as f32;
    parked.send(AppCommand::SetSpeed(1.0));
    parked.send(AppCommand::Pause);
    std::thread::sleep(Duration::from_millis(500));
    let g = parked.generation();
    parked.send(AppCommand::SetPlayhead(tc(at)));
    parked.wait_new(g, Duration::from_secs(20));
    parked.settle();
    let (at_rest, _, _) = picture(&parked).expect("the parked picture");
    let rest = at_rest.iter().sum::<f32>() / at_rest.len() as f32;
    let apart = during.iter().zip(&at_rest).map(|(a, b)| (a - b).abs()).sum::<f32>() / during.len() as f32;
    assert!(apart < 12.0 && (luma - rest).abs() < 12.0, "the frame parked at {at}: luma {luma} against {rest}, {apart} apart: {seen:?}");
}
