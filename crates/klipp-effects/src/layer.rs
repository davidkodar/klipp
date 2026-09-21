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
//! All of it is straight-alpha linear RGBA, and all of it is CPU-only:
//! `chain_supports_gpu` sends a chain carrying one of these down the
//! CPU path whole, as it does for Frei0r.

use crate::cpu::{decode, each_pixel, gaussian};

/// Take the corners off the picture. `radius` and `feather` are
/// fractions of the frame's shorter side, so a card keeps its shape
/// whatever the delivery size.
pub fn rounded_corners(p: Vec<f32>, w: u32, h: u32, radius: f32, feather: f32) -> Vec<f32> {
    let short = w.min(h) as f32;
    let r = radius.clamp(0.0, 0.5) * short;
    if r <= 0.5 {
        return p;
    }
    // At least a pixel of softness, or the corners come out stepped.
    let f = (feather.max(0.0) * short).max(1.0);
    let (wf, hf) = (w as f32, h as f32);
    each_pixel(&p, w, h, |x, y, src| {
        let mut o = src.at(x, y);
        // How far the pixel is outside the rounded rectangle that
        // covers the frame: the distance to the nearest corner circle's
        // centre, less its radius. Negative inside.
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let dx = (r - px).max(px - (wf - r)).max(0.0);
        let dy = (r - py).max(py - (hf - r)).max(0.0);
        let d = (dx * dx + dy * dy).sqrt() - r;
        o[3] *= (0.5 - d / f).clamp(0.0, 1.0);
        o
    })
}

/// Lay the layer's silhouette under it, offset and blurred, in `color`.
/// Offsets and blur are fractions of the frame width.
pub fn drop_shadow(p: Vec<f32>, w: u32, h: u32, offset_x: f32, offset_y: f32, blur: f32, color: [u8; 3], opacity: f32) -> Vec<f32> {
    spill(p, w, h, offset_x, offset_y, blur, color, opacity)
}

/// The same, centred: light spilling out of the layer's edge onto what
/// is behind it. `radius` is a fraction of the frame width.
pub fn outer_glow(p: Vec<f32>, w: u32, h: u32, radius: f32, intensity: f32, color: [u8; 3]) -> Vec<f32> {
    spill(p, w, h, 0.0, 0.0, radius, color, intensity)
}

/// The layer over a blurred, offset, coloured copy of its own alpha.
fn spill(p: Vec<f32>, w: u32, h: u32, offset_x: f32, offset_y: f32, blur: f32, color: [u8; 3], opacity: f32) -> Vec<f32> {
    let o = opacity.clamp(0.0, 1.0);
    if o <= 0.001 {
        return p;
    }
    let wf = w as f32;
    let (ox, oy) = ((offset_x * wf).round() as i32, (offset_y * wf).round() as i32);
    let sigma = blur.max(0.0) * wf;
    if ox == 0 && oy == 0 && sigma < 0.5 {
        // Nothing would show: the copy sits exactly under the layer.
        return p;
    }
    let col = [decode(color[0] as f32 / 255.0), decode(color[1] as f32 / 255.0), decode(color[2] as f32 / 255.0)];
    // The colour goes everywhere and only the alpha is drawn, so the
    // blur cannot drag the colour toward black at the silhouette's edge.
    let mut cast = vec![0.0f32; p.len()];
    for px in cast.chunks_exact_mut(4) {
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
            cast[(y as usize * w as usize + x as usize) * 4 + 3] = p[(sy as usize * w as usize + sx as usize) * 4 + 3];
        }
    }
    let cast = if sigma >= 0.5 { gaussian(&cast, w, h, sigma, true) } else { cast };
    // The layer over its own shadow, straight-alpha src-over.
    let mut out = p;
    for (dst, src) in out.chunks_exact_mut(4).zip(cast.chunks_exact(4)) {
        let sa = src[3] * o;
        if sa <= 0.0005 {
            continue;
        }
        let da = dst[3];
        let oa = da + sa * (1.0 - da);
        if oa <= 0.0 {
            continue;
        }
        for c in 0..3 {
            dst[c] = (dst[c] * da + src[c] * sa * (1.0 - da)) / oa;
        }
        dst[3] = oa;
    }
    out
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

        // A radius of nothing changes nothing.
        let flat = vec![1.0f32; (w * h * 4) as usize];
        assert_eq!(rounded_corners(flat.clone(), w, h, 0.0, 0.0), flat);
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
