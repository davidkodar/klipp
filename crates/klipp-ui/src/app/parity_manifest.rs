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
    /// A keyframable row's key lane drawn by the renderer: its keys move
    /// the frame, and a key edited through either view draws the same
    /// frame at a key and between keys (`parity_keyed_pixels`).
    KeyedPixels,
    /// A masked effect's row edited through either view draws the same
    /// masked frame on each route (`parity_masked_pixels`).
    MaskedPixels,
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

// A test's catalog, built once: its rows' presentations kept inline.
#[allow(clippy::large_enum_variant)]
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
    // An effect type in the Grade category is a grade compound's marker in
    // a clip's list (#1746): no card, no row, never added as an effect.
    for ty in klipp_effects::registry::effect_types().filter(|t| t.desc.category != klipp_effects::registry::Category::Grade) {
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

    /// Require the central rules' representative obligations
    /// (`rules::REPRESENTATIVE`), which no catalog parameter carries.
    pub fn require_representatives(&mut self) {
        for key in rules::representative_keys() {
            self.require(key);
        }
    }

    /// The central rules' representative obligations that nothing
    /// required: a family deleted or skipped can't define them away.
    /// Checked by [`Report::complete_for_deletion`], the gate on removing
    /// the old route; a single family's manifest needn't hold them.
    pub fn missing_representatives(&self) -> Vec<String> {
        rules::representative_keys().into_iter().filter(|k| !self.required.contains(k)).map(|key| format!("missing obligation: {key:?}")).collect()
    }

    /// Families choose concrete fixtures, not how little behavior counts
    /// as coverage. The central rules expand their interaction obligations.
    pub fn require_fixture(&mut self, entry: &CatalogEntry, surface: Surface, variant: &str) {
        if let Some(reason) = rules::entry_exclusion(entry) {
            self.exclude(entry.type_id, &entry.parameter, reason);
            return;
        }
        let interactions = match rules::fixture_interactions(entry, surface, variant) {
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
            if let Some(reason) = rules::key_exclusion(entry, surface).filter(|_| rules::is_key(interaction)) {
                self.exclude_key(key, reason);
            } else {
                self.require(key);
            }
        }
    }

    /// `entry`'s fixture `variant` owed on every sequence grade, on each
    /// clip grade page it requires (M3 step 19): `Surface::NonClipGrade`
    /// under the variant [`Report::merge_on_root`] gives a case run there.
    pub fn require_on_sequence_grades(&mut self, entry: &CatalogEntry, variant: &str) {
        let Ok(surfaces) = rules::surfaces(entry) else { return };
        for (root, page) in sequence_grade_obligations(&surfaces) {
            self.require_fixture(entry, Surface::NonClipGrade, &format!("{root}: {page:?} {variant}"));
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

    pub(super) fn rule_errors(&self, entries: &[CatalogEntry]) -> Vec<String> {
        let catalog: BTreeMap<_, _> = entries
            .iter()
            .map(|e| ((e.type_id, e.parameter.as_str()), e))
            .collect();
        let mut errors = self.registration_failures.clone();
        for entry in entries {
            if self
                .exclusions
                .contains_key(&(entry.type_id.into(), entry.parameter.clone()))
            {
                continue;
            }
            match rules::surfaces(entry) {
                Ok(surfaces) => {
                    for surface in &surfaces {
                        if !self.required.iter().any(|key| {
                            key.type_id == entry.type_id
                                && key.parameter == entry.parameter
                                && key.surface == *surface
                        }) {
                            errors.push(format!(
                                "missing surface: {} / {} / {surface:?}",
                                entry.type_id, entry.parameter
                            ));
                        }
                    }
                    // The sequence's grades are owed through representative
                    // rows on each page (`rules::ROOT_NUMERIC`), checked
                    // with the other representatives, not per entry.
                }
                Err(error) => errors.push(error),
            }
        }
        let representative = rules::representative_keys();
        let mut contexts = BTreeSet::new();
        for key in &self.required {
            if representative.contains(key) {
                continue;
            }
            let Some(entry) = catalog.get(&(key.type_id.as_str(), key.parameter.as_str())) else {
                errors.push(format!("required key outside catalog: {key:?}"));
                continue;
            };
            if let Some(reason) = rules::entry_exclusion(entry) {
                errors.push(format!(
                    "excluded entry registered as required: {key:?}: {reason}"
                ));
            }
            if key.surface == Surface::GraphInspector {
                errors.push(format!(
                    "excluded surface registered as required: {key:?}: {}",
                    rules::GRAPH_INSPECTOR_EXCLUSION
                ));
            }
            if key.surface == Surface::CurveEditor && entry.grade && !rules::grade_curve_representative(entry) {
                errors.push(format!("excluded surface registered as required: {key:?}: {}", rules::GRADE_CURVE_EXCLUSION));
            }
            if !contexts.insert((
                key.type_id.as_str(),
                key.parameter.as_str(),
                key.surface,
                key.variant.as_str(),
            )) {
                continue;
            }
            match rules::fixture_interactions(entry, key.surface, &key.variant) {
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
            // A representative's exclusion is the central rules', as the
            // representative is.
            if !catalog.contains_key(&(key.type_id.as_str(), key.parameter.as_str())) && !representative.contains(key) {
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
                    && rules::entry_exclusion(entry).is_none()
            })
            .map(|entry| format!("{} / {}", entry.type_id, entry.parameter))
            .collect()
    }
}

/// The central rules' representative keys of one interaction: what the
/// family earning that interaction requires of them.
pub(super) fn representatives_of(interaction: Interaction) -> Vec<CoverageKey> {
    rules::representative_keys().into_iter().filter(|k| k.interaction == interaction).collect()
}

/// The grade key controls' representative rows, as (page, type, parameter)
/// (`rules::GRADE_KEY_REPRESENTATIVE`).
pub(super) fn grade_key_representatives() -> [(Surface, &'static str, &'static str); 3] {
    rules::GRADE_KEY_REPRESENTATIVE
}

/// The grade bypass's representative nodes, as (page, type)
/// (`rules::BYPASS_REPRESENTATIVE`).
pub(super) fn bypass_representatives() -> [(Surface, &'static str); 3] {
    rules::BYPASS_REPRESENTATIVE
}

/// The variant the grade bypass representatives are earned under.
pub(super) fn bypass_variant() -> &'static str {
    rules::BYPASS_VARIANT
}

/// The one page a grade entry is owed on per type (`rules::surfaces`): a
/// node's bypass, a lean specialist editor; `None` for an entry the
/// central rules exclude.
pub(super) fn owed_page(entry: &CatalogEntry) -> Option<Surface> {
    if rules::entry_exclusion(entry).is_some() {
        return None;
    }
    rules::surfaces(entry).ok()?.into_iter().next()
}

/// The clip grade page a grade row owes its per-type `AddKey` on
/// (`rules::primary_grade_page`), by its type and parameter.
pub(super) fn primary_grade_page(type_id: &str, parameter: &str) -> Option<Surface> {
    catalog().into_iter().find(|e| e.type_id == type_id && e.parameter == parameter).and_then(|e| rules::primary_grade_page(&e))
}

/// The variant the grade key representatives are earned under.
pub(super) fn grade_key_variant() -> &'static str {
    rules::GRADE_KEY_VARIANT
}

/// Whether a curve-editor lane is one the central rules owe a drawn edit
/// on (`rules::CURVE_PIXEL_REPRESENTATIVE`).
pub(super) fn grade_curve_representative(type_id: &str, parameter: &str) -> bool {
    rules::GRADE_CURVE_REPRESENTATIVE.iter().any(|(t, p)| *t == type_id && *p == parameter)
}

pub(super) fn curve_pixel_representative(type_id: &str, parameter: &str) -> bool {
    rules::CURVE_PIXEL_REPRESENTATIVE.iter().any(|(t, p)| *t == type_id && *p == parameter)
}

/// The sequence grades a `Surface::NonClipGrade` entry is owed on, each
/// with each clip grade page among `surfaces`: as `(grade, page)`.
fn sequence_grade_obligations(surfaces: &BTreeSet<Surface>) -> Vec<(&'static str, Surface)> {
    let pages = [Surface::GradePerNode, Surface::GradeTabbedRight, Surface::GradeBottomTools];
    super::parity_harness::GradeRoot::SEQUENCE
        .iter()
        .flat_map(|root| pages.iter().filter(|p| surfaces.contains(p)).map(move |p| (root.variant(), *p)))
        .collect()
}

/// Whether a `Surface::NonClipGrade` variant names one of the sequence's
/// own grades a family ran on (`Report::merge_on_root`): the timeline
/// grade or a group's pre-clip or post-clip stage. A track's grade has no
/// parity fixture yet (#1408).
fn on_a_sequence_grade(variant: &str) -> bool {
    super::parity_harness::GradeRoot::SEQUENCE.iter().any(|root| variant.starts_with(&format!("{}: ", root.variant())))
}

thread_local! {
    /// Whether a family is running with its grade on one of the sequence's
    /// own grades (`parity_controls::on_roots`, M3 step 19), and how often
    /// the case running now drew or edited that grade.
    static ON_ROOT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static ROOT_TOUCHES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// A family run on a sequence grade, while it lives: dropped (a panic
/// included), the run is over, so no later case is held to it.
pub(super) struct OnRoot(());

impl OnRoot {
    pub(super) fn start() -> Self {
        ON_ROOT.with(|on| on.set(true));
        OnRoot(())
    }
}

impl Drop for OnRoot {
    fn drop(&mut self) {
        ON_ROOT.with(|on| on.set(false));
    }
}

/// The case running now drew or edited the sequence grade it runs on.
pub(super) fn touched_the_root() {
    ROOT_TOUCHES.with(|n| n.set(n.get() + 1));
}

/// `run`, held, during a family's run on a sequence grade, to having drawn
/// or edited that grade: a case that passes without (a helper that built
/// its own fixture on a clip) fails, so no key is counted for a grade the
/// case never touched.
fn touching_the_root(run: impl FnOnce() -> Result<(), String>) -> impl FnOnce() -> Result<(), String> {
    move || {
        ROOT_TOUCHES.with(|n| n.set(0));
        let result = run();
        let untouched = ON_ROOT.with(std::cell::Cell::get) && ROOT_TOUCHES.with(std::cell::Cell::get) == 0;
        match result {
            Ok(()) if untouched => Err("passed without drawing or editing the sequence grade it counts for".into()),
            other => other,
        }
    }
}

#[derive(Debug, Default, serde::Serialize)]
pub(super) struct Report {
    pub passed: BTreeSet<CoverageKey>,
    /// Successful cases that still exercised the legacy implementation.
    /// These count for parity, but never authorize removing that fallback.
    #[serde(serialize_with = "serialize_fallbacks")]
    pub fallback: BTreeMap<CoverageKey, String>,
    pub failures: Vec<String>,
    /// Cases that couldn't run on the route their obligation names (a
    /// Pixels case whose frame the native GPU stack declined or left), and
    /// why: open, never passed and never a failure of the case itself, so
    /// the report shows them rather than a skip (`Report::case_or_blocked`).
    #[serde(serialize_with = "serialize_fallbacks")]
    pub blocked: BTreeMap<CoverageKey, String>,
    /// Passed Pixels cases drawn on the CPU route alone, for a family whose
    /// production route is the CPU by design: the GPU half not applicable,
    /// with the M4 step that moves it (`Report::case_cpu_by_design`).
    #[serde(serialize_with = "serialize_fallbacks")]
    pub cpu_by_design: BTreeMap<CoverageKey, String>,
}

fn serialize_fallbacks<S: serde::Serializer>(
    fallback: &BTreeMap<CoverageKey, String>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    // JSON object keys cannot hold a structured CoverageKey. Keep the
    // key and its reason together in an auditable array instead.
    #[derive(serde::Serialize)]
    struct Evidence<'a> {
        key: &'a CoverageKey,
        reason: &'a str,
    }
    let entries: Vec<_> = fallback
        .iter()
        .map(|(key, reason)| Evidence { key, reason })
        .collect();
    serde::Serialize::serialize(&entries, serializer)
}

impl Report {
    /// Isolate a family's setup as well as its individual cases, so even
    /// a fixture panic cannot prevent subsequent families from running.
    /// `other`, a family's report run with its grade on one of the
    /// sequence's own grades (`root`, M3 step 19), folded in: each key it
    /// earned on a clip grade's page counted on `Surface::NonClipGrade`,
    /// under a variant naming the grade, the page and the fixture it was
    /// drawn as; each failure named with the grade.
    pub fn merge_on_root(&mut self, root: &str, other: Report) {
        let rekey = |key: CoverageKey| {
            let variant = format!("{root}: {:?} {}", key.surface, key.variant);
            CoverageKey { surface: Surface::NonClipGrade, variant, ..key }
        };
        let rekey_map = |map: BTreeMap<CoverageKey, String>| map.into_iter().map(|(k, why)| (rekey(k), why)).collect::<BTreeMap<_, _>>();
        self.passed.extend(other.passed.into_iter().map(rekey));
        self.fallback.extend(rekey_map(other.fallback));
        self.blocked.extend(rekey_map(other.blocked));
        self.cpu_by_design.extend(rekey_map(other.cpu_by_design));
        self.failures.extend(other.failures.into_iter().map(|f| format!("{root}: {f}")));
    }

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
        match catch_unwind(AssertUnwindSafe(touching_the_root(run))) {
            Ok(Ok(())) => self.passed.extend(keys),
            Ok(Err(error)) => self.failures.push(format!("{name}: {error}")),
            Err(panic) => {
                self.failures
                    .push(format!("{name}: {}", panic_message(panic.as_ref())));
            }
        }
    }

    pub fn case_via_fallback(
        &mut self,
        name: &str,
        keys: impl IntoIterator<Item = CoverageKey>,
        reason: &str,
        run: impl FnOnce() -> Result<(), String>,
    ) {
        let keys: Vec<_> = keys.into_iter().collect();
        if !cited(reason) {
            self.failures
                .push(format!("{name}: fallback needs a cited reason"));
            return;
        }
        let before = self.failures.len();
        self.case(name, keys.clone(), run);
        if self.failures.len() == before {
            self.fallback
                .extend(keys.into_iter().map(|key| (key, reason.into())));
        }
    }

    /// [`Report::case`], except that an error beginning `BLOCKED` records
    /// the keys as blocked, with the reason, instead of a failure: the
    /// case couldn't be run as its obligation asks, so it earns nothing
    /// and stays open in the completeness report.
    pub fn case_or_blocked(
        &mut self,
        name: &str,
        keys: impl IntoIterator<Item = CoverageKey>,
        run: impl FnOnce() -> Result<(), String>,
    ) {
        let keys: Vec<_> = keys.into_iter().collect();
        match catch_unwind(AssertUnwindSafe(touching_the_root(run))) {
            Ok(Ok(())) => self.passed.extend(keys),
            Ok(Err(error)) if error.starts_with("BLOCKED") => {
                for key in keys {
                    self.blocked.insert(key, format!("{name}: {error}"));
                }
            }
            Ok(Err(error)) => self.failures.push(format!("{name}: {error}")),
            Err(panic) => self
                .failures
                .push(format!("{name}: {}", panic_message(panic.as_ref()))),
        }
    }

    /// [`Report::case`] for a case drawn on the CPU route alone because
    /// its family runs on the CPU by design: `reason` must name the M4
    /// step that moves it, and is kept beside the keys it earns, so a
    /// declared route is never mistaken for a native one.
    pub fn case_cpu_by_design(
        &mut self,
        name: &str,
        keys: impl IntoIterator<Item = CoverageKey>,
        reason: &str,
        run: impl FnOnce() -> Result<(), String>,
    ) {
        if !reason.contains("M4 step") {
            self.failures.push(format!("{name}: a CPU-by-design route names the M4 step that moves it"));
            return;
        }
        let keys: Vec<_> = keys.into_iter().collect();
        let failed = self.failures.len();
        self.case(name, keys.iter().cloned(), run);
        if self.failures.len() == failed {
            for key in keys {
                self.cpu_by_design.insert(key, format!("n/a on the GPU: CPU by design, {reason}"));
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
                .map(|key| match self.blocked.get(key) {
                    Some(why) => format!("missing: {key:?} ({why})"),
                    None => format!("missing: {key:?}"),
                }),
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
        for (key, reason) in &self.fallback {
            eprintln!("fallback: {key:?}: {reason}");
            if !self.passed.contains(key) || !cited(reason) {
                errors.push(format!("invalid fallback evidence: {key:?}: {reason}"));
            }
        }
        for key in &self.passed {
            // The timeline grade and a group's stages draw generated at
            // their own roots (M3 step 19): a case run on one of them
            // (`parity_controls::on_roots`, its variant naming the grade)
            // is the generated route's. Any other non-clip variant is still
            // labelled fallback.
            let legacy = key.surface == Surface::NonClipGrade && !on_a_sequence_grade(&key.variant);
            if legacy && !self.fallback.contains_key(key) {
                errors.push(format!("fallback evidence not labelled: {key:?} (#997: unnamed non-clip roots still use the legacy path)"));
            }
        }
        let mut exclusions = manifest.exclusion_lines();
        exclusions.push(format!(
            "excluded surface: GraphInspector: {}",
            rules::GRAPH_INSPECTOR_EXCLUSION
        ));
        exclusions.push(format!("excluded surface variant: NonClipGrade track: {}", rules::TRACK_GRADE_EXCLUSION));
        exclusions.push(format!("excluded surface: CurveEditor for grade rows: {}", rules::GRADE_CURVE_EXCLUSION));
        for entry in entries {
            if let Some(reason) = rules::entry_exclusion(entry) {
                let line = format!(
                    "excluded: {} / {}: {reason}",
                    entry.type_id, entry.parameter
                );
                if !exclusions.contains(&line) {
                    exclusions.push(line);
                }
            }
        }
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

    /// Step 19 has a stronger gate than parity: passing through old code
    /// is proof that code must remain until the case passes without it.
    pub fn complete_for_deletion(
        &self,
        manifest: &Manifest,
        entries: &[CatalogEntry],
    ) -> Result<(), String> {
        let mut errors = self
            .complete(manifest, entries)
            .err()
            .into_iter()
            .collect::<Vec<_>>();
        errors.extend(manifest.missing_representatives());
        errors.extend(
            self.fallback
                .iter()
                .map(|(key, reason)| format!("legacy deletion blocked: {key:?}: {reason}")),
        );
        if errors.is_empty() {
            Ok(())
        } else {
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

    /// The keys a family can earn: every required key but those excluded
    /// with a citation, which stay in the manifest for audit and must never
    /// count as passed.
    fn earnable(manifest: &Manifest) -> Vec<CoverageKey> {
        manifest.required.iter().filter(|k| !manifest.excluded_keys.contains_key(*k)).cloned().collect()
    }

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

    fn require_every_surface(manifest: &mut Manifest, entry: &CatalogEntry) {
        for surface in rules::surfaces(entry).unwrap() {
            manifest.require_fixture(entry, surface, "sample");
        }
        manifest.require_on_sequence_grades(entry, "sample");
    }

    /// The sequence's grades are owed through a representative row on each
    /// clip grade page, on each of them (flip readiness ruling, 10-09): a
    /// manifest that requires the timeline grade on one page leaves every
    /// other grade and page named as missing, and none once the
    /// representatives are required; and no entry is owed per root.
    #[test]
    fn each_sequence_grade_and_page_is_owed_through_its_representative() {
        let entry = catalog()
            .into_iter()
            .find(|e| e.type_id == "klipp.grade.corrector" && e.parameter == "exposure")
            .unwrap();
        let surfaces = rules::surfaces(&entry).unwrap();
        let mut narrowed = Manifest::default();
        for surface in &surfaces {
            narrowed.require_fixture(&entry, *surface, "sample");
        }
        narrowed.require_fixture(&entry, Surface::NonClipGrade, "timeline: GradeTabbedRight sample");
        let missing = narrowed.missing_representatives();
        for root in super::super::parity_harness::GradeRoot::SEQUENCE {
            for (page, _, _) in rules::GRADE_KEY_REPRESENTATIVE {
                let numeric = format!("{}: {page:?} {}", root.variant(), rules::ROOT_NUMERIC_VARIANT);
                let covered = numeric == "timeline: GradeTabbedRight sample";
                assert_eq!(missing.iter().any(|e| e.contains(&format!("{numeric:?}"))), !covered, "{numeric}: {missing:#?}");
                let keyed = format!("{}: {page:?} {}", root.variant(), rules::GRADE_KEY_VARIANT);
                assert!(missing.iter().any(|e| e.contains(&format!("{keyed:?}"))), "{keyed} not owed");
            }
        }
        assert!(!narrowed.rule_errors(std::slice::from_ref(&entry)).iter().any(|e| e.contains("NonClipGrade group")), "an entry is owed per root");
        let mut whole = Manifest::default();
        whole.require_representatives();
        assert!(whole.missing_representatives().is_empty());
    }

    #[test]
    fn every_routed_grade_node_has_a_required_per_node_fixture() {
        let entries = catalog();
        for ty in klipp_nodes::registry::node_types() {
            let routed = crate::app::main_layout::per_node_kind(ty.kind);
            let mut manifest = Manifest::default();
            for entry in entries.iter().filter(|entry| entry.type_id == ty.desc.id) {
                require_every_surface(&mut manifest, entry);
            }
            let registered = manifest
                .required
                .iter()
                .any(|key| key.surface == Surface::GradePerNode);
            assert_eq!(
                registered, routed,
                "{}: production routing and required fixtures disagree",
                ty.desc.id
            );
        }
    }

    #[test]
    fn required_surfaces_follow_actual_effect_controls_and_reviewed_grade_routes() {
        let entries = catalog();
        // A grade compound's marker (#1746) has no card: not catalogued.
        for ty in klipp_effects::registry::effect_types().filter(|t| t.desc.category != klipp_effects::registry::Category::Grade) {
            let entry = entries
                .iter()
                .find(|e| e.type_id == ty.desc.id && matches!(e.value, CatalogValue::Enabled))
                .unwrap();
            let surfaces = rules::surfaces(entry).unwrap();
            assert!(
                !surfaces.contains(&Surface::VideoTab),
                "{} has no Video bypass",
                ty.tag
            );
            assert!(surfaces.contains(&Surface::EffectsStack), "{}", ty.tag);
            assert!(!surfaces.contains(&Surface::GraphInspector));
        }
        for (ty, parameter, video) in [
            ("opacity", "value", true),
            ("transform", "position_x", true),
            ("transform", "position_y", true),
            ("transform", "scale_x", true),
            ("transform", "scale_y", true),
            ("transform", "rotation", true),
            ("transform", "rotation_x", true),
            ("transform", "rotation_y", true),
            ("transform", "perspective", true),
            ("transform", "anchor_x", false),
            ("transform", "anchor_y", false),
            ("speed_ramp", "speed", false),
            ("temporal_denoise", "strength", false),
        ] {
            let entry = entries
                .iter()
                .find(|e| e.type_id == format!("klipp.fx.{ty}") && e.parameter == parameter)
                .unwrap();
            let surfaces = rules::surfaces(entry).unwrap();
            assert!(
                surfaces.contains(&Surface::EffectsStack),
                "{ty} / {parameter}"
            );
            assert_eq!(
                surfaces.contains(&Surface::VideoTab),
                video,
                "{ty} / {parameter}"
            );
        }
        let wheel = entries
            .iter()
            .find(|e| {
                e.type_id == "klipp.grade.wheel_sat_bri"
                    && matches!(e.value, CatalogValue::Parameter { .. })
            })
            .unwrap();
        let surfaces = rules::surfaces(wheel).unwrap();
        assert!(surfaces.contains(&Surface::GradeBottomTools));
        assert!(!surfaces.contains(&Surface::GradeTabbedRight));
        assert!(surfaces.contains(&Surface::GradePerNode));
        // Reachable since #1351: a PrimariesMatrix's From and To on its
        // own page, an InputTransform's bypass in its header on the
        // tabbed page's route.
        for (ty, param, surface) in [
            ("klipp.grade.primaries_matrix", "primaries_from", Surface::GradePerNode),
            ("klipp.grade.input_transform", "@enabled", Surface::GradeTabbedRight),
        ] {
            let entry = entries
                .iter()
                .find(|e| e.type_id == ty && e.parameter == param)
                .unwrap();
            assert!(rules::surfaces(entry).unwrap().contains(&surface), "{ty} {param}");
            assert!(rules::entry_exclusion(entry).is_none(), "{ty} {param}");
        }
    }

    /// A case on one of the sequence's own grades (its variant naming the
    /// timeline grade or a group stage, as `merge_on_root` writes it) is the
    /// generated route's and needs no fallback label (M3 step 19); a
    /// non-clip case on any other variant still does.
    /// A track's grade is excluded by name (#1408).
    #[test]
    fn a_sequence_grade_case_is_generated_and_other_roots_stay_fallback() {
        let entry = catalog()
            .into_iter()
            .find(|e| e.type_id == "klipp.grade.corrector" && e.parameter == "exposure")
            .unwrap();
        let mut manifest = Manifest::default();
        require_every_surface(&mut manifest, &entry);
        let errors = |surface, variant: &str| {
            let key = CoverageKey::new(entry.type_id, &entry.parameter, surface, variant, Interaction::OpenWithoutWrite);
            let mut report = Report::default();
            report.case("one", [key.clone()], || Ok(()));
            let all = report.complete(&manifest, std::slice::from_ref(&entry)).unwrap_err();
            (all.contains(&format!("fallback evidence not labelled: {key:?}")), all)
        };
        for root in super::super::parity_harness::GradeRoot::SEQUENCE {
            let variant = format!("{}: GradeTabbedRight sample", root.variant());
            assert!(!errors(Surface::NonClipGrade, &variant).0, "{variant}: the generated route's");
        }
        for variant in ["sample", "track: GradeTabbedRight sample", "clip: GradeTabbedRight sample"] {
            assert!(errors(Surface::NonClipGrade, variant).0, "{variant}: not a sequence grade, still fallback");
        }
        let (_, all) = errors(Surface::NonClipGrade, "timeline: GradeTabbedRight sample");
        assert!(all.contains("NonClipGrade track: #1408"), "the track exclusion is listed: {all}");
    }

    #[test]
    fn covering_one_grade_layout_cannot_hide_other_layouts_or_fallbacks() {
        let entry = catalog()
            .into_iter()
            .find(|e| e.type_id == "klipp.grade.corrector" && e.parameter == "exposure")
            .unwrap();
        let mut manifest = Manifest::default();
        manifest.require_fixture(&entry, Surface::GradeTabbedRight, "sample");
        let errors = manifest
            .rule_errors(std::slice::from_ref(&entry))
            .join("\n");
        for surface in [
            "GradeBottomTools",
            "NonClipGrade",
        ] {
            assert!(errors.contains(surface), "{surface}: {errors}");
        }
        require_every_surface(&mut manifest, &entry);
        assert!(manifest
            .rule_errors(std::slice::from_ref(&entry))
            .is_empty());
        let mut mislabeled = Report::default();
        mislabeled.case(
            "all labelled new",
            earnable(&manifest),
            || Ok(()),
        );
        assert!(mislabeled
            .complete(&manifest, std::slice::from_ref(&entry))
            .unwrap_err()
            .contains("fallback evidence not labelled"));

        let fallback = |key: &&CoverageKey| {
            key.surface == Surface::NonClipGrade
        };
        let mut report = Report::default();
        report.case(
            "generated",
            earnable(&manifest).iter().filter(|key| !fallback(key)).cloned(),
            || Ok(()),
        );
        report.case_via_fallback(
            "legacy roots",
            earnable(&manifest).iter().filter(fallback).cloned(),
            "#997: nonclip rows retain their legacy route",
            || Ok(()),
        );
        assert!(report
            .complete(&manifest, std::slice::from_ref(&entry))
            .is_ok());
        assert!(report
            .complete_for_deletion(&manifest, &[entry])
            .unwrap_err()
            .contains("legacy deletion blocked"));
    }

    #[test]
    fn absent_port_and_grade_mix_controls_are_cited_without_earned_coverage() {
        let entries: Vec<_> = catalog()
            .into_iter()
            .filter(|e| rules::entry_exclusion(e).is_some_and(|reason| reason.contains("#1348")))
            .collect();
        assert!(entries
            .iter()
            .any(|e| matches!(e.value, CatalogValue::Port { .. })));
        assert!(entries
            .iter()
            .any(|e| e.grade && matches!(e.value, CatalogValue::Mix)));
        let mut manifest = Manifest::default();
        for entry in &entries {
            manifest.require_fixture(entry, Surface::GradePerNode, "sample");
        }
        assert!(manifest.required.is_empty());
        assert!(manifest
            .exclusion_lines()
            .iter()
            .all(|line| line.contains("#1348")));
        let report = Report::default();
        assert!(report.complete(&manifest, &entries).is_ok());
        assert!(report.passed.is_empty());
        let mut new_port = entries
            .iter()
            .find(|e| matches!(e.value, CatalogValue::Port { .. }))
            .unwrap()
            .clone();
        if let CatalogValue::Port { port, .. } = &mut new_port.value {
            *port = "new_port";
        }
        assert!(
            rules::entry_exclusion(&new_port).is_none(),
            "new port controls need their own ruling"
        );
    }

    #[test]
    fn implicit_node_mix_has_no_animation_track_but_effect_mix_can_have_one() {
        let entries = catalog();
        let mix = entries
            .iter()
            .find(|e| !e.grade && matches!(e.value, CatalogValue::Mix))
            .unwrap();
        let required = rules::interactions(mix, Surface::EffectsStack).unwrap();
        assert!(required.contains(&Interaction::Drag));
        assert!(!required.iter().copied().any(rules::is_key));
        assert!(rules::interactions(mix, Surface::CurveEditor).is_err());
        let animated = entries
            .iter()
            .find(|e| {
                !e.grade
                    && e.parameter == "mix"
                    && matches!(e.value, CatalogValue::Parameter { desc, .. } if desc.keyframable)
            })
            .unwrap();
        assert!(rules::interactions(animated, Surface::CurveEditor)
            .unwrap()
            .contains(&Interaction::AddKey));
    }

    #[test]
    fn fallback_evidence_passes_parity_but_blocks_legacy_deletion() {
        let entry = slider();
        let mut manifest = Manifest::default();
        require_every_surface(&mut manifest, &entry);
        // The deletion gate demands the central representatives too.
        manifest.require_representatives();
        let mut report = Report::default();
        report.case_via_fallback(
            "old path",
            earnable(&manifest),
            "#997: legacy fixture still required",
            || Ok(()),
        );
        let json =
            serde_json::to_value(&report).expect("fallback evidence must remain machine-readable");
        assert_eq!(
            json["fallback"].as_array().unwrap().len(),
            report.fallback.len()
        );
        assert!(json["fallback"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("#997"));
        assert!(report
            .complete(&manifest, std::slice::from_ref(&entry))
            .is_ok());
        let errors = report
            .complete_for_deletion(&manifest, std::slice::from_ref(&entry))
            .unwrap_err();
        assert!(errors.contains("legacy deletion blocked"));
        assert!(errors.contains("#997: legacy fixture still required"));

        let mut converted = Report::default();
        converted.case("new path", earnable(&manifest), || Ok(()));
        assert!(converted.complete_for_deletion(&manifest, &[entry]).is_ok());
    }

    #[test]
    fn failed_fallback_cases_earn_neither_passes_nor_fallback_evidence() {
        let mut report = Report::default();
        report.case_via_fallback("failure", [key("radius")], "#997: legacy fixture", || {
            Err("assertion failed".into())
        });
        report.case_via_fallback("panic", [key("radius")], "#997: legacy fixture", || {
            panic!("fixture failed")
        });
        report.case_via_fallback("uncited", [key("radius")], "old code", || Ok(()));
        assert!(report.passed.is_empty());
        assert!(report.fallback.is_empty());
        assert_eq!(report.failures.len(), 3);
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
        require_every_surface(&mut manifest, &entry);
        assert!(
            report
                .complete(&manifest, std::slice::from_ref(&entry))
                .unwrap_err()
                .contains("missing:"),
            "registering requirements earns no evidence"
        );
        report.case(
            "assertions completed",
            earnable(&manifest),
            || Ok(()),
        );
        assert!(report.complete(&manifest, &[entry]).is_ok());
    }

    #[test]
    fn every_catalog_editor_has_a_central_rule_and_new_specialists_fail_closed() {
        for entry in catalog() {
            rules::interactions(&entry, Surface::EffectsStack)
                .unwrap_or_else(|why| panic!("{} / {}: {why}", entry.type_id, entry.parameter));
            let surfaces = rules::surfaces(&entry).unwrap_or_else(|why| panic!("{why}"));
            assert!(!surfaces.is_empty() || rules::entry_exclusion(&entry).is_some());
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
        require_every_surface(&mut manifest, &entry);
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
            earnable(&manifest),
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
    fn reviewed_key_exclusions_are_narrow_and_cite_their_decisions() {
        use klipp_effects::registry::presentation::Widget;
        let mut excluded = 0;
        let mut wheel_tracks = 0;
        let mut effect_tracks = 0;
        for entry in catalog() {
            let mut manifest = Manifest::default();
            manifest.require_fixture(&entry, Surface::GradePerNode, "sample");
            for (key, reason) in &manifest.excluded_keys {
                assert!(rules::is_key(key.interaction), "non-key exclusion: {key:?}");
                match entry.value {
                    CatalogValue::Parameter { presentation, .. } if matches!(presentation.widget, Widget::Toggle | Widget::Combo) => {
                        assert!(reason.contains("#1318"), "{key:?}: {reason}")
                    }
                    CatalogValue::Parameter { presentation, .. } if matches!(presentation.widget, Widget::WheelPart(_)) => {
                        assert!(reason.contains("#891"), "{key:?}: {reason}")
                    }
                    // A grade specialist editor (curves, the warper) draws no diamond.
                    CatalogValue::Parameter { presentation, .. } if entry.grade && matches!(presentation.widget, Widget::Custom(_)) => {
                        assert!(reason.contains("#891"), "{key:?}: {reason}")
                    }
                    _ => panic!("other widget excluded: {key:?}"),
                }
                excluded += 1;
            }
            assert!(!manifest
                .excluded_keys
                .keys()
                .any(|k| k.interaction == Interaction::Pixels));
            let CatalogValue::Parameter { desc, presentation } = entry.value else { continue };
            if !desc.keyframable {
                continue;
            }
            let mut curves = Manifest::default();
            curves.require_fixture(&entry, Surface::CurveEditor, "track");
            let actions = [Interaction::AddKey, Interaction::EditKey, Interaction::RemoveKey];
            let key = |action| CoverageKey::new(entry.type_id, &entry.parameter, Surface::CurveEditor, "track", action);
            if entry.grade && !rules::grade_curve_representative(&entry) {
                // A grade track, a wheel's parts among them: one lane by
                // path whatever its node (#891), owed on the
                // representatives, excluded centrally here.
                for action in actions.into_iter().filter(|a| curves.required.contains(&key(*a))) {
                    assert!(curves.excluded_keys.get(&key(action)).is_some_and(|r| r.contains("#891")), "{:?}", key(action));
                }
                if matches!(presentation.widget, Widget::WheelPart(_)) {
                    wheel_tracks += 1;
                }
            } else {
                // An effect track: its keys are required on the curve
                // editor, Previous/Next among them, which walk the track the
                // editor shows (#1401). None is excluded.
                for action in actions {
                    assert!(curves.required.contains(&key(action)), "{:?}", key(action));
                }
                assert!(curves.excluded_keys.is_empty(), "{:?}", curves.excluded_keys);
                effect_tracks += 1;
            }
            assert!(
                !curves.required.iter().any(|k| matches!(k.interaction, Interaction::WheelDrag(_))),
                "the curve editor does not draw a wheel"
            );
        }
        assert!(
            excluded > 0 && wheel_tracks > 0 && effect_tracks > 0,
            "exercise every catalog class"
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

    /// The central representatives are demanded whether or not a family
    /// requires them: without the masked family's require() the deletion
    /// gate reports both mask kinds missing, and with it, neither; and a
    /// representative is never read as a key outside the catalog.
    #[test]
    fn representative_obligations_are_missing_until_required() {
        let without = Manifest::default();
        let errors = Report::default().complete_for_deletion(&without, &[]).unwrap_err();
        for kind in ["@mask/in-the-grade", "@mask/beside"] {
            assert!(errors.lines().any(|e| e.starts_with("missing obligation") && e.contains(kind)), "{kind} not reported: {errors}");
        }
        let mut with = Manifest::default();
        with.require_representatives();
        assert!(with.missing_representatives().is_empty());
        assert!(!with.rule_errors(&[]).iter().any(|e| e.starts_with("required key outside catalog")), "a representative read as outside the catalog");
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
        assert!(
            !missing.iter().any(|e| e.ends_with("mask.channel")),
            "mask.channel has the central #1348 exclusion"
        );
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
