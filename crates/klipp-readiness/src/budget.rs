//! The frame-time budget every family that moves off the CPU owes
//! ([`crate::Obligation::FrameBudget`]), shared by them all: the viewer's
//! frame time with the family's node against the same scene without it,
//! within the family's budget, and no layer drawn on the CPU but the ones
//! it declares.
//!
//! Only the gate (`m4_readiness`) runs it: a time measured beside other
//! builds is noisy, so a budget is a ratio of the scene without the node,
//! measured in the same session, with a floor of slack for a frame too
//! quick to time.

use std::sync::Arc;
use std::time::Instant;

use crate::route::{Scene, Session, Source};
use crate::Route;

/// What a family may cost: its frame time at most `ratio` × the frame time
/// of the same scene without its node, plus `slack_ms`; and at most
/// `cpu_layers` layers drawn on the CPU (a declared local CPU segment, its
/// step cited where the family sets it).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Budget {
    pub ratio: f64,
    pub slack_ms: f64,
    pub cpu_layers: u32,
}

impl Budget {
    /// Twice the bare frame and a millisecond, nothing on the CPU: a node
    /// that runs on the GPU with the rest of the frame.
    pub const GPU: Budget = Budget { ratio: 2.0, slack_ms: 1.0, cpu_layers: 0 };
}

/// What a budget run measured: the median frame time with the node and
/// without it, the most layers one frame drew on the CPU, how noisy the
/// run was (the wider of the two sides' spread, the middle half of its
/// times over its median), and whether it took a second try.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Measured {
    pub with_ms: f64,
    pub without_ms: f64,
    pub cpu_layers: u32,
    pub noise: f64,
    pub retried: bool,
}

/// The fewest frames timed on each side.
pub const MIN_FRAMES: usize = 15;

/// A run noisier than this (its middle half wider than half its median)
/// gets one more try before it fails.
const NOISY: f64 = 0.5;

/// The median of `times`.
fn median(mut times: Vec<f64>) -> f64 {
    times.sort_by(|a, b| a.total_cmp(b));
    times.get(times.len() / 2).copied().unwrap_or(0.0)
}

/// How spread `times` are: the middle half's width over the median.
fn spread(mut times: Vec<f64>) -> f64 {
    times.sort_by(|a, b| a.total_cmp(b));
    let n = times.len();
    if n < 4 {
        return 0.0;
    }
    let m = times[n / 2];
    if m <= 0.0 {
        return 0.0;
    }
    (times[3 * n / 4] - times[n / 4]) / m
}

/// One run: a session with the node and one without, each warmed by a
/// frame, then their frames timed in turn (with, without, with, …) so
/// the load of whatever else runs hits both alike; `frames` repeated to
/// at least [`MIN_FRAMES`] on each side.
fn run(with: &Arc<Scene>, without: &Arc<Scene>, source: &Arc<dyn Source>, frames: &[i64], setup: &dyn Fn(&mut Session)) -> Measured {
    let mut a = Session::new(Route::Viewer, true, Arc::clone(with), Arc::clone(source));
    let mut b = Session::new(Route::Viewer, true, Arc::clone(without), Arc::clone(source));
    setup(&mut a);
    setup(&mut b);
    a.frame(frames[0]);
    b.frame(frames[0]);
    let count = frames.len().max(MIN_FRAMES);
    let (mut ta, mut tb, mut cpu) = (Vec::with_capacity(count), Vec::with_capacity(count), 0);
    for &f in frames.iter().cycle().take(count) {
        let start = Instant::now();
        let frame = a.frame(f);
        ta.push(start.elapsed().as_secs_f64() * 1000.0);
        cpu = cpu.max(frame.stats.cpu_layers);
        let start = Instant::now();
        b.frame(f);
        tb.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    let noise = spread(ta.clone()).max(spread(tb.clone()));
    Measured { with_ms: median(ta), without_ms: median(tb), cpu_layers: cpu, noise, retried: false }
}

/// Why `m` is over `budget`, if it is.
fn breach(m: &Measured, budget: &Budget) -> Option<String> {
    let allowed = budget.ratio * m.without_ms + budget.slack_ms;
    if m.with_ms > allowed {
        return Some(format!("{:.2} ms a frame against {:.2} ms without the node, over its {allowed:.2} ms ({}× and {} ms; noise {:.2})", m.with_ms, m.without_ms, budget.ratio, budget.slack_ms, m.noise));
    }
    if m.cpu_layers > budget.cpu_layers {
        return Some(format!("{} layers drawn on the CPU, {} declared", m.cpu_layers, budget.cpu_layers));
    }
    None
}

/// `with` (a scene with the family's node) against `without` (the same
/// scene without it), over `frames` on the viewer: within `budget`, with
/// what was measured, or why not. A time breach in a noisy run (another
/// job's load) is tried once more, and the result says it was; the CPU
/// layer count isn't timing, so it's never retried.
pub fn within(with: Arc<Scene>, without: Arc<Scene>, source: Arc<dyn Source>, frames: &[i64], budget: Budget) -> Result<Measured, String> {
    within_set(with, without, source, frames, budget, &|_| {})
}

/// [`within`], each session given what the family's node reads before
/// its first frame by `setup` (an analysis family's asset store).
pub fn within_set(with: Arc<Scene>, without: Arc<Scene>, source: Arc<dyn Source>, frames: &[i64], budget: Budget, setup: &dyn Fn(&mut Session)) -> Result<Measured, String> {
    if frames.is_empty() {
        return Err("a budget needs frames to time".into());
    }
    let first = run(&with, &without, &source, frames, setup);
    let Some(why) = breach(&first, &budget) else { return Ok(first) };
    if first.cpu_layers > budget.cpu_layers || first.noise <= NOISY {
        return Err(why);
    }
    let second = Measured { retried: true, ..run(&with, &without, &source, frames, setup) };
    match breach(&second, &budget) {
        None => Ok(second),
        Some(again) => Err(format!("{again}; a second try after a noisy first ({why})")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_median_is_the_middle_time() {
        assert_eq!(median(vec![5.0, 1.0, 3.0]), 3.0);
        assert_eq!(median(vec![]), 0.0);
    }

    #[test]
    fn the_spread_is_the_middle_half_over_the_median() {
        assert_eq!(spread(vec![10.0; 8]), 0.0, "steady times are quiet");
        assert!(spread(vec![1.0, 2.0, 10.0, 10.0, 10.0, 20.0, 30.0, 40.0]) > NOISY, "a wide middle half is noisy");
    }

    #[test]
    fn a_breach_names_the_times_and_cpu_layers_count_too() {
        let m = Measured { with_ms: 10.0, without_ms: 3.0, cpu_layers: 0, noise: 0.1, retried: false };
        assert!(breach(&m, &Budget::GPU).is_some(), "10 ms against 3 is over 2× + 1");
        assert!(breach(&Measured { with_ms: 6.0, ..m }, &Budget::GPU).is_none());
        assert!(breach(&Measured { with_ms: 6.0, cpu_layers: 1, ..m }, &Budget::GPU).is_some(), "an undeclared CPU layer");
    }

    #[test]
    fn the_gpu_budget_allows_twice_the_bare_frame_and_nothing_on_the_cpu() {
        assert_eq!(Budget::GPU, Budget { ratio: 2.0, slack_ms: 1.0, cpu_layers: 0 });
    }
}
