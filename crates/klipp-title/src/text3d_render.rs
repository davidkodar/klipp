//! Drawing 3D text (#1512): the glyph solids of [`crate::text3d_mesh`]
//! turned about the text's centre, seen through a perspective camera
//! (or none), lit from a direction in linear light, and laid into a
//! frame with a depth buffer at a few samples a pixel.
//!
//! The camera sits as far from the text as keeps its unturned front
//! face at its flat size, so a 3D text at rest covers what its flat
//! self would; the field of view only sets how strongly it recedes.
//! Walls and bevels are triangles, their normals interpolated with
//! perspective; the front and back faces are planar polygons, filled
//! row by row by winding (holes and all) at their plane's depth.

use crate::text3d_mesh::{GlyphMesh, Part};
use klipp_timeline::Text3d;
use rayon::prelude::*;

/// A glyph of a 3D text where its layout placed it: its font (the
/// renderer's cache key), glyph, size in pixels and baseline origin on
/// the frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Glyph3d {
    pub font: String,
    pub glyph: u16,
    pub px: f32,
    pub origin: [f32; 2],
    /// A colour glyph (an emoji) as its bitmap: drawn flat on the front
    /// plane, turned with the text, without depth.
    pub sprite: Option<Sprite>,
}

/// A bitmap on the text's front plane: its top left corner on the
/// frame, its size and its straight-alpha sRGB pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct Sprite {
    pub x: f32,
    pub y: f32,
    pub width: usize,
    pub height: usize,
    pub rgba: std::sync::Arc<Vec<u8>>,
}

/// What is drawn: the glyph solids, the 3D settings (their keys
/// already evaluated), the point they turn about (the text's centre,
/// half way into its depth), and the colours of the faces, the walls
/// and the bevels.
pub struct Scene<'a> {
    pub meshes: &'a [GlyphMesh],
    pub sprites: &'a [Sprite],
    pub settings: &'a Text3d,
    pub pivot: [f32; 3],
    pub face: [u8; 4],
    pub side: [u8; 4],
    pub bevel: [u8; 4],
}

type V3 = [f32; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn norm(a: V3) -> V3 {
    let l = dot(a, a).sqrt().max(1e-12);
    [a[0] / l, a[1] / l, a[2] / l]
}

/// The turn: about x, then y, then z, by `deg` degrees each.
fn rotation(deg: [f32; 3]) -> [[f32; 3]; 3] {
    let [x, y, z] = deg.map(f32::to_radians);
    let (sx, cx, sy, cy, sz, cz) = (x.sin(), x.cos(), y.sin(), y.cos(), z.sin(), z.cos());
    // Rz · Ry · Rx
    [
        [cz * cy, cz * sy * sx - sz * cx, cz * sy * cx + sz * sx],
        [sz * cy, sz * sy * sx + cz * cx, sz * sy * cx - cz * sx],
        [-sy, cy * sx, cy * cx],
    ]
}

fn apply(m: &[[f32; 3]; 3], v: V3) -> V3 {
    [dot(m[0], v), dot(m[1], v), dot(m[2], v)]
}

fn to_linear(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn to_srgb(v: f32) -> u8 {
    let v = v.clamp(0.0, 1.0);
    let c = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
    (c * 255.0).round() as u8
}

/// The view: object points to camera space and the frame.
struct View {
    rot: [[f32; 3]; 3],
    pivot: V3,
    /// The camera's distance to the front plane, the focal length too;
    /// `None` for orthographic.
    focal: Option<f32>,
    /// Samples per pixel along each axis.
    ss: f32,
    /// The sample grid's origin in frame pixels.
    origin: [f32; 2],
}

impl View {
    /// An object point in camera space: turned about the pivot, the
    /// camera at the origin looking along +z.
    /// A camera-space point back in object space: the turn undone.
    fn object(&self, c: V3) -> V3 {
        let c = match self.focal {
            Some(f) => [c[0], c[1], c[2] - f - self.pivot[2]],
            None => c,
        };
        let r = &self.rot;
        let q = [r[0][0] * c[0] + r[1][0] * c[1] + r[2][0] * c[2], r[0][1] * c[0] + r[1][1] * c[1] + r[2][1] * c[2], r[0][2] * c[0] + r[1][2] * c[1] + r[2][2] * c[2]];
        [q[0] + self.pivot[0], q[1] + self.pivot[1], q[2] + self.pivot[2]]
    }

    fn camera(&self, p: V3) -> V3 {
        let q = apply(&self.rot, sub(p, self.pivot));
        match self.focal {
            Some(f) => [q[0], q[1], q[2] + f + (self.pivot[2])],
            None => q,
        }
    }

    /// A camera-space point on the sample grid, and its depth.
    fn screen(&self, c: V3) -> Option<[f32; 3]> {
        let (x, y) = match self.focal {
            Some(f) => {
                if c[2] < f * 0.02 {
                    return None;
                }
                (c[0] * f / c[2], c[1] * f / c[2])
            }
            None => (c[0], c[1]),
        };
        Some([(self.pivot[0] + x - self.origin[0]) * self.ss, (self.pivot[1] + y - self.origin[1]) * self.ss, c[2]])
    }

    /// The camera-space ray through sample point `(sx, sy)`: its origin
    /// and direction.
    fn ray(&self, sx: f32, sy: f32) -> (V3, V3) {
        let (x, y) = (sx / self.ss + self.origin[0] - self.pivot[0], sy / self.ss + self.origin[1] - self.pivot[1]);
        match self.focal {
            Some(f) => ([0.0; 3], [x / f, y / f, 1.0]),
            None => ([x, y, -1.0e6], [0.0, 0.0, 1.0]),
        }
    }
}

/// The light in camera space, and how a surface takes it.
struct Light {
    dir: V3,
    intensity: f32,
    ambient: f32,
    specular: f32,
    shininess: f32,
}

impl Light {
    fn of(t: &Text3d) -> Self {
        let (az, el) = (t.light.azimuth.to_radians(), t.light.elevation.to_radians());
        Light {
            dir: norm([el.cos() * az.cos(), el.cos() * az.sin(), -el.sin()]),
            intensity: t.light.intensity,
            ambient: t.light.ambient,
            specular: t.light.specular,
            shininess: t.light.shininess.max(1.0),
        }
    }

    /// `base` (linear) lit at camera-space point `p` with normal `n`,
    /// seen from `view_dir` (towards the viewer).
    fn shade(&self, base: V3, n: V3, view_dir: V3) -> V3 {
        let n = norm(n);
        let diffuse = dot(n, self.dir).max(0.0) * self.intensity;
        let spec = if diffuse > 0.0 {
            let h = norm([self.dir[0] + view_dir[0], self.dir[1] + view_dir[1], self.dir[2] + view_dir[2]]);
            self.specular * dot(n, h).max(0.0).powf(self.shininess) * self.intensity
        } else {
            0.0
        };
        let k = self.ambient + diffuse;
        [base[0] * k + spec, base[1] * k + spec, base[2] * k + spec]
    }
}

/// A triangle ready to fill: its corners on the sample grid with their
/// camera depth, its normals and camera points over that depth (for
/// interpolation with perspective), and its colour.
struct Prepared {
    s: [[f32; 3]; 3],
    n_over_z: [V3; 3],
    p_over_z: [V3; 3],
    base: V3,
    rows: (i32, i32),
}

/// A planar face ready to fill: its outline on the sample grid, its
/// plane in camera space and its shade.
struct PreparedFace<'a> {
    edges: Vec<([f32; 2], [f32; 2])>,
    normal: V3,
    point: V3,
    base: V3,
    rows: (i32, i32),
    /// A sprite's bitmap, sampled where the plane is hit, unlit.
    sprite: Option<&'a Sprite>,
}

/// The frame of `scene` at `width`×`height`, straight-alpha sRGB bytes,
/// at `ss`×`ss` samples a pixel. Pixels it does not touch are clear.
pub fn render(scene: &Scene, width: u32, height: u32, ss: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let mut out = vec![0u8; w * h * 4];
    let t = scene.settings;
    let ss_f = ss.max(1) as f32;
    let focal = (!t.camera.orthographic).then(|| (height as f32 * 0.5) / (t.camera.fov.clamp(1.0, 170.0).to_radians() * 0.5).tan());
    let mut view = View { rot: rotation(t.rotation), pivot: scene.pivot, focal, ss: ss_f, origin: [0.0, 0.0] };
    let light = Light::of(t);
    let lin = |c: [u8; 4]| [to_linear(c[0]), to_linear(c[1]), to_linear(c[2])];
    let (face_c, side_c, bevel_c) = (lin(scene.face), lin(scene.side), lin(scene.bevel));

    // Where the solid lands on the frame: the box round every corner.
    let mut lo = [f32::MAX; 2];
    let mut hi = [f32::MIN; 2];
    let mut grow = |c: V3| {
        // `screen` is on the sample grid; the box is in pixels.
        if let Some(s) = view.screen(c) {
            let (x, y) = (s[0] / ss_f, s[1] / ss_f);
            lo = [lo[0].min(x), lo[1].min(y)];
            hi = [hi[0].max(x), hi[1].max(y)];
        }
    };
    for m in scene.meshes {
        for t in &m.tris {
            for p in t.p {
                grow(view.camera(p));
            }
        }
        for f in [&m.front, &m.back] {
            for c in &f.contours {
                for p in c {
                    grow(view.camera([p[0], p[1], f.z]));
                }
            }
        }
    }
    for sp in scene.sprites {
        for (x, y) in [(sp.x, sp.y), (sp.x + sp.width as f32, sp.y), (sp.x, sp.y + sp.height as f32), (sp.x + sp.width as f32, sp.y + sp.height as f32)] {
            grow(view.camera([x, y, 0.0]));
        }
    }
    if lo[0] > hi[0] {
        return out;
    }
    let (x0, y0) = ((lo[0].floor() as i64 - 1).clamp(0, w as i64) as usize, (lo[1].floor() as i64 - 1).clamp(0, h as i64) as usize);
    let (x1, y1) = ((hi[0].ceil() as i64 + 2).clamp(0, w as i64) as usize, (hi[1].ceil() as i64 + 2).clamp(0, h as i64) as usize);
    if x1 <= x0 || y1 <= y0 {
        return out;
    }
    view.origin = [x0 as f32, y0 as f32];
    let (bw, bh) = ((x1 - x0) * ss as usize, (y1 - y0) * ss as usize);

    // Every triangle and face on the sample grid.
    let mut tris: Vec<Prepared> = Vec::new();
    let mut faces: Vec<PreparedFace> = Vec::new();
    for m in scene.meshes {
        for tri in &m.tris {
            let cam = tri.p.map(|p| view.camera(p));
            let Some(s0) = view.screen(cam[0]) else { continue };
            let Some(s1) = view.screen(cam[1]) else { continue };
            let Some(s2) = view.screen(cam[2]) else { continue };
            let s = [s0, s1, s2];
            let zs = s.map(|v| if focal.is_some() { v[2] } else { 1.0 });
            let n = tri.n.map(|n| apply(&view.rot, n));
            let rows = ((s0[1].min(s1[1]).min(s2[1]).floor() as i32).max(0), (s0[1].max(s1[1]).max(s2[1]).ceil() as i32).min(bh as i32));
            tris.push(Prepared {
                s,
                n_over_z: [0, 1, 2].map(|k| [n[k][0] / zs[k], n[k][1] / zs[k], n[k][2] / zs[k]]),
                p_over_z: [0, 1, 2].map(|k| [cam[k][0] / zs[k], cam[k][1] / zs[k], cam[k][2] / zs[k]]),
                base: if tri.part == Part::Bevel { bevel_c } else { side_c },
                rows,
            });
        }
        for f in [&m.front, &m.back] {
            let mut edges = Vec::new();
            let (mut top, mut bottom) = (f32::MAX, f32::MIN);
            let mut whole = true;
            for c in &f.contours {
                let pts: Vec<[f32; 2]> = c
                    .iter()
                    .filter_map(|p| view.screen(view.camera([p[0], p[1], f.z])).map(|s| [s[0], s[1]]))
                    .collect();
                whole &= pts.len() == c.len();
                for k in 0..pts.len() {
                    let (a, b) = (pts[k], pts[(k + 1) % pts.len()]);
                    top = top.min(a[1]);
                    bottom = bottom.max(a[1]);
                    edges.push((a, b));
                }
            }
            if !whole || edges.is_empty() {
                continue;
            }
            faces.push(PreparedFace {
                edges,
                normal: apply(&view.rot, f.normal),
                point: view.camera([0.0, 0.0, f.z]),
                base: face_c,
                rows: ((top.floor() as i32).max(0), (bottom.ceil() as i32).min(bh as i32)),
                sprite: None,
            });
        }
    }
    for sp in scene.sprites {
        let corners = [[sp.x, sp.y], [sp.x + sp.width as f32, sp.y], [sp.x + sp.width as f32, sp.y + sp.height as f32], [sp.x, sp.y + sp.height as f32]];
        let pts: Vec<[f32; 2]> = corners.iter().filter_map(|p| view.screen(view.camera([p[0], p[1], 0.0])).map(|s| [s[0], s[1]])).collect();
        if pts.len() < 4 {
            continue;
        }
        let (top, bottom) = pts.iter().fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[1]), b.max(p[1])));
        faces.push(PreparedFace {
            edges: (0..4).map(|k| (pts[k], pts[(k + 1) % 4])).collect(),
            normal: apply(&view.rot, [0.0, 0.0, -1.0]),
            point: view.camera([0.0, 0.0, 0.0]),
            base: face_c,
            rows: ((top.floor() as i32).max(0), (bottom.ceil() as i32).min(bh as i32)),
            sprite: Some(sp),
        });
    }

    // The samples, a row band at a time: each band its own depth.
    const BAND: usize = 16;
    let samples: Vec<(Vec<f32>, Vec<[f32; 3]>)> = (0..bh.div_ceil(BAND))
        .into_par_iter()
        .map(|band| {
            let (r0, r1) = (band * BAND, ((band + 1) * BAND).min(bh));
            let mut depth = vec![f32::INFINITY; (r1 - r0) * bw];
            let mut colour = vec![[0.0f32; 3]; (r1 - r0) * bw];
            let view_dir = |p: V3| if focal.is_some() { norm([-p[0], -p[1], -p[2]]) } else { [0.0, 0.0, -1.0] };
            for tri in tris.iter().filter(|t| t.rows.0 < r1 as i32 && t.rows.1 >= r0 as i32) {
                let [a, b, c] = tri.s;
                let area = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
                if area.abs() < 1e-9 {
                    continue;
                }
                let xa = (a[0].min(b[0]).min(c[0]).floor() as i64).max(0) as usize;
                let xb = ((a[0].max(b[0]).max(c[0]).ceil() as i64).max(0) as usize).min(bw);
                for row in r0.max(tri.rows.0.max(0) as usize)..r1.min(tri.rows.1.max(0) as usize + 1) {
                    let py = row as f32 + 0.5;
                    for col in xa..xb {
                        let px = col as f32 + 0.5;
                        let w0 = ((b[0] - px) * (c[1] - py) - (b[1] - py) * (c[0] - px)) / area;
                        let w1 = ((c[0] - px) * (a[1] - py) - (c[1] - py) * (a[0] - px)) / area;
                        let w2 = 1.0 - w0 - w1;
                        if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                            continue;
                        }
                        let inv = if focal.is_some() { w0 / a[2] + w1 / b[2] + w2 / c[2] } else { 1.0 };
                        let z = if focal.is_some() { 1.0 / inv } else { w0 * a[2] + w1 * b[2] + w2 * c[2] };
                        let i = (row - r0) * bw + col;
                        if z >= depth[i] {
                            continue;
                        }
                        let k = if focal.is_some() { z } else { 1.0 };
                        let mix = |v: [V3; 3]| [0, 1, 2].map(|d| (w0 * v[0][d] + w1 * v[1][d] + w2 * v[2][d]) * k);
                        let (n, p) = (mix(tri.n_over_z), mix(tri.p_over_z));
                        depth[i] = z;
                        colour[i] = light.shade(tri.base, n, view_dir(p));
                    }
                }
            }
            for f in faces.iter().filter(|f| f.rows.0 < r1 as i32 && f.rows.1 >= r0 as i32) {
                let mut xs: Vec<(f32, i32)> = Vec::new();
                for row in r0..r1 {
                    let py = row as f32 + 0.5;
                    xs.clear();
                    for &(a, b) in &f.edges {
                        if (a[1] <= py) != (b[1] <= py) {
                            let x = a[0] + (py - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
                            xs.push((x, if b[1] > a[1] { 1 } else { -1 }));
                        }
                    }
                    if xs.is_empty() {
                        continue;
                    }
                    xs.sort_by(|p, q| p.0.total_cmp(&q.0));
                    let mut wind = 0;
                    for k in 0..xs.len() - 1 {
                        wind += xs[k].1;
                        if wind == 0 {
                            continue;
                        }
                        let (xa, xb) = (((xs[k].0 - 0.5).ceil().max(0.0)) as usize, ((xs[k + 1].0 - 0.5).ceil().max(0.0) as usize).min(bw));
                        for col in xa..xb {
                            let (o, d) = view.ray(col as f32 + 0.5, py);
                            let denom = dot(f.normal, d);
                            if denom.abs() < 1e-9 {
                                continue;
                            }
                            let tt = dot(f.normal, sub(f.point, o)) / denom;
                            let p = [o[0] + d[0] * tt, o[1] + d[1] * tt, o[2] + d[2] * tt];
                            let z = p[2];
                            let i = (row - r0) * bw + col;
                            if z >= depth[i] {
                                continue;
                            }
                            if let Some(sp) = f.sprite {
                                // The bitmap's pixel where the plane is hit.
                                let o = view.object(p);
                                let (u, v) = ((o[0] - sp.x).floor(), (o[1] - sp.y).floor());
                                if u < 0.0 || v < 0.0 || u as usize >= sp.width || v as usize >= sp.height {
                                    continue;
                                }
                                let k = ((v as usize) * sp.width + u as usize) * 4;
                                if sp.rgba[k + 3] < 128 {
                                    continue;
                                }
                                depth[i] = z;
                                colour[i] = [to_linear(sp.rgba[k]), to_linear(sp.rgba[k + 1]), to_linear(sp.rgba[k + 2])];
                                continue;
                            }
                            depth[i] = z;
                            colour[i] = light.shade(f.base, f.normal, view_dir(p));
                        }
                    }
                }
            }
            (depth, colour)
        })
        .collect();

    // Each pixel the mean of its samples: the covered ones' colour, the
    // share covered its alpha.
    let ssu = ss as usize;
    for py in y0..y1 {
        for px in x0..x1 {
            let (mut sum, mut n) = ([0.0f32; 3], 0usize);
            for sy in 0..ssu {
                let row = (py - y0) * ssu + sy;
                let (band, r) = (row / BAND, row % BAND);
                let (depth, colour) = &samples[band];
                for sx in 0..ssu {
                    let i = r * bw + (px - x0) * ssu + sx;
                    if depth[i].is_finite() {
                        let c = colour[i];
                        sum = [sum[0] + c[0], sum[1] + c[1], sum[2] + c[2]];
                        n += 1;
                    }
                }
            }
            if n > 0 {
                let o = (py * w + px) * 4;
                let k = 1.0 / n as f32;
                out[o..o + 4].copy_from_slice(&[to_srgb(sum[0] * k), to_srgb(sum[1] * k), to_srgb(sum[2] * k), ((n as f32 / (ssu * ssu) as f32) * 255.0).round() as u8]);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text3d_mesh::{contours_of, extrude};

    /// A square slab 200 px a side, 40 deep, at (300..500, 200..400).
    fn slab() -> GlyphMesh {
        let square = contours_of(vec![vec![[300.0, 200.0], [500.0, 200.0], [500.0, 400.0], [300.0, 400.0]]]);
        extrude(&square, 40.0, None, None, 25.0)
    }

    fn draw(turn_y: f32, light_azimuth: f32, light_elevation: f32) -> Vec<u8> {
        let mesh = [slab()];
        let mut t = Text3d { front_bevel: None, rotation: [0.0, turn_y, 0.0], ..Default::default() };
        t.light.azimuth = light_azimuth;
        t.light.elevation = light_elevation;
        t.light.specular = 0.0;
        let scene = Scene { meshes: &mesh, sprites: &[], settings: &t, pivot: [400.0, 300.0, 20.0], face: [200, 200, 200, 255], side: [200, 200, 200, 255], bevel: [200, 200, 200, 255] };
        render(&scene, 800, 600, 2)
    }

    fn at(px: &[u8], x: usize, y: usize) -> [u8; 4] {
        let i = (y * 800 + x) * 4;
        [px[i], px[i + 1], px[i + 2], px[i + 3]]
    }

    /// Unturned, the front face covers just what the flat square would,
    /// solid, its edges partly covered.
    #[test]
    fn unturned_the_front_face_is_the_flat_shape() {
        let px = draw(0.0, -90.0, 90.0);
        assert_eq!(at(&px, 400, 300)[3], 255);
        assert_eq!(at(&px, 302, 202)[3], 255);
        assert_eq!(at(&px, 297, 300)[3], 0, "nothing outside the square");
        assert_eq!(at(&px, 503, 300)[3], 0);
        let covered = px.chunks_exact(4).filter(|p| p[3] > 0).count();
        assert!(covered.abs_diff(200 * 200) < 900, "{covered}");
    }

    /// Lit head on, the face is brightest; lit from the side it is
    /// darker, by the angle.
    #[test]
    fn the_face_is_lit_by_the_angle_to_the_light() {
        let head_on = at(&draw(0.0, 0.0, 90.0), 400, 300)[0];
        let slanted = at(&draw(0.0, 0.0, 30.0), 400, 300)[0];
        let grazing = at(&draw(0.0, 0.0, 0.0), 400, 300)[0];
        assert!(head_on > slanted && slanted > grazing, "{head_on} {slanted} {grazing}");
    }

    /// Turned in y under perspective, the square narrows, its near edge
    /// grows taller than its far one, and a wall shows.
    #[test]
    fn turned_it_narrows_in_perspective_and_shows_a_wall() {
        let px = draw(50.0, -90.0, 45.0);
        let cols: Vec<usize> = (0..800).filter(|&x| (0..600).any(|y| at(&px, x, y)[3] > 128)).collect();
        let (l, r) = (*cols.first().unwrap(), *cols.last().unwrap());
        assert!(r - l < 200 && r - l > 100, "{l}..{r}");
        let tall = |x: usize| (0..600).filter(|&y| at(&px, x, y)[3] > 128).count();
        let (left, right) = (tall(l + 3), tall(r - 3));
        assert!(left.abs_diff(right) > 10, "one edge nearer: {left} vs {right}");
    }

    /// A colour emoji in a 3D text is a flat sprite on the front plane:
    /// drawn in its colours, and turned with the text (narrower turned).
    /// Skips where the system has no emoji font with colour bitmaps.
    #[test]
    fn a_colour_emoji_is_a_flat_sprite_that_turns_with_the_text() {
        let has = crate::fallback::system_font_for('\u{1f308}', true).and_then(|path| {
            let font = crate::shaping::LoadedFont::new(std::borrow::Cow::Owned(std::fs::read(&path).ok()?))?;
            font.colour_glyph(font.font.lookup_glyph_index('\u{1f308}'), 48.0).map(|_| ())
        });
        if has.is_none() {
            eprintln!("skipped: no system emoji font with colour bitmaps has the rainbow");
            return;
        }
        let draw = |turn: f32| {
            let t = Text3d { rotation: [0.0, turn, 0.0], ..Default::default() };
            let tb = klipp_timeline::TextBox { text: "A \u{1f308}".into(), font_size: 200.0, three_d: Some(t), ..Default::default() };
            crate::TitleRenderer::new().render_with_window(&klipp_timeline::TitleData::single_text(tb), 1280, 720, None, 1.0, Some(4.0))
        };
        // The colourful pixels (the text is white, its walls grey).
        let colourful = |buf: &[u8]| -> Vec<usize> {
            buf.chunks_exact(4)
                .enumerate()
                .filter(|(_, p)| p[3] > 200 && p[0].max(p[1]).max(p[2]) - p[0].min(p[1]).min(p[2]) > 60)
                .map(|(i, _)| i % 1280)
                .collect()
        };
        let (flat, turned) = (colourful(&draw(0.0)), colourful(&draw(50.0)));
        assert!(flat.len() > 400, "the rainbow is drawn in colour: {}", flat.len());
        let span = |xs: &[usize]| xs.iter().max().unwrap() - xs.iter().min().unwrap();
        assert!(!turned.is_empty() && span(&turned) < span(&flat), "it turns with the text: {} then {}", span(&flat), span(&turned));
    }
}
