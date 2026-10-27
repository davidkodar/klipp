//! Read the same lifted pictures the graph editor addresses.

use klipp_graph_format::{Node, ParamValue, types};
use klipp_graph_format::path::{Root, TrackPart};
use klipp_graph_lift::paths::Documents;
use klipp_effects::registry::{NodeTypeDesc, ParamKind};
use klipp_timeline::Sequence;
use serde_json::{json, Value};

pub(crate) fn documents(seq: &Sequence, root: Root) -> Result<Documents, String> {
    let mut docs = Documents::default();
    match root {
        Root::Clip { clip } => {
            let c = seq.tracks.iter().flat_map(|t| &t.clips).find(|c| c.id.0 == clip).ok_or("no such clip on this sequence's tracks")?;
            docs.clips.insert(clip, c.picture_v2()?.picture.clone());
        }
        // A sequence's own grades, lifted as the editor lifts them (with
        // their corrections' mix and mask channel, #1321).
        Root::TimelineGrade { sequence } => {
            let grade = seq.root_grade(root)?.ok_or("this sequence has no timeline grade")?;
            docs.timeline.insert(sequence, seq.lift_root_grade(root, grade)?);
        }
        Root::GroupStage { group, stage } => {
            let grade = seq.root_grade(root)?.ok_or("this group has no such stage")?;
            docs.stages.insert((group, stage), seq.lift_root_grade(root, grade)?);
        }
        Root::Track { track, part } => {
            let (effects, grade) = seq.lift_track_parts(track)?;
            match part {
                TrackPart::Effects => { docs.track_effects.insert(track, effects); }
                TrackPart::Grade => { docs.track_grades.insert(track, grade.ok_or("this track has no grade")?); }
            }
        }
    }
    Ok(docs)
}

pub(crate) fn descriptor(node: &Node) -> Option<&'static NodeTypeDesc> {
    if node.type_id.as_str() == types::LEGACY_EFFECT {
        return klipp_effects::registry::effect_type(klipp_graph_lift::effect_params::tag(node)?).map(|t| &t.desc);
    }
    if node.type_id.as_str() == types::LEGACY_NODE {
        let kind = serde_json::from_value(node.legacy.as_ref()?.payload.get("kind")?.clone()).ok()?;
        return klipp_nodes::registry::node_type(kind).map(|t| &t.desc);
    }
    klipp_effects::registry::effect_types().map(|t| &t.desc)
        .chain(klipp_nodes::registry::node_types().map(|t| &t.desc))
        .find(|d| d.id == node.type_id.as_str())
}

pub(crate) fn new_node(type_id: &str, id: uuid::Uuid, label: Option<&str>) -> Result<Node, String> {
    let (type_id, payload) = if let Some(t) = klipp_effects::registry::effect_types().find(|t| t.desc.id == type_id) {
        (types::LEGACY_EFFECT, serde_json::to_value((t.sample)()).map_err(|e| e.to_string())?)
    } else if let Some(t) = klipp_nodes::registry::node_types().find(|t| t.desc.id == type_id) {
        let kind = format!("{:?}", t.kind);
        if klipp_nodes::kinds::addable_node_kind(&kind).is_none() { return Err("this structural node cannot be added".into()); }
        (types::LEGACY_NODE, json!({"kind":t.kind,"label":label.unwrap_or(t.desc.name),"params":klipp_nodes::NodeParams::default()}))
    } else { return Err(format!("unknown node type {type_id}; read node_types")); };
    serde_json::from_value(json!({"id":id,"type_id":type_id,"label":label.unwrap_or(""),"legacy":{"payload":payload}})).map_err(|e| e.to_string())
}

pub(crate) fn parameter_op(node: &Node, path: klipp_graph_format::path::NodePath, key: &str, value: &Value) -> Result<klipp_graph_lift::ops::GraphOp, String> {
    use klipp_graph_lift::ops::GraphOp;
    if key == "masks" && descriptor(node).is_some_and(|d| d.id == "klipp.grade.mask") {
        return Ok(GraphOp::SetPayload { path, payload: crate::graph_masks::payload(node, value)? });
    }
    if let Some(payload) = frei0r_payload(node, key, value)? {
        return Ok(GraphOp::SetPayload { path, payload });
    }
    Ok(GraphOp::SetParam { path, key: key.into(), value: parameter(node, key, value)? })
}

pub(crate) fn parameter(node: &Node, key: &str, value: &Value) -> Result<ParamValue, String> {
    let desc = descriptor(node).ok_or("this node has no registered parameter description")?;
    let p = desc.params.iter().find(|p| p.key == key && !p.derived).ok_or_else(|| format!("unknown or derived parameter {key}"))?;
    // The graph engine currently writes a flat saved field. Refuse aliases,
    // nested fields and array components instead of reporting a silent no-op.
    // A keyframe alias (a chroma key's `spill` for its `soft_edge`) is
    // written to its field by the effect's own setter.
    let aliased = p.key != p.field && node.type_id.as_str() == types::LEGACY_EFFECT;
    // A grade node's keyed setting (a luma key's `luma_key_low`, saved as
    // `luma_key.low`) is written by the grade's own override (#2758),
    // which takes a number or a switch.
    let grade_keyed = p.key != p.field && node.type_id.as_str() == types::LEGACY_NODE && p.index.is_none() && klipp_nodes::NodeParams::default().apply_override(p.key, 1.0);
    // A difference key's clean frame (#2758): seconds into its clip's media.
    let clean_frame = node.type_id.as_str() == types::LEGACY_NODE && p.field == "difference_key.plate_frame";
    // A grade node's setting inside its settings' struct (a difference
    // key's `difference_key.tolerance`, #2758): a number or a switch the
    // lift writes into the struct, made at its default when unset.
    let struct_field = node.type_id.as_str() == types::LEGACY_NODE && p.key == p.field && p.index.is_none() && p.field.matches('.').count() == 1 && matches!(p.kind, ParamKind::Float | ParamKind::Whole | ParamKind::Bool);
    if !grade_keyed && !clean_frame && !struct_field && ((p.key != p.field && !aliased) || p.index.is_some() || p.field.contains('.')) {
        return Err(format!("{key} needs a saved-field adapter before it can be set through graph operations"));
    }
    let wrong = || format!("{key} expects {:?}", p.kind);
    let number = || -> Result<f32, String> {
        let n = value.as_f64().ok_or_else(wrong)?;
        let f = n as f32;
        if !n.is_finite() || !f.is_finite() { return Err(format!("{key} must be finite")); }
        if p.range.is_some_and(|(min, max)| f < min || f > max) { return Err(format!("{key} is outside its catalog range")); }
        Ok(f)
    };
    match p.kind {
        ParamKind::Float => Ok(ParamValue::Float(number()?)),
        ParamKind::Whole => {
            number()?;
            let n = value.as_u64().filter(|n| *n <= i32::MAX as u64).ok_or_else(wrong)?;
            Ok(ParamValue::Int(n as i32))
        }
        ParamKind::Bool => value.as_bool().map(ParamValue::Bool).ok_or_else(wrong),
        ParamKind::Choice(options) => {
            let choice = value.as_str().ok_or_else(wrong)?;
            let at = options.iter().position(|name| *name == choice).ok_or_else(wrong)?;
            let entry = (node.type_id.as_str() == types::LEGACY_EFFECT).then(|| klipp_graph_lift::effect_params::entry_of(node).ok()).flatten();
            let stored = match &entry {
                Some(entry) => entry.get(p.field),
                None => node.legacy.as_ref().and_then(|l| {
                    let payload = if node.type_id.as_str() == types::LEGACY_NODE { l.payload.get("params")? } else { &l.payload };
                    payload.get(p.field)
                }),
            };
            if stored.is_some_and(Value::is_string) { Ok(ParamValue::Text(choice.into())) }
            else { Ok(ParamValue::Enum(at as u32)) }
        }
        // An effect's colour (#2758), sRGB from 0 to 1 as the effect holds
        // it: `[r, g, b]`, `[r, g, b, a]` (its alpha unused) or `#rrggbb`.
        ParamKind::Colour if node.type_id.as_str() == types::LEGACY_EFFECT => {
            let hex = value.as_str().and_then(|s| s.strip_prefix('#')).filter(|h| h.len() == 6);
            let rgb: Vec<f32> = match hex {
                Some(h) => (0..3).map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).map(|b| b as f32 / 255.0)).collect::<Result<_, _>>().map_err(|_| wrong())?,
                None => value.as_array().ok_or_else(wrong)?.iter().map(|c| c.as_f64().map(|c| c as f32)).collect::<Option<_>>().ok_or_else(wrong)?,
            };
            match rgb.as_slice() {
                [r, g, b] | [r, g, b, _] if [r, g, b].iter().all(|c| (0.0..=1.0).contains(*c)) => {
                    Ok(ParamValue::Color { rgb: [*r, *g, *b], space: klipp_graph_lift::effect_params::SRGB.into() })
                }
                _ => Err(format!("{key} takes [r, g, b] from 0 to 1, or \"#rrggbb\"")),
            }
        }
        // An effect's text, as a media source's media id (#2758): a field
        // the effect stores as text takes text.
        ParamKind::Other if node.type_id.as_str() == types::LEGACY_EFFECT && value.is_string() && effect_stores_text(node, p.field) => {
            Ok(ParamValue::Text(value.as_str().unwrap_or_default().into()))
        }
        // A source time from 0; past the media's end the render refuses
        // it, said, as it knows the media's length.
        ParamKind::Other if clean_frame => match value.as_f64() {
            Some(secs) if secs.is_finite() && secs >= 0.0 && (secs as f32).is_finite() => Ok(ParamValue::Float(secs as f32)),
            _ => Err(format!("{key} takes the source seconds of a frame of the clip's own media, a number from 0")),
        },
        ParamKind::Colour | ParamKind::Other => Err(format!("{key} requires its native editor until its value has a typed graph representation")),
    }
}


/// Whether the effect node `node` stores `field` as text.
fn effect_stores_text(node: &Node, field: &str) -> bool {
    klipp_graph_lift::effect_params::tag(node)
        .and_then(klipp_effects::registry::effect_type)
        .and_then(|t| serde_json::to_value((t.sample)()).ok())
        .is_some_and(|sample| sample.get(field).is_some_and(Value::is_string))
}

/// A frei0r effect's entry with its plug-in (`type_id`, "frei0r:<path>:<name>")
/// or its values (`params`, `[{"Double": 0.5}, ...]` in the plug-in's
/// order) set: the whole entry, checked as the effect it is. `None` for any
/// other node or key.
fn frei0r_payload(node: &Node, key: &str, value: &Value) -> Result<Option<Value>, String> {
    use klipp_effects::BuiltinEffect;
    // Only an effect entry's payload is tagged with its type.
    if klipp_graph_lift::effect_params::tag(node) != Some("frei0r") || !matches!(key, "type_id" | "params") {
        return Ok(None);
    }
    let entry = klipp_graph_lift::effect_params::entry_of(node)?;
    let Ok(BuiltinEffect::Frei0r { mut type_id, mut params }) = serde_json::from_value::<BuiltinEffect>(entry) else {
        return Err("this frei0r node's entry isn't a frei0r effect".into());
    };
    if key == "type_id" {
        let id = value.as_str().filter(|s| s.starts_with("frei0r:")).ok_or("type_id is a frei0r plug-in's id, frei0r:<path>:<name>")?;
        if id != type_id {
            // Another plug-in's values aren't this one's.
            params.clear();
        }
        type_id = id.to_owned();
    } else {
        params = serde_json::from_value(value.clone()).map_err(|e| format!("params are the plug-in's values in order, each like {{\"Double\": 0.5}}: {e}"))?;
    }
    serde_json::to_value(BuiltinEffect::Frei0r { type_id, params }).map(Some).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A difference key's clean frame (#2758) takes source seconds from 0,
    /// and nothing else, said.
    #[test]
    fn a_clean_frame_takes_seconds_from_zero() {
        let key = new_node("klipp.grade.difference_key", uuid::Uuid::new_v4(), None).unwrap();
        assert_eq!(parameter(&key, "difference_key.plate_frame", &json!(2.5)).unwrap(), ParamValue::Float(2.5));
        // Its other settings, inside its struct, as numbers and switches.
        assert_eq!(parameter(&key, "difference_key.tolerance", &json!(0.2)).unwrap(), ParamValue::Float(0.2));
        assert_eq!(parameter(&key, "difference_key.invert", &json!(true)).unwrap(), ParamValue::Bool(true));
        for bad in [json!(-1), json!("2"), json!(null), json!(1e300)] {
            let why = parameter(&key, "difference_key.plate_frame", &bad).unwrap_err();
            assert!(why.contains("source seconds"), "{bad}: {why}");
        }
    }

    #[test]
    fn scalar_values_are_typed_and_soft_slider_ranges_are_not_hard_limits() {
        let blur = new_node("klipp.fx.gaussian_blur", uuid::Uuid::new_v4(), None).unwrap();
        assert_eq!(parameter(&blur, "radius", &json!(125)).unwrap(), ParamValue::Float(125.0));
        assert!(parameter(&blur, "radius", &json!("12")).is_err());
        assert!(parameter(&blur, "radius", &json!(1e100)).is_err());
        assert!(parameter(&blur, "not_a_parameter", &json!(1)).is_err());
        let median = new_node("klipp.fx.median_filter", uuid::Uuid::new_v4(), None).unwrap();
        assert!(parameter(&median, "radius", &json!(1.5)).is_err());
        assert!(parameter(&median, "radius", &json!(-1)).is_err());
        assert_eq!(parameter(&median, "radius", &json!(2)).unwrap(), ParamValue::Int(2));
        let radial = new_node("klipp.fx.radial_blur", uuid::Uuid::new_v4(), None).unwrap();
        assert_eq!(parameter(&radial, "mode", &json!("spin")).unwrap(), ParamValue::Enum(1));
        assert!(parameter(&radial, "mode", &json!("not a mode")).is_err());
    }

    /// An effect's keyframe alias (a chroma key's `spill`) is taken: the
    /// effect's own setter writes it to its field (`soft_edge`), which
    /// `recipes::tests::the_recipe_op_is_one_picture_step` reads back. A
    /// grade node's alias is still refused.
    #[test]
    fn an_effects_alias_is_taken_for_its_field() {
        let node = new_node("klipp.fx.chroma_key", uuid::Uuid::new_v4(), None).unwrap();
        assert_eq!(parameter(&node, "spill", &json!(0.2)).unwrap(), ParamValue::Float(0.2));
    }

    /// A frei0r node added by its registry id takes its plug-in and its
    /// values as typed writes of its entry; another plug-in drops the
    /// values; a value that isn't one, or an id that isn't frei0r's, is
    /// refused; any other effect is left to its own parameters.
    #[test]
    fn a_frei0r_nodes_plug_in_and_values_are_set_whole() {
        use klipp_effects::BuiltinEffect;
        let mut node = new_node("klipp.fx.frei0r", uuid::Uuid::new_v4(), None).unwrap();
        let set = |node: &Node, key: &str, value: Value| frei0r_payload(node, key, &value).map(|p| p.map(|p| serde_json::from_value::<BuiltinEffect>(p).unwrap()));
        let id = "frei0r:/usr/lib/frei0r-1/saturat0r.so:saturat0r";
        let with_plugin = set(&node, "type_id", json!(id)).unwrap().unwrap();
        assert!(matches!(&with_plugin, BuiltinEffect::Frei0r { type_id, params } if type_id == id && params.is_empty()));
        node.legacy.as_mut().unwrap().payload = serde_json::to_value(&with_plugin).unwrap();
        let valued = set(&node, "params", json!([{"Double": 0.25}])).unwrap().unwrap();
        assert!(matches!(&valued, BuiltinEffect::Frei0r { params, .. } if params.len() == 1));
        node.legacy.as_mut().unwrap().payload = serde_json::to_value(&valued).unwrap();
        let other = set(&node, "type_id", json!("frei0r:/x.so:x")).unwrap().unwrap();
        assert!(matches!(&other, BuiltinEffect::Frei0r { params, .. } if params.is_empty()), "another plug-in's values aren't kept");
        assert!(set(&node, "params", json!([0.25])).is_err(), "a bare number isn't a frei0r value");
        assert!(set(&node, "type_id", json!("saturat0r")).is_err(), "not a frei0r id");
        let blur = new_node("klipp.fx.gaussian_blur", uuid::Uuid::new_v4(), None).unwrap();
        assert!(frei0r_payload(&blur, "type_id", &json!(id)).unwrap().is_none());
    }
}
