//! In-process probe of the viewer's GPU stack over a stretch of a project
//! (#1850): each frame drawn as the viewer draws it while playing, timed.
//!
//! ```text
//! cargo run --release -p klipp-render --example stack_probe -- <project dir> <from s> <to s> [passes]
//! ```
//!
//! The project is read in place and never written. Media frames are not
//! decoded (the source gives none), so the stretch should be one of titles
//! and generators.

use std::path::Path;
use std::time::Instant;

use klipp_compose::{DisplayDelivery, FrameSource, RenderOptions, Renderer, SourceFrame};
use klipp_core::Timecode;
use klipp_timeline::Clip;

struct NoMedia;

impl FrameSource for NoMedia {
    fn frame(&mut self, _: &Clip, _: f64, _: u32, _: u32) -> Option<SourceFrame> {
        None
    }
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = Path::new(&args[1]);
    let (from, to): (f64, f64) = (args[2].parse().unwrap(), args[3].parse().unwrap());
    let passes: usize = args.get(4).map_or(5, |p| p.parse().unwrap());
    let (sequences, _pool, colour, ..) = klipp_project::load_project(dir).expect("the project loads");
    let seq = klipp_timeline::flatten::flatten_sequence(&sequences[0], &sequences);
    let (w, h) = (seq.resolution.width, seq.resolution.height);
    let fps = seq.fps;
    let mut r = Renderer::new(klipp_plugin::OfxHost::new(), klipp_title::TitleRenderer::new());
    r.set_working_colorspace(colour.working_colorspace);
    let mut src = NoMedia;
    let first = (from * fps.as_f64()).round() as i64;
    let last = (to * fps.as_f64()).round() as i64;
    for pass in 0..passes {
        let mut totals = Vec::new();
        let mut walls = Vec::new();
        let mut effects = Vec::new();
        let mut composite = Vec::new();
        let mut declined = 0;
        for f in first..last {
            let time = Timecode::from_secs_f64(f as f64 / fps.as_f64(), fps.rate());
            let t = Instant::now();
            let out = DisplayDelivery { texture: None, bytes: false };
            match r.render_display_on_gpu(&seq, time, w, h, &mut src, RenderOptions::default(), out) {
                Some((_, stats)) => {
                    walls.push(t.elapsed().as_secs_f64() * 1e3);
                    totals.push(stats.total_ms);
                    effects.push(stats.effects_ms);
                    composite.push(stats.composite_ms);
                }
                None => declined += 1,
            }
        }
        if totals.is_empty() {
            println!("pass {pass}: every frame declined ({:?})", r.last_decline());
            continue;
        }
        println!(
            "pass {pass}: {} frames, {declined} declined | total {:.2} ms (wall {:.2}) effects {:.2} composite {:.2}",
            totals.len(),
            median(&mut totals),
            median(&mut walls),
            median(&mut effects),
            median(&mut composite),
        );
    }
}
