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

/// Convert wheel puck offset (x, y in -1..1) to an RGB color shift vector.
/// Uses polar coordinates: angle → hue, distance → intensity.
/// master: overall luminance offset added to all channels.
pub fn wheel_offset_to_rgb(offset_x: f32, offset_y: f32, master: f32) -> (f32, f32, f32) {
    let dist = (offset_x * offset_x + offset_y * offset_y).sqrt();
    if dist < 0.001 && master.abs() < 0.001 {
        return (0.0, 0.0, 0.0);
    }
    let hue = offset_y.atan2(offset_x).to_degrees().rem_euclid(360.0);
    let sat = dist.min(1.0);
    let (r, g, b) = hsv_to_rgb(hue, sat, 1.0);
    // Convert from 0..1 color to a bipolar shift: (0.5 → 0, 1.0 → +0.5, 0.0 → -0.5)
    let scale = dist * 0.25; // FCP-like sensitivity
    (
        (r - 0.5) * 2.0 * scale + master,
        (g - 0.5) * 2.0 * scale + master,
        (b - 0.5) * 2.0 * scale + master,
    )
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
/// A channel spread wider than the wheel's rim reaches (0.5) is clamped
/// to the rim.
pub fn rgb_to_wheel_offset(rgb: (f32, f32, f32)) -> (f32, f32, f32) {
    let (r, g, b) = rgb;
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let spread = max - min;
    if spread < 1e-6 {
        return (0.0, 0.0, (r + g + b) / 3.0);
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
    // The chroma part spans `0.5 · dist²` from its lowest channel to its highest.
    let dist = (2.0 * spread).sqrt().min(1.0);
    let (x, y) = (dist * hue.to_radians().cos(), dist * hue.to_radians().sin());
    let (cr, cg, cb) = wheel_offset_to_rgb(x, y, 0.0);
    let master = (r + g + b) / 3.0 - (cr + cg + cb) / 3.0;
    (x, y, master)
}

#[cfg(test)]
mod inverse_tests {
    use super::*;

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
        assert_eq!(rgb_to_wheel_offset((0.1, 0.1, 0.1)), (0.0, 0.0, 0.1), "a neutral shift is the master alone");
    }
}
