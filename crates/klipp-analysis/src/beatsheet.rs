//! A screencast/demo beat sheet: from the shots (scene changes) and the
//! motion (how much the picture moves, second by second), a map of where
//! the screen is *doing something* versus *settled* — so a cut can land
//! when an action finishes and a title can sit where the picture holds.
//!
//! It reads the stored `shots` and `motion` results and synthesises them;
//! it decodes nothing itself. Motion is per-second, so the segmentation is
//! coarse: it points at moments — "a change settles around 14 s" — that a
//! caller refines by looking at the frames there (shot boundaries are the
//! precise cut times). Cheap, deterministic, cached like any analyser.

use serde::{Deserialize, Serialize};

use crate::motion::Motion;
use crate::shots::Shots;

pub const ID: &str = "beatsheet";
pub const VERSION: u32 = 1;

/// YDIF at or above which a second counts as *action* — ffmpeg's scale,
/// the "moving" band of [`crate::motion::class_for`]. Below it the picture
/// is settled enough to read, to cut on, or to hold a title over.
const ACTION_YDIF: f32 = 4.0;
/// The shortest settled stretch worth offering as a title / hold window.
const MIN_WINDOW_SECS: f64 = 1.2;
/// Two cut candidates closer than this are the same opportunity.
const CUT_MERGE_SECS: f64 = 0.6;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Segment {
    pub start_secs: f64,
    pub end_secs: f64,
    /// "idle" (settled, readable) or "action" (the screen is changing).
    pub kind: String,
    pub motion_mean: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CutPoint {
    pub secs: f64,
    /// 3 (a real scene change, or a change that just finished) down to 1.
    pub strength: u8,
    pub why: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Window {
    pub start_secs: f64,
    pub end_secs: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BeatSheet {
    pub duration_secs: f64,
    pub segments: Vec<Segment>,
    /// Times where an action ended and the picture settled — a reveal
    /// completed; the best moments to cut *to* and hold on.
    pub reveals: Vec<f64>,
    pub cut_points: Vec<CutPoint>,
    /// Settled stretches long enough to hold a title without it fighting
    /// a moving picture.
    pub title_windows: Vec<Window>,
    pub has_shots: bool,
    pub has_motion: bool,
}

/// Build the beat sheet from whatever of shots/motion is available. Pure
/// so it can be tested without ffmpeg or the store.
pub fn build(duration_secs: f64, shots: Option<&Shots>, motion: Option<&Motion>) -> BeatSheet {
    let kind_at = |ydif: f32| if ydif >= ACTION_YDIF { "action" } else { "idle" };

    // Per-second idle/action, merged into contiguous segments.
    let mut segments: Vec<Segment> = Vec::new();
    if let Some(m) = motion {
        let mut i = 0usize;
        while i < m.per_second.len() {
            let k = kind_at(m.per_second[i]);
            let start = i;
            let (mut sum, mut n) = (0.0f32, 0u32);
            while i < m.per_second.len() && kind_at(m.per_second[i]) == k {
                sum += m.per_second[i];
                n += 1;
                i += 1;
            }
            segments.push(Segment {
                start_secs: start as f64,
                end_secs: (i as f64).min(duration_secs),
                kind: k.to_string(),
                motion_mean: if n > 0 { sum / n as f32 } else { 0.0 },
            });
        }
    }

    // A reveal is where an action segment gives way to an idle one.
    let reveals: Vec<f64> = segments
        .windows(2)
        .filter(|w| w[0].kind == "action" && w[1].kind == "idle")
        .map(|w| w[1].start_secs)
        .collect();

    // Cut candidates: real scene changes, plus the edges of actions.
    let mut cuts: Vec<CutPoint> = Vec::new();
    if let Some(s) = shots {
        for b in &s.boundaries {
            cuts.push(CutPoint { secs: *b, strength: 3, why: "shot change".into() });
        }
    }
    for w in segments.windows(2) {
        match (w[0].kind.as_str(), w[1].kind.as_str()) {
            ("action", "idle") => cuts.push(CutPoint { secs: w[1].start_secs, strength: 3, why: "a change just settled".into() }),
            ("idle", "action") => cuts.push(CutPoint { secs: w[1].start_secs, strength: 2, why: "just before a change starts".into() }),
            _ => {}
        }
    }
    cuts.sort_by(|a, b| a.secs.partial_cmp(&b.secs).unwrap_or(std::cmp::Ordering::Equal));
    // Merge near-identical candidates, keeping the strongest and its reason.
    let mut cut_points: Vec<CutPoint> = Vec::new();
    for c in cuts {
        match cut_points.last_mut() {
            Some(last) if (c.secs - last.secs).abs() < CUT_MERGE_SECS => {
                if c.strength > last.strength {
                    *last = c;
                }
            }
            _ => cut_points.push(c),
        }
    }

    // Title / hold windows: settled stretches long enough to hold text.
    let title_windows: Vec<Window> = segments
        .iter()
        .filter(|s| s.kind == "idle" && s.end_secs - s.start_secs >= MIN_WINDOW_SECS)
        .map(|s| Window { start_secs: s.start_secs, end_secs: s.end_secs })
        .collect();

    BeatSheet {
        duration_secs,
        segments,
        reveals,
        cut_points,
        title_windows,
        has_shots: shots.is_some(),
        has_motion: motion.is_some(),
    }
}

impl BeatSheet {
    /// A short account for a model to read.
    pub fn summary(&self) -> String {
        use std::fmt::Write as _;
        let (idle, action) = self
            .segments
            .iter()
            .fold((0, 0), |(i, a), s| if s.kind == "action" { (i, a + 1) } else { (i + 1, a) });
        let mut out = String::new();
        let _ = writeln!(
            out,
            "Beat sheet over {:.2} s: {} segments ({idle} idle, {action} action), {} reveals, {} cut points, {} title windows.",
            self.duration_secs,
            self.segments.len(),
            self.reveals.len(),
            self.cut_points.len(),
            self.title_windows.len(),
        );
        if !self.has_motion {
            let _ = writeln!(out, "(No motion analysis stored yet — cut points are shot changes only; the idle/action segmentation is unavailable.)");
        }
        let strong: Vec<&CutPoint> = self.cut_points.iter().filter(|c| c.strength >= 2).collect();
        if !strong.is_empty() {
            let _ = writeln!(out, "Strong cut points (cut to a settle, hold until readable):");
            for c in strong.iter().take(24) {
                let _ = writeln!(out, "  {:.2} s  strength {}  {}", c.secs, c.strength, c.why);
            }
        }
        if !self.title_windows.is_empty() {
            let _ = writeln!(out, "Title / hold windows (place a title where the picture holds):");
            for w in self.title_windows.iter().take(24) {
                let _ = writeln!(out, "  {:.2}-{:.2} s", w.start_secs, w.end_secs);
            }
        }
        out
    }
}

pub struct BeatSheetAnalyser;

impl crate::Analyser for BeatSheetAnalyser {
    fn id(&self) -> &'static str {
        ID
    }
    fn schema_version(&self) -> u32 {
        VERSION
    }
    fn wants(&self, item: &klipp_media::MediaItem) -> bool {
        !item.is_still && !item.video_codec.is_empty() && item.duration.to_secs_f64() > 0.0
    }
    fn run(&self, item: &klipp_media::MediaItem, _ctx: &crate::AnalysisCtx) -> Result<serde_json::Value, String> {
        // Synthesised from the stored shots and motion — which run before
        // this one, so they are on disk under the same cache key. If a
        // file has no shot/motion result (analysis failed), the sheet is
        // built from whatever is present and says so.
        let key = crate::store::key_for(&item.path).ok_or("file not readable")?;
        let shots: Option<Shots> = crate::store::load(&key, crate::shots::ID, crate::shots::VERSION).and_then(|v| serde_json::from_value(v).ok());
        let motion: Option<Motion> = crate::store::load(&key, crate::motion::ID, crate::motion::VERSION).and_then(|v| serde_json::from_value(v).ok());
        let sheet = build(item.duration.to_secs_f64(), shots.as_ref(), motion.as_ref());
        serde_json::to_value(sheet).map_err(|e| e.to_string())
    }
    /// Redo a beat sheet that was built before its motion existed, so a
    /// thin (shots-only) sheet is replaced once the motion pass lands.
    fn stale(&self, stored: &serde_json::Value) -> bool {
        stored.get("has_motion").and_then(|v| v.as_bool()) == Some(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shots::Shots;

    fn motion(per_second: Vec<f32>) -> Motion {
        Motion { duration_secs: per_second.len() as f64, per_second, mean: 0.0, p95: 0.0 }
    }

    #[test]
    fn idle_and_action_segments_from_motion() {
        // idle, idle, action, action, idle  (YDIF)
        let m = motion(vec![0.5, 1.0, 8.0, 12.0, 0.5]);
        let bs = build(5.0, None, Some(&m));
        assert_eq!(bs.segments.len(), 3);
        assert_eq!(bs.segments[0].kind, "idle");
        assert_eq!((bs.segments[0].start_secs, bs.segments[0].end_secs), (0.0, 2.0));
        assert_eq!(bs.segments[1].kind, "action");
        assert_eq!((bs.segments[1].start_secs, bs.segments[1].end_secs), (2.0, 4.0));
        assert_eq!(bs.segments[2].kind, "idle");
        // A reveal where the action settled back to idle.
        assert_eq!(bs.reveals, vec![4.0]);
    }

    #[test]
    fn cut_points_prefer_settles_and_scene_changes() {
        let m = motion(vec![0.5, 1.0, 8.0, 12.0, 0.5]);
        let shots = Shots::from_boundaries(5.0, vec![2.0]);
        let bs = build(5.0, Some(&shots), Some(&m));
        // The idle→action edge at 2 s coincides with the shot change; kept
        // once at the strongest (3).
        let at2 = bs.cut_points.iter().find(|c| (c.secs - 2.0).abs() < 0.01).unwrap();
        assert_eq!(at2.strength, 3);
        // The action→idle settle at 4 s is a strength-3 cut point.
        let at4 = bs.cut_points.iter().find(|c| (c.secs - 4.0).abs() < 0.01).unwrap();
        assert_eq!(at4.strength, 3);
        assert_eq!(at4.why, "a change just settled");
    }

    #[test]
    fn title_windows_are_the_long_idle_stretches() {
        // A 3 s idle open, a 1 s action, then a short idle tail.
        let m = motion(vec![0.5, 0.5, 0.5, 9.0, 0.5]);
        let bs = build(5.0, None, Some(&m));
        // Only the 3 s idle stretch clears MIN_WINDOW_SECS.
        assert_eq!(bs.title_windows, vec![Window { start_secs: 0.0, end_secs: 3.0 }]);
    }

    #[test]
    fn no_motion_falls_back_to_shots_only() {
        let shots = Shots::from_boundaries(10.0, vec![3.0, 6.0]);
        let bs = build(10.0, Some(&shots), None);
        assert!(!bs.has_motion);
        assert!(bs.segments.is_empty());
        assert_eq!(bs.cut_points.len(), 2);
        assert!(bs.cut_points.iter().all(|c| c.why == "shot change"));
    }
}
