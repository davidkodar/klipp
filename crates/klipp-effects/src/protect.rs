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
//! The pull is one for the whole title: the one its hardest part needs
//! ([`title_pull`]), laid evenly through the halo. Worked out place by
//! place, a title over a picture half bright, half dark got a shade
//! behind one half only, an off-centre patch that read as a mistake.
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
pub const HALO_FULL: f32 = 0.04;

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

/// The pull for a whole title, from its blurred measuring grid (picture
/// luminance, its square, text coverage, text luminance × coverage per
/// cell): what its hardest part needs, light text's least `k` and dark
/// text's greatest `t`, over the cells the halo covers whole, or over any
/// the text reaches when none is (small or thin text). `(k, t)`: `k` 1
/// and `t` 0 when nothing needs protecting.
pub fn title_pull(cells: &[[f32; 4]], mode: u32) -> (f32, f32) {
    // Each part's need for the text the whole title is, light or dark: a
    // part's own read a stroke round light letters as dark text.
    let (light, lt) = title_tone(cells);
    let need = |full: bool| {
        let (mut k, mut t, mut any) = (1.0f32, 0.0f32, false);
        // Over the cells the halo of the title's tone reaches: what
        // dark ink beside light text sits on needs no darkening.
        for m in cells.iter().filter(|m| if full { tone_coverage(m, light) >= HALO_FULL } else { tone_coverage(m, light) > 1e-3 }) {
            any = true;
            let busy = (m[1] - m[0] * m[0]).max(0.0).sqrt();
            match pull(m[0], busy, lt, mode) {
                Ok(kc) => k = k.min(kc),
                Err(tc) => t = t.max(tc),
            }
        }
        any.then_some((k, t))
    };
    need(true).or_else(|| need(false)).unwrap_or((1.0, 0.0))
}

/// How much of a measuring cell `m` is ink of the title's tone: in a
/// light-text title its light ink (ink as light as [`LIGHT_TEXT`] or
/// lighter counting whole, darker less, black not at all), in a dark-text
/// title its dark ink, the same way round. The
/// halo and the pull follow it, so black letterbox bars or a logo block
/// beside white words take none of the words' darkening.
pub fn tone_coverage(m: &[f32; 4], light: bool) -> f32 {
    let cov = m[2].max(0.0);
    if light {
        // Ink as light as light text or lighter counts whole: a grey
        // attribution under white words keeps its halo.
        (m[3] / LIGHT_TEXT).clamp(0.0, cov)
    } else {
        ((m[2] - m[3]) / (1.0 - LIGHT_TEXT)).clamp(0.0, cov)
    }
}

/// The share of a title's ink, by coverage, that must be light text for
/// the whole title to be light text.
pub const LIGHT_SHARE: f32 = 0.1;

/// The least share of a title's edge ink that may be light text for it to
/// count as light text by its need ([`title_tone`]).
pub const MIN_LIGHT_SHARE: f32 = 0.02;

/// A measuring cell the title covers this much or more is solid ink, not
/// an edge of it against the picture.
pub const SOLID: f32 = 0.98;

/// Whether the whole title is light text, and the luminance its need is
/// worked out for, from its measuring grid. One decision holds for the
/// whole title: decided place by place, the cells a black stroke filled
/// read as dark text round white letters, and the picture was lightened
/// into a bright rim about the darkened patch.
///
/// Read from the ink that meets the picture (cells the title covers
/// solidly, a letterbox bar's or a logo block's inside, are left out), the
/// title is light text when the cells reading as light text hold
/// [`LIGHT_SHARE`] of it or more, or, holding [`MIN_LIGHT_SHARE`] of it or
/// more, need more protecting than the dark ink does; at the light ink's
/// luminance. Light text is what darkening
/// protects, and darkening leaves dark ink beside it readable. Taken as
/// the average of all its ink, a big dark block (a logo's place) or black
/// letterbox bars made the white words dark text, and the picture round
/// them was lightened to their own white. Otherwise it is dark text, at
/// its dark ink's luminance.
pub fn title_tone(cells: &[[f32; 4]]) -> (bool, f32) {
    // The ink that meets the picture: a cell the title covers solidly (the
    // inside of a letterbox bar, a logo's block) borders none of it. Only
    // when all of it is solid are those cells the title's.
    let edge = |m: &&[f32; 4]| m[2] > 1e-3 && m[2] < SOLID;
    let any_edge = cells.iter().any(|m| edge(&m));
    let (mut l, mut a, mut ll, mut la) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    // What each kind of ink needs at its own tone, by its coverage: the
    // light ink's darkening (1 - k), the dark ink's lightening t.
    let (mut need_light, mut need_dark) = (0.0f32, 0.0f32);
    for m in cells.iter().filter(|m| if any_edge { edge(m) } else { m[2] > 1e-3 }) {
        l += m[3];
        a += m[2];
        let busy = (m[1] - m[0] * m[0]).max(0.0).sqrt();
        let own = (m[3] / m[2]).clamp(0.0, 1.0);
        if own >= LIGHT_TEXT {
            ll += m[3];
            la += m[2];
            need_light += (1.0 - pull(m[0], busy, own, 1).unwrap_or(1.0)) * m[2];
        } else {
            need_dark += pull(m[0], busy, own, 1).err().unwrap_or(0.0) * m[2];
        }
    }
    if a <= 0.0 {
        return (true, 1.0);
    }
    // Light text by its share of the ink, or where the light ink needs
    // more than the dark: white words between black letterbox bars over a
    // bright sky need it darkened, the bars nothing.
    // A few cells where white letters meet their own black box read as
    // light: not enough to count.
    let light_needs_more = la >= MIN_LIGHT_SHARE * a && need_light / la > need_dark / (a - la).max(1e-6);
    if la >= LIGHT_SHARE * a || light_needs_more {
        (true, (ll / la).clamp(0.0, 1.0))
    } else {
        (false, ((l - ll) / (a - la).max(1e-6)).clamp(0.0, 1.0))
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
    let (k, t) = title_pull(&small, mode);
    let (light, _) = title_tone(&small);
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
            // The halo round the ink of the title's own tone: black bars
            // round white words spread none of the words' darkening.
            let a = tone_coverage(&m, light);
            if a <= 1e-3 {
                continue;
            }
            let halo = halo(a);
            let p = &mut row[x * 4..x * 4 + 3];
            if light {
                let f = 1.0 - halo * (1.0 - k);
                p.iter_mut().for_each(|c| *c *= f);
            } else {
                p.iter_mut().for_each(|c| *c += (1.0 - *c) * halo * t);
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

    /// White text with a black stroke round it (Readability on Auto, a
    /// title's Stroke turned on) is light text: the picture behind it is
    /// darkened or left, never lightened. Where the stroke filled a cell
    /// the text read as dark there, so the pass lightened the picture
    /// round the edge into a white rim about a dark patch.
    #[test]
    fn stroked_light_text_never_lightens_the_picture() {
        let (w, h) = (256, 144);
        let mut acc = frame(w, h, [0.35, 0.3, 0.25, 1.0]);
        let before = acc.clone();
        let mut layer = frame(w, h, [0.0; 4]);
        // Letters 24 high, a 4-pixel black stroke round each.
        for letter in 0..5u32 {
            let x0 = 40 + letter * 36;
            for y in 56..88u32 {
                for x in x0..x0 + 28 {
                    let inside = (60..84).contains(&y) && (x0 + 4..x0 + 24).contains(&x);
                    let i = ((y * w + x) * 4) as usize;
                    let c = if inside { 1.0 } else { 0.0 };
                    layer[i..i + 4].copy_from_slice(&[c, c, c, 1.0]);
                }
            }
        }
        protect_backdrop(&mut acc, &layer, w, h, 1.0, 1, K);
        let lighter = acc.chunks(4).zip(before.chunks(4)).filter(|(a, b)| luma(a, K) > luma(b, K) + 1e-3).count();
        assert_eq!(lighter, 0, "{lighter} pixels of the picture lightened round stroked white text");
    }

    /// A title of a big dark block (a logo's place) above a line of white
    /// text, over a bright sky (the End Screen template): the white text is
    /// light text, and the sky round it is darkened, never lightened. Read
    /// as one title by its ink on average, the block made it dark text, and
    /// the sky round the white words was lightened to white.
    #[test]
    fn white_text_beside_a_dark_block_is_light_text() {
        let (w, h) = (256, 144);
        let mut acc = frame(w, h, [0.7, 0.8, 0.95, 1.0]);
        let before = acc.clone();
        let mut layer = frame(w, h, [0.0; 4]);
        let mut put = |x0: u32, y0: u32, x1: u32, y1: u32, c: f32| {
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = ((y * w + x) * 4) as usize;
                    layer[i..i + 4].copy_from_slice(&[c, c, c, 1.0]);
                }
            }
        };
        // The block, 80 by 64 (far more ink than the words, as the
        // template's is), and under it a line of five letters.
        put(88, 4, 168, 68, 0.03);
        for letter in 0..5u32 {
            put(80 + letter * 20, 90, 92 + letter * 20, 104, 1.0);
        }
        protect_backdrop(&mut acc, &layer, w, h, 1.0, 1, K);
        // The sky just under the letters.
        let lighter = (106..116u32).flat_map(|y| (80..172u32).map(move |x| (y * w + x) as usize * 4)).filter(|&i| luma(&acc[i..i + 4], K) > luma(&before[i..i + 4], K) + 1e-3).count();
        assert_eq!(lighter, 0, "{lighter} pixels of sky round the white text lightened");
    }

    /// Black letterbox bars across the frame with white words between them
    /// (the Opener template), over a bright sky: the words are protected,
    /// but the sky just inside the bars is left as it is. With one halo
    /// round all the ink, the bars' edges took the darkening the white
    /// words asked for, a dark band along each bar.
    #[test]
    fn letterbox_bars_round_white_words_get_no_dark_band() {
        let (w, h) = (256, 144);
        let mut acc = frame(w, h, [0.7, 0.8, 0.95, 1.0]);
        let before = acc.clone();
        let mut layer = frame(w, h, [0.0; 4]);
        let mut put = |x0: u32, y0: u32, x1: u32, y1: u32, c: f32| {
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = ((y * w + x) * 4) as usize;
                    layer[i..i + 4].copy_from_slice(&[c, c, c, 1.0]);
                }
            }
        };
        // The bars, 20 high, and five letters in the middle.
        put(0, 0, w, 20, 0.0);
        put(0, h - 20, w, h, 0.0);
        for letter in 0..5u32 {
            put(80 + letter * 20, 64, 92 + letter * 20, 80, 1.0);
        }
        protect_backdrop(&mut acc, &layer, w, h, 1.0, 1, K);
        let band = |y0: u32, y1: u32| (y0..y1).flat_map(|y| (0..w).map(move |x| (y * w + x) as usize * 4)).map(|i| luma(&acc[i..i + 4], K) - luma(&before[i..i + 4], K)).fold(0.0f32, f32::min);
        let (top, bottom) = (band(20, 44), band(h - 44, h - 20));
        assert!(top > -0.02 && bottom > -0.02, "the sky inside the bars darkened by {top} (top) and {bottom} (bottom)");
        // The words still get their shade.
        let under = (82..86u32).flat_map(|y| (80..172u32).map(move |x| (y * w + x) as usize * 4)).map(|i| luma(&acc[i..i + 4], K) - luma(&before[i..i + 4], K)).fold(0.0f32, f32::min);
        assert!(under < -0.1, "the sky round the words is darkened: {under}");
    }

    /// White text over a picture half bright, half dark: the shade behind
    /// it is the same over both halves, as much as the bright half needs.
    /// Worked out place by place, it darkened the bright half alone.
    #[test]
    fn a_title_over_bright_and_dark_is_shaded_evenly() {
        let (w, h) = (256, 144);
        let back = |x: u32| if x < 128 { [0.9, 0.9, 0.9, 1.0] } else { [0.02, 0.03, 0.25, 1.0] };
        let mut acc: Vec<f32> = (0..h).flat_map(|_| (0..w).flat_map(back)).collect();
        let before = acc.clone();
        let mut layer = frame(w, h, [0.0; 4]);
        for y in 60..84 {
            for x in 64..192 {
                let i = ((y * w + x) * 4) as usize;
                layer[i..i + 4].copy_from_slice(&[1.0, 1.0, 1.0, 1.0]);
            }
        }
        protect_backdrop(&mut acc, &layer, w, h, 1.0, 1, K);
        let factor = |x: u32| {
            let i = ((72 * w + x) * 4) as usize;
            acc[i + 2] / before[i + 2]
        };
        let (bright, dark) = (factor(90), factor(166));
        assert!(bright < 0.5, "the bright half is shaded: {bright}");
        assert!((bright - dark).abs() < 0.02, "the same shade over both halves: {bright} and {dark}");
        // Over dark alone it needs none, and the picture stays as it was.
        let mut dark_only = frame(w, h, [0.02, 0.03, 0.25, 1.0]);
        let kept = dark_only.clone();
        protect_backdrop(&mut dark_only, &layer, w, h, 1.0, 1, K);
        assert_eq!(dark_only, kept, "a title that needs no protection leaves the picture clean");
    }

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
