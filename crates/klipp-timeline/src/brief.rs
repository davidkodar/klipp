//! The brief: what a piece is for, said once, where everything that
//! makes or checks the piece can read it — its length, its shape, its
//! loudness, who is in it. An editor is handed one before they start;
//! so is an agent.

use serde::{Deserialize, Serialize};

/// Someone who appears in the piece. Names and roles on screen come
/// from here, spelt as they are here, never from a guess.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CastMember {
    pub name: String,
    /// What goes under the name: a title, a role, a place.
    #[serde(default)]
    pub role: String,
    /// How to tell them on screen or in the sound, for whoever is
    /// placing the lower third: "on the right, long hair", "the host".
    #[serde(default)]
    pub described: String,
}

/// Something the piece shows off: a feature of the product, a beat of
/// the story. The edit plan's segments name these, so every feature
/// the brief ranks can be checked for in the cut.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Feature {
    pub title: String,
    /// What it is and what the viewer should take from seeing it.
    #[serde(default)]
    pub description: String,
    /// 1 is the headline; higher numbers matter less. 0 is unranked.
    #[serde(default)]
    pub rank: u32,
}

/// What the piece is for. Every field is optional in the sense that an
/// empty or absent one asks for nothing; what is given is checked.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Brief {
    #[serde(default)]
    pub title: String,
    /// What the piece is about, in a sentence or two.
    #[serde(default)]
    pub about: String,
    /// How it should feel: "tight, no fuss", "slow and warm".
    #[serde(default)]
    pub tone: String,
    /// Where it goes: "web", "broadcast", "instagram reel".
    #[serde(default)]
    pub platform: String,
    /// How long it should run, in seconds.
    #[serde(default)]
    pub target_secs: Option<f64>,
    /// The delivery's picture size.
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    /// The delivery's integrated loudness, in LUFS: about -14 for the
    /// web, -23 for broadcast.
    #[serde(default)]
    pub loudness_lufs: Option<f32>,
    /// Whether the piece is captioned.
    #[serde(default)]
    pub captions: bool,
    /// Whether it has music under it.
    #[serde(default)]
    pub music: bool,
    #[serde(default)]
    pub cast: Vec<CastMember>,
    /// What it shows, ranked: the features a promo walks through.
    #[serde(default)]
    pub features: Vec<Feature>,
    /// The shape of the piece: "hook, three features, call to action".
    #[serde(default)]
    pub arc: String,
    /// What the viewer is asked to do at the end: "try it free at …".
    #[serde(default)]
    pub cta: String,
    /// The brand's colours as `#rrggbb`, first the one titles take.
    #[serde(default)]
    pub brand_colors: Vec<String>,
    /// How things are spelt: names, products and terms of art the
    /// speech-to-text would otherwise guess at ("Omarchy", "QML"). The
    /// cast's names count without being listed.
    #[serde(default)]
    pub vocabulary: Vec<String>,
    /// The language the speech is in, as an ISO 639-1 code ("en", "sv")
    /// for the speech-to-text (#531). Left out, it is detected per file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Anything else the editor should know.
    #[serde(default)]
    pub notes: String,
    /// The brand's type system: named looks for titles. A title op or
    /// a plan title names one by `style`; its own fields override the
    /// look's. `heading`, `callout` and `card` are built in and a brief
    /// style of the same name replaces the built-in one.
    #[serde(default)]
    pub type_styles: Vec<TypeStyle>,
    /// The look the piece is graded to, as numbers: black, white and
    /// mid lightness, contrast, chroma, the cast on neutrals, skin hue.
    /// `{name: "web_punchy"}` alone takes a preset (broadcast_neutral,
    /// web_punchy, film_soft, warm_interview, golden, cool_tech, screencast);
    /// style_profile lifts one from a reference. Left out: neutral. It is
    /// the camera footage's look: a screen recording, or a clip marked
    /// graphics, is judged as a screen whatever it says (#1906).
    #[serde(default)]
    pub look: Option<klipp_color::measure::LookTarget>,
}

/// A named look for titles, every field optional: what the brief
/// settles once so every title agrees.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TypeStyle {
    pub name: String,
    #[serde(default)]
    pub font: Option<String>,
    #[serde(default)]
    pub bold: Option<bool>,
    /// In px of a 1080-line frame.
    #[serde(default)]
    pub size: Option<f32>,
    /// `#rrggbb`; left out, the brand's first colour (white on a card).
    #[serde(default)]
    pub color: Option<String>,
    /// A card's fill or a backing panel's colour, `#rrggbb`.
    #[serde(default)]
    pub fill: Option<String>,
    /// A dark panel behind the words, 0 to 1.
    #[serde(default)]
    pub backing: Option<f32>,
    /// A full-frame card behind everything.
    #[serde(default)]
    pub full_frame: Option<bool>,
    /// Where the words sit, as fractions of the frame (centre).
    #[serde(default)]
    pub x: Option<f32>,
    #[serde(default)]
    pub y: Option<f32>,
    #[serde(default)]
    pub align: Option<String>,
    #[serde(default)]
    pub outline: Option<f32>,
    #[serde(default)]
    pub outline_color: Option<String>,
    #[serde(default)]
    pub glow: Option<f32>,
    #[serde(default)]
    pub glow_color: Option<String>,
    #[serde(default)]
    pub gradient: Option<[String; 2]>,
    #[serde(default)]
    pub animate: Option<String>,
    /// Explicit motion, overriding the style's whole entrance/exit.
    /// Do not combine `enter` with legacy animate/animate_secs/stagger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enter: Option<crate::brief::TitleMotionSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<crate::brief::TitleMotionSpec>,
    #[serde(default)]
    pub animate_secs: Option<f32>,
    #[serde(default)]
    pub stagger: Option<String>,
    #[serde(default)]
    pub loop_effect: Option<String>,
    /// A drop shadow behind the words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow: Option<ShadowSpec>,
    /// A metal face by its preset's name (#2706): gold, silver, steel,
    /// copper, bronze, chrome or brushed_metal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<String>,
}

/// A drop shadow behind a title's text, what keeps bright words
/// readable over a bright sky. `x` and `y` move it in pixels of a
/// 1080-line frame (right and down for positive), `blur` softens it in
/// the same pixels, `color` is `#rrggbb`, `opacity` 0 to 1. Left out,
/// they are 0, 4, 8, black and 0.6.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShadowSpec {
    #[serde(default)]
    pub x: Option<f32>,
    #[serde(default)]
    pub y: Option<f32>,
    #[serde(default)]
    pub blur: Option<f32>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub opacity: Option<f32>,
}

impl TypeStyle {
    /// The three looks every brief has unless it says otherwise: a
    /// heading in the top tenth that rises in, a callout low and backed
    /// that fades in, a full-frame card.
    pub fn builtin(name: &str) -> Option<TypeStyle> {
        let base = TypeStyle { name: name.to_string(), x: Some(0.5), bold: Some(true), ..Default::default() };
        Some(match name {
            "heading" => TypeStyle { y: Some(0.10), size: Some(72.0), backing: Some(0.45), animate: Some("rise".into()), animate_secs: Some(0.4), ..base },
            "callout" => TypeStyle { y: Some(0.85), size: Some(40.0), bold: Some(false), backing: Some(0.6), animate: Some("fade".into()), animate_secs: Some(0.3), ..base },
            "card" => TypeStyle { y: Some(0.5), size: Some(84.0), full_frame: Some(true), color: Some("#ffffff".into()), fill: Some("#101418".into()), animate: Some("fade".into()), animate_secs: Some(0.4), ..base },
            _ => return None,
        })
    }

    /// The names of the built-in looks.
    pub const BUILTIN: [&'static str; 3] = ["heading", "callout", "card"];
}

impl Brief {
    /// The type style called `name`: the brief's own first, then a
    /// built-in one.
    pub fn type_style(&self, name: &str) -> Option<TypeStyle> {
        let want = name.trim().to_lowercase();
        self.type_styles.iter().find(|s| s.name.trim().to_lowercase() == want).cloned().or_else(|| TypeStyle::builtin(&want))
    }

    /// Every style name a title may use: the brief's, then the built-in
    /// ones it does not replace.
    pub fn type_style_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.type_styles.iter().map(|s| s.name.trim().to_lowercase()).filter(|n| !n.is_empty()).collect();
        for b in TypeStyle::BUILTIN {
            if !names.iter().any(|n| n == b) {
                names.push(b.to_string());
            }
        }
        names
    }

    /// The cast member called `name`, compared without case or outer space.
    pub fn cast_member(&self, name: &str) -> Option<&CastMember> {
        let want = name.trim().to_lowercase();
        self.cast.iter().find(|c| c.name.trim().to_lowercase() == want)
    }

    /// Every name and term whose spelling the brief settles: the cast's
    /// names, then the vocabulary.
    pub fn spellings(&self) -> Vec<String> {
        self.cast.iter().map(|c| c.name.clone()).chain(self.vocabulary.iter().cloned()).filter(|w| !w.trim().is_empty()).collect()
    }

    /// The feature called `title`, compared without case or outer space.
    pub fn feature(&self, title: &str) -> Option<&Feature> {
        let want = title.trim().to_lowercase();
        self.features.iter().find(|f| f.title.trim().to_lowercase() == want)
    }

    /// The features in rank order, the unranked last, ties as listed.
    pub fn ranked_features(&self) -> Vec<&Feature> {
        let mut v: Vec<&Feature> = self.features.iter().collect();
        v.sort_by_key(|f| if f.rank == 0 { u32::MAX } else { f.rank });
        v
    }

    /// The first brand colour, the one titles take, if any.
    pub fn primary_color(&self) -> Option<&str> {
        self.brand_colors.iter().map(|c| c.trim()).find(|c| !c.is_empty())
    }

    /// How far a length may be from the target and still be the brief's:
    /// a twentieth of it, and never under two seconds.
    pub fn length_tolerance_secs(&self) -> Option<f64> {
        self.target_secs.map(|t| (t * 0.05).max(2.0))
    }

    /// The brief as a few lines of text.
    pub fn text(&self) -> String {
        let mut out = Vec::new();
        let mut line = |label: &str, v: String| {
            if !v.trim().is_empty() {
                out.push(format!("{label}: {v}"));
            }
        };
        line("Title", self.title.clone());
        line("About", self.about.clone());
        line("Tone", self.tone.clone());
        line("Platform", self.platform.clone());
        line("Length", self.target_secs.map(|s| format!("{s:.0} s")).unwrap_or_default());
        line("Picture", match (self.width, self.height) { (Some(w), Some(h)) => format!("{w}x{h}"), _ => String::new() });
        line("Loudness", self.loudness_lufs.map(|l| format!("{l:.0} LUFS")).unwrap_or_default());
        line("Captions", if self.captions { "yes".into() } else { String::new() });
        line("Music", if self.music { "yes".into() } else { String::new() });
        for c in &self.cast {
            let mut who = c.name.clone();
            if !c.role.is_empty() {
                who.push_str(&format!(", {}", c.role));
            }
            if !c.described.is_empty() {
                who.push_str(&format!(" ({})", c.described));
            }
            out.push(format!("Cast: {who}"));
        }
        if let Some(look) = &self.look {
            out.push(format!("Look: {}{}", if look.name.is_empty() { "custom" } else { look.name.as_str() }, look.mid_l.map(|m| format!(" (mid L* {m:.0}, contrast {:.0}, chroma {:.0})", look.contrast_range.unwrap_or(0.0), look.chroma_mean.unwrap_or(0.0))).unwrap_or_default()));
        }
        if !self.type_styles.is_empty() {
            let looks: Vec<String> = self.type_styles.iter().map(|s| {
                let mut bits = Vec::new();
                if let Some(f) = &s.font { bits.push(f.clone()); }
                if let Some(z) = s.size { bits.push(format!("{z:.0} px")); }
                if let Some(c) = &s.color { bits.push(c.clone()); }
                if let Some(a) = &s.animate { bits.push(a.clone()); }
                if let Some(m) = &s.enter { bits.push(m.approval_text(false)); }
                if let Some(m) = &s.exit { bits.push(m.approval_text(true)); }
                if bits.is_empty() { s.name.clone() } else { format!("{} ({})", s.name, bits.join(", ")) }
            }).collect();
            out.push(format!("Type styles: {}", looks.join("; ")));
        }
        for f in self.ranked_features() {
            let mut what = f.title.clone();
            if !f.description.trim().is_empty() {
                what.push_str(&format!(" — {}", f.description));
            }
            if f.rank > 0 {
                out.push(format!("Feature {}: {what}", f.rank));
            } else {
                out.push(format!("Feature: {what}"));
            }
        }
        if !self.arc.trim().is_empty() {
            out.push(format!("Arc: {}", self.arc));
        }
        if !self.cta.trim().is_empty() {
            out.push(format!("Call to action: {}", self.cta));
        }
        if !self.brand_colors.is_empty() {
            out.push(format!("Colours: {}", self.brand_colors.join(", ")));
        }
        if !self.vocabulary.is_empty() {
            out.push(format!("Spelt: {}", self.vocabulary.join(", ")));
        }
        if !self.notes.trim().is_empty() {
            out.push(format!("Notes: {}", self.notes));
        }
        out.join("\n")
    }
}

/// Agent-facing entrance or exit configuration. Names use snake_case; this
/// lowers to the existing renderer Motion without changing its serialization.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TitleMotionSpec {
    /// A supported MotionKind name, or "none" to disable this side.
    pub kind: String,
    #[serde(default)]
    /// Positive seconds, at most 5; default 0.6.
    pub duration_secs: Option<f32>,
    /// Entrance: delay after clip start. Exit: lead time before clip end.
    /// Zero to 3600 seconds; default 0.
    #[serde(default)]
    pub delay_secs: Option<f32>,
    #[serde(default)]
    pub easing: Option<String>,
    #[serde(default)]
    pub stagger: Option<String>,
    #[serde(default)]
    /// Zero to 60 seconds; default 0.05 (typewriter defaults to char/0.04).
    pub stagger_secs: Option<f32>,
    /// Font-size fraction for rise/drop/slide_left/slide_right only. Other
    /// kinds reject this field. Zero to 100; default 0.6.
    #[serde(default)]
    pub distance: Option<f32>,
    /// How far grow, shrink, scale_up and scale_down scale, as a factor
    /// of the resting size: above 1 for grow and scale_down, below 1 for
    /// shrink and scale_up (#736). Other kinds reject this field.
    #[serde(default)]
    pub scale: Option<f32>,
    /// What grow, shrink, scale_up, scale_down, pop, slam and focus_in
    /// scale about: "solid" (the point deepest inside the letters, so a
    /// word grown past the frame fills it with a stroke), "centre", or
    /// {x, y} as fractions of the title's box (#787).
    #[serde(default)]
    pub anchor: Option<MotionAnchorSpec>,
}

/// A motion anchor as written: a name, or a point of the title's box.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MotionAnchorSpec {
    Named(String),
    Point { x: f32, y: f32 },
}

impl TitleMotionSpec {
    /// Approval timing uses the same defaults and implicit typewriter unit as lowering.
    pub fn span_secs(&self, text: &str) -> f64 {
        let defaults = klipp_core::title_motion::Motion::default();
        let typewriter = matches!(self.kind.to_lowercase().as_str(), "typewriter" | "type");
        let unit = self.stagger.as_deref().map(str::to_lowercase)
            .unwrap_or_else(|| if typewriter { "char" } else { "none" }.into());
        let units = match unit.as_str() {
            "word" => text.split_whitespace().count(), "line" => text.lines().count(),
            "char" | "character" => text.chars().filter(|c| !c.is_whitespace()).count(), _ => 1,
        };
        let stagger = self.stagger_secs.unwrap_or(if typewriter && self.stagger.is_none() {0.04} else {defaults.stagger_secs});
        f64::from(self.delay_secs.unwrap_or(defaults.delay_secs) + self.duration_secs.unwrap_or(defaults.duration_secs)
            + units.saturating_sub(1) as f32 * stagger)
    }
    /// Motion details shown before a human approves a brief or edit plan.
    pub fn approval_text(&self, exit: bool) -> String {
        let side = if exit { "exit" } else { "enter" };
        if self.kind.eq_ignore_ascii_case("none") { return format!("{side}: disabled"); }
        let mut bits = vec![format!("{side}: {}", self.kind)];
        if let Some(v) = self.duration_secs { bits.push(format!("duration {v} s")); }
        if let Some(v) = self.delay_secs {
            bits.push(if exit { format!("exits {v} s early") } else { format!("delay {v} s") });
        }
        if let Some(v) = &self.stagger { bits.push(format!("by {v}")); }
        else if matches!(self.kind.to_lowercase().as_str(), "typewriter" | "type") { bits.push("by char".into()); }
        if let Some(v) = self.stagger_secs { bits.push(format!("stagger {v} s")); }
        if let Some(v) = &self.easing { bits.push(format!("easing {v}")); }
        if let Some(v) = self.distance { bits.push(format!("distance {v}")); }
        bits.join(", ")
    }
}
