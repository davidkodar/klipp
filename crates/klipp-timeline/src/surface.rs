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

/// A plane projective map between fractions of the frame, row by row
/// (#2754).
pub type Map = [f64; 9];

/// How a frame of a track was followed (#2754).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackState {
    /// Followed.
    #[default]
    Good,
    /// Followed, but the surface was hard to see or the fit uncertain.
    Low,
    /// Not followed: the key holds the last good map.
    Lost,
}

impl TrackState {
    fn is_good(&self) -> bool {
        *self == TrackState::Good
    }
}

/// Where the surface is in one file frame: the map from its reference
/// corners to their place in this frame (#2754), so a layer can sit
/// anywhere on the plane, not only on those corners.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SurfaceKey {
    /// File time, seconds: the start of the frame.
    pub source_secs: f64,
    pub map: Map,
    /// How sure the tracker was of this frame, 0 to 1. `None` for a key
    /// whose tracker did not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    #[serde(default, skip_serializing_if = "TrackState::is_good")]
    pub state: TrackState,
}

fn homography(m: &Map) -> klipp_effects::homography::Homography {
    klipp_effects::homography::Homography { h: [[m[0], m[1], m[2]], [m[3], m[4], m[5]], [m[6], m[7], m[8]]] }
}

fn map_of(h: &klipp_effects::homography::Homography) -> Map {
    let h = &h.h;
    [h[0][0], h[0][1], h[0][2], h[1][0], h[1][1], h[1][2], h[2][0], h[2][1], h[2][2]]
}

/// The map taking `from`'s corners to `to`'s, `None` where either is
/// degenerate.
pub fn map_between(from: &Quad, to: &Quad) -> Option<Map> {
    let pt = |q: &Quad| -> [(f64, f64); 4] { q.map(|c| (c[0] as f64, c[1] as f64)) };
    klipp_effects::homography::Homography::from_4_points(&pt(from), &pt(to)).map(|h| map_of(&h))
}

/// `m` after `n`: the point `n` takes somewhere, then `m` from there.
pub fn compose(m: &Map, n: &Map) -> Map {
    map_of(&homography(m).multiply(&homography(n)))
}

/// `q` taken through `m`.
pub fn mapped(m: &Map, q: &Quad) -> Quad {
    let h = homography(m);
    q.map(|c| {
        let (x, y) = h.transform(c[0] as f64, c[1] as f64);
        [x as f32, y as f32]
    })
}

impl SurfaceKey {
    /// The key whose surface is at `corners` in its frame, from reference
    /// corners `reference`; `None` where either quad is degenerate.
    pub fn at_corners(source_secs: f64, reference: &Quad, corners: &Quad) -> Option<Self> {
        Some(Self { source_secs, map: map_between(reference, corners)?, confidence: None, state: TrackState::Good })
    }

    /// The surface's corners in this key's frame, from its reference
    /// corners.
    pub fn corners(&self, reference: &Quad) -> Quad {
        mapped(&self.map, reference)
    }
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
            return first.corners(&self.corners);
        }
        if source_secs >= last.source_secs {
            return last.corners(&self.corners);
        }
        let after = keys.partition_point(|k| k.source_secs <= source_secs);
        let (a, b) = (keys[after - 1].corners(&self.corners), keys[after].corners(&self.corners));
        let span = keys[after].source_secs - keys[after - 1].source_secs;
        let f = if span > 0.0 { ((source_secs - keys[after - 1].source_secs) / span) as f32 } else { 0.0 };
        let mut out = a;
        for (o, (p, q)) in out.iter_mut().zip(a.iter().zip(b.iter())) {
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
            Some(k) if (k.source_secs - shown).abs() <= near => k.corners(&self.corners),
            _ => self.corners_at(shown),
        }
    }

    /// The track with `newer` laid over it: keys from `newer` across the
    /// file time it covers, the earlier keys kept either side of it, so a
    /// track continued from where it was lost (#684 phase 2) keeps what
    /// was good. `newer`'s maps start from `from`, its own reference
    /// corners, and are taken to start from this surface's.
    pub fn merge_keys(&mut self, newer: Vec<SurfaceKey>, from: &Quad) {
        let (Some(first), Some(last)) = (newer.first().map(|k| k.source_secs), newer.last().map(|k| k.source_secs)) else { return };
        let rebase = map_between(&self.corners, from);
        let newer = newer.into_iter().map(|k| SurfaceKey { map: rebase.map_or(k.map, |r| compose(&k.map, &r)), ..k });
        let near = if self.media_fps > 0.0 { 0.5 / self.media_fps } else { 1e-6 };
        let mut keys: Vec<SurfaceKey> = self.keys.iter().copied().filter(|k| k.source_secs < first - near || k.source_secs > last + near).collect();
        keys.extend(newer);
        keys.sort_by(|a, b| a.source_secs.total_cmp(&b.source_secs));
        self.keys = keys;
    }

    /// The surface's width over its height, from its reference corners in
    /// the picture's pixels: what a picture fitted to it keeps its shape
    /// against.
    pub fn aspect(&self) -> Option<f32> {
        let [w, h] = self.source_size;
        let (w, h) = (if w > 0 { w as f32 } else { 1.0 }, if h > 0 { h as f32 } else { 1.0 });
        let len = |a: [f32; 2], b: [f32; 2]| ((a[0] - b[0]) * w).hypot((a[1] - b[1]) * h);
        let c = self.corners;
        let across = (len(c[0], c[1]) + len(c[3], c[2])) * 0.5;
        let down = (len(c[0], c[3]) + len(c[1], c[2])) * 0.5;
        (across > 0.0 && down > 0.0).then(|| across / down)
    }

    /// The file time the track covers, first to last key.
    pub fn tracked_secs(&self) -> Option<(f64, f64)> {
        Some((self.keys.first()?.source_secs, self.keys.last()?.source_secs))
    }

    /// The stretches of the track not followed with confidence (#2754):
    /// for each run of keys in the same weak state, its first and last
    /// file time and that state.
    pub fn weak_spans(&self) -> Vec<(f64, f64, TrackState)> {
        let mut out: Vec<(f64, f64, TrackState)> = Vec::new();
        let mut prev_weak = false;
        for k in &self.keys {
            if k.state == TrackState::Good {
                prev_weak = false;
                continue;
            }
            match out.last_mut() {
                Some(span) if prev_weak && span.2 == k.state => span.1 = k.source_secs,
                _ => out.push((k.source_secs, k.source_secs, k.state)),
            }
            prev_weak = true;
        }
        out
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
    /// How the picture takes the surface's shape (#684 phase 2).
    #[serde(default, skip_serializing_if = "PinFit::is_stretch")]
    pub fit: PinFit,
    /// A margin left inside the surface's edges, a fraction of its width
    /// and height on each side: a screen's bezel when its outer corners
    /// were tracked.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub inset: f32,
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

/// How a pinned picture takes a surface's shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PinFit {
    /// Onto the four corners whatever its shape.
    #[default]
    Stretch,
    /// Its own shape kept, as large as fits, in the middle.
    Fit,
}

impl PinFit {
    fn is_stretch(&self) -> bool {
        *self == PinFit::Stretch
    }

    /// Stretch when the picture is near enough the surface's shape that
    /// nobody would see it squeezed, else fit.
    pub fn for_shapes(picture: Option<[u32; 2]>, surface_aspect: Option<f32>) -> PinFit {
        match (picture, surface_aspect) {
            (Some([w, h]), Some(a)) if w > 0 && h > 0 && a > 0.0 => {
                if ((w as f32 / h as f32) / a).ln().abs() < 1.08f32.ln() { PinFit::Stretch } else { PinFit::Fit }
            }
            _ => PinFit::Stretch,
        }
    }
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

/// The inverse of [`source_quad_in_frame`]: a quad in fractions of the
/// frame, as it is drawn on the viewer, as fractions of the clip's own
/// picture, through which the tracker follows it.
pub fn frame_quad_in_source(q: &Quad, source: [u32; 2], frame: [u32; 2], placement: Option<Placement>) -> Quad {
    let [x0, y0, dw, dh] = fitted(source, frame);
    let (fw, fh) = (frame[0].max(1) as f64, frame[1].max(1) as f64);
    q.map(|c| {
        let (mut x, mut y) = (c[0] as f64 * fw, c[1] as f64 * fh);
        if let Some((px, py, sx, sy, rot, ax, ay)) = placement {
            let (ax, ay) = (ax as f64 * fw, ay as f64 * fh);
            let (dx, dy) = (x - ax - px as f64, y - ay - py as f64);
            let (sin, cos) = (rot as f64).to_radians().sin_cos();
            let (rx, ry) = (dx * cos + dy * sin, -dx * sin + dy * cos);
            let (sx, sy) = (if sx.abs() > f32::EPSILON { sx as f64 } else { 1.0 }, if sy.abs() > f32::EPSILON { sy as f64 } else { 1.0 });
            x = ax + rx / sx;
            y = ay + ry / sy;
        }
        [((x - x0) / dw) as f32, ((y - y0) / dh) as f32]
    })
}

/// Where on a surface's quad (in the frame) a picture goes: all of it for
/// a stretch, or the part its shape fills for a fit, and `inset` in from
/// the edges either way, mapped through the surface's own perspective.
pub fn pin_target(quad: &Quad, surface_aspect: Option<f32>, picture: Option<[u32; 2]>, fit: PinFit, inset: f32) -> Option<Quad> {
    let inset = inset.clamp(0.0, 0.45);
    let (mut u, mut v) = ([inset, 1.0 - inset], [inset, 1.0 - inset]);
    if let (PinFit::Fit, Some([w, h]), Some(a)) = (fit, picture, surface_aspect) {
        if w > 0 && h > 0 && a > 0.0 {
            let picture = w as f32 / h as f32;
            let span = 1.0 - 2.0 * inset;
            if picture > a {
                let keep = span * a / picture;
                v = [0.5 - keep * 0.5, 0.5 + keep * 0.5];
            } else {
                let keep = span * picture / a;
                u = [0.5 - keep * 0.5, 0.5 + keep * 0.5];
            }
        }
    }
    if u == [0.0, 1.0] && v == [0.0, 1.0] {
        return Some(*quad);
    }
    let unit = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
    let to = quad.map(|c| (c[0] as f64, c[1] as f64));
    let h = klipp_effects::homography::Homography::from_4_points(&unit, &to)?;
    let mut out = [[0.0f32; 2]; 4];
    for (o, (x, y)) in out.iter_mut().zip([(u[0], v[0]), (u[1], v[0]), (u[1], v[1]), (u[0], v[1])]) {
        let (mx, my) = h.map(x as f64, y as f64)?;
        *o = [mx as f32, my as f32];
    }
    Some(out)
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

    fn near(a: Quad, b: Quad) -> bool {
        a.iter().zip(&b).all(|(p, q)| (p[0] - q[0]).abs() < 1e-5 && (p[1] - q[1]).abs() < 1e-5)
    }

    /// A track continued from corners set on another frame (#2754): its
    /// maps start from those corners, and merged they are taken to start
    /// from the surface's own, so the corners land where that run found
    /// them.
    #[test]
    fn a_continued_track_from_other_corners_lands_where_it_was_found() {
        let reference = [[0.1, 0.1], [0.4, 0.1], [0.4, 0.3], [0.1, 0.3]];
        let mut s = Surface { id: uuid::Uuid::nil(), name: "S".into(), corners: reference, reference_secs: 0.0, keys: vec![SurfaceKey::at_corners(0.0, &reference, &reference).unwrap()], source_size: [0, 0], media_fps: 25.0 };
        // The run's own corners, set on frame 10, and where it found them on frame 11.
        let theirs = [[0.5, 0.5], [0.8, 0.52], [0.79, 0.7], [0.51, 0.71]];
        let found = [[0.52, 0.5], [0.82, 0.53], [0.8, 0.71], [0.53, 0.72]];
        s.merge_keys(vec![SurfaceKey::at_corners(0.4, &theirs, &theirs).unwrap(), SurfaceKey::at_corners(0.44, &theirs, &found).unwrap()], &theirs);
        assert!(near(s.corners_shown_at(0.4), theirs));
        assert!(near(s.corners_shown_at(0.44), found));
    }

    /// The stretches followed uncertainly or lost, each run once.
    #[test]
    fn the_weak_stretches_are_the_runs_of_low_and_lost_keys() {
        let q = square(0.1);
        let k = |f: i32, state: TrackState| SurfaceKey { state, ..SurfaceKey::at_corners(f as f64, &q, &q).unwrap() };
        let s = Surface {
            id: uuid::Uuid::nil(), name: "S".into(), corners: q, reference_secs: 0.0,
            keys: vec![k(0, TrackState::Good), k(1, TrackState::Low), k(2, TrackState::Low), k(3, TrackState::Lost), k(4, TrackState::Lost), k(5, TrackState::Good), k(6, TrackState::Low)],
            source_size: [0, 0], media_fps: 1.0,
        };
        assert_eq!(s.weak_spans(), vec![(1.0, 2.0, TrackState::Low), (3.0, 4.0, TrackState::Lost), (6.0, 6.0, TrackState::Low)]);
    }

    #[test]
    fn corners_interpolate_between_keys_and_hold_outside() {
        let s = Surface {
            id: uuid::Uuid::nil(),
            name: "Screen".into(),
            corners: square(0.5),
            reference_secs: 1.0,
            keys: vec![SurfaceKey::at_corners(1.0, &square(0.5), &square(0.2)).unwrap(), SurfaceKey::at_corners(2.0, &square(0.5), &square(0.4)).unwrap()],
            source_size: [0, 0],
            media_fps: 0.0,
        };
        assert!(near(s.corners_at(0.0), square(0.2)));
        assert!(near(s.corners_at(3.0), square(0.4)));
        assert!((s.corners_at(1.5)[0][0] - 0.3).abs() < 1e-6);
        assert_eq!(s.tracked_secs(), Some((1.0, 2.0)));
        let untracked = Surface { keys: Vec::new(), ..s };
        assert_eq!(untracked.corners_at(1.5), square(0.5), "untracked, it stays on its corners");
    }

    /// A key per file frame at 30 fps: a time inside a frame shows that
    /// frame's corners, not a blend towards the next.
    #[test]
    fn the_shown_frame_holds_its_key() {
        let keys = (0..4).map(|f| SurfaceKey::at_corners(f as f64 / 30.0, &square(0.0), &square(0.1 * f as f32)).unwrap()).collect();
        let s = Surface { id: uuid::Uuid::nil(), name: "S".into(), corners: square(0.0), reference_secs: 0.0, keys, source_size: [0, 0], media_fps: 30.0 };
        // 0.04 s is 1.2 frames in: frame 1 is shown.
        assert!(near(s.corners_shown_at(0.04), square(0.1)));
        // 0.05 s is 1.5 frames in: frame 2 starts within half a frame.
        assert!(near(s.corners_shown_at(0.05), square(0.2)));
        assert!(near(s.corners_shown_at(10.0), square(0.3)), "held after the track");
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

    /// Frame to picture undoes picture to frame, through a fit and a
    /// placement that moves, scales and turns.
    #[test]
    fn frame_corners_go_back_to_the_picture() {
        let q = [[0.2, 0.3], [0.7, 0.25], [0.75, 0.8], [0.15, 0.7]];
        let placed = Some((120.0, -40.0, 1.3, 0.8, 12.0, 0.4, 0.6));
        let there = source_quad_in_frame(&q, [1080, 1920], [1920, 1080], placed);
        let back = frame_quad_in_source(&there, [1080, 1920], [1920, 1080], placed);
        for (a, b) in q.iter().flatten().zip(back.iter().flatten()) {
            assert!((a - b).abs() < 1e-4, "{q:?} came back {back:?}");
        }
    }

    /// Continuing a track from where it was lost lays the new keys over
    /// that stretch and keeps the rest.
    #[test]
    fn a_continued_track_keeps_what_was_good() {
        let key = |f: i32, x: f32| SurfaceKey::at_corners(f as f64 / 25.0, &square(0.0), &square(x)).unwrap();
        let mut s = Surface { id: uuid::Uuid::nil(), name: "S".into(), corners: square(0.0), reference_secs: 0.0, keys: (0..10).map(|f| key(f, 0.0)).collect(), source_size: [0, 0], media_fps: 25.0 };
        s.merge_keys((8..20).map(|f| key(f, 0.5)).collect(), &square(0.0));
        assert_eq!(s.keys.len(), 20);
        assert!(near(s.keys[7].corners(&s.corners), square(0.0)), "before the new stretch, kept");
        assert!(near(s.keys[8].corners(&s.corners), square(0.5)), "over it, the new keys");
        assert!(s.keys.windows(2).all(|w| w[0].source_secs < w[1].source_secs));
    }

    /// A square picture on a wide surface keeps its shape in the middle;
    /// one of the surface's own shape stretches onto it; an inset leaves a
    /// margin.
    #[test]
    fn a_pin_fits_its_picture_to_the_surface() {
        let quad = [[0.1, 0.1], [0.9, 0.1], [0.9, 0.5], [0.1, 0.5]];
        assert_eq!(PinFit::for_shapes(Some([1920, 1080]), Some(16.0 / 9.0)), PinFit::Stretch);
        assert_eq!(PinFit::for_shapes(Some([1000, 1000]), Some(16.0 / 9.0)), PinFit::Fit);
        let square = pin_target(&quad, Some(2.0), Some([500, 500]), PinFit::Fit, 0.0).unwrap();
        // Half the width, centred: x from 0.3 to 0.7, the full height.
        assert!((square[0][0] - 0.3).abs() < 1e-5 && (square[1][0] - 0.7).abs() < 1e-5 && (square[0][1] - 0.1).abs() < 1e-5, "{square:?}");
        assert_eq!(pin_target(&quad, Some(2.0), Some([500, 500]), PinFit::Stretch, 0.0), Some(quad));
        let inset = pin_target(&quad, Some(2.0), None, PinFit::Stretch, 0.05).unwrap();
        assert!((inset[0][0] - 0.14).abs() < 1e-5 && (inset[0][1] - 0.12).abs() < 1e-5, "{inset:?}");
        let s = Surface { id: uuid::Uuid::nil(), name: "S".into(), corners: [[0.0, 0.0], [0.5, 0.0], [0.5, 0.5], [0.0, 0.5]], reference_secs: 0.0, keys: Vec::new(), source_size: [1920, 1080], media_fps: 0.0 };
        assert!((s.aspect().unwrap() - 16.0 / 9.0).abs() < 1e-4);
    }

    #[test]
    fn a_quad_needs_an_area() {
        assert!(quad_is_usable(&square(0.1)));
        assert!(!quad_is_usable(&[[0.1, 0.1]; 4]));
        assert!(!quad_is_usable(&[[f32::NAN, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]));
    }
}
