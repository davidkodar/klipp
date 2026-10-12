//! Which system font draws a character a title's face lacks (#1503).
//!
//! A title's text is set in the face its box or style run names; a
//! character that face has no glyph for is drawn from the first face in
//! a chain that has one: the system's fonts that cover it, then the
//! faces that ship with the app. This module is the system part of the
//! chain, without fontconfig: the font files under the font directories
//! are listed once, and each file's character map is read the first
//! time a character is looked for past it, then kept as ranges of
//! codepoints, so a later character is looked up in memory. A file is
//! read once a process; most titles never read one, as their faces
//! cover their text.
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

use read_fonts::tables::cmap::PlatformId;
use read_fonts::{FontRef, TableProvider};

/// Bumped whenever the chain changes which face draws a character, so
/// a glyph cache laid out before does not match (#1503).
pub const FALLBACK_VERSION: u32 = 1;

/// Words in a font file's name that mark a face other than the Regular.
const STYLE_WORDS: &[&str] = &[
    "bold", "black", "heavy", "light", "thin", "medium", "semi", "demi", "extra", "ultra", "italic", "oblique", "condensed", "narrow",
];

/// A system font file and, once read, the codepoints it maps.
struct Candidate {
    path: PathBuf,
    emoji: bool,
    ranges: Option<Vec<(u32, u32)>>,
}

/// The system's font files in the order they are tried, and every
/// decision made: the file that draws a character (as emoji or not).
struct SystemFonts {
    candidates: Vec<Candidate>,
    decided: HashMap<(char, bool), Option<usize>>,
}

static SYSTEM: OnceLock<Mutex<SystemFonts>> = OnceLock::new();

/// The system font file that draws `c`, an emoji font first when
/// `emoji`; `None` when none has it.
pub(crate) fn system_font_for(c: char, emoji: bool) -> Option<PathBuf> {
    let fonts = SYSTEM.get_or_init(|| Mutex::new(SystemFonts::list()));
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
    /// Every TrueType, OpenType and collection file under the font
    /// directories, in the order they are tried; none is read yet.
    fn list() -> Self {
        let mut paths = Vec::new();
        for dir in crate::font_discovery::font_directories() {
            collect(Path::new(&dir), &mut paths);
        }
        let mut candidates: Vec<Candidate> = paths
            .into_iter()
            .map(|path| {
                let name = path.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
                Candidate { emoji: name.contains("emoji"), path, ranges: None }
            })
            .collect();
        candidates.sort_by_cached_key(|c| {
            let name = c.path.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
            let size = std::fs::metadata(&c.path).map_or(0, |m| m.len());
            (STYLE_WORDS.iter().any(|w| name.contains(w)), std::cmp::Reverse(size), c.path.clone())
        });
        Self { candidates, decided: HashMap::new() }
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
        let found = order.into_iter().find(|&i| {
            let candidate = &mut self.candidates[i];
            let ranges = candidate.ranges.get_or_insert_with(|| codepoints(&candidate.path));
            covers(ranges, c as u32)
        });
        self.decided.insert((c, emoji), found);
        found.map(|i| self.candidates[i].path.clone())
    }
}

/// The font files under `dir`, at any depth.
fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if let Some(ext) = path.extension() {
            let ext = ext.to_string_lossy().to_lowercase();
            if ext == "ttf" || ext == "otf" || ext == "ttc" {
                out.push(path);
            }
        }
    }
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
}
