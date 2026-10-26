//! The step to revision 4 (M4 step F): the conversion is
//! `klipp_graph_lift::revision4` (so a copy can be stepped by any reader,
//! the render check's included); this is its run, by step 3's machinery.

pub use klipp_graph_lift::revision4::{truth_picture, Converted, Declared, DROPPED_RAMP_FIELDS, FROM};

/// `project`'s text at revision 4 ([`klipp_graph_lift::revision4::convert`]),
/// its pictures checked against the effect and grade registries.
pub fn convert(project: &str) -> Result<Converted, String> {
    klipp_graph_lift::revision4::convert(project, &klipp_graph_lift::registry_catalog::RegistryCatalog)
}

/// [`convert`] on a project's JSON.
pub fn project(raw: &Value) -> Result<Value, String> {
    klipp_graph_lift::revision4::project(raw, &klipp_graph_lift::registry_catalog::RegistryCatalog)
}

use std::collections::BTreeMap;

use serde_json::Value;

use crate::canon;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

// ── The run ─────────────────────────────────────────────────────────────
//
// Step 3's run, backups, journal and rollback, for this step: its check
// names what each project declares, its conversion is [`convert`], and its
// proof converts the original again and requires the same text.

/// Revision 4's step, for `step3`'s run.
pub const STEP: crate::step3::Step = crate::step3::Step {
    name: "revision 4",
    from: FROM,
    to: klipp_graph_lift::revision4::TO,
    journal: "run4.json",
    side: crate::revision2::REVISION,
    check,
    convert: convert_text,
    verify,
};

/// Why a revision-3 project is to be converted: what it declares, by row;
/// or why it can't be (a named refusal, a stop in the manifest).
fn check(raw: &Value) -> Result<String, String> {
    let text = serde_json::to_string(raw).map_err(err)?;
    let c = convert(&text)?;
    let mut by_row: BTreeMap<&str, usize> = BTreeMap::new();
    for d in &c.declared {
        *by_row.entry(d.row).or_default() += 1;
    }
    let rows: Vec<String> = by_row.iter().map(|(r, n)| format!("{r} ×{n}")).collect();
    Ok(if rows.is_empty() { "at revision 3, converted; declares nothing".into() } else { format!("at revision 3, converted; declares {}", rows.join(", ")) })
}

fn convert_text(text: &str) -> Result<String, String> {
    convert(text).map(|c| c.text)
}

/// A written revision-4 text held to its original: the conversion of the
/// original again, the same document, and it reads back at revision 4.
pub fn verify(original: &str, written: &str) -> Result<(), String> {
    let again: Value = serde_json::from_str(&convert(original)?.text).map_err(err)?;
    let back: Value = serde_json::from_str(written).map_err(|e| format!("the written file doesn't read back: {e}"))?;
    if back.get("format_revision").and_then(Value::as_u64) != Some(klipp_graph_lift::revision4::TO) {
        return Err(format!("written at revision {:?}", back.get("format_revision")));
    }
    if canon::sorted(back) != canon::sorted(again) {
        return Err("the written file isn't the original's conversion".into());
    }
    Ok(())
}

/// The manifest of revision 4's run ([`crate::step3::preflight_with`]).
pub fn preflight(s: &crate::run::Settings) -> crate::step3::Manifest {
    crate::step3::preflight_with(&STEP, s)
}

/// Revision 4's dry run ([`crate::step3::dry_run_with`]).
pub fn dry_run(s: &crate::run::Settings, out: &std::path::Path, build: &str, reload: &dyn Fn(&std::path::Path) -> Result<(), String>, render: Option<&dyn Fn(&std::path::Path, &std::path::Path) -> Result<String, String>>) -> Result<crate::step3::DryReport, String> {
    crate::step3::dry_run_with(&STEP, s, out, build, reload, render)
}

/// Revision 4's real run ([`crate::step3::run_with`]).
pub fn run(s: &crate::run::Settings, approved: &str, preconditions: &dyn Fn() -> Result<(), String>, hooks: &crate::step3::Hooks) -> Result<crate::step3::Report, crate::step3::RunError> {
    crate::step3::run_with(&STEP, s, approved, preconditions, hooks)
}
