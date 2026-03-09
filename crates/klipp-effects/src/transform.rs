/// Affine transform via inverse mapping with nearest-neighbour sampling.
#[allow(clippy::too_many_arguments)]
pub fn apply(
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    position_x: f32,
    position_y: f32,
    scale_x: f32,
    scale_y: f32,
    rotation: f32,
    anchor_x: f32,
    anchor_y: f32,
) -> Vec<u8> {
    // Identity check
    if position_x == 0.0
        && position_y == 0.0
        && (scale_x - 1.0).abs() < f32::EPSILON
        && (scale_y - 1.0).abs() < f32::EPSILON
        && rotation.abs() < f32::EPSILON
    {
        return rgba;
    }

    let w = width as f32;
    let h = height as f32;
    let ax = anchor_x * w;
    let ay = anchor_y * h;

    let rot_rad = rotation.to_radians();
    let cos_r = rot_rad.cos();
    let sin_r = rot_rad.sin();

    // Inverse scale (map output → source)
    let inv_sx = if scale_x.abs() > f32::EPSILON { 1.0 / scale_x } else { 1.0 };
    let inv_sy = if scale_y.abs() > f32::EPSILON { 1.0 / scale_y } else { 1.0 };

    let mut output = vec![0u8; rgba.len()];

    for oy in 0..height {
        for ox in 0..width {
            // Translate output pixel to anchor-relative coords, then undo position offset
            let dx = ox as f32 - ax - position_x;
            let dy = oy as f32 - ay - position_y;

            // Inverse rotation
            let rx = dx * cos_r + dy * sin_r;
            let ry = -dx * sin_r + dy * cos_r;

            // Inverse scale
            let sx = rx * inv_sx + ax;
            let sy = ry * inv_sy + ay;

            // Nearest-neighbour sample
            let src_x = sx.round() as i32;
            let src_y = sy.round() as i32;

            if src_x >= 0 && src_x < width as i32 && src_y >= 0 && src_y < height as i32 {
                let src_idx = (src_y as u32 * width + src_x as u32) as usize * 4;
                let dst_idx = (oy * width + ox) as usize * 4;
                output[dst_idx..dst_idx + 4].copy_from_slice(&rgba[src_idx..src_idx + 4]);
            }
            // Out-of-bounds → stays transparent (0,0,0,0)
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_transform() {
        let input = vec![100, 150, 200, 255, 100, 150, 200, 255, 100, 150, 200, 255, 100, 150, 200, 255]; // 2x2
        let output = apply(input.clone(), 2, 2, 0.0, 0.0, 1.0, 1.0, 0.0, 0.5, 0.5);
        assert_eq!(input, output);
    }

    #[test]
    fn translate_moves_pixels() {
        // 4x1: red, green, blue, white — shift right by 1
        let input = vec![
            255, 0, 0, 255,
            0, 255, 0, 255,
            0, 0, 255, 255,
            255, 255, 255, 255,
        ];
        let output = apply(input, 4, 1, 1.0, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0);
        // Pixel 0 should be transparent (nothing maps here)
        assert_eq!(output[3], 0);
        // Pixel 1 should be red (shifted from position 0)
        assert_eq!(&output[4..8], &[255, 0, 0, 255]);
    }

    #[test]
    fn rotation_180_flips() {
        // 5x5 image with anchor at center pixel (2,2)
        let mut input = vec![0u8; 5 * 5 * 4];
        // Pixel (0,0) = red
        input[0..4].copy_from_slice(&[255, 0, 0, 255]);
        // Pixel (4,4) = yellow
        let br = (4 * 5 + 4) * 4;
        input[br..br + 4].copy_from_slice(&[255, 255, 0, 255]);

        // anchor at (0.5, 0.5) means ax=2.5, ay=2.5 on a 5-wide image
        // 180° rotation: output(0,0) → dx=-2.5, dy=-2.5 → rx=2.5, ry=2.5 → src(5,5) = OOB
        // Use anchor (0.4, 0.4) → ax=2, ay=2 for exact pixel mapping
        let output = apply(input, 5, 5, 0.0, 0.0, 1.0, 1.0, 180.0, 0.4, 0.4);
        // With anchor at pixel (2,2), rotation 180:
        // output(0,0) → dx=-2, dy=-2, rx=2, ry=2 → src(4,4) = yellow
        assert_eq!(&output[0..4], &[255, 255, 0, 255]);
        // output(4,4) → dx=2, dy=2, rx=-2, ry=-2 → src(0,0) = red
        assert_eq!(&output[br..br + 4], &[255, 0, 0, 255]);
    }

    #[test]
    fn scale_2x_zooms_in() {
        // 10x10 image: set corners to red, center to green
        let mut input = vec![0u8; 10 * 10 * 4];
        // Center pixel (5,5) = green
        let ci = (5 * 10 + 5) * 4;
        input[ci..ci + 4].copy_from_slice(&[0, 255, 0, 255]);
        // Corner (0,0) = red
        input[0..4].copy_from_slice(&[255, 0, 0, 255]);

        let output = apply(input, 10, 10, 0.0, 0.0, 2.0, 2.0, 0.0, 0.5, 0.5);
        // At 2x scale, center should still map to center
        assert_eq!(output[ci + 3], 255); // center has content
        // Corner (0,0) should be OOB (transparent) — scale zooms in
        assert_eq!(output[3], 0);
    }
}
