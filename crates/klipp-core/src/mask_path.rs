//! A freeform mask's path (#74): points with curve handles, and the
//! outline both renderers fill, flattened to line segments at the
//! render's size.
//!
//! Points are in frame fractions (x across the width, y down the
//! height). A handle is an offset from its point, so a point moved
//! carries its handles. A point whose handles are both zero is a corner,
//! and a path of corners is the polygon masks were before curves: it is
//! saved, flattened and filled exactly as that polygon was.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// One point of a freeform mask: where it is, and the handles that pull
/// the curve into it (`in`) and out of it (`out`), each an offset from
/// the point in frame fractions. `smooth` keeps the two handles in line
/// while they are edited.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BezierPoint {
    pub x: f32,
    pub y: f32,
    pub in_dx: f32,
    pub in_dy: f32,
    pub out_dx: f32,
    pub out_dy: f32,
    pub smooth: bool,
}

impl BezierPoint {
    /// A corner at `(x, y)`: no handles.
    pub fn corner(x: f32, y: f32) -> Self {
        Self { x, y, ..Self::default() }
    }

    /// Whether the point has no handles and isn't smooth: a polygon's
    /// corner, saved as `[x, y]`.
    pub fn is_corner(&self) -> bool {
        !self.smooth && self.in_dx == 0.0 && self.in_dy == 0.0 && self.out_dx == 0.0 && self.out_dy == 0.0
    }

    pub fn pos(&self) -> (f32, f32) {
        (self.x, self.y)
    }

    /// Where the in handle's end is.
    pub fn in_handle(&self) -> (f32, f32) {
        (self.x + self.in_dx, self.y + self.in_dy)
    }

    /// Where the out handle's end is.
    pub fn out_handle(&self) -> (f32, f32) {
        (self.x + self.out_dx, self.y + self.out_dy)
    }
}

impl From<(f32, f32)> for BezierPoint {
    fn from((x, y): (f32, f32)) -> Self {
        Self::corner(x, y)
    }
}

/// The saved form: a corner as `[x, y]`, as a polygon's points always
/// were; a point with handles as an object.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum SavedPoint {
    Corner([f32; 2]),
    Curved {
        x: f32,
        y: f32,
        #[serde(default, rename = "in", skip_serializing_if = "is_zero")]
        handle_in: [f32; 2],
        #[serde(default, rename = "out", skip_serializing_if = "is_zero")]
        handle_out: [f32; 2],
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        smooth: bool,
    },
}

fn is_zero(v: &[f32; 2]) -> bool {
    v[0] == 0.0 && v[1] == 0.0
}

impl Serialize for BezierPoint {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if self.is_corner() {
            SavedPoint::Corner([self.x, self.y]).serialize(s)
        } else {
            SavedPoint::Curved { x: self.x, y: self.y, handle_in: [self.in_dx, self.in_dy], handle_out: [self.out_dx, self.out_dy], smooth: self.smooth }.serialize(s)
        }
    }
}

impl<'de> Deserialize<'de> for BezierPoint {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match SavedPoint::deserialize(d)? {
            SavedPoint::Corner([x, y]) => Self::corner(x, y),
            SavedPoint::Curved { x, y, handle_in, handle_out, smooth } => {
                Self { x, y, in_dx: handle_in[0], in_dy: handle_in[1], out_dx: handle_out[0], out_dy: handle_out[1], smooth }
            }
        })
    }
}

/// How a freeform mask's shape moves from one key to the next (#74).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PathEase {
    /// At an even pace.
    #[default]
    Linear,
    /// Slowing into the next key and out of this one.
    Ease,
    /// Holding this shape until the next key.
    Hold,
}

/// A freeform mask's shape at one moment (#74): every point and handle,
/// as many points as every other key of the mask holds, in the same
/// order, so each point has a place in every key to move between.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PathKey {
    pub time_secs: f64,
    pub points: Vec<BezierPoint>,
    #[serde(default, skip_serializing_if = "is_linear")]
    pub ease: PathEase,
}

fn is_linear(e: &PathEase) -> bool {
    *e == PathEase::Linear
}

/// The shape `keys` (earliest first) give at `time_secs`: before the
/// first and after the last, held; between two, each point and handle
/// moved as the earlier key's ease says. `None` without keys, or between
/// two that don't hold the same number of points, which no edit makes.
pub fn shape_at(keys: &[PathKey], time_secs: f64) -> Option<Vec<BezierPoint>> {
    let (first, last) = (keys.first()?, keys.last()?);
    if time_secs <= first.time_secs {
        return Some(first.points.clone());
    }
    if time_secs >= last.time_secs {
        return Some(last.points.clone());
    }
    let i = keys.partition_point(|k| k.time_secs <= time_secs);
    let (a, b) = (&keys[i - 1], &keys[i]);
    if a.points.len() != b.points.len() {
        return Some(a.points.clone());
    }
    let span = b.time_secs - a.time_secs;
    let t = if span > 0.0 { ((time_secs - a.time_secs) / span) as f32 } else { 0.0 };
    let t = match a.ease {
        PathEase::Linear => t,
        PathEase::Ease => t * t * (3.0 - 2.0 * t),
        PathEase::Hold => return Some(a.points.clone()),
    };
    let l = |x: f32, y: f32| x + (y - x) * t;
    Some(
        a.points
            .iter()
            .zip(&b.points)
            .map(|(p, q)| BezierPoint {
                x: l(p.x, q.x),
                y: l(p.y, q.y),
                in_dx: l(p.in_dx, q.in_dx),
                in_dy: l(p.in_dy, q.in_dy),
                out_dx: l(p.out_dx, q.out_dx),
                out_dy: l(p.out_dy, q.out_dy),
                smooth: p.smooth,
            })
            .collect(),
    )
}

/// `keys` with the shape at `time_secs` set to `points`: the key there
/// (within `tolerance`) changed, else a key added in its place, as Linear.
pub fn with_key(keys: &[PathKey], time_secs: f64, points: Vec<BezierPoint>, tolerance: f64) -> Vec<PathKey> {
    let mut out = keys.to_vec();
    match out.iter_mut().find(|k| (k.time_secs - time_secs).abs() <= tolerance) {
        Some(k) => k.points = points,
        None => {
            let at = out.partition_point(|k| k.time_secs < time_secs);
            out.insert(at, PathKey { time_secs, points, ease: PathEase::Linear });
        }
    }
    out
}

type P = (f32, f32);

fn lerp(a: P, b: P, t: f32) -> P {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

/// The cubic from point `i` to the next, its four control points.
fn segment(points: &[BezierPoint], i: usize) -> [P; 4] {
    let (a, b) = (&points[i], &points[(i + 1) % points.len()]);
    [a.pos(), a.out_handle(), b.in_handle(), b.pos()]
}

/// `points` with a point put on the curve from point `seg` to the next,
/// at `t` along it, the curve keeping its shape: the segment's handles
/// are split as de Casteljau splits it. The new point is at `seg + 1`;
/// it is smooth when the curve through it is.
pub fn split_segment(points: &[BezierPoint], seg: usize, t: f32) -> Vec<BezierPoint> {
    let n = points.len();
    if n == 0 {
        return Vec::new();
    }
    let seg = seg % n;
    let t = t.clamp(0.0, 1.0);
    let c = segment(points, seg);
    let (ab, bc, cd) = (lerp(c[0], c[1], t), lerp(c[1], c[2], t), lerp(c[2], c[3], t));
    let (abc, bcd) = (lerp(ab, bc, t), lerp(bc, cd, t));
    let m = lerp(abc, bcd, t);
    let mut out = points.to_vec();
    let next = (seg + 1) % n;
    out[seg].out_dx = ab.0 - c[0].0;
    out[seg].out_dy = ab.1 - c[0].1;
    out[next].in_dx = cd.0 - c[3].0;
    out[next].in_dy = cd.1 - c[3].1;
    let straight = points[seg].out_dx == 0.0 && points[seg].out_dy == 0.0 && points[next].in_dx == 0.0 && points[next].in_dy == 0.0;
    let new = if straight {
        BezierPoint::corner(m.0, m.1)
    } else {
        BezierPoint { x: m.0, y: m.1, in_dx: abc.0 - m.0, in_dy: abc.1 - m.1, out_dx: bcd.0 - m.0, out_dy: bcd.1 - m.1, smooth: true }
    };
    out.insert(seg + 1, new);
    out
}

/// `points` less those in `remove`, or `None` when fewer than three
/// would be left: a shape needs three.
pub fn without_points(points: &[BezierPoint], remove: &std::collections::BTreeSet<usize>) -> Option<Vec<BezierPoint>> {
    let kept: Vec<BezierPoint> = points.iter().enumerate().filter(|(i, _)| !remove.contains(i)).map(|(_, p)| *p).collect();
    (kept.len() >= 3 && kept.len() < points.len()).then_some(kept)
}

/// The most points one outline is flattened to.
pub const MAX_OUTLINE_POINTS: usize = 4096;

/// How far, in pixels, a flattened curve may stray from the true one.
pub const OUTLINE_TOLERANCE_PX: f32 = 0.25;

/// The closed path through `points`, each segment a cubic from a point
/// along its out handle and the next point's in handle, flattened to line
/// segments no further than `tolerance` (in frame widths) from the curve,
/// on a frame `aspect` (height over width) tall. In frame fractions, the
/// first point first. A segment between two corners is the straight line
/// it always was, so a polygon comes back as its own points.
pub fn flatten(points: &[BezierPoint], tolerance: f32, aspect: f32) -> Vec<(f32, f32)> {
    let aspect = if aspect.is_finite() && aspect > 0.0 { aspect } else { 9.0 / 16.0 };
    let tolerance = if tolerance.is_finite() && tolerance > 0.0 { tolerance } else { 1e-4 };
    let n = points.len();
    let mut out = Vec::with_capacity(n * 4);
    // In frame widths both ways, where a curve's flatness is measured.
    let w = |p: (f32, f32)| (p.0, p.1 * aspect);
    for i in 0..n {
        let (a, b) = (&points[i], &points[(i + 1) % n]);
        out.push(a.pos());
        if out.len() >= MAX_OUTLINE_POINTS {
            break;
        }
        let straight = a.out_dx == 0.0 && a.out_dy == 0.0 && b.in_dx == 0.0 && b.in_dy == 0.0;
        if straight {
            continue;
        }
        let c = [w(a.pos()), w(a.out_handle()), w(b.in_handle()), w(b.pos())];
        subdivide(&c, tolerance, 0, &mut out, aspect);
    }
    out.truncate(MAX_OUTLINE_POINTS);
    out
}

/// The inner points of the cubic `c` (frame widths), pushed in frame
/// fractions, its last point left to the next segment.
fn subdivide(c: &[(f32, f32); 4], tolerance: f32, depth: u32, out: &mut Vec<(f32, f32)>, aspect: f32) {
    if out.len() >= MAX_OUTLINE_POINTS {
        return;
    }
    if depth >= 16 || flat(c) <= tolerance {
        return;
    }
    let mid = |p: (f32, f32), q: (f32, f32)| ((p.0 + q.0) * 0.5, (p.1 + q.1) * 0.5);
    let (ab, bc, cd) = (mid(c[0], c[1]), mid(c[1], c[2]), mid(c[2], c[3]));
    let (abc, bcd) = (mid(ab, bc), mid(bc, cd));
    let m = mid(abc, bcd);
    subdivide(&[c[0], ab, abc, m], tolerance, depth + 1, out, aspect);
    if out.len() < MAX_OUTLINE_POINTS {
        out.push((m.0, m.1 / aspect));
    }
    subdivide(&[m, bcd, cd, c[3]], tolerance, depth + 1, out, aspect);
}

/// How far the cubic `c` strays from its chord, at most: the further of
/// its two inner control points' distances to the chord's line, which
/// bounds the curve's.
fn flat(c: &[(f32, f32); 4]) -> f32 {
    let (ax, ay) = c[0];
    let (bx, by) = c[3];
    let (ex, ey) = (bx - ax, by - ay);
    let len = (ex * ex + ey * ey).sqrt();
    let off = |p: (f32, f32)| {
        if len < 1e-9 {
            ((p.0 - ax).powi(2) + (p.1 - ay).powi(2)).sqrt()
        } else {
            ((p.0 - ax) * ey - (p.1 - ay) * ex).abs() / len
        }
    };
    off(c[1]).max(off(c[2]))
}

type OutlineKey = (Vec<u32>, u32, u32);

/// Outlines already flattened, by their path's points and the frame's
/// size: a shape drawn again unchanged (a paused frame, a drag that
/// rests) is not flattened again. Bounded; cleared whole when full.
fn cache() -> &'static Mutex<HashMap<OutlineKey, Arc<Vec<(f32, f32)>>>> {
    static CACHE: OnceLock<Mutex<HashMap<OutlineKey, Arc<Vec<(f32, f32)>>>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

const CACHE_ENTRIES: usize = 256;

/// [`flatten`] at [`OUTLINE_TOLERANCE_PX`] on a `width` by `height`
/// frame, kept for the next ask of the same path at the same size.
pub fn outline(points: &[BezierPoint], width: u32, height: u32) -> Arc<Vec<(f32, f32)>> {
    let key: OutlineKey = (
        points.iter().flat_map(|p| [p.x, p.y, p.in_dx, p.in_dy, p.out_dx, p.out_dy].map(f32::to_bits)).collect(),
        width,
        height,
    );
    if let Some(hit) = cache().lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return hit.clone();
    }
    let aspect = height.max(1) as f32 / width.max(1) as f32;
    let flat = Arc::new(flatten(points, OUTLINE_TOLERANCE_PX / width.max(1) as f32, aspect));
    let mut cache = cache().lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() >= CACHE_ENTRIES {
        cache.clear();
    }
    cache.insert(key, flat.clone());
    flat
}

/// Whether [`outline`] keeps this path's outline at this size.
#[cfg(test)]
fn cached(points: &[BezierPoint], width: u32, height: u32) -> bool {
    let key: OutlineKey = (points.iter().flat_map(|p| [p.x, p.y, p.in_dx, p.in_dy, p.out_dx, p.out_dy].map(f32::to_bits)).collect(), width, height);
    cache().lock().unwrap().contains_key(&key)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A circle of radius `r` about the centre as four cubic arcs, the
    /// usual k = 0.5523 handles, on a square frame.
    fn circle(r: f32) -> Vec<BezierPoint> {
        let k = 0.552_284_8 * r;
        let p = |x: f32, y: f32, ix: f32, iy: f32| BezierPoint { x: 0.5 + x, y: 0.5 + y, in_dx: ix, in_dy: iy, out_dx: -ix, out_dy: -iy, smooth: true };
        vec![p(r, 0.0, 0.0, -k), p(0.0, r, k, 0.0), p(-r, 0.0, 0.0, k), p(0.0, -r, -k, 0.0)]
    }

    /// The flattened circle stays on the true one within the tolerance
    /// (and the four-arc circle's own 0.03 % error), and is finer than its
    /// four points.
    #[test]
    fn a_curve_flattens_within_its_tolerance() {
        let r = 0.3;
        let tol = 0.25 / 1920.0;
        let flat = flatten(&circle(r), tol, 1.0);
        assert!(flat.len() > 16, "{}", flat.len());
        let worst = flat.iter().map(|&(x, y)| (((x - 0.5).powi(2) + (y - 0.5).powi(2)).sqrt() - r).abs()).fold(0.0, f32::max);
        assert!(worst < tol + r * 3e-4, "{worst}");
        // And between the points too: each chord's middle.
        for i in 0..flat.len() {
            let (a, b) = (flat[i], flat[(i + 1) % flat.len()]);
            let m = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
            let off = r - ((m.0 - 0.5).powi(2) + (m.1 - 0.5).powi(2)).sqrt();
            assert!(off < tol * 1.5 + r * 3e-4, "chord {i} sags {off}");
        }
    }

    /// A path of corners is its own polygon, point for point.
    #[test]
    fn corners_flatten_to_themselves() {
        let pts: Vec<BezierPoint> = [(0.1, 0.2), (0.8, 0.25), (0.5, 0.9)].into_iter().map(BezierPoint::from).collect();
        assert_eq!(flatten(&pts, 1e-4, 0.5625), vec![(0.1, 0.2), (0.8, 0.25), (0.5, 0.9)]);
    }

    /// A corner is saved as `[x, y]`, as polygons always were, and reads
    /// back; a curved point saves its handles and reads back whole.
    #[test]
    fn points_save_as_before_and_with_handles() {
        let corner = BezierPoint::corner(0.25, 0.5);
        assert_eq!(serde_json::to_string(&corner).unwrap(), "[0.25,0.5]");
        let read: BezierPoint = serde_json::from_str("[0.25,0.5]").unwrap();
        assert_eq!(read, corner);
        let curved = BezierPoint { x: 0.5, y: 0.5, in_dx: -0.1, in_dy: 0.0, out_dx: 0.1, out_dy: 0.0, smooth: true };
        let text = serde_json::to_string(&curved).unwrap();
        assert_eq!(serde_json::from_str::<BezierPoint>(&text).unwrap(), curved, "{text}");
        let one_handle = BezierPoint { out_dx: 0.2, ..BezierPoint::corner(0.1, 0.1) };
        let text = serde_json::to_string(&one_handle).unwrap();
        assert!(!text.contains("\"in\"") && !text.contains("smooth"), "{text}");
        assert_eq!(serde_json::from_str::<BezierPoint>(&text).unwrap(), one_handle);
    }

    /// The outline is kept by path and size: asked again it is the same
    /// one, not flattened again; another size is another outline.
    #[test]
    fn an_outline_is_kept_for_the_same_path_and_size() {
        let pts = circle(0.21);
        let a = outline(&pts, 1280, 720);
        assert!(cached(&pts, 1280, 720));
        assert!(Arc::ptr_eq(&a, &outline(&pts, 1280, 720)));
        assert!(!cached(&pts, 640, 360));
        let b = outline(&pts, 640, 360);
        assert!(b.len() < a.len(), "coarser at a smaller size: {} {}", b.len(), a.len());
    }

    /// No outline grows past its cap, however fine the tolerance.
    #[test]
    fn an_outline_is_capped() {
        let pts: Vec<BezierPoint> = (0..400).map(|i| {
            let t = i as f32 / 400.0 * std::f32::consts::TAU;
            BezierPoint { x: 0.5 + 0.4 * t.cos(), y: 0.5 + 0.4 * t.sin(), out_dx: 0.05, out_dy: -0.05, in_dx: -0.05, in_dy: 0.05, smooth: false }
        }).collect();
        assert!(flatten(&pts, 1e-7, 1.0).len() <= MAX_OUTLINE_POINTS);
    }

    /// A stack of polygons saves as it did before curves, field for
    /// field, and reads back; a curved mask with the new settings keeps
    /// them.
    #[test]
    fn a_polygon_mask_saves_as_before() {
        let before = r#"{"masks":[{"shape":{"Bezier":{"points":[[0.1,0.2],[0.8,0.25],[0.5,0.9]]}},"feather":0.1,"invert":false,"opacity":1.0,"enabled":true,"keyframes":{"center_x":[],"center_y":[],"radius":[],"width":[],"height":[],"rotation":[],"feather":[],"opacity":[],"start_x":[],"start_y":[],"end_x":[],"end_y":[]}}]}"#;
        let stack: crate::MaskStack = serde_json::from_str(before).unwrap();
        assert_eq!(serde_json::to_string(&stack).unwrap(), before);
        let mut curved = stack.clone();
        curved.masks[0].feather_inner = 0.05;
        curved.masks[0].combine = crate::MaskCombine::Subtract;
        if let crate::MaskShape::Bezier { points } = &mut curved.masks[0].shape {
            points[1].out_dx = 0.1;
            points[1].smooth = true;
        }
        let text = serde_json::to_string(&curved).unwrap();
        assert_eq!(serde_json::from_str::<crate::MaskStack>(&text).unwrap(), curved, "{text}");
    }

    fn sq(off: f32) -> Vec<BezierPoint> {
        [(0.2, 0.2), (0.6, 0.2), (0.6, 0.6), (0.2, 0.6)].into_iter().map(|(x, y)| BezierPoint::corner(x + off, y)).collect()
    }

    fn key(t: f64, off: f32, ease: PathEase) -> PathKey {
        PathKey { time_secs: t, points: sq(off), ease }
    }

    /// #74: a keyed shape is held before its first key and after its
    /// last, moves evenly between two Linear keys, slowly at the ends of
    /// an Ease, and not at all from a Hold.
    #[test]
    fn a_keyed_shape_moves_between_its_keys() {
        let keys = vec![key(1.0, 0.0, PathEase::Linear), key(3.0, 0.2, PathEase::Hold), key(5.0, 0.0, PathEase::Linear)];
        assert_eq!(shape_at(&keys, 0.0).unwrap()[0].x, 0.2);
        assert!((shape_at(&keys, 2.0).unwrap()[0].x - 0.3).abs() < 1e-6, "halfway");
        assert_eq!(shape_at(&keys, 4.5).unwrap()[0].x, 0.4, "held");
        assert_eq!(shape_at(&keys, 9.0).unwrap()[0].x, 0.2);
        let eased = vec![key(0.0, 0.0, PathEase::Ease), key(1.0, 0.4, PathEase::Linear)];
        let early = shape_at(&eased, 0.1).unwrap()[0].x - 0.2;
        assert!(early < 0.4 * 0.1, "slow out of an ease: {early}");
        assert!((shape_at(&eased, 0.5).unwrap()[0].x - 0.4).abs() < 1e-6);
        assert!(shape_at(&[], 1.0).is_none());
        // Handles move with the points.
        let mut a = key(0.0, 0.0, PathEase::Linear);
        a.points[0].out_dx = 0.1;
        let b = key(2.0, 0.0, PathEase::Linear);
        assert!((shape_at(&[a, b], 1.0).unwrap()[0].out_dx - 0.05).abs() < 1e-6);
    }

    /// A key is changed where there is one, else added in time order.
    #[test]
    fn a_key_is_set_where_one_is_or_added_in_order() {
        let keys = vec![key(1.0, 0.0, PathEase::Ease), key(3.0, 0.2, PathEase::Linear)];
        let set = with_key(&keys, 1.01, sq(0.1), 0.02);
        assert_eq!(set.len(), 2);
        assert_eq!((set[0].points[0].x, set[0].ease), (0.3, PathEase::Ease), "changed, its ease kept");
        let added = with_key(&keys, 2.0, sq(0.1), 0.02);
        assert_eq!(added.iter().map(|k| k.time_secs).collect::<Vec<_>>(), [1.0, 2.0, 3.0]);
    }

    /// A keyed mask saves its keys and reads them back; Linear isn't
    /// written.
    #[test]
    fn shape_keys_save_and_read_back() {
        let mut m = crate::Mask { shape: crate::MaskShape::Bezier { points: sq(0.0) }, ..crate::Mask::default() };
        m.keyframes.path = vec![key(0.5, 0.0, PathEase::Linear), key(1.5, 0.1, PathEase::Hold)];
        let text = serde_json::to_string(&m).unwrap();
        assert!(text.contains("\"path\"") && text.contains("Hold") && !text.contains("Linear"), "{text}");
        assert_eq!(serde_json::from_str::<crate::Mask>(&text).unwrap(), m);
    }

    /// A point added or taken away goes into every key alike, each keeping
    /// its shape and as many points as the others; keying on keeps the
    /// shape as drawn, keying off keeps it as it is at that moment.
    #[test]
    fn points_come_and_go_in_every_key() {
        let mut m = crate::Mask { shape: crate::MaskShape::Bezier { points: sq(0.0) }, ..crate::Mask::default() };
        m.keyframes.path = vec![key(0.0, 0.0, PathEase::Linear), key(2.0, 0.2, PathEase::Linear)];
        let split = m.with_point_split(2, 0.5);
        assert!(split.keyframes.path.iter().all(|k| k.points.len() == 5));
        assert_eq!(split.keyframes.path[1].points[3].pos(), (0.6, 0.6), "on the key's own side: (0.8+0.4)/2");
        assert_eq!(split.path_at(1.0).unwrap().len(), 5);
        let fewer = split.without_path_points(&[3].into()).unwrap();
        assert!(fewer.keyframes.path.iter().all(|k| k.points == sq(k.time_secs as f32 / 10.0)));
        assert!(m.without_path_points(&[0, 1].into()).is_none(), "two left");

        let on = crate::Mask { shape: crate::MaskShape::Bezier { points: sq(0.1) }, ..crate::Mask::default() }.with_path_keyed(true, 1.5);
        assert_eq!(on.keyframes.path, vec![key(1.5, 0.1, PathEase::Linear)]);
        let off = m.with_path_keyed(false, 1.0);
        assert!(off.keyframes.path.is_empty());
        assert_eq!(off.path_at(9.0).unwrap()[0].x, 0.3, "the shape as it was at 1 s");
        // Edited at a moment: the key there, or a new one.
        let edited = m.with_path_at(1.0, sq(0.05), 0.02);
        assert_eq!(edited.keyframes.path.len(), 3);
        assert_eq!(edited.evaluated_at(1.0).path_at(1.0).unwrap()[0].x, 0.25);
    }
}
