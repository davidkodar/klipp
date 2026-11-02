//! The write paths of a clip's effects and grade that don't go through
//! graph ops yet, by name (node-graph M2 step 15c). A clip's plain effects
//! are written through `Clip::apply_picture_ops` (`PictureEdit`); what is
//! left records today's whole-value entries:
//! - the kinds the clip places itself (Transform, Opacity, SpeedRamp,
//!   temporal denoise), which take no graph ops while a clip's picture is
//!   stored as fields;
//! - the grade closures and the document ops' grade builders, written
//!   whole through `set_grade` until M3's generated rows send ops.
//!
//! Each count is exact: a new constructor of one of these entries fails
//! here, and so does one that goes away, so the list is lowered with it.
//! The history's own replay (`history.rs`, `apply.rs`) builds these
//! entries to undo them and isn't a write path.

const FILES: &[(&str, &str)] = &[
    ("klipp-document/src/ops.rs", include_str!("../../klipp-document/src/ops.rs")),
    ("klipp-ui/src/app/effects.rs", include_str!("../src/app/effects.rs")),
    ("klipp-ui/src/app/inspector_edit_host.rs", include_str!("../src/app/inspector_edit_host.rs")),
    ("klipp-ui/src/app/colour.rs", include_str!("../src/app/colour.rs")),
    ("klipp-ui/src/app/compound.rs", include_str!("../src/app/compound.rs")),
    ("klipp-ui/src/app/stabilization.rs", include_str!("../src/app/stabilization.rs")),
    ("klipp-ui/src/app/grade_edits.rs", include_str!("../src/app/grade_edits.rs")),
];

/// (file, entry, count): every constructor left, tests included.
const KNOWN: &[(&str, &str, usize)] = &[
    // The whole-list write the placed kinds fall back to (and reframe's
    // Transform), and the tests that check for it, for its refusal, and
    // that it undoes exactly on a picture-truth clip (#2459).
    ("klipp-document/src/ops.rs", "SetBuiltinEffects", 5),
    // The document ops' grade builders (grade, look, qualify, zones, track
    // and stage grades, the opacity gradient #2704) and their tests (the
    // zones placed for their source and the foliage key, #2767, among
    // them), and the test that a grade change undoes exactly on a
    // picture-truth clip (#2459), and a split shot's grade copied whole
    // onto its other pieces (on: "shot"), as `from` copies one (#3390).
    ("klipp-document/src/ops.rs", "SetNodeGraph", 29),
    // A drag left open, closed on the state alone (M3 step 2). The list
    // handlers and their drags go through the picture (M4 step 12c-5).
    ("klipp-ui/src/app/effects.rs", "EditBuiltinEffect", 2),
    // The grade closures (node edits, balance, a look) and
    // their tests (the zones the colour page and the op place alike,
    // #2767, among them).
    ("klipp-ui/src/app/colour.rs", "SetNodeGraph", 9),
    // A compound's grade moved in and out with its clips.
    // Keys generated from a track, on a grade window.
    ("klipp-ui/src/app/stabilization.rs", "SetNodeGraph", 3),
    // A still's grade put on clips, and its tests.
    ("klipp-ui/src/app/grade_edits.rs", "SetNodeGraph", 3),
];

const ENTRIES: [&str; 4] = ["SetBuiltinEffects", "SetNodeGraph", "EditBuiltinEffect", "ToggleBuiltinEffectEnabled"];

#[test]
fn the_picture_writes_left_off_graph_ops_are_the_known_ones() {
    let mut found = Vec::new();
    for (file, text) in FILES {
        for entry in ENTRIES {
            let n = text.matches(&format!("EditCommand::{entry} {{")).count();
            if n > 0 {
                found.push((*file, entry, n));
            }
        }
    }
    let known: Vec<(&str, &str, usize)> = KNOWN.to_vec();
    assert_eq!(found, known, "a write path of a clip's picture left graph ops, or one more moved onto them: update KNOWN with it");
}
