//! M4's families: each names the subjects it owes (the central rules
//! decide the rest) and runs the cases that earn them. At M4 step 0 every
//! family exists with its subjects and no cases, so the first report shows
//! the whole of M4 as open; a step's test-first change adds its cases
//! (`#[ignore]`d or pinned until the port), and the port earns them. The
//! step's owner keeps its family.

use super::{Family, Manifest, Report};

/// A family: its subjects, and its cases.
pub struct FamilyEntry {
    pub family: Family,
    pub require: fn(&mut Manifest),
    pub run: fn(&mut Report),
}

/// The subjects a family owes, by name: a node type to come, a native
/// fixture, or a format row. Owners rename a placeholder subject to the
/// node type id when its step lands it.
const SUBJECTS: &[(Family, &[&str])] = &[
    // M4 step 3: the native OFX fixtures (#932's), as nodes.
    (Family::Ofx, &["ofx.fixture.invert", "ofx.fixture.alpha", "ofx.fixture.series"]),
    // M4 step 4: a temporal OFX fixture that records the times it asked for.
    (Family::OfxTemporal, &["ofx.fixture.time_request"]),
    // M4 step 5: the native frei0r fixtures (step 2's).
    (Family::Frei0r, &["frei0r.fixture.colour", "frei0r.fixture.stateful", "frei0r.fixture.tiled"]),
    // M4 step 6.
    (Family::TemporalDenoise, &["klipp.temporal_denoise"]),
    // M4 step 7.
    (Family::FrameBlend, &["klipp.frame_blend"]),
    // M4 step 9.
    (Family::RollingShutter, &["klipp.fx.rolling_shutter_correction"]),
    (Family::Stabilisation, &["klipp.fx.video_stabilization"]),
    (Family::Deflicker, &["klipp.fx.deflicker"]),
    // M4 step 11: the legacy chain's last callers (Equalize and the
    // exempt types, m2-step11-deletion-map §2 and §3).
    (Family::LegacyChain, &["klipp.fx.equalize"]),
    // M4 step F: picture revision 4, one conversion (M4 plan v2 §4).
    (Family::Format, &["#1391", "#1406", "#1409", "#1386", "#1394", "#1321", "klipp.frei0r", "klipp.ofx"]),
];

/// Every family's subjects.
pub fn subjects() -> impl Iterator<Item = &'static str> {
    SUBJECTS.iter().flat_map(|(_, s)| s.iter().copied())
}

fn require_family(family: Family, manifest: &mut Manifest) {
    for (f, subjects) in SUBJECTS {
        if *f == family {
            for subject in *subjects {
                manifest.require(family, subject);
            }
        }
    }
}

/// A family with no cases yet: its step's test-first change adds them.
fn none_yet(_: &mut Report) {}

macro_rules! family {
    ($f:ident, $require:ident) => {
        fn $require(m: &mut Manifest) {
            require_family(Family::$f, m)
        }
    };
}

family!(Ofx, require_ofx);
family!(OfxTemporal, require_ofx_temporal);
family!(Frei0r, require_frei0r);
family!(TemporalDenoise, require_temporal_denoise);
family!(FrameBlend, require_frame_blend);
family!(RollingShutter, require_rolling_shutter);
family!(Stabilisation, require_stabilisation);
family!(Deflicker, require_deflicker);
family!(LegacyChain, require_legacy_chain);
family!(Format, require_format);

/// Every family, registered: the gate runs these, and a cheap test holds
/// every `Family` to having one.
pub const ALL: &[FamilyEntry] = &[
    FamilyEntry { family: Family::Ofx, require: require_ofx, run: none_yet },
    FamilyEntry { family: Family::OfxTemporal, require: require_ofx_temporal, run: none_yet },
    FamilyEntry { family: Family::Frei0r, require: require_frei0r, run: none_yet },
    FamilyEntry { family: Family::TemporalDenoise, require: require_temporal_denoise, run: crate::temporal_denoise::run },
    FamilyEntry { family: Family::FrameBlend, require: require_frame_blend, run: none_yet },
    FamilyEntry { family: Family::RollingShutter, require: require_rolling_shutter, run: none_yet },
    FamilyEntry { family: Family::Stabilisation, require: require_stabilisation, run: none_yet },
    FamilyEntry { family: Family::Deflicker, require: require_deflicker, run: none_yet },
    FamilyEntry { family: Family::LegacyChain, require: require_legacy_chain, run: none_yet },
    FamilyEntry { family: Family::Format, require: require_format, run: super::format::run },
];

/// The renderer stages each family deletes, by symbol, as its step lands
/// (M4 plan v2 §5, step 11's scan): a family adds its symbols in the
/// change that deletes them, and `old_stages_stay_gone` holds them gone.
pub const DELETED: &[(Family, &[&str])] = &[
    // Step 6: the renderer's pre-grade denoise stage and its per-clip
    // neighbour readers, replaced by the source-boundary node.
    (Family::TemporalDenoise, &["fn temporal_denoise(", "tnr_frames"]),
];
