//! Resolve `GridInterval` choices to concrete seconds at the current
//! ruler zoom + sequence fps, and provide the constant-time
//! `nearest_grid_candidate` helper that the snap-resolution layer
//! consults as a post-pass.
//!
//! Lives in klipp-ui (not klipp-core) because `Adaptive` resolution
//! reuses `ruler::choose_major_interval`, which is UI-only by virtue
//! of its place in the rendering stack.

use klipp_core::GridInterval;

use super::ruler::choose_major_interval;

/// Resolve a `GridInterval` choice to a concrete grid spacing in
/// seconds at the current zoom + fps. Returns `None` when grid snap
/// should not contribute a candidate (interval = `Off`, or a derived
/// value would be non-positive).
///
/// `pps` is pixels-per-second of the current timeline zoom.
/// `fps_rate` is the active sequence fps used to derive `Frame`.
pub fn resolve_grid_interval_secs(
    interval: GridInterval,
    fps_rate: u32,
    pps: f32,
) -> Option<f64> {
    match interval {
        GridInterval::Off => None,
        GridInterval::Adaptive => {
            // Share the ruler's tick-density logic. choose_major_interval
            // returns the smallest interval where ticks are >= 80 px
            // apart, which is far wider than the snap threshold —
            // snap candidates can never be denser than visible ticks.
            let secs = choose_major_interval(pps);
            if secs > 0.0 { Some(secs) } else { None }
        }
        GridInterval::Frame => {
            if fps_rate == 0 {
                None
            } else {
                Some(1.0 / fps_rate as f64)
            }
        }
        GridInterval::HalfSecond => Some(0.5),
        GridInterval::Second => Some(1.0),
        GridInterval::TwoSeconds => Some(2.0),
        GridInterval::FiveSeconds => Some(5.0),
        GridInterval::Custom(secs) => {
            if secs > 0.0 { Some(secs) } else { None }
        }
    }
}

/// Constant-time grid-snap candidate. Returns the nearest grid point
/// to `t_secs` at the given `interval_secs` if it lies within
/// `threshold_secs`. Returns `None` otherwise.
///
/// Critical: this does **not** enumerate grid points. Per
/// audit-finding (l), the snap collector runs per paint per track —
/// any grid-point enumeration would compound that into an
/// O(timeline-length) cost for no good reason. Constant-time
/// arithmetic is the only acceptable shape.
pub fn nearest_grid_candidate(
    t_secs: f64,
    interval_secs: f64,
    threshold_secs: f64,
) -> Option<f64> {
    if interval_secs <= 0.0 {
        return None;
    }
    let nearest = (t_secs / interval_secs).round() * interval_secs;
    if (t_secs - nearest).abs() < threshold_secs {
        Some(nearest)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── resolve_grid_interval_secs ─────────────────────────────────

    #[test]
    fn off_resolves_to_none_for_back_compat() {
        // Pinned: GridInterval::Off MUST resolve to None at any fps /
        // pps. Snap callers branch on the Option to skip the grid
        // post-pass entirely. If this returns Some, every existing
        // user gets surprise grid snap on first launch.
        for &fps in &[24u32, 25, 30, 50, 60] {
            for &pps in &[10.0_f32, 50.0, 100.0, 500.0] {
                assert_eq!(
                    resolve_grid_interval_secs(GridInterval::Off, fps, pps),
                    None,
                    "Off must always resolve to None (fps={}, pps={})",
                    fps,
                    pps
                );
            }
        }
    }

    #[test]
    fn frame_respects_sequence_fps() {
        // Pinned: 24 fps -> 1/24 grid; 30 fps -> 1/30 grid; 60 fps -> 1/60.
        let cases = [(24u32, 1.0 / 24.0), (25, 1.0 / 25.0), (30, 1.0 / 30.0), (50, 1.0 / 50.0), (60, 1.0 / 60.0)];
        for (fps, expected) in cases {
            let secs = resolve_grid_interval_secs(GridInterval::Frame, fps, 100.0).expect("Frame yields Some");
            assert!((secs - expected).abs() < 1e-9, "fps={} -> expected {}, got {}", fps, expected, secs);
        }
    }

    #[test]
    fn frame_with_zero_fps_resolves_to_none() {
        // Defensive: degenerate fps must not produce a divide-by-zero
        // grid spacing (would freeze the snap loop or produce NaN).
        assert_eq!(resolve_grid_interval_secs(GridInterval::Frame, 0, 100.0), None);
    }

    #[test]
    fn fixed_intervals_resolve_to_stated_seconds() {
        assert_eq!(resolve_grid_interval_secs(GridInterval::HalfSecond, 30, 100.0), Some(0.5));
        assert_eq!(resolve_grid_interval_secs(GridInterval::Second, 30, 100.0), Some(1.0));
        assert_eq!(resolve_grid_interval_secs(GridInterval::TwoSeconds, 30, 100.0), Some(2.0));
        assert_eq!(resolve_grid_interval_secs(GridInterval::FiveSeconds, 30, 100.0), Some(5.0));
    }

    #[test]
    fn custom_honours_user_value() {
        assert_eq!(resolve_grid_interval_secs(GridInterval::Custom(0.25), 30, 100.0), Some(0.25));
        assert_eq!(resolve_grid_interval_secs(GridInterval::Custom(10.0), 30, 100.0), Some(10.0));
    }

    #[test]
    fn custom_rejects_non_positive_values() {
        // Defensive: 0 or negative custom values would produce a
        // divide-by-zero or negative grid spacing.
        assert_eq!(resolve_grid_interval_secs(GridInterval::Custom(0.0), 30, 100.0), None);
        assert_eq!(resolve_grid_interval_secs(GridInterval::Custom(-1.0), 30, 100.0), None);
    }

    #[test]
    fn adaptive_picks_coarser_interval_at_lower_zoom() {
        // Pinned: adaptive grid must auto-coarsen as the user zooms
        // out. At 5 px/sec the ruler can only fit ~16 sec between
        // labels (80 px / 5 px per sec), so the major interval must
        // be at least that wide. At 500 px/sec, it can be much
        // tighter.
        let dense = resolve_grid_interval_secs(GridInterval::Adaptive, 30, 500.0).expect("dense");
        let sparse = resolve_grid_interval_secs(GridInterval::Adaptive, 30, 5.0).expect("sparse");
        assert!(
            sparse > dense,
            "adaptive at low pps must coarsen: dense (500 pps) = {}, sparse (5 pps) = {}",
            dense,
            sparse
        );
    }

    #[test]
    fn adaptive_at_typical_zoom_picks_a_reasonable_interval() {
        // At 100 px/sec — a typical mid-zoom — major ticks are 80 px
        // apart, so the major interval is just under 1 second
        // (matches a CANDIDATES entry: 1.0 / 1.5x = ~0.83). Verify
        // we get a positive value in a sensible range.
        let secs = resolve_grid_interval_secs(GridInterval::Adaptive, 30, 100.0).expect("typical");
        assert!(secs > 0.0 && secs <= 5.0, "expected 0 < secs <= 5 at 100 pps, got {}", secs);
    }

    // ── nearest_grid_candidate ─────────────────────────────────────

    #[test]
    fn nearest_grid_returns_none_when_outside_threshold() {
        // Pinned: grid post-pass must respect the existing snap
        // threshold (audit-finding (l): user-visible behaviour
        // identical to clip-edge snap at the threshold boundary).
        // 0.7s is 0.2 from grid points {0.5, 1.0}; with a 0.1
        // threshold, neither qualifies.
        assert_eq!(nearest_grid_candidate(0.7, 0.5, 0.1), None);
    }

    #[test]
    fn nearest_grid_picks_closer_neighbour() {
        // 0.4 is closer to 0.5 than to 0.0; 0.51 also rounds to 0.5;
        // 0.74 rounds to 0.5 (with 1.0s grid this would round to 1.0,
        // but with 0.5s grid the nearest is still 0.5 because
        // 0.74 is 0.24 from 0.5 and 0.26 from 1.0).
        assert!((nearest_grid_candidate(0.4, 0.5, 0.2).expect("hit") - 0.5).abs() < 1e-9);
        assert!((nearest_grid_candidate(0.51, 0.5, 0.2).expect("hit") - 0.5).abs() < 1e-9);
    }

    #[test]
    fn nearest_grid_respects_zero_or_negative_interval() {
        // Defensive: degenerate interval must produce None, not
        // divide-by-zero or infinity.
        assert_eq!(nearest_grid_candidate(1.0, 0.0, 0.5), None);
        assert_eq!(nearest_grid_candidate(1.0, -0.5, 0.5), None);
    }

    #[test]
    fn nearest_grid_handles_negative_times() {
        // Snap shouldn't break on negative time values (e.g. when a
        // drag overshoots before t=0). Math should produce the
        // nearest negative grid point.
        let nearest = nearest_grid_candidate(-0.4, 0.5, 0.2).expect("hit");
        assert!((nearest - (-0.5)).abs() < 1e-9, "expected -0.5, got {}", nearest);
    }

    #[test]
    fn nearest_grid_is_constant_time_no_enumeration() {
        // Pinned: the grid post-pass must NOT enumerate candidates
        // across the timeline. We verify this by snapping at very
        // large t values — if the impl ever enumerated, this test
        // would either run for hours or stack-overflow. A direct
        // arithmetic impl runs in microseconds regardless of t.
        // (Audit-finding (l): unbounded enumeration would compound
        // 60 Hz × N tracks × O(timeline-length).)
        let start = std::time::Instant::now();
        for &t in &[1.0, 1_000.0, 1_000_000.0, 1_000_000_000.0] {
            let _ = nearest_grid_candidate(t, 1.0, 0.1);
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() < 10,
            "constant-time pin: 4 lookups must run in << 10ms even at huge t (took {:?})",
            elapsed
        );
    }
}
