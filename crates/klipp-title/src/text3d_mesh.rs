//! The solid of a glyph for 3D text (#1512): its outline as closed
//! contours, extruded to a depth with a bevel on its front and back
//! edges. The faces themselves are planar and stay polygons (the
//! rasteriser fills them with their plane's depth); the walls and the
//! bevels are triangles between rings of the contours, each ring the
//! outline inset by the bevel's profile at one depth.
//!
//! Coordinates are frame pixels, y down, z away from the viewer: the
//! front face at z = 0, the back at z = depth.

use ttf_parser::{GlyphId, OutlineBuilder};

/// One closed outline of a glyph, oriented so its signed area (y down)
/// is positive for the outside of a stroke and negative for a hole:
/// either way an edge's normal `(dy, -dx)` points away from the ink.
#[derive(Debug, Clone, PartialEq)]
pub struct Contour {
    pub points: Vec<[f32; 2]>,
    pub hole: bool,
}

/// A bevel in pixels: how far it cuts into the face and into the
/// depth, in how many steps, round or straight.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BevelPx {
    pub size: f32,
    pub segments: u8,
    pub round: bool,
}

/// What part of the solid a triangle is, for its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Wall,
    Bevel,
}

/// A triangle of the walls or bevels, with a normal at each corner.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tri {
    pub p: [[f32; 3]; 3],
    pub n: [[f32; 3]; 3],
    pub part: Part,
}

/// A planar face: its contours (holes included, filled by winding) at
/// depth `z`, facing `normal`.
#[derive(Debug, Clone, PartialEq)]
pub struct Face {
    pub contours: Vec<Vec<[f32; 2]>>,
    pub z: f32,
    pub normal: [f32; 3],
}

/// A glyph's solid: its front and back faces and the triangles between.
#[derive(Debug, Clone, PartialEq)]
pub struct GlyphMesh {
    pub front: Face,
    pub back: Face,
    pub tris: Vec<Tri>,
    /// The bevels as cut, after any clamping to the glyph's strokes.
    pub front_bevel: f32,
    pub back_bevel: f32,
}

/// Collects a glyph outline, flattened to within `tolerance` pixels,
/// scaled by `scale` (pixels per font unit, y flipped) about `origin`.
struct Flattener {
    scale: f32,
    origin: [f32; 2],
    tolerance: f32,
    contours: Vec<Vec<[f32; 2]>>,
    at: [f32; 2],
}

impl Flattener {
    fn pt(&self, x: f32, y: f32) -> [f32; 2] {
        [self.origin[0] + x * self.scale, self.origin[1] - y * self.scale]
    }
    fn push(&mut self, p: [f32; 2]) {
        if let Some(c) = self.contours.last_mut() {
            c.push(p);
        }
        self.at = p;
    }
}

impl OutlineBuilder for Flattener {
    fn move_to(&mut self, x: f32, y: f32) {
        let p = self.pt(x, y);
        self.contours.push(vec![p]);
        self.at = p;
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.pt(x, y);
        self.push(p);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (p0, p1, p2) = (self.at, self.pt(x1, y1), self.pt(x, y));
        let dd = len([p0[0] - 2.0 * p1[0] + p2[0], p0[1] - 2.0 * p1[1] + p2[1]]);
        let n = ((dd / (4.0 * self.tolerance)).sqrt().ceil() as usize).clamp(1, 64);
        for k in 1..=n {
            let t = k as f32 / n as f32;
            let u = 1.0 - t;
            self.push([u * u * p0[0] + 2.0 * u * t * p1[0] + t * t * p2[0], u * u * p0[1] + 2.0 * u * t * p1[1] + t * t * p2[1]]);
        }
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (p0, p1, p2, p3) = (self.at, self.pt(x1, y1), self.pt(x2, y2), self.pt(x, y));
        let dd = len([p0[0] - 2.0 * p1[0] + p2[0], p0[1] - 2.0 * p1[1] + p2[1]]).max(len([p1[0] - 2.0 * p2[0] + p3[0], p1[1] - 2.0 * p2[1] + p3[1]]));
        let n = ((dd * 0.75 / self.tolerance).sqrt().ceil() as usize).clamp(1, 64);
        for k in 1..=n {
            let t = k as f32 / n as f32;
            let u = 1.0 - t;
            let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            self.push([a * p0[0] + b * p1[0] + c * p2[0] + d * p3[0], a * p0[1] + b * p1[1] + c * p2[1] + d * p3[1]]);
        }
    }
    fn close(&mut self) {}
}

fn len(v: [f32; 2]) -> f32 {
    (v[0] * v[0] + v[1] * v[1]).sqrt()
}

/// Twice the signed area of a closed polygon, y down: positive when it
/// runs clockwise on screen.
pub fn signed_area(points: &[[f32; 2]]) -> f32 {
    let n = points.len();
    (0..n).map(|i| {
        let (a, b) = (points[i], points[(i + 1) % n]);
        a[0] * b[1] - b[0] * a[1]
    }).sum::<f32>() * 0.5
}

/// Whether `p` is inside the closed polygon `poly` (even-odd).
fn inside(p: [f32; 2], poly: &[[f32; 2]]) -> bool {
    let mut odd = false;
    let n = poly.len();
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]) {
            odd = !odd;
        }
    }
    odd
}

/// A glyph's outline as oriented contours in frame pixels: `scale`
/// pixels per font unit about `origin` (the glyph's baseline start),
/// curves flattened to within `tolerance` pixels. A contour inside an
/// odd number of others is a hole. `None` for a glyph without one.
pub fn glyph_contours(face: &ttf_parser::Face, glyph: GlyphId, scale: f32, origin: [f32; 2], tolerance: f32) -> Option<Vec<Contour>> {
    let mut f = Flattener { scale, origin, tolerance: tolerance.max(0.01), contours: Vec::new(), at: [0.0; 2] };
    face.outline_glyph(glyph, &mut f)?;
    Some(contours_of(f.contours))
}

/// Raw closed polylines as oriented [`Contour`]s: repeated points and
/// the closing repeat dropped, slivers dropped, holes found by nesting.
pub fn contours_of(raw: Vec<Vec<[f32; 2]>>) -> Vec<Contour> {
    let mut polys: Vec<Vec<[f32; 2]>> = raw
        .into_iter()
        .map(|mut c| {
            c.dedup_by(|b, a| len([b[0] - a[0], b[1] - a[1]]) < 1e-3);
            while c.len() > 1 && len([c[0][0] - c[c.len() - 1][0], c[0][1] - c[c.len() - 1][1]]) < 1e-3 {
                c.pop();
            }
            c
        })
        .filter(|c| c.len() >= 3 && signed_area(c).abs() > 1e-3)
        .collect();
    let holes: Vec<bool> = (0..polys.len())
        .map(|i| (0..polys.len()).filter(|&j| j != i && inside(polys[i][0], &polys[j])).count() % 2 == 1)
        .collect();
    for (c, &hole) in polys.iter_mut().zip(&holes) {
        if (signed_area(c) > 0.0) == hole {
            c.reverse();
        }
    }
    polys.into_iter().zip(holes).map(|(points, hole)| Contour { points, hole }).collect()
}

/// The unit normal of edge `a`→`b`, away from the ink.
fn edge_normal(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    let d = [b[0] - a[0], b[1] - a[1]];
    let l = len(d).max(1e-9);
    [d[1] / l, -d[0] / l]
}

/// Each vertex's direction away from the ink, scaled so a step of `d`
/// along it moves both of its edges by `d` (a mitre, limited to 3).
fn vertex_offsets(points: &[[f32; 2]]) -> Vec<[f32; 2]> {
    let n = points.len();
    (0..n)
        .map(|i| {
            let (a, b) = (edge_normal(points[(i + n - 1) % n], points[i]), edge_normal(points[i], points[(i + 1) % n]));
            let m = [a[0] + b[0], a[1] + b[1]];
            let l = len(m);
            if l < 1e-6 {
                return b;
            }
            let m = [m[0] / l, m[1] / l];
            let k = (1.0 / (m[0] * b[0] + m[1] * b[1]).max(1.0 / 3.0)).min(3.0);
            [m[0] * k, m[1] * k]
        })
        .collect()
}

/// `points` moved into the ink by `d` pixels.
fn inset(points: &[[f32; 2]], offsets: &[[f32; 2]], d: f32) -> Vec<[f32; 2]> {
    points.iter().zip(offsets).map(|(p, o)| [p[0] - o[0] * d, p[1] - o[1] * d]).collect()
}

/// Whether insetting every contour by `d` keeps each one the way it
/// ran, its edges unflipped: an inset deeper than half a stroke turns
/// edges over.
fn inset_holds(contours: &[Contour], d: f32) -> bool {
    contours.iter().all(|c| {
        let off = vertex_offsets(&c.points);
        let moved = inset(&c.points, &off, d);
        let n = c.points.len();
        let area = signed_area(&moved);
        // Each edge keeps its direction, and the contour its turn.
        (0..n).all(|i| {
            let (a, b) = (c.points[i], c.points[(i + 1) % n]);
            let (p, q) = (moved[i], moved[(i + 1) % n]);
            (b[0] - a[0]) * (q[0] - p[0]) + (b[1] - a[1]) * (q[1] - p[1]) > 0.0
        }) && (area > 0.0) == (signed_area(&c.points) > 0.0)
    })
}

/// The deepest bevel up to `want` the contours can take: halved until
/// an inset of it holds, none when even a tenth of a pixel does not.
pub fn clamp_bevel(contours: &[Contour], want: f32) -> f32 {
    let mut d = want.max(0.0);
    while d > 0.1 {
        if inset_holds(contours, d) {
            return d;
        }
        d *= 0.5;
    }
    0.0
}

/// A step of the solid's profile from its front face to its back: how
/// far the outline is inset there, at what depth, the profile's angle
/// (0 facing the viewer or away, a right angle along the walls), and
/// whether that step is on the front's side.
#[derive(Clone, Copy)]
struct Step {
    inset: f32,
    z: f32,
    angle: f32,
    front: bool,
}

/// The profile of a bevel of `b` pixels from the face at `z0` towards
/// the walls, `toward` +1 into the depth (front) or -1 (back): each
/// step's inset, depth and angle.
fn bevel_profile(b: &BevelPx, z0: f32, toward: f32) -> Vec<(f32, f32, f32)> {
    let steps = if b.round { b.segments.max(1) as usize } else { 1 };
    (0..=steps)
        .map(|k| {
            let t = k as f32 / steps as f32 * std::f32::consts::FRAC_PI_2;
            (b.size * (1.0 - t.sin()), z0 + toward * b.size * (1.0 - t.cos()), t)
        })
        .collect()
}

/// The solid of `contours` `depth` pixels deep, its front and back
/// edges bevelled (each clamped to what the strokes can take, and to
/// half the depth). Walls are shaded smooth across vertices that turn
/// less than `smooth_deg`, and creased at sharper ones.
pub fn extrude(contours: &[Contour], depth: f32, front: Option<BevelPx>, back: Option<BevelPx>, smooth_deg: f32) -> GlyphMesh {
    let depth = depth.max(0.0);
    let clamp = |b: Option<BevelPx>, room: f32| b.map(|b| BevelPx { size: clamp_bevel(contours, b.size.min(room)), ..b }).filter(|b| b.size > 0.0);
    let front = clamp(front, depth * 0.5);
    let back = clamp(back, depth * 0.5);
    let (fb, bb) = (front.map_or(0.0, |b| b.size), back.map_or(0.0, |b| b.size));
    let cos_smooth = smooth_deg.to_radians().cos();
    let mut tris = Vec::new();
    let mut front_face = Vec::new();
    let mut back_face = Vec::new();
    for c in contours {
        let off = vertex_offsets(&c.points);
        let n = c.points.len();
        let ring = |d: f32, z: f32| -> Vec<[f32; 3]> { inset(&c.points, &off, d).into_iter().map(|p| [p[0], p[1], z]).collect() };
        // The profile from the front face to the back face.
        let right = std::f32::consts::FRAC_PI_2;
        let mut steps: Vec<Step> = match &front {
            Some(b) => bevel_profile(b, 0.0, 1.0).into_iter().map(|(inset, z, angle)| Step { inset, z, angle, front: true }).collect(),
            None => vec![Step { inset: 0.0, z: 0.0, angle: right, front: true }],
        };
        steps.push(Step { inset: 0.0, z: depth - bb, angle: right, front: false });
        if let Some(b) = &back {
            steps.extend(bevel_profile(b, depth, -1.0).into_iter().rev().skip(1).map(|(inset, z, angle)| Step { inset, z, angle, front: false }));
        }
        let rings: Vec<Vec<[f32; 3]>> = steps.iter().map(|s| ring(s.inset, s.z)).collect();
        front_face.push(rings[0].iter().map(|p| [p[0], p[1]]).collect::<Vec<_>>());
        back_face.push(rings[rings.len() - 1].iter().map(|p| [p[0], p[1]]).collect::<Vec<_>>());
        // Each vertex's direction away from the ink in the plane, and
        // each edge's; a vertex turning less than the smoothing angle
        // shares one normal between its edges.
        let edge_n: Vec<[f32; 2]> = (0..n).map(|i| edge_normal(c.points[i], c.points[(i + 1) % n])).collect();
        let smooth: Vec<Option<[f32; 2]>> = (0..n)
            .map(|i| {
                let (a, b) = (edge_n[(i + n - 1) % n], edge_n[i]);
                (a[0] * b[0] + a[1] * b[1] >= cos_smooth).then(|| {
                    let m = [a[0] + b[0], a[1] + b[1]];
                    let l = len(m).max(1e-9);
                    [m[0] / l, m[1] / l]
                })
            })
            .collect();
        // The normal at a ring vertex: away from the ink in the plane
        // by the profile's angle, toward the viewer (front) or away.
        let normal = |xy: [f32; 2], angle: f32, front_side: bool| -> [f32; 3] {
            let z = angle.cos() * if front_side { -1.0 } else { 1.0 };
            [xy[0] * angle.sin(), xy[1] * angle.sin(), z]
        };
        for w in 0..steps.len() - 1 {
            let (s0, s1) = (steps[w], steps[w + 1]);
            let wall = (s0.angle - right).abs() < 1e-4 && (s1.angle - right).abs() < 1e-4;
            let bevel = if s0.front { &front } else { &back };
            // A straight bevel is one slope across its step.
            let (a0, a1) = if !wall && bevel.is_some_and(|b| !b.round) { ((s0.angle + s1.angle) * 0.5, (s0.angle + s1.angle) * 0.5) } else { (s0.angle, s1.angle) };
            let side = s0.front && s1.front || (s0.front && !wall);
            let part = if wall { Part::Wall } else { Part::Bevel };
            let (r0, r1) = (&rings[w], &rings[w + 1]);
            for i in 0..n {
                let j = (i + 1) % n;
                let (ni, nj) = (smooth[i].unwrap_or(edge_n[i]), smooth[j].unwrap_or(edge_n[i]));
                let (n0i, n0j, n1i, n1j) = (normal(ni, a0, side), normal(nj, a0, side), normal(ni, a1, side), normal(nj, a1, side));
                tris.push(Tri { p: [r0[i], r0[j], r1[j]], n: [n0i, n0j, n1j], part });
                tris.push(Tri { p: [r0[i], r1[j], r1[i]], n: [n0i, n1j, n1i], part });
            }
        }
    }
    GlyphMesh {
        front: Face { contours: front_face, z: 0.0, normal: [0.0, 0.0, -1.0] },
        back: Face { contours: back_face, z: depth, normal: [0.0, 0.0, 1.0] },
        tris,
        front_bevel: fb,
        back_bevel: bb,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inter() -> ttf_parser::Face<'static> {
        let bytes = crate::bundled::BUNDLED.iter().find(|f| f.family == "Inter" && f.style == "Regular").unwrap().bytes;
        ttf_parser::Face::parse(bytes, 0).unwrap()
    }

    /// `ch` of Inter at 200 px, its baseline at (100, 300).
    fn glyph(ch: char) -> Vec<Contour> {
        let face = inter();
        let scale = 200.0 / face.units_per_em() as f32;
        glyph_contours(&face, face.glyph_index(ch).unwrap(), scale, [100.0, 300.0], 0.25).unwrap()
    }

    fn centre(c: &Contour) -> [f32; 2] {
        let n = c.points.len() as f32;
        let s = c.points.iter().fold([0.0, 0.0], |a, p| [a[0] + p[0], a[1] + p[1]]);
        [s[0] / n, s[1] / n]
    }

    /// An O is an outline round a hole: the outside runs one way, the
    /// hole the other, and every edge's normal points away from the
    /// ink: out of the O, and into its hole.
    #[test]
    fn an_o_is_an_outside_round_a_hole_with_normals_off_the_ink() {
        let o = glyph('O');
        assert_eq!(o.len(), 2, "an outside and a hole");
        let (outer, hole) = if o[0].hole { (&o[1], &o[0]) } else { (&o[0], &o[1]) };
        assert!(!outer.hole && hole.hole);
        assert!(signed_area(&outer.points) > 0.0 && signed_area(&hole.points) < 0.0);
        let mid = centre(outer);
        for (c, away) in [(outer, 1.0f32), (hole, -1.0)] {
            let n = c.points.len();
            let off = (0..n).filter(|&i| {
                let (a, b) = (c.points[i], c.points[(i + 1) % n]);
                let e = edge_normal(a, b);
                let m = [(a[0] + b[0]) * 0.5 - mid[0], (a[1] + b[1]) * 0.5 - mid[1]];
                (e[0] * m[0] + e[1] * m[1]) * away <= 0.0
            }).count();
            assert_eq!(off, 0, "every edge's normal points away from the ink ({} of {n} do not)", off);
        }
        // Flattened finely: no edge longer than a curve's worth.
        assert!(outer.points.len() > 40, "{}", outer.points.len());
        let b = glyph('B');
        assert_eq!((b.len(), b.iter().filter(|c| c.hole).count()), (3, 2), "a B has two holes");
    }

    /// The solid: its faces are its first and last rings, its triangles
    /// span the depth, the walls face out from the ink, the faces face
    /// the viewer and away.
    #[test]
    fn an_extruded_o_is_closed_and_faces_out() {
        let o = glyph('O');
        let bevel = BevelPx { size: 4.0, segments: 3, round: true };
        let mesh = extrude(&o, 40.0, Some(bevel), Some(BevelPx { round: false, ..bevel }), 25.0);
        assert_eq!((mesh.front_bevel, mesh.back_bevel), (4.0, 4.0));
        assert_eq!((mesh.front.z, mesh.back.z, mesh.front.normal[2], mesh.back.normal[2]), (0.0, 40.0, -1.0, 1.0));
        assert_eq!(mesh.front.contours.len(), 2);
        // Rings per contour: four for the round front bevel, the wall's
        // back, and one for the straight back bevel.
        let rings = 4 + 1 + 1;
        let edges: usize = o.iter().map(|c| c.points.len()).sum();
        assert_eq!(mesh.tris.len(), edges * 2 * (rings - 1));
        let zs = mesh.tris.iter().flat_map(|t| t.p).map(|p| p[2]);
        let (lo, hi) = zs.fold((f32::MAX, f32::MIN), |(a, b), z| (a.min(z), b.max(z)));
        assert_eq!((lo, hi), (0.0, 40.0));
        // The front face is inset by the bevel: smaller than the outline.
        let outer = o.iter().position(|c| !c.hole).unwrap();
        assert!(signed_area(&mesh.front.contours[outer]) < signed_area(&o[outer].points));
        // Every corner normal is unit length; a wall's lies in the plane.
        let mut walls = 0;
        for t in &mesh.tris {
            for n in &t.n {
                assert!((n[0] * n[0] + n[1] * n[1] + n[2] * n[2] - 1.0).abs() < 1e-3, "{n:?}");
                if t.part == Part::Wall {
                    walls += 1;
                    assert!(n[2].abs() < 1e-3, "a wall faces sideways: {n:?}");
                }
            }
        }
        assert!(walls > 0);
        // A bevel near the front tilts toward the viewer; near the back, away.
        assert!(mesh.tris.iter().filter(|t| t.part == Part::Bevel).all(|t| t.p.iter().zip(&t.n).all(|(p, n)| if p[2] < 20.0 { n[2] <= 1e-4 } else { n[2] >= -1e-4 })));
    }

    /// A bevel deeper than a thin stroke can take is cut back until the
    /// inset outline still runs the way it did; one that fits is kept.
    #[test]
    fn a_bevel_too_deep_for_a_thin_stroke_is_cut_back() {
        let l = glyph('l');
        let wide = l[0].points.iter().map(|p| p[0]).fold(f32::MIN, f32::max) - l[0].points.iter().map(|p| p[0]).fold(f32::MAX, f32::min);
        let cut = clamp_bevel(&l, 100.0);
        assert!(cut > 0.0 && cut < wide * 0.5, "cut to {cut} for a stroke {wide} wide");
        assert!(inset_holds(&l, cut));
        assert!(!inset_holds(&l, wide), "a whole stroke's inset turns it over");
        assert_eq!(clamp_bevel(&l, 2.0), 2.0, "a small bevel is kept");
        let mesh = extrude(&l, 30.0, Some(BevelPx { size: 100.0, segments: 2, round: true }), None, 25.0);
        assert!(mesh.front_bevel <= 15.0, "no more than half the depth: {}", mesh.front_bevel);
        assert!(signed_area(&mesh.front.contours[0]) > 0.0, "the face still runs the way the outline does");
    }

    /// Without bevels the faces are the outline itself, and the walls
    /// run straight from the front to the back.
    #[test]
    fn without_bevels_the_faces_are_the_outline() {
        let i = glyph('I');
        let mesh = extrude(&i, 20.0, None, None, 25.0);
        assert_eq!(mesh.front.contours[0], i[0].points);
        assert_eq!(mesh.back.contours[0], i[0].points);
        assert_eq!(mesh.tris.len(), i[0].points.len() * 2);
        assert!(mesh.tris.iter().all(|t| t.part == Part::Wall && t.n.iter().all(|n| n[2].abs() < 1e-6)));
    }
}
