//! Bounded static windows for the graph wire; the saved MaskStack stays unchanged.

use klipp_graph_format::Node;
use serde_json::Value;

pub(crate) const FORMAT: &str = r#"Static JSON: {"masks":[{"shape":{"Rectangle":{"center_x":0.5,"center_y":0.5,"width":0.5,"height":0.5,"rotation":0}},"feather":0,"opacity":1,"invert":false,"enabled":true}]}. Other shapes: {"Circle":{"center_x":0.5,"center_y":0.5,"radius":0.2}}, {"LinearGradient":{"start_x":0,"start_y":0,"end_x":1,"end_y":1}}, {"Bezier":{"points":[[0,0],[1,0],[0.5,1]]}}. At most 16 masks, 3..256 Bezier points each. Coordinates, sizes, feather and opacity: 0..1; rotation: -180..180. Finite numbers only; unknown fields and non-empty keyframes/tracking refused. {"masks":[]} clears shapes."#;

fn fields<'a>(value: &'a Value, allowed: &[&str], at: &str) -> Result<&'a serde_json::Map<String, Value>, String> {
    let object = value.as_object().ok_or_else(|| format!("{at} must be an object"))?;
    if let Some(key) = object.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(format!("{at}.{key} is an unknown field"));
    }
    Ok(object)
}

// Bounds match the Mask editor in panels/node_inspector.rs::render_masks_body.
fn number(value: &Value, at: &str, min: f32, max: f32) -> Result<(), String> {
    let n = value.as_f64().ok_or_else(|| format!("{at} must be a finite number"))?;
    if !(n as f32).is_finite() { return Err(format!("{at} must be finite")); }
    if !((min as f64)..=(max as f64)).contains(&n) { return Err(format!("{at} must be in {min}..{max}")); }
    Ok(())
}

pub(crate) fn validate(value: &Value) -> Result<klipp_core::MaskStack, String> {
    let root = fields(value, &["masks"], "masks")?;
    let masks = root.get("masks").and_then(Value::as_array).ok_or("masks.masks must be an array")?;
    if masks.len() > 16 { return Err("masks takes at most 16 shapes".into()); }
    for (i, mask) in masks.iter().enumerate() {
        let at = format!("masks[{i}]");
        let m = fields(mask, &["shape", "feather", "opacity", "invert", "enabled", "keyframes"], &at)?;
        for key in ["feather", "opacity"] {
            let n = m.get(key).ok_or_else(|| format!("{at}.{key} is required"))?;
            number(n, &format!("{at}.{key}"), 0.0, 1.0)?;
        }
        let shape = m.get("shape").and_then(Value::as_object).filter(|s| s.len() == 1).ok_or_else(|| format!("{at}.shape needs one shape kind"))?;
        let (kind, body) = shape.iter().next().unwrap();
        let keys: &[&str] = match kind.as_str() {
            "Circle" => &["center_x", "center_y", "radius"],
            "Rectangle" => &["center_x", "center_y", "width", "height", "rotation"],
            "LinearGradient" => &["start_x", "start_y", "end_x", "end_y"],
            "Bezier" => &["points"],
            _ => return Err(format!("{at}.shape: unknown kind {kind}")),
        };
        let shape_at = format!("{at}.shape.{kind}");
        let body = fields(body, keys, &shape_at)?;
        if kind == "Bezier" {
            let points = body.get("points").and_then(Value::as_array).ok_or_else(|| format!("{shape_at}.points must be an array"))?;
            if !(3..=256).contains(&points.len()) { return Err(format!("{shape_at}.points needs 3..256 points")); }
            for (p, point) in points.iter().enumerate() {
                let pair = point.as_array().filter(|p| p.len() == 2).ok_or_else(|| format!("{shape_at}.points[{p}] needs [x,y]"))?;
                for (axis, n) in pair.iter().enumerate() { number(n, &format!("{shape_at}.points[{p}][{axis}]"), 0.0, 1.0)?; }
            }
        } else {
            for key in keys {
                let n = body.get(*key).ok_or_else(|| format!("{shape_at}.{key} is required"))?;
                let (min, max) = if *key == "rotation" { (-180.0, 180.0) } else { (0.0, 1.0) };
                number(n, &format!("{shape_at}.{key}"), min, max)?;
            }
        }
        if let Some(k) = m.get("keyframes") {
            let tracks = fields(k, &["center_x", "center_y", "radius", "width", "height", "rotation", "feather", "opacity", "start_x", "start_y", "end_x", "end_y", "track_x", "track_y", "track_scale", "track_rotation", "track_perspective"], &format!("{at}.keyframes"))?;
            for (key, value) in tracks {
                let empty = if key == "track_perspective" { value.is_null() } else { value.as_array().is_some_and(Vec::is_empty) };
                if !empty { return Err(format!("{at}.keyframes.{key}: graph masks currently accept static shapes only")); }
            }
        }
    }
    serde_json::from_value(value.clone()).map_err(|e| format!("masks: {e}"))
}

pub(crate) fn payload(node: &Node, value: &Value) -> Result<Value, String> {
    let masks = validate(value)?;
    let mut payload = node.legacy.as_ref().ok_or("Mask has no saved entry")?.payload.clone();
    let params = payload.get_mut("params").and_then(Value::as_object_mut).ok_or("Mask has no saved parameters")?;
    params.insert("masks".into(), serde_json::to_value(masks).map_err(|e| e.to_string())?);
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn window() -> Value {
        json!({"masks":[{"shape":{"Rectangle":{"center_x":0.5,"center_y":0.5,"width":0.5,"height":0.5,"rotation":0}},"feather":0,"opacity":1,"invert":false,"enabled":true}]})
    }

    #[test]
    fn static_windows_are_strict_bounded_and_readable_as_the_saved_type() {
        let value = window();
        let masks = validate(&value).unwrap();
        let canonical = serde_json::to_value(&masks).unwrap();
        assert_eq!(validate(&canonical).unwrap(), masks, "serialized empty keyframes round-trip");
        let mut node = crate::graph_model::new_node("klipp.grade.mask", uuid::Uuid::new_v4(), Some("Window")).unwrap();
        let payload = payload(&node, &value).unwrap();
        assert_eq!(payload["label"], "Window");
        assert_eq!(payload["params"]["masks"], canonical);
        node.legacy.as_mut().unwrap().payload = payload;
        let params: klipp_nodes::NodeParams = serde_json::from_value(node.legacy.unwrap().payload["params"].clone()).unwrap();
        assert_eq!(params.masks.unwrap(), masks);
    }

    #[test]
    fn invalid_windows_name_the_bad_field() {
        let refuse = |value: Value, name: &str| {
            let error = validate(&value).unwrap_err();
            assert!(error.contains(name), "{error}");
        };
        let mut v = window(); v["masks"] = json!(vec![v["masks"][0].clone();17]); refuse(v,"16");
        let mut v = window(); v["masks"][0]["opacity"] = json!(1e100); refuse(v,"opacity");
        let mut v = window(); v["masks"][0]["feather"] = json!(-0.1); refuse(v,"feather");
        let mut v = window(); v["masks"][0]["shape"]["Rectangle"]["width"] = json!(1.000000001); refuse(v,"width");
        let mut v = window(); v["masks"][0]["shape"]["Rectangle"]["rotaton"] = json!(2); refuse(v,"rotaton");
        let mut v = window(); v["masks"][0]["bogus"] = json!(1); refuse(v,"bogus");
        let mut v = window(); v["bogus"] = json!(1); refuse(v,"bogus");
        let mut v = window(); v["masks"][0].as_object_mut().unwrap().remove("feather"); refuse(v,"feather");
        let mut v = window(); v["masks"][0]["keyframes"] = json!({"opacity":[{"time_secs":0,"value":1}]}); refuse(v,"static");
        let mut v = window(); v["masks"][0]["keyframes"] = json!({"opacty":[]}); refuse(v,"opacty");
        let mut v = window(); v["masks"][0]["shape"] = json!({"Bezier":{"points":vec![[0.5,0.5];257]}}); refuse(v,"256");
    }
}
