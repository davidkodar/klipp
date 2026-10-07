// Frozen LUT path parsing from klipp-core at 7d309e9f.
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

/// The bundled LUT library: next to the executable in an install, else
/// in a checkout the executable sits inside (`target/release/klipp-app`
/// under the repository), else in the source tree the build came from.
/// Looked up once. A release built in a worktree that was removed
/// afterwards and copied into the main checkout has no source tree left;
/// the checkout it runs from still has the library (#456).
pub fn bundled_luts_dir() -> Option<&'static Path> {
    static DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let beside_exe = std::env::current_exe()
            .ok()
            .and_then(|exe| luts_above(&exe));
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
    exe.ancestors()
        .skip(1)
        .map(|d| d.join("assets").join("luts"))
        .find(|p| p.is_dir())
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
