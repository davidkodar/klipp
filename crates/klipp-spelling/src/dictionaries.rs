//! The Hunspell dictionaries installed on the system (#3236), found by a
//! language tag and read as untrusted input.
//!
//! Klipp ships no dictionary. One is looked for, by the file names a tag
//! gives ([`file_stems`]), in each folder [`system_dirs`] lists. A
//! dictionary is a pair of text files anyone can drop into one of those
//! folders, so it is read as hostile: its name only ever made from a
//! checked tag, never from a path a project names; each file capped in
//! size; its declared encoding (`SET`) honoured or the dictionary refused;
//! a file that does not parse refused with its name. A dictionary is read
//! once a session and kept.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The largest `.aff` file read; real ones are well under a megabyte.
pub const MAX_AFF_BYTES: u64 = 4 << 20;
/// The largest `.dic` file read; the largest real ones are about 5 MB.
pub const MAX_DIC_BYTES: u64 = 32 << 20;

/// A dictionary read and ready: the checker, the file it came from, and
/// the letters it spells words in.
pub struct Dictionary {
    pub checker: klipp_spell::Dictionary,
    /// The file stem it was read from (`en_US`).
    pub name: String,
    /// The letters its words are written in: a word with a letter outside
    /// them is in another script, and is not marked.
    pub alphabet: HashSet<char>,
}

/// Why a language has no dictionary: none found, or one found and
/// refused.
#[derive(Debug, Clone, PartialEq)]
pub enum Missing {
    /// No dictionary for the language: install `package` (as the
    /// distributions name it) to check it.
    NotInstalled { language: String, packages: String },
    /// A dictionary found and refused, with why.
    Refused { language: String, why: String },
}

impl Missing {
    /// What a person is told, once.
    pub fn say(&self) -> String {
        match self {
            Missing::NotInstalled { language, packages } => format!("no {language} dictionary is installed; install {packages} to check {language} spelling"),
            Missing::Refused { language, why } => format!("the {language} dictionary was not read: {why}"),
        }
    }
}

/// The dictionaries a session reads, each once.
pub struct Dictionaries {
    dirs: Vec<PathBuf>,
    /// The region of the system's locale (`GB` for `en_GB.UTF-8`): the
    /// first choice for a tag without one.
    region: Option<String>,
    loaded: Mutex<HashMap<String, Result<Arc<Dictionary>, Missing>>>,
}

impl Dictionaries {
    /// Looked for in `dirs`, in order, a region-less tag preferring
    /// `region`.
    pub fn in_dirs(dirs: Vec<PathBuf>, region: Option<String>) -> Self {
        Self { dirs, region, loaded: Mutex::new(HashMap::new()) }
    }

    /// The system's: [`system_dirs`], and the region of `LANG`.
    pub fn system() -> Self {
        Self::in_dirs(system_dirs(), locale_region(std::env::var("LANG").ok().as_deref()))
    }

    /// The session's one set of system dictionaries.
    pub fn shared() -> &'static Dictionaries {
        static SHARED: std::sync::OnceLock<Dictionaries> = std::sync::OnceLock::new();
        SHARED.get_or_init(Dictionaries::system)
    }

    /// The dictionary for `language` (a tag: `en`, `en-GB`, `de_AT`), read
    /// the first time it is asked for.
    pub fn get(&self, language: &str) -> Result<Arc<Dictionary>, Missing> {
        let key = language.trim().to_ascii_lowercase().replace('_', "-");
        if let Some(found) = self.loaded.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
            return found.clone();
        }
        let found = self.read(&key);
        if let Err(missing) = &found {
            tracing::info!("spelling: {}", missing.say());
        }
        self.loaded.lock().unwrap_or_else(|e| e.into_inner()).insert(key, found.clone());
        found
    }

    fn read(&self, tag: &str) -> Result<Arc<Dictionary>, Missing> {
        let Some(stems) = file_stems(tag, self.region.as_deref()) else {
            return Err(Missing::Refused { language: tag.to_string(), why: format!("{tag:?} is not a language tag") });
        };
        for stem in &stems {
            for dir in &self.dirs {
                let (aff, dic) = (dir.join(format!("{stem}.aff")), dir.join(format!("{stem}.dic")));
                if aff.is_file() && dic.is_file() {
                    return read_pair(&aff, &dic, stem).map(Arc::new).map_err(|why| Missing::Refused { language: tag.to_string(), why });
                }
            }
        }
        let lang = tag.split('-').next().unwrap_or(tag);
        Err(Missing::NotInstalled { language: tag.to_string(), packages: format!("hunspell-{lang} (Arch-based), hunspell-{lang} or hunspell-{lang}-* (Ubuntu, Fedora)") })
    }
}

/// The folders dictionaries are looked for in: the user's own
/// (`$XDG_DATA_HOME/hunspell`), each of `$XDG_DATA_DIRS`' `hunspell`, then
/// the usual system folders.
pub fn system_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(home) = dirs::data_dir() {
        out.push(home.join("hunspell"));
    }
    let shared = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    for d in shared.split(':').filter(|d| !d.is_empty()) {
        out.push(Path::new(d).join("hunspell"));
    }
    for d in ["/usr/share/hunspell", "/usr/share/myspell/dicts", "/usr/share/myspell"] {
        out.push(PathBuf::from(d));
    }
    let mut seen = HashSet::new();
    out.retain(|d| seen.insert(d.clone()));
    out
}

/// The region of a locale such as `en_GB.UTF-8`, upper case.
pub fn locale_region(lang: Option<&str>) -> Option<String> {
    let code = lang?.split(['.', '@']).next()?;
    let region = code.split('_').nth(1)?;
    (region.len() == 2 && region.chars().all(|c| c.is_ascii_alphabetic())).then(|| region.to_ascii_uppercase())
}

/// The language of the system's locale (`en` for `en_GB.UTF-8`), when it
/// has one.
pub fn locale_language(lang: Option<&str>) -> Option<String> {
    let code = lang?.split(['.', '@', '_']).next()?.to_ascii_lowercase();
    ((2..=3).contains(&code.len()) && code.chars().all(|c| c.is_ascii_lowercase())).then_some(code)
}

/// The file stems a language tag is looked for by, in order: `de-AT` as
/// `de_AT`, then the language's home region (`de_DE`, `sv_SE`), then the
/// bare language; a tag with no region tries `region` first. `None` for what
/// is not a tag (two or three letters, an optional two-letter region):
/// nothing else is ever made into a file name.
pub fn file_stems(tag: &str, region: Option<&str>) -> Option<Vec<String>> {
    let mut parts = tag.split(['-', '_']);
    let lang = parts.next()?.to_ascii_lowercase();
    let reg = parts.next().map(|r| r.to_ascii_uppercase());
    if parts.next().is_some() || !(2..=3).contains(&lang.len()) || !lang.chars().all(|c| c.is_ascii_lowercase()) {
        return None;
    }
    if reg.as_ref().is_some_and(|r| r.len() != 2 || !r.chars().all(|c| c.is_ascii_uppercase())) {
        return None;
    }
    let mut out = Vec::new();
    let mut push = |s: String| {
        if !out.contains(&s) {
            out.push(s);
        }
    };
    match reg {
        Some(r) => push(format!("{lang}_{r}")),
        None => {
            if let Some(r) = region.filter(|r| r.len() == 2 && r.chars().all(|c| c.is_ascii_alphabetic())) {
                push(format!("{lang}_{}", r.to_ascii_uppercase()));
            }
        }
    }
    // The language's home region, and a few defaults where it isn't the
    // language's own code upper-cased.
    let home = match lang.as_str() {
        "en" => vec!["en_US", "en_GB"],
        "sv" => vec!["sv_SE"],
        "da" => vec!["da_DK"],
        "nb" => vec!["nb_NO"],
        "nn" => vec!["nn_NO"],
        "cs" => vec!["cs_CZ"],
        "el" => vec!["el_GR"],
        "uk" => vec!["uk_UA"],
        "ja" => vec!["ja_JP"],
        "ko" => vec!["ko_KR"],
        "ca" => vec!["ca_ES"],
        "pt" => vec!["pt_PT", "pt_BR"],
        _ => Vec::new(),
    };
    if home.is_empty() {
        push(format!("{lang}_{}", lang.to_ascii_uppercase()));
    }
    for h in home {
        push(h.to_string());
    }
    push(lang);
    Some(out)
}

/// The pair `aff`/`dic` read, checked and parsed; why not, when refused.
pub fn read_pair(aff: &Path, dic: &Path, name: &str) -> Result<Dictionary, String> {
    let aff_bytes = read_capped(aff, MAX_AFF_BYTES)?;
    let dic_bytes = read_capped(dic, MAX_DIC_BYTES)?;
    let encoding = declared_encoding(&aff_bytes).map_err(|why| format!("{}: {why}", aff.display()))?;
    let decode = |bytes: &[u8], path: &Path| -> Result<String, String> {
        let (text, _, bad) = encoding.decode(bytes);
        if bad {
            return Err(format!("{}: not {} text", path.display(), encoding.name()));
        }
        Ok(text.into_owned())
    };
    let (aff_text, dic_text) = (decode(&aff_bytes, aff)?, decode(&dic_bytes, dic)?);
    let checker = klipp_spell::Dictionary::new(&aff_text, &dic_text).map_err(|e| format!("{} does not read as a Hunspell dictionary: {e}", aff.display()))?;
    Ok(Dictionary { checker, name: name.to_string(), alphabet: alphabet(&aff_text, &dic_text) })
}

/// A file's bytes, refused past `cap` before any is read.
fn read_capped(path: &Path, cap: u64) -> Result<Vec<u8>, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    if meta.len() > cap {
        return Err(format!("{} is {} bytes, more than the {cap} a dictionary file may be", path.display(), meta.len()));
    }
    use std::io::Read;
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    std::fs::File::open(path).and_then(|f| f.take(cap + 1).read_to_end(&mut bytes)).map_err(|e| format!("{}: {e}", path.display()))?;
    if bytes.len() as u64 > cap {
        return Err(format!("{} grew past {cap} bytes while read", path.display()));
    }
    Ok(bytes)
}

/// The encoding an `.aff` file's `SET` line declares (UTF-8 when it has
/// none); refused when it names one not known.
fn declared_encoding(aff: &[u8]) -> Result<&'static encoding_rs::Encoding, String> {
    for line in aff.split(|b| *b == b'\n').take(200) {
        let line = String::from_utf8_lossy(line);
        let mut words = line.split_whitespace();
        if words.next() == Some("SET") {
            let label = words.next().unwrap_or("");
            // Hunspell's names for Microsoft's code pages and KOI8 differ
            // from the WHATWG labels.
            let label = match label.to_ascii_uppercase().as_str() {
                "MICROSOFT-CP1251" => "windows-1251".to_string(),
                "ISCII-DEVANAGARI" => return Err("its encoding, ISCII-DEVANAGARI, is not read".into()),
                l => l.to_string(),
            };
            return encoding_rs::Encoding::for_label(label.as_bytes()).ok_or_else(|| format!("its encoding, {label}, is not one known"));
        }
    }
    Ok(encoding_rs::UTF_8)
}

/// The letters a dictionary spells in: its `TRY` and `WORDCHARS` lines',
/// and those of the first words of its word list.
fn alphabet(aff: &str, dic: &str) -> HashSet<char> {
    let mut out = HashSet::new();
    for line in aff.lines() {
        let mut words = line.split_whitespace();
        if matches!(words.next(), Some("TRY") | Some("WORDCHARS")) {
            out.extend(words.next().unwrap_or("").chars().flat_map(char::to_lowercase).filter(|c| c.is_alphabetic()));
        }
    }
    for line in dic.lines().skip(1).take(5000) {
        out.extend(line.split('/').next().unwrap_or("").chars().flat_map(char::to_lowercase).filter(|c| c.is_alphabetic()));
    }
    out
}
