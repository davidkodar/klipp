//! A screencast/demo beat sheet: where the screen is *settled* versus
//! *changing*, so a cut can land when an action finishes and a title can
//! sit where the picture holds.
//!
//! Screen recordings defeat the camera-tuned signals: a UI change is small
//! and localised, so the whole-frame luma difference (`motion`) rounds to
//! zero and the scene-change score (`shots`) never fires — a four-minute
//! demo reads as one frozen shot. So this analyser measures stillness
//! directly with ffmpeg's `freezedetect`: a run of near-identical frames is
//! a *freeze* (the screen is settled); the gaps between freezes are where
//! something is happening. A freeze that *begins* is a change that just
//! settled — a reveal, and the best moment to cut to and hold.
//!
//! Coarse on purpose (a low sampling rate): it points at moments — "a
//! change settles around 14 s" — that a caller refines by looking at the
//! frames there. The scene changes from `shots`, when there are any, are
//! added as precise cut points.

use serde::{Deserialize, Serialize};

use crate::shots::Shots;

pub const ID: &str = "beatsheet";
// 10: made for screen recordings only (#500); a camera clip's beat sheet
// from before is dropped.
pub const VERSION: u32 = 10;

/// Frames sampled a second for freeze detection: enough to place a settle
/// to a sixth of a second, cheap to decode.
pub(crate) const SAMPLE_FPS: u32 = 6;
/// Analysis width; small UI changes survive this, unlike the 160 px the
/// `motion` pass uses.
pub(crate) const SCALE_W: u32 = 480;
/// How different two frames may be and still count as frozen (a ratio,
/// 0–1). Above it the screen is changing. Low enough to catch a panel
/// opening, high enough to ignore a blinking cursor or caret.
pub(crate) const FREEZE_NOISE: &str = "0.003";
/// The shortest stillness that counts as a settle / hold window.
pub(crate) const FREEZE_MIN_SECS: &str = "0.5";
/// A settled stretch at least this long is offered as a title / hold window.
const MIN_WINDOW_SECS: f64 = 1.2;
/// Two cut candidates closer than this are the same opportunity.
const CUT_MERGE_SECS: f64 = 0.6;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Segment {
    pub start_secs: f64,
    pub end_secs: f64,
    /// "idle" (settled, readable) or "action" (the screen is changing).
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CutPoint {
    pub secs: f64,
    /// 3 (a scene change, or a change that just settled) down to 2.
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
    /// Times where the screen settled after a change — a reveal completed;
    /// the best moments to cut *to* and hold on.
    pub reveals: Vec<f64>,
    pub cut_points: Vec<CutPoint>,
    /// Settled stretches long enough to hold a title without it fighting a
    /// moving picture.
    pub title_windows: Vec<Window>,
    pub has_shots: bool,
    /// Whether freeze detection found any settled stretch at all.
    pub has_freeze: bool,
    /// A patch of the picture that moved all through the recording — a
    /// presenter's webcam inset — as fractions of the frame (left, top,
    /// width, height), masked out before the screen's own settling was
    /// measured. Absent when the whole picture was read.
    #[serde(default)]
    pub busy_inset: Option<[f32; 4]>,
    /// Where each thing happened: the centre of the cells that changed
    /// at a grid event, as fractions of the frame. A follow or a
    /// highlight goes to the spot.
    #[serde(default)]
    pub spots: Vec<Spot>,
}

/// A moment something happened on screen, and where.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Spot {
    pub secs: f64,
    /// The centre of the change, as fractions of the frame.
    pub x: f32,
    pub y: f32,
}

/// Build the beat sheet from the frozen (settled) spans and the shots.
/// `frozen` is the list of `(start, end)` settled stretches, in seconds.
/// Pure, so it can be tested without ffmpeg.
pub fn build(duration_secs: f64, shots: Option<&Shots>, frozen: &[(f64, f64)]) -> BeatSheet {
    // Normalise the frozen spans: clamp to the file, drop empties, sort,
    // and merge any that touch.
    let mut spans: Vec<(f64, f64)> = frozen
        .iter()
        .map(|(a, b)| (a.max(0.0), b.min(duration_secs)))
        .filter(|(a, b)| b - a > 1e-3)
        .collect();
    spans.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    // Spans that overlap are one; spans that merely touch are two: a
    // freeze that ends at the instant the next begins is one frame that
    // differed — a change that settled at once — and that instant is a
    // reveal, so a sample's gap is opened between them.
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for mut s in spans {
        match merged.last_mut() {
            Some(last) if s.0 < last.1 - 1e-3 => last.1 = last.1.max(s.1),
            Some(last) if s.0 <= last.1 + 1e-3 => {
                s.0 = last.1 + 1.0 / SAMPLE_FPS as f64;
                if s.1 > s.0 {
                    merged.push(s);
                }
            }
            _ => merged.push(s),
        }
    }

    // Walk the file: frozen spans are idle, the gaps between them action.
    let mut segments: Vec<Segment> = Vec::new();
    let mut reveals: Vec<f64> = Vec::new();
    let mut cursor = 0.0;
    for &(fs, fe) in &merged {
        if fs - cursor > 1e-3 {
            segments.push(Segment { start_secs: cursor, end_secs: fs, kind: "action".into() });
        }
        // A freeze that begins after some action is a settle / reveal.
        if fs > 1e-3 {
            reveals.push(fs);
        }
        segments.push(Segment { start_secs: fs, end_secs: fe, kind: "idle".into() });
        cursor = fe;
    }
    if duration_secs - cursor > 1e-3 {
        segments.push(Segment { start_secs: cursor, end_secs: duration_secs, kind: "action".into() });
    }

    // Cut candidates: real scene changes, plus the edges of freezes.
    let mut cuts: Vec<CutPoint> = Vec::new();
    if let Some(s) = shots {
        for b in &s.boundaries {
            cuts.push(CutPoint { secs: *b, strength: 3, why: "shot change".into() });
        }
    }
    for &(fs, fe) in &merged {
        if fs > 1e-3 {
            cuts.push(CutPoint { secs: fs, strength: 3, why: "a change just settled".into() });
        }
        if fe < duration_secs - 1e-3 {
            cuts.push(CutPoint { secs: fe, strength: 2, why: "just before a change starts".into() });
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
    let title_windows: Vec<Window> = merged
        .iter()
        .filter(|(a, b)| b - a >= MIN_WINDOW_SECS)
        .map(|&(a, b)| Window { start_secs: a, end_secs: b })
        .collect();

    BeatSheet {
        duration_secs,
        segments,
        reveals,
        cut_points,
        title_windows,
        has_shots: shots.is_some(),
        has_freeze: !merged.is_empty(),
        busy_inset: None,
        spots: Vec::new(),
    }
}

/// The activity grid: how much each cell of a coarse grid changed
/// from one sampled frame to the next, over the whole file.
pub(crate) const GRID_W: usize = 16;
pub(crate) const GRID_H: usize = 9;
pub(crate) const GRID_FPS: u32 = 2;
/// A cell's mean change above this (of 255) counts as the cell moving.
const GRID_MOVED: u8 = 3;

/// The drawbox filter that blanks `rect` (fractions of the frame).
fn mask_filter(rect: [f32; 4]) -> String {
    let [x, y, w, h] = rect;
    format!("drawbox=x=iw*{x:.4}:y=ih*{y:.4}:w=iw*{w:.4}:h=ih*{h:.4}:color=black:t=fill,")
}

/// Run ffmpeg's `freezedetect` over the file and return the frozen
/// (settled) spans as `(start, end)` seconds, and — on the first pass,
/// when `with_grid` — the activity grid, in one decode. `mask` blanks a
/// patch of the picture first. Empty on a file that never settles, or
/// on an ffmpeg failure (the caller degrades gracefully).
pub fn detect_frozen(ffmpeg: &std::path::Path, path: &std::path::Path, codec: &str, duration_secs: f64, mask: Option<[f32; 4]>, with_grid: bool) -> (Vec<(f64, f64)>, Vec<u8>) {
    let masked = mask.map(mask_filter).unwrap_or_default();
    // A list of other inputs is not media: nothing to read.
    let Ok(input) = klipp_media::input_args(path) else { return (Vec::new(), Vec::new()) };
    for plan in crate::decode_plans(ffmpeg) {
        let scaled = crate::scale_filter(ffmpeg, plan, SCALE_W, -2);
        let mut cmd = std::process::Command::new(ffmpeg);
        cmd.args(["-hide_banner", "-nostats", "-loglevel", "info"]);
        cmd.args(crate::decode_input_args(ffmpeg, plan, codec));
        cmd.args(&input).args(["-an", "-sn"]);
        if with_grid {
            let graph = format!(
                "[0:v:0]fps={SAMPLE_FPS},{scaled},{masked}split[fz][act];[fz]freezedetect=n={FREEZE_NOISE}:d={FREEZE_MIN_SECS},nullsink;[act]fps={GRID_FPS},format=gray,tblend=all_mode=difference,scale={GRID_W}:{GRID_H}:flags=area[acto]"
            );
            cmd.args(["-filter_complex", &graph, "-map", "[acto]", "-f", "rawvideo", "-pix_fmt", "gray", "-"]);
        } else {
            let vf = format!("fps={SAMPLE_FPS},{scaled},{masked}freezedetect=n={FREEZE_NOISE}:d={FREEZE_MIN_SECS}");
            cmd.args(["-vf", &vf, "-f", "null", "-"]);
        }
        // freezedetect logs `freeze_start`/`freeze_end` at info level.
        let Ok(out) = crate::hold::output(&mut cmd) else { return (Vec::new(), Vec::new()) };
        if !out.status.success() {
            tracing::warn!("freezedetect of {} with {plan:?} failed; trying the next", path.display());
            continue;
        }
        return (parse_freezedetect(&String::from_utf8_lossy(&out.stderr), duration_secs), if with_grid { out.stdout } else { Vec::new() });
    }
    (Vec::new(), Vec::new())
}

/// A cell whose mean change from one sampled frame to the next is above
/// this (of 255) saw something happen there — a dialog, a menu, a
/// window — even when the whole frame's change stayed under
/// freezedetect's noise.
pub(crate) const GRID_EVENT: u8 = 10;

/// The times at which some cell of the grid jumped: local changes that
/// freezedetect, which reads the whole frame, does not see. Two within
/// half a second are one.
pub fn grid_events(grid: &[u8]) -> Vec<f64> {
    let cells = GRID_W * GRID_H;
    let mut out: Vec<f64> = Vec::new();
    for (f, frame) in grid.chunks_exact(cells).enumerate() {
        if frame.iter().any(|v| *v > GRID_EVENT) {
            let t = f as f64 / GRID_FPS as f64;
            if out.last().is_none_or(|last| t - *last >= 0.5) {
                out.push(t);
            }
        }
    }
    out
}

/// The grid's events with where they happened: the centre of the cells
/// that changed, weighted by how much, on the event's frame.
pub fn grid_event_spots(grid: &[u8]) -> Vec<Spot> {
    let cells = GRID_W * GRID_H;
    let mut out: Vec<Spot> = Vec::new();
    for (f, frame) in grid.chunks_exact(cells).enumerate() {
        if !frame.iter().any(|v| *v > GRID_EVENT) {
            continue;
        }
        let t = f as f64 / GRID_FPS as f64;
        if out.last().is_some_and(|last| t - last.secs < 0.5) {
            continue;
        }
        let (mut sx, mut sy, mut sw) = (0.0f32, 0.0f32, 0.0f32);
        for (c, v) in frame.iter().enumerate() {
            if *v > GRID_EVENT {
                let w = *v as f32;
                sx += ((c % GRID_W) as f32 + 0.5) / GRID_W as f32 * w;
                sy += ((c / GRID_W) as f32 + 0.5) / GRID_H as f32 * w;
                sw += w;
            }
        }
        out.push(Spot { secs: t, x: sx / sw, y: sy / sw });
    }
    out
}

/// Frozen spans cut at the grid's events, so a stretch the whole frame
/// held still through is two beats when a dialog came up in the middle
/// of it. A cut that would leave a piece under half a second is not made.
pub fn split_frozen(frozen: &[(f64, f64)], events: &[f64]) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    for (a, b) in frozen {
        let mut start = *a;
        for e in events.iter().filter(|e| **e > *a && **e < *b) {
            if e - start >= 0.5 && b - e >= 0.5 {
                out.push((start, *e));
                start = *e;
            }
        }
        out.push((start, *b));
    }
    out
}

/// A patch that moved nearly all the time while the rest of the picture
/// mostly held still — a webcam inset — from the activity grid: the
/// cells that changed in over half the frames and at three times the
/// median cell, at least two of them, boxed, and no more than half the
/// frame either way. As fractions of the frame, half a cell of margin.
pub fn busy_inset_from_grid(grid: &[u8]) -> Option<[f32; 4]> {
    let cells = GRID_W * GRID_H;
    let frames = grid.len() / cells;
    if frames < 20 {
        return None;
    }
    let mut hot = vec![0usize; cells];
    for f in 0..frames {
        for (c, h) in hot.iter_mut().enumerate() {
            if grid[f * cells + c] > GRID_MOVED {
                *h += 1;
            }
        }
    }
    let share: Vec<f32> = hot.iter().map(|h| *h as f32 / frames as f32).collect();
    let mut sorted = share.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = sorted[cells / 2];
    let threshold = (3.0 * median).max(0.5);
    let busy: Vec<(usize, usize)> = (0..cells).filter(|c| share[*c] > threshold).map(|c| (c % GRID_W, c / GRID_W)).collect();
    if busy.len() < 2 {
        return None;
    }
    let (x0, x1) = (busy.iter().map(|b| b.0).min()?, busy.iter().map(|b| b.0).max()?);
    let (y0, y1) = (busy.iter().map(|b| b.1).min()?, busy.iter().map(|b| b.1).max()?);
    let (cw, ch) = (x1 + 1 - x0, y1 + 1 - y0);
    if cw * 2 > GRID_W || ch * 2 > GRID_H {
        return None; // half the screen moving is the screen, not an inset
    }
    let (fx, fy) = (1.0 / GRID_W as f32, 1.0 / GRID_H as f32);
    let x = (x0 as f32 - 0.5) * fx;
    let y = (y0 as f32 - 0.5) * fy;
    let w = (cw as f32 + 1.0) * fx;
    let h = (ch as f32 + 1.0) * fy;
    Some([x.max(0.0), y.max(0.0), w.min(1.0 - x.max(0.0)), h.min(1.0 - y.max(0.0))])
}

/// Parse freezedetect's log lines into `(start, end)` spans. A freeze that
/// is still open at the end of the log closes at `duration_secs`.
pub fn parse_freezedetect(stderr: &str, duration_secs: f64) -> Vec<(f64, f64)> {
    let value_after = |line: &str, key: &str| -> Option<f64> {
        let i = line.find(key)? + key.len();
        line[i..].split_whitespace().next()?.parse().ok()
    };
    let mut spans: Vec<(f64, f64)> = Vec::new();
    let mut open: Option<f64> = None;
    for line in stderr.lines() {
        if let Some(t) = value_after(line, "freeze_start:") {
            open = Some(t);
        } else if let Some(t) = value_after(line, "freeze_end:") {
            if let Some(start) = open.take() {
                spans.push((start, t));
            }
        }
    }
    if let Some(start) = open {
        spans.push((start, duration_secs));
    }
    spans
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
        if !self.has_freeze {
            let _ = writeln!(out, "(The screen never settled for long — freeze detection found no hold; try a shorter freeze threshold, or the footage is genuinely continuous motion.)");
        }
        if let Some([x, y, w, h]) = self.busy_inset {
            let _ = writeln!(out, "A patch at {:.0}%,{:.0}% ({:.0}% by {:.0}%) moved all through — a presenter inset — and was left out of the settling.", x * 100.0, y * 100.0, w * 100.0, h * 100.0);
        }
        if !self.spots.is_empty() {
            let _ = writeln!(out, "Where things happened (a follow or highlight goes to the spot):");
            for s in self.spots.iter().take(30) {
                let _ = writeln!(out, "  {:.2} s  at {:.0}%,{:.0}%", s.secs, s.x * 100.0, s.y * 100.0);
            }
        }
        let strong: Vec<&CutPoint> = self.cut_points.iter().filter(|c| c.strength >= 2).collect();
        if !strong.is_empty() {
            let _ = writeln!(out, "Cut points (cut TO a settle, hold until it reads; never mid-action):");
            for c in strong.iter().take(30) {
                let _ = writeln!(out, "  {:.2} s  strength {}  {}", c.secs, c.strength, c.why);
            }
        }
        if !self.title_windows.is_empty() {
            let _ = writeln!(out, "Title / hold windows (place a title where the picture holds):");
            for w in self.title_windows.iter().take(30) {
                let _ = writeln!(out, "  {:.2}-{:.2} s  ({:.1} s)", w.start_secs, w.end_secs, w.end_secs - w.start_secs);
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
        !item.is_still && !item.video_codec.is_empty() && item.duration.to_secs_f64() > 0.0 && crate::looks_like_screen(item)
    }
    fn run(&self, item: &klipp_media::MediaItem, ctx: &crate::AnalysisCtx) -> Result<serde_json::Value, String> {
        let duration = item.duration.to_secs_f64();
        // One pass reads the settling and the activity grid; a moving
        // inset found in the grid is blanked and the settling read again,
        // so a presenter's nod does not break the screen's hold.
        let pass = crate::videopass::get(ctx, item)?;
        let mut frozen = pass.frozen.clone();
        let busy_inset = busy_inset_from_grid(&pass.grid);
        if let Some(rect) = busy_inset {
            let (masked, _) = detect_frozen(&ctx.ffmpeg, &item.path, &item.video_codec, duration, Some(rect), false);
            if !masked.is_empty() {
                frozen = masked;
            }
        }
        // A dialog or a window that came up inside a still stretch is a
        // beat of its own, though the whole frame's change was under the
        // noise. The inset's own cells are not events.
        let mut grid = pass.grid.clone();
        if let Some([x, y, w, h]) = busy_inset {
            let cells = GRID_W * GRID_H;
            for frame in grid.chunks_exact_mut(cells) {
                for (c, v) in frame.iter_mut().enumerate() {
                    let (cx, cy) = ((c % GRID_W) as f32 / GRID_W as f32, (c / GRID_W) as f32 / GRID_H as f32);
                    if cx >= x - 0.01 && cx < x + w && cy >= y - 0.01 && cy < y + h {
                        *v = 0;
                    }
                }
            }
        }
        frozen = split_frozen(&frozen, &grid_events(&grid));
        // The scene changes, when they exist, are precise cut points; a
        // screen recording usually has none, and that is fine.
        let shots: Option<Shots> = crate::store::key_for(&item.path)
            .and_then(|key| crate::store::load(&key, crate::shots::ID, crate::shots::VERSION))
            .and_then(|v| serde_json::from_value(v).ok());
        let mut sheet = build(duration, shots.as_ref(), &frozen);
        sheet.busy_inset = busy_inset;
        sheet.spots = grid_event_spots(&grid);
        serde_json::to_value(sheet).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shots::Shots;

    #[test]
    fn a_corner_that_moves_all_the_time_is_a_busy_inset_and_a_busy_screen_is_not() {
        let cells = GRID_W * GRID_H;
        let mut grid = vec![0u8; cells * 100];
        // Two cells bottom right move in nine frames of ten; the rest in one of ten.
        for f in 0..100 {
            for c in 0..cells {
                let (x, y) = (c % GRID_W, c / GRID_W);
                let inset = (14..16).contains(&x) && (4..6).contains(&y);
                grid[f * cells + c] = if inset || f % 10 == 0 { 20 } else { 0 };
            }
        }
        let [x, y, w, h] = busy_inset_from_grid(&grid).expect("an inset");
        assert!((x - 13.5 / 16.0).abs() < 1e-5 && (y - 3.5 / 9.0).abs() < 1e-5, "{x} {y}");
        assert!((w - 2.5 / 16.0).abs() < 1e-5 && (h - 3.0 / 9.0).abs() < 1e-5, "clipped to the frame edge: {w} {h}");
        // A screen that moves everywhere has no inset; nor does one cell alone; nor too few frames.
        assert!(busy_inset_from_grid(&vec![20u8; cells * 100]).is_none());
        let mut one = vec![0u8; cells * 100];
        for f in 0..100 { one[f * cells + 5] = 20; }
        assert!(busy_inset_from_grid(&one).is_none());
        assert!(busy_inset_from_grid(&grid[..cells * 10]).is_none());
        assert!(mask_filter([0.5, 0.25, 0.1, 0.2]).starts_with("drawbox=x=iw*0.5000:y=ih*0.2500:w=iw*0.1000:h=ih*0.2000:color=black:t=fill,"));
    }

    #[test]
    fn a_still_stretch_is_split_where_a_cell_of_the_grid_jumps() {
        let cells = GRID_W * GRID_H;
        // 20 s at 2 fps: one cell jumps at 6.0 s and again at 6.2 s (one event), and at 15 s.
        let mut grid = vec![0u8; cells * 40];
        grid[12 * cells + 5] = 60;
        grid[12 * cells + 6] = 60;
        grid[30 * cells + 100] = 90;
        assert_eq!(grid_events(&grid), vec![6.0, 15.0]);
        let spots = grid_event_spots(&grid);
        assert_eq!(spots.len(), 2);
        assert!((spots[0].secs - 6.0).abs() < 1e-9);
        assert_eq!(split_frozen(&[(0.0, 20.0)], &[6.0, 15.0]), vec![(0.0, 6.0), (6.0, 15.0), (15.0, 20.0)]);
        // Too close to an edge: no cut.
        assert_eq!(split_frozen(&[(5.8, 20.0)], &[6.0]), vec![(5.8, 20.0)]);
        assert_eq!(split_frozen(&[(0.0, 3.0), (10.0, 12.0)], &[6.0]), vec![(0.0, 3.0), (10.0, 12.0)]);
    }

    #[test]
    fn spans_that_touch_are_a_change_that_settled_at_once() {
        // freezedetect: a freeze to 5.5, another from 5.5: one frame differed.
        let bs = build(20.0, None, &[(0.0, 5.5), (5.5, 15.0)]);
        assert_eq!(bs.reveals, vec![5.5 + 1.0 / SAMPLE_FPS as f64], "{:?}", bs.reveals);
        assert_eq!(bs.segments.iter().filter(|s| s.kind == "idle").count(), 2);
        // Overlapping spans are still one.
        let bs = build(20.0, None, &[(0.0, 6.0), (5.5, 15.0)]);
        assert_eq!(bs.segments.iter().filter(|s| s.kind == "idle").count(), 1);
    }

    #[test]
    fn frozen_spans_become_idle_and_gaps_action() {
        // Idle 0-3, action 3-4, idle 4-8 on a 10 s file (action tail 8-10).
        let bs = build(10.0, None, &[(0.0, 3.0), (4.0, 8.0)]);
        assert_eq!(bs.segments.len(), 4);
        assert_eq!((bs.segments[0].kind.as_str(), bs.segments[0].start_secs, bs.segments[0].end_secs), ("idle", 0.0, 3.0));
        assert_eq!((bs.segments[1].kind.as_str(), bs.segments[1].start_secs, bs.segments[1].end_secs), ("action", 3.0, 4.0));
        assert_eq!((bs.segments[2].kind.as_str(), bs.segments[2].start_secs, bs.segments[2].end_secs), ("idle", 4.0, 8.0));
        assert_eq!((bs.segments[3].kind.as_str(), bs.segments[3].start_secs, bs.segments[3].end_secs), ("action", 8.0, 10.0));
        // The freeze that begins at 4 s (after action) is a reveal; the one
        // at 0 is not.
        assert_eq!(bs.reveals, vec![4.0]);
    }

    #[test]
    fn settle_and_scene_change_are_cut_points() {
        let shots = Shots::from_boundaries(10.0, vec![4.0]);
        let bs = build(10.0, Some(&shots), &[(0.0, 3.0), (4.0, 8.0)]);
        // Settle at 4 s (freeze start) coincides with the shot change: kept
        // once at the strongest.
        let at4 = bs.cut_points.iter().find(|c| (c.secs - 4.0).abs() < 0.01).unwrap();
        assert_eq!(at4.strength, 3);
        // Freeze end at 3 s = just before a change: strength 2.
        let at3 = bs.cut_points.iter().find(|c| (c.secs - 3.0).abs() < 0.01).unwrap();
        assert_eq!(at3.strength, 2);
    }

    #[test]
    fn title_windows_are_the_long_freezes() {
        let bs = build(10.0, None, &[(0.0, 3.0), (4.0, 4.8)]);
        // Only the 3 s freeze clears MIN_WINDOW_SECS; the 0.8 s one does not.
        assert_eq!(bs.title_windows, vec![Window { start_secs: 0.0, end_secs: 3.0 }]);
    }

    #[test]
    fn no_freeze_is_all_action() {
        let bs = build(10.0, None, &[]);
        assert!(!bs.has_freeze);
        assert_eq!(bs.segments, vec![Segment { start_secs: 0.0, end_secs: 10.0, kind: "action".into() }]);
        assert!(bs.title_windows.is_empty());
    }

    #[test]
    fn freezedetect_log_parses_into_spans() {
        let log = "\
[freezedetect @ 0x1] lavfi.freezedetect.freeze_start: 5.005\n\
[freezedetect @ 0x1] lavfi.freezedetect.freeze_duration: 2.002\n\
[freezedetect @ 0x1] lavfi.freezedetect.freeze_end: 7.007\n\
[freezedetect @ 0x1] lavfi.freezedetect.freeze_start: 12.0\n";
        let spans = parse_freezedetect(log, 20.0);
        assert_eq!(spans, vec![(5.005, 7.007), (12.0, 20.0)]);
    }
}
