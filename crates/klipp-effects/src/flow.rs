//! Optical-flow frame interpolation: the in-between frames of a slowed
//! clip drawn by following each pixel's motion, instead of repeating a
//! frame (judder) or cross-fading two (ghosting).
//!
//! Two neighbouring source frames, A and B, are reduced to a luma
//! pyramid at a capped work size ([`WORK_MAX_WIDTH`]). From the coarsest
//! level down, each pixel searches a small window around the motion the
//! level above predicted, by patch difference, and the field is
//! smoothed by match quality. That gives a dense flow both ways: A to B
//! and B to A.
//!
//! An in-between frame at `t` (0 at A, 1 at B) samples A and B where its
//! pixel came from and goes to, with the flow at time `t` approximated
//! from the two fields (the quadratic form of Jiang et al., "Super
//! SloMo"). Each side's sample counts by how well the two fields agree
//! there (forward-backward consistency): where they disagree, the pixel
//! was covered or uncovered and that side doesn't see it. Where neither
//! side is trusted, or the two samples don't look alike, the pixel falls
//! back to the plain cross-fade. Where most of the frame can't be
//! followed (a cut inside the media, motion past the search range) the
//! whole frame is the plain cross-fade ([`UNRELIABLE_FRACTION`]).
//!
//! This is the CPU reference. The renderer runs the same steps on the
//! GPU (`gpu::flow`), whose shaders mirror these functions one to one;
//! the two agree to within float rounding. Only `floor` is used to place
//! samples, never `round`: WGSL's `round` takes halves to even, Rust's
//! away from zero.

use rayon::prelude::*;

/// The widest the flow is estimated at, in pixels: a wider frame is
/// reduced to this. The warp still runs at the frame's own size, with
/// the flow scaled up to it.
pub const WORK_MAX_WIDTH: u32 = 960;

/// The most pyramid levels: with [`SEARCH_RADIUS`], motion up to about
/// `SEARCH_RADIUS · 2^MAX_LEVELS` work pixels (128 at 960 wide) between
/// two frames is followed.
pub const MAX_LEVELS: u32 = 6;

/// A level is only added while the one above it would be at least this
/// many pixels on its shorter side.
pub const MIN_LEVEL_SIDE: u32 = 8;

/// How far each level searches around the motion predicted for it, in
/// that level's pixels.
pub const SEARCH_RADIUS: i32 = 2;

/// The matched patch's half size: `2 · PATCH_RADIUS + 1` pixels square.
pub const PATCH_RADIUS: i32 = 2;

/// Added to a candidate's mean patch difference per pixel of extra
/// motion, so a flat patch, which matches anywhere equally, stays where
/// it was predicted rather than drifting to the search window's edge.
pub const MOTION_PENALTY: f32 = 0.002;

/// Forward-backward disagreement, in work pixels, at which a side's
/// sample counts for `1/e` of a trusted one.
pub const CONSISTENCY_SCALE: f32 = 1.0;

/// The difference in encoded luma between the two warped samples at
/// which the warp counts for `1/e`: two samples that don't look alike
/// followed different things.
pub const PHOTOMETRIC_SCALE: f32 = 0.1;

/// The share of the frame that can be unreliable before the whole frame
/// is cross-faded instead of warped. A pixel is unreliable where the two
/// fields disagree by more than [`UNRELIABLE_DISAGREEMENT`] or its best
/// match is worse than [`UNRELIABLE_COST`]. Past this share the picture
/// is a cut inside the media, or motion faster than the search follows,
/// and a warp would tear it, where a cross-fade only ghosts: two in five
/// lets a large occluding object through and still catches a cut, where
/// nearly every pixel fails.
pub const UNRELIABLE_FRACTION: f32 = 0.4;

/// Forward-backward disagreement, in work pixels, past which a pixel is
/// counted unreliable for [`UNRELIABLE_FRACTION`].
pub const UNRELIABLE_DISAGREEMENT: f32 = 2.0;

/// Mean patch difference, in encoded luma, past which a pixel's best
/// match is counted unreliable for [`UNRELIABLE_FRACTION`].
pub const UNRELIABLE_COST: f32 = 0.08;

/// The size the flow is estimated at for a frame of `width` × `height`:
/// the frame's own up to [`WORK_MAX_WIDTH`], else that wide at the
/// frame's aspect (in whole pixels, by integer arithmetic, so the GPU
/// and CPU agree).
pub fn work_size(width: u32, height: u32) -> (u32, u32) {
    if width <= WORK_MAX_WIDTH {
        return (width.max(1), height.max(1));
    }
    let h = (u64::from(height) * u64::from(WORK_MAX_WIDTH) + u64::from(width) / 2) / u64::from(width);
    (WORK_MAX_WIDTH, (h as u32).max(1))
}

/// The pyramid's level sizes, level 0 the work size.
pub fn level_sizes(work: (u32, u32)) -> Vec<(u32, u32)> {
    let mut sizes = vec![work];
    while (sizes.len() as u32) < MAX_LEVELS {
        let (w, h) = *sizes.last().expect("level 0 is there");
        let next = (w.div_ceil(2), h.div_ceil(2));
        if next.0.min(next.1) < MIN_LEVEL_SIDE {
            break;
        }
        sizes.push(next);
    }
    sizes
}

/// What [`interpolate`] made.
#[derive(Clone, Debug)]
pub struct Interpolated {
    /// The in-between frame: linear RGBA, the frames' size.
    pub pixels: Vec<f32>,
    /// The share of the work frame counted unreliable.
    pub unreliable: f32,
    /// Whether the whole frame was cross-faded ([`UNRELIABLE_FRACTION`]).
    pub cross_faded: bool,
}

/// A dense flow field at one size: per pixel the motion `(dx, dy)` in
/// that size's pixels, and the best match's mean patch difference.
#[derive(Clone, Debug)]
pub struct Field {
    pub width: u32,
    pub height: u32,
    /// `[dx, dy, cost]` per pixel, row by row.
    pub data: Vec<[f32; 3]>,
}

/// Both ways' flow between two frames at the work size, and the share of
/// it counted unreliable.
#[derive(Clone, Debug)]
pub struct Flow {
    pub forward: Field,
    pub backward: Field,
    pub unreliable: f32,
}

/// One plane of floats.
struct Plane {
    width: u32,
    height: u32,
    data: Vec<f32>,
}

impl Plane {
    /// The value at whole pixel `(x, y)`, clamped to the edge.
    fn at(&self, x: i32, y: i32) -> f32 {
        let x = x.clamp(0, self.width as i32 - 1) as usize;
        let y = y.clamp(0, self.height as i32 - 1) as usize;
        self.data[y * self.width as usize + x]
    }

    /// Bilinear sample at `(x, y)` (pixel centres on whole numbers),
    /// clamped to the edge.
    fn sample(&self, x: f32, y: f32) -> f32 {
        let x = x.clamp(0.0, (self.width - 1) as f32);
        let y = y.clamp(0.0, (self.height - 1) as f32);
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as i32, y0 as i32);
        let top = self.at(x0, y0) + (self.at(x0 + 1, y0) - self.at(x0, y0)) * fx;
        let bottom = self.at(x0, y0 + 1) + (self.at(x0 + 1, y0 + 1) - self.at(x0, y0 + 1)) * fx;
        top + (bottom - top) * fy
    }
}

/// Encoded luma: the square root of linear luma, so a difference means
/// about as much in the shadows as in the highlights.
pub fn encode_luma(r: f32, g: f32, b: f32, luma: [f32; 3]) -> f32 {
    (r * luma[0] + g * luma[1] + b * luma[2]).max(0.0).sqrt()
}

/// `frame`'s encoded luma at `work` size: each work pixel the mean over
/// the whole frame pixels it covers, `[x·W/w, max(x0+1, (x+1)·W/w))`.
fn work_luma(frame: &[f32], width: u32, height: u32, work: (u32, u32), luma: [f32; 3]) -> Plane {
    let (ww, wh) = work;
    let data = (0..wh)
        .into_par_iter()
        .flat_map_iter(|y| {
            let y0 = y * height / wh;
            let y1 = ((y + 1) * height / wh).max(y0 + 1);
            (0..ww).map(move |x| {
                let x0 = x * width / ww;
                let x1 = ((x + 1) * width / ww).max(x0 + 1);
                let mut sum = 0.0;
                for sy in y0..y1 {
                    for sx in x0..x1 {
                        let i = (sy * width + sx) as usize * 4;
                        sum += encode_luma(frame[i], frame[i + 1], frame[i + 2], luma);
                    }
                }
                sum / ((x1 - x0) * (y1 - y0)) as f32
            })
        })
        .collect();
    Plane { width: ww, height: wh, data }
}

/// The level below `p` in the pyramid: each pixel the mean of the 2 × 2
/// above it, clamped to the edge.
fn halve(p: &Plane, size: (u32, u32)) -> Plane {
    let data = (0..size.1)
        .flat_map(|y| (0..size.0).map(move |x| (x as i32 * 2, y as i32 * 2)))
        .map(|(x, y)| (p.at(x, y) + p.at(x + 1, y) + p.at(x, y + 1) + p.at(x + 1, y + 1)) * 0.25)
        .collect();
    Plane { width: size.0, height: size.1, data }
}

fn pyramid(level0: Plane, sizes: &[(u32, u32)]) -> Vec<Plane> {
    let mut levels = vec![level0];
    for &size in &sizes[1..] {
        let next = halve(levels.last().expect("level 0 is there"), size);
        levels.push(next);
    }
    levels
}

impl Field {
    fn zero(width: u32, height: u32) -> Self {
        Field { width, height, data: vec![[0.0; 3]; (width * height) as usize] }
    }

    fn at(&self, x: i32, y: i32) -> [f32; 3] {
        let x = x.clamp(0, self.width as i32 - 1) as usize;
        let y = y.clamp(0, self.height as i32 - 1) as usize;
        self.data[y * self.width as usize + x]
    }

    /// Bilinear sample of the motion at `(x, y)`, clamped to the edge.
    pub fn motion(&self, x: f32, y: f32) -> [f32; 2] {
        let x = x.clamp(0.0, (self.width - 1) as f32);
        let y = y.clamp(0.0, (self.height - 1) as f32);
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as i32, y0 as i32);
        let mut out = [0.0; 2];
        for (c, o) in out.iter_mut().enumerate() {
            let top = self.at(x0, y0)[c] + (self.at(x0 + 1, y0)[c] - self.at(x0, y0)[c]) * fx;
            let bottom = self.at(x0, y0 + 1)[c] + (self.at(x0 + 1, y0 + 1)[c] - self.at(x0, y0 + 1)[c]) * fx;
            *o = top + (bottom - top) * fy;
        }
        out
    }
}

/// Mean absolute difference between `from`'s patch at whole pixel `(x,
/// y)` and `to`'s at `(x + dx, y + dy)`.
fn patch_cost(from: &Plane, to: &Plane, x: i32, y: i32, dx: f32, dy: f32) -> f32 {
    let mut sum = 0.0;
    for oy in -PATCH_RADIUS..=PATCH_RADIUS {
        for ox in -PATCH_RADIUS..=PATCH_RADIUS {
            sum += (from.at(x + ox, y + oy) - to.sample((x + ox) as f32 + dx, (y + oy) as f32 + dy)).abs();
        }
    }
    let n = (2 * PATCH_RADIUS + 1) * (2 * PATCH_RADIUS + 1);
    sum / n as f32
}

/// The sub-pixel offset of a parabola's minimum through three costs a
/// pixel apart, the middle one the least; within half a pixel.
fn parabola(minus: f32, centre: f32, plus: f32) -> f32 {
    let denom = minus - 2.0 * centre + plus;
    if denom > 1e-6 { ((minus - plus) / (2.0 * denom)).clamp(-0.5, 0.5) } else { 0.0 }
}

/// One level's search: each pixel of `from` looks for itself in `to`
/// within [`SEARCH_RADIUS`] of the motion `coarser` predicts (twice its
/// motion at the matching place; none at the coarsest level), the first
/// least cost in row order winning, refined to sub-pixel.
fn search(from: &Plane, to: &Plane, coarser: Option<&Field>) -> Field {
    let (w, h) = (from.width, from.height);
    let data = (0..h)
        .into_par_iter()
        .flat_map_iter(|y| {
            (0..w).map(move |x| {
                let (x, y) = (x as i32, y as i32);
                let pred = coarser.map_or([0.0, 0.0], |c| {
                    let m = c.motion((x as f32 + 0.5) * 0.5 - 0.5, (y as f32 + 0.5) * 0.5 - 0.5);
                    [m[0] * 2.0, m[1] * 2.0]
                });
                let cost = |dx: i32, dy: i32| patch_cost(from, to, x, y, pred[0] + dx as f32, pred[1] + dy as f32) + MOTION_PENALTY * (dx.abs() + dy.abs()) as f32;
                let mut best = (0, 0, f32::INFINITY);
                for dy in -SEARCH_RADIUS..=SEARCH_RADIUS {
                    for dx in -SEARCH_RADIUS..=SEARCH_RADIUS {
                        let c = cost(dx, dy);
                        if c < best.2 {
                            best = (dx, dy, c);
                        }
                    }
                }
                let (bx, by, bc) = best;
                let sx = parabola(cost(bx - 1, by), bc, cost(bx + 1, by));
                let sy = parabola(cost(bx, by - 1), bc, cost(bx, by + 1));
                [pred[0] + bx as f32 + sx, pred[1] + by as f32 + sy, bc]
            })
        })
        .collect();
    Field { width: w, height: h, data }
}

/// `raw` smoothed: each pixel's motion the mean of its 3 × 3, weighted
/// by match quality (`1 / (cost + 0.01)`); its cost kept.
fn smooth(raw: &Field) -> Field {
    let (w, h) = (raw.width, raw.height);
    let data = (0..h as i32)
        .flat_map(|y| (0..w as i32).map(move |x| (x, y)))
        .map(|(x, y)| {
            let (mut sx, mut sy, mut sw) = (0.0, 0.0, 0.0);
            for oy in -1..=1 {
                for ox in -1..=1 {
                    let v = raw.at(x + ox, y + oy);
                    let weight = 1.0 / (v[2] + 0.01);
                    sx += v[0] * weight;
                    sy += v[1] * weight;
                    sw += weight;
                }
            }
            [sx / sw, sy / sw, raw.at(x, y)[2]]
        })
        .collect();
    Field { width: w, height: h, data }
}

/// How far `forward` and `backward` disagree at `(x, y)`: the motion
/// there, followed and sent back, misses its start by this much.
fn disagreement(forward: &Field, backward: &Field, x: f32, y: f32) -> f32 {
    let f = forward.motion(x, y);
    let b = backward.motion(x + f[0], y + f[1]);
    ((f[0] + b[0]).powi(2) + (f[1] + b[1]).powi(2)).sqrt()
}

/// The share of the work frame counted unreliable: the two fields
/// disagree past [`UNRELIABLE_DISAGREEMENT`] where the motion stays in
/// the frame (a pixel leaving it has nothing to agree with), or the
/// best match is worse than [`UNRELIABLE_COST`].
fn unreliable_share(forward: &Field, backward: &Field) -> f32 {
    let (w, h) = (forward.width, forward.height);
    let count: u32 = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let v = forward.at(x as i32, y as i32);
            let (tx, ty) = (x as f32 + v[0], y as f32 + v[1]);
            let inside = tx >= 0.0 && ty >= 0.0 && tx <= (w - 1) as f32 && ty <= (h - 1) as f32;
            (inside && disagreement(forward, backward, x as f32, y as f32) > UNRELIABLE_DISAGREEMENT) || v[2] > UNRELIABLE_COST
        })
        .count() as u32;
    count as f32 / (w * h) as f32
}

/// The flow both ways between frames `a` and `b` (linear RGBA, `width` ×
/// `height`), at the work size. `luma` is the working space's luma
/// weights.
pub fn estimate(a: &[f32], b: &[f32], width: u32, height: u32, luma: [f32; 3]) -> Flow {
    let sizes = level_sizes(work_size(width, height));
    let work = sizes[0];
    let pa = pyramid(work_luma(a, width, height, work, luma), &sizes);
    let pb = pyramid(work_luma(b, width, height, work, luma), &sizes);
    let (mut forward, mut backward): (Option<Field>, Option<Field>) = (None, None);
    for level in (0..sizes.len()).rev() {
        forward = Some(smooth(&search(&pa[level], &pb[level], forward.as_ref())));
        backward = Some(smooth(&search(&pb[level], &pa[level], backward.as_ref())));
    }
    let forward = forward.unwrap_or_else(|| Field::zero(work.0, work.1));
    let backward = backward.unwrap_or_else(|| Field::zero(work.0, work.1));
    let unreliable = unreliable_share(&forward, &backward);
    Flow { forward, backward, unreliable }
}

/// Bilinear RGBA sample of a `width` × `height` frame at `(x, y)`,
/// clamped to the edge.
fn sample_rgba(frame: &[f32], width: u32, height: u32, x: f32, y: f32) -> [f32; 4] {
    let x = x.clamp(0.0, (width - 1) as f32);
    let y = y.clamp(0.0, (height - 1) as f32);
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let (x0, y0) = (x0 as u32, y0 as u32);
    let (x1, y1) = ((x0 + 1).min(width - 1), (y0 + 1).min(height - 1));
    let px = |x: u32, y: u32, c: usize| frame[(y * width + x) as usize * 4 + c];
    let mut out = [0.0; 4];
    for (c, o) in out.iter_mut().enumerate() {
        let top = px(x0, y0, c) + (px(x1, y0, c) - px(x0, y0, c)) * fx;
        let bottom = px(x0, y1, c) + (px(x1, y1, c) - px(x0, y1, c)) * fx;
        *o = top + (bottom - top) * fy;
    }
    out
}

/// One output pixel of the in-between frame at `t`: see the module's
/// doc. `(x, y)` is a whole pixel of the `width` × `height` frame.
#[allow(clippy::too_many_arguments)]
fn synthesise_pixel(a: &[f32], b: &[f32], width: u32, height: u32, flow: &Flow, t: f32, luma: [f32; 3], x: u32, y: u32) -> [f32; 4] {
    let i = (y * width + x) as usize * 4;
    let mut blend = [0.0; 4];
    for c in 0..4 {
        blend[c] = a[i + c] + (b[i + c] - a[i + c]) * t;
    }
    let (ww, wh) = (flow.forward.width, flow.forward.height);
    // Work pixels per output pixel, each way.
    let (kx, ky) = (ww as f32 / width as f32, wh as f32 / height as f32);
    let to_work = |px: f32, py: f32| ((px + 0.5) * kx - 0.5, (py + 0.5) * ky - 0.5);
    let (qx, qy) = to_work(x as f32, y as f32);
    let f01 = flow.forward.motion(qx, qy);
    let f10 = flow.backward.motion(qx, qy);
    let s = 1.0 - t;
    // Where this pixel was at A and will be at B, in output pixels.
    let ta = [(-s * t * f01[0] + t * t * f10[0]) / kx, (-s * t * f01[1] + t * t * f10[1]) / ky];
    let tb = [(s * s * f01[0] - t * s * f10[0]) / kx, (s * s * f01[1] - t * s * f10[1]) / ky];
    let (ax, ay) = (x as f32 + ta[0], y as f32 + ta[1]);
    let (bx, by) = (x as f32 + tb[0], y as f32 + tb[1]);
    let inside = |px: f32, py: f32| px >= -0.5 && py >= -0.5 && px <= width as f32 - 0.5 && py <= height as f32 - 0.5;
    let trust = |forward: &Field, backward: &Field, px: f32, py: f32| {
        if !inside(px, py) {
            return 0.0;
        }
        let (wx, wy) = to_work(px, py);
        let e = disagreement(forward, backward, wx, wy) / CONSISTENCY_SCALE;
        (-e * e).exp()
    };
    let ra = trust(&flow.forward, &flow.backward, ax, ay);
    let rb = trust(&flow.backward, &flow.forward, bx, by);
    let ca = sample_rgba(a, width, height, ax, ay);
    let cb = sample_rgba(b, width, height, bx, by);
    let (wa, wb) = (s * ra, t * rb);
    if wa + wb <= 1e-3 {
        return blend;
    }
    let d = (encode_luma(ca[0], ca[1], ca[2], luma) - encode_luma(cb[0], cb[1], cb[2], luma)).abs() / PHOTOMETRIC_SCALE;
    let alike = (-d * d).exp();
    // Both sides trusted: they should look alike. One side alone (the
    // other covered) has nothing to agree with.
    let confidence = ra.max(rb) * (1.0 + (alike - 1.0) * ra.min(rb));
    let mut out = [0.0; 4];
    for c in 0..4 {
        let warped = (wa * ca[c] + wb * cb[c]) / (wa + wb);
        out[c] = blend[c] + (warped - blend[c]) * confidence;
    }
    out
}

/// The in-between frame at `t` from `flow` ([`estimate`]): the plain
/// cross-fade when the flow's unreliable share passes
/// [`UNRELIABLE_FRACTION`].
pub fn synthesise(a: &[f32], b: &[f32], width: u32, height: u32, flow: &Flow, t: f32, luma: [f32; 3]) -> Interpolated {
    let t = t.clamp(0.0, 1.0);
    let cross_faded = flow.unreliable > UNRELIABLE_FRACTION;
    let pixels = if cross_faded {
        a.iter().zip(b).map(|(a, b)| a + (b - a) * t).collect()
    } else {
        (0..height)
            .into_par_iter()
            .flat_map_iter(|y| (0..width).flat_map(move |x| synthesise_pixel(a, b, width, height, flow, t, luma, x, y)))
            .collect()
    };
    Interpolated { pixels, unreliable: flow.unreliable, cross_faded }
}

/// The in-between frame at `t` between `a` and `b`, by optical flow
/// ([`estimate`] then [`synthesise`]).
pub fn interpolate(a: &[f32], b: &[f32], width: u32, height: u32, t: f32, luma: [f32; 3]) -> Interpolated {
    let flow = estimate(a, b, width, height, luma);
    synthesise(a, b, width, height, &flow, t, luma)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

    /// A textured square on a textured background, its top-left corner
    /// at `(left, top)`: grey ramps the search can lock on to.
    pub(crate) fn square_frame(width: u32, height: u32, left: f32, top: f32, side: f32) -> Vec<f32> {
        let mut out = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                let (fx, fy) = (x as f32, y as f32);
                let inside = fx >= left && fx < left + side && fy >= top && fy < top + side;
                let v = if inside {
                    // A checker inside the square, moving with it.
                    let (u, w) = ((fx - left) as i32 / 4, (fy - top) as i32 / 4);
                    if (u + w) % 2 == 0 { 0.9 } else { 0.6 }
                } else {
                    0.05 + 0.1 * (((x / 7) + (y / 5)) % 3) as f32
                };
                out.extend_from_slice(&[v, v, v, 1.0]);
            }
        }
        out
    }

    /// The square's centre in a frame: the mean place of its bright
    /// pixels.
    fn bright_centre(frame: &[f32], width: u32) -> (f32, f32) {
        let (mut sx, mut sy, mut n) = (0.0, 0.0, 0.0);
        for (i, px) in frame.chunks_exact(4).enumerate() {
            if px[0] > 0.5 {
                sx += (i as u32 % width) as f32;
                sy += (i as u32 / width) as f32;
                n += 1.0;
            }
        }
        (sx / n, sy / n)
    }

    #[test]
    fn work_size_keeps_aspect_and_caps_width() {
        assert_eq!(work_size(640, 360), (640, 360));
        assert_eq!(work_size(3840, 2160), (960, 540));
        assert_eq!(work_size(1920, 1081), (960, 541));
        assert_eq!(level_sizes((960, 540)).len(), MAX_LEVELS as usize);
        assert_eq!(level_sizes((12, 12)), vec![(12, 12)]);
        assert_eq!(level_sizes((128, 96)).len(), 4);
    }

    #[test]
    fn a_moving_square_lands_halfway() {
        let (w, h) = (128, 96);
        let a = square_frame(w, h, 20.0, 30.0, 24.0);
        let b = square_frame(w, h, 36.0, 34.0, 24.0);
        let mid = interpolate(&a, &b, w, h, 0.5, LUMA);
        assert!(!mid.cross_faded, "unreliable share {}", mid.unreliable);
        let (cx, cy) = bright_centre(&mid.pixels, w);
        // Halfway: left 28, top 32, so centre (39.5, 43.5).
        assert!((cx - 39.5).abs() < 1.0 && (cy - 43.5).abs() < 1.0, "centre ({cx}, {cy})");
        // A cross-fade would leave two half-bright squares: here the
        // square is whole, its pixels as bright as the frames'.
        let solid = mid.pixels.chunks_exact(4).filter(|p| p[0] > 0.55).count();
        let original = a.chunks_exact(4).filter(|p| p[0] > 0.55).count();
        assert!((solid as f32 - original as f32).abs() < original as f32 * 0.15, "{solid} bright pixels, the frames have {original}");
    }

    #[test]
    fn its_ends_are_the_frames() {
        let (w, h) = (64, 48);
        let a = square_frame(w, h, 10.0, 10.0, 16.0);
        let b = square_frame(w, h, 14.0, 12.0, 16.0);
        let flow = estimate(&a, &b, w, h, LUMA);
        let at0 = synthesise(&a, &b, w, h, &flow, 0.0, LUMA);
        let at1 = synthesise(&a, &b, w, h, &flow, 1.0, LUMA);
        let worst = |x: &[f32], y: &[f32]| x.iter().zip(y).map(|(p, q)| (p - q).abs()).fold(0.0f32, f32::max);
        assert!(worst(&at0.pixels, &a) < 1e-4, "t = 0 differs from A by {}", worst(&at0.pixels, &a));
        assert!(worst(&at1.pixels, &b) < 1e-4, "t = 1 differs from B by {}", worst(&at1.pixels, &b));
    }

    #[test]
    fn unrelated_frames_cross_fade() {
        let (w, h) = (64, 48);
        let a = square_frame(w, h, 10.0, 10.0, 16.0);
        // Another picture altogether: noise-like stripes.
        let b: Vec<f32> = (0..w * h).flat_map(|i| {
            let v = ((i * 7919) % 13) as f32 / 13.0;
            [v, v, v, 1.0]
        }).collect();
        let mid = interpolate(&a, &b, w, h, 0.25, LUMA);
        assert!(mid.cross_faded, "unreliable share {}", mid.unreliable);
        for ((o, a), b) in mid.pixels.iter().zip(&a).zip(&b) {
            assert!((o - (a + (b - a) * 0.25)).abs() < 1e-6);
        }
    }

    #[test]
    fn a_still_frame_stays_put() {
        let (w, h) = (64, 48);
        let a = square_frame(w, h, 10.0, 10.0, 16.0);
        let mid = interpolate(&a, &a, w, h, 0.5, LUMA);
        let worst = mid.pixels.iter().zip(&a).map(|(p, q)| (p - q).abs()).fold(0.0f32, f32::max);
        assert!(worst < 1e-5, "{worst}");
    }

    #[test]
    fn a_wide_frame_is_estimated_small_and_warped_whole() {
        // Wider than the work size: the flow at 960, the warp at 1280.
        let (w, h) = (1280, 96);
        let a = square_frame(w, h, 200.0, 30.0, 40.0);
        let b = square_frame(w, h, 224.0, 34.0, 40.0);
        let flow = estimate(&a, &b, w, h, LUMA);
        assert_eq!((flow.forward.width, flow.forward.height), (960, 72));
        let mid = synthesise(&a, &b, w, h, &flow, 0.5, LUMA);
        assert_eq!(mid.pixels.len(), (w * h * 4) as usize);
        let (cx, cy) = bright_centre(&mid.pixels, w);
        assert!((cx - 231.5).abs() < 1.5 && (cy - 51.5).abs() < 1.5, "centre ({cx}, {cy})");
    }
}
