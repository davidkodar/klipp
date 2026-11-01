//! Ready-made caption looks (#3304): an animated caption's motion, face,
//! weight, case, outline, glow and a few colours to choose from, one data
//! file a look under `assets/presets/caption_looks/`. The captions op and
//! the Subtitle Editor take a look by its id and make the caption's
//! [`CaptionAnimation`] from it; a new look is a new file.

use serde::Deserialize;

use crate::{CaptionAnimation, CaptionGlow, CaptionMotion};


/// One look, as its file describes it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptionLook {
    pub id: String,
    pub name: String,
    pub motion: CaptionMotion,
    /// The face; the captions' own when none is named.
    #[serde(default)]
    pub font: Option<String>,
    #[serde(default)]
    pub weight: Option<u16>,
    #[serde(default)]
    pub caps: bool,
    /// In pixels of a 1080-line frame; the captions' own when left out.
    #[serde(default)]
    pub outline: Option<f32>,
    #[serde(default)]
    pub glow: Option<LookGlow>,
    /// The colours offered, the first the look's own. The words, the
    /// spoken word and the glow all take the one chosen.
    #[serde(rename = "colour", default)]
    pub colours: Vec<LookColour>,
}

/// A look's glow at amount 1: its blur radius in pixels of a 1080-line
/// frame and its strength, 0 to 1.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LookGlow {
    pub radius: f32,
    pub intensity: f32,
}

/// A colour a look offers, by name.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LookColour {
    pub name: String,
    /// `#rrggbb`.
    pub colour: String,
}

impl LookColour {
    pub fn rgba(&self) -> Option<[u8; 4]> {
        let hex = self.colour.trim().strip_prefix('#').filter(|h| h.len() == 6 && h.chars().all(|c| c.is_ascii_hexdigit()))?;
        let byte = |k: usize| u8::from_str_radix(&hex[k..k + 2], 16).ok();
        Some([byte(0)?, byte(2)?, byte(4)?, 255])
    }
}

const FILES: [&str; 1] = [include_str!("../../../assets/presets/caption_looks/glow_caps.toml")];

/// Every look, in the menu's order, read once. A file that doesn't read
/// is left out and logged.
pub fn all() -> &'static [CaptionLook] {
    static ALL: std::sync::OnceLock<Vec<CaptionLook>> = std::sync::OnceLock::new();
    ALL.get_or_init(|| {
        FILES
            .iter()
            .filter_map(|text| toml::from_str(text).map_err(|e| tracing::warn!("a caption look file doesn't read: {e}")).ok())
            .collect()
    })
}

/// The look with `id`, in any case.
pub fn by_id(id: &str) -> Option<&'static CaptionLook> {
    all().iter().find(|l| l.id.eq_ignore_ascii_case(id.trim()))
}

impl CaptionLook {
    /// The colour named `name` (in any case), or the look's own.
    pub fn colour(&self, name: Option<&str>) -> Option<&LookColour> {
        match name {
            Some(n) => self.colours.iter().find(|c| c.name.eq_ignore_ascii_case(n.trim())),
            None => self.colours.first(),
        }
    }

    /// The caption this look makes in `colour`, its glow scaled by
    /// `glow` (0 for none, 1 as designed): the radius by it, the strength
    /// by it up to full.
    pub fn animation(&self, colour: &LookColour, glow: f32) -> CaptionAnimation {
        let mut out = CaptionAnimation { style: self.motion, font: self.font.clone(), weight: self.weight, caps: self.caps, ..Default::default() };
        if let Some(o) = self.outline {
            out.outline = o;
        }
        if let Some(c) = colour.rgba() {
            out.text_color = Some(c);
            out.active_color = c;
        }
        let glow = glow.max(0.0);
        out.glow = self.glow.filter(|_| glow > 0.0).map(|g| CaptionGlow {
            color: out.text_color.unwrap_or([255, 255, 255, 255]),
            radius: g.radius * glow,
            intensity: (g.intensity * glow).min(1.0),
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every look's file reads, each with its own id, a name, colours
    /// that are colours, and a face Klipp ships or can fetch.
    #[test]
    fn every_caption_look_reads_and_its_colours_and_face_are_real() {
        let all = all();
        assert_eq!(all.len(), FILES.len(), "every file reads");
        let mut ids: Vec<&str> = all.iter().map(|l| l.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), all.len(), "each its own id");
        for l in all {
            assert!(!l.name.is_empty() && !l.colours.is_empty(), "{}: a name and colours", l.id);
            assert!(l.colours.iter().all(|c| c.rgba().is_some() && !c.name.is_empty()), "{}: colours as #rrggbb, named", l.id);
            assert!(l.weight.is_none_or(|w| (100..=900).contains(&w)), "{}: weight", l.id);
            if let Some(f) = &l.font {
                assert!(klipp_title_catalogue_has(f), "{}: {f} is not in the font catalogue", l.id);
            }
        }
        let g = by_id("Glow_Caps").expect("glow caps");
        let a = g.animation(g.colour(Some("mint")).expect("mint"), 2.0);
        assert!(a.caps && a.weight == Some(800) && a.text_color == Some([0xd6, 0xff, 0xe8, 255]));
        let glow = a.glow.expect("a glow");
        assert_eq!((glow.color, glow.radius), (a.text_color.unwrap(), 36.0), "the glow in the chosen colour, twice as wide");
        assert!(g.animation(g.colour(None).unwrap(), 0.0).glow.is_none(), "glow 0: none");
    }

    /// The catalogue's families, read from its file: klipp-title, which
    /// reads it in the app, is not a dependency of this crate.
    fn klipp_title_catalogue_has(family: &str) -> bool {
        include_str!("../../../assets/fonts/catalogue.toml").lines().any(|l| l.trim() == format!("name = \"{family}\""))
    }
}
