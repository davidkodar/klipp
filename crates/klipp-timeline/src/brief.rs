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
        if !self.notes.trim().is_empty() {
            out.push(format!("Notes: {}", self.notes));
        }
        out.join("\n")
    }
}
