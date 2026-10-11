//! `OldStageGone`'s helper: a family's deleted stage is listed by symbol
//! (`families::DELETED`), and no source file in the workspace names one.

use crate::{families, Family};

/// Every place a deleted symbol is named again, over the workspace's
/// sources but this crate (its list names them) and build output.
pub fn named_again() -> Vec<String> {
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut sources = Vec::new();
    let mut dirs = vec![crates];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                if !path.ends_with("klipp-readiness") && !path.ends_with("target") {
                    dirs.push(path);
                }
            } else if path.extension().is_some_and(|e| e == "rs") {
                sources.push(path);
            }
        }
    }
    let mut found = Vec::new();
    for (family, symbols) in families::DELETED {
        for path in &sources {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            for symbol in *symbols {
                if text.contains(symbol) {
                    found.push(format!("{family:?}: {symbol} in {}", path.display()));
                }
            }
        }
    }
    found
}

/// `family`'s old stage gone: it lists what it deleted, and none of it is
/// named again.
pub fn gone(family: Family) -> Result<(), String> {
    let listed = families::DELETED.iter().filter(|(f, _)| *f == family).flat_map(|(_, s)| s.iter()).count();
    if listed == 0 {
        return Err(format!("{family:?} lists no deleted stage in families::DELETED"));
    }
    let back: Vec<String> = named_again().into_iter().filter(|l| l.starts_with(&format!("{family:?}:"))).collect();
    if back.is_empty() {
        Ok(())
    } else {
        Err(format!("named again: {back:?}"))
    }
}
