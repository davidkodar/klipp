//! The project's `format_revision`, as the converter reads and writes it.
//!
//! The app reads an absent revision as 0. The converter is stricter: a
//! value that isn't a non-negative whole number is a damaged or foreign
//! file, refused rather than read as 0, and a revision newer than the
//! converter knows is refused too. A converted file is written with the
//! lowest revision its content needs, worked out from the content, so a
//! file never claims more than it holds.

use serde_json::Value;

/// The newest revision this converter reads and writes: 1, the structural
/// form (graphs holding only `klipp.legacy.*` nodes).
pub const KNOWN_REVISION: u64 = 1;

#[derive(Debug, Clone, PartialEq)]
pub enum RevisionError {
    /// Not a whole number of zero or more: a string, a negative, a
    /// fraction, a bool.
    NotARevision(Value),
    /// Newer than this converter.
    Newer(u64),
}

impl std::fmt::Display for RevisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotARevision(v) => write!(f, "format_revision {v} isn't a project format revision"),
            Self::Newer(r) => write!(
                f,
                "this project was saved with format revision {r}; this converter reads up to {KNOWN_REVISION}"
            ),
        }
    }
}

impl std::error::Error for RevisionError {}

/// The project's revision: absent is 0; anything but a whole number from
/// 0 to [`KNOWN_REVISION`] is refused.
pub fn read_revision(project: &Value) -> Result<u64, RevisionError> {
    let Some(v) = project.get("format_revision") else {
        return Ok(0);
    };
    let r = v.as_u64().ok_or_else(|| RevisionError::NotARevision(v.clone()))?;
    if r > KNOWN_REVISION {
        return Err(RevisionError::Newer(r));
    }
    Ok(r)
}

/// The lowest revision that holds `project`: 1 when any store carries a
/// converted picture (`graph_v2` on a clip, a track's effects, a sequence,
/// a grade group or a grade version), else 0.
pub fn needed_revision(project: &Value) -> u64 {
    let converted = |o: &Value| o.get(crate::old_fields::V2_FIELD).is_some();
    let list = |v: &Value, key: &str| v.get(key).and_then(Value::as_array).cloned().unwrap_or_default();
    let sequences = match project.get("sequences") {
        Some(_) => list(project, "sequences"),
        None => project.get("sequence").cloned().into_iter().collect(),
    };
    let any = sequences.iter().any(|seq| {
        converted(seq)
            || list(seq, "grade_groups").iter().any(converted)
            || list(seq, "tracks").iter().any(|track| {
                track.get("effects").is_some_and(converted)
                    || list(track, "clips").iter().any(|c| converted(c) || list(c, "grade_versions").iter().any(converted))
            })
    });
    u64::from(any)
}

/// Set the project's `format_revision` to [`needed_revision`], leaving it
/// out when that is 0, as today's saves do.
pub fn write_revision(project: &mut Value) {
    let r = needed_revision(project);
    if let Some(o) = project.as_object_mut() {
        if r == 0 {
            o.remove("format_revision");
        } else {
            o.insert("format_revision".into(), r.into());
        }
    }
}
