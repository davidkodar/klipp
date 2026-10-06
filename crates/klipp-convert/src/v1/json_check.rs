// Frozen refusal rules from klipp-nodes at 2bcf8b1d (#1050).
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
//!
//! A field that does not come back is not always unknown: a known one is
//! left out of the written form when it holds its default. An unknown one
//! is dropped whatever it holds, and its 0 or `false` may not be a newer
//! build's default (a mix, an opacity) (#1050). So a field that comes back
//! missing while holding an empty value is tried again holding a
//! non-empty value of the same type: a known field then comes back, or
//! fails to read, and an unknown one is still dropped. Unknown fields
//! never fail a read, since none of these types denies them.

use serde_json::Value;

use super::{Node, NodeKind, NodeParams};

/// A non-empty value of `v`'s type, for a field holding the empty `v`, or
/// `None` when dropping `v` loses nothing whatever the field: a `null`
/// reads as absent, and an empty object as a map or settings left at
/// their defaults.
fn probe(v: &Value) -> Option<Value> {
    match v {
        Value::Null => None,
        Value::Bool(false) => Some(Value::Bool(true)),
        Value::Number(n) if n.as_f64() == Some(0.0) => Some(if n.is_f64() {
            Value::from(0.123_456)
        } else {
            Value::from(7)
        }),
        Value::String(s) if s.is_empty() => Some(Value::from("probe")),
        Value::Array(a) if a.is_empty() => Some(Value::Array(vec![Value::Null])),
        Value::Object(o) if o.is_empty() => None,
        _ => Some(v.clone()),
    }
}

/// One step of a JSON pointer.
fn pointer_step(key: &str) -> String {
    format!("/{}", key.replace('~', "~0").replace('/', "~1"))
}

/// Fields in `saved` that `back` (the same value read by this build and
/// written again) no longer has, as (path for the message, JSON pointer,
/// saved value).
fn dropped(
    saved: &Value,
    back: &Value,
    path: &str,
    ptr: &str,
    out: &mut Vec<(String, String, Value)>,
) {
    match (saved, back) {
        (Value::Object(s), Value::Object(b)) => {
            for (k, v) in s {
                let (at, at_ptr) = (format!("{path}.{k}"), format!("{ptr}{}", pointer_step(k)));
                match b.get(k) {
                    None => out.push((at, at_ptr, v.clone())),
                    Some(bv) => dropped(v, bv, &at, &at_ptr, out),
                }
            }
        }
        (Value::Array(s), Value::Array(b)) if s.len() == b.len() => {
            for (i, (v, bv)) in s.iter().zip(b).enumerate() {
                dropped(v, bv, &format!("{path}[{i}]"), &format!("{ptr}/{i}"), out);
            }
        }
        _ => {}
    }
}

/// The paths of fields in `saved` that reading it with `reread` (read by
/// this build and written again; `None` when it cannot be read) would
/// lose: dropped holding a value, or dropped holding an empty value that
/// a non-empty one of its type does not bring back (module doc).
fn lost_fields(saved: &Value, path: &str, reread: impl Fn(&Value) -> Option<Value>) -> Vec<String> {
    let Some(back) = reread(saved) else {
        return Vec::new();
    };
    let mut missing = Vec::new();
    dropped(saved, &back, path, "", &mut missing);
    missing
        .into_iter()
        .filter(|(_, ptr, v)| {
            let Some(p) = probe(v) else { return false };
            if p == *v {
                return true;
            }
            let mut tried = saved.clone();
            if let Some(slot) = tried.pointer_mut(ptr) {
                *slot = p;
            }
            reread(&tried).is_some_and(|b| b.pointer(ptr).is_none())
        })
        .map(|(at, _, _)| at)
        .collect()
}

/// `v` without `params.compound_subgraph`: nested graphs are checked as
/// nodes of their own.
fn without_subgraph(mut v: Value, params_at_root: bool) -> Value {
    let params = if params_at_root {
        Some(&mut v)
    } else {
        v.get_mut("params")
    };
    if let Some(p) = params.and_then(Value::as_object_mut) {
        p.remove("compound_subgraph");
    }
    v
}

/// What a saved set of node settings (a `NodeParams` on its own, as a
/// tool preset keeps one) holds that this build would lose by reading
/// it: the paths of fields it would drop, and any node a nested graph
/// holds that it would lose. Empty when nothing would be lost, and one
/// entry naming the error if it cannot be read at all.
pub fn params_losses(saved: &Value) -> Vec<String> {
    if let Err(e) = serde_json::from_value::<NodeParams>(saved.clone()) {
        return vec![format!("settings this Klipp cannot read ({e})")];
    }
    let reread = |v: &Value| {
        let p = serde_json::from_value::<NodeParams>(v.clone()).ok()?;
        Some(without_subgraph(serde_json::to_value(&p).ok()?, true))
    };
    let mut out = lost_fields(&without_subgraph(saved.clone(), true), "params", reread);
    if let Some(sub) = saved.get("compound_subgraph") {
        walk(sub, "params.compound_subgraph", &mut out);
    }
    out
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
        Value::Object(o) => o
            .iter()
            .for_each(|(k, child)| walk(child, &format!("{path}.{k}"), out)),
        Value::Array(a) => a
            .iter()
            .enumerate()
            .for_each(|(i, child)| walk(child, &format!("{path}[{i}]"), out)),
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
    if let Err(e) = serde_json::from_value::<Node>(saved.clone()) {
        out.push(format!(
            "a grade node this Klipp cannot read at {path} ({e})"
        ));
        return;
    }
    // Nested graphs (a compound's) are walked as nodes of their own.
    let reread = |v: &Value| {
        let n = serde_json::from_value::<Node>(v.clone()).ok()?;
        Some(without_subgraph(serde_json::to_value(&n).ok()?, false))
    };
    let fields = lost_fields(&without_subgraph(saved.clone(), false), path, reread);
    if !fields.is_empty() {
        out.push(format!(
            "settings this Klipp does not know ({}) on the grade node at {path}",
            fields.join(", ")
        ));
    }
    // `NodeParams` must read every field it writes; a failure here is this
    // build's bug, not the file's, and is not reported as the file's.
    debug_assert!(serde_json::from_value::<NodeParams>(saved["params"].clone()).is_ok());
}
