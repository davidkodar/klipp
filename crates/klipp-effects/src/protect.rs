//! Contrast protection for titles (#2734): the picture behind a title's
//! text is darkened (light text) or lightened (dark text) in a soft halo
//! round it, as far as the text needs to stand out from it.
//!
//! The picture and the title are measured small ([`SCALE`] to a side):
//! the picture's luminance and its spread, which is how busy it is, and
//! the title's coverage and luminance. Both are blurred over a few
//! percent of the frame, so the halo follows the words, not each glyph.
//! Where the text's contrast with what is behind it, picture brightness
//! plus its busyness, is under [`TARGET`], the picture is pulled towards
//! the side that gives it that contrast, and no further.
//!
//! This is the CPU twin of the GPU pass
//! (`GpuEffectsPipeline::stack_contrast_protect`): the same grid, blur,
//! sample and formula, so the two compose to the same pixels.

use rayon::prelude::*;

/// The measuring grid's cell, in frame pixels to a side.
pub const SCALE: u32 = 4;

/// The contrast ratio protection holds the text to: 4.5:1, what text on
/// a screen needs to be read at a glance.
pub const TARGET: f32 = 4.5;

/// The halo's blur radius as a share of the frame's short side.
pub const RADIUS_SHARE: f32 = 0.03;

/// How many times the grid is blurred across and down: twice softens the
/// halo's edge into a falloff.
pub const BLUR_ROUNDS: usize = 2;

/// The blurred coverage at which the halo is whole; it eases in below.
pub const HALO_FULL: f32 = 0.08;

/// The halo's strength at blurred coverage `a`.
pub fn halo(a: f32) -> f32 {
    let t = (a / HALO_FULL).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Text luminance at and above which it is light text: where its
/// contrast with black equals its contrast with white.
pub const LIGHT_TEXT: f32 = 0.179;

/// The blur radius in grid cells for a `width` × `height` frame.
pub fn radius_cells(width: u32, height: u32) -> u32 {
    (width.min(height) as f32 * RADIUS_SHARE / SCALE as f32).round().max(1.0) as u32
}

fn luma(p: &[f32], k: [f32; 3]) -> f32 {
    (p[0] * k[0] + p[1] * k[1] + p[2] * k[2]).clamp(0.0, 1.0)
}

/// How far the picture behind text is darkened (`Some(k)`, multiply by
/// `1 - halo (1 - k)`) or lightened (`Err(t)`, mix towards white by
/// `halo t`), from the blurred measures: picture luminance `lb`, its
/// standard deviation `busy`, text luminance `lt`.
pub fn pull(lb: f32, busy: f32, lt: f32, mode: u32) -> Result<f32, f32> {
    if lt >= LIGHT_TEXT {
        let need = (lt + 0.05) / TARGET - 0.05;
        let cur = (lb + busy).min(1.0);
        let mut k = if cur <= need { 1.0 } else { (need.max(0.0) / cur).clamp(0.12, 1.0) };
        if mode == 2 {
            k = k.min(0.55);
        }
        Ok(k)
    } else {
        let need = TARGET * (lt + 0.05) - 0.05;
        let cur = (lb - busy).max(0.0);
        let mut t = ((need - cur) / (1.0 - cur).max(1e-4)).clamp(0.0, 0.88);
        if mode == 2 {
            t = t.max(0.45);
        }
        Err(t)
    }
}

/// Protects the text of `layer` over `acc`, both straight-alpha linear
/// RGBA `width` × `height`: changes `acc` round the text by `mode` (1
/// only where the text needs it, 2 a halo always, stronger where the
/// text needs it; 0 leaves it). `opacity` is the layer's, as it will be
/// blended; `luma` the working space's luminance weights.
pub fn protect_backdrop(acc: &mut [f32], layer: &[f32], width: u32, height: u32, opacity: f32, mode: u32, luma_k: [f32; 3]) {
    let (w, h) = (width as usize, height as usize);
    if mode == 0 || opacity <= 0.001 || acc.len() != w * h * 4 || layer.len() != w * h * 4 {
        return;
    }
    let s = SCALE as usize;
    let (sw, sh) = (w.div_ceil(s), h.div_ceil(s));
    // (picture luminance, its square, text coverage, text luminance × coverage).
    let mut small = vec![[0.0f32; 4]; sw * sh];
    small.par_chunks_mut(sw).enumerate().for_each(|(sy, row)| {
        for (sx, cell) in row.iter_mut().enumerate() {
            let mut sum = [0.0f32; 4];
            let mut n = 0.0;
            for y in sy * s..((sy + 1) * s).min(h) {
                for x in sx * s..((sx + 1) * s).min(w) {
                    let i = (y * w + x) * 4;
                    let lb = luma(&acc[i..i + 3], luma_k) * acc[i + 3].clamp(0.0, 1.0);
                    let a = (layer[i + 3] * opacity).clamp(0.0, 1.0);
                    sum[0] += lb;
                    sum[1] += lb * lb;
                    sum[2] += a;
                    sum[3] += luma(&layer[i..i + 3], luma_k) * a;
                    n += 1.0;
                }
            }
            *cell = sum.map(|v| v / f32::max(n, 1.0));
        }
    });
    let r = radius_cells(width, height) as isize;
    let norm = 1.0 / (2 * r + 1) as f32;
    let mut across = vec![[0.0f32; 4]; sw * sh];
    for _ in 0..BLUR_ROUNDS {
    across.par_chunks_mut(sw).enumerate().for_each(|(sy, row)| {
        for (sx, out) in row.iter_mut().enumerate() {
            let mut sum = [0.0f32; 4];
            for d in -r..=r {
                let cx = (sx as isize + d).clamp(0, sw as isize - 1) as usize;
                let c = small[sy * sw + cx];
                for k in 0..4 {
                    sum[k] += c[k];
                }
            }
            *out = sum.map(|v| v * norm);
        }
    });
    small.par_chunks_mut(sw).enumerate().for_each(|(sy, row)| {
        for (sx, out) in row.iter_mut().enumerate() {
            let mut sum = [0.0f32; 4];
            for d in -r..=r {
                let cy = (sy as isize + d).clamp(0, sh as isize - 1) as usize;
                let c = across[cy * sw + sx];
                for k in 0..4 {
                    sum[k] += c[k];
                }
            }
            *out = sum.map(|v| v * norm);
        }
    });
    }
    let at = |x: isize, y: isize| small[y.clamp(0, sh as isize - 1) as usize * sw + x.clamp(0, sw as isize - 1) as usize];
    acc.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        let v = (y as f32 + 0.5) / s as f32 - 0.5;
        let (y0, fy) = (v.floor() as isize, v - v.floor());
        for x in 0..w {
            let u = (x as f32 + 0.5) / s as f32 - 0.5;
            let (x0, fx) = (u.floor() as isize, u - u.floor());
            let mut m = [0.0f32; 4];
            let (c00, c10, c01, c11) = (at(x0, y0), at(x0 + 1, y0), at(x0, y0 + 1), at(x0 + 1, y0 + 1));
            for k in 0..4 {
                let top = c00[k] + (c10[k] - c00[k]) * fx;
                let bottom = c01[k] + (c11[k] - c01[k]) * fx;
                m[k] = top + (bottom - top) * fy;
            }
            let a = m[2];
            if a <= 1e-3 {
                continue;
            }
            let halo = halo(a);
            let busy = (m[1] - m[0] * m[0]).max(0.0).sqrt();
            let p = &mut row[x * 4..x * 4 + 3];
            match pull(m[0], busy, (m[3] / a).clamp(0.0, 1.0), mode) {
                Ok(k) => {
                    let f = 1.0 - halo * (1.0 - k);
                    p.iter_mut().for_each(|c| *c *= f);
                }
                Err(t) => p.iter_mut().for_each(|c| *c += (1.0 - *c) * halo * t),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG contrast between two luminances.
    fn contrast(a: f32, b: f32) -> f32 {
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    fn frame(w: u32, h: u32, c: [f32; 4]) -> Vec<f32> {
        (0..w * h).flat_map(|_| c).collect()
    }

    const K: [f32; 3] = [0.2126, 0.7152, 0.0722];

    #[test]
    fn white_text_over_white_gets_its_contrast() {
        let (w, h) = (256, 144);
        let mut acc = frame(w, h, [0.9, 0.9, 0.9, 1.0]);
        let mut layer = frame(w, h, [0.0; 4]);
        for y in 60..84 {
            for x in 80..176 {
                let i = ((y * w + x) * 4) as usize;
                layer[i..i + 4].copy_from_slice(&[1.0, 1.0, 1.0, 1.0]);
            }
        }
        protect_backdrop(&mut acc, &layer, w, h, 1.0, 1, K);
        let i = ((72 * w + 128) * 4) as usize;
        assert!(contrast(1.0, luma(&acc[i..i + 3], K)) >= 4.4, "behind the text: {:?}", &acc[i..i + 3]);
        // Far from the text the picture is left as it was.
        assert_eq!(&acc[..3], &[0.9, 0.9, 0.9]);
    }

    #[test]
    fn auto_leaves_a_picture_that_already_reads() {
        let (w, h) = (128, 72);
        let mut acc = frame(w, h, [0.02, 0.02, 0.02, 1.0]);
        let before = acc.clone();
        let layer = frame(w, h, [1.0, 1.0, 1.0, 1.0]);
        protect_backdrop(&mut acc, &layer, w, h, 1.0, 1, K);
        assert_eq!(acc, before);
        protect_backdrop(&mut acc, &layer, w, h, 1.0, 2, K);
        assert!(acc[0] < before[0], "always still lays its halo");
    }

    #[test]
    fn dark_text_lightens_a_dark_picture() {
        let (w, h) = (128, 72);
        let mut acc = frame(w, h, [0.05, 0.05, 0.05, 1.0]);
        let layer = frame(w, h, [0.0, 0.0, 0.0, 1.0]);
        protect_backdrop(&mut acc, &layer, w, h, 1.0, 1, K);
        assert!(contrast(0.0, luma(&acc[..3], K)) >= 4.4, "{:?}", &acc[..3]);
    }
}
