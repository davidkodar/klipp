//! Posed piece transitions (#985, phase 2): pieces of the picture that
//! tilt away in depth, turn, fall or shrink, drawn forward, front to
//! back, with soft shadows under them and motion blur across the shutter.
//!
//! The strips, panels and halves that only slide are inverse-mapped in the
//! transition shader (phase 1). A piece that turns or tilts has no closed
//! form inverse and overlaps others, so here every piece is a convex
//! polygon of the picture with a pose: a homography from the frame to the
//! screen, built from a 3D rotation, a scale and a move seen through a
//! perspective camera. [`plan`] works the pieces and their poses out once
//! a frame, on the CPU, for both renderers; `SHADER_PIECES` in `gpu.rs`
//! and [`render`] here then do the same per-pixel work with them: each
//! pixel looks through the pieces nearest first, maps itself back into
//! each one's picture, and takes it where it falls inside, with a pixel's
//! width of soft edge once the piece has moved.
//!
//! The settings are the piece transitions' four parameter slots
//! (`klipp_core::piece_transition::PieceParams` packs them); this file
//! reads the same bits the shader does.

use rayon::prelude::*;

/// Floats a piece takes in the plan.
pub const STRIDE: usize = 60;
/// The most edges a piece has.
pub const MAX_PLANES: usize = 12;
/// The most pieces a transition is cut into.
pub const MAX_PIECES: usize = 128;
/// The most instants a frame's motion blur blends.
pub const MAX_SAMPLES: u32 = 8;

/// Where each of a piece's values sits in its `STRIDE` floats.
pub const O_HINV: usize = 0; // 9: the screen-to-picture homography, row by row
pub const O_BOX: usize = 9; // 4: its box on screen (min x, min y, max x, max y)
pub const O_JAC: usize = 13; // picture pixels to a screen pixel, near its middle
pub const O_SIDE: usize = 14; // 0 the outgoing picture, 1 the incoming
pub const O_SHADE: usize = 15; // its light, turned away
pub const O_OPACITY: usize = 16;
pub const O_SHADOW: usize = 17; // 4: the shadow's offset x, y, its strength and softness
pub const O_AA: usize = 21; // the soft edge's width, in screen pixels (0: a hard edge)
pub const O_MATTE: usize = 22; // the subject matte's part (#985 phase 3): 1 the subject, -1 the rest of its side, 0 none
pub const O_PLANES: usize = 24; // 12 × 3: its edges, a x + b y + c >= 0 inside, in picture pixels

/// The piece kinds' ids in the transition registry.
pub const STRIPS: u32 = 68;
pub const PANELS: u32 = 69;
pub const HALVES: u32 = 70;
pub const SHARDS: u32 = 71;

/// The packed settings in `param3` (`PieceParams`).
pub fn bits(param3: f32) -> u32 {
    (param3.max(0.0) + 0.5) as u32
}

/// How the pieces move: 0 slide, 1 tilt, 2 turn, 3 fall, 4 shrink.
pub fn motion(param3: f32) -> u32 {
    (bits(param3) >> 12) & 7
}

/// Whether the transition `kind_id` with `param3` draws posed pieces
/// (here) rather than sliding ones (the transition shader).
pub fn is_posed(kind_id: u32, param3: f32) -> bool {
    kind_id == SHARDS || ((STRIPS..=HALVES).contains(&kind_id) && motion(param3) != 0)
}

/// Whether the transition draws posed pieces with `subject` (see
/// [`plan_with`]; 0 none) cut out: a subject cut out is always posed,
/// sliding included (#985 phase 3).
pub fn is_posed_with(kind_id: u32, param3: f32, subject: u32) -> bool {
    is_posed(kind_id, param3) || ((STRIPS..=SHARDS).contains(&kind_id) && subject != 0)
}

/// The pieces of a frame, posed, for [`render`] and the shader.
#[derive(Debug, Clone, PartialEq)]
pub struct PiecePlan {
    pub width: u32,
    pub height: u32,
    /// Pieces an instant.
    pub count: u32,
    /// Instants blended (motion blur): `count` pieces each, nearest first.
    pub samples: u32,
    /// What shows where no piece is: the incoming picture (pieces
    /// leaving), or the outgoing (pieces arriving).
    pub background_to: bool,
    pub data: Vec<f32>,
    /// How the subject moves (see [`plan_with`]); 0 none.
    pub subject: u32,
    /// The subject side's matte, the frame's size, one value a pixel:
    /// the outgoing picture's when pieces leave, the incoming's when they
    /// assemble ([`PiecePlan::matte_side_to`]). With none, the subject
    /// isn't seen and its side's pieces are whole: the plain pieces.
    pub matte: Option<Vec<f32>>,
}

impl PiecePlan {
    /// Whether the subject is the incoming picture's (pieces assembling)
    /// rather than the outgoing's.
    pub fn matte_side_to(&self) -> bool {
        !self.background_to
    }

    /// The matte the subject piece reads, as the renderers do: `None`
    /// with no subject, an empty one (nothing is subject) without a
    /// matte.
    fn matte_for(&self) -> Option<&[f32]> {
        if self.subject == 0 {
            return None;
        }
        Some(self.matte.as_deref().unwrap_or(&[]))
    }
}

fn hash_u(v: u32) -> u32 {
    let mut x = v;
    x = ((x >> 16) ^ x).wrapping_mul(0x45d9f3b);
    x = ((x >> 16) ^ x).wrapping_mul(0x45d9f3b);
    (x >> 16) ^ x
}

/// The transition shader's `rand3`, so a random order is the one the
/// sliding pieces take.
fn rand3(a: u32, b: u32, c: u32) -> f32 {
    hash_u(a ^ hash_u(b ^ hash_u(c))) as f32 / 4294967295.0
}

type P2 = [f64; 2];

/// `poly` cut to where `a x + b y + c >= 0`.
fn clip(poly: &[P2], a: f64, b: f64, c: f64) -> Vec<P2> {
    let mut out = Vec::with_capacity(poly.len() + 1);
    for i in 0..poly.len() {
        let (p, q) = (poly[i], poly[(i + 1) % poly.len()]);
        let (dp, dq) = (a * p[0] + b * p[1] + c, a * q[0] + b * q[1] + c);
        if dp >= 0.0 {
            out.push(p);
        }
        if (dp >= 0.0) != (dq >= 0.0) {
            let t = dp / (dp - dq);
            out.push([p[0] + (q[0] - p[0]) * t, p[1] + (q[1] - p[1]) * t]);
        }
    }
    out
}

fn centroid(poly: &[P2]) -> P2 {
    let (mut a, mut cx, mut cy) = (0.0, 0.0, 0.0);
    for i in 0..poly.len() {
        let (p, q) = (poly[i], poly[(i + 1) % poly.len()]);
        let cross = p[0] * q[1] - q[0] * p[1];
        a += cross;
        cx += (p[0] + q[0]) * cross;
        cy += (p[1] + q[1]) * cross;
    }
    if a.abs() < 1e-9 {
        let n = poly.len().max(1) as f64;
        return [poly.iter().map(|p| p[0]).sum::<f64>() / n, poly.iter().map(|p| p[1]).sum::<f64>() / n];
    }
    [cx / (3.0 * a), cy / (3.0 * a)]
}

/// One piece before it is posed.
struct Piece {
    poly: Vec<P2>,
    centre: P2,
    /// Its way out (unit).
    dir: P2,
    /// Its place in the order (0 first).
    key: f32,
    id: u32,
}

/// The settings, unpacked.
#[derive(Clone)]
struct Settings {
    order: u32,
    ease: u32,
    assemble: bool,
    alternate: bool,
    gap: f64,
    motion: u32,
    shadow: bool,
    seed: u32,
    stagger: f32,
}

fn settings(p: [f32; 4]) -> Settings {
    let b = bits(p[3]);
    Settings {
        order: b & 7,
        ease: (b >> 3) & 3,
        assemble: b & 32 != 0,
        alternate: b & 64 != 0,
        gap: ((b >> 7) & 31) as f64,
        motion: (b >> 12) & 7,
        shadow: b & (1 << 15) != 0,
        seed: (b >> 16) & 255,
        stagger: p[2].clamp(0.0, 0.9),
    }
}

/// The transition shader's `piece_key`.
fn key(s: &Settings, id: u32, along: f32, centre: f32) -> f32 {
    match s.order {
        1 => 1.0 - along,
        2 => centre,
        3 => 1.0 - centre,
        4 => rand3(id, 985, 3),
        5 => 0.0,
        _ => along,
    }
}

/// The transition shader's `piece_travel`.
fn travel(s: &Settings, progress: f32, k: f32) -> f32 {
    let t = ((progress - s.stagger * k) / (1.0 - s.stagger)).clamp(0.0, 1.0);
    match s.ease {
        1 => t,
        2 => t * t,
        3 => {
            let u = t - 1.0;
            1.0 + 2.70158 * u * u * u + 1.70158 * u * u
        }
        _ => t * t * (3.0 - 2.0 * t),
    }
}

/// The frame cut into `kind`'s pieces, each with its way out and its
/// place in the order.
fn cut(kind: u32, p: [f32; 4], s: &Settings, w: f64, h: f64) -> Vec<Piece> {
    let frame: Vec<P2> = vec![[0.0, 0.0], [w, 0.0], [w, h], [0.0, h]];
    let c = [w * 0.5, h * 0.5];
    let ang = (p[1] * 0.017453292) as f64;
    let m = [ang.cos(), ang.sin()];
    let nrm = [-m[1], m[0]];
    let mut out = Vec::new();
    let mut push = |poly: Vec<P2>, dir: P2, key: f32, id: u32| {
        if poly.len() >= 3 && out.len() < MAX_PIECES {
            let centre = centroid(&poly);
            out.push(Piece { poly, centre, dir, key, id });
        }
    };
    match kind {
        STRIPS => {
            let n = (p[0].round_ties_even().clamp(1.0, 64.0)) as usize;
            let ln = (w * m[1]).abs() + (h * m[0]).abs();
            let bw = ln / n as f64;
            let mid = (n as f32 - 1.0) * 0.5;
            for i in 0..n {
                let (v0, v1) = (-ln * 0.5 + i as f64 * bw, -ln * 0.5 + (i + 1) as f64 * bw);
                // v = (P - c) · nrm, between v0 and v1.
                let k0 = -(c[0] * nrm[0] + c[1] * nrm[1]);
                let poly = clip(&clip(&frame, nrm[0], nrm[1], k0 - v0), -nrm[0], -nrm[1], v1 - k0);
                let sgn = if s.alternate && i % 2 == 1 { -1.0 } else { 1.0 };
                let fi = i as f32;
                push(poly, [m[0] * sgn, m[1] * sgn], key(s, i as u32, fi / (n as f32 - 1.0).max(1.0), (fi - mid).abs() / mid.max(0.5)), i as u32);
            }
        }
        PANELS => {
            let rows = (p[0].round_ties_even().clamp(1.0, 8.0)) as usize;
            let cols = ((rows as f64 * w / h).round().clamp(1.0, 16.0)) as usize;
            let q4 = (p[1] / 90.0).round_ties_even();
            let quarter = (q4 - 4.0 * (q4 / 4.0).floor()) as u32;
            let vertical = quarter == 1 || quarter == 3;
            let dir = if quarter >= 2 { -1.0 } else { 1.0 };
            let (pw, ph) = (w / cols as f64, h / rows as f64);
            let (na, nb) = if vertical { (rows, cols) } else { (cols, rows) };
            for r in 0..rows {
                for j in 0..cols {
                    let poly = vec![[j as f64 * pw, r as f64 * ph], [(j + 1) as f64 * pw, r as f64 * ph], [(j + 1) as f64 * pw, (r + 1) as f64 * ph], [j as f64 * pw, (r + 1) as f64 * ph]];
                    // Along the move and across it, as the sliding panels.
                    let (ia, ib) = if vertical { (r, j) } else { (j, r) };
                    let sgn = if s.alternate && ib % 2 == 1 { -dir } else { dir };
                    let mut along = ia as f32 / (na as f32 - 1.0).max(1.0);
                    if sgn > 0.0 {
                        along = 1.0 - along;
                    }
                    let across = ib as f32 / (nb as f32 - 1.0).max(1.0);
                    let (ca, cb) = ((ia as f32 + 0.5) / na as f32 - 0.5, (ib as f32 + 0.5) / nb as f32 - 0.5);
                    let centre = ((ca * ca + cb * cb).sqrt() / 0.7072).clamp(0.0, 1.0);
                    let d = if vertical { [0.0, sgn] } else { [sgn, 0.0] };
                    push(poly, d, key(s, (ib * 32 + ia) as u32, (along + across) * 0.5, centre), (ib * 32 + ia) as u32);
                }
            }
        }
        HALVES => {
            // The line through the middle at the angle; its normal is
            // (-sin, cos), as the sliding halves have it.
            let shear = p[0] > 0.5;
            for (side, i) in [(-1.0f64, 0u32), (1.0, 1)] {
                let k0 = -(c[0] * nrm[0] + c[1] * nrm[1]);
                let poly = clip(&frame, nrm[0] * side, nrm[1] * side, k0 * side);
                let d = if shear { [m[0] * side, m[1] * side] } else { [nrm[0] * side, nrm[1] * side] };
                push(poly, d, key(s, i, i as f32, 0.0), i);
            }
        }
        _ => {
            // Shards: a Voronoi cut of a jittered grid of sites.
            let n = p[0].round_ties_even().clamp(4.0, 96.0) as f64;
            let cols = ((n * w / h).sqrt().round()).max(1.0) as usize;
            let rows = ((n / cols as f64).round()).max(1.0) as usize;
            let mut sites = Vec::with_capacity(rows * cols);
            for j in 0..rows {
                for i in 0..cols {
                    let (jx, jy) = (rand3(i as u32, j as u32, s.seed * 7 + 1) as f64 - 0.5, rand3(i as u32, j as u32, s.seed * 7 + 2) as f64 - 0.5);
                    sites.push([(i as f64 + 0.5 + jx * 0.7) * w / cols as f64, (j as f64 + 0.5 + jy * 0.7) * h / rows as f64]);
                }
            }
            let reach = 2.5 * (w / cols as f64).max(h / rows as f64);
            let projections: Vec<f64> = sites.iter().map(|q| (q[0] - c[0]) * m[0] + (q[1] - c[1]) * m[1]).collect();
            let (lo, hi) = projections.iter().fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
            let far = sites.iter().map(|q| ((q[0] - c[0]).powi(2) + (q[1] - c[1]).powi(2)).sqrt()).fold(1.0, f64::max);
            for (k, site) in sites.iter().enumerate() {
                let mut poly = frame.clone();
                for (o, other) in sites.iter().enumerate() {
                    if o == k || (other[0] - site[0]).abs() > reach || (other[1] - site[1]).abs() > reach {
                        continue;
                    }
                    // Nearer this site than the other.
                    let (dx, dy) = (other[0] - site[0], other[1] - site[1]);
                    let cc = (other[0] * other[0] + other[1] * other[1] - site[0] * site[0] - site[1] * site[1]) * 0.5;
                    poly = clip(&poly, -dx, -dy, cc);
                }
                // Outwards from the middle, pushed the way the angle says.
                let (ox, oy) = (site[0] - c[0] + m[0] * 0.25 * w.min(h), site[1] - c[1] + m[1] * 0.25 * w.min(h));
                let len = (ox * ox + oy * oy).sqrt().max(1e-6);
                let along = 1.0 - ((projections[k] - lo) / (hi - lo).max(1e-6)) as f32;
                let centre = (((site[0] - c[0]).powi(2) + (site[1] - c[1]).powi(2)).sqrt() / far) as f32;
                push(poly, [ox / len, oy / len], key(s, k as u32, along, centre), k as u32);
            }
        }
    }
    out
}

type M3 = [[f64; 3]; 3];

fn mul(a: &M3, b: &M3) -> M3 {
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    r
}

/// A turn of `angle` about the unit axis `(x, y, z)`.
fn rotation(x: f64, y: f64, z: f64, angle: f64) -> M3 {
    let (s, c) = angle.sin_cos();
    let t = 1.0 - c;
    [[t * x * x + c, t * x * y - s * z, t * x * z + s * y], [t * x * y + s * z, t * y * y + c, t * y * z - s * x], [t * x * z - s * y, t * y * z + s * x, t * z * z + c]]
}

fn inverse(m: &M3) -> Option<M3> {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0]) + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if det.abs() < 1e-12 {
        return None;
    }
    let d = 1.0 / det;
    Some([
        [(m[1][1] * m[2][2] - m[1][2] * m[2][1]) * d, (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * d, (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * d],
        [(m[1][2] * m[2][0] - m[1][0] * m[2][2]) * d, (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * d, (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * d],
        [(m[1][0] * m[2][1] - m[1][1] * m[2][0]) * d, (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * d, (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * d],
    ])
}

fn apply(h: &M3, p: P2) -> Option<P2> {
    let wz = h[2][0] * p[0] + h[2][1] * p[1] + h[2][2];
    (wz > 1e-9).then(|| [(h[0][0] * p[0] + h[0][1] * p[1] + h[0][2]) / wz, (h[1][0] * p[0] + h[1][1] * p[1] + h[1][2]) / wz])
}

fn smoothstep(lo: f64, hi: f64, x: f64) -> f64 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Piece `pc` posed `t` of the way out (0 at rest; past 1 or below 0 with
/// an overshoot), as its `STRIDE` floats and its depth; `None` when it
/// can't be seen.
fn pose(pc: &Piece, s: &Settings, t: f64, e: f64, side: f32, w: f64, h: f64) -> Option<([f32; STRIDE], f64)> {
    let f = 2.0 * w.max(h);
    let (cx, cy) = (w * 0.5, h * 0.5);
    let r1 = rand3(pc.id, s.seed, 21) as f64;
    let r2 = rand3(pc.id, s.seed, 22) as f64;
    let r3 = rand3(pc.id, s.seed, 23) as f64;
    let d = pc.dir;
    // The piece's reach along its way out, either side of its middle.
    let proj: Vec<f64> = pc.poly.iter().map(|q| (q[0] - pc.centre[0]) * d[0] + (q[1] - pc.centre[1]) * d[1]).collect();
    let back = proj.iter().cloned().fold(f64::MAX, f64::min);
    let radius = pc.poly.iter().map(|q| ((q[0] - pc.centre[0]).powi(2) + (q[1] - pc.centre[1]).powi(2)).sqrt()).fold(0.0, f64::max);
    // Far enough along `d` for the piece, turned any way, to have left
    // the frame: from its middle less its reach to the frame's far side.
    let far = [[0.0, 0.0], [w, 0.0], [0.0, h], [w, h]].iter().map(|q: &P2| q[0] * d[0] + q[1] * d[1]).fold(f64::MIN, f64::max);
    let out = far - (pc.centre[0] * d[0] + pc.centre[1] * d[1] - radius) + 4.0;
    let tt = t.clamp(-0.5, 1.5);
    let mut pivot = pc.centre;
    let mut rot: M3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let mut scale = 1.0;
    let mut mv = [0.0, 0.0, 0.0];
    let mut fade = 1.0;
    match s.motion {
        1 => {
            // Tilt: swung away about its trailing edge, a quarter turn
            // into the picture, drifting on.
            pivot = [pc.centre[0] + d[0] * back, pc.centre[1] + d[1] * back];
            rot = rotation(-d[1], d[0], 0.0, -tt * std::f64::consts::FRAC_PI_2);
            mv = [d[0] * tt * 0.2 * out, d[1] * tt * 0.2 * out, tt * 0.15 * f];
            fade = 1.0 - smoothstep(0.8, 1.0, tt);
        }
        2 => {
            // Turn: spun on its middle, shrinking a little, flung out.
            let spin = if r2 < 0.5 { -1.0 } else { 1.0 } * (0.4 + 0.8 * r1) * std::f64::consts::PI;
            rot = rotation(0.0, 0.0, 1.0, spin * tt);
            scale = 1.0 - 0.35 * tt;
            mv = [d[0] * tt * out, d[1] * tt * out, 0.0];
            fade = 1.0 - smoothstep(0.85, 1.0, tt);
        }
        3 => {
            // Fall: pushed out, then down under gravity, tumbling.
            let axis = r2 * std::f64::consts::TAU;
            let tumble = rotation(axis.cos(), axis.sin(), 0.0, (0.3 + 0.5 * r3) * std::f64::consts::PI * tt);
            rot = mul(&rotation(0.0, 0.0, 1.0, (r1 * 2.0 - 1.0) * 0.7 * std::f64::consts::PI * tt), &tumble);
            let drop = tt * tt.abs() * (h + 2.0 * radius) * 1.2;
            mv = [d[0] * tt * 0.25 * out, d[1] * tt * 0.25 * out + drop, tt * 0.1 * f];
            fade = 1.0 - smoothstep(0.85, 1.0, tt);
        }
        4 => {
            // Shrink: to nothing on its middle, turning a little.
            rot = rotation(0.0, 0.0, 1.0, (r1 * 2.0 - 1.0) * 0.25 * std::f64::consts::PI * tt);
            scale = 1.0 - tt;
            if scale <= 1e-3 {
                return None;
            }
        }
        5 => {
            // The subject stepping forward (#985 phase 3): larger, in
            // place, `t` below 0.
            scale = 1.0 - tt;
        }
        _ => {
            // Slide (the shards only come here sliding).
            mv = [d[0] * tt * out, d[1] * tt * out, 0.0];
        }
    }
    if fade <= 1e-4 {
        return None;
    }
    // P = scale R (X - pivot) + pivot + move, seen from a camera `f`
    // before the frame: the screen point is c + f (P - c) / (f + P.z).
    let r = rot;
    let t3 = [
        pivot[0] + mv[0] - scale * (r[0][0] * pivot[0] + r[0][1] * pivot[1]),
        pivot[1] + mv[1] - scale * (r[1][0] * pivot[0] + r[1][1] * pivot[1]),
        mv[2] - scale * (r[2][0] * pivot[0] + r[2][1] * pivot[1]),
    ];
    let hm: M3 = [
        [f * scale * r[0][0] + cx * scale * r[2][0], f * scale * r[0][1] + cx * scale * r[2][1], f * t3[0] + cx * t3[2]],
        [f * scale * r[1][0] + cy * scale * r[2][0], f * scale * r[1][1] + cy * scale * r[2][1], f * t3[1] + cy * t3[2]],
        [scale * r[2][0], scale * r[2][1], f + t3[2]],
    ];
    let hinv = inverse(&hm)?;
    // Its box on screen, and how many picture pixels a screen pixel
    // spans near its middle (for the soft edge).
    let mut bx = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for q in &pc.poly {
        let sq = apply(&hm, *q)?;
        bx = [bx[0].min(sq[0]), bx[1].min(sq[1]), bx[2].max(sq[0]), bx[3].max(sq[1])];
    }
    let c0 = apply(&hm, pc.centre)?;
    let (cxp, cyp) = (apply(&hm, [pc.centre[0] + 1.0, pc.centre[1]])?, apply(&hm, [pc.centre[0], pc.centre[1] + 1.0])?);
    let det = ((cxp[0] - c0[0]) * (cyp[1] - c0[1]) - (cxp[1] - c0[1]) * (cyp[0] - c0[0])).abs();
    if det < 1e-6 {
        return None;
    }
    let jac = (1.0 / det.sqrt()).clamp(0.05, 8.0);
    // Lit by how square it faces the camera; darker turned round.
    let nz = r[2][2];
    let shade = (0.55 + 0.45 * nz.abs()) * if nz < 0.0 { 0.7 } else { 1.0 };
    let moved = tt.abs().min(1.0);
    let gh = s.gap * 0.5 * (8.0 * e * (1.0 - e)).clamp(0.0, 1.0);
    let aa = (moved * 12.0).min(1.0);
    let lift = moved * w.min(h) * 0.05;
    let grow = 3.0 + aa + lift;
    if bx[2] + grow < 0.0 || bx[0] - grow > w || bx[3] + grow < 0.0 || bx[1] - grow > h {
        return None;
    }
    let mut v = [0.0f32; STRIDE];
    for i in 0..3 {
        for j in 0..3 {
            v[O_HINV + i * 3 + j] = hinv[i][j] as f32;
        }
    }
    let shadow_alpha = if s.shadow { 0.5 * (5.0 * moved).min(1.0) * fade } else { 0.0 };
    let shadow = [0.5 * lift, 0.85 * lift, shadow_alpha, 2.0 + lift * 0.8];
    let pad = 2.0 + aa + if shadow_alpha > 0.0 { shadow[3] + lift } else { 0.0 };
    v[O_BOX..O_BOX + 4].copy_from_slice(&[(bx[0] - pad) as f32, (bx[1] - pad) as f32, (bx[2] + pad) as f32, (bx[3] + pad) as f32]);
    v[O_JAC] = jac as f32;
    v[O_SIDE] = side;
    v[O_SHADE] = shade as f32;
    v[O_OPACITY] = fade as f32;
    v[O_SHADOW..O_SHADOW + 4].copy_from_slice(&shadow.map(|x| x as f32));
    v[O_AA] = aa as f32;
    // The edges, inward and normalised so a plane's value is the distance
    // inside it in picture pixels, moved in by half the gap. Unused ones
    // are always inside.
    let n = pc.poly.len().min(MAX_PLANES);
    for k in 0..MAX_PLANES {
        let o = O_PLANES + k * 3;
        if k >= n {
            v[o..o + 3].copy_from_slice(&[0.0, 0.0, 1.0e6]);
            continue;
        }
        let (p0, p1) = (pc.poly[k], pc.poly[(k + 1) % pc.poly.len()]);
        let (mut a, mut b) = (-(p1[1] - p0[1]), p1[0] - p0[0]);
        let len = (a * a + b * b).sqrt().max(1e-9);
        a /= len;
        b /= len;
        let mut c = -(a * p0[0] + b * p0[1]);
        if a * pc.centre[0] + b * pc.centre[1] + c < 0.0 {
            a = -a;
            b = -b;
            c = -c;
        }
        v[o..o + 3].copy_from_slice(&[a as f32, b as f32, (c - gh) as f32]);
    }
    // Depth: its middle's, nearest first; the one further on breaks a tie.
    let z = scale * (r[2][0] * (pc.centre[0] - pivot[0]) + r[2][1] * (pc.centre[1] - pivot[1])) + mv[2];
    Some((v, z - moved * 1e-3))
}

/// The pieces of transition `kind_id` with `p` at `progress`, posed, for
/// a `width` × `height` frame; with motion blur, `samples` instants across
/// `shutter` of the progress centred on it.
pub fn plan(kind_id: u32, progress: f32, width: u32, height: u32, p: [f32; 4], shutter: f32, samples: u32) -> PiecePlan {
    plan_with(kind_id, progress, width, height, p, 0, shutter, samples)
}

/// `t` of the way through `[lo, hi]`, held at either end.
fn window(t: f32, lo: f32, hi: f32) -> f32 {
    ((t - lo) / (hi - lo)).clamp(0.0, 1.0)
}

/// Where the background pieces and the subject move within the
/// transition, for pieces leaving (#985 phase 3): the subject staying
/// goes once the rest has nearly gone; going first, the rest follows it;
/// stepping forward, it grows while the rest leaves, then fades.
fn windows(subject: u32) -> ([f32; 2], [f32; 2]) {
    match subject {
        1 => ([0.0, 0.7], [0.6, 1.0]),
        2 => ([0.0, 0.75], [0.0, 0.75]),
        3 => ([0.3, 1.0], [0.0, 0.4]),
        _ => ([0.0, 1.0], [0.0, 1.0]),
    }
}

/// [`plan`] with the subject cut out as its own piece (#985 phase 3),
/// moving as `subject` says: 0 not cut out, 1 it stays and goes last, 2
/// it steps forward and fades, 3 it goes first. Its matte is put on the
/// plan by whoever has it.
#[allow(clippy::too_many_arguments)]
pub fn plan_with(kind_id: u32, progress: f32, width: u32, height: u32, p: [f32; 4], subject: u32, shutter: f32, samples: u32) -> PiecePlan {
    let s = settings(p);
    let (w, h) = (width as f64, height as f64);
    let pieces = cut(kind_id, p, &s, w, h);
    let samples = if shutter > 0.0 { samples.clamp(2, MAX_SAMPLES) } else { 1 };
    let side = if s.assemble { 1.0 } else { 0.0 };
    // The subject: the whole frame, kept to the matte as it is drawn.
    let ang = (p[1] * 0.017453292) as f64;
    let whole = Piece { poly: vec![[0.0, 0.0], [w, 0.0], [w, h], [0.0, h]], centre: [w * 0.5, h * 0.5], dir: [ang.cos(), ang.sin()], key: 0.0, id: 977 };
    let (bg_win, subject_win) = windows(subject);
    // Assembling runs the leaving windows backwards.
    let bg_win = if s.assemble { [1.0 - bg_win[1], 1.0 - bg_win[0]] } else { bg_win };
    let count = pieces.len() + usize::from(subject != 0);
    let mut data = Vec::with_capacity(samples as usize * count * STRIDE);
    for k in 0..samples {
        let at = if samples > 1 { (progress + shutter * (k as f32 / (samples - 1) as f32 - 0.5)).clamp(0.0, 1.0) } else { progress };
        let mut posed: Vec<([f32; STRIDE], f64)> = Vec::with_capacity(count);
        let bg_at = if subject != 0 { window(at, bg_win[0], bg_win[1]) } else { at };
        for pc in &pieces {
            let e = travel(&s, bg_at, pc.key) as f64;
            let t = if s.assemble { 1.0 - e } else { e };
            if let Some(mut v) = pose(pc, &s, t, e, side, w, h) {
                if subject != 0 {
                    v.0[O_MATTE] = -1.0;
                }
                posed.push(v);
            }
        }
        if subject != 0 {
            // Worked as if leaving: assembling is that, backwards.
            let u = if s.assemble { 1.0 - at } else { at };
            let local = window(u, subject_win[0], subject_win[1]);
            let still = Settings { stagger: 0.0, ..s.clone() };
            let posed_subject = if subject == 2 {
                let forward = Settings { motion: 5, shadow: true, ..still };
                let grow = travel(&forward, local, 0.0) as f64;
                pose(&whole, &forward, -0.12 * grow, 0.0, side, w, h).map(|(mut v, z)| {
                    v[O_OPACITY] *= 1.0 - smoothstep(0.75, 1.0, u as f64) as f32;
                    (v, z)
                })
            } else {
                let e = travel(&still, local, 0.0) as f64;
                pose(&whole, &still, e, e, side, w, h)
            };
            if let Some((mut v, _)) = posed_subject.filter(|(v, _)| v[O_OPACITY] > 1e-4) {
                v[O_MATTE] = 1.0;
                // Nearest of all, so the rest of its side is cut around it.
                posed.push((v, f64::MIN));
            }
        }
        posed.sort_by(|a, b| a.1.total_cmp(&b.1));
        // A piece not seen fills its place with an empty box.
        let mut hidden = [0.0f32; STRIDE];
        hidden[O_BOX..O_BOX + 4].copy_from_slice(&[1.0, 1.0, -1.0, -1.0]);
        for i in 0..count {
            data.extend_from_slice(posed.get(i).map(|v| &v.0[..]).unwrap_or(&hidden[..]));
        }
    }
    PiecePlan { width, height, count: count as u32, samples, background_to: !s.assemble, data, subject, matte: None }
}

/// The matte at picture point `(sx, sy)`, bilinear and clamped as the
/// pictures are; nothing where there is no matte.
fn matte_sample(m: &[f32], w: u32, h: u32, sx: f32, sy: f32) -> f32 {
    if m.len() < (w as usize) * (h as usize) {
        return 0.0;
    }
    let fx = sx.max(0.0).min(w as f32 - 1.0);
    let fy = sy.max(0.0).min(h as f32 - 1.0);
    let x0 = fx.floor() as u32;
    let y0 = fy.floor() as u32;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let tx = fx - x0 as f32;
    let ty = fy - y0 as f32;
    let at = |x: u32, y: u32| m[(y * w + x) as usize];
    let top = at(x0, y0) + (at(x1, y0) - at(x0, y0)) * tx;
    let bot = at(x0, y1) + (at(x1, y1) - at(x0, y1)) * tx;
    (top + (bot - top) * ty).clamp(0.0, 1.0)
}

/// The share of piece `d` the matte leaves at picture point `(sx, sy)`:
/// the matte for the subject, the rest for the other pieces of its side.
fn matte_share(d: &[f32], matte: Option<&[f32]>, w: u32, h: u32, sx: f32, sy: f32) -> f32 {
    match matte {
        Some(m) if d[O_MATTE] != 0.0 => {
            let v = matte_sample(m, w, h, sx - 0.5, sy - 0.5);
            if d[O_MATTE] > 0.0 { v } else { 1.0 - v }
        }
        _ => 1.0,
    }
}

/// A bilinear sample of `buf`, clamped to the frame: the shader's
/// `piece_sample`.
fn sample(buf: &[f32], w: u32, h: u32, sx: f32, sy: f32) -> [f32; 4] {
    let fx = sx.max(0.0).min(w as f32 - 1.0);
    let fy = sy.max(0.0).min(h as f32 - 1.0);
    let x0 = fx.floor() as u32;
    let y0 = fy.floor() as u32;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let tx = fx - x0 as f32;
    let ty = fy - y0 as f32;
    let at = |x: u32, y: u32| {
        let i = ((y * w + x) * 4) as usize;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    };
    let (c00, c10, c01, c11) = (at(x0, y0), at(x1, y0), at(x0, y1), at(x1, y1));
    let mut out = [0.0; 4];
    for k in 0..4 {
        let top = c00[k] + (c10[k] - c00[k]) * tx;
        let bot = c01[k] + (c11[k] - c01[k]) * tx;
        out[k] = top + (bot - top) * ty;
    }
    out
}

/// The distance inside piece `d`'s edges of the picture point the screen
/// point `(x, y)` maps to, in picture pixels, and that point; `None`
/// outside its box or behind the camera. The shader's `piece_at`.
fn piece_at(d: &[f32], x: f32, y: f32) -> Option<(f32, f32, f32)> {
    if x < d[O_BOX] || y < d[O_BOX + 1] || x > d[O_BOX + 2] || y > d[O_BOX + 3] {
        return None;
    }
    let wz = d[6] * x + d[7] * y + d[8];
    if wz <= 1e-9 {
        return None;
    }
    let sx = (d[0] * x + d[1] * y + d[2]) / wz;
    let sy = (d[3] * x + d[4] * y + d[5]) / wz;
    let mut sd = 1.0e6f32;
    for k in 0..MAX_PLANES {
        let o = O_PLANES + k * 3;
        sd = sd.min(d[o] * sx + d[o + 1] * sy + d[o + 2]);
    }
    Some((sd, sx, sy))
}

/// Coverage of a piece whose edge is `sd` picture pixels away: hard at
/// rest (judged a hair off the centre, as the sliding pieces are), a
/// pixel's width of soft edge once it moves.
fn coverage(d: &[f32], sd: f32) -> f32 {
    let aa = d[O_AA] * d[O_JAC];
    if aa <= 1e-4 {
        // At rest, neighbours share their edges: a hundredth of a pixel
        // over, so rounding never leaves a pixel between two.
        if sd >= -0.01 { 1.0 } else { 0.0 }
    } else {
        (sd / aa + 0.5).clamp(0.0, 1.0)
    }
}

/// One output pixel: the shader's `main`.
fn pixel(plan: &PiecePlan, from: &[f32], to: &[f32], idx: u32) -> [f32; 4] {
    let (w, h) = (plan.width, plan.height);
    let x = (idx % w) as f32 + 0.5;
    let y = (idx / w) as f32 + 0.5;
    // Judged a hair off the centre (see the sliding pieces).
    let (hx, hy) = (x + 0.001953125, y + 0.0029296875);
    let i4 = (idx * 4) as usize;
    let bg_src = if plan.background_to { to } else { from };
    let bg = [bg_src[i4], bg_src[i4 + 1], bg_src[i4 + 2], bg_src[i4 + 3]];
    let matte = plan.matte_for();
    let mut total = [0.0f32; 4];
    for s in 0..plan.samples {
        let mut acc = [0.0f32; 4];
        let mut alpha = 0.0f32;
        let mut shadow = 0.0f32;
        // The subject's share of this pixel: the rest of its side fills
        // what it leaves, so at rest the two make the picture whole.
        let mut subject_alpha = 0.0f32;
        for i in 0..plan.count {
            let base = ((s * plan.count + i) as usize) * STRIDE;
            let d = &plan.data[base..base + STRIDE];
            if alpha < 0.999 {
                if let Some((sd, _, _)) = piece_at(d, hx, hy) {
                    let cov = coverage(d, sd) * d[O_OPACITY];
                    if cov > 0.0 {
                        let (_, sx, sy) = piece_at(d, x, y).unwrap_or((sd, 0.0, 0.0));
                        let cov = cov * matte_share(d, matte, w, h, sx, sy);
                        let c = sample(if d[O_SIDE] > 0.5 { to } else { from }, w, h, sx - 0.5, sy - 0.5);
                        let k = if d[O_MATTE] < 0.0 && matte.is_some() {
                            ((1.0 - alpha) * cov / (1.0 - subject_alpha)).min(1.0 - alpha)
                        } else {
                            (1.0 - alpha) * cov
                        };
                        if d[O_MATTE] > 0.0 {
                            subject_alpha += k;
                        }
                        acc[0] += k * c[0] * d[O_SHADE];
                        acc[1] += k * c[1] * d[O_SHADE];
                        acc[2] += k * c[2] * d[O_SHADE];
                        acc[3] += k * c[3];
                        alpha += k;
                    }
                }
            }
            // Its shadow on what is behind every piece.
            if d[O_SHADOW + 2] > 0.0 {
                if let Some((sd, sx, sy)) = piece_at(d, hx - d[O_SHADOW], hy - d[O_SHADOW + 1]) {
                    let a = d[O_SHADOW + 2] * (sd / d[O_JAC] / d[O_SHADOW + 3] + 0.5).clamp(0.0, 1.0) * matte_share(d, matte, w, h, sx, sy);
                    shadow = 1.0 - (1.0 - shadow) * (1.0 - a);
                }
            }
        }
        let dark = 1.0 - shadow;
        let rest = 1.0 - alpha;
        total[0] += acc[0] + rest * bg[0] * dark;
        total[1] += acc[1] + rest * bg[1] * dark;
        total[2] += acc[2] + rest * bg[2] * dark;
        total[3] += acc[3] + rest * bg[3];
    }
    let n = plan.samples as f32;
    [total[0] / n, total[1] / n, total[2] / n, total[3] / n]
}

/// The posed pieces of `plan` between two linear-light RGBA frames, on
/// the CPU: the picture `SHADER_PIECES` draws on the GPU.
pub fn render(from: &[f32], to: &[f32], plan: &PiecePlan) -> Vec<f32> {
    let mut out = vec![0.0f32; (plan.width as usize) * (plan.height as usize) * 4];
    out.par_chunks_exact_mut(4).enumerate().for_each(|(i, o)| o.copy_from_slice(&pixel(plan, from, to, i as u32)));
    out
}
