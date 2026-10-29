//! Word lists by language for the transcript heuristics (#2889): the
//! hesitation sounds a filler cut takes out, the words that make a short's
//! opening line a hook, and the words that label a review comment. Each
//! language is a data file, `assets/lang/<code>/words.toml` for those Klipp
//! ships, and `lang/<code>/words.toml` in the user's Klipp data folder for
//! one added (or a shipped one changed). A transcript's language picks its
//! list; a language with none is said, not matched against nothing.
//!
//! A user's file is read as untrusted: size-capped, its format checked,
//! an unknown field refused, each list and word bounded.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use serde::Deserialize;

/// The format the files are in; a later one is refused.
pub const FORMAT: u32 = 1;
/// The language a transcript with none named is read in.
pub const DEFAULT: &str = "en";
/// The most a user's file may weigh.
const MAX_FILE_BYTES: u64 = 256 * 1024;
/// The most words one list may hold, and the longest word.
const MAX_WORDS: usize = 2000;
const MAX_WORD_CHARS: usize = 64;

/// The languages Klipp ships, each its file.
const SHIPPED: &[(&str, &str)] = &[
    ("en", include_str!("../../../assets/lang/en/words.toml")),
    ("sv", include_str!("../../../assets/lang/sv/words.toml")),
];

/// One language's lists, each lower case.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Words {
    pub format: u32,
    pub language: String,
    /// Hesitation sounds a filler cut offers to take out.
    pub fillers: Vec<String>,
    pub hooks: Hooks,
    pub review: Review,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hooks {
    /// Words that speak to the viewer.
    pub address: Vec<String>,
    /// Openers that leave a sentence hanging on what came before.
    pub dangling: Vec<String>,
    /// Openers that pull a viewer in.
    pub strong: Vec<String>,
    /// Words that turn toward a point.
    pub turns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    /// Words before a colon that label a comment rather than sign it.
    pub labels: Vec<String>,
}

impl Words {
    /// No words at all, for a language with no list: what is read by
    /// words alone finds nothing in it.
    pub fn none(language: &str) -> Self {
        let empty = Vec::new;
        Self { format: FORMAT, language: language.to_string(), fillers: empty(), hooks: Hooks { address: empty(), dangling: empty(), strong: empty(), turns: empty() }, review: Review { labels: empty() } }
    }

    /// Whether `text` is one of the fillers, whatever its case and
    /// punctuation.
    pub fn is_filler(&self, text: &str) -> bool {
        let bare: String = text.chars().filter(|c| c.is_alphabetic()).collect::<String>().to_lowercase();
        !bare.is_empty() && self.fillers.contains(&bare)
    }

    /// Read from a file's text, checked: its format, every list bounded.
    fn read(text: &str) -> Result<Self, String> {
        let peek: toml::Table = toml::from_str(text).map_err(|e| format!("not a word list: {e}"))?;
        let format = peek.get("format").and_then(toml::Value::as_integer).unwrap_or(0);
        if format > i64::from(FORMAT) {
            return Err(format!("a word list of a later Klipp (format {format})"));
        }
        let mut words: Words = toml::from_str(text).map_err(|e| format!("not a word list: {e}"))?;
        for list in [&mut words.fillers, &mut words.hooks.address, &mut words.hooks.dangling, &mut words.hooks.strong, &mut words.hooks.turns, &mut words.review.labels] {
            if list.len() > MAX_WORDS || list.iter().any(|w| w.chars().count() > MAX_WORD_CHARS) {
                return Err("a word list too long, or a word too long".into());
            }
            for w in list.iter_mut() {
                *w = w.trim().to_lowercase();
            }
        }
        Ok(words)
    }
}

/// `language` as a list is named: its first part, lower case
/// (`en-US` → `en`); none for nothing named.
pub fn code(language: &str) -> Option<String> {
    let code = language.trim().split(['-', '_']).next().unwrap_or("").to_lowercase();
    (!code.is_empty() && code.chars().all(|c| c.is_ascii_alphabetic()) && code.len() <= 8).then_some(code)
}

/// Where a user's lists are kept: `KLIPP_LANG_DIR` when set (tests), else
/// `klipp/lang` in the user's data folder.
pub fn user_dir() -> Option<PathBuf> {
    std::env::var_os("KLIPP_LANG_DIR").map(PathBuf::from).or_else(|| dirs::data_dir().map(|d| d.join("klipp").join("lang")))
}

/// Every list there is, by code: the shipped ones, each replaced by the
/// user's file of its code, and the user's for languages added; a user's
/// file that fails its checks is left out with a warning.
fn load(user: Option<&Path>) -> BTreeMap<String, Arc<Words>> {
    let mut out = BTreeMap::new();
    for (code, text) in SHIPPED {
        match Words::read(text) {
            Ok(w) => {
                out.insert((*code).to_string(), Arc::new(w));
            }
            Err(why) => tracing::error!("the shipped {code} word list does not read: {why}"),
        }
    }
    let Some(dir) = user else { return out };
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for entry in entries.flatten() {
        let Some(code) = entry.file_name().to_str().and_then(code) else { continue };
        let file = entry.path().join("words.toml");
        let read = || -> Result<Words, String> {
            let meta = std::fs::symlink_metadata(&file).map_err(|e| e.to_string())?;
            if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
                return Err("not a word list this reads".into());
            }
            Words::read(&std::fs::read_to_string(&file).map_err(|e| e.to_string())?)
        };
        match read() {
            Ok(w) => {
                out.insert(code, Arc::new(w));
            }
            Err(why) => tracing::warn!("{}: {why}; left out", file.display()),
        }
    }
    out
}

fn all_lists() -> &'static BTreeMap<String, Arc<Words>> {
    static LISTS: OnceLock<BTreeMap<String, Arc<Words>>> = OnceLock::new();
    LISTS.get_or_init(|| load(user_dir().as_deref()))
}

/// The lists for `language` (a transcript's, `None` for one not named:
/// English): `Err` names the language there is no list for, and the
/// languages there are.
pub fn words(language: Option<&str>) -> Result<Arc<Words>, String> {
    let code = language.and_then(code).unwrap_or_else(|| DEFAULT.to_string());
    let lists = all_lists();
    lists.get(&code).cloned().ok_or_else(|| {
        format!("no word list for the language \"{code}\" (there are lists for {}); add one as lang/{code}/words.toml in Klipp's data folder", lists.keys().cloned().collect::<Vec<_>>().join(", "))
    })
}

/// Every language's lists, for what is not said in one language (a
/// review's labels).
pub fn every() -> Vec<Arc<Words>> {
    all_lists().values().cloned().collect()
}

/// The English lists, always there.
pub fn english() -> Arc<Words> {
    words(Some(DEFAULT)).expect("the English list ships with Klipp")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shipped_list_reads() {
        for (code, text) in SHIPPED {
            let w = Words::read(text).unwrap_or_else(|e| panic!("{code}: {e}"));
            assert_eq!(&w.language, code);
            assert!(!w.fillers.is_empty() && !w.hooks.strong.is_empty() && !w.review.labels.is_empty(), "{code}");
        }
    }

    #[test]
    fn a_language_picks_its_list_and_one_without_is_said() {
        assert!(words(None).unwrap().is_filler("Uhh..."), "English when none is named");
        assert!(words(Some("en-US")).unwrap().is_filler("um"));
        assert!(!english().is_filler("umbrella") && !english().is_filler("like"));
        let sv = words(Some("sv")).unwrap();
        assert!(sv.is_filler("Öhm,") && !sv.is_filler("um"));
        let err = words(Some("xx")).unwrap_err();
        assert!(err.contains("no word list for the language \"xx\"") && err.contains("en, sv"), "{err}");
    }

    /// A user's list adds a language and is read as untrusted: a later
    /// format, an unknown field, a too-large file or a list too long is
    /// left out, the others kept.
    #[test]
    fn a_users_list_is_added_and_a_hostile_one_left_out() {
        let dir = std::env::temp_dir().join(format!("klipp-lang-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let list = |code: &str, body: &str| {
            std::fs::create_dir_all(dir.join(code)).unwrap();
            std::fs::write(dir.join(code).join("words.toml"), body).unwrap();
        };
        let good = |code: &str, fillers: &str| format!("format = 1\nlanguage = \"{code}\"\nfillers = [{fillers}]\n[hooks]\naddress = []\ndangling = []\nstrong = []\nturns = []\n[review]\nlabels = []\n");
        list("de", &good("de", "\"äh\", \"ähm\""));
        list("fr", &good("fr", "\"euh\"").replace("format = 1", "format = 2"));
        list("es", &format!("{}extra = 1\n", good("es", "\"eh\"")));
        list("it", &good("it", &vec!["\"ehm\""; MAX_WORDS + 1].join(", ")));
        list("nl", &"x".repeat(MAX_FILE_BYTES as usize + 1));
        list("en", &good("en", "\"uhm\""));
        let lists = load(Some(&dir));
        assert!(lists["de"].is_filler("Äh"), "an added language");
        for code in ["fr", "es", "it", "nl"] {
            assert!(!lists.contains_key(code), "{code} was read");
        }
        assert_eq!(lists["en"].fillers, ["uhm"], "the user's file replaces the shipped one");
        assert!(lists.contains_key("sv"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
