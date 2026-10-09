//! The central rules: what a family's subject owes, decided here and not
//! by the family, so no family can define an obligation away. A family
//! names its subjects; these rules expand each over the entries it can be
//! placed in, the routes that draw them, and the obligations its kind
//! owes (m4-readiness design, "The key" and "Central rules").

use super::{AnalysisCase, Entry, Family, InstanceCase, Key, Obligation, Outcome, Route, TimeCase};
use klipp_effects::registry::{effect_type, effect_types, ColourDomain, NodeTypeDesc, Placement, Temporal};
use klipp_graph_format::lifted::types as lifted;

const ROUTES: [Route; 2] = [Route::Viewer, Route::Export];

/// The source-boundary node types a catalog effect is lifted to, by the
/// effect's tag: a subject named by its node type is that effect's.
const LIFTED: &[(&str, &str)] = &[(lifted::TEMPORAL_DENOISE, "temporal_denoise"), (lifted::FRAME_BLEND, "speed_ramp")];

/// What the catalog says of `subject`: the effect type it is, or the one
/// its node type is lifted from. `None` for a fixture or a format row.
fn catalog_type(subject: &str) -> Option<&'static NodeTypeDesc> {
    let lifted_from = LIFTED.iter().find(|(node, _)| *node == subject).and_then(|(_, tag)| effect_type(tag));
    effect_types().find(|t| t.desc.id == subject).or(lifted_from).map(|t| &t.desc)
}

/// The entries `subject` can be placed in: its catalog type's placement.
/// A subject the catalog doesn't describe yet (a plug-in fixture, until
/// its step makes it a node type) runs on every effect list, as its stage
/// does today; a format row on its conversion.
fn entries(family: Family, subject: &str) -> Vec<Entry> {
    if family == Family::Format {
        return vec![Entry::Conversion];
    }
    match catalog_type(subject) {
        Some(desc) => desc
            .placement()
            .iter()
            .map(|p| match p {
                Placement::Clip => Entry::Clip,
                Placement::Title => Entry::Title,
                Placement::Adjustment => Entry::Adjustment,
            })
            .collect(),
        None => vec![Entry::Clip, Entry::Title, Entry::Adjustment],
    }
}

/// Whether `subject` converts the picture's encoding to compute (its
/// catalog domain isn't linear light), so switched off it must still pass
/// its input on unconverted.
fn converts(subject: &str) -> bool {
    catalog_type(subject).is_some_and(|d| d.domain != ColourDomain::Linear)
}

/// The catalog's node types that read other times than their frame's,
/// which M4 makes nodes, that no subject in `declared` names (by its id or
/// the node type it's lifted to): each one an M4 family must declare.
pub fn undeclared<'a>(declared: impl IntoIterator<Item = &'a str>) -> Vec<&'static str> {
    let declared: Vec<&str> = declared.into_iter().collect();
    effect_types()
        .filter(|t| t.desc.temporal != Temporal::None)
        .filter(|t| !declared.iter().any(|s| catalog_type(s).is_some_and(|d| d.id == t.desc.id)))
        .map(|t| t.desc.id)
        .collect()
}

/// Whether a family's node is a plug-in's.
fn plugin(family: Family) -> bool {
    matches!(family, Family::Ofx | Family::OfxTemporal | Family::Frei0r)
}

/// Whether a family reads frames other than the one it draws.
fn temporal(family: Family) -> bool {
    matches!(family, Family::OfxTemporal | Family::TemporalDenoise | Family::FrameBlend | Family::RollingShutter | Family::Stabilisation | Family::Deflicker)
}

/// Whether a family reads an analysis.
fn analysis(family: Family) -> bool {
    matches!(family, Family::RollingShutter | Family::Stabilisation | Family::Deflicker)
}

/// Whether a family moves off the CPU in M4, owing a frame-time budget:
/// frei0r's local segment (step 5), denoise (step 6) and the analysis
/// effects (step 9).
fn moves_off_the_cpu(family: Family) -> bool {
    matches!(family, Family::Frei0r | Family::TemporalDenoise | Family::RollingShutter | Family::Stabilisation | Family::Deflicker)
}

/// Whether a family declares a capability limit (a frei0r plug-in's
/// sizes, an OFX plug-in's contexts), owing the Unsupported outcome.
fn limited(family: Family) -> bool {
    plugin(family)
}

/// Everything `subject` of `family` owes.
pub fn keys(family: Family, subject: &str) -> Result<Vec<Key>, String> {
    if subject.trim().is_empty() {
        return Err("a subject needs a name".into());
    }
    let key = |entry: Entry, route: Option<Route>, obligation: Obligation| Key { family, subject: subject.into(), entry, route, obligation };
    let mut out = Vec::new();
    if family == Family::Format {
        // A format row: the conversion lossless, its changes counted and
        // listed (decision 1 of the M4 plan), the converted file kept.
        for o in [Obligation::Converted, Obligation::DeclaredChange, Obligation::Saved] {
            out.push(key(Entry::Conversion, None, o));
        }
        return Ok(out);
    }
    // Every node family, on every entry and route that can draw it.
    for entry in entries(family, subject) {
        for route in ROUTES {
            for o in [Obligation::ExpectedPixels, Obligation::CpuGpuAgree, Obligation::ViewerEqualsExport] {
                out.push(key(entry, Some(route), o));
            }
            if plugin(family) || converts(subject) {
                out.push(key(entry, Some(route), Obligation::DisabledIsIdentity));
            }
        }
    }
    // Its outcomes (step 10), on a clip, each route: a bypass and a render
    // failure for every family; a missing plug-in for a plug-in's; a
    // missing neighbour for a temporal one; an unsupported capability for
    // one that declares limits.
    let mut outcomes = vec![Outcome::Bypass, Outcome::RenderFailed];
    if plugin(family) {
        outcomes.push(Outcome::PluginMissing);
    }
    if temporal(family) {
        outcomes.push(Outcome::NeighbourMissing);
    }
    if limited(family) {
        outcomes.push(Outcome::Unsupported);
    }
    for route in ROUTES {
        for o in &outcomes {
            out.push(key(Entry::Clip, Some(route), Obligation::Outcome(*o)));
        }
    }
    if temporal(family) {
        use TimeCase::*;
        for route in ROUTES {
            for t in [TrimmedStart, TrimmedEnd, Reversed, SpeedRamped, OutOfRange, BackwardSeek] {
                out.push(key(Entry::Clip, Some(route), Obligation::Time(t)));
            }
        }
    }
    if plugin(family) {
        use InstanceCase::*;
        for i in [TwoNodesOfOnePlugin, TwoClips, ViewerAndExportTogether, ResetOnSeek, ResetOnReverse, ResetOnResolution, ResetOnSchema] {
            out.push(key(Entry::Clip, None, Obligation::Instances(i)));
        }
    }
    if analysis(family) {
        use AnalysisCase::*;
        for a in [Absent, Valid, Stale, Alternating, InvalidatedByEdit, KeptByEdit, Split, Duplicate, Relink, Proxy] {
            out.push(key(Entry::Clip, None, Obligation::Analysis(a)));
        }
    }
    if moves_off_the_cpu(family) {
        out.push(key(Entry::Clip, Some(Route::Viewer), Obligation::FrameBudget));
    }
    out.push(key(Entry::Clip, None, Obligation::Saved));
    out.push(key(Entry::Clip, None, Obligation::OldStageGone));
    Ok(out)
}

/// Whether `reason` cites an issue (`#` and a number).
pub fn cited(reason: &str) -> bool {
    reason.char_indices().any(|(i, c)| c == '#' && reason[i + 1..].starts_with(|d: char| d.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plug-in owes its instances and a missing plug-in; a source family
    /// doesn't, and owes the time cases instead; a format row owes only
    /// its conversion's.
    #[test]
    fn each_kind_owes_its_own_obligations() {
        let has = |f: Family, o: Obligation| keys(f, "s").unwrap().iter().any(|k| k.obligation == o);
        assert!(has(Family::Frei0r, Obligation::Instances(InstanceCase::TwoClips)));
        assert!(has(Family::Frei0r, Obligation::Outcome(Outcome::PluginMissing)));
        assert!(!has(Family::TemporalDenoise, Obligation::Outcome(Outcome::PluginMissing)));
        assert!(has(Family::TemporalDenoise, Obligation::Time(TimeCase::Reversed)));
        assert!(has(Family::Stabilisation, Obligation::Analysis(AnalysisCase::Stale)));
        assert!(has(Family::Stabilisation, Obligation::FrameBudget));
        assert!(!has(Family::Ofx, Obligation::FrameBudget));
        assert!(keys(Family::Format, "#1406").unwrap().iter().all(|k| k.entry == Entry::Conversion && k.route.is_none()));
    }

    /// A plug-in is owed on a title and an adjustment layer too, both
    /// routes: a family finished on the clip in the viewer alone isn't.
    #[test]
    fn a_plugin_is_owed_on_every_entry_and_route() {
        let k = keys(Family::Ofx, "s").unwrap();
        for entry in [Entry::Clip, Entry::Title, Entry::Adjustment] {
            for route in ROUTES {
                assert!(k.iter().any(|k| k.entry == entry && k.route == Some(route) && k.obligation == Obligation::ExpectedPixels), "{entry:?} {route:?}");
            }
        }
    }

    /// A family's entries are its catalog type's placement: a node that
    /// reads the source's other times on a clip only, one that doesn't on
    /// every effect list; a subject named by its lifted node type is its
    /// effect's.
    #[test]
    fn a_familys_entries_are_the_catalogs_placement() {
        let entries_of = |f: Family, s: &str| keys(f, s).unwrap().iter().filter(|k| k.obligation == Obligation::ExpectedPixels).map(|k| k.entry).collect::<std::collections::BTreeSet<_>>();
        for (family, subject) in [(Family::TemporalDenoise, lifted::TEMPORAL_DENOISE), (Family::FrameBlend, lifted::FRAME_BLEND), (Family::Deflicker, "klipp.fx.deflicker"), (Family::LegacyChain, "klipp.fx.equalize")] {
            let desc = catalog_type(subject).unwrap_or_else(|| panic!("{subject} isn't in the catalog"));
            let placed: std::collections::BTreeSet<Entry> = entries(family, subject).into_iter().collect();
            assert_eq!(entries_of(family, subject), placed, "{subject}");
            assert_eq!(placed.len(), desc.placement().len(), "{subject}");
        }
        assert_eq!(entries_of(Family::Stabilisation, "klipp.fx.video_stabilization"), [Entry::Clip].into());
        assert_eq!(entries_of(Family::LegacyChain, "klipp.fx.equalize"), [Entry::Clip, Entry::Title, Entry::Adjustment].into());
    }

    /// Switched off, a node that converts the picture's encoding owes its
    /// input passed on unconverted, as a plug-in does; a linear one doesn't.
    #[test]
    fn a_domain_conversion_owes_the_disabled_identity() {
        let has = |f: Family, s: &str| keys(f, s).unwrap().iter().any(|k| k.obligation == Obligation::DisabledIsIdentity);
        assert!(has(Family::LegacyChain, "klipp.fx.equalize"));
        assert!(has(Family::Frei0r, "frei0r.fixture.colour"));
        assert!(!has(Family::Deflicker, "klipp.fx.deflicker"));
    }

    /// A catalog type that reads other times and no family names is
    /// undeclared; named by its id or its lifted node type, it isn't.
    #[test]
    fn an_undeclared_catalog_type_is_found() {
        let all = undeclared([]);
        for id in ["klipp.fx.temporal_denoise", "klipp.fx.speed_ramp", "klipp.fx.deflicker", "klipp.fx.video_stabilization", "klipp.fx.rolling_shutter_correction"] {
            assert!(all.contains(&id), "{id} not found undeclared: {all:?}");
        }
        assert!(!all.contains(&"klipp.fx.equalize"), "it reads its frame only");
        let left = undeclared([lifted::TEMPORAL_DENOISE, "klipp.fx.deflicker"]);
        assert!(!left.contains(&"klipp.fx.temporal_denoise") && !left.contains(&"klipp.fx.deflicker"));
        assert!(left.contains(&"klipp.fx.speed_ramp"));
    }

    #[test]
    fn an_exclusion_must_cite_an_issue() {
        assert!(cited("#1456 (no control)"));
        assert!(!cited("no control yet"));
        assert!(!cited("# not a number"));
    }
}
