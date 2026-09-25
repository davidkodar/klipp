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
            t.words.into_iter().map(|w| SpokenWord { start_secs: w.start_secs, end_secs: w.end_secs, text: w.text }).collect()
        });
        let silences = report.audio.map(|a| a.silences.iter().map(|s| (s.start_secs, s.end_secs)).collect()).unwrap_or_default();
        self.words.borrow_mut().insert(item.id, words);
        self.silences.borrow_mut().insert(item.id, silences);
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

/// The hesitation sounds a filler sweep offers to cut. Kept to sounds
/// with no meaning of their own: "like" and "you know" are words too
/// often meant for a sweep to guess.
const FILLERS: &[&str] = &["um", "umm", "uh", "uhh", "erm", "er", "hmm", "mm", "mmm", "ah", "eh"];

/// Whether `text` is a filler, whatever its case and punctuation.
pub fn is_filler(text: &str) -> bool {
    let bare: String = text.chars().filter(|c| c.is_alphabetic()).collect::<String>().to_lowercase();
    FILLERS.contains(&bare.as_str())
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

/// The cuts for `ranges` as `Op::Cut`s, latest first: each is resolved
/// on the timeline the ones before it left, and cutting from the end
/// leaves the earlier times where they were.
pub fn cut_ops(mut ranges: Vec<(f64, f64)>) -> Vec<klipp_document::ops::Op> {
    use klipp_document::ops::{Anchor, Op};
    ranges.retain(|(a, b)| b > a);
    ranges.sort_by(|a, b| b.0.total_cmp(&a.0));
    // Overlapping ranges (fillers either side of one pause) merge.
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for (a, b) in ranges {
        match merged.last_mut() {
            Some(last) if b >= last.0 => last.0 = last.0.min(a),
            _ => merged.push((a, b)),
        }
    }
    merged.into_iter().map(|(a, b)| Op::Cut { from: Anchor::Secs(a), to: Anchor::Secs(b) }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A word heard from `clip`: its id and its timeline span.
    fn w_in(clip: &(klipp_core::ClipId, f64, f64), start: f64, end: f64, text: &str) -> HeardWord {
        HeardWord { start, end, text: text.into(), after_cut: false, clip_id: clip.0, clip_in: clip.1, clip_out: clip.2 }
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
    fn cuts_go_latest_first_and_overlapping_ones_merge() {
        use klipp_document::ops::{Anchor, Op};
        let ops = cut_ops(vec![(1.0, 2.0), (5.0, 6.0), (1.8, 2.5)]);
        let spans: Vec<(f64, f64)> = ops
            .iter()
            .map(|op| match op {
                Op::Cut { from: Anchor::Secs(a), to: Anchor::Secs(b) } => (*a, *b),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(spans, vec![(5.0, 6.0), (1.0, 2.5)]);
    }
}
