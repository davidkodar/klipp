//! A caption track in another language (#2213): the captions read off a
//! subtitle track as whole sentences, since a caption is often half of
//! one and a fragment translates in the wrong word order; each
//! sentence's translation spread back over its captions' times; and a
//! new track under the source carrying them, in its caption style,
//! re-flowed to the caption line limits. Translating is the caller's:
//! this crate has no model.
use crate::history::EditCommand;
use klipp_core::ClipId;
use klipp_timeline::{Sequence, Track, TrackKind};

/// The most characters a caption line holds before it wraps, and the
/// most lines, as the captions op makes them.
pub const MAX_CHARS: usize = 42;
pub const MAX_LINES: usize = 2;

/// Above this many characters a second a caption is hard to read in the
/// time it is shown.
pub const MAX_CHARS_PER_SEC: f64 = 20.0;

/// A pause longer than this between two captions ends a sentence even
/// without its full stop.
pub const SENTENCE_GAP_SECS: f64 = 1.0;

/// The most characters a sentence gathers before it is ended at the
/// next caption anyway: captions with no full stop and no pause would
/// otherwise run on into one line too long to translate whole. About 60
/// tokens, well inside what the translator reads and writes.
pub const MAX_SENTENCE_CHARS: usize = 240;

/// One caption: its clip, when it shows, and its text with its lines
/// joined into one.
#[derive(Debug, Clone, PartialEq)]
pub struct Caption {
    pub id: ClipId,
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// The captions on track `track`, in timeline order.
pub fn captions(seq: &Sequence, track: usize) -> Vec<Caption> {
    let Some(t) = seq.tracks.get(track) else { return Vec::new() };
    let mut out: Vec<Caption> = t
        .clips
        .iter()
        .filter_map(|c| {
            let text = c.title_data()?.primary_text()?.text.split_whitespace().collect::<Vec<_>>().join(" ");
            (!text.is_empty()).then(|| Caption { id: c.id, start: c.timeline_in.to_secs_f64(), end: c.timeline_out.to_secs_f64(), text })
        })
        .collect();
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    out
}

/// One sentence to translate: its text, and the captions it came from
/// with each one's share of it in characters.
#[derive(Debug, Clone, PartialEq)]
pub struct Sentence {
    pub text: String,
    pub parts: Vec<(ClipId, usize)>,
}

/// Whether `text` ends a sentence: a full stop, question or exclamation
/// mark or an ellipsis, with any closing quote or bracket after it.
fn ends_sentence(text: &str) -> bool {
    let t = text.trim_end_matches(['"', '\'', ')', ']', '»', '”', '’']);
    t.ends_with(['.', '!', '?', '…', '。', '！', '？'])
}

/// `captions` gathered into sentences: consecutive captions joined until
/// one ends a sentence, a pause longer than [`SENTENCE_GAP_SECS`] comes,
/// or the next would take it past [`MAX_SENTENCE_CHARS`]; always at a
/// caption's edge.
pub fn sentences(captions: &[Caption]) -> Vec<Sentence> {
    let mut out: Vec<Sentence> = Vec::new();
    let mut open = false;
    for (i, c) in captions.iter().enumerate() {
        let after_gap = i > 0 && c.start - captions[i - 1].end > SENTENCE_GAP_SECS;
        match out.last_mut() {
            Some(s) if open && !after_gap && s.text.chars().count() + 1 + c.text.chars().count() <= MAX_SENTENCE_CHARS => {
                s.text.push(' ');
                s.text.push_str(&c.text);
                s.parts.push((c.id, c.text.chars().count()));
            }
            _ => out.push(Sentence { text: c.text.clone(), parts: vec![(c.id, c.text.chars().count())] }),
        }
        open = !ends_sentence(&c.text);
    }
    out
}

/// `text` cut at word boundaries into `weights.len()` pieces whose
/// lengths follow `weights`. Every word lands in one piece, in order; a
/// piece is empty only when there are fewer words than pieces.
pub fn spread(text: &str, weights: &[usize]) -> Vec<String> {
    let n = weights.len();
    if n <= 1 {
        return vec![text.trim().to_string(); n];
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    let total_weight: usize = weights.iter().sum::<usize>().max(1);
    let total_chars: usize = words.iter().map(|w| w.chars().count() + 1).sum();
    let mut out = vec![String::new(); n];
    let (mut piece, mut done_chars, mut weight_so_far) = (0, 0usize, weights[0]);
    for (wi, word) in words.iter().enumerate() {
        let words_left = words.len() - wi;
        let pieces_left = n - piece;
        // Move on when this piece has its share, or when each piece left
        // needs a word of its own; never past the last piece.
        let share = total_chars * weight_so_far / total_weight;
        let mid = done_chars + word.chars().count().div_ceil(2);
        if piece + 1 < n && !out[piece].is_empty() && (mid > share || words_left < pieces_left) {
            piece += 1;
            weight_so_far += weights[piece];
        }
        if !out[piece].is_empty() {
            out[piece].push(' ');
        }
        out[piece].push_str(word);
        done_chars += word.chars().count() + 1;
    }
    out
}

/// The last caption of each sentence whose translation stopped short,
/// with why: `cuts[i]` is `sentences[i]`'s.
pub fn cut_captions(sentences: &[Sentence], cuts: &[Option<&'static str>]) -> Vec<(ClipId, &'static str)> {
    sentences.iter().zip(cuts).filter_map(|(s, cut)| Some((s.parts.last()?.0, (*cut)?))).collect()
}

/// Each caption's translation: `translated[i]` is `sentences[i]`'s,
/// spread over its captions.
pub fn per_caption(sentences: &[Sentence], translated: &[String]) -> Vec<(ClipId, String)> {
    sentences
        .iter()
        .zip(translated)
        .flat_map(|(s, t)| {
            let weights: Vec<usize> = s.parts.iter().map(|(_, w)| *w).collect();
            s.parts.iter().map(|(id, _)| *id).zip(spread(t, &weights)).collect::<Vec<_>>()
        })
        .collect()
}

/// A translated caption that will be hard to read: where it starts, and
/// why.
#[derive(Debug, Clone, PartialEq)]
pub struct Flagged {
    pub at_secs: f64,
    pub text: String,
    pub why: &'static str,
}

/// The new track and what in it needs a look.
#[derive(Debug, Clone)]
pub struct Translated {
    pub edit: EditCommand,
    pub track_index: usize,
    pub flagged: Vec<Flagged>,
}

/// The name of the track a translation into `language` of `source`
/// gets: its own, the language after it, numbered when the sequence
/// already has one by that name.
fn track_name(seq: &Sequence, source: &str, language: &str) -> String {
    let base = format!("{source} ({language})");
    if !seq.tracks.iter().any(|t| t.name == base) {
        return base;
    }
    (2..).map(|n| format!("{source} ({language} {n})")).find(|name| !seq.tracks.iter().any(|t| t.name == *name)).expect("a free name")
}

/// A subtitle track named for `language`, inserted under track `track`
/// (the next index is the track below), carrying `translations` (each
/// caption clip's id on that track and its translated text) at the
/// source captions' times, in the source track's caption style, wrapped
/// to [`MAX_CHARS`] × [`MAX_LINES`]. Captions that read faster than
/// [`MAX_CHARS_PER_SEC`], or run past the line limits once wrapped, are
/// flagged, as are those in `cut` (a caption whose sentence the
/// translator stopped short, with why). The track is in `code`, the
/// language's tag, and named after `language`, its name (#2223).
pub fn translated_track(seq: &Sequence, track: usize, translations: &[(ClipId, String)], cut: &[(ClipId, &'static str)], language: &str, code: &str) -> Result<Translated, String> {
    let source = seq.tracks.get(track).ok_or("the caption track is gone")?;
    if !matches!(source.kind, TrackKind::Subtitle | TrackKind::Text) {
        return Err("that is not a caption track".into());
    }
    let mut new = Track::new(&track_name(seq, &source.name, language), TrackKind::Subtitle);
    new.caption_style = source.caption_style.clone();
    new.language = Some(code.to_string());
    let mut flagged = Vec::new();
    for (id, text) in translations {
        let Some(clip) = source.clips.iter().find(|c| c.id == *id) else { continue };
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        let wrapped = crate::ops::wrap_lines(text, MAX_CHARS, MAX_LINES);
        let at = clip.timeline_in.to_secs_f64();
        let secs = (clip.timeline_out - clip.timeline_in).to_secs_f64().max(1e-3);
        if let Some((_, why)) = cut.iter().find(|(c, _)| c == id) {
            flagged.push(Flagged { at_secs: at, text: text.to_string(), why });
        } else if text.chars().count() as f64 / secs > MAX_CHARS_PER_SEC {
            flagged.push(Flagged { at_secs: at, text: text.to_string(), why: "reads too fast for its time on screen" });
        } else if wrapped.lines().count() > MAX_LINES || wrapped.lines().any(|l| l.chars().count() > MAX_CHARS + 4) {
            flagged.push(Flagged { at_secs: at, text: text.to_string(), why: "too long for two lines" });
        }
        let td = crate::ops::caption_title(&wrapped);
        let td = match new.caption_style.as_ref() {
            Some(style) => crate::ops::apply_caption_style(&td, style),
            None => td,
        };
        new.clips.push(crate::ops::title_clip(td, clip.timeline_in, clip.timeline_out, seq.fps));
    }
    if new.clips.is_empty() {
        return Err("nothing was translated".into());
    }
    let track_index = track + 1;
    Ok(Translated { edit: EditCommand::AddTrack { index: track_index, track: new }, track_index, flagged })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caption_sequence(lines: &[(f64, f64, &str)]) -> (Sequence, usize, Vec<ClipId>) {
        let mut seq = Sequence::new_default();
        let mut track = Track::new("Subtitle 1", TrackKind::Subtitle);
        let rate = seq.fps.rate();
        let tc = |s: f64| klipp_core::Timecode::from_secs_f64(s, rate);
        let mut ids = Vec::new();
        for (from, to, text) in lines {
            let clip = crate::ops::title_clip(crate::ops::caption_title(text), tc(*from), tc(*to), seq.fps);
            ids.push(clip.id);
            track.clips.push(clip);
        }
        seq.tracks.push(track);
        (seq.clone(), seq.tracks.len() - 1, ids)
    }

    #[test]
    fn captions_read_in_timeline_order_with_their_lines_joined() {
        let (seq, ti, ids) = caption_sequence(&[(2.0, 4.0, "Good  morning"), (0.0, 2.0, "Hello\nthere")]);
        let read: Vec<(ClipId, String)> = captions(&seq, ti).into_iter().map(|c| (c.id, c.text)).collect();
        assert_eq!(read, vec![(ids[1], "Hello there".into()), (ids[0], "Good morning".into())]);
    }

    #[test]
    fn half_sentences_are_translated_whole() {
        let (seq, ti, ids) = caption_sequence(&[
            (0.0, 1.5, "And then we went"),
            (1.5, 3.0, "to the station."),
            (3.0, 4.0, "Done?"),
            (4.0, 5.0, "Not yet"),
            // A long pause: a new sentence, full stop or not.
            (7.0, 8.0, "Later"),
        ]);
        let s = sentences(&captions(&seq, ti));
        assert_eq!(s.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), vec!["And then we went to the station.", "Done?", "Not yet", "Later"]);
        assert_eq!(s[0].parts, vec![(ids[0], 16), (ids[1], 15)]);
    }

    #[test]
    fn a_sentences_translation_spreads_over_its_captions_by_length_losing_no_word() {
        let text = "Und dann sind wir zum Bahnhof gegangen.";
        let pieces = spread(text, &[16, 15]);
        assert_eq!(pieces.len(), 2);
        assert!(pieces.iter().all(|p| !p.is_empty()), "{pieces:?}");
        assert_eq!(pieces.join(" "), text, "every word, in order");
        let (a, b) = (pieces[0].chars().count() as i64, pieces[1].chars().count() as i64);
        assert!((a - b).abs() <= 12, "about even, as the source was: {pieces:?}");
        // Fewer words than captions: the words go first, nothing lost.
        let few = spread("Ja", &[5, 5, 5]);
        assert_eq!(few, vec!["Ja".to_string(), String::new(), String::new()]);
        // Weighted: the long caption takes most of it.
        let lopsided = spread("eins zwei drei vier fünf sechs sieben acht", &[30, 5]);
        assert!(lopsided[0].split(' ').count() > lopsided[1].split(' ').count(), "{lopsided:?}");
        assert_eq!(lopsided.join(" "), "eins zwei drei vier fünf sechs sieben acht");
    }

    #[test]
    fn a_translated_track_keeps_each_captions_time_and_flags_the_hard_ones() {
        let (seq, ti, ids) = caption_sequence(&[(2.0, 4.0, "Good morning"), (0.0, 2.0, "Hello there")]);
        let long = "Dies ist ein sehr langer Satz, der in der kurzen Zeit auf dem Bildschirm kaum zu lesen ist.";
        let out = translated_track(&seq, ti, &[(ids[1], "Hallo".into()), (ids[0], long.into())], &[], "German", "de").unwrap();
        let EditCommand::AddTrack { index, track } = &out.edit else { panic!("an added track") };
        assert_eq!(*index, ti + 1, "the index after the source is the track under it");
        assert_eq!(track.name, "Subtitle 1 (German)");
        assert_eq!(track.kind, TrackKind::Subtitle);
        assert_eq!(track.clips.len(), 2);
        let first = track.clips.iter().find(|c| c.timeline_in.to_secs_f64() < 1.0).unwrap();
        assert_eq!(first.title_data().unwrap().primary_text().unwrap().text, "Hallo");
        let second = track.clips.iter().find(|c| c.timeline_in.to_secs_f64() > 1.0).unwrap();
        assert!((second.timeline_out.to_secs_f64() - 4.0).abs() < 1e-6, "the source caption's time");
        let shown = &second.title_data().unwrap().primary_text().unwrap().text;
        assert!(shown.contains('\n'), "wrapped to two lines");
        assert_eq!(shown.split_whitespace().collect::<Vec<_>>(), long.split_whitespace().collect::<Vec<_>>(), "no word lost in the wrap");
        assert_eq!(out.flagged.len(), 1);
        assert_eq!(out.flagged[0].why, "reads too fast for its time on screen");
    }

    #[test]
    fn captions_with_no_full_stop_and_no_pause_still_end_at_a_caption_within_the_budget() {
        let lines: Vec<(f64, f64, String)> = (0..40).map(|i| (i as f64, i as f64 + 1.0, format!("and then we kept on talking {i}"))).collect();
        let refs: Vec<(f64, f64, &str)> = lines.iter().map(|(a, b, t)| (*a, *b, t.as_str())).collect();
        let (seq, ti, _) = caption_sequence(&refs);
        let caps = captions(&seq, ti);
        let s = sentences(&caps);
        assert!(s.len() > 1, "not one run-on line");
        assert!(s.iter().all(|s| s.text.chars().count() <= MAX_SENTENCE_CHARS), "each within the budget");
        assert_eq!(s.iter().map(|s| s.parts.len()).sum::<usize>(), caps.len(), "every caption in one sentence, none split");
    }

    #[test]
    fn a_sentence_the_translator_cut_short_is_flagged_on_its_last_caption() {
        let (seq, ti, ids) = caption_sequence(&[(0.0, 3.0, "And then we went"), (3.0, 6.0, "to the station.")]);
        let s = sentences(&captions(&seq, ti));
        let cut = cut_captions(&s, &[Some("the translation reached its length limit: its end may be missing")]);
        assert_eq!(cut, vec![(ids[1], "the translation reached its length limit: its end may be missing")]);
        let per = per_caption(&s, &["Und dann gingen wir zum".into()]);
        let out = translated_track(&seq, ti, &per, &cut, "German", "de").unwrap();
        assert_eq!(out.flagged.len(), 1);
        assert!(out.flagged[0].why.contains("length limit"));
        assert!((out.flagged[0].at_secs - 3.0).abs() < 1e-6);
    }

    #[test]
    fn a_caption_too_long_for_two_lines_is_flagged() {
        let (seq, ti, ids) = caption_sequence(&[(0.0, 30.0, "Hello")]);
        let long = "word ".repeat(40);
        let out = translated_track(&seq, ti, &[(ids[0], long)], &[], "German", "de").unwrap();
        assert_eq!(out.flagged.len(), 1, "{:?}", out.flagged);
        assert_eq!(out.flagged[0].why, "too long for two lines");
    }

    #[test]
    fn a_second_translation_into_the_same_language_gets_its_own_name() {
        let (mut seq, ti, ids) = caption_sequence(&[(0.0, 2.0, "Hello")]);
        let out = translated_track(&seq, ti, &[(ids[0], "Hallo".into())], &[], "German", "de").unwrap();
        let EditCommand::AddTrack { index, track } = out.edit else { panic!("an added track") };
        seq.tracks.insert(index, track);
        let again = translated_track(&seq, ti, &[(ids[0], "Hallo".into())], &[], "German", "de").unwrap();
        let EditCommand::AddTrack { track, .. } = again.edit else { panic!("an added track") };
        assert_eq!(track.name, "Subtitle 1 (German 2)");
        assert_eq!(track.language.as_deref(), Some("de"), "in the language's tag, not only its name");
    }

    #[test]
    fn nothing_to_translate_is_refused() {
        let (seq, ti, _) = caption_sequence(&[(0.0, 2.0, "Hello")]);
        assert!(translated_track(&seq, ti, &[], &[], "German", "de").is_err());
        assert!(translated_track(&seq, 99, &[], &[], "German", "de").is_err());
    }
}
