//! Brand kits (#2722): a creator's colours, fonts and type styles, logo
//! and its watermark place, intro and outro, lower third and end screen,
//! and caption look, kept once for the user and taken by any project.
//!
//! A kit is a folder under the user's Klipp data folder,
//! `brand_kits/<safe name>/`, holding `kit.toml` and the files it names
//! (its logo, an intro or outro clip). Three starter kits ship with Klipp,
//! read-only. A project that takes a kit keeps its own copy in its Brief,
//! so it stays whole without the kit.
//!
//! A kit is read as untrusted, whoever wrote it: the file is size-capped,
//! a later format is refused, an unknown field is refused, every number is
//! held to its range, every colour is `#rrggbb`, and every file it names
//! is a plain name inside its folder, not a link, of an allowed kind and
//! size.

use std::path::{Component, Path, PathBuf};

use klipp_timeline::{CaptionStyle, TypeStyle};
use serde::{Deserialize, Serialize};

/// The kit file's format; a later one is refused.
pub const FORMAT: u32 = 1;
/// The kit file in its folder.
pub const KIT_FILE: &str = "kit.toml";
/// The most a kit file may weigh.
const MAX_KIT_BYTES: u64 = 256 * 1024;
/// The most a logo may weigh.
pub const MAX_LOGO_BYTES: u64 = 16 * 1024 * 1024;
/// The most an intro or outro clip may weigh.
pub const MAX_CLIP_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// The picture kinds a logo may be.
pub const LOGO_KINDS: &[&str] = &["png", "jpg", "jpeg", "webp"];
/// The video kinds an intro or outro clip may be.
pub const CLIP_KINDS: &[&str] = &["mp4", "mov", "mkv", "webm"];
/// The most type styles a kit may hold, and the longest name or text.
const MAX_STYLES: usize = 32;
const MAX_TEXT: usize = 128;

/// The starter kits, shipped read-only.
const STARTERS: &[&str] = &[
    include_str!("../../../assets/brand_kits/clean.toml"),
    include_str!("../../../assets/brand_kits/bold.toml"),
    include_str!("../../../assets/brand_kits/editorial.toml"),
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrandKit {
    pub format: u32,
    pub name: String,
    #[serde(default)]
    pub colours: Colours,
    #[serde(default)]
    pub fonts: Fonts,
    /// Named looks for titles, as a Brief holds them; left out, the
    /// heading, callout and card looks are made from the fonts and
    /// colours when the kit is taken.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub type_styles: Vec<TypeStyle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo: Option<Logo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intro: Option<Bookend>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outro: Option<Bookend>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lower_third: Option<TemplateRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_screen: Option<TemplateRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captions: Option<CaptionStyle>,
}

/// The kit's colours, each `#rrggbb`: the primary is what titles take.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Colours {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secondary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent: Option<String>,
}

/// The kit's font pairing: a family for headings, one for body text.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fonts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heading: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

/// The logo and where it sits as a watermark: a corner, a margin and a
/// width as fractions of the frame's width, and its opacity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Logo {
    /// The picture, by its name in the kit's folder.
    pub file: String,
    #[serde(default)]
    pub corner: Corner,
    #[serde(default = "default_margin")]
    pub margin: f32,
    #[serde(default = "default_scale")]
    pub scale: f32,
    #[serde(default = "default_opacity")]
    pub opacity: f32,
}

fn default_margin() -> f32 {
    0.03
}
fn default_scale() -> f32 {
    0.12
}
fn default_opacity() -> f32 {
    0.8
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    #[default]
    BottomRight,
}

/// An intro or outro: a composition template by name, or a clip in the
/// kit's folder; one of them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bookend {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
}

/// A template by name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateRef {
    pub template: String,
}

/// Where a kit comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Shipped with Klipp, read-only.
    Builtin,
    /// The user's own, in the data folder.
    Mine,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::Builtin => "builtin",
            Source::Mine => "mine",
        }
    }
}

/// A kit as found: what it is, where from, and its folder (the user's).
#[derive(Debug, Clone)]
pub struct Found {
    pub kit: BrandKit,
    pub source: Source,
    pub dir: Option<PathBuf>,
}

impl BrandKit {
    /// An empty kit named `name`.
    pub fn new(name: &str) -> Self {
        Self { format: FORMAT, name: name.trim().to_string(), colours: Colours::default(), fonts: Fonts::default(), type_styles: Vec::new(), logo: None, intro: None, outro: None, lower_third: None, end_screen: None, captions: None }
    }

    /// Read from a kit file's text, checked (see the module).
    pub fn read(text: &str) -> Result<Self, String> {
        let peek: toml::Table = toml::from_str(text).map_err(|e| format!("not a brand kit: {e}"))?;
        let format = peek.get("format").and_then(toml::Value::as_integer).unwrap_or(0);
        if format > i64::from(FORMAT) {
            return Err(format!("a brand kit of a later Klipp (format {format}): update Klipp to open it"));
        }
        let kit: BrandKit = toml::from_str(text).map_err(|e| format!("not a brand kit: {e}"))?;
        kit.check()?;
        Ok(kit)
    }

    /// The kit as its file holds it.
    pub fn write(&self) -> Result<String, String> {
        toml::to_string_pretty(self).map_err(|e| e.to_string())
    }

    /// Whether every value is one a kit may hold: a name, colours,
    /// numbers in range, plain file names of the right kind, one of a
    /// template or a file for an intro or outro.
    pub fn check(&self) -> Result<(), String> {
        let text = |what: &str, s: &str| -> Result<(), String> {
            if s.trim().is_empty() || s.chars().count() > MAX_TEXT || s.chars().any(char::is_control) {
                return Err(format!("{what} must be 1 to {MAX_TEXT} plain characters"));
            }
            Ok(())
        };
        text("the kit's name", &self.name)?;
        if !self.name.chars().any(char::is_alphanumeric) {
            return Err("the kit's name needs a letter or a digit".into());
        }
        for (which, c) in [("primary", &self.colours.primary), ("secondary", &self.colours.secondary), ("accent", &self.colours.accent)] {
            if let Some(c) = c {
                if !is_hex_colour(c) {
                    return Err(format!("the {which} colour \"{c}\" is not #rrggbb"));
                }
            }
        }
        for (which, f) in [("heading", &self.fonts.heading), ("body", &self.fonts.body)] {
            if let Some(f) = f {
                text(&format!("the {which} font"), f)?;
            }
        }
        if self.type_styles.len() > MAX_STYLES {
            return Err(format!("{} type styles: a kit holds {MAX_STYLES} at most", self.type_styles.len()));
        }
        for s in &self.type_styles {
            text("a type style's name", &s.name)?;
        }
        if let Some(logo) = &self.logo {
            file_name("the logo", &logo.file, LOGO_KINDS)?;
            if !(0.0..=0.25).contains(&logo.margin) || !(0.02..=0.5).contains(&logo.scale) || !(0.0..=1.0).contains(&logo.opacity) {
                return Err("the logo's margin is 0 to 0.25, its scale 0.02 to 0.5 and its opacity 0 to 1".into());
            }
        }
        for (which, b) in [("intro", &self.intro), ("outro", &self.outro)] {
            if let Some(b) = b {
                match (&b.template, &b.file) {
                    (Some(t), None) => text(&format!("the {which}'s template"), t)?,
                    (None, Some(f)) => file_name(&format!("the {which}"), f, CLIP_KINDS)?,
                    _ => return Err(format!("the {which} is a template or a file, one of them")),
                }
            }
        }
        for (which, t) in [("lower third", &self.lower_third), ("end screen", &self.end_screen)] {
            if let Some(t) = t {
                text(&format!("the {which}'s template"), &t.template)?;
            }
        }
        Ok(())
    }

    /// The title looks the kit gives (#2722): its own type styles, or,
    /// with none, the built-in heading, callout and card in its fonts and
    /// colours (the heading and card in the heading font, the callout in
    /// the body font, all in the primary colour, the card on the
    /// secondary), each built-in look's size, place and motion kept.
    pub fn title_styles(&self) -> Vec<TypeStyle> {
        if !self.type_styles.is_empty() {
            return self.type_styles.clone();
        }
        let (heading, body) = (&self.fonts.heading, &self.fonts.body);
        let (primary, secondary) = (&self.colours.primary, &self.colours.secondary);
        [("heading", heading), ("callout", body), ("card", heading)]
            .into_iter()
            .filter_map(|(name, font)| {
                let mut s = TypeStyle::builtin(name)?;
                if font.is_none() && primary.is_none() && !(name == "card" && secondary.is_some()) {
                    return None;
                }
                if font.is_some() {
                    s.font = font.clone();
                }
                if primary.is_some() {
                    s.color = primary.clone();
                }
                if name == "card" && secondary.is_some() {
                    s.fill = secondary.clone();
                }
                Some(s)
            })
            .collect()
    }

    /// `current` (a project's Brief) with the kit taken in: its colours as
    /// the brand colours, primary first, and its title looks, each
    /// replacing the brief's of the same name; everything else kept.
    pub fn into_brief(&self, current: &klipp_timeline::Brief) -> klipp_timeline::Brief {
        let mut b = current.clone();
        let colours: Vec<String> = [&self.colours.primary, &self.colours.secondary, &self.colours.accent].into_iter().flatten().cloned().collect();
        if !colours.is_empty() {
            b.brand_colors = colours;
        }
        for style in self.title_styles() {
            match b.type_styles.iter_mut().find(|s| s.name.trim().eq_ignore_ascii_case(style.name.trim())) {
                Some(s) => *s = style,
                None => b.type_styles.push(style),
            }
        }
        b
    }

    /// The caption look the kit gives a subtitle track: its own, or the
    /// default look in its body font; none when it names neither.
    pub fn caption_look(&self) -> Option<CaptionStyle> {
        if let Some(c) = &self.captions {
            return Some(c.clone());
        }
        let font = self.fonts.body.clone()?;
        Some(CaptionStyle { font, ..CaptionStyle::default() })
    }

    /// The files the kit names in its folder.
    pub fn files(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        if let Some(l) = &self.logo {
            out.push(&l.file);
        }
        for b in [&self.intro, &self.outro].into_iter().flatten() {
            if let Some(f) = &b.file {
                out.push(f);
            }
        }
        out
    }
}

fn is_hex_colour(c: &str) -> bool {
    c.len() == 7 && c.starts_with('#') && c[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

/// A plain file name of one of `kinds`: one part, no separator, not `..`.
fn file_name(what: &str, name: &str, kinds: &[&str]) -> Result<(), String> {
    let p = Path::new(name);
    let plain = p.components().count() == 1 && matches!(p.components().next(), Some(Component::Normal(_))) && !name.contains(['/', '\\', '\0']);
    let kind = p.extension().and_then(|e| e.to_str()).map(str::to_lowercase);
    if !plain || name.chars().count() > MAX_TEXT {
        return Err(format!("{what} \"{name}\" is not a plain file name in the kit's folder"));
    }
    if !kind.as_deref().is_some_and(|k| kinds.contains(&k)) {
        return Err(format!("{what} \"{name}\" is not one of {}", kinds.join(", ")));
    }
    Ok(())
}

/// Where the user's kits are kept under the data `root`.
pub fn kits_dir(root: &Path) -> PathBuf {
    root.join("brand_kits")
}

/// The starter kits Klipp ships.
pub fn starters() -> Vec<BrandKit> {
    STARTERS.iter().filter_map(|t| BrandKit::read(t).map_err(|e| tracing::error!("a starter brand kit does not read: {e}")).ok()).collect()
}

/// The kit in `dir`, read as untrusted, its files checked: there, inside
/// the folder, not links, of their kind's size.
pub fn read_dir(dir: &Path) -> Result<BrandKit, String> {
    let file = dir.join(KIT_FILE);
    let meta = std::fs::symlink_metadata(&file).map_err(|_| format!("no {KIT_FILE} in {}", dir.display()))?;
    if !meta.is_file() || meta.len() > MAX_KIT_BYTES {
        return Err(format!("{KIT_FILE} is not a brand kit this reads"));
    }
    let kit = BrandKit::read(&std::fs::read_to_string(&file).map_err(|e| e.to_string())?)?;
    for name in kit.files() {
        let cap = if LOGO_KINDS.iter().any(|k| name.to_lowercase().ends_with(k)) { MAX_LOGO_BYTES } else { MAX_CLIP_BYTES };
        let meta = std::fs::symlink_metadata(dir.join(name)).map_err(|_| format!("the kit names \"{name}\", which is not in its folder"))?;
        if !meta.is_file() || meta.len() > cap {
            return Err(format!("the kit's \"{name}\" is not a file it can hold"));
        }
    }
    Ok(kit)
}

/// Every kit: the user's, then the starters not replaced by one of the
/// user's of the same name. A user's kit that fails its checks is left
/// out with a warning.
pub fn list(root: Option<&Path>) -> Vec<Found> {
    let mut out: Vec<Found> = Vec::new();
    if let Some(root) = root {
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(kits_dir(root)).into_iter().flatten().flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.path()).collect();
        dirs.sort();
        for dir in dirs {
            match read_dir(&dir) {
                Ok(kit) => out.push(Found { kit, source: Source::Mine, dir: Some(dir) }),
                Err(why) => tracing::warn!("brand kit {}: {why}; left out", dir.display()),
            }
        }
    }
    for kit in starters() {
        if !out.iter().any(|f| f.kit.name.eq_ignore_ascii_case(&kit.name)) {
            out.push(Found { kit, source: Source::Builtin, dir: None });
        }
    }
    out
}

/// The kit named `name` (any case): the user's first, then a starter.
pub fn get(root: Option<&Path>, name: &str) -> Result<Found, String> {
    let all = list(root);
    let names: Vec<String> = all.iter().map(|f| f.kit.name.clone()).collect();
    all.into_iter().find(|f| f.kit.name.eq_ignore_ascii_case(name.trim())).ok_or_else(|| format!("no brand kit \"{}\"; there are {}", name.trim(), names.join(", ")))
}

/// Keep `kit` as the user's, in its folder under `root`, with `files`
/// (each a path on this machine) copied in under their own names; a kit
/// of the same name is replaced. Its folder, or why not.
pub fn save(root: &Path, kit: &BrandKit, files: &[PathBuf]) -> Result<PathBuf, String> {
    kit.check()?;
    let dir = kits_dir(root).join(klipp_core::safe_name(&kit.name));
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot make the kit's folder: {e}"))?;
    for src in files {
        let name = src.file_name().and_then(|n| n.to_str()).ok_or_else(|| format!("{} has no usable name", src.display()))?;
        if !kit.files().contains(&name) {
            return Err(format!("{name} is not a file the kit names"));
        }
        let meta = std::fs::symlink_metadata(src).map_err(|e| format!("{}: {e}", src.display()))?;
        let cap = if LOGO_KINDS.iter().any(|k| name.to_lowercase().ends_with(k)) { MAX_LOGO_BYTES } else { MAX_CLIP_BYTES };
        if !meta.is_file() || meta.len() > cap {
            return Err(format!("{} is not a file a kit can hold", src.display()));
        }
        std::fs::copy(src, dir.join(name)).map_err(|e| format!("{}: {e}", src.display()))?;
    }
    for name in kit.files() {
        if !dir.join(name).is_file() {
            return Err(format!("the kit names \"{name}\", which is not given and not in its folder"));
        }
    }
    let tmp = dir.join(format!("{KIT_FILE}.tmp"));
    std::fs::write(&tmp, kit.write()?).map_err(|e| format!("cannot write the kit: {e}"))?;
    std::fs::rename(&tmp, dir.join(KIT_FILE)).map_err(|e| format!("cannot write the kit: {e}"))?;
    Ok(dir)
}

/// Remove the user's kit `name` and its folder; a starter is refused.
pub fn remove(root: &Path, name: &str) -> Result<(), String> {
    let found = get(Some(root), name)?;
    let Some(dir) = found.dir.filter(|_| found.source == Source::Mine) else {
        return Err(format!("\"{}\" ships with Klipp and is not removed; save a kit of that name to replace it", found.kit.name));
    };
    let meta = std::fs::symlink_metadata(&dir).map_err(|e| e.to_string())?;
    if !meta.is_dir() || dir.parent() != Some(kits_dir(root).as_path()) {
        return Err("the kit's folder is not one this removes".into());
    }
    std::fs::remove_dir_all(&dir).map_err(|e| format!("cannot remove the kit: {e}"))
}

/// A shared brand kit's file extension (#2722): a zip of `kit.toml` and
/// the files it names, each with its licence, read by the template
/// pack's reader within its caps.
pub const BRAND_EXTENSION: &str = "klippbrand";

/// A file's licence, beside it in a kit's folder or a shared kit.
fn licence_name(file: &str) -> String {
    format!("{file}.licence.txt")
}

/// Write the kit `name` (the user's or a starter) to `out`, a
/// `.klippbrand`: its `kit.toml` and each file it names with its licence,
/// given in `licences` by file name or as `<file>.licence.txt` in the
/// kit's folder; a file with neither is refused, named. What was written.
pub fn export_kit(root: Option<&Path>, name: &str, out: &Path, licences: &std::collections::BTreeMap<String, String>) -> Result<String, String> {
    use std::io::Write;
    use zip::write::{SimpleFileOptions, ZipWriter};
    if out.extension().and_then(|e| e.to_str()) != Some(BRAND_EXTENSION) {
        return Err(format!("give a path ending in .{BRAND_EXTENSION}"));
    }
    let found = get(root, name)?;
    let kit = &found.kit;
    let dir = found.dir.clone();
    let licence_of = |file: &str| -> Option<String> {
        licences.get(file).cloned().or_else(|| dir.as_ref().and_then(|d| std::fs::read_to_string(d.join(licence_name(file))).ok())).filter(|t| !t.trim().is_empty())
    };
    let unlicensed: Vec<&str> = kit.files().into_iter().filter(|f| licence_of(f).is_none()).collect();
    if !unlicensed.is_empty() {
        return Err(format!("no licence for {}: give licences {{\"<file>\": \"<licence text>\"}}, or put <file>.licence.txt in the kit's folder", unlicensed.join(", ")));
    }
    let tmp = out.with_extension(format!("{BRAND_EXTENSION}.part"));
    let written = (|| -> Result<(), String> {
        let mut zip = ZipWriter::new(std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?);
        let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        let mut put = |path: &str, bytes: &[u8]| -> Result<(), String> {
            zip.start_file(path, options).map_err(|e| e.to_string())?;
            zip.write_all(bytes).map_err(|e| e.to_string())
        };
        put(KIT_FILE, kit.write()?.as_bytes())?;
        for f in kit.files() {
            let src = dir.as_ref().map(|d| d.join(f)).ok_or_else(|| format!("{f} has no folder to come from"))?;
            put(f, &std::fs::read(&src).map_err(|e| format!("{}: {e}", src.display()))?)?;
            put(&licence_name(f), licence_of(f).unwrap_or_default().as_bytes())?;
        }
        zip.finish().map_err(|e| e.to_string())?;
        Ok(())
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, out).map_err(|e| format!("{}: {e}", out.display()))?;
    Ok(format!("wrote the brand kit \"{}\" with {} file{} to {}", kit.name, kit.files().len(), if kit.files().len() == 1 { "" } else { "s" }, out.display()))
}

/// Install the shared kit at `path` as the user's own, read as untrusted:
/// every entry within the pack's caps, the kit checked as any kit is, only
/// the files it names read, each with its licence, written under plain
/// names into its own folder. A kit of its name is kept unless `replace`.
pub fn import_kit(root: &Path, path: &Path, replace: bool) -> Result<Found, String> {
    use crate::template_pack::{read_entry, MAX_ENTRY_BYTES, MAX_PACK_BYTES};
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("not a brand kit file: {e}"))?;
    let mut left = MAX_PACK_BYTES;
    let text = read_entry(&mut zip, KIT_FILE, MAX_KIT_BYTES, &mut left)?.ok_or(format!("not a brand kit file: it has no {KIT_FILE}"))?;
    let kit = BrandKit::read(&String::from_utf8(text).map_err(|_| format!("{KIT_FILE} is not text"))?)?;
    if !replace && list(Some(root)).iter().any(|f| f.source == Source::Mine && f.kit.name.eq_ignore_ascii_case(&kit.name)) {
        return Err(format!("you have a brand kit \"{}\" already: import with replace to take its place", kit.name));
    }
    // Each file read before anything is written: a missing or oversized
    // one leaves nothing behind.
    let mut files = Vec::new();
    for f in kit.files() {
        let cap = if LOGO_KINDS.iter().any(|k| f.to_lowercase().ends_with(k)) { MAX_LOGO_BYTES } else { MAX_ENTRY_BYTES };
        let bytes = read_entry(&mut zip, f, cap, &mut left)?.ok_or_else(|| format!("the kit names {f}, which the file does not hold"))?;
        let licence = read_entry(&mut zip, &licence_name(f), MAX_KIT_BYTES, &mut left)?.filter(|l| !l.is_empty()).ok_or_else(|| format!("{f} comes with no licence"))?;
        files.push((f.to_string(), bytes, licence));
    }
    let dir = crate::path_util::inside(&kits_dir(root), &[&klipp_core::safe_name(&kit.name)])?;
    let staging = crate::path_util::inside(&kits_dir(root), &[&format!(".{}.importing", klipp_core::safe_name(&kit.name))])?;
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("cannot make the kit's folder: {e}"))?;
    let staged = (|| -> Result<(), String> {
        for (f, bytes, licence) in &files {
            std::fs::write(crate::path_util::inside(&staging, &[f])?, bytes).map_err(|e| e.to_string())?;
            std::fs::write(crate::path_util::inside(&staging, &[&licence_name(f)])?, licence).map_err(|e| e.to_string())?;
        }
        std::fs::write(staging.join(KIT_FILE), kit.write()?).map_err(|e| e.to_string())
    })();
    if let Err(e) = staged {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("cannot replace the kit: {e}"))?;
    }
    std::fs::rename(&staging, &dir).map_err(|e| format!("cannot install the kit: {e}"))?;
    let kit = read_dir(&dir)?;
    Ok(Found { kit, source: Source::Mine, dir: Some(dir) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("klipp-brand-kit-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_starter_kits_read() {
        let names: Vec<String> = starters().into_iter().map(|k| k.name).collect();
        assert_eq!(names, ["Clean", "Bold", "Editorial"]);
        for k in starters() {
            for f in [&k.fonts.heading, &k.fonts.body].into_iter().flatten() {
                assert!(klipp_title::pairings::available(f), "{}: {f} is neither bundled nor in the catalogue", k.name);
            }
        }
    }

    /// A kit saved with its logo is listed as the user's before the
    /// starters, read back the same, replaced by a save of its name, and
    /// removed; a starter is not removed.
    #[test]
    fn a_kit_is_saved_listed_read_and_removed() {
        let (root, src) = (tmp("root"), tmp("src"));
        std::fs::write(src.join("logo.png"), b"\x89PNG not really").unwrap();
        let mut kit = BrandKit::new("Studio North");
        kit.colours.primary = Some("#1d3557".into());
        kit.fonts = Fonts { heading: Some("Playfair Display".into()), body: Some("Work Sans".into()) };
        kit.logo = Some(Logo { file: "logo.png".into(), corner: Corner::TopRight, margin: 0.04, scale: 0.1, opacity: 0.7 });
        kit.intro = Some(Bookend { template: Some("Hook Card".into()), file: None });
        let dir = save(&root, &kit, &[src.join("logo.png")]).unwrap();
        assert!(dir.join("logo.png").is_file() && dir.ends_with("brand_kits/studio-north"));
        let all = list(Some(&root));
        assert_eq!(all[0].kit, kit);
        assert_eq!(all[0].source, Source::Mine);
        assert_eq!(all.len(), 4, "the user's and the three starters");
        assert_eq!(get(Some(&root), "studio north").unwrap().kit, kit, "by name, any case");
        kit.colours.primary = Some("#000000".into());
        save(&root, &kit, &[]).unwrap();
        assert_eq!(get(Some(&root), "Studio North").unwrap().kit.colours.primary.as_deref(), Some("#000000"), "replaced");
        assert!(remove(&root, "Bold").is_err(), "a starter stays");
        remove(&root, "Studio North").unwrap();
        assert!(!dir.exists() && get(Some(&root), "Studio North").is_err());
    }

    /// #2722: a kit taken into a Brief gives its colours, primary first,
    /// and the built-in looks in its fonts and colours (their size and
    /// place kept), replacing only those of the brief's; a caption look in
    /// its body font.
    #[test]
    fn a_kit_is_taken_into_a_brief_and_a_caption_look() {
        let editorial = starters().into_iter().find(|k| k.name == "Editorial").unwrap();
        let mut current = klipp_timeline::Brief { title: "Harbour".into(), ..Default::default() };
        current.type_styles.push(TypeStyle { name: "kicker".into(), size: Some(30.0), ..Default::default() });
        current.type_styles.push(TypeStyle { name: "Heading".into(), font: Some("Comic".into()), ..Default::default() });
        let b = editorial.into_brief(&current);
        assert_eq!(b.title, "Harbour", "the rest of the brief kept");
        assert_eq!(b.brand_colors, ["#f5efe6", "#2b2118", "#b5651d"]);
        let heading = b.type_style("heading").unwrap();
        assert_eq!((heading.font.as_deref(), heading.color.as_deref(), heading.size), (Some("Playfair Display"), Some("#f5efe6"), Some(72.0)));
        assert_eq!(b.type_styles.iter().filter(|s| s.name.eq_ignore_ascii_case("heading")).count(), 1, "replaced, not doubled");
        assert_eq!(b.type_style("callout").unwrap().font.as_deref(), Some("Lora"));
        assert_eq!(b.type_style("card").unwrap().fill.as_deref(), Some("#2b2118"));
        assert_eq!(b.type_style("kicker").unwrap().size, Some(30.0), "the brief's own look kept");
        assert_eq!(editorial.caption_look().unwrap().font, "Lora");
        assert!(BrandKit::new("Empty").caption_look().is_none() && BrandKit::new("Empty").title_styles().is_empty());
    }

    /// A kit shared as a file and installed elsewhere is the same kit, its
    /// logo and licence with it; a file without a licence is not shared; a
    /// kit already there is kept unless replaced.
    #[test]
    fn a_kit_is_shared_as_a_file_and_installed() {
        let (root, src, other) = (tmp("share-root"), tmp("share-src"), tmp("share-other"));
        std::fs::write(src.join("logo.png"), b"\x89PNG the logo").unwrap();
        let mut kit = BrandKit::new("Shared");
        kit.colours.primary = Some("#123456".into());
        kit.logo = Some(Logo { file: "logo.png".into(), corner: Corner::TopLeft, margin: 0.05, scale: 0.1, opacity: 1.0 });
        save(&root, &kit, &[src.join("logo.png")]).unwrap();
        let out = src.join("shared.klippbrand");
        assert!(export_kit(Some(&root), "Shared", &out, &Default::default()).unwrap_err().contains("no licence for logo.png"));
        let licences = [("logo.png".to_string(), "CC0".to_string())].into_iter().collect();
        export_kit(Some(&root), "Shared", &out, &licences).unwrap();
        let found = import_kit(&other, &out, false).unwrap();
        assert_eq!(found.kit, kit);
        let dir = found.dir.unwrap();
        assert_eq!(std::fs::read(dir.join("logo.png")).unwrap(), b"\x89PNG the logo");
        assert_eq!(std::fs::read_to_string(dir.join("logo.png.licence.txt")).unwrap(), "CC0");
        assert!(import_kit(&other, &out, false).unwrap_err().contains("already"));
        import_kit(&other, &out, true).unwrap();
        assert!(export_kit(Some(&root), "Shared", &src.join("x.zip"), &licences).is_err(), "the extension");
        export_kit(None, "Bold", &src.join("bold.klippbrand"), &Default::default()).unwrap();
    }

    /// A hostile shared kit: not a zip, no kit file, a later format, a
    /// file name that climbs out, a named file missing, one with no
    /// licence, an oversized logo and an oversized kit file are each
    /// refused, and nothing is left in the kits folder.
    #[test]
    fn a_hostile_shared_kit_is_refused() {
        use std::io::Write;
        let (dir, root) = (tmp("hostile-share"), tmp("hostile-share-root"));
        let zip_of = |name: &str, entries: &[(&str, Vec<u8>)]| {
            let path = dir.join(name);
            let mut z = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
            for (p, b) in entries {
                z.start_file(*p, zip::write::SimpleFileOptions::default()).unwrap();
                z.write_all(b).unwrap();
            }
            z.finish().unwrap();
            path
        };
        let kit = |extra: &str| format!("format = 1\nname = \"K\"\n{extra}").into_bytes();
        std::fs::write(dir.join("notzip.klippbrand"), b"not a zip").unwrap();
        let logo = "[logo]\nfile = \"logo.png\"\n";
        let cases = [
            (dir.join("notzip.klippbrand"), "not a zip"),
            (zip_of("nokit.klippbrand", &[("other.txt", b"x".to_vec())]), "no kit file"),
            (zip_of("later.klippbrand", &[(KIT_FILE, b"format = 9\nname = \"K\"\n".to_vec())]), "a later format"),
            (zip_of("climb.klippbrand", &[(KIT_FILE, kit("[logo]\nfile = \"../../evil.png\"\n")), ("../../evil.png", b"x".to_vec())]), "a name that climbs out"),
            (zip_of("missing.klippbrand", &[(KIT_FILE, kit(logo))]), "a missing file"),
            (zip_of("unlicensed.klippbrand", &[(KIT_FILE, kit(logo)), ("logo.png", b"x".to_vec())]), "no licence"),
            (zip_of("huge.klippbrand", &[(KIT_FILE, kit(logo)), ("logo.png", vec![0u8; MAX_LOGO_BYTES as usize + 1]), ("logo.png.licence.txt", b"CC0".to_vec())]), "an oversized logo"),
            (zip_of("hugekit.klippbrand", &[(KIT_FILE, vec![b'#'; MAX_KIT_BYTES as usize + 1])]), "an oversized kit file"),
        ];
        for (path, why) in cases {
            assert!(import_kit(&root, &path, true).is_err(), "{why} was installed");
        }
        let left: Vec<_> = std::fs::read_dir(kits_dir(&root)).map(|d| d.flatten().map(|e| e.file_name()).collect()).unwrap_or_default();
        assert!(left.is_empty(), "nothing left behind: {left:?}");
    }

    /// A hostile kit is refused: a later format, an unknown field, a colour
    /// that is not one, a number out of range, a file outside its folder or
    /// of another kind, an intro both template and file, a file too large,
    /// and a link where the logo should be.
    #[test]
    fn a_hostile_kit_is_refused() {
        let base = "format = 1\nname = \"K\"\n";
        for (body, why) in [
            ("format = 2\nname = \"K\"\n", "a later format"),
            (&*format!("{base}evil = 1\n"), "an unknown field"),
            (&*format!("{base}[colours]\nprimary = \"red\"\n"), "a colour"),
            (&*format!("{base}[logo]\nfile = \"logo.png\"\nopacity = 5.0\n"), "a range"),
            (&*format!("{base}[logo]\nfile = \"../../etc/passwd.png\"\n"), "a path out"),
            (&*format!("{base}[logo]\nfile = \"/etc/logo.png\"\n"), "an absolute path"),
            (&*format!("{base}[logo]\nfile = \"logo.sh\"\n"), "a kind"),
            (&*format!("{base}[intro]\ntemplate = \"Hook Card\"\nfile = \"a.mp4\"\n"), "both"),
            ("format = 1\nname = \"---\"\n", "a name with nothing in it"),
        ] {
            assert!(BrandKit::read(body).is_err(), "{why} was read");
        }
        let dir = tmp("hostile");
        std::fs::write(dir.join(KIT_FILE), format!("{base}[logo]\nfile = \"logo.png\"\n")).unwrap();
        assert!(read_dir(&dir).is_err(), "the logo is not there");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/hostname", dir.join("logo.png")).unwrap();
            assert!(read_dir(&dir).is_err(), "a link is not the logo");
        }
        std::fs::write(dir.join(KIT_FILE), "x".repeat(MAX_KIT_BYTES as usize + 1)).unwrap();
        assert!(read_dir(&dir).is_err(), "too large");
    }
}
