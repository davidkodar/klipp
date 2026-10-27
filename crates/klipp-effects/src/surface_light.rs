//! A layer pinned to a surface taking that surface's light across it and
//! its texture (#2751): paint on a wall darkens where the wall is in
//! shade, and the wall's grain shows through it.
//!
//! The picture under the layer is measured small ([`SCALE`] to a side)
//! and its luminance blurred over [`LOCAL_SHARE`] of the frame: the light
//! at each place. The mean of that light under the layer, weighted by
//! how much of each cell the layer covers, is the surface's light. Each
//! place's light against it is the shade there; the picture's own
//! luminance against its local light is its texture. The layer is
//! multiplied by the shade to the power `shade` and by the texture to the
//! power `texture`, each 0 (none) to 1 (all of it), before it is blended.
//! The overall light from frame to frame is the pin's `light` (part C1),
//! not this.
//!
//! This is the CPU twin of the GPU pass
//! (`GpuEffectsPipeline::stack_take_surface`): the same grid, blur,
//! mean, sample and formula.

use rayon::prelude::*;

/// The measuring grid's cell, in frame pixels to a side.
pub const SCALE: u32 = 4;
/// The local light's blur radius as a share of the frame's short side.
pub const LOCAL_SHARE: f32 = 0.012;
/// The blur's rounds across and down.
pub const BLUR_ROUNDS: usize = 2;
/// A ratio is never past this either way, so a black hole or a glint
/// under the layer doesn't blow it out or black it in.
pub const MAX_RATIO: f32 = 4.0;
/// The darkest and brightest the two together make the layer.
pub const FACTOR_RANGE: (f32, f32) = (0.1, 4.0);
const EPS: f32 = 1e-3;

/// The blur radius in grid cells for a `width` × `height` frame.
pub fn radius_cells(width: u32, height: u32) -> u32 {
    (width.min(height) as f32 / SCALE as f32 * LOCAL_SHARE).round().max(1.0) as u32
}

/// What the layer is multiplied by where the picture's luminance is `l`,
/// its local light `local` and the surface's light `mean`.
pub fn factor(l: f32, local: f32, mean: f32, shade: f32, texture: f32) -> f32 {
    let ratio = |a: f32, b: f32| ((a + EPS) / (b + EPS)).clamp(1.0 / MAX_RATIO, MAX_RATIO);
    (ratio(local, mean).powf(shade) * ratio(l, local).powf(texture)).clamp(FACTOR_RANGE.0, FACTOR_RANGE.1)
}

fn luma(p: &[f32], k: [f32; 3]) -> f32 {
    (p[0] * k[0] + p[1] * k[1] + p[2] * k[2]).max(0.0)
}

/// `layer` given the light and texture of `acc` under it, both straight
/// alpha linear RGBA `width` × `height`: its colour multiplied, its alpha
/// kept. `shade` and `texture` are 0 to 1; `luma_k` the working space's
/// luminance weights.
pub fn take_surface(layer: &mut [f32], acc: &[f32], width: u32, height: u32, shade: f32, texture: f32, luma_k: [f32; 3]) {
    let (w, h) = (width as usize, height as usize);
    if (shade <= 0.0 && texture <= 0.0) || acc.len() != w * h * 4 || layer.len() != w * h * 4 {
        return;
    }
    let s = SCALE as usize;
    let (sw, sh) = (w.div_ceil(s), h.div_ceil(s));
    // Each cell's picture luminance, and how much of it the layer covers.
    let mut small = vec![[0.0f32; 2]; sw * sh];
    small.par_chunks_mut(sw).enumerate().for_each(|(sy, row)| {
        for (sx, cell) in row.iter_mut().enumerate() {
            let (mut sum, mut cover, mut n) = (0.0f32, 0.0f32, 0.0f32);
            for y in sy * s..((sy + 1) * s).min(h) {
                for x in sx * s..((sx + 1) * s).min(w) {
                    let i = (y * w + x) * 4;
                    sum += luma(&acc[i..i + 3], luma_k) * acc[i + 3].clamp(0.0, 1.0);
                    cover += layer[i + 3].clamp(0.0, 1.0);
                    n += 1.0;
                }
            }
            *cell = [sum / n.max(1.0), cover / n.max(1.0)];
        }
    });
    // The luminance blurred into the local light; the coverage kept.
    let r = radius_cells(width, height) as isize;
    let norm = 1.0 / (2 * r + 1) as f32;
    let mut across = vec![[0.0f32; 2]; sw * sh];
    for _ in 0..BLUR_ROUNDS {
        across.par_chunks_mut(sw).enumerate().for_each(|(sy, row)| {
            for (sx, out) in row.iter_mut().enumerate() {
                let sum: f32 = (-r..=r).map(|d| small[sy * sw + (sx as isize + d).clamp(0, sw as isize - 1) as usize][0]).sum();
                *out = [sum * norm, small[sy * sw + sx][1]];
            }
        });
        small.par_chunks_mut(sw).enumerate().for_each(|(sy, row)| {
            for (sx, out) in row.iter_mut().enumerate() {
                let sum: f32 = (-r..=r).map(|d| across[(sy as isize + d).clamp(0, sh as isize - 1) as usize * sw + sx][0]).sum();
                *out = [sum * norm, across[sy * sw + sx][1]];
            }
        });
    }
    // The surface's light: the local light under the layer, row by row
    // and then the rows, as the GPU sums it.
    let rows: Vec<[f32; 2]> = small.chunks(sw).map(|row| row.iter().fold([0.0, 0.0], |a, c| [a[0] + c[0] * c[1], a[1] + c[1]])).collect();
    let total = rows.iter().fold([0.0f32, 0.0], |a, r| [a[0] + r[0], a[1] + r[1]]);
    if total[1] <= EPS {
        return;
    }
    let mean = total[0] / total[1];
    let at = |x: isize, y: isize| small[y.clamp(0, sh as isize - 1) as usize * sw + x.clamp(0, sw as isize - 1) as usize][0];
    layer.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        let v = (y as f32 + 0.5) / s as f32 - 0.5;
        let (y0, fy) = (v.floor() as isize, v - v.floor());
        for x in 0..w {
            if row[x * 4 + 3] <= 0.0 {
                continue;
            }
            let u = (x as f32 + 0.5) / s as f32 - 0.5;
            let (x0, fx) = (u.floor() as isize, u - u.floor());
            let top = at(x0, y0) + (at(x0 + 1, y0) - at(x0, y0)) * fx;
            let bottom = at(x0, y0 + 1) + (at(x0 + 1, y0 + 1) - at(x0, y0 + 1)) * fx;
            let local = top + (bottom - top) * fy;
            let i = (y * w + x) * 4;
            let l = luma(&acc[i..i + 3], luma_k) * acc[i + 3].clamp(0.0, 1.0);
            let f = factor(l, local, mean, shade, texture);
            row[x * 4..x * 4 + 3].iter_mut().for_each(|c| *c *= f);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const K: [f32; 3] = [0.2126, 0.7152, 0.0722];

    /// A wall lit on its left, in shade on its right, with a fine grain;
    /// a flat grey layer over it.
    fn wall(w: u32, h: u32) -> Vec<f32> {
        (0..w * h)
            .flat_map(|i| {
                let (x, y) = (i % w, i / w);
                let lit = if x < w / 2 { 0.6 } else { 0.15 };
                let grain = if (x / 2 + y / 2) % 2 == 0 { 1.15 } else { 0.85 };
                let v = lit * grain;
                [v, v, v, 1.0]
            })
            .collect()
    }

    fn mean(layer: &[f32], w: u32, xs: std::ops::Range<u32>, ys: std::ops::Range<u32>) -> f32 {
        let mut sum = 0.0;
        let mut n = 0.0;
        for y in ys.clone() {
            for x in xs.clone() {
                sum += layer[((y * w + x) * 4) as usize];
                n += 1.0;
            }
        }
        sum / n
    }

    /// #2751: the layer darkens where the wall is in shade and brightens
    /// where it is lit, against the light round it; with texture its
    /// pixels follow the wall's grain; with neither it is untouched.
    #[test]
    fn a_pinned_layer_takes_the_walls_shade_and_grain() {
        let (w, h) = (480, 270);
        let acc = wall(w, h);
        let grey = |shade, texture| {
            let mut layer: Vec<f32> = (0..w * h).flat_map(|_| [0.5, 0.5, 0.5, 1.0]).collect();
            take_surface(&mut layer, &acc, w, h, shade, texture, K);
            layer
        };
        let none = grey(0.0, 0.0);
        assert!(none.iter().all(|&v| v == 0.5 || v == 1.0), "nothing asked, nothing changed");
        let shaded = grey(1.0, 0.0);
        let (lit, dark) = (mean(&shaded, w, 40..140, 100..170), mean(&shaded, w, 340..440, 100..170));
        assert!(lit > 0.55 && dark < 0.45 && lit / dark > 2.0, "lit {lit}, in shade {dark}");
        // Texture: neighbouring pixels of the grain differ, as the wall's.
        let textured = grey(0.0, 1.0);
        let at = |x: u32, y: u32| textured[((y * w + x) * 4) as usize];
        let (a, b) = (at(60, 100), at(62, 100));
        assert!((a / b - 1.15 / 0.85).abs() < 0.2 || (b / a - 1.15 / 0.85).abs() < 0.2, "grain through: {a} {b}");
        // Never past the bounds, however black or bright under it.
        let mut layer: Vec<f32> = (0..w * h).flat_map(|_| [0.5, 0.5, 0.5, 1.0]).collect();
        let black: Vec<f32> = (0..w * h).flat_map(|i| if i % w < 10 { [0.0, 0.0, 0.0, 1.0] } else { [1.0, 1.0, 1.0, 1.0] }).collect();
        take_surface(&mut layer, &black, w, h, 1.0, 1.0, K);
        assert!(layer.chunks(4).all(|p| p[0] >= 0.5 * FACTOR_RANGE.0 - 1e-6 && p[0] <= 0.5 * FACTOR_RANGE.1 + 1e-6));
    }
}
