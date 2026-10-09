//! The flip's readiness (M3 step 18): every parity family's obligations in
//! one manifest, every family run into one report, and the deletion gate
//! (`Report::complete_for_deletion`) over the whole catalog, so a family
//! skipped, a key missing or a route left open can't pass the flip.
//!
//! `flip_readiness` runs every family and is `#[ignore]`: the step 18
//! runner runs it, and requires it to pass. Its full list (missing,
//! blocked, CPU by design, fallback and excluded keys) is printed and
//! written to `target/parity-flip-report.txt`, to be compared between
//! runs. Two cheap tests run on every gate: the orchestrator's manifest
//! has exactly the pinned rule errors (`parity_flip_open.txt`, a list
//! that only shrinks), and every parity family is registered here.

use super::parity_manifest::{catalog, Manifest, Report};

/// Every family's obligations, as the families declare them, and the
/// central rules' representatives.
fn manifest() -> Manifest {
    let mut m = Manifest::default();
    super::parity_numeric::require(&mut m);
    super::parity_effect_keys::require(&mut m);
    super::parity_keyed_pixels::require(&mut m);
    super::parity_masked_pixels::require(&mut m);
    super::parity_specialists::require(&mut m);
    super::parity_video_transform::require(&mut m);
    super::parity_curve_editor::require(&mut m);
    super::parity_controls::require(&mut m);
    super::parity_grade_specialists::require(&mut m);
    m.require_representatives();
    m
}

/// Every family's cases, each as a family of the report.
fn run_all(report: &mut Report) {
    report.family("numeric", super::parity_numeric::run);
    report.family("effect keys", super::parity_effect_keys::run);
    report.family("keyed pixels", super::parity_keyed_pixels::run);
    report.family("masked pixels", super::parity_masked_pixels::run);
    report.family("specialists", super::parity_specialists::run);
    report.family("specialist pixels", super::parity_specialist_pixels::run);
    report.family("video transform", super::parity_video_transform::exercise);
    report.family("curve editor", super::parity_curve_editor::run);
    report.family("effect controls", super::parity_controls::run);
    // The grade families, on the clip's grade and on the sequence's grades
    // (`on_roots`), where they earn `NonClipGrade`.
    use super::parity_controls::on_roots;
    report.family("grade controls", super::parity_controls::run_grade);
    report.family("grade controls on the sequence's grades", |r| on_roots(r, super::parity_controls::run_grade));
    report.family("grade pages", super::parity_controls::run_grade_pages);
    report.family("grade pages on the sequence's grades", |r| on_roots(r, super::parity_controls::run_grade_pages));
    report.family("grade LUT", super::parity_grade_specialists::run_lut);
    report.family("grade LUT on the sequence's grades", |r| on_roots(r, super::parity_grade_specialists::run_lut));
    report.family("grade masks", super::parity_grade_specialists::run_masks);
    report.family("grade masks on the sequence's grades", |r| on_roots(r, super::parity_grade_specialists::run_masks));
}

/// The report's lists, one line each, sorted: what the flip still lacks
/// and what it accepts by declaration.
fn listing(manifest: &Manifest, report: &Report, gate: &Result<(), String>) -> String {
    let mut lines: Vec<String> = Vec::new();
    if let Err(errors) = gate {
        lines.extend(errors.lines().map(|l| format!("OPEN {l}")));
    }
    lines.extend(report.blocked.iter().map(|(k, why)| format!("BLOCKED {k:?}: {why}")));
    lines.extend(report.cpu_by_design.iter().map(|(k, why)| format!("CPU-BY-DESIGN {k:?}: {why}")));
    lines.extend(report.fallback.iter().map(|(k, why)| format!("FALLBACK {k:?}: {why}")));
    lines.extend(manifest.exclusion_lines().into_iter().map(|l| format!("EXCLUDED {l}")));
    lines.sort();
    lines.dedup();
    // Counts that add up: of the required keys, each is earned, excluded
    // with a citation, or missing; the gate's open lines also hold what
    // isn't a required key (a surface no family declares, a key earned but
    // never required, an unlabelled fallback), so they're counted apart.
    let required = &manifest.required;
    let earned = required.iter().filter(|k| report.passed.contains(*k)).count();
    let excluded = required.iter().filter(|k| manifest.excluded_keys.contains_key(*k) && !report.passed.contains(*k)).count();
    let missing = required.len() - earned - excluded;
    let unexpected = report.passed.iter().filter(|k| !required.contains(*k)).count();
    let open = lines.iter().filter(|l| l.starts_with("OPEN")).count();
    let head = format!(
        "required {} = earned {earned} + excluded {excluded} + missing {missing}; also earned but not required {unexpected}; gate errors {open} (each missing key, each surface no family declares, each unexpected or unlabelled key)",
        required.len()
    );
    lines.push(format!("RETAINED NestedGrade: {}", super::parity_manifest::rules_nested_grade()));
    lines.sort();
    // The keys still missing, by interaction and surface, with the types
    // that owe them: which family's work each group is.
    let mut groups: std::collections::BTreeMap<(String, String), std::collections::BTreeSet<String>> = Default::default();
    for key in manifest.required.iter().filter(|k| !report.passed.contains(*k) && !manifest.excluded_keys.contains_key(*k)) {
        groups.entry((format!("{:?}", key.interaction), format!("{:?}", key.surface))).or_default().insert(key.type_id.clone());
    }
    let summary = groups.into_iter().map(|((interaction, surface), types)| format!("missing {interaction} on {surface}: {} types: {}", types.len(), types.into_iter().collect::<Vec<_>>().join(", ")));
    std::iter::once(head).chain(summary).chain(std::iter::once(String::new())).chain(lines).collect::<Vec<_>>().join("\n")
}

/// Where the listing is written: the target directory the tests build in.
fn report_path() -> std::path::PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR").map(std::path::PathBuf::from).unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    target.join("parity-flip-report.txt")
}

/// The flip's gate: every family run, the deletion gate over the whole
/// catalog. Run by the step 18 runner (`--ignored`), which requires it to
/// pass.
#[test]
#[ignore = "the step 18 flip gate: run by its runner, which requires it to pass"]
fn flip_readiness() {
    let manifest = manifest();
    let mut report = Report::default();
    run_all(&mut report);
    let gate = report.complete_for_deletion(&manifest, &catalog());
    let listing = listing(&manifest, &report, &gate);
    eprintln!("{listing}");
    let path = report_path();
    if let Err(e) = std::fs::write(&path, &listing) {
        eprintln!("the listing wasn't written to {}: {e}", path.display());
    }
    assert!(gate.is_ok(), "the flip isn't ready; the list is in {}", path.display());
}

/// The rule errors the orchestrator's manifest has today, one per line:
/// catalog surfaces no family yet declares (the workflows' own family is
/// pending, d9's workflow Report family; the rest are the step 18 work
/// still open). A shrinking list: a family that comes to cover a surface
/// takes its line out in the same change.
const OPEN: &str = include_str!("parity_flip_open.txt");

/// The orchestrator's manifest, without running anything, has exactly the
/// pinned rule errors over the catalog: a family that requires the wrong
/// keys, or a surface newly left uncovered, fails every gate, and so does
/// a pinned error that's gone, so the pin can't outlive it.
#[test]
fn the_flip_manifest_has_only_the_pinned_rule_errors() {
    let now: std::collections::BTreeSet<String> = manifest().rule_errors(&catalog()).into_iter().collect();
    let pinned: std::collections::BTreeSet<String> = OPEN.lines().filter(|l| !l.is_empty()).map(str::to_owned).collect();
    let new: Vec<_> = now.difference(&pinned).collect();
    let gone: Vec<_> = pinned.difference(&now).collect();
    assert!(new.is_empty() && gone.is_empty(), "rule errors not pinned:\n{}\npinned but gone (take them out of parity_flip_open.txt):\n{}", new.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n"), gone.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n"));
}

/// Every parity family is registered here: each `app/` module that defines
/// a family's `require` or `run` entry is named in this file, so one can't
/// be left out of the flip by forgetting it.
#[test]
fn every_parity_family_is_in_the_flip() {
    let this = include_str!("parity_flip.rs");
    let families: [(&str, &str); 13] = [
        ("parity_numeric", include_str!("parity_numeric.rs")),
        ("parity_effect_keys", include_str!("parity_effect_keys.rs")),
        ("parity_keyed_pixels", include_str!("parity_keyed_pixels.rs")),
        ("parity_masked_pixels", include_str!("parity_masked_pixels.rs")),
        ("parity_specialists", include_str!("parity_specialists.rs")),
        ("parity_specialist_pixels", include_str!("parity_specialist_pixels.rs")),
        ("parity_video_transform", include_str!("parity_video_transform.rs")),
        ("parity_curve_editor", include_str!("parity_curve_editor.rs")),
        ("parity_controls", include_str!("parity_controls.rs")),
        ("parity_grade_specialists", include_str!("parity_grade_specialists.rs")),
        ("parity_grade_workflows", include_str!("parity_grade_workflows.rs")),
        ("parity_pixels", include_str!("parity_pixels.rs")),
        ("parity_harness", include_str!("parity_harness.rs")),
    ];
    // The modules on disk are the ones app.rs declares.
    let app = include_str!("../app.rs");
    let declared: Vec<&str> = app.lines().filter_map(|l| l.trim().strip_prefix("mod ")).map(|m| m.trim_end_matches(';')).filter(|m| m.starts_with("parity_")).filter(|m| *m != "parity_flip" && *m != "parity_manifest").collect();
    for m in &declared {
        assert!(families.iter().any(|(name, _)| name == m), "app.rs declares {m}, which this scan doesn't list");
    }
    for (name, source) in families {
        let entry = source.contains("pub(super) fn require(") || source.contains("(report: &mut Report)");
        if entry {
            assert!(this.contains(&format!("super::{name}::")), "{name} has a family entry the flip doesn't call");
        }
    }
}
