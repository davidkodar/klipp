//! Effects that draw outside the picture.
//!
//! Every other effect in the chain changes pixels where the picture
//! already is. These change where the picture *is*. A transform that
//! places a clip smaller than the frame leaves the rest of the buffer
//! transparent, and that transparent margin is exactly what the
//! compositor blends over the track below — so a shadow laid in it
//! falls on the background rather than on the layer, which is what
//! `Glow`, `Bloom` and the rest cannot do: they are image-space passes
//! over a whole frame.
//!
//! Three of them, matching how a composited card is built:
//!
//! * [`rounded_corners`] takes the corners off the picture, before the
//!   transform, so they scale with the card.
//! * [`drop_shadow`] lays the card's silhouette, offset and blurred,
//!   underneath it.
//! * [`outer_glow`] does the same without an offset, as light rather
//!   than shade. A card usually wants both at once, which is why they
//!   are two effects over one routine rather than one effect with a
//!   colour.
//!
//! All of it is straight-alpha linear RGBA. The GPU counterparts live
//! in `gpu::layer` and keep the same silhouette and spill ordering.

use crate::cpu::{decode, each_pixel, gaussian};

/// Take the corners off the picture. The corners are those of what is
/// left of it — the box round everything not already transparent — so a
/// picture cropped to a window gets the window's corners rounded, not
/// the frame's, which the crop has already cut away. `radius` and
/// `feather` are fractions of that box's shorter side, so a card keeps
/// its shape whatever its size.
pub fn rounded_corners(p: Vec<f32>, w: u32, h: u32, radius: f32, feather: f32) -> Vec<f32> {
    let Some((x0, y0, x1, y1)) = opaque_box(&p, w as usize, h as usize) else { return p };
    let short = (x1 - x0).min(y1 - y0);
    let r = radius.clamp(0.0, 0.5) * short;
    if r <= 0.5 {
        return p;
    }
    // At least a pixel of softness, or the corners come out stepped.
    let f = (feather.max(0.0) * short).max(1.0);
    each_pixel(&p, w, h, |x, y, src| {
        let mut o = src.at(x, y);
        // How far the pixel is outside the rounded rectangle on the box:
        // the distance to the nearest corner circle's centre, less its
        // radius. Negative inside.
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let dx = (x0 + r - px).max(px - (x1 - r)).max(0.0);
        let dy = (y0 + r - py).max(py - (y1 - r)).max(0.0);
        let d = (dx * dx + dy * dy).sqrt() - r;
        o[3] *= (0.5 - d / f).clamp(0.0, 1.0);
        o
    })
}

/// The box round every pixel that is not transparent, as edges in
/// pixels, or `None` when there is nothing there.
fn opaque_box(p: &[f32], w: usize, h: usize) -> Option<(f32, f32, f32, f32)> {
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0usize, 0usize);
    for y in 0..h {
        let row = &p[y * w * 4..(y + 1) * w * 4];
        let Some(first) = (0..w).find(|&x| row[x * 4 + 3] > 0.001) else { continue };
        let last = (0..w).rev().find(|&x| row[x * 4 + 3] > 0.001).unwrap_or(first);
        x0 = x0.min(first);
        x1 = x1.max(last + 1);
        y0 = y0.min(y);
        y1 = y + 1;
    }
    (x1 > x0).then_some((x0 as f32, y0 as f32, x1 as f32, y1 as f32))
}

/// Lay the layer's silhouette under it, offset and blurred, in `color`.
/// Offsets and blur are fractions of the frame width.
pub fn drop_shadow(p: Vec<f32>, w: u32, h: u32, offset_x: f32, offset_y: f32, blur: f32, color: [u8; 3], opacity: f32) -> Vec<f32> {
    spills(p, w, h, &[Spill { offset_x, offset_y, blur, color, opacity }])
}

/// The same, centred: light spilling out of the layer's edge onto what
/// is behind it. `radius` is a fraction of the frame width.
pub fn outer_glow(p: Vec<f32>, w: u32, h: u32, radius: f32, intensity: f32, color: [u8; 3]) -> Vec<f32> {
    spills(p, w, h, &[Spill { offset_x: 0.0, offset_y: 0.0, blur: radius, color, opacity: intensity }])
}

/// One blurred, offset, coloured copy of a layer's silhouette.
#[derive(Debug, Clone, Copy)]
pub struct Spill {
    pub offset_x: f32,
    pub offset_y: f32,
    pub blur: f32,
    pub color: [u8; 3],
    pub opacity: f32,
}

impl Spill {
    /// The spill an effect lays, if it is one.
    pub fn of(effect: &crate::BuiltinEffect) -> Option<Self> {
        match *effect {
            crate::BuiltinEffect::DropShadow { offset_x, offset_y, blur, color, opacity } => Some(Self { offset_x, offset_y, blur, color, opacity }),
            crate::BuiltinEffect::OuterGlow { radius, intensity, color } => Some(Self { offset_x: 0.0, offset_y: 0.0, blur: radius, color, opacity: intensity }),
            _ => None,
        }
    }
}

/// The layer over every spill in `run`, each cast from the layer's own
/// silhouette. Applied one after another, a glow would be cast from the
/// layer *and its shadow* — a wide grey plateau with the shadow lost in
/// it — so a run of them is laid together. The first sits nearest the
/// layer.
pub fn spills(p: Vec<f32>, w: u32, h: u32, run: &[Spill]) -> Vec<f32> {
    let casts: Vec<Vec<f32>> = run.iter().filter_map(|s| cast(&p, w, h, s)).collect();
    if casts.is_empty() {
        return p;
    }
    // Stack the casts, the first on top, then the layer over them all.
    let mut under = casts.last().cloned().unwrap_or_default();
    for c in casts.iter().rev().skip(1) {
        under = over(c, under);
    }
    over(&p, under)
}

/// A spill's cast: the silhouette moved and blurred, in its colour, with
/// its opacity in the alpha. `None` when it would not show.
fn cast(p: &[f32], w: u32, h: u32, s: &Spill) -> Option<Vec<f32>> {
    let o = s.opacity.clamp(0.0, 1.0);
    if o <= 0.001 {
        return None;
    }
    let wf = w as f32;
    let (ox, oy) = ((s.offset_x * wf).round() as i32, (s.offset_y * wf).round() as i32);
    let sigma = s.blur.max(0.0) * wf;
    if ox == 0 && oy == 0 && sigma < 0.5 {
        // Nothing would show: the copy sits exactly under the layer.
        return None;
    }
    let col = [decode(s.color[0] as f32 / 255.0), decode(s.color[1] as f32 / 255.0), decode(s.color[2] as f32 / 255.0)];
    // The colour goes everywhere and only the alpha is drawn, so the
    // blur cannot drag the colour toward black at the silhouette's edge.
    let mut out = vec![0.0f32; p.len()];
    for px in out.chunks_exact_mut(4) {
        px[..3].copy_from_slice(&col);
    }
    let (wi, hi) = (w as i32, h as i32);
    for y in 0..hi {
        for x in 0..wi {
            let (sx, sy) = (x - ox, y - oy);
            if sx < 0 || sy < 0 || sx >= wi || sy >= hi {
                continue;
            }
            // Off the frame is empty, not the edge repeated: a shadow
            // must not smear in from the border.
            out[(y as usize * w as usize + x as usize) * 4 + 3] = p[(sy as usize * w as usize + sx as usize) * 4 + 3] * o;
        }
    }
    Some(if sigma >= 0.5 { gaussian(&out, w, h, sigma, true) } else { out })
}

/// How much smaller than the frame [`fill_bars`] builds its fill. It is
/// blurred, so it keeps no detail a full-size buffer would, and a blur
/// an eighth the size reaches as far for a sixty-fourth of the pixels
/// and an eighth of the taps.
pub const FILL_SHRINK: u32 = 8;

/// The size of [`fill_bars`]'s fill for a `w`×`h` frame.
pub fn fill_size(w: u32, h: u32) -> (u32, u32) {
    if w < 8 * FILL_SHRINK || h < 8 * FILL_SHRINK {
        (w, h)
    } else {
        (w.div_ceil(FILL_SHRINK), h.div_ceil(FILL_SHRINK))
    }
}

/// `buf` (`w`×`h`) at `(x, y)` in pixel centres, linear between the four
/// round it, clamped to the pixels `lo..=hi`.
fn bilinear(buf: &[f32], w: u32, x: f32, y: f32, lo: (f32, f32), hi: (f32, f32)) -> [f32; 4] {
    let x = x.clamp(lo.0, hi.0);
    let y = y.clamp(lo.1, hi.1);
    let (ix, iy) = (x.floor(), y.floor());
    let (tx, ty) = (x - ix, y - iy);
    let (x0, y0) = (ix as usize, iy as usize);
    let (x1, y1) = ((ix + 1.0).min(hi.0) as usize, (iy + 1.0).min(hi.1) as usize);
    let at = |x: usize, y: usize, c: usize| buf[(y * w as usize + x) * 4 + c];
    std::array::from_fn(|c| {
        let top = at(x0, y0, c) + (at(x1, y0, c) - at(x0, y0, c)) * tx;
        let bottom = at(x0, y1, c) + (at(x1, y1, c) - at(x0, y1, c)) * tx;
        top + (bottom - top) * ty
    })
}

/// Fill the bars round the picture — a phone's upright shot in a wide
/// frame, a wide one in a tall frame — with the picture itself: scaled
/// until it covers the frame (and `zoom` times more), blurred by `blur`
/// (a fraction of the frame width), darkened by `darken` (0 to 1), and
/// laid under it (#680). What Resolve calls Blanking Fill, and what is
/// otherwise built by hand from a second, enlarged copy of the clip on
/// the track below. A picture that already fills the frame is left as it
/// is, and the frame comes out opaque.
pub fn fill_bars(p: Vec<f32>, w: u32, h: u32, blur: f32, zoom: f32, darken: f32) -> Vec<f32> {
    let Some((x0, y0, x1, y1)) = opaque_box(&p, w as usize, h as usize) else { return p };
    if x0 <= 0.0 && y0 <= 0.0 && x1 >= w as f32 && y1 >= h as f32 {
        return p;
    }
    let (wf, hf) = (w as f32, h as f32);
    let (sw, sh) = fill_size(w, h);
    // Scaled about the picture's middle to cover the frame, centred.
    let scale = (wf / (x1 - x0)).max(hf / (y1 - y0)) * zoom.max(1.0);
    let (cx, cy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
    let mut small = vec![0.0f32; (sw * sh * 4) as usize];
    for (k, px) in small.chunks_exact_mut(4).enumerate() {
        let (i, j) = ((k as u32 % sw) as f32, (k as u32 / sw) as f32);
        let fx = (i + 0.5) * wf / sw as f32;
        let fy = (j + 0.5) * hf / sh as f32;
        let c = bilinear(&p, w, cx + (fx - wf * 0.5) / scale - 0.5, cy + (fy - hf * 0.5) / scale - 0.5, (x0, y0), (x1 - 1.0, y1 - 1.0));
        px.copy_from_slice(&[c[0], c[1], c[2], 1.0]);
    }
    let sigma = blur.max(0.0) * sw as f32;
    let small = if sigma >= 0.5 { gaussian(&small, sw, sh, sigma, true) } else { small };
    let keep = 1.0 - darken.clamp(0.0, 1.0);
    let (rx, ry) = (sw as f32 / wf, sh as f32 / hf);
    let mut out = p;
    for (k, px) in out.chunks_exact_mut(4).enumerate() {
        let (x, y) = ((k as u32 % w) as f32, (k as u32 / w) as f32);
        let bg = bilinear(&small, sw, (x + 0.5) * rx - 0.5, (y + 0.5) * ry - 0.5, (0.0, 0.0), ((sw - 1) as f32, (sh - 1) as f32));
        let a = px[3];
        for c in 0..3 {
            px[c] = px[c] * a + bg[c] * keep * (1.0 - a);
        }
        px[3] = 1.0;
    }
    out
}

/// `top` over `bottom`, straight-alpha src-over, into `bottom`.
fn over(top: &[f32], mut bottom: Vec<f32>) -> Vec<f32> {
    for (dst, src) in bottom.chunks_exact_mut(4).zip(top.chunks_exact(4)) {
        let (sa, da) = (src[3], dst[3]);
        let oa = sa + da * (1.0 - sa);
        if oa <= 0.0 {
            continue;
        }
        for c in 0..3 {
            dst[c] = (src[c] * sa + dst[c] * da * (1.0 - sa)) / oa;
        }
        dst[3] = oa;
    }
    bottom
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame that is opaque white in a centred square and transparent
    /// around it — what a transform leaves when it places a layer.
    fn placed(w: u32, h: u32, half: u32) -> Vec<f32> {
        let mut p = vec![0.0f32; (w * h * 4) as usize];
        let (cx, cy) = (w / 2, h / 2);
        for y in cy - half..cy + half {
            for x in cx - half..cx + half {
                let i = ((y * w + x) * 4) as usize;
                p[i..i + 4].copy_from_slice(&[1.0, 1.0, 1.0, 1.0]);
            }
        }
        p
    }

    fn alpha(p: &[f32], w: u32, x: u32, y: u32) -> f32 {
        p[((y * w + x) * 4 + 3) as usize]
    }

    #[test]
    fn a_shadow_falls_in_the_margin_around_the_layer_and_not_on_it() {
        let (w, h) = (64u32, 64u32);
        let before = placed(w, h, 16);
        // Down and right by 4 px of the frame width, softly.
        let after = drop_shadow(before.clone(), w, h, 4.0 / 64.0, 4.0 / 64.0, 2.0 / 64.0, [0, 0, 0], 0.8);

        // Outside the layer, below and right of it, the frame was empty
        // and now carries shadow: that is what reaches the background.
        assert!(alpha(&before, w, 34, 50) < 0.001);
        assert!(alpha(&after, w, 34, 50) > 0.2, "{}", alpha(&after, w, 34, 50));

        // On the layer nothing changed: still opaque, still white.
        assert!((alpha(&after, w, 32, 32) - 1.0).abs() < 1e-6);
        assert!((after[((32 * w + 32) * 4) as usize] - 1.0).abs() < 1e-6);

        // And away from the layer altogether it is still empty.
        assert!(alpha(&after, w, 2, 2) < 0.001);
    }

    #[test]
    fn a_glow_reaches_out_on_every_side_and_a_shadow_only_where_it_is_cast() {
        let (w, h) = (64u32, 64u32);
        let lit = outer_glow(placed(w, h, 16), w, h, 4.0 / 64.0, 1.0, [255, 255, 255]);
        // Above the layer as much as below: a glow has no offset. The
        // square covers rows 16 to 47, so three clear of each edge.
        let above = alpha(&lit, w, 32, 13);
        let below = alpha(&lit, w, 32, 50);
        assert!(above > 0.1 && (above - below).abs() < 0.05, "{above} vs {below}");

        // A shadow cast downward leaves the top alone.
        let cast = drop_shadow(placed(w, h, 16), w, h, 0.0, 6.0 / 64.0, 1.0 / 64.0, [0, 0, 0], 1.0);
        assert!(alpha(&cast, w, 32, 13) < 0.01);
        assert!(alpha(&cast, w, 32, 50) > 0.5);
    }

    #[test]
    fn a_glow_and_a_shadow_are_both_cast_from_the_layer_not_from_each_other() {
        let (w, h) = (64u32, 64u32);
        let glow = Spill { offset_x: 0.0, offset_y: 0.0, blur: 3.0 / 64.0, color: [255, 255, 255], opacity: 0.5 };
        let shadow = Spill { offset_x: 0.0, offset_y: 8.0 / 64.0, blur: 1.0 / 64.0, color: [0, 0, 0], opacity: 1.0 };
        let alone = spills(placed(w, h, 16), w, h, &[glow]);
        let both = spills(placed(w, h, 16), w, h, &[shadow, glow]);
        // Above the card, where the shadow does not reach, the glow is
        // what it was without it: not cast from the shadow as well.
        assert!((alpha(&alone, w, 32, 12) - alpha(&both, w, 32, 12)).abs() < 1e-4);
        // Below it the shadow shows, dark, over the glow.
        let i = ((54 * w + 32) * 4) as usize;
        assert!(both[i + 3] > 0.5 && both[i] < 0.2, "the shadow is there and dark: {:?}", &both[i..i + 4]);
    }

    #[test]
    fn rounded_corners_take_the_corner_out_of_the_alpha_and_leave_the_middle_alone() {
        let (w, h) = (64u32, 64u32);
        let full = vec![1.0f32; (w * h * 4) as usize];
        let card = rounded_corners(full, w, h, 0.25, 0.0);

        // The very corner is gone, the middle of an edge is not, and the
        // centre is untouched.
        assert!(alpha(&card, w, 0, 0) < 0.01);
        assert!(alpha(&card, w, 32, 0) > 0.99);
        assert!((alpha(&card, w, 32, 32) - 1.0).abs() < 1e-6);

        // The colour is left as it was; only the alpha is carved.
        assert!((card[((32 * w + 0) * 4) as usize] - 1.0).abs() < 1e-6);

        // Cropped to a window, the window's corners are the ones rounded.
        let mut cropped = vec![0.0f32; (w * h * 4) as usize];
        for y in 16..48u32 {
            for x in 8..56u32 {
                let i = ((y * w + x) * 4) as usize;
                cropped[i..i + 4].copy_from_slice(&[1.0; 4]);
            }
        }
        let card = rounded_corners(cropped, w, h, 0.25, 0.0);
        assert!(alpha(&card, w, 8, 16) < 0.01, "the window's corner is gone");
        assert!(alpha(&card, w, 32, 16) > 0.99, "the middle of its top edge is not");

        // A radius of nothing changes nothing.
        let flat = vec![1.0f32; (w * h * 4) as usize];
        assert_eq!(rounded_corners(flat.clone(), w, h, 0.0, 0.0), flat);
    }

    /// #680: an upright picture in a wide frame has its bars filled with
    /// itself, blurred and darkened, the picture untouched and the frame
    /// opaque; a picture that fills the frame, or nothing at all, is
    /// left as it is.
    #[test]
    fn the_bars_are_filled_with_the_picture_itself() {
        let (w, h) = (160u32, 90u32);
        // An upright picture in the middle third: red on its left half,
        // blue on its right.
        let mut upright = vec![0.0f32; (w * h * 4) as usize];
        for y in 0..h {
            for x in 55..105u32 {
                let i = ((y * w + x) * 4) as usize;
                let c = if x < 80 { [0.8, 0.1, 0.1, 1.0] } else { [0.1, 0.1, 0.8, 1.0] };
                upright[i..i + 4].copy_from_slice(&c);
            }
        }
        let filled = fill_bars(upright.clone(), w, h, 0.02, 1.0, 0.25);
        let at = |p: &[f32], x: u32, y: u32| -> [f32; 4] { let i = ((y * w + x) * 4) as usize; [p[i], p[i + 1], p[i + 2], p[i + 3]] };
        // The picture itself is as it was.
        assert_eq!(at(&filled, 60, 45), at(&upright, 60, 45));
        // The left bar is the picture's left side enlarged: red, a
        // quarter darker; the right bar its right side, blue.
        let left = at(&filled, 10, 45);
        assert!(left[3] == 1.0 && left[0] > 0.45 && left[0] < 0.62 && left[2] < 0.15, "{left:?}");
        let right = at(&filled, 150, 45);
        assert!(right[2] > 0.45 && right[0] < 0.15, "{right:?}");
        assert!(filled.chunks_exact(4).all(|p| p[3] == 1.0), "the frame is opaque");
        // More darken, darker.
        let darker = fill_bars(upright.clone(), w, h, 0.02, 1.0, 0.75);
        assert!(at(&darker, 10, 45)[0] < left[0] * 0.5);
        // Nothing to fill: a full frame, and an empty one.
        let full = vec![0.5f32; (w * h * 4) as usize].chunks_exact(4).flat_map(|_| [0.5, 0.5, 0.5, 1.0]).collect::<Vec<f32>>();
        assert_eq!(fill_bars(full.clone(), w, h, 0.02, 1.0, 0.25), full);
        let empty = vec![0.0f32; (w * h * 4) as usize];
        assert_eq!(fill_bars(empty.clone(), w, h, 0.02, 1.0, 0.25), empty);
    }

    #[test]
    fn a_layer_effect_with_nothing_to_draw_returns_the_frame_it_was_given() {
        let (w, h) = (16u32, 16u32);
        let p = placed(w, h, 4);
        assert_eq!(drop_shadow(p.clone(), w, h, 0.0, 0.0, 0.0, [0, 0, 0], 1.0), p);
        assert_eq!(drop_shadow(p.clone(), w, h, 0.1, 0.1, 0.05, [0, 0, 0], 0.0), p);
        assert_eq!(outer_glow(p.clone(), w, h, 0.05, 0.0, [255, 255, 255]), p);
    }
}
