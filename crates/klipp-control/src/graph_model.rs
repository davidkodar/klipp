//! Read the same lifted pictures the graph editor addresses.

use klipp_graph_format::{Node, ParamValue, types};
use klipp_graph_format::path::{Root, TrackPart};
use klipp_graph_format::lifted::Stage;
use klipp_graph_lift::{paths::Documents, to_v2, lift, splice};
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
        Root::TimelineGrade { sequence } => {
            if sequence != seq.id.0 { return Err("no such active sequence".into()); }
            let raw = json!({"id":sequence,"timeline_graph":seq.timeline_grade()});
            let graph = to_v2::timeline_to_v2(raw.as_object().unwrap()).map_err(|e| e.to_string())?.ok_or("this sequence has no timeline grade")?;
            docs.timeline.insert(sequence, lift::lift_grade_only(&graph, false)?);
        }
        Root::GroupStage { group, stage } => {
            let group_data = seq.grade_groups.iter().find(|g| g.id.0 == group).ok_or("no such grade group")?;
            let raw = serde_json::to_value(group_data).map_err(|e| e.to_string())?;
            let (pre, post) = to_v2::stages_to_v2(raw.as_object().ok_or("a grade group must be an object")?).map_err(|e| e.to_string())?;
            let graph = match stage { Stage::Pre => pre, Stage::Post => post }.ok_or("this group has no such stage")?;
            docs.stages.insert((group, stage), splice::stage_body(&graph)?);
        }
        Root::Track { track, part } => {
            let track_data = seq.tracks.iter().find(|t| t.id.0 == track).ok_or("no such track")?;
            let raw = serde_json::to_value(track_data).map_err(|e| e.to_string())?;
            let graph = to_v2::track_to_v2(raw.as_object().ok_or("a track must be an object")?).map_err(|e| e.to_string())?;
            let (effects, grade) = splice::track_parts(&graph)?;
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
        return klipp_effects::registry::effect_type(node.legacy.as_ref()?.payload.get("type")?.as_str()?).map(|t| &t.desc);
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

pub(crate) fn parameter(node: &Node, key: &str, value: &Value) -> Result<ParamValue, String> {
    let desc = descriptor(node).ok_or("this node has no registered parameter description")?;
    let p = desc.params.iter().find(|p| p.key == key && !p.derived).ok_or_else(|| format!("unknown or derived parameter {key}"))?;
    // The graph engine currently writes a flat saved field. Refuse aliases,
    // nested fields and array components instead of reporting a silent no-op.
    if p.key != p.field || p.index.is_some() || p.field.contains('.') {
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
            let stored = node.legacy.as_ref().and_then(|l| {
                let payload = if node.type_id.as_str() == types::LEGACY_NODE { l.payload.get("params")? } else { &l.payload };
                payload.get(p.field)
            });
            if stored.is_some_and(Value::is_string) { Ok(ParamValue::Text(choice.into())) }
            else { Ok(ParamValue::Enum(at as u32)) }
        }
        ParamKind::Colour | ParamKind::Other => Err(format!("{key} requires its native editor until its value has a typed graph representation")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn aliases_do_not_report_success_for_an_ignored_saved_key() {
        let node = new_node("klipp.fx.chroma_key", uuid::Uuid::new_v4(), None).unwrap();
        assert!(parameter(&node, "spill", &json!(0.2)).unwrap_err().contains("saved-field adapter"));
    }
}
