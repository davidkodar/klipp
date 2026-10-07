//! The legacy effects route is gone (phase 2b M2, step 11d) and stays
//! gone: none of its names is in any crate's source. A clip's effects run
//! as nodes of a plan, or, where a plan can't run them, on the CPU
//! kernels; there is no third way to bring back.
//!
//! Scans every `.rs` file under each crate's `src`, as text. The names are
//! whole words, so a longer name that merely contains one isn't caught.

use std::path::{Path, PathBuf};

/// The deleted names, each with what replaced it.
const GONE: &[(&str, &str)] = &[
    ("set_legacy_effects", "effects run as plan nodes (11d-3b)"),
    ("KLIPP_LEGACY_EFFECTS", "effects run as plan nodes (11d-3b)"),
    ("legacy_effects", "effects run as plan nodes (11d-3b)"),
    ("stack_layer_apply_effects", "a layer's effects as a plan over its slot, `effects_on_layer` (11d-3a)"),
    ("run_effects_in_chain", "`chain_run_effects`, with `run_break_pass` for a break (11d-4)"),
    ("run_effects_in_chain_from", "`chain_run_effects`, with `run_break_pass` for a break (11d-4)"),
    ("RecordedRun", "the chain's own encoder (11d-4)"),
    ("submit_recorded", "the chain's own encoder (11d-4)"),
    ("effects_on_output", "effects spliced into the grade's plan (step 10)"),
];

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Whether `word` stands alone in `text` at least once: not part of a
/// longer identifier.
fn has_word(text: &str, word: &str) -> bool {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + word.len()..].chars().next();
        !before.is_some_and(ident) && !after.is_some_and(ident)
    })
}

#[test]
fn the_legacy_effects_route_is_not_in_any_crates_source() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("the crates folder");
    let mut files = Vec::new();
    for krate in std::fs::read_dir(crates).expect("the crates folder").flatten() {
        sources(&krate.path().join("src"), &mut files);
    }
    assert!(files.len() > 100, "the scan found {} source files: the crates folder moved?", files.len());
    let mut found = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).unwrap_or_default();
        for (name, instead) in GONE {
            if has_word(&text, name) {
                found.push(format!("{} names `{name}`, which is gone: {instead}", file.strip_prefix(crates).unwrap_or(file).display()));
            }
        }
    }
    assert!(found.is_empty(), "{}", found.join("\n"));
}

#[test]
fn a_name_is_matched_whole() {
    assert!(has_word("fn run_effects_in_chain(", "run_effects_in_chain"));
    assert!(!has_word("fn run_effects_in_chain_from(", "run_effects_in_chain"));
    assert!(!has_word("set_legacy_effects_off", "set_legacy_effects"));
    assert!(has_word("// legacy_effects", "legacy_effects"));
}
