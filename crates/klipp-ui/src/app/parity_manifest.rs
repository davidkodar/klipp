//! Coverage accounting shared by the inspector parity families.
//!
//! A catalog entry is an obligation to classify, not evidence of a test.
//! Cases earn coverage only after their assertions succeed. Each family
//! can run alone; the orchestrator alone checks the union for completeness.
// The family runners are integrated in the next parity slices.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{catch_unwind, AssertUnwindSafe};

use klipp_effects::registry::presentation::Presentation;
use klipp_effects::registry::{ParamDesc, PortParamDesc};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub(super) enum Surface {
    EffectsStack,
    VideoTab,
    GraphInspector,
    GradePerNode,
    GradeTabbedRight,
    GradeBottomTools,
    NestedGrade,
    NonClipGrade,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub(super) enum WheelAxis {
    X,
    Y,
    Master,
    Saturation,
    Brightness,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub(super) enum CurveAction {
    Add,
    Drag,
    Remove,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub(super) enum KeyInterpolation {
    Linear,
    Step,
    Bezier,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub(super) enum Interaction {
    OpenWithoutWrite,
    TypeNumber,
    Drag,
    CancelDrag,
    Reset,
    Toggle(bool),
    ComboSelect(usize),
    ColourEdit,
    AlphaEdit,
    WheelDrag(WheelAxis),
    CurvePoint(CurveAction),
    FileSelect,
    FileClear,
    FileCancel,
    MissingAsset,
    PluginSelect,
    PluginParameter,
    StartAnalysis,
    CancelAnalysis,
    ResultDuringDrag,
    AutoWhiteBalance,
    Eyedropper,
    ColourMatch,
    AddKey,
    RemoveKey,
    EditKey,
    PreviousKey,
    NextKey,
    Interpolation(KeyInterpolation),
    AddModifier,
    EditModifier,
    RemoveModifier,
    UndoRedo,
    Pixels,
    PreserveHidden,
    ConditionalVisibility,
    ConditionalEnabled,
    AddNode,
    RemoveNode,
    ReorderNode,
    SharedBranchRefusal,
    Reveal,
}

/// `variant` names the concrete fixture, including condition state,
/// encoding, selected channel or nested path. It is checked against the
/// required set, so a misspelling is unexpected coverage, not success.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub(super) struct CoverageKey {
    pub type_id: String,
    pub parameter: String,
    pub surface: Surface,
    pub variant: String,
    pub interaction: Interaction,
}

impl CoverageKey {
    pub fn new(
        type_id: &str,
        parameter: &str,
        surface: Surface,
        variant: &str,
        interaction: Interaction,
    ) -> Self {
        Self {
            type_id: type_id.into(),
            parameter: parameter.into(),
            surface,
            variant: variant.into(),
            interaction,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) enum CatalogValue {
    Parameter {
        desc: &'static ParamDesc,
        presentation: Presentation,
    },
    Port {
        port: &'static str,
        desc: &'static PortParamDesc,
    },
    /// The graph's implicit mix parameter, absent from `desc.params`.
    Mix,
    /// The card/header control, absent from `desc.params`.
    Enabled,
    /// Specialist work outside an ordinary parameter row.
    Workflow(&'static str),
}

#[derive(Clone, Debug)]
pub(super) struct CatalogEntry {
    pub type_id: &'static str,
    pub parameter: String,
    pub grade: bool,
    pub value: CatalogValue,
}

/// Include every resolved parameter presentation, not just the overrides
/// or the rows visible in the default fixture. Port controls and implicit
/// graph controls are catalogued separately so they cannot disappear from
/// coverage when ordinary row tests are expanded.
pub(super) fn catalog() -> Vec<CatalogEntry> {
    let mut out = Vec::new();
    let mut add = |desc: &'static klipp_effects::registry::NodeTypeDesc,
                   grade: bool,
                   presentation: &dyn Fn(&ParamDesc) -> Presentation,
                   workflow: Option<&'static str>| {
        for param in desc.params {
            out.push(CatalogEntry {
                type_id: desc.id,
                parameter: param.key.into(),
                grade,
                value: CatalogValue::Parameter {
                    desc: param,
                    presentation: presentation(param),
                },
            });
        }
        for port in desc.inputs {
            for param in port.params {
                out.push(CatalogEntry {
                    type_id: desc.id,
                    parameter: klipp_effects::registry::port_param_key(port.key, param.key),
                    grade,
                    value: CatalogValue::Port {
                        port: port.key,
                        desc: param,
                    },
                });
            }
        }
        if desc.mixable {
            out.push(CatalogEntry {
                type_id: desc.id,
                parameter: "@mix".into(),
                grade,
                value: CatalogValue::Mix,
            });
        }
        out.push(CatalogEntry {
            type_id: desc.id,
            parameter: "@enabled".into(),
            grade,
            value: CatalogValue::Enabled,
        });
        if let Some(id) = workflow {
            out.push(CatalogEntry {
                type_id: desc.id,
                parameter: format!("@workflow/{id}"),
                grade,
                value: CatalogValue::Workflow(id),
            });
        }
    };
    for ty in klipp_effects::registry::effect_types() {
        add(
            &ty.desc,
            false,
            &|p| ty.presentation(p),
            ty.type_presentation().extra_ui,
        );
    }
    for ty in klipp_nodes::registry::node_types() {
        add(
            &ty.desc,
            true,
            &|p| ty.presentation(p),
            ty.type_presentation().extra_ui,
        );
    }
    out
}

/// Every catalog entry must have at least one required key or a specific
/// reviewed exclusion. Exclusions name the decision/issue and reason;
/// they are reported separately and never counted as passed coverage.
#[derive(Default)]
pub(super) struct Manifest {
    pub required: BTreeSet<CoverageKey>,
    pub exclusions: BTreeMap<(String, String), String>,
}

impl Manifest {
    pub fn require(&mut self, key: CoverageKey) {
        self.required.insert(key);
    }

    pub fn exclude(&mut self, type_id: &str, parameter: &str, reason: &str) {
        assert!(
            !reason.trim().is_empty(),
            "an exclusion must name its decision and reason"
        );
        self.exclusions
            .insert((type_id.into(), parameter.into()), reason.into());
    }

    /// A new catalog parameter must be explicitly classified even before
    /// it has an interaction script. A family handler's mere existence
    /// cannot satisfy this check or the later passed-key check.
    pub fn unclassified(&self, entries: &[CatalogEntry]) -> Vec<String> {
        entries
            .iter()
            .filter(|entry| {
                !self
                    .required
                    .iter()
                    .any(|key| key.type_id == entry.type_id && key.parameter == entry.parameter)
                    && !self
                        .exclusions
                        .contains_key(&(entry.type_id.into(), entry.parameter.clone()))
            })
            .map(|entry| format!("{} / {}", entry.type_id, entry.parameter))
            .collect()
    }
}

#[derive(Debug, Default, serde::Serialize)]
pub(super) struct Report {
    pub passed: BTreeSet<CoverageKey>,
    pub failures: Vec<String>,
}

impl Report {
    /// Isolate a family's setup as well as its individual cases, so even
    /// a fixture panic cannot prevent subsequent families from running.
    pub fn family(&mut self, name: &str, run: impl FnOnce(&mut Self)) {
        if let Err(panic) = catch_unwind(AssertUnwindSafe(|| run(self))) {
            self.failures
                .push(format!("{name}: {}", panic_message(panic.as_ref())));
        }
    }

    /// Run a case even if earlier cases failed. A panic is a failed case,
    /// not a reason to lose the rest of the coverage report. Assertions
    /// should check the actual UI result, complete picture, history and
    /// side commands before returning `Ok(())`.
    pub fn case(
        &mut self,
        name: &str,
        keys: impl IntoIterator<Item = CoverageKey>,
        run: impl FnOnce() -> Result<(), String>,
    ) {
        let keys: Vec<_> = keys.into_iter().collect();
        match catch_unwind(AssertUnwindSafe(run)) {
            Ok(Ok(())) => self.passed.extend(keys),
            Ok(Err(error)) => self.failures.push(format!("{name}: {error}")),
            Err(panic) => {
                self.failures
                    .push(format!("{name}: {}", panic_message(panic.as_ref())));
            }
        }
    }

    /// A standalone family's test judges failures, not whole-catalog
    /// coverage. The orchestrator calls the same runner then `complete`.
    pub fn family_result(&self) -> Result<(), String> {
        if self.failures.is_empty() {
            Ok(())
        } else {
            Err(self.failures.join("\n"))
        }
    }

    pub fn complete(&self, manifest: &Manifest, entries: &[CatalogEntry]) -> Result<(), String> {
        let mut errors = self.failures.clone();
        errors.extend(
            manifest
                .unclassified(entries)
                .into_iter()
                .map(|key| format!("unclassified: {key}")),
        );
        errors.extend(
            manifest
                .required
                .difference(&self.passed)
                .map(|key| format!("missing: {key:?}")),
        );
        errors.extend(
            self.passed
                .difference(&manifest.required)
                .map(|key| format!("unexpected: {key:?}")),
        );
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("\n"))
        }
    }
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> &str {
    panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap_or("non-string panic")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(parameter: &str) -> CoverageKey {
        CoverageKey::new(
            "klipp.fx.gaussian_blur",
            parameter,
            Surface::EffectsStack,
            "sample",
            Interaction::Drag,
        )
    }

    #[test]
    fn failed_and_panicking_cases_do_not_earn_coverage_or_stop_later_cases() {
        let mut report = Report::default();
        report.case("failed", [key("radius")], || Err("wrong pixels".into()));
        report.case("panic", [key("radius")], || panic!("wrong undo"));
        report.case("later family", [key("other")], || Ok(()));
        assert!(!report.passed.contains(&key("radius")));
        assert!(report.passed.contains(&key("other")));
        assert_eq!(report.failures.len(), 2);
        assert!(report.family_result().is_err());
    }

    #[test]
    fn a_family_setup_failure_does_not_stop_the_next_family() {
        let mut report = Report::default();
        report.family("broken fixture", |_| panic!("fixture unavailable"));
        report.family("next family", |r| {
            r.case("valid", [key("radius")], || Ok(()))
        });
        assert!(report.passed.contains(&key("radius")));
        assert_eq!(report.failures, ["broken fixture: fixture unavailable"]);
    }

    #[test]
    fn completeness_reports_missing_and_unexpected_keys_together() {
        let mut manifest = Manifest::default();
        manifest.require(key("radius"));
        let mut report = Report::default();
        report.case("misspelled", [key("raduis")], || Ok(()));
        let errors = report.complete(&manifest, &[]).unwrap_err();
        assert!(errors.contains("missing:"));
        assert!(errors.contains("unexpected:"));
        assert!(
            report.family_result().is_ok(),
            "standalone cases do not decide completeness"
        );
    }

    #[test]
    fn a_new_catalog_parameter_requires_classification_not_a_handler_name() {
        let entries = catalog();
        let blur: Vec<_> = entries
            .into_iter()
            .filter(|e| e.type_id == "klipp.fx.gaussian_blur")
            .collect();
        let mut manifest = Manifest::default();
        manifest.require(key("radius"));
        let missing = manifest.unclassified(&blur);
        assert!(missing.iter().any(|e| e.ends_with("@enabled")));
        assert!(missing.iter().any(|e| e.ends_with("mask.channel")));
        assert!(missing.iter().any(|e| e.ends_with("@mix")));
        let report = Report::default();
        assert!(
            report
                .complete(&manifest, &blur)
                .unwrap_err()
                .contains("missing:"),
            "classification is not execution evidence"
        );
    }

    #[test]
    fn inventory_contains_all_params_with_unique_keys_including_hidden_and_custom() {
        let entries = catalog();
        let keys: BTreeSet<_> = entries
            .iter()
            .map(|e| (e.type_id, e.parameter.as_str()))
            .collect();
        assert_eq!(keys.len(), entries.len(), "ambiguous catalog coverage keys");
        let declared = klipp_effects::registry::effect_types()
            .map(|t| t.desc.params.len())
            .sum::<usize>()
            + klipp_nodes::registry::node_types()
                .map(|t| t.desc.params.len())
                .sum::<usize>();
        assert_eq!(
            entries
                .iter()
                .filter(|e| matches!(e.value, CatalogValue::Parameter { .. }))
                .count(),
            declared
        );
        assert!(entries.iter().any(|e| matches!(&e.value, CatalogValue::Parameter { presentation, .. } if presentation.widget == klipp_effects::registry::presentation::Widget::Hidden)));
        assert!(entries.iter().any(|e| matches!(&e.value, CatalogValue::Parameter { presentation, .. } if matches!(presentation.widget, klipp_effects::registry::presentation::Widget::Custom(_)))));
    }
}
