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
    /// The text's colour, `#rrggbb`; left out, the brand's first colour
    /// (white on a card).
    #[serde(default)]
    pub color: Option<String>,
    /// A card's fill, or a heading's or callout's backing panel,
    /// `#rrggbb`; left out, the brand's first colour for a card and a
    /// dark panel otherwise.
    #[serde(default)]
    pub fill: Option<String>,
    /// A glow round the letters, 0 to 1, in `glow_color` (`#rrggbb`;
    /// the text's colour when left out).
    #[serde(default)]
    pub glow: Option<f32>,
    #[serde(default)]
    pub glow_color: Option<String>,
    /// A vertical gradient fill, top and bottom colours, `#rrggbb`.
    #[serde(default)]
    pub gradient: Option<[String; 2]>,
    /// How it comes in: fade, rise, drop, slide_left, slide_right,
    /// scale_up, scale_down, blur or typewriter; the style's own when
    /// left out. `animate_secs` is how long that takes; `stagger`
    /// (char, word, line) has the letters, words or lines arrive one
    /// by one.
    #[serde(default)]
    pub animate: Option<String>,
    #[serde(default)]
    pub animate_secs: Option<f64>,
    #[serde(default)]
    pub stagger: Option<String>,
    /// Where it sits, as fractions of the frame: 0,0 is the top left,
    /// 1,1 the bottom right, and the point is the title's centre. Left
    /// out, the style decides.
    #[serde(default)]
    pub x: Option<f32>,
    #[serde(default)]
    pub y: Option<f32>,
    /// Whether it fills the frame and hides the picture under it.
    /// A "card" does by nature; this says so for any style, and lets a
    /// card sit over the shot instead.
    #[serde(default)]
    pub full_frame: Option<bool>,
}

/// A sound effect on a segment: a whoosh on the cut, a click on a
/// reveal. It sits on a track of its own, marked as an effect to the
/// mix.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PlanSfx {
    pub media_id: MediaId,
    /// Seconds after the segment starts; 0 (the cut itself) when left
    /// out.
    #[serde(default)]
    pub at_secs: Option<f64>,
    /// Where in the file it starts, in seconds; 0 when left out.
    #[serde(default)]
    pub source_in_secs: Option<f64>,
    /// How much of the file, in seconds; to the file's end, at most
    /// eight seconds, when left out.
    #[serde(default)]
    pub secs: Option<f64>,
    /// Its level in dB (0 as it is).
    #[serde(default)]
    pub volume_db: Option<f32>,
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

/// Where a segment's picture sits in the frame, as fractions of it:
/// `x`,`y` is the centre, `width`,`height` the size. Full frame is
/// 0.5, 0.5, 1, 1 — and anything smaller leaves room for whatever is
/// behind it, which is the point.
///
/// A zoom magnifies a picture that still fills the frame; this places
/// the picture itself, so two of them can sit side by side.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanPlacement {
    #[serde(default = "half")]
    pub x: f32,
    #[serde(default = "half")]
    pub y: f32,
    #[serde(default = "one")]
    pub width: f32,
    #[serde(default = "one")]
    pub height: f32,
    /// Turned, in degrees, clockwise about its own centre.
    #[serde(default)]
    pub rotation_deg: f32,
    /// How much of it is seen, 0 to 1; 1 when left out.
    #[serde(default)]
    pub opacity: Option<f32>,
}

fn half() -> f32 { 0.5 }

impl Default for PlanPlacement {
    fn default() -> Self {
        Self { x: 0.5, y: 0.5, width: 1.0, height: 1.0, rotation_deg: 0.0, opacity: None }
    }
}

impl PlanPlacement {
    /// Whether this is what a clip does anyway: the whole frame, square
    /// on, centred and fully seen. Anything else leaves some of the frame
    /// to see past, which is when a background matters.
    pub fn fills_frame(&self) -> bool {
        self.width >= 1.0
            && self.height >= 1.0
            && self.rotation_deg == 0.0
            && self.x == 0.5
            && self.y == 0.5
            && self.opacity.unwrap_or(1.0) >= 1.0
    }
}

/// What goes behind a segment whose picture does not fill the frame:
/// a generator by name ("gradient", "living_gradient", "grain",
/// "solid"), with its colours when it takes any.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PlanBackground {
    pub generator: String,
    /// `#rrggbb`, as many as the generator takes: one for a solid, two
    /// for a gradient. Left out, the brief's brand colours.
    #[serde(default)]
    pub colors: Vec<String>,
}

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
    /// A sound effect with the segment.
    #[serde(default)]
    pub sfx: Option<PlanSfx>,
    /// Where the picture sits in the frame. Left out, it fills it.
    #[serde(default)]
    pub placement: Option<PlanPlacement>,
    /// What is behind it when the placement leaves room.
    #[serde(default)]
    pub background: Option<PlanBackground>,
    /// A name shared by consecutive segments that belong together — a
    /// montage, a title sequence. A section is cut to its own rhythm
    /// and read by its own rules: shots inside a montage are short on
    /// purpose, and are not held long enough to read one by one.
    #[serde(default)]
    pub section: String,
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
    /// Where in the file the bed starts, in seconds: put the first
    /// downbeat at 0 of the cut. 0 when left out.
    #[serde(default)]
    pub source_in_secs: Option<f64>,
    /// The bed's level in dB (0 as it is), and its fades in seconds.
    #[serde(default)]
    pub volume_db: Option<f32>,
    #[serde(default)]
    pub fade_in_secs: Option<f64>,
    #[serde(default)]
    pub fade_out_secs: Option<f64>,
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
    /// Cut on the music's bars: each segment's length is rounded to
    /// whole bars of the bed (half bars under one), and the bed starts
    /// on its first beat, when the plan is set.
    #[serde(default)]
    pub snap_to_bars: bool,
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
            if let Some(pl) = &s.placement {
                if !pl.fills_frame() {
                    line.push_str(&format!("  at {:.2},{:.2} sized {:.2}x{:.2}", pl.x, pl.y, pl.width, pl.height));
                }
                if pl.rotation_deg != 0.0 {
                    line.push_str(&format!(" turned {:.0}°", pl.rotation_deg));
                }
            }
            if let Some(b) = &s.background {
                line.push_str(&format!("  over {}", b.generator));
            }
            if !s.section.trim().is_empty() {
                line.push_str(&format!("  [{} section]", s.section.trim()));
            }
            out.push(line);
            if !s.note.trim().is_empty() {
                out.push(format!("      {}", s.note.trim()));
            }
            if let Some(t) = &s.title {
                let style = if t.style.trim().is_empty() { "heading" } else { t.style.trim() };
                let mut extras = Vec::new();
                if let Some(a) = &t.animate {
                    extras.push(match &t.stagger {
                        Some(st) => format!("{a} by {st}"),
                        None => a.clone(),
                    });
                }
                if t.glow.is_some() {
                    extras.push("glow".into());
                }
                if t.gradient.is_some() {
                    extras.push("gradient".into());
                }
                let extras = if extras.is_empty() { String::new() } else { format!("  {}", extras.join(", ")) };
                out.push(format!("      title ({style}): \"{}\"{extras}", t.text));
            }
            if let Some(x) = &s.sfx {
                out.push(format!("      sfx {} at +{:.2} s", x.media_id.0, x.at_secs.unwrap_or(0.0)));
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
        PlanSegment { feature: String::new(), media_id: MediaId::new(), source_in_secs: a, source_out_secs: b, note: String::new(), with_audio: false, title: None, transition: None, sfx: None, zoom: None, placement: None, background: None, section: String::new() }
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
