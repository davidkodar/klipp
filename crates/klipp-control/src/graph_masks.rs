//! Bounded static windows for the graph wire; the saved MaskStack stays unchanged.

use klipp_graph_format::Node;
use serde_json::Value;

pub(crate) const FORMAT: &str = r#"JSON: {"masks":[{"shape":{"Rectangle":{"center_x":0.5,"center_y":0.5,"width":0.5,"height":0.5,"rotation":0}},"feather":0,"opacity":1,"invert":false,"enabled":true}]}. Other shapes: {"Circle":{"center_x":0.5,"center_y":0.5,"radius":0.2}}, {"LinearGradient":{"start_x":0,"start_y":0,"end_x":1,"end_y":1}}, {"RadialGradient":{"center_x":0.5,"center_y":0.5,"inner":0.1,"outer":0.4}}, {"Bezier":{"points":[[0,0],[1,0],[0.5,1]]}}. A Bezier point is a corner [x,y], or curved {"x":0.5,"y":0.5,"in":[-0.1,0],"out":[0.1,0],"smooth":true} with its handles as offsets from it. Optional per mask: "feather_inner" (0..1, the soft edge inside the line), "combine" ("Add", "Subtract" or "Intersect": how it joins the masks before it), "name" (up to 64 characters), "locked" (true keeps it from viewer edits). A gradient ramps across its feathers: "feather":0.5 with "feather_inner":0.5 fades it over its whole length, start to end or inner ring to outer. A Bezier may be keyed over time: "keyframes":{"path":[{"time_secs":0,"points":[...]},{"time_secs":2,"points":[...],"ease":"Ease"}]}, times rising, every key with as many points as the shape, ease "Linear" (default), "Ease" or "Hold". At most 16 masks, 3..256 Bezier points each, 256 keys. Coordinates, sizes, feather and opacity: 0..1; handle offsets: -1..1; rotation: -180..180. Finite numbers only; unknown fields, and keyframes other than a Bezier's path, refused. {"masks":[]} clears shapes."#;

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
        let m = fields(mask, &["shape", "feather", "opacity", "invert", "enabled", "keyframes", "feather_inner", "combine", "name", "locked"], &at)?;
        for key in ["feather", "opacity"] {
            let n = m.get(key).ok_or_else(|| format!("{at}.{key} is required"))?;
            number(n, &format!("{at}.{key}"), 0.0, 1.0)?;
        }
        if let Some(n) = m.get("feather_inner") {
            number(n, &format!("{at}.feather_inner"), 0.0, 1.0)?;
        }
        if let Some(c) = m.get("combine") {
            if !matches!(c.as_str(), Some("Add" | "Subtract" | "Intersect")) {
                return Err(format!("{at}.combine must be \"Add\", \"Subtract\" or \"Intersect\""));
            }
        }
        if let Some(n) = m.get("name") {
            let name = n.as_str().ok_or_else(|| format!("{at}.name must be text"))?;
            if name.chars().count() > 64 { return Err(format!("{at}.name takes at most 64 characters")); }
        }
        if let Some(l) = m.get("locked") {
            l.as_bool().ok_or_else(|| format!("{at}.locked must be true or false"))?;
        }
        let shape = m.get("shape").and_then(Value::as_object).filter(|s| s.len() == 1).ok_or_else(|| format!("{at}.shape needs one shape kind"))?;
        let (kind, body) = shape.iter().next().unwrap();
        let keys: &[&str] = match kind.as_str() {
            "Circle" => &["center_x", "center_y", "radius"],
            "Rectangle" => &["center_x", "center_y", "width", "height", "rotation"],
            "LinearGradient" => &["start_x", "start_y", "end_x", "end_y"],
            "RadialGradient" => &["center_x", "center_y", "inner", "outer"],
            "Bezier" => &["points"],
            _ => return Err(format!("{at}.shape: unknown kind {kind}")),
        };
        let shape_at = format!("{at}.shape.{kind}");
        let body = fields(body, keys, &shape_at)?;
        let mut point_count = None;
        if kind == "Bezier" {
            point_count = Some(points(body.get("points"), &format!("{shape_at}.points"))?);
        } else {
            for key in keys {
                let n = body.get(*key).ok_or_else(|| format!("{shape_at}.{key} is required"))?;
                let (min, max) = if *key == "rotation" { (-180.0, 180.0) } else { (0.0, 1.0) };
                number(n, &format!("{shape_at}.{key}"), min, max)?;
            }
        }
        if let Some(k) = m.get("keyframes") {
            let tracks = fields(k, &["center_x", "center_y", "radius", "width", "height", "rotation", "feather", "opacity", "start_x", "start_y", "end_x", "end_y", "inner", "outer", "track_x", "track_y", "track_scale", "track_rotation", "track_perspective", "path"], &format!("{at}.keyframes"))?;
            for (key, value) in tracks {
                if key == "path" {
                    path_keys(value, point_count, &format!("{at}.keyframes.path"))?;
                    continue;
                }
                let empty = if key == "track_perspective" { value.is_null() } else { value.as_array().is_some_and(Vec::is_empty) };
                if !empty { return Err(format!("{at}.keyframes.{key}: graph masks accept a Bezier's path keys, and no other keyframes")); }
            }
        }
    }
    serde_json::from_value(value.clone()).map_err(|e| format!("masks: {e}"))
}

/// A freeform shape's points at `at`: corners `[x,y]` or curved
/// `{x, y, in, out, smooth}`, 3..256 of them. How many.
fn points(value: Option<&Value>, at: &str) -> Result<usize, String> {
    let points = value.and_then(Value::as_array).ok_or_else(|| format!("{at} must be an array"))?;
    if !(3..=256).contains(&points.len()) { return Err(format!("{at} needs 3..256 points")); }
    for (p, point) in points.iter().enumerate() {
        let here = format!("{at}[{p}]");
        if let Some(pair) = point.as_array() {
            if pair.len() != 2 { return Err(format!("{here} needs [x,y]")); }
            for (axis, n) in pair.iter().enumerate() { number(n, &format!("{here}[{axis}]"), 0.0, 1.0)?; }
            continue;
        }
        let o = fields(point, &["x", "y", "in", "out", "smooth"], &here).map_err(|_| format!("{here} needs [x,y] or {{\"x\",\"y\",\"in\",\"out\",\"smooth\"}}"))?;
        for key in ["x", "y"] {
            number(o.get(key).ok_or_else(|| format!("{here}.{key} is required"))?, &format!("{here}.{key}"), 0.0, 1.0)?;
        }
        for key in ["in", "out"] {
            if let Some(h) = o.get(key) {
                let pair = h.as_array().filter(|p| p.len() == 2).ok_or_else(|| format!("{here}.{key} needs [dx,dy]"))?;
                for (axis, n) in pair.iter().enumerate() { number(n, &format!("{here}.{key}[{axis}]"), -1.0, 1.0)?; }
            }
        }
        if let Some(sm) = o.get("smooth") {
            sm.as_bool().ok_or_else(|| format!("{here}.smooth must be true or false"))?;
        }
    }
    Ok(points.len())
}

/// A Bezier's shape keys at `at` (#74): times rising, each key as many
/// points as the shape (`count`), its ease one of three.
fn path_keys(value: &Value, count: Option<usize>, at: &str) -> Result<(), String> {
    let keys = value.as_array().ok_or_else(|| format!("{at} must be an array"))?;
    if keys.is_empty() { return Ok(()); }
    let count = count.ok_or_else(|| format!("{at}: only a Bezier's shape is keyed"))?;
    if keys.len() > 256 { return Err(format!("{at} takes at most 256 keys")); }
    let mut last = f64::NEG_INFINITY;
    for (k, key) in keys.iter().enumerate() {
        let here = format!("{at}[{k}]");
        let o = fields(key, &["time_secs", "points", "ease"], &here)?;
        let t = o.get("time_secs").and_then(Value::as_f64).filter(|t| t.is_finite() && *t >= 0.0).ok_or_else(|| format!("{here}.time_secs must be a time of 0 or more"))?;
        if t <= last { return Err(format!("{here}.time_secs must come after the key before")); }
        last = t;
        let n = points(o.get("points"), &format!("{here}.points"))?;
        if n != count { return Err(format!("{here}.points has {n} points; every key holds as many as the shape, {count}")); }
        if let Some(e) = o.get("ease") {
            if !matches!(e.as_str(), Some("Linear" | "Ease" | "Hold")) {
                return Err(format!("{here}.ease must be \"Linear\", \"Ease\" or \"Hold\""));
            }
        }
    }
    Ok(())
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
        let mut v = window(); v["masks"][0]["keyframes"] = json!({"opacity":[{"time_secs":0,"value":1}]}); refuse(v,"no other keyframes");
        let mut v = window(); v["masks"][0]["keyframes"] = json!({"opacty":[]}); refuse(v,"opacty");
        let mut v = window(); v["masks"][0]["shape"] = json!({"Bezier":{"points":vec![[0.5,0.5];257]}}); refuse(v,"256");
    }

    fn freeform() -> Value {
        json!({"masks":[{
            "shape":{"Bezier":{"points":[[0.2,0.2],{"x":0.8,"y":0.2,"in":[-0.1,0],"out":[0.1,0.05],"smooth":true},[0.5,0.8]]}},
            "feather":0.02,"opacity":1,"invert":false,"enabled":true,
            "feather_inner":0.05,"combine":"Subtract","name":"Face","locked":true,
            "keyframes":{"path":[
                {"time_secs":0,"points":[[0.2,0.2],[0.8,0.2],[0.5,0.8]]},
                {"time_secs":2,"points":[[0.3,0.2],[0.9,0.2],[0.6,0.8]],"ease":"Ease"}
            ]}
        }]})
    }

    /// #74: a curved, keyed, subtracted, named and locked freeform mask
    /// reads as the saved type, its keys moving the shape.
    #[test]
    fn a_keyed_curved_mask_reads_as_the_saved_type() {
        let masks = validate(&freeform()).unwrap();
        let m = &masks.masks[0];
        assert_eq!((m.combine, m.feather_inner, m.name.as_str(), m.locked), (klipp_core::MaskCombine::Subtract, 0.05, "Face", true));
        let klipp_core::MaskShape::Bezier { points } = &m.shape else { panic!() };
        assert!(points[1].smooth && points[1].out_dy == 0.05);
        assert_eq!(m.keyframes.path.len(), 2);
        assert_eq!(m.keyframes.path[1].ease, klipp_core::mask_path::PathEase::Ease);
        assert!((m.path_at(1.0).unwrap()[0].x - 0.25).abs() < 1e-6, "eased halfway");
        let canonical = serde_json::to_value(&masks).unwrap();
        assert_eq!(validate(&canonical).unwrap(), masks, "the saved form validates as itself");
    }

    /// Each new field is held to its bounds and named when it isn't.
    #[test]
    fn a_bad_curve_or_key_names_its_field() {
        let refuse = |value: Value, name: &str| {
            let error = validate(&value).unwrap_err();
            assert!(error.contains(name), "{name}: {error}");
        };
        let mut v = freeform(); v["masks"][0]["combine"] = json!("Multiply"); refuse(v, "combine");
        let mut v = freeform(); v["masks"][0]["feather_inner"] = json!(1.5); refuse(v, "feather_inner");
        let mut v = freeform(); v["masks"][0]["name"] = json!("x".repeat(65)); refuse(v, "name");
        let mut v = freeform(); v["masks"][0]["locked"] = json!("yes"); refuse(v, "locked");
        let mut v = freeform(); v["masks"][0]["shape"]["Bezier"]["points"][1]["out"] = json!([2, 0]); refuse(v, "out[0]");
        let mut v = freeform(); v["masks"][0]["shape"]["Bezier"]["points"][1]["wobble"] = json!(1); refuse(v, "points[1]");
        let mut v = freeform(); v["masks"][0]["keyframes"]["path"][1]["points"] = json!([[0.1,0.1],[0.2,0.2],[0.3,0.3],[0.4,0.4]]); refuse(v, "as many as the shape");
        let mut v = freeform(); v["masks"][0]["keyframes"]["path"][1]["time_secs"] = json!(0); refuse(v, "after the key before");
        let mut v = freeform(); v["masks"][0]["keyframes"]["path"][1]["ease"] = json!("Bounce"); refuse(v, "ease");
        let mut v = window(); v["masks"][0]["keyframes"] = json!({"path":[{"time_secs":0,"points":[[0,0],[1,0],[0,1]]}]}); refuse(v, "only a Bezier");
    }
}
