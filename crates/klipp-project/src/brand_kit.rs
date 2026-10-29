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
