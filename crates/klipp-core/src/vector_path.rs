//! Editable cubic paths. Coordinates are fractions of the object's box;
//! contour/vertex order is the explicit morph correspondence.
use crate::title_motion::Easing;
use serde::{Deserialize, Serialize};

pub const MAX_PATH_VERTICES: usize = 256;
pub const MAX_PATH_CONTOURS: usize = 16;
pub const MAX_PATH_KEYS: usize = 256;
fn linear_easing() -> Easing {
    Easing::Linear
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PathVertex {
    pub point: [f32; 2],
    /// Absolute handle positions in the same normalized box as `point`.
    pub incoming: [f32; 2],
    pub outgoing: [f32; 2],
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PathContour {
    pub closed: bool,
    pub vertices: Vec<PathVertex>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PathPose {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke_glow: Option<StrokeGlow>,
    pub contours: Vec<PathContour>,
    /// Straight RGBA in authored sRGB, 0..1. Alpha zero disables paint.
    pub fill: [f32; 4],
    pub stroke: [f32; 4],
    /// Width in pixels at 1080p, scaled with output resolution.
    pub stroke_width: f32,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PathKeyframe {
    pub time_secs: f64,
    pub pose: PathPose,
    /// Easing from this key to the next; topology never changes between keys.
    #[serde(default = "linear_easing")]
    pub easing: Easing,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct VectorPath {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trim: Option<PathTrim>,
    pub pose: PathPose,
    #[serde(default)]
    pub keyframes: Vec<PathKeyframe>,
}
impl Default for VectorPath {
    fn default() -> Self {
        let vertices = [[0.1, 0.1], [0.9, 0.1], [0.9, 0.9], [0.1, 0.9]]
            .into_iter()
            .map(|point| PathVertex {
                point,
                incoming: point,
                outgoing: point,
            })
            .collect();
        Self {
            trim: None,
            pose: PathPose {
                stroke_glow: None,
                contours: vec![PathContour {
                    closed: true,
                    vertices,
                }],
                fill: [0.2, 0.6, 1.0, 1.0],
                stroke: [1.0; 4],
                stroke_width: 2.0,
            },
            keyframes: vec![],
        }
    }
}

/// A ready-made shape for a title's shape layer (#67): drawn as a path
/// filling the layer's box, so it rotates, scales, strokes and morphs as
/// any path does.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShapePreset {
    Ellipse,
    Triangle,
    /// A regular polygon of `sides` (3 to 64), a point at the top.
    Polygon { sides: u32 },
    /// A star of `points` (3 to 64); `inner` is the inner radius as a
    /// fraction of the outer (0.1 to 0.95).
    Star { points: u32, inner: f32 },
    /// An arrow pointing right, the shaft a third of the box's height.
    Arrow,
    /// A straight line across the middle of the box: stroke only.
    Line,
}

/// A corner: both handles on the point.
fn corner(x: f32, y: f32) -> PathVertex {
    PathVertex { point: [x, y], incoming: [x, y], outgoing: [x, y] }
}

/// Points round the box's centre, the first at the top, clockwise on
/// screen: `radius(k)` is point `k`'s distance as a fraction of the half box.
fn ring(n: usize, radius: impl Fn(usize) -> f32) -> Vec<PathVertex> {
    (0..n)
        .map(|k| {
            let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * k as f32 / n as f32;
            let r = radius(k) * 0.5;
            corner(0.5 + r * a.cos(), 0.5 + r * a.sin())
        })
        .collect()
}

impl VectorPath {
    /// `preset` as a path in a layer's box, painted with `fill` and
    /// `stroke` (straight RGBA 0..1) at `stroke_width` pixels of 1080.
    /// A line has no fill.
    pub fn preset(preset: ShapePreset, fill: [f32; 4], stroke: [f32; 4], stroke_width: f32) -> Self {
        // A circle from four cubic arcs: each handle this far along the
        // tangent, as a fraction of the radius.
        const KAPPA: f32 = 0.552_284_8;
        let (closed, vertices) = match preset {
            ShapePreset::Ellipse => {
                let h = 0.5 * KAPPA;
                let v = |p: [f32; 2], inc: [f32; 2], out: [f32; 2]| PathVertex { point: p, incoming: inc, outgoing: out };
                (
                    true,
                    vec![
                        v([0.5, 0.0], [0.5 - h, 0.0], [0.5 + h, 0.0]),
                        v([1.0, 0.5], [1.0, 0.5 - h], [1.0, 0.5 + h]),
                        v([0.5, 1.0], [0.5 + h, 1.0], [0.5 - h, 1.0]),
                        v([0.0, 0.5], [0.0, 0.5 + h], [0.0, 0.5 - h]),
                    ],
                )
            }
            ShapePreset::Triangle => (true, vec![corner(0.5, 0.0), corner(1.0, 1.0), corner(0.0, 1.0)]),
            ShapePreset::Polygon { sides } => (true, ring(sides.clamp(3, 64) as usize, |_| 1.0)),
            ShapePreset::Star { points, inner } => {
                let inner = if inner.is_finite() { inner.clamp(0.1, 0.95) } else { 0.45 };
                (true, ring(2 * points.clamp(3, 64) as usize, |k| if k % 2 == 0 { 1.0 } else { inner }))
            }
            ShapePreset::Arrow => (
                true,
                vec![
                    corner(0.0, 0.333),
                    corner(0.6, 0.333),
                    corner(0.6, 0.0),
                    corner(1.0, 0.5),
                    corner(0.6, 1.0),
                    corner(0.6, 0.667),
                    corner(0.0, 0.667),
                ],
            ),
            ShapePreset::Line => (false, vec![corner(0.0, 0.5), corner(1.0, 0.5)]),
        };
        let fill = if preset == ShapePreset::Line { [0.0; 4] } else { fill };
        Self {
            trim: None,
            pose: PathPose { stroke_glow: None, contours: vec![PathContour { closed, vertices }], fill, stroke, stroke_width },
            keyframes: Vec::new(),
        }
    }
}

impl VectorPath {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(t) = &self.trim {
            t.validate()?;
        }
        if self.keyframes.len() > MAX_PATH_KEYS {
            return Err("a path supports at most 256 morph keys".into());
        }
        self.pose.validate()?;
        let mut previous = None;
        for k in &self.keyframes {
            if !k.time_secs.is_finite()
                || k.time_secs < 0.0
                || previous.is_some_and(|p| k.time_secs - p < 1e-6)
            {
                return Err(
                    "path key times must be finite, nonnegative, sorted and distinct".into(),
                );
            }
            k.pose.validate()?;
            if !self.pose.same_topology(&k.pose) {
                return Err("morph topology mismatch: contour order, closed flags and vertex counts must match; resample explicitly before authoring".into());
            }
            previous = Some(k.time_secs);
        }
        Ok(())
    }
    pub fn at(&self, time: f64) -> PathPose {
        if self.keyframes.is_empty() {
            return self.pose.clone();
        }
        let next = self.keyframes.partition_point(|k| k.time_secs <= time);
        if next == 0 {
            return self.keyframes[0].pose.clone();
        }
        let a = &self.keyframes[next - 1];
        let Some(b) = self.keyframes.get(next) else {
            return a.pose.clone();
        };
        if !a.pose.same_topology(&b.pose) {
            return a.pose.clone();
        }
        let t = a
            .easing
            .apply(((time - a.time_secs) / (b.time_secs - a.time_secs)) as f32);
        a.pose.interpolate(&b.pose, t)
    }
    /// Key insertion preserves existing easing; topology changes require clearing keys.
    pub fn key(&mut self, time: f64, pose: PathPose) -> Result<(), String> {
        let mut candidate = self.clone();
        if let Some(k) = candidate
            .keyframes
            .iter_mut()
            .find(|k| (k.time_secs - time).abs() < 1e-6)
        {
            k.pose = pose;
        } else {
            candidate.keyframes.push(PathKeyframe {
                time_secs: time,
                pose,
                easing: Easing::Linear,
            });
            candidate
                .keyframes
                .sort_by(|a, b| a.time_secs.total_cmp(&b.time_secs));
        }
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }
}
impl PathPose {
    pub fn same_topology(&self, other: &Self) -> bool {
        self.contours.len() == other.contours.len()
            && self
                .contours
                .iter()
                .zip(&other.contours)
                .all(|(a, b)| a.closed == b.closed && a.vertices.len() == b.vertices.len())
    }
    pub fn validate(&self) -> Result<(), String> {
        if let Some(g) = self.stroke_glow {
            if !g.radius.is_finite()
                || !(0.0..=200.0).contains(&g.radius)
                || !g.strength.is_finite()
                || !(0.0..=1.0).contains(&g.strength)
            {
                return Err("stroke glow needs radius 0..200 and strength 0..1".into());
            }
        }
        if self.contours.is_empty() || self.contours.len() > MAX_PATH_CONTOURS {
            return Err("a path needs 1..16 contours".into());
        }
        let count: usize = self.contours.iter().map(|c| c.vertices.len()).sum();
        if count > MAX_PATH_VERTICES || self.contours.iter().any(|c| c.vertices.len() < 2) {
            return Err("contours need at least two vertices, at most 256 total".into());
        }
        if self
            .contours
            .iter()
            .flat_map(|c| &c.vertices)
            .flat_map(|v| [v.point, v.incoming, v.outgoing])
            .flatten()
            .any(|v| !v.is_finite() || !(-4.0..=5.0).contains(&v))
        {
            return Err(
                "path points and handles must be finite normalized coordinates in -4..5".into(),
            );
        }
        if self
            .fill
            .iter()
            .chain(&self.stroke)
            .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
            || !self.stroke_width.is_finite()
            || !(0.0..=200.0).contains(&self.stroke_width)
        {
            return Err("path paint needs RGBA 0..1 and stroke width 0..200".into());
        }
        Ok(())
    }
    fn interpolate(&self, b: &Self, t: f32) -> Self {
        let lerp = |a: f32, b: f32| a + (b - a) * t;
        let point = |a: [f32; 2], b: [f32; 2]| [lerp(a[0], b[0]), lerp(a[1], b[1])];
        let mut out = self.clone();
        for (c, d) in out.contours.iter_mut().zip(&b.contours) {
            for (v, w) in c.vertices.iter_mut().zip(&d.vertices) {
                v.point = point(v.point, w.point);
                v.incoming = point(v.incoming, w.incoming);
                v.outgoing = point(v.outgoing, w.outgoing);
            }
        }
        for i in 0..4 {
            out.fill[i] = lerp(self.fill[i], b.fill[i]).clamp(0.0, 1.0);
            out.stroke[i] = lerp(self.stroke[i], b.stroke[i]).clamp(0.0, 1.0);
        }
        out.stroke_width = lerp(self.stroke_width, b.stroke_width).clamp(0.0, 200.0);
        out.stroke_glow = if self.stroke_glow.is_some() || b.stroke_glow.is_some() {
            let a = self.stroke_glow.unwrap_or_default();
            let b = b.stroke_glow.unwrap_or_default();
            Some(StrokeGlow {
                radius: lerp(a.radius, b.radius).clamp(0.0, 200.0),
                strength: lerp(a.strength, b.strength).clamp(0.0, 1.0),
            })
        } else {
            None
        };
        out
    }
}

/// Stroke-local halo paint, keyed with the pose independently of trim.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StrokeGlow {
    pub radius: f32,
    pub strength: f32,
}

/// Stroke-only reveal, independently keyed from the path's morph poses.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TrimRange {
    pub start: f32,
    pub end: f32,
    #[serde(default)]
    pub offset: f32,
}
impl Default for TrimRange {
    fn default() -> Self {
        Self {
            start: 0.0,
            end: 1.0,
            offset: 0.0,
        }
    }
}
impl TrimRange {
    fn validate(self) -> Result<(), String> {
        if !self.start.is_finite()
            || !self.end.is_finite()
            || !self.offset.is_finite()
            || !(0.0..=1.0).contains(&self.start)
            || !(self.start..=1.0).contains(&self.end)
            || !(-100.0..=100.0).contains(&self.offset)
        {
            return Err("trim needs 0 <= start <= end <= 1 and finite offset in -100..100".into());
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TrimKeyframe {
    pub time_secs: f64,
    pub range: TrimRange,
    #[serde(default = "linear_easing")]
    pub easing: Easing,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PathTrim {
    #[serde(default)]
    pub range: TrimRange,
    #[serde(default)]
    pub keyframes: Vec<TrimKeyframe>,
}
impl PathTrim {
    pub fn validate(&self) -> Result<(), String> {
        self.range.validate()?;
        if self.keyframes.len() > MAX_PATH_KEYS {
            return Err("trim supports at most 256 keys".into());
        }
        let mut previous = None;
        for key in &self.keyframes {
            key.range.validate()?;
            if !key.time_secs.is_finite()
                || key.time_secs < 0.0
                || previous.is_some_and(|p| key.time_secs - p < 1e-6)
            {
                return Err(
                    "trim key times must be finite, nonnegative, sorted and distinct".into(),
                );
            }
            previous = Some(key.time_secs);
        }
        Ok(())
    }
    pub fn at(&self, time: f64) -> TrimRange {
        if self.keyframes.is_empty() {
            return self.range;
        }
        let next = self.keyframes.partition_point(|k| k.time_secs <= time);
        if next == 0 {
            return self.keyframes[0].range;
        }
        let a = &self.keyframes[next - 1];
        let Some(b) = self.keyframes.get(next) else {
            return a.range;
        };
        let t = a
            .easing
            .apply(((time - a.time_secs) / (b.time_secs - a.time_secs)) as f32)
            .clamp(0.0, 1.0);
        let lerp = |a: f32, b: f32| a + (b - a) * t;
        TrimRange {
            start: lerp(a.range.start, b.range.start),
            end: lerp(a.range.end, b.range.end),
            offset: lerp(a.range.offset, b.range.offset),
        }
    }
    pub fn key(&mut self, time: f64, range: TrimRange) -> Result<(), String> {
        let mut next = self.clone();
        if let Some(k) = next
            .keyframes
            .iter_mut()
            .find(|k| (k.time_secs - time).abs() < 1e-6)
        {
            k.range = range;
        } else {
            next.keyframes.push(TrimKeyframe {
                time_secs: time,
                range,
                easing: Easing::Linear,
            });
            next.keyframes
                .sort_by(|a, b| a.time_secs.total_cmp(&b.time_secs));
        }
        next.validate()?;
        *self = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #67: every ready-made shape is a valid path inside its box, a
    /// closed outline except the line, with the vertex count it promises.
    #[test]
    fn presets_are_valid_paths_in_their_box() {
        use super::ShapePreset::*;
        for (preset, count, closed) in [
            (Ellipse, 4, true),
            (Triangle, 3, true),
            (Polygon { sides: 6 }, 6, true),
            (Polygon { sides: 1 }, 3, true),
            (Star { points: 5, inner: 0.4 }, 10, true),
            (Arrow, 7, true),
            (Line, 2, false),
        ] {
            let p = VectorPath::preset(preset, [1.0; 4], [0.0, 0.0, 0.0, 1.0], 2.0);
            p.validate().unwrap_or_else(|e| panic!("{preset:?}: {e}"));
            let c = &p.pose.contours[0];
            assert_eq!((c.vertices.len(), c.closed), (count, closed), "{preset:?}");
            for v in &c.vertices {
                for xy in [v.point, v.incoming, v.outgoing] {
                    assert!(xy.iter().all(|c| (-1e-4..=1.0 + 1e-4).contains(c)), "{preset:?} leaves its box at {xy:?}");
                }
            }
        }
        assert_eq!(VectorPath::preset(Line, [1.0; 4], [1.0; 4], 2.0).pose.fill[3], 0.0, "a line is not filled");
        // A star alternates between the outer and the inner radius.
        let star = VectorPath::preset(Star { points: 5, inner: 0.4 }, [1.0; 4], [1.0; 4], 0.0);
        let r = |v: &PathVertex| ((v.point[0] - 0.5).powi(2) + (v.point[1] - 0.5).powi(2)).sqrt();
        let v = &star.pose.contours[0].vertices;
        assert!((r(&v[0]) - 0.5).abs() < 1e-5 && (r(&v[1]) - 0.2).abs() < 1e-5);
        assert!(v[0].point[1].abs() < 1e-6, "its first point is at the top");
    }

    #[test]
    fn static_glow_and_keyed_glow_do_not_require_trim() {
        let mut p = VectorPath::default();
        p.pose.stroke_glow = Some(StrokeGlow {
            radius: 10.0,
            strength: 0.2,
        });
        p.key(0.0, p.pose.clone()).unwrap();
        let mut end = p.pose.clone();
        end.stroke_glow = Some(StrokeGlow {
            radius: 30.0,
            strength: 0.8,
        });
        p.key(1.0, end).unwrap();
        assert!(p.trim.is_none());
        let g = p.at(0.5).stroke_glow.unwrap();
        assert_eq!(g.radius, 20.0);
        assert!((g.strength - 0.5).abs() < 1e-6);
        let mut t = PathTrim::default();
        t.key(
            0.0,
            TrimRange {
                end: 0.0,
                ..Default::default()
            },
        )
        .unwrap();
        t.key(1.0, TrimRange::default()).unwrap();
        assert_eq!(t.at(0.5).end, 0.5);
        t.keyframes[0].easing = Easing::OutCubic;
        t.key(
            0.0,
            TrimRange {
                end: 0.0,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(t.keyframes[0].easing, Easing::OutCubic);
        assert!(t
            .key(
                0.0,
                TrimRange {
                    start: 0.8,
                    end: 0.2,
                    offset: 0.0
                }
            )
            .is_err());
    }
    #[test]
    fn overshoot_moves_geometry_but_keeps_paint_valid() {
        let mut p = VectorPath::default();
        let mut end = p.pose.clone();
        end.contours[0].vertices[0].point = [0.5, 0.5];
        end.fill = [1.0; 4];
        p.key(0.0, p.pose.clone()).unwrap();
        p.key(1.0, end).unwrap();
        p.keyframes[0].easing = Easing::OutBack;
        let pose = p.at(0.7);
        assert!(pose.contours[0].vertices[0].point[0] > 0.5);
        assert!(pose.fill.iter().all(|v| (0.0..=1.0).contains(v)));
    }
    #[test]
    fn morphs_points_handles_and_paint_with_explicit_correspondence() {
        let mut p = VectorPath::default();
        let start = p.pose.clone();
        let mut end = start.clone();
        end.contours[0].vertices[0].point = [0.3, 0.4];
        end.contours[0].vertices[0].outgoing = [0.5, 0.2];
        end.fill = [1.0, 0.0, 0.0, 0.5];
        end.stroke_width = 10.0;
        p.key(0.0, start).unwrap();
        p.key(2.0, end).unwrap();
        let mid = p.at(1.0);
        assert!((mid.contours[0].vertices[0].point[0] - 0.2).abs() < 1e-6);
        assert_eq!(mid.stroke_width, 6.0);
        assert!((mid.fill[3] - 0.75).abs() < 1e-6);
        p.keyframes[0].easing = Easing::OutCubic;
        assert!(p.at(1.0).stroke_width > 6.0);
    }
    #[test]
    fn rejects_mismatched_topology_and_nonfinite_geometry_without_mutation() {
        let mut p = VectorPath::default();
        let before = p.clone();
        let mut bad = p.pose.clone();
        bad.contours[0].vertices.pop();
        assert!(p.key(1.0, bad).is_err());
        assert_eq!(p, before);
        let mut bad = p.pose.clone();
        bad.contours[0].closed = false;
        assert!(p.key(1.0, bad).is_err());
        let mut bad = p.pose.clone();
        bad.contours[0].vertices[0].incoming[0] = f32::NAN;
        assert!(p.key(1.0, bad).is_err());
    }
}
