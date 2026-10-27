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
        query.split_whitespace().map(str::to_lowercase).all(|w| name.contains(&w) || self.tags().any(|t| t == w || t.starts_with(&w)))
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

/// The fetches finished so far, for [`FETCHES`]'s readers.
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

/// Where a template pack puts a font it carries (#2757): `packs/<family>`
/// under `root`, beside the fetched catalogue families.
pub fn pack_font_dir_in(root: &Path, family: &str) -> PathBuf {
    root.join("packs").join(family.trim().to_lowercase().replace(' ', "-"))
}

/// The faces a template pack carried in for `name` under `root`.
pub fn pack_faces_in(root: &Path, name: &str) -> Option<Vec<FontInfo>> {
    let faces: Vec<FontInfo> = crate::font_discovery::scan_folder(&pack_font_dir_in(root, name)).into_values().flatten().collect();
    (!faces.is_empty()).then_some(faces)
}

/// A family's faces that are not installed on the machine: fetched from
/// the catalogue, else carried in by a template pack (#2757).
pub fn extra_faces(name: &str) -> Option<Vec<FontInfo>> {
    fetched_faces(name).or_else(|| pack_faces_in(&fonts_dir(), name))
}

/// Says fonts were added beside the catalogue's (a pack's, #2757), so
/// renderers draw again what they drew while a face was missing.
pub fn note_fonts_added() {
    fetched_one();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// The catalogue as shipped: every family named once, tagged, with its
    /// licence, every file by an address pinned to one commit, with a size
    /// and a SHA-256; the writing systems the issue asks for covered.
    #[test]
    fn the_catalogue_names_hashes_and_tags_every_family() {
        let all = families();
        assert!(all.len() >= 30, "{} families", all.len());
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
        for script in ["japanese", "korean", "chinese", "arabic", "hebrew", "devanagari"] {
            assert!(!query(script).is_empty(), "covers {script}");
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
        assert_eq!(names("japanese"), ["Noto Sans JP"]);
        assert!(names("wedding script").contains(&"Great Vibes".to_string()));
        assert_eq!(names("cinzel"), ["Cinzel"], "by name too");
        assert!(family("playfair display").is_some() && family("No Such Face").is_none());
    }

    /// Serves `body` once per request on a local port.
    fn serve(body: Vec<u8>, requests: usize) -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for _ in 0..requests {
                let Ok((mut s, _)) = listener.accept() else { break };
                let _ = s.read(&mut [0u8; 4096]);
                let _ = s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes());
                let _ = s.write_all(&body);
            }
        });
        port
    }

    fn sha(bytes: &[u8]) -> String {
        use sha2::Digest;
        hex::encode(sha2::Sha256::digest(bytes))
    }

    /// A fetch keeps a file only when its hash is the catalogue's, the
    /// licence beside the font; a file that does not match is refused and
    /// nothing is kept.
    #[test]
    fn a_fetch_checks_every_file_and_keeps_its_licence() {
        let font = crate::bundled::BUNDLED[0].bytes.to_vec();
        let licence = b"SIL Open Font License 1.1".to_vec();
        let family = |font_url: String, licence_url: String, font_sha: String| CatalogueFamily {
            name: "Test Face".into(),
            style: vec!["sans".into()],
            mood: vec!["test".into()],
            scripts: vec!["latin".into()],
            licence: "OFL-1.1".into(),
            licence_url,
            licence_sha256: sha(&licence),
            licence_size: licence.len() as u64,
            files: vec![CatalogueFile { name: "TestFace.ttf".into(), url: font_url, sha256: font_sha, size: font.len() as u64 }],
        };
        let root = std::env::temp_dir().join(format!("klipp-font-fetch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (fp, lp) = (serve(font.clone(), 1), serve(licence.clone(), 1));
        let good = family(format!("http://127.0.0.1:{fp}/TestFace.ttf"), format!("http://127.0.0.1:{lp}/OFL.txt"), sha(&font));
        assert!(!is_fetched_in(&root, &good));
        let dir = fetch_in(&root, &good, &AtomicBool::new(false)).unwrap();
        assert!(is_fetched_in(&root, &good));
        assert_eq!(std::fs::read(dir.join("TestFace.ttf")).unwrap(), font);
        assert_eq!(std::fs::read(dir.join("OFL.txt")).unwrap(), licence, "the licence beside it");

        let other = root.join("other");
        let (fp, lp) = (serve(font.clone(), 1), serve(licence.clone(), 1));
        let bad = family(format!("http://127.0.0.1:{fp}/TestFace.ttf"), format!("http://127.0.0.1:{lp}/OFL.txt"), "0".repeat(64));
        assert!(fetch_in(&other, &bad, &AtomicBool::new(false)).is_err(), "a file that does not match is refused");
        assert!(!is_fetched_in(&other, &bad) && !bad.dir_in(&other).join("TestFace.ttf").exists(), "and not kept");
        let _ = std::fs::remove_dir_all(&root);
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
