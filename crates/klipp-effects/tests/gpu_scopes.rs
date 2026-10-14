//! The GPU scopes measure what they are given: a flat grey is a line at
//! the grey's encoded level, a ramp is a ramp, the three parade panels
//! each carry their channel, the transfer moves the line where it should,
//! and the histogram counts every level the picture holds. Skips without
//! a GPU.

use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_effects::gpu_scopes::{
    histogram_panels, parade_gap, HistogramJob, HistogramLayout, HistogramPanel, ScopeInput, ScopeJob, ScopeTransfer, VectorscopeJob, WaveformJob,
    WaveformMode,
};

const W: u32 = 256;
const H: u32 = 64;
const OUT: u32 = 256;
const REC709: [f32; 3] = [0.2126, 0.7152, 0.0722];
const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

fn flat(v: f32) -> Vec<f32> {
    (0..W * H).flat_map(|_| [v, v, v, 1.0]).collect()
}

fn job(mode: WaveformMode, out_w: u32) -> ScopeJob {
    ScopeJob::Waveform(WaveformJob { mode, intensity: -1.0, channels: 7, envelope: false, glow: false, out_w, out_h: OUT })
}

fn vector(zoom: f32, colorize: bool) -> ScopeJob {
    ScopeJob::Vectorscope(VectorscopeJob { zoom, intensity: -1.0, colorize, luma_lo: 0.0, luma_hi: 1.0, glow: false, out_w: OUT, out_h: OUT })
}

/// The brightest pixel of an `OUT × OUT` image.
fn brightest_pixel(img: &[u8]) -> (u32, u32, [u8; 3]) {
    let (i, _) = img.chunks_exact(4).enumerate().max_by_key(|(_, p)| p[0] as u32 + p[1] as u32 + p[2] as u32).unwrap();
    let px = &img[i * 4..i * 4 + 3];
    ((i as u32) % OUT, (i as u32) / OUT, [px[0], px[1], px[2]])
}

/// Where a chroma lands in the output at `zoom`.
fn plane_px(cb: f32, cr: f32, zoom: f32) -> (f32, f32) {
    ((cb * zoom + 0.5) * OUT as f32, (0.5 - cr * zoom) * OUT as f32)
}

/// The brightest row of column `x` in an `out_w × OUT` RGBA8 image, by
/// the sum of its channels.
fn brightest_row(img: &[u8], out_w: u32, x: u32) -> (usize, u32) {
    (0..OUT as usize)
        .map(|y| {
            let i = (y * out_w as usize + x as usize) * 4;
            (y, img[i] as u32 + img[i + 1] as u32 + img[i + 2] as u32)
        })
        .max_by_key(|(_, s)| *s)
        .unwrap()
}

/// The row the light of column `x` is centred on. The centre, not the
/// brightest row: a dense trace saturates to white over several rows, and
/// the brightest of those is whichever tie comes last.
fn light_row(img: &[u8], out_w: u32, x: u32) -> f32 {
    let (mut light, mut moment) = (0f32, 0f32);
    for y in 0..OUT as usize {
        let i = (y * out_w as usize + x as usize) * 4;
        let l = (img[i] as u32 + img[i + 1] as u32 + img[i + 2] as u32) as f32;
        light += l;
        moment += l * y as f32;
    }
    assert!(light > 0.0, "column {x} is dark");
    moment / light
}

/// The output row a level lands on: row 0 is level 1.0.
fn row_of(level: f32) -> f32 {
    (1.0 - level) * OUT as f32
}

fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

#[test]
fn a_flat_grey_is_a_line_at_its_encoded_level_in_the_chosen_transfer() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else { return };
    let frame = flat(0.18);
    for (transfer, level) in [(ScopeTransfer::Srgb, linear_to_srgb(0.18)), (ScopeTransfer::Bt1886, 0.18f32.powf(1.0 / 2.4))] {
        let input = ScopeInput::Linear { rgba: &frame, width: W, height: H, matrix: IDENTITY, luma: REC709, transfer, scale: 1.0 };
        let out = gpu.scopes().render(&input, 1, &[job(WaveformMode::Luma, OUT)]);
        let img = out[0].as_ref().expect("a rendered waveform");
        assert_eq!(img.len(), (OUT * OUT * 4) as usize);
        let (row, sum) = brightest_row(img, OUT, 128);
        let expect = row_of(level);
        assert!((row as f32 - expect).abs() <= 1.5, "{transfer:?}: line at row {row}, expected {expect:.1}");
        assert!(sum > 3 * 150, "a flat area is a bright, saturated line: {sum}");
        // Away from the line there is nothing.
        let far = ((row as i32 + 40) as usize * OUT as usize + 128) * 4;
        assert_eq!(&img[far..far + 3], &[0, 0, 0]);
    }
}

#[test]
fn a_ramp_is_a_ramp_and_display_bytes_are_binned_as_codes() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else { return };
    let ramp: Vec<f32> = (0..W * H)
        .flat_map(|i| {
            let v = (i % W) as f32 / (W - 1) as f32;
            [v, v, v, 1.0]
        })
        .collect();
    let input = ScopeInput::Linear { rgba: &ramp, width: W, height: H, matrix: IDENTITY, luma: REC709, transfer: ScopeTransfer::Srgb, scale: 1.0 };
    let out = gpu.scopes().render(&input, 1, &[job(WaveformMode::RgbOverlay, OUT)]);
    let img = out[0].as_ref().unwrap();
    let left = light_row(img, OUT, 20);
    let mid = light_row(img, OUT, 128);
    let right = light_row(img, OUT, 240);
    assert!(left > mid && mid > right, "the trace rises left to right: {left:.1} {mid:.1} {right:.1}");
    let expect_mid = row_of(linear_to_srgb(128.0 / 255.0));
    assert!((mid - expect_mid).abs() <= 2.0, "mid column at row {mid:.1}, expected {expect_mid:.1}");

    let grey: Vec<u8> = (0..W * H).flat_map(|_| [118u8, 118, 118, 255]).collect();
    let input = ScopeInput::Display { rgba: &grey, width: W, height: H, luma: REC709 };
    let out = gpu.scopes().render(&input, 1, &[job(WaveformMode::Luma, OUT)]);
    let (row, _) = brightest_row(out[0].as_ref().unwrap(), OUT, 100);
    let expect = row_of(118.0 / 255.0);
    assert!((row as f32 - expect).abs() <= 1.5, "code 118 at row {row}, expected {expect:.1}");
}

#[test]
fn the_parade_puts_each_channel_in_its_own_panel_and_measures_every_second_pixel_the_same() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else { return };
    // Red 0.5, green 0.18, blue 0.02 in linear light.
    let frame: Vec<f32> = (0..W * H).flat_map(|_| [0.5, 0.18, 0.02, 1.0]).collect();
    let input = ScopeInput::Linear { rgba: &frame, width: W, height: H, matrix: IDENTITY, luma: REC709, transfer: ScopeTransfer::Srgb, scale: 1.0 };
    let gap = parade_gap(3 * OUT + 16);
    let out_w = 3 * OUT + 2 * gap;
    let full = gpu.scopes().render(&input, 1, &[job(WaveformMode::Parade, out_w)]);
    let half = gpu.scopes().render(&input, 2, &[job(WaveformMode::Parade, out_w)]);
    let (full, half) = (full[0].as_ref().unwrap(), half[0].as_ref().unwrap());
    let panel_w = (out_w - 2 * gap) / 3;
    for (panel, level, ch) in [(0u32, 0.5f32, 0usize), (1, 0.18, 1), (2, 0.02, 2)] {
        let x = panel * (panel_w + gap) + panel_w / 2;
        let (row, _) = brightest_row(full, out_w, x);
        let expect = row_of(linear_to_srgb(level));
        assert!((row as f32 - expect).abs() <= 1.5, "panel {panel} line at row {row}, expected {expect:.1}");
        let i = (row * out_w as usize + x as usize) * 4;
        let px = &full[i..i + 3];
        assert!(px[ch] > px[(ch + 1) % 3] && px[ch] > px[(ch + 2) % 3], "panel {panel} is drawn in its channel's colour: {px:?}");
        let (row_half, _) = brightest_row(half, out_w, x);
        assert_eq!(row, row_half, "sampling every second pixel measures the same levels");
    }
    // The gap between panels is black.
    let gap_x = panel_w + gap / 2;
    for y in [10usize, 128, 250] {
        let i = (y * out_w as usize + gap_x as usize) * 4;
        assert_eq!(&full[i..i + 3], &[0, 0, 0], "gap at ({gap_x}, {y})");
    }
}

#[test]
fn glow_and_envelope_add_light_without_moving_the_trace() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else { return };
    // Enough samples that the trace is drawn unblurred, so the light beside
    // the line is the glow's alone.
    let frame: Vec<f32> = (0..W * 320).flat_map(|_| [0.4, 0.4, 0.4, 1.0]).collect();
    let input = ScopeInput::Linear { rgba: &frame, width: W, height: 320, matrix: IDENTITY, luma: REC709, transfer: ScopeTransfer::Srgb, scale: 1.0 };
    let plain = job(WaveformMode::Luma, OUT);
    let ScopeJob::Waveform(inner) = plain else { unreachable!() };
    let glowing = ScopeJob::Waveform(WaveformJob { glow: true, envelope: true, ..inner });
    let out = gpu.scopes().render(&input, 1, &[plain, glowing]);
    let (a, b) = (out[0].as_ref().unwrap(), out[1].as_ref().unwrap());
    let (row_a, _) = brightest_row(a, OUT, 128);
    let (row_b, _) = brightest_row(b, OUT, 128);
    assert!((row_a as i32 - row_b as i32).abs() <= 1, "glow keeps the line where it is: {row_a} vs {row_b}");
    let near = ((row_a + 4) * OUT as usize + 128) * 4;
    assert_eq!(&a[near..near + 3], &[0, 0, 0], "no glow: dark beside the line");
    assert!(b[near] + b[near + 1] + b[near + 2] > 0, "glow: light beside the line");
}

#[test]
fn the_vectorscope_puts_the_bars_on_their_targets_and_zooms_about_the_centre() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else { return };
    // Rec.709 Y′CbCr of an encoded colour.
    let chroma = |rgb: [f32; 3]| {
        let y = REC709[0] * rgb[0] + REC709[1] * rgb[1] + REC709[2] * rgb[2];
        ((rgb[2] - y) * 0.5 / (1.0 - REC709[2]), (rgb[0] - y) * 0.5 / (1.0 - REC709[0]))
    };
    // 75 % bars, one frame each, as the linear light of encoded 0.75 so
    // the scope's own encode lands them on 0.75.
    let level = 0.75f32;
    let lin = ((level + 0.055) / 1.055).powf(2.4);
    let bars = [[1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0], [0.0, 1.0, 1.0], [0.0, 0.0, 1.0], [1.0, 0.0, 1.0]];
    for (bar, rgb) in bars.iter().enumerate() {
        let frame: Vec<f32> = (0..W * H).flat_map(|_| [rgb[0] * lin, rgb[1] * lin, rgb[2] * lin, 1.0]).collect();
        let input = ScopeInput::Linear { rgba: &frame, width: W, height: H, matrix: IDENTITY, luma: REC709, transfer: ScopeTransfer::Srgb, scale: 1.0 };
        let (cb, cr) = chroma(rgb.map(|c| c * level));
        let out = gpu.scopes().render(&input, 1, &[vector(1.0, true)]);
        let img = out[0].as_ref().expect("a vectorscope");
        assert_eq!(img.len(), (OUT * OUT * 4) as usize);
        let (x, y, px) = brightest_pixel(img);
        let (ex, ey) = plane_px(cb, cr, 1.0);
        assert!((x as f32 - ex).abs() <= 2.5 && (y as f32 - ey).abs() <= 2.5, "bar {bar}: dot at ({x}, {y}), expected ({ex:.1}, {ey:.1})");
        // Tinted the bar's own way: its strongest channel leads.
        let lead = (0..3).max_by_key(|c| (rgb[*c] * 1000.0) as u32).unwrap();
        assert!(px[lead] >= px[(lead + 1) % 3] && px[lead] >= px[(lead + 2) % 3], "bar {bar} tint {px:?}");
    }
    // Zoom magnifies about the centre: a 20 % red bar sits at twice the
    // distance at 2×, and a 75 % one has left the view.
    let small = 0.2f32;
    let lin_small = ((small + 0.055) / 1.055).powf(2.4);
    let frame: Vec<f32> = (0..W * H).flat_map(|_| [lin_small, 0.0, 0.0, 1.0]).collect();
    let input = ScopeInput::Linear { rgba: &frame, width: W, height: H, matrix: IDENTITY, luma: REC709, transfer: ScopeTransfer::Srgb, scale: 1.0 };
    let (cb, cr) = chroma([small, 0.0, 0.0]);
    let out = gpu.scopes().render(&input, 1, &[vector(2.0, true)]);
    let (x, y, _) = brightest_pixel(out[0].as_ref().unwrap());
    let (ex, ey) = plane_px(cb, cr, 2.0);
    assert!((x as f32 - ex).abs() <= 2.5 && (y as f32 - ey).abs() <= 2.5, "20 % red at 2×: dot at ({x}, {y}), expected ({ex:.1}, {ey:.1})");
    let frame: Vec<f32> = (0..W * H).flat_map(|_| [lin, 0.0, 0.0, 1.0]).collect();
    let input = ScopeInput::Linear { rgba: &frame, width: W, height: H, matrix: IDENTITY, luma: REC709, transfer: ScopeTransfer::Srgb, scale: 1.0 };
    let out = gpu.scopes().render(&input, 1, &[vector(2.0, true)]);
    assert!(out[0].as_ref().unwrap().chunks_exact(4).all(|p| p[0] == 0 && p[1] == 0 && p[2] == 0), "75 % red is outside the 2× view");
    // Grey sits at the centre, and mono mode draws it without a hue.
    let grey = flat(0.3);
    let input = ScopeInput::Linear { rgba: &grey, width: W, height: H, matrix: IDENTITY, luma: REC709, transfer: ScopeTransfer::Srgb, scale: 1.0 };
    let out = gpu.scopes().render(&input, 1, &[vector(1.0, false)]);
    let (x, y, px) = brightest_pixel(out[0].as_ref().unwrap());
    assert!((x as f32 - OUT as f32 / 2.0).abs() <= 2.0 && (y as f32 - OUT as f32 / 2.0).abs() <= 2.0, "grey at ({x}, {y})");
    assert!(px[1] >= px[0] && px[1] >= px[2], "mono trace: {px:?}");
}

#[test]
fn the_vectorscope_luma_window_keeps_only_the_levels_asked_for() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else { return };
    // Left half a dark red, right half a bright red: same hue, different Y′.
    let frame: Vec<f32> = (0..W * H)
        .flat_map(|i| if (i % W) < W / 2 { [0.02, 0.0, 0.0, 1.0] } else { [0.8, 0.1, 0.1, 1.0] })
        .collect();
    let input = ScopeInput::Linear { rgba: &frame, width: W, height: H, matrix: IDENTITY, luma: REC709, transfer: ScopeTransfer::Srgb, scale: 1.0 };
    // The window is part of the measurement, so each is its own render.
    let every = VectorscopeJob { zoom: 1.0, intensity: -1.0, colorize: true, luma_lo: 0.0, luma_hi: 1.0, glow: false, out_w: OUT, out_h: OUT };
    let dark = VectorscopeJob { luma_hi: 0.3, ..every };
    let a = gpu.scopes().render(&input, 1, &[ScopeJob::Vectorscope(every)]);
    let d = gpu.scopes().render(&input, 1, &[ScopeJob::Vectorscope(dark)]);
    let lit = |img: &[u8]| img.chunks_exact(4).filter(|p| p[0] as u32 + p[1] as u32 + p[2] as u32 > 0).count();
    let (a, d) = (a[0].as_ref().unwrap(), d[0].as_ref().unwrap());
    assert!(lit(a) > lit(d) && lit(d) > 0, "the window drops the bright red: {} vs {}", lit(a), lit(d));
}

const HW: u32 = 512;
const HH: u32 = 256;

fn histogram(layout: HistogramLayout, log: bool) -> ScopeJob {
    ScopeJob::Histogram(HistogramJob { layout, log, fill: 0.5, glow: false, out_w: HW, out_h: HH })
}

/// How far up column `x` of `panel` anything is drawn, in pixels above
/// the panel's floor.
fn column_height(img: &[u8], panel: &HistogramPanel, x: u32) -> u32 {
    for y in 0..panel.h {
        let i = (((panel.y + y) * HW + panel.x + x) * 4) as usize;
        if img[i] as u32 + img[i + 1] as u32 + img[i + 2] as u32 > 24 {
            return panel.h - y;
        }
    }
    0
}

/// The tallest column of a panel, and its height.
fn tallest_column(img: &[u8], panel: &HistogramPanel) -> (u32, u32) {
    (0..panel.w).map(|x| (x, column_height(img, panel, x))).max_by_key(|(_, h)| *h).unwrap()
}

/// The tallest column within two of `x`.
fn height_near(img: &[u8], panel: &HistogramPanel, x: u32) -> u32 {
    (x.saturating_sub(2)..=(x + 2).min(panel.w - 1)).map(|c| column_height(img, panel, c)).max().unwrap()
}

/// The panel column an encoded level lands in.
fn column_of(level: f32, w: u32) -> u32 {
    ((level * 1023.0).round() as u32 * w) / 1024
}

fn linear_input(frame: &[f32]) -> ScopeInput<'_> {
    ScopeInput::Linear { rgba: frame, width: W, height: H, matrix: IDENTITY, luma: REC709, transfer: ScopeTransfer::Srgb, scale: 1.0 }
}

#[test]
fn a_flat_grey_is_one_spike_at_its_level_filled_to_the_peak_height() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else { return };
    let frame = flat(0.18);
    let input = linear_input(&frame);
    let panel = histogram_panels(HistogramLayout::Luma, HW, HH)[0];
    let expect = column_of(linear_to_srgb(0.18), panel.w);
    for step in [1u32, 2] {
        let out = gpu.scopes().render(&input, step, &[histogram(HistogramLayout::Luma, false)]);
        let img = out[0].as_ref().expect("a histogram");
        assert_eq!(img.len(), (HW * HH * 4) as usize);
        let (x, h) = tallest_column(img, &panel);
        assert!((x as i32 - expect as i32).abs() <= 2, "step {step}: spike at column {x}, expected {expect}");
        let peak = 0.9 * HH as f32;
        assert!((h as f32 - peak).abs() <= 2.0, "step {step}: the peak fills nine tenths of the panel: {h} vs {peak}");
        for far in [expect - 40, expect + 40] {
            assert_eq!(column_height(img, &panel, far), 0, "step {step}: nothing drawn at column {far}");
        }
    }
}

#[test]
fn every_8_bit_code_equally_often_is_a_flat_plateau_without_a_comb() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else { return };
    // Every code 0..=255 in every row: 8-bit codes land four 10-bit levels
    // apart, the comb a naive histogram draws.
    let ramp: Vec<u8> = (0..W * H)
        .flat_map(|i| {
            let c = (i % W) as u8;
            [c, c, c, 255]
        })
        .collect();
    let input = ScopeInput::Display { rgba: &ramp, width: W, height: H, luma: REC709 };
    let out = gpu.scopes().render(&input, 1, &[histogram(HistogramLayout::Luma, false)]);
    let img = out[0].as_ref().unwrap();
    let panel = histogram_panels(HistogramLayout::Luma, HW, HH)[0];
    // Away from the two ends, where the smoothing is cut off.
    let heights: Vec<u32> = (16..panel.w - 16).map(|x| column_height(img, &panel, x)).collect();
    let (lo, hi) = (*heights.iter().min().unwrap(), *heights.iter().max().unwrap());
    let peak = 0.9 * HH as f32;
    assert!(hi - lo <= 3, "a flat plateau, not a comb: heights {lo}..{hi}");
    assert!((hi as f32 - peak).abs() <= 3.0, "the plateau is the peak: {hi} vs {peak}");
}

#[test]
fn the_parade_and_stack_draw_each_channel_in_its_own_panel_and_the_overlay_adds() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else { return };
    let frame: Vec<f32> = (0..W * H).flat_map(|_| [0.5, 0.18, 0.02, 1.0]).collect();
    let input = linear_input(&frame);
    let levels = [linear_to_srgb(0.5), linear_to_srgb(0.18), linear_to_srgb(0.02)];
    for layout in [HistogramLayout::Parade, HistogramLayout::Stack] {
        let out = gpu.scopes().render(&input, 1, &[histogram(layout, false)]);
        let img = out[0].as_ref().unwrap();
        let panels = histogram_panels(layout, HW, HH);
        assert_eq!(panels.len(), 3);
        for (ch, panel) in panels.iter().enumerate() {
            assert_eq!(panel.channels, 1 << ch);
            let (x, h) = tallest_column(img, panel);
            let expect = column_of(levels[ch], panel.w);
            assert!((x as i32 - expect as i32).abs() <= 2, "{layout:?} panel {ch}: spike at {x}, expected {expect}");
            assert!(h as f32 >= 0.75 * panel.h as f32, "{layout:?} panel {ch}: spike {h} of {}", panel.h);
            let i = (((panel.y + panel.h - h / 2) * HW + panel.x + x) * 4) as usize;
            let px = &img[i..i + 3];
            assert!(px[ch] > px[(ch + 1) % 3] && px[ch] > px[(ch + 2) % 3], "{layout:?} panel {ch} is drawn in its colour: {px:?}");
        }
        // Between the panels is black.
        let (a, b) = (panels[0], panels[1]);
        let (gx, gy) = if layout == HistogramLayout::Parade { ((a.x + a.w + b.x) / 2, HH - 4) } else { (column_of(levels[0], HW), (a.y + a.h + b.y) / 2) };
        let i = ((gy * HW + gx) * 4) as usize;
        assert_eq!(&img[i..i + 3], &[0, 0, 0], "{layout:?}: gap at ({gx}, {gy})");
    }
    // Red and green at one level, drawn over each other, add to yellow.
    let frame: Vec<f32> = (0..W * H).flat_map(|_| [0.5, 0.5, 0.0, 1.0]).collect();
    let out = gpu.scopes().render(&linear_input(&frame), 1, &[histogram(HistogramLayout::Overlay, false)]);
    let img = out[0].as_ref().unwrap();
    let x = column_of(linear_to_srgb(0.5), HW);
    let i = (((HH - 20) * HW + x) * 4) as usize;
    let px = &img[i..i + 3];
    assert!(px[0] > 2 * px[2] && px[1] > 2 * px[2], "red over green reads yellow: {px:?}");
}

#[test]
fn log_levels_lift_a_small_population_and_clipped_samples_do_not_set_the_scale() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else { return };
    let panel = histogram_panels(HistogramLayout::Luma, HW, HH)[0];
    // Nine tenths at one grey, a tenth at a darker one.
    let frame: Vec<f32> = (0..W * H)
        .flat_map(|i| {
            let v = if i % 10 == 0 { 0.05 } else { 0.4 };
            [v, v, v, 1.0]
        })
        .collect();
    let small = column_of(linear_to_srgb(0.05), panel.w);
    let out = gpu.scopes().render(&linear_input(&frame), 1, &[histogram(HistogramLayout::Luma, false), histogram(HistogramLayout::Luma, true)]);
    let lin = height_near(out[0].as_ref().unwrap(), &panel, small) as f32;
    let log = height_near(out[1].as_ref().unwrap(), &panel, small) as f32;
    // A spike's smoothed height depends on where between two levels it
    // falls, by up to an eighth; the ratio of two spikes carries that.
    let peak = 0.9 * HH as f32;
    let expect_lin = peak / 9.0;
    let expect_log = (1.0 + 1024.0 / 9.0f32).log2() / 1025.0f32.log2() * peak;
    assert!((lin - expect_lin).abs() <= expect_lin * 0.15 + 2.0, "linear: a ninth of the peak, {lin} vs {expect_lin:.1}");
    assert!((log - expect_log).abs() <= expect_log * 0.05 + 2.0, "log: {log} vs {expect_log:.1}");

    // Six tenths clipped white, the rest a mid grey.
    let frame: Vec<f32> = (0..W * H)
        .flat_map(|i| {
            let v = if i % 10 < 6 { 2.0 } else { 0.18 };
            [v, v, v, 1.0]
        })
        .collect();
    let out = gpu.scopes().render(&linear_input(&frame), 1, &[histogram(HistogramLayout::Luma, false)]);
    let img = out[0].as_ref().unwrap();
    let grey = height_near(img, &panel, column_of(linear_to_srgb(0.18), panel.w)) as f32;
    // The curve's line sits up to two pixels above the fill it tops.
    assert!(grey >= 0.78 * HH as f32 && grey <= peak + 2.5, "the grey still sets the scale: {grey}");
    assert_eq!(column_height(img, &panel, panel.w - 1), panel.h, "the clipped pile is a spike to the top");
}

#[test]
fn drawn_at_a_pane_size_a_one_pixel_column_stays_one_column_and_the_parade_keeps_its_levels() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else { return };
    // Black, but for one column of white pixels, measured at the width it
    // is drawn at, from a full HD frame: samples enough that the trace is
    // not blurred, so the column is as narrow as the measurement.
    const WW: u32 = 1920;
    const WH: u32 = 1080;
    let (out_w, out_h) = (1920u32, 700u32);
    let frame: Vec<f32> = (0..WW * WH).flat_map(|i| if i % WW == 1000 { [1.0, 1.0, 1.0, 1.0] } else { [0.0, 0.0, 0.0, 1.0] }).collect();
    let input = ScopeInput::Linear { rgba: &frame, width: WW, height: WH, matrix: IDENTITY, luma: REC709, transfer: ScopeTransfer::Srgb, scale: 1.0 };
    let job = ScopeJob::Waveform(WaveformJob { mode: WaveformMode::Luma, intensity: -1.0, channels: 7, envelope: false, glow: false, out_w, out_h });
    let out = gpu.scopes().render(&input, 1, &[job]);
    let img = out[0].as_ref().unwrap();
    assert_eq!(img.len(), (out_w * out_h * 4) as usize);
    let lit: Vec<u32> = (0..out_w)
        .filter(|&x| {
            (0..4u32).any(|y| {
                let i = ((y * out_w + x) * 4) as usize;
                img[i] as u32 + img[i + 1] as u32 + img[i + 2] as u32 > 60
            })
        })
        .collect();
    assert_eq!(lit, vec![1000], "the white column is one scope column wide");

    // A parade at an arbitrary pane size keeps each panel's line at its level.
    let frame: Vec<f32> = (0..W * H).flat_map(|_| [0.5, 0.18, 0.02, 1.0]).collect();
    let input = linear_input(&frame);
    let (pw, ph) = (1537u32, 811u32);
    let job = ScopeJob::Waveform(WaveformJob { mode: WaveformMode::Parade, intensity: -1.0, channels: 7, envelope: false, glow: false, out_w: pw, out_h: ph });
    let out = gpu.scopes().render(&input, 2, &[job]);
    let img = out[0].as_ref().unwrap();
    let gap = parade_gap(pw);
    let panel_w = (pw - 2 * gap) / 3;
    for (panel, level) in [(0u32, 0.5f32), (1, 0.18), (2, 0.02)] {
        let x = panel * (panel_w + gap) + panel_w / 2;
        // The centre of the line's light: a trace this sparse is blurred,
        // and its brightest row is any of the saturated ones.
        let expect = (1.0 - linear_to_srgb(level)) * ph as f32;
        let (mut light, mut moment) = (0f32, 0f32);
        for y in ((expect - 40.0).max(0.0) as u32)..((expect + 40.0) as u32).min(ph) {
            let i = ((y * pw + x) * 4) as usize;
            let l = (img[i] as u32 + img[i + 1] as u32 + img[i + 2] as u32) as f32;
            light += l;
            moment += l * y as f32;
        }
        let row = moment / light.max(1.0);
        assert!((row - expect).abs() <= 2.0, "panel {panel}: line centred on row {row:.1}, expected {expect:.1}");
    }
}

#[test]
fn a_sparse_noisy_picture_draws_a_smooth_trace_not_specks() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else { return };
    // 160x90 samples for a 1024x512 scope: about fourteen per column, each
    // at a noisy level about a rising ramp, as a small picture measured into
    // a large pane is.
    const SW: u32 = 160;
    const SH: u32 = 90;
    let mut seed = 0x2545_f491u32;
    let mut rand = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as f32 / u32::MAX as f32
    };
    let frame: Vec<f32> = (0..SW * SH)
        .flat_map(|i| {
            let x = (i % SW) as f32 / (SW - 1) as f32;
            let enc = 0.3 + 0.4 * x + (rand() - 0.5) * 0.12;
            let lin = ((enc + 0.055) / 1.055).powf(2.4);
            [lin, lin, lin, 1.0]
        })
        .collect();
    let input = ScopeInput::Linear { rgba: &frame, width: SW, height: SH, matrix: IDENTITY, luma: REC709, transfer: ScopeTransfer::Srgb, scale: 1.0 };
    let (ow, oh) = (1024u32, 512u32);
    let job = ScopeJob::Waveform(WaveformJob { mode: WaveformMode::Luma, intensity: -1.0, channels: 7, envelope: false, glow: false, out_w: ow, out_h: oh });
    let out = gpu.scopes().render(&input, 1, &[job]);
    let img = out[0].as_ref().unwrap();
    let lum = |x: u32, y: u32| {
        let i = ((y * ow + x) * 4) as usize;
        img[i] as u32 + img[i + 1] as u32 + img[i + 2] as u32
    };
    // A lit pixel whose four neighbours are all dark is a speck.
    let (mut lit, mut specks) = (0u32, 0u32);
    for y in 1..oh - 1 {
        for x in 1..ow - 1 {
            if lum(x, y) > 30 {
                lit += 1;
                if lum(x - 1, y) <= 30 && lum(x + 1, y) <= 30 && lum(x, y - 1) <= 30 && lum(x, y + 1) <= 30 {
                    specks += 1;
                }
            }
        }
    }
    assert!(lit > 10_000, "the band is drawn: {lit} lit pixels");
    assert!((specks as f32) < 0.002 * lit as f32, "{specks} isolated specks among {lit} lit pixels");
    // And the band sits where the picture is: about the middle columns its
    // light is centred on the ramp's middle level. (The brightest single row
    // of a band this sparse is anywhere in it; the centre of its light is not.)
    let (mut light, mut moment) = (0f64, 0f64);
    for x in 496..528 {
        for y in 0..oh {
            light += lum(x, y) as f64;
            moment += lum(x, y) as f64 * y as f64;
        }
    }
    let row = moment / light;
    let expect = 0.5 * oh as f64;
    assert!((row - expect).abs() <= 8.0, "the band's light centred on row {row:.1}, expected {expect}");
}

/// Fine texture: a checkerboard of two greys a pixel apart.
fn checkerboard(w: u32, h: u32) -> Vec<f32> {
    (0..w * h)
        .flat_map(|i| {
            let v = if (i % w + i / w).is_multiple_of(2) { 0.1 } else { 0.3 };
            [v, v, v, 1.0]
        })
        .collect()
}

/// How far column `x`'s light spreads about its centre, in rows.
fn light_spread(img: &[u8], out_w: u32, x: u32) -> f32 {
    let centre = light_row(img, out_w, x);
    let (mut light, mut moment) = (0f32, 0f32);
    for y in 0..OUT as usize {
        let i = (y * out_w as usize + x as usize) * 4;
        let l = (img[i] as u32 + img[i + 1] as u32 + img[i + 2] as u32) as f32;
        light += l;
        moment += l * (y as f32 - centre).powi(2);
    }
    (moment / light).sqrt()
}

#[test]
fn the_low_pass_filter_draws_fine_texture_as_one_line_at_its_mean() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else { return };
    let (w, h) = (2048, 1080);
    let frame = checkerboard(w, h);
    let input = ScopeInput::Linear { rgba: &frame, width: w, height: h, matrix: IDENTITY, luma: REC709, transfer: ScopeTransfer::Srgb, scale: 1.0 };
    let wave = [job(WaveformMode::Luma, OUT)];
    let sharp = gpu.scopes().render(&input, 1, &wave).remove(0).expect("a waveform");
    gpu.scopes().set_low_pass(true);
    let smooth = gpu.scopes().render(&input, 1, &wave).remove(0).expect("a waveform");
    gpu.scopes().set_low_pass(false);
    let (sharp_spread, smooth_spread) = (light_spread(&sharp, OUT, 128), light_spread(&smooth, OUT, 128));
    assert!(smooth_spread * 4.0 < sharp_spread, "two lines become one: spread {sharp_spread:.1} rows, then {smooth_spread:.1}");
    // The line sits at the texture's mean level, between its two greys.
    let expect = row_of((linear_to_srgb(0.1) + linear_to_srgb(0.3)) / 2.0);
    let row = light_row(&smooth, OUT, 128);
    assert!((row - expect).abs() <= 2.0, "line at row {row:.1}, expected {expect:.1}");
}

#[test]
fn the_low_pass_filter_leaves_the_histogram_and_vectorscope_measuring_the_picture() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else { return };
    let (w, h) = (2048, 1080);
    let frame = checkerboard(w, h);
    let input = ScopeInput::Linear { rgba: &frame, width: w, height: h, matrix: IDENTITY, luma: REC709, transfer: ScopeTransfer::Srgb, scale: 1.0 };
    let hist = ScopeJob::Histogram(HistogramJob { layout: HistogramLayout::Overlay, log: false, fill: 0.7, glow: false, out_w: OUT, out_h: OUT });
    let jobs = [job(WaveformMode::Luma, OUT), vector(1.0, true), hist];
    let sharp = gpu.scopes().render(&input, 1, &jobs);
    gpu.scopes().set_low_pass(true);
    let smooth = gpu.scopes().render(&input, 1, &jobs);
    gpu.scopes().set_low_pass(false);
    assert_ne!(sharp[0], smooth[0], "the waveform measures the low-passed copy");
    assert!(sharp[1] == smooth[1], "the vectorscope measures the picture itself");
    assert!(sharp[2] == smooth[2], "the histogram counts the picture itself");
}

/// #466: with a viewing LUT set, the scopes measure the picture through
/// it: grey code 118 under an inverting LUT reads as 137, from display
/// bytes and from linear light alike; with the LUT taken away, as 118
/// again.
#[test]
fn the_scopes_measure_after_a_viewing_lut_when_one_is_set() {
    let Some(mut gpu) = GpuEffectsPipeline::new() else { return };
    let n = 5u32;
    let mut invert = Vec::new();
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                let v = |i: u32| 1.0 - i as f32 / (n - 1) as f32;
                invert.extend_from_slice(&[v(r), v(g), v(b)]);
            }
        }
    }
    let grey: Vec<u8> = (0..W * H).flat_map(|_| [118u8, 118, 118, 255]).collect();
    let display = ScopeInput::Display { rgba: &grey, width: W, height: H, luma: REC709 };
    let line = |gpu: &mut GpuEffectsPipeline, input: &ScopeInput| {
        let out = gpu.scopes().render(input, 1, &[job(WaveformMode::Luma, OUT)]);
        brightest_row(out[0].as_ref().expect("a waveform"), OUT, 100).0 as f32
    };
    gpu.scopes().set_lut(Some((1, n, &invert)));
    let through = line(&mut gpu, &display);
    assert!((through - row_of(137.0 / 255.0)).abs() <= 1.5, "through the LUT: row {through}");
    // Linear light encoded to the same code reads the same through it.
    let lin = {
        let c = 118.0f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    let frame = flat(lin);
    let linear = ScopeInput::Linear { rgba: &frame, width: W, height: H, matrix: IDENTITY, luma: REC709, transfer: ScopeTransfer::Srgb, scale: 1.0 };
    let through_linear = line(&mut gpu, &linear);
    assert!((through_linear - row_of(137.0 / 255.0)).abs() <= 2.0, "linear through the LUT: row {through_linear}");
    gpu.scopes().set_lut(None);
    let plain = line(&mut gpu, &display);
    assert!((plain - row_of(118.0 / 255.0)).abs() <= 1.5, "without it: row {plain}");
}
