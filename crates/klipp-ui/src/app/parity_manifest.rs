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

mod rules;

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
    CurveEditor,
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
    SelectEncoding,
    AddMask,
    EditMask,
    RemoveMask,
    AddLayer,
    EditLayer,
    RemoveLayer,
    CapturePlate,
    CaptureReference,
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
    // Whole-node tools have no ordinary parameter row or extra_ui entry.
    for (type_id, id) in [
        ("klipp.grade.corrector", "white_balance"),
        ("klipp.grade.qualifier", "qualifier_picker"),
        ("klipp.grade.color_match", "colour_match"),
    ] {
        out.push(CatalogEntry {
            type_id,
            parameter: format!("@workflow/{id}"),
            grade: true,
            value: CatalogValue::Workflow(id),
        });
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
    pub excluded_keys: BTreeMap<CoverageKey, String>,
    registration_failures: Vec<String>,
}

impl Manifest {
    pub fn require(&mut self, key: CoverageKey) {
        self.required.insert(key);
    }

    /// Families choose concrete fixtures, not how little behavior counts
    /// as coverage. The central rules expand their interaction obligations.
    pub fn require_fixture(&mut self, entry: &CatalogEntry, surface: Surface, variant: &str) {
        let interactions = match rules::interactions(entry, surface) {
            Ok(interactions) => interactions,
            Err(error) => {
                self.registration_failures.push(format!(
                    "{} / {} / {surface:?} / {variant}: {error}",
                    entry.type_id, entry.parameter
                ));
                return;
            }
        };
        for interaction in interactions {
            let key = CoverageKey::new(
                entry.type_id,
                &entry.parameter,
                surface,
                variant,
                interaction,
            );
            if let Some(reason) =
                rules::key_exclusion(entry, surface).filter(|_| rules::is_key(interaction))
            {
                self.exclude_key(key, reason);
            } else {
                self.require(key);
            }
        }
    }

    pub fn exclude(&mut self, type_id: &str, parameter: &str, reason: &str) {
        assert!(
            cited(reason),
            "an exclusion must cite an issue or m3-decisions entry and its reason"
        );
        self.exclusions
            .insert((type_id.into(), parameter.into()), reason.into());
    }

    /// Exempt a deliberate absent affordance without hiding the rest of
    /// the parameter. The required key remains in the manifest for audit.
    pub fn exclude_key(&mut self, key: CoverageKey, reason: &str) {
        assert!(
            cited(reason),
            "an exclusion must cite an issue or m3-decisions entry and its reason"
        );
        self.required.insert(key.clone());
        self.excluded_keys.insert(key, reason.into());
    }

    pub fn exclusion_lines(&self) -> Vec<String> {
        self.exclusions
            .iter()
            .map(|((ty, param), reason)| format!("excluded: {ty} / {param}: {reason}"))
            .chain(
                self.excluded_keys
                    .iter()
                    .map(|(key, reason)| format!("excluded: {key:?}: {reason}")),
            )
            .collect()
    }

    fn rule_errors(&self, entries: &[CatalogEntry]) -> Vec<String> {
        let catalog: BTreeMap<_, _> = entries
            .iter()
            .map(|e| ((e.type_id, e.parameter.as_str()), e))
            .collect();
        let mut errors = self.registration_failures.clone();
        let mut contexts = BTreeSet::new();
        for key in &self.required {
            let Some(entry) = catalog.get(&(key.type_id.as_str(), key.parameter.as_str())) else {
                errors.push(format!("required key outside catalog: {key:?}"));
                continue;
            };
            if !contexts.insert((
                key.type_id.as_str(),
                key.parameter.as_str(),
                key.surface,
                key.variant.as_str(),
            )) {
                continue;
            }
            match rules::interactions(entry, key.surface) {
                Ok(interactions) => {
                    for interaction in interactions {
                        let required = CoverageKey::new(
                            entry.type_id,
                            &entry.parameter,
                            key.surface,
                            &key.variant,
                            interaction,
                        );
                        if !self.required.contains(&required) {
                            errors.push(format!("missing obligation: {required:?}"));
                        }
                    }
                }
                Err(error) => errors.push(error),
            }
        }
        for ((ty, parameter), reason) in &self.exclusions {
            if !catalog.contains_key(&(ty.as_str(), parameter.as_str())) {
                errors.push(format!("stale exclusion: {ty} / {parameter}"));
            }
            if !cited(reason) {
                errors.push(format!("uncited exclusion: {ty} / {parameter}"));
            }
            if self
                .required
                .iter()
                .any(|k| k.type_id == *ty && k.parameter == *parameter)
            {
                errors.push(format!("both required and excluded: {ty} / {parameter}"));
            }
        }
        for (key, reason) in &self.excluded_keys {
            if !self.required.contains(key) {
                errors.push(format!("excluded key was not required: {key:?}"));
            }
            if !catalog.contains_key(&(key.type_id.as_str(), key.parameter.as_str())) {
                errors.push(format!("stale excluded key: {key:?}"));
            }
            if !cited(reason) {
                errors.push(format!("uncited exclusion: {key:?}"));
            }
        }
        errors
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
        errors.extend(manifest.rule_errors(entries));
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
                .filter(|key| !manifest.excluded_keys.contains_key(*key))
                .map(|key| format!("missing: {key:?}")),
        );
        errors.extend(
            self.passed
                .difference(&manifest.required)
                .map(|key| format!("unexpected: {key:?}")),
        );
        errors.extend(
            self.passed
                .iter()
                .filter(|key| manifest.excluded_keys.contains_key(*key))
                .map(|key| format!("excluded case counted as passed: {key:?}")),
        );
        let exclusions = manifest.exclusion_lines();
        for line in &exclusions {
            eprintln!("{line}");
        }
        if errors.is_empty() {
            Ok(())
        } else {
            errors.extend(exclusions);
            Err(errors.join("\n"))
        }
    }
}

fn cited(reason: &str) -> bool {
    let issue = reason
        .split('#')
        .skip(1)
        .any(|tail| tail.starts_with(|c: char| c.is_ascii_digit()));
    let decision = reason.contains("m3-decisions")
        && reason
            .split("IC")
            .skip(1)
            .any(|tail| tail.starts_with(|c: char| c.is_ascii_digit()));
    (issue || decision) && reason.split_whitespace().count() > 1
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

    fn slider() -> CatalogEntry {
        let mut entry = catalog()
            .into_iter()
            .find(|e| e.type_id == "klipp.fx.gaussian_blur" && e.parameter == "radius")
            .unwrap();
        if let CatalogValue::Parameter { presentation, .. } = &mut entry.value {
            presentation.widget = klipp_effects::registry::presentation::Widget::Slider;
        }
        entry
    }

    #[test]
    fn opening_a_slider_does_not_satisfy_its_interaction_obligations() {
        let entry = slider();
        let mut manifest = Manifest::default();
        let opened = CoverageKey {
            interaction: Interaction::OpenWithoutWrite,
            ..key("radius")
        };
        manifest.require(opened.clone());
        let mut report = Report::default();
        report.case("only opened", [opened], || Ok(()));
        let errors = report
            .complete(&manifest, std::slice::from_ref(&entry))
            .unwrap_err();
        for missing in [
            "TypeNumber",
            "Drag",
            "CancelDrag",
            "Reset",
            "AddKey",
            "EditKey",
            "RemoveKey",
            "UndoRedo",
            "Pixels",
        ] {
            assert!(errors.contains(missing), "missing {missing}: {errors}");
        }
        manifest.require_fixture(&entry, Surface::EffectsStack, "sample");
        assert!(
            report
                .complete(&manifest, std::slice::from_ref(&entry))
                .unwrap_err()
                .contains("missing:"),
            "registering requirements earns no evidence"
        );
        report.case(
            "assertions completed",
            manifest.required.iter().cloned(),
            || Ok(()),
        );
        assert!(report.complete(&manifest, &[entry]).is_ok());
    }

    #[test]
    fn every_catalog_editor_has_a_central_rule_and_new_specialists_fail_closed() {
        for entry in catalog() {
            rules::interactions(&entry, Surface::EffectsStack)
                .unwrap_or_else(|why| panic!("{} / {}: {why}", entry.type_id, entry.parameter));
        }
        let mut unknown = slider();
        unknown.value = CatalogValue::Workflow("new_editor");
        assert!(rules::interactions(&unknown, Surface::EffectsStack)
            .unwrap_err()
            .contains("unclassified specialist"));
        let mut manifest = Manifest::default();
        manifest.require_fixture(&unknown, Surface::EffectsStack, "new");
        manifest.require_fixture(&slider(), Surface::EffectsStack, "later");
        assert!(
            !manifest.required.is_empty(),
            "an unknown editor cannot stop later registration"
        );
        assert!(Report::default()
            .complete(&manifest, &[unknown])
            .unwrap_err()
            .contains("unclassified specialist"));
    }

    #[test]
    fn an_exclusion_is_cited_visible_and_never_earned_coverage() {
        let entry = slider();
        let mut manifest = Manifest::default();
        manifest.require_fixture(&entry, Surface::EffectsStack, "sample");
        let excluded = CoverageKey {
            interaction: Interaction::CancelDrag,
            ..key("radius")
        };
        manifest.exclude_key(
            excluded.clone(),
            "#997: synthetic exception for this accounting test",
        );
        let mut report = Report::default();
        report.case(
            "remaining assertions",
            manifest
                .required
                .iter()
                .filter(|k| **k != excluded)
                .cloned(),
            || Ok(()),
        );
        assert!(report
            .complete(&manifest, std::slice::from_ref(&entry))
            .is_ok());
        assert!(manifest.exclusion_lines()[0].contains("#997"));
        assert!(!report.passed.contains(&excluded));
        report.case("wrongly credited", [excluded], || Ok(()));
        let error = report.complete(&manifest, &[entry]).unwrap_err();
        assert!(error.contains("excluded case counted as passed"));
        assert!(error.contains("#997: synthetic exception"));
        assert!(!cited("not supported yet"));
        assert!(!cited("#997"), "a reference alone is not a reason");
        assert!(cited("m3-decisions IC13: explicit undo difference"));
    }

    #[test]
    fn removed_catalog_entries_cannot_be_required_or_excluded_silently() {
        let mut manifest = Manifest::default();
        manifest.require(key("removed"));
        manifest.exclude("removed.type", "gone", "#997: fixture removed");
        let mut report = Report::default();
        report.case("obsolete", [key("removed")], || Ok(()));
        let errors = report.complete(&manifest, &[]).unwrap_err();
        assert!(errors.contains("required key outside catalog"));
        assert!(errors.contains("stale exclusion"));
        assert!(errors.contains("#997: fixture removed"));
    }

    #[test]
    fn reviewed_key_exclusions_are_narrow_and_leave_curve_editor_keys_required() {
        use klipp_effects::registry::presentation::Widget;
        let mut excluded = 0;
        let mut wheel_tracks = 0;
        for entry in catalog() {
            let mut manifest = Manifest::default();
            manifest.require_fixture(&entry, Surface::GradePerNode, "sample");
            for (key, reason) in &manifest.excluded_keys {
                assert!(rules::is_key(key.interaction), "non-key exclusion: {key:?}");
                assert!(
                    matches!(entry.value, CatalogValue::Parameter { presentation, .. }
                    if matches!(presentation.widget, Widget::Toggle | Widget::Combo | Widget::WheelPart(_))),
                    "other widget excluded: {key:?}"
                );
                assert!(reason.contains("#1318"));
                excluded += 1;
            }
            assert!(!manifest
                .excluded_keys
                .keys()
                .any(|k| k.interaction == Interaction::Pixels));
            if matches!(entry.value, CatalogValue::Parameter { desc, presentation }
                if desc.keyframable && matches!(presentation.widget, Widget::WheelPart(_)))
            {
                let mut curves = Manifest::default();
                curves.require_fixture(&entry, Surface::CurveEditor, "wheel-track");
                assert!(curves.excluded_keys.is_empty());
                for action in [
                    Interaction::AddKey,
                    Interaction::EditKey,
                    Interaction::RemoveKey,
                ] {
                    assert!(curves.required.contains(&CoverageKey::new(
                        entry.type_id,
                        &entry.parameter,
                        Surface::CurveEditor,
                        "wheel-track",
                        action
                    )));
                }
                assert!(
                    !curves
                        .required
                        .iter()
                        .any(|k| matches!(k.interaction, Interaction::WheelDrag(_))),
                    "the curve editor does not draw a wheel"
                );
                wheel_tracks += 1;
            }
        }
        assert!(
            excluded > 0 && wheel_tracks > 0,
            "exercise both actual catalog classes"
        );
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
