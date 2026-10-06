//! The two properties of the structural conversion, checked on one store
//! at a time, for the tests and for the dry run over real projects:
//! `to_v1(to_v2(x)) == canon(x)` and `to_v2(to_v1(g)) == g`. Each check
//! says where the first difference is, and never panics on odd input.

use crate::canon::{canon, Store};
use crate::{to_v1, to_v2};
use serde_json::{json, Map, Value};

/// A JSON object, or why it isn't one.
fn as_obj(v: &Value) -> Result<&Map<String, Value>, String> {
    v.as_object().ok_or_else(|| "not an object".to_owned())
}

/// Where two values first differ, as a JSON pointer, with both sides.
pub fn first_diff(a: &Value, b: &Value, at: &str) -> Option<String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let keys: std::collections::BTreeSet<&String> = x.keys().chain(y.keys()).collect();
            keys.into_iter().find_map(|k| first_diff(x.get(k).unwrap_or(&Value::Null), y.get(k).unwrap_or(&Value::Null), &format!("{at}/{k}")))
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => x.iter().zip(y).enumerate().find_map(|(i, (p, q))| first_diff(p, q, &format!("{at}/{i}"))),
        _ if a == b => None,
        _ => Some(format!("{at}: {a} vs {b}")),
    }
}

pub fn graph_diff<T: serde::Serialize>(a: &T, b: &T) -> String {
    first_diff(&serde_json::to_value(a).unwrap_or_default(), &serde_json::to_value(b).unwrap_or_default(), "").unwrap_or_default()
}

pub fn check_clip(c: &Value) -> Result<(), String> {
    let record = to_v2::clip_to_v2(as_obj(c)?).map_err(|e| format!("to_v2 refused it: {e:?}"))?;
    let record_json = serde_json::to_value(&record).map_err(|e| e.to_string())?;
    let back = to_v1::clip(&record_json).map_err(|e| format!("to_v1 refused to_v2's record: {e}"))?;
    if back != canon(Store::Clip, c) {
        return Err(format!("to_v1(to_v2(x)) is not canon(x): {}", first_diff(&back, &canon(Store::Clip, c), "").unwrap_or_default()));
    }
    let mut again = back;
    again["id"] = c["id"].clone();
    let record2 = to_v2::clip_to_v2(as_obj(&again)?).map_err(|e| format!("to_v2 refused to_v1's picture: {e:?}"))?;
    if record2 != record {
        return Err(format!("to_v2(to_v1(g)) is not g: {}", graph_diff(&record2, &record)));
    }
    Ok(())
}

pub fn check_track(t: &Value) -> Result<(), String> {
    let g = to_v2::track_to_v2(as_obj(t)?).map_err(|e| format!("to_v2 refused it: {e:?}"))?;
    let back = to_v1::track(&g).map_err(|e| format!("to_v1 refused: {e}"))?;
    let raw = t.get("effects").filter(|v| !v.is_null()).cloned().unwrap_or(json!({}));
    if back != canon(Store::Track, &raw) {
        return Err(format!("track: {}", first_diff(&back, &canon(Store::Track, &raw), "").unwrap_or_default()));
    }
    let g2 = to_v2::track_to_v2(as_obj(&json!({ "id": t["id"].clone(), "effects": back }))?).map_err(|e| format!("{e:?}"))?;
    if g2 != g {
        return Err(format!("track round trip: {}", graph_diff(&g2, &g)));
    }
    Ok(())
}

/// A grade graph on its own, through `to_v2`'s grade-version path, which
/// takes any grade alone.
pub fn check_grade(g: &Value) -> Result<(), String> {
    let id = uuid::Uuid::nil();
    let v = to_v2::version_to_v2(id, 0, as_obj(&json!({ "name": "", "node_graph": g }))?).map_err(|e| format!("to_v2 refused it: {e:?}"))?;
    let back = to_v1::grade_only(&v).map_err(|e| format!("to_v1 refused: {e}"))?;
    if back != canon(Store::Graph, g) {
        return Err(format!("grade: {}", first_diff(&back, &canon(Store::Graph, g), "").unwrap_or_default()));
    }
    let v2 = to_v2::version_to_v2(id, 0, as_obj(&json!({ "name": "", "node_graph": back }))?).map_err(|e| format!("{e:?}"))?;
    if v2 != v {
        return Err(format!("grade round trip: {}", graph_diff(&v2, &v)));
    }
    Ok(())
}

/// Both properties for a grade-only store: `to_v2` of `wrap(g)`, back
/// with `to_v1::grade_only`, and `to_v2` of the store rebuilt around
/// what came back.
pub fn check_store(what: &str, g: &Value, wrap: &dyn Fn(Value) -> Value, conv: &dyn Fn(&Value) -> Result<Option<klipp_graph_format::Graph>, String>) -> Result<(), String> {
    let Some(v) = conv(&wrap(g.clone()))? else { return Err(format!("{what}: to_v2 gave no graph for a grade")) };
    let back = to_v1::grade_only(&v).map_err(|e| format!("{what}: to_v1 refused: {e}"))?;
    if back != canon(Store::Graph, g) {
        return Err(format!("{what}: {}", first_diff(&back, &canon(Store::Graph, g), "").unwrap_or_default()));
    }
    let v2 = conv(&wrap(back))?.ok_or_else(|| format!("{what}: no graph the second time"))?;
    if v2 != v {
        return Err(format!("{what} round trip: {}", graph_diff(&v2, &v)));
    }
    Ok(())
}

pub fn look(id: &str) -> impl Fn(Value) -> Value + '_ {
    move |g| json!({ "id": id, "name": "Look", "node_graph": g })
}

pub fn still(g: Value) -> Value {
    json!({ "name": "Still", "filename": "a.png", "grade": g })
}

pub fn conv_look(v: &Value) -> Result<Option<klipp_graph_format::Graph>, String> {
    to_v2::look_to_v2(as_obj(v)?).map(Some).map_err(|e| format!("{e:?}"))
}

pub fn conv_still(v: &Value) -> Result<Option<klipp_graph_format::Graph>, String> {
    to_v2::still_to_v2(as_obj(v)?).map_err(|e| format!("{e:?}"))
}

pub fn conv_preset(v: &Value) -> Result<Option<klipp_graph_format::Graph>, String> {
    to_v2::colour_preset_to_v2("preset.json", as_obj(v)?).map(Some).map_err(|e| format!("{e:?}"))
}

