//! A font family as Collect Project carries it (#3009): its files and the
//! licence beside them, found where this machine has the family, and
//! whether the font's own embedding permission lets it go.

use std::path::{Path, PathBuf};

/// The licence files a font folder holds, by how their names begin.
const LICENCE_NAMES: [&str; 5] = ["OFL", "LICENSE", "LICENCE", "COPYING", "UFL"];

/// `family`'s font files and licence as this machine has it: installed,
/// else fetched from the catalogue. `Err` says why it can't be carried:
/// not found, no licence file beside it, or a face whose embedding
/// permission (its OS/2 `fsType`) is restricted.
pub fn font_files(family: &str) -> Result<(Vec<PathBuf>, PathBuf), String> {
    let registry = crate::font_discovery::discover_system_fonts();
    let faces = registry
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(family.trim()))
        .map(|(_, faces)| faces.clone())
        .or_else(|| crate::catalogue::fetched_faces(family))
        .ok_or("not found on this machine")?;
    let mut files: Vec<PathBuf> = faces.into_iter().map(|f| f.path).collect();
    files.sort();
    files.dedup();
    let licence = files.iter().filter_map(|f| f.parent()).find_map(licence_in).ok_or("no licence file found beside it")?;
    for f in &files {
        let bytes = std::fs::read(f).map_err(|e| format!("{}: {e}", f.display()))?;
        if !embedding_allowed(&bytes) {
            return Err("its licence does not allow embedding".into());
        }
    }
    Ok((files, licence))
}

/// The licence file in `dir`, if one is there.
fn licence_in(dir: &Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| LICENCE_NAMES.iter().any(|k| n.to_uppercase().starts_with(k))))
        .collect();
    found.sort();
    found.into_iter().next()
}

/// Whether a font's own permission lets it be carried with a document:
/// anything but restricted-licence embedding. A file that does not read
/// as a font is not carried.
pub fn embedding_allowed(bytes: &[u8]) -> bool {
    match ttf_parser::Face::parse(bytes, 0) {
        Ok(face) => face.permissions() != Some(ttf_parser::Permissions::Restricted),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The OS/2 table's `fsType`, set to `value`: offset 8 in the table.
    pub(crate) fn with_fs_type(mut bytes: Vec<u8>, value: u16) -> Vec<u8> {
        let tables = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
        for i in 0..tables {
            let at = 12 + i * 16;
            if &bytes[at..at + 4] == b"OS/2" {
                let offset = u32::from_be_bytes(bytes[at + 8..at + 12].try_into().unwrap()) as usize;
                bytes[offset + 8..offset + 10].copy_from_slice(&value.to_be_bytes());
                return bytes;
            }
        }
        panic!("no OS/2 table");
    }

    /// A bundled face may be embedded; the same face marked
    /// restricted may not; something that is not a font is not carried.
    #[test]
    fn a_restricted_font_is_not_carried() {
        let bebas = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/fonts/BebasNeue/BebasNeue-Regular.ttf")).unwrap();
        assert!(embedding_allowed(&bebas));
        assert!(!embedding_allowed(&with_fs_type(bebas.clone(), 0x0002)), "restricted licence embedding");
        assert!(embedding_allowed(&with_fs_type(bebas, 0x0004)), "preview and print embedding");
        assert!(!embedding_allowed(b"not a font"));
    }
}
