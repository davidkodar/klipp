//! The revision-2 conversion (`klipp_graph_lift::revision2`), and the
//! checks the dry run and the run make on what they wrote, which hold a
//! written file to the frozen reader's view of the original.

pub use klipp_graph_lift::revision2::*;

use klipp_graph_lift::old_fields::{self, V2_FIELD};
use klipp_graph_lift::revision;
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
    if r != PROJECT_REVISION {
        return Err(format!("written with revision {r}, not {PROJECT_REVISION}"));
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

/// `o` without the keys a conversion replaces.
fn without(o: &Value, keys: &[&str]) -> Value {
    let mut m = o.as_object().cloned().unwrap_or_default();
    for k in keys {
        m.remove(*k);
    }
    Value::Object(m)
}

/// The rest of a written store against the original, outside the picture
/// fields: what no conversion may change.
fn same_rest(original: &Value, written: &Value, picture: &str, what: &str) -> Result<(), String> {
    let (o, w) = (without(original, &[picture]), without(written, &[V2_FIELD, "format_revision"]));
    if o != w {
        return Err(format!("{what}: outside the picture: {}", crate::verify::first_diff(&w, &o, "").unwrap_or_default()));
    }
    Ok(())
}

/// Prove a written revision-2 Look: the conversion of the original, and
/// outside its graph the original itself.
pub fn verify_look(original: &Value, written: &str) -> Result<(), String> {
    verify_side(&look(original)?, written)?;
    let back: Value = serde_json::from_str(written).map_err(err)?;
    same_rest(original, &back, "node_graph", "the Look")
}

/// Prove a written revision-2 stills index: the conversion of the
/// original, and every still, outside its grade, the original's.
pub fn verify_stills(original: &Value, written: &str) -> Result<(), String> {
    verify_side(&stills(original)?, written)?;
    let back: Value = serde_json::from_str(written).map_err(err)?;
    let (o, w) = (original.as_array().ok_or("the stills index isn't a list")?, back["stills"].as_array().ok_or("the written index has no list")?);
    if o.len() != w.len() {
        return Err(format!("{} stills written for {}", w.len(), o.len()));
    }
    for (i, (a, b)) in o.iter().zip(w).enumerate() {
        same_rest(&without(a, &["grade"]), &without(b, &[V2_FIELD]), "grade", &format!("still {i}"))?;
    }
    Ok(())
}

/// Prove a written revision-2 colour preset: the conversion of the
/// original, and nothing but its revision and its graph.
pub fn verify_colour_preset(file_name: &str, original: &Value, written: &str) -> Result<(), String> {
    verify_side(&colour_preset(file_name, original)?, written)?;
    let back: Value = serde_json::from_str(written).map_err(err)?;
    let extra: Vec<&String> = back.as_object().into_iter().flatten().map(|(k, _)| k).filter(|k| k.as_str() != V2_FIELD && k.as_str() != "format_revision").collect();
    if !extra.is_empty() {
        return Err(format!("the preset holds {extra:?} besides its graph"));
    }
    Ok(())
}
