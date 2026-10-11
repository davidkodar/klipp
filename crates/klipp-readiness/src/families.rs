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
    // M4 step 3: the native OFX fixtures (#932's), as nodes; a series is
    // channels then alpha in one clip.
    (Family::Ofx, &["org.klipp.fixture.identity", "org.klipp.fixture.channels", "org.klipp.fixture.alpha"]),
    // M4 step 4: a temporal OFX fixture that records the times it asked for.
    (Family::OfxTemporal, &[crate::ofx_temporal::SUBJECT]),
    // M4 step 5: the native frei0r fixtures (step 2's).
    (Family::Frei0r, &[crate::frei0r::COLOUR, crate::frei0r::STATEFUL, crate::frei0r::TILED]),
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
    FamilyEntry { family: Family::Ofx, require: require_ofx, run: crate::ofx::run },
    FamilyEntry { family: Family::OfxTemporal, require: require_ofx_temporal, run: crate::ofx_temporal::run },
    FamilyEntry { family: Family::Frei0r, require: require_frei0r, run: crate::frei0r::run },
    FamilyEntry { family: Family::TemporalDenoise, require: require_temporal_denoise, run: crate::temporal_denoise::run },
    FamilyEntry { family: Family::FrameBlend, require: require_frame_blend, run: crate::frame_blend::run },
    FamilyEntry { family: Family::RollingShutter, require: require_rolling_shutter, run: crate::analysis::run_rolling_shutter },
    FamilyEntry { family: Family::Stabilisation, require: require_stabilisation, run: crate::analysis::run_stabilisation },
    FamilyEntry { family: Family::Deflicker, require: require_deflicker, run: crate::analysis::run_deflicker },
    FamilyEntry { family: Family::LegacyChain, require: require_legacy_chain, run: crate::legacy_chain::run },
    FamilyEntry { family: Family::Format, require: require_format, run: super::format::run },
];

/// The renderer stages each family deletes, by symbol, as its step lands
/// (M4 plan v2 §5, step 11's scan): a family adds its symbols in the
/// change that deletes them, and `old_stages_stay_gone` holds them gone.
pub const DELETED: &[(Family, &[&str])] = &[
    // Step 6: the renderer's pre-grade denoise stage and its per-clip
    // neighbour readers, replaced by the source-boundary node.
    (Family::TemporalDenoise, &["fn temporal_denoise(", "tnr_frames"]),
    // Step 7: the renderer's pre-grade frame blend.
    (Family::FrameBlend, &["fn blend_frames("]),
    // Step 3: the renderer's OFX stage (a clip's plug-ins as one chain on
    // the instance shared by name) and its temporal-plug-in name guess,
    // replaced by a node per plug-in with an instance of its own.
    (Family::Ofx, &["fn ofx(", "is_temporal_plugin"]),
    // Step 4: the renderer's temporal-plug-in name guess (with step 3), and
    // the OFX chain's temporal entry, whose plug-in was told the current
    // frame for a neighbour it wasn't given; a temporal plug-in runs as a
    // node, handed its own frames.
    (Family::OfxTemporal, &["is_temporal_plugin", "fn apply_effects_chain_temporal(", "missing_is_missing"]),
    // Step 5: the effect chain's GPU veto on a chain with a frei0r plug-in,
    // replaced by a frei0r node of its own, a local CPU segment.
    (Family::Frei0r, &["chain_supports_gpu"]),
    // Step 9: a clip's analysis fields (an analysis is its media's asset
    // now), and the renderer's own rolling-shutter measuring stage.
    (Family::Deflicker, &["deflicker_data", "deflicker_gains"]),
    (Family::Stabilisation, &[".stabilization_data", "stabilization_data:", "smoothed_trajectory"]),
    (Family::RollingShutter, &["fn prepare_temporal_effects(", "fn motion_frame("]),
    // Step 11: the GPU's legacy effect loop (a clip's or layer's effects
    // run outside a plan, recorded runs of their own, the unrecorded layer
    // passes), replaced by effects as plan nodes; Equalize keeps its one
    // pass, a break of the chain.
    (
        Family::LegacyChain,
        &[
            "fn run_effects_in_chain(",
            "fn run_effects_in_chain_from(",
            "struct RecordedRun",
            "fn submit_recorded(",
            "fn record_into(",
            "fn record_spills_into(",
            "fn stack_layer_apply_effects(",
            "fn effects_on_output(",
            "fn download_chain_slot_linear(",
            "fn layer_corners(",
            "fn layer_fill(",
        ],
    ),
];
