//! The central rules: what a family's subject owes, decided here and not
//! by the family, so no family can define an obligation away. A family
//! names its subjects; these rules expand each over the entries it can be
//! placed in, the routes that draw them, and the obligations its kind
//! owes (m4-readiness design, "The key" and "Central rules").

use super::{AnalysisCase, Entry, Family, InstanceCase, Key, Obligation, Outcome, Route, TimeCase};

const ROUTES: [Route; 2] = [Route::Viewer, Route::Export];

/// The entries a family's node can be placed in. A plug-in or the legacy
/// chain runs on every effect list (a clip's, a title's, an adjustment
/// layer's), as its stage does today; a source-boundary or analysis
/// family runs at a clip's source. When the catalog carries a node type's
/// placement (M4 step 1), this reads it from there instead.
fn entries(family: Family) -> &'static [Entry] {
    use Family::*;
    match family {
        Ofx | OfxTemporal | Frei0r | LegacyChain => &[Entry::Clip, Entry::Title, Entry::Adjustment],
        TemporalDenoise | FrameBlend | RollingShutter | Stabilisation | Deflicker => &[Entry::Clip],
        Format => &[Entry::Conversion],
    }
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
    for &entry in entries(family) {
        for route in ROUTES {
            for o in [Obligation::ExpectedPixels, Obligation::CpuGpuAgree, Obligation::ViewerEqualsExport] {
                out.push(key(entry, Some(route), o));
            }
            if plugin(family) {
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

    #[test]
    fn an_exclusion_must_cite_an_issue() {
        assert!(cited("#1456 (no control)"));
        assert!(!cited("no control yet"));
        assert!(!cited("# not a number"));
    }
}
