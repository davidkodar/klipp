//! Spelling in titles, captions and subtitles (#3236).
//!
//! Each text is checked against the Hunspell dictionary of its language,
//! read from the system ([`dictionaries`]); the language is the text's
//! own, then its track's, then the brief's, then the system's. A word
//! is not marked when the project knows it ([`Known`]: the brief's
//! vocabulary and cast, the media's file names, words passed in such as
//! a brand kit's names, and the person's own list, [`personal`]), or
//! when it is not checked at all ([`words`]: numbers, addresses, handles,
//! and words in another script). A language with no dictionary is said
//! once, with the package that would give one, and its text is left
//! unmarked.

#![forbid(unsafe_code)]

pub mod dictionaries;
pub mod personal;
pub mod words;

use std::collections::HashSet;
use std::sync::Arc;

use klipp_core::ClipId;
use klipp_timeline::{Brief, Sequence, TextBox, TitleObjectContent, TitleObjectId, Track};

pub use dictionaries::{Dictionaries, Dictionary, Missing};

/// The most suggestions given for a word.
pub const MAX_SUGGESTIONS: usize = 5;

/// The words a project spells its own way, taken as right.
#[derive(Debug, Clone, Default)]
pub struct Known {
    /// Lower case.
    words: HashSet<String>,
}

impl Known {
    /// The brief's vocabulary and cast names, and `extra`: names, file
    /// names, the person's list. Each is split into words, so "Ana
    /// Ruiz" or "harbour_dawn_04.mov" makes words of its parts.
    pub fn new<'a>(brief: Option<&Brief>, extra: impl IntoIterator<Item = &'a str>) -> Self {
        let mut known = Self::default();
        if let Some(b) = brief {
            for v in &b.vocabulary {
                known.add(v);
            }
            for c in &b.cast {
                known.add(&c.name);
            }
        }
        for e in extra {
            known.add(e);
        }
        known
    }

    /// The words of `text` taken as right from now on.
    pub fn add(&mut self, text: &str) {
        for w in text.split(|c: char| !(c.is_alphanumeric() || matches!(c, '\'' | '\u{2019}' | '-'))) {
            for part in std::iter::once(w).chain(w.split('-')) {
                if part.chars().any(char::is_alphabetic) {
                    self.words.insert(words::for_dictionary(part).to_lowercase());
                }
            }
        }
    }

    pub fn contains(&self, word: &str) -> bool {
        self.words.contains(&words::for_dictionary(word).to_lowercase())
    }
}

/// A word marked: where it is and what it might have been.
#[derive(Debug, Clone, PartialEq)]
pub struct Misspelling {
    pub clip_id: ClipId,
    /// The track's index on the sequence.
    pub track: usize,
    pub object: TitleObjectId,
    /// The word's bytes in the text.
    pub start: usize,
    pub end: usize,
    pub word: String,
    /// The language it was checked in.
    pub language: String,
    /// Up to [`MAX_SUGGESTIONS`], likeliest first; none when not asked
    /// for.
    pub suggestions: Vec<String>,
    /// Where the title starts.
    pub at_secs: f64,
}

/// A sequence's spelling: the words marked, in timeline order, and the
/// languages that could not be checked.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    pub misspellings: Vec<Misspelling>,
    pub missing: Vec<Missing>,
}

/// The words of `text` that `dict` does not know and `known` does not
/// either.
pub fn misspelt<'a>(text: &'a str, dict: &Dictionary, known: &Known) -> Vec<words::Word<'a>> {
    words::words(text)
        .into_iter()
        .filter(|w| words::in_alphabet(w.text, &dict.alphabet) && !known.contains(w.text) && !dict.checker.check(&words::for_dictionary(w.text)))
        .collect()
}

/// What `word` might have been, likeliest first, at most `max`.
pub fn suggestions(word: &str, dict: &Dictionary, max: usize) -> Vec<String> {
    if max == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    dict.checker.suggest(&words::for_dictionary(word), &mut out);
    out.truncate(max);
    out
}

/// The language a text is checked in: its own, its track's, the brief's,
/// else `fallback` (the system's).
pub fn language_of(tb: &TextBox, track: &Track, brief: Option<&Brief>, fallback: &str) -> String {
    let given = |l: &Option<String>| l.as_deref().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string);
    given(&tb.language).or_else(|| given(&track.language)).or_else(|| brief.and_then(|b| given(&b.language))).unwrap_or_else(|| fallback.to_string())
}

/// The language the system is set to (`LANG`), else English.
pub fn system_language() -> String {
    dictionaries::locale_language(std::env::var("LANG").ok().as_deref()).unwrap_or_else(|| "en".to_string())
}

/// Every word marked in `seq`'s titles and captions, each with up to
/// `max_suggestions` suggestions; a title on a track or in a language
/// not checked is passed over, and a language with no dictionary listed
/// once.
pub fn check_sequence(seq: &Sequence, known: &Known, dicts: &Dictionaries, fallback: &str, max_suggestions: usize) -> Report {
    let mut report = Report::default();
    let brief = seq.brief.as_ref();
    for (ti, track) in seq.tracks.iter().enumerate() {
        for clip in &track.clips {
            let Some(td) = clip.title_data() else { continue };
            for o in &td.objects {
                let TitleObjectContent::TextBox(tb) = &o.content else { continue };
                let language = language_of(tb, track, brief, fallback);
                let dict: Arc<Dictionary> = match dicts.get(&language) {
                    Ok(d) => d,
                    Err(m) => {
                        if !report.missing.contains(&m) {
                            report.missing.push(m);
                        }
                        continue;
                    }
                };
                for w in misspelt(&tb.text, &dict, known) {
                    report.misspellings.push(Misspelling {
                        clip_id: clip.id,
                        track: ti,
                        object: o.id,
                        start: w.start,
                        end: w.end,
                        word: w.text.to_string(),
                        language: language.clone(),
                        suggestions: suggestions(w.text, &dict, max_suggestions),
                        at_secs: clip.timeline_in.to_secs_f64(),
                    });
                }
            }
        }
    }
    report.misspellings.sort_by(|a, b| a.at_secs.total_cmp(&b.at_secs).then(a.track.cmp(&b.track)).then(a.start.cmp(&b.start)));
    report
}
