//! The person's own words (#3236): a word list Klipp keeps in its
//! config folder, one word a line, that every project's spelling takes
//! as right. "Add to dictionary" adds to it.
//!
//! The file is the person's, but anything can write it, so it is read
//! with a cap and its lines checked; a word is added by writing the whole
//! list to a new file and moving it over the old one, so a crash leaves
//! the old list or the new one, never half of one.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The largest word list read.
pub const MAX_BYTES: u64 = 1 << 20;
/// The longest word kept.
pub const MAX_WORD_CHARS: usize = 64;

/// Where the list is: `~/.config/klipp/dictionary.txt`.
pub fn default_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("klipp").join("dictionary.txt"))
}

/// The words of the list at `path`; none when it is not there, too big
/// or not text. A line that is not a word ([`is_word`]) is passed over.
pub fn read(path: &Path) -> BTreeSet<String> {
    let Ok(meta) = std::fs::metadata(path) else { return BTreeSet::new() };
    if !meta.is_file() || meta.len() > MAX_BYTES {
        tracing::warn!("spelling: {} is not read: not a file, or more than {MAX_BYTES} bytes", path.display());
        return BTreeSet::new();
    }
    let Ok(text) = std::fs::read_to_string(path) else { return BTreeSet::new() };
    text.lines().map(str::trim).filter(|w| is_word(w)).map(str::to_string).collect()
}

/// Whether `word` can go on the list: one word of letters (an apostrophe
/// or hyphen inside it allowed), no longer than [`MAX_WORD_CHARS`].
pub fn is_word(word: &str) -> bool {
    let n = word.chars().count();
    (1..=MAX_WORD_CHARS).contains(&n)
        && word.chars().any(char::is_alphabetic)
        && word.chars().all(|c| c.is_alphabetic() || matches!(c, '\'' | '\u{2019}' | '-') || unicode_mark(c))
}

/// A combining mark, as some scripts write a letter with.
fn unicode_mark(c: char) -> bool {
    matches!(c as u32, 0x0300..=0x036F | 0x0900..=0x0DFF | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF)
}

/// `word` added to the list at `path`, which is made if need be.
/// Refused when it is not a word, or the list would grow past
/// [`MAX_BYTES`].
pub fn learn(path: &Path, word: &str) -> Result<(), String> {
    let word = word.trim();
    if !is_word(word) {
        return Err(format!("{word:?} is not one word, so it is not added"));
    }
    let mut words = read(path);
    if !words.insert(word.to_string()) {
        return Ok(());
    }
    let mut text = String::new();
    for w in &words {
        text.push_str(w);
        text.push('\n');
    }
    if text.len() as u64 > MAX_BYTES {
        return Err(format!("the word list is full: {} would be more than {MAX_BYTES} bytes", path.display()));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let new = path.with_extension("txt.new");
    std::fs::write(&new, text).map_err(|e| format!("{}: {e}", new.display()))?;
    std::fs::rename(&new, path).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("klipp-spelling-{name}-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir.join("dictionary.txt")
    }

    #[test]
    fn a_learnt_word_is_read_back_and_a_bad_line_passed_over() {
        let path = scratch("learn");
        let _ = std::fs::remove_file(&path);
        assert!(read(&path).is_empty(), "no list: no words");
        learn(&path, "Omarchy").unwrap();
        learn(&path, "Hyprland").unwrap();
        learn(&path, "Omarchy").unwrap();
        assert_eq!(read(&path).into_iter().collect::<Vec<_>>(), ["Hyprland", "Omarchy"]);
        assert!(learn(&path, "two words").is_err());
        assert!(learn(&path, "../etc").is_err());
        assert!(learn(&path, &"a".repeat(MAX_WORD_CHARS + 1)).is_err());
        std::fs::write(&path, "Omarchy\nnot a word\n123\nwgpu\n").unwrap();
        assert_eq!(read(&path).into_iter().collect::<Vec<_>>(), ["Omarchy", "wgpu"]);
    }

    #[test]
    fn an_oversized_list_is_not_read() {
        let path = scratch("big");
        std::fs::write(&path, "word\n".repeat(MAX_BYTES as usize / 5 + 1)).unwrap();
        assert!(read(&path).is_empty());
    }
}
