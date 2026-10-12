//! The old fields a converted project writes, so that a build from before
//! the conversion refuses the file instead of opening and re-saving it.
//!
//! Builds since 6fd04616 refuse a newer `format_revision`. Older ones
//! don't read it, but every build since 3ebe6af8 (#223) refuses an effect
//! type it doesn't know, naming the clip, before it reads any sequence.
//! So every clip of a converted file carries one such effect, the
//! sentinel, in its old effect list, and the picture fields old builds
//! would read are left out. The converter's own reader checks and strips
//! the sentinel again ([`strip_sentinels`]).
//!
//! A build from before track effects (#846) ignores a track's effects in
//! every file, so the track sentinel guards only builds from the day
//! between #846 and the revision check; the dry run reports projects
//! whose only picture data is on tracks.

use serde_json::{json, Map, Value};

/// The effect type no build before the conversion knows.
pub const SENTINEL_TYPE: &str = "graph_v2";

/// Where a converted clip or track keeps its picture.
pub const V2_FIELD: &str = "graph_v2";

/// The clip fields that hold picture data in today's model, left out of a
/// converted file because old builds would read them.
const CLIP_PICTURE_FIELDS: [&str; 11] = [
    "builtin_effects_enabled",
    "effect_keyframes",
    "node_graph",
    "opacity_keyframes",
    "transform_pos_x_keyframes",
    "transform_pos_y_keyframes",
    "transform_scale_keyframes",
    "transform_scale_y_keyframes",
    "transform_rotation_keyframes",
    "transform_tilt_x_keyframes",
    "transform_tilt_y_keyframes",
];

/// The `param_modifiers` keys that stay clip fields: sound, not picture.
const AUDIO_MODIFIERS: [&str; 3] = ["volume", "pan", "gain"];

fn sentinel() -> Value {
    json!([{ "type": SENTINEL_TYPE }])
}

fn is_sentinel(v: &Value) -> bool {
    v.get("type").and_then(Value::as_str) == Some(SENTINEL_TYPE)
}

/// What a converted project can't be written or read as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OldFieldsError {
    /// A clip or track to be written has no converted picture.
    NoPicture { what: &'static str, id: String },
    /// A clip or track carries the sentinel but no converted picture: a
    /// damaged or hand-edited file.
    SentinelWithoutPicture { what: &'static str, id: String },
    /// The sentinel sits among other effects, which no converter writes.
    SentinelMixed { what: &'static str, id: String },
    /// The project isn't shaped like a project.
    NotAProject(&'static str),
}

impl std::fmt::Display for OldFieldsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoPicture { what, id } => write!(f, "{what} {id} has no converted picture to write"),
            Self::SentinelWithoutPicture { what, id } => write!(
                f,
                "{what} {id}: marked as converted but its converted picture is missing; the file is damaged"
            ),
            Self::SentinelMixed { what, id } => {
                write!(f, "{what} {id}: the conversion marker sits among other effects; the file is damaged")
            }
            Self::NotAProject(why) => write!(f, "not a project: {why}"),
        }
    }
}

impl std::error::Error for OldFieldsError {}

fn id_of(o: &Map<String, Value>) -> String {
    o.get("id").and_then(Value::as_str).unwrap_or("?").to_owned()
}

fn objects(v: Option<&mut Value>) -> impl Iterator<Item = &mut Map<String, Value>> {
    v.and_then(Value::as_array_mut).into_iter().flatten().filter_map(Value::as_object_mut)
}

/// The minimal grade graph, Source then Output with nothing between: what
/// a grade version's `node_graph` holds in a converted file, since old
/// readers require the field. Not the app's default graph, which carries a
/// correction an old build would run.
pub fn placeholder_graph() -> Value {
    let source = "00000000-0000-0000-0000-00000000c0a1";
    let output = "00000000-0000-0000-0000-00000000c0a2";
    json!({
        "nodes": [
            { "id": source, "kind": "Source", "label": "Source", "enabled": true },
            { "id": output, "kind": "Output", "label": "Output", "enabled": true }
        ],
        "edges": [{ "from_node": source, "from_port": 0, "to_node": output, "to_port": 0 }],
        "output_node_id": output
    })
}

/// A project's sequences, mutable: the `sequences` list, or the single
/// `sequence` of the oldest files, which today's reader still opens.
pub fn sequences_mut(project: &mut Value) -> Result<Vec<&mut Map<String, Value>>, OldFieldsError> {
    let o = project.as_object_mut().ok_or(OldFieldsError::NotAProject("not an object"))?;
    if o.contains_key("sequences") {
        let list = o.get_mut("sequences").and_then(Value::as_array_mut).ok_or(OldFieldsError::NotAProject("sequences isn't a list"))?;
        return Ok(list.iter_mut().filter_map(Value::as_object_mut).collect());
    }
    match o.get_mut("sequence").and_then(Value::as_object_mut) {
        Some(seq) => Ok(vec![seq]),
        None => Err(OldFieldsError::NotAProject("no sequences")),
    }
}

/// Whether a track's effects carry a converted picture.
fn track_converted(track: &Map<String, Value>) -> bool {
    track.get("effects").and_then(|e| e.get(V2_FIELD)).is_some()
}

/// Write the old fields of a project whose picture data now lives in each
/// store's `graph_v2`: the sentinel in every clip's effect list, the old
/// picture fields left out, audio modifiers kept; the sentinel in every
/// track whose effects carry a converted picture; no timeline graph; grade
/// groups without stages; grade versions holding [`placeholder_graph`].
pub fn write_old_fields(project: &mut Value) -> Result<(), OldFieldsError> {
    for seq in sequences_mut(project)? {
        seq.remove("timeline_graph");
        for group in objects(seq.get_mut("grade_groups")) {
            group.remove("pre");
            group.remove("post");
        }
        for track in objects(seq.get_mut("tracks")) {
            if track_converted(track) {
                if let Some(effects) = track.get_mut("effects").and_then(Value::as_object_mut) {
                    effects.insert("video".into(), sentinel());
                    effects.remove("video_enabled");
                    effects.remove("grade");
                }
            }
            for clip in objects(track.get_mut("clips")) {
                write_clip(clip)?;
            }
        }
    }
    Ok(())
}

fn write_clip(clip: &mut Map<String, Value>) -> Result<(), OldFieldsError> {
    if !clip.contains_key(V2_FIELD) {
        return Err(OldFieldsError::NoPicture { what: "clip", id: id_of(clip) });
    }
    clip.insert("builtin_effects".into(), sentinel());
    clip.insert("effects".into(), json!([]));
    for field in CLIP_PICTURE_FIELDS {
        clip.remove(field);
    }
    if let Some(mods) = clip.get_mut("param_modifiers").and_then(Value::as_object_mut) {
        mods.retain(|k, _| AUDIO_MODIFIERS.contains(&k.as_str()));
    }
    for version in objects(clip.get_mut("grade_versions")) {
        version.insert("node_graph".into(), placeholder_graph());
    }
    Ok(())
}

/// Check and remove the sentinels a converted project carries, before its
/// picture is read from `graph_v2`. A sentinel without a converted
/// picture, or among other effects, refuses the file, naming the clip or
/// track.
pub fn strip_sentinels(project: &mut Value) -> Result<(), OldFieldsError> {
    for seq in sequences_mut(project)? {
        for track in objects(seq.get_mut("tracks")) {
            let has_picture = track_converted(track);
            let id = id_of(track);
            if let Some(effects) = track.get_mut("effects").and_then(Value::as_object_mut) {
                if strip_list(effects, "video", "track", &id, has_picture)? {
                    effects.remove("video");
                }
            }
            for clip in objects(track.get_mut("clips")) {
                let has_picture = clip.contains_key(V2_FIELD);
                let id = id_of(clip);
                if strip_list(clip, "builtin_effects", "clip", &id, has_picture)? {
                    clip.remove("builtin_effects");
                }
            }
        }
    }
    Ok(())
}

/// Whether `o[field]` is exactly the sentinel, refusing it when it's
/// mixed with other effects or stands without a converted picture.
fn strip_list(
    o: &Map<String, Value>,
    field: &str,
    what: &'static str,
    id: &str,
    has_picture: bool,
) -> Result<bool, OldFieldsError> {
    let Some(list) = o.get(field).and_then(Value::as_array) else {
        return Ok(false);
    };
    let marked = list.iter().filter(|v| is_sentinel(v)).count();
    match (marked, list.len()) {
        (0, _) => Ok(false),
        (1, 1) if has_picture => Ok(true),
        (1, 1) => Err(OldFieldsError::SentinelWithoutPicture { what, id: id.to_owned() }),
        _ => Err(OldFieldsError::SentinelMixed { what, id: id.to_owned() }),
    }
}
