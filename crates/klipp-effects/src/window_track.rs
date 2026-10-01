//! The window tracker (#238): follows what is under a grade window from
//! one frame to the next and says how it has moved, grown and turned, so
//! the window can follow a face that comes closer or a sign the camera
//! circles, not only one that crosses the frame.
//!
//! Points with texture inside the window are matched frame to frame, a
//! patch at a time to a fraction of a pixel, and a similarity — move,
//! scale, turn — is fitted from where each point was on the reference
//! frame to where it is now, by RANSAC, so a point that slid onto the
//! background or into a shadow is outvoted rather than followed. Points
//! lost along the way are replaced inside the window as it stands.
//!
//! For a window on a flat surface seen from an angle — a screen, a sign,
//! a wall the camera walks past — [`track_window_perspective`] fits a
//! full plane projective map instead, so the window's four corners each
//! go their own way as the surface turns away.
//!
//! Everything here is in the pixels of the frames given, grey, one byte
//! a pixel; the caller decodes them and maps the result back to the
//! window.

/// A grey frame, one byte a pixel, row by row.
#[derive(Debug, Clone)]
pub struct GrayFrame {
    pub data: Vec<u8>,
    pub width: usize,
    pub height: usize,
}

impl GrayFrame {
    fn at(&self, x: i32, y: i32) -> u8 {
        let x = x.clamp(0, self.width as i32 - 1) as usize;
        let y = y.clamp(0, self.height as i32 - 1) as usize;
        self.data[y * self.width + x]
    }

    /// Between pixels, bilinearly.
    fn sample(&self, x: f64, y: f64) -> f64 {
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as i32, y0 as i32);
        let p = |dx: i32, dy: i32| self.at(x0 + dx, y0 + dy) as f64;
        (p(0, 0) * (1.0 - fx) + p(1, 0) * fx) * (1.0 - fy) + (p(0, 1) * (1.0 - fx) + p(1, 1) * fx) * fy
    }
}

/// How the window has moved since the reference frame: its pivot moved
/// by `dx`, `dy` pixels, and it scaled by `scale` and turned `rotation`
/// radians (clockwise on a picture whose y runs down) about the pivot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowFit {
    pub dx: f64,
    pub dy: f64,
    pub scale: f64,
    pub rotation: f64,
}

impl WindowFit {
    pub const STILL: WindowFit = WindowFit { dx: 0.0, dy: 0.0, scale: 1.0, rotation: 0.0 };
}

/// What to solve and how hard to look.
#[derive(Debug, Clone, Copy)]
pub struct TrackOptions {
    /// How far a point may move from one frame to the next, in pixels.
    pub search_radius: i32,
    /// Half the side of a matched patch, in pixels.
    pub patch_half: i32,
    /// The mean difference per pixel, 0–255, past which a patch is lost.
    pub loss_threshold: u32,
    /// Solve for size: a subject that comes closer or goes away.
    pub scale: bool,
    /// Solve for a turn.
    pub rotation: bool,
    /// At most this many points follow the window.
    pub max_points: usize,
}

impl Default for TrackOptions {
    fn default() -> Self {
        Self { search_radius: 12, patch_half: 5, loss_threshold: 28, scale: true, rotation: true, max_points: 48 }
    }
}

use crate::homography::{Homography, Match};

/// p' = [a -b; b a] p + (tx, ty): a similarity in the frame's pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Sim {
    a: f64,
    b: f64,
    tx: f64,
    ty: f64,
}

impl Sim {
    fn map(&self, p: (f64, f64)) -> (f64, f64) {
        (self.a * p.0 - self.b * p.1 + self.tx, self.b * p.0 + self.a * p.1 + self.ty)
    }

    fn inverse_map(&self, p: (f64, f64)) -> (f64, f64) {
        let d = self.a * self.a + self.b * self.b;
        let (x, y) = (p.0 - self.tx, p.1 - self.ty);
        ((self.a * x + self.b * y) / d, (-self.b * x + self.a * y) / d)
    }

    /// This after `first`.
    fn after(&self, first: &Sim) -> Sim {
        let t = self.map((first.tx, first.ty));
        Sim { a: self.a * first.a - self.b * first.b, b: self.a * first.b + self.b * first.a, tx: t.0, ty: t.1 }
    }

    fn scale(&self) -> f64 {
        self.a.hypot(self.b)
    }

    fn turn(&self) -> f64 {
        self.b.atan2(self.a)
    }

    fn about(&self, pivot: (f64, f64)) -> WindowFit {
        let moved = self.map(pivot);
        WindowFit { dx: moved.0 - pivot.0, dy: moved.1 - pivot.1, scale: self.a.hypot(self.b), rotation: self.b.atan2(self.a) }
    }
}

/// How a window has moved from its anchor frame, as the tracker carries
/// it: a [`Sim`] for a move, size and turn, a [`Proj`] for perspective.
trait Warp: Copy {
    fn identity() -> Self;
    fn map(&self, p: (f64, f64)) -> (f64, f64);
    fn inverse_map(&self, p: (f64, f64)) -> (f64, f64);
    /// This after `first`.
    fn after(&self, first: &Self) -> Self;
    /// Whether the window, `quad` on the anchor frame, has changed so
    /// much under this that its patches there no longer look like
    /// themselves.
    fn drifted(&self, quad: &[(f64, f64); 4]) -> bool;
}

impl Warp for Sim {
    fn identity() -> Self {
        Sim { a: 1.0, b: 0.0, tx: 0.0, ty: 0.0 }
    }
    fn map(&self, p: (f64, f64)) -> (f64, f64) {
        Sim::map(self, p)
    }
    fn inverse_map(&self, p: (f64, f64)) -> (f64, f64) {
        Sim::inverse_map(self, p)
    }
    fn after(&self, first: &Self) -> Self {
        Sim::after(self, first)
    }
    fn drifted(&self, _quad: &[(f64, f64); 4]) -> bool {
        (self.scale() - 1.0).abs() > 0.2 || self.turn().abs() > 15f64.to_radians()
    }
}

/// A plane projective map in the frame's pixels and its inverse, kept
/// together because a patch is sampled back through the inverse at
/// every pixel.
#[derive(Debug, Clone, Copy)]
struct Proj {
    h: Homography,
    inv: Homography,
}

impl Proj {
    /// Inverted before it is scaled down: a unit-size map of a large
    /// frame has a determinant small enough for the inverse to refuse.
    fn new(h: Homography) -> Option<Proj> {
        let inv = normalised(h.inverse()?)?;
        let h = normalised(h)?;
        Some(Proj { h, inv })
    }
}

/// `h` scaled to unit size, keeping its sign: a map whose horizon
/// passes between the window and the frame's corner has its corner
/// entry negative where the window is in front, and dividing by that
/// entry would put the window behind the eye. `None` when the map is
/// zero or not finite.
fn normalised(mut h: Homography) -> Option<Homography> {
    let norm = h.h.iter().flatten().map(|v| v * v).sum::<f64>().sqrt();
    if !(norm.is_finite() && norm > 1e-300) {
        return None;
    }
    h.h.iter_mut().flatten().for_each(|v| *v /= norm);
    Some(h)
}

impl Warp for Proj {
    fn identity() -> Self {
        Proj { h: Homography::identity(), inv: Homography::identity() }
    }
    fn map(&self, p: (f64, f64)) -> (f64, f64) {
        self.h.transform(p.0, p.1)
    }
    fn inverse_map(&self, p: (f64, f64)) -> (f64, f64) {
        self.inv.transform(p.0, p.1)
    }
    fn after(&self, first: &Self) -> Self {
        let h = self.h.multiply(&first.h);
        let inv = first.inv.multiply(&self.inv);
        Proj { h: normalised(h).unwrap_or(h), inv: normalised(inv).unwrap_or(inv) }
    }
    /// Any side of the window grown or shrunk by a fifth, or turned by
    /// 15°: a surface that turns away foreshortens one side first.
    fn drifted(&self, quad: &[(f64, f64); 4]) -> bool {
        (0..4).any(|i| {
            let (a, b) = (quad[i], quad[(i + 1) % 4]);
            let (ma, mb) = (self.map(a), self.map(b));
            let (ex, ey) = (b.0 - a.0, b.1 - a.1);
            let (mx, my) = (mb.0 - ma.0, mb.1 - ma.1);
            let (len, mlen) = (ex.hypot(ey), mx.hypot(my));
            if len < 1e-9 {
                return false;
            }
            let turn = (ex * my - ey * mx).atan2(ex * mx + ey * my);
            (mlen / len - 1.0).abs() > 0.2 || turn.abs() > 15f64.to_radians()
        })
    }
}

/// The similarity that best takes `src` to `dst` in the least-squares
/// sense, held to no scale or no turn as `opts` asks.
fn fit(src: &[(f64, f64)], dst: &[(f64, f64)], opts: &TrackOptions) -> Option<Sim> {
    let n = src.len();
    if n == 0 || n != dst.len() {
        return None;
    }
    let nf = n as f64;
    let ms = (src.iter().map(|p| p.0).sum::<f64>() / nf, src.iter().map(|p| p.1).sum::<f64>() / nf);
    let md = (dst.iter().map(|p| p.0).sum::<f64>() / nf, dst.iter().map(|p| p.1).sum::<f64>() / nf);
    let (mut dot, mut cross, mut var) = (0.0, 0.0, 0.0);
    for (s, d) in src.iter().zip(dst) {
        let (sx, sy) = (s.0 - ms.0, s.1 - ms.1);
        let (dx, dy) = (d.0 - md.0, d.1 - md.1);
        dot += sx * dx + sy * dy;
        cross += sx * dy - sy * dx;
        var += sx * sx + sy * sy;
    }
    let (a, b) = match (opts.scale && n >= 2 && var > 1e-9, opts.rotation && n >= 2) {
        (true, true) => (dot / var, cross / var),
        (true, false) => (dot / var, 0.0),
        (false, true) => {
            let th = cross.atan2(dot);
            (th.cos(), th.sin())
        }
        (false, false) => (1.0, 0.0),
    };
    if !(a.is_finite() && b.is_finite()) || a.hypot(b) < 0.2 || a.hypot(b) > 5.0 {
        return None;
    }
    Some(Sim { a, b, tx: md.0 - (a * ms.0 - b * ms.1), ty: md.1 - (b * ms.0 + a * ms.1) })
}

/// [`fit`] by RANSAC: the fit most points agree with to within
/// `tolerance` pixels, refitted on those, and which points they are.
fn fit_robust(src: &[(f64, f64)], dst: &[(f64, f64)], opts: &TrackOptions, tolerance: f64) -> Option<(Sim, Vec<bool>)> {
    let n = src.len();
    if n < 2 {
        return None;
    }
    let agree = |s: &Sim| -> Vec<bool> {
        src.iter().zip(dst).map(|(p, q)| {
            let m = s.map(*p);
            (m.0 - q.0).hypot(m.1 - q.1) <= tolerance
        }).collect()
    };
    let mut best: Option<(Sim, Vec<bool>, usize)> = None;
    // Every pair when there are few, a fixed sample of pairs otherwise:
    // the same answer every run.
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let pairs: Vec<(usize, usize)> = if n <= 12 {
        (0..n).flat_map(|i| (i + 1..n).map(move |j| (i, j))).collect()
    } else {
        (0..96).map(|_| ((next() % n as u64) as usize, (next() % n as u64) as usize)).filter(|(i, j)| i != j).collect()
    };
    for (i, j) in pairs {
        let Some(s) = fit(&[src[i], src[j]], &[dst[i], dst[j]], opts) else { continue };
        let ins = agree(&s);
        let count = ins.iter().filter(|&&x| x).count();
        if best.as_ref().is_none_or(|b| count > b.2) {
            best = Some((s, ins, count));
        }
    }
    let (_, ins, count) = best?;
    if count < 2 {
        return None;
    }
    let (s2, d2): (Vec<_>, Vec<_>) = src.iter().zip(dst).zip(&ins).filter(|(_, &k)| k).map(|((s, d), _)| (*s, *d)).unzip();
    let refit = fit(&s2, &d2, opts)?;
    let ins = agree(&refit);
    Some((refit, ins))
}

/// Points moved to their centroid and scaled to a mean distance of √2
/// from it, and the map that does so: a homography fitted on such
/// points is far better conditioned than one on raw pixels.
fn conditioning(points: &[(f64, f64)]) -> Homography {
    let n = points.len().max(1) as f64;
    let (mx, my) = (points.iter().map(|p| p.0).sum::<f64>() / n, points.iter().map(|p| p.1).sum::<f64>() / n);
    let spread = points.iter().map(|p| (p.0 - mx).hypot(p.1 - my)).sum::<f64>() / n;
    let k = if spread > 1e-9 { std::f64::consts::SQRT_2 / spread } else { 1.0 };
    Homography { h: [[k, 0.0, -k * mx], [0.0, k, -k * my], [0.0, 0.0, 1.0]] }
}

/// The homography that best takes `src` to `dst`, four points or more,
/// least squares on conditioned points. `None` when none fits, or the
/// one that does folds the window over, sends part of it past the
/// horizon, or grows or shrinks it beyond any track's reach.
fn fit_projective(src: &[(f64, f64)], dst: &[(f64, f64)]) -> Option<Proj> {
    if src.len() < 4 || src.len() != dst.len() {
        return None;
    }
    let (ts, td) = (conditioning(src), conditioning(dst));
    let matches: Vec<Match> = src.iter().zip(dst).map(|(s, d)| {
        let (sx, sy) = ts.transform(s.0, s.1);
        let (dx, dy) = td.transform(d.0, d.1);
        Match { src_x: sx, src_y: sy, dst_x: dx, dst_y: dy }
    }).collect();
    let mut h = td.inverse()?.multiply(&Homography::from_n_points(&matches)?).multiply(&ts);
    // A fit is only known up to its sign: the one with the points in
    // front of the eye.
    let n = src.len() as f64;
    let c = (src.iter().map(|p| p.0).sum::<f64>() / n, src.iter().map(|p| p.1).sum::<f64>() / n);
    if h.h[2][0] * c.0 + h.h[2][1] * c.1 + h.h[2][2] < 0.0 {
        h.h.iter_mut().flatten().for_each(|v| *v = -*v);
    }
    let proj = Proj::new(h)?;
    // Every point in front of the eye, and the map's local area change
    // at their middle neither mirrored nor out of reach.
    if src.iter().any(|p| proj.h.map(p.0, p.1).is_none()) {
        return None;
    }
    let h = &proj.h.h;
    let w = h[2][0] * c.0 + h[2][1] * c.1 + h[2][2];
    let (x, y) = proj.h.map(c.0, c.1)?;
    let j = [[(h[0][0] - x * h[2][0]) / w, (h[0][1] - x * h[2][1]) / w], [(h[1][0] - y * h[2][0]) / w, (h[1][1] - y * h[2][1]) / w]];
    let det = j[0][0] * j[1][1] - j[0][1] * j[1][0];
    (det.is_finite() && (0.04..=25.0).contains(&det)).then_some(proj)
}

/// [`fit_projective`] by RANSAC, as [`fit_robust`] is for a similarity:
/// the map most points agree with to within `tolerance` pixels, from
/// fours of them, refitted on those, and which points they are.
fn fit_projective_robust(src: &[(f64, f64)], dst: &[(f64, f64)], tolerance: f64) -> Option<(Proj, Vec<bool>)> {
    let n = src.len();
    if n < 4 {
        return None;
    }
    let agree = |p: &Proj| -> Vec<bool> {
        src.iter().zip(dst).map(|(s, d)| {
            let m = p.map(*s);
            (m.0 - d.0).hypot(m.1 - d.1) <= tolerance
        }).collect()
    };
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    // Every four when there are few, a fixed sample of fours otherwise:
    // the same answer every run.
    let fours: Vec<[usize; 4]> = if n <= 9 {
        let mut all = Vec::new();
        for a in 0..n {
            for b in a + 1..n {
                for c in b + 1..n {
                    for d in c + 1..n {
                        all.push([a, b, c, d]);
                    }
                }
            }
        }
        all
    } else {
        (0..160).map(|_| [0; 4].map(|_| (next() % n as u64) as usize)).filter(|q| (0..4).all(|i| (i + 1..4).all(|j| q[i] != q[j]))).collect()
    };
    let mut best: Option<(Vec<bool>, usize)> = None;
    for q in fours {
        let Some(p) = fit_projective(&q.map(|i| src[i]), &q.map(|i| dst[i])) else { continue };
        let ins = agree(&p);
        let count = ins.iter().filter(|&&x| x).count();
        if best.as_ref().is_none_or(|b| count > b.1) {
            best = Some((ins, count));
        }
    }
    let (ins, count) = best?;
    if count < 4 {
        return None;
    }
    let (s2, d2): (Vec<_>, Vec<_>) = src.iter().zip(dst).zip(&ins).filter(|(_, &k)| k).map(|((s, d), _)| (*s, *d)).unzip();
    let refit = fit_projective(&s2, &d2)?;
    let ins = agree(&refit);
    Some((refit, ins))
}

/// Where the point at `from` on `anchor` is in `cur`, the anchor's patch
/// warped by `rel` (anchor to now, as far as is known) so a patch on a
/// subject that has grown or turned still looks like itself, searched
/// within `opts.search_radius` of where `rel` puts it, to a fraction of
/// a pixel. `None` when nothing there is close enough, or nothing stands
/// out.
fn match_patch<W: Warp>(anchor: &GrayFrame, from: (f64, f64), rel: &W, cur: &GrayFrame, opts: &TrackOptions) -> Option<(f64, f64)> {
    let pred = rel.map(from);
    let (gx, gy) = (pred.0.round() as i32, pred.1.round() as i32);
    let ph = opts.patch_half;
    let sr = opts.search_radius;
    // The template, laid on the whole pixels around the prediction.
    let side_t = (2 * ph + 1) as usize;
    let mut template = vec![0.0f64; side_t * side_t];
    for ky in -ph..=ph {
        for kx in -ph..=ph {
            let back = rel.inverse_map(((gx + kx) as f64, (gy + ky) as f64));
            template[((ky + ph) as usize) * side_t + (kx + ph) as usize] = anchor.sample(back.0, back.1);
        }
    }
    let side = (2 * sr + 1) as usize;
    let mut sad = vec![0.0f64; side * side];
    let mut best = (f64::MAX, 0i32, 0i32);
    for oy in -sr..=sr {
        for ox in -sr..=sr {
            let mut sum = 0.0;
            for ky in -ph..=ph {
                for kx in -ph..=ph {
                    let a = template[((ky + ph) as usize) * side_t + (kx + ph) as usize];
                    sum += (a - cur.at(gx + ox + kx, gy + oy + ky) as f64).abs();
                }
            }
            sad[((oy + sr) as usize) * side + (ox + sr) as usize] = sum;
            if sum < best.0 {
                best = (sum, ox, oy);
            }
        }
    }
    let area = (side_t * side_t) as f64;
    if best.0 / area > opts.loss_threshold as f64 {
        return None;
    }
    // A match has to stand out from the search around it: on a frame
    // where the content is gone — a flat wall, a cut — every place
    // scores alike, and the best of them is no match at all.
    let typical = {
        let mut f = sad.clone();
        f.sort_by(|a, b| a.total_cmp(b));
        f[f.len() / 2]
    };
    if best.0 > 0.75 * typical {
        return None;
    }
    // A parabola through the best and its neighbours, each way, on the
    // squared differences: their trough is round where the absolute
    // ones make a V, whose parabola reads every fraction too small and
    // pins a point to whatever whole pixel it was predicted on.
    let ssd = |ox: i32, oy: i32| -> Option<f64> {
        (ox.abs() <= sr && oy.abs() <= sr).then(|| {
            let mut sum = 0.0;
            for ky in -ph..=ph {
                for kx in -ph..=ph {
                    let d = template[((ky + ph) as usize) * side_t + (kx + ph) as usize] - cur.at(gx + ox + kx, gy + oy + ky) as f64;
                    sum += d * d;
                }
            }
            sum
        })
    };
    let at = |ox: i32, oy: i32| ssd(ox, oy);
    let centre = ssd(best.1, best.2).unwrap_or(0.0);
    let refine = |l: Option<f64>, c: f64, r: Option<f64>| -> f64 {
        match (l, r) {
            (Some(l), Some(r)) => {
                let d = l - 2.0 * c + r;
                if d > 1e-9 { (0.5 * (l - r) / d).clamp(-0.5, 0.5) } else { 0.0 }
            }
            _ => 0.0,
        }
    };
    let sx = refine(at(best.1 - 1, best.2), centre, at(best.1 + 1, best.2));
    let sy = refine(at(best.1, best.2 - 1), centre, at(best.1, best.2 + 1));
    // The template sat on whole pixels round the prediction; the point
    // sits that fraction off them and moves with the content.
    Some((pred.0 + best.1 as f64 + sx, pred.1 + best.2 as f64 + sy))
}

/// Points worth following inside `quad` (four corners, in order): a grid
/// over it, kept where a patch has texture enough to match, the most
/// textured first.
fn seed(frame: &GrayFrame, quad: &[(f64, f64); 4], opts: &TrackOptions) -> Vec<(f64, f64)> {
    let (x0, x1) = quad.iter().fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p.0), b.max(p.0)));
    let (y0, y1) = quad.iter().fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p.1), b.max(p.1)));
    let inside = |p: (f64, f64)| {
        // Same side of every edge.
        let mut sign = 0.0f64;
        for i in 0..4 {
            let (a, b) = (quad[i], quad[(i + 1) % 4]);
            let c = (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0);
            if c.abs() < 1e-12 {
                continue;
            }
            if sign == 0.0 {
                sign = c.signum();
            } else if c.signum() != sign {
                return false;
            }
        }
        true
    };
    let margin = (opts.patch_half + 1) as f64;
    let (w, h) = (frame.width as f64, frame.height as f64);
    let span = (x1 - x0).min(y1 - y0).max(1.0);
    let step = (span / 8.0).max(opts.patch_half as f64).max(2.0);
    let mut out: Vec<(u64, (f64, f64))> = Vec::new();
    let mut y = y0 + step * 0.5;
    while y < y1 {
        let mut x = x0 + step * 0.5;
        while x < x1 {
            if inside((x, y)) && x > margin && y > margin && x < w - margin && y < h - margin {
                let (cx, cy) = (x.round() as i32, y.round() as i32);
                let ph = opts.patch_half;
                let mut texture = 0u64;
                for ky in -ph..ph {
                    for kx in -ph..ph {
                        let v = frame.at(cx + kx, cy + ky) as i64;
                        texture += (v - frame.at(cx + kx + 1, cy + ky) as i64).unsigned_abs() + (v - frame.at(cx + kx, cy + ky + 1) as i64).unsigned_abs();
                    }
                }
                let area = (4 * ph * ph).max(1) as u64;
                if texture / area >= 4 {
                    out.push((texture, (x, y)));
                }
            }
            x += step;
        }
        y += step;
    }
    out.sort_by_key(|p| std::cmp::Reverse(p.0));
    out.into_iter().take(opts.max_points).map(|(_, p)| p).collect()
}

/// Follow the window `quad` (its four corners on the reference frame, in
/// pixels) from `reference` through `frames`, in the order given — the
/// frames after it for a forward track, before it, nearest first, for a
/// backward one. `next` gives each frame; `progress` hears the share
/// done and stops the track by returning false.
///
/// Returns the window's motion on each frame followed, about `pivot`,
/// from the reference frame. It stops at the first frame where too few
/// points agree to say where the window went, rather than guessing.
pub fn track_window(
    reference: &GrayFrame,
    quad: [(f64, f64); 4],
    pivot: (f64, f64),
    frames: &[usize],
    next: &mut dyn FnMut(usize) -> Option<GrayFrame>,
    opts: &TrackOptions,
    progress: &dyn Fn(f32) -> bool,
) -> Vec<(usize, WindowFit)> {
    follow::<Sim>(reference, quad, frames, next, opts, progress, 2, &|src, dst| fit_robust(src, dst, opts, 1.0))
        .into_iter()
        .map(|(f, s)| (f, s.about(pivot)))
        .collect()
}

/// [`track_window`] in perspective: the window on a flat surface whose
/// corners each go their own way as it turns away from the camera.
///
/// Returns, for each frame followed, the map from the reference frame's
/// pixels to that frame's: where any point of the window went. A track
/// in perspective needs four points to agree where a move needs two,
/// and stops as readily.
pub fn track_window_perspective(
    reference: &GrayFrame,
    quad: [(f64, f64); 4],
    frames: &[usize],
    next: &mut dyn FnMut(usize) -> Option<GrayFrame>,
    opts: &TrackOptions,
    progress: &dyn Fn(f32) -> bool,
) -> Vec<(usize, Homography)> {
    follow::<Proj>(reference, quad, frames, next, opts, progress, 4, &|src, dst| fit_projective_robust(src, dst, 1.0))
        .into_iter()
        .map(|(f, p)| (f, p.h))
        .collect()
}

/// The track itself, for either warp: `least` points are the fewest a
/// warp can be fitted from, and `fit` fits one robustly.
#[allow(clippy::too_many_arguments)]
fn follow<W: Warp>(
    reference: &GrayFrame,
    quad: [(f64, f64); 4],
    frames: &[usize],
    next: &mut dyn FnMut(usize) -> Option<GrayFrame>,
    opts: &TrackOptions,
    progress: &dyn Fn(f32) -> bool,
    least: usize,
    fit: &dyn Fn(&[(f64, f64)], &[(f64, f64)]) -> Option<(W, Vec<bool>)>,
) -> Vec<(usize, W)> {
    let mut out = Vec::new();
    // Points are matched against an anchor frame — the reference, until
    // the window has changed so much since it that its patches no longer
    // look like themselves, then the frame where that happened — each
    // point known by where it is there. `base` takes the reference to
    // the anchor, `rel` the anchor to the frame last followed.
    let mut anchor = reference.clone();
    let mut anchor_quad = quad;
    let mut base = W::identity();
    let mut rel = base;
    let mut points: Vec<(f64, f64)> = seed(reference, &quad, opts);
    let mut seeded = points.len();
    if seeded < least {
        return out;
    }
    for (k, &f) in frames.iter().enumerate() {
        if !progress(k as f32 / frames.len().max(1) as f32) {
            break;
        }
        let Some(cur) = next(f) else { break };
        let matched: Vec<((f64, f64), (f64, f64))> = points.iter().filter_map(|&p| match_patch(&anchor, p, &rel, &cur, opts).map(|m| (p, m))).collect();
        let (src, dst): (Vec<_>, Vec<_>) = matched.iter().copied().unzip();
        let Some((fitted, ins_first)) = fit(&src, &dst) else { break };
        // Matched again from where the fit puts each point: the template
        // then lies where the content is, and the fraction of a pixel
        // read off the parabola is the small one it reads best.
        let (matched, ins) = {
            let again: Vec<((f64, f64), (f64, f64))> = matched.iter().zip(&ins_first).filter(|(_, k)| **k).filter_map(|(&(p, _), _)| match_patch(&anchor, p, &fitted, &cur, opts).map(|m| (p, m))).collect();
            let (src, dst): (Vec<_>, Vec<_>) = again.iter().copied().unzip();
            match fit(&src, &dst) {
                Some((refit, ins2)) if ins2.iter().filter(|&&k| k).count() >= ins_first.iter().filter(|&&k| k).count() * 3 / 4 => (again, Some((refit, ins2))),
                _ => (matched, None),
            }
        };
        let (fitted, ins) = match ins {
            Some(r) => r,
            None => (fitted, ins_first),
        };
        let agreeing = ins.iter().filter(|&&k| k).count();
        if agreeing < (least + 1).min(seeded) {
            break;
        }
        rel = fitted;
        let total = rel.after(&base);
        out.push((f, total));
        points = matched.into_iter().zip(ins).filter(|(_, k)| *k).map(|((p, _), _)| p).collect();
        // Re-anchor on this frame when the patches have drifted far from
        // how the anchor shows them, or too few points are left.
        if rel.drifted(&anchor_quad) || points.len() < (seeded / 2).max(6) {
            let now = quad.map(|c| total.map(c));
            let fresh = seed(&cur, &now, opts);
            if fresh.len() > least {
                points = fresh;
                seeded = points.len();
                anchor = cur;
                anchor_quad = now;
                base = total;
                rel = W::identity();
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A picture with texture everywhere: smoothed noise, the same every
    /// run, sampled anywhere by a smooth function so a warped frame is
    /// exact.
    fn texture(x: f64, y: f64) -> f64 {
        let mut v = 0.0;
        for (i, (fx, fy, ph)) in [(0.21, 0.13, 0.3), (0.07, 0.31, 1.7), (0.43, 0.05, 2.9), (0.11, 0.17, 4.1), (0.33, 0.29, 0.8), (0.05, 0.09, 3.3)].iter().enumerate() {
            v += ((x * fx + y * fy + ph) * (1.0 + i as f64 * 0.37)).sin();
        }
        128.0 + 20.0 * v
    }

    /// The frame where the content at `p` on the reference has gone to
    /// `motion` about `pivot`.
    fn frame(w: usize, h: usize, motion: WindowFit, pivot: (f64, f64)) -> GrayFrame {
        let (s, c) = motion.rotation.sin_cos();
        let mut data = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                // Back from this pixel to where it came from.
                let (ux, uy) = (x as f64 - pivot.0 - motion.dx, y as f64 - pivot.1 - motion.dy);
                let (rx, ry) = ((c * ux + s * uy) / motion.scale, (-s * ux + c * uy) / motion.scale);
                data[y * w + x] = texture(rx + pivot.0, ry + pivot.1).clamp(0.0, 255.0) as u8;
            }
        }
        GrayFrame { data, width: w, height: h }
    }

    /// #238: a window over a subject that comes closer, turns and moves is
    /// followed in all three, frame after frame, to within a fraction of
    /// a pixel and a degree, with no drift building up.
    #[test]
    fn a_window_follows_a_subject_that_grows_turns_and_moves() {
        let (w, h) = (240, 160);
        let pivot = (120.0, 80.0);
        let quad = [(90.0, 55.0), (150.0, 55.0), (150.0, 105.0), (90.0, 105.0)];
        let motion = |k: usize| WindowFit { dx: 1.5 * k as f64, dy: -0.8 * k as f64, scale: 1.0 + 0.012 * k as f64, rotation: (0.9 * k as f64).to_radians() };
        let reference = frame(w, h, WindowFit::STILL, pivot);
        // Thirty frames: the subject a third bigger by the end, so the
        // track re-anchors on the way.
        let frames: Vec<usize> = (1..=30).collect();
        let got = track_window(&reference, quad, pivot, &frames, &mut |k| Some(frame(w, h, motion(k), pivot)), &TrackOptions::default(), &|_| true);
        assert_eq!(got.len(), 30, "every frame followed");
        for (k, fit) in &got {
            let want = motion(*k);
            assert!((fit.dx - want.dx).abs() < 0.6 && (fit.dy - want.dy).abs() < 0.6, "frame {k}: moved {:.2},{:.2}, wanted {:.2},{:.2}", fit.dx, fit.dy, want.dx, want.dy);
            assert!((fit.scale / want.scale - 1.0).abs() < 0.01, "frame {k}: scale {:.4}, wanted {:.4}", fit.scale, want.scale);
            assert!((fit.rotation - want.rotation).to_degrees().abs() < 0.6, "frame {k}: turned {:.2}°, wanted {:.2}°", fit.rotation.to_degrees(), want.rotation.to_degrees());
        }
    }

    /// Held to moving alone, a turning subject's window keeps its size
    /// and angle.
    #[test]
    fn a_window_held_to_moving_does_not_scale_or_turn() {
        let (w, h) = (200, 140);
        let pivot = (100.0, 70.0);
        let quad = [(75.0, 50.0), (125.0, 50.0), (125.0, 90.0), (75.0, 90.0)];
        let reference = frame(w, h, WindowFit::STILL, pivot);
        let opts = TrackOptions { scale: false, rotation: false, ..TrackOptions::default() };
        let got = track_window(&reference, quad, pivot, &[1, 2, 3], &mut |k| Some(frame(w, h, WindowFit { dx: 2.0 * k as f64, dy: 1.0 * k as f64, scale: 1.0, rotation: 0.0 }, pivot)), &opts, &|_| true);
        assert_eq!(got.len(), 3);
        let last = got[2].1;
        assert!(last.scale == 1.0 && last.rotation == 0.0);
        assert!((last.dx - 6.0).abs() < 0.5 && (last.dy - 3.0).abs() < 0.5, "{last:?}");
    }

    /// A frame where the window's content is gone stops the track there,
    /// rather than inventing a place for it.
    #[test]
    fn a_lost_window_stops_rather_than_guesses() {
        let (w, h) = (200, 140);
        let pivot = (100.0, 70.0);
        let quad = [(75.0, 50.0), (125.0, 50.0), (125.0, 90.0), (75.0, 90.0)];
        let reference = frame(w, h, WindowFit::STILL, pivot);
        let flat = GrayFrame { data: vec![128; w * h], width: w, height: h };
        let got = track_window(&reference, quad, pivot, &[1, 2, 3], &mut |k| Some(if k < 2 { frame(w, h, WindowFit { dx: 1.0, ..WindowFit::STILL }, pivot) } else { flat.clone() }), &TrackOptions::default(), &|_| true);
        assert_eq!(got.iter().map(|g| g.0).collect::<Vec<_>>(), vec![1], "followed to the frame before it was lost");
    }

    /// The frame where the content at a reference point `p` has gone to
    /// `h`: each pixel sampled back through its inverse.
    fn warped(w: usize, h: usize, map: &Homography) -> GrayFrame {
        let back = map.inverse().unwrap();
        let mut data = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                let (rx, ry) = back.transform(x as f64, y as f64);
                data[y * w + x] = texture(rx, ry).clamp(0.0, 255.0) as u8;
            }
        }
        GrayFrame { data, width: w, height: h }
    }

    /// #238: a window on a surface that turns away from the camera — its
    /// near side growing, its far side shrinking, the whole drifting —
    /// is followed in perspective: each corner lands where the surface
    /// took it, to a fraction of a pixel, through re-anchoring.
    #[test]
    fn a_window_in_perspective_follows_each_corner() {
        let (w, h) = (240, 160);
        let quad = [(80.0, 50.0), (160.0, 50.0), (160.0, 110.0), (80.0, 110.0)];
        // Each corner on its own course, per frame.
        let course = [(0.6, -0.5), (0.2, 0.9), (0.1, -0.3), (0.7, 0.5)];
        let motion = |k: usize| {
            let to = std::array::from_fn(|i| (quad[i].0 + course[i].0 * k as f64, quad[i].1 + course[i].1 * k as f64));
            Homography::from_4_points(&quad, &to).unwrap()
        };
        let reference = warped(w, h, &Homography::identity());
        let frames: Vec<usize> = (1..=24).collect();
        let got = track_window_perspective(&reference, quad, &frames, &mut |k| Some(warped(w, h, &motion(k))), &TrackOptions::default(), &|_| true);
        assert_eq!(got.len(), 24, "every frame followed");
        for (k, map) in &got {
            let want = motion(*k);
            // The middle to a tenth or so of a pixel; the corners,
            // which the points inside the window only reach by
            // extrapolation, to under a pixel.
            for (c, within) in [(quad[0], 1.0), (quad[1], 1.0), (quad[2], 1.0), (quad[3], 1.0), ((120.0, 80.0), 0.25)] {
                let (g, t) = (map.map(c.0, c.1).unwrap_or_else(|| panic!("frame {k}: {c:?} past the horizon, {map:?}")), want.transform(c.0, c.1));
                assert!((g.0 - t.0).hypot(g.1 - t.1) < within, "frame {k}: {c:?} at {g:.2?}, wanted {t:.2?}");
            }
        }
        // The far side really did shrink against the near one: a
        // similarity could not have followed this.
        let last = motion(24);
        let side = |a: (f64, f64), b: (f64, f64)| {
            let (a, b) = (last.transform(a.0, a.1), last.transform(b.0, b.1));
            (a.0 - b.0).hypot(a.1 - b.1)
        };
        assert!(side(quad[1], quad[2]) / side(quad[0], quad[3]) < 0.7);
    }

    /// In perspective too, a frame where the content is gone stops the
    /// track.
    #[test]
    fn a_lost_window_in_perspective_stops() {
        let (w, h) = (200, 140);
        let quad = [(75.0, 50.0), (125.0, 50.0), (125.0, 90.0), (75.0, 90.0)];
        let reference = warped(w, h, &Homography::identity());
        let flat = GrayFrame { data: vec![128; w * h], width: w, height: h };
        let shifted = Homography { h: [[1.0, 0.0, 1.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] };
        let got = track_window_perspective(&reference, quad, &[1, 2, 3], &mut |k| Some(if k < 2 { warped(w, h, &shifted) } else { flat.clone() }), &TrackOptions::default(), &|_| true);
        assert_eq!(got.iter().map(|g| g.0).collect::<Vec<_>>(), vec![1]);
    }
}
