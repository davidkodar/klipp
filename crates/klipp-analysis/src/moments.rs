//! Moments inside a long take (#502): a phone walk of two minutes with
//! no cut is one shot to the cut detector, though it holds a turn, a pan
//! that settles, a stretch of sea and someone stepping in. A moment is a
//! stretch of the shot that keeps one kind of movement and one framing,
//! split where either changes, so a search can find "a steady three
//! seconds, wide" inside the walk.
//!
//! Pure: it reads the motion analyser's per-second YDIF and the framing
//! analyser's sampled people, both already in the report.

use crate::framing::{framing_for, position_for, FrameSample};
use crate::motion::{class_for, Motion};

/// A shot is split into moments only when it is longer than this.
pub const LONG_SHOT_SECS: f64 = 12.0;
/// No moment is shorter than this: a second's wobble is not a change.
const MIN_MOMENT_SECS: f64 = 2.0;

/// One stretch of a long take.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Moment {
    pub start_secs: f64,
    pub end_secs: f64,
    /// static, gentle, moving or busy, as shots have it; `None` where the
    /// motion was not measured.
    pub motion: Option<&'static str>,
    /// The framing of the largest person, as shots have it.
    pub framing: &'static str,
    /// The most people in one sampled frame of it.
    pub people: u32,
    /// Where the largest person stands.
    pub position: &'static str,
}

impl Moment {
    /// Whether the camera holds still enough to use as it is.
    pub fn steady(&self) -> bool {
        matches!(self.motion, Some("static" | "gentle"))
    }
}

/// The moments of the shot `from..to`: empty for a shot no longer than
/// [`LONG_SHOT_SECS`] or one that keeps one movement and framing through.
pub fn moments(from: f64, to: f64, motion: Option<&Motion>, samples: &[FrameSample]) -> Vec<Moment> {
    if to - from <= LONG_SHOT_SECS {
        return Vec::new();
    }
    let seconds: Vec<f64> = (0..((to - from).floor() as usize)).map(|k| from + k as f64).collect();
    // Each second's movement, the median of it and its neighbours, so
    // one jolt is not a change of kind.
    let raw: Vec<Option<f32>> = seconds.iter().map(|t| motion.and_then(|m| m.mean_within(*t, t + 1.0))).collect();
    let moving: Vec<Option<&'static str>> = (0..raw.len())
        .map(|k| {
            let mut near: Vec<f32> = raw[k.saturating_sub(1)..(k + 2).min(raw.len())].iter().flatten().copied().collect();
            near.sort_by(f32::total_cmp);
            near.get(near.len() / 2).map(|m| class_for(*m))
        })
        .collect();
    let sample_at = |t: f64| samples.iter().filter(|s| (s.secs - t).abs() <= 1.5).min_by(|a, b| (a.secs - t).abs().total_cmp(&(b.secs - t).abs()));
    let framed: Vec<&'static str> = seconds.iter().map(|t| sample_at(t + 0.5).map(|s| framing_for(&s.people)).unwrap_or("no people")).collect();
    // Runs of seconds alike in both.
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for k in 0..seconds.len() {
        match runs.last_mut() {
            Some((_, end)) if moving[*end - 1] == moving[k] && framed[*end - 1] == framed[k] => *end = k + 1,
            _ => runs.push((k, k + 1)),
        }
    }
    // A run too short to be a moment joins its longer neighbour.
    while runs.len() > 1 {
        let Some(short) = (0..runs.len()).filter(|&i| (runs[i].1 - runs[i].0) as f64 + 1e-9 < MIN_MOMENT_SECS).min_by_key(|&i| runs[i].1 - runs[i].0) else { break };
        let len = |i: usize| runs[i].1 - runs[i].0;
        let into = match (short.checked_sub(1), (short + 1 < runs.len()).then_some(short + 1)) {
            (Some(p), Some(n)) => if len(p) >= len(n) { p } else { n },
            (Some(p), None) => p,
            (None, Some(n)) => n,
            (None, None) => break,
        };
        let (a, b) = (runs[short].0.min(runs[into].0), runs[short].1.max(runs[into].1));
        runs[into.min(short)] = (a, b);
        runs.remove(into.max(short));
    }
    if runs.len() < 2 {
        return Vec::new();
    }
    runs.iter()
        .enumerate()
        .map(|(i, &(a, b))| {
            let start = seconds[a];
            let end = if i + 1 == runs.len() { to } else { seconds[b] };
            let inside: Vec<&FrameSample> = samples.iter().filter(|s| s.secs >= start && s.secs < end).collect();
            let largest = inside.iter().max_by_key(|s| s.people.len()).map(|s| s.people.as_slice()).unwrap_or(&[]);
            Moment {
                start_secs: start,
                end_secs: end,
                motion: motion.and_then(|m| m.mean_within(start, end)).map(class_for),
                framing: framing_for(largest),
                people: inside.iter().map(|s| s.people.len() as u32).max().unwrap_or(0),
                position: position_for(largest),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk(ydif: &[(usize, f32)]) -> Motion {
        let samples: Vec<Option<f32>> = ydif.iter().flat_map(|&(secs, v)| std::iter::repeat_n(Some(v), secs)).collect();
        Motion::from_samples(samples.len() as f64, samples)
    }

    fn person(h: f32) -> (f32, f32, f32, f32) {
        (0.4, 0.1, h * 0.4, h)
    }

    #[test]
    fn a_walk_splits_where_the_camera_settles_and_someone_steps_in() {
        // 0-20 s walking (moving), 20-30 s held on the sea (static), and
        // from 30 s someone in a medium shot, still held.
        let motion = walk(&[(20, 7.0), (20, 1.0)]);
        let samples: Vec<FrameSample> = (0..40)
            .map(|k| FrameSample { secs: k as f64 + 0.5, people: if k >= 30 { vec![person(0.6)] } else { vec![] }, faces: vec![] })
            .collect();
        let m = moments(0.0, 40.0, Some(&motion), &samples);
        let spans: Vec<(f64, f64, Option<&str>, &str)> = m.iter().map(|x| (x.start_secs, x.end_secs, x.motion, x.framing)).collect();
        assert_eq!(spans.len(), 3, "{spans:?}");
        assert_eq!(spans[0].2, Some("moving"));
        assert!((spans[1].0 - 20.0).abs() <= 1.0 && spans[1].2 == Some("static") && spans[1].3 == "no people", "{spans:?}");
        assert!((spans[2].0 - 30.0).abs() <= 1.0 && spans[2].3 == "medium", "{spans:?}");
        assert_eq!(m[2].people, 1);
        assert!(m[1].steady() && !m[0].steady());
        assert_eq!(m.last().unwrap().end_secs, 40.0, "the last runs to the shot's end");
    }

    #[test]
    fn a_jolt_is_not_a_moment_and_a_short_shot_has_none() {
        // Static, a one-second jolt, static: one kind of movement.
        let motion = walk(&[(10, 1.0), (1, 20.0), (10, 1.0)]);
        assert!(moments(0.0, 21.0, Some(&motion), &[]).is_empty());
        let motion = walk(&[(5, 7.0), (5, 1.0)]);
        assert!(moments(0.0, 10.0, Some(&motion), &[]).is_empty(), "ten seconds is a shot, not a take to split");
    }
}
