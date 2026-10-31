//! The words of a text that are checked (#3236), and which are not.
//!
//! A text is split into words at Unicode word boundaries. Not checked:
//! a word with a digit in it; a web address, an e-mail address, an
//! @handle or a #tag (the whole of each); and a word in a script the
//! dictionary does not spell in, so a Greek word in an English title, or
//! Chinese or Thai text with no dictionary, is not marked.

use std::collections::HashSet;

use unicode_segmentation::UnicodeSegmentation;

/// A word of a text, by its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word<'a> {
    pub text: &'a str,
    pub start: usize,
    pub end: usize,
}

/// The words of `text` that are checked, in order.
pub fn words(text: &str) -> Vec<Word<'_>> {
    let skip = skipped_spans(text);
    text.unicode_word_indices()
        .filter(|(start, w)| {
            let end = start + w.len();
            !skip.iter().any(|(a, b)| *start < *b && *a < end) && !w.chars().any(|c| c.is_numeric()) && w.chars().any(|c| c.is_alphabetic())
        })
        .map(|(start, w)| Word { text: w, start, end: start + w.len() })
        .collect()
}

/// Whether `word` is spelt in `alphabet` (lower case letters): a word
/// with any letter outside it is in another script.
pub fn in_alphabet(word: &str, alphabet: &HashSet<char>) -> bool {
    alphabet.is_empty() || word.chars().filter(|c| c.is_alphabetic()).flat_map(char::to_lowercase).all(|c| alphabet.contains(&c))
}

/// `word` as a dictionary takes it: a typographic apostrophe as a plain
/// one, as dictionaries write them.
pub fn for_dictionary(word: &str) -> std::borrow::Cow<'_, str> {
    if word.contains('\u{2019}') {
        word.replace('\u{2019}', "'").into()
    } else {
        word.into()
    }
}

/// The spans not checked: each whitespace-separated piece that is a web
/// or e-mail address, an @handle or a #tag.
fn skipped_spans(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in text.char_indices().chain(std::iter::once((text.len(), ' '))) {
        match (c.is_whitespace(), start) {
            (false, None) => start = Some(i),
            (true, Some(s)) => {
                let piece = text[s..i].trim_matches(|c: char| matches!(c, '(' | ')' | '"' | '\'' | ',' | '.' | ';' | ':' | '!' | '?'));
                let lower = piece.to_ascii_lowercase();
                if piece.starts_with('@') || piece.starts_with('#') || piece.contains('@') || lower.contains("://") || lower.starts_with("www.") || looks_like_domain(&lower) {
                    out.push((s, i));
                }
                start = None;
            }
            _ => {}
        }
    }
    out
}

/// `example.com`, `klipp.app/guide`: a dotted name ending in a top-level
/// domain of letters, with no spaces.
fn looks_like_domain(piece: &str) -> bool {
    let host = piece.split('/').next().unwrap_or(piece);
    let mut labels = host.split('.');
    let (Some(first), Some(_)) = (labels.next(), labels.next()) else { return false };
    let tld = host.rsplit('.').next().unwrap_or("");
    !first.is_empty() && (2..=6).contains(&tld.len()) && tld.chars().all(|c| c.is_ascii_lowercase()) && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(t: &str) -> Vec<&str> {
        words(t).into_iter().map(|w| w.text).collect()
    }

    #[test]
    fn words_skip_numbers_addresses_handles_and_tags() {
        assert_eq!(texts("Teh quick brown fox"), ["Teh", "quick", "brown", "fox"]);
        assert_eq!(texts("Part 2 of 3: the 4K cut"), ["Part", "of", "the", "cut"]);
        assert_eq!(texts("See https://klipp.app/guide or www.example.org"), ["See", "or"]);
        assert_eq!(texts("Mail me at ana@example.com, @anaruiz #klippedit"), ["Mail", "me", "at"]);
        assert_eq!(texts("Visit klipp.app today."), ["Visit", "today"]);
        assert_eq!(texts("don\u{2019}t"), ["don\u{2019}t"], "an apostrophe keeps a word whole");
        let w = &words("ab cd")[1];
        assert_eq!((w.start, w.end), (3, 5));
    }

    #[test]
    fn a_word_in_another_script_is_not_in_the_alphabet() {
        let latin: HashSet<char> = "abcdefghijklmnopqrstuvwxyzéèàç".chars().collect();
        assert!(in_alphabet("Café", &latin));
        assert!(!in_alphabet("Καλημέρα", &latin));
        assert!(!in_alphabet("東京", &latin));
        assert!(in_alphabet("東京", &HashSet::new()), "no alphabet known: everything checked");
    }
}
