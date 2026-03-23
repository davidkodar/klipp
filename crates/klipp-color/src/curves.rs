/// Build a 256-entry lookup table from a set of control points using
/// monotone cubic Hermite interpolation (Fritsch-Carlson method).
/// Points are [x, y] pairs in 0..1 range. Returns a [u8; 256] LUT.
pub fn build_curve_lut(points: &[[f32; 2]]) -> [u8; 256] {
    let mut lut = [0u8; 256];

    if points.len() < 2 {
        // Identity: output = input
        for (i, val) in lut.iter_mut().enumerate() {
            *val = i as u8;
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
            let y = lerp_points(&pts, t);
            *val = (y.clamp(0.0, 1.0) * 255.0) as u8;
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

        *lut_val = (y.clamp(0.0, 1.0) * 255.0) as u8;
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

/// Apply curve LUTs to an RGBA buffer.
/// Each channel is looked up independently: r_out = red[master[r_in]], etc.
pub fn apply_curves(
    rgba: &mut [u8],
    master: &[u8; 256],
    red: &[u8; 256],
    green: &[u8; 256],
    blue: &[u8; 256],
) {
    for pixel in rgba.chunks_exact_mut(4) {
        pixel[0] = red[master[pixel[0] as usize] as usize];
        pixel[1] = green[master[pixel[1] as usize] as usize];
        pixel[2] = blue[master[pixel[2] as usize] as usize];
    }
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
    fn apply_curves_identity() {
        let identity: [u8; 256] = std::array::from_fn(|i| i as u8);
        let mut rgba = vec![100, 150, 200, 255];
        apply_curves(&mut rgba, &identity, &identity, &identity, &identity);
        assert_eq!(rgba, vec![100, 150, 200, 255]);
    }

    #[test]
    fn apply_curves_invert() {
        let invert: [u8; 256] = std::array::from_fn(|i| (255 - i) as u8);
        let identity: [u8; 256] = std::array::from_fn(|i| i as u8);
        let mut rgba = vec![100, 150, 200, 255];
        apply_curves(&mut rgba, &invert, &identity, &identity, &identity);
        assert_eq!(rgba[0], 155); // master inverts: 255 - 100 = 155
        assert_eq!(rgba[1], 105); // master inverts: 255 - 150 = 105
        assert_eq!(rgba[2], 55);  // master inverts: 255 - 200 = 55
    }

    #[test]
    fn is_identity_check() {
        assert!(is_identity(&[[0.0, 0.0], [1.0, 1.0]]));
        assert!(!is_identity(&[[0.0, 0.0], [0.5, 0.7], [1.0, 1.0]]));
        assert!(!is_identity(&[[0.0, 0.1], [1.0, 1.0]]));
    }
}
