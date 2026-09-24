//! Resolution-adaptive cubic rasterization. Closed contours use even-odd
//! fill; open contours are stroke-only. Strokes have round caps and joins.
use klipp_timeline::{ObjectTransform, PathPose};
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
pub(crate) fn paint(
    buf: &mut [u8],
    pose: &PathPose,
    transform: &ObjectTransform,
    width: u32,
    height: u32,
) {
    if width == 0 || height == 0 {
        return;
    }
    let mut contours = contours(pose, transform, width, height);
    let half = pose.stroke_width * height as f32 / 1080.0 * 0.5;
    // Bound all coverage and blending to the visible geometry.
    let mut lo = [f32::INFINITY; 2];
    let mut hi = [f32::NEG_INFINITY; 2];
    for p in contours.iter().flat_map(|(_, p)| p) {
        for axis in 0..2 {
            if !p[axis].is_finite() {
                return;
            }
            lo[axis] = lo[axis].min(p[axis]);
            hi[axis] = hi[axis].max(p[axis]);
        }
    }
    let pad = if pose.stroke[3] > 0.0 {
        half + 1.0
    } else {
        1.0
    };
    let x0 = (lo[0] - pad).floor().clamp(0.0, width as f32) as u32;
    let y0 = (lo[1] - pad).floor().clamp(0.0, height as f32) as u32;
    let x1 = (hi[0] + pad).ceil().clamp(0.0, width as f32) as u32;
    let y1 = (hi[1] + pad).ceil().clamp(0.0, height as f32) as u32;
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let frame_width = width;
    let (width, height) = (x1 - x0, y1 - y0);
    for p in contours.iter_mut().flat_map(|(_, p)| p) {
        p[0] -= x0 as f32;
        p[1] -= y0 as f32;
    }
    let count = width as usize * height as usize;
    let mut fill = vec![0u8; count];
    let mut stroke = vec![0u8; count];
    // Two-by-two coverage, scan-converted in output space. Fill work is
    // proportional to painted area + scanlines × edges, not pixels × edges.
    for sy in 0..height * 2 {
        let y = (sy as f32 + 0.5) * 0.5;
        let mut crossings = Vec::new();
        for (_, points) in contours.iter().filter(|(closed, _)| *closed) {
            for seg in points.windows(2) {
                let (a, b) = (seg[0], seg[1]);
                if (a[1] > y) != (b[1] > y) {
                    crossings.push(a[0] + (y - a[1]) * (b[0] - a[0]) / (b[1] - a[1]));
                }
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
    if half > 0.0 && pose.stroke[3] > 0.0 {
        stroke = stroke_coverage(&contours, width, height, half);
    }
    for (i, (f, s)) in fill.into_iter().zip(stroke).enumerate() {
        let fa = f.count_ones() as f32 * 0.25 * pose.fill[3];
        let sa = s.count_ones() as f32 * 0.25 * pose.stroke[3];
        let overlap = (f & s).count_ones() as f32 * 0.25 * pose.fill[3] * pose.stroke[3];
        let alpha = fa + sa - overlap;
        if alpha > 0.0 {
            let mut src = [0; 4];
            for c in 0..3 {
                src[c] = ((pose.fill[c] * (fa - overlap) + pose.stroke[c] * sa) / alpha * 255.0)
                    .round() as u8;
            }
            src[3] = (alpha * 255.0).round() as u8;
            let target = ((y0 as usize + i / width as usize) * frame_width as usize
                + x0 as usize
                + i % width as usize)
                * 4;
            super::over_blend_chunk(&mut buf[target..target + 4], &src);
        }
    }
}

/// Scan-convert the union of round-capped segments. Each scanline merges
/// analytic capsule intervals before filling coverage, so overlapping wide
/// segment boxes do not each visit every covered sample.
fn stroke_coverage(contours: &[(bool, Vec<[f32; 2]>)], w: u32, h: u32, r: f32) -> Vec<u8> {
    let mut rows = vec![Vec::<(f32, f32)>::new(); (h * 2) as usize];
    for (_, points) in contours {
        for seg in points.windows(2) {
            let (a, b) = (seg[0], seg[1]);
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scanline_capsules_match_distance_coverage_including_a_dot() {
        for points in [vec![[8.3,9.1],[30.4,18.2],[12.1,23.6]],vec![[20.3,20.7],[20.3,20.7]]] {
            let mask=stroke_coverage(&[(false,points.clone())],40,40,4.2);
            for y in 0..40 {for x in 0..40 {let mut bits=0;
                for sy in 0..2 {for sx in 0..2 {
                    let p=[x as f32+(sx as f32+0.5)*0.5,y as f32+(sy as f32+0.5)*0.5];
                    if points.windows(2).any(|s|distance(p,s[0],s[1])<=4.2) {bits|=1<<(sy*2+sx);}
                }}
                assert_eq!(mask[y*40+x],bits,"pixel {x},{y}");
            }}
        }
    }
}
