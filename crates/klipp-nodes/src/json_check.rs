//! What a saved graph holds that this build would lose by reading it
//! (#993).
//!
//! `NodeKind` reads any kind it does not know as `Unknown`, which runs as
//! a passthrough and is written back as `"Unknown"`; `NodeParams` and the
//! settings inside it drop fields they do not know. A project from a
//! newer Klipp therefore opened without complaint and lost those nodes
//! and settings on the next save. An effect this build does not know has
//! always refused the project instead; a graph now does too.
//!
//! The check works on the saved JSON before it is read: each node is read
//! as this build reads it, written back, and compared with what was
//! saved. A kind that comes back `Unknown`, or a field that does not come
//! back, is something the save would lose.

use serde_json::Value;

use crate::{Node, NodeKind, NodeParams};

/// Whether a saved value carries nothing a default would not: a field
/// holding it can be lost without losing anything.
fn is_empty(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Bool(b) => !b,
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
    }
}

/// The paths, under `path`, of fields in `saved` that `back` (the same
/// value read by this build and written again) no longer has.
fn dropped(saved: &Value, back: &Value, path: &str, out: &mut Vec<String>) {
    match (saved, back) {
        (Value::Object(s), Value::Object(b)) => {
            for (k, v) in s {
                let at = format!("{path}.{k}");
                match b.get(k) {
                    None if !is_empty(v) => out.push(at),
                    None => {}
                    Some(bv) => dropped(v, bv, &at, out),
                }
            }
        }
        (Value::Array(s), Value::Array(b)) if s.len() == b.len() => {
            for (i, (v, bv)) in s.iter().zip(b).enumerate() {
                dropped(v, bv, &format!("{path}[{i}]"), out);
            }
        }
        _ => {}
    }
}

/// What a saved set of node settings (a `NodeParams` on its own, as a
/// tool preset keeps one) holds that this build would lose by reading
/// it: the paths of fields it would drop. Empty when nothing would be
/// lost, and one entry naming the error if it cannot be read at all.
pub fn params_losses(saved: &Value) -> Vec<String> {
    match serde_json::from_value::<NodeParams>(saved.clone()) {
        Ok(p) => {
            let mut out = Vec::new();
            dropped(saved, &serde_json::to_value(&p).unwrap_or(Value::Null), "params", &mut out);
            out
        }
        Err(e) => vec![format!("settings this Klipp cannot read ({e})")],
    }
}

/// Whether `text` is JSON this build reads in full: it parses, and no
/// graph in it holds a node kind or setting this build would lose. A
/// store must not delete or overwrite a file that is not.
pub fn reads_in_full(text: &str) -> bool {
    serde_json::from_str::<Value>(text).is_ok_and(|v| losses(&v).is_empty())
}

/// Whether `v` is a saved node: an object with an id, a kind and params.
fn is_node(v: &Value) -> bool {
    v.get("id").is_some() && v.get("kind").is_some() && v.get("params").is_some()
}

/// Everything in `saved` (any JSON holding graphs, a whole project
/// included) that this build would lose by reading it, one sentence per
/// node, with where it is. Empty when nothing would be lost.
pub fn losses(saved: &Value) -> Vec<String> {
    let mut out = Vec::new();
    walk(saved, "", &mut out);
    out
}

fn walk(v: &Value, path: &str, out: &mut Vec<String>) {
    if is_node(v) {
        check_node(v, path, out);
    }
    match v {
        Value::Object(o) => o.iter().for_each(|(k, child)| walk(child, &format!("{path}.{k}"), out)),
        Value::Array(a) => a.iter().enumerate().for_each(|(i, child)| walk(child, &format!("{path}[{i}]"), out)),
        _ => {}
    }
}

fn check_node(saved: &Value, path: &str, out: &mut Vec<String>) {
    let kind = &saved["kind"];
    match serde_json::from_value::<NodeKind>(kind.clone()) {
        Ok(NodeKind::Unknown) | Err(_) => {
            out.push(format!("a grade node of kind {kind} at {path}"));
            return;
        }
        Ok(_) => {}
    }
    let node: Node = match serde_json::from_value(saved.clone()) {
        Ok(n) => n,
        Err(e) => {
            out.push(format!("a grade node this Klipp cannot read at {path} ({e})"));
            return;
        }
    };
    let back = serde_json::to_value(&node).unwrap_or(Value::Null);
    let mut fields = Vec::new();
    // Nested graphs (a compound's) are walked as nodes of their own.
    let mut saved_shallow = saved.clone();
    let mut back_shallow = back;
    for v in [&mut saved_shallow, &mut back_shallow] {
        if let Some(p) = v.get_mut("params").and_then(Value::as_object_mut) {
            p.remove("compound_subgraph");
        }
    }
    dropped(&saved_shallow, &back_shallow, path, &mut fields);
    if !fields.is_empty() {
        out.push(format!("settings this Klipp does not know ({}) on the grade node at {path}", fields.join(", ")));
    }
    // `NodeParams` must read every field it writes; a failure here is this
    // build's bug, not the file's, and is not reported as the file's.
    debug_assert!(serde_json::from_value::<NodeParams>(saved["params"].clone()).is_ok());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(kind: &str, params: Value) -> Value {
        serde_json::json!({ "id": "00000000-0000-0000-0000-000000000001", "kind": kind, "label": "n", "enabled": true, "params": params })
    }

    #[test]
    fn a_settings_set_alone_is_checked_too() {
        assert!(params_losses(&serde_json::json!({ "exposure": 0.5 })).is_empty());
        let l = params_losses(&serde_json::json!({ "exposure": 0.5, "sparkle": 1.0 }));
        assert_eq!(l, vec!["params.sparkle".to_string()]);
    }

    #[test]
    fn a_known_node_loses_nothing() {
        assert!(losses(&node("Exposure", serde_json::json!({ "exposure": 0.5 }))).is_empty());
    }

    #[test]
    fn an_unknown_kind_is_named() {
        let l = losses(&serde_json::json!({ "nodes": [node("HolographicKey", serde_json::json!({}))] }));
        assert_eq!(l.len(), 1, "{l:?}");
        assert!(l[0].contains("HolographicKey") && l[0].contains(".nodes[0]"), "{l:?}");
    }

    #[test]
    fn an_unknown_setting_is_named_and_an_empty_one_is_not() {
        let l = losses(&node("Exposure", serde_json::json!({ "exposure": 0.5, "sparkle": 2.0, "unused": null })));
        assert_eq!(l.len(), 1, "{l:?}");
        assert!(l[0].contains("params.sparkle") && !l[0].contains("unused"), "{l:?}");
    }

    #[test]
    fn an_unknown_setting_inside_a_known_one_is_named() {
        let q = serde_json::to_value(klipp_core::QualifierSettings::default()).unwrap();
        let mut q = q.as_object().unwrap().clone();
        q.insert("halo".into(), Value::from(1.5));
        let l = losses(&node("Qualifier", serde_json::json!({ "qualifier": q })));
        assert!(l.len() == 1 && l[0].contains("params.qualifier.halo"), "{l:?}");
    }
}
