use klipp_core::{MarkerColor, MarkerId, Timecode};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    fn range_marker_serializes_round_trip() {
        let m = Marker {
            id: MarkerId::new(),
            position: Timecode { value: 48, rate: 24 },
            label: "Act 1".into(),
            color: MarkerColor::Red,
            duration: Some(Timecode { value: 120, rate: 24 }), // 5s @24fps
        };
        let json = serde_json::to_string(&m).expect("serialize");
        let back: Marker = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.position.value, 48);
        assert_eq!(back.position.rate, 24);
        assert_eq!(back.label, "Act 1");
        let dur = back.duration.expect("duration preserved");
        assert_eq!(dur.value, 120);
        assert_eq!(dur.rate, 24);
    }
}
