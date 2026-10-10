//! The conversion to revision 4 keeps every entry's stored node id
//! (`revision4::keep_ids`): a frei0r and an OFX plug-in (whose stored
//! forms change) and a built-in Transform, each from a genuine revision-3
//! file (the Format family's fixtures), have the same ids after.

use std::collections::BTreeMap;

use klipp_convert::step4;
use serde_json::Value;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../klipp-readiness/fixtures/rev3");

/// Each entry of the first clip's picture with a list place: its place
/// (plug-in or built-in, slot) and id.
fn ids(project: &Value) -> BTreeMap<(bool, u64), String> {
    let nodes = project["sequences"][0]["tracks"][0]["clips"][0]["graph_v2"]["graph"]["nodes"].as_array().unwrap();
    nodes
        .iter()
        .filter_map(|n| {
            let slot = n["legacy"]["slot"].as_u64()?;
            let t = n["type_id"].as_str()?;
            Some(((t == "klipp.legacy.ofx" || t == "klipp.ofx", slot), n["id"].as_str()?.to_owned()))
        })
        .collect()
}

#[test]
fn every_entry_keeps_its_stored_id() {
    for name in ["klipp.frei0r", "klipp.ofx", "row1394"] {
        let text = std::fs::read_to_string(format!("{FIXTURES}/{name}/project.json")).unwrap();
        let before: Value = serde_json::from_str(&text).unwrap();
        let after: Value = serde_json::from_str(&step4::convert(&text).unwrap_or_else(|e| panic!("{name}: {e}")).text).unwrap();
        let (a, b) = (ids(&before), ids(&after));
        assert!(!a.is_empty(), "{name}: entries to keep");
        assert_eq!(a, b, "{name}: every entry's id kept by its place");
    }
}
