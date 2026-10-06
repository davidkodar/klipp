//! A raw document and its interpreted picture fields. The raw value is the
//! authority for output; typed views must never replace unconverted JSON.
use crate::v1::*;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// The on-disk store being inspected. Explicit selection avoids interpreting
/// unrelated JSON objects as graphs or effects merely because names match.
#[derive(Debug, Clone, Copy)]
pub enum StoreKind {
    Project,
    Look,
    Stills,
    GradePreset,
    ToolPreset,
    EffectPreset,
}

#[derive(Debug)]
pub enum Picture {
    Clip(Box<ClipPicture>),
    Track(TrackPicture),
    Graph(NodeGraph),
    Group(GroupPicture),
    Look(Look),
    Still(StillPicture),
    Tool(Box<ToolPreset>),
    Effect(EffectPreset),
}

/// JSON Pointer location and the frozen interpretation at that location.
#[derive(Debug)]
pub struct PictureField {
    pub pointer: String,
    pub picture: Picture,
}

#[derive(Debug)]
pub struct Document {
    raw: Value,
    pub pictures: Vec<PictureField>,
}

fn read<T: DeserializeOwned>(v: &Value, at: &str) -> Result<T, String> {
    serde_json::from_value(v.clone()).map_err(|e| format!("{at}: {e}"))
}

impl Document {
    pub fn parse(text: &str, kind: StoreKind) -> Result<Self, String> {
        let raw: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        Self::from_value(raw, kind)
    }

    pub fn from_value(raw: Value, kind: StoreKind) -> Result<Self, String> {
        let mut losses = crate::v1::json_check::losses(&raw);
        if matches!(kind, StoreKind::ToolPreset) {
            losses.extend(crate::v1::json_check::params_losses(&raw["params"]));
        }
        if !losses.is_empty() {
            return Err(losses.join("; "));
        }
        let mut pictures = Vec::new();
        match kind {
            StoreKind::Project => {
                let revision = raw
                    .get("format_revision")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                if revision > 0 {
                    return Err(format!(
                        "project format {revision} is newer than the frozen v1 reader"
                    ));
                }
                if let Some(sequences) = raw.get("sequences") {
                    for (i, seq) in array(sequences, "/sequences")?.iter().enumerate() {
                        sequence(seq, &format!("/sequences/{i}"), &mut pictures)?;
                    }
                } else if let Some(seq) = raw.get("sequence") {
                    // The old singular spelling is interpreted, not rewritten.
                    sequence(seq, "/sequence", &mut pictures)?;
                } else {
                    return Err("project has no sequences".into());
                }
            }
            StoreKind::Look => pictures.push(PictureField {
                pointer: String::new(),
                picture: Picture::Look(read(&raw, "")?),
            }),
            StoreKind::GradePreset => pictures.push(PictureField {
                pointer: String::new(),
                picture: Picture::Graph(read(&raw, "")?),
            }),
            StoreKind::ToolPreset => pictures.push(PictureField {
                pointer: String::new(),
                picture: Picture::Tool(Box::new(read(&raw, "")?)),
            }),
            StoreKind::EffectPreset => pictures.push(PictureField {
                pointer: String::new(),
                picture: Picture::Effect(read(&raw, "")?),
            }),
            StoreKind::Stills => {
                for (i, still) in array(&raw, "")?.iter().enumerate() {
                    pictures.push(PictureField {
                        pointer: format!("/{i}"),
                        picture: Picture::Still(read(still, &format!("/{i}"))?),
                    });
                }
            }
        }
        Ok(Self { raw, pictures })
    }

    /// Unconverted output: preserves every JSON key, including unknown ones,
    /// original enum spellings, legacy OFX forms, and omitted defaults.
    pub fn raw(&self) -> &Value {
        &self.raw
    }
    pub fn into_raw(self) -> Value {
        self.raw
    }
}

fn array<'a>(v: &'a Value, at: &str) -> Result<&'a Vec<Value>, String> {
    v.as_array().ok_or_else(|| format!("{at}: expected a list"))
}

fn sequence(seq: &Value, at: &str, out: &mut Vec<PictureField>) -> Result<(), String> {
    if let Some(graph) = seq.get("timeline_graph").filter(|v| !v.is_null()) {
        let pointer = format!("{at}/timeline_graph");
        out.push(PictureField {
            picture: Picture::Graph(read(graph, &pointer)?),
            pointer,
        });
    }
    if let Some(groups) = seq.get("grade_groups") {
        for (i, group) in array(groups, at)?.iter().enumerate() {
            let pointer = format!("{at}/grade_groups/{i}");
            out.push(PictureField {
                picture: Picture::Group(read(group, &pointer)?),
                pointer,
            });
        }
    }
    let tracks = seq
        .get("tracks")
        .ok_or_else(|| format!("{at}: missing tracks"))?;
    for (ti, track) in array(tracks, at)?.iter().enumerate() {
        let track_at = format!("{at}/tracks/{ti}");
        if let Some(effects) = track.get("effects").filter(|v| !v.is_null()) {
            let pointer = format!("{track_at}/effects");
            out.push(PictureField {
                picture: Picture::Track(read(effects, &pointer)?),
                pointer,
            });
        }
        let clips = track
            .get("clips")
            .ok_or_else(|| format!("{track_at}: missing clips"))?;
        for (ci, clip) in array(clips, &track_at)?.iter().enumerate() {
            let pointer = format!("{track_at}/clips/{ci}");
            out.push(PictureField {
                picture: Picture::Clip(Box::new(read(clip, &pointer)?)),
                pointer,
            });
        }
    }
    Ok(())
}

/// Frozen effect-slot key interpretation. Invalid names are retained in raw
/// JSON, as the current reader does; callers must not silently discard them.
pub fn effect_key(key: &str) -> Option<(usize, &str)> {
    let (index, name) = key.split_once(':')?;
    Some((index.parse().ok()?, name))
}
