/// Affine + 3D perspective transform via inverse mapping with nearest-neighbour sampling.
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
    rotation_x: f32,
    rotation_y: f32,
    perspective: f32,
) -> Vec<u8> {
    // Identity check
    if position_x == 0.0
        && position_y == 0.0
        && (scale_x - 1.0).abs() < f32::EPSILON
        && (scale_y - 1.0).abs() < f32::EPSILON
        && rotation.abs() < f32::EPSILON
        && rotation_x.abs() < f32::EPSILON
        && rotation_y.abs() < f32::EPSILON
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

    // 3D rotation around X and Y axes
    let rx_rad = rotation_x.to_radians();
    let ry_rad = rotation_y.to_radians();
    let cos_rx = rx_rad.cos();
    let sin_rx = rx_rad.sin();
    let cos_ry = ry_rad.cos();
    let sin_ry = ry_rad.sin();

    // Inverse scale
    let inv_sx = if scale_x.abs() > f32::EPSILON { 1.0 / scale_x } else { 1.0 };
    let inv_sy = if scale_y.abs() > f32::EPSILON { 1.0 / scale_y } else { 1.0 };

    // Perspective focal length (higher = less perspective distortion)
    let focal = if perspective > 0.001 { w / perspective } else { 0.0 };
    let has_3d = rotation_x.abs() > 0.01 || rotation_y.abs() > 0.01;

    let mut output = vec![0u8; rgba.len()];

    for oy in 0..height {
        for ox in 0..width {
            let dx = ox as f32 - ax - position_x;
            let dy = oy as f32 - ay - position_y;

            // Inverse Z-rotation (2D)
            let rx = dx * cos_r + dy * sin_r;
            let ry = -dx * sin_r + dy * cos_r;

            // Inverse scale
            let mut sx = rx * inv_sx;
            let mut sy = ry * inv_sy;

            // 3D projection: apply inverse 3D rotations
            if has_3d {
                if focal > 0.0 {
                    // Perspective projection: unproject from screen to 3D, then inverse rotate
                    // Forward: p3d = Ry * Rx * [sx, sy, 0], then project: px = focal*x/z, py = focal*y/z
                    // Inverse: we solve for source coords given output coords
                    // Simplified: apply forward 3D rotation to get the source sampling point
                    let z = 0.0f32;
                    // Rotate around X (tilt)
                    let y1 = sy * cos_rx - z * sin_rx;
                    let z1 = sy * sin_rx + z * cos_rx;
                    // Rotate around Y (pan)
                    let x2 = sx * cos_ry + z1 * sin_ry;
                    let z2 = -sx * sin_ry + z1 * cos_ry;
                    // Perspective divide
                    let w_factor = 1.0 + z2 / focal;
                    if w_factor > 0.01 {
                        sx = x2 / w_factor;
                        sy = y1 / w_factor;
                    }
                } else {
                    // Orthographic 3D rotation (no perspective)
                    let y1 = sy * cos_rx;
                    let x2 = sx * cos_ry;
                    sx = x2;
                    sy = y1;
                }
            }

            sx += ax;
            sy += ay;

            let src_x = sx.round() as i32;
            let src_y = sy.round() as i32;

            if src_x >= 0 && src_x < width as i32 && src_y >= 0 && src_y < height as i32 {
                let src_idx = (src_y as u32 * width + src_x as u32) as usize * 4;
                let dst_idx = (oy * width + ox) as usize * 4;
                output[dst_idx..dst_idx + 4].copy_from_slice(&rgba[src_idx..src_idx + 4]);
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_passthrough() {
        let input = vec![255u8; 16]; // 2x2 white
        let result = apply(input.clone(), 2, 2, 0.0, 0.0, 1.0, 1.0, 0.0, 0.5, 0.5, 0.0, 0.0, 0.0);
        assert_eq!(result, input);
    }
}
