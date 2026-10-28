//! An effect node's values as its `params` map (M3 step 10, amendment
//! M3-2): the lift reads a stored effect entry into the map, and the
//! writer that stores a picture (the delift, the save) writes the entry
//! back from it. In between the map is the node's only truth: a value
//! edit is a map write, as on any other node, and the renderer builds the
//! effect from the map.
//!
//! The map holds the entry losslessly, so a picture read and stored again
//! is the same bytes. Each field is keyed as the node's animation keys it
//! (its saved name; an element of an array field that the catalog keys
//! element by element, by that element's key: a corner pin's `tl_x`) and
//! held as the [`ParamValue`] its kind describes: a number as `Float` or
//! `Int`, a choice as `Enum`, an sRGB colour as `Color`. A value that kind
//! can't give back exactly (a colour with alpha, a path, a field the
//! catalog doesn't describe) is held as its JSON in `Text`, which a
//! catalog field never is. Keys with a `.` are the node's own
//! (`mask.channel`), never an entry's field.

use std::collections::BTreeMap;

use klipp_effects::registry::{effect_type, ParamDesc, ParamKind};
use klipp_graph_format::lifted::{types as lt, ClipPicture};
use klipp_graph_format::{types, Graph, Node, ParamKey, ParamValue};
use serde_json::{Map, Value};

/// The colour space a [`ParamValue::Color`] of an effect is in: sRGB, as
/// the effect stores it in bytes.
pub const SRGB: &str = "srgb";

/// The key an entry's field, or one element of it, is held under.
fn key_of(desc: &ParamDesc) -> &'static str {
    if desc.index.is_some() {
        desc.key
    } else {
        desc.field
    }
}

/// Whether `key` names a field of the entry rather than the node.
pub fn is_field_key(key: &ParamKey) -> bool {
    !key.as_str().contains('.')
}

/// The descriptions of the effect saved under `tag`; empty when the
/// catalog doesn't describe it.
fn descs(tag: &str) -> &'static [ParamDesc] {
    effect_type(tag).map_or(&[], |t| t.desc.params)
}

/// How the effect stores `field` when it is reset: the form a choice is
/// written in (its name, or its number).
fn form(tag: &str, field: &str) -> Option<Value> {
    let sample = serde_json::to_value((effect_type(tag)?.sample)()).ok()?;
    sample.get(field).cloned()
}

/// `value` as the kind `kind` describes it, when that gives it back
/// exactly; else its JSON.
fn encode(kind: Option<ParamKind>, value: &Value, form: Option<&Value>) -> ParamValue {
    let typed = match (kind, value) {
        (Some(ParamKind::Choice(names)), Value::String(s)) => names.iter().position(|n| n == s).map(|i| ParamValue::Enum(i as u32)),
        (Some(ParamKind::Choice(_)), Value::Number(n)) => n.as_u64().and_then(|n| u32::try_from(n).ok()).map(ParamValue::Enum),
        (Some(ParamKind::Colour), Value::Array(a)) if a.len() == 3 => {
            let bytes: Option<Vec<u8>> = a.iter().map(|c| c.as_u64().and_then(|c| u8::try_from(c).ok())).collect();
            bytes.map(|b| ParamValue::Color { rgb: [b[0] as f32 / 255.0, b[1] as f32 / 255.0, b[2] as f32 / 255.0], space: SRGB.into() })
        }
        (_, Value::Bool(b)) => Some(ParamValue::Bool(*b)),
        (_, Value::Number(n)) if n.is_f64() => n.as_f64().map(|f| ParamValue::Float(f as f32)),
        (_, Value::Number(n)) => n.as_i64().and_then(|i| i32::try_from(i).ok()).map(ParamValue::Int),
        _ => None,
    };
    match typed {
        Some(p) if decode(kind, &p, form).as_ref() == Ok(value) => p,
        _ => ParamValue::Text(value.to_string()),
    }
}

/// The stored JSON of `value`, a field of the kind `kind` stored as
/// `form`.
fn decode(kind: Option<ParamKind>, value: &ParamValue, form: Option<&Value>) -> Result<Value, String> {
    Ok(match (kind, value) {
        (_, ParamValue::Text(json)) => serde_json::from_str(json).map_err(|e| format!("{json:?} isn't a stored value: {e}"))?,
        (Some(ParamKind::Choice(names)), ParamValue::Enum(i)) => match form {
            Some(Value::String(_)) => Value::from(*names.get(*i as usize).ok_or_else(|| format!("choice {i} is past the last, {}", names.len().saturating_sub(1)))?),
            _ => Value::from(*i),
        },
        (_, ParamValue::Enum(i)) => Value::from(*i),
        (_, ParamValue::Color { rgb, space }) if space == SRGB => {
            let byte = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
            Value::from(rgb.iter().map(|c| byte(*c)).collect::<Vec<u8>>())
        }
        (_, ParamValue::Color { space, .. }) => return Err(format!("an effect's colour is sRGB, not {space}")),
        (_, ParamValue::Bool(b)) => Value::Bool(*b),
        (_, ParamValue::Float(f)) => serde_json::to_value(f).map_err(|e| e.to_string())?,
        (_, ParamValue::Int(i)) => Value::from(*i),
        (_, other) => return Err(format!("{other:?} isn't an effect's value")),
    })
}

/// The stored value `value` of the effect `tag`'s field (or element)
/// `key`, as its map holds it.
pub fn held(tag: &str, key: &ParamKey, value: &Value) -> ParamValue {
    let descs = descs(tag);
    match descs.iter().find(|d| d.index.is_some() && d.key == key.as_str()) {
        Some(d) => encode(Some(d.kind), value, form(tag, d.field).as_ref().and_then(|f| f.get(d.index.unwrap_or_default()))),
        None => {
            let kind = descs.iter().find(|d| d.field == key.as_str() && d.index.is_none()).map(|d| d.kind);
            encode(kind, value, form(tag, key.as_str()).as_ref())
        }
    }
}

/// A stored effect entry (`{"type": tag, field: value, ...}`) as its tag
/// and its values.
pub fn to_params(entry: &Value) -> Result<(String, BTreeMap<ParamKey, ParamValue>), String> {
    let fields = entry.as_object().ok_or("an effect entry is an object")?;
    let tag = fields.get("type").and_then(Value::as_str).ok_or("an effect entry has a type")?.to_owned();
    let descs = descs(&tag);
    let mut map = BTreeMap::new();
    for (field, value) in fields {
        if field == "type" {
            continue;
        }
        if field.contains('.') {
            return Err(format!("{tag}'s field {field:?} would read as the node's own"));
        }
        let form = form(&tag, field);
        let elements: Vec<&ParamDesc> = descs.iter().filter(|d| d.field == field && d.index.is_some()).collect();
        // An array keyed element by element, when every element is held
        // exactly and the array is no longer than its keys.
        if !elements.is_empty() {
            if let Value::Array(items) = value {
                let held: Option<Vec<(ParamKey, ParamValue)>> = (items.len() == elements.len())
                    .then(|| {
                        elements
                            .iter()
                            .map(|d| {
                                let i = d.index?;
                                let item = items.get(i)?;
                                let p = encode(Some(d.kind), item, form.as_ref().and_then(|f| f.get(i)));
                                (!matches!(p, ParamValue::Text(_))).then(|| (ParamKey::from(key_of(d)), p))
                            })
                            .collect()
                    })
                    .flatten();
                if let Some(held) = held {
                    map.extend(held);
                    continue;
                }
            }
        }
        let kind = descs.iter().find(|d| d.field == field && d.index.is_none()).map(|d| d.kind);
        map.insert(ParamKey::from(field.as_str()), encode(kind, value, form.as_ref()));
    }
    Ok((tag, map))
}

/// The stored entry of the effect `tag` whose values are `params`: what
/// [`to_params`] read it from. The node's own keys are passed over.
pub fn from_params(tag: &str, params: &BTreeMap<ParamKey, ParamValue>) -> Result<Value, String> {
    let descs = descs(tag);
    let mut fields = Map::new();
    fields.insert("type".into(), Value::from(tag));
    let mut arrays: BTreeMap<&'static str, Vec<Option<Value>>> = BTreeMap::new();
    for (key, value) in params.iter().filter(|(k, _)| is_field_key(k)) {
        let key = key.as_str();
        if let Some(d) = descs.iter().find(|d| d.index.is_some() && d.key == key) {
            let i = d.index.unwrap_or_default();
            let form = form(tag, d.field);
            let item = decode(Some(d.kind), value, form.as_ref().and_then(|f| f.get(i)))?;
            let slots = arrays.entry(d.field).or_default();
            if slots.len() <= i {
                slots.resize(i + 1, None);
            }
            slots[i] = Some(item);
            continue;
        }
        let kind = descs.iter().find(|d| d.field == key && d.index.is_none()).map(|d| d.kind);
        fields.insert(key.to_owned(), decode(kind, value, form(tag, key).as_ref())?);
    }
    for (field, slots) in arrays {
        let items: Option<Vec<Value>> = slots.into_iter().collect();
        let items = items.ok_or_else(|| format!("{tag}'s {field} is missing an element"))?;
        if fields.insert(field.to_owned(), Value::Array(items)).is_some() {
            return Err(format!("{tag}'s {field} is held both whole and by element"));
        }
    }
    Ok(Value::Object(fields))
}

/// An effect's values held inside their safe ranges (#2995), the one
/// guard every edit of them passes: a number past its range is clamped
/// in `params` and logged, a number that isn't one refused. The entry
/// they store, guarded.
pub fn guard_params(tag: &str, params: &mut BTreeMap<ParamKey, ParamValue>) -> Result<Value, String> {
    let mut entry = from_params(tag, params)?;
    for g in klipp_effects::registry::guard_entry(&mut entry, klipp_effects::registry::NotFinite::Refuse)? {
        tracing::info!("{tag}'s {} held at {} (asked {})", g.key, g.now, g.was);
        let key = ParamKey::from(g.key);
        let held = held(tag, &key, &serde_json::json!(g.now));
        params.insert(key, held);
    }
    Ok(entry)
}

/// The effect `tag` whose values are `params`, as the renderer runs it:
/// every number inside its safe range (#2995), whatever wrote it, a
/// number that isn't one at its default.
pub fn effect(tag: &str, params: &BTreeMap<ParamKey, ParamValue>) -> Result<klipp_effects::BuiltinEffect, String> {
    let mut entry = from_params(tag, params)?;
    klipp_effects::registry::guard_entry(&mut entry, klipp_effects::registry::NotFinite::Default)?;
    serde_json::from_value(entry).map_err(|e| format!("{tag}: {e}"))
}

/// The saved tag of the effect node `n` (`"gaussian_blur"`): the one
/// reader of an effect node's entry after the lift, which holds only the
/// tag there.
pub fn tag(n: &Node) -> Option<&str> {
    n.legacy.as_ref()?.payload.get("type")?.as_str()
}

/// The effect node `n` as the renderer runs it, built from its map. A
/// node still holding its stored entry is refused: its values are read
/// from one place.
pub fn effect_of(n: &Node) -> Result<klipp_effects::BuiltinEffect, String> {
    if !in_memory(n) {
        return Err(format!("effect node {} holds its stored entry, not its values", n.id));
    }
    let tag = tag(n).ok_or_else(|| format!("effect node {} has no type", n.id))?;
    effect(tag, &n.params).map_err(|e| format!("effect node {}: {e}", n.id))
}

/// The effect node `n`'s entry as it would be stored: written from its
/// map when its values are held there, its stored entry as it is when they
/// aren't yet. What a reader that shows the entry (the socket's graph
/// read) reads, so it reads one place.
pub fn entry_of(n: &Node) -> Result<Value, String> {
    if !in_memory(n) {
        return n.legacy.as_ref().map(|l| l.payload.clone()).ok_or_else(|| format!("effect node {} has no entry", n.id));
    }
    from_params(tag(n).ok_or_else(|| format!("effect node {} has no type", n.id))?, &n.params)
}

fn is_effect(n: &Node) -> bool {
    n.type_id.as_str() == types::LEGACY_EFFECT
}

/// Whether the effect node's entry holds only its tag: its values are in
/// its map.
pub(crate) fn in_memory(n: &Node) -> bool {
    n.legacy.as_ref().and_then(|l| l.payload.as_object()).is_some_and(|o| o.len() == 1)
}

/// The effect node `n` as it is held in memory: its stored entry's values
/// moved into its map, the entry left holding its tag. A node already
/// held so, or no effect node, is left as it is.
pub fn into_memory(n: &mut Node) -> Result<(), String> {
    frei0r::from_stored(n)?;
    ofx::from_stored(n)?;
    if !is_effect(n) || in_memory(n) {
        return Ok(());
    }
    let Some(legacy) = n.legacy.as_mut() else { return Ok(()) };
    let (tag, values) = to_params(&legacy.payload)?;
    if let Some(k) = values.keys().find(|k| n.params.contains_key(*k)) {
        return Err(format!("effect node {}: {k} is both stored and held", n.id));
    }
    legacy.payload = serde_json::json!({ "type": tag });
    n.params.extend(values);
    Ok(())
}

/// A node an edit adds, held in memory: one that brings its stored entry
/// brings its values there, so any field keys it also carries (a copy of
/// another effect's node, its entry replaced) are left behind for the
/// entry's.
pub fn added_into_memory(n: &mut Node) -> Result<(), String> {
    if is_effect(n) && !in_memory(n) {
        n.params.retain(|k, _| !is_field_key(k));
    }
    into_memory(n)
}

/// The effect node `n` as a file stores it: its entry written from its
/// map ([`into_entry`]), and a plug-in's as its own node (`klipp.frei0r`,
/// `klipp.ofx`: picture revision 4). The inverse of [`into_memory`]; a node
/// already stored, or no effect node, is left.
pub fn into_stored(n: &mut Node) -> Result<(), String> {
    if n.type_id.as_str() == types::LEGACY_OFX {
        return ofx::into_stored(n);
    }
    let was = is_effect(n) && in_memory(n);
    into_entry(n)?;
    if was && tag(n) == Some(frei0r::TAG) {
        let entry = n.legacy.as_ref().map(|l| l.payload.clone()).unwrap_or(Value::Null);
        return frei0r::into_stored(n, &entry);
    }
    Ok(())
}

/// The effect node `n` with its entry written from its map, and only the
/// node's own keys left there: the entry a reader of entries (the delift)
/// reads. A node already so, or no effect node, is left.
pub fn into_entry(n: &mut Node) -> Result<(), String> {
    if !is_effect(n) || !in_memory(n) {
        return Ok(());
    }
    let tag = tag(n).ok_or_else(|| format!("effect node {} has no type", n.id))?.to_owned();
    let entry = from_params(&tag, &n.params)?;
    n.params.retain(|k, _| !is_field_key(k));
    if let Some(legacy) = n.legacy.as_mut() {
        legacy.payload = entry;
    }
    Ok(())
}

/// A stored `klipp.frei0r` or `klipp.ofx` node as the entry it holds, for
/// a reader of stored entries (the delift); any other node is left.
pub fn plugin_entry(n: &mut Node) -> Result<(), String> {
    frei0r::from_stored(n)?;
    ofx::from_stored(n)
}

/// An OFX plug-in's stored form (picture revision 4, d9's fields): a
/// `klipp.ofx` node whose `plugin`, `context` and `transition_amount` are
/// its entry's, and whose `p.{name}` is each value by name at full
/// precision, its kind in its variant. The entry's own `enabled` is the
/// node's there (in memory an OFX node is on, its entry saying whether it
/// runs). In memory it is the `klipp.legacy.ofx` node it was.
mod ofx {
    use super::*;

    const PREFIX: &str = "p.";
    /// A value whose shape the stored form doesn't type, kept as its JSON.
    const RAW: &str = "p-json.";
    /// The node's own key saying an OFX node is off as a node (in memory
    /// it never is; kept rather than refused).
    const NODE_OFF: &str = "node.enabled";

    pub fn into_stored(n: &mut Node) -> Result<(), String> {
        let legacy = n.legacy.as_mut().ok_or_else(|| format!("OFX node {} has no entry", n.id))?;
        let entry = legacy.payload.as_object().ok_or_else(|| format!("OFX node {}'s entry isn't an object", n.id))?.clone();
        // An OFX node is on in memory, its entry saying whether it runs; one
        // off as a node too is kept so, under the node's own key, as is any
        // field of the entry the stored form names no key for, so storing
        // never fails and nothing is lost.
        if !n.enabled {
            n.params.insert(ParamKey::from(NODE_OFF), ParamValue::Bool(false));
        }
        for (k, v) in entry.iter().filter(|(k, _)| !matches!(k.as_str(), "plugin_name" | "enabled" | "param_values" | "context" | "transition_amount")) {
            n.params.insert(ParamKey::from(format!("{EXTRA}{k}").as_str()), ParamValue::Text(v.to_string()));
        }
        let text = |k: &str| entry.get(k).and_then(Value::as_str).map(str::to_owned).ok_or_else(|| format!("OFX node {}: no {k}", n.id));
        n.params.insert(ParamKey::from("plugin"), ParamValue::Text(text("plugin_name")?));
        if entry.contains_key("context") {
            n.params.insert(ParamKey::from("context"), ParamValue::Text(text("context")?));
        }
        if let Some(t) = entry.get("transition_amount") {
            n.params.insert(ParamKey::from("transition_amount"), ParamValue::Float64(t.as_f64().ok_or_else(|| format!("OFX node {}: its transition amount isn't a number", n.id))?));
        }
        for (name, v) in entry.get("param_values").and_then(Value::as_object).into_iter().flatten() {
            // A value in a shape the stored form doesn't type is kept as
            // its JSON, under its own key, so storing never fails.
            match value(v) {
                Some(p) => n.params.insert(ParamKey::from(format!("{PREFIX}{name}").as_str()), p),
                None => n.params.insert(ParamKey::from(format!("{RAW}{name}").as_str()), ParamValue::Text(v.to_string())),
            };
        }
        n.enabled = entry.get("enabled").and_then(Value::as_bool).ok_or_else(|| format!("OFX node {}: no enabled flag", n.id))?;
        legacy.payload = Value::Null;
        n.type_id = klipp_graph_format::NodeTypeId::new(lt::OFX);
        Ok(())
    }

    pub fn from_stored(n: &mut Node) -> Result<(), String> {
        if n.type_id.as_str() != lt::OFX {
            return Ok(());
        }
        let mut entry = Map::new();
        let take = |n: &mut Node, k: &str| n.params.remove(&ParamKey::from(k));
        match take(n, "plugin") {
            Some(ParamValue::Text(p)) => entry.insert("plugin_name".into(), Value::from(p)),
            other => return Err(format!("OFX node {}: its plug-in is {other:?}, not a name", n.id)),
        };
        entry.insert("enabled".into(), Value::Bool(n.enabled));
        match take(n, "context") {
            Some(ParamValue::Text(c)) => {
                entry.insert("context".into(), Value::from(c));
            }
            None => {}
            other => return Err(format!("OFX node {}: its context is {other:?}", n.id)),
        }
        match take(n, "transition_amount") {
            Some(ParamValue::Float64(t)) => {
                entry.insert("transition_amount".into(), serde_json::json!(t));
            }
            None => {}
            other => return Err(format!("OFX node {}: its transition amount is {other:?}", n.id)),
        }
        let mut values = Map::new();
        for key in n.params.keys().filter(|k| k.as_str().starts_with(PREFIX)).cloned().collect::<Vec<_>>() {
            let v = n.params.remove(&key).unwrap_or(ParamValue::Bool(false));
            let name = key.as_str()[PREFIX.len()..].to_owned();
            values.insert(name, stored(&v).ok_or_else(|| format!("OFX node {}: {key} is {v:?}, no OFX value", n.id))?);
        }
        for key in n.params.keys().filter(|k| k.as_str().starts_with(RAW)).cloned().collect::<Vec<_>>() {
            let Some(ParamValue::Text(json)) = n.params.remove(&key) else { return Err(format!("OFX node {}: {key} isn't a kept value", n.id)) };
            values.insert(key.as_str()[RAW.len()..].to_owned(), serde_json::from_str(&json).map_err(|e| format!("OFX node {}: {key}: {e}", n.id))?);
        }
        entry.insert("param_values".into(), Value::Object(values));
        extras_back(n, &mut entry)?;
        let off = n.params.remove(&ParamKey::from(NODE_OFF)) == Some(ParamValue::Bool(false));
        let legacy = n.legacy.as_mut().ok_or_else(|| format!("OFX node {} has no slot", n.id))?;
        legacy.payload = Value::Object(entry);
        n.enabled = !off;
        n.type_id = klipp_graph_format::NodeTypeId::new(types::LEGACY_OFX);
        Ok(())
    }

    /// A stored OFX value (`{"t": "Double2", "v": [a, b]}`, as the file's
    /// `OfxValueJson` writes it) as the typed value of its kind.
    fn value(v: &Value) -> Option<ParamValue> {
        let o = v.as_object().filter(|o| o.len() == 2)?;
        let (kind, x) = (o.get("t")?.as_str()?, o.get("v")?);
        let floats = |n: usize| -> Option<Vec<f64>> { x.as_array().filter(|a| a.len() == n)?.iter().map(Value::as_f64).collect() };
        let ints = |n: usize| -> Option<Vec<i32>> { x.as_array().filter(|a| a.len() == n)?.iter().map(|i| i.as_i64().and_then(|i| i32::try_from(i).ok())).collect() };
        let int = || x.as_i64().and_then(|i| i32::try_from(i).ok());
        Some(match kind {
            "Double" => ParamValue::Float64(x.as_f64()?),
            "Double2" => floats(2).map(|f| ParamValue::Point2F64([f[0], f[1]]))?,
            "Double3" => floats(3).map(|f| ParamValue::Point3F64([f[0], f[1], f[2]]))?,
            "Int" => ParamValue::Int(int()?),
            "Int2" => ints(2).map(|i| ParamValue::Int2([i[0], i[1]]))?,
            "Int3" => ints(3).map(|i| ParamValue::Int3([i[0], i[1], i[2]]))?,
            "Bool" => ParamValue::Bool(x.as_bool()?),
            "Choice" => ParamValue::Choice(int()?),
            "String" => ParamValue::Text(x.as_str()?.to_owned()),
            "Rgba" => floats(4).map(|f| ParamValue::ColorF64 { rgba: [f[0], f[1], f[2], f[3]], alpha: true })?,
            "Rgb" => floats(3).map(|f| ParamValue::ColorF64 { rgba: [f[0], f[1], f[2], 0.0], alpha: false })?,
            _ => return None,
        })
    }

    /// [`value`]'s inverse.
    fn stored(v: &ParamValue) -> Option<Value> {
        let tv = |t: &str, x: Value| serde_json::json!({ "t": t, "v": x });
        Some(match v {
            ParamValue::Float64(x) => tv("Double", serde_json::json!(x)),
            ParamValue::Point2F64(p) => tv("Double2", serde_json::json!(p)),
            ParamValue::Point3F64(p) => tv("Double3", serde_json::json!(p)),
            ParamValue::Int(i) => tv("Int", serde_json::json!(i)),
            ParamValue::Int2(i) => tv("Int2", serde_json::json!(i)),
            ParamValue::Int3(i) => tv("Int3", serde_json::json!(i)),
            ParamValue::Bool(b) => tv("Bool", serde_json::json!(b)),
            ParamValue::Choice(i) => tv("Choice", serde_json::json!(i)),
            ParamValue::Text(s) => tv("String", serde_json::json!(s)),
            ParamValue::ColorF64 { rgba, alpha: true } => tv("Rgba", serde_json::json!(rgba)),
            ParamValue::ColorF64 { rgba, alpha: false } => tv("Rgb", serde_json::json!(&rgba[..3])),
            _ => return None,
        })
    }
}

/// The prefix of a plug-in node's own key holding an entry field its
/// stored form names no key for, as that field's JSON.
const EXTRA: &str = "entry.";

/// The entry fields a stored plug-in node keeps under [`EXTRA`] keys, put
/// back into `entry` and taken off the node.
fn extras_back(n: &mut Node, entry: &mut Map<String, Value>) -> Result<(), String> {
    for key in n.params.keys().filter(|k| k.as_str().starts_with(EXTRA)).cloned().collect::<Vec<_>>() {
        let Some(ParamValue::Text(json)) = n.params.remove(&key) else { return Err(format!("plug-in node {}: {key} isn't a kept field", n.id)) };
        let v = serde_json::from_str(&json).map_err(|e| format!("plug-in node {}: {key}: {e}", n.id))?;
        entry.insert(key.as_str()[EXTRA.len()..].to_owned(), v);
    }
    Ok(())
}

/// A frei0r plug-in's stored form (picture revision 4, d9's fields): a
/// `klipp.frei0r` node whose `plugin` is the entry's `type_id` and whose
/// `p{i}` is its `i`th value by ABI type (a double and a position as f64,
/// which an f32 can't hold; a colour sRGB, as frei0r takes it), so each
/// value has a key and a type in the file. Its entry keeps only its tag
/// (and its slot, keys and modifiers), as an effect's in memory does. In
/// memory it is an effect node like any other: its entry rebuilt, then
/// its values read into its map as every effect's are.
mod frei0r {
    use super::*;

    pub const TAG: &str = "frei0r";

    fn index(key: &ParamKey) -> Option<usize> {
        key.as_str().strip_prefix('p').filter(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit())).and_then(|d| d.parse().ok())
    }

    /// `entry` (`{"type":"frei0r","type_id":…,"params":[…]}`) as `n`'s
    /// stored form; `n`'s map already holds only its own keys.
    pub fn into_stored(n: &mut Node, entry: &Value) -> Result<(), String> {
        let plugin = entry.get("type_id").and_then(Value::as_str).ok_or_else(|| format!("frei0r node {} names no plug-in", n.id))?;
        let values = entry.get("params").and_then(Value::as_array).ok_or_else(|| format!("frei0r node {} has no values", n.id))?;
        // A field of the entry the stored form names no key for is kept
        // under the node's own key, so storing never fails.
        for (k, v) in entry.as_object().into_iter().flatten().filter(|(k, _)| !matches!(k.as_str(), "type" | "type_id" | "params")) {
            n.params.insert(ParamKey::from(format!("{EXTRA}{k}").as_str()), ParamValue::Text(v.to_string()));
        }
        n.params.insert(ParamKey::from("plugin"), ParamValue::Text(plugin.to_owned()));
        for (i, v) in values.iter().enumerate() {
            n.params.insert(ParamKey::from(format!("p{i}").as_str()), value(v).ok_or_else(|| format!("frei0r node {}: value {i} isn't a frei0r value: {v}", n.id))?);
        }
        n.type_id = klipp_graph_format::NodeTypeId::new(lt::FREI0R);
        if let Some(legacy) = n.legacy.as_mut() {
            legacy.payload = serde_json::json!({ "type": TAG });
        }
        Ok(())
    }

    /// A stored `klipp.frei0r` node as the effect node memory holds; any
    /// other node is left as it is.
    pub fn from_stored(n: &mut Node) -> Result<(), String> {
        if n.type_id.as_str() != lt::FREI0R {
            return Ok(());
        }
        let plugin = match n.params.remove(&ParamKey::from("plugin")) {
            Some(ParamValue::Text(p)) => p,
            other => return Err(format!("frei0r node {}: its plug-in is {other:?}, not a name", n.id)),
        };
        let mut values: BTreeMap<usize, Value> = BTreeMap::new();
        for key in n.params.keys().filter(|k| index(k).is_some()).cloned().collect::<Vec<_>>() {
            let i = index(&key).unwrap_or_default();
            let v = n.params.remove(&key).unwrap_or(ParamValue::Bool(false));
            values.insert(i, entry(&v).ok_or_else(|| format!("frei0r node {}: {key} is {v:?}, no frei0r value", n.id))?);
        }
        if values.keys().enumerate().any(|(at, i)| at != *i) {
            return Err(format!("frei0r node {}: its values skip an index", n.id));
        }
        let mut entry = serde_json::json!({ "type": TAG, "type_id": plugin, "params": values.into_values().collect::<Vec<_>>() });
        if let Some(o) = entry.as_object_mut() {
            extras_back(n, o)?;
        }
        let legacy = n.legacy.as_mut().ok_or_else(|| format!("frei0r node {} has no slot", n.id))?;
        legacy.payload = entry;
        n.type_id = klipp_graph_format::NodeTypeId::new(types::LEGACY_EFFECT);
        Ok(())
    }

    /// A stored frei0r value (`{"Double": 0.5}`, as `Frei0rValue` writes
    /// it) as the typed value its ABI type is.
    fn value(v: &Value) -> Option<ParamValue> {
        let (kind, x) = v.as_object().filter(|o| o.len() == 1)?.iter().next()?;
        let floats = |n: usize| -> Option<Vec<f64>> { x.as_array().filter(|a| a.len() == n)?.iter().map(Value::as_f64).collect() };
        Some(match kind.as_str() {
            "Bool" => ParamValue::Bool(x.as_bool()?),
            "Double" => ParamValue::Float64(x.as_f64()?),
            "Color" => {
                let c = floats(3)?;
                ParamValue::Color { rgb: [c[0] as f32, c[1] as f32, c[2] as f32], space: SRGB.into() }
            }
            "Position" => {
                let p = floats(2)?;
                ParamValue::Point2F64([p[0], p[1]])
            }
            "String" => ParamValue::Text(x.as_str()?.to_owned()),
            _ => return None,
        })
    }

    /// [`value`]'s inverse.
    fn entry(v: &ParamValue) -> Option<Value> {
        Some(match v {
            ParamValue::Bool(b) => serde_json::json!({ "Bool": b }),
            ParamValue::Float64(x) => serde_json::json!({ "Double": x }),
            ParamValue::Color { rgb, space } if space == SRGB => serde_json::json!({ "Color": rgb }),
            ParamValue::Point2F64(p) => serde_json::json!({ "Position": p }),
            ParamValue::Text(s) => serde_json::json!({ "String": s }),
            _ => return None,
        })
    }
}

/// Each effect node of `graph`, and of every group body in it, through `f`.
pub fn each_effect_node(graph: &mut Graph, f: &mut dyn FnMut(&mut Node) -> Result<(), String>) -> Result<(), String> {
    for n in &mut graph.nodes {
        if let Some(body) = n.group.as_mut() {
            each_effect_node(&mut body.graph, f)?;
        }
        f(n)?;
    }
    Ok(())
}

/// A picture's effect nodes held in memory ([`into_memory`]).
pub fn picture_into_memory(picture: &mut ClipPicture) -> Result<(), String> {
    each_effect_node(&mut picture.graph, &mut into_memory)
}

/// A picture's effect nodes as stored ([`into_stored`]).
pub fn picture_into_stored(picture: &mut ClipPicture) -> Result<(), String> {
    each_effect_node(&mut picture.graph, &mut into_stored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_effects::registry::effect_types;

    /// Every described effect, at its reset values and at values moved off
    /// them, goes to the map and back the same, typed where its catalog
    /// kind can hold it.
    #[test]
    fn from_params_round_trips_every_effect() {
        let mut n = 0;
        for t in effect_types() {
            let entry = serde_json::to_value((t.sample)()).unwrap();
            let (tag, map) = to_params(&entry).unwrap();
            assert_eq!(tag, t.tag);
            assert_eq!(from_params(&tag, &map).unwrap(), entry, "{}", t.tag);
            assert_eq!(serde_json::to_value(effect(&tag, &map).unwrap()).unwrap(), entry, "{}", t.tag);
            for d in t.desc.params.iter().filter(|d| !d.derived) {
                // A setting the entry leaves out at its default (a crop's
                // cover shape) has nothing to hold.
                let Some(held) = map.get(&ParamKey::from(key_of(d))) else {
                    assert!(entry.get(d.field).is_none(), "{}: {} isn't held", t.tag, key_of(d));
                    continue;
                };
                let typed = match d.kind {
                    ParamKind::Float => matches!(held, ParamValue::Float(_)),
                    ParamKind::Whole => matches!(held, ParamValue::Int(_)),
                    ParamKind::Bool => matches!(held, ParamValue::Bool(_)),
                    ParamKind::Choice(_) => matches!(held, ParamValue::Enum(_)),
                    ParamKind::Colour => matches!(held, ParamValue::Color { .. } | ParamValue::Text(_)),
                    ParamKind::Other => true,
                };
                assert!(typed, "{}: {} is held as {held:?}", t.tag, key_of(d));
            }
            // Each float moved: still the same entry back.
            let mut moved = entry.clone();
            for d in t.desc.params.iter().filter(|d| d.kind == ParamKind::Float && d.index.is_none()) {
                if let Some(v) = moved.get_mut(d.field).filter(|v| v.is_f64()) {
                    *v = serde_json::to_value(v.as_f64().unwrap() as f32 + 0.1).unwrap();
                }
            }
            let (_, map) = to_params(&moved).unwrap();
            assert_eq!(from_params(t.tag, &map).unwrap(), moved, "{} moved", t.tag);
            n += 1;
        }
        // The luma key among them (#1638), displace (#2702), reflection
        // (#2707), the surface pin, the warp and the gradient map (#2758).
        assert_eq!(n, 72, "every described effect");
    }

    /// A frei0r plug-in is stored as `klipp.frei0r` (picture revision 4):
    /// its plug-in by name, each value by ABI index and type, a double and
    /// a position as f64, exactly; held in memory it is the effect node it
    /// was, and stored again the same.
    #[test]
    fn a_frei0r_entry_is_stored_by_index_and_type_and_back() {
        let entry = serde_json::json!({"type": "frei0r", "type_id": "frei0r:/usr/lib/frei0r-1/glow.so:Glow", "params": [
            {"Bool": true}, {"Double": 0.1}, {"Color": [0.25f32, 0.5f32, 1.0f32]}, {"Position": [0.1, 0.7]}, {"String": "soft"}
        ]});
        let mut n: Node = serde_json::from_value(serde_json::json!({"id": uuid::Uuid::nil(), "type_id": types::LEGACY_EFFECT, "label": "", "legacy": {"slot": 0, "payload": entry}})).unwrap();
        into_memory(&mut n).unwrap();
        let memory = n.clone();
        assert_eq!(n.type_id.as_str(), types::LEGACY_EFFECT, "an effect node in memory");
        into_stored(&mut n).unwrap();
        assert_eq!(n.type_id.as_str(), lt::FREI0R);
        assert_eq!(n.legacy.as_ref().unwrap().payload, serde_json::json!({"type": "frei0r"}));
        let p = |k: &str| n.params.get(&ParamKey::from(k)).cloned();
        assert_eq!(p("plugin"), Some(ParamValue::Text("frei0r:/usr/lib/frei0r-1/glow.so:Glow".into())));
        assert_eq!(p("p0"), Some(ParamValue::Bool(true)));
        assert_eq!(p("p1"), Some(ParamValue::Float64(0.1)), "0.1 as f64, which an f32 can't hold");
        assert_eq!(p("p2"), Some(ParamValue::Color { rgb: [0.25, 0.5, 1.0], space: SRGB.into() }));
        assert_eq!(p("p3"), Some(ParamValue::Point2F64([0.1, 0.7])));
        assert_eq!(p("p4"), Some(ParamValue::Text("soft".into())));
        assert!(p("plugin.name").is_none(), "no stamp until an authored edit");
        let stored = serde_json::to_value(&n).unwrap();
        into_memory(&mut n).unwrap();
        assert_eq!(n, memory, "held in memory again, the same");
        assert_eq!(entry_of(&n).unwrap(), entry, "its entry is the one it was");
        into_stored(&mut n).unwrap();
        assert_eq!(serde_json::to_value(&n).unwrap(), stored, "stored again, the same");
        // A stored node that skips an index is refused, not read short.
        let mut gap = n.clone();
        gap.params.remove(&ParamKey::from("p2"));
        assert!(into_memory(&mut gap).unwrap_err().contains("skip"));
    }

    /// An OFX plug-in is stored as `klipp.ofx` (picture revision 4): each
    /// value by name, in its kind, at full precision, an RGB colour's unused
    /// alpha 0; held in memory it is the OFX entry it was, and stored again
    /// the same.
    #[test]
    fn an_ofx_entry_is_stored_by_name_and_kind_and_back() {
        let entry = serde_json::json!({"plugin_name": "org.example.Glow", "enabled": false, "context": "Filter", "transition_amount": 0.1, "param_values": {
            "amount": {"t": "Double", "v": 0.1}, "centre": {"t": "Double2", "v": [0.1, 0.7]}, "axis": {"t": "Double3", "v": [0.1, 0.2, 0.3]},
            "count": {"t": "Int", "v": -3}, "size": {"t": "Int2", "v": [640, 480]}, "box": {"t": "Int3", "v": [1, 2, 3]}, "on": {"t": "Bool", "v": true},
            "mode": {"t": "Choice", "v": -1}, "name": {"t": "String", "v": "soft"}, "tint": {"t": "Rgba", "v": [0.1, 0.2, 0.3, 0.4]}, "key": {"t": "Rgb", "v": [0.5, 0.6, 0.7]},
            "old": {"Double": 0.5}
        }});
        let mut n: Node = serde_json::from_value(serde_json::json!({"id": uuid::Uuid::nil(), "type_id": types::LEGACY_OFX, "label": "", "legacy": {"slot": 0, "payload": entry}})).unwrap();
        let memory = n.clone();
        into_stored(&mut n).unwrap();
        assert_eq!(n.type_id.as_str(), lt::OFX);
        assert!(!n.enabled, "the entry's flag is the node's");
        assert_eq!(n.legacy.as_ref().unwrap().payload, Value::Null);
        let p = |k: &str| n.params.get(&ParamKey::from(k)).cloned();
        assert_eq!(p("plugin"), Some(ParamValue::Text("org.example.Glow".into())));
        assert_eq!(p("context"), Some(ParamValue::Text("Filter".into())));
        assert_eq!(p("transition_amount"), Some(ParamValue::Float64(0.1)));
        assert_eq!(p("p.amount"), Some(ParamValue::Float64(0.1)));
        assert_eq!(p("p.centre"), Some(ParamValue::Point2F64([0.1, 0.7])));
        assert_eq!(p("p.axis"), Some(ParamValue::Point3F64([0.1, 0.2, 0.3])));
        assert_eq!(p("p.count"), Some(ParamValue::Int(-3)));
        assert_eq!(p("p.size"), Some(ParamValue::Int2([640, 480])));
        assert_eq!(p("p.box"), Some(ParamValue::Int3([1, 2, 3])));
        assert_eq!(p("p.on"), Some(ParamValue::Bool(true)));
        assert_eq!(p("p.mode"), Some(ParamValue::Choice(-1)));
        assert_eq!(p("p.name"), Some(ParamValue::Text("soft".into())));
        assert_eq!(p("p.tint"), Some(ParamValue::ColorF64 { rgba: [0.1, 0.2, 0.3, 0.4], alpha: true }));
        assert_eq!(p("p.key"), Some(ParamValue::ColorF64 { rgba: [0.5, 0.6, 0.7, 0.0], alpha: false }));
        assert_eq!(p("p-json.old"), Some(ParamValue::Text("{\"Double\":0.5}".into())), "a value in another shape kept as its JSON");
        let stored = serde_json::to_value(&n).unwrap();
        into_memory(&mut n).unwrap();
        assert_eq!(n, memory, "the OFX entry it was");
        into_stored(&mut n).unwrap();
        assert_eq!(serde_json::to_value(&n).unwrap(), stored, "stored again, the same");
    }

    /// Storing a plug-in never fails: an OFX node off as a node, and an
    /// entry field the stored form names no key for, are kept under the
    /// node's own keys and come back as they were.
    #[test]
    fn a_plug_in_node_off_or_with_an_extra_field_is_kept_not_refused() {
        let ofx = serde_json::json!({"plugin_name": "org.example.Glow", "enabled": true, "context": "Filter", "transition_amount": 0.0, "param_values": {}, "later": [1, 2]});
        let mut n: Node = serde_json::from_value(serde_json::json!({"id": uuid::Uuid::nil(), "type_id": types::LEGACY_OFX, "label": "", "legacy": {"slot": 0, "payload": ofx}})).unwrap();
        n.enabled = false;
        let memory = n.clone();
        into_stored(&mut n).unwrap();
        assert!(n.enabled, "the entry's flag is the node's");
        into_memory(&mut n).unwrap();
        assert_eq!(n, memory, "off as a node and its extra field, back as they were");

        let frei0r = serde_json::json!({"type": "frei0r", "type_id": "frei0r:/x.so:X", "params": [{"Bool": true}]});
        let mut n: Node = serde_json::from_value(serde_json::json!({"id": uuid::Uuid::nil(), "type_id": types::LEGACY_EFFECT, "label": "", "legacy": {"slot": 0, "payload": frei0r}})).unwrap();
        into_memory(&mut n).unwrap();
        n.params.insert(ParamKey::from("later"), ParamValue::Text("[1,2]".into()));
        let memory = n.clone();
        into_stored(&mut n).unwrap();
        assert_eq!(n.type_id.as_str(), lt::FREI0R);
        into_memory(&mut n).unwrap();
        assert_eq!(n, memory, "the extra field back as it was");
    }

    #[test]
    fn the_spill_alias_and_elements_are_keyed_as_the_animation_is() {
        let key = serde_json::json!({"type": "chroma_key", "key_color": [20, 200, 40], "tolerance": 0.3, "soft_edge": 0.1, "spill_method": "none", "spill_strength": 0.5});
        let (_, map) = to_params(&key).unwrap();
        assert!(map.contains_key(&ParamKey::from("soft_edge")) && !map.contains_key(&ParamKey::from("spill")));
        let pin = serde_json::to_value(effect_type("corner_pin").map(|t| (t.sample)()).unwrap()).unwrap();
        let (_, map) = to_params(&pin).unwrap();
        assert!(map.contains_key(&ParamKey::from("tl_x")), "{map:?}");
        assert_eq!(from_params("corner_pin", &map).unwrap(), pin);
    }

    #[test]
    fn what_a_kind_cant_hold_is_held_as_its_json() {
        // A colour with alpha, an integer where a float is described, and
        // a field the catalog doesn't know.
        let fade = serde_json::json!({"type": "color_fade", "duration_secs": 1, "mode": "out", "color": [0, 0, 0, 255], "progress": 0.0, "extra": {"a": [1]}});
        let (_, map) = to_params(&fade).unwrap();
        assert_eq!(map[&ParamKey::from("color")], ParamValue::Text("[0,0,0,255]".into()));
        assert_eq!(map[&ParamKey::from("extra")], ParamValue::Text("{\"a\":[1]}".into()));
        assert_eq!(from_params("color_fade", &map).unwrap(), fade);
    }

    #[test]
    fn the_nodes_own_keys_are_not_fields() {
        let entry = serde_json::json!({"type": "invert", "mix": 0.5});
        let (_, mut map) = to_params(&entry).unwrap();
        map.insert(ParamKey::from("mask.channel"), ParamValue::Text("green".into()));
        assert_eq!(from_params("invert", &map).unwrap(), entry);
    }
}
