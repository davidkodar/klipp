use klipp_core::{MarkerColor, MarkerId, Timecode};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Marker {
    pub id: MarkerId,
    pub position: Timecode,
    pub label: String,
    #[serde(default = "default_marker_color")]
    pub color: MarkerColor,
}

fn default_marker_color() -> MarkerColor {
    MarkerColor::Blue
}
