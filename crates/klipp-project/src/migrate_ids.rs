//! A saved project read through the id migration table (#2888): an
//! effect, a grade node or an effect's setting saved under a name that
//! has since been renamed reads as it is called now, and one that has
//! been removed is refused with the table's reason.
//!
//! A project saves effects by their serde tag and fields, and grade nodes
//! by their kind's name, not by the stable ids the table is written in.
//! Their ids follow from those names: an effect's is `klipp.fx.<tag>`
//! (as its registry entry gives it, while it is declared), a grade
//! node's `klipp.grade.<kind in snake case>`, and a setting's key is its
//! field's, as its registry entry gives it. A grade node kind neither
//! declared nor in the table is refused here by name; an effect or a
//! setting is left as it is, for the load to refuse or keep.
//!
//! A grade node's settings are saved as `NodeParams`' nested fields, not
//! by their keys; a renamed one is read by a serde alias there.

use klipp_timeline::registry_ids::{resolve_in, Migration, Resolved, Subject, MIGRATIONS};
use serde_json::Value;

/// The effect node type a saved picture stores a built-in effect under.
const LEGACY_EFFECT: &str = "klipp.legacy.effect";

/// `project`, its renamed effects, grade nodes and effect settings read
/// as they are called now, through [`MIGRATIONS`].
pub fn migrate(project: &mut Value) -> Result<(), String> {
    migrate_in(project, MIGRATIONS)
}

/// [`migrate`] through `table`.
pub fn migrate_in(project: &mut Value, table: &[Migration]) -> Result<(), String> {
    let resolve = |s: Subject<'_>| resolve_in(table, s, &klipp_timeline::registry_ids::is_current);
    walk(project, &resolve)
}

fn walk(v: &mut Value, resolve: &dyn Fn(Subject<'_>) -> Resolved) -> Result<(), String> {
    if v.get("type_id").and_then(Value::as_str) == Some(LEGACY_EFFECT) {
        effect(v, resolve)?;
    }
    // A grade node: its kind by name, its label and its settings (in a
    // saved picture's group body, a `klipp.legacy.node`'s payload).
    if v.get("params").is_some_and(Value::is_object) && v.get("label").is_some_and(Value::is_string) {
        if let Some(Value::String(kind)) = v.get_mut("kind") {
            grade_kind(kind, resolve)?;
        }
    }
    match v {
        Value::Object(o) => o.values_mut().try_for_each(|c| walk(c, resolve)),
        Value::Array(a) => a.iter_mut().try_for_each(|c| walk(c, resolve)),
        _ => Ok(()),
    }
}

/// What a removed or looping id refuses the project with.
fn refused(what: &str, name: &str, r: &Resolved) -> Option<String> {
    match r {
        Resolved::Removed(why) => Some(format!("This project has the {what} {name}, which Klipp no longer has: {why}.")),
        Resolved::Loops => Some(format!("This project has the {what} {name}, whose renames run in a loop in this Klipp.")),
        Resolved::Current(_) | Resolved::Unknown => None,
    }
}

/// A saved effect node's type, settings and animated settings as they
/// are called now.
fn effect(node: &mut Value, resolve: &dyn Fn(Subject<'_>) -> Resolved) -> Result<(), String> {
    let Some(tag) = node.pointer("/legacy/payload/type").and_then(Value::as_str).map(str::to_owned) else { return Ok(()) };
    let t = match klipp_effects::registry::effect_type(&tag) {
        Some(t) => t,
        None => {
            let r = resolve(Subject::Type(&format!("klipp.fx.{tag}")));
            if let Some(why) = refused("effect", &tag, &r) {
                return Err(why);
            }
            let Resolved::Current(id) = r else { return Ok(()) };
            let Some(t) = klipp_effects::registry::effect_types().find(|t| t.desc.id == id) else { return Ok(()) };
            node["legacy"]["payload"]["type"] = Value::from(t.tag);
            t
        }
    };
    let params = t.desc.params;
    // A setting by its saved name: its field, or an element's key.
    let saved = |d: &klipp_effects::registry::ParamDesc| if d.index.is_some() { d.key } else { d.field };
    let renamed = |name: &str| -> Result<Option<&'static str>, String> {
        if params.iter().any(|d| d.field == name || saved(d) == name) {
            return Ok(None);
        }
        let r = resolve(Subject::Param(t.desc.id, name));
        if let Some(why) = refused(&format!("{} setting", t.desc.name), name, &r) {
            return Err(why);
        }
        Ok(match r {
            Resolved::Current(key) => params.iter().find(|d| d.key == key).map(saved),
            _ => None,
        })
    };
    for at in ["/legacy/payload", "/anim"] {
        let Some(Value::Object(fields)) = node.pointer_mut(at) else { continue };
        let names: Vec<String> = fields.keys().filter(|k| *k != "type").cloned().collect();
        for name in names {
            if let Some(now) = renamed(&name)? {
                if !fields.contains_key(now) {
                    let value = fields.remove(&name).expect("a key just listed");
                    fields.insert(now.to_owned(), value);
                }
            }
        }
    }
    Ok(())
}

/// A saved grade node's kind as it is called now.
fn grade_kind(kind: &mut String, resolve: &dyn Fn(Subject<'_>) -> Resolved) -> Result<(), String> {
    if matches!(serde_json::from_value::<klipp_nodes::NodeKind>(Value::from(kind.as_str())), Ok(k) if k != klipp_nodes::NodeKind::Unknown) {
        return Ok(());
    }
    let r = resolve(Subject::Type(&kind_id(kind)));
    if let Some(why) = refused("grade node", kind, &r) {
        return Err(why);
    }
    match r {
        Resolved::Current(id) => {
            if let Some(t) = klipp_nodes::registry::node_types().find(|t| t.desc.id == id) {
                *kind = format!("{:?}", t.kind);
            }
            Ok(())
        }
        // Read, it would run as a passthrough and the next save would
        // write it away: refused by name, the file left as it is.
        _ => Err(format!("This project has a grade node of kind {kind}, which this Klipp does not know. It was saved by a newer Klipp: open it there. Opening it here would lose it on the next save.")),
    }
}

/// A grade node kind's id: its name in snake case, an acronym one word
/// and a number its own (`RGBCurves` `klipp.grade.rgb_curves`, `Lut3D`
/// `klipp.grade.lut_3d`).
pub fn kind_id(kind: &str) -> String {
    let c: Vec<char> = kind.chars().collect();
    let mut out = String::from("klipp.grade.");
    for (i, &ch) in c.iter().enumerate() {
        let prev = i.checked_sub(1).map(|p| c[p]);
        let next = c.get(i + 1).copied();
        let word = match prev {
            None => false,
            Some(p) if ch.is_ascii_digit() => !p.is_ascii_digit(),
            Some(p) if ch.is_ascii_uppercase() => p.is_ascii_lowercase() || (p.is_ascii_uppercase() && next.is_some_and(|n| n.is_ascii_lowercase())),
            Some(_) => false,
        };
        if word {
            out.push('_');
        }
        out.push(ch.to_ascii_lowercase());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_timeline::registry_ids::Change;

    /// Every grade node kind saved today maps to its registry id by the
    /// rule a renamed kind's old name is read by.
    #[test]
    fn a_kind_names_its_id() {
        for t in klipp_nodes::registry::node_types() {
            assert_eq!(kind_id(&format!("{:?}", t.kind)), t.desc.id);
        }
    }

    fn effect_node(payload: Value, anim: Value) -> Value {
        serde_json::json!({"nodes": [{"id": "n", "type_id": LEGACY_EFFECT, "legacy": {"payload": payload, "slot": 0}, "anim": anim}]})
    }

    #[test]
    fn renamed_effects_nodes_and_settings_read_as_they_are_now_and_removed_ones_say_why() {
        let table = [
            Migration { old: Subject::Type("klipp.fx.old_blur"), change: Change::Renamed("klipp.fx.gaussian_blur") },
            Migration { old: Subject::Param("klipp.fx.gaussian_blur", "size"), change: Change::Renamed("radius") },
            Migration { old: Subject::Type("klipp.fx.retired"), change: Change::Removed("drawn by gaussian_blur now") },
            Migration { old: Subject::Type("klipp.grade.old_exposure"), change: Change::Renamed("klipp.grade.exposure") },
            Migration { old: Subject::Type("klipp.grade.gone"), change: Change::Removed("folded into the corrector") },
        ];
        let mut p = effect_node(serde_json::json!({"type": "old_blur", "size": 4.0}), serde_json::json!({"size": {"keys": []}}));
        migrate_in(&mut p, &table).unwrap();
        assert_eq!(p["nodes"][0]["legacy"]["payload"], serde_json::json!({"type": "gaussian_blur", "radius": 4.0}));
        assert_eq!(p["nodes"][0]["anim"], serde_json::json!({"radius": {"keys": []}}));

        let mut gone = effect_node(serde_json::json!({"type": "retired"}), Value::Null);
        assert!(migrate_in(&mut gone, &table).unwrap_err().contains("drawn by gaussian_blur now"));
        let mut unknown = effect_node(serde_json::json!({"type": "never_was", "x": 1}), Value::Null);
        let before = unknown.clone();
        migrate_in(&mut unknown, &table).unwrap();
        assert_eq!(unknown, before, "left for the load to refuse by name");

        let mut g = serde_json::json!({"node_graph": {"nodes": [{"id": "a", "kind": "OldExposure", "label": "", "params": {}}, {"payload": {"kind": "Corrector", "label": "", "params": {}}}]}});
        migrate_in(&mut g, &table).unwrap();
        assert_eq!(g["node_graph"]["nodes"][0]["kind"], "Exposure");
        assert_eq!(g["node_graph"]["nodes"][1]["payload"]["kind"], "Corrector");
        let mut g = serde_json::json!({"nodes": [{"payload": {"kind": "Shiny", "label": "", "params": {}}}]});
        assert!(migrate_in(&mut g, &table).unwrap_err().contains("kind Shiny, which this Klipp does not know"));
        let mut g = serde_json::json!({"nodes": [{"legacy": {"payload": {"kind": "Gone", "label": "", "params": {}}}}]});
        assert!(migrate_in(&mut g, &table).unwrap_err().contains("folded into the corrector"));
    }
}
