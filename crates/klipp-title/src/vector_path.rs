//! Resolution-adaptive cubic rasterization. Closed contours use even-odd
//! fill; open contours are stroke-only. Strokes have round caps and joins.
use klipp_core::fill_gradient::{GradientKind, GradientRamp};
use klipp_timeline::{ObjectTransform, PathPose, TrimRange};
/// Resolve path keys once before selecting the cached or animated title route.
/// The derived value has no path keys, so re-entry terminates. A fully revealed
/// closed contour is independent of trim offset; open or mixed contours are not.
pub(crate) fn evaluated_title(title: &klipp_timeline::TitleData, time: f64) -> Option<klipp_timeline::TitleData> {
    use klipp_timeline::{TitleObjectContent, ShapeKind};
    if !keys_path_geometry(title) {
        return None;
    }
    let mut evaluated = title.clone();
    for obj in &mut evaluated.objects {
        let TitleObjectContent::Shape(s) = &mut obj.content else { continue };
        let ShapeKind::Path(p) = &mut s.kind else { continue };
        p.pose = p.at(time);
        p.keyframes.clear();
        if let Some(trim) = &mut p.trim {
            trim.range = trim.at(time);
            trim.keyframes.clear();
            if trim.range.end - trim.range.start >= 1.0
                && p.pose.contours.iter().all(|c| c.closed) {
                trim.range = TrimRange::default();
            }
        }
    }
    Some(evaluated)
}

/// Whether a path in `title` has morph or trim keys, so its geometry, and
/// with it the resting raster, changes over the clip (#2201).
pub fn keys_path_geometry(title: &klipp_timeline::TitleData) -> bool {
    use klipp_timeline::{TitleObjectContent, ShapeKind};
    let keyed = |p: &klipp_timeline::VectorPath| !p.keyframes.is_empty()
        || p.trim.as_ref().is_some_and(|t| !t.keyframes.is_empty());
    title.objects.iter().any(|o| matches!(&o.content,
        TitleObjectContent::Shape(s) if matches!(&s.kind, ShapeKind::Path(p) if keyed(p))))
}

fn midpoint(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5]
}
fn distance(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let d = [b[0] - a[0], b[1] - a[1]];
    let n = d[0] * d[0] + d[1] * d[1];
    let t = if n > 1e-12 {
        ((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1]) / n
    } else {
        0.0
    }
    .clamp(0.0, 1.0);
    ((p[0] - a[0] - t * d[0]).powi(2) + (p[1] - a[1] - t * d[1]).powi(2)).sqrt()
}
fn cubic(p: [[f32; 2]; 4], depth: u8, out: &mut Vec<[f32; 2]>) {
    if depth >= 12 || distance(p[1], p[0], p[3]).max(distance(p[2], p[0], p[3])) <= 0.2 {
        out.push(p[3]);
        return;
    }
    let a = midpoint(p[0], p[1]);
    let b = midpoint(p[1], p[2]);
    let c = midpoint(p[2], p[3]);
    let d = midpoint(a, b);
    let e = midpoint(b, c);
    let m = midpoint(d, e);
    cubic([p[0], a, d, m], depth + 1, out);
    cubic([m, e, c, p[3]], depth + 1, out);
}
pub(crate) fn contours(
    pose: &PathPose,
    transform: &ObjectTransform,
    width: u32,
    height: u32,
) -> Vec<(bool, Vec<[f32; 2]>)> {
    let size = [
        transform.size.x * width as f32,
        transform.size.y * height as f32,
    ];
    let anchor = [transform.anchor_norm.x, transform.anchor_norm.y];
    let pivot = [
        transform.center.x * width as f32 + (anchor[0] - 0.5) * size[0],
        transform.center.y * height as f32 + (anchor[1] - 0.5) * size[1],
    ];
    let (sin, cos) = transform.rotation.to_radians().sin_cos();
    let map = |p: [f32; 2]| {
        let x = (p[0] - anchor[0]) * size[0];
        let y = (p[1] - anchor[1]) * size[1];
        [pivot[0] + cos * x - sin * y, pivot[1] + sin * x + cos * y]
    };
    pose.contours
        .iter()
        .filter(|c| c.vertices.len() >= 2)
        .map(|c| {
            let mut points = vec![map(c.vertices[0].point)];
            let n = if c.closed {
                c.vertices.len()
            } else {
                c.vertices.len() - 1
            };
            for i in 0..n {
                let a = &c.vertices[i];
                let b = &c.vertices[(i + 1) % c.vertices.len()];
                cubic(
                    [map(a.point), map(a.outgoing), map(b.incoming), map(b.point)],
                    0,
                    &mut points,
                );
            }
            (c.closed, points)
        })
        .collect()
}
/// Where a frame pixel falls in the object's box, as box fractions
/// (`contours`' mapping, undone), and the box's size in pixels.
fn box_fractions(transform: &ObjectTransform, width: u32, height: u32) -> (impl Fn(f32, f32) -> (f32, f32), [f32; 2]) {
    let size = [
        (transform.size.x * width as f32).max(1.0),
        (transform.size.y * height as f32).max(1.0),
    ];
    let anchor = [transform.anchor_norm.x, transform.anchor_norm.y];
    let pivot = [
        transform.center.x * width as f32 + (anchor[0] - 0.5) * size[0],
        transform.center.y * height as f32 + (anchor[1] - 0.5) * size[1],
    ];
    let (sin, cos) = transform.rotation.to_radians().sin_cos();
    let unmap = move |x: f32, y: f32| {
        let (dx, dy) = (x - pivot[0], y - pivot[1]);
        ((cos * dx + sin * dy) / size[0] + anchor[0], (cos * dy - sin * dx) / size[1] + anchor[1])
    };
    (unmap, size)
}
#[cfg(test)]
pub(crate) fn paint<C: super::Channel>(
    buf: &mut [C],
    pose: &PathPose,
    transform: &ObjectTransform,
    width: u32,
    height: u32,
    trim: Option<TrimRange>,
    gpu: Option<&mut dyn super::AlphaMaskProcessor>,
) {
    paint_filled(buf, pose, transform, width, height, trim, None, gpu)
}
/// A path's coverage as [`paint_filled`] scans it (#2201): the padded box
/// it paints, in frame pixels, and the closed contours' edges and the
/// revealed stroke segments in that box's pixels, so another rasteriser
/// given the same points covers the same samples.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PathCoverage {
    /// x0, y0, x1, y1.
    pub bounds: [u32; 4],
    /// a.x, a.y, b.x, b.y; empty when the fill is not drawn.
    pub fill_edges: Vec<[f32; 4]>,
    /// a.x, a.y, b.x, b.y; empty when the stroke is not drawn.
    pub stroke_segments: Vec<[f32; 4]>,
    /// The stroke's half width in pixels.
    pub half: f32,
    /// The pose's colours, 0 to 1.
    pub fill: [f32; 4],
    pub stroke: [f32; 4],
    /// The stroke glow's radius in whole pixels and its strength.
    pub glow: Option<(f32, f32)>,
}

/// What [`paint_filled`] paints of `pose`, or `None` when it paints
/// nothing. A `gradient` fill is drawn whatever the pose's fill alpha.
pub(crate) fn coverage(
    pose: &PathPose,
    transform: &ObjectTransform,
    width: u32,
    height: u32,
    trim: Option<TrimRange>,
    gradient: bool,
) -> Option<PathCoverage> {
    if width == 0 || height == 0 {
        return None;
    }
    let empty = trim.is_some_and(|r| r.end <= r.start);
    let fill_alpha = if gradient { 1.0 } else { pose.fill[3] };
    if fill_alpha <= 0.0 && (pose.stroke_width <= 0.0 || pose.stroke[3] <= 0.0 || empty) {
        return None;
    }
    let mut contours = contours(pose, transform, width, height);
    let half = pose.stroke_width * crate::frame_scale(width, height) * 0.5;
    let glow_radius = pose
        .stroke_glow
        .map_or(0.0, |g| g.radius * crate::frame_scale(width, height))
        .round();
    let glow_strength = pose.stroke_glow.map_or(0.0, |g| g.strength);
    // Bound all coverage and blending to the visible geometry.
    let mut lo = [f32::INFINITY; 2];
    let mut hi = [f32::NEG_INFINITY; 2];
    for p in contours.iter().flat_map(|(_, p)| p) {
        for axis in 0..2 {
            if !p[axis].is_finite() {
                return None;
            }
            lo[axis] = lo[axis].min(p[axis]);
            hi[axis] = hi[axis].max(p[axis]);
        }
    }
    let pad = if pose.stroke[3] > 0.0 {
        half + 1.0
            + if glow_strength > 0.0 {
                3.0 * glow_radius
            } else {
                0.0
            }
    } else {
        1.0
    };
    let x0 = (lo[0] - pad).floor().clamp(0.0, width as f32) as u32;
    let y0 = (lo[1] - pad).floor().clamp(0.0, height as f32) as u32;
    let x1 = (hi[0] + pad).ceil().clamp(0.0, width as f32) as u32;
    let y1 = (hi[1] + pad).ceil().clamp(0.0, height as f32) as u32;
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    for p in contours.iter_mut().flat_map(|(_, p)| p) {
        p[0] -= x0 as f32;
        p[1] -= y0 as f32;
    }
    let fill_edges = if fill_alpha > 0.0 {
        contours.iter().filter(|(closed, _)| *closed)
            .flat_map(|(_, points)| points.windows(2).map(|s| [s[0][0], s[0][1], s[1][0], s[1][1]]))
            .collect()
    } else {
        Vec::new()
    };
    let stroked = half > 0.0 && pose.stroke[3] > 0.0;
    let stroke_segments = if stroked {
        contours.iter()
            .flat_map(|(closed, points)| revealed_segments(points, *closed, trim.unwrap_or_default()))
            .map(|[a, b]| [a[0], a[1], b[0], b[1]])
            .collect()
    } else {
        Vec::new()
    };
    let glow = (stroked && glow_radius >= 1.0 && glow_strength > 0.0).then_some((glow_radius, glow_strength));
    Some(PathCoverage { bounds: [x0, y0, x1, y1], fill_edges, stroke_segments, half, fill: pose.fill, stroke: pose.stroke, glow })
}

/// What the title renderer paints of `path` in `transform` at `time`
/// without a gradient (#2201), or `None` when it paints nothing. The time
/// is the renderer's own, single precision, so the keys land alike.
pub fn path_coverage(path: &klipp_timeline::VectorPath, transform: &ObjectTransform, width: u32, height: u32, time: f32) -> Option<PathCoverage> {
    coverage(&path.at(time as f64), transform, width, height, path.trim.as_ref().map(|t| t.at(time as f64)), false)
}

/// [`paint`], with the fill taken from a gradient over the object's box
/// in place of the pose's fill colour when one is given (#1509).
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_filled<C: super::Channel>(
    buf: &mut [C],
    pose: &PathPose,
    transform: &ObjectTransform,
    width: u32,
    height: u32,
    trim: Option<TrimRange>,
    gradient: Option<&(GradientRamp, GradientKind)>,
    gpu: Option<&mut dyn super::AlphaMaskProcessor>,
) {
    let Some(cov) = coverage(pose, transform, width, height, trim, gradient.is_some()) else {
        return;
    };
    let (unmap, box_size) = box_fractions(transform, width, height);
    let [x0, y0, x1, y1] = cov.bounds;
    let frame_width = width;
    let (width, height) = (x1 - x0, y1 - y0);
    let count = width as usize * height as usize;
    let mut fill = vec![0u8; count];
    let mut stroke = vec![0u8; count];
    // Two-by-two coverage, scan-converted in output space. Fill work is
    // proportional to painted area + scanlines × edges, not pixels × edges.
    for sy in 0..if cov.fill_edges.is_empty() { 0 } else { height * 2 } {
        let y = (sy as f32 + 0.5) * 0.5;
        let mut crossings = Vec::new();
        for &[ax, ay, bx, by] in &cov.fill_edges {
            if (ay > y) != (by > y) {
                crossings.push(ax + (y - ay) * (bx - ax) / (by - ay));
            }
        }
        crossings.sort_by(f32::total_cmp);
        for pair in crossings.chunks_exact(2) {
            let start = (pair[0] * 2.0 - 0.5).ceil().max(0.0) as u32;
            let end = (pair[1] * 2.0 - 0.5)
                .ceil()
                .min((width * 2) as f32)
                .max(0.0) as u32;
            for sx in start..end {
                fill[((sy / 2) * width + sx / 2) as usize] |= 1 << ((sy % 2) * 2 + sx % 2);
            }
        }
    }
    if !cov.stroke_segments.is_empty() {
        let segments: Vec<_> = cov.stroke_segments.iter().map(|&[ax, ay, bx, by]| [[ax, ay], [bx, by]]).collect();
        stroke = stroke_coverage(&segments, width, height, cov.half);
    }
    let (glow_radius, glow_strength) = cov.glow.unwrap_or_default();
    let halo = if cov.glow.is_some() && stroke.iter().any(|bits| *bits != 0) {
        let mask: Vec<u8> = stroke.iter()
            .map(|bits| (bits.count_ones() as f32 * 0.25 * pose.stroke[3] * 255.0).round() as u8)
            .collect();
        Some(match gpu {
            // GPU blur accepts sigma; the three box radii match the CPU.
            Some(g) => g.blur(&mask, width, height, (glow_radius * (glow_radius + 1.0)).sqrt()),
            None => super::blur_alpha(&mask, width as usize, height as usize, glow_radius),
        })
    } else { None };
    // Rows are independent. Composite the halo then the stroke/fill at each
    // pixel, retaining the exact rounding and layer order of the serial path.
    // Empty interiors of outlined paths need neither colour math nor blending.
    use rayon::prelude::*;
    buf.par_chunks_exact_mut(frame_width as usize * 4)
        .skip(y0 as usize).take(height as usize).enumerate()
        .for_each(|(y, row)| {
            for (x, dst) in row[x0 as usize * 4..x1 as usize * 4].chunks_exact_mut(4).enumerate() {
                let i = y * width as usize + x;
                if let Some(alpha) = halo.as_ref().map(|h| h[i]).filter(|&a| a > 0) {
                    let src = [
                        (pose.stroke[0] * 255.0).round() as u8,
                        (pose.stroke[1] * 255.0).round() as u8,
                        (pose.stroke[2] * 255.0).round() as u8,
                        (alpha as f32 * glow_strength).round() as u8,
                    ];
                    super::over_blend_chunk(dst, &src);
                }
                let (f, s) = (fill[i], stroke[i]);
                if f == 0 && s == 0 { continue; }
                let fill_colour = match gradient {
                    Some((ramp, kind)) if f != 0 => {
                        let (u, v) = unmap((x0 as usize + x) as f32 + 0.5, (y0 as usize + y) as f32 + 0.5);
                        ramp.at(kind.position_at(u, v, box_size[0], box_size[1])).map(|c| c as f32 / 255.0)
                    }
                    _ => pose.fill,
                };
                let fa = f.count_ones() as f32 * 0.25 * fill_colour[3];
                let sa = s.count_ones() as f32 * 0.25 * pose.stroke[3];
                let overlap = (f & s).count_ones() as f32 * 0.25 * fill_colour[3] * pose.stroke[3];
                let alpha = fa + sa - overlap;
                if alpha > 0.0 {
                    let mut src = [0; 4];
                    for c in 0..3 {
                        src[c] = ((fill_colour[c] * (fa - overlap) + pose.stroke[c] * sa) / alpha * 255.0).round() as u8;
                    }
                    src[3] = (alpha * 255.0).round() as u8;
                    super::over_blend_chunk(dst, &src);
                }
            }
        });

}

/// Scan-convert the union of round-capped segments. Each scanline merges
/// analytic capsule intervals before filling coverage, so overlapping wide
/// segment boxes do not each visit every covered sample.
fn stroke_coverage(segments: &[[[f32; 2]; 2]], w: u32, h: u32, r: f32) -> Vec<u8> {
    let mut rows = vec![Vec::<(f32, f32)>::new(); (h * 2) as usize];
    for &[a, b] in segments {
        let dx = b[0] - a[0];
        let dy = b[1] - a[1];
        let len = dx.hypot(dy);
        let (nx, ny) = if len > 1e-6 {
            (-dy / len * r, dx / len * r)
        } else {
            (0.0, 0.0)
        };
        let corners = [
            [a[0] + nx, a[1] + ny],
            [b[0] + nx, b[1] + ny],
            [b[0] - nx, b[1] - ny],
            [a[0] - nx, a[1] - ny],
        ];
        let y0 = ((a[1].min(b[1]) - r) * 2.0 - 0.5)
            .ceil()
            .clamp(0.0, (h * 2) as f32) as usize;
        let y1 = ((a[1].max(b[1]) + r) * 2.0 - 0.5)
            .ceil()
            .clamp(0.0, (h * 2) as f32) as usize;
        for (sy, row) in rows.iter_mut().enumerate().take(y1).skip(y0) {
            let y = (sy as f32 + 0.5) * 0.5;
            let mut lo = f32::INFINITY;
            let mut hi = f32::NEG_INFINITY;
            for c in [a, b] {
                let d = y - c[1];
                if d.abs() <= r {
                    let x = (r * r - d * d).max(0.0).sqrt();
                    lo = lo.min(c[0] - x);
                    hi = hi.max(c[0] + x);
                }
            }
            for i in 0..4 {
                let p = corners[i];
                let q = corners[(i + 1) % 4];
                if (p[1] > y) != (q[1] > y) {
                    let x = p[0] + (y - p[1]) * (q[0] - p[0]) / (q[1] - p[1]);
                    lo = lo.min(x);
                    hi = hi.max(x);
                }
            }
            if lo <= hi {
                row.push((lo, hi));
            }
        }
    }
    let mut out = vec![0u8; (w * h) as usize];
    for (sy, row) in rows.iter_mut().enumerate() {
        row.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut end = 0u32;
        for (lo, hi) in row {
            let start = ((*lo * 2.0 - 0.5).ceil().clamp(0.0, (w * 2) as f32) as u32).max(end);
            let stop = (*hi * 2.0 - 0.5).ceil().clamp(0.0, (w * 2) as f32) as u32;
            for sx in start..stop {
                out[(sy / 2 * w as usize) + sx as usize / 2] |=
                    1 << ((sy % 2) * 2 + sx as usize % 2);
            }
            end = end.max(stop);
        }
    }
    out
}

/// Every contour has its own normalized length. Offsets clip open paths and
/// wrap closed paths. Arc lengths are measured on resolution-adaptive cubics.
fn revealed_segments(points: &[[f32; 2]], closed: bool, range: TrimRange) -> Vec<[[f32; 2]; 2]> {
    let span = range.end - range.start;
    if span <= 0.0 {
        return vec![];
    }
    let start = range.start + range.offset;
    let intervals = if closed {
        if span >= 1.0 {
            vec![(0.0, 1.0)]
        } else {
            let a = start.rem_euclid(1.0);
            let b = a + span;
            if b > 1.0 {
                vec![(a, 1.0), (0.0, b - 1.0)]
            } else {
                vec![(a, b)]
            }
        }
    } else {
        vec![(start.clamp(0.0, 1.0), (start + span).clamp(0.0, 1.0))]
    };
    let lengths: Vec<f32> = points
        .windows(2)
        .map(|s| ((s[1][0] - s[0][0]).powi(2) + (s[1][1] - s[0][1]).powi(2)).sqrt())
        .collect();
    let total: f32 = lengths.iter().sum();
    if total <= 1e-6 {
        return if intervals.iter().any(|(lo, hi)| lo <= &0.0 && hi > &0.0) {
            points.first().map(|p| vec![[*p, *p]]).unwrap_or_default()
        } else {
            vec![]
        };
    }
    let mut result = Vec::new();
    for (start, end) in intervals {
        let (lo, hi) = (start * total, end * total);
        let mut at = 0.0;
        for (seg, len) in points.windows(2).zip(&lengths) {
            let from = lo.max(at);
            let to = hi.min(at + len);
            if to > from && *len > 1e-6 {
                let point = |t: f32| {
                    [
                        seg[0][0] + (seg[1][0] - seg[0][0]) * t,
                        seg[0][1] + (seg[1][1] - seg[0][1]) * t,
                    ]
                };
                result.push([point((from - at) / len), point((to - at) / len)]);
            }
            at += len;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    /// #67: the ready-made shapes paint as their names say: an ellipse
    /// fills its middle but not its box's corners, a line only its stroke.
    #[test]
    fn ready_made_shapes_paint_their_outline() {
        use klipp_core::vector_path::{ShapePreset, VectorPath};
        let (w, h) = (100u32, 100u32);
        let transform = ObjectTransform {
            center: klipp_timeline::TitlePosition { x: 0.5, y: 0.5 },
            size: klipp_timeline::TitlePosition { x: 0.8, y: 0.8 },
            ..klipp_timeline::default_text_transform()
        };
        let alpha = |preset: ShapePreset, width: f32, x: usize, y: usize| {
            let mut buf = vec![0u8; (w * h * 4) as usize];
            let path = VectorPath::preset(preset, [1.0; 4], [1.0; 4], width);
            paint(&mut buf, &path.pose, &transform, w, h, None, None);
            buf[(y * w as usize + x) * 4 + 3]
        };
        // The box runs from 10 to 90.
        assert!(alpha(ShapePreset::Ellipse, 0.0, 50, 50) > 200, "the middle is filled");
        assert_eq!(alpha(ShapePreset::Ellipse, 0.0, 14, 14), 0, "the box's corner is outside the ellipse");
        assert!(alpha(ShapePreset::Star { points: 5, inner: 0.45 }, 0.0, 50, 50) > 200);
        assert_eq!(alpha(ShapePreset::Triangle, 0.0, 14, 14), 0, "a triangle's top corners are empty");
        assert!(alpha(ShapePreset::Triangle, 0.0, 50, 85) > 200, "its base is filled");
        assert!(alpha(ShapePreset::Line, 20.0, 50, 50) > 200, "a line draws its stroke");
        assert_eq!(alpha(ShapePreset::Line, 20.0, 50, 25), 0, "and nothing else");
    }

    #[test]
    fn scanline_capsules_match_distance_coverage_including_a_dot() {
        for points in [
            vec![[8.3, 9.1], [30.4, 18.2], [12.1, 23.6]],
            vec![[20.3, 20.7], [20.3, 20.7]],
        ] {
            let segments: Vec<_> = points.windows(2).map(|s| [s[0], s[1]]).collect();
            let mask = stroke_coverage(&segments, 40, 40, 4.2);
            for y in 0..40 {
                for x in 0..40 {
                    let mut bits = 0;
                    for sy in 0..2 {
                        for sx in 0..2 {
                            let p = [
                                x as f32 + (sx as f32 + 0.5) * 0.5,
                                y as f32 + (sy as f32 + 0.5) * 0.5,
                            ];
                            if points.windows(2).any(|s| distance(p, s[0], s[1]) <= 4.2) {
                                bits |= 1 << (sy * 2 + sx);
                            }
                        }
                    }
                    assert_eq!(mask[y * 40 + x], bits, "pixel {x},{y}");
                }
            }
        }
    }
}

#[cfg(test)]
mod trim_tests {
    use super::*;
    fn length(segments: &[[[f32; 2]; 2]]) -> f32 {
        segments
            .iter()
            .map(|s| ((s[1][0] - s[0][0]).powi(2) + (s[1][1] - s[0][1]).powi(2)).sqrt())
            .sum()
    }
    #[test]
    fn reveal_is_distance_based_across_unequal_segments() {
        let p = [[0.0, 0.0], [10.0, 0.0], [110.0, 0.0]];
        let s = revealed_segments(
            &p,
            false,
            TrimRange {
                start: 0.0,
                end: 0.5,
                offset: 0.0,
            },
        );
        assert!((length(&s) - 55.0).abs() < 1e-5);
        assert_eq!(s.last().unwrap()[1], [55.0, 0.0]);
        let mut curve = vec![[0.0, 0.0]];
        cubic(
            [[0.0, 0.0], [10.0, 90.0], [80.0, 10.0], [100.0, 100.0]],
            0,
            &mut curve,
        );
        curve.push([105.0, 100.0]);
        let all = revealed_segments(&curve, false, TrimRange::default());
        for i in 0..10 {
            let part = revealed_segments(
                &curve,
                false,
                TrimRange {
                    start: i as f32 / 10.0,
                    end: (i + 1) as f32 / 10.0,
                    offset: 0.0,
                },
            );
            assert!((length(&part) / length(&all) - 0.1).abs() < 1e-5);
        }
    }
    #[test]
    fn open_clips_closed_wraps_and_empty_reveals_nothing() {
        let p = [[0.0, 0.0], [100.0, 0.0]];
        let r = TrimRange {
            start: 0.0,
            end: 0.5,
            offset: 0.75,
        };
        assert!((length(&revealed_segments(&p, false, r)) - 25.0).abs() < 1e-5);
        assert!((length(&revealed_segments(&p, true, r)) - 50.0).abs() < 1e-5);
        assert!(revealed_segments(&p, true, TrimRange { end: 0.0, ..r }).is_empty());
        assert!(
            (length(&revealed_segments(
                &p,
                true,
                TrimRange {
                    offset: -0.2,
                    ..TrimRange::default()
                }
            )) - 100.0)
                .abs()
                < 1e-5
        );
    }
    #[test]
    fn each_contour_uses_its_own_length() {
        let r = TrimRange {
            start: 0.25,
            end: 0.75,
            offset: 0.0,
        };
        assert_eq!(
            length(&revealed_segments(&[[0.0, 0.0], [20.0, 0.0]], false, r)),
            10.0
        );
        assert_eq!(
            length(&revealed_segments(&[[0.0, 0.0], [80.0, 0.0]], false, r)),
            40.0
        );
    }
}

#[cfg(test)]
mod halo_work_tests {
    use super::*;
    struct Probe {
        calls: usize,
        area: usize,
    }
    impl super::super::AlphaMaskProcessor for Probe {
        fn blur(&mut self, mask: &[u8], w: u32, h: u32, _radius: f32) -> Vec<u8> {
            self.calls += 1;
            self.area = (w * h) as usize;
            mask.to_vec()
        }
        fn dilate(&mut self, mask: &[u8], _w: u32, _h: u32, _radius: f32) -> Vec<u8> {
            mask.to_vec()
        }
    }
    #[test]
    fn halo_uses_available_processor_only_for_visible_bounded_stroke() {
        let mut p = klipp_timeline::VectorPath::default();
        p.pose.fill = [0.0; 4];
        p.pose.stroke_glow = Some(klipp_timeline::StrokeGlow {
            radius: 20.0,
            strength: 1.0,
        });
        let mut transform = klipp_timeline::default_text_transform();
        transform.size = klipp_timeline::TitlePosition { x: 0.1, y: 0.1 };
        let mut buf = vec![0; 640 * 360 * 4];
        let mut gpu = Probe { calls: 0, area: 0 };
        paint(
            &mut buf,
            &p.pose,
            &transform,
            640,
            360,
            Some(TrimRange {
                end: 0.0,
                ..Default::default()
            }),
            Some(&mut gpu),
        );
        assert_eq!(gpu.calls, 0);
        p.pose.stroke[3] = 0.0;
        paint(
            &mut buf,
            &p.pose,
            &transform,
            640,
            360,
            None,
            Some(&mut gpu),
        );
        assert_eq!(gpu.calls, 0);
        p.pose.stroke[3] = 1.0;
        paint(
            &mut buf,
            &p.pose,
            &transform,
            640,
            360,
            None,
            Some(&mut gpu),
        );
        assert_eq!(gpu.calls, 1);
        assert!(gpu.area < 640 * 360 / 4);
    }
}
