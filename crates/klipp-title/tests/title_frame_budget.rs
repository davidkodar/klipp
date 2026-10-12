//! #1725: each heavy title case plays at 4K within a 30 fps frame
//! budget. Timing, so it runs on request in a release build on the
//! machine it is about:
//! `cargo test --release -p klipp-title --test title_frame_budget -- --ignored --nocapture`

use std::time::Instant;

use klipp_title::TitleRenderer;

#[path = "common/perf_cases.rs"]
mod perf_cases;

#[test]
#[ignore = "timing: run in release on the machine being measured"]
fn every_heavy_title_plays_at_4k_within_the_frame_budget() {
    let budget_ms = 1000.0 / 30.0;
    let mut over = Vec::new();
    for (name, td) in perf_cases::all() {
        let mut r = TitleRenderer::new();
        let mut times: Vec<f64> = (0..120)
            .map(|f| {
                let t = f as f32 * perf_cases::SECS / 120.0;
                let start = Instant::now();
                let _ = r.frame_with_window(&td, 3840, 2160, None, t, Some(perf_cases::SECS));
                start.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        times.sort_by(f64::total_cmp);
        let (median, p95) = (times[60], times[114]);
        println!("{name:32} median {median:6.1} ms  p95 {p95:6.1} ms");
        if p95 > budget_ms {
            over.push(format!("{name}: p95 {p95:.1} ms"));
        }
    }
    assert!(over.is_empty(), "over the {budget_ms:.1} ms budget at 4K: {over:?}");
}
