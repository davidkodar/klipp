//! Template packs (#2757): templates shared as one `.klipppack` zip.
//!
//! `pack.json` holds the format, the pack's name and its templates: title
//! templates, composition templates and recipes, whole. `assets/` holds
//! each picture a template draws, with its licence text beside it, and
//! each font that is not in the catalogue, with its licence. A catalogue
//! font goes by name only: it is fetched where the pack is used.
//!
//! Importing writes the templates into the user's folders and the assets
//! beside them, and reports what could not come across and what is drawn
//! instead: a font neither carried, catalogued nor installed, a picture
//! the pack lacks.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use klipp_core::TemplateValue;
use klipp_timeline::composition_template::{CompositionTemplate, CompositionTemplateStore};
use klipp_timeline::{ClipKind, TemplateFile, TitleData, TitleObjectContent};
use serde::{Deserialize, Serialize};

/// A pack file's extension.
pub const PACK_EXTENSION: &str = "klipppack";

/// The pack format this Klipp reads; a newer one is refused, not guessed
/// at. 2 carries presets (#3095); a pack without any is written as 1, so
/// a Klipp from before reads it.
pub const PACK_FORMAT: u32 = 2;

/// The most preset files a pack carries of one kind, and in all.
pub const MAX_PACK_PRESET_FILES: usize = klipp_core::preset_file::MAX_FILES;
pub const MAX_PACK_PRESETS: usize = 256;

/// `pack.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackManifest {
    pub format: u32,
    pub name: String,
    #[serde(default)]
    pub written_by: String,
    /// Who made it, its version and its licence, as the pack says.
    #[serde(default, flatten)]
    pub about: PackAbout,
    /// Title templates (`.klipptitle`).
    #[serde(default)]
    pub titles: Vec<TemplateFile>,
    /// Composition templates and recipes (`.klipptemplate`).
    #[serde(default)]
    pub templates: Vec<CompositionTemplate>,
    #[serde(default)]
    pub fonts: Vec<PackFont>,
    #[serde(default)]
    pub pictures: Vec<PackPicture>,
    /// Preset files (#3095): each a kind's folder name and its file in
    /// the pack, `presets/<kind>/<name>.toml`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub presets: Vec<PackPreset>,
}

/// A preset file a pack carries: the kind it is, by the folder a person
/// keeps that kind in, and where it is in the pack.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackPreset {
    pub kind: String,
    pub file: String,
}

/// A preset file to pack: its kind's folder, its file name and its text.
#[derive(Debug, Clone, PartialEq)]
pub struct PackPresetFile {
    pub kind: String,
    pub file_name: String,
    pub text: String,
}

/// What a pack says of itself (#2757): who made it, which version, under
/// which licence. Shown with the installed pack; nothing reads it as more.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PackAbout {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub author: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub licence: String,
}

/// A font the templates use: by catalogue name, or carried as its files
/// and licence (paths in the pack).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackFont {
    pub family: String,
    #[serde(default)]
    pub catalogue: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub licence: Option<String>,
}

/// A picture the templates draw, and its licence text (paths in the pack).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackPicture {
    pub file: String,
    pub licence: String,
}

/// What the pack needs to know of fonts, from whoever has the font list.
pub trait FontLookup {
    /// Shipped inside Klipp: nothing to carry or fetch.
    fn bundled(&self, family: &str) -> bool;
    /// In the font catalogue: fetched by name where it is used.
    fn in_catalogue(&self, family: &str) -> bool;
    /// An installed family's font files and its licence text, to carry;
    /// `None` when it is not installed or no licence is found beside it.
    fn carry(&self, family: &str) -> Option<(Vec<PathBuf>, String)>;
    /// Whether the family can be drawn here, now or on use.
    fn available(&self, family: &str) -> bool;
    /// What a title draws a missing family in.
    fn fallback(&self) -> String;
}

/// Where an import puts things.
#[derive(Debug, Clone)]
pub struct PackTargets {
    /// Title templates.
    pub titles: PathBuf,
    /// Composition templates and recipes.
    pub templates: PathBuf,
    /// Pictures, in a folder per pack.
    pub assets: PathBuf,
    /// Carried fonts, a folder each (`<family>`): the pack's own
    /// `fonts` folder when installed, which the title renderer reads.
    pub fonts: PathBuf,
    /// Preset files, a folder per kind: the pack's own `presets` folder
    /// when installed, which every kind reads (#3095).
    pub presets: PathBuf,
}

/// What an import did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImportReport {
    pub added: Vec<String>,
    /// Templates of a name already there, left as they were.
    pub skipped: Vec<String>,
    /// What could not come across, and what is drawn instead.
    pub fallbacks: Vec<String>,
    pub fonts_added: Vec<String>,
    /// Preset files carried in, `<kind>/<file>`, and what of their
    /// entries won't be offered, said (#3095).
    pub presets_added: Vec<String>,
    pub presets_skipped: Vec<String>,
}

impl ImportReport {
    /// The report as one reply.
    pub fn text(&self, pack: &str) -> String {
        let mut out = format!("imported \"{pack}\": {}", if self.added.is_empty() { "no templates".to_string() } else { self.added.join(", ") });
        if !self.skipped.is_empty() {
            out.push_str(&format!("; already there, kept (replace true takes the pack's): {}", self.skipped.join(", ")));
        }
        if !self.fonts_added.is_empty() {
            out.push_str(&format!("; fonts carried in: {}", self.fonts_added.join(", ")));
        }
        if !self.presets_added.is_empty() {
            out.push_str(&format!("; presets carried in: {}", self.presets_added.join(", ")));
        }
        if !self.presets_skipped.is_empty() {
            out.push_str(&format!("; presets not offered: {}", self.presets_skipped.join("; ")));
        }
        if self.fallbacks.is_empty() {
            out.push_str("; nothing missing");
        } else {
            out.push_str(&format!("; missing: {}", self.fallbacks.join("; ")));
        }
        out
    }
}

/// Every title in `titles` and `templates`, to look at or change.
fn each_title<'a>(titles: &'a mut [TemplateFile], templates: &'a mut [CompositionTemplate], mut f: impl FnMut(&mut TitleData)) {
    for t in titles.iter_mut() {
        f(&mut t.data);
    }
    for t in templates.iter_mut() {
        for clip in t.sequences.iter_mut().flat_map(|s| s.tracks.iter_mut()).flat_map(|t| t.clips.iter_mut()) {
            if let ClipKind::Title(td) = &mut clip.kind {
                f(td);
            }
        }
    }
}

/// The fonts and pictures the templates use, each once, in order met.
fn uses(titles: &mut [TemplateFile], templates: &mut [CompositionTemplate]) -> (Vec<String>, Vec<String>) {
    let (mut fonts, mut pictures) = (Vec::<String>::new(), Vec::<String>::new());
    each_title(titles, templates, |td| {
        for o in &td.objects {
            match &o.content {
                TitleObjectContent::TextBox(tb) if !tb.font_name.trim().is_empty() && !fonts.contains(&tb.font_name) => fonts.push(tb.font_name.clone()),
                TitleObjectContent::Image(img) if !img.path.trim().is_empty() && !pictures.contains(&img.path) => pictures.push(img.path.clone()),
                _ => {}
            }
        }
    });
    (fonts, pictures)
}

/// Each picture path in the templates, and each picture parameter's
/// default, through `map`; one not in it is left.
fn repath(titles: &mut [TemplateFile], templates: &mut [CompositionTemplate], map: &BTreeMap<String, String>) {
    each_title(titles, templates, |td| {
        for o in td.objects.iter_mut() {
            if let TitleObjectContent::Image(img) = &mut o.content {
                if let Some(to) = map.get(&img.path) {
                    img.path = to.clone();
                }
            }
        }
    });
    for t in templates.iter_mut() {
        for p in t.parameters.values_mut() {
            if let TemplateValue::Text(path) = &mut p.default {
                if let Some(to) = map.get(path.as_str()) {
                    *path = to.clone();
                }
            }
        }
    }
}

/// The most one entry of a pack may hold: a picture or a font.
pub const MAX_ENTRY_BYTES: u64 = 100 * 1024 * 1024;

/// The most a whole pack may unpack to.
pub const MAX_PACK_BYTES: u64 = 500 * 1024 * 1024;

/// A file name from a shared file, safe to join: its stem and extension
/// each a [`klipp_core::safe_name`].
pub(crate) fn safe_file(name: &str) -> String {
    let leaf = name.rsplit(['/', '\\']).next().unwrap_or(name);
    match leaf.rsplit_once('.') {
        Some((stem, ext)) if !stem.trim_matches('.').is_empty() => format!("{}.{}", klipp_core::safe_name(stem), klipp_core::safe_name(ext)),
        _ => klipp_core::safe_name(leaf),
    }
}

use crate::path_util::inside;

/// Reads one entry of the pack, at most [`MAX_ENTRY_BYTES`] (or `cap`)
/// and within what is left of [`MAX_PACK_BYTES`]; `None` when the pack
/// has no such entry.
pub(crate) fn read_entry(zip: &mut zip::ZipArchive<std::fs::File>, path: &str, cap: u64, left: &mut u64) -> Result<Option<Vec<u8>>, String> {
    let entry = match zip.by_name(path) {
        Ok(e) => e,
        Err(_) => return Ok(None),
    };
    let limit = cap.min(*left);
    if entry.size() > limit {
        return Err(format!("{path} is {} MB unpacked, more than a pack may hold ({} MB)", entry.size() / (1024 * 1024), limit / (1024 * 1024)));
    }
    let mut bytes = Vec::new();
    entry.take(limit + 1).read_to_end(&mut bytes).map_err(|e| format!("{path}: {e}"))?;
    if bytes.len() as u64 > limit {
        return Err(format!("{path} unpacks past what a pack may hold ({} MB)", limit / (1024 * 1024)));
    }
    *left -= bytes.len() as u64;
    Ok(Some(bytes))
}

/// A picture's licence: given by its path or file name in `licences`, or
/// a `<file>.licence.txt` (or `.license.txt`) beside it.
fn picture_licence(path: &Path, licences: &BTreeMap<String, String>) -> Option<String> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    licences.get(path.to_string_lossy().as_ref()).or_else(|| licences.get(name)).filter(|t| !t.trim().is_empty()).cloned().or_else(|| {
        ["licence.txt", "license.txt"].iter().find_map(|ext| std::fs::read_to_string(path.with_file_name(format!("{name}.{ext}"))).ok()).filter(|t| !t.trim().is_empty())
    })
}

/// Writes `titles` and `templates` as the pack `name` to `out`. Every
/// picture needs its licence (see [`picture_licence`]): those without
/// are refused, named. A font is named when it is in the catalogue,
/// carried when it is installed with a licence beside it, and said to be
/// left out otherwise. Returns what the pack holds, as a reply.
#[allow(clippy::too_many_arguments)]
pub fn export_pack(out: &Path, name: &str, about: &PackAbout, titles: Vec<TemplateFile>, templates: Vec<CompositionTemplate>, licences: &BTreeMap<String, String>, fonts: &dyn FontLookup) -> Result<String, String> {
    export_pack_with(out, name, about, titles, templates, &[], licences, fonts)
}

/// [`export_pack`], with `presets` carried too (#3095), from
/// [`presets_for_pack`].
#[allow(clippy::too_many_arguments)]
pub fn export_pack_with(out: &Path, name: &str, about: &PackAbout, mut titles: Vec<TemplateFile>, mut templates: Vec<CompositionTemplate>, presets: &[PackPresetFile], licences: &BTreeMap<String, String>, fonts: &dyn FontLookup) -> Result<String, String> {
    use zip::write::{SimpleFileOptions, ZipWriter};
    let name = name.trim();
    if name.is_empty() {
        return Err("a pack needs a name".into());
    }
    if titles.is_empty() && templates.is_empty() && presets.is_empty() {
        return Err("no templates or presets to pack".into());
    }
    let (families, pictures) = uses(&mut titles, &mut templates);
    let unlicensed: Vec<&String> = pictures.iter().filter(|p| picture_licence(Path::new(p), licences).is_none()).collect();
    if !unlicensed.is_empty() {
        return Err(format!(
            "no licence for {}: give licences {{\"<file name>\": \"<licence text>\"}}, or put <file name>.licence.txt beside each",
            unlicensed.iter().map(|p| Path::new(p).file_name().and_then(|n| n.to_str()).unwrap_or(p)).collect::<Vec<_>>().join(", ")
        ));
    }
    let file = std::fs::File::create(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let put = |zip: &mut ZipWriter<std::fs::File>, path: &str, bytes: &[u8]| -> Result<(), String> {
        zip.start_file(path, options).map_err(|e| e.to_string())?;
        zip.write_all(bytes).map_err(|e| e.to_string())
    };
    let mut manifest_pictures = Vec::new();
    let mut map = BTreeMap::new();
    for (n, p) in pictures.iter().enumerate() {
        let path = Path::new(p);
        let bytes = std::fs::read(path).map_err(|e| format!("picture {}: {e}", path.display()))?;
        let file = format!("assets/pictures/{}-{}", n + 1, safe_file(path.file_name().and_then(|n| n.to_str()).unwrap_or("picture")));
        let licence = format!("{file}.licence.txt");
        put(&mut zip, &file, &bytes)?;
        put(&mut zip, &licence, picture_licence(path, licences).unwrap_or_default().as_bytes())?;
        map.insert(p.clone(), file.clone());
        manifest_pictures.push(PackPicture { file, licence });
    }
    repath(&mut titles, &mut templates, &map);
    let (mut named, mut carried, mut left) = (Vec::new(), Vec::new(), Vec::new());
    let mut manifest_fonts = Vec::new();
    for family in families.iter().filter(|f| !fonts.bundled(f)) {
        if fonts.in_catalogue(family) {
            named.push(family.clone());
            manifest_fonts.push(PackFont { family: family.clone(), catalogue: true, files: Vec::new(), licence: None });
        } else if let Some((files, licence)) = fonts.carry(family) {
            let dir = format!("assets/fonts/{}", klipp_core::safe_name(family));
            let mut packed = Vec::new();
            for f in &files {
                let bytes = std::fs::read(f).map_err(|e| format!("font {}: {e}", f.display()))?;
                let at = format!("{dir}/{}", safe_file(f.file_name().and_then(|n| n.to_str()).unwrap_or("font")));
                put(&mut zip, &at, &bytes)?;
                packed.push(at);
            }
            let at = format!("{dir}/LICENCE.txt");
            put(&mut zip, &at, licence.as_bytes())?;
            carried.push(family.clone());
            manifest_fonts.push(PackFont { family: family.clone(), catalogue: false, files: packed, licence: Some(at) });
        } else {
            left.push(family.clone());
            manifest_fonts.push(PackFont { family: family.clone(), catalogue: false, files: Vec::new(), licence: None });
        }
    }
    let mut manifest_presets = Vec::new();
    for p in presets {
        let file = format!("presets/{}/{}", p.kind, p.file_name);
        put(&mut zip, &file, p.text.as_bytes())?;
        manifest_presets.push(PackPreset { kind: p.kind.clone(), file });
    }
    let count = titles.len() + templates.len();
    // Format 1 when there are no presets: a Klipp from before reads it.
    let format = if manifest_presets.is_empty() { 1 } else { PACK_FORMAT };
    let manifest = PackManifest { format, name: name.to_string(), written_by: env!("CARGO_PKG_VERSION").to_string(), about: about.clone(), titles, templates, fonts: manifest_fonts, pictures: manifest_pictures, presets: manifest_presets };
    put(&mut zip, "pack.json", serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?.as_bytes())?;
    zip.finish().map_err(|e| e.to_string())?;
    let mut text = format!("packed \"{name}\" to {}: {count} template{}, {} picture{}", out.display(), if count == 1 { "" } else { "s" }, pictures.len(), if pictures.len() == 1 { "" } else { "s" });
    if !presets.is_empty() {
        text.push_str(&format!(", preset files: {}", presets.iter().map(|p| format!("{}/{}", p.kind, p.file_name)).collect::<Vec<_>>().join(", ")));
    }
    if !named.is_empty() {
        text.push_str(&format!("; catalogue fonts by name: {}", named.join(", ")));
    }
    if !carried.is_empty() {
        text.push_str(&format!("; fonts carried with their licence: {}", carried.join(", ")));
    }
    if !left.is_empty() {
        text.push_str(&format!("; left out (not in the catalogue, no licence found beside the font): {}, drawn in {} where they are missing", left.join(", "), fonts.fallback()));
    }
    Ok(text)
}

/// Reads the pack at `pack` into `to`. A template whose name is taken is
/// kept as it is unless `replace`. Pictures go to a folder of the pack's
/// own, carried fonts beside the catalogue's.
pub fn import_pack(pack: &Path, to: &PackTargets, fonts: &dyn FontLookup, replace: bool) -> Result<(String, ImportReport), String> {
    import_pack_within(pack, to, fonts, replace, MAX_ENTRY_BYTES, MAX_PACK_BYTES)
}

/// [`import_pack`] with its caps given: `entry` bytes a file, `total`
/// the pack.
fn import_pack_within(pack: &Path, to: &PackTargets, fonts: &dyn FontLookup, replace: bool, entry: u64, total: u64) -> Result<(String, ImportReport), String> {
    let file = std::fs::File::open(pack).map_err(|e| format!("{}: {e}", pack.display()))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("not a template pack: {e}"))?;
    // A pack is someone else's file: every entry read within its caps,
    // every path written built from safe names and checked to stay in
    // its folder.
    let mut left = total;
    let json = read_entry(&mut zip, "pack.json", entry, &mut left)?.ok_or("not a template pack: it has no pack.json")?;
    let mut manifest: PackManifest = serde_json::from_slice(&json).map_err(|e| format!("pack.json does not read: {e}"))?;
    if manifest.format > PACK_FORMAT {
        return Err(format!("made by a newer Klipp (pack format {}, this one reads {PACK_FORMAT})", manifest.format));
    }
    let mut report = ImportReport::default();
    // Pictures, into a folder of the pack's own.
    let pack_folder = klipp_core::safe_name(&manifest.name);
    let mut map = BTreeMap::new();
    for p in &manifest.pictures {
        let leaf = safe_file(&p.file);
        let bytes = if p.file.starts_with("assets/pictures/") { read_entry(&mut zip, &p.file, entry, &mut left)? } else { None };
        match bytes {
            Some(bytes) => {
                let at = inside(&to.assets, &[&pack_folder, &leaf])?;
                std::fs::create_dir_all(at.parent().unwrap_or(&to.assets)).map_err(|e| format!("{}: {e}", to.assets.display()))?;
                std::fs::write(&at, bytes).map_err(|e| format!("{}: {e}", at.display()))?;
                if let Some(licence) = read_entry(&mut zip, &p.licence, entry, &mut left)? {
                    std::fs::write(inside(&to.assets, &[&pack_folder, &format!("{leaf}.licence.txt")])?, licence).map_err(|e| e.to_string())?;
                }
                map.insert(p.file.clone(), at.to_string_lossy().into_owned());
            }
            None => report.fallbacks.push(format!("picture {leaf} is not in the pack: its layer is left empty")),
        }
    }
    repath(&mut manifest.titles, &mut manifest.templates, &map);
    // Carried fonts, beside the catalogue's.
    for f in manifest.fonts.iter().filter(|f| !f.files.is_empty()) {
        if fonts.available(&f.family) {
            continue;
        }
        let family = klipp_core::safe_name(&f.family);
        let mut any = false;
        for path in f.files.iter().chain(f.licence.iter()).filter(|p| p.starts_with("assets/fonts/")) {
            if let Some(bytes) = read_entry(&mut zip, path, entry, &mut left)? {
                let at = inside(&to.fonts, &[&family, &safe_file(path)])?;
                std::fs::create_dir_all(at.parent().unwrap_or(&to.fonts)).map_err(|e| format!("{}: {e}", to.fonts.display()))?;
                std::fs::write(&at, bytes).map_err(|e| format!("{}: {e}", at.display()))?;
                any |= f.files.contains(path);
            }
        }
        if any {
            report.fonts_added.push(f.family.clone());
        }
    }
    // Presets (#3095), every file checked whole before any is written.
    for (folder, leaf, text) in read_pack_presets(&mut zip, &manifest.presets, entry.min(klipp_core::preset_file::MAX_FILE_BYTES as u64), &mut left)? {
        let at = inside(&to.presets, &[&folder, &leaf])?;
        std::fs::create_dir_all(at.parent().unwrap_or(&to.presets)).map_err(|e| format!("{}: {e}", to.presets.display()))?;
        std::fs::write(&at, text).map_err(|e| format!("{}: {e}", at.display()))?;
        report.presets_added.push(format!("{folder}/{leaf}"));
    }
    // Templates, a name already there kept unless replaced.
    for t in &manifest.titles {
        match klipp_timeline::save_template(&to.titles, &t.name, &t.category, &t.data, replace) {
            Ok(_) => report.added.push(t.name.clone()),
            Err(e) if e.contains("saved already") => report.skipped.push(t.name.clone()),
            Err(e) => return Err(format!("title template \"{}\": {e}", t.name)),
        }
    }
    let mut store = CompositionTemplateStore::load(Some(to.templates.clone()));
    for t in &manifest.templates {
        let mut t = t.clone();
        if let Some(existing) = store.templates().chain(store.recipes()).find(|e| e.name.eq_ignore_ascii_case(t.name.trim())) {
            if !replace {
                report.skipped.push(t.name.clone());
                continue;
            }
            t.id = existing.id;
        }
        let name = t.name.clone();
        store.save(t).map_err(|e| format!("template \"{name}\": {e}"))?;
        report.added.push(name);
    }
    // Fonts that are still not here, after all that.
    let used: BTreeSet<String> = manifest.fonts.iter().map(|f| f.family.clone()).collect();
    for family in used.iter().filter(|f| !fonts.bundled(f) && !fonts.available(f) && !report.fonts_added.contains(f)) {
        report.fallbacks.push(format!("font {family} is not here: drawn in {}", fonts.fallback()));
    }
    Ok((manifest.name.clone(), report))
}

/// Where a pack's preset goes, `(kind folder, file name)`, or why it is
/// refused: its file is `presets/<kind>/<name>.toml` and nothing else,
/// its kind one Klipp keeps, its name a safe file name that is not
/// hidden.
fn preset_place(p: &PackPreset) -> Result<(String, String), String> {
    let refused = |why: &str| Err(format!("preset file \"{}\": {why}", p.file));
    let kind = klipp_core::preset_kinds::kind_in(&p.kind).map(|k| k.folder);
    let Some(kind) = kind else {
        let kinds: Vec<&str> = klipp_core::preset_kinds::PRESET_KINDS.iter().map(|k| k.folder).collect();
        return refused(&format!("\"{}\" is not a kind of preset; the kinds are {}", p.kind, kinds.join(", ")));
    };
    if p.file.contains('\\') || p.file.contains('\0') {
        return refused("a path of / and plain names");
    }
    let parts: Vec<&str> = p.file.split('/').collect();
    let [top, folder, leaf] = parts[..] else { return refused("a path of presets/<kind>/<name>.toml") };
    if top != "presets" || folder != kind {
        return refused(&format!("a path of presets/{kind}/<name>.toml"));
    }
    if leaf.starts_with('.') || leaf.len() <= ".toml".len() || !leaf.ends_with(".toml") || safe_file(leaf) != leaf {
        return refused("a name of letters, digits, - and _, not hidden, ending .toml");
    }
    Ok((kind.to_string(), leaf.to_string()))
}

/// The preset files `presets` names, read from the pack and each checked
/// whole as its kind's file, or why the pack is refused: a place
/// [`preset_place`] refuses, the same place twice, more than
/// [`MAX_PACK_PRESET_FILES`] of one kind or [`MAX_PACK_PRESETS`] in all
/// (counted before any is read), an entry that is a link or a folder or
/// is missing, one that unpacks past `cap` bytes (counted as it unpacks,
/// whatever its header says), text that isn't UTF-8, or a file the kind's
/// reader refuses (another kind, a newer format, entries past the cap).
fn read_pack_presets(zip: &mut zip::ZipArchive<std::fs::File>, presets: &[PackPreset], cap: u64, left: &mut u64) -> Result<Vec<(String, String, String)>, String> {
    if presets.len() > MAX_PACK_PRESETS {
        return Err(format!("{} preset files: a pack carries at most {MAX_PACK_PRESETS}", presets.len()));
    }
    let mut places = Vec::new();
    for p in presets {
        let place = preset_place(p)?;
        if places.contains(&place) {
            return Err(format!("preset file \"{}\" is named twice", p.file));
        }
        places.push(place);
    }
    for kind in klipp_core::preset_kinds::PRESET_KINDS {
        let n = places.iter().filter(|(k, _)| k == kind.folder).count();
        if n > MAX_PACK_PRESET_FILES {
            return Err(format!("{n} {} preset files: a pack carries at most {MAX_PACK_PRESET_FILES} of a kind", kind.folder));
        }
    }
    let mut out = Vec::new();
    for (p, (folder, leaf)) in presets.iter().zip(places) {
        {
            let e = zip.by_name(&p.file).map_err(|_| format!("preset file \"{}\" is not in the pack", p.file))?;
            let link = e.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000);
            if link || e.is_dir() {
                return Err(format!("preset file \"{}\" is not a plain file", p.file));
            }
        }
        let bytes = read_entry(zip, &p.file, cap, left)?.ok_or_else(|| format!("preset file \"{}\" is not in the pack", p.file))?;
        let text = String::from_utf8(bytes).map_err(|_| format!("preset file \"{}\" is not text", p.file))?;
        let kind = klipp_core::preset_kinds::kind_in(&folder).expect("checked by preset_place");
        klipp_core::preset_file::read::<toml::Table>(&text, kind.kind, kind.version, kind.table).map_err(|why| format!("preset file \"{}\": {why}", p.file))?;
        out.push((folder, leaf, text));
    }
    Ok(out)
}

/// The presets named `wanted` (kind folder, name) from the person's own
/// folders, for a pack (#3095): a file a kind, its entries whole as the
/// person wrote them, named `<file_name>.toml`. A name none of the
/// person's files has is refused, with what there is.
pub fn presets_for_pack(wanted: &[(String, String)], file_name: &str) -> Result<Vec<PackPresetFile>, String> {
    let mut out = Vec::new();
    for kind in klipp_core::preset_kinds::PRESET_KINDS {
        let names: Vec<&String> = wanted.iter().filter(|(k, _)| k == kind.folder).map(|(_, n)| n).collect();
        if names.is_empty() {
            continue;
        }
        let dir = klipp_core::preset_file::user_dir(kind.folder).ok_or("no presets folder")?;
        let (entries, _) = klipp_core::preset_file::read_dir::<toml::Table>(&dir, kind.kind, kind.version, kind.table);
        let key = |e: &toml::Table| e.get(kind.key).and_then(toml::Value::as_str).map(|n| n.trim().to_lowercase());
        let mut picked = Vec::new();
        for name in names {
            let folded = name.trim().to_lowercase();
            match entries.iter().find(|e| key(e).as_deref() == Some(folded.as_str())) {
                Some(e) => picked.push(toml::Value::Table(e.clone())),
                None => {
                    let there: Vec<String> = entries.iter().filter_map(|e| e.get(kind.key).and_then(toml::Value::as_str).map(|n| format!("\"{n}\""))).collect();
                    return Err(format!("no {} of yours called \"{}\"{}", kind.folder, name.trim(), if there.is_empty() { String::new() } else { format!(": yours are {}", there.join(", ")) }));
                }
            }
        }
        let mut file = toml::Table::new();
        file.insert("kind".into(), toml::Value::String(kind.kind.into()));
        file.insert("format_version".into(), toml::Value::Integer(i64::from(kind.version)));
        file.insert(kind.table.into(), toml::Value::Array(picked));
        out.push(PackPresetFile { kind: kind.folder.to_string(), file_name: format!("{}.toml", klipp_core::safe_name(file_name)), text: toml::to_string(&file).map_err(|e| e.to_string())? });
    }
    if let Some((k, _)) = wanted.iter().find(|(k, _)| klipp_core::preset_kinds::kind_in(k).is_none()) {
        let kinds: Vec<&str> = klipp_core::preset_kinds::PRESET_KINDS.iter().map(|k| k.folder).collect();
        return Err(format!("\"{k}\" is not a kind of preset; the kinds are {}", kinds.join(", ")));
    }
    Ok(out)
}

/// What of a pack's presets won't be offered beside what is already
/// there (#3095): for each kind it carries, the entries its folder
/// `adding` adds after `before` (the person's own and the packs installed
/// before it), and what of it is skipped, said, beside what was already.
fn presets_beside(adding: &Path, before: impl Fn(&str) -> Vec<PathBuf>) -> Vec<String> {
    let mut said = Vec::new();
    for kind in klipp_core::preset_kinds::PRESET_KINDS {
        let dir = adding.join(kind.folder);
        if !dir.is_dir() {
            continue;
        }
        let without = before(kind.folder);
        let with: Vec<PathBuf> = without.iter().cloned().chain(std::iter::once(dir)).collect();
        let was = klipp_core::preset_kinds::skipped_in(kind.folder, &without).unwrap_or_default();
        for why in klipp_core::preset_kinds::skipped_in(kind.folder, &with).unwrap_or_default() {
            if !was.contains(&why) {
                said.push(format!("{}: {why}", kind.folder));
            }
        }
    }
    said
}

/// An installed pack (#2757), as its folder's `pack.json` and contents say.
#[derive(Debug, Clone, PartialEq)]
pub struct InstalledPack {
    /// The folder's name under `packs/`: the pack's name made safe.
    pub folder: String,
    pub name: String,
    pub about: PackAbout,
    pub titles: usize,
    pub templates: usize,
}

/// What an installed pack's `pack.json` keeps: its name and what it says
/// of itself, not its templates, which are files beside it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct InstalledManifest {
    format: u32,
    name: String,
    #[serde(default)]
    written_by: String,
    #[serde(default, flatten)]
    about: PackAbout,
    /// When the pack was first installed, seconds since 1970 (#3095): an
    /// update keeps it, so of two packs with a preset of one name the one
    /// installed first keeps it. 0 for one installed before it was kept.
    #[serde(default)]
    installed_secs: u64,
}

/// The pack at `pack` installed under `root` (Klipp's data folder) as a
/// folder of its own, `packs/<safe name>/` (#2757): its title templates,
/// templates and recipes, pictures and carried fonts, and its `pack.json`.
/// One already installed under that name is refused unless `update`, which
/// replaces it whole. It is unpacked beside the others first and moved in
/// only when whole, so a failed install leaves what was there.
pub fn install_pack(pack: &Path, root: &Path, fonts: &dyn FontLookup, update: bool) -> Result<(InstalledPack, ImportReport), String> {
    let file = std::fs::File::open(pack).map_err(|e| format!("{}: {e}", pack.display()))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("not a template pack: {e}"))?;
    let mut left = MAX_PACK_BYTES;
    let json = read_entry(&mut zip, "pack.json", MAX_ENTRY_BYTES, &mut left)?.ok_or("not a template pack: it has no pack.json")?;
    let head: InstalledManifest = serde_json::from_slice(&json).map_err(|e| format!("pack.json does not read: {e}"))?;
    let folder = klipp_core::safe_name(&head.name);
    let packs = klipp_timeline::template_places::packs(root);
    let target = inside(&packs, &[&folder])?;
    if target.exists() && !update {
        return Err(format!("a pack called \"{}\" is installed already: update it, or remove it first", head.name));
    }
    let staging = inside(&packs, &[&format!(".installing-{}", uuid::Uuid::new_v4())])?;
    let result = (|| -> Result<ImportReport, String> {
        std::fs::create_dir_all(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
        let to = PackTargets { titles: staging.join("titles"), templates: staging.join("templates"), assets: staging.join("pictures"), fonts: staging.join("fonts"), presets: staging.join("presets") };
        let (name, mut report) = import_pack(pack, &to, fonts, false)?;
        // What of its presets won't be offered: beside the person's own
        // and the packs installed before it (an update keeps its place,
        // and is checked beside those before it, itself left out).
        let first_secs = if target.exists() { klipp_core::preset_places::installed_secs(&target) } else { u64::MAX };
        let before = |folder: &str| -> Vec<PathBuf> {
            klipp_core::preset_file::user_dir(folder)
                .into_iter()
                .chain(klipp_core::preset_places::pack_dirs(root, folder).into_iter().filter(|d| {
                    let pack = d.parent().and_then(Path::parent).unwrap_or(d);
                    pack != target && klipp_core::preset_places::installed_secs(pack) <= first_secs
                }))
                .collect()
        };
        report.presets_skipped = presets_beside(&to.presets, before);
        // First installed now, or when the pack it updates was.
        let first = if target.exists() { klipp_core::preset_places::installed_secs(&target) } else { 0 };
        let installed_secs = if first > 0 { first } else { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_secs().max(1)) };
        let manifest = InstalledManifest { format: PACK_FORMAT, name: name.clone(), written_by: head.written_by.clone(), about: head.about.clone(), installed_secs };
        std::fs::write(staging.join("pack.json"), serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        if target.exists() {
            std::fs::remove_dir_all(&target).map_err(|e| format!("{}: {e}", target.display()))?;
        }
        std::fs::rename(&staging, &target).map_err(|e| format!("{}: {e}", target.display()))?;
        // The templates' pictures, named where they were unpacked.
        let (from, to) = (staging.to_string_lossy().into_owned(), target.to_string_lossy().into_owned());
        for dir in [target.join("titles"), target.join("templates")] {
            for f in std::fs::read_dir(&dir).into_iter().flatten().flatten().map(|e| e.path()) {
                if let Ok(text) = std::fs::read_to_string(&f) {
                    if text.contains(&from) {
                        std::fs::write(&f, text.replace(&from, &to)).map_err(|e| e.to_string())?;
                    }
                }
            }
        }
        Ok(report)
    })();
    if staging.exists() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    let report = result?;
    let installed = installed_packs(root).into_iter().find(|p| p.folder == folder).ok_or("the pack did not install")?;
    Ok((installed, report))
}

/// The pack `name` (as it is called, or its folder's name) removed from
/// under `root`: its folder and nothing else.
pub fn remove_pack(root: &Path, name: &str) -> Result<InstalledPack, String> {
    let folder = klipp_core::safe_name(name);
    let Some(pack) = installed_packs(root).into_iter().find(|p| p.folder == folder) else {
        let there: Vec<String> = installed_packs(root).into_iter().map(|p| format!("\"{}\"", p.name)).collect();
        return Err(format!("no pack called \"{}\" is installed{}", name.trim(), if there.is_empty() { String::new() } else { format!(": there are {}", there.join(", ")) }));
    };
    let target = inside(&klipp_timeline::template_places::packs(root), &[&pack.folder])?;
    std::fs::remove_dir_all(&target).map_err(|e| format!("{}: {e}", target.display()))?;
    Ok(pack)
}

/// The packs installed under `root`, by folder.
pub fn installed_packs(root: &Path) -> Vec<InstalledPack> {
    let count = |dir: &Path, ext: &str| std::fs::read_dir(dir).into_iter().flatten().flatten().filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some(ext)).count();
    klipp_timeline::template_places::installed(root)
        .into_iter()
        .filter(|(folder, _)| !folder.starts_with('.'))
        .map(|(folder, path)| {
            let head: Option<InstalledManifest> = crate::path_util::read_to_string_capped(&path.join("pack.json"), 1024 * 1024).ok().and_then(|t| serde_json::from_str(&t).ok());
            InstalledPack {
                name: head.as_ref().map_or_else(|| folder.clone(), |h| h.name.clone()),
                about: head.map(|h| h.about).unwrap_or_default(),
                titles: count(&path.join("titles"), klipp_timeline::TEMPLATE_EXTENSION),
                templates: count(&path.join("templates"), klipp_timeline::composition_template::EXTENSION),
                folder,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_timeline::{Clip, ImageLayer, Sequence, TextBox};

    /// Fonts as a test sees them: Inter shipped, Anton in the catalogue,
    /// "House Sans" installed at `house` with a licence, nothing else,
    /// and whatever was carried into `fonts` under `packs/`.
    struct Fonts {
        house: Option<PathBuf>,
        fonts: PathBuf,
    }

    impl FontLookup for Fonts {
        fn bundled(&self, f: &str) -> bool {
            f == "Inter"
        }
        fn in_catalogue(&self, f: &str) -> bool {
            f == "Anton"
        }
        fn carry(&self, f: &str) -> Option<(Vec<PathBuf>, String)> {
            (f == "House Sans").then(|| self.house.clone()).flatten().map(|p| (vec![p], "House Sans licence".into()))
        }
        fn available(&self, f: &str) -> bool {
            self.bundled(f) || self.in_catalogue(f) || self.fonts.join(f.to_lowercase().replace(' ', "-")).is_dir()
        }
        fn fallback(&self) -> String {
            "Inter".into()
        }
    }

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("klipp-pack-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn title(fonts: &[&str], picture: Option<&Path>) -> TitleData {
        let mut td = TitleData::single_text(TextBox { text: "Launch".into(), font_name: fonts[0].into(), ..Default::default() });
        let at = td.objects[0].transform.clone();
        for f in &fonts[1..] {
            td.add_object(TitleObjectContent::TextBox(TextBox { text: "sub".into(), font_name: (*f).into(), ..Default::default() }), at.clone());
        }
        if let Some(p) = picture {
            td.add_object(TitleObjectContent::Image(ImageLayer { path: p.to_string_lossy().into_owned(), fit: Default::default() }), at);
        }
        td
    }

    /// #2757: a title template and a composition template with a picture
    /// and three kinds of font, packed and imported into clean folders:
    /// the same templates, the picture installed with its licence and
    /// the template pointing at it, the carried font installed, the
    /// catalogue one by name, a shipped recipe whole; imported again,
    /// kept unless replaced. A
    /// picture without a licence is refused, named.
    #[test]
    fn a_pack_round_trips_into_a_clean_template_folder() {
        let src = dir("src");
        let photo = src.join("photo.png");
        std::fs::write(&photo, b"PNG-BYTES").unwrap();
        let house = src.join("HouseSans.ttf");
        std::fs::write(&house, b"FONT-BYTES").unwrap();
        let card = TemplateFile { name: "Card".into(), category: "Mine".into(), data: title(&["Anton", "House Sans"], Some(&photo)), format: 1 };
        let mut scene = Sequence::new_default();
        scene.tracks[0].clips.push(Clip { kind: ClipKind::Title(title(&["Inter", "Nowhere Serif"], Some(&photo))), ..Clip::gap(klipp_core::RationalTime::from_secs_f64(0.0, 24), klipp_core::RationalTime::from_secs_f64(3.0, 24)) });
        let mut stack = klipp_timeline::composition_template::template_from_scene(&scene, &[], "Stack", None).unwrap();
        stack.category = "Mine".into();
        // A recipe as shipped: a graph, no composition.
        let recipe: CompositionTemplate = serde_json::from_str(include_str!("../../../assets/recipes/matted_glow.klipptemplate")).unwrap();
        let fonts = Fonts { house: Some(house.clone()), fonts: dir("fonts-src") };
        let out = src.join("mine.klipppack");

        let refused = export_pack(&out, "Mine", &PackAbout::default(), vec![card.clone()], vec![stack.clone(), recipe.clone()], &BTreeMap::new(), &fonts).unwrap_err();
        assert!(refused.contains("no licence for photo.png"), "{refused}");
        let licences: BTreeMap<String, String> = [("photo.png".to_string(), "CC0, the author".to_string())].into_iter().collect();
        let text = export_pack(&out, "Mine", &PackAbout::default(), vec![card.clone()], vec![stack.clone(), recipe.clone()], &licences, &fonts).unwrap();
        assert!(text.contains("3 templates, 1 picture") && text.contains("by name: Anton") && text.contains("licence: House Sans") && text.contains("left out") && text.contains("Nowhere Serif"), "{text}");

        // A clean machine: none of the source files, empty folders.
        let clean = dir("clean");
        let to = PackTargets { titles: clean.join("titles"), templates: clean.join("templates"), assets: clean.join("assets"), fonts: clean.join("fonts"), presets: clean.join("presets") };
        let there = Fonts { house: None, fonts: to.fonts.clone() };
        std::fs::remove_file(&photo).unwrap();
        let (name, report) = import_pack(&out, &to, &there, false).unwrap();
        assert_eq!(name, "Mine");
        assert_eq!(report.added, ["Card", "Stack", recipe.name.as_str()]);
        assert_eq!(report.fonts_added, ["House Sans"]);
        assert_eq!(report.fallbacks, ["font Nowhere Serif is not here: drawn in Inter"], "only the font that was left out");
        let installed = to.assets.join("mine").join("1-photo.png");
        assert_eq!(std::fs::read(&installed).unwrap(), b"PNG-BYTES");
        assert_eq!(std::fs::read_to_string(to.assets.join("mine").join("1-photo.png.licence.txt")).unwrap(), "CC0, the author");
        assert_eq!(std::fs::read(to.fonts.join("house-sans/housesans.ttf")).unwrap(), b"FONT-BYTES");
        assert!(to.fonts.join("house-sans/licence.txt").is_file());

        // The templates as made, the picture's path now the installed one.
        let titles = klipp_timeline::read_templates(&to.titles);
        assert_eq!(titles.len(), 1);
        let mut want = card.data.clone();
        if let TitleObjectContent::Image(img) = &mut want.objects[2].content {
            img.path = installed.to_string_lossy().into_owned();
        }
        assert_eq!(titles[0].data, want, "the title template as made, its picture repathed");
        let store = CompositionTemplateStore::load(Some(to.templates.clone()));
        let back = store.templates().find(|t| t.name == "Stack").expect("the composition template");
        assert_eq!(back.parameters, stack.parameters);
        assert_eq!(back.bindings, stack.bindings);
        let picture = back.sequences.iter().flat_map(|s| s.all_clips()).find_map(|c| c.title_data().and_then(|t| t.objects.iter().find_map(|o| match &o.content { TitleObjectContent::Image(i) => Some(i.path.clone()), _ => None }))).unwrap();
        assert_eq!(picture, installed.to_string_lossy());
        let r = store.recipes().find(|t| t.name == recipe.name).expect("the recipe");
        assert_eq!((r.kind, &r.graph, &r.bindings, &r.slots), (recipe.kind, &recipe.graph, &recipe.bindings, &recipe.slots), "the recipe as shipped");

        // Again: kept, unless replaced.
        let (_, again) = import_pack(&out, &to, &there, false).unwrap();
        assert_eq!(again.skipped, ["Card", "Stack", recipe.name.as_str()]);
        assert!(again.added.is_empty());
        let (_, replaced) = import_pack(&out, &to, &there, true).unwrap();
        assert_eq!(replaced.added, ["Card", "Stack", recipe.name.as_str()]);
        let after = CompositionTemplateStore::load(Some(to.templates.clone()));
        assert_eq!((after.templates().count(), after.recipes().count()), (1, 1), "replaced, not doubled");
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&clean);
    }

    /// A pack missing a picture it lists imports with the fallback named;
    /// one of a newer format is refused.
    #[test]
    fn a_pack_missing_a_picture_or_too_new_says_so() {
        use zip::write::{SimpleFileOptions, ZipWriter};
        let d = dir("broken");
        let write = |format: u32| {
            let path = d.join(format!("p{format}.klipppack"));
            let mut zip = ZipWriter::new(std::fs::File::create(&path).unwrap());
            let manifest = PackManifest { format, name: "Broken".into(), written_by: String::new(), about: PackAbout::default(), titles: vec![TemplateFile { name: "Card".into(), category: String::new(), data: title(&["Inter"], Some(Path::new("assets/pictures/1-gone.png"))), format: 1 }], templates: Vec::new(), fonts: Vec::new(), pictures: vec![PackPicture { file: "assets/pictures/1-gone.png".into(), licence: "assets/pictures/1-gone.png.licence.txt".into() }], presets: Vec::new() };
            zip.start_file("pack.json", SimpleFileOptions::default()).unwrap();
            zip.write_all(serde_json::to_string(&manifest).unwrap().as_bytes()).unwrap();
            zip.finish().unwrap();
            path
        };
        let to = PackTargets { titles: d.join("titles"), templates: d.join("templates"), assets: d.join("assets"), fonts: d.join("fonts"), presets: d.join("presets") };
        let fonts = Fonts { house: None, fonts: to.fonts.clone() };
        let (_, report) = import_pack(&write(1), &to, &fonts, false).unwrap();
        assert_eq!(report.added, ["Card"]);
        assert!(report.fallbacks[0].contains("1-gone.png is not in the pack"), "{:?}", report.fallbacks);
        assert!(report.text("Broken").contains("missing: picture"));
        assert!(import_pack(&write(9), &to, &fonts, false).unwrap_err().contains("newer Klipp"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A hostile pack (#2757): a name of dots, a font family and picture
    /// and font paths that climb out with "../". Everything lands inside
    /// the targets, under safe names, and nothing beside them; an entry
    /// past the cap is refused, named, before it is unpacked.
    #[test]
    fn a_hostile_pack_stays_inside_its_folders() {
        use zip::write::{SimpleFileOptions, ZipWriter};
        let sandbox = dir("hostile");
        let path = sandbox.join("evil.klipppack");
        let mut zip = ZipWriter::new(std::fs::File::create(&path).unwrap());
        let o = SimpleFileOptions::default();
        let evil_picture = "assets/pictures/../../../../evil.png";
        let evil_font = "assets/fonts/../../../evil.ttf";
        let manifest = PackManifest {
            format: 1,
            name: "..".into(),
            written_by: String::new(),
            about: PackAbout::default(),
            titles: vec![TemplateFile { name: "../../Card".into(), category: String::new(), data: title(&["../../x"], Some(Path::new(evil_picture))), format: 1 }],
            templates: Vec::new(),
            fonts: vec![PackFont { family: "../../x".into(), catalogue: false, files: vec![evil_font.into()], licence: Some("assets/fonts/../../LICENCE".into()) }],
            pictures: vec![PackPicture { file: evil_picture.into(), licence: "assets/pictures/../../../evil.txt".into() }],
            presets: Vec::new(),
        };
        for (name, bytes) in [("pack.json", serde_json::to_vec(&manifest).unwrap()), (evil_picture, b"PIC".to_vec()), (evil_font, b"FONT".to_vec()), ("assets/fonts/../../LICENCE", b"L".to_vec()), ("assets/pictures/../../../evil.txt", b"L".to_vec()), ("assets/pictures/big.png", vec![0u8; 3000])] {
            zip.start_file(name, o).unwrap();
            zip.write_all(&bytes).unwrap();
        }
        zip.finish().unwrap();
        let root = sandbox.join("targets");
        let to = PackTargets { titles: root.join("titles"), templates: root.join("templates"), assets: root.join("assets"), fonts: root.join("fonts"), presets: root.join("presets") };
        let fonts = Fonts { house: None, fonts: to.fonts.clone() };
        let (_, report) = import_pack(&path, &to, &fonts, false).unwrap();
        assert_eq!(report.fonts_added, ["../../x"]);
        // Everything written is under the targets, at safe names.
        let mut written = Vec::new();
        fn walk(d: &Path, out: &mut Vec<PathBuf>) {
            for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
                if e.path().is_dir() { walk(&e.path(), out) } else { out.push(e.path()) }
            }
        }
        walk(&sandbox, &mut written);
        let outside: Vec<&PathBuf> = written.iter().filter(|p| !p.starts_with(&root) && **p != path).collect();
        assert!(outside.is_empty(), "written outside the targets: {outside:?}");
        assert!(to.assets.join("item").join("evil.png").is_file(), "the pack's folder named safely: {written:?}");
        assert!(to.fonts.join("x").join("evil.ttf").is_file(), "{written:?}");
        assert!(written.iter().all(|p| p.components().all(|c| c.as_os_str() != "..")));

        // An entry past the cap is refused before it is unpacked.
        let big = PackManifest { pictures: vec![PackPicture { file: "assets/pictures/big.png".into(), licence: String::new() }], fonts: Vec::new(), titles: Vec::new(), ..manifest };
        let path = sandbox.join("big.klipppack");
        let mut zip = ZipWriter::new(std::fs::File::create(&path).unwrap());
        for (name, bytes) in [("pack.json", serde_json::to_vec(&big).unwrap()), ("assets/pictures/big.png", vec![0u8; 3000])] {
            zip.start_file(name, o).unwrap();
            zip.write_all(&bytes).unwrap();
        }
        zip.finish().unwrap();
        let err = import_pack_within(&path, &to, &fonts, false, 2048, 1 << 20).unwrap_err();
        assert!(err.contains("assets/pictures/big.png") && err.contains("more than a pack may hold"), "{err}");
        let err = import_pack_within(&path, &to, &fonts, false, 1 << 20, 2048).unwrap_err();
        assert!(err.contains("more than a pack may hold"), "the whole pack's cap too: {err}");
        let _ = std::fs::remove_dir_all(&sandbox);
    }

    /// #2757: a pack installs as a folder of its own under `packs/`, with
    /// what it says of itself; a second install of its name is refused
    /// and an update replaces it whole; a name in the user's own and in
    /// the pack is both, the user's found first unless the pack's is
    /// asked for; removing it takes its folder and nothing else.
    #[test]
    fn a_pack_installs_updates_and_removes_as_a_unit() {
        use klipp_timeline::template_places::{installed, mine_templates, mine_titles, packs, Source};
        let src = dir("src3");
        let photo = src.join("photo.png");
        std::fs::write(&photo, b"PNG").unwrap();
        let house = src.join("HouseSans.ttf");
        std::fs::write(&house, b"FONT").unwrap();
        let card = TemplateFile { name: "Card".into(), category: "Packed".into(), data: title(&["House Sans"], Some(&photo)), format: 1 };
        let mut scene = Sequence::new_default();
        scene.tracks[0].clips.push(Clip { kind: ClipKind::Title(title(&["Inter"], None)), ..Clip::gap(klipp_core::RationalTime::from_secs_f64(0.0, 24), klipp_core::RationalTime::from_secs_f64(3.0, 24)) });
        let stack = klipp_timeline::composition_template::template_from_scene(&scene, &[], "Stack", None).unwrap();
        let licences: BTreeMap<String, String> = [("photo.png".to_string(), "CC0".to_string())].into_iter().collect();
        let out = src.join("bold.klipppack");
        let about = PackAbout { author: "A. Maker".into(), version: "1.0".into(), licence: "CC-BY-4.0".into() };
        export_pack(&out, "Bold Cards", &about, vec![card.clone()], vec![stack.clone()], &licences, &Fonts { house: Some(house), fonts: dir("f-src") }).unwrap();

        // A data folder with the user's own Card and Stack, another pack,
        // and a file beside them, none of which the pack may touch.
        let root = dir("root3");
        klipp_timeline::save_template(&mine_titles(&root), "Card", "Mine", &title(&["Inter"], None), false).unwrap();
        let mut mine_stack = stack.clone();
        mine_stack.id = uuid::Uuid::new_v4();
        klipp_timeline::composition_template::CompositionTemplateStore::load(Some(mine_templates(&root))).save(mine_stack.clone()).unwrap();
        let other = packs(&root).join("other");
        std::fs::create_dir_all(other.join("templates")).unwrap();
        std::fs::write(other.join("pack.json"), r#"{"format":1,"name":"Other"}"#).unwrap();
        std::fs::write(root.join("keep.txt"), b"keep").unwrap();
        let fonts = Fonts { house: None, fonts: packs(&root).join("bold-cards").join("fonts") };

        let (pack, report) = install_pack(&out, &root, &fonts, false).unwrap();
        assert_eq!((pack.folder.as_str(), pack.name.as_str(), pack.titles, pack.templates), ("bold-cards", "Bold Cards", 1, 1));
        assert_eq!(pack.about, about);
        assert_eq!(report.fonts_added, ["House Sans"]);
        let folder = packs(&root).join("bold-cards");
        assert!(folder.join("fonts/house-sans/housesans.ttf").is_file(), "its fonts in its own folder");
        let titles = klipp_timeline::read_all_templates(&root);
        let packed = titles.iter().find(|(s, _)| *s == Source::Pack("bold-cards".into())).map(|(_, t)| t).expect("the pack's Card");
        let TitleObjectContent::Image(img) = &packed.data.objects.last().unwrap().content else { panic!() };
        assert!(std::path::Path::new(&img.path).starts_with(&folder) && std::path::Path::new(&img.path).is_file(), "its picture in its folder: {}", img.path);
        assert!(!installed(&root).iter().any(|(n, _)| n.starts_with('.')), "nothing left half-installed");

        // A name in two sources: both, the user's own first.
        assert_eq!(titles.iter().filter(|(_, t)| t.name == "Card").count(), 2);
        assert_eq!(klipp_timeline::find_template(&titles, "card", None).unwrap().category, "Mine");
        assert_eq!(klipp_timeline::find_template(&titles, "card", Some(&Source::Pack("bold-cards".into()))).unwrap().category, "Packed");
        let store = klipp_timeline::composition_template::CompositionTemplateStore::load(Some(mine_templates(&root))).with_packs(&root).with_builtins();
        assert_eq!(store.templates().filter(|t| t.name == "Stack").count(), 2);
        assert_eq!(store.find("Stack", None).unwrap().id, mine_stack.id, "the user's own first");
        let theirs = store.find("Stack", Some(&Source::Pack("bold-cards".into()))).unwrap();
        assert_eq!(store.source_of(theirs.id), Source::Pack("bold-cards".into()));

        // Installed again: refused; updated: replaced whole.
        assert!(install_pack(&out, &root, &fonts, false).unwrap_err().contains("installed already"));
        std::fs::write(folder.join("templates").join("left-behind.klipptemplate"), b"{}").unwrap();
        // When it was first installed is kept (#3095): set back, an update
        // leaves it there.
        let first = klipp_core::preset_places::installed_secs(&folder);
        assert!(first > 0, "an install says when");
        let json = std::fs::read_to_string(folder.join("pack.json")).unwrap().replace(&format!("\"installed_secs\": {first}"), "\"installed_secs\": 5");
        std::fs::write(folder.join("pack.json"), json).unwrap();
        install_pack(&out, &root, &fonts, true).unwrap();
        assert!(!folder.join("templates").join("left-behind.klipptemplate").exists(), "an update replaces the folder");
        assert_eq!(klipp_core::preset_places::installed_secs(&folder), 5, "an update keeps when the pack was first installed");

        // Removed: its folder, and nothing else.
        assert!(remove_pack(&root, "No Such Pack").unwrap_err().contains("there are \"Bold Cards\", \"Other\""));
        let gone = remove_pack(&root, "Bold Cards").unwrap();
        assert_eq!(gone.folder, "bold-cards");
        assert!(!folder.exists());
        assert!(other.join("pack.json").is_file() && std::fs::read(root.join("keep.txt")).unwrap() == b"keep");
        assert_eq!(klipp_timeline::read_templates(&mine_titles(&root)).len(), 1, "the user's own untouched");
        assert_eq!(klipp_timeline::composition_template::CompositionTemplateStore::load(Some(mine_templates(&root))).templates().count(), 1);
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod preset_pack_tests {
    use super::*;
    use std::io::Write;
    use zip::write::{SimpleFileOptions, ZipWriter};

    struct NoFonts;
    impl FontLookup for NoFonts {
        fn bundled(&self, _: &str) -> bool {
            true
        }
        fn in_catalogue(&self, _: &str) -> bool {
            false
        }
        fn carry(&self, _: &str) -> Option<(Vec<PathBuf>, String)> {
            None
        }
        fn available(&self, _: &str) -> bool {
            true
        }
        fn fallback(&self) -> String {
            "Noto Sans".into()
        }
    }

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("klipp-preset-pack-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A file of `kind`'s, with `body` after its head.
    fn preset(kind: &str, body: &str) -> String {
        let k = klipp_core::preset_kinds::kind_in(kind).unwrap();
        format!("kind = \"{}\"\nformat_version = {}\n{body}", k.kind, k.version)
    }

    /// One entry of each kind, each as that kind is written.
    fn one_of_each() -> Vec<(&'static str, String)> {
        vec![
            ("generators", preset("generators", "[[generator]]\nname = \"Pack Red\"\ncategory = \"Pack\"\ndata = { SolidColor = { color = [1, 2, 3, 255] } }\n")),
            ("project_formats", preset("project_formats", "[[format]]\nname = \"Pack 50p\"\nwidth = 1920\nheight = 1080\nrates = [[50, 1]]\n")),
            ("hdr_zones", preset("hdr_zones", "[[preset]]\nname = \"Pack lift\"\ndescription = \"d\"\nexposure = { shadow = 0.5 }\n")),
            ("delivery_templates", preset("delivery_templates", "[[template]]\nname = \"Pack 21:9\"\naspect = [21, 9]\nsuffix = \"21x9\"\npreset = \"00000000-0000-4000-a000-000000000001\"\n")),
            ("false_colour", preset("false_colour", "[[palette]]\nname = \"Pack palette\"\n[[palette.stop]]\nluma = [0, 255]\ncolor = [1, 2, 3]\nlabel = \"All\"\n")),
            ("animators", preset("animators", "[[preset]]\nid = \"pack_drift\"\nname = \"Pack drift\"\n")),
            ("audio_effects", preset("audio_effects", "[[preset]]\neffect = \"denoiser\"\nname = \"Pack hum\"\ndescription = \"d\"\nparams = { strength = 0.4 }\n")),
        ]
    }

    enum Entry {
        File(String, Vec<u8>),
        Link(String),
        /// Its header saying it unpacks to `.2` bytes, whatever it holds.
        Lying(String, Vec<u8>, u32),
    }

    /// A pack at `at` of `pack_json` (written as given, so it can say
    /// what Klipp never would) and `entries`.
    fn pack(at: &Path, pack_json: &str, entries: Vec<Entry>) {
        let mut zip = ZipWriter::new(std::fs::File::create(at).unwrap());
        let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("pack.json", stored).unwrap();
        zip.write_all(pack_json.as_bytes()).unwrap();
        let mut lies = Vec::new();
        for e in entries {
            match e {
                Entry::File(name, bytes) => {
                    zip.start_file(name, stored).unwrap();
                    zip.write_all(&bytes).unwrap();
                }
                Entry::Link(name) => zip.add_symlink(name, "/etc/passwd", stored).unwrap(),
                Entry::Lying(name, bytes, says) => {
                    zip.start_file(name.clone(), stored).unwrap();
                    zip.write_all(&bytes).unwrap();
                    lies.push((name, bytes.len() as u32, says));
                }
            }
        }
        zip.finish().unwrap();
        // The size a lying entry's headers say it unpacks to, set small:
        // in its local header (at 22 past its signature) and in the
        // central directory (at 24).
        let mut raw = std::fs::read(at).unwrap();
        for (name, real, says) in lies {
            for (sig, at_size) in [(&b"PK\x03\x04"[..], 22usize), (&b"PK\x01\x02"[..], 24usize)] {
                let mut i = 0;
                while let Some(off) = raw[i..].windows(4).position(|w| w == sig).map(|o| o + i) {
                    let name_at = if sig == b"PK\x03\x04" { off + 30 } else { off + 46 };
                    if raw.get(name_at..name_at + name.len()) == Some(name.as_bytes()) {
                        assert_eq!(u32::from_le_bytes(raw[off + at_size..off + at_size + 4].try_into().unwrap()), real);
                        raw[off + at_size..off + at_size + 4].copy_from_slice(&says.to_le_bytes());
                    }
                    i = off + 4;
                }
            }
        }
        std::fs::write(at, raw).unwrap();
    }

    fn manifest(format: u32, name: &str, presets: &[(&str, &str)], extra: &str) -> String {
        let presets: Vec<String> = presets.iter().map(|(k, f)| format!("{{\"kind\": \"{k}\", \"file\": \"{f}\"}}")).collect();
        format!("{{\"format\": {format}, \"name\": \"{name}\", \"presets\": [{}]{extra}}}", presets.join(", "))
    }

    /// A pack holding one preset of each kind installs them into its own
    /// folder, each kind reading them after Klipp's; one carrying
    /// `installed_secs` of its own is given Klipp's, not its; removed,
    /// it takes them with it.
    #[test]
    fn a_pack_of_one_preset_of_each_kind_installs_and_is_removed_whole() {
        let src = dir("each-src");
        let root = dir("each-root");
        let files = one_of_each();
        let listed: Vec<(String, String)> = files.iter().map(|(k, _)| (k.to_string(), format!("presets/{k}/pack.toml"))).collect();
        let refs: Vec<(&str, &str)> = listed.iter().map(|(k, f)| (k.as_str(), f.as_str())).collect();
        let at = src.join("each.klipppack");
        pack(&at, &manifest(2, "Each Kind", &refs, ", \"installed_secs\": 1"), files.iter().map(|(k, t)| Entry::File(format!("presets/{k}/pack.toml"), t.as_bytes().to_vec())).collect());
        let (installed, report) = install_pack(&at, &root, &NoFonts, false).unwrap();
        assert_eq!(report.presets_added.len(), 7, "{:?}", report.presets_added);
        let folder = klipp_timeline::template_places::packs(&root).join(&installed.folder);
        for (kind, text) in &files {
            assert_eq!(std::fs::read_to_string(folder.join("presets").join(kind).join("pack.toml")).unwrap(), *text);
            let dirs = klipp_core::preset_places::pack_dirs(&root, kind);
            assert_eq!(klipp_core::preset_kinds::added_in(kind, &dirs), 1, "{kind}: listed after Klipp's");
        }
        assert!(report.presets_skipped.is_empty(), "{:?}", report.presets_skipped);
        let secs = klipp_core::preset_places::installed_secs(&folder);
        assert!(secs > 1_000_000_000, "Klipp's own time, not the pack's 1: {secs}");
        remove_pack(&root, "Each Kind").unwrap();
        assert!(!folder.exists());
        for (kind, _) in &files {
            assert_eq!(klipp_core::preset_kinds::added_in(kind, &klipp_core::preset_places::pack_dirs(&root, kind)), 0, "{kind}: gone with it");
        }
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A pack whose presets someone made to do harm is refused whole,
    /// saying why, and nothing of it is written, inside the packs or out:
    /// a path that climbs out or is absolute, a kind Klipp hasn't got, a
    /// file under another kind's folder, a hidden name or one of other
    /// characters, a link, a file past the size, one whose header lies
    /// about how much it unpacks to, too many of a kind or in all, a
    /// newer pack or preset format, a file of another kind, one that
    /// isn't text, one the pack names but hasn't got, one named twice.
    #[test]
    fn a_hostile_pack_s_presets_are_refused_and_nothing_is_written() {
        let src = dir("hostile-src");
        let root = dir("hostile-root");
        let good = one_of_each()[0].1.clone();
        let big = format!("{good}# {}\n", "x".repeat(klipp_core::preset_file::MAX_FILE_BYTES));
        let many = |n: usize, kinds: &[&str]| -> (Vec<(String, String)>, Vec<Entry>) {
            let mut listed = Vec::new();
            let mut entries = Vec::new();
            for kind in kinds {
                let text = one_of_each().into_iter().find(|(k, _)| k == kind).unwrap().1;
                for i in 0..n {
                    let f = format!("presets/{kind}/p{i}.toml");
                    listed.push((kind.to_string(), f.clone()));
                    entries.push(Entry::File(f, text.as_bytes().to_vec()));
                }
            }
            (listed, entries)
        };
        let one = |kind: &str, file: &str, bytes: Vec<u8>| (vec![(kind.to_string(), file.to_string())], vec![Entry::File(file.to_string(), bytes)]);
        let mut cases: Vec<(&str, u32, (Vec<(String, String)>, Vec<Entry>), &str)> = vec![
            ("climbs", 2, one("generators", "presets/../../x.toml", good.clone().into_bytes()), "a path of presets/<kind>/<name>.toml"),
            ("absolute", 2, one("generators", "/etc/x.toml", good.clone().into_bytes()), "a path of presets/generators/<name>.toml"),
            ("unknown kind", 2, one("shaders", "presets/shaders/x.toml", good.clone().into_bytes()), "\"shaders\" is not a kind of preset"),
            ("other folder", 2, one("generators", "presets/hdr_zones/x.toml", good.clone().into_bytes()), "a path of presets/generators/<name>.toml"),
            ("hidden", 2, one("generators", "presets/generators/.x.toml", good.clone().into_bytes()), "not hidden"),
            ("odd name", 2, one("generators", "presets/generators/a b.toml", good.clone().into_bytes()), "a name of letters"),
            ("backslash", 2, one("generators", "presets\\\\generators\\\\x.toml", good.clone().into_bytes()), "a path of / and plain names"),
            ("too big", 2, one("generators", "presets/generators/x.toml", big.clone().into_bytes()), "more than a pack may hold"),
            ("newer pack", 3, one("generators", "presets/generators/x.toml", good.clone().into_bytes()), "made by a newer Klipp"),
            ("newer preset", 2, one("generators", "presets/generators/x.toml", good.replace("format_version = 1", "format_version = 99").into_bytes()), "made by a newer Klipp"),
            ("other kind", 2, one("generators", "presets/generators/x.toml", one_of_each()[2].1.clone().into_bytes()), "a hdr_zones_preset file, not a generator_entry file"),
            ("not text", 2, one("generators", "presets/generators/x.toml", vec![0xff, 0xfe, 0x00]), "is not text"),
            ("of a kind", 2, many(65, &["generators"]), "a pack carries at most 64 of a kind"),
            ("in all", 2, many(60, &["generators", "hdr_zones", "animators", "false_colour", "audio_effects"]), "a pack carries at most 256"),
        ];
        cases.push(("missing", 2, (vec![("generators".into(), "presets/generators/x.toml".into())], Vec::new()), "is not in the pack"));
        cases.push(("twice", 2, (vec![("generators".into(), "presets/generators/x.toml".into()), ("generators".into(), "presets/generators/x.toml".into())], vec![Entry::File("presets/generators/x.toml".into(), good.clone().into_bytes())]), "is named twice"));
        cases.push(("link", 2, (vec![("generators".into(), "presets/generators/x.toml".into())], vec![Entry::Link("presets/generators/x.toml".into())]), "is not a plain file"));
        cases.push(("lying header", 2, (vec![("generators".into(), "presets/generators/x.toml".into())], vec![Entry::Lying("presets/generators/x.toml".into(), big.clone().into_bytes(), 100)]), "unpacks past what a pack may hold"));
        for (what, format, (listed, entries), says) in cases {
            let at = src.join("hostile.klipppack");
            let refs: Vec<(&str, &str)> = listed.iter().map(|(k, f)| (k.as_str(), f.as_str())).collect();
            pack(&at, &manifest(format, "Hostile", &refs, ""), entries);
            let why = install_pack(&at, &root, &NoFonts, false).err().unwrap_or_else(|| panic!("{what}: installed"));
            assert!(why.contains(says), "{what}: {why}");
            let left: Vec<_> = std::fs::read_dir(klipp_timeline::template_places::packs(&root)).into_iter().flatten().flatten().map(|e| e.file_name()).collect();
            assert!(left.is_empty(), "{what}: left {left:?}");
        }
        let anywhere = |d: &Path| std::fs::read_dir(d).into_iter().flatten().flatten().any(|e| e.file_name() == "x.toml");
        assert!(!anywhere(&root) && !anywhere(&src) && !anywhere(&std::env::temp_dir()), "nothing written outside");
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// What of a pack's presets won't be offered is said in its install's
    /// report, read beside Klipp's and the packs installed before it: a
    /// name an earlier pack has (that pack keeps it, whatever the later
    /// one says of when it was installed), a built-in's name, a field
    /// the kind doesn't know, a value past what the kind takes.
    #[test]
    fn a_pack_s_presets_are_said_beside_klipp_s_and_earlier_packs() {
        let src = dir("beside-src");
        let root = dir("beside-root");
        let shared = |colour: u8| preset("generators", &format!("[[generator]]\nname = \"Shared\"\ncategory = \"Pack\"\ndata = {{ SolidColor = {{ color = [{colour}, 0, 0, 255] }} }}\n"));
        let first = src.join("first.klipppack");
        pack(&first, &manifest(2, "First", &[("generators", "presets/generators/a.toml")], ""), vec![Entry::File("presets/generators/a.toml".into(), shared(1).into_bytes())]);
        install_pack(&first, &root, &NoFonts, false).unwrap();
        // Installed a second later, saying it was installed at 1.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let second = src.join("second.klipppack");
        let generators = shared(2) + "[[generator]]\nname = \"countdown\"\ncategory = \"Pack\"\ndata = \"Countdown\"\n";
        let animators = preset("animators", "[[preset]]\nid = \"huge\"\nname = \"Huge\"\nprops = { scale = 1e30 }\n[[preset]]\nid = \"odd\"\nname = \"Odd\"\nsparkle = 1\n");
        pack(&second, &manifest(2, "Second", &[("generators", "presets/generators/b.toml"), ("animators", "presets/animators/b.toml")], ", \"installed_secs\": 1"), vec![Entry::File("presets/generators/b.toml".into(), generators.into_bytes()), Entry::File("presets/animators/b.toml".into(), animators.into_bytes())]);
        let (_, report) = install_pack(&second, &root, &NoFonts, false).unwrap();
        let said = |what: &str| assert!(report.presets_skipped.iter().any(|s| s.contains(what)), "{what}: {:?}", report.presets_skipped);
        said("generators: \"Shared\" is the name of one read before it");
        said("generators: \"countdown\" is the name of one of Klipp's own");
        said("animators: b.toml: animator_preset entry 2: unknown field `sparkle`");
        assert!(report.text("Second").contains("presets not offered"));
        let listing = klipp_core::generator_entries::generator_listing_in(&klipp_core::preset_places::pack_dirs(&root, "generators"));
        let kept: Vec<&klipp_core::GeneratorData> = listing.entries.iter().filter(|e| e.name == "Shared").map(|e| &e.data).collect();
        assert_eq!(kept, [&klipp_core::GeneratorData::SolidColor { color: [1, 0, 0, 255] }], "the first pack's, its own time or no");
        // The value past what an animator takes, in a file that reads.
        let (_, report) = {
            let third = src.join("third.klipppack");
            pack(&third, &manifest(2, "Third", &[("animators", "presets/animators/c.toml")], ""), vec![Entry::File("presets/animators/c.toml".into(), preset("animators", "[[preset]]\nid = \"huge\"\nname = \"Huge\"\nprops = { scale = 1e30 }\n").into_bytes())]);
            install_pack(&third, &root, &NoFonts, false).unwrap()
        };
        assert!(report.presets_skipped.iter().any(|s| s.contains("\"huge\": scale") && s.contains("is outside 0 to 20")), "{:?}", report.presets_skipped);
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A person's presets go into a pack as they wrote them, a file a
    /// kind, and the pack is format 2; one without presets stays format
    /// 1. A name the person has none of is refused, with what they have.
    #[test]
    fn a_person_s_presets_are_packed_as_they_wrote_them() {
        let src = dir("export-src");
        let files = vec![PackPresetFile { kind: "generators".into(), file_name: "mine.toml".into(), text: one_of_each()[0].1.clone() }];
        let out = src.join("mine.klipppack");
        let text = export_pack_with(&out, "Mine", &PackAbout::default(), Vec::new(), Vec::new(), &files, &BTreeMap::new(), &NoFonts).unwrap();
        assert!(text.contains("preset files: generators/mine.toml"), "{text}");
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
        let mut left = MAX_PACK_BYTES;
        let head: PackManifest = serde_json::from_slice(&read_entry(&mut zip, "pack.json", MAX_ENTRY_BYTES, &mut left).unwrap().unwrap()).unwrap();
        assert_eq!((head.format, head.presets), (2, vec![PackPreset { kind: "generators".into(), file: "presets/generators/mine.toml".into() }]));
        assert!(presets_for_pack(&[("generators".into(), "No Such Generator 3095".into())], "x").unwrap_err().contains("no generators of yours called \"No Such Generator 3095\""));
        assert!(presets_for_pack(&[("shaders".into(), "x".into())], "x").unwrap_err().contains("not a kind of preset"));
        let _ = std::fs::remove_dir_all(&src);
    }
}
