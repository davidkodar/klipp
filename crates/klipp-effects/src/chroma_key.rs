/// Chroma key: key out pixels close to the key colour in CbCr space.
pub fn apply(mut rgba: Vec<u8>, key_color: [u8; 3], tolerance: f32, spill_suppression: f32) -> Vec<u8> {
    let (key_cb, key_cr) = rgb_to_cbcr(key_color[0], key_color[1], key_color[2]);

    for pixel in rgba.chunks_exact_mut(4) {
        let (cb, cr) = rgb_to_cbcr(pixel[0], pixel[1], pixel[2]);

        // Distance in CbCr space (normalized 0–1)
        let dcb = (cb - key_cb) / 255.0;
        let dcr = (cr - key_cr) / 255.0;
        let dist = (dcb * dcb + dcr * dcr).sqrt();

        if dist < tolerance {
            // Fully keyed out
            pixel[0] = 0;
            pixel[1] = 0;
            pixel[2] = 0;
            pixel[3] = 0;
        } else if dist < tolerance + spill_suppression {
            // Soft edge
            let factor = (dist - tolerance) / spill_suppression;
            pixel[3] = (pixel[3] as f32 * factor).clamp(0.0, 255.0) as u8;
        }
        // Beyond tolerance + spill_suppression → untouched
    }
    rgba
}

/// Convert RGB to CbCr (BT.601). Returns (Cb, Cr) in 0–255 range.
fn rgb_to_cbcr(r: u8, g: u8, b: u8) -> (f32, f32) {
    let rf = r as f32;
    let gf = g as f32;
    let bf = b as f32;
    let cb = 128.0 + (-0.168736 * rf - 0.331264 * gf + 0.5 * bf);
    let cr = 128.0 + (0.5 * rf - 0.418688 * gf - 0.081312 * bf);
    (cb, cr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_green_keyed_out() {
        let input = vec![0, 255, 0, 255]; // pure green pixel
        let output = apply(input, [0, 255, 0], 0.3, 0.2);
        assert_eq!(output[3], 0); // should be fully transparent
    }

    #[test]
    fn non_key_color_preserved() {
        let input = vec![255, 0, 0, 255]; // pure red pixel
        let output = apply(input.clone(), [0, 255, 0], 0.3, 0.2);
        // Red is far from green in CbCr space — should be preserved
        assert_eq!(output[3], 255);
        assert_eq!(output[0], 255);
    }

    #[test]
    fn white_not_keyed_by_green() {
        let input = vec![255, 255, 255, 255]; // white pixel
        let output = apply(input, [0, 255, 0], 0.3, 0.2);
        // White has different CbCr than pure green
        assert!(output[3] > 0);
    }

    #[test]
    fn blue_screen_key() {
        let input = vec![0, 0, 255, 255]; // pure blue pixel
        let output = apply(input, [0, 0, 255], 0.3, 0.2);
        assert_eq!(output[3], 0); // keyed out
    }

    #[test]
    fn near_green_gets_soft_edge() {
        // A slightly off-green pixel — should get partial alpha
        let input = vec![30, 220, 30, 255];
        let output = apply(input, [0, 255, 0], 0.05, 0.5);
        // Should have reduced but non-zero alpha (soft edge)
        assert!(output[3] > 0);
        assert!(output[3] < 255);
    }
}
