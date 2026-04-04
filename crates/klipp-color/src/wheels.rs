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
            let (rv, gv, bv) = hsv_to_rgb(hue, sat, 0.82);

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
        // Center should be grey (V=0.82, S=0)
        let expected = (0.82 * 255.0) as u8;
        assert!((pixels[idx] as i16 - expected as i16).unsigned_abs() < 2);
        assert_eq!(pixels[idx + 3], 255); // opaque
    }
}
