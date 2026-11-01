//! The font catalogue (#2732): open-licence families Klipp does not ship
//! but fetches on request, so a title or a template can ask for a
//! condensed news sans or an engraved Roman serif and get one on every
//! machine. `assets/fonts/catalogue.toml` names each family, its files by
//! address, size and SHA-256, its licence, and tags for what it is
//! (`style`), what it suits (`mood`) and the writing it covers
//! (`scripts`). A fetched family lives in the user's data folder, one
//! folder each, its licence beside it; the fonts that ship with the app
//! need none of this and work offline.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;

use serde::Deserialize;

use crate::font_discovery::FontInfo;

/// One font file of a family.
#[derive(Debug, Clone, Deserialize)]
pub struct CatalogueFile {
    pub name: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

/// One family of the catalogue.
#[derive(Debug, Clone, Deserialize)]
pub struct CatalogueFamily {
    pub name: String,
    pub style: Vec<String>,
    pub mood: Vec<String>,
    #[serde(default = "latin")]
    pub scripts: Vec<String>,
    pub licence: String,
    pub licence_url: String,
    pub licence_sha256: String,
    pub licence_size: u64,
    #[serde(rename = "file")]
    pub files: Vec<CatalogueFile>,
}

fn latin() -> Vec<String> {
    vec!["latin".into()]
}

#[derive(Deserialize)]
struct Catalogue {
    family: Vec<CatalogueFamily>,
}

/// Every family of the catalogue, in its order.
pub fn families() -> &'static [CatalogueFamily] {
    static CATALOGUE: OnceLock<Vec<CatalogueFamily>> = OnceLock::new();
    CATALOGUE.get_or_init(|| {
        toml::from_str::<Catalogue>(include_str!("../../../assets/fonts/catalogue.toml")).map(|c| c.family).unwrap_or_else(|e| {
            tracing::error!("the font catalogue does not read: {e}");
            Vec::new()
        })
    })
}

/// The family named `name`, in any case.
pub fn family(name: &str) -> Option<&'static CatalogueFamily> {
    families().iter().find(|f| f.name.eq_ignore_ascii_case(name.trim()))
}

impl CatalogueFamily {
    /// Its tags: what it is, what it suits, the writing it covers.
    pub fn tags(&self) -> impl Iterator<Item = &str> {
        self.style.iter().chain(&self.mood).chain(&self.scripts).map(String::as_str)
    }

    /// Whether every word of `query` is in its name or one of its tags
    /// ("condensed news sans", "japanese", "wedding script").
    pub fn matches(&self, query: &str) -> bool {
        let name = self.name.to_lowercase();
        query.split_whitespace().map(str::to_lowercase).all(|w| name.contains(&w) || self.tags().any(|t| tag_fits(t, &w)))
    }

    /// Its tags a word of `query` names, in its own order (#3011).
    pub fn matched_tags(&self, query: &str) -> Vec<&str> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        self.tags().filter(|t| words.iter().any(|w| tag_fits(t, w))).collect()
    }

    /// Its folder under `root`.
    fn dir_in(&self, root: &Path) -> PathBuf {
        root.join(self.name.to_lowercase().replace(' ', "-"))
    }

    /// The licence's file name, as the source calls it (OFL.txt, LICENSE.txt).
    fn licence_file(&self) -> &str {
        self.licence_url.rsplit('/').next().unwrap_or("LICENSE.txt")
    }
}

/// Whether the tag `t` is the word `w`, or begins with it ("cond" finds
/// condensed).
fn tag_fits(t: &str, w: &str) -> bool {
    t == w || t.starts_with(w)
}

/// How the family named `name`, on this machine or in the catalogue, is
/// found by a search for `query` (#3011): by its name (the name holds the
/// whole query, any case), `Some` of no tags; else, a catalogue family,
/// by its tags, `Some` of the tags that matched; else `None`. A name
/// match lists first.
pub fn search(name: &str, query: &str) -> Option<Vec<&'static str>> {
    let q = query.trim().to_lowercase();
    if q.is_empty() || name.to_lowercase().contains(&q) {
        return Some(Vec::new());
    }
    family(name).filter(|f| f.matches(&q)).map(|f| f.matched_tags(&q))
}

/// The families that match `query` (see [`CatalogueFamily::matches`]),
/// in the catalogue's order.
pub fn query(query: &str) -> Vec<&'static CatalogueFamily> {
    families().iter().filter(|f| f.matches(query)).collect()
}

/// Where fetched families are kept: `KLIPP_FONTS_DIR` when set (tests),
/// else `klipp/fonts` in the user's data folder.
pub fn fonts_dir() -> PathBuf {
    std::env::var_os("KLIPP_FONTS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("klipp").join("fonts"))
}

/// Whether `family` is fetched under `root`: every file there. A fetch
/// writes each to a partial file and renames it only once its hash is
/// checked, so a file there is whole.
pub fn is_fetched_in(root: &Path, family: &CatalogueFamily) -> bool {
    let dir = family.dir_in(root);
    family.files.iter().all(|f| dir.join(&f.name).is_file())
}

/// Whether `family` is fetched (see [`fonts_dir`]).
pub fn is_fetched(family: &CatalogueFamily) -> bool {
    is_fetched_in(&fonts_dir(), family)
}

/// Whether a family of this name is in the catalogue and fetched.
pub fn fetched(name: &str) -> bool {
    family(name).is_some_and(is_fetched)
}

/// How many fetches have finished in this process: a renderer that has
/// seen fewer drops what it drew while a face was missing, and the app
/// draws its titles again (#2734).
static FETCHES: AtomicU64 = AtomicU64::new(0);

/// The fetches finished so far, for `FETCHES`'s readers.
pub fn fetches() -> u64 {
    FETCHES.load(Ordering::Acquire)
}

/// Counts a fetch finished.
pub(crate) fn fetched_one() {
    FETCHES.fetch_add(1, Ordering::Release);
}

/// Fetch `family` into its folder under `root`: each file and its licence
/// from its address, checked against its size and SHA-256 before it is
/// kept, a file already there at its size left. Returns its folder.
///
/// A finished fetch is counted for the whole process ([`fetches`]), and
/// every renderer in it drops what it has drawn: a test that finishes one
/// belongs in a test binary of its own (`tests/catalogue_fetch_checks.rs`,
/// `tests/fetched_face_redraw.rs`), or it makes the renderers of the tests
/// beside it draw again (#3262).
pub fn fetch_in(root: &Path, family: &CatalogueFamily, cancel: &AtomicBool) -> Result<PathBuf, String> {
    let dir = family.dir_in(root);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let licence = CatalogueFile { name: family.licence_file().to_string(), url: family.licence_url.clone(), sha256: family.licence_sha256.clone(), size: family.licence_size };
    for file in family.files.iter().chain(std::iter::once(&licence)) {
        let target = dir.join(&file.name);
        if std::fs::metadata(&target).is_ok_and(|m| m.len() == file.size) {
            continue;
        }
        let response = ureq::get(&file.url).call().map_err(|e| format!("{} {}: {e}", family.name, file.name))?;
        let mut body = response.into_reader();
        let partial = dir.join(format!("{}.partial", file.name));
        klipp_ml::download::write_and_verify(&mut body, &file.sha256, file.size, &partial, &target, &mut klipp_ml::download::NoOpProgress, cancel, &file.name)
            .map_err(|e| format!("{} {}: {e}", family.name, file.name))?;
    }
    fetched_one();
    Ok(dir)
}

/// Fetch `family` into [`fonts_dir`].
pub fn fetch(family: &CatalogueFamily) -> Result<PathBuf, String> {
    fetch_in(&fonts_dir(), family, &AtomicBool::new(false))
}

/// The faces of the fetched family `name` under `root`, as the font scan
/// lists a family's faces; `None` when it is not in the catalogue or not
/// fetched. What a title asks for by a catalogue name is loaded from
/// these when no font installed on the machine has the name.
pub fn fetched_faces_in(root: &Path, name: &str) -> Option<Vec<FontInfo>> {
    let family = family(name)?;
    if !is_fetched_in(root, family) {
        return None;
    }
    let faces: Vec<FontInfo> = crate::font_discovery::scan_folder(&family.dir_in(root)).into_values().flatten().collect();
    (!faces.is_empty()).then_some(faces)
}

/// [`fetched_faces_in`] under [`fonts_dir`].
pub fn fetched_faces(name: &str) -> Option<Vec<FontInfo>> {
    fetched_faces_in(&fonts_dir(), name)
}

/// The faces an installed template pack carried in for `name` (#2757):
/// in any pack's `fonts/<family>` folder under `root`, Klipp's data folder
/// ([`klipp_timeline::template_places::data_root`]).
pub fn pack_faces_in(root: &Path, name: &str) -> Option<Vec<FontInfo>> {
    let family = klipp_core::safe_name(name);
    let faces: Vec<FontInfo> = klipp_timeline::template_places::installed(root)
        .into_iter()
        .flat_map(|(_, pack)| crate::font_discovery::scan_folder(&pack.join("fonts").join(&family)).into_values().flatten())
        .collect();
    (!faces.is_empty()).then_some(faces)
}

/// A family's faces that are not installed on the machine: fetched from
/// the catalogue, else carried in by an installed template pack (#2757),
/// else carried by the open project (#3009).
pub fn extra_faces(name: &str) -> Option<Vec<FontInfo>> {
    fetched_faces(name).or_else(|| klipp_timeline::template_places::data_root().and_then(|root| pack_faces_in(&root, name))).or_else(|| project_faces(name))
}

/// The open project's own font files by family (#3009), as its font list
/// names them once checked.
static PROJECT_FONTS: std::sync::RwLock<Vec<(String, Vec<PathBuf>)>> = std::sync::RwLock::new(Vec::new());

/// The open project's fonts, each family with its files: drawn where the
/// machine lacks the family. Empty for a project that carries none.
pub fn set_project_fonts(fonts: Vec<(String, Vec<PathBuf>)>) {
    *PROJECT_FONTS.write().unwrap_or_else(|e| e.into_inner()) = fonts;
    fetched_one();
}

/// The faces of `name` the open project carries.
pub fn project_faces(name: &str) -> Option<Vec<FontInfo>> {
    let fonts = PROJECT_FONTS.read().unwrap_or_else(|e| e.into_inner());
    let (_, files) = fonts.iter().find(|(f, _)| f.eq_ignore_ascii_case(name.trim()))?;
    let mut dirs: Vec<&Path> = files.iter().filter_map(|f| f.parent()).collect();
    dirs.dedup();
    let faces: Vec<FontInfo> = dirs
        .into_iter()
        .flat_map(|d| crate::font_discovery::scan_folder(d).into_values().flatten())
        .filter(|face| files.contains(&face.path))
        .collect();
    (!faces.is_empty()).then_some(faces)
}

/// Says fonts were added beside the catalogue's (a pack's, #2757), so
/// renderers draw again what they drew while a face was missing.
pub fn note_fonts_added() {
    fetched_one();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The catalogue as shipped: every family named once, tagged, with its
    /// licence, every file by an address pinned to one commit, with a size
    /// and a SHA-256; the writing systems the issue asks for covered.
    #[test]
    fn the_catalogue_names_hashes_and_tags_every_family() {
        let all = families();
        assert!(all.len() >= 70, "{} families", all.len());
        let mut names: Vec<String> = all.iter().map(|f| f.name.to_lowercase()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), all.len(), "each family named once");
        for f in all {
            assert!(!f.style.is_empty() && !f.mood.is_empty() && !f.scripts.is_empty(), "{}: tagged", f.name);
            assert!(f.licence == "OFL-1.1" || f.licence == "Apache-2.0", "{}: an open licence", f.name);
            assert!(f.licence_size > 0 && f.licence_sha256.len() == 64, "{}: its licence text", f.name);
            assert!(!f.files.is_empty(), "{}: a file", f.name);
            for file in f.files.iter().map(|x| &x.url).chain([&f.licence_url]) {
                assert!(file.starts_with("https://raw.githubusercontent.com/google/fonts/") && !file.contains("/main/"), "{}: pinned, {file}", f.name);
            }
            for file in &f.files {
                assert!(file.size > 0 && file.sha256.len() == 64 && file.sha256.bytes().all(|b| b.is_ascii_hexdigit()), "{} {}", f.name, file.name);
            }
        }
        // Three or more families for each major writing system (#3010).
        for (script, _) in SAMPLES {
            let n = all.iter().filter(|f| f.scripts.iter().any(|s| s == script)).count();
            assert!(n >= 3, "{script}: {n} families");
        }
        for style in ["serif", "display", "condensed", "rounded", "script", "handwritten", "mono"] {
            assert!(!query(style).is_empty(), "has a {style}");
        }
    }

    #[test]
    fn a_query_finds_families_by_what_they_are() {
        let names = |q: &str| query(q).iter().map(|f| f.name.clone()).collect::<Vec<_>>();
        let news = names("condensed news sans");
        assert!(news.contains(&"Oswald".to_string()) && news.contains(&"Roboto Condensed".to_string()), "{news:?}");
        assert!(news.iter().all(|n| !n.contains("Garamond")), "{news:?}");
        let japanese = names("japanese");
        assert!(japanese.contains(&"Noto Sans JP".to_string()) && japanese.len() >= 3, "{japanese:?}");
        assert_eq!(names("japanese serif"), ["Noto Serif JP"], "every word a tag");
        assert!(names("wedding script").contains(&"Great Vibes".to_string()));
        assert_eq!(names("cinzel"), ["Cinzel"], "by name too");
        assert!(family("playfair display").is_some() && family("No Such Face").is_none());
    }

    /// Words in each writing system the catalogue covers, to check a
    /// family tagged with it has their glyphs (#3010).
    const SAMPLES: &[(&str, &str)] = &[
        ("latin", "Hamburgefonstiv"),
        ("cyrillic", "Съешь же ещё"),
        ("greek", "Γειά σου κόσμε"),
        ("arabic", "مرحبا بالعالم"),
        ("hebrew", "שלום עולם"),
        ("devanagari", "नमस्ते दुनिया"),
        ("bengali", "আমার সোনার বাংলা"),
        ("tamil", "வணக்கம் உலகம்"),
        ("thai", "สวัสดีชาวโลก"),
        ("chinese", "你好世界电影"),
        ("japanese", "こんにちは世界"),
        ("korean", "안녕하세요 세계"),
    ];

    /// #3010, online: with `KLIPP_FONT_CATALOGUE_ONLINE` naming a folder
    /// (kept between runs), every family is fetched as the app fetches it,
    /// every file matches its SHA-256, each family has the glyphs of every
    /// writing system it is tagged with, and a title in each writing
    /// system is drawn in a catalogue family for it.
    #[test]
    fn every_family_downloads_covers_its_scripts_and_draws() {
        let Some(root) = std::env::var_os("KLIPP_FONT_CATALOGUE_ONLINE").map(std::path::PathBuf::from) else {
            eprintln!("skipping: KLIPP_FONT_CATALOGUE_ONLINE not set");
            return;
        };
        let mut wrong = Vec::new();
        for f in families() {
            let dir = match fetch_in(&root, f, &AtomicBool::new(false)) {
                Ok(dir) => dir,
                Err(e) => {
                    wrong.push(format!("{}: {e}", f.name));
                    continue;
                }
            };
            let faces: Vec<Vec<u8>> = f.files.iter().map(|x| std::fs::read(dir.join(&x.name)).unwrap()).collect();
            for (x, bytes) in f.files.iter().zip(&faces) {
                if sha(bytes) != x.sha256 {
                    wrong.push(format!("{} {}: SHA-256 differs", f.name, x.name));
                }
            }
            for script in &f.scripts {
                let Some((_, sample)) = SAMPLES.iter().find(|(s, _)| s == script) else {
                    wrong.push(format!("{}: no sample for \"{script}\"", f.name));
                    continue;
                };
                let missing: String = sample.chars().filter(|c| !c.is_whitespace()).filter(|&c| !faces.iter().any(|b| ttf_parser::Face::parse(b, 0).is_ok_and(|face| face.glyph_index(c).is_some()))).collect();
                if !missing.is_empty() {
                    wrong.push(format!("{}: tagged {script} but has no glyph for {missing}", f.name));
                }
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
        std::env::set_var("KLIPP_FONTS_DIR", &root);
        for (script, sample) in SAMPLES {
            let f = families().iter().find(|f| f.scripts.iter().any(|s| s == script)).expect("a family for each");
            let mut r = crate::TitleRenderer::new();
            if !r.font_registry.contains_key(&f.name) {
                let key = r.ensure_font_loaded(&f.name, klipp_timeline::FontStyle::Regular);
                assert!(r.font_cache.contains_key(&key), "{script}: {} loads from the catalogue's folder", f.name);
            }
            let td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: (*sample).into(), font_name: f.name.clone(), font_size: 120.0, color: [255, 255, 255, 255], ..Default::default() });
            let ink = r.render(&td, 640, 360).chunks_exact(4).filter(|p| p[3] > 128).count();
            assert!(ink > 500, "{script}: a title in {} draws ({ink} pixels)", f.name);
        }
    }

    fn sha(bytes: &[u8]) -> String {
        use sha2::Digest;
        hex::encode(sha2::Sha256::digest(bytes))
    }

    /// A title set in a fetched catalogue family is drawn in it: the
    /// renderer finds its faces in the catalogue's folder when no font
    /// installed on the machine has the name (Great Vibes is not).
    #[test]
    fn a_fetched_family_loads_where_none_is_installed() {
        let root = std::env::temp_dir().join(format!("klipp-font-catalogue-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let gv = family("Great Vibes").unwrap();
        let dir = gv.dir_in(&root);
        std::fs::create_dir_all(&dir).unwrap();
        // A real face stands in for the fetched file.
        std::fs::write(dir.join(&gv.files[0].name), crate::bundled::BUNDLED[0].bytes).unwrap();
        assert!(fetched_faces_in(&root, "Great Vibes").is_some_and(|f| !f.is_empty()));
        assert!(fetched_faces_in(&root, "Cinzel").is_none(), "not fetched");
        std::env::set_var("KLIPP_FONTS_DIR", &root);
        let mut r = crate::TitleRenderer::new();
        if r.font_registry.contains_key("Great Vibes") {
            eprintln!("Great Vibes is installed here: the installed face is used");
        } else {
            let key = r.ensure_font_loaded("Great Vibes", klipp_timeline::FontStyle::Regular);
            assert!(r.font_cache.contains_key(&key), "loaded from the catalogue's folder");
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
