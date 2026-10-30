//! Stats + JSON record helpers. No regression detection — early alpha,
//! we eyeball numbers.
//!
//! One thing this module *does* enforce: it refuses to present a number
//! as trustworthy when the samples say otherwise. The first harness run
//! after the GPU has been idle was measured 30-45% off in *both*
//! directions — one scenario reading high, another reading low — while
//! three back-to-back runs on a warmed GPU agreed to within 4%. A
//! harness whose noise floor exceeds the effect it is meant to detect is
//! worse than no harness, because the numbers still look confident.
//!
//! So each result carries a drift figure: the p50 of the first half of
//! the steady samples against the p50 of the second half. A settled
//! measurement has both halves agreeing; a run that is still ramping
//! shows the trend and is reported as UNSETTLED rather than PASS/FAIL.

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
    /// Sample standard deviation over the steady window.
    pub stddev_ms: f64,
    /// p50 of the first / second half of the steady window. Equal
    /// halves mean the measurement settled before it was taken.
    pub p50_first_half_ms: f64,
    pub p50_second_half_ms: f64,
    /// Second-half p50 relative to the first, in percent. Positive =
    /// getting slower through the run, negative = still speeding up.
    pub drift_pct: f64,
    /// False when `drift_pct` exceeds [`DRIFT_LIMIT_PCT`]. A false here
    /// invalidates `passed` — you cannot claim a budget verdict on a
    /// sample that was still moving.
    pub settled: bool,
    pub committed_at_utc: String,
    /// The process's GPU memory after the run, in MB, where a scenario
    /// reads it (#3073).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu_mem_mb: Option<f64>,
}

/// Half-to-half drift beyond which a run is not a measurement.
/// 5% sits above the ~4% run-to-run spread seen on a warm GPU and well
/// below the 30%+ seen on a cold one.
pub const DRIFT_LIMIT_PCT: f64 = 5.0;

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

    let stddev_ms = if steady.len() < 2 {
        0.0
    } else {
        let var = steady.iter().map(|x| (x - mean_ms).powi(2)).sum::<f64>()
            / (steady.len() as f64 - 1.0);
        var.sqrt()
    };

    // Half-to-half drift. Split in sample order (not sorted) so this
    // measures change over time, which is what "still warming up" looks
    // like — a sorted split would only re-derive the spread.
    let half = steady.len() / 2;
    let median_of = |xs: &[f64]| -> f64 {
        if xs.is_empty() {
            return 0.0;
        }
        let mut v = xs.to_vec();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        v[v.len() / 2]
    };
    let p50_first_half_ms = median_of(&steady[..half]);
    let p50_second_half_ms = median_of(&steady[half..]);
    let drift_pct = if p50_first_half_ms > 0.0 {
        (p50_second_half_ms - p50_first_half_ms) / p50_first_half_ms * 100.0
    } else {
        0.0
    };
    let settled = half >= 2 && drift_pct.abs() <= DRIFT_LIMIT_PCT;

    // An unsettled sample cannot support a verdict either way.
    let passed = settled && p99_ms <= budget_p99_ms;

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
        stddev_ms,
        p50_first_half_ms,
        p50_second_half_ms,
        drift_pct,
        settled,
        committed_at_utc: iso8601_now(),
        gpu_mem_mb: None,
    }
}

pub fn print_human(r: &ScenarioResult) {
    // UNSETTLED is not a third grade of pass — it means the sample does
    // not support a verdict, so it replaces one rather than decorating it.
    let verdict = if !r.settled {
        "UNSET"
    } else if r.passed {
        "PASS "
    } else {
        "FAIL "
    };
    println!(
        "{name:<40} {verdict}  p50={p50:6.2}ms  p99={p99:6.2}ms  sd={sd:5.2}ms  \
         drift={drift:+6.1}%  budget_p99={budget:.1}ms  @ {w}x{h}  n={n}",
        name = r.scenario,
        verdict = verdict,
        p50 = r.p50_ms,
        p99 = r.p99_ms,
        sd = r.stddev_ms,
        drift = r.drift_pct,
        budget = r.budget_p99_ms,
        w = r.width,
        h = r.height,
        n = r.steady_frames,
    );
    if r.steady_frames == 0 {
        // Skipped, not unstable — saying "still moving" about zero
        // samples reads as a measurement problem when it is an absence.
        println!("{:>46}└─ no samples: scenario did not run (see the note above)", "");
    } else if !r.settled {
        println!(
            "{:>46}└─ still moving: first-half p50 {:.2}ms vs second-half {:.2}ms \
             ({:+.1}%, limit ±{:.0}%). Re-run once the GPU is warm.",
            "", r.p50_first_half_ms, r.p50_second_half_ms, r.drift_pct, DRIFT_LIMIT_PCT,
        );
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn summarise_ms(samples: &[f64], budget: f64) -> ScenarioResult {
        summarise("t", 1920, 1080, samples, 0, budget)
    }

    #[test]
    fn a_steady_run_settles_and_takes_a_verdict() {
        // Tight samples either side of 8 ms.
        let s: Vec<f64> = (0..60).map(|i| 8.0 + if i % 2 == 0 { 0.05 } else { -0.05 }).collect();
        let r = summarise_ms(&s, 10.0);
        assert!(r.settled, "steady samples must settle (drift {:.2}%)", r.drift_pct);
        assert!(r.passed, "8 ms p99 under a 10 ms budget must pass");
        assert!(r.drift_pct.abs() < 1.0, "drift should be ~0, got {:.2}%", r.drift_pct);
    }

    #[test]
    fn a_run_that_is_still_speeding_up_is_not_a_measurement() {
        // What a warming GPU looks like: first half slow, second half fast.
        // This is the case that produced a confident-looking 30%-off number
        // before the drift check existed.
        let mut s = vec![12.0; 30];
        s.extend(vec![8.0; 30]);
        let r = summarise_ms(&s, 20.0);
        assert!(!r.settled, "a 33% ramp must not be reported as settled");
        assert!(r.drift_pct < -5.0, "drift should be strongly negative, got {:.2}%", r.drift_pct);
    }

    #[test]
    fn an_unsettled_run_cannot_pass_even_when_well_under_budget() {
        // The guarantee that matters. Every sample here is far inside the
        // budget, so the old code would have printed a confident PASS on a
        // sample that was still moving.
        let mut s = vec![4.0; 30];
        s.extend(vec![1.0; 30]);
        let r = summarise_ms(&s, 100.0);
        assert!(r.p99_ms < 100.0, "precondition: comfortably under budget");
        assert!(!r.settled);
        assert!(!r.passed, "an unsettled sample must not yield a PASS");
    }

    #[test]
    fn drift_is_positive_when_the_run_gets_slower() {
        let mut s = vec![8.0; 30];
        s.extend(vec![10.0; 30]);
        let r = summarise_ms(&s, 20.0);
        assert!(r.drift_pct > 5.0, "expected positive drift, got {:.2}%", r.drift_pct);
        assert!(!r.settled);
    }

    #[test]
    fn drift_uses_sample_order_not_sorted_order() {
        // Alternating high/low has a wide spread but no trend. Sorting
        // before splitting would invent a huge drift and reject a
        // perfectly usable measurement.
        let s: Vec<f64> = (0..60).map(|i| if i % 2 == 0 { 6.0 } else { 10.0 }).collect();
        let r = summarise_ms(&s, 20.0);
        assert!(r.settled, "noisy-but-trendless samples must still settle (drift {:.2}%)", r.drift_pct);
        assert!(r.stddev_ms > 1.0, "precondition: the spread is genuinely wide");
    }
}
