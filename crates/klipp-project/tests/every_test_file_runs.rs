//! Crates whose integration tests share one binary list their test files
//! by hand (`autotests = false`): a file added to `tests/` and listed
//! nowhere would never run. Every such file is a module of `tests/all` or
//! a `[[test]]` of its own.

use std::path::Path;

#[test]
fn every_test_file_is_built() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut checked = 0;
    for entry in std::fs::read_dir(crates).unwrap() {
        let dir = entry.unwrap().path();
        let Ok(manifest) = std::fs::read_to_string(dir.join("Cargo.toml")) else { continue };
        if !manifest.contains("autotests = false") {
            continue;
        }
        let all = std::fs::read_to_string(dir.join("tests/all/main.rs")).unwrap_or_default();
        for file in std::fs::read_dir(dir.join("tests")).unwrap() {
            let path = file.unwrap().path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            let listed = all.contains(&format!("#[path = \"../{name}\"]")) || manifest.contains(&format!("path = \"tests/{name}\""));
            assert!(listed, "{} is in no test binary: add it to tests/all/main.rs or a [[test]] in {}", path.display(), dir.join("Cargo.toml").display());
            checked += 1;
        }
    }
    assert!(checked > 0, "no hand-listed test files found under {}", crates.display());
}
