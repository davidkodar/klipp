//! How long heavy titles take a frame (#1725): a turning 3D text, a
//! 200-letter animator with blur, and a blended, masked multi-layer
//! title, at 1080p and 4K, over four seconds at 30 frames a second,
//! through `frame_with_window` as the compositor asks for them.
//!
//! `cargo run --release -p klipp-title --example title_perf`

use std::time::Instant;

use klipp_title::{TitleFrame, TitleRenderer};

#[path = "../tests/common/perf_cases.rs"]
mod perf_cases;
use perf_cases::SECS;
const FPS: f32 = 30.0;

fn main() {
    let only = std::env::var("CASES").unwrap_or_default();
    let all = perf_cases::all();
    let cases: Vec<_> = all.into_iter().filter(|(n, _)| only.is_empty() || only.split(',').any(|o| n.contains(o))).collect();
    let budget_ms = 1000.0 / FPS as f64;
    // TIMES=0.5,1.0: each case's frame at those times alone, fresh.
    if let Ok(times) = std::env::var("TIMES") {
        for (name, td) in &cases {
            for t in times.split(',').filter_map(|t| t.parse::<f32>().ok()) {
                let mut r = TitleRenderer::new();
                let start = Instant::now();
                let _ = r.frame_with_window(td, 1920, 1080, None, t, Some(SECS));
                println!("{name:30} t={t:5.2}  {:8.1} ms", start.elapsed().as_secs_f64() * 1000.0);
            }
        }
        return;
    }
    println!("frame budget at {FPS} fps: {budget_ms:.1} ms");
    for (w, h) in [(1920u32, 1080u32), (3840, 2160)] {
        for (name, td) in &cases {
            let mut r = TitleRenderer::new();
            let mut times = Vec::new();
            let mut kinds = [0usize; 4];
            let frames = std::env::var("FRAMES").ok().and_then(|f| f.parse().ok()).unwrap_or((SECS * FPS) as usize);
            for f in 0..frames {
                let t = f as f32 * SECS / frames as f32;
                let start = Instant::now();
                let frame = r.frame_with_window(td, w, h, None, t, Some(SECS));
                times.push(start.elapsed().as_secs_f64() * 1000.0);
                kinds[match frame {
                    TitleFrame::Placed { .. } => 0,
                    TitleFrame::Glyphs { .. } => 1,
                    TitleFrame::Rendered { .. } => 2,
                    TitleFrame::Layers { .. } => 3,
                }] += 1;
            }
            times.sort_by(f64::total_cmp);
            let (median, p95, max) = (times[times.len() / 2], times[times.len() * 95 / 100], times[times.len() - 1]);
            println!(
                "{w}x{h}  {name:30}  median {median:7.1} ms  p95 {p95:7.1} ms  max {max:7.1} ms  {} placed / {} glyphs / {} rendered / {} layers  {}",
                kinds[0], kinds[1], kinds[2], kinds[3], if p95 <= budget_ms { "within budget" } else { "OVER budget" }
            );
        }
    }
}
