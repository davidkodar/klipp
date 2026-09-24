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
    if pose.validate().is_err() || width == 0 || height == 0 {
        return;
    }
    let contours = contours(pose, transform, width, height);
    let half = pose.stroke_width * height as f32 / 1080.0 * 0.5;
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
        for (_, points) in &contours {
            for seg in points.windows(2) {
                let (a, b) = (seg[0], seg[1]);
                let x0 = ((a[0].min(b[0]) - half) * 2.0 - 0.5).ceil().max(0.0) as u32;
                let x1 = ((a[0].max(b[0]) + half) * 2.0 - 0.5)
                    .ceil()
                    .clamp(0.0, (width * 2) as f32) as u32;
                let y0 = ((a[1].min(b[1]) - half) * 2.0 - 0.5).ceil().max(0.0) as u32;
                let y1 = ((a[1].max(b[1]) + half) * 2.0 - 0.5)
                    .ceil()
                    .clamp(0.0, (height * 2) as f32) as u32;
                for sy in y0..y1 {
                    for sx in x0..x1 {
                        if distance([(sx as f32 + 0.5) * 0.5, (sy as f32 + 0.5) * 0.5], a, b)
                            <= half
                        {
                            stroke[((sy / 2) * width + sx / 2) as usize] |=
                                1 << ((sy % 2) * 2 + sx % 2);
                        }
                    }
                }
            }
        }
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
            super::over_blend_chunk(&mut buf[i * 4..i * 4 + 4], &src);
        }
    }
}
