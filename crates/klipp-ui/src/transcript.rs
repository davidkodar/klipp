//! What is said on the timeline, for the Transcript panel and for the
//! document ops the GUI runs (captions, cuts).
//!
//! The words come from the analysers' transcripts, one file per media
//! item on disk. Reading one is a JSON parse, far too much for every
//! frame, so [`TranscriptSource`] keeps what it has read until the
//! analysis generation moves — the counter the app bumps when an
//! analysis finishes. It implements the document's `Transcripts`, so the
//! same words feed the panel, `Op::Captions` and the agent alike.
//!
//! The rest is the arithmetic of text-based editing, kept free of the UI
//! so it can be tested: where a selection of words is cut, which words
//! are fillers, which pauses are long, how words fall into paragraphs.

use std::cell::RefCell;
use std::collections::HashMap;

use klipp_core::MediaId;
use klipp_document::ops::{HeardWord, SpokenWord, Transcripts};

/// Transcripts and measured silences by media item, read once per
/// analysis generation.
#[derive(Default)]
pub struct TranscriptSource {
    generation: std::cell::Cell<u64>,
    words: RefCell<HashMap<MediaId, Option<Vec<SpokenWord>>>>,
    silences: RefCell<HashMap<MediaId, Vec<(f64, f64)>>>,
}

impl TranscriptSource {
    /// Forget what was read when the analyses have moved on.
    pub fn sync(&self, generation: u64) {
        if self.generation.get() != generation {
            self.generation.set(generation);
            self.words.borrow_mut().clear();
            self.silences.borrow_mut().clear();
        }
    }

    fn read(&self, item: &klipp_media::MediaItem) {
        if self.words.borrow().contains_key(&item.id) {
            return;
        }
        let report = klipp_analysis::report::MediaAnalysis::load(&item.path);
        let words = report.transcript.map(|t| {
            t.words.into_iter().map(|w| SpokenWord { start_secs: w.start_secs, end_secs: w.end_secs, text: w.text, confidence: w.confidence }).collect()
        });
        let silences = report.audio.map(|a| a.silences.iter().map(|s| (s.start_secs, s.end_secs)).collect()).unwrap_or_default();
        self.words.borrow_mut().insert(item.id, words);
        self.silences.borrow_mut().insert(item.id, silences);
    }
}

#[cfg(test)]
impl TranscriptSource {
    /// A source that has `words` for `media`, as if read from its analysis.
    pub fn with_words(media: MediaId, words: Vec<SpokenWord>) -> Self {
        let source = Self::default();
        source.words.borrow_mut().insert(media, Some(words));
        source.silences.borrow_mut().insert(media, Vec::new());
        source
    }
}

impl Transcripts for TranscriptSource {
    fn words(&self, item: &klipp_media::MediaItem) -> Option<Vec<SpokenWord>> {
        self.read(item);
        self.words.borrow().get(&item.id).cloned().flatten()
    }

    fn silences(&self, item: &klipp_media::MediaItem) -> Vec<(f64, f64)> {
        self.read(item);
        self.silences.borrow().get(&item.id).cloned().unwrap_or_default()
    }

    /// The faces its framing analysis found, per sampled frame: what a
    /// smooth cut's framing is checked by (#2219). Read when asked, which
    /// is when a smooth cut is put on a cut.
    fn faces(&self, item: &klipp_media::MediaItem) -> Vec<(f64, [f32; 4])> {
        klipp_analysis::report::MediaAnalysis::load(&item.path)
            .framing
            .map(|f| f.samples.iter().flat_map(|s| s.faces.iter().map(move |b| (s.secs, [b.x, b.y, b.w, b.h]))).collect())
            .unwrap_or_default()
    }
}

/// A pause longer than this starts a new paragraph, as it ends a caption.
pub const PARAGRAPH_PAUSE_SECS: f64 = 0.7;

/// A cut never lands further than this from the word it takes out. The
/// pause on either side of a word is a breath; anything longer is content
/// the transcript does not describe.
pub const MAX_PAD_SECS: f64 = 0.25;

/// Where cutting the words `first..=last` out of `words` should cut, in
/// timeline seconds: halfway into the pause on each side, so what is left
/// keeps a natural breath rather than butting one word onto the next —
/// but no further than [`MAX_PAD_SECS`] from the word, and never past the
/// clip the word is heard from. What lies between two words from
/// different clips, or beyond a clip's edge, is not a pause but whatever
/// the timeline puts there: a music bed, a montage, picture with no
/// speech. At either end of the transcript it is a short margin instead.
pub fn cut_for_words(words: &[HeardWord], first: usize, last: usize) -> (f64, f64) {
    const MARGIN_SECS: f64 = 0.05;
    let (head, tail) = (&words[first], &words[last]);
    let from = match first.checked_sub(1).map(|p| &words[p]) {
        Some(prev) if prev.clip_id == head.clip_id && prev.end <= head.start => (prev.end + head.start) / 2.0,
        _ => head.start - MARGIN_SECS,
    };
    let to = match words.get(last + 1) {
        Some(next) if next.clip_id == tail.clip_id && next.start >= tail.end => (tail.end + next.start) / 2.0,
        _ => tail.end + MARGIN_SECS,
    };
    let from = from.max(head.start - MAX_PAD_SECS).max(head.clip_in).max(0.0);
    let to = to.min(tail.end + MAX_PAD_SECS).min(tail.clip_out);
    (from, to)
}

/// The word lists of the language the project's speech is in (#2889),
/// English when none is set; `Err` names a language with none.
pub fn word_lists() -> Result<std::sync::Arc<klipp_core::lang::Words>, String> {
    klipp_core::lang::words(klipp_analysis::transcript::language().as_deref())
}

/// Whether `text` is one of the hesitation sounds a filler sweep offers to
/// cut, in the project's language, whatever its case and punctuation;
/// nothing is, in a language with no list.
pub fn is_filler(text: &str) -> bool {
    word_lists().is_ok_and(|w| w.is_filler(text))
}

/// The indices of the filler words.
pub fn fillers(words: &[HeardWord]) -> Vec<usize> {
    (0..words.len()).filter(|&i| is_filler(&words[i].text)).collect()
}

/// Pauses between words longer than `longer_than`, each as the stretch to
/// cut so that `keep` seconds of it remain, in timeline seconds. Only a
/// gap between two words heard from the same clip is a pause: between
/// clips lies whatever was put there, transcribed or not.
pub fn long_pauses(words: &[HeardWord], longer_than: f64, keep: f64) -> Vec<(f64, f64)> {
    words
        .windows(2)
        .filter(|w| w[0].clip_id == w[1].clip_id && w[1].start - w[0].end > longer_than.max(keep))
        .map(|w| (w[0].end + keep / 2.0, w[1].start - keep / 2.0))
        .collect()
}

/// Where each paragraph starts: at the first word, after a pause, and
/// at a cut in the sound.
pub fn paragraph_starts(words: &[HeardWord]) -> Vec<usize> {
    let mut starts = Vec::new();
    for i in 0..words.len() {
        let pause = i > 0 && words[i].start - words[i - 1].end > PARAGRAPH_PAUSE_SECS;
        if i == 0 || pause || words[i].after_cut {
            starts.push(i);
        }
    }
    starts
}

/// The word being said at `secs`, or the last one said before it.
pub fn word_at(words: &[HeardWord], secs: f64) -> Option<usize> {
    let after = words.partition_point(|w| w.start <= secs);
    after.checked_sub(1)
}

/// What names a heard word across a change to the timeline: the clip
/// it is heard from, where it starts within that clip, and its text.
/// A word keeps its key when its clip is moved; it loses it when the
/// clip is split, trimmed at the head or transcribed again.
#[derive(Debug, Clone, PartialEq)]
pub struct WordKey {
    clip_id: klipp_core::ClipId,
    offset: f64,
    text: String,
}

impl WordKey {
    pub fn of(word: &HeardWord) -> Self {
        Self { clip_id: word.clip_id, offset: word.start - word.clip_in, text: word.text.clone() }
    }

    /// The word this names among `words`, if it is still heard.
    pub fn find(&self, words: &[HeardWord]) -> Option<usize> {
        words.iter().position(|w| w.clip_id == self.clip_id && w.text == self.text && (w.start - w.clip_in - self.offset).abs() < 0.01)
    }
}

/// A cut a sweep proposes: what it takes out, where, and whether it is
/// ticked for cutting.
#[derive(Debug, Clone)]
pub struct Proposal {
    pub label: String,
    pub range: (f64, f64),
    pub ticked: bool,
    at: ProposalAt,
}

#[derive(Debug, Clone)]
enum ProposalAt {
    /// A filler word.
    Word(WordKey),
    /// The pause between two words.
    Pause(WordKey, WordKey),
}

/// Every filler, or every long pause, offered as cuts to review and
/// apply together. Each proposal knows the words it came from, so the
/// sweep can be fitted again to the words after the timeline changes
/// under it, instead of vanishing mid-review.
#[derive(Debug, Clone)]
pub struct Sweep {
    pub title: String,
    pub cuts: Vec<Proposal>,
    longer_than: f64,
    keep: f64,
}

impl Sweep {
    /// Every filler among `words`.
    pub fn fillers(words: &[HeardWord]) -> Self {
        // A language with no list says so, rather than finding none (#2889).
        let title = match word_lists() {
            Ok(_) => "Fillers".to_string(),
            Err(why) => format!("Fillers: {why}"),
        };
        let mut sweep = Self { title, cuts: Vec::new(), longer_than: 0.0, keep: 0.0 };
        for i in fillers(words) {
            let at = ProposalAt::Word(WordKey::of(&words[i]));
            sweep.cuts.push(Proposal { label: String::new(), range: (0.0, 0.0), ticked: true, at });
        }
        sweep.refit(words);
        sweep
    }

    /// Every pause among `words` longer than `longer_than`, each cut
    /// down to `keep` seconds.
    pub fn pauses(words: &[HeardWord], longer_than: f64, keep: f64) -> Self {
        let mut sweep = Self { title: "Long pauses".into(), cuts: Vec::new(), longer_than, keep };
        for w in words.windows(2).filter(|w| !long_pauses(w, longer_than, keep).is_empty()) {
            let at = ProposalAt::Pause(WordKey::of(&w[0]), WordKey::of(&w[1]));
            sweep.cuts.push(Proposal { label: String::new(), range: (0.0, 0.0), ticked: true, at });
        }
        sweep.refit(words);
        sweep
    }

    /// Place every proposal again from its words, as `words` now has
    /// them; one whose words are no longer heard, no longer next to each
    /// other, or no longer a filler or a long pause, is dropped. Ticks
    /// are kept. Returns how many were dropped.
    pub fn refit(&mut self, words: &[HeardWord]) -> usize {
        let before = self.cuts.len();
        let (longer_than, keep) = (self.longer_than, self.keep);
        self.cuts.retain_mut(|cut| {
            let placed = match &cut.at {
                ProposalAt::Word(key) => key.find(words).filter(|&i| is_filler(&words[i].text)).map(|i| {
                    (format!("{}  \u{201C}{}\u{201D}", format_secs(words[i].start), words[i].text), cut_for_words(words, i, i))
                }),
                ProposalAt::Pause(a, b) => a.find(words).zip(b.find(words)).filter(|&(i, j)| j == i + 1).and_then(|(i, _)| {
                    let (a, b) = long_pauses(&words[i..=i + 1], longer_than, keep).pop()?;
                    Some((format!("{}  {:.1} s pause", format_secs(a), b - a + keep), (a, b)))
                }),
            };
            match placed {
                Some((label, range)) => {
                    cut.label = label;
                    cut.range = range;
                    true
                }
                None => false,
            }
        });
        before - self.cuts.len()
    }

    /// The ranges ticked for cutting, each with the clip it is heard in.
    pub fn ticked(&self) -> Vec<CutRange> {
        self.cuts
            .iter()
            .filter(|c| c.ticked)
            .map(|c| {
                let clip = match &c.at {
                    ProposalAt::Word(key) | ProposalAt::Pause(key, _) => key.clip_id,
                };
                (c.range, vec![clip])
            })
            .collect()
    }
}

/// Timeline seconds as `mm:ss.ss`.
pub fn format_secs(s: f64) -> String {
    let s = s.max(0.0);
    let m = (s / 60.0).floor();
    format!("{:02}:{:05.2}", m as u64, s - m * 60.0)
}

/// A stretch of the timeline to cut, and the clips heard there it is
/// cut from: their tracks and the tracks linked to them, not every
/// track (#2568).
pub type CutRange = ((f64, f64), Vec<klipp_core::ClipId>);

/// The clips the words `first` to `last` are heard from, each once.
pub fn clips_of(words: &[HeardWord], first: usize, last: usize) -> Vec<klipp_core::ClipId> {
    let mut clips: Vec<klipp_core::ClipId> = Vec::new();
    for w in &words[first..=last] {
        if !clips.contains(&w.clip_id) {
            clips.push(w.clip_id);
        }
    }
    clips
}

/// The cuts for `ranges` as `Op::Cut`s, latest first: each is resolved
/// on the timeline the ones before it left, and cutting from the end
/// leaves the earlier times where they were. Each is kept to the tracks
/// of its clips, so music and a picture over the voice play on.
pub fn cut_ops(mut ranges: Vec<CutRange>) -> Vec<klipp_document::ops::Op> {
    use klipp_document::ops::{Anchor, ClipRef, Op};
    ranges.retain(|((a, b), _)| b > a);
    ranges.sort_by(|a, b| b.0 .0.total_cmp(&a.0 .0));
    // Overlapping ranges (fillers either side of one pause) merge, with
    // the clips of both.
    let mut merged: Vec<CutRange> = Vec::new();
    for ((a, b), clips) in ranges {
        match merged.last_mut() {
            Some(((last_a, _), last_clips)) if b >= *last_a => {
                *last_a = last_a.min(a);
                last_clips.extend(clips.into_iter().filter(|c| !last_clips.contains(c)).collect::<Vec<_>>());
            }
            _ => merged.push(((a, b), clips)),
        }
    }
    merged
        .into_iter()
        .map(|((a, b), clips)| Op::Cut { from: Anchor::Secs(a), to: Anchor::Secs(b), tracks_of: clips.into_iter().map(ClipRef::Id).collect() })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A word heard from `clip`: its id and its timeline span.
    fn w_in(clip: &(klipp_core::ClipId, f64, f64), start: f64, end: f64, text: &str) -> HeardWord {
        HeardWord { start, end, text: text.into(), after_cut: false, clip_id: clip.0, media_id: klipp_core::MediaId::new(), role: klipp_core::ClipRole::Dialogue, clip_in: clip.1, clip_out: clip.2 }
    }

    /// One clip, 0-10 s, with a pause before "Next".
    fn said() -> Vec<HeardWord> {
        let c = (klipp_core::ClipId::new(), 0.0, 10.0);
        vec![w_in(&c, 1.0, 1.3, "So,"), w_in(&c, 1.5, 1.7, "um,"), w_in(&c, 1.8, 2.2, "the"), w_in(&c, 2.3, 2.9, "plan."), w_in(&c, 5.0, 5.4, "Next")]
    }

    /// An interview 0-10 s, a music montage with no transcript 10-15 s,
    /// the interview again from 15 s.
    fn montage() -> Vec<HeardWord> {
        let before = (klipp_core::ClipId::new(), 0.0, 10.0);
        let after = (klipp_core::ClipId::new(), 15.0, 25.0);
        vec![
            w_in(&before, 9.0, 9.3, "and"),
            w_in(&before, 9.4, 9.8, "then."),
            w_in(&after, 15.1, 15.3, "Um,"),
            w_in(&after, 15.5, 15.9, "later"),
        ]
    }

    #[test]
    fn a_cut_lands_halfway_into_the_pauses_either_side() {
        let words = said();
        let (from, to) = cut_for_words(&words, 1, 1);
        assert!((from - 1.4).abs() < 1e-9 && (to - 1.75).abs() < 1e-9, "{from} {to}");
        // The first word: a margin before it, not a negative time.
        let (from, _) = cut_for_words(&words, 0, 0);
        assert!((from - 0.95).abs() < 1e-9);
        // The last: a margin after it.
        let (_, to) = cut_for_words(&words, 4, 4);
        assert!((to - 5.45).abs() < 1e-9);
        // Before a long pause: no further than the pad, not halfway.
        let (_, to) = cut_for_words(&words, 3, 3);
        assert!((to - (2.9 + MAX_PAD_SECS)).abs() < 1e-9, "{to}");
    }

    #[test]
    fn a_cut_stays_out_of_what_lies_between_clips() {
        let words = montage();
        // The last word before the montage: the cut ends at its own margin,
        // not halfway into the music.
        let (_, to) = cut_for_words(&words, 1, 1);
        assert!((to - 9.85).abs() < 1e-9, "{to}");
        // The filler after it: the cut starts at its margin, inside its
        // clip, and ends halfway to the next word as usual.
        let (from, to) = cut_for_words(&words, 2, 2);
        assert!((from - 15.05).abs() < 1e-9 && (to - 15.4).abs() < 1e-9, "{from} {to}");
        // A word ending on its clip's edge: the cut ends there.
        let mut words = montage();
        words[1].end = 10.0;
        let (_, to) = cut_for_words(&words, 1, 1);
        assert!((to - 10.0).abs() < 1e-9, "{to}");
        // The words either side of the montage are not a pause to shorten.
        assert!(long_pauses(&montage(), 1.0, 0.4).is_empty());
    }

    #[test]
    fn fillers_are_found_whatever_the_punctuation_and_nothing_else_is() {
        assert_eq!(fillers(&said()), vec![1]);
        assert!(is_filler("Uhh..."));
        assert!(!is_filler("like") && !is_filler("umbrella"));
    }

    #[test]
    fn a_long_pause_is_cut_down_to_what_should_remain() {
        let cuts = long_pauses(&said(), 1.0, 0.4);
        assert_eq!(cuts.len(), 1);
        let (a, b) = cuts[0];
        assert!((a - 3.1).abs() < 1e-9 && (b - 4.8).abs() < 1e-9, "{a} {b}");
    }

    #[test]
    fn paragraphs_break_at_pauses_and_the_word_at_a_time_is_found() {
        let words = said();
        assert_eq!(paragraph_starts(&words), vec![0, 4]);
        assert_eq!(word_at(&words, 2.0), Some(2));
        assert_eq!(word_at(&words, 3.5), Some(3), "between words, the last one said");
        assert_eq!(word_at(&words, 0.5), None);
    }

    #[test]
    fn a_sweep_follows_its_words_when_the_timeline_moves_and_drops_what_is_gone() {
        let words = said();
        let mut fillers = Sweep::fillers(&words);
        let mut pauses = Sweep::pauses(&words, 1.0, 0.4);
        assert_eq!(fillers.cuts.len(), 1);
        assert_eq!(pauses.cuts.len(), 1);
        fillers.cuts[0].ticked = false;
        assert!(fillers.ticked().is_empty() && pauses.ticked().len() == 1);

        // The clip moved a second later: every proposal moves with it,
        // and the tick stays.
        let moved: Vec<HeardWord> = words
            .iter()
            .map(|w| HeardWord { start: w.start + 1.0, end: w.end + 1.0, clip_in: w.clip_in + 1.0, clip_out: w.clip_out + 1.0, ..w.clone() })
            .collect();
        assert_eq!(fillers.refit(&moved), 0);
        assert_eq!(pauses.refit(&moved), 0);
        let (a, b) = fillers.cuts[0].range;
        assert!((a - 2.4).abs() < 1e-9 && (b - 2.75).abs() < 1e-9, "{a} {b}");
        assert!(!fillers.cuts[0].ticked);
        assert_eq!(pauses.cuts[0].label, "00:04.10  2.1 s pause");

        // The filler cut out, and the pause closed up: both proposals go.
        let without: Vec<HeardWord> = moved.iter().filter(|w| w.text != "um,").cloned().collect();
        assert_eq!(fillers.refit(&without), 1);
        assert!(fillers.cuts.is_empty());
        let mut closed = moved.clone();
        closed[4].start = 4.2;
        closed[4].end = 4.6;
        assert_eq!(pauses.refit(&closed), 1);
        assert!(pauses.cuts.is_empty());
    }

    #[test]
    fn cuts_go_latest_first_and_overlapping_ones_merge() {
        use klipp_document::ops::{Anchor, Op};
        let (voice, other) = (klipp_core::ClipId::new(), klipp_core::ClipId::new());
        let ops = cut_ops(vec![((1.0, 2.0), vec![voice]), ((5.0, 6.0), vec![voice]), ((1.8, 2.5), vec![other])]);
        let spans: Vec<((f64, f64), usize)> = ops
            .iter()
            .map(|op| match op {
                Op::Cut { from: Anchor::Secs(a), to: Anchor::Secs(b), tracks_of } => ((*a, *b), tracks_of.len()),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(spans, vec![((5.0, 6.0), 1), ((1.0, 2.5), 2)], "merged, with the clips of both");
    }
}
