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
    /// The picture the corners are fractions of, width and height as
    /// shown (turned for a phone shot): where they land in the frame
    /// depends on how that picture is fitted. Zero when not known, read
    /// as the frame's own shape.
    #[serde(default)]
    pub source_size: [u32; 2],
    /// The file's frame rate: a key per file frame, and a frame shown is
    /// that frame's key, not a blend towards the next (#712 review).
    #[serde(default)]
    pub media_fps: f64,
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

    /// The corners on the file frame shown at `source_secs`: the frame
    /// whose start is at or before it by half a frame, as playback and
    /// export choose it (#696), so a pin holds while the picture does.
    /// Between keys more than a frame apart, a straight line.
    pub fn corners_shown_at(&self, source_secs: f64) -> Quad {
        if self.media_fps <= 0.0 {
            return self.corners_at(source_secs);
        }
        let frame = (source_secs * self.media_fps + 0.5 + 1e-9).floor();
        let shown = frame / self.media_fps;
        let near = 0.25 / self.media_fps;
        let at = self.keys.partition_point(|k| k.source_secs < shown - near);
        match self.keys.get(at) {
            Some(k) if (k.source_secs - shown).abs() <= near => k.corners,
            _ => self.corners_at(shown),
        }
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
    /// The pinned picture's width and height as shown: it is fitted into
    /// the frame first, and that fitted picture, not the frame round it,
    /// goes onto the surface. `None` reads as the frame's own shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub picture_size: Option<[u32; 2]>,
}

/// A clip's placement on the frame: position, scale, rotation and anchor
/// as its `Transform` effect takes them (position in frame pixels,
/// rotation in degrees, anchor in fractions).
pub type Placement = (f32, f32, f32, f32, f32, f32, f32);

/// Where a picture of `size` sits when fitted into a `frame`, as the
/// renderer fits it: one scale to fit, centred. Pixels: x, y, width,
/// height.
fn fitted(size: [u32; 2], frame: [u32; 2]) -> [f64; 4] {
    let (fw, fh) = (frame[0].max(1) as f64, frame[1].max(1) as f64);
    if size[0] == 0 || size[1] == 0 {
        return [0.0, 0.0, fw, fh];
    }
    let (w, h) = (size[0] as f64, size[1] as f64);
    let scale = (fw / w).min(fh / h);
    let (dw, dh) = ((w * scale).round().clamp(1.0, fw), (h * scale).round().clamp(1.0, fh));
    [((fw - dw) / 2.0).floor(), ((fh - dh) / 2.0).floor(), dw, dh]
}

/// A quad in fractions of a clip's own picture (`source`, as shown) as
/// fractions of the `frame` it plays in: fitted as the renderer fits the
/// picture, then moved by the clip's placement.
pub fn source_quad_in_frame(q: &Quad, source: [u32; 2], frame: [u32; 2], placement: Option<Placement>) -> Quad {
    let [x0, y0, dw, dh] = fitted(source, frame);
    let (fw, fh) = (frame[0].max(1) as f64, frame[1].max(1) as f64);
    q.map(|c| {
        let (mut x, mut y) = (x0 + c[0] as f64 * dw, y0 + c[1] as f64 * dh);
        if let Some((px, py, sx, sy, rot, ax, ay)) = placement {
            // As the transform maps forward: about the anchor, scaled,
            // turned, then moved.
            let (ax, ay) = (ax as f64 * fw, ay as f64 * fh);
            let (dx, dy) = ((x - ax) * sx as f64, (y - ay) * sy as f64);
            let (sin, cos) = (rot as f64).to_radians().sin_cos();
            x = ax + px as f64 + dx * cos - dy * sin;
            y = ay + py as f64 + dx * sin + dy * cos;
        }
        [(x / fw) as f32, (y / fh) as f32]
    })
}

/// The corner pin, in fractions of the frame, that puts a picture of
/// `picture` size, fitted into the frame, exactly onto `quad`: the map
/// from the fitted picture to the quad, carried out to the frame's
/// corners. `None` when the quad cannot carry it.
pub fn pin_frame_corners(quad: &Quad, picture: Option<[u32; 2]>, frame: [u32; 2]) -> Option<Quad> {
    if !quad_is_usable(quad) {
        return None;
    }
    let (fw, fh) = (frame[0].max(1) as f64, frame[1].max(1) as f64);
    let [x0, y0, dw, dh] = fitted(picture.unwrap_or(frame), frame);
    if (x0, y0, dw, dh) == (0.0, 0.0, fw, fh) {
        return Some(*quad);
    }
    let src = [(x0, y0), (x0 + dw, y0), (x0 + dw, y0 + dh), (x0, y0 + dh)];
    let dst = quad.map(|c| (c[0] as f64 * fw, c[1] as f64 * fh));
    let h = klipp_effects::homography::Homography::from_4_points(&src, &dst)?;
    let mut out = [[0.0f32; 2]; 4];
    for (o, (x, y)) in out.iter_mut().zip([(0.0, 0.0), (fw, 0.0), (fw, fh), (0.0, fh)]) {
        let (mx, my) = h.map(x, y)?;
        *o = [(mx / fw) as f32, (my / fh) as f32];
    }
    out.iter().flatten().all(|v| v.is_finite()).then_some(out)
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
            source_size: [0, 0],
            media_fps: 0.0,
        };
        assert_eq!(s.corners_at(0.0), square(0.2));
        assert_eq!(s.corners_at(3.0), square(0.4));
        assert!((s.corners_at(1.5)[0][0] - 0.3).abs() < 1e-6);
        assert_eq!(s.tracked_secs(), Some((1.0, 2.0)));
        let untracked = Surface { keys: Vec::new(), ..s };
        assert_eq!(untracked.corners_at(1.5), square(0.5), "untracked, it stays on its corners");
    }

    /// A key per file frame at 30 fps: a time inside a frame shows that
    /// frame's corners, not a blend towards the next.
    #[test]
    fn the_shown_frame_holds_its_key() {
        let keys = (0..4).map(|f| SurfaceKey { source_secs: f as f64 / 30.0, corners: square(0.1 * f as f32) }).collect();
        let s = Surface { id: uuid::Uuid::nil(), name: "S".into(), corners: square(0.0), reference_secs: 0.0, keys, source_size: [0, 0], media_fps: 30.0 };
        // 0.04 s is 1.2 frames in: frame 1 is shown.
        assert_eq!(s.corners_shown_at(0.04), square(0.1));
        // 0.05 s is 1.5 frames in: frame 2 starts within half a frame.
        assert_eq!(s.corners_shown_at(0.05), square(0.2));
        assert_eq!(s.corners_shown_at(10.0), square(0.3), "held after the track");
    }

    /// A portrait picture in a wide frame sits in the middle third: its
    /// corners are moved there, and the clip's placement moves them on.
    #[test]
    fn source_corners_land_where_the_picture_is_fitted() {
        let q = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        let f = source_quad_in_frame(&q, [1080, 1920], [1920, 1080], None);
        let left = (1920.0 - 607.0) / 2.0 / 1920.0;
        assert!((f[0][0] - left as f32).abs() < 1e-3 && f[0][1].abs() < 1e-6, "{f:?}");
        assert!((f[2][0] - (1.0 - left as f32)).abs() < 2e-3 && (f[2][1] - 1.0).abs() < 1e-6, "{f:?}");
        // Moved 192 px right and scaled to half about the centre.
        let g = source_quad_in_frame(&q, [1920, 1080], [1920, 1080], Some((192.0, 0.0, 0.5, 0.5, 0.0, 0.5, 0.5)));
        assert!((g[0][0] - 0.35).abs() < 1e-6 && (g[0][1] - 0.25).abs() < 1e-6, "{g:?}");
    }

    /// A square picture fitted into a wide frame: the pin's frame corners
    /// put the picture itself, not its bars, on the quad.
    #[test]
    fn a_fitted_picture_goes_onto_the_quad() {
        let quad = [[0.3, 0.3], [0.6, 0.32], [0.62, 0.6], [0.28, 0.58]];
        assert_eq!(pin_frame_corners(&quad, None, [1920, 1080]), Some(quad), "a frame-shaped picture is the quad");
        let corners = pin_frame_corners(&quad, Some([1000, 1000]), [1920, 1080]).unwrap();
        let src = [(0.0, 0.0), (1920.0, 0.0), (1920.0, 1080.0), (0.0, 1080.0)];
        let dst = corners.map(|c| (c[0] as f64 * 1920.0, c[1] as f64 * 1080.0));
        let h = klipp_effects::homography::Homography::from_4_points(&src, &dst).unwrap();
        let x0 = (1920.0 - 1080.0) / 2.0;
        for ((x, y), want) in [(x0, 0.0), (x0 + 1080.0, 0.0), (x0 + 1080.0, 1080.0), (x0, 1080.0)].into_iter().zip(quad) {
            let (mx, my) = h.map(x, y).unwrap();
            assert!((mx / 1920.0 - want[0] as f64).abs() < 1e-4 && (my / 1080.0 - want[1] as f64).abs() < 1e-4, "{mx},{my} vs {want:?}");
        }
    }

    #[test]
    fn a_quad_needs_an_area() {
        assert!(quad_is_usable(&square(0.1)));
        assert!(!quad_is_usable(&[[0.1, 0.1]; 4]));
        assert!(!quad_is_usable(&[[f32::NAN, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]));
    }
}
