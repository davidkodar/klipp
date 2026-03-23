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

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::Mask;

    fn make_circle_mask(cx: f32, cy: f32, r: f32, feather: f32) -> MaskStack {
        MaskStack {
            masks: vec![Mask {
                shape: MaskShape::Circle { center_x: cx, center_y: cy, radius: r },
                feather,
                invert: false,
                opacity: 1.0,
                enabled: true,
            }],
        }
    }

    /// Sample matte at a specific normalized coordinate.
    /// Uses a 100×100 pixel grid for reasonable precision.
    fn sample_matte(masks: &MaskStack, nx: f32, ny: f32) -> f32 {
        let w = 100u32;
        let h = 100u32;
        let matte = generate_mask_matte(w, h, masks);
        // Map normalized coords to pixel indices
        let px = ((nx * (w - 1) as f32) as u32).min(w - 1);
        let py = ((ny * (h - 1) as f32) as u32).min(h - 1);
        matte[(py * w + px) as usize]
    }

    #[test]
    fn circle_mask_centre_pixel() {
        let masks = make_circle_mask(0.5, 0.5, 0.3, 0.0);
        let m = sample_matte(&masks, 0.5, 0.5);
        // Dead centre of circle → fully inside
        assert!((m - 1.0).abs() < 0.01, "centre should be 1.0: got {m}");
    }

    #[test]
    fn circle_mask_outside_pixel() {
        let masks = make_circle_mask(0.5, 0.5, 0.3, 0.0);
        let m = sample_matte(&masks, 0.9, 0.9);
        // Far outside circle → matte ≈ 0
        assert!(m <= 0.05, "outside should be ~0: got {m}");
    }

    #[test]
    fn circle_mask_feather_zone() {
        // Circle radius 0.3, feather 0.1. Sample at ~radius + feather/2 from centre
        let masks = make_circle_mask(0.5, 0.5, 0.3, 0.1);
        // Point at (0.5 + 0.35, 0.5) = (0.85, 0.5) — 0.35 from centre,
        // which is radius(0.3) + 0.05 into the feather zone (feather=0.1)
        // Hermite smooth_step at t=0.5: 1 - 0.25*(3-1) = 0.5
        let m = sample_matte(&masks, 0.85, 0.5);
        // Should be in the feather transition zone
        assert!(m > 0.1 && m < 0.9,
            "feather zone should have partial matte: got {m}");
    }

    #[test]
    fn circle_mask_invert() {
        let mut masks = make_circle_mask(0.5, 0.5, 0.3, 0.0);
        masks.masks[0].invert = true;
        // Centre should now be outside (inverted)
        let m_centre = sample_matte(&masks, 0.5, 0.5);
        assert!(m_centre <= 0.05, "inverted centre should be ~0: got {m_centre}");
        // Far outside should now be inside
        let m_outside = sample_matte(&masks, 0.05, 0.05);
        assert!(m_outside >= 0.95, "inverted outside should be ~1: got {m_outside}");
    }

    #[test]
    fn rectangle_mask_inside() {
        let masks = MaskStack {
            masks: vec![Mask {
                shape: MaskShape::Rectangle {
                    center_x: 0.5, center_y: 0.5,
                    width: 0.4, height: 0.4, rotation: 0.0,
                },
                feather: 0.0, invert: false, opacity: 1.0, enabled: true,
            }],
        };
        let m = sample_matte(&masks, 0.5, 0.5);
        assert!((m - 1.0).abs() < 0.01, "rect centre should be 1.0: got {m}");
    }

    #[test]
    fn rectangle_mask_outside() {
        let masks = MaskStack {
            masks: vec![Mask {
                shape: MaskShape::Rectangle {
                    center_x: 0.5, center_y: 0.5,
                    width: 0.4, height: 0.4, rotation: 0.0,
                },
                feather: 0.0, invert: false, opacity: 1.0, enabled: true,
            }],
        };
        let m = sample_matte(&masks, 0.05, 0.05);
        assert!(m <= 0.05, "rect far corner should be ~0: got {m}");
    }

    #[test]
    fn mask_stack_union() {
        // Two non-overlapping circles
        let masks = MaskStack {
            masks: vec![
                Mask {
                    shape: MaskShape::Circle { center_x: 0.2, center_y: 0.5, radius: 0.1 },
                    feather: 0.0, invert: false, opacity: 1.0, enabled: true,
                },
                Mask {
                    shape: MaskShape::Circle { center_x: 0.8, center_y: 0.5, radius: 0.1 },
                    feather: 0.0, invert: false, opacity: 1.0, enabled: true,
                },
            ],
        };
        // Inside circle 1
        let m1 = sample_matte(&masks, 0.2, 0.5);
        assert!(m1 >= 0.95, "inside circle 1: got {m1}");
        // Inside circle 2
        let m2 = sample_matte(&masks, 0.8, 0.5);
        assert!(m2 >= 0.95, "inside circle 2: got {m2}");
        // Outside both
        let m_out = sample_matte(&masks, 0.5, 0.5);
        assert!(m_out <= 0.05, "outside both circles: got {m_out}");
    }
}
