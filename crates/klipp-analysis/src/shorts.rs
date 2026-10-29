//! Moments for short clips (#2720): the stretches of a long talk,
//! podcast, stream or vlog that stand on their own as a short, ranked,
//! each with why.
//!
//! A candidate starts at a sentence after a real pause and ends at a
//! sentence's end, inside the length asked for. It is scored on its
//! hook (a short first line, a question, a number, "you", a strong
//! opener), whether it stands alone (it doesn't open mid-thought on
//! "and", "so", "it" …, and it ends on a full stop and a pause), its
//! payoff (a turn, "but", "because", "turns out", and an end after it),
//! a laugh (the transcriber's laughter marks), its energy (how fast it
//! is spoken against the whole) and how near it is to the length asked
//! for. The best are picked, never overlapping.
//!
//! Pure: it reads the transcript's words, already in the report. No
//! model: an agent reading the text can rank them again.

use crate::transcript::Word;

/// What to look for.
#[derive(Debug, Clone, PartialEq)]
pub struct Ask {
    /// The length aimed for, seconds.
    pub target_secs: f64,
    pub min_secs: f64,
    pub max_secs: f64,
    /// How many moments.
    pub count: usize,
    /// Words one of which a moment must hold (none: any).
    pub include: Vec<String>,
    /// Words no moment may hold.
    pub exclude: Vec<String>,
}

impl Default for Ask {
    fn default() -> Self {
        Self { target_secs: 30.0, min_secs: 15.0, max_secs: 60.0, count: 5, include: Vec::new(), exclude: Vec::new() }
    }
}

/// A moment found.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ShortMoment {
    pub from_secs: f64,
    pub to_secs: f64,
    /// Its first sentence: what a viewer hears first.
    pub hook: String,
    pub text: String,
    /// 0 to 1.
    pub score: f32,
    /// Why it scored as it did, a few words each.
    pub reasons: Vec<String>,
}

/// A sentence of the transcript: its words' span, its text, and the
/// pauses either side.
#[derive(Debug, Clone)]
struct Sentence {
    start: f64,
    end: f64,
    text: String,
    words: usize,
    gap_before: f64,
    gap_after: f64,
    /// Whether it closes on a full stop, a question or an exclamation.
    closed: bool,
}

/// A pause this long ends a sentence whatever its punctuation.
const SENTENCE_PAUSE: f64 = 0.7;
/// A moment starts after at least this pause, or at the start.
const START_PAUSE: f64 = 0.35;

fn sentences(words: &[Word]) -> Vec<Sentence> {
    let mut out: Vec<Sentence> = Vec::new();
    let mut current: Vec<&Word> = Vec::new();
    for (i, w) in words.iter().enumerate() {
        current.push(w);
        let next_gap = words.get(i + 1).map_or(f64::INFINITY, |n| n.start_secs - w.end_secs);
        let t = w.text.trim();
        let closed = t.ends_with(['.', '?', '!']);
        if closed || next_gap >= SENTENCE_PAUSE {
            let first = current[0];
            let gap_before = out.last().map_or(f64::INFINITY, |p: &Sentence| first.start_secs - p.end);
            let text = current.iter().map(|w| w.text.trim()).collect::<Vec<_>>().join(" ");
            out.push(Sentence { start: first.start_secs, end: w.end_secs, text, words: current.len(), gap_before, gap_after: next_gap, closed });
            current.clear();
        }
    }
    out
}

/// The words of `text`, lower case, without punctuation.
fn tokens(text: &str) -> Vec<String> {
    text.split_whitespace().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'').to_lowercase()).filter(|w| !w.is_empty()).collect()
}


fn hook_score(first: &Sentence, words: &klipp_core::lang::Words, reasons: &mut Vec<String>) -> f32 {
    let t = tokens(&first.text);
    let lower = first.text.to_lowercase();
    let mut s = 0.0f32;
    if first.words <= 14 {
        s += 0.3;
    }
    if first.text.trim().ends_with('?') {
        s += 0.35;
        reasons.push("opens on a question".into());
    }
    if first.text.chars().any(|c| c.is_ascii_digit()) {
        s += 0.15;
        reasons.push("a number up front".into());
    }
    if t.iter().any(|w| words.hooks.address.contains(w)) {
        s += 0.15;
    }
    if words.hooks.strong.iter().any(|o| lower.starts_with(o.as_str())) {
        s += 0.3;
        reasons.push("a strong first line".into());
    }
    s.min(1.0)
}

/// The moments of `words` that `ask` wants, best first.
pub fn find(words: &[Word], ask: &Ask, lists: &klipp_core::lang::Words) -> Vec<ShortMoment> {
    let ss = sentences(words);
    if ss.is_empty() {
        return Vec::new();
    }
    // How fast the whole is spoken, for each moment's energy against it.
    let spoken: f64 = ss.iter().map(|s| (s.end - s.start).max(0.0)).sum();
    let rate_all = ss.iter().map(|s| s.words).sum::<usize>() as f64 / spoken.max(1e-6);
    let include: Vec<String> = ask.include.iter().map(|w| w.to_lowercase()).collect();
    let exclude: Vec<String> = ask.exclude.iter().map(|w| w.to_lowercase()).collect();
    let mut found: Vec<ShortMoment> = Vec::new();
    for i in 0..ss.len() {
        if ss[i].gap_before < START_PAUSE {
            continue;
        }
        for j in i..ss.len() {
            let len = ss[j].end - ss[i].start;
            if len > ask.max_secs {
                break;
            }
            if len < ask.min_secs || !(ss[j].closed || ss[j].gap_after >= 0.5) {
                continue;
            }
            let span = &ss[i..=j];
            let text = span.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join(" ");
            let lower = text.to_lowercase();
            if exclude.iter().any(|w| lower.contains(w.as_str())) || (!include.is_empty() && !include.iter().any(|w| lower.contains(w.as_str()))) {
                continue;
            }
            let mut reasons = Vec::new();
            let hook = hook_score(&ss[i], lists, &mut reasons);
            let opens_clean = !tokens(&ss[i].text).first().is_some_and(|w| lists.hooks.dangling.contains(w));
            let ends_clean = ss[j].closed && ss[j].gap_after >= 0.4;
            let alone = 0.6 * f32::from(u8::from(opens_clean)) + 0.4 * f32::from(u8::from(ends_clean));
            if !opens_clean {
                reasons.push("opens mid-thought".into());
            }
            let turn = span.len() > 1 && span[1..].iter().any(|s| {
                let l = s.text.to_lowercase();
                lists.hooks.turns.iter().any(|t| l.starts_with(t.as_str()) || l.contains(&format!(" {t} ")))
            });
            let payoff = if turn && ends_clean { 1.0 } else { 0.0 };
            if payoff > 0.0 {
                reasons.push("turns to a point and lands it".into());
            }
            let laugh = if lower.contains("laugh") || lower.contains("chuckle") { 1.0 } else { 0.0 };
            if laugh > 0.0 {
                reasons.push("a laugh".into());
            }
            let words_in: usize = span.iter().map(|s| s.words).sum();
            let rate = words_in as f64 / span.iter().map(|s| (s.end - s.start).max(0.0)).sum::<f64>().max(1e-6);
            let energy = ((rate / rate_all.max(1e-6) - 0.5).clamp(0.0, 1.0)) as f32;
            if energy > 0.75 {
                reasons.push("spoken with pace".into());
            }
            let fit = (1.0 - (len - ask.target_secs).abs() / ask.target_secs.max(1.0)).clamp(0.0, 1.0) as f32;
            let score = 0.3 * hook + 0.25 * alone + 0.15 * payoff + 0.1 * laugh + 0.1 * energy + 0.1 * fit;
            found.push(ShortMoment { from_secs: ss[i].start, to_secs: ss[j].end, hook: ss[i].text.clone(), text, score, reasons });
        }
    }
    found.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.from_secs.total_cmp(&b.from_secs)));
    let mut picked: Vec<ShortMoment> = Vec::new();
    for m in found {
        if picked.len() >= ask.count {
            break;
        }
        if picked.iter().all(|p| m.to_secs <= p.from_secs || m.from_secs >= p.to_secs) {
            picked.push(m);
        }
    }
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Words of `lines`, each a sentence spoken at three words a second,
    /// with `gaps` seconds of silence before each.
    fn spoken(lines: &[(&str, f64)]) -> Vec<Word> {
        let mut t = 0.0;
        let mut out = Vec::new();
        for (line, gap) in lines {
            t += gap;
            for w in line.split_whitespace() {
                out.push(Word { start_secs: t, end_secs: t + 0.3, text: w.into(), confidence: 0.9, corrected: false });
                t += 0.33;
            }
        }
        out
    }

    fn talk() -> Vec<Word> {
        spoken(&[
            ("and then we went back to the office and kept going with the same plan for a while.", 0.1),
            ("it was fine I guess and nothing much changed over the next few weeks of the project.", 0.2),
            ("Why do most startups fail in their first year?", 1.2),
            ("It is not the money and it is not the market and it is not the idea either.", 0.2),
            ("We looked at two hundred companies over ten years and the same thing kept coming up.", 0.2),
            ("But the founders who made it all did one thing differently from the rest.", 0.2),
            ("They talked to their customers every single week, no matter what.", 0.2),
            ("so yeah that was that and then we wrapped up the meeting and went home for the day.", 1.0),
            ("and the next morning it all started again with the same people in the same room.", 0.2),
        ])
    }

    /// #2720: the stretch that opens on a question after a pause, turns
    /// with "but" and lands its point ranks first; one that opens on
    /// "and" doesn't, and moments never overlap and keep to the length.
    #[test]
    fn a_hook_that_lands_its_point_ranks_first() {
        let ask = Ask { target_secs: 20.0, min_secs: 10.0, max_secs: 30.0, count: 3, ..Ask::default() };
        let found = find(&talk(), &ask, &klipp_core::lang::english());
        assert!(!found.is_empty());
        let best = &found[0];
        assert!(best.hook.starts_with("Why do most startups"), "{best:?}");
        assert!(best.reasons.iter().any(|r| r.contains("question")) && best.reasons.iter().any(|r| r.contains("point")), "{:?}", best.reasons);
        for m in &found {
            assert!((10.0..=30.0).contains(&(m.to_secs - m.from_secs)), "{m:?}");
            assert!(!m.hook.starts_with("and ") || m.score < best.score);
        }
        for (a, b) in found.iter().zip(found.iter().skip(1)) {
            assert!(a.score >= b.score);
        }
        for (k, a) in found.iter().enumerate() {
            for b in &found[k + 1..] {
                assert!(a.to_secs <= b.from_secs || a.from_secs >= b.to_secs, "{a:?} overlaps {b:?}");
            }
        }
    }

    /// A topic left out is never in a moment; one asked for always is.
    #[test]
    fn topics_include_and_exclude() {
        let ask = Ask { target_secs: 20.0, min_secs: 10.0, max_secs: 30.0, count: 5, ..Ask::default() };
        let without = find(&talk(), &Ask { exclude: vec!["customers".into()], ..ask.clone() }, &klipp_core::lang::english());
        assert!(without.iter().all(|m| !m.text.to_lowercase().contains("customers")));
        let with = find(&talk(), &Ask { include: vec!["office".into()], ..ask }, &klipp_core::lang::english());
        assert!(!with.is_empty() && with.iter().all(|m| m.text.to_lowercase().contains("office")));
    }

    /// Too short a talk for the length asked gives none.
    #[test]
    fn nothing_long_enough_is_nothing() {
        assert!(find(&spoken(&[("Hello there.", 0.0)]), &Ask::default(), &klipp_core::lang::english()).is_empty());
        assert!(find(&[], &Ask::default(), &klipp_core::lang::english()).is_empty());
    }
}
