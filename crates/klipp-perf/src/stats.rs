//! Stats + JSON record helpers. No regression detection — early alpha,
//! we eyeball numbers.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct ScenarioResult {
    pub scenario: String,
    pub width: u32,
    pub height: u32,
    pub frames_total: usize,
    pub warmup_frames: usize,
    pub steady_frames: usize,
    pub min_ms: f64,
    pub p50_ms: f64,
    pub p99_ms: f64,
    pub mean_ms: f64,
    pub max_ms: f64,
    pub budget_p99_ms: f64,
    pub passed: bool,
    pub committed_at_utc: String,
}

/// Compute percentile stats over steady-state samples (warmup dropped).
pub fn summarise(
    scenario: &str,
    width: u32,
    height: u32,
    all_samples_ms: &[f64],
    warmup: usize,
    budget_p99_ms: f64,
) -> ScenarioResult {
    let steady: Vec<f64> = all_samples_ms.iter().skip(warmup).copied().collect();
    let mut sorted = steady.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let pick = |q: f64| -> f64 {
        if sorted.is_empty() {
            0.0
        } else {
            let idx = ((sorted.len() as f64 - 1.0) * q).round() as usize;
            sorted[idx.min(sorted.len() - 1)]
        }
    };

    let min_ms = sorted.first().copied().unwrap_or(0.0);
    let max_ms = sorted.last().copied().unwrap_or(0.0);
    let p50_ms = pick(0.50);
    let p99_ms = pick(0.99);
    let mean_ms = if steady.is_empty() {
        0.0
    } else {
        steady.iter().sum::<f64>() / steady.len() as f64
    };
    let passed = p99_ms <= budget_p99_ms;

    ScenarioResult {
        scenario: scenario.to_string(),
        width,
        height,
        frames_total: all_samples_ms.len(),
        warmup_frames: warmup,
        steady_frames: steady.len(),
        min_ms,
        p50_ms,
        p99_ms,
        mean_ms,
        max_ms,
        budget_p99_ms,
        passed,
        committed_at_utc: iso8601_now(),
    }
}

pub fn print_human(r: &ScenarioResult) {
    let verdict = if r.passed { "PASS" } else { "FAIL" };
    println!(
        "{name:<40} {verdict}  p50={p50:6.2}ms  p99={p99:6.2}ms  max={max:6.2}ms  \
         mean={mean:6.2}ms  budget_p99={budget:.1}ms  @ {w}x{h}  n={n}",
        name = r.scenario,
        verdict = verdict,
        p50 = r.p50_ms,
        p99 = r.p99_ms,
        max = r.max_ms,
        mean = r.mean_ms,
        budget = r.budget_p99_ms,
        w = r.width,
        h = r.height,
        n = r.steady_frames,
    );
}

fn iso8601_now() -> String {
    // Avoid pulling chrono — use SystemTime → u64 epoch seconds → manual ISO.
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Minimal ISO formatting — good enough for a filename/audit field.
    // We don't need sub-second precision here.
    let days = secs / 86400;
    let remainder = secs % 86400;
    let h = remainder / 3600;
    let m = (remainder % 3600) / 60;
    let s = remainder % 60;
    // Approximate year/month/day from Unix epoch. Precise conversion
    // would require a date library; for local telemetry the epoch
    // offset is plenty.
    format!("epoch-day-{days} {h:02}:{m:02}:{s:02}Z")
}
