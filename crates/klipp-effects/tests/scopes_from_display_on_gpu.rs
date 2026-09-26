//! The scopes measured from the display encode's buffer, where it is on
//! the device, draw what they draw from the same bytes read back and
//! uploaded again. Playing, that round trip was a 33 MB readback every
//! scope pass at 4K, 24 ms each (#333). Skips without a GPU.

use klipp_effects::gpu::GpuEffectsPipeline;
use klipp_effects::gpu_scopes::{HistogramJob, HistogramLayout, ScopeInput, ScopeJob, VectorscopeJob, WaveformJob, WaveformMode};

const REC709: [f32; 3] = [0.2126, 0.7152, 0.0722];

fn jobs() -> Vec<ScopeJob> {
    let wave = |mode| ScopeJob::Waveform(WaveformJob { mode, intensity: -1.0, channels: 7, envelope: true, glow: false, out_w: 512, out_h: 256 });
    vec![
        wave(WaveformMode::RgbOverlay),
        wave(WaveformMode::Parade),
        ScopeJob::Vectorscope(VectorscopeJob { zoom: 1.0, intensity: -1.0, colorize: true, luma_lo: 0.0, luma_hi: 1.0, glow: false, out_w: 256, out_h: 256 }),
        ScopeJob::Histogram(HistogramJob { layout: HistogramLayout::Overlay, log: false, fill: 0.6, glow: false, out_w: 512, out_h: 256 }),
    ]
}

/// Brightness across, hue down, so every scope has a shape to draw.
fn picture(w: u32, h: u32) -> Vec<f32> {
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let t = x as f32 / (w - 1) as f32;
            let u = y as f32 / (h - 1) as f32;
            v.extend_from_slice(&[t * (0.3 + 0.7 * u), t * t, t * (1.0 - 0.6 * u), 1.0]);
        }
    }
    v
}

/// Both ways at `step`; the worst per-byte difference over every scope,
/// or `None` without a GPU.
fn both_ways(w: u32, h: u32, step: u32) -> Option<u8> {
    let mut gpu = GpuEffectsPipeline::new()?;
    gpu.ensure_chain_buffers(w as u64 * h as u64 * 16);
    gpu.upload_chain_input_linear(&picture(w, h));
    let bytes = gpu.encode_chain_to_display(true, w, h, None, false, None, None, true)?.expect("the bytes were asked for");
    let jobs = jobs();
    let read_back = gpu.scopes().render(&ScopeInput::Display { rgba: &bytes, width: w, height: h, luma: REC709 }, step, &jobs);
    let in_place = gpu.render_scopes_from_display(w, h, REC709, step, &jobs).expect("the encode left a frame");
    let mut worst = 0u8;
    for (i, (a, b)) in read_back.iter().zip(&in_place).enumerate() {
        let (a, b) = (a.as_ref().expect("drawn"), b.as_ref().expect("drawn"));
        assert_eq!(a.len(), b.len(), "scope {i}: sizes differ");
        assert!(b.iter().any(|&v| v > 0), "scope {i} drew nothing from the GPU input");
        worst = worst.max(a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0));
    }
    Some(worst)
}

#[test]
fn every_pixel_measured_the_two_are_the_same_picture() {
    let Some(worst) = both_ways(640, 360, 1) else {
        eprintln!("no GPU; skipping");
        return;
    };
    assert_eq!(worst, 0, "the scopes from the GPU input differ from the read-back ones");
}

/// At 4K while playing the scopes sample every second pixel: the read-
/// back path picks those pixels on the CPU, the GPU input has the
/// shaders step over the others. The same samples either way.
#[test]
fn sampled_while_playing_the_two_measure_the_same_pixels() {
    let Some(worst) = both_ways(3840, 2160, 2) else {
        eprintln!("no GPU; skipping");
        return;
    };
    eprintln!("4K at step 2: worst difference {worst} levels");
    assert_eq!(worst, 0, "step 2: the GPU input's scopes differ by {worst} levels");
}
