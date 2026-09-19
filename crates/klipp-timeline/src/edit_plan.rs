//! The edit plan: a paper edit. Which stretch of which file goes down,
//! in what order, for what feature of the brief, with what on top —
//! said before a single clip is placed, so a person can read it, change
//! it and approve it while changing it is cheap. Once approved it is
//! built into the timeline by lowering to ordinary ops (in the document
//! crate, which owns them); everything after that — resolve, apply,
//! undo, check, export — is the same as for any other edit.

use klipp_core::MediaId;
use serde::{Deserialize, Serialize};

/// A title over a segment: what it says, how long it holds, what sort.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PlanTitle {
    pub text: String,
    /// How long it holds, in seconds; 0 or absent means the segment's length.
    #[serde(default)]
    pub secs: Option<f64>,
    /// "heading" (large, upper third, rises in), "callout" (small, low,
    /// backed, fades in) or "card" (full-frame colour card: a section
    /// break or a call to action). Left out: a heading.
    #[serde(default)]
    pub style: String,
}

/// An animated push on a segment, scale `from` to `to` toward `x`,`y`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanZoom {
    #[serde(default = "one")]
    pub from: f32,
    pub to: f32,
    #[serde(default)]
    pub x: f32,
    #[serde(default)]
    pub y: f32,
}

fn one() -> f32 { 1.0 }

/// One shot of the cut, in order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanSegment {
    /// The brief's feature this shows, by its title; empty for a beat
    /// that is not one (the hook, a breath, the close).
    #[serde(default)]
    pub feature: String,
    pub media_id: MediaId,
    /// The stretch of the file, in seconds.
    pub source_in_secs: f64,
    pub source_out_secs: f64,
    /// What it shows and why it is here: the editor's reasoning, for
    /// whoever approves the plan.
    #[serde(default)]
    pub note: String,
    /// The file's own sound with it. Off by default: a screen recording
    /// is cut to a voice or music placed apart.
    #[serde(default)]
    pub with_audio: bool,
    #[serde(default)]
    pub title: Option<PlanTitle>,
    /// How this segment joins the one before it: a transition's name
    /// ("cross_dissolve", "dip_to_black"); absent, a cut.
    #[serde(default)]
    pub transition: Option<String>,
    #[serde(default)]
    pub zoom: Option<PlanZoom>,
}

impl PlanSegment {
    pub fn duration_secs(&self) -> f64 {
        (self.source_out_secs - self.source_in_secs).max(0.0)
    }
}

/// A music bed under the whole cut, ducked under whatever else is heard.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanMusic {
    pub media_id: MediaId,
    /// How far the music drops under speech, in dB; 9 by default.
    #[serde(default)]
    pub duck_db: Option<f32>,
}

/// The paper edit. `approved` is set by a person (or on their word),
/// never by whoever proposed it; a plan is built only once it is.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EditPlan {
    #[serde(default)]
    pub segments: Vec<PlanSegment>,
    #[serde(default)]
    pub music: Option<PlanMusic>,
    #[serde(default)]
    pub approved: bool,
    /// Anything the reader should know about the plan as a whole.
    #[serde(default)]
    pub notes: String,
}

impl EditPlan {
    /// How long the cut runs: the segments end to end.
    pub fn duration_secs(&self) -> f64 {
        self.segments.iter().map(|s| s.duration_secs()).sum()
    }

    /// The plan as an editor would read it aloud: each segment with
    /// where it lands, what it is, and what goes on it; then the total,
    /// against `target_secs` when the brief has one.
    pub fn text(&self, target_secs: Option<f64>) -> String {
        let mut out = Vec::new();
        out.push(if self.approved { "Edit plan (approved)".to_string() } else { "Edit plan (awaiting approval)".to_string() });
        let mut at = 0.0;
        for (i, s) in self.segments.iter().enumerate() {
            let d = s.duration_secs();
            let mut line = format!("{:>2}. {}  ({:.1} s)  src {:.2}-{:.2}", i + 1, clock(at), d, s.source_in_secs, s.source_out_secs);
            if !s.feature.trim().is_empty() {
                line.push_str(&format!("  [{}]", s.feature));
            }
            if let Some(t) = &s.transition {
                line.push_str(&format!("  join: {t}"));
            }
            if let Some(z) = &s.zoom {
                line.push_str(&format!("  zoom {:.2}->{:.2}", z.from, z.to));
            }
            out.push(line);
            if !s.note.trim().is_empty() {
                out.push(format!("      {}", s.note.trim()));
            }
            if let Some(t) = &s.title {
                let style = if t.style.trim().is_empty() { "heading" } else { t.style.trim() };
                out.push(format!("      title ({style}): \"{}\"", t.text));
            }
            at += d;
        }
        if let Some(m) = &self.music {
            out.push(format!("Music: {} under it all, ducked {} dB", m.media_id.0, m.duck_db.unwrap_or(9.0)));
        }
        match target_secs {
            Some(t) => out.push(format!("Runs {:.1} s; the brief asks for {t:.0} s", self.duration_secs())),
            None => out.push(format!("Runs {:.1} s", self.duration_secs())),
        }
        if !self.notes.trim().is_empty() {
            out.push(format!("Notes: {}", self.notes.trim()));
        }
        out.join("\n")
    }
}

fn clock(secs: f64) -> String {
    let m = (secs / 60.0).floor() as u64;
    let s = secs - (m as f64) * 60.0;
    format!("{m}:{s:05.2}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(a: f64, b: f64) -> PlanSegment {
        PlanSegment { feature: String::new(), media_id: MediaId::new(), source_in_secs: a, source_out_secs: b, note: String::new(), with_audio: false, title: None, transition: None, zoom: None }
    }

    #[test]
    fn the_plan_runs_as_long_as_its_segments_end_to_end() {
        let plan = EditPlan { segments: vec![seg(10.0, 14.5), seg(30.0, 33.0)], ..Default::default() };
        assert!((plan.duration_secs() - 7.5).abs() < 1e-9);
        let text = plan.text(Some(8.0));
        assert!(text.contains("awaiting approval"), "{text}");
        assert!(text.contains("2. 0:04.50  (3.0 s)  src 30.00-33.00"), "{text}");
        assert!(text.contains("Runs 7.5 s; the brief asks for 8 s"), "{text}");
    }

    #[test]
    fn a_plan_from_before_a_field_existed_still_reads() {
        let json = r#"{"segments":[{"media_id":"00000000-0000-0000-0000-000000000000","source_in_secs":1.0,"source_out_secs":2.0}]}"#;
        let plan: EditPlan = serde_json::from_str(json).unwrap();
        assert_eq!(plan.segments.len(), 1);
        assert!(!plan.approved);
        assert_eq!(plan.segments[0].transition, None);
    }
}
