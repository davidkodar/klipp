//! Which system font draws a character a title's face lacks (#1503).
//!
//! A title's text is set in the face its box or style run names; a
//! character that face has no glyph for is drawn from the first face in
//! a chain that has one: the system's fonts that cover it, then the
//! faces that ship with the app. This module is the system part of the
//! chain, without fontconfig: the font files under the font directories
//! are listed once, and each file's character map is read the first
//! time a character is looked for past it, then kept as ranges of
//! codepoints, so a later character is looked up in memory. Most
//! titles never read one, as their faces cover their text.
//!
//! The ranges are kept on disk too, in Klipp's cache folder (#1673),
//! with each file's path, size and modification time, and so are the
//! characters no file has: a later run reads only the files that are
//! new or changed since, and a character no font has is not searched
//! for again while the files are the ones that were read.
//!
//! Files are tried in a fixed order: a file without a style word in its
//! name (a Regular) before a bold or light one, then the larger file
//! first, then by path. The larger file is the fuller font: a pan-CJK
//! face before a small one that maps a handful of the same characters
//! for one historic script. An emoji font comes first for a character
//! drawn as emoji and last for any other, so a digit or a "#" never
//! falls to it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use read_fonts::tables::cmap::PlatformId;
use read_fonts::{FontRef, TableProvider};
use serde::{Deserialize, Serialize};

/// Bumped whenever the chain changes which face draws a character, so
/// a glyph cache laid out before does not match (#1503).
pub const FALLBACK_VERSION: u32 = 1;

/// Words in a font file's name that mark a face other than the Regular.
const STYLE_WORDS: &[&str] = &[
    "bold", "black", "heavy", "light", "thin", "medium", "semi", "demi", "extra", "ultra", "italic", "oblique", "condensed", "narrow",
];

/// Bumped whenever the coverage index's shape changes, so an index
/// written before is not read (#1673).
const INDEX_VERSION: u32 = 1;

/// A system font file and, once read, the codepoints it maps.
struct Candidate {
    path: PathBuf,
    emoji: bool,
    /// The file's size and modification time (nanoseconds since the
    /// epoch, 0 when unknown), which key its ranges on disk.
    size: u64,
    mtime: u64,
    ranges: Option<Vec<(u32, u32)>>,
}

/// The system's font files in the order they are tried, and every
/// decision made: the file that draws a character (as emoji or not).
struct SystemFonts {
    candidates: Vec<Candidate>,
    decided: HashMap<(char, bool), Option<usize>>,
    /// Where the coverage index is kept between runs; `None` keeps it
    /// for the process only.
    index: Option<PathBuf>,
    /// Font files read, for the tests (#1673).
    files_read: usize,
}

/// The coverage index on disk (#1673): the ranges of every font file
/// read, and the characters no file has.
#[derive(Serialize, Deserialize)]
struct CoverageIndex {
    version: u32,
    files: Vec<IndexedFile>,
    /// Characters, as emoji or not, that no file has; trusted only when
    /// every file listed now is in `files` unchanged.
    missing: Vec<(u32, bool)>,
}

#[derive(Serialize, Deserialize)]
struct IndexedFile {
    path: PathBuf,
    size: u64,
    mtime: u64,
    ranges: Vec<(u32, u32)>,
}

static SYSTEM: OnceLock<Mutex<SystemFonts>> = OnceLock::new();

/// The system font file that draws `c`, an emoji font first when
/// `emoji`; `None` when none has it.
pub(crate) fn system_font_for(c: char, emoji: bool) -> Option<PathBuf> {
    let fonts = SYSTEM.get_or_init(|| {
        let dirs: Vec<PathBuf> = crate::font_discovery::font_directories().into_iter().map(PathBuf::from).collect();
        let index = dirs::cache_dir().map(|d| d.join("klipp").join("font_coverage.json"));
        Mutex::new(SystemFonts::list(&dirs, index))
    });
    let mut fonts = fonts.lock().unwrap_or_else(|e| e.into_inner());
    fonts.find(c, emoji)
}

/// Whether `c` reads as emoji: a pictograph from the emoji blocks, or
/// any character asking for emoji presentation (`next` is U+FE0F).
pub(crate) fn wants_emoji(c: char, next: Option<char>) -> bool {
    (0x1F000..=0x1FAFF).contains(&(c as u32)) || next == Some('\u{FE0F}')
}

/// A character that belongs to the one before it and is drawn in its
/// face (#1503): a variation selector, a zero-width joiner, a skin-tone
/// modifier, the keycap mark or a tag, all parts of emoji sequences.
pub(crate) fn joins_previous(c: char) -> bool {
    matches!(c as u32, 0xFE00..=0xFE0F | 0x200D | 0x1F3FB..=0x1F3FF | 0x20E3 | 0xE0020..=0xE007F | 0xE0100..=0xE01EF)
}

impl SystemFonts {
    /// Every TrueType, OpenType and collection file under `dirs`, in
    /// the order they are tried, with the ranges of each the index at
    /// `index` holds unchanged; no file is read.
    fn list(dirs: &[PathBuf], index: Option<PathBuf>) -> Self {
        let mut paths = Vec::new();
        for dir in dirs {
            collect(dir, &mut paths);
        }
        let mut candidates: Vec<Candidate> = paths
            .into_iter()
            .map(|path| {
                let name = path.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
                let meta = std::fs::metadata(&path).ok();
                let size = meta.as_ref().map_or(0, |m| m.len());
                let mtime = meta
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_nanos() as u64);
                Candidate { emoji: name.contains("emoji"), path, size, mtime, ranges: None }
            })
            .collect();
        candidates.sort_by_cached_key(|c| {
            let name = c.path.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
            (STYLE_WORDS.iter().any(|w| name.contains(w)), std::cmp::Reverse(c.size), c.path.clone())
        });
        let mut fonts = Self { candidates, decided: HashMap::new(), index, files_read: 0 };
        fonts.read_index();
        fonts
    }

    /// Take the ranges of the files the index holds unchanged, and the
    /// characters no file has when every file is among them. An index
    /// that is missing, does not parse or is of another version gives
    /// nothing.
    fn read_index(&mut self) {
        let Some(path) = &self.index else { return };
        let Some(index) = std::fs::read(path).ok().and_then(|data| serde_json::from_slice::<CoverageIndex>(&data).ok()) else { return };
        if index.version != INDEX_VERSION {
            return;
        }
        let mut files: HashMap<PathBuf, IndexedFile> = index.files.into_iter().map(|f| (f.path.clone(), f)).collect();
        for candidate in &mut self.candidates {
            if let Some(file) = files.remove(&candidate.path) {
                if (file.size, file.mtime) == (candidate.size, candidate.mtime) {
                    candidate.ranges = Some(file.ranges);
                }
            }
        }
        if self.candidates.iter().all(|c| c.ranges.is_some()) {
            for (cp, emoji) in index.missing {
                if let Some(c) = char::from_u32(cp) {
                    self.decided.insert((c, emoji), None);
                }
            }
        }
    }

    /// Write the index: to a file of its own, then renamed over it, so
    /// a reader never sees half of one. A failure leaves it to the next
    /// run.
    fn write_index(&self) {
        let Some(path) = &self.index else { return };
        let index = CoverageIndex {
            version: INDEX_VERSION,
            files: self
                .candidates
                .iter()
                .filter_map(|c| Some(IndexedFile { path: c.path.clone(), size: c.size, mtime: c.mtime, ranges: c.ranges.clone()? }))
                .collect(),
            // A character no file has was looked for in every file, so
            // every file is in `files`.
            missing: self.decided.iter().filter(|(_, found)| found.is_none()).map(|(&(c, emoji), _)| (c as u32, emoji)).collect(),
        };
        let Ok(json) = serde_json::to_vec(&index) else { return };
        let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&tmp, json))
            .and_then(|()| std::fs::rename(&tmp, path));
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            tracing::warn!("Font fallback: failed to write the coverage index: {e}");
        }
    }

    fn find(&mut self, c: char, emoji: bool) -> Option<PathBuf> {
        if let Some(found) = self.decided.get(&(c, emoji)) {
            return found.map(|i| self.candidates[i].path.clone());
        }
        // Emoji fonts first for emoji, last for anything else.
        let order: Vec<usize> = {
            let (first, rest): (Vec<usize>, Vec<usize>) = (0..self.candidates.len()).partition(|&i| self.candidates[i].emoji == emoji);
            first.into_iter().chain(rest).collect()
        };
        let mut read = 0;
        let found = order.into_iter().find(|&i| {
            let candidate = &mut self.candidates[i];
            let ranges = candidate.ranges.get_or_insert_with(|| {
                read += 1;
                codepoints(&candidate.path)
            });
            covers(ranges, c as u32)
        });
        self.files_read += read;
        self.decided.insert((c, emoji), found);
        // Something was learnt: a file's ranges, or a character no
        // file has.
        if read > 0 || found.is_none() {
            self.write_index();
        }
        found.map(|i| self.candidates[i].path.clone())
    }
}

/// The font files under `dir`, at any depth.
fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    collect_in(dir, out, &mut Vec::new());
}

/// [`collect`], through the folders `seen` holds (the ones it is inside),
/// as deep as the font scan goes and never into one twice.
fn collect_in(dir: &Path, out: &mut Vec<PathBuf>, seen: &mut Vec<PathBuf>) {
    if !crate::font_discovery::enter_font_dir(dir, seen) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        seen.pop();
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_in(&path, out, seen);
        } else if let Some(ext) = path.extension() {
            let ext = ext.to_string_lossy().to_lowercase();
            if ext == "ttf" || ext == "otf" || ext == "ttc" {
                out.push(path);
            }
        }
    }
    seen.pop();
}

/// The codepoints the first face in a font file maps to a glyph, as
/// sorted ranges; none for a file that does not read, or whose map is
/// a symbol font's (its codepoints stand for other characters).
fn codepoints(path: &Path) -> Vec<(u32, u32)> {
    let Ok(data) = std::fs::read(path) else { return Vec::new() };
    let Ok(font) = FontRef::from_index(&data, 0) else { return Vec::new() };
    let Ok(cmap) = font.cmap() else { return Vec::new() };
    let Some((_, record, subtable)) = cmap.best_subtable() else { return Vec::new() };
    if record.platform_id() == PlatformId::Windows && record.encoding_id() == 0 {
        return Vec::new();
    }
    let mut mapped: Vec<u32> = subtable.iter().filter(|(_, gid)| gid.to_u32() != 0).map(|(cp, _)| cp).collect();
    mapped.sort_unstable();
    mapped.dedup();
    ranges_of(&mapped)
}

/// Sorted, distinct codepoints as inclusive ranges.
fn ranges_of(sorted: &[u32]) -> Vec<(u32, u32)> {
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    for &cp in sorted {
        match ranges.last_mut() {
            Some(last) if last.1 + 1 == cp => last.1 = cp,
            _ => ranges.push((cp, cp)),
        }
    }
    ranges
}

fn covers(ranges: &[(u32, u32)], cp: u32) -> bool {
    let i = ranges.partition_point(|r| r.1 < cp);
    ranges.get(i).is_some_and(|r| r.0 <= cp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_cover_their_codepoints() {
        let ranges = ranges_of(&[32, 33, 34, 65, 0x4E00, 0x4E01]);
        assert_eq!(ranges, [(32, 34), (65, 65), (0x4E00, 0x4E01)]);
        for cp in [32, 34, 65, 0x4E01] {
            assert!(covers(&ranges, cp));
        }
        for cp in [0, 35, 64, 66, 0x4DFF, 0x4E02] {
            assert!(!covers(&ranges, cp));
        }
    }

    #[test]
    fn emoji_and_their_joiners() {
        assert!(wants_emoji('😀', None));
        assert!(wants_emoji('❤', Some('\u{FE0F}')));
        assert!(!wants_emoji('❤', None));
        assert!(!wants_emoji('A', None));
        assert!(joins_previous('\u{FE0F}') && joins_previous('\u{200D}') && joins_previous('\u{1F3FD}'));
        assert!(!joins_previous('a') && !joins_previous('😀'));
    }

    /// #1673: a second run over the same fonts and index reads no font
    /// file, for a character a font has or one none has; a file that
    /// changed is read again, the others not; and a character no font
    /// has is looked for once.
    #[test]
    fn the_coverage_index_spares_reading_fonts_again() {
        let root = std::env::temp_dir().join(format!("klipp-1673-coverage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let fonts = root.join("fonts");
        std::fs::create_dir_all(fonts.join("sub")).unwrap();
        let file = |family: &str| crate::bundled::lookup(family, "Regular").unwrap();
        std::fs::write(fonts.join("Inter-Regular.ttf"), file("Inter")).unwrap();
        std::fs::write(fonts.join("BebasNeue-Regular.ttf"), file("Bebas Neue")).unwrap();
        std::fs::write(fonts.join("sub").join("JetBrainsMono-Regular.ttf"), file("JetBrains Mono")).unwrap();
        let dirs = [fonts.clone()];
        let index = Some(root.join("cache").join("font_coverage.json"));

        // First run: the cross is only JetBrains Mono's; "漢" is no one's.
        let mut first = SystemFonts::list(&dirs, index.clone());
        let cross = first.find('┼', false);
        assert!(cross.as_ref().is_some_and(|p| p.ends_with("JetBrainsMono-Regular.ttf")), "{cross:?}");
        assert_eq!(first.find('漢', false), None);
        assert_eq!(first.files_read, 3, "every file is read once");
        assert_eq!(first.find('漢', false), None);
        assert_eq!(first.files_read, 3, "a character no font has is looked for once");

        // Second run: nothing to read.
        let mut second = SystemFonts::list(&dirs, index.clone());
        assert_eq!(second.find('┼', false), cross);
        assert_eq!(second.find('漢', false), None);
        assert!(second.find('A', false).is_some());
        assert_eq!(second.files_read, 0, "a font file was read again");

        // A changed file is read again, and only it; "漢" is looked
        // for again, as the changed file might have it.
        std::fs::write(fonts.join("BebasNeue-Regular.ttf"), file("Libre Baskerville")).unwrap();
        let mut third = SystemFonts::list(&dirs, index.clone());
        assert_eq!(third.find('漢', false), None);
        assert_eq!(third.files_read, 1, "only the changed file is read");
        assert_eq!(third.find('┼', false), cross);
        assert_eq!(third.files_read, 1);

        let mut fourth = SystemFonts::list(&dirs, index.clone());
        assert_eq!(fourth.find('漢', false), None);
        assert_eq!(fourth.files_read, 0);
        // The index was renamed into place: no file of its own is left.
        let left: Vec<_> = std::fs::read_dir(root.join("cache")).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left, [std::ffi::OsString::from("font_coverage.json")]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
