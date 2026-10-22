//! M4's readiness gate (M4 plan v2 §2 and step 0): one manifest of what
//! every execution family owes, the central rules that decide it, and a
//! report of what the families' tests earn.
//!
//! A family is one of M4's execution families (OFX, frei0r, temporal
//! denoise, frame blend, the analysis effects, the legacy chain) or the
//! picture-format revision. What it owes is a [`Key`]: a subject (a node
//! type, a fixture, a format row) on an entry it can be placed in, drawn
//! by a route, owing an [`Obligation`]. The families don't choose their
//! keys: they name their subjects, and the central rules ([`rules`])
//! expand those over the entries and routes they owe and the obligations
//! their kind owes. Nothing is earned by comparison with an old route:
//! every case is held to the result the step's own test computes.
//!
//! `m4_readiness` (in `tests/`) is the gate: it runs every family and
//! passes when nothing is open. Cheap tests beside it hold the open list
//! to a pinned one that only shrinks, every family registered, and every
//! deleted stage gone.

mod analysis;
pub mod budget;
pub mod residency;
pub mod families;
pub mod fixtures;
pub mod format;
mod frame_blend;
pub mod frei0r;
mod legacy_chain;
mod ofx;
mod ofx_temporal;
pub mod old_stage;
pub mod route;
pub mod route2;
mod on_file;
pub mod rules;
pub mod saved;
mod temporal_denoise;

use std::collections::{BTreeMap, BTreeSet};

/// One of M4's execution families, or the picture-format revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Family {
    Ofx,
    OfxTemporal,
    Frei0r,
    TemporalDenoise,
    FrameBlend,
    RollingShutter,
    Stabilisation,
    Deflicker,
    LegacyChain,
    Format,
}

/// Where a node can be placed: the paths its stage was reached on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Entry {
    Clip,
    Title,
    Adjustment,
    /// The format revision's conversion, for [`Family::Format`].
    Conversion,
}

/// The routes that draw an entry: the viewer's and export's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Route {
    Viewer,
    Export,
}

/// A time case a temporal or source-neighbour family owes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TimeCase {
    TrimmedStart,
    TrimmedEnd,
    Reversed,
    SpeedRamped,
    OutOfRange,
    BackwardSeek,
}

/// An instance case a plug-in family owes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InstanceCase {
    TwoNodesOfOnePlugin,
    TwoClips,
    ViewerAndExportTogether,
    ResetOnSeek,
    ResetOnReverse,
    ResetOnResolution,
    ResetOnSchema,
}

/// Step 10's outcomes, each owed distinct and visible.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Outcome {
    Bypass,
    NeighbourMissing,
    PluginMissing,
    Unsupported,
    RenderFailed,
}

/// An analysis case an analysis family owes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AnalysisCase {
    Absent,
    Valid,
    Stale,
    Alternating,
    InvalidatedByEdit,
    KeptByEdit,
    Split,
    Duplicate,
    Relink,
    Proxy,
}

/// What a key owes (m4-readiness design, "The key").
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Obligation {
    /// The node's frame equals the expectation the test computes.
    ExpectedPixels,
    /// Both routes agree within pixel_check's bar, or the key is a
    /// declared local CPU segment (its rules cite the step).
    CpuGpuAgree,
    /// The viewer's frame is export's.
    ViewerEqualsExport,
    /// A disabled node passes its input on with no conversion.
    DisabledIsIdentity,
    Time(TimeCase),
    Instances(InstanceCase),
    Outcome(Outcome),
    Analysis(AnalysisCase),
    /// Save and reopen keep the authored data.
    Saved,
    /// The family's frame time against the same scene with no effect,
    /// within its budget, with no layer drawn on the CPU but the
    /// declared one (run by the gate only).
    FrameBudget,
    /// The family's deleted stage is in the source scan, which passes.
    OldStageGone,
    /// The conversion is lossless on the corpus fixtures.
    Converted,
    /// Every project the row changes is counted and listed.
    DeclaredChange,
}

/// One thing a family owes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key {
    pub family: Family,
    /// The node type, fixture or format row.
    pub subject: String,
    pub entry: Entry,
    /// `None` for an obligation no route draws (Saved, OldStageGone, the
    /// format's).
    pub route: Option<Route>,
    pub obligation: Obligation,
}

/// What every family owes, and the keys excluded with their issue.
#[derive(Default)]
pub struct Manifest {
    pub required: BTreeSet<Key>,
    pub excluded: BTreeMap<Key, String>,
    /// Families a rule refused to expand, with why.
    pub errors: Vec<String>,
}

impl Manifest {
    /// `subject` of `family` owed what the central rules say it owes.
    pub fn require(&mut self, family: Family, subject: &str) {
        match rules::keys(family, subject) {
            Ok(keys) => self.required.extend(keys),
            Err(e) => self.errors.push(format!("{family:?} / {subject}: {e}")),
        }
    }

    /// `key` owed, and excluded with `reason`, which must cite an issue.
    pub fn exclude(&mut self, key: Key, reason: &str) {
        if !rules::cited(reason) {
            self.errors.push(format!("uncited exclusion: {key:?}: {reason}"));
            return;
        }
        self.required.insert(key.clone());
        self.excluded.insert(key, reason.into());
    }
}

/// What the families' cases earned.
#[derive(Default)]
pub struct Report {
    pub passed: BTreeSet<Key>,
    pub failures: Vec<String>,
    /// Each format row's declared changes: what of which project it
    /// changes, for the report's DECLARED-CHANGE lines (#1465).
    pub declared: BTreeMap<String, Vec<String>>,
}

impl Report {
    /// `row` declares that it changes `what` (a project, a clip): listed
    /// apart, never counted as earned or missing.
    pub fn declare(&mut self, row: &str, what: impl Into<String>) {
        self.declared.entry(row.into()).or_default().push(what.into());
    }

    /// A case: `run` held to its expectation earns `keys` when it passes.
    pub fn case(&mut self, name: &str, keys: impl IntoIterator<Item = Key>, run: impl FnOnce() -> Result<(), String>) {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)) {
            Ok(Ok(())) => self.passed.extend(keys),
            Ok(Err(e)) => self.failures.push(format!("{name}: {e}")),
            Err(_) => self.failures.push(format!("{name}: panicked")),
        }
    }

    /// What's open: each key owed and not earned nor excluded, each earned
    /// and not owed, each failure, each rule error. Sorted.
    pub fn open(&self, manifest: &Manifest) -> Vec<String> {
        let mut out: Vec<String> = manifest.errors.iter().map(|e| format!("rule: {e}")).collect();
        out.extend(self.failures.iter().map(|f| format!("failed: {f}")));
        out.extend(manifest.required.iter().filter(|k| !self.passed.contains(*k) && !manifest.excluded.contains_key(*k)).map(|k| format!("missing: {k:?}")));
        out.extend(self.passed.iter().filter(|k| !manifest.required.contains(*k)).map(|k| format!("unexpected: {k:?}")));
        out.extend(self.passed.iter().filter(|k| manifest.excluded.contains_key(*k)).map(|k| format!("excluded and earned: {k:?}")));
        out.sort();
        out
    }

    /// The report's head line and its lines, for the report file.
    pub fn listing(&self, manifest: &Manifest) -> String {
        // Each owed key counted once: excluded (whether or not a case also
        // earned it, which `open` reports), else earned, else missing.
        let required = manifest.required.len();
        let excluded = manifest.excluded.len();
        let earned = manifest.required.iter().filter(|k| self.passed.contains(*k) && !manifest.excluded.contains_key(*k)).count();
        let open = self.open(manifest);
        let mut by_family: BTreeMap<Family, usize> = BTreeMap::new();
        for k in manifest.required.iter().filter(|k| !self.passed.contains(*k) && !manifest.excluded.contains_key(*k)) {
            *by_family.entry(k.family).or_default() += 1;
        }
        let mut lines = vec![format!("required {required} = earned {earned} + excluded {excluded} + missing {}; open {}", required - earned - excluded, open.len())];
        lines.extend(by_family.iter().map(|(f, n)| format!("missing {f:?}: {n}")));
        lines.push(String::new());
        lines.extend(open);
        lines.extend(manifest.excluded.iter().map(|(k, r)| format!("EXCLUDED {k:?}: {r}")));
        lines.extend(self.declared.iter().map(|(row, what)| format!("DECLARED-CHANGE {row}: {} ({})", what.len(), what.join(", "))));
        lines.join("\n")
    }
}

/// Every family's manifest and every family's cases (`families`). A
/// catalog type M4 makes a node that no family declares is a rule error:
/// the gate fails closed on a type left out.
pub fn manifest() -> Manifest {
    let mut m = Manifest::default();
    for family in families::ALL {
        (family.require)(&mut m);
    }
    for id in rules::undeclared(families::subjects()) {
        m.errors.push(format!("no family declares {id}"));
    }
    m
}

/// Every family's cases run into one report.
pub fn run_all() -> Report {
    let mut r = Report::default();
    for family in families::ALL {
        (family.run)(&mut r);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key both excluded and earned is counted once, as excluded, and
    /// reported open: the head line adds up and the listing is written.
    #[test]
    fn a_key_excluded_and_earned_is_counted_once_and_reported() {
        let mut m = Manifest::default();
        m.require(Family::LegacyChain, "s");
        let key = m.required.iter().next().unwrap().clone();
        m.exclude(key.clone(), "#1 (a reason)");
        let mut r = Report::default();
        r.case("earns the excluded key", [key], || Ok(()));
        let listing = r.listing(&m);
        let required = m.required.len();
        assert!(listing.starts_with(&format!("required {required} = earned 0 + excluded 1 + missing {}", required - 1)), "{listing}");
        assert!(r.open(&m).iter().any(|l| l.starts_with("excluded and earned")));
    }
}
