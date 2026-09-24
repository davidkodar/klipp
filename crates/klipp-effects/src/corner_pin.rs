//! The corner pin's geometry, shared by the CPU and GPU resamplers and
//! by the viewer's handles: the four corners, the inverse map an output
//! pixel is sampled through, and what "still" means.

use crate::homography::Homography;
use crate::BuiltinEffect;

/// The four corners of the picture as it is: top-left, top-right,
/// bottom-right, bottom-left, as fractions of the frame.
pub const STILL: [[f32; 2]; 4] = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];

/// The parameter names the corners are keyed under, in corner order:
/// `tl_x`, `tl_y`, `tr_x`, … .
pub const PARAMS: [[&str; 2]; 4] = [["tl_x", "tl_y"], ["tr_x", "tr_y"], ["br_x", "br_y"], ["bl_x", "bl_y"]];

/// A pin that leaves the picture where it is.
pub fn still() -> BuiltinEffect {
    BuiltinEffect::CornerPin { tl: STILL[0], tr: STILL[1], br: STILL[2], bl: STILL[3] }
}

/// The pin's corners in order, or `None` for another effect.
pub fn corners(effect: &BuiltinEffect) -> Option<[[f32; 2]; 4]> {
    match effect {
        BuiltinEffect::CornerPin { tl, tr, br, bl } => Some([*tl, *tr, *br, *bl]),
        _ => None,
    }
}

/// A pin from corners in order.
pub fn with_corners(c: [[f32; 2]; 4]) -> BuiltinEffect {
    BuiltinEffect::CornerPin { tl: c[0], tr: c[1], br: c[2], bl: c[3] }
}

/// Whether the corners are the picture's own, to within a hair.
pub fn is_still(corners: &[[f32; 2]; 4]) -> bool {
    corners.iter().zip(&STILL).all(|(c, s)| (c[0] - s[0]).abs() < 1e-6 && (c[1] - s[1]).abs() < 1e-6)
}

/// The map an output pixel is sampled through: from a pixel of the
/// frame back to the pixel of the picture that lands there, as nine
/// `f32` coefficients, row-major, with the perspective row signed so
/// that a positive denominator means "in front of the eye". The CPU
/// and the GPU both resample with exactly these numbers, so they agree
/// to the bit of the arithmetic rather than to the precision of two
/// derivations. `None` when the corners make no quadrilateral.
pub fn inverse_map(width: u32, height: u32, corners: &[[f32; 2]; 4]) -> Option<[f32; 9]> {
    let (w, h) = (width as f64, height as f64);
    let dst: [(f64, f64); 4] = std::array::from_fn(|i| (corners[i][0] as f64 * w, corners[i][1] as f64 * h));
    let forward = Homography::from_4_points(&[(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)], &dst)?;
    let inverse = forward.inverse()?;
    // The four corners' centre is inside any convex quadrilateral; the
    // sign of the denominator there is the sign of "in front".
    let (cx, cy) = dst.iter().fold((0.0, 0.0), |acc, p| (acc.0 + p.0 * 0.25, acc.1 + p.1 * 0.25));
    let wc = inverse.h[2][0] * cx + inverse.h[2][1] * cy + inverse.h[2][2];
    if wc.abs() < 1e-12 {
        return None;
    }
    let sign = wc.signum();
    let mut out = [0.0f32; 9];
    for (i, v) in inverse.h.iter().flatten().enumerate() {
        out[i] = (v * sign) as f32;
    }
    Some(out)
}

/// Where the pixel at `(x, y)` of the frame samples the picture, or
/// `None` behind the eye. The same arithmetic as the shader's, in
/// `f32`.
pub fn sample_point(inv: &[f32; 9], x: f32, y: f32) -> Option<(f32, f32)> {
    let w = inv[6] * x + inv[7] * y + inv[8];
    if w <= 1e-6 {
        return None;
    }
    Some(((inv[0] * x + inv[1] * y + inv[2]) / w, (inv[3] * x + inv[4] * y + inv[5]) / w))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The picture as it is samples every pixel from itself.
    #[test]
    fn the_still_pin_is_the_identity_map() {
        assert!(is_still(&STILL));
        assert!(crate::is_identity_effect(&still()));
        let inv = inverse_map(160, 90, &STILL).unwrap();
        for (x, y) in [(0.0, 0.0), (37.0, 12.0), (159.0, 89.0)] {
            let (sx, sy) = sample_point(&inv, x, y).unwrap();
            assert!((sx - x).abs() < 1e-4 && (sy - y).abs() < 1e-4, "({x},{y}) sampled from ({sx},{sy})");
        }
    }

    /// Pinned into the top-left quarter, the frame's quarter point is
    /// the picture's centre, and the far corner sees nothing of it.
    #[test]
    fn a_quarter_pin_samples_the_picture_at_double_scale() {
        let corners = [[0.0, 0.0], [0.5, 0.0], [0.5, 0.5], [0.0, 0.5]];
        let inv = inverse_map(200, 100, &corners).unwrap();
        let (sx, sy) = sample_point(&inv, 50.0, 25.0).unwrap();
        assert!((sx - 100.0).abs() < 1e-3 && (sy - 50.0).abs() < 1e-3, "({sx},{sy})");
        let (sx, sy) = sample_point(&inv, 150.0, 75.0).unwrap();
        assert!(sx > 200.0 && sy > 100.0, "outside the picture: ({sx},{sy})");
    }

    /// A trapezoid is a surface in perspective: beyond its horizon the
    /// map says there is nothing, whichever way round the fit came out.
    #[test]
    fn beyond_the_horizon_is_behind_the_eye() {
        let corners = [[0.0, 0.0], [1.0, 0.0], [0.6, 0.5], [0.4, 0.5]];
        let inv = inverse_map(100, 100, &corners).unwrap();
        assert!(sample_point(&inv, 50.0, 25.0).is_some());
        assert!(sample_point(&inv, 50.0, 500.0).is_none());
        let flipped = [[0.4, 0.5], [0.6, 0.5], [1.0, 0.0], [0.0, 0.0]];
        let inv = inverse_map(100, 100, &flipped).unwrap();
        assert!(sample_point(&inv, 50.0, 25.0).is_some(), "the fit's sign does not decide what is in front");
    }

    /// No quadrilateral, no map.
    #[test]
    fn collapsed_corners_make_no_map() {
        assert!(inverse_map(100, 100, &[[0.2, 0.2]; 4]).is_none());
        assert!(inverse_map(100, 100, &[[0.0, 0.0], [0.5, 0.0], [1.0, 0.0], [0.0, 1.0]]).is_none());
    }
}
