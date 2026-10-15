//! Cached planar-face tessellation by nonzero winding. Splitting at vertices
//! and edge crossings makes each filled interval a trapezoid, including holes
//! and self-intersecting inset contours. No per-frame tessellation is needed.
use crate::text3d_mesh::Face;

/// A face as triangles in object coordinates. Reject nonfinite or excessive
/// geometry rather than changing its winding rule or exhausting memory.
pub fn triangulate(face: &Face, max_triangles: usize) -> Option<Vec<[[f32; 3]; 3]>> {
    let mut edges = Vec::new();
    let mut ys = Vec::new();
    for contour in &face.contours {
        for (a, b) in contour
            .iter()
            .zip(contour.iter().cycle().skip(1))
            .take(contour.len())
        {
            if a.iter().chain(b).any(|v| !v.is_finite()) || !face.z.is_finite() {
                return None;
            }
            if ys.len() >= 8192 {
                return None;
            }
            ys.push(a[1]);
            if a[1] != b[1] {
                edges.push((*a, *b));
            }
        }
    }
    // Quadratic crossing discovery is bounded; long outlines keep the CPU path.
    if edges.len() > 4096 {
        return None;
    }
    for (i, (a, b)) in edges.iter().enumerate() {
        let ab = [b[0] - a[0], b[1] - a[1]];
        for (c, d) in &edges[..i] {
            let cd = [d[0] - c[0], d[1] - c[1]];
            let det = ab[0] * cd[1] - ab[1] * cd[0];
            if det.abs() < 1e-12 {
                continue;
            }
            let ca = [c[0] - a[0], c[1] - a[1]];
            let t = (ca[0] * cd[1] - ca[1] * cd[0]) / det;
            let u = (ca[0] * ab[1] - ca[1] * ab[0]) / det;
            if t > 0.0 && t < 1.0 && u > 0.0 && u < 1.0 {
                if ys.len() >= 8192 {
                    return None;
                }
                ys.push(a[1] + t * ab[1]);
            }
        }
    }
    ys.sort_by(f32::total_cmp);
    ys.dedup();
    let x_at =
        |(a, b): &([f32; 2], [f32; 2]), y: f32| a[0] + (y - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
    let mut triangles = Vec::new();
    for span in ys.windows(2) {
        let (y0, y1) = (span[0], span[1]);
        let mid = y0 + (y1 - y0) * 0.5;
        if mid <= y0 || mid >= y1 {
            continue;
        }
        let mut crossings: Vec<_> = edges
            .iter()
            .enumerate()
            .filter(|(_, (a, b))| (a[1] <= mid) != (b[1] <= mid))
            .map(|(i, e)| (x_at(e, mid), i, if e.1[1] > e.0[1] { 1 } else { -1 }))
            .collect();
        crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut winding = 0;
        for pair in crossings.windows(2) {
            winding += pair[0].2;
            if winding == 0 || pair[1].0 <= pair[0].0 {
                continue;
            }
            let left = &edges[pair[0].1];
            let right = &edges[pair[1].1];
            let p = [
                [x_at(left, y0), y0, face.z],
                [x_at(right, y0), y0, face.z],
                [x_at(right, y1), y1, face.z],
                [x_at(left, y1), y1, face.z],
            ];
            if p.iter().flatten().any(|v| !v.is_finite()) {
                return None;
            }
            for tri in [[p[0], p[1], p[2]], [p[0], p[2], p[3]]] {
                let [a, b, c] = tri;
                let area = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
                if area.abs() > 1e-9 {
                    if triangles.len() >= max_triangles {
                        return None;
                    }
                    triangles.push(tri);
                }
            }
        }
    }
    Some(triangles)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn face(contours: Vec<Vec<[f32; 2]>>) -> Face {
        Face {
            contours,
            z: 7.0,
            normal: [0.0, 0.0, -1.0],
        }
    }
    fn winding(face: &Face, p: [f32; 2]) -> bool {
        let mut wind = 0;
        for c in &face.contours {
            for (a, b) in c.iter().zip(c.iter().cycle().skip(1)).take(c.len()) {
                if (a[1] <= p[1]) != (b[1] <= p[1]) {
                    let x = a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
                    if x <= p[0] {
                        wind += if b[1] > a[1] { 1 } else { -1 };
                    }
                }
            }
        }
        wind != 0
    }
    fn contains(t: [[f32; 3]; 3], p: [f32; 2]) -> bool {
        let cross = |a: [f32; 3], b: [f32; 3]| {
            (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])
        };
        let v = [cross(t[0], t[1]), cross(t[1], t[2]), cross(t[2], t[0])];
        v.iter().all(|v| *v >= 0.0) || v.iter().all(|v| *v <= 0.0)
    }
    #[test]
    fn tessellation_preserves_holes_concavity_and_crossings() {
        let outer = vec![[0.0, 0.0], [20.0, 0.0], [20.0, 20.0], [0.0, 20.0]];
        let hole = vec![[5.0, 5.0], [5.0, 15.0], [15.0, 15.0], [15.0, 5.0]];
        let cases = [
            face(vec![outer, hole]),
            face(vec![vec![
                [0.0, 0.0],
                [20.0, 20.0],
                [0.0, 20.0],
                [20.0, 0.0],
            ]]),
            face(vec![vec![
                [0.0, 0.0],
                [20.0, 0.0],
                [9.0, 9.0],
                [20.0, 20.0],
                [0.0, 20.0],
            ]]),
        ];
        for f in cases {
            let tris = triangulate(&f, 1000).unwrap();
            for y in -1..42 {
                for x in -1..42 {
                    let p = [x as f32 * 0.5 + 0.17, y as f32 * 0.5 + 0.31];
                    assert_eq!(
                        tris.iter().any(|t| contains(*t, p)),
                        winding(&f, p),
                        "at {p:?}"
                    );
                }
            }
            assert!(tris.iter().flatten().all(|v| v[2] == 7.0));
            assert!(triangulate(&f, 0).is_none());
        }
    }
}
