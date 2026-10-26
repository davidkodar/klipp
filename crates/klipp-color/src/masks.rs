use klipp_core::{MaskShape, MaskStack};

/// Generate a combined mask matte from a stack of masks.
/// Returns a `Vec<f32>` with one value per pixel (0.0 = outside, 1.0 = inside).
/// Coordinates are normalized: x in 0..1 (left to right), y in 0..1 (top to bottom).
/// Sizes and distances are in frame widths both ways, so a circle is round
/// and a square square on any frame: a vertical distance is the share of
/// the height times `aspect` (height over width).
pub fn generate_mask_matte(
    width: u32,
    height: u32,
    masks: &MaskStack,
) -> Vec<f32> {
    let pixel_count = (width * height) as usize;
    // Empty, or the whole frame when the first mask takes away from it
    // or keeps only what it shares (#74).
    let start = masks.masks.iter().find(|m| m.enabled).map_or(0.0, |m| if m.combine.is_add() { 0.0 } else { 1.0 });
    let mut matte = vec![start; pixel_count];
    let aspect = height as f32 / width as f32;

    for mask in &masks.masks {
        if !mask.enabled { continue; }
        // A freeform shape's outline, flattened once for this size.
        let outline = match &mask.shape {
            MaskShape::Bezier { points } => Some(klipp_core::mask_path::outline(points, width, height)),
            _ => None,
        };
        let outline = outline.as_deref().map(Vec::as_slice);

        for y in 0..height {
            let ny = y as f32 / (height - 1).max(1) as f32;
            for x in 0..width {
                let nx = x as f32 / (width - 1).max(1) as f32;
                let idx = (y * width + x) as usize;

                // A window tracked in perspective is measured where this
                // pixel was on the frame it was drawn on (#238); a pixel
                // past the surface's horizon is outside it.
                let dist = match mask.keyframes.warp {
                    Some(warp) => warp.apply((nx, ny)).map_or(f32::MAX, |(wx, wy)| shape_sdf(wx, wy, &mask.shape, outline, aspect)),
                    None => shape_sdf(nx, ny, &mask.shape, outline, aspect),
                };
                let mut m = klipp_core::soft_edge(dist, mask.feather_inner, mask.feather);
                if mask.invert {
                    m = 1.0 - m;
                }
                m *= mask.opacity;

                matte[idx] = mask.combine.apply(matte[idx], m);
            }
        }
    }

    matte
}

/// Signed distance function for a mask shape.
/// Returns negative inside the shape, positive outside.
/// Distance is in normalized frame-width units.
fn shape_sdf(nx: f32, ny: f32, shape: &MaskShape, outline: Option<&[(f32, f32)]>, aspect: f32) -> f32 {
    match shape {
        MaskShape::Circle { center_x, center_y, radius } => {
            let dx = nx - center_x;
            let dy = (ny - center_y) * aspect; // in frame widths, as the sizes are
            let dist = (dx * dx + dy * dy).sqrt();
            dist - radius
        }
        MaskShape::Rectangle { center_x, center_y, width, height, rotation } => {
            let rad = rotation.to_radians();
            let cos_r = rad.cos();
            let sin_r = rad.sin();
            // Transform point into rectangle's local space
            let dx = nx - center_x;
            let dy = (ny - center_y) * aspect;
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
            let gx = end_x - start_x;
            let gy = (end_y - start_y) * aspect;
            let glen = (gx * gx + gy * gy).sqrt();
            if glen < 0.0001 { return 0.0; }
            let dx = nx - start_x;
            let dy = (ny - start_y) * aspect;
            let t = (dx * gx + dy * gy) / (glen * glen);
            t - 0.5
        }
        // In gradient widths from the ring midway between the radii, as
        // a linear gradient is from its midpoint (#2704): -0.5 at
        // `inner`, 0.5 at `outer`; a ring of no width is a hard circle.
        MaskShape::RadialGradient { center_x, center_y, inner, outer } => {
            let dx = nx - center_x;
            let dy = (ny - center_y) * aspect;
            let r = (dx * dx + dy * dy).sqrt();
            let span = outer - inner;
            let mid = (inner + outer) * 0.5;
            if span.abs() < 0.0001 { r - mid } else { (r - mid) / span }
        }
        MaskShape::Bezier { .. } => {
            // The path's outline, flattened by the caller (#74).
            let points = outline.unwrap_or(&[]);
            if points.len() < 3 { return 1.0; } // need at least a triangle
            // Signed distance to closed polygon (winding number for inside/outside)
            let px = nx;
            let py = ny * aspect;
            let n = points.len();
            let mut min_dist_sq = f32::MAX;
            let mut winding = 0i32;
            for i in 0..n {
                let (ax, ay_raw) = points[i];
                let (bx, by_raw) = points[(i + 1) % n];
                let ay = ay_raw * aspect;
                let by = by_raw * aspect;
                // Closest point on segment to (px, py)
                let ex = bx - ax;
                let ey = by - ay;
                let dx = px - ax;
                let dy = py - ay;
                let t = ((dx * ex + dy * ey) / (ex * ex + ey * ey)).clamp(0.0, 1.0);
                let cx = ax + t * ex - px;
                let cy = ay + t * ey - py;
                min_dist_sq = min_dist_sq.min(cx * cx + cy * cy);
                // Winding number contribution
                if ay <= py {
                    if by > py {
                        let cross = ex * (py - ay) - ey * (px - ax);
                        if cross > 0.0 { winding += 1; }
                    }
                } else if by <= py {
                    let cross = ex * (py - ay) - ey * (px - ax);
                    if cross < 0.0 { winding -= 1; }
                }
            }
            let dist = min_dist_sq.sqrt();
            if winding != 0 { -dist } else { dist } // negative inside
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_core::Mask;

    fn make_circle_mask(cx: f32, cy: f32, r: f32, feather: f32) -> MaskStack {
        MaskStack {
            masks: vec![Mask { feather_inner: 0.0, combine: klipp_core::MaskCombine::Add, name: String::new(), locked: false,
                shape: MaskShape::Circle { center_x: cx, center_y: cy, radius: r },
                feather,
                invert: false,
                opacity: 1.0,
                enabled: true,
                keyframes: Default::default(),
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
            masks: vec![Mask { feather_inner: 0.0, combine: klipp_core::MaskCombine::Add, name: String::new(), locked: false,
                shape: MaskShape::Rectangle {
                    center_x: 0.5, center_y: 0.5,
                    width: 0.4, height: 0.4, rotation: 0.0,
                },
                feather: 0.0, invert: false, opacity: 1.0, enabled: true, keyframes: Default::default(),
            }],
        };
        let m = sample_matte(&masks, 0.5, 0.5);
        assert!((m - 1.0).abs() < 0.01, "rect centre should be 1.0: got {m}");
    }

    #[test]
    fn rectangle_mask_outside() {
        let masks = MaskStack {
            masks: vec![Mask { feather_inner: 0.0, combine: klipp_core::MaskCombine::Add, name: String::new(), locked: false,
                shape: MaskShape::Rectangle {
                    center_x: 0.5, center_y: 0.5,
                    width: 0.4, height: 0.4, rotation: 0.0,
                },
                feather: 0.0, invert: false, opacity: 1.0, enabled: true, keyframes: Default::default(),
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
                Mask { feather_inner: 0.0, combine: klipp_core::MaskCombine::Add, name: String::new(), locked: false,
                    shape: MaskShape::Circle { center_x: 0.2, center_y: 0.5, radius: 0.1 },
                    feather: 0.0, invert: false, opacity: 1.0, enabled: true, keyframes: Default::default(),
                },
                Mask { feather_inner: 0.0, combine: klipp_core::MaskCombine::Add, name: String::new(), locked: false,
                    shape: MaskShape::Circle { center_x: 0.8, center_y: 0.5, radius: 0.1 },
                    feather: 0.0, invert: false, opacity: 1.0, enabled: true, keyframes: Default::default(),
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

    /// #663: a window is as tall as it is wide in pixels on a wide frame —
    /// a circle round, a square turned 45° a diamond with equal
    /// diagonals. Before, a vertical distance was divided by the aspect
    /// instead of multiplied, and on 16:9 a circle came out under a third
    /// as tall as it was wide.
    #[test]
    fn a_window_is_round_on_a_wide_frame() {
        let (w, h) = (160u32, 90u32);
        let at = |m: &[f32], x: u32, y: u32| m[(y * w + x) as usize];
        let one = |shape| MaskStack { masks: vec![Mask { shape, feather: 0.0, ..Mask::default() }] };
        // 0.2 of 160 is 32 px; the centre is pixel 79.5, 44.5.
        let c = generate_mask_matte(w, h, &one(MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.2 }));
        assert_eq!(at(&c, 80 + 30, 45), 1.0);
        assert_eq!(at(&c, 80, 45 + 30), 1.0, "as far down as across");
        assert_eq!(at(&c, 80, 45 + 34), 0.0);
        assert_eq!(at(&c, 80 + 34, 45), 0.0);
        // Half a diagonal: 0.125 × √2 × 160 ≈ 28 px, both ways.
        let d = generate_mask_matte(w, h, &one(MaskShape::Rectangle { center_x: 0.5, center_y: 0.5, width: 0.25, height: 0.25, rotation: 45.0 }));
        assert_eq!(at(&d, 80 + 26, 45), 1.0);
        assert_eq!(at(&d, 80, 45 + 26), 1.0);
        assert_eq!(at(&d, 80, 45 + 31), 0.0);
        assert_eq!(at(&d, 80 + 31, 45), 0.0);
    }

    // ── SDF unit tests (shape_sdf directly) ──────────────────────────────

    /// Helper: evaluate shape_sdf with aspect=1 (square frame) for simplicity.
    fn sdf(nx: f32, ny: f32, shape: &MaskShape) -> f32 {
        sdf_aspect(nx, ny, shape, 1.0)
    }

    /// The edge as masks always had it: all inside, a fall-off outside.
    fn smooth_step(dist: f32, feather: f32) -> f32 {
        klipp_core::soft_edge(dist, 0.0, feather)
    }

    /// Helper: evaluate shape_sdf with a non-square aspect ratio.
    fn sdf_aspect(nx: f32, ny: f32, shape: &MaskShape, aspect: f32) -> f32 {
        let outline = match shape {
            MaskShape::Bezier { points } => klipp_core::mask_path::flatten(points, 1e-5, aspect),
            _ => Vec::new(),
        };
        shape_sdf(nx, ny, shape, Some(&outline), aspect)
    }

    // ── 1. Circle: center inside, far away outside ───────────────────────

    #[test]
    fn sdf_circle_center_is_inside() {
        let shape = MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.3 };
        let d = sdf(0.5, 0.5, &shape);
        assert!(d < 0.0, "center should be inside (negative): got {d}");
        assert!((d - (-0.3)).abs() < 1e-5, "distance at center should be -radius: got {d}");
    }

    #[test]
    fn sdf_circle_far_away_is_outside() {
        let shape = MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.1 };
        let d = sdf(1.0, 1.0, &shape);
        assert!(d > 0.0, "far corner should be outside (positive): got {d}");
    }

    // ── 2. Circle: boundary ≈ 0 ─────────────────────────────────────────

    #[test]
    fn sdf_circle_boundary_approximately_zero() {
        let shape = MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.25 };
        // Point exactly on boundary (right edge): (0.75, 0.5)
        let d = sdf(0.75, 0.5, &shape);
        assert!(d.abs() < 1e-5, "boundary point should be ~0: got {d}");
        // Point on boundary (top): (0.5, 0.25) — requires aspect=1
        let d_top = sdf(0.5, 0.25, &shape);
        assert!(d_top.abs() < 1e-5, "top boundary point should be ~0: got {d_top}");
    }

    #[test]
    fn sdf_circle_boundary_with_aspect_ratio() {
        // On 16:9, as round in pixels as on a square frame.
        // Boundary along x-axis: center_x + radius = 0.5 + 0.2 = 0.7
        let shape = MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.2 };
        let d = sdf_aspect(0.7, 0.5, &shape, 0.5625);
        assert!(d.abs() < 1e-5, "x-axis boundary with aspect should be ~0: got {d}");
    }

    // ── 3. Rectangle: center inside, outside positive ────────────────────

    #[test]
    fn sdf_rectangle_center_is_inside() {
        let shape = MaskShape::Rectangle {
            center_x: 0.5, center_y: 0.5, width: 0.4, height: 0.3, rotation: 0.0,
        };
        let d = sdf(0.5, 0.5, &shape);
        assert!(d < 0.0, "rect center should be inside: got {d}");
        // Distance to nearest edge = min(hw, hh) = min(0.2, 0.15) = -0.15
        assert!((d - (-0.15)).abs() < 1e-5, "distance at center should be -0.15: got {d}");
    }

    #[test]
    fn sdf_rectangle_outside_is_positive() {
        let shape = MaskShape::Rectangle {
            center_x: 0.5, center_y: 0.5, width: 0.2, height: 0.2, rotation: 0.0,
        };
        // Point (0.0, 0.0) is well outside
        let d = sdf(0.0, 0.0, &shape);
        assert!(d > 0.0, "outside rect should be positive: got {d}");
    }

    #[test]
    fn sdf_rectangle_edge_approximately_zero() {
        let shape = MaskShape::Rectangle {
            center_x: 0.5, center_y: 0.5, width: 0.4, height: 0.4, rotation: 0.0,
        };
        // Right edge: center_x + width/2 = 0.5 + 0.2 = 0.7
        let d = sdf(0.7, 0.5, &shape);
        assert!(d.abs() < 1e-5, "right edge should be ~0: got {d}");
    }

    // ── 4. Rotated rectangle still contains expected points ──────────────

    #[test]
    fn sdf_rectangle_rotated_45_degrees() {
        let shape = MaskShape::Rectangle {
            center_x: 0.5, center_y: 0.5, width: 0.4, height: 0.4, rotation: 45.0,
        };
        // Center should still be inside
        let d_center = sdf(0.5, 0.5, &shape);
        assert!(d_center < 0.0, "rotated rect center should be inside: got {d_center}");

        // Point just right of center (0.65, 0.5) — was inside unrotated rect.
        // After 45deg rotation, this point at local coords:
        // lx = |0.15 * cos45 + 0 * sin45| = |0.106|
        // ly = |-0.15 * sin45 + 0 * cos45| = |0.106|
        // hw = hh = 0.2, so 0.106 < 0.2 → inside
        let d_nearby = sdf(0.65, 0.5, &shape);
        assert!(d_nearby < 0.0, "point near center of rotated rect should be inside: got {d_nearby}");

        // Corner of the unrotated rect (0.7, 0.7) is outside the 45deg rotated rect.
        // lx = |0.2 * cos45 + 0.2 * sin45| = |0.283|
        // hw = 0.2, so 0.283 > 0.2 → outside
        let d_corner = sdf(0.7, 0.7, &shape);
        assert!(d_corner > 0.0, "corner of unrotated rect should be outside rotated rect: got {d_corner}");
    }

    #[test]
    fn sdf_rectangle_rotated_90_degrees() {
        // A non-square rect: 0.6 wide, 0.2 tall, rotated 90deg → effectively 0.2 wide, 0.6 tall
        let shape = MaskShape::Rectangle {
            center_x: 0.5, center_y: 0.5, width: 0.6, height: 0.2, rotation: 90.0,
        };
        // Point (0.5, 0.25) is above center by 0.25 — should be inside rotated rect
        // (original width/2 = 0.3, now becomes half-height after rotation)
        let d = sdf(0.5, 0.25, &shape);
        assert!(d < 0.0, "point along long axis of 90deg rotated rect should be inside: got {d}");

        // Point (0.8, 0.5) is right of center by 0.3 — should be outside
        // (original height/2 = 0.1, now becomes half-width)
        let d_out = sdf(0.8, 0.5, &shape);
        assert!(d_out > 0.0, "point along short axis of 90deg rotated rect should be outside: got {d_out}");
    }

    // ── RadialGradient: whole inside the inner ring, gone past the outer,
    // falling in between (#2704) ───────────────────────────────────────
    #[test]
    fn radial_gradient_fades_from_its_inner_ring_to_its_outer() {
        let shape = MaskShape::RadialGradient { center_x: 0.5, center_y: 0.5, inner: 0.1, outer: 0.3 };
        let at = |r: f32| klipp_core::soft_edge(sdf_aspect(0.5 + r, 0.5, &shape, 1.0), 0.5, 0.5);
        assert_eq!(at(0.0), 1.0);
        assert_eq!(at(0.1), 1.0);
        assert!((at(0.2) - 0.5).abs() < 1e-4, "midway is half: {}", at(0.2));
        assert!(at(0.15) > at(0.2) && at(0.2) > at(0.25), "falls outward");
        assert_eq!(at(0.3), 0.0);
        assert_eq!(at(0.45), 0.0);
        // Measured in widths both ways: as far down is as faded.
        let down = klipp_core::soft_edge(sdf_aspect(0.5, 0.5 + 0.15 / 0.5, &shape, 0.5), 0.5, 0.5);
        assert!((down - at(0.15)).abs() < 1e-4);
        // A ring of no width is a hard circle at its radius.
        let ring = MaskShape::RadialGradient { center_x: 0.5, center_y: 0.5, inner: 0.2, outer: 0.2 };
        assert!(sdf_aspect(0.69, 0.5, &ring, 1.0) < 0.0 && sdf_aspect(0.71, 0.5, &ring, 1.0) > 0.0);
    }

    // ── 5. LinearGradient: start inside, past end outside ────────────────

    #[test]
    fn sdf_linear_gradient_at_start() {
        let shape = MaskShape::LinearGradient {
            start_x: 0.2, start_y: 0.5, end_x: 0.8, end_y: 0.5,
        };
        // At start point t=0, sdf = 0 - 0.5 = -0.5 (inside)
        let d = sdf(0.2, 0.5, &shape);
        assert!(d < 0.0, "start of gradient should be inside: got {d}");
    }

    #[test]
    fn sdf_linear_gradient_past_end() {
        let shape = MaskShape::LinearGradient {
            start_x: 0.2, start_y: 0.5, end_x: 0.8, end_y: 0.5,
        };
        // At end point t=1, sdf = 1 - 0.5 = 0.5 (outside)
        let d = sdf(0.8, 0.5, &shape);
        assert!(d > 0.0, "end of gradient should be outside: got {d}");
    }

    #[test]
    fn sdf_linear_gradient_midpoint_is_zero() {
        let shape = MaskShape::LinearGradient {
            start_x: 0.0, start_y: 0.5, end_x: 1.0, end_y: 0.5,
        };
        // At midpoint t=0.5, sdf = 0.5 - 0.5 = 0
        let d = sdf(0.5, 0.5, &shape);
        assert!(d.abs() < 1e-5, "gradient midpoint should be ~0: got {d}");
    }

    #[test]
    fn sdf_linear_gradient_vertical() {
        let shape = MaskShape::LinearGradient {
            start_x: 0.5, start_y: 0.0, end_x: 0.5, end_y: 1.0,
        };
        // At top (start), t=0 → sdf = -0.5
        let d_top = sdf(0.5, 0.0, &shape);
        assert!(d_top < 0.0, "top of vertical gradient should be inside: got {d_top}");
        // At bottom (end), t=1 → sdf = 0.5
        let d_bot = sdf(0.5, 1.0, &shape);
        assert!(d_bot > 0.0, "bottom of vertical gradient should be outside: got {d_bot}");
    }

    // ── 6. Bezier polygon (triangle): inside/outside ─────────────────────

    #[test]
    fn sdf_bezier_triangle_inside() {
        // Triangle with vertices at (0.5, 0.2), (0.2, 0.8), (0.8, 0.8)
        let shape = MaskShape::polygon(vec![(0.5, 0.2), (0.2, 0.8), (0.8, 0.8)]);
        // Centroid at (0.5, 0.6) should be inside
        let d = sdf(0.5, 0.6, &shape);
        assert!(d < 0.0, "centroid of triangle should be inside: got {d}");
    }

    #[test]
    fn sdf_bezier_triangle_outside() {
        let shape = MaskShape::polygon(vec![(0.5, 0.2), (0.2, 0.8), (0.8, 0.8)]);
        // Point far above apex should be outside
        let d = sdf(0.5, 0.05, &shape);
        assert!(d > 0.0, "point above triangle apex should be outside: got {d}");
        // Point to the far left should be outside
        let d_left = sdf(0.05, 0.5, &shape);
        assert!(d_left > 0.0, "point far left of triangle should be outside: got {d_left}");
    }

    #[test]
    fn sdf_bezier_triangle_vertex_approximately_zero() {
        let shape = MaskShape::polygon(vec![(0.5, 0.2), (0.2, 0.8), (0.8, 0.8)]);
        // A vertex is on the boundary, so distance should be ~0
        let d = sdf(0.5, 0.2, &shape);
        assert!(d.abs() < 1e-4, "vertex of triangle should be ~0: got {d}");
    }

    // ── 7. Bezier polygon (square): winding number for 4 vertices ────────

    #[test]
    fn sdf_bezier_square_inside() {
        // Square from (0.3,0.3) to (0.7,0.7) — CCW winding
        let shape = MaskShape::polygon(vec![(0.3, 0.3), (0.7, 0.3), (0.7, 0.7), (0.3, 0.7)]);
        let d = sdf(0.5, 0.5, &shape);
        assert!(d < 0.0, "center of bezier square should be inside: got {d}");
    }

    #[test]
    fn sdf_bezier_square_outside() {
        let shape = MaskShape::polygon(vec![(0.3, 0.3), (0.7, 0.3), (0.7, 0.7), (0.3, 0.7)]);
        let d = sdf(0.1, 0.1, &shape);
        assert!(d > 0.0, "point outside bezier square should be positive: got {d}");
    }

    #[test]
    fn sdf_bezier_square_near_edge() {
        let shape = MaskShape::polygon(vec![(0.3, 0.3), (0.7, 0.3), (0.7, 0.7), (0.3, 0.7)]);
        // Point on edge midpoint: (0.7, 0.5) should be ~0
        let d = sdf(0.7, 0.5, &shape);
        assert!(d.abs() < 1e-4, "edge midpoint of bezier square should be ~0: got {d}");
    }

    #[test]
    fn sdf_bezier_square_winding_both_orderings() {
        // CW winding order
        let shape_cw = MaskShape::polygon(vec![(0.3, 0.3), (0.3, 0.7), (0.7, 0.7), (0.7, 0.3)]);
        let d_cw = sdf(0.5, 0.5, &shape_cw);
        assert!(d_cw < 0.0, "CW square center should be inside: got {d_cw}");

        // CCW winding order
        let shape_ccw = MaskShape::polygon(vec![(0.3, 0.3), (0.7, 0.3), (0.7, 0.7), (0.3, 0.7)]);
        let d_ccw = sdf(0.5, 0.5, &shape_ccw);
        assert!(d_ccw < 0.0, "CCW square center should be inside: got {d_ccw}");
    }

    // ── 8. Feathering: intermediate values in feather zone ───────────────

    #[test]
    fn feather_zone_has_intermediate_values() {
        let masks = MaskStack {
            masks: vec![Mask { feather_inner: 0.0, combine: klipp_core::MaskCombine::Add, name: String::new(), locked: false,
                shape: MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.2 },
                feather: 0.15,
                invert: false,
                opacity: 1.0,
                enabled: true,
                keyframes: Default::default(),
            }],
        };
        // Use a larger grid for precision
        let w = 200u32;
        let h = 200u32;
        let matte = generate_mask_matte(w, h, &masks);

        // Sample a ring of points at varying distances from center
        let mut found_intermediate = false;
        for px in 0..w {
            let _nx = px as f32 / (w - 1) as f32;
            let ny = 0.5; // horizontal slice through center
            let idx = ((ny * (h - 1) as f32) as u32 * w + px) as usize;
            let v = matte[idx];
            if v > 0.05 && v < 0.95 {
                found_intermediate = true;
                break;
            }
        }
        assert!(found_intermediate, "should find intermediate matte values in feather zone");
    }

    #[test]
    fn feather_smooth_step_hermite_values() {
        // Directly test smooth_step function
        // dist <= 0 → 1.0
        assert_eq!(smooth_step(-0.1, 0.1), 1.0);
        assert_eq!(smooth_step(0.0, 0.1), 1.0);
        // dist >= feather → 0.0
        assert_eq!(smooth_step(0.1, 0.1), 0.0);
        assert_eq!(smooth_step(0.2, 0.1), 0.0);
        // dist at midpoint: t=0.5, hermite = 1 - 0.25*(3-1) = 0.5
        let mid = smooth_step(0.05, 0.1);
        assert!((mid - 0.5).abs() < 1e-5, "hermite midpoint should be 0.5: got {mid}");
        // Monotonicity: closer to edge → smaller matte
        let near = smooth_step(0.02, 0.1);
        let far = smooth_step(0.08, 0.1);
        assert!(near > far, "closer to shape should have higher matte: near={near}, far={far}");
    }

    #[test]
    fn feather_zero_gives_hard_edge() {
        let masks_hard = make_circle_mask(0.5, 0.5, 0.2, 0.0);
        let m_inside = sample_matte(&masks_hard, 0.5, 0.5);
        let m_outside = sample_matte(&masks_hard, 0.9, 0.5);
        assert!((m_inside - 1.0).abs() < 0.01, "inside hard edge: got {m_inside}");
        assert!(m_outside < 0.01, "outside hard edge: got {m_outside}");
    }

    // ── 9. Invert: opposite values ───────────────────────────────────────

    #[test]
    fn invert_produces_opposite_values() {
        // Normal mask
        let masks_normal = make_circle_mask(0.5, 0.5, 0.25, 0.0);
        let normal_center = sample_matte(&masks_normal, 0.5, 0.5);
        let normal_outside = sample_matte(&masks_normal, 0.95, 0.95);

        // Inverted mask
        let mut masks_inv = make_circle_mask(0.5, 0.5, 0.25, 0.0);
        masks_inv.masks[0].invert = true;
        let inv_center = sample_matte(&masks_inv, 0.5, 0.5);
        let inv_outside = sample_matte(&masks_inv, 0.95, 0.95);

        // Values should be complements
        assert!((normal_center + inv_center - 1.0).abs() < 0.02,
            "inverted center should complement normal: {normal_center} + {inv_center}");
        assert!((normal_outside + inv_outside - 1.0).abs() < 0.02,
            "inverted outside should complement normal: {normal_outside} + {inv_outside}");
    }

    #[test]
    fn invert_rectangle() {
        let masks = MaskStack {
            masks: vec![Mask { feather_inner: 0.0, combine: klipp_core::MaskCombine::Add, name: String::new(), locked: false,
                shape: MaskShape::Rectangle {
                    center_x: 0.5, center_y: 0.5,
                    width: 0.4, height: 0.4, rotation: 0.0,
                },
                feather: 0.0, invert: true, opacity: 1.0, enabled: true, keyframes: Default::default(),
            }],
        };
        // Center of inverted rect should be 0
        let m_center = sample_matte(&masks, 0.5, 0.5);
        assert!(m_center < 0.05, "inverted rect center should be ~0: got {m_center}");
        // Outside inverted rect should be 1
        let m_outside = sample_matte(&masks, 0.05, 0.05);
        assert!(m_outside > 0.95, "inverted rect outside should be ~1: got {m_outside}");
    }

    // ── 10. Opacity: half opacity halves matte ───────────────────────────

    #[test]
    fn opacity_scales_matte_value() {
        // Full opacity
        let masks_full = make_circle_mask(0.5, 0.5, 0.3, 0.0);
        let v_full = sample_matte(&masks_full, 0.5, 0.5);

        // Half opacity
        let mut masks_half = make_circle_mask(0.5, 0.5, 0.3, 0.0);
        masks_half.masks[0].opacity = 0.5;
        let v_half = sample_matte(&masks_half, 0.5, 0.5);

        assert!((v_full - 1.0).abs() < 0.01, "full opacity inside: got {v_full}");
        assert!((v_half - 0.5).abs() < 0.01, "half opacity inside: got {v_half}");
    }

    #[test]
    fn opacity_zero_produces_zero_matte() {
        let mut masks = make_circle_mask(0.5, 0.5, 0.3, 0.0);
        masks.masks[0].opacity = 0.0;
        let m = sample_matte(&masks, 0.5, 0.5);
        assert!(m < 0.01, "zero opacity should produce zero matte: got {m}");
    }

    #[test]
    fn opacity_scales_feathered_values() {
        let mut masks = make_circle_mask(0.5, 0.5, 0.2, 0.15);
        masks.masks[0].opacity = 0.5;
        // Inside: matte = 1.0 * 0.5 = 0.5
        let m_inside = sample_matte(&masks, 0.5, 0.5);
        assert!((m_inside - 0.5).abs() < 0.02, "half opacity inside feathered: got {m_inside}");
        // Outside: matte = 0.0 * 0.5 = 0.0
        let m_outside = sample_matte(&masks, 0.95, 0.95);
        assert!(m_outside < 0.01, "half opacity outside feathered: got {m_outside}");
    }

    // ── 11. Multiple masks: union (max) behavior ─────────────────────────

    #[test]
    fn mask_union_takes_max_value() {
        // Two circles with different opacities overlapping at center
        let masks = MaskStack {
            masks: vec![
                Mask { feather_inner: 0.0, combine: klipp_core::MaskCombine::Add, name: String::new(), locked: false,
                    shape: MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.3 },
                    feather: 0.0, invert: false, opacity: 0.3, enabled: true, keyframes: Default::default(),
                },
                Mask { feather_inner: 0.0, combine: klipp_core::MaskCombine::Add, name: String::new(), locked: false,
                    shape: MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.3 },
                    feather: 0.0, invert: false, opacity: 0.7, enabled: true, keyframes: Default::default(),
                },
            ],
        };
        // Union = max(0.3, 0.7) = 0.7, not additive 1.0
        let m = sample_matte(&masks, 0.5, 0.5);
        assert!((m - 0.7).abs() < 0.02, "union should be max(0.3, 0.7) = 0.7: got {m}");
    }

    #[test]
    fn mask_union_disjoint_regions() {
        // Left circle and right rectangle — no overlap
        let masks = MaskStack {
            masks: vec![
                Mask { feather_inner: 0.0, combine: klipp_core::MaskCombine::Add, name: String::new(), locked: false,
                    shape: MaskShape::Circle { center_x: 0.15, center_y: 0.5, radius: 0.1 },
                    feather: 0.0, invert: false, opacity: 1.0, enabled: true, keyframes: Default::default(),
                },
                Mask { feather_inner: 0.0, combine: klipp_core::MaskCombine::Add, name: String::new(), locked: false,
                    shape: MaskShape::Rectangle {
                        center_x: 0.85, center_y: 0.5, width: 0.2, height: 0.2, rotation: 0.0,
                    },
                    feather: 0.0, invert: false, opacity: 1.0, enabled: true, keyframes: Default::default(),
                },
            ],
        };
        // Inside circle
        let m_circle = sample_matte(&masks, 0.15, 0.5);
        assert!(m_circle > 0.95, "inside circle region: got {m_circle}");
        // Inside rectangle
        let m_rect = sample_matte(&masks, 0.85, 0.5);
        assert!(m_rect > 0.95, "inside rect region: got {m_rect}");
        // Between them
        let m_gap = sample_matte(&masks, 0.5, 0.5);
        assert!(m_gap < 0.05, "gap between shapes: got {m_gap}");
    }

    #[test]
    fn disabled_mask_is_ignored() {
        let masks = MaskStack {
            masks: vec![Mask { feather_inner: 0.0, combine: klipp_core::MaskCombine::Add, name: String::new(), locked: false,
                shape: MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.3 },
                feather: 0.0, invert: false, opacity: 1.0, enabled: false, keyframes: Default::default(),
            }],
        };
        // Everything should be 0 since mask is disabled
        let m = sample_matte(&masks, 0.5, 0.5);
        assert!(m < 0.01, "disabled mask should produce zero matte: got {m}");
    }

    #[test]
    fn empty_mask_stack_produces_zero_matte() {
        let masks = MaskStack { masks: vec![] };
        let matte = generate_mask_matte(10, 10, &masks);
        assert!(matte.iter().all(|&v| v == 0.0), "empty stack should be all zeros");
    }

    // ── Edge cases ───────────────────────────────────────────────────────

    #[test]
    fn bezier_with_fewer_than_3_points_returns_outside() {
        let shape = MaskShape::polygon(vec![(0.5, 0.5), (0.6, 0.6)]);
        let d = sdf(0.5, 0.5, &shape);
        assert!(d > 0.0, "degenerate bezier (2 points) should return outside: got {d}");
    }

    #[test]
    fn linear_gradient_degenerate_zero_length() {
        let shape = MaskShape::LinearGradient {
            start_x: 0.5, start_y: 0.5, end_x: 0.5, end_y: 0.5,
        };
        // Zero-length gradient → returns 0.0
        let d = sdf(0.5, 0.5, &shape);
        assert!(d.abs() < 1e-4, "zero-length gradient should return 0: got {d}");
    }

    #[test]
    fn single_pixel_image() {
        let masks = make_circle_mask(0.5, 0.5, 0.3, 0.0);
        let matte = generate_mask_matte(1, 1, &masks);
        assert_eq!(matte.len(), 1);
        // Single pixel at (0,0) → normalized (0,0), circle at (0.5,0.5) with r=0.3
        // Distance = sqrt(0.25+0.25) - 0.3 = 0.707 - 0.3 = 0.407 > 0 → outside
        assert!(matte[0] < 0.01, "single pixel outside circle: got {}", matte[0]);
    }

    // ── #74: curves, the soft edge inside the line, joining masks ─────

    /// A circle of radius `r` about the middle as four curved points, on a
    /// square frame.
    fn curved_circle(r: f32) -> MaskShape {
        let k = 0.552_284_8 * r;
        let p = |x: f32, y: f32, ix: f32, iy: f32| klipp_core::mask_path::BezierPoint { x: 0.5 + x, y: 0.5 + y, in_dx: ix, in_dy: iy, out_dx: -ix, out_dy: -iy, smooth: true };
        MaskShape::Bezier { points: vec![p(r, 0.0, 0.0, -k), p(0.0, r, k, 0.0), p(-r, 0.0, 0.0, k), p(0.0, -r, -k, 0.0)] }
    }

    fn stack(masks: Vec<Mask>) -> MaskStack {
        MaskStack { masks }
    }

    fn hard(shape: MaskShape) -> Mask {
        Mask { shape, feather: 0.0, ..Mask::default() }
    }

    /// A curved shape is filled to its curve, not its points' polygon: a
    /// pixel on the circle's diagonal just inside it is in, though the
    /// diamond through its four points leaves it out; one just outside
    /// is out.
    #[test]
    fn a_curved_shape_fills_to_its_curve() {
        let n = 201;
        let matte = generate_mask_matte(n, n, &stack(vec![hard(curved_circle(0.3))]));
        let at = |x: f32, y: f32| matte[((y * (n - 1) as f32).round() as u32 * n + (x * (n - 1) as f32).round() as u32) as usize];
        let d = 0.3 / 2f32.sqrt();
        assert_eq!(at(0.5 + d * 0.95, 0.5 + d * 0.95), 1.0, "inside the curve, outside the diamond");
        assert_eq!(at(0.5 + d * 1.06, 0.5 + d * 1.06), 0.0, "outside the curve");
        assert_eq!(at(0.5, 0.5), 1.0);
    }

    /// The soft edge reaches inside the line as far as `feather_inner`:
    /// full well inside, partway at the line, nothing past the outer
    /// feather; with none inside, the line itself is full, as before.
    #[test]
    fn an_inner_feather_softens_inside_the_line() {
        let circle = MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.3 };
        let n = 201;
        let at = |m: &Vec<f32>, x: f32| m[(100 * n + (x * (n - 1) as f32).round() as u32) as usize];
        let soft = generate_mask_matte(n, n, &stack(vec![Mask { feather: 0.05, feather_inner: 0.1, ..hard(circle.clone()) }]));
        assert_eq!(at(&soft, 0.5), 1.0);
        let line = at(&soft, 0.8);
        assert!(line > 0.2 && line < 0.9, "at the line: {line}");
        assert!(at(&soft, 0.75) > line, "further in, more covered");
        assert_eq!(at(&soft, 0.86), 0.0);
        let before = generate_mask_matte(n, n, &stack(vec![Mask { feather: 0.05, ..hard(circle) }]));
        assert_eq!(at(&before, 0.8), 1.0, "no inner feather: the line is full");
        assert_eq!(klipp_core::soft_edge(0.02, 0.0, 0.05), 1.0 - 0.4 * 0.4 * (3.0 - 0.8));
    }

    /// Masks join in order: a subtracted one cuts a hole in what is
    /// there, an intersected one keeps only what both cover, and a stack
    /// that starts by subtracting takes it out of the whole frame.
    #[test]
    fn masks_join_as_their_combine_says() {
        let n = 101;
        let at = |m: &Vec<f32>, x: f32, y: f32| m[((y * (n - 1) as f32).round() as u32 * n + (x * (n - 1) as f32).round() as u32) as usize];
        let rect = hard(MaskShape::Rectangle { center_x: 0.5, center_y: 0.5, width: 0.6, height: 0.6, rotation: 0.0 });
        let small = |cx: f32| MaskShape::Circle { center_x: cx, center_y: 0.5, radius: 0.1 };

        let sub = generate_mask_matte(n, n, &stack(vec![rect.clone(), Mask { combine: klipp_core::MaskCombine::Subtract, ..hard(small(0.5)) }]));
        assert_eq!(at(&sub, 0.5, 0.5), 0.0, "the hole");
        assert_eq!(at(&sub, 0.3, 0.5), 1.0, "the rest of the rectangle");
        assert_eq!(at(&sub, 0.05, 0.05), 0.0);

        let both = generate_mask_matte(n, n, &stack(vec![rect.clone(), Mask { combine: klipp_core::MaskCombine::Intersect, ..hard(small(0.8)) }]));
        assert_eq!(at(&both, 0.75, 0.5), 1.0, "in both");
        assert_eq!(at(&both, 0.85, 0.5), 0.0, "in the circle only");
        assert_eq!(at(&both, 0.3, 0.5), 0.0, "in the rectangle only");

        let first = generate_mask_matte(n, n, &stack(vec![Mask { combine: klipp_core::MaskCombine::Subtract, ..hard(small(0.5)) }]));
        assert_eq!(at(&first, 0.5, 0.5), 0.0);
        assert_eq!(at(&first, 0.05, 0.05), 1.0, "taken out of the whole frame");

        let added = generate_mask_matte(n, n, &stack(vec![rect, hard(small(0.9))]));
        assert_eq!(at(&added, 0.95, 0.5), 1.0, "added, as always");
    }
}
