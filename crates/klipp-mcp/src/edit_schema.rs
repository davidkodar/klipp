//! Strict checks of the edits agents use most often, made before any
//! call; their forms, and every other op's, are in the guide's ops topic,
//! and the rest are checked by the resolver.

use serde_json::Value;

const COMMON: &[&str] = &[
    "cut",
    "split",
    "trim",
    "place",
    "move",
    "remove",
    "add_track",
];

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
            "cut" => (&["from", "to"], &["op", "from", "to", "tracks_of"]),
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
                    "sync_source_secs",
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
            if !valid_track_kind(value) {
                return Err(format!("ops[{index}].kind must name a track kind"));
            }
        }
        for key in ["ripple", "linked", "insert", "with_audio"] {
            if op.get(key).is_some_and(|v| !v.is_boolean()) {
                return Err(format!("ops[{index}].{key} must be boolean"));
            }
        }
        for key in ["source_in", "source_out", "sync_source_secs"] {
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

fn valid_track_kind(value: &Value) -> bool {
    value.as_str().is_some_and(|kind| matches!(kind.to_lowercase().as_str(),
        "video" | "audio" | "text" | "title" | "subtitle" | "subtitles" | "captions"))
}

fn validate_track_ref(value: &Value, path: &str) -> Result<(), String> {
    if nonnegative_integer(value) || valid_track_kind(value) {
        Ok(())
    } else {
        Err(format!(
            "{path} must be a track index or video/audio/text/title/subtitle/subtitles/captions"
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
    use serde_json::json;

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
}
