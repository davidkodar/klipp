use klipp_core::{MarkerColor, MarkerId, MarkerKind, ReviewStatus, Timecode};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub id: MarkerId,
    pub position: Timecode,
    pub label: String,
    #[serde(default = "default_marker_color")]
    pub color: MarkerColor,
    /// When set, this is a range marker spanning `position..position + duration`.
    /// `None` means a point-in-time marker (rendered as a triangle). Old project
    /// files without this field deserialize to `None` via the serde default.
    #[serde(default)]
    pub duration: Option<Timecode>,
    /// Free-text comment / note. Empty by default — legacy projects
    /// without this field deserialise to "" via the serde default.
    #[serde(default)]
    pub comment: String,
    /// Marker role (Chapter / ToDo / Comment / BestTake / Section /
    /// None). Decouples role from colour so users can mix them. Legacy
    /// projects deserialise to `MarkerKind::None`.
    #[serde(default)]
    pub kind: MarkerKind,
    /// A review comment's author, status and round (#2214): set on
    /// markers of [`MarkerKind::Review`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewNote>,
}

/// Who left a review comment, where it stands, and in which round.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ReviewNote {
    pub author: String,
    #[serde(default)]
    pub status: ReviewStatus,
    #[serde(default)]
    pub round: u32,
}

fn default_marker_color() -> MarkerColor {
    MarkerColor::Blue
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::MarkerId;

    #[test]
    fn marker_duration_defaults_to_none_on_deserialize() {
        // Legacy shape with no `duration` field must deserialize cleanly.
        let legacy = r#"{
            "id": "00000000-0000-0000-0000-000000000001",
            "position": { "value": 0, "rate": 24 },
            "label": "legacy",
            "color": "Blue"
        }"#;
        let m: Marker = serde_json::from_str(legacy).expect("legacy marker parses");
        assert!(m.duration.is_none());
        assert_eq!(m.label, "legacy");
    }

    #[test]
    fn legacy_marker_without_comment_or_kind_deserializes() {
        // Legacy shape (pre-GAP-EDIT-011) must still load — comment
        // defaults to "" and kind to None via serde defaults.
        let legacy = r#"{
            "id": "00000000-0000-0000-0000-000000000001",
            "position": { "value": 0, "rate": 24 },
            "label": "old",
            "color": "Blue"
        }"#;
        let m: Marker = serde_json::from_str(legacy).expect("legacy marker parses");
        assert_eq!(m.comment, "");
        assert_eq!(m.kind, MarkerKind::None);
    }

    #[test]
    fn range_marker_serializes_round_trip() {
        let m = Marker { review: None,
            id: MarkerId::new(),
            position: Timecode { value: 48, rate: 24 },
            label: "Act 1".into(),
            color: MarkerColor::Red,
            duration: Some(Timecode { value: 120, rate: 24 }), // 5s @24fps
            comment: String::new(),
            kind: MarkerKind::Section,
        };
        let json = serde_json::to_string(&m).expect("serialize");
        let back: Marker = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.position.value, 48);
        assert_eq!(back.position.rate, 24);
        assert_eq!(back.label, "Act 1");
        let dur = back.duration.expect("duration preserved");
        assert_eq!(dur.value, 120);
        assert_eq!(dur.rate, 24);
        assert_eq!(back.kind, MarkerKind::Section);
    }

    #[test]
    fn marker_with_comment_and_kind_round_trips() {
        let m = Marker { review: None,
            id: MarkerId::new(),
            position: Timecode { value: 24, rate: 24 },
            label: "Cut on action".into(),
            color: MarkerColor::Yellow,
            duration: None,
            comment: "Maybe trim 2 frames off the head?".into(),
            kind: MarkerKind::ToDo,
        };
        let json = serde_json::to_string(&m).expect("serialize");
        let back: Marker = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.comment, "Maybe trim 2 frames off the head?");
        assert_eq!(back.kind, MarkerKind::ToDo);
    }
}
