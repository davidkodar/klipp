/// Crop effect: zero out pixels outside the crop rectangle.
/// left/right/top/bottom are fractions (0.0–1.0) of the dimension to remove.
/// feather is in pixels — linear alpha ramp within that distance of the edge.
#[allow(clippy::too_many_arguments)]
pub fn apply(
    mut rgba: Vec<u8>,
    width: u32,
    height: u32,
    left: f32,
    right: f32,
    top: f32,
    bottom: f32,
    feather: f32,
) -> Vec<u8> {
    if left <= 0.0 && right <= 0.0 && top <= 0.0 && bottom <= 0.0 {
        return rgba;
    }

    let w = width as f32;
    let h = height as f32;
    let crop_left = (left * w) as i32;
    let crop_right = ((1.0 - right) * w) as i32;
    let crop_top = (top * h) as i32;
    let crop_bottom = ((1.0 - bottom) * h) as i32;

    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 4) as usize;
            let xi = x as i32;
            let yi = y as i32;

            if xi < crop_left || xi >= crop_right || yi < crop_top || yi >= crop_bottom {
                // Outside crop — fully transparent
                rgba[idx] = 0;
                rgba[idx + 1] = 0;
                rgba[idx + 2] = 0;
                rgba[idx + 3] = 0;
            } else if feather > 0.0 {
                // Distance from the nearest crop edge
                let dist = ((xi - crop_left) as f32)
                    .min((crop_right - 1 - xi) as f32)
                    .min((yi - crop_top) as f32)
                    .min((crop_bottom - 1 - yi) as f32);

                if dist < feather {
                    let factor = dist / feather;
                    rgba[idx + 3] = (rgba[idx + 3] as f32 * factor).clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_crop_is_passthrough() {
        let input = vec![100, 150, 200, 255, 100, 150, 200, 255, 100, 150, 200, 255, 100, 150, 200, 255]; // 2x2
        let output = apply(input.clone(), 2, 2, 0.0, 0.0, 0.0, 0.0, 0.0);
        assert_eq!(input, output);
    }

    #[test]
    fn full_crop_all_transparent() {
        let input = vec![100, 150, 200, 255, 100, 150, 200, 255, 100, 150, 200, 255, 100, 150, 200, 255]; // 2x2
        let output = apply(input, 2, 2, 0.5, 0.5, 0.5, 0.5, 0.0);
        for pixel in output.chunks_exact(4) {
            assert_eq!(pixel, &[0, 0, 0, 0]);
        }
    }

    #[test]
    fn crop_left_half() {
        // 4x1 image, crop left 50%
        let input = vec![
            255, 0, 0, 255,
            0, 255, 0, 255,
            0, 0, 255, 255,
            255, 255, 0, 255,
        ];
        let output = apply(input, 4, 1, 0.5, 0.0, 0.0, 0.0, 0.0);
        // First two pixels should be transparent
        assert_eq!(&output[0..4], &[0, 0, 0, 0]);
        assert_eq!(&output[4..8], &[0, 0, 0, 0]);
        // Last two should be preserved
        assert_eq!(output[11], 255);
        assert_eq!(output[15], 255);
    }

    #[test]
    fn crop_top_half() {
        // 2x2 image, crop top 50%
        let input = vec![
            255, 0, 0, 255,
            0, 255, 0, 255,
            0, 0, 255, 255,
            255, 255, 0, 255,
        ];
        let output = apply(input, 2, 2, 0.0, 0.0, 0.5, 0.0, 0.0);
        // Top row should be transparent
        assert_eq!(&output[0..4], &[0, 0, 0, 0]);
        assert_eq!(&output[4..8], &[0, 0, 0, 0]);
        // Bottom row preserved
        assert_eq!(output[11], 255);
        assert_eq!(output[15], 255);
    }
}
