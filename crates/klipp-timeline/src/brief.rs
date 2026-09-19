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
    /// Anything else the editor should know.
    #[serde(default)]
    pub notes: String,
}

impl Brief {
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
