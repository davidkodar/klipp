//! The GPU waveform and parade measure what they are given: a flat grey
//! is a line at the grey's encoded level, a ramp is a ramp, the three
//! parade panels each carry their channel, and the transfer moves the
//! line where it should. Skips without a GPU.

use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_effects::gpu_scopes::{parade_gap, ScopeInput, ScopeTransfer, WaveformJob, WaveformMode};

const W: u32 = 256;
const H: u32 = 64;
const OUT: u32 = 256;
const REC709: [f32; 3] = [0.2126, 0.7152, 0.0722];
const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

fn flat(v: f32) -> Vec<f32> {
    (0..W * H).flat_map(|_| [v, v, v, 1.0]).collect()
}

fn job(mode: WaveformMode, out_w: u32) -> WaveformJob {
    WaveformJob { mode, intensity: -1.0, channels: 7, envelope: false, glow: false, out_w, out_h: OUT }
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
    let (left, _) = brightest_row(img, OUT, 20);
    let (mid, _) = brightest_row(img, OUT, 128);
    let (right, _) = brightest_row(img, OUT, 240);
    assert!(left > mid && mid > right, "the trace rises left to right: {left} {mid} {right}");
    let expect_mid = row_of(linear_to_srgb(128.0 / 255.0));
    assert!((mid as f32 - expect_mid).abs() <= 2.0, "mid column at row {mid}, expected {expect_mid:.1}");

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
    let frame = flat(0.4);
    let input = ScopeInput::Linear { rgba: &frame, width: W, height: H, matrix: IDENTITY, luma: REC709, transfer: ScopeTransfer::Srgb, scale: 1.0 };
    let plain = job(WaveformMode::Luma, OUT);
    let glowing = WaveformJob { glow: true, envelope: true, ..plain };
    let out = gpu.scopes().render(&input, 1, &[plain, glowing]);
    let (a, b) = (out[0].as_ref().unwrap(), out[1].as_ref().unwrap());
    let (row_a, _) = brightest_row(a, OUT, 128);
    let (row_b, _) = brightest_row(b, OUT, 128);
    assert!((row_a as i32 - row_b as i32).abs() <= 1, "glow keeps the line where it is: {row_a} vs {row_b}");
    let near = ((row_a + 4) * OUT as usize + 128) * 4;
    assert_eq!(&a[near..near + 3], &[0, 0, 0], "no glow: dark beside the line");
    assert!(b[near] + b[near + 1] + b[near + 2] > 0, "glow: light beside the line");
}
