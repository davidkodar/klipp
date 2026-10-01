//! Small, strict schemas for the edits agents use most often. Other ops
//! remain discoverable through the guide and are checked by the resolver.

use serde_json::{json, Value};

const COMMON: &[&str] = &[
    "cut",
    "split",
    "trim",
    "place",
    "move",
    "remove",
    "add_track",
];

pub(super) fn ops_schema() -> Value {
    let anchor = json!({"oneOf": [
        {"type":"object","properties":{"secs":{"type":"number","minimum":0}},"required":["secs"],"additionalProperties":false},
        {"type":"object","properties":{"frame":{"type":"integer","minimum":0}},"required":["frame"],"additionalProperties":false},
        {"type":"object","properties":{"clip_start":{"$ref":"#/properties/ops/$defs/clip_ref"}},"required":["clip_start"],"additionalProperties":false},
        {"type":"object","properties":{"clip_end":{"$ref":"#/properties/ops/$defs/clip_ref"}},"required":["clip_end"],"additionalProperties":false},
        {"type":"object","properties":{"track_end":{"$ref":"#/properties/ops/$defs/track_ref"}},"required":["track_end"],"additionalProperties":false},
        {"type":"object","properties":{"marker":{"type":"string","minLength":1}},"required":["marker"],"additionalProperties":false},
        {"type":"object","properties":{"offset":{"type":"object","properties":{"from":{"$ref":"#/properties/ops/$defs/anchor"},"secs":{"type":"number"}},"required":["from","secs"],"additionalProperties":false}},"required":["offset"],"additionalProperties":false}
    ]});
    let clip_ref = json!({"oneOf":[
        {"type":"string","minLength":1},
        {"type":"object","properties":{"track":{"type":"integer","minimum":0},"secs":{"type":"number","minimum":0}},"required":["track","secs"],"additionalProperties":false}
    ]});
    let track_ref = json!({"oneOf":[{"type":"integer","minimum":0},{"type":"string","enum":["video","audio","text","subtitle"]}]});
    let common = [
        variant(
            "cut",
            &["from", "to"],
            json!({"from":{"$ref":"#/properties/ops/$defs/anchor"},"to":{"$ref":"#/properties/ops/$defs/anchor"}}),
        ),
        variant(
            "split",
            &["clip_id", "at"],
            json!({"clip_id":{"$ref":"#/properties/ops/$defs/clip_ref"},"at":{"$ref":"#/properties/ops/$defs/anchor"}}),
        ),
        variant(
            "trim",
            &["clip_id", "edge", "to"],
            json!({"clip_id":{"$ref":"#/properties/ops/$defs/clip_ref"},"edge":{"type":"string","enum":["in","out"]},"to":{"$ref":"#/properties/ops/$defs/anchor"},"ripple":{"type":"boolean"},"linked":{"type":"boolean"}}),
        ),
        variant(
            "place",
            &["media_id", "track", "at"],
            json!({"media_id":{"type":"string","minLength":1},"track":{"$ref":"#/properties/ops/$defs/track_ref"},"at":{"$ref":"#/properties/ops/$defs/anchor"},"source_in":{"type":"number","minimum":0},"source_out":{"type":"number","minimum":0},"insert":{"type":"boolean"},"with_audio":{"type":"boolean"}}),
        ),
        variant(
            "move",
            &["clip_id", "to"],
            json!({"clip_id":{"$ref":"#/properties/ops/$defs/clip_ref"},"to":{"$ref":"#/properties/ops/$defs/anchor"}}),
        ),
        variant(
            "remove",
            &["clip_id"],
            json!({"clip_id":{"$ref":"#/properties/ops/$defs/clip_ref"},"ripple":{"type":"boolean"},"linked":{"type":"boolean"}}),
        ),
        variant(
            "add_track",
            &["kind"],
            json!({"kind":{"type":"string","enum":["video","audio","text","subtitle"]},"name":{"type":"string"}}),
        ),
    ];
    let mut other_ops = vec![
        "vector_path",
        "j_cut",
        "l_cut",
        "captions",
        "audio",
        "volume",
        "duck",
        "lower_third",
        "set_text",
        "grade",
        "effect",
        "redact",
        "slip",
        "role",
        "matte",
        "format",
        "stabilize",
        "reframe",
        "zoom",
        "pan",
        "follow",
        "highlight",
        "ramp",
        "grade_group",
        "remove_grade_group",
        "look",
        "qualify",
        "retime",
        "clean_audio",
        "transition",
        "style",
        "title",
        "compose",
        "camera",
        "crop",
        "generator",
        "depth",
        "motion_blur",
    ];
    other_ops.retain(|op| !COMMON.contains(op));
    let mut alternatives = common.to_vec();
    alternatives.push(json!({"type":"object","properties":{"op":{"type":"string","enum":other_ops}},"required":["op"],"description":"Less common operation; read klipp_guide before using it."}));
    json!({"type":"array","minItems":1,"items":{"oneOf":alternatives},"$defs":{"anchor":anchor,"clip_ref":clip_ref,"track_ref":track_ref}})
}

fn variant(name: &str, required: &[&str], fields: Value) -> Value {
    let mut properties = fields.as_object().cloned().unwrap_or_default();
    properties.insert("op".into(), json!({"const":name}));
    let mut required = required.to_vec();
    required.push("op");
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}

pub(super) fn validate_ops(args: &Value) -> Result<(), String> {
    let ops = args
        .get("ops")
        .and_then(Value::as_array)
        .ok_or("ops must be an array")?;
    if ops.is_empty() {
        return Err("ops must contain at least one operation".into());
    }
    for (index, op) in ops.iter().enumerate() {
        let kind = op
            .get("op")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("ops[{index}].op must be a string"))?;
        if !COMMON.contains(&kind) {
            continue;
        }
        let (required, allowed): (&[&str], &[&str]) = match kind {
            "cut" => (&["from", "to"], &["op", "from", "to"]),
            "split" => (&["clip_id", "at"], &["op", "clip_id", "at"]),
            "trim" => (
                &["clip_id", "edge", "to"],
                &["op", "clip_id", "edge", "to", "ripple", "linked"],
            ),
            "place" => (
                &["media_id", "track", "at"],
                &[
                    "op",
                    "media_id",
                    "track",
                    "at",
                    "source_in",
                    "source_out",
                    "insert",
                    "with_audio",
                ],
            ),
            "move" => (&["clip_id", "to"], &["op", "clip_id", "to"]),
            "remove" => (&["clip_id"], &["op", "clip_id", "ripple", "linked"]),
            "add_track" => (&["kind"], &["op", "kind", "name"]),
            _ => unreachable!(),
        };
        let object = op
            .as_object()
            .ok_or_else(|| format!("ops[{index}] must be an object"))?;
        for key in object.keys() {
            if !allowed.contains(&key.as_str()) {
                return Err(format!("ops[{index}] ({kind}): unknown field {key:?}"));
            }
        }
        for key in required {
            if !object.contains_key(*key) {
                return Err(format!("ops[{index}] ({kind}): missing {key}"));
            }
        }
        for key in ["from", "to", "at"] {
            if let Some(value) = op.get(key) {
                validate_anchor(value, &format!("ops[{index}].{key}"), 0)?;
            }
        }
        if let Some(value) = op.get("clip_id") {
            validate_clip_ref(value, &format!("ops[{index}].clip_id"))?;
        }
        if let Some(value) = op.get("track") {
            validate_track_ref(value, &format!("ops[{index}].track"))?;
        }
        if kind == "trim" && !matches!(op["edge"].as_str(), Some("in" | "out")) {
            return Err(format!("ops[{index}].edge must be in or out"));
        }
        if let Some(value) = op.get("kind") {
            if !matches!(
                value.as_str(),
                Some("video" | "audio" | "text" | "subtitle")
            ) {
                return Err(format!("ops[{index}].kind must name a track kind"));
            }
        }
        for key in ["ripple", "linked", "insert", "with_audio"] {
            if op.get(key).is_some_and(|v| !v.is_boolean()) {
                return Err(format!("ops[{index}].{key} must be boolean"));
            }
        }
        for key in ["source_in", "source_out"] {
            if op.get(key).is_some_and(|v| !nonnegative_number(v)) {
                return Err(format!("ops[{index}].{key} must be nonnegative seconds"));
            }
        }
        for key in ["media_id", "name"] {
            if op
                .get(key)
                .is_some_and(|v| v.as_str().is_none_or(str::is_empty))
            {
                return Err(format!("ops[{index}].{key} must be a nonempty string"));
            }
        }
    }
    Ok(())
}

fn nonnegative_number(value: &Value) -> bool {
    value.as_f64().is_some_and(|n| n.is_finite() && n >= 0.0)
}
fn nonnegative_integer(value: &Value) -> bool {
    value.as_u64().is_some()
}

fn validate_track_ref(value: &Value, path: &str) -> Result<(), String> {
    if nonnegative_integer(value)
        || matches!(
            value.as_str(),
            Some("video" | "audio" | "text" | "subtitle")
        )
    {
        Ok(())
    } else {
        Err(format!(
            "{path} must be a track index or video/audio/text/subtitle"
        ))
    }
}

fn validate_clip_ref(value: &Value, path: &str) -> Result<(), String> {
    if value.as_str().is_some_and(|s| !s.is_empty()) {
        return Ok(());
    }
    let object = value
        .as_object()
        .ok_or_else(|| format!("{path} must be a clip ID or {{track, secs}}"))?;
    if object.len() != 2
        || !object.contains_key("track")
        || !object.contains_key("secs")
        || !nonnegative_integer(&object["track"])
        || !nonnegative_number(&object["secs"])
    {
        return Err(format!(
            "{path} must be a clip ID or {{track: index, secs: timeline seconds}}"
        ));
    }
    Ok(())
}

fn validate_anchor(value: &Value, path: &str, depth: usize) -> Result<(), String> {
    if depth > 8 {
        return Err(format!("{path} has too many nested offsets"));
    }
    let object = value
        .as_object()
        .ok_or_else(|| format!("{path} must be an anchor object"))?;
    if object.len() != 1 {
        return Err(format!("{path} must have exactly one anchor form"));
    }
    let (form, value) = object.iter().next().unwrap();
    match form.as_str() {
        "secs" if nonnegative_number(value) => Ok(()),
        "frame" if nonnegative_integer(value) => Ok(()),
        "clip_start" | "clip_end" => validate_clip_ref(value, path),
        "track_end" => validate_track_ref(value, path),
        "marker" if value.as_str().is_some_and(|s| !s.is_empty()) => Ok(()),
        "offset" => {
            let offset = value
                .as_object()
                .ok_or_else(|| format!("{path}.offset must have from and secs"))?;
            if offset.len() != 2
                || !offset.contains_key("from")
                || !offset
                    .get("secs")
                    .and_then(Value::as_f64)
                    .is_some_and(f64::is_finite)
            {
                return Err(format!("{path}.offset must have from and numeric secs"));
            }
            validate_anchor(&offset["from"], &format!("{path}.offset.from"), depth + 1)
        }
        _ => Err(format!("{path}: unknown or invalid anchor form {form:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_common_edits() {
        assert!(validate_ops(&json!({"ops":[{"op":"cut","from":{"frame":3},"to":{"offset":{"from":{"frame":10},"secs":-0.2}}}]})).is_ok());
        assert!(validate_ops(&json!({"ops":[{"op":"trim","clip_id":"abc","edge":"out","to":{"secs":2},"riple":true}]})).unwrap_err().contains("riple"));
        assert!(
            validate_ops(&json!({"ops":[{"op":"split","clip_id":"abc","at":{"frames":4}}]}))
                .unwrap_err()
                .contains("anchor form")
        );
        assert!(validate_ops(
            &json!({"ops":[{"op":"move","clip_id":{"track":0,"seconds":1},"to":{"secs":2}}]})
        )
        .unwrap_err()
        .contains("clip ID"));
    }

    #[test]
    fn schema_lists_precise_common_variants() {
        let schema = ops_schema();
        assert_eq!(schema["items"]["oneOf"][0]["additionalProperties"], false);
        assert_eq!(
            schema["items"]["oneOf"][0]["properties"]["op"]["const"],
            "cut"
        );
        assert!(schema["$defs"]["anchor"]["oneOf"].as_array().unwrap().len() >= 7);
    }
}
