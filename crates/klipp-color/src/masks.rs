use klipp_core::{MaskShape, MaskStack};

/// Generate a combined mask matte from a stack of masks.
/// Returns a Vec<f32> with one value per pixel (0.0 = outside, 1.0 = inside).
/// Coordinates are normalized: x in 0..1 (left to right), y in 0..1 (top to bottom).
/// Aspect ratio correction: radius and distances are in frame-width units.
pub fn generate_mask_matte(
    width: u32,
    height: u32,
    masks: &MaskStack,
) -> Vec<f32> {
    let pixel_count = (width * height) as usize;
    let mut matte = vec![0.0f32; pixel_count];
    let aspect = height as f32 / width as f32;

    for mask in &masks.masks {
        if !mask.enabled { continue; }

        for y in 0..height {
            let ny = y as f32 / (height - 1).max(1) as f32;
            for x in 0..width {
                let nx = x as f32 / (width - 1).max(1) as f32;
                let idx = (y * width + x) as usize;

                let dist = shape_sdf(nx, ny, &mask.shape, aspect);
                let mut m = smooth_step(dist, mask.feather);
                if mask.invert {
                    m = 1.0 - m;
                }
                m *= mask.opacity;

                // Additive combine: union of masks (max)
                matte[idx] = matte[idx].max(m);
            }
        }
    }

    matte
}

/// Signed distance function for a mask shape.
/// Returns negative inside the shape, positive outside.
/// Distance is in normalized frame-width units.
fn shape_sdf(nx: f32, ny: f32, shape: &MaskShape, aspect: f32) -> f32 {
    match shape {
        MaskShape::Circle { center_x, center_y, radius } => {
            let dx = nx - center_x;
            let dy = (ny - center_y) / aspect; // correct for aspect ratio
            let dist = (dx * dx + dy * dy).sqrt();
            dist - radius
        }
        MaskShape::Rectangle { center_x, center_y, width, height, rotation } => {
            let rad = rotation.to_radians();
            let cos_r = rad.cos();
            let sin_r = rad.sin();
            // Transform point into rectangle's local space
            let dx = nx - center_x;
            let dy = (ny - center_y) / aspect;
            let lx = (dx * cos_r + dy * sin_r).abs();
            let ly = (-dx * sin_r + dy * cos_r).abs();
            let hw = width / 2.0;
            let hh = height / 2.0;
            // SDF for axis-aligned box
            let ox = lx - hw;
            let oy = ly - hh;
            let outside = (ox.max(0.0) * ox.max(0.0) + oy.max(0.0) * oy.max(0.0)).sqrt();
            let inside = ox.max(oy).min(0.0);
            outside + inside
        }
        MaskShape::LinearGradient { start_x, start_y, end_x, end_y } => {
            // Distance along the gradient direction, normalized
            let gx = end_x - start_x;
            let gy = (end_y - start_y) / aspect;
            let glen = (gx * gx + gy * gy).sqrt();
            if glen < 0.0001 { return 0.0; }
            let dx = nx - start_x;
            let dy = (ny - start_y) / aspect;
            let t = (dx * gx + dy * gy) / (glen * glen);
            // t=0 at start (inside), t=1 at end (outside)
            t - 0.5 // centered: negative in first half, positive in second
        }
    }
}

/// Smooth step from 1.0 (inside) to 0.0 (outside) over feather distance.
fn smooth_step(dist: f32, feather: f32) -> f32 {
    if dist <= 0.0 {
        1.0
    } else if feather > 0.0001 && dist < feather {
        let t = dist / feather;
        // Smooth hermite interpolation
        1.0 - t * t * (3.0 - 2.0 * t)
    } else {
        0.0
    }
}
