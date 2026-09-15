//! Draw the GPU scopes for a real picture and write them out to look at.
//!
//! Ignored: needs a GPU and a picture. Run with
//! `KLIPP_SCOPE_LOOK_IN=frame.png KLIPP_SCOPE_LOOK_OUT=dir cargo test -p
//! klipp-effects --test gpu_scopes_look -- --ignored --nocapture`. Prints
//! the time of each render, measured at the picture's own size.

use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_effects::gpu_scopes::{HistogramJob, HistogramLayout, ScopeInput, ScopeJob, ScopeTransfer, VectorscopeJob, WaveformJob, WaveformMode};
use std::time::Instant;

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

#[test]
#[ignore]
fn draw_the_scopes_of_a_picture() {
    let (Ok(input), Ok(out_dir)) = (std::env::var("KLIPP_SCOPE_LOOK_IN"), std::env::var("KLIPP_SCOPE_LOOK_OUT")) else { return };
    let Some(mut gpu) = GpuEffectsPipeline::new() else { return };
    let img = image::open(&input).expect("a readable picture").to_rgba8();
    let (w, h) = img.dimensions();
    let bytes = img.into_raw();
    let linear: Vec<f32> = bytes.iter().enumerate().map(|(i, b)| if i % 4 == 3 { 1.0 } else { srgb_to_linear(*b as f32 / 255.0) }).collect();
    let luma = [0.2126, 0.7152, 0.0722];
    let (out_w, out_h) = (1024u32, 512u32);
    let jobs = [
        ("waveform_rgb", WaveformMode::RgbOverlay),
        ("waveform_luma", WaveformMode::Luma),
        ("parade", WaveformMode::Parade),
    ];
    for step in [1u32, 2] {
        for (name, mode) in jobs {
            for glow in [false, true] {
                let job = ScopeJob::Waveform(WaveformJob { mode, intensity: -1.0, channels: 7, envelope: false, glow, out_w, out_h });
                let scope = ScopeInput::Linear { rgba: &linear, width: w, height: h, matrix: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], luma, transfer: ScopeTransfer::Srgb, scale: 1.0 };
                // Warm once, then time.
                let _ = gpu.scopes().render(&scope, step, &[job]);
                let t = Instant::now();
                let out = gpu.scopes().render(&scope, step, &[job]);
                let ms = t.elapsed().as_secs_f64() * 1000.0;
                let rgba = out[0].as_ref().expect("rendered");
                let file = format!("{out_dir}/{name}_step{step}{}.png", if glow { "_glow" } else { "" });
                image::save_buffer(&file, rgba, out_w, out_h, image::ColorType::Rgba8).unwrap();
                println!("{name} step {step} glow {glow}: {ms:.2} ms for {w}x{h} → {file}");
            }
        }
        // Both waveform and parade from one measurement, as the engine asks.
        let scope = ScopeInput::Linear { rgba: &linear, width: w, height: h, matrix: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], luma, transfer: ScopeTransfer::Srgb, scale: 1.0 };
        let all = [
            ScopeJob::Waveform(WaveformJob { mode: WaveformMode::RgbOverlay, intensity: -1.0, channels: 7, envelope: false, glow: true, out_w, out_h }),
            ScopeJob::Waveform(WaveformJob { mode: WaveformMode::Parade, intensity: -1.0, channels: 7, envelope: false, glow: true, out_w, out_h }),
            ScopeJob::Vectorscope(VectorscopeJob { zoom: 1.0, intensity: -1.0, colorize: true, luma_lo: 0.0, luma_hi: 1.0, glow: true, out_w: 1024, out_h: 1024 }),
            ScopeJob::Vectorscope(VectorscopeJob { zoom: 2.0, intensity: -1.0, colorize: true, luma_lo: 0.0, luma_hi: 1.0, glow: true, out_w: 1024, out_h: 1024 }),
            ScopeJob::Vectorscope(VectorscopeJob { zoom: 1.0, intensity: -1.0, colorize: false, luma_lo: 0.0, luma_hi: 1.0, glow: true, out_w: 1024, out_h: 1024 }),
            ScopeJob::Histogram(HistogramJob { layout: HistogramLayout::Overlay, log: false, fill: 0.7, glow: true, out_w, out_h }),
            ScopeJob::Histogram(HistogramJob { layout: HistogramLayout::Parade, log: false, fill: 0.7, glow: true, out_w, out_h }),
            ScopeJob::Histogram(HistogramJob { layout: HistogramLayout::Stack, log: false, fill: 0.7, glow: true, out_w, out_h }),
            ScopeJob::Histogram(HistogramJob { layout: HistogramLayout::Luma, log: false, fill: 0.7, glow: true, out_w, out_h }),
            ScopeJob::Histogram(HistogramJob { layout: HistogramLayout::Overlay, log: true, fill: 0.7, glow: true, out_w, out_h }),
        ];
        let t = Instant::now();
        let out = gpu.scopes().render(&scope, step, &all);
        println!("waveform + parade + vectorscope + histogram together, step {step}: {:.2} ms ({} results)", t.elapsed().as_secs_f64() * 1000.0, out.iter().filter(|o| o.is_some()).count());
        for (name, o) in [("vectorscope", &out[2]), ("vectorscope_zoom2", &out[3]), ("vectorscope_mono", &out[4])] {
            let file = format!("{out_dir}/{name}_step{step}.png");
            image::save_buffer(&file, o.as_ref().unwrap(), 1024, 1024, image::ColorType::Rgba8).unwrap();
        }
        for (name, o) in [("histogram_overlay", &out[5]), ("histogram_parade", &out[6]), ("histogram_stack", &out[7]), ("histogram_luma", &out[8]), ("histogram_overlay_log", &out[9])] {
            let file = format!("{out_dir}/{name}_step{step}.png");
            image::save_buffer(&file, o.as_ref().unwrap(), out_w, out_h, image::ColorType::Rgba8).unwrap();
        }
        // The histogram alone, as the dock asks for it.
        let hist = [ScopeJob::Histogram(HistogramJob { layout: HistogramLayout::Overlay, log: false, fill: 0.7, glow: true, out_w, out_h })];
        let _ = gpu.scopes().render(&scope, step, &hist);
        let t = Instant::now();
        let _ = gpu.scopes().render(&scope, step, &hist);
        println!("histogram alone, step {step}: {:.2} ms", t.elapsed().as_secs_f64() * 1000.0);
    }
    // The display-bytes path, as ACES projects and skims use it.
    let scope = ScopeInput::Display { rgba: &bytes, width: w, height: h, luma };
    let job = ScopeJob::Waveform(WaveformJob { mode: WaveformMode::RgbOverlay, intensity: -1.0, channels: 7, envelope: false, glow: true, out_w, out_h });
    let _ = gpu.scopes().render(&scope, 1, &[job]);
    let t = Instant::now();
    let _ = gpu.scopes().render(&scope, 1, &[job]);
    println!("display bytes, step 1: {:.2} ms", t.elapsed().as_secs_f64() * 1000.0);

    // Drawn at a pane's size, beside the fixed 1024x512 image stretched to
    // that size as the UI used to show it.
    let (pw, ph) = (1750u32, 875u32);
    let scope = ScopeInput::Linear { rgba: &linear, width: w, height: h, matrix: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], luma, transfer: ScopeTransfer::Srgb, scale: 1.0 };
    for (name, mode) in [("waveform_rgb", WaveformMode::RgbOverlay), ("parade", WaveformMode::Parade)] {
        let native = ScopeJob::Waveform(WaveformJob { mode, intensity: -1.0, channels: 7, envelope: false, glow: true, out_w: pw, out_h: ph });
        let fixed = ScopeJob::Waveform(WaveformJob { mode, intensity: -1.0, channels: 7, envelope: false, glow: true, out_w: 1024, out_h: 512 });
        let _ = gpu.scopes().render(&scope, 2, &[native]);
        let t = Instant::now();
        let out = gpu.scopes().render(&scope, 2, &[native]);
        println!("{name} at {pw}x{ph}, step 2: {:.2} ms", t.elapsed().as_secs_f64() * 1000.0);
        image::save_buffer(format!("{out_dir}/{name}_native_{pw}x{ph}.png"), out[0].as_ref().unwrap(), pw, ph, image::ColorType::Rgba8).unwrap();
        let out = gpu.scopes().render(&scope, 2, &[fixed]);
        let small = image::RgbaImage::from_raw(1024, 512, out[0].clone().unwrap()).unwrap();
        let stretched = image::imageops::resize(&small, pw, ph, image::imageops::FilterType::Triangle);
        stretched.save(format!("{out_dir}/{name}_stretched_{pw}x{ph}.png")).unwrap();
    }
    let big = ScopeJob::Vectorscope(VectorscopeJob { zoom: 1.0, intensity: -1.0, colorize: true, luma_lo: 0.0, luma_hi: 1.0, glow: true, out_w: 2000, out_h: 2000 });
    let _ = gpu.scopes().render(&scope, 2, &[big]);
    let t = Instant::now();
    let _ = gpu.scopes().render(&scope, 2, &[big]);
    println!("vectorscope at 2000x2000, step 2: {:.2} ms", t.elapsed().as_secs_f64() * 1000.0);

    // A waveform at the pane size in KLIPP_SCOPE_LOOK_PANE (`WxH`, physical
    // pixels), at full and medium quality: the whole image, a 1:1 crop, and
    // the same crop scaled as a screenshot of a 4K screen at half size is.
    if let Ok(pane) = std::env::var("KLIPP_SCOPE_LOOK_PANE") {
        let (pw, ph) = pane.split_once('x').map(|(a, b)| (a.parse::<u32>().unwrap(), b.parse::<u32>().unwrap())).expect("WxH");
        for step in [1u32, 2] {
            let job = ScopeJob::Waveform(WaveformJob { mode: WaveformMode::RgbOverlay, intensity: -1.0, channels: 7, envelope: false, glow: true, out_w: pw, out_h: ph });
            let _ = gpu.scopes().render(&scope, step, &[job]);
            let t = Instant::now();
            let out = gpu.scopes().render(&scope, step, &[job]);
            println!("pane {pw}x{ph} step {step}: {:.2} ms", t.elapsed().as_secs_f64() * 1000.0);
            let img = image::RgbaImage::from_raw(pw, ph, out[0].clone().unwrap()).unwrap();
            img.save(format!("{out_dir}/pane_{pw}x{ph}_step{step}.png")).unwrap();
            let (cx, cy) = ((pw * 29 / 100).min(pw.saturating_sub(1000)), (ph * 26 / 100).min(ph.saturating_sub(576)));
            image::imageops::crop_imm(&img, cx, cy, 520.min(pw), 300.min(ph)).to_image().save(format!("{out_dir}/pane_crop1to1_step{step}.png")).unwrap();
            let wide = image::imageops::crop_imm(&img, cx, cy, 998.min(pw), 576.min(ph)).to_image();
            image::imageops::resize(&wide, 520, 300, image::imageops::FilterType::Triangle).save(format!("{out_dir}/pane_as_screenshot_step{step}.png")).unwrap();
        }
    }
}
