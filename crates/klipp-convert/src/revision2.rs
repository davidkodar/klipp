//! The revision-2 conversion (`klipp_graph_lift::revision2`), and the
//! checks the dry run and the run make on what they wrote, which hold a
//! written file to the frozen reader's view of the original.

pub use klipp_graph_lift::revision2::*;

use klipp_graph_lift::{old_fields, revision};
use serde_json::Value;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Prove a written revision-2 project: it reads back at revision 2, its
/// sentinels strip, every store's `graph_v2` is exactly the lift of the
/// original store, and the rest of the project is the original's, field
/// by field.
pub fn verify_project(original: &Value, written: &str) -> Result<(), String> {
    let mut back: Value = serde_json::from_str(written).map_err(|e| format!("the written file doesn't read back: {e}"))?;
    let r = revision::read_revision(&back).map_err(err)?;
    if r != REVISION {
        return Err(format!("written with revision {r}, not {REVISION}"));
    }
    old_fields::strip_sentinels(&mut back).map_err(err)?;
    let (expected, _) = project(original)?;
    let mut expected = expected;
    old_fields::strip_sentinels(&mut expected).map_err(err)?;
    if back != expected {
        return Err(format!("the written project isn't the conversion of the original: {}", crate::verify::first_diff(&back, &expected, "").unwrap_or_default()));
    }
    let (o, b) = (crate::assemble::without_pictures(original), crate::assemble::without_pictures(&back));
    if o != b {
        return Err(format!("outside the pictures: {}", crate::verify::first_diff(&b, &o, "").unwrap_or_default()));
    }
    Ok(())
}

/// Prove a written revision-2 side store against its original: the same
/// conversion, and nothing else changed.
pub fn verify_side(converted_again: &Value, written: &str) -> Result<(), String> {
    let back: Value = serde_json::from_str(written).map_err(|e| format!("the written file doesn't read back: {e}"))?;
    if back.get("format_revision").and_then(Value::as_u64) != Some(REVISION) {
        return Err("a side store not at revision 2".into());
    }
    if &back != converted_again {
        return Err(format!("the written file isn't the conversion: {}", crate::verify::first_diff(&back, converted_again, "").unwrap_or_default()));
    }
    Ok(())
}
