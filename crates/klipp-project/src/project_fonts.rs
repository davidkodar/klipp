//! The fonts a collected project carries (#3009): the files of each family
//! its titles name that is not bundled with Klipp, with the licence beside
//! them, copied into the project's `fonts` folder and listed in
//! `fonts.json` beside the project file by paths relative to its folder.
//! Opened elsewhere, a family the machine lacks is drawn from them.
//!
//! The list is read as untrusted: an unknown format is refused, the file
//! and each font are size-capped, and every path must be a plain relative
//! one that stays inside the project's folder, its last part not a link.

use std::path::{Component, Path, PathBuf};

use klipp_timeline::{ClipKind, Sequence, TitleObjectContent};
use serde::{Deserialize, Serialize};

/// The list's name, beside the project file.
pub const FONTS_FILE: &str = "fonts.json";
/// The folder the files are copied into.
pub const FONTS_DIR: &str = "fonts";
/// The list's format; a later one is refused.
pub const FONTS_FORMAT: u32 = 1;
/// The most a list may weigh.
const MAX_LIST_BYTES: u64 = 1024 * 1024;
/// The most one font file may weigh: far past any real face.
pub const MAX_FONT_BYTES: u64 = 64 * 1024 * 1024;
/// The most files a list may name.
const MAX_FILES: usize = 512;

/// A family as the project folder holds it: its files and licence, by
/// paths relative to the folder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectFont {
    pub family: String,
    pub files: Vec<String>,
    pub licence: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FontList {
    format: u32,
    fonts: Vec<ProjectFont>,
}

/// A family to collect, as whoever has the font list found it: its files
/// and licence, or why it can't go (not found, no licence, a licence
/// that does not allow embedding).
#[derive(Debug, Clone, PartialEq)]
pub struct CollectFont {
    pub family: String,
    pub source: Result<(Vec<PathBuf>, PathBuf), String>,
}

/// The font families `sequences` name: each title's text and its runs,
/// an audition's other takes' too, and each subtitle track's caption
/// look, once each, in order met.
pub fn fonts_used(sequences: &[Sequence]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |name: &str| {
        let name = name.trim();
        if !name.is_empty() && !out.iter().any(|f| f.eq_ignore_ascii_case(name)) {
            out.push(name.to_string());
        }
    };
    for seq in sequences {
        for track in &seq.tracks {
            if let Some(style) = &track.caption_style {
                add(&style.font);
            }
            // An audition's other takes' titles too (#3209).
            for clip in track.clips.iter().flat_map(klipp_timeline::Clip::with_takes) {
                let ClipKind::Title(td) = &clip.kind else { continue };
                for o in &td.objects {
                    if let TitleObjectContent::TextBox(tb) = &o.content {
                        add(&tb.font_name);
                        for run in &tb.style_runs {
                            if let Some(f) = &run.font_name {
                                add(f);
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

/// Copy each family of `fonts` that can go into `dest/fonts/<family>/`,
/// its licence beside its files: what was copied, as the list names it,
/// and a line for each left out, saying why.
pub fn copy_fonts(fonts: &[CollectFont], dest: &Path) -> (Vec<ProjectFont>, Vec<String>) {
    let (mut copied, mut left) = (Vec::new(), Vec::new());
    for font in fonts {
        let (files, licence) = match &font.source {
            Ok(found) => found,
            Err(why) => {
                left.push(format!("font {} not copied: {why}", font.family));
                continue;
            }
        };
        match copy_family(&font.family, files, licence, dest) {
            Ok(f) => copied.push(f),
            Err(why) => left.push(format!("font {} not copied: {why}", font.family)),
        }
    }
    (copied, left)
}

fn copy_family(family: &str, files: &[PathBuf], licence: &Path, dest: &Path) -> Result<ProjectFont, String> {
    if files.is_empty() {
        return Err("no font files found".into());
    }
    let folder = format!("{FONTS_DIR}/{}", klipp_core::safe_name(family));
    std::fs::create_dir_all(dest.join(&folder)).map_err(|e| format!("cannot make {folder}: {e}"))?;
    let copy = |src: &Path| -> Result<String, String> {
        // The file itself, never a link's target, and of a font's size.
        let meta = std::fs::symlink_metadata(src).map_err(|e| format!("{}: {e}", src.display()))?;
        if !meta.is_file() || meta.len() > MAX_FONT_BYTES {
            return Err(format!("{} is not a font file this can carry", src.display()));
        }
        let name = src.file_name().and_then(|n| n.to_str()).filter(|n| safe_component(n)).ok_or_else(|| format!("{} has no usable name", src.display()))?;
        let at = format!("{folder}/{name}");
        std::fs::copy(src, dest.join(&at)).map_err(|e| format!("{}: {e}", src.display()))?;
        Ok(at)
    };
    let files = files.iter().map(|f| copy(f)).collect::<Result<Vec<_>, _>>()?;
    let licence = copy(licence)?;
    Ok(ProjectFont { family: family.to_string(), files, licence })
}

/// Write the list beside the project file in `dir`; none written for no
/// fonts.
pub fn save_fonts(dir: &Path, fonts: &[ProjectFont]) -> Result<(), String> {
    if fonts.is_empty() {
        return Ok(());
    }
    let json = serde_json::to_string_pretty(&FontList { format: FONTS_FORMAT, fonts: fonts.to_vec() }).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(FONTS_FILE), json).map_err(|e| format!("cannot write {FONTS_FILE}: {e}"))
}

/// The families the project in `dir` carries, each with its font files'
/// paths, from its list: none when it has no list. Refused whole when the
/// list is too large, of a later format or unreadable, or names a path
/// that is absolute, climbs out (`..`), is a link, or is not a file
/// inside the folder.
pub fn load_fonts(dir: &Path) -> Result<Vec<(String, Vec<PathBuf>)>, String> {
    let list = dir.join(FONTS_FILE);
    let Ok(meta) = std::fs::symlink_metadata(&list) else { return Ok(Vec::new()) };
    if !meta.is_file() || meta.len() > MAX_LIST_BYTES {
        return Err(format!("{FONTS_FILE} is not a font list this can read"));
    }
    let text = std::fs::read_to_string(&list).map_err(|e| format!("{FONTS_FILE}: {e}"))?;
    let peek: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("{FONTS_FILE}: {e}"))?;
    let format = peek.get("format").and_then(serde_json::Value::as_u64).unwrap_or(0);
    if format > u64::from(FONTS_FORMAT) {
        return Err(format!("{FONTS_FILE} is of a later Klipp (format {format}); open the project there"));
    }
    let parsed: FontList = serde_json::from_value(peek).map_err(|e| format!("{FONTS_FILE}: {e}"))?;
    if parsed.fonts.iter().map(|f| f.files.len()).sum::<usize>() > MAX_FILES {
        return Err(format!("{FONTS_FILE} names more font files than a project carries"));
    }
    let root = dir.canonicalize().map_err(|e| format!("{}: {e}", dir.display()))?;
    parsed
        .fonts
        .into_iter()
        .map(|f| {
            let files = f.files.iter().map(|rel| inside(&root, rel)).collect::<Result<Vec<_>, _>>()?;
            Ok((f.family, files))
        })
        .collect()
}

/// `rel` under `root`, refused unless it is a plain relative path to a
/// file inside it, not a link, of a font's size.
fn inside(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let path = Path::new(rel);
    if path.as_os_str().is_empty() || !path.components().all(|c| matches!(c, Component::Normal(n) if n.to_str().is_some_and(safe_component))) {
        return Err(format!("{FONTS_FILE} names \"{rel}\", which is not a path inside the project's folder"));
    }
    let full = root.join(path);
    let meta = std::fs::symlink_metadata(&full).map_err(|_| format!("{FONTS_FILE} names \"{rel}\", which is not there"))?;
    if !meta.is_file() || meta.len() > MAX_FONT_BYTES {
        return Err(format!("{FONTS_FILE} names \"{rel}\", which is not a font file"));
    }
    let real = full.canonicalize().map_err(|e| format!("{rel}: {e}"))?;
    if !real.starts_with(root) {
        return Err(format!("{FONTS_FILE} names \"{rel}\", which leads out of the project's folder"));
    }
    Ok(real)
}

/// A file or folder name with nothing a path can be steered by.
fn safe_component(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '\0'])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("klipp-project-fonts-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Copied with its licence, listed by relative paths, read back.
    #[test]
    fn a_family_is_copied_listed_and_read_back() {
        let (src, dest) = (tmp("src"), tmp("dest"));
        std::fs::write(src.join("Face-Regular.ttf"), b"font bytes").unwrap();
        std::fs::write(src.join("OFL.txt"), b"licence").unwrap();
        let fonts = vec![
            CollectFont { family: "Some Face".into(), source: Ok((vec![src.join("Face-Regular.ttf")], src.join("OFL.txt"))) },
            CollectFont { family: "Locked".into(), source: Err("its licence does not allow embedding".into()) },
        ];
        let (copied, left) = copy_fonts(&fonts, &dest);
        assert_eq!(copied, [ProjectFont { family: "Some Face".into(), files: vec!["fonts/some-face/Face-Regular.ttf".into()], licence: "fonts/some-face/OFL.txt".into() }]);
        assert_eq!(left, ["font Locked not copied: its licence does not allow embedding"]);
        assert!(dest.join("fonts/some-face/OFL.txt").is_file());
        save_fonts(&dest, &copied).unwrap();
        let read = load_fonts(&dest).unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].0, "Some Face");
        assert!(read[0].1[0].ends_with("fonts/some-face/Face-Regular.ttf"));
        assert_eq!(load_fonts(&src).unwrap(), Vec::new(), "no list, no fonts");
    }

    /// A hostile list: a path out of the folder, an absolute one, a link
    /// out, a later format and an unknown field are each refused whole.
    #[test]
    fn a_hostile_font_list_is_refused() {
        let dir = tmp("hostile");
        let outside = tmp("outside");
        std::fs::write(outside.join("secret.ttf"), b"x").unwrap();
        std::fs::create_dir_all(dir.join("fonts")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.join("secret.ttf"), dir.join("fonts/link.ttf")).unwrap();
        let list = |files: &str, format: u32, extra: &str| std::fs::write(dir.join(FONTS_FILE), format!(r#"{{"format": {format}, "fonts": [{{"family": "F", "files": [{files}], "licence": "fonts/OFL.txt"{extra}}}]}}"#)).unwrap();
        let outside_rel = format!("\"../{}/secret.ttf\"", outside.file_name().unwrap().to_str().unwrap());
        for (files, format, extra, why) in [
            (outside_rel.as_str(), 1, "", "out of the folder"),
            ("\"/etc/passwd\"", 1, "", "absolute"),
            ("\"fonts/link.ttf\"", 1, "", "a link out"),
            ("\"fonts/none.ttf\"", 1, "", "not there"),
            ("\"fonts/link.ttf\"", 2, "", "a later format"),
            ("\"fonts/link.ttf\"", 1, ", \"run\": \"x\"", "an unknown field"),
        ] {
            list(files, format, extra);
            assert!(load_fonts(&dir).is_err(), "{why} was read");
        }
    }

    #[test]
    fn the_fonts_used_are_each_named_once() {
        let mut td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "A".into(), font_name: "Oswald".into(), ..Default::default() });
        if let TitleObjectContent::TextBox(tb) = &mut td.objects[0].content {
            tb.style_runs.push(klipp_timeline::StyleRun { font_name: Some("Great Vibes".into()), ..klipp_timeline::StyleRun::default_for_range(0, 1) });
        }
        let mut seq = Sequence::new_default();
        let mut track = klipp_timeline::Track::new("T", klipp_timeline::TrackKind::Text);
        let title = |td: klipp_timeline::TitleData| klipp_timeline::Clip { kind: ClipKind::Title(td), ..klipp_timeline::Clip::gap(klipp_core::Timecode::from_frames(0, 24), klipp_core::Timecode::from_frames(24, 24)) };
        track.clips = vec![title(td.clone()), title(td)];
        seq.tracks = vec![track];
        assert_eq!(fonts_used(&[seq]), ["Oswald", "Great Vibes"]);
    }
}
