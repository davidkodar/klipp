//! The step to revision 5 (#2754): a tracked surface's keys as maps from
//! its reference corners. The conversion is
//! `klipp_graph_lift::revision5`; this is its run, by step 3's machinery.

pub use klipp_graph_lift::revision5::{project, FROM, TO};

use serde_json::Value;

/// Revision 5's step, for `step3`'s run.
pub const STEP: crate::step3::Step = crate::step3::Step {
    name: "revision 5",
    from: FROM,
    to: TO,
    journal: "run5.json",
    side: crate::revision2::REVISION,
    check,
    convert: klipp_graph_lift::revision5::project_text,
    verify: klipp_graph_lift::revision5::verify,
};

/// Why a revision-4 project is to be converted: the surfaces it holds.
fn check(raw: &Value) -> Result<String, String> {
    let (surfaces, keys) = klipp_graph_lift::revision5::check(raw)?;
    Ok(if surfaces == 0 { "at revision 4, converted; no tracked surfaces".into() } else { format!("at revision 4, converted; {surfaces} surfaces, {keys} keys as maps") })
}

/// The manifest of revision 5's run ([`crate::step3::preflight_with`]).
pub fn preflight(s: &crate::run::Settings) -> crate::step3::Manifest {
    crate::step3::preflight_with(&STEP, s)
}

/// Revision 5's dry run ([`crate::step3::dry_run_with`]).
pub fn dry_run(s: &crate::run::Settings, out: &std::path::Path, build: &str, reload: &dyn Fn(&std::path::Path) -> Result<(), String>, render: Option<&dyn Fn(&std::path::Path, &std::path::Path) -> Result<String, String>>) -> Result<crate::step3::DryReport, String> {
    crate::step3::dry_run_with(&STEP, s, out, build, reload, render)
}

/// Revision 5's real run ([`crate::step3::run_with`]).
pub fn run(s: &crate::run::Settings, approved: &str, preconditions: &dyn Fn() -> Result<(), String>, hooks: &crate::step3::Hooks) -> Result<crate::step3::Report, crate::step3::RunError> {
    crate::step3::run_with(&STEP, s, approved, preconditions, hooks)
}

/// A project's text converted.
pub struct Converted {
    pub text: String,
}

/// `project`'s text at revision 5.
pub fn convert(project: &str) -> Result<Converted, String> {
    klipp_graph_lift::revision5::project_text(project).map(|text| Converted { text })
}

pub use klipp_graph_lift::revision5::verify;
