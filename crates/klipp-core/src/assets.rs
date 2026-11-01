//! Where the app's bundled assets are, and how a project names them.
//!
//! A look applied from the bundled LUT library used to be saved as the
//! absolute path the library had on the machine that applied it —
//! `/home/…/Klipp/assets/luts/fuji-xtrans/…cube`, inside a source
//! checkout — so the project broke on any other machine, in a packaged
//! install, or once the checkout moved. A path inside the library is now
//! saved as `bundled:<path inside the library>` and resolved against
//! wherever this install keeps it. A LUT the user brought is outside the
//! library and keeps its own path.

use std::path::{Path, PathBuf};

/// How a project names a file inside the bundled LUT library.
pub const BUNDLED_LUT_PREFIX: &str = "bundled:";

/// The bundled LUT library's folders and what each holds, in the order
/// they are listed: the Effects browser's LUTs, the Colour tab's presets
/// and the socket's looks are these (#3255).
pub const BUNDLED_LUT_FOLDERS: [(&str, &str); 7] = [
    ("film-bw", "Film B&W"),
    ("film-slide", "Film Slide"),
    ("film-negative", "Film Negative"),
    ("film-print", "Film Print"),
    ("film-instant", "Film Instant"),
    ("fuji-xtrans", "Fuji X-Trans"),
    // A camera's log or flat profile to normal: on normal footage they
    // lift its blacks (#2769). The folder keeps its old name, which saved
    // grades point into.
    ("cinematic", "Camera conversion (log footage)"),
];

/// The bundled LUT library: next to the executable in an install, else
/// in a checkout the executable sits inside (`target/release/klipp-app`
/// under the repository), else in the source tree the build came from.
/// Looked up once. A release built in a worktree that was removed
/// afterwards and copied into the main checkout has no source tree left;
/// the checkout it runs from still has the library (#456).
pub fn bundled_luts_dir() -> Option<&'static Path> {
    static DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let beside_exe = std::env::current_exe().ok().and_then(|exe| luts_above(&exe));
        let source_tree = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.join("assets").join("luts"))
            .filter(|p| p.is_dir());
        beside_exe.or(source_tree)
    })
    .as_deref()
}

/// The nearest `assets/luts` library in the directories holding `exe`:
/// beside it in an install, at the repository root for a cargo build.
fn luts_above(exe: &Path) -> Option<PathBuf> {
    exe.ancestors().skip(1).map(|d| d.join("assets").join("luts")).find(|p| p.is_dir())
}

/// The name a project stores for `path`: `bundled:<relative>` when it
/// lies in the library at `library`, the path itself otherwise.
pub fn portable_lut_path(path: &Path, library: Option<&Path>) -> String {
    if let Some(rel) = library.and_then(|lib| path.strip_prefix(lib).ok()) {
        return format!("{BUNDLED_LUT_PREFIX}{}", rel.to_string_lossy());
    }
    path.to_string_lossy().into_owned()
}

/// The file a stored name refers to on this machine. `bundled:` names
/// resolve into the library. So does an absolute path written before
/// names were portable — one into some install's or checkout's
/// `assets/luts/` — when this library holds the same file: it is the
/// bundled look, and taking it from here means the next save stores it
/// portably. So does a bare `<path inside the library>`, the name a
/// build that could not find its library once saved for a `bundled:`
/// one. Everything else is taken as it is.
pub fn resolve_lut_path(stored: &str, library: Option<&Path>) -> PathBuf {
    if let Some(rel) = stored.strip_prefix(BUNDLED_LUT_PREFIX) {
        return match library {
            Some(lib) => lib.join(rel),
            // Kept whole, so the next save writes the name back as it
            // was: stripped to `rel`, it was saved as a path relative to
            // nothing and the look was lost for good.
            None => PathBuf::from(stored),
        };
    }
    if let (Some(lib), Some(i)) = (library, stored.find("/assets/luts/")) {
        let here = lib.join(&stored[i + "/assets/luts/".len()..]);
        if here.exists() {
            return here;
        }
    }
    if let Some(lib) = library.filter(|_| Path::new(stored).is_relative()) {
        let here = lib.join(stored);
        if here.exists() {
            return here;
        }
    }
    PathBuf::from(stored)
}

/// Serde for an optional LUT path: portable on the way out, resolved on
/// the way in. For `#[serde(with = "klipp_core::assets::lut_path_serde")]`.
pub mod lut_path_serde {
    use super::*;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(path: &Option<PathBuf>, s: S) -> Result<S::Ok, S::Error> {
        match path {
            Some(p) => s.serialize_some(&portable_lut_path(p, bundled_luts_dir())),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<PathBuf>, D::Error> {
        let stored = Option::<String>::deserialize(d)?;
        Ok(stored.map(|s| resolve_lut_path(&s, bundled_luts_dir())))
    }
}

#[cfg(test)]
mod tests {
    /// A cargo build's executable finds the library at the repository
    /// root above `target/release`; an install finds the one beside it.
    #[test]
    fn the_library_is_found_above_the_executable() {
        let root = std::env::temp_dir().join(format!("klipp-luts-above-{}", std::process::id()));
        let luts = root.join("assets").join("luts");
        let release = root.join("target").join("release");
        std::fs::create_dir_all(&luts).unwrap();
        std::fs::create_dir_all(&release).unwrap();
        assert_eq!(super::luts_above(&release.join("klipp-app")), Some(luts.clone()));
        let beside = release.join("assets").join("luts");
        std::fs::create_dir_all(&beside).unwrap();
        assert_eq!(super::luts_above(&release.join("klipp-app")), Some(beside), "the nearest one wins");
        let _ = std::fs::remove_dir_all(&root);
    }

    use super::*;

    #[test]
    fn a_library_lut_is_stored_by_its_place_in_the_library() {
        let lib = Path::new("/opt/klipp/assets/luts");
        let p = lib.join("fuji-xtrans/classic_chrome.cube");
        assert_eq!(portable_lut_path(&p, Some(lib)), "bundled:fuji-xtrans/classic_chrome.cube");
        assert_eq!(resolve_lut_path("bundled:fuji-xtrans/classic_chrome.cube", Some(lib)), p);
    }

    #[test]
    fn a_users_own_lut_keeps_its_path() {
        let lib = Path::new("/opt/klipp/assets/luts");
        assert_eq!(portable_lut_path(Path::new("/home/u/looks/mine.cube"), Some(lib)), "/home/u/looks/mine.cube");
        assert_eq!(resolve_lut_path("/home/u/looks/mine.cube", Some(lib)), PathBuf::from("/home/u/looks/mine.cube"));
    }

    /// A build without the library keeps a `bundled:` name as it is, so
    /// saving writes it back, and a build with the library opens it.
    #[test]
    fn a_bundled_name_survives_a_build_without_the_library() {
        let name = "bundled:film-slide/kodak_kodachrome_64.cube";
        let held = resolve_lut_path(name, None);
        assert_eq!(portable_lut_path(&held, None), name);
        let lib = Path::new("/opt/klipp/assets/luts");
        assert_eq!(resolve_lut_path(&portable_lut_path(&held, None), Some(lib)), lib.join("film-slide/kodak_kodachrome_64.cube"));
    }

    /// A project that such a build saved with the bare name inside the
    /// library opens with the bundled look again.
    #[test]
    fn a_bare_library_name_finds_the_lut_here() {
        let Some(lib) = bundled_luts_dir() else { return };
        let here = lib.join("film-slide/kodak_kodachrome_64.cube");
        assert!(here.exists(), "the bundled library should hold this LUT");
        assert_eq!(resolve_lut_path("film-slide/kodak_kodachrome_64.cube", Some(lib)), here);
        assert_eq!(portable_lut_path(&here, Some(lib)), "bundled:film-slide/kodak_kodachrome_64.cube");
    }

    /// #331: a project saved with the library's absolute path in another
    /// checkout opens with the same LUT from this install's library.
    #[test]
    fn an_old_absolute_library_path_finds_the_lut_here() {
        let Some(lib) = bundled_luts_dir() else { return };
        let old = "/home/someone/old-checkout/Klipp/assets/luts/fuji-xtrans/fuji_xtrans_iii_classic_chrome.cube";
        let here = lib.join("fuji-xtrans/fuji_xtrans_iii_classic_chrome.cube");
        assert!(here.exists(), "the bundled library should hold this LUT");
        assert_eq!(resolve_lut_path(old, Some(lib)), here);
    }
}
