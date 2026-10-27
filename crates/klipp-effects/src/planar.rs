//! A flat surface followed through a shot by its whole region (#2754),
//! not by points inside it: the region of the reference frame is
//! aligned to each frame by the homography that best maps its pixels
//! there, coarse to fine.
//!
//! The alignment is inverse-compositional Lucas–Kanade. The region is
//! compared in two channels, the brightness and the strength of its
//! edges, each with a gain and an offset fitted as it goes, so a change
//! of light does not read as a move. The region is taken a little larger than the
//! surface, so a plain surface is held by its own outline against what
//! is around it, where a point tracker finds nothing to follow. Each
//! residual is weighted (Tukey), so what passes in front, a reflection
//! or a highlight pulls less.
//!
//! Each frame is aligned to the frame before (which looks most like it),
//! then refined against the reference (so small errors do not add up to
//! drift). Each frame's confidence comes from how well the reference
//! matches it and how well the alignment back to the frame before
//! returns to where it started (forward-backward). A frame that fails
//! both is lost; the reference is then searched for in the frames after,
//! and the track picks up where it is found again.

use crate::homography::Homography;
use crate::window_track::GrayFrame;

/// How a frame was followed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum State {
    /// Followed, and confident.
    Good,
    /// Followed, but the surface is hard to see or the fit uncertain.
    Low,
    /// Not followed: its map is the last good one, held, or a straight
    /// bridge between known frames either side ([`blend`]).
    Lost,
}

/// One frame followed: the map from the reference frame's pixels to its
/// own, how sure the fit is, 0 to 1, and how far a round trip to the
/// frame before and back missed, in pixels of the frames given.
#[derive(Debug, Clone, Copy)]
pub struct Tracked {
    pub frame: usize,
    pub h: Homography,
    pub confidence: f32,
    pub state: State,
    pub round_trip_px: f32,
}

/// How the track runs.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// How much larger than the surface the region compared is, so its
    /// outline is part of it.
    pub margin: f64,
    /// The smallest the region's side gets at the top of the pyramid,
    /// pixels.
    pub top_side: f64,
    /// How far the top level is searched before it is refined, pixels at
    /// that level.
    pub search: i32,
    /// The most pixels of the region compared at one level.
    pub max_points: usize,
    /// Refinement steps at each level.
    pub iterations: usize,
    /// Frames in a row that fail before the track counts as lost.
    pub lost_after: usize,
    /// Frames after a loss the reference is searched for in.
    pub reacquire_for: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self { margin: 0.08, top_side: 40.0, search: 6, max_points: 24_000, iterations: 25, lost_after: 3, reacquire_for: 48 }
    }
}

/// A picture of floats, row by row.
#[derive(Debug, Clone)]
struct Img {
    w: usize,
    h: usize,
    px: Vec<f32>,
}

impl Img {
    fn at(&self, x: i32, y: i32) -> f32 {
        let x = x.clamp(0, self.w as i32 - 1) as usize;
        let y = y.clamp(0, self.h as i32 - 1) as usize;
        self.px[y * self.w + x]
    }

    /// Between pixels, bilinearly; `None` outside the picture.
    fn sample(&self, x: f64, y: f64) -> Option<f32> {
        if !(x >= 0.0 && y >= 0.0 && x <= (self.w - 1) as f64 && y <= (self.h - 1) as f64) {
            return None;
        }
        let (x0, y0) = (x.floor() as i32, y.floor() as i32);
        let (fx, fy) = ((x - x0 as f64) as f32, (y - y0 as f64) as f32);
        let top = self.at(x0, y0) * (1.0 - fx) + self.at(x0 + 1, y0) * fx;
        let bottom = self.at(x0, y0 + 1) * (1.0 - fx) + self.at(x0 + 1, y0 + 1) * fx;
        Some(top * (1.0 - fy) + bottom * fy)
    }
}

/// Half the size, blurred first so nothing aliases: pixel (x, y) here is
/// pixel (2x, 2y) of the level below.
fn halve(img: &Img) -> Img {
    const K: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];
    let (w, h) = (img.w.div_ceil(2), img.h.div_ceil(2));
    let mut across = Img { w: img.w, h, px: vec![0.0; img.w * h] };
    for y in 0..h {
        for x in 0..img.w {
            across.px[y * img.w + x] = (0..5).map(|k| K[k] * img.at(x as i32, 2 * y as i32 + k as i32 - 2)).sum();
        }
    }
    let mut out = Img { w, h, px: vec![0.0; w * h] };
    for y in 0..h {
        for x in 0..w {
            out.px[y * w + x] = (0..5).map(|k| K[k] * across.at(2 * x as i32 + k as i32 - 2, y as i32)).sum();
        }
    }
    out
}

/// The strength of the edges, central differences.
fn edges(img: &Img) -> Img {
    let mut out = Img { w: img.w, h: img.h, px: vec![0.0; img.w * img.h] };
    for y in 0..img.h as i32 {
        for x in 0..img.w as i32 {
            let gx = (img.at(x + 1, y) - img.at(x - 1, y)) * 0.5;
            let gy = (img.at(x, y + 1) - img.at(x, y - 1)) * 0.5;
            out.px[y as usize * img.w + x as usize] = (gx * gx + gy * gy).sqrt();
        }
    }
    out
}

/// One level of a frame's pyramid: its two channels.
struct Level {
    chans: [Img; 2],
}

/// A frame as the aligner reads it, finest level first.
pub(crate) struct Pyramid {
    levels: Vec<Level>,
}

impl Pyramid {
    fn new(frame: &GrayFrame, levels: usize) -> Self {
        let mut img = Img { w: frame.width, h: frame.height, px: frame.data.iter().map(|&v| v as f32 / 255.0).collect() };
        let mut out = Vec::with_capacity(levels);
        for l in 0..levels {
            if l > 0 {
                img = halve(&img);
            }
            let e = edges(&img);
            out.push(Level { chans: [img.clone(), e] });
        }
        Self { levels: out }
    }
}

/// The region's own coordinates: its centre at 0, its half-size 1, the
/// same at every level, so the fit is well conditioned.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Frame {
    pub(crate) cx: f64,
    pub(crate) cy: f64,
    pub(crate) s: f64,
}

impl Frame {
    pub(crate) fn of(quad: &[(f64, f64); 4]) -> Self {
        let (cx, cy) = (quad.iter().map(|p| p.0).sum::<f64>() / 4.0, quad.iter().map(|p| p.1).sum::<f64>() / 4.0);
        let s = quad.iter().map(|p| (p.0 - cx).hypot(p.1 - cy)).fold(0.0, f64::max).max(1.0);
        Self { cx, cy, s }
    }

    /// From the region's coordinates to the frame's pixels.
    pub(crate) fn px(self) -> Homography {
        Homography { h: [[self.s, 0.0, self.cx], [0.0, self.s, self.cy], [0.0, 0.0, 1.0]] }
    }
}

/// The scale from the finest level's pixels to level `l`'s.
pub(crate) fn to_level(l: usize) -> Homography {
    let k = 1.0 / (1u32 << l) as f64;
    Homography { h: [[k, 0.0, 0.0], [0.0, k, 0.0], [0.0, 0.0, 1.0]] }
}

pub(crate) fn from_level(l: usize) -> Homography {
    let k = (1u32 << l) as f64;
    Homography { h: [[k, 0.0, 0.0], [0.0, k, 0.0], [0.0, 0.0, 1.0]] }
}

/// A pixel of the template: where it is in the region's coordinates,
/// its two values, and how each would change with each of the eight
/// parameters of the map (its steepest descent images).
struct Point {
    u: (f64, f64),
    val: [f32; 2],
    sd: [[f64; 8]; 2],
    /// How much each channel says about the map here.
    strength: [f64; 2],
}

/// The region of a frame, ready to align others to, level by level.
pub(crate) struct Template {
    frame: Frame,
    levels: Vec<Vec<Point>>,
}

/// Whether `p` lies inside the convex quad `q`, either winding.
fn inside(q: &[(f64, f64); 4], p: (f64, f64)) -> bool {
    let mut sign = 0.0f64;
    for i in 0..4 {
        let (a, b) = (q[i], q[(i + 1) % 4]);
        let c = (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0);
        if c != 0.0 {
            if sign == 0.0 {
                sign = c.signum();
            } else if c.signum() != sign {
                return false;
            }
        }
    }
    true
}

/// Whether `p` lies inside the polygon `poly`, by the even-odd rule.
fn in_polygon(poly: &[(f64, f64)], p: (f64, f64)) -> bool {
    let mut inside = false;
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < a.0 + (p.1 - a.1) / (b.1 - a.1) * (b.0 - a.0) {
            inside = !inside;
        }
    }
    inside
}

/// The pixels of each level a template of `quad` reads, on levels of
/// `sizes` (finest first): the region grown by the margin, less what lies
/// in an `exclude` polygon (pixels of the finest level), at most
/// [`Options::max_points`] of its pixels evenly taken, a pixel's border
/// left out. With the region's own coordinates.
pub(crate) fn cells(sizes: &[(usize, usize)], quad: &[(f64, f64); 4], exclude: &[Vec<(f64, f64)>], opts: &Options) -> (Frame, Vec<Vec<(i32, i32)>>) {
    let frame = Frame::of(quad);
    let grown: [(f64, f64); 4] = std::array::from_fn(|i| (frame.cx + (quad[i].0 - frame.cx) * (1.0 + opts.margin), frame.cy + (quad[i].1 - frame.cy) * (1.0 + opts.margin)));
    let levels = sizes
        .iter()
        .enumerate()
        .map(|(l, &(w, h))| {
            let k = 1.0 / (1u32 << l) as f64;
            let q: [(f64, f64); 4] = grown.map(|p| (p.0 * k, p.1 * k));
            let (x0, x1) = (q.iter().map(|p| p.0).fold(f64::MAX, f64::min).floor().max(1.0) as i32, q.iter().map(|p| p.0).fold(f64::MIN, f64::max).ceil() as i32);
            let (y0, y1) = (q.iter().map(|p| p.1).fold(f64::MAX, f64::min).floor().max(1.0) as i32, q.iter().map(|p| p.1).fold(f64::MIN, f64::max).ceil() as i32);
            let (x1, y1) = (x1.min(w as i32 - 2), y1.min(h as i32 - 2));
            let mut cells: Vec<(i32, i32)> = Vec::new();
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let finest = (x as f64 / k, y as f64 / k);
                    if inside(&q, (x as f64, y as f64)) && !exclude.iter().any(|e| in_polygon(e, finest)) {
                        cells.push((x, y));
                    }
                }
            }
            let stride = cells.len().div_ceil(opts.max_points.max(1)).max(1);
            cells.into_iter().step_by(stride).collect()
        })
        .collect();
    (frame, levels)
}

/// The sizes of a frame `w` × `h`'s pyramid of `levels`, finest first.
pub(crate) fn level_sizes(w: usize, h: usize, levels: usize) -> Vec<(usize, usize)> {
    let mut out = vec![(w, h)];
    while out.len() < levels {
        let (pw, ph) = *out.last().expect("one");
        out.push((pw.div_ceil(2), ph.div_ceil(2)));
    }
    out
}

impl Template {
    fn new(pyr: &Pyramid, quad: &[(f64, f64); 4], exclude: &[Vec<(f64, f64)>], opts: &Options) -> Self {
        let sizes: Vec<(usize, usize)> = pyr.levels.iter().map(|l| (l.chans[0].w, l.chans[0].h)).collect();
        let (frame, cells) = cells(&sizes, quad, exclude, opts);
        let levels = cells
            .into_iter()
            .zip(&pyr.levels)
            .enumerate()
            .map(|(l, (cells, level))| {
                let k = 1.0 / (1u32 << l) as f64;
                // Pixels of the level per unit of the region's coordinates.
                let per_u = frame.s * k;
                cells
                    .into_iter()
                    .map(|(x, y)| {
                        let u = ((x as f64 / k - frame.cx) / frame.s, (y as f64 / k - frame.cy) / frame.s);
                        let (a, b) = u;
                        let dw = [[a, b, 1.0, 0.0, 0.0, 0.0, -a * a, -a * b], [0.0, 0.0, 0.0, a, b, 1.0, -a * b, -b * b]];
                        let mut sd = [[0.0f64; 8]; 2];
                        let mut val = [0.0f32; 2];
                        for (c, chan) in level.chans.iter().enumerate() {
                            val[c] = chan.at(x, y);
                            let gx = (chan.at(x + 1, y) - chan.at(x - 1, y)) as f64 * 0.5 * per_u;
                            let gy = (chan.at(x, y + 1) - chan.at(x, y - 1)) as f64 * 0.5 * per_u;
                            for j in 0..8 {
                                sd[c][j] = gx * dw[0][j] + gy * dw[1][j];
                            }
                        }
                        let strength = sd.map(|s| s.iter().map(|v| v * v).sum::<f64>());
                        Point { u, val, sd, strength }
                    })
                    .collect()
            })
            .collect();
        Self { frame, levels }
    }
}

/// What an alignment ends with.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Fit {
    /// The map from the region's coordinates to the frame's finest
    /// pixels.
    pub(crate) g: Homography,
    /// The smallest over the largest eigenvalue of the last step's
    /// weighted Hessian: near 0, the region has nothing to hold the map.
    pub(crate) conditioning: f64,
}

/// The eight-parameter step as a map.
fn step_map(p: &[f64; 8]) -> Homography {
    Homography { h: [[1.0 + p[0], p[1], p[2]], [p[3], 1.0 + p[4], p[5]], [p[6], p[7], 1.0]] }
}

/// The eigenvalues of a symmetric 8×8, by Jacobi rotations.
#[allow(clippy::needless_range_loop)]
fn eigenvalues(mut a: [[f64; 8]; 8]) -> [f64; 8] {
    for _ in 0..60 {
        let mut off = 0.0;
        for i in 0..8 {
            for j in 0..8 {
                if i != j {
                    off += a[i][j] * a[i][j];
                }
            }
        }
        if off < 1e-18 {
            break;
        }
        for p in 0..8 {
            for q in p + 1..8 {
                if a[p][q].abs() < 1e-30 {
                    continue;
                }
                let theta = (a[q][q] - a[p][p]) / (2.0 * a[p][q]);
                let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
                let t = if theta == 0.0 { 1.0 } else { t };
                let (c, s) = (1.0 / (t * t + 1.0).sqrt(), t / (t * t + 1.0).sqrt());
                for k in 0..8 {
                    let (akp, akq) = (a[k][p], a[k][q]);
                    a[k][p] = c * akp - s * akq;
                    a[k][q] = s * akp + c * akq;
                }
                for k in 0..8 {
                    let (apk, aqk) = (a[p][k], a[q][k]);
                    a[p][k] = c * apk - s * aqk;
                    a[q][k] = s * apk + c * aqk;
                }
            }
        }
    }
    std::array::from_fn(|i| a[i][i])
}

/// Align `points` (one level of a template) to `level` of a frame,
/// starting from `g` (region coordinates to that level's pixels).
#[allow(clippy::needless_range_loop)]
fn align_level(points: &[Point], level: &Level, mut g: Homography, iterations: usize) -> Option<(Homography, f64)> {
    let mut conditioning = 0.0;
    // Each pixel's weight in the last step, per channel: all at first.
    let mut trust: Vec<[f64; 2]> = vec![[1.0, 1.0]; points.len()];
    // The median strength of each channel: the half above it is where the
    // region's information is.
    let strong: [f64; 2] = std::array::from_fn(|c| {
        let mut v: Vec<f64> = points.iter().map(|p| p.strength[c]).collect();
        if v.is_empty() {
            return 0.0;
        }
        let mid = v.len() / 2;
        v.select_nth_unstable_by(mid, f64::total_cmp);
        v[mid]
    });
    let mut residual = Vec::with_capacity(points.len() * 2);
    for _ in 0..iterations {
        residual.clear();
        let mut seen: Vec<Option<[f64; 2]>> = Vec::with_capacity(points.len());
        for p in points {
            let at = g.map(p.u.0, p.u.1).and_then(|(x, y)| Some([level.chans[0].sample(x, y)? as f64, level.chans[1].sample(x, y)? as f64]));
            seen.push(at);
        }
        // The frame's light against the template's, per channel, fitted
        // each step over the pixels the last step trusted, so a change of
        // light is not read as a move and what passes in front does not
        // set the light: the brightness by a gain, an offset and a slope
        // across the region; the edges, which a slope barely touches and
        // which hold a plain surface by its outline, by a gain and an
        // offset alone.
        let light: [[f64; 4]; 2] = std::array::from_fn(|c| {
            let terms = if c == 0 { 4 } else { 2 };
            let mut a = [[0.0f64; 4]; 4];
            let mut b = [0.0f64; 4];
            let mut n = 0.0f64;
            for ((p, v), w) in points.iter().zip(&seen).zip(&trust) {
                if let Some(v) = v {
                    let w = w[c];
                    let row = [p.val[c] as f64, 1.0, p.u.0, p.u.1];
                    for i in 0..terms {
                        b[i] += w * row[i] * v[c];
                        for j in 0..terms {
                            a[i][j] += w * row[i] * row[j];
                        }
                    }
                    n += w;
                }
            }
            // Unused terms solve to nothing.
            for k in terms..4 {
                a[k][k] = 1.0;
            }
            match (n >= 16.0).then(|| crate::homography::solve(&mut a, &mut b)).flatten() {
                Some(x) if (0.3..=3.0).contains(&x[0]) => x,
                _ => [1.0, 0.0, 0.0, 0.0],
            }
        });
        for (p, v) in points.iter().zip(&seen) {
            residual.push(v.map(|v| -> [f64; 2] { std::array::from_fn(|c| { let l = &light[c]; (v[c] - l[1] - l[2] * p.u.0 - l[3] * p.u.1) / l[0] - p.val[c] as f64 }) }));
        }
        // The residuals' spread, read where the region has something to
        // say: on a plain surface most pixels are flat and their tiny
        // residuals would set the cut so low the edges, all that holds
        // the map, were cut away.
        let mut abs: Vec<f64> = points
            .iter()
            .zip(&residual)
            .filter_map(|(p, e)| e.map(|e| (p, e)))
            .flat_map(|(p, e)| (0..2).filter(move |&c| p.strength[c] >= strong[c]).map(move |c| e[c].abs()))
            .collect();
        if abs.len() < 16 {
            return None;
        }
        let mid = abs.len() / 2;
        abs.select_nth_unstable_by(mid, f64::total_cmp);
        let sigma = (abs[mid] * 1.4826).max(0.004);
        let cut = 4.685 * sigma;
        let mut hess = [[0.0f64; 8]; 8];
        let mut b = [0.0f64; 8];
        for ((p, e), t) in points.iter().zip(&residual).zip(trust.iter_mut()) {
            let Some(e) = e else { continue };
            for c in 0..2 {
                let r = e[c] / cut;
                if r.abs() >= 1.0 {
                    t[c] = 0.0;
                    continue;
                }
                let w = (1.0 - r * r) * (1.0 - r * r);
                t[c] = w;
                let sd = &p.sd[c];
                for i in 0..8 {
                    b[i] += w * sd[i] * e[c];
                    for j in i..8 {
                        hess[i][j] += w * sd[i] * sd[j];
                    }
                }
            }
        }
        for i in 0..8 {
            for j in 0..i {
                hess[i][j] = hess[j][i];
            }
        }
        let ev = eigenvalues(hess);
        let (lo, hi) = (ev.iter().copied().fold(f64::MAX, f64::min), ev.iter().copied().fold(0.0, f64::max));
        conditioning = if hi > 0.0 { (lo / hi).max(0.0) } else { 0.0 };
        let mut a = hess;
        let dp = crate::homography::solve(&mut a, &mut b)?;
        g = g.multiply(&step_map(&dp).inverse()?);
        if dp.iter().map(|v| v * v).sum::<f64>() < 1e-10 {
            break;
        }
    }
    Some((g, conditioning))
}

/// The top level searched for the best shift of `g` before it is
/// refined: a move larger than the refinement could reach in one go.
fn search_top(points: &[Point], level: &Level, g: Homography, radius: i32) -> Homography {
    let step = points.len().div_ceil(1500).max(1);
    let cost = |g: &Homography| {
        let (mut sum, mut n) = (0.0f64, 0usize);
        for p in points.iter().step_by(step) {
            if let Some((x, y)) = g.map(p.u.0, p.u.1) {
                if let Some(v) = level.chans[0].sample(x, y) {
                    sum += (v - p.val[0]).abs() as f64;
                    n += 1;
                }
            }
        }
        if n * 2 < points.len() / step { f64::MAX } else { sum / n as f64 }
    };
    let mut best = (cost(&g), g);
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let shifted = Homography { h: [[1.0, 0.0, dx as f64], [0.0, 1.0, dy as f64], [0.0, 0.0, 1.0]] }.multiply(&g);
            let c = cost(&shifted);
            if c < best.0 {
                best = (c, shifted);
            }
        }
    }
    best.1
}

/// Align `tpl` to `pyr` from `g` (region coordinates to the finest
/// pixels), coarse to fine from level `top` down to `bottom`, the top
/// searched `search` pixels around first.
fn align(tpl: &Template, pyr: &Pyramid, mut g: Homography, top: usize, bottom: usize, search: i32, iterations: usize) -> Option<Fit> {
    let top = top.min(pyr.levels.len() - 1).min(tpl.levels.len() - 1);
    let mut conditioning = 0.0;
    for l in (bottom..=top).rev() {
        let mut gl = to_level(l).multiply(&g);
        if l == top && search > 0 {
            gl = search_top(&tpl.levels[l], &pyr.levels[l], gl, search);
        }
        let (fitted, cond) = align_level(&tpl.levels[l], &pyr.levels[l], gl, iterations)?;
        conditioning = cond;
        g = from_level(l).multiply(&fitted);
    }
    Some(Fit { g, conditioning })
}

/// How alike the template's brightness and the frame's under `g` are at
/// level `l`, -1 to 1 (0 where either is flat).
fn zncc(tpl: &Template, pyr: &Pyramid, g: &Homography, l: usize) -> f64 {
    let l = l.min(pyr.levels.len() - 1).min(tpl.levels.len() - 1);
    let gl = to_level(l).multiply(g);
    let pairs: Vec<(f64, f64)> = tpl.levels[l].iter().filter_map(|p| {
        let (x, y) = gl.map(p.u.0, p.u.1)?;
        Some((p.val[0] as f64, pyr.levels[l].chans[0].sample(x, y)? as f64))
    }).collect();
    if pairs.len() < 16 {
        return 0.0;
    }
    let n = pairs.len() as f64;
    let (ma, mb) = (pairs.iter().map(|p| p.0).sum::<f64>() / n, pairs.iter().map(|p| p.1).sum::<f64>() / n);
    let (mut ab, mut aa, mut bb) = (0.0, 0.0, 0.0);
    for (a, b) in &pairs {
        ab += (a - ma) * (b - mb);
        aa += (a - ma) * (a - ma);
        bb += (b - mb) * (b - mb);
    }
    if aa < 1e-9 || bb < 1e-9 { 0.0 } else { ab / (aa * bb).sqrt() }
}

/// How sure a frame's fit is, from how alike the reference and the frame
/// are (`zncc`), how far the round trip missed (`round_trip`, pixels) on
/// a region `size` pixels across in this frame, and whether the region
/// holds the map at all (`conditioning`). A miss counts against the
/// region's size: a few pixels on a surface filling the frame is a
/// fraction of one on a small one.
fn confidence(zncc: f64, round_trip: f64, size: f64, conditioning: f64) -> f32 {
    let alike = ((zncc - 0.4) / 0.45).clamp(0.0, 1.0);
    let scale = (size / 200.0).max(1.0);
    let returns = (1.0 - (round_trip / scale - 0.5) / 3.0).clamp(0.0, 1.0);
    let held = if conditioning < 1e-7 { 0.3 } else { 1.0 };
    (alike * returns * held) as f32
}

/// The state a confidence makes.
fn state_of(confidence: f32) -> State {
    if confidence >= 0.6 {
        State::Good
    } else if confidence >= 0.2 {
        State::Low
    } else {
        State::Lost
    }
}

/// The pyramid's depth for a region `side` pixels across.
fn depth(side: f64, opts: &Options) -> usize {
    let mut levels = 1;
    while levels < 6 && side / (1u32 << levels) as f64 >= opts.top_side {
        levels += 1;
    }
    levels
}

/// What the track runs on: the CPU here, or the GPU's twin
/// (`crate::gpu::planar`). A pyramid of a frame, a template of a region of
/// one, an alignment of a template to a pyramid, and how alike they are.
pub(crate) trait Backend {
    type Pyr;
    type Tpl;
    fn pyramid(&mut self, frame: &GrayFrame, levels: usize) -> Self::Pyr;
    /// A template of `quad`, less what lies in `exclude` (pixels of
    /// that frame).
    fn template(&mut self, pyr: &Self::Pyr, quad: &[(f64, f64); 4], exclude: &[Vec<(f64, f64)>], opts: &Options) -> Self::Tpl;
    /// The template's region coordinates.
    fn region(&self, tpl: &Self::Tpl) -> Frame;
    #[allow(clippy::too_many_arguments)]
    fn align(&mut self, tpl: &Self::Tpl, pyr: &Self::Pyr, g: Homography, top: usize, bottom: usize, search: i32, iterations: usize) -> Option<Fit>;
    fn zncc(&mut self, tpl: &Self::Tpl, pyr: &Self::Pyr, g: &Homography, l: usize) -> f64;
}

/// Channel `c` of level `l` of a CPU pyramid, for the GPU's tests.
#[cfg(test)]
pub(crate) fn cpu_channel(pyr: &Pyramid, l: usize, c: usize) -> Vec<f32> {
    pyr.levels[l].chans[c].px.clone()
}

/// The track on the CPU: the reference the GPU's twin is held to.
pub(crate) struct Cpu;

impl Backend for Cpu {
    type Pyr = Pyramid;
    type Tpl = Template;
    fn pyramid(&mut self, frame: &GrayFrame, levels: usize) -> Pyramid {
        Pyramid::new(frame, levels)
    }
    fn template(&mut self, pyr: &Pyramid, quad: &[(f64, f64); 4], exclude: &[Vec<(f64, f64)>], opts: &Options) -> Template {
        Template::new(pyr, quad, exclude, opts)
    }
    fn region(&self, tpl: &Template) -> Frame {
        tpl.frame
    }
    fn align(&mut self, tpl: &Template, pyr: &Pyramid, g: Homography, top: usize, bottom: usize, search: i32, iterations: usize) -> Option<Fit> {
        align(tpl, pyr, g, top, bottom, search, iterations)
    }
    fn zncc(&mut self, tpl: &Template, pyr: &Pyramid, g: &Homography, l: usize) -> f64 {
        zncc(tpl, pyr, g, l)
    }
}

/// Follow the surface `quad` of `reference` through `frames`, each read
/// by `next`, one after another: for each, the map from the reference's
/// pixels to that frame's, with its confidence and state. What lies in an
/// `exclude` polygon (the reference's pixels; it moves with the surface)
/// is left out: a part of the area that isn't on the plane, a screen
/// playing in a wall. A lost frame holds the last good map; the track goes
/// on looking for the surface for [`Options::reacquire_for`] frames, then
/// stops. `progress` is told how far it has got and stops it by returning
/// false.
pub fn track(
    reference: &GrayFrame,
    quad: [(f64, f64); 4],
    exclude: &[Vec<(f64, f64)>],
    frames: &[usize],
    next: &mut dyn FnMut(usize) -> Option<GrayFrame>,
    opts: &Options,
    progress: &dyn Fn(f32) -> bool,
) -> Vec<Tracked> {
    track_on(&mut Cpu, reference, quad, exclude, frames, next, opts, progress)
}

/// [`track`] on `backend`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn track_on<B: Backend>(
    backend: &mut B,
    reference: &GrayFrame,
    quad: [(f64, f64); 4],
    exclude: &[Vec<(f64, f64)>],
    frames: &[usize],
    next: &mut dyn FnMut(usize) -> Option<GrayFrame>,
    opts: &Options,
    progress: &dyn Fn(f32) -> bool,
) -> Vec<Tracked> {
    let side = Frame::of(&quad).s * 2.0;
    let levels = depth(side, opts);
    let anchor_pyr = backend.pyramid(reference, levels);
    let anchor = backend.template(&anchor_pyr, &quad, exclude, opts);
    // What is left out, where a map takes it.
    let away = |h: &Homography| -> Vec<Vec<(f64, f64)>> { exclude.iter().map(|e| e.iter().map(|p| h.transform(p.0, p.1)).collect()).collect() };
    let anchor_frame = backend.region(&anchor);
    let to_anchor = anchor_frame.px().inverse().expect("a region of some size");
    let mut out = Vec::new();
    // The frame before: its pyramid, its region as a template, and the
    // map from the reference to it.
    let mut prev_pyr = anchor_pyr;
    let mut prev = backend.template(&prev_pyr, &quad, exclude, opts);
    let mut last = Homography::identity();
    // The move from the frame before the last to the last, the guess for
    // the next.
    let mut motion = Homography::identity();
    let mut failing = 0usize;
    let mut lost_for = 0usize;
    let top = levels - 1;
    let map_quad = |h: &Homography| quad.map(|p| h.transform(p.0, p.1));
    for (k, &f) in frames.iter().enumerate() {
        if !progress(k as f32 / frames.len().max(1) as f32) {
            break;
        }
        let Some(cur) = next(f) else { break };
        let cur_pyr = backend.pyramid(&cur, levels);
        let lost = failing >= opts.lost_after;
        let fitted = if lost {
            // Looking for the surface again: the reference, from where it
            // was last, searched widely.
            backend.align(&anchor, &cur_pyr, last.multiply(&anchor_frame.px()), top, 0, opts.search * 3, opts.iterations).map(|a| (a.g.multiply(&to_anchor), a.conditioning, None))
        } else {
            // From the frame before, then refined against the reference.
            let prev_to_px = backend.region(&prev).px();
            backend.align(&prev, &cur_pyr, motion.multiply(&prev_to_px), top, 0, opts.search, opts.iterations).and_then(|step| {
                let moved = step.g.multiply(&prev_to_px.inverse()?);
                let guess = moved.multiply(&last);
                let refined = backend.align(&anchor, &cur_pyr, guess.multiply(&anchor_frame.px()), 1, 0, 0, opts.iterations)?;
                Some((refined.g.multiply(&to_anchor), refined.conditioning.min(step.conditioning), Some(moved)))
            })
        };
        let Some((h, conditioning, moved)) = fitted else {
            failing += 1;
            out.push(Tracked { frame: f, h: last, confidence: 0.0, state: State::Lost, round_trip_px: f32::INFINITY });
            if failing >= opts.lost_after {
                lost_for += 1;
                if lost_for > opts.reacquire_for {
                    break;
                }
            }
            continue;
        };
        let alike = backend.zncc(&anchor, &cur_pyr, &h.multiply(&anchor_frame.px()), 1);
        // Back to the frame before from this one: where the corners land
        // against where they were.
        let here = map_quad(&h);
        let cur_tpl = backend.template(&cur_pyr, &here, &away(&h), opts);
        let round_trip = match moved.and_then(|m| m.inverse()) {
            Some(back_guess) => {
                let cur_to_px = backend.region(&cur_tpl).px();
                match backend.align(&cur_tpl, &prev_pyr, back_guess.multiply(&cur_to_px), top, 0, 0, opts.iterations).and_then(|b| Some(b.g.multiply(&cur_to_px.inverse()?))) {
                    Some(back) => {
                        let before = map_quad(&last);
                        here.iter().zip(&before).map(|(c, b)| { let (x, y) = back.transform(c.0, c.1); (x - b.0).hypot(y - b.1) }).sum::<f64>() / 4.0
                    }
                    None => f64::INFINITY,
                }
            }
            // Found again after a loss: no frame before to go back to.
            None => if alike > 0.75 { 0.5 } else { f64::INFINITY },
        };
        let size = here.iter().map(|p| (p.0 - here[0].0).hypot(p.1 - here[0].1)).fold(0.0, f64::max);
        let confidence = confidence(alike, round_trip, size, conditioning);
        let state = state_of(confidence);
        if state == State::Lost {
            failing += 1;
            out.push(Tracked { frame: f, h: last, confidence, state, round_trip_px: round_trip as f32 });
            if failing >= opts.lost_after {
                lost_for += 1;
                if lost_for > opts.reacquire_for {
                    break;
                }
            }
            continue;
        }
        // Followed: the next frame's guess is this frame's move.
        motion = match moved {
            Some(m) => m,
            None => Homography::identity(),
        };
        failing = 0;
        lost_for = 0;
        last = h;
        prev_pyr = cur_pyr;
        prev = cur_tpl;
        out.push(Tracked { frame: f, h, confidence, state, round_trip_px: round_trip as f32 });
    }
    out
}

/// A frame where the surface is known (#2754): the reference frame, or a
/// correction set by hand. `h` maps the reference's pixels to its own.
#[derive(Debug, Clone, Copy)]
pub struct Anchor {
    pub frame: usize,
    pub h: Homography,
}

/// The surface re-solved between two anchors (#2754): followed forward
/// from `start` and backward from `end`, each run held to its own anchor,
/// and the two blended frame by frame, each weighted by its confidence and
/// by how near its anchor is. With one anchor missing, the run from the
/// other goes to the end of `span` (the first and last frames there are).
/// `quad` and `exclude` are the area followed and the parts left out, in
/// the reference's pixels; each run takes them to its anchor. The maps
/// returned are from the reference's pixels, for the frames strictly
/// between the anchors, in order.
#[allow(clippy::too_many_arguments)]
pub fn solve_between(
    quad: [(f64, f64); 4],
    exclude: &[Vec<(f64, f64)>],
    start: Option<Anchor>,
    end: Option<Anchor>,
    span: (usize, usize),
    frame: &mut dyn FnMut(usize) -> Option<GrayFrame>,
    opts: &Options,
    progress: &dyn Fn(f32) -> bool,
) -> Vec<Tracked> {
    solve_between_on(&mut Cpu, quad, exclude, start, end, span, frame, opts, progress)
}

/// [`solve_between`] on `backend`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn solve_between_on<B: Backend>(
    backend: &mut B,
    quad: [(f64, f64); 4],
    exclude: &[Vec<(f64, f64)>],
    start: Option<Anchor>,
    end: Option<Anchor>,
    span: (usize, usize),
    frame: &mut dyn FnMut(usize) -> Option<GrayFrame>,
    opts: &Options,
    progress: &dyn Fn(f32) -> bool,
) -> Vec<Tracked> {
    let lo = start.map_or(span.0, |a| a.frame + 1);
    let hi = end.map_or(span.1, |a| a.frame.saturating_sub(1));
    if lo > hi {
        return Vec::new();
    }
    let ways = usize::from(start.is_some()) + usize::from(end.is_some());
    // One run from an anchor over `frames`: maps from the reference.
    let mut run = |a: Anchor, frames: Vec<usize>, part: usize| -> Vec<Tracked> {
        let Some(at) = frame(a.frame) else { return Vec::new() };
        let moved = quad.map(|p| a.h.transform(p.0, p.1));
        let away: Vec<Vec<(f64, f64)>> = exclude.iter().map(|e| e.iter().map(|p| a.h.transform(p.0, p.1)).collect()).collect();
        let share = |p: f32| progress((part as f32 + p) / ways as f32);
        let mut t = track_on(backend, &at, moved, &away, &frames, frame, opts, &share);
        for x in &mut t {
            x.h = x.h.multiply(&a.h);
        }
        t
    };
    let ahead = start.map(|a| run(a, (lo..=hi).collect(), 0)).unwrap_or_default();
    let behind = end.map(|b| run(b, (lo..=hi).rev().collect(), usize::from(start.is_some()))).unwrap_or_default();
    blend(&quad, start, end, &ahead, &behind)
}

/// The runs from two anchors as one: `ahead` from `a` (frames in the
/// order followed) and `behind` from `b` (likewise, so latest first).
///
/// Between two anchors, a run counts only until it first loses the
/// surface: found again after that, it is no longer held to its anchor,
/// and a plain or long surface can be found again in the wrong place with
/// confidence (a stripe matches itself shifted along its length). The
/// other run covers those frames from its own anchor. With one anchor, its
/// run is taken whole, found again or not, less a lost tail.
///
/// A frame both runs reach is the two mixed, each weighted by its
/// confidence and by how near its anchor is; one only one reaches, or one
/// run lost, is that run's. Frames neither reaches between two known ones
/// (an anchor or a frame followed) are bridged straight from one to the
/// other and stay lost: something passed in front from both sides.
pub fn blend(quad: &[(f64, f64); 4], a: Option<Anchor>, b: Option<Anchor>, ahead: &[Tracked], behind: &[Tracked]) -> Vec<Tracked> {
    let held = |run: &[Tracked], other: bool| -> Vec<Tracked> {
        let mut run: Vec<Tracked> = if other { run.iter().take_while(|t| t.state != State::Lost).copied().collect() } else { run.to_vec() };
        while run.last().is_some_and(|t| t.state == State::Lost) {
            run.pop();
        }
        run
    };
    let (ahead, behind) = (held(ahead, b.is_some()), held(behind, a.is_some()));
    let mut frames: Vec<usize> = ahead.iter().chain(&behind).map(|t| t.frame).collect();
    frames.sort_unstable();
    frames.dedup();
    let find = |run: &[Tracked], f: usize| run.iter().find(|t| t.frame == f).copied();
    let (fa, fb) = (a.map(|a| a.frame), b.map(|b| b.frame));
    let mut out: Vec<Tracked> = frames
        .into_iter()
        .filter_map(|f| {
            let (x, y) = match (find(&ahead, f), find(&behind, f)) {
                (Some(x), Some(y)) => (x, y),
                (Some(x), None) | (None, Some(x)) => return Some(x),
                (None, None) => return None,
            };
            match (x.state == State::Lost, y.state == State::Lost) {
                (true, false) => return Some(y),
                (false, true) => return Some(x),
                _ => {}
            }
            // Nearness: the far anchor's share falls to nothing at the near one.
            let (da, db) = (fa.map_or(1.0, |a| f.abs_diff(a) as f64), fb.map_or(1.0, |b| f.abs_diff(b) as f64));
            let (wx, wy) = (x.confidence.max(1e-3) as f64 * db, y.confidence.max(1e-3) as f64 * da);
            let k = wy / (wx + wy);
            if x.state == State::Lost {
                // Both lost: the nearer hold.
                return Some(if k > 0.5 { y } else { x });
            }
            let h = between(quad, &x.h, &y.h, k);
            let confidence = (x.confidence as f64 * (1.0 - k) + y.confidence as f64 * k) as f32;
            let round_trip_px = (x.round_trip_px as f64 * (1.0 - k) + y.round_trip_px as f64 * k) as f32;
            Some(Tracked { frame: f, h, confidence, state: state_of(confidence), round_trip_px })
        })
        .collect();
    // The gaps: between each pair of known frames, anchors included.
    let mut known: Vec<(usize, Homography)> = out.iter().map(|t| (t.frame, t.h)).chain(a.map(|a| (a.frame, a.h))).chain(b.map(|b| (b.frame, b.h))).collect();
    known.sort_by_key(|k| k.0);
    for w in known.windows(2) {
        let ((f0, h0), (f1, h1)) = (w[0], w[1]);
        for f in f0 + 1..f1 {
            let k = (f - f0) as f64 / (f1 - f0) as f64;
            out.push(Tracked { frame: f, h: between(quad, &h0, &h1, k), confidence: 0.0, state: State::Lost, round_trip_px: f32::INFINITY });
        }
    }
    out.sort_by_key(|t| t.frame);
    out
}

/// The map whose corners of `quad` are `k` of the way from where `x`
/// takes them to where `y` does.
fn between(quad: &[(f64, f64); 4], x: &Homography, y: &Homography, k: f64) -> Homography {
    let to: [(f64, f64); 4] = std::array::from_fn(|i| {
        let (p, q) = (x.transform(quad[i].0, quad[i].1), y.transform(quad[i].0, quad[i].1));
        (p.0 + (q.0 - p.0) * k, p.1 + (q.1 - p.1) * k)
    });
    Homography::from_4_points(quad, &to).unwrap_or(if k > 0.5 { *y } else { *x })
}

/// How light `quad` is in `frame` (#2751): the mean of a 16 × 16 grid
/// of points spread over it (bilinear across its corners), 0 to 1 as
/// the frame encodes it. Points off the frame are left out; `None` when
/// all are.
pub fn region_light(frame: &GrayFrame, quad: &[(f64, f64); 4]) -> Option<f32> {
    const N: usize = 16;
    let (mut sum, mut n) = (0.0f64, 0usize);
    for j in 0..N {
        for i in 0..N {
            let (u, v) = ((i as f64 + 0.5) / N as f64, (j as f64 + 0.5) / N as f64);
            let top = (quad[0].0 + (quad[1].0 - quad[0].0) * u, quad[0].1 + (quad[1].1 - quad[0].1) * u);
            let bottom = (quad[3].0 + (quad[2].0 - quad[3].0) * u, quad[3].1 + (quad[2].1 - quad[3].1) * u);
            let (x, y) = (top.0 + (bottom.0 - top.0) * v, top.1 + (bottom.1 - top.1) * v);
            if x < 0.0 || y < 0.0 || x >= frame.width as f64 || y >= frame.height as f64 {
                continue;
            }
            sum += frame.data[y as usize * frame.width + x as usize] as f64;
            n += 1;
        }
    }
    (n > 0).then(|| (sum / n as f64 / 255.0) as f32)
}

/// The spans of `tracked` (in order) that were not followed with
/// confidence: `(first, last, state)` for each run of Low or Lost frames.
pub fn weak_spans(tracked: &[Tracked]) -> Vec<(usize, usize, State)> {
    let mut out: Vec<(usize, usize, State)> = Vec::new();
    for t in tracked.iter().filter(|t| t.state != State::Good) {
        match out.last_mut() {
            Some(span) if span.2 == t.state && tracked.iter().position(|x| x.frame == span.1).and_then(|i| tracked.get(i + 1)).is_some_and(|n| n.frame == t.frame) => span.1 = t.frame,
            _ => out.push((t.frame, t.frame, t.state)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A textured world: a sum of sines, bright and dark at many scales.
    fn texture(x: f64, y: f64) -> f64 {
        128.0 + 40.0 * (x * 0.21).sin() * (y * 0.17).cos() + 30.0 * (x * 0.05 + y * 0.07).sin() + 25.0 * ((x - y) * 0.11).cos() + 15.0 * (x * 0.6).sin() * (y * 0.5).sin()
    }

    /// A frame of `world` (reference coordinates) seen through `map`
    /// (reference pixels to this frame's).
    fn seen(w: usize, h: usize, map: &Homography, world: &dyn Fn(f64, f64) -> f64) -> GrayFrame {
        let back = map.inverse().unwrap();
        let mut data = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                // Four samples a pixel, so edges are smooth as a camera's.
                let mut v = 0.0;
                for (ox, oy) in [(-0.25, -0.25), (0.25, -0.25), (-0.25, 0.25), (0.25, 0.25)] {
                    let (rx, ry) = back.transform(x as f64 + ox, y as f64 + oy);
                    v += world(rx, ry) / 4.0;
                }
                data[y * w + x] = v.clamp(0.0, 255.0) as u8;
            }
        }
        GrayFrame { data, width: w, height: h }
    }

    /// The map that takes `quad` to its corners moved by `course` × `k`.
    fn course_map(quad: &[(f64, f64); 4], course: &[(f64, f64); 4], k: usize) -> Homography {
        let to: [(f64, f64); 4] = std::array::from_fn(|i| (quad[i].0 + course[i].0 * k as f64, quad[i].1 + course[i].1 * k as f64));
        Homography::from_4_points(quad, &to).unwrap()
    }

    fn corner_error(quad: &[(f64, f64); 4], got: &Homography, want: &Homography) -> f64 {
        quad.iter().map(|p| { let (a, b) = (got.transform(p.0, p.1), want.transform(p.0, p.1)); (a.0 - b.0).hypot(a.1 - b.1) }).sum::<f64>() / 4.0
    }

    fn run(w: usize, h: usize, quad: [(f64, f64); 4], n: usize, map: &dyn Fn(usize) -> Homography, world: &dyn Fn(f64, f64) -> f64) -> Vec<Tracked> {
        let reference = seen(w, h, &Homography::identity(), world);
        let frames: Vec<usize> = (1..=n).collect();
        track(&reference, quad, &[], &frames, &mut |k| Some(seen(w, h, &map(k), world)), &Options::default(), &|_| true)
    }

    /// A textured surface turning away in perspective is followed, each
    /// corner within a pixel, every frame confident, no drift.
    #[test]
    fn a_textured_surface_in_perspective_is_followed_without_drift() {
        let (w, h) = (320, 220);
        let quad = [(100.0, 60.0), (220.0, 70.0), (215.0, 160.0), (95.0, 150.0)];
        let course = [(0.9, -0.4), (0.5, 0.6), (0.7, -0.2), (1.1, 0.5)];
        let got = run(w, h, quad, 30, &|k| course_map(&quad, &course, k), &texture);
        assert_eq!(got.len(), 30);
        for t in &got {
            let e = corner_error(&quad, &t.h, &course_map(&quad, &course, t.frame));
            assert!(e < 0.6, "frame {}: corners {e:.3} px off", t.frame);
            assert_eq!(t.state, State::Good, "frame {}: {} confident", t.frame, t.confidence);
        }
    }

    /// A plain surface, one colour with its outline against a different
    /// plain background, is held by that outline: where points inside it
    /// find nothing.
    #[test]
    fn a_plain_surface_is_held_by_its_outline() {
        let (w, h) = (320, 220);
        let quad = [(110.0, 70.0), (210.0, 75.0), (205.0, 150.0), (105.0, 145.0)];
        let plain = move |x: f64, y: f64| if inside(&quad, (x, y)) { 200.0 } else { 70.0 + 10.0 * ((x * 0.02).sin() + (y * 0.03).cos()) };
        let course = [(0.8, 0.3), (0.6, -0.2), (0.9, 0.4), (0.5, 0.1)];
        let got = run(w, h, quad, 24, &|k| course_map(&quad, &course, k), &plain);
        assert_eq!(got.len(), 24);
        let worst = got.iter().map(|t| corner_error(&quad, &t.h, &course_map(&quad, &course, t.frame))).fold(0.0, f64::max);
        assert!(worst < 1.2, "a plain surface followed by its outline, worst {worst:.3} px");
        assert!(got.iter().all(|t| t.state != State::Lost));
    }

    /// Light falling across the surface during the shot moves nothing.
    #[test]
    fn a_change_of_light_is_not_a_move() {
        let (w, h) = (300, 200);
        let quad = [(90.0, 60.0), (200.0, 60.0), (200.0, 140.0), (90.0, 140.0)];
        let reference = seen(w, h, &Homography::identity(), &texture);
        let frames: Vec<usize> = (1..=12).collect();
        let shift = |k: usize| Homography { h: [[1.0, 0.0, 0.7 * k as f64], [0.0, 1.0, -0.4 * k as f64], [0.0, 0.0, 1.0]] };
        let got = track(&reference, quad, &[], &frames, &mut |k| {
            let lit = move |x: f64, y: f64| texture(x, y) * (1.0 - 0.04 * k as f64) + 2.0 * k as f64 * (x / 300.0);
            Some(seen(w, h, &shift(k), &lit))
        }, &Options::default(), &|_| true);
        assert_eq!(got.len(), 12);
        for t in &got {
            // Within a pixel as the light falls by two fifths and slopes
            // across the frame.
            assert!(corner_error(&quad, &t.h, &shift(t.frame)) < 0.8, "frame {} off {:.3} conf {} {:?}", t.frame, corner_error(&quad, &t.h, &shift(t.frame)), t.confidence, t.state);
        }
    }

    /// Something passing in front of part of the surface pulls the fit
    /// little: the rest holds it.
    #[test]
    fn something_passing_in_front_does_not_carry_the_surface_off() {
        let (w, h) = (320, 220);
        let quad = [(90.0, 50.0), (230.0, 50.0), (230.0, 170.0), (90.0, 170.0)];
        let reference = seen(w, h, &Homography::identity(), &texture);
        let frames: Vec<usize> = (1..=16).collect();
        let shift = |k: usize| Homography { h: [[1.0, 0.0, 0.5 * k as f64], [0.0, 1.0, 0.3 * k as f64], [0.0, 0.0, 1.0]] };
        let got = track(&reference, quad, &[], &frames, &mut |k| {
            let mut f = seen(w, h, &shift(k), &texture);
            // A dark bar moving the other way, over a fifth of the region.
            let bar = 100 + 3 * k;
            for y in 40..180 {
                for x in bar..bar + 28 {
                    f.data[y * w + x] = 20;
                }
            }
            Some(f)
        }, &Options::default(), &|_| true);
        assert_eq!(got.len(), 16);
        let worst = got.iter().map(|t| corner_error(&quad, &t.h, &shift(t.frame))).fold(0.0, f64::max);
        assert!(worst < 1.0, "worst {worst:.3} px");
    }

    /// A screen in the wall, playing something else every frame, over a
    /// third of the area tracked: left out, the wall holds the track
    /// exactly; left in, the track is no better.
    #[test]
    fn a_part_left_out_does_not_pull_the_track() {
        let (w, h) = (320, 220);
        let quad = [(80.0, 50.0), (240.0, 50.0), (240.0, 170.0), (80.0, 170.0)];
        // The screen, in the reference's pixels; it moves with the wall.
        let screen = vec![(140.0, 70.0), (225.0, 70.0), (225.0, 150.0), (140.0, 150.0)];
        let course = [(0.7, -0.3), (0.5, 0.5), (0.6, -0.2), (0.9, 0.4)];
        let play = |k: usize| move |x: f64, y: f64| {
            if in_polygon(&[(140.0, 70.0), (225.0, 70.0), (225.0, 150.0), (140.0, 150.0)], (x, y)) {
                // A pattern that runs across the screen fast.
                128.0 + 90.0 * ((x + 7.0 * k as f64) * 0.3).sin() * ((y - 5.0 * k as f64) * 0.25).cos()
            } else {
                texture(x, y)
            }
        };
        let reference = seen(w, h, &Homography::identity(), &play(0));
        let frames: Vec<usize> = (1..=16).collect();
        let run = |exclude: &[Vec<(f64, f64)>]| track(&reference, quad, exclude, &frames, &mut |k| Some(seen(w, h, &course_map(&quad, &course, k), &play(k))), &Options::default(), &|_| true);
        let worst = |got: &[Tracked]| got.iter().map(|t| corner_error(&quad, &t.h, &course_map(&quad, &course, t.frame))).fold(0.0, f64::max);
        let without = run(std::slice::from_ref(&screen));
        assert_eq!(without.len(), 16);
        assert!(without.iter().all(|t| t.state == State::Good), "{:?}", without.iter().map(|t| t.state).collect::<Vec<_>>());
        assert!(worst(&without) < 0.6, "the screen left out: {:.3} px", worst(&without));
        let with = run(&[]);
        assert!(worst(&with) >= worst(&without), "left in it is no better: {:.3} vs {:.3}", worst(&with), worst(&without));
    }

    /// A fast pan, many pixels a frame, is caught by the coarse search.
    #[test]
    fn a_fast_pan_is_caught() {
        let (w, h) = (360, 220);
        let quad = [(60.0, 60.0), (160.0, 60.0), (160.0, 150.0), (60.0, 150.0)];
        let pan = |k: usize| Homography { h: [[1.0, 0.0, 9.0 * k as f64], [0.0, 1.0, 2.0 * k as f64], [0.0, 0.0, 1.0]] };
        let got = run(w, h, quad, 14, &pan, &texture);
        assert_eq!(got.len(), 14);
        for t in &got {
            assert!(corner_error(&quad, &t.h, &pan(t.frame)) < 0.8, "frame {}", t.frame);
        }
    }

    /// When the surface is gone the frames are flagged lost, holding the
    /// last good map, and when it comes back the track picks it up.
    #[test]
    fn a_lost_surface_is_flagged_and_found_again() {
        let (w, h) = (300, 200);
        let quad = [(90.0, 60.0), (200.0, 60.0), (200.0, 140.0), (90.0, 140.0)];
        let reference = seen(w, h, &Homography::identity(), &texture);
        let flat = GrayFrame { data: vec![128; w * h], width: w, height: h };
        let shift = |k: usize| Homography { h: [[1.0, 0.0, 0.6 * k as f64], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] };
        let frames: Vec<usize> = (1..=20).collect();
        let got = track(&reference, quad, &[], &frames, &mut |k| Some(if (6..12).contains(&k) { flat.clone() } else { seen(w, h, &shift(k), &texture) }), &Options::default(), &|_| true);
        let state = |k: usize| got.iter().find(|t| t.frame == k).map(|t| t.state);
        for k in 6..12 {
            assert_eq!(state(k), Some(State::Lost), "frame {k} has no surface");
        }
        let held = got.iter().find(|t| t.frame == 8).unwrap();
        assert!(corner_error(&quad, &held.h, &shift(5)) < 0.6, "a lost frame holds the last good map");
        for k in 12..=20 {
            let t = got.iter().find(|t| t.frame == k).unwrap_or_else(|| panic!("frame {k} followed again"));
            assert_ne!(t.state, State::Lost, "frame {k}");
            assert!(corner_error(&quad, &t.h, &shift(k)) < 0.8, "frame {k}");
        }
        assert_eq!(weak_spans(&got).iter().filter(|s| s.2 == State::Lost).map(|s| (s.0, s.1)).collect::<Vec<_>>(), vec![(6, 11)]);
    }

    /// [`solve_between`] over `frame`, every frame it gives in the span.
    fn solve(quad: [(f64, f64); 4], start: Option<Anchor>, end: Option<Anchor>, span: (usize, usize), frame: &dyn Fn(usize) -> GrayFrame) -> Vec<Tracked> {
        solve_between(quad, &[], start, end, span, &mut |k| Some(frame(k)), &Options::default(), &|_| true)
            .into_iter()
            .inspect(|t| assert!(t.frame >= span.0 && t.frame <= span.1, "frame {} is in the span", t.frame))
            .collect()
    }

    /// Between two anchors where the surface truly is, each frame is
    /// found from both and is where the surface is, confident.
    #[test]
    fn between_two_anchors_each_frame_is_where_the_surface_is() {
        let (w, h) = (320, 220);
        let quad = [(100.0, 60.0), (220.0, 70.0), (215.0, 160.0), (95.0, 150.0)];
        let course = [(0.9, -0.4), (0.5, 0.6), (0.7, -0.2), (1.1, 0.5)];
        let map = |k: usize| course_map(&quad, &course, k);
        let got = solve(quad, Some(Anchor { frame: 0, h: map(0) }), Some(Anchor { frame: 20, h: map(20) }), (0, 20), &|k| seen(w, h, &map(k), &texture));
        assert_eq!(got.iter().map(|t| t.frame).collect::<Vec<_>>(), (1..20).collect::<Vec<_>>());
        for t in &got {
            assert!(corner_error(&quad, &t.h, &map(t.frame)) < 0.6, "frame {}", t.frame);
            assert_eq!(t.state, State::Good, "frame {}", t.frame);
        }
    }

    /// A correction pulls the frames near it onto the surface: from a
    /// start anchor four pixels off and a correct one at the end, the
    /// frames near the end are within a pixel, and the error falls the
    /// nearer they are.
    #[test]
    fn a_correction_pulls_the_frames_near_it_into_place() {
        let (w, h) = (320, 220);
        let quad = [(100.0, 60.0), (220.0, 60.0), (220.0, 160.0), (100.0, 160.0)];
        let shift = |k: usize| Homography { h: [[1.0, 0.0, 0.6 * k as f64], [0.0, 1.0, 0.3 * k as f64], [0.0, 0.0, 1.0]] };
        let off = Homography { h: [[1.0, 0.0, 4.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] };
        let got = solve(quad, Some(Anchor { frame: 0, h: off }), Some(Anchor { frame: 24, h: shift(24) }), (0, 24), &|k| seen(w, h, &shift(k), &texture));
        assert_eq!(got.len(), 23);
        let err = |k: usize| corner_error(&quad, &got.iter().find(|t| t.frame == k).unwrap().h, &shift(k));
        assert!(err(23) < 1.0, "next to the correction: {:.3} px", err(23));
        assert!(err(2) > err(12) && err(12) > err(22), "{:.2} {:.2} {:.2}", err(2), err(12), err(22));
    }

    /// With no anchor after it, a correction's run goes on to the end of
    /// the span, as a track from it would.
    #[test]
    fn with_one_anchor_the_run_goes_to_the_end() {
        let (w, h) = (300, 200);
        let quad = [(90.0, 60.0), (200.0, 60.0), (200.0, 140.0), (90.0, 140.0)];
        let shift = |k: usize| Homography { h: [[1.0, 0.0, 0.5 * k as f64], [0.0, 1.0, -0.2 * k as f64], [0.0, 0.0, 1.0]] };
        let forward = solve(quad, Some(Anchor { frame: 5, h: shift(5) }), None, (0, 15), &|k| seen(w, h, &shift(k), &texture));
        assert_eq!(forward.iter().map(|t| t.frame).collect::<Vec<_>>(), (6..=15).collect::<Vec<_>>());
        let backward = solve(quad, None, Some(Anchor { frame: 5, h: shift(5) }), (0, 15), &|k| seen(w, h, &shift(k), &texture));
        assert_eq!(backward.iter().map(|t| t.frame).collect::<Vec<_>>(), (0..5).collect::<Vec<_>>());
        for t in forward.iter().chain(&backward) {
            assert!(corner_error(&quad, &t.h, &shift(t.frame)) < 0.8, "frame {}", t.frame);
        }
    }

    /// Frames where the surface is gone stay lost from both sides,
    /// bridged straight across from the frames either side; the frames
    /// round them are each found from the anchor that reaches them.
    #[test]
    fn a_gap_between_anchors_stays_lost_and_the_rest_is_found() {
        let (w, h) = (300, 200);
        let quad = [(90.0, 60.0), (200.0, 60.0), (200.0, 140.0), (90.0, 140.0)];
        let flat = GrayFrame { data: vec![128; w * h], width: w, height: h };
        let shift = |k: usize| Homography { h: [[1.0, 0.0, 0.6 * k as f64], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] };
        let frame = |k: usize| if (9..13).contains(&k) { flat.clone() } else { seen(w, h, &shift(k), &texture) };
        let got = solve(quad, Some(Anchor { frame: 0, h: shift(0) }), Some(Anchor { frame: 22, h: shift(22) }), (0, 22), &frame);
        for t in &got {
            if (9..13).contains(&t.frame) {
                assert_eq!(t.state, State::Lost, "frame {} has no surface", t.frame);
                // The course is straight, so the bridge is on it.
                assert!(corner_error(&quad, &t.h, &shift(t.frame)) < 0.8, "frame {} bridged", t.frame);
            } else {
                assert_ne!(t.state, State::Lost, "frame {}", t.frame);
                assert!(corner_error(&quad, &t.h, &shift(t.frame)) < 0.8, "frame {}", t.frame);
            }
        }
        assert_eq!(got.iter().map(|t| t.frame).collect::<Vec<_>>(), (1..22).collect::<Vec<_>>(), "every frame between the anchors");
    }

    /// The blend: a frame one run lost is the other's; a frame both have
    /// is nearer the run whose anchor is nearer.
    #[test]
    fn the_blend_leans_to_the_nearer_anchor_and_skips_a_lost_run() {
        let quad = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        let at = |f: usize, dx: f64, state: State| Tracked { frame: f, h: Homography { h: [[1.0, 0.0, dx], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] }, confidence: if state == State::Lost { 0.0 } else { 1.0 }, state, round_trip_px: 0.0 };
        let ahead = [at(1, 0.0, State::Good), at(2, 0.0, State::Lost)];
        let behind = [at(2, 4.0, State::Good), at(1, 4.0, State::Good)];
        let anchor = |frame: usize, dx: f64| Anchor { frame, h: Homography { h: [[1.0, 0.0, dx], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] } };
        let got = blend(&quad, Some(anchor(0, 0.0)), Some(anchor(4, 4.0)), &ahead, &behind);
        // Frame 1: a quarter of the way, so three quarters the start's.
        assert!((got[0].h.transform(0.0, 0.0).0 - 1.0).abs() < 1e-6, "{:?}", got[0].h);
        assert_eq!(got[1].h.transform(0.0, 0.0).0, 4.0, "the lost run is skipped");
    }

    /// Between two anchors a run found again after a loss is not used: a
    /// long surface can be found again shifted along itself, confident.
    /// The frames after the loss are the other run's.
    #[test]
    fn a_run_found_again_after_a_loss_yields_to_the_other_anchor() {
        let quad = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        let shift = |dx: f64| Homography { h: [[1.0, 0.0, dx], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] };
        let at = |f: usize, dx: f64, state: State| Tracked { frame: f, h: shift(dx), confidence: if state == State::Lost { 0.0 } else { 0.95 }, state, round_trip_px: 0.0 };
        // Forward from 0: good to 2, lost at 3, "found" at 4 and 5 forty
        // pixels off. Backward from 6: good all the way, on the course.
        let ahead = [at(1, 1.0, State::Good), at(2, 2.0, State::Good), at(3, 2.0, State::Lost), at(4, 44.0, State::Good), at(5, 45.0, State::Good)];
        let behind = [at(5, 5.0, State::Good), at(4, 4.0, State::Good), at(3, 3.0, State::Good), at(2, 2.0, State::Good), at(1, 1.0, State::Good)];
        let got = blend(&quad, Some(Anchor { frame: 0, h: shift(0.0) }), Some(Anchor { frame: 6, h: shift(6.0) }), &ahead, &behind);
        for t in &got {
            assert!((t.h.transform(0.0, 0.0).0 - t.frame as f64).abs() < 1e-6, "frame {}: {:?}", t.frame, t.h);
        }
        // With no anchor after, the forward run is all there is: taken
        // whole, found again or not.
        let alone = blend(&quad, Some(Anchor { frame: 0, h: shift(0.0) }), None, &ahead, &[]);
        assert_eq!(alone.iter().find(|t| t.frame == 4).unwrap().h.transform(0.0, 0.0).0, 44.0);
    }

    /// #2751: a region's light is its mean brightness, what is off the
    /// frame left out.
    #[test]
    fn a_regions_light_is_its_mean() {
        let (w, h) = (100, 50);
        let mut data = vec![0u8; w * h];
        for y in 0..h {
            for x in 50..w {
                data[y * w + x] = 200;
            }
        }
        let f = GrayFrame { data, width: w, height: h };
        let all = region_light(&f, &[(0.0, 0.0), (100.0, 0.0), (100.0, 50.0), (0.0, 50.0)]).unwrap();
        assert!((all - 100.0 / 255.0).abs() < 0.01, "{all}");
        let half_off = region_light(&f, &[(50.0, 0.0), (150.0, 0.0), (150.0, 50.0), (50.0, 50.0)]).unwrap();
        assert!((half_off - 200.0 / 255.0).abs() < 0.01, "only what is on the frame: {half_off}");
        assert_eq!(region_light(&f, &[(200.0, 0.0), (300.0, 0.0), (300.0, 50.0), (200.0, 50.0)]), None);
    }
}

