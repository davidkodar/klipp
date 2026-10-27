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
    /// Explicit motion, overriding the style's whole entrance/exit.
    /// Do not combine `enter` with legacy animate/animate_secs/stagger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enter: Option<crate::brief::TitleMotionSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<crate::brief::TitleMotionSpec>,
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
    /// How the push moves; linear when left out, a steady drift.
    #[serde(default, skip_serializing_if = "is_linear")]
    pub ease: klipp_core::keyframe::Ease,
}

fn is_linear(e: &klipp_core::keyframe::Ease) -> bool {
    *e == klipp_core::keyframe::Ease::Linear
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
/// "solid", "circle", "countdown", "placeholder", "color_bars"), with its
/// colours when it takes any. Built as a generator clip below picture and titles.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PlanBackground {
    pub generator: String,
    /// `#rrggbb`, as many as the generator takes: one for a solid, two
    /// for a gradient. Left out, the brief's brand colours.
    #[serde(default)]
    pub colors: Vec<String>,
}

/// How a scene's card comes in or goes (#1926): `kind` is fade, rise,
/// drop, slide_left (in from the right), slide_right (in from the left)
/// or scale_up, over `secs` (0.5 when left out). An exit leaves the way
/// its kind came in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayerMotion {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secs: Option<f64>,
    /// How the move eases; in and out when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ease: Option<klipp_core::keyframe::Ease>,
}

/// The kinds a card's motion takes.
pub const LAYER_MOTIONS: &[&str] = &["fade", "rise", "drop", "slide_left", "slide_right", "scale_up"];

impl LayerMotion {
    /// How long it takes.
    pub fn duration_secs(&self) -> f64 {
        self.secs.unwrap_or(0.5)
    }
}

/// One layer of a scene (#1926): something over a segment's picture
/// with its own start and length inside the segment. `at_secs` is
/// seconds into the segment; a layer later in the list sits above an
/// earlier one it overlaps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlanLayer {
    /// A card: a still, or a stretch of a file from `source_in_secs`,
    /// placed in the frame and moving in and out.
    Media {
        media_id: MediaId,
        #[serde(default)]
        at_secs: f64,
        /// How long it holds; to the segment's end when left out.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        secs: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_in_secs: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placement: Option<PlanPlacement>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enter: Option<LayerMotion>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit: Option<LayerMotion>,
    },
    /// A title, as a segment's takes it, starting `at_secs` in.
    Title {
        #[serde(default)]
        at_secs: f64,
        title: Box<PlanTitle>,
    },
    /// A straight line from `from` to `to` (fractions of the frame),
    /// drawn on from `from` over `reveal_secs` (0.6 when left out).
    Connector {
        from: [f32; 2],
        to: [f32; 2],
        #[serde(default)]
        at_secs: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        secs: Option<f64>,
        /// `#rrggbb`; white when left out.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<String>,
        /// The line's width in pixels of a 1080 frame; 6 when left out.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stroke_width: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reveal_secs: Option<f64>,
    },
}

impl PlanLayer {
    /// Seconds into its segment it starts.
    pub fn at_secs(&self) -> f64 {
        match self {
            Self::Media { at_secs, .. } | Self::Title { at_secs, .. } | Self::Connector { at_secs, .. } => *at_secs,
        }
    }

    /// Where it starts and ends, in seconds into a segment
    /// `segment_secs` long: a title holds as a segment's does, capped at
    /// the segment's end; anything else to the end when `secs` is left out.
    pub fn span(&self, segment_secs: f64) -> (f64, f64) {
        let at = self.at_secs();
        let rest = (segment_secs - at).max(0.0);
        let held = match self {
            Self::Title { title, .. } => title.duration_secs(rest),
            Self::Media { secs, .. } | Self::Connector { secs, .. } => secs.unwrap_or(rest),
        };
        (at, at + held)
    }

    /// What it is, in a few words, for the plan's text.
    fn describe(&self) -> String {
        match self {
            Self::Media { media_id, placement, enter, exit, .. } => {
                let mut out = format!("card {}", media_id.0);
                if let Some(p) = placement {
                    out.push_str(&format!(" at {:.2},{:.2} sized {:.2}", p.x, p.y, p.width));
                }
                let motion = |m: &LayerMotion, way: &str| format!(", {} {way} {:.2} s", klipp_core::quote::word(&m.kind), m.duration_secs());
                if let Some(m) = enter { out.push_str(&motion(m, "in")); }
                if let Some(m) = exit { out.push_str(&motion(m, "out")); }
                out
            }
            Self::Title { title, .. } => format!("title {}", klipp_core::quote::quoted(&title.text)),
            Self::Connector { from, to, reveal_secs, .. } => format!("connector {:.2},{:.2} to {:.2},{:.2}, drawn on over {:.2} s", from[0], from[1], to[0], to[1], reveal_secs.unwrap_or(0.6)),
        }
    }
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
    /// What goes over the picture, each with its own timing: a scene
    /// (#1926). The picture is the scene's base and sets its length.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<PlanLayer>,
}

impl PlanSegment {
    pub fn duration_secs(&self) -> f64 {
        (self.source_out_secs - self.source_in_secs).max(0.0)
    }

    /// Where each pair of its layers that overlap does, in seconds into
    /// the segment: the layers by their place in the list, from 1.
    pub fn layer_overlaps(&self) -> Vec<(usize, usize, f64, f64)> {
        let d = self.duration_secs();
        let spans: Vec<_> = self.layers.iter().map(|l| l.span(d)).collect();
        let mut out = Vec::new();
        for (a, sa) in spans.iter().enumerate() {
            for (b, sb) in spans.iter().enumerate().skip(a + 1) {
                let (from, to) = (sa.0.max(sb.0), sa.1.min(sb.1));
                if to - from > 1e-6 {
                    out.push((a + 1, b + 1, from, to));
                }
            }
        }
        out
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
        use klipp_core::quote::{quoted, word};
        let mut out = Vec::new();
        out.push(if self.approved { "Edit plan (approved)".to_string() } else { "Edit plan (awaiting approval)".to_string() });
        let mut at = 0.0;
        for (i, s) in self.segments.iter().enumerate() {
            let d = s.duration_secs();
            let mut line = format!("{:>2}. {}  ({:.1} s)  src {:.2}-{:.2}", i + 1, clock(at), d, s.source_in_secs, s.source_out_secs);
            if !s.feature.trim().is_empty() {
                line.push_str(&format!("  [{}]", quoted(&s.feature)));
            }
            if let Some(t) = &s.transition {
                line.push_str(&format!("  join: {}", word(t)));
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
                line.push_str(&format!("  over {}", word(&b.generator)));
            }
            if !s.section.trim().is_empty() {
                line.push_str(&format!("  [{} section]", quoted(s.section.trim())));
            }
            out.push(line);
            if !s.note.trim().is_empty() {
                out.push(format!("      note {}", quoted(s.note.trim())));
            }
            if let Some(t) = &s.title {
                let style = if t.style.trim().is_empty() { "heading".to_string() } else { word(t.style.trim()) };
                let mut extras = Vec::new();
                match t.full_frame {
                    Some(true) => extras.push("full-frame: covers the shot".into()),
                    Some(false) => extras.push("overlay: keeps the shot visible".into()),
                    None if style.eq_ignore_ascii_case("card") => extras.push("full-frame by default: covers the shot; full_frame false makes it an overlay".into()),
                    None => {}
                }
                if let Some(x) = t.x { extras.push(format!("x {x:.2}")); }
                if let Some(y) = t.y { extras.push(format!("y {y:.2}")); }

                if let Some(a) = &t.animate {
                    extras.push(match &t.stagger {
                        Some(st) => format!("{} by {}", word(a), word(st)),
                        None => word(a),
                    });
                }
                if let Some(m) = &t.enter { extras.push(m.approval_text(false)); }
                if let Some(m) = &t.exit { extras.push(m.approval_text(true)); }
                if t.enter.is_some() || t.exit.is_some() {
                    let window = t.duration_secs(s.duration_secs());
                    extras.push(format!("title window {window} s"));
                    let span = |m: &crate::brief::TitleMotionSpec| m.span_secs(&t.text);
                    let entrance = t.enter.as_ref().filter(|m| !m.kind.eq_ignore_ascii_case("none")).map_or(0.0,span);
                    let exit = t.exit.as_ref().filter(|m| !m.kind.eq_ignore_ascii_case("none")).map_or(0.0,span);
                    if entrance > window { extras.push("entrance continues past the title window".into()); }
                    if exit > window { extras.push("exit is already in progress at the title start".into()); }
                    if entrance > 0.0 && exit > 0.0 && entrance + exit > window { extras.push("entrance and exit overlap".into()); }
                }
                if t.enter.is_none() && t.animate.is_none() && (t.animate_secs.is_some() || t.stagger.is_some()) {
                    extras.push("legacy timing/stagger overrides apply only if the inherited entrance is active; otherwise ignored".into());
                }
                if t.glow.is_some() {
                    extras.push("glow".into());
                }
                if t.gradient.is_some() {
                    extras.push("gradient".into());
                }
                let extras = if extras.is_empty() { String::new() } else { format!("  {}", extras.join(", ")) };
                out.push(format!("      title ({style}): {}{extras}", quoted(&t.text)));
            }
            if let Some(x) = &s.sfx {
                out.push(format!("      sfx {} at +{:.2} s", x.media_id.0, x.at_secs.unwrap_or(0.0)));
            }
            // A scene's layers where the timeline will have them, and
            // where they overlap (#1926).
            for (k, layer) in s.layers.iter().enumerate() {
                let (from, to) = layer.span(d);
                out.push(format!("      layer {} {}-{}  {}", k + 1, clock(at + from), clock(at + to), layer.describe()));
            }
            let overlaps = s.layer_overlaps();
            if !overlaps.is_empty() {
                let list: Vec<String> = overlaps.iter().map(|(a, b, from, to)| format!("{a}-{b} {}-{}", clock(at + from), clock(at + to))).collect();
                out.push(format!("      overlap: layers {}", list.join(", ")));
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
            out.push(format!("Notes: {}", quoted(self.notes.trim())));
        }
        out.join("\n")
    }
}

fn clock(secs: f64) -> String {
    let m = (secs / 60.0).floor() as u64;
    let s = secs - (m as f64) * 60.0;
    format!("{m}:{s:05.2}")
}


impl PlanTitle {
    /// Duration used by both plan validation and title-op lowering.
    pub fn duration_secs(&self, segment_secs: f64) -> f64 {
        self.secs.filter(|s|*s>0.0).unwrap_or(3.0).min(segment_secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(a: f64, b: f64) -> PlanSegment {
        PlanSegment { feature: String::new(), media_id: MediaId::new(), source_in_secs: a, source_out_secs: b, note: String::new(), with_audio: false, title: None, transition: None, sfx: None, zoom: None, placement: None, background: None, section: String::new(), layers: Vec::new() }
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
