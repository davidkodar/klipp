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
            pose: PathPose {
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
impl VectorPath {
    pub fn validate(&self) -> Result<(), String> {
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
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overshoot_moves_geometry_but_keeps_paint_valid() {
        let mut p=VectorPath::default();let mut end=p.pose.clone();end.contours[0].vertices[0].point=[0.5,0.5];end.fill=[1.0;4];
        p.key(0.0,p.pose.clone()).unwrap();p.key(1.0,end).unwrap();p.keyframes[0].easing=Easing::OutBack;
        let pose=p.at(0.7);assert!(pose.contours[0].vertices[0].point[0]>0.5);assert!(pose.fill.iter().all(|v|(0.0..=1.0).contains(v)));
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
