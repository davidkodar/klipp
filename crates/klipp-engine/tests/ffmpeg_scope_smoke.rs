//! Smoke tests for the libavfilter scope renderer.
//!
//! Covers the regressions we've already burned ourselves on:
//!
//! * **All-green parade**: hit twice (no `format=gbrp,` prefix; then
//!   `scale=` placed before `format=` letting the FFmpeg filter-graph
//!   optimiser pick a YUV intermediate). Each `test_*_has_per_channel_*`
//!   here feeds a buffer where each input column has a single dominant
//!   channel and asserts the corresponding output region carries the
//!   matching colour.
//!
//! * **Settings-change forgotten**: the per-mode `FfmpegScope` instance
//!   in the bank used to skip rebuild on a settings flip. `test_filter_
//!   change_rebuilds_graph` flips the filter mid-run and confirms a
//!   visible change in output statistics.
//!
//! * **Bank mode crosstalk**: rendering different modes through one
//!   bank used to share a single graph briefly. `test_modes_keyed_
//!   separately` exercises waveform + parade back-to-back and asserts
//!   each pulls its own filter shape.

use klipp_core::{
    ChannelMask, HistogramDisplayMode, HistogramLevels, HistogramSettings, ScopeFfmpegSettings,
    ScopeQuality, VectorscopeMode, VectorscopeSettings, WaveformFilter, WaveformParadeSettings,
};
use klipp_engine::ffmpeg_scope::{FfmpegScope, FfmpegScopeBank, SCOPE_OUT_H, SCOPE_OUT_W};

/// Build a linear-light f32 RGBA frame where the left third is pure
/// red, the middle third is pure green, and the right third is pure
/// blue. Parade rendering should map these to three colour-coded
/// regions in the output.
fn make_rgb_band_frame(w: usize, h: usize) -> Vec<f32> {
    let mut buf = vec![0.0f32; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let idx = (y * w + x) * 4;
            let third = w / 3;
            if x < third {
                buf[idx] = 1.0; // R
            } else if x < 2 * third {
                buf[idx + 1] = 1.0; // G
            } else {
                buf[idx + 2] = 1.0; // B
            }
            buf[idx + 3] = 1.0; // A
        }
    }
    buf
}

fn default_settings() -> ScopeFfmpegSettings {
    ScopeFfmpegSettings {
        waveform: WaveformParadeSettings::default(),
        parade: WaveformParadeSettings::default(),
        vectorscope: VectorscopeSettings::default(),
        histogram: HistogramSettings::default(),
        quality: ScopeQuality::Medium,
    }
}

/// The output is RGBA u8 at SCOPE_OUT_W × SCOPE_OUT_H. Returns
/// (max_r, max_g, max_b) across the whole buffer — a quick way to
/// confirm we got a non-empty multi-channel image.
fn channel_maxima(rgba: &[u8]) -> (u8, u8, u8) {
    let (mut r, mut g, mut b) = (0u8, 0u8, 0u8);
    for px in rgba.chunks_exact(4) {
        r = r.max(px[0]);
        g = g.max(px[1]);
        b = b.max(px[2]);
    }
    (r, g, b)
}

fn channel_sums_in_region(rgba: &[u8], w: u32, h: u32, x_start: u32, x_end: u32) -> (u64, u64, u64) {
    let (mut r, mut g, mut b) = (0u64, 0u64, 0u64);
    let row_bytes = (w * 4) as usize;
    for y in 0..h as usize {
        let row = y * row_bytes;
        for x in x_start as usize..x_end as usize {
            let i = row + x * 4;
            r += rgba[i] as u64;
            g += rgba[i + 1] as u64;
            b += rgba[i + 2] as u64;
        }
    }
    (r, g, b)
}

#[test]
fn render_waveform_produces_output_of_expected_size() {
    let frame = make_rgb_band_frame(640, 360);
    let mut scope = FfmpegScope::new();
    let bytes = scope
        .render_linear_f32(0, &frame, 640, 360, default_settings())
        .expect("render ok")
        .expect("filter produced output");
    let expected_len = (SCOPE_OUT_W * SCOPE_OUT_H * 4) as usize;
    assert_eq!(bytes.len(), expected_len, "scope output is RGBA at {SCOPE_OUT_W}×{SCOPE_OUT_H}");
    let (rmax, gmax, bmax) = channel_maxima(&bytes);
    assert!(
        rmax > 0 && gmax > 0 && bmax > 0,
        "all three channels should have content; got R={rmax} G={gmax} B={bmax}",
    );
}

#[test]
fn render_parade_has_per_channel_coloring() {
    // The all-green-parade regression: if libavfilter picked a YUV
    // intermediate before the waveform filter ran, all three parade
    // sections came back green-tinted. With per-channel content in
    // the source, the R section should have visibly more red energy
    // than green/blue, the G section more green, etc.
    let frame = make_rgb_band_frame(640, 360);
    let mut scope = FfmpegScope::new();
    let bytes = scope
        .render_linear_f32(1, &frame, 640, 360, default_settings())
        .expect("render ok")
        .expect("filter produced output");
    // Parade with components=7 splits the output horizontally into
    // three equal sections: R | G | B. The 1024px-wide output gives
    // ~341 pixels per section.
    let third = SCOPE_OUT_W / 3;
    let (r_in_r, g_in_r, b_in_r) =
        channel_sums_in_region(&bytes, SCOPE_OUT_W, SCOPE_OUT_H, 0, third);
    let (r_in_g, g_in_g, b_in_g) =
        channel_sums_in_region(&bytes, SCOPE_OUT_W, SCOPE_OUT_H, third, 2 * third);
    let (r_in_b, g_in_b, b_in_b) =
        channel_sums_in_region(&bytes, SCOPE_OUT_W, SCOPE_OUT_H, 2 * third, SCOPE_OUT_W);
    // Red section should be predominantly red.
    assert!(
        r_in_r > g_in_r && r_in_r > b_in_r,
        "parade R section should be red-dominant; got R={r_in_r} G={g_in_r} B={b_in_r}",
    );
    // Green section predominantly green.
    assert!(
        g_in_g > r_in_g && g_in_g > b_in_g,
        "parade G section should be green-dominant; got R={r_in_g} G={g_in_g} B={b_in_g}",
    );
    // Blue section predominantly blue.
    assert!(
        b_in_b > r_in_b && b_in_b > g_in_b,
        "parade B section should be blue-dominant; got R={r_in_b} G={g_in_b} B={b_in_b}",
    );
}

#[test]
fn render_vectorscope_produces_output() {
    let frame = make_rgb_band_frame(640, 360);
    let mut scope = FfmpegScope::new();
    let bytes = scope
        .render_linear_f32(2, &frame, 640, 360, default_settings())
        .expect("render ok")
        .expect("filter produced output");
    let (rmax, gmax, bmax) = channel_maxima(&bytes);
    // Vectorscope output is colour-tinted; we just want a non-zero buffer.
    // Sum in u16 to avoid u8 overflow when all channels saturate.
    assert!(rmax as u16 + gmax as u16 + bmax as u16 > 0, "vectorscope output should not be all-zero");
}

#[test]
fn render_histogram_produces_output() {
    let frame = make_rgb_band_frame(640, 360);
    let mut scope = FfmpegScope::new();
    let bytes = scope
        .render_linear_f32(3, &frame, 640, 360, default_settings())
        .expect("render ok")
        .expect("filter produced output");
    let (rmax, gmax, bmax) = channel_maxima(&bytes);
    assert!(
        rmax > 0 && gmax > 0 && bmax > 0,
        "histogram should have all three channels lit; got R={rmax} G={gmax} B={bmax}",
    );
}

#[test]
fn flat_filter_works_from_scratch() {
    // Isolate whether filter=flat itself works, vs. the rebuild path.
    let frame = make_rgb_band_frame(640, 360);
    let mut scope = FfmpegScope::new();
    let mut s = default_settings();
    s.waveform.filter = WaveformFilter::Flat;
    let bytes = scope
        .render_linear_f32(0, &frame, 640, 360, s)
        .expect("render ok")
        .expect("filter produced output");
    assert!(!bytes.is_empty());
}

#[test]
fn filter_change_rebuilds_graph() {
    let frame = make_rgb_band_frame(640, 360);
    let mut scope = FfmpegScope::new();

    let mut s1 = default_settings();
    s1.waveform.filter = WaveformFilter::Lowpass;
    let lowpass = scope
        .render_linear_f32(0, &frame, 640, 360, s1)
        .expect("render ok")
        .expect("filter produced output");

    let mut s2 = default_settings();
    s2.waveform.filter = WaveformFilter::Flat;
    let flat = scope
        .render_linear_f32(0, &frame, 640, 360, s2)
        .expect("render ok")
        .expect("filter produced output");

    // The two filters paint very differently — lowpass smears columns,
    // flat is hard-edged. Demand the buffers not be byte-identical so
    // we know the settings comparison correctly rebuilt the graph.
    assert_ne!(lowpass, flat, "filter change should produce a different image");
}

#[test]
fn bank_keeps_modes_keyed_separately() {
    let frame = make_rgb_band_frame(640, 360);
    let mut bank = FfmpegScopeBank::new();

    let waveform = bank
        .render_linear(0, &frame, 640, 360, default_settings())
        .expect("render ok")
        .expect("filter produced output");
    let parade = bank
        .render_linear(1, &frame, 640, 360, default_settings())
        .expect("render ok")
        .expect("filter produced output");

    // Waveform=stack and parade=parade are visually distinct — stack
    // overlays the three channels; parade splits them horizontally.
    // The buffers should differ.
    assert_ne!(
        waveform, parade,
        "bank must keep separate graphs per mode — got identical output",
    );
}

#[test]
fn vectorscope_mode_change_alters_output() {
    let frame = make_rgb_band_frame(640, 360);
    let mut scope = FfmpegScope::new();

    let mut s1 = default_settings();
    s1.vectorscope.mode = VectorscopeMode::Color3;
    let m3 = scope
        .render_linear_f32(2, &frame, 640, 360, s1)
        .expect("render ok")
        .expect("filter produced output");

    let mut s2 = default_settings();
    s2.vectorscope.mode = VectorscopeMode::Gray;
    let gray = scope
        .render_linear_f32(2, &frame, 640, 360, s2)
        .expect("render ok")
        .expect("filter produced output");

    assert_ne!(m3, gray, "vectorscope mode change should produce a different image");
}

#[test]
fn histogram_levels_change_alters_output() {
    let frame = make_rgb_band_frame(640, 360);
    let mut scope = FfmpegScope::new();

    let mut s1 = default_settings();
    s1.histogram.levels = HistogramLevels::Linear;
    let linear = scope
        .render_linear_f32(3, &frame, 640, 360, s1)
        .expect("render ok")
        .expect("filter produced output");

    let mut s2 = default_settings();
    s2.histogram.levels = HistogramLevels::Logarithmic;
    let log = scope
        .render_linear_f32(3, &frame, 640, 360, s2)
        .expect("render ok")
        .expect("filter produced output");

    assert_ne!(
        linear, log,
        "histogram levels change (linear ↔ log) should produce a different image",
    );
}

#[test]
fn components_mask_drops_red_channel() {
    // With show_r=false, the parade R section should be dramatically
    // dimmer than with show_r=true. This catches a regression where
    // the components= bitmask was hardcoded instead of read from
    // settings.
    let frame = make_rgb_band_frame(640, 360);
    let mut scope = FfmpegScope::new();

    let mut s1 = default_settings();
    s1.parade.channels = ChannelMask { r: true, g: true, b: true };
    let all = scope
        .render_linear_f32(1, &frame, 640, 360, s1)
        .expect("render ok")
        .expect("filter produced output");

    let mut s2 = default_settings();
    s2.parade.channels = ChannelMask { r: false, g: true, b: true };
    let no_red = scope
        .render_linear_f32(1, &frame, 640, 360, s2)
        .expect("render ok")
        .expect("filter produced output");

    let third = SCOPE_OUT_W / 3;
    let (r_all, _, _) = channel_sums_in_region(&all, SCOPE_OUT_W, SCOPE_OUT_H, 0, third);
    let (r_no_red, _, _) =
        channel_sums_in_region(&no_red, SCOPE_OUT_W, SCOPE_OUT_H, 0, third);
    assert!(
        r_no_red < r_all,
        "dropping R from components should reduce red energy in the R section; got all={r_all}, no_red={r_no_red}",
    );
}

#[test]
fn histogram_display_mode_choice_changes_output() {
    let frame = make_rgb_band_frame(640, 360);
    let mut scope = FfmpegScope::new();

    let mut s1 = default_settings();
    s1.histogram.display_mode = HistogramDisplayMode::Parade;
    let parade = scope
        .render_linear_f32(3, &frame, 640, 360, s1)
        .expect("render ok")
        .expect("filter produced output");

    let mut s2 = default_settings();
    s2.histogram.display_mode = HistogramDisplayMode::Stack;
    let stack = scope
        .render_linear_f32(3, &frame, 640, 360, s2)
        .expect("render ok")
        .expect("filter produced output");

    assert_ne!(
        parade, stack,
        "histogram display_mode change should produce a different image",
    );
}
