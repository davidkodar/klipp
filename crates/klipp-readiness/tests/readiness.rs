//! M4's readiness gate and the cheap tests beside it (m4-readiness
//! design, "The gate and the cheap tests").

use klipp_readiness::{families, manifest, run_all, Family};

/// Where the report is written: the target directory the tests build in.
fn report_path() -> std::path::PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR").map(std::path::PathBuf::from).unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    target.join("m4-readiness-report.txt")
}

/// The gate: every family run, nothing open. Run on request (`--ignored`)
/// at each family's landing and each pixel checkpoint; M4 closes when it
/// passes.
#[test]
#[ignore = "M4's readiness gate: run on request, required to pass at M4's close"]
fn m4_readiness() {
    let m = manifest();
    let r = run_all();
    let listing = r.listing(&m);
    eprintln!("{listing}");
    let path = report_path();
    if let Err(e) = std::fs::write(&path, &listing) {
        eprintln!("the listing wasn't written to {}: {e}", path.display());
    }
    assert!(r.open(&m).is_empty(), "M4 isn't ready; the list is in {}", path.display());
}

/// The rule errors the manifest has today, one per line: a list that only
/// shrinks, so a family registered wrong, or a rule left unable to expand
/// a subject, fails every gate, and a pin can't outlive its error.
const OPEN: &str = include_str!("m4_readiness_open.txt");

#[test]
fn the_manifest_has_only_the_pinned_rule_errors() {
    let now: std::collections::BTreeSet<String> = manifest().errors.into_iter().collect();
    let pinned: std::collections::BTreeSet<String> = OPEN.lines().filter(|l| !l.is_empty()).map(str::to_owned).collect();
    let new: Vec<_> = now.difference(&pinned).collect();
    let gone: Vec<_> = pinned.difference(&now).collect();
    assert!(new.is_empty() && gone.is_empty(), "rule errors not pinned: {new:#?}\npinned but gone (take them out of m4_readiness_open.txt): {gone:#?}");
}

/// Every family is registered once, and each owes something: one can't be
/// left out of the gate by forgetting it.
#[test]
fn every_family_is_registered_and_owes_something() {
    use Family::*;
    for family in [Ofx, OfxTemporal, Frei0r, TemporalDenoise, FrameBlend, RollingShutter, Stabilisation, Deflicker, LegacyChain, Format] {
        let entries = families::ALL.iter().filter(|e| e.family == family).count();
        assert_eq!(entries, 1, "{family:?} is registered {entries} times");
        let mut m = klipp_readiness::Manifest::default();
        let entry = families::ALL.iter().find(|e| e.family == family).unwrap();
        (entry.require)(&mut m);
        assert!(m.errors.is_empty(), "{family:?}: {:?}", m.errors);
        assert!(m.required.iter().all(|k| k.family == family), "{family:?} requires another family's keys");
        assert!(!m.required.is_empty(), "{family:?} owes nothing");
    }
}

/// Every deleted stage stays gone: no source file in the workspace names
/// a symbol a family deleted (`families::DELETED`).
#[test]
fn old_stages_stay_gone() {
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut sources = Vec::new();
    let mut dirs = vec![crates];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                // Not this crate (its list names them), nor build output.
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
    assert!(found.is_empty(), "deleted stages named again: {found:#?}");
}
