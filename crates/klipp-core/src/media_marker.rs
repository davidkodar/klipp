//! Marks an editor leaves on a pool clip while going through footage: a
//! note at a moment, a favourite stretch, a rejected one. They live on
//! the clip in the project, show on the source viewer's scrub bar, and
//! the browser can gather clips by them.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaMarkerKind {
    Note,
    Favourite,
    Reject,
}

impl MediaMarkerKind {
    pub fn label(self) -> &'static str {
        match self {
            MediaMarkerKind::Note => "Marker",
            MediaMarkerKind::Favourite => "Favourite",
            MediaMarkerKind::Reject => "Reject",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MediaMarker {
    /// Where in the file it starts, in seconds.
    pub secs: f64,
    /// How long it runs; 0 for a moment.
    pub duration_secs: f64,
    pub name: String,
    pub kind: MediaMarkerKind,
}

impl Default for MediaMarker {
    fn default() -> Self {
        Self { secs: 0.0, duration_secs: 0.0, name: String::new(), kind: MediaMarkerKind::Note }
    }
}

impl MediaMarker {
    pub fn note(secs: f64) -> Self {
        Self { secs, ..Default::default() }
    }

    pub fn range(kind: MediaMarkerKind, from_secs: f64, to_secs: f64) -> Self {
        let (a, b) = if to_secs < from_secs { (to_secs, from_secs) } else { (from_secs, to_secs) };
        Self { secs: a, duration_secs: b - a, name: String::new(), kind }
    }

    pub fn is_range(&self) -> bool {
        self.duration_secs > 0.0
    }

    pub fn end_secs(&self) -> f64 {
        self.secs + self.duration_secs
    }

    /// Whether `secs` falls inside the marker's stretch.
    pub fn covers(&self, secs: f64) -> bool {
        secs >= self.secs && secs <= self.end_secs()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_range_is_ordered_and_knows_what_it_covers() {
        let r = MediaMarker::range(MediaMarkerKind::Favourite, 8.0, 3.0);
        assert_eq!((r.secs, r.end_secs()), (3.0, 8.0));
        assert!(r.is_range() && r.covers(5.0) && !r.covers(9.0));
        let n = MediaMarker::note(2.0);
        assert!(!n.is_range() && n.covers(2.0));
        assert_eq!(n.kind.label(), "Marker");
    }
}
