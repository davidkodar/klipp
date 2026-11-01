//! Animated captions (#2710): captions that show a few words at a time and
//! mark the word being spoken, popping it, colouring it, lifting it, or
//! typing the words out as they are said.
//!
//! Built from the title's own animators: a range selector over the words,
//! its start and end keyed in steps at the words' times, so a caption
//! made again from a corrected transcript is timed again, and every part
//! of it can be changed in the inspector afterwards.

use klipp_core::keyframe::{Interpolation, Keyframe};
use klipp_core::title_motion::{Animator, AnimatorProps, RangeSelector, SelectShape, SelectUnit};
use serde::{Deserialize, Serialize};

use crate::{TextAlignment, TextBox, TextOutline, TitleData, TitlePosition};

/// How the word being spoken is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CaptionMotion {
    /// The word being spoken grows and takes the active colour.
    #[default]
    Pop,
    /// The word being spoken takes the active colour.
    Highlight,
    /// The words said so far take the active colour, filling the line.
    Karaoke,
    /// The word being spoken jumps up and takes the active colour.
    Bounce,
    /// The word being spoken sits on a box of the active colour, dark.
    Box,
    /// Each word appears as it is said.
    Typewriter,
}

impl CaptionMotion {
    /// Every style, as the ops and the captions form name them.
    pub const ALL: [CaptionMotion; 6] = [Self::Pop, Self::Highlight, Self::Karaoke, Self::Bounce, Self::Box, Self::Typewriter];

    /// As the captions form lists it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Pop => "Pop",
            Self::Highlight => "Highlight",
            Self::Karaoke => "Karaoke",
            Self::Bounce => "Bounce",
            Self::Box => "Box",
            Self::Typewriter => "Typewriter",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Pop => "pop",
            Self::Highlight => "highlight",
            Self::Karaoke => "karaoke",
            Self::Bounce => "bounce",
            Self::Box => "box",
            Self::Typewriter => "typewriter",
        }
    }
}

/// Where animated captions sit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CaptionPlace {
    /// Low, above the area a phone's interface covers on a tall frame.
    #[default]
    Lower,
    /// In the middle of the frame.
    Centre,
}

/// An animated caption's look (#2710).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptionAnimation {
    #[serde(default)]
    pub style: CaptionMotion,
    /// Words on screen at a time, 1 to 5.
    #[serde(default = "default_words")]
    pub words_per_screen: u8,
    /// The colour the word being spoken takes (#rrggbb in the ops).
    #[serde(default = "default_active")]
    pub active_color: [u8; 4],
    /// How far a pop grows or a bounce lifts, 1 as designed (0 to 3).
    #[serde(default = "default_amount")]
    pub amount: f32,
    #[serde(default)]
    pub position: CaptionPlace,
    /// Words set in the emphasis colour wherever they come, in any case.
    #[serde(default)]
    pub emphasis: Vec<String>,
    #[serde(default = "default_emphasis")]
    pub emphasis_color: [u8; 4],
    /// The face; the bundled sans when none is named.
    #[serde(default)]
    pub font: Option<String>,
    /// The outline's width in pixels of a 1080-line frame.
    #[serde(default = "default_outline")]
    pub outline: f32,
    /// The words' colour; white when none is given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_color: Option<[u8; 4]>,
    /// The weight, 100 to 900; bold when none is given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<u16>,
    /// Set in capitals, whatever the transcript's case.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub caps: bool,
    /// A soft glow round the words (#3304).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glow: Option<CaptionGlow>,
}

/// A caption's glow: its colour, its blur radius in pixels of a
/// 1080-line frame, and its strength, 0 to 1.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptionGlow {
    pub color: [u8; 4],
    pub radius: f32,
    pub intensity: f32,
}

fn default_words() -> u8 {
    3
}
fn default_active() -> [u8; 4] {
    [255, 214, 10, 255]
}
fn default_amount() -> f32 {
    1.0
}
fn default_emphasis() -> [u8; 4] {
    [64, 200, 255, 255]
}
fn default_outline() -> f32 {
    6.0
}

impl Default for CaptionAnimation {
    fn default() -> Self {
        Self {
            style: CaptionMotion::default(),
            words_per_screen: default_words(),
            active_color: default_active(),
            amount: default_amount(),
            position: CaptionPlace::default(),
            emphasis: Vec::new(),
            emphasis_color: default_emphasis(),
            font: None,
            outline: default_outline(),
            text_color: None,
            weight: None,
            caps: false,
            glow: None,
        }
    }
}

/// A word heard, in seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct TimedWord {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

/// One screen of words and its time on the timeline, in seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct CaptionScreen {
    pub words: Vec<TimedWord>,
    pub start: f64,
    pub end: f64,
}

/// A pause at least this long starts a new screen, whatever its count.
const PAUSE: f64 = 0.6;

/// How long the last screen stays after its last word, at most.
const LINGER: f64 = 0.4;

/// `words` (in time order) in screens of `per_screen` at most, a new one
/// after a pause or a sentence's end; each screen runs from its first
/// word to the next screen's, its last word's end plus a linger after a
/// pause, and never past `until`.
pub fn screens(words: &[TimedWord], per_screen: u8, until: f64) -> Vec<CaptionScreen> {
    let per = per_screen.clamp(1, 5) as usize;
    let mut out: Vec<CaptionScreen> = Vec::new();
    for w in words {
        let start_new = match out.last() {
            None => true,
            Some(s) => {
                let last = s.words.last().expect("a screen has words");
                s.words.len() >= per || w.start - last.end >= PAUSE || last.text.ends_with(['.', '!', '?'])
            }
        };
        if start_new {
            out.push(CaptionScreen { words: Vec::new(), start: w.start, end: w.end });
        }
        let s = out.last_mut().expect("just made");
        s.end = w.end;
        s.words.push(w.clone());
    }
    for i in 0..out.len() {
        let next = out.get(i + 1).map(|n| n.start);
        let s = &mut out[i];
        s.end = match next {
            Some(n) if n - s.end < PAUSE => n,
            Some(n) => (s.end + LINGER).min(n),
            None => s.end + LINGER,
        }
        .min(until)
        .max(s.start + 0.05);
    }
    out
}

fn key(time_secs: f64, value: f32) -> Keyframe {
    Keyframe { time_secs, value, interpolation: Interpolation::Step, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }
}

/// The caption for `screen`, on a `width` × `height` frame, as `anim`
/// says: its words on one line, the animators keyed at their times from
/// the screen's start.
pub fn animated_caption(screen: &CaptionScreen, anim: &CaptionAnimation, width: u32, height: u32) -> TitleData {
    let text = screen.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
    let tall = height as f32 > width as f32 * 1.2;
    let mut tb = TextBox {
        text: text.clone(),
        font_size: if tall { 84.0 } else { 64.0 },
        font_name: anim.font.clone().unwrap_or_else(|| "Inter".into()),
        font_style: crate::FontStyle::Bold,
        font_weight: anim.weight,
        case_transform: if anim.caps { crate::TextCase::Uppercase } else { crate::TextCase::default() },
        color: anim.text_color.unwrap_or([255, 255, 255, 255]),
        alignment: TextAlignment::Center,
        ..Default::default()
    };
    for word in &anim.emphasis {
        let word = word.trim().to_lowercase();
        let mut at = 0;
        for w in text.split(' ') {
            let bare = w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
            if !word.is_empty() && bare == word {
                crate::apply_style_to_range(&mut tb.style_runs, text.len(), at, at + w.len(), |r| r.color = Some(anim.emphasis_color));
            }
            at += w.len() + 1;
        }
    }
    let mut td = TitleData::single_text(tb);
    let fx = td.objects[0].fx_mut();
    fx.outlines.push(TextOutline { color: [0, 0, 0, 255], width: anim.outline.max(0.0), enabled: anim.outline > 0.0, width_keyframes: Vec::new() });
    if let Some(g) = anim.glow {
        let radius = g.radius.max(0.0);
        fx.glows.push(crate::TextGlow { enabled: radius > 0.0, color: g.color, radius, radius_keyframes: Vec::new(), intensity: g.intensity.clamp(0.0, 1.0), intensity_keyframes: Vec::new() });
    }
    fx.animators.push(word_animator(screen, anim));
    let o = &mut td.objects[0];
    // Low on a tall frame it stays clear of the band and the buttons at
    // the right that phones cover (#2734), wrapping inside them.
    let y = match (anim.position, tall) {
        (CaptionPlace::Centre, _) => 0.5,
        (CaptionPlace::Lower, true) => 0.70,
        (CaptionPlace::Lower, false) => 0.82,
    };
    o.transform.center = TitlePosition { x: 0.5, y };
    o.transform.size.x = if tall { 0.72 } else { 0.9 };
    // Animated captions read off the picture by their outline; the
    // backing of a plain caption would sit under a moving word.
    td.protect = crate::ContrastProtect::Off;
    td
}

/// The animator that shows the spoken word as `anim.style` says.
fn word_animator(screen: &CaptionScreen, anim: &CaptionAnimation) -> Animator {
    let n = screen.words.len().max(1) as f32;
    let at = |w: &TimedWord| (w.start - screen.start).max(0.0);
    let amount = anim.amount.clamp(0.0, 3.0);
    // The word being spoken: from its start to the next word's.
    let current = || {
        let mut start = vec![key(0.0, 0.0)];
        let mut end = vec![key(0.0, 0.0)];
        for (i, w) in screen.words.iter().enumerate() {
            start.push(key(at(w), i as f32 / n));
            end.push(key(at(w), (i + 1) as f32 / n));
        }
        RangeSelector { unit: SelectUnit::Word, shape: SelectShape::Square, start: 0.0, end: 0.0, start_keyframes: start, end_keyframes: end, ..Default::default() }
    };
    // Every word said so far.
    let said = || {
        let mut end = vec![key(0.0, 0.0)];
        end.extend(screen.words.iter().enumerate().map(|(i, w)| key(at(w), (i + 1) as f32 / n)));
        RangeSelector { unit: SelectUnit::Word, shape: SelectShape::Square, start: 0.0, end: 0.0, end_keyframes: end, ..Default::default() }
    };
    let colour = Some(anim.active_color);
    let (selector, props) = match anim.style {
        CaptionMotion::Pop => (current(), AnimatorProps { scale: 1.0 + 0.22 * amount, colour, ..Default::default() }),
        CaptionMotion::Highlight => (current(), AnimatorProps { colour, ..Default::default() }),
        CaptionMotion::Karaoke => (said(), AnimatorProps { colour, ..Default::default() }),
        CaptionMotion::Bounce => (current(), AnimatorProps { dy: -0.25 * amount, scale: 1.0 + 0.08 * amount, colour, ..Default::default() }),
        CaptionMotion::Box => (current(), AnimatorProps { backing: colour, colour: Some([16, 16, 20, 255]), ..Default::default() }),
        // The words not yet said: from the last said to the end, unseen.
        CaptionMotion::Typewriter => {
            let mut start = vec![key(0.0, 0.0)];
            start.extend(screen.words.iter().enumerate().map(|(i, w)| key(at(w), (i + 1) as f32 / n)));
            (RangeSelector { unit: SelectUnit::Word, shape: SelectShape::Square, start: 0.0, end: 1.0, start_keyframes: start, ..Default::default() }, AnimatorProps { opacity: 0.0, ..Default::default() })
        }
    };
    Animator { name: format!("Caption {}", anim.style.name()), enabled: true, selector, props, within: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(list: &[(&str, f64, f64)]) -> Vec<TimedWord> {
        list.iter().map(|&(t, s, e)| TimedWord { text: t.into(), start: s, end: e }).collect()
    }

    #[test]
    fn screens_hold_a_few_words_and_break_at_pauses_and_sentences() {
        let w = words(&[("one", 0.0, 0.3), ("two", 0.3, 0.6), ("three", 0.6, 0.9), ("four.", 0.9, 1.2), ("five", 1.25, 1.5), ("six", 2.5, 2.8)]);
        let s = screens(&w, 3, 10.0);
        let texts: Vec<Vec<&str>> = s.iter().map(|s| s.words.iter().map(|w| w.text.as_str()).collect()).collect();
        assert_eq!(texts, vec![vec!["one", "two", "three"], vec!["four."], vec!["five"], vec!["six"]]);
        assert_eq!(s[0].end, s[1].start, "back to back while the speech runs on");
        assert!((s[2].end - (1.5 + LINGER)).abs() < 1e-9, "a pause: the last word and a linger");
        assert!(s[3].end <= 10.0);
    }
}
