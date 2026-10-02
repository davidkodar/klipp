//! Tracked surfaces (#684): a flat thing in a shot, a phone screen, a
//! sign, a monitor, followed through the clip, and what is pinned to it.
//!
//! A surface lives on the clip it was tracked in. Its keys are in the
//! file's own time, so trimming, splitting, moving or retiming that clip
//! leaves the track on the picture it was made from. A pin is a clip
//! above with a corner pin whose resting corners are the surface's; the
//! renderer reads the live corners from the surface, so re-tracking moves
//! everything pinned to it with no other step, and a pin whose surface is
//! gone stays where it was on the reference frame.

use klipp_core::ClipId;
use serde::{Deserialize, Serialize};

/// Four corners, top left, top right, bottom right, bottom left, as
/// fractions of the frame.
pub type Quad = [[f32; 2]; 4];

/// Where the surface is in one file frame.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SurfaceKey {
    /// File time, seconds: the start of the frame.
    pub source_secs: f64,
    pub corners: Quad,
}

/// A surface tracked in a clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Surface {
    pub id: uuid::Uuid,
    pub name: String,
    /// The corners on the reference frame: what a pin is placed on.
    pub corners: Quad,
    /// File time of the reference frame, seconds.
    pub reference_secs: f64,
    /// The tracked corners, earliest first. Empty until tracked.
    #[serde(default)]
    pub keys: Vec<SurfaceKey>,
}

impl Surface {
    /// The corners at `source_secs` of the file: between keys, a straight
    /// line from one to the next; before the first or after the last, held.
    pub fn corners_at(&self, source_secs: f64) -> Quad {
        let keys = &self.keys;
        let (Some(first), Some(last)) = (keys.first(), keys.last()) else { return self.corners };
        if source_secs <= first.source_secs {
            return first.corners;
        }
        if source_secs >= last.source_secs {
            return last.corners;
        }
        let after = keys.partition_point(|k| k.source_secs <= source_secs);
        let (a, b) = (&keys[after - 1], &keys[after]);
        let span = b.source_secs - a.source_secs;
        let f = if span > 0.0 { ((source_secs - a.source_secs) / span) as f32 } else { 0.0 };
        let mut out = a.corners;
        for (o, (p, q)) in out.iter_mut().zip(a.corners.iter().zip(b.corners.iter())) {
            o[0] = p[0] + (q[0] - p[0]) * f;
            o[1] = p[1] + (q[1] - p[1]) * f;
        }
        out
    }

    /// The file time the track covers, first to last key.
    pub fn tracked_secs(&self) -> Option<(f64, f64)> {
        Some((self.keys.first()?.source_secs, self.keys.last()?.source_secs))
    }
}

/// A clip pinned to a surface on another clip.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SurfacePin {
    /// The clip the surface was tracked in.
    pub host: ClipId,
    pub surface: uuid::Uuid,
}

/// A quad that can carry a picture: every corner finite, and an area
/// that is not degenerate.
pub fn quad_is_usable(q: &Quad) -> bool {
    if q.iter().flatten().any(|v| !v.is_finite()) {
        return false;
    }
    // Shoelace area, in frame fractions squared.
    let mut twice = 0.0f32;
    for i in 0..4 {
        let (a, b) = (q[i], q[(i + 1) % 4]);
        twice += a[0] * b[1] - b[0] * a[1];
    }
    twice.abs() * 0.5 > 1e-5
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: f32) -> Quad {
        [[x, 0.0], [x + 0.1, 0.0], [x + 0.1, 0.1], [x, 0.1]]
    }

    #[test]
    fn corners_interpolate_between_keys_and_hold_outside() {
        let s = Surface {
            id: uuid::Uuid::nil(),
            name: "Screen".into(),
            corners: square(0.5),
            reference_secs: 1.0,
            keys: vec![SurfaceKey { source_secs: 1.0, corners: square(0.2) }, SurfaceKey { source_secs: 2.0, corners: square(0.4) }],
        };
        assert_eq!(s.corners_at(0.0), square(0.2));
        assert_eq!(s.corners_at(3.0), square(0.4));
        assert!((s.corners_at(1.5)[0][0] - 0.3).abs() < 1e-6);
        assert_eq!(s.tracked_secs(), Some((1.0, 2.0)));
        let untracked = Surface { keys: Vec::new(), ..s };
        assert_eq!(untracked.corners_at(1.5), square(0.5), "untracked, it stays on its corners");
    }

    #[test]
    fn a_quad_needs_an_area() {
        assert!(quad_is_usable(&square(0.1)));
        assert!(!quad_is_usable(&[[0.1, 0.1]; 4]));
        assert!(!quad_is_usable(&[[f32::NAN, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]));
    }
}
