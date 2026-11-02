//! Where words begin in text that puts no spaces between them (#3238):
//! Thai, Lao, Khmer and Burmese, whose characters UAX #14 gives the class
//! SA, "complex context", and leaves to a dictionary. The line breaker
//! above finds no break inside such a run, so a long Thai line could only
//! break at its few spaces. ICU4X's dictionary segmenter finds the words;
//! everything else in the text is left to the rules it had.

use icu_segmenter::options::WordBreakInvariantOptions;
use icu_segmenter::WordSegmenter;

/// Whether `c` is written without spaces between words and needs a
/// dictionary to find them: the Thai, Lao, Khmer and Myanmar blocks and
/// their extensions.
pub fn is_complex(c: char) -> bool {
    matches!(c as u32,
        0x0E00..=0x0E7F // Thai
        | 0x0E80..=0x0EFF // Lao
        | 0x1000..=0x109F // Myanmar
        | 0x1780..=0x17FF // Khmer
        | 0x19E0..=0x19FF // Khmer symbols
        | 0xA9E0..=0xA9FF // Myanmar Extended-B
        | 0xAA60..=0xAA7F // Myanmar Extended-A
    )
}

/// The byte offsets in `text` where a word begins inside a run of
/// [`is_complex`] characters, by the dictionaries: between two such
/// characters only, so the rest of the text breaks as it did.
pub fn complex_breaks(text: &str) -> Vec<usize> {
    if !text.chars().any(is_complex) {
        return Vec::new();
    }
    let segmenter = WordSegmenter::new_dictionary(WordBreakInvariantOptions::default());
    segmenter
        .segment_str(text)
        .filter(|&at| {
            let before = text[..at].chars().next_back();
            let after = text[at..].chars().next();
            before.is_some_and(is_complex) && after.is_some_and(is_complex)
        })
        .collect()
}

/// The byte offsets in `text` where words begin: after whitespace, or at
/// the start, as before, and inside runs of [`is_complex`] characters
/// where [`complex_breaks`] finds a word beginning.
pub fn word_starts(text: &str) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut prev_space = true;
    for (i, c) in text.char_indices() {
        if !c.is_whitespace() && prev_space {
            starts.push(i);
        }
        prev_space = c.is_whitespace();
    }
    starts.extend(complex_breaks(text));
    starts.sort_unstable();
    starts.dedup();
    starts
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The words of `text` by `breaks`.
    fn words<'a>(text: &'a str, breaks: &[usize]) -> Vec<&'a str> {
        let mut cuts = vec![0];
        cuts.extend_from_slice(breaks);
        cuts.push(text.len());
        cuts.windows(2).map(|w| &text[w[0]..w[1]]).collect()
    }

    /// In each script, sentences whose words are known break only between
    /// them, and break at least once: the dictionaries may keep a compound
    /// whole, but never split a word.
    #[test]
    fn complex_scripts_break_only_between_words() {
        let cases: [(&str, &[&str]); 5] = [
            // Thai: "Thai is very easy" and "hello".
            ("th", &["ภาษา", "ไทย", "ง่าย", "นิด", "เดียว"]),
            ("th", &["สวัสดี", "ครับ"]),
            // Lao: "the Lao language".
            ("lo", &["ພາສາ", "ລາວ"]),
            // Khmer: "I love you".
            ("km", &["ខ្ញុំ", "ស្រលាញ់", "អ្នក"]),
            // Burmese: "Myanmar writing".
            ("my", &["မြန်မာ", "စာ"]),
        ];
        for (lang, known) in cases {
            let text: String = known.concat();
            let mut allowed = Vec::new();
            let mut at = 0;
            for w in &known[..known.len() - 1] {
                at += w.len();
                allowed.push(at);
            }
            let breaks = complex_breaks(&text);
            assert!(!breaks.is_empty(), "{lang}: {text} found no word in it");
            assert!(breaks.iter().all(|b| allowed.contains(b)), "{lang}: {:?} splits a word of {known:?}", words(&text, &breaks));
        }
    }

    /// Other text, and the edges of a complex run, keep the breaks they
    /// had: no boundary between a space or a Latin letter and Thai.
    #[test]
    fn only_complex_runs_gain_breaks() {
        assert!(complex_breaks("Hello world, 日本語 text").is_empty());
        let text = "Klipp ภาษาไทย ok";
        for b in complex_breaks(text) {
            let (before, after) = (text[..b].chars().next_back().unwrap(), text[b..].chars().next().unwrap());
            assert!(is_complex(before) && is_complex(after), "a break at {b} beside {before:?} {after:?}");
        }
        assert_eq!(word_starts("two  words"), [0, 5]);
        let mixed = word_starts("Klipp ภาษาไทย ok");
        assert!(mixed.len() > 3 && mixed.contains(&0) && mixed.contains(&6), "{mixed:?}");
    }
}
