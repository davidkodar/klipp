//! With the scopes open, a rendered frame's waveform, parade, vectorscope
//! and histogram come from the GPU scope renderer and measure the frame
//! the viewer shows: a flat grey frame is a line at the grey's display
//! code, and the histogram's spike.

mod common;

use klipp_core::AppCommand;
use std::time::{Duration, Instant};

/// The brightest row of column `x` in a `w × h` RGBA8 image.
fn brightest_row(img: &[u8], w: u32, h: u32, x: u32) -> usize {
    (0..h as usize)
        .max_by_key(|y| {
            let i = (y * w as usize + x as usize) * 4;
            img[i] as u32 + img[i + 1] as u32 + img[i + 2] as u32
        })
        .unwrap()
}

#[test]
fn the_waveform_and_parade_are_gpu_drawn_and_measure_the_shown_frame() {
    let Some(src) = common::hevc_ramp() else { return };
    let parked = common::Parked::start_ungraded(src, common::RAMP_W, common::RAMP_H);
    parked.send(AppCommand::SetScopesVisible(true));
    parked.send(AppCommand::SetActiveScopeModes { modes: [0u8, 1, 2, 3].into_iter().collect() });
    // A slider step renders the frame again with the scopes due.
    parked.begin_drag();
    parked.step(0.0);
    parked.end_drag();

    let deadline = Instant::now() + Duration::from_secs(5);
    let (wave, parade, vector, hist) = loop {
        let w = parked.scope_texture(0).filter(|e| !e.data.is_empty());
        let p = parked.scope_texture(1).filter(|e| !e.data.is_empty());
        let v = parked.scope_texture(2).filter(|e| !e.data.is_empty());
        let h = parked.scope_texture(3).filter(|e| !e.data.is_empty());
        if let (Some(w), Some(p), Some(v), Some(h)) = (w, p, v, h) {
            break (w, p, v, h);
        }
        assert!(Instant::now() < deadline, "no scope textures within 5 s");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(wave.gpu && parade.gpu && vector.gpu && hist.gpu, "all four scopes are drawn on the GPU");
    assert_eq!((vector.w, vector.h), (1024, 1024));
    assert_eq!((hist.w, hist.h), (1024, 512));
    assert_eq!((wave.w, wave.h), (1024, 512));
    assert_eq!(wave.data.len(), (wave.w * wave.h * 4) as usize);

    // The frame is flat but for codec noise: every waveform column
    // carries a line at the frame's most common display code.
    let frame = parked.frame();
    let mut codes = [0u32; 256];
    for px in frame.chunks_exact(4) {
        codes[px[1] as usize] += 1;
    }
    let code = (0..256).max_by_key(|c| codes[*c]).unwrap() as f64;
    let expect = (1.0 - code / 255.0) * wave.h as f64;
    for x in [100u32, 512, 900] {
        let row = brightest_row(&wave.data, wave.w, wave.h, x) as f64;
        assert!((row - expect).abs() <= 4.0, "column {x}: line at row {row}, expected {expect:.1} for code {code}");
    }
    let gap = klipp_effects::gpu_scopes::parade_gap(parade.w);
    let panel_w = (parade.w - 2 * gap) / 3;
    for panel in 0..3u32 {
        let x = panel * (panel_w + gap) + panel_w / 2;
        let row = brightest_row(&parade.data, parade.w, parade.h, x) as f64;
        assert!((row - expect).abs() <= 4.0, "parade panel {panel}: line at row {row}, expected {expect:.1}");
    }
    // A grey frame's chroma sits at the centre of the vectorscope.
    let (i, _) = vector.data.chunks_exact(4).enumerate().max_by_key(|(_, p)| p[0] as u32 + p[1] as u32 + p[2] as u32).unwrap();
    let (vx, vy) = ((i as u32 % vector.w) as f64, (i as u32 / vector.w) as f64);
    assert!((vx - 512.0).abs() <= 4.0 && (vy - 512.0).abs() <= 4.0, "grey at ({vx}, {vy})");
    // The histogram, in its default overlay, is tallest at the frame's code.
    let panel = klipp_effects::gpu_scopes::histogram_panels(klipp_effects::gpu_scopes::HistogramLayout::Overlay, hist.w, hist.h)[0];
    let lit_height = |x: u32| {
        (0..hist.h)
            .find(|y| {
                let i = ((y * hist.w + x) * 4) as usize;
                hist.data[i] as u32 + hist.data[i + 1] as u32 + hist.data[i + 2] as u32 > 24
            })
            .map_or(0, |y| hist.h - y)
    };
    let (hx, hh) = (0..panel.w).map(|x| (x, lit_height(x))).max_by_key(|(_, h)| *h).unwrap();
    let expect_x = code / 255.0 * panel.w as f64;
    assert!((hx as f64 - expect_x).abs() <= 6.0, "histogram spike at column {hx}, expected {expect_x:.1} for code {code}");
    assert!(hh as f64 >= 0.8 * hist.h as f64, "the spike fills the panel: {hh}");

    // Told the size the UI shows each scope at, the engine draws it there:
    // the vectorscope square at its smaller side. Nothing else happens —
    // paused, a pane resized has to redraw its scope by itself.
    parked.send(AppCommand::SetScopeSizes { sizes: [(0u8, (1700u32, 860u32)), (2, (900, 1000)), (3, (1300, 500))].into_iter().collect() });
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let w = parked.scope_texture(0).map(|e| (e.w, e.h));
        let v = parked.scope_texture(2).map(|e| (e.w, e.h));
        let h = parked.scope_texture(3).map(|e| (e.w, e.h));
        if (w, v, h) == (Some((1700, 860)), Some((900, 900)), Some((1300, 500))) {
            break;
        }
        assert!(Instant::now() < deadline, "scopes not redrawn at the pane sizes: {w:?} {v:?} {h:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
    let wave = parked.scope_texture(0).unwrap();
    assert_eq!(wave.data.len(), 1700 * 860 * 4);
    let expect = (1.0 - code / 255.0) * 860.0;
    let row = brightest_row(&wave.data, wave.w, wave.h, 850) as f64;
    assert!((row - expect).abs() <= 6.0, "waveform at its pane size: line at row {row}, expected {expect:.1}");
}
