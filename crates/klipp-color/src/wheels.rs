/// Standard HSV to RGB conversion.
/// H in degrees [0, 360), S and V in [0, 1]. Returns (r, g, b) in [0, 1].
pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
    if s <= 0.0 {
        return (v, v, v);
    }
    let h = h.rem_euclid(360.0);
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = if h < 60.0 {
        (c, x, 0.0)
    } else if h < 120.0 {
        (x, c, 0.0)
    } else if h < 180.0 {
        (0.0, c, x)
    } else if h < 240.0 {
        (0.0, x, c)
    } else if h < 300.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    (r + m, g + m, b + m)
}

/// Rec.709 luma of an RGB triple: the brightness a wheel's hue shift
/// must leave alone.
fn luma(r: f32, g: f32, b: f32) -> f32 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// The direction of a wheel shift at `hue`: that hue at full strength
/// with its own luma taken out, so the channels move apart around an
/// unchanged brightness. Its highest and lowest channel are 1 apart.
fn chroma_direction(hue: f32) -> (f32, f32, f32) {
    let (r, g, b) = hsv_to_rgb(hue, 1.0, 1.0);
    let y = luma(r, g, b);
    (r - y, g - y, b - y)
}

/// Convert a wheel puck offset (x, y) to an RGB shift, plus `master`
/// on every channel.
///
/// The puck's angle is the hue and its distance how far the channels
/// move apart — `0.5 · dist²` between the highest and lowest, out to
/// the unit circle, then linearly — around an unchanged luma: moving
/// the puck changes the balance, never the brightness, which is the Y
/// slider's (`master`). It used to add the hue at full value, so every
/// channel went up and a nudge toward blue on Gain brightened the
/// picture (R +0.017, G +0.018, B +0.021).
pub fn wheel_offset_to_rgb(offset_x: f32, offset_y: f32, master: f32) -> (f32, f32, f32) {
    let dist = (offset_x * offset_x + offset_y * offset_y).sqrt();
    if dist < 0.001 {
        return (master, master, master);
    }
    let hue = offset_y.atan2(offset_x).to_degrees().rem_euclid(360.0);
    let spread = 0.5 * dist.min(1.0) * dist;
    let (r, g, b) = chroma_direction(hue);
    (r * spread + master, g * spread + master, b * spread + master)
}

/// Generate RGBA pixel data for a color wheel texture of given radius.
/// Center = neutral grey, perimeter = full saturation hues, V=0.82.
/// Returns a (2*radius × 2*radius) RGBA buffer.
/// How saturated the wheel disc gets at its rim, and how bright it is.
///
/// Deliberately short of full. The wheel is a control, not a colour
/// reference, and it sits a few centimetres from the image being
/// judged: a fully saturated disc at high value is the brightest, most
/// saturated thing on screen and drags the eye's adaptation with it.
/// Grading rooms dim the surround for the same reason, and every
/// grading application ships a desaturated interface. The hue mapping
/// is unaffected — only how loudly it is drawn.
pub const WHEEL_RIM_SATURATION: f32 = 0.62;
pub const WHEEL_VALUE: f32 = 0.68;

pub fn generate_wheel_texture(radius: u32) -> Vec<u8> {
    let size = radius * 2;
    let mut pixels = vec![0u8; (size * size * 4) as usize];
    // Use 0.5-offset center so the circle is symmetric across pixel grid
    let cx = radius as f32 - 0.5;
    let cy = radius as f32 - 0.5;
    let r = radius as f32 - 1.5; // shrink slightly so edge never clips

    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let dist = (dx * dx + dy * dy).sqrt();

            let idx = ((y * size + x) * 4) as usize;

            if dist > r + 1.0 {
                pixels[idx + 3] = 0;
                continue;
            }

            let hue = (dy.atan2(dx).to_degrees() + 360.0) % 360.0;
            let sat = (dist / r).clamp(0.0, 1.0);
            // Muted on purpose. The wheel is a control, not a colour
            // reference, and it sits a few centimetres from the image
            // being judged — a fully saturated disc at 82% value is the
            // brightest, most saturated thing on the screen and drags
            // the eye's adaptation with it. Grading rooms dim the
            // surround for the same reason, and every grading
            // application ships a desaturated UI. The hue mapping is
            // unchanged; only its shout is.
            let (rv, gv, bv) = hsv_to_rgb(hue, sat * WHEEL_RIM_SATURATION, WHEEL_VALUE);

            let alpha = (r + 1.0 - dist).clamp(0.0, 1.0);

            pixels[idx]     = (rv * 255.0) as u8;
            pixels[idx + 1] = (gv * 255.0) as u8;
            pixels[idx + 2] = (bv * 255.0) as u8;
            pixels[idx + 3] = (alpha * 255.0) as u8;
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_red() {
        let (r, g, b) = hsv_to_rgb(0.0, 1.0, 1.0);
        assert!((r - 1.0).abs() < 0.01);
        assert!(g < 0.01);
        assert!(b < 0.01);
    }

    #[test]
    fn hsv_green() {
        let (r, g, b) = hsv_to_rgb(120.0, 1.0, 1.0);
        assert!(r < 0.01);
        assert!((g - 1.0).abs() < 0.01);
        assert!(b < 0.01);
    }

    #[test]
    fn hsv_desaturated() {
        let (r, g, b) = hsv_to_rgb(0.0, 0.0, 0.5);
        assert!((r - 0.5).abs() < 0.01);
        assert!((g - 0.5).abs() < 0.01);
        assert!((b - 0.5).abs() < 0.01);
    }

    #[test]
    fn wheel_offset_center_is_zero() {
        let (r, g, b) = wheel_offset_to_rgb(0.0, 0.0, 0.0);
        assert_eq!(r, 0.0);
        assert_eq!(g, 0.0);
        assert_eq!(b, 0.0);
    }

    #[test]
    fn wheel_texture_correct_size() {
        let radius = 54;
        let pixels = generate_wheel_texture(radius);
        assert_eq!(pixels.len(), (radius * 2 * radius * 2 * 4) as usize);
    }

    #[test]
    fn wheel_texture_center_is_grey() {
        let radius = 54;
        let pixels = generate_wheel_texture(radius);
        let size = radius * 2;
        let cx = radius;
        let cy = radius;
        let idx = ((cy * size + cx) * 4) as usize;
        // The centre is neutral: saturation 0, whatever the disc's
        // brightness is set to. Read from the constant rather than
        // repeating it, so changing how loud the wheel is does not
        // look like breaking what the centre means.
        let expected = (WHEEL_VALUE * 255.0) as u8;
        assert!((pixels[idx] as i16 - expected as i16).unsigned_abs() < 2);
        assert_eq!(pixels[idx + 3], 255); // opaque
    }
}

/// The wheel position `(x, y, master)` whose [`wheel_offset_to_rgb`] is
/// `rgb`, for putting a per-channel correction — a CDL — on the wheels.
/// The master is the triple's luma; what is left is a pure balance
/// shift, whose hue is the puck's angle and whose spread its distance.
/// A spread wider than the unit circle reaches (0.5) is clamped to it.
pub fn rgb_to_wheel_offset(rgb: (f32, f32, f32)) -> (f32, f32, f32) {
    let (r, g, b) = rgb;
    let master = luma(r, g, b);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let spread = max - min;
    if spread < 1e-6 {
        return (0.0, 0.0, master);
    }
    // The hue of a triple is unchanged by a constant shift.
    let (nr, ng, nb) = (r - min, g - min, b - min);
    let hue = if nr >= ng && nr >= nb {
        60.0 * ((ng - nb) / spread).rem_euclid(6.0)
    } else if ng >= nb {
        60.0 * ((nb - nr) / spread + 2.0)
    } else {
        60.0 * ((nr - ng) / spread + 4.0)
    };
    let dist = (2.0 * spread).sqrt().min(1.0);
    (dist * hue.to_radians().cos(), dist * hue.to_radians().sin(), master)
}

#[cfg(test)]
mod inverse_tests {
    use super::*;

    /// The other direction, which is the one typing into a wheel's RGB
    /// pills depends on: a triple the wheel can reach is reproduced
    /// exactly through a puck position and a master.
    ///
    /// Without this, typing `+0.250` into R would land on some nearby
    /// number instead — which is the whole reason a colourist types
    /// rather than drags.
    #[test]
    fn a_reachable_triple_comes_back_as_itself() {
        for rgb in [
            (0.25, 0.0, 0.0),
            (0.0, 0.12, 0.0),
            (0.0, 0.0, -0.2),
            (0.1, -0.05, 0.02),
            (-0.2, 0.2, 0.0),
            (0.05, 0.05, 0.05),
        ] {
            let (x, y, m) = rgb_to_wheel_offset(rgb);
            let back = wheel_offset_to_rgb(x, y, m);
            assert!(
                (back.0 - rgb.0).abs() < 1e-4
                    && (back.1 - rgb.1).abs() < 1e-4
                    && (back.2 - rgb.2).abs() < 1e-4,
                "typed {rgb:?} came back as {back:?}",
            );
        }
    }

    /// How far apart the channels can be pulled, which is what limits
    /// what can be typed into a wheel's RGB pills.
    ///
    /// The rim reaches a spread of 0.5 between the highest and lowest
    /// channel. Ask for more and the puck stops at the rim, so the
    /// triple that comes back is the widest the wheel can actually
    /// make — `(-0.3, 0.3, 0)` returns as `(-0.25, 0.25, 0)`.
    ///
    /// That is the honest outcome rather than a failure: the number
    /// shown afterwards is what the grade is doing, and the puck sits
    /// visibly on the rim saying so. A wheel is a bounded control and
    /// typing into it does not make it unbounded.
    #[test]
    fn a_spread_past_the_rim_clamps_to_what_the_wheel_can_make() {
        let (x, y, m) = rgb_to_wheel_offset((-0.3, 0.3, 0.0));
        let dist = (x * x + y * y).sqrt();
        assert!(dist <= 1.0 + 1e-6, "the puck left the wheel at {dist}");
        assert!(dist > 0.99, "an over-wide spread should reach the rim, got {dist}");

        let back = wheel_offset_to_rgb(x, y, m);
        let spread = back.0.max(back.1).max(back.2) - back.0.min(back.1).min(back.2);
        assert!(
            (spread - 0.5).abs() < 1e-3,
            "the rim should give a spread of 0.5, gave {spread} from {back:?}",
        );
    }

    #[test]
    fn a_wheel_position_survives_the_round_trip() {
        for (x, y, m) in [(0.3, 0.1, 0.05), (-0.4, 0.5, -0.1), (0.0, -0.7, 0.0), (0.2, 0.2, 0.3)] {
            let rgb = wheel_offset_to_rgb(x, y, m);
            let (x2, y2, m2) = rgb_to_wheel_offset(rgb);
            let rgb2 = wheel_offset_to_rgb(x2, y2, m2);
            assert!(
                (rgb.0 - rgb2.0).abs() < 1e-4 && (rgb.1 - rgb2.1).abs() < 1e-4 && (rgb.2 - rgb2.2).abs() < 1e-4,
                "{rgb:?} vs {rgb2:?}"
            );
        }
        let (x, y, m) = rgb_to_wheel_offset((0.1, 0.1, 0.1));
        assert_eq!((x, y), (0.0, 0.0), "a neutral shift is the master alone");
        assert!((m - 0.1).abs() < 1e-6);
    }

    /// #320: moving the puck shifts the balance and leaves the
    /// brightness alone, in every direction and at every distance; the
    /// Y slider is what changes brightness.
    #[test]
    fn a_puck_move_leaves_the_luma_where_it_was() {
        for deg in (0..360).step_by(15) {
            for dist in [0.1_f32, 0.5, 1.0, 1.6] {
                let (x, y) = (dist * (deg as f32).to_radians().cos(), dist * (deg as f32).to_radians().sin());
                let (r, g, b) = wheel_offset_to_rgb(x, y, 0.0);
                assert!(luma(r, g, b).abs() < 1e-5, "{deg} deg at {dist}: luma {}", luma(r, g, b));
                let (r2, g2, b2) = wheel_offset_to_rgb(x, y, 0.2);
                assert!((luma(r2, g2, b2) - 0.2).abs() < 1e-5, "the master alone sets the luma");
            }
        }
    }

    /// Toward blue, blue goes up and red and green come down — not all
    /// three up, as before.
    #[test]
    fn toward_blue_is_bluer_not_brighter() {
        let (x, y) = (0.3 * 240f32.to_radians().cos(), 0.3 * 240f32.to_radians().sin());
        let (r, g, b) = wheel_offset_to_rgb(x, y, 0.0);
        assert!(b > 0.0 && r < 0.0 && g < 0.0, "({r}, {g}, {b})");
    }
}
