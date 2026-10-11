//! The test-only fault (`fault`) is never built into the app: only
//! `[dev-dependencies]` turn `fault-hook` on, and the readiness crate,
//! which nothing depends on and which is never published.

use std::path::Path;

/// The places in `toml` that turn `fault-hook` on outside a dev
/// dependencies table: `(section, line)`.
fn outside_dev(toml: &str) -> Vec<(String, String)> {
    let mut section = String::new();
    let mut found = Vec::new();
    for line in toml.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            section = t.trim_matches(|c| c == '[' || c == ']').to_string();
            continue;
        }
        if t.starts_with('#') || !t.contains("fault-hook") || section == "features" {
            continue;
        }
        if !section.ends_with("dev-dependencies") {
            found.push((section.clone(), t.to_string()));
        }
    }
    found
}

#[test]
fn fault_hook_is_turned_on_only_for_tests() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut manifests = vec![root.join("Cargo.toml")];
    for entry in std::fs::read_dir(root.join("crates")).expect("the crates folder").flatten() {
        let m = entry.path().join("Cargo.toml");
        if m.is_file() {
            manifests.push(m);
        }
    }
    let mut found = Vec::new();
    for m in &manifests {
        let toml = std::fs::read_to_string(m).unwrap_or_default();
        // The readiness crate is a test harness: unpublished, and no crate
        // depends on it.
        if m.parent().is_some_and(|d| d.ends_with("klipp-readiness")) {
            assert!(toml.contains("publish = false"), "klipp-readiness must stay unpublished to turn fault-hook on");
            continue;
        }
        found.extend(outside_dev(&toml).into_iter().map(|(s, l)| format!("{}: [{s}] {l}", m.display())));
    }
    for m in &manifests {
        let toml = std::fs::read_to_string(m).unwrap_or_default();
        if !m.parent().is_some_and(|d| d.ends_with("klipp-readiness")) && toml.lines().any(|l| l.trim_start().starts_with("klipp-readiness")) {
            found.push(format!("{} depends on klipp-readiness, which turns fault-hook on", m.display()));
        }
    }
    assert!(found.is_empty(), "fault-hook outside the tests: {found:#?}");
}
