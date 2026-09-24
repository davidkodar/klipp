//! Shot boundaries: where the picture changes enough to be a cut, from
//! ffmpeg's scene-change score on a downscaled decode. One record per
//! shot, so everything else can be bucketed by shot.

use serde::{Deserialize, Serialize};

pub const ID: &str = "shots";
pub const VERSION: u32 = 5;

/// Scene-change score above which a frame starts a new shot; ffmpeg's
/// `scene` is 0 to 1, and 0.3 is the customary threshold.
/// At two frames a second a slow change adds up; this is what it has
/// to add up to.
pub(crate) const SLOW_THRESHOLD: f32 = 0.22;

pub(crate) const THRESHOLD: f32 = 0.3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Shot {
    pub start_secs: f64,
    pub end_secs: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Shots {
    pub duration_secs: f64,
    /// Where each new shot starts, seconds from the file's start; the
    /// first shot starts at 0 and is not listed.
    pub boundaries: Vec<f64>,
    /// Subset detected only at 2 Hz. These can lag the actual change by
    /// up to 0.5 s; a boundary's numeric value does not identify its source.
    pub slow_boundaries: Vec<f64>,
    pub shots: Vec<Shot>,
}

impl Shots {
    pub fn from_boundaries(duration_secs: f64, mut boundaries: Vec<f64>) -> Self {
        boundaries.retain(|b| *b > 0.0 && *b < duration_secs);
        boundaries.sort_by(|a, b| a.partial_cmp(b).unwrap());
        // Within half a second of each other, the same change: the two
        // passes see a cut half a frame apart.
        boundaries.dedup_by(|a, b| (*a - *b).abs() < 0.6);
        let mut shots = Vec::with_capacity(boundaries.len() + 1);
        let mut start = 0.0;
        for b in &boundaries {
            shots.push(Shot { start_secs: start, end_secs: *b });
            start = *b;
        }
        shots.push(Shot { start_secs: start, end_secs: duration_secs });
        Self { duration_secs, boundaries, slow_boundaries: Vec::new(), shots }
    }

    /// Merge detector passes while retaining the full-rate time whenever
    /// both passes detect the same change. Explicit boundaries alone are
    /// treated as precise; only a slow-only detection gains uncertainty.
    pub fn from_passes(duration_secs: f64, fast: Vec<f64>, slow: Vec<f64>) -> Self {
        let mut fast = fast;
        fast.retain(|t| t.is_finite() && *t > 0.0 && *t < duration_secs);
        fast.sort_by(f64::total_cmp);
        // Match raw slow detections first: pre-coalescing can discard a
        // real cut through a duplicate that is itself removed by fast.
        let slow_boundaries: Vec<f64> = slow.into_iter().filter(|t| {
            if !t.is_finite() || *t <= 0.0 || *t >= duration_secs { return false; }
            let i = fast.partition_point(|f| *f < *t);
            !fast.get(i).is_some_and(|f| (*f - *t).abs() < 0.6)
                && !(i > 0 && (fast[i - 1] - *t).abs() < 0.6)
        }).collect();
        let mut boundaries = fast;
        boundaries.extend(&slow_boundaries);
        boundaries.sort_by(f64::total_cmp);
        let mut result = Self::from_boundaries(duration_secs, boundaries);
        result.slow_boundaries = result.boundaries.iter().copied().filter(|t| slow_boundaries.contains(t)).collect();
        result
    }

    /// The shots overlapping `from..to` of the file.
    pub fn within(&self, from: f64, to: f64) -> Vec<&Shot> {
        self.shots.iter().filter(|s| s.end_secs > from && s.start_secs < to).collect()
    }
}

/// The `pts_time:` of every frame `showinfo` printed: with the select
/// filter ahead of it, the frames that start a shot.
pub fn parse_showinfo(stderr: &str) -> Vec<f64> {
    let mut out = Vec::new();
    for line in stderr.lines() {
        if !line.contains("showinfo") {
            continue;
        }
        if let Some(i) = line.find("pts_time:") {
            let rest = &line[i + "pts_time:".len()..];
            let token: String = rest.trim_start().chars().take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-').collect();
            if let Ok(t) = token.parse::<f64>() {
                out.push(t);
            }
        }
    }
    out
}

pub struct ShotsAnalyser;

impl crate::Analyser for ShotsAnalyser {
    fn id(&self) -> &'static str {
        ID
    }
    fn schema_version(&self) -> u32 {
        VERSION
    }
    fn wants(&self, item: &klipp_media::MediaItem) -> bool {
        !item.is_still && !item.video_codec.is_empty() && item.duration.to_secs_f64() > 0.0
    }
    fn run(&self, item: &klipp_media::MediaItem, ctx: &crate::AnalysisCtx) -> Result<serde_json::Value, String> {
        // Two passes: frame to frame for cuts, and at two frames a second
        // for a change that takes a moment — a split screen becoming a
        // stacked one, a share opening — which frame to frame is a run
        // of small scores and never a cut. A podcast whose layout
        // changed four times came back as two shots.
        // Both rates come out of the one decode every whole-picture
        // analyser shares (see `videopass`).
        let pass = crate::videopass::get(ctx, item)?;
        let shots = Shots::from_passes(item.duration.to_secs_f64(), pass.scene_fast.clone(), pass.scene_slow.clone());
        serde_json::to_value(shots).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_matches_raw_fast_before_coalescing() {
        let s=Shots::from_passes(5.0,vec![2.0,2.4],vec![2.9]);
        assert_eq!(s.boundaries,vec![2.0]);assert!(s.slow_boundaries.is_empty());
    }
    #[test]
    fn showinfo_lines_give_the_boundaries_and_shots_follow() {
        let text = "\
[Parsed_showinfo_2 @ 0x1] n:   0 pts:  50 pts_time:2.04    pos: 1 fmt:yuv420p\n\
[Parsed_showinfo_2 @ 0x1] n:   1 pts: 125 pts_time:5.0 pos: 2\n\
some other line pts_time:9.9\n";
        let b = parse_showinfo(text);
        assert_eq!(b, vec![2.04, 5.0]);
        let s = Shots::from_boundaries(8.0, b);
        assert_eq!(s.shots.len(), 3);
        assert_eq!(s.shots[1], Shot { start_secs: 2.04, end_secs: 5.0 });
        assert_eq!(s.within(4.0, 6.0).len(), 2);
    }

    #[test]
    fn boundaries_outside_the_file_and_duplicates_are_dropped() {
        let s = Shots::from_boundaries(10.0, vec![12.0, 3.0, 3.01, 0.0, 7.0]);
        assert_eq!(s.boundaries, vec![3.0, 7.0]);
        assert_eq!(s.shots.len(), 3);
    }
    #[test]
    fn fast_timestamps_win_duplicates_and_slow_only_provenance_survives_json() {
        let shots = Shots::from_passes(8.0, vec![2.98, 5.0], vec![3.0, 5.0, 7.0]);
        assert_eq!(shots.boundaries, vec![2.98, 5.0, 7.0]);
        assert_eq!(shots.slow_boundaries, vec![7.0]);
        assert_eq!(serde_json::from_value::<Shots>(serde_json::to_value(&shots).unwrap()).unwrap(), shots);
    }

}
