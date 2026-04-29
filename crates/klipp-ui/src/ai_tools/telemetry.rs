//! Per-model usage telemetry — last-used timestamp + rolling
//! inference-time average. Lives on `AiToolsState`, read by the
//! AI Models dialog to show "Last used: 2m ago • avg 142ms".
//!
//! Ephemeral by design: state lives in memory only, resets on app
//! restart. Persisting across sessions is filed under the broader
//! plugin-manager work — once that lands, telemetry can ride along
//! with per-model settings.

use std::collections::VecDeque;
use std::time::SystemTime;

/// Cap on the rolling window so a noisy ML run can't spike the
/// average forever — old values fall off after WINDOW_SIZE entries.
const WINDOW_SIZE: usize = 10;

#[derive(Debug, Clone)]
pub struct ModelTelemetry {
    pub last_used: SystemTime,
    pub recent_inference_ms: VecDeque<u64>,
    pub total_runs: u64,
}

impl Default for ModelTelemetry {
    fn default() -> Self {
        Self {
            last_used: SystemTime::UNIX_EPOCH,
            recent_inference_ms: VecDeque::with_capacity(WINDOW_SIZE),
            total_runs: 0,
        }
    }
}

impl ModelTelemetry {
    /// Record one successful inference. Called on every
    /// `InferenceState::Idle → Done` transition the dispatcher
    /// observes.
    pub fn record_run(&mut self, inference_ms: u64) {
        self.last_used = SystemTime::now();
        if self.recent_inference_ms.len() >= WINDOW_SIZE {
            self.recent_inference_ms.pop_front();
        }
        self.recent_inference_ms.push_back(inference_ms);
        self.total_runs = self.total_runs.saturating_add(1);
    }

    /// Average over the rolling window. `None` if the model hasn't
    /// been run yet (the dialog renders "—" in that case).
    pub fn avg_inference_ms(&self) -> Option<f64> {
        if self.recent_inference_ms.is_empty() {
            return None;
        }
        let sum: u64 = self.recent_inference_ms.iter().sum();
        Some(sum as f64 / self.recent_inference_ms.len() as f64)
    }

    /// `true` once the model has been run at least once. The dialog
    /// uses this to decide between "—" and a real timestamp.
    pub fn has_runs(&self) -> bool {
        self.total_runs > 0
    }
}

/// Format a duration since `last_used` for the dialog. Intentionally
/// approximate — "2m ago" reads better than "117 seconds ago" and
/// the precise timestamp isn't actionable.
pub fn format_relative(then: SystemTime) -> String {
    let now = SystemTime::now();
    let secs = now
        .duration_since(then)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if secs < 5 {
        "just now".into()
    } else if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_has_no_runs() {
        let t = ModelTelemetry::default();
        assert!(!t.has_runs());
        assert_eq!(t.avg_inference_ms(), None);
        assert_eq!(t.total_runs, 0);
    }

    #[test]
    fn record_run_updates_state() {
        let mut t = ModelTelemetry::default();
        t.record_run(100);
        assert!(t.has_runs());
        assert_eq!(t.total_runs, 1);
        assert_eq!(t.avg_inference_ms(), Some(100.0));
    }

    #[test]
    fn rolling_window_caps_at_ten() {
        let mut t = ModelTelemetry::default();
        // 11 runs at 100ms each, last one at 1000 — window drops the
        // first 100 so avg stays high.
        for _ in 0..10 {
            t.record_run(100);
        }
        t.record_run(1000);
        assert_eq!(t.recent_inference_ms.len(), WINDOW_SIZE);
        // 9 × 100 + 1000 = 1900 / 10 = 190
        assert_eq!(t.avg_inference_ms(), Some(190.0));
        // total_runs counts all 11 even though only 10 are kept.
        assert_eq!(t.total_runs, 11);
    }

    #[test]
    fn avg_handles_mixed_timings() {
        let mut t = ModelTelemetry::default();
        t.record_run(50);
        t.record_run(150);
        assert_eq!(t.avg_inference_ms(), Some(100.0));
    }

    #[test]
    fn format_relative_just_now_under_five_seconds() {
        // Fresh "now" — should fall in the "just now" bucket.
        assert_eq!(format_relative(SystemTime::now()), "just now");
    }

    #[test]
    fn format_relative_minutes() {
        let then = SystemTime::now() - std::time::Duration::from_secs(125);
        assert_eq!(format_relative(then), "2m ago");
    }

    #[test]
    fn format_relative_hours() {
        let then = SystemTime::now() - std::time::Duration::from_secs(7200);
        assert_eq!(format_relative(then), "2h ago");
    }
}
