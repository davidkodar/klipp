//! The clip builder for tests (`test_clip`) is built only for tests: no
//! crate turns its `test-clip` feature on from a normal dependency, nor
//! does the workspace's own dependency table. Only `[dev-dependencies]`
//! may.

use std::path::Path;

/// The places in `toml` (a Cargo manifest) that turn `test-clip` on
/// outside a dev-dependencies table: `(section, line)`.
fn outside_dev(toml: &str) -> Vec<(String, String)> {
    let mut section = String::new();
    let mut found = Vec::new();
    for line in toml.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            section = t.trim_matches(|c| c == '[' || c == ']').to_string();
            continue;
        }
        if t.starts_with('#') || !t.contains("test-clip") {
            continue;
        }
        // The feature's own definition, in klipp-timeline's [features].
        if section == "features" {
            continue;
        }
        if !section.ends_with("dev-dependencies") {
            found.push((section.clone(), t.to_string()));
        }
    }
    found
}

#[test]
fn test_clip_is_turned_on_only_by_dev_dependencies() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut manifests = vec![root.join("Cargo.toml")];
    for entry in std::fs::read_dir(root.join("crates")).expect("the crates folder").flatten() {
        let m = entry.path().join("Cargo.toml");
        if m.is_file() {
            manifests.push(m);
        }
    }
    assert!(manifests.len() > 20, "found {} manifests: the crates folder moved?", manifests.len());
    let mut wrong = Vec::new();
    for m in &manifests {
        let text = std::fs::read_to_string(m).unwrap_or_default();
        for (section, line) in outside_dev(&text) {
            wrong.push(format!("{} [{section}]: {line}", m.display()));
        }
    }
    assert!(wrong.is_empty(), "test-clip turned on outside [dev-dependencies]:\n{}", wrong.join("\n"));
}

#[test]
fn the_scan_sees_a_normal_dependency_turning_it_on() {
    let bad = "[dependencies]\nklipp-timeline = { workspace = true, features = [\"test-clip\"] }\n";
    assert_eq!(outside_dev(bad).len(), 1);
    let target = "[target.'cfg(unix)'.dependencies]\nklipp-timeline = { workspace = true, features = [\"test-clip\"] }\n";
    assert_eq!(outside_dev(target).len(), 1, "a target table is a normal one");
    let good = "[dev-dependencies]\nklipp-timeline = { workspace = true, features = [\"test-clip\"] }\n";
    assert!(outside_dev(good).is_empty());
    let defined = "[features]\ntest-clip = []\n";
    assert!(outside_dev(defined).is_empty(), "its definition");
}
