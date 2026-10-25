/// [`build_curve_lut_f32`] quantised to 8 bits, for drawing the curve
/// in the inspector's editor. The grade reads the float table.
pub fn build_curve_lut(points: &[[f32; 2]]) -> [u8; 256] {
    build_curve_lut_f32(points).map(|v| (v * 255.0) as u8)
}

/// Build a 256-entry float lookup table from a set of control points
/// using monotone cubic Hermite interpolation (Fritsch-Carlson method).
/// Points are [x, y] pairs in 0..1 range; the output is clamped there.
///
/// Float, not 8-bit: the grade reads these tables per channel in float,
/// and 1/255 steps both banded smooth gradients and — since the curve's
/// slope at its ends is measured from the last two entries, to continue
/// it past them — scaled highlights above white by up to half a step.
pub fn build_curve_lut_f32(points: &[[f32; 2]]) -> [f32; 256] {
    let mut lut = [0.0f32; 256];

    if points.len() < 2 {
        // Identity: output = input
        for (i, val) in lut.iter_mut().enumerate() {
            *val = i as f32 / 255.0;
        }
        return lut;
    }

    // Sort points by x (should already be sorted, but be safe)
    let mut pts: Vec<[f32; 2]> = points.to_vec();
    pts.sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap_or(std::cmp::Ordering::Equal));

    let n = pts.len();

    if n == 2 {
        // Linear interpolation
        for (i, val) in lut.iter_mut().enumerate() {
            let t = i as f32 / 255.0;
            *val = lerp_points(&pts, t).clamp(0.0, 1.0);
        }
        return lut;
    }

    // Compute slopes (secants between adjacent points)
    let mut delta = vec![0.0f32; n - 1];
    for i in 0..n - 1 {
        let dx = pts[i + 1][0] - pts[i][0];
        if dx.abs() > f32::EPSILON {
            delta[i] = (pts[i + 1][1] - pts[i][1]) / dx;
        }
    }

    // Initialize tangents
    let mut m = vec![0.0f32; n];
    m[0] = delta[0];
    m[n - 1] = delta[n - 2];
    for i in 1..n - 1 {
        if delta[i - 1].signum() != delta[i].signum() {
            m[i] = 0.0;
        } else {
            m[i] = (delta[i - 1] + delta[i]) / 2.0;
        }
    }

    // Fritsch-Carlson monotonicity correction
    for i in 0..n - 1 {
        if delta[i].abs() < f32::EPSILON {
            m[i] = 0.0;
            m[i + 1] = 0.0;
        } else {
            let alpha = m[i] / delta[i];
            let beta = m[i + 1] / delta[i];
            let tau = alpha * alpha + beta * beta;
            if tau > 9.0 {
                let s = 3.0 / tau.sqrt();
                m[i] = s * alpha * delta[i];
                m[i + 1] = s * beta * delta[i];
            }
        }
    }

    // Evaluate spline at each of 256 values
    for (i, lut_val) in lut.iter_mut().enumerate() {
        let t = i as f32 / 255.0;

        // Beyond the outer points the curve holds their values, as the
        // two-point line does; the end pieces' cubic run on past them
        // bent every value there (#2554).
        if t <= pts[0][0] {
            *lut_val = pts[0][1].clamp(0.0, 1.0);
            continue;
        }
        if t >= pts[n - 1][0] {
            *lut_val = pts[n - 1][1].clamp(0.0, 1.0);
            continue;
        }

        // Find segment
        let mut seg = 0;
        for (j, pt) in pts[..n - 1].iter().enumerate() {
            if t >= pt[0] {
                seg = j;
            }
        }
        if seg >= n - 1 {
            seg = n - 2;
        }

        let x0 = pts[seg][0];
        let x1 = pts[seg + 1][0];
        let y0 = pts[seg][1];
        let y1 = pts[seg + 1][1];
        let dx = x1 - x0;

        let y = if dx.abs() < f32::EPSILON {
            y0
        } else {
            let s = (t - x0) / dx;
            let s2 = s * s;
            let s3 = s2 * s;
            let h00 = 2.0 * s3 - 3.0 * s2 + 1.0;
            let h10 = s3 - 2.0 * s2 + s;
            let h01 = -2.0 * s3 + 3.0 * s2;
            let h11 = s3 - s2;
            h00 * y0 + h10 * dx * m[seg] + h01 * y1 + h11 * dx * m[seg + 1]
        };

        *lut_val = y.clamp(0.0, 1.0);
    }

    lut
}

fn lerp_points(pts: &[[f32; 2]], t: f32) -> f32 {
    if t <= pts[0][0] { return pts[0][1]; }
    if t >= pts[pts.len() - 1][0] { return pts[pts.len() - 1][1]; }
    for i in 0..pts.len() - 1 {
        if t >= pts[i][0] && t <= pts[i + 1][0] {
            let dx = pts[i + 1][0] - pts[i][0];
            if dx.abs() < f32::EPSILON { return pts[i][1]; }
            let s = (t - pts[i][0]) / dx;
            return pts[i][1] + s * (pts[i + 1][1] - pts[i][1]);
        }
    }
    pts[pts.len() - 1][1]
}

/// Check if curve points represent identity (no adjustment).
pub fn is_identity(points: &[[f32; 2]]) -> bool {
    points.len() == 2
        && (points[0][0] - 0.0).abs() < 0.001
        && (points[0][1] - 0.0).abs() < 0.001
        && (points[1][0] - 1.0).abs() < 0.001
        && (points[1][1] - 1.0).abs() < 0.001
}

#[cfg(test)]
mod float_lut_tests {
    use super::*;

    /// The tables carry more than 8 bits: a shallow curve's entries do
    /// not all land on the 1/255 grid, so a smooth gradient through a
    /// curve does not band.
    #[test]
    fn the_tables_are_not_quantised_to_8_bits() {
        let lut = build_curve_lut_f32(&[[0.0, 0.0], [0.5, 0.502], [1.0, 1.0]]);
        let off_grid = lut
            .iter()
            .filter(|v| {
                let steps = *v * 255.0;
                (steps - steps.round()).abs() > 0.02
            })
            .count();
        assert!(off_grid > 200, "only {off_grid} of 256 entries carry sub-8-bit detail");
    }

    /// The slope a curve ends with survives the table, because the grade
    /// reads it to continue the curve past white. Through 8 bits, a curve
    /// ending at half slope measured 0 or 1 instead of 0.5.
    #[test]
    fn an_end_slope_survives_the_table() {
        let lut = build_curve_lut_f32(&[[0.0, 0.0], [1.0, 0.5]]);
        let slope = (lut[255] - lut[254]) * 255.0;
        assert!((slope - 0.5).abs() < 0.02, "end slope {slope}, want 0.5");
    }

    /// An identity curve is still identity, to float precision.
    #[test]
    fn identity_stays_identity() {
        let lut = build_curve_lut_f32(&[[0.0, 0.0], [1.0, 1.0]]);
        for (i, v) in lut.iter().enumerate() {
            assert!((v - i as f32 / 255.0).abs() < 1e-6, "entry {i} is {v}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_curve() {
        let pts = vec![[0.0, 0.0], [1.0, 1.0]];
        let lut = build_curve_lut(&pts);
        for i in 0..256 {
            assert!((lut[i] as i16 - i as i16).unsigned_abs() <= 1,
                "identity lut[{i}] = {}, expected {i}", lut[i]);
        }
    }

    #[test]
    fn s_curve_increases_contrast() {
        let pts = vec![[0.0, 0.0], [0.25, 0.15], [0.75, 0.85], [1.0, 1.0]];
        let lut = build_curve_lut(&pts);
        // Shadows should be darker than identity
        assert!(lut[64] < 64, "shadows should be darker: lut[64] = {}", lut[64]);
        // Highlights should be brighter than identity
        assert!(lut[192] > 192, "highlights should be brighter: lut[192] = {}", lut[192]);
    }

    #[test]
    fn is_identity_check() {
        assert!(is_identity(&[[0.0, 0.0], [1.0, 1.0]]));
        assert!(!is_identity(&[[0.0, 0.0], [0.5, 0.7], [1.0, 1.0]]));
        assert!(!is_identity(&[[0.0, 0.1], [1.0, 1.0]]));
    }

    /// #2554: a hue curve picked to three points round a hue, its middle
    /// dragged up, changes only the hues between the outer two; beyond
    /// them the curve holds the outer points' values, here neutral. The
    /// end pieces' cubic used to run on past them, and every other hue
    /// gained saturation.
    #[test]
    fn beyond_the_outer_points_the_curve_holds_their_values() {
        let (lo, hue, hi) = (200.0 / 360.0 - 0.06, 200.0 / 360.0, 200.0 / 360.0 + 0.06);
        let lut = build_curve_lut_f32(&[[lo, 0.5], [hue, 0.9], [hi, 0.5]]);
        for (i, v) in lut.iter().enumerate() {
            let t = i as f32 / 255.0;
            if t < lo || t > hi {
                assert!((v - 0.5).abs() < 1e-4, "at {:.0}° the curve is {v}, not the neutral 0.5", t * 360.0);
            }
        }
        assert!(lut[(hue * 255.0).round() as usize] > 0.85, "the picked hue is raised");
    }
}
