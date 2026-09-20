//! The render path held against a known chart.
//!
//! Every judgement the colour tools make — a cast, a skin hue, a ΔE to a
//! neighbouring shot — is made on pixels that came back out of this
//! crate. If the path itself moves colour, those judgements are partly
//! measurements of Klipp, and a grading loop will spend its rounds
//! chasing the pipeline instead of the picture. So before trusting a
//! reading, send a chart whose values are published and known
//! ([`klipp_color::chart`]) through the same renderer the viewer and the
//! export both use, and read it back.
//!
//! The chart enters as a decoder would hand it over: sRGB 8-bit decoded
//! to display-referred linear in the working space. It leaves as the
//! viewer encodes it for the screen, through
//! [`klipp_color::transfer::linear_to_srgb`]. In between is the real
//! `Renderer` — the same track stacking, the same grade chain, the same
//! working-space conversions, on the CPU or on the GPU as asked.
//!
//! What it cannot see: the window system's own colour management, and a
//! monitor profile. Those are outside the program.

use std::sync::Arc;

use klipp_core::{ClipId, MediaId, RationalTime, Timecode};
use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

use crate::render::{FrameSource, RenderOptions, Renderer, SourceFrame, SourcePixels};

/// Hands the chart to the renderer as a decoded frame.
struct ChartSource {
    linear: Arc<Vec<f32>>,
    width: u32,
    height: u32,
}

impl FrameSource for ChartSource {
    fn frame(&mut self, _clip: &Clip, _source_secs: f64, _width: u32, _height: u32) -> Option<SourceFrame> {
        Some(SourceFrame {
            pixels: SourcePixels::Linear(Arc::clone(&self.linear)),
            width: self.width,
            height: self.height,
            pts: None,
        })
    }
}

/// A one-clip, one-track timeline holding the chart, with no grade on
/// it: the identity through the whole composite.
fn chart_sequence(fps: f64, width: u32, height: u32) -> Sequence {
    let mut seq = Sequence::new_default();
    seq.resolution = klipp_core::Resolution { width, height };
    let rate = seq.fps.rate();
    let _ = fps;
    let mut clip = Clip::gap(RationalTime::from_secs_f64(0.0, rate), RationalTime::from_secs_f64(2.0, rate));
    clip.id = ClipId::new();
    clip.kind = ClipKind::Media { media_id: MediaId::new() };
    let mut track = Track::new("chart", TrackKind::Video);
    track.clips = vec![clip];
    seq.tracks = vec![track];
    seq
}

/// Send the chart through `renderer` and read it back.
///
/// `width` and `height` are the chart's size; 480×320 is plenty — the
/// patches are large and flat, and a bigger chart only costs time.
pub fn run(renderer: &mut Renderer, width: u32, height: u32, opts: RenderOptions) -> klipp_color::chart::ChartCheck {
    let srgb = klipp_color::chart::chart_rgba(width, height);
    // As a decoder hands a frame over: display-referred linear, straight
    // alpha, in the working space.
    let linear: Vec<f32> = srgb
        .chunks_exact(4)
        .flat_map(|px| {
            let c = klipp_color::transfer::srgb_u8_to_linear([px[0], px[1], px[2], px[3]]);
            [c[0], c[1], c[2], c[3]]
        })
        .collect();
    let seq = chart_sequence(24.0, width, height);
    let mut src = ChartSource { linear: Arc::new(linear), width, height };
    let out = renderer.render(&seq, Timecode::from_secs_f64(1.0, seq.fps.rate()), width, height, &mut src, opts);
    // As the viewer encodes for the screen.
    let back: Vec<u8> = out
        .chunks_exact(4)
        .flat_map(|px| klipp_color::transfer::linear_to_srgb_u8([px[0], px[1], px[2], px[3]]))
        .collect();
    klipp_color::chart::check(&back, width, height)
}

/// The self-check on both paths the app can render with, so a GPU that
/// disagrees with the CPU is caught as its own fault: the viewer shows
/// one of them and the export may take the other.
pub fn run_both(width: u32, height: u32, working: klipp_core::WorkingColorspace, opts: RenderOptions) -> [(bool, klipp_color::chart::ChartCheck); 2] {
    [false, true].map(|gpu| {
        let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
        r.set_gpu_enabled(gpu);
        r.set_working_colorspace(working);
        (gpu, run(&mut r, width, height, opts))
    })
}
