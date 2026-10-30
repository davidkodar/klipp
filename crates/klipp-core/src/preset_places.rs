//! Where a kind's presets are read from (#3095): a person's own folder,
//! then the `presets/<folder>/` of each installed pack, the one installed
//! first first, so of two packs with a preset of one name the earlier
//! keeps it, an update included.

use std::path::{Path, PathBuf};

/// The largest `pack.json` read for when its pack was installed.
const MAX_PACK_JSON_BYTES: u64 = 1024 * 1024;

/// Klipp's data folder, as the packs are installed under it:
/// `$KLIPP_DATA_DIR`, else the system's data folder's `klipp`.
pub fn data_root() -> Option<PathBuf> {
    std::env::var_os("KLIPP_DATA_DIR").map(PathBuf::from).or_else(|| dirs::data_dir().map(|d| d.join("klipp")))
}

/// When an installed pack's folder says it was first installed, in
/// seconds since 1970; 0 for one installed before it was written, or
/// whose `pack.json` doesn't read.
pub fn installed_secs(pack: &Path) -> u64 {
    #[derive(serde::Deserialize)]
    struct Head {
        #[serde(default)]
        installed_secs: u64,
    }
    use std::io::Read;
    let mut text = String::new();
    let read = std::fs::File::open(pack.join("pack.json")).and_then(|f| f.take(MAX_PACK_JSON_BYTES).read_to_string(&mut text));
    read.ok().and_then(|_| serde_json::from_str::<Head>(&text).ok()).map_or(0, |h| h.installed_secs)
}

/// The `presets/<folder>/` of every pack installed under `root`, the one
/// installed first first (by [`installed_secs`], then by folder name). A
/// pack's folder that is a link, or one being installed, is passed over.
pub fn pack_dirs(root: &Path, folder: &str) -> Vec<PathBuf> {
    let mut packs: Vec<(u64, String, PathBuf)> = std::fs::read_dir(root.join("packs"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| Some((e.file_name().to_str()?.to_string(), e.path())))
        .filter(|(name, _)| !name.starts_with('.'))
        .map(|(name, path)| (installed_secs(&path), name, path))
        .collect();
    packs.sort();
    packs.into_iter().map(|(_, _, path)| path.join("presets").join(folder)).collect()
}

/// Every folder a kind's presets in `folder` are read from: the
/// person's own, then the installed packs'.
pub fn dirs_for(folder: &str) -> Vec<PathBuf> {
    crate::preset_file::user_dir(folder).into_iter().chain(data_root().map(|root| pack_dirs(&root, folder)).unwrap_or_default()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The packs' folders come in the order they were installed, not by
    /// name; one installed before the time was kept (none said) first, by
    /// name; a folder being installed and a link are passed over.
    #[test]
    fn the_packs_are_read_in_the_order_they_were_installed() {
        let root = std::env::temp_dir().join(format!("klipp-pack-dirs-{}", std::process::id()));
        let packs = root.join("packs");
        for (name, secs) in [("aaa", Some(300)), ("bbb", Some(100)), ("ccc", None), ("ddd", Some(200)), (".installing-x", Some(1))] {
            std::fs::create_dir_all(packs.join(name)).unwrap();
            let json = match secs {
                Some(s) => format!("{{\"format\": 2, \"name\": \"{name}\", \"installed_secs\": {s}}}"),
                None => format!("{{\"format\": 1, \"name\": \"{name}\"}}"),
            };
            std::fs::write(packs.join(name).join("pack.json"), json).unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(packs.join("aaa"), packs.join("eee")).unwrap();
        let order: Vec<String> = pack_dirs(&root, "generators").iter().map(|p| p.parent().unwrap().parent().unwrap().file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(order, ["ccc", "bbb", "ddd", "aaa"]);
        assert!(pack_dirs(&root, "generators").iter().all(|p| p.ends_with("presets/generators")));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
