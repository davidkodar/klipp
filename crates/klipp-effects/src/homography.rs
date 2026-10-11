//! A plane projective map, shared by the rolling-shutter and
//! stabilisation estimators, which fit one to matched points, and the
//! corner pin, which builds one from four corners and resamples through
//! its inverse. Hand-rolled: a 3×3 needs no library.

/// One matched point: where it was, and where it went.
#[derive(Debug, Clone, Copy)]
pub struct Match {
    pub src_x: f64,
    pub src_y: f64,
    pub dst_x: f64,
    pub dst_y: f64,
}

/// A 3×3 projective transform of the plane, `h[2][2]` normalised to 1
/// when fitted.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct Homography {
    pub h: [[f64; 3]; 3],
}

/// Solve `A x = b` by Gaussian elimination with partial pivoting; `None`
/// when the system is singular. One solver for the 8×8 homography and
/// the 6×6 affine fits, which each had a copy.
#[allow(clippy::needless_range_loop)]
pub fn solve<const N: usize>(a: &mut [[f64; N]; N], b: &mut [f64; N]) -> Option<[f64; N]> {
    for col in 0..N {
        let mut max_row = col;
        let mut max_val = a[col][col].abs();
        for row in (col + 1)..N {
            let v = a[row][col].abs();
            if v > max_val {
                max_val = v;
                max_row = row;
            }
        }
        if max_val < 1e-12 {
            return None;
        }
        if max_row != col {
            a.swap(col, max_row);
            b.swap(col, max_row);
        }
        let pivot = a[col][col];
        for row in (col + 1)..N {
            let factor = a[row][col] / pivot;
            for k in col..N {
                a[row][k] -= factor * a[col][k];
            }
            b[row] -= factor * b[col];
        }
    }
    let mut x = [0.0f64; N];
    for col in (0..N).rev() {
        let mut sum = b[col];
        for k in (col + 1)..N {
            sum -= a[col][k] * x[k];
        }
        if a[col][col].abs() < 1e-12 {
            return None;
        }
        x[col] = sum / a[col][col];
    }
    Some(x)
}

impl Homography {
    /// Identity homography.
    pub fn identity() -> Self {
        Self {
            h: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        }
    }

    /// Compute homography from 4 point correspondences using DLT.
    /// Solves 8x8 system (setting `h[2][2] = 1`).
    pub fn from_4_points(src: &[(f64, f64); 4], dst: &[(f64, f64); 4]) -> Option<Self> {
        // Build 8x8 matrix A and 8x1 vector b, where A * [h00,h01,h02,h10,h11,h12,h20,h21]^T = b
        // with h22 = 1.
        //
        // Each correspondence (sx,sy) -> (dx,dy) gives:
        //   h00*sx + h01*sy + h02 - h20*dx*sx - h21*dx*sy = dx
        //   h10*sx + h11*sy + h12 - h20*dy*sx - h21*dy*sy = dy
        let mut a = [[0.0f64; 8]; 8];
        let mut b = [0.0f64; 8];

        for i in 0..4 {
            let (sx, sy) = src[i];
            let (dx, dy) = dst[i];
            let r0 = i * 2;
            let r1 = r0 + 1;

            a[r0] = [sx, sy, 1.0, 0.0, 0.0, 0.0, -dx * sx, -dx * sy];
            b[r0] = dx;

            a[r1] = [0.0, 0.0, 0.0, sx, sy, 1.0, -dy * sx, -dy * sy];
            b[r1] = dy;
        }

        solve(&mut a, &mut b).map(|h_vec| {
            Self {
                h: [
                    [h_vec[0], h_vec[1], h_vec[2]],
                    [h_vec[3], h_vec[4], h_vec[5]],
                    [h_vec[6], h_vec[7], 1.0],
                ],
            }
        })
    }

    /// Compute homography from N point correspondences (N >= 4) using DLT
    /// with least squares (solves overdetermined system via A^T A x = A^T b).
    pub fn from_n_points(matches: &[Match]) -> Option<Self> {
        let n = matches.len();
        if n < 4 {
            return None;
        }
        if n == 4 {
            let src = [
                (matches[0].src_x, matches[0].src_y),
                (matches[1].src_x, matches[1].src_y),
                (matches[2].src_x, matches[2].src_y),
                (matches[3].src_x, matches[3].src_y),
            ];
            let dst = [
                (matches[0].dst_x, matches[0].dst_y),
                (matches[1].dst_x, matches[1].dst_y),
                (matches[2].dst_x, matches[2].dst_y),
                (matches[3].dst_x, matches[3].dst_y),
            ];
            return Self::from_4_points(&src, &dst);
        }

        // Build overdetermined system: 2N x 8, solve via normal equations.
        // A^T A is 8x8, A^T b is 8x1.
        let mut ata = [[0.0f64; 8]; 8];
        let mut atb = [0.0f64; 8];

        for m in matches {
            let (sx, sy) = (m.src_x, m.src_y);
            let (dx, dy) = (m.dst_x, m.dst_y);

            let row0 = [sx, sy, 1.0, 0.0, 0.0, 0.0, -dx * sx, -dx * sy];
            let row1 = [0.0, 0.0, 0.0, sx, sy, 1.0, -dy * sx, -dy * sy];

            // Accumulate A^T A and A^T b for both rows.
            for i in 0..8 {
                for j in 0..8 {
                    ata[i][j] += row0[i] * row0[j] + row1[i] * row1[j];
                }
                atb[i] += row0[i] * dx + row1[i] * dy;
            }
        }

        solve(&mut ata, &mut atb).map(|h_vec| Self {
            h: [
                [h_vec[0], h_vec[1], h_vec[2]],
                [h_vec[3], h_vec[4], h_vec[5]],
                [h_vec[6], h_vec[7], 1.0],
            ],
        })
    }

    /// The homography from the unit square — `(0,0) (1,0) (1,1) (0,1)`,
    /// top-left, top-right, bottom-right, bottom-left — to four corners.
    /// `None` when no map fits; corners that make no quadrilateral
    /// (three in a line, two the same) may instead fit a map that
    /// flattens the plane, which [`Self::inverse`] then refuses.
    pub fn from_unit_square(corners: &[(f64, f64); 4]) -> Option<Self> {
        Self::from_4_points(&[(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)], corners)
    }

    /// The point `(x, y)` maps to, or `None` when it lies on or beyond
    /// the line the map sends to infinity — behind the eye, where a
    /// picture in perspective has no pixels. [`Self::transform`] hands
    /// such a point back unchanged, which suits a motion estimate and
    /// not a resample.
    pub fn map(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        let w = self.h[2][0] * x + self.h[2][1] * y + self.h[2][2];
        if w <= 1e-12 {
            return None;
        }
        Some((
            (self.h[0][0] * x + self.h[0][1] * y + self.h[0][2]) / w,
            (self.h[1][0] * x + self.h[1][1] * y + self.h[1][2]) / w,
        ))
    }

    /// Apply the homography to a point (x, y) -> (x', y').
    pub fn transform(&self, x: f64, y: f64) -> (f64, f64) {
        let w = self.h[2][0] * x + self.h[2][1] * y + self.h[2][2];
        if w.abs() < 1e-12 {
            return (x, y); // Degenerate, return original
        }
        let xp = (self.h[0][0] * x + self.h[0][1] * y + self.h[0][2]) / w;
        let yp = (self.h[1][0] * x + self.h[1][1] * y + self.h[1][2]) / w;
        (xp, yp)
    }

    /// Linearly interpolate this homography toward the identity matrix.
    /// At t=0 returns identity, at t=1 returns self.
    pub fn interpolate_toward_identity(&self, t: f64) -> Homography {
        let id = Self::identity();
        let mut result = [[0.0f64; 3]; 3];
        for (res_row, (id_row, self_row)) in result
            .iter_mut()
            .zip(id.h.iter().zip(self.h.iter()))
        {
            for (res_elem, (id_elem, self_elem)) in
                res_row.iter_mut().zip(id_row.iter().zip(self_row.iter()))
            {
                *res_elem = id_elem * (1.0 - t) + self_elem * t;
            }
        }
        Homography { h: result }
    }

    /// Matrix multiplication: self * other (apply other first, then self).
    pub fn multiply(&self, other: &Homography) -> Homography {
        let mut result = [[0.0f64; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                result[i][j] = self.h[i][0] * other.h[0][j]
                    + self.h[i][1] * other.h[1][j]
                    + self.h[i][2] * other.h[2][j];
            }
        }
        Homography { h: result }
    }

    /// Compute the inverse homography using the adjugate / Cramer's rule for 3x3.
    pub fn inverse(&self) -> Option<Homography> {
        let m = &self.h;

        let minor00 = m[1][1] * m[2][2] - m[1][2] * m[2][1];
        let minor01 = m[1][0] * m[2][2] - m[1][2] * m[2][0];
        let minor02 = m[1][0] * m[2][1] - m[1][1] * m[2][0];

        let det = m[0][0] * minor00 - m[0][1] * minor01 + m[0][2] * minor02;
        if det.abs() < 1e-12 {
            return None;
        }

        let inv_det = 1.0 / det;

        let mut inv = [[0.0f64; 3]; 3];
        inv[0][0] = minor00 * inv_det;
        inv[0][1] = (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * inv_det;
        inv[0][2] = (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * inv_det;
        inv[1][0] = -minor01 * inv_det;
        inv[1][1] = (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * inv_det;
        inv[1][2] = (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * inv_det;
        inv[2][0] = minor02 * inv_det;
        inv[2][1] = (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * inv_det;
        inv[2][2] = (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * inv_det;

        Some(Homography { h: inv })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9
    }

    /// The unit square onto itself is the identity, and onto a
    /// quadrilateral each corner lands where it was sent, with the
    /// inverse taking it back.
    #[test]
    fn four_corners_land_where_they_are_sent_and_come_back() {
        let id = Homography::from_unit_square(&[(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]).unwrap();
        for (i, row) in id.h.iter().enumerate() {
            for (j, v) in row.iter().enumerate() {
                assert!((v - if i == j { 1.0 } else { 0.0 }).abs() < 1e-12, "{:?}", id.h);
            }
        }
        let quad = [(0.1, 0.2), (0.9, 0.1), (0.7, 0.8), (0.2, 0.95)];
        let h = Homography::from_unit_square(&quad).unwrap();
        let inv = h.inverse().unwrap();
        for (src, dst) in [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)].iter().zip(&quad) {
            assert!(near(h.map(src.0, src.1).unwrap(), *dst));
            assert!(near(inv.map(dst.0, dst.1).unwrap(), *src));
        }
        let mid = h.map(0.5, 0.5).unwrap();
        assert!(near(inv.map(mid.0, mid.1).unwrap(), (0.5, 0.5)));
    }

    /// Three corners in a line make no quadrilateral: the fit is
    /// refused, or flattens the plane and cannot be inverted — either
    /// way nothing can be resampled through it.
    #[test]
    fn a_degenerate_quadrilateral_cannot_be_inverted() {
        for corners in [[(0.0, 0.0), (0.5, 0.0), (1.0, 0.0), (0.0, 1.0)], [(0.3, 0.3); 4]] {
            assert!(Homography::from_unit_square(&corners).and_then(|h| h.inverse()).is_none(), "{corners:?}");
        }
    }

    /// A trapezoid narrowing downward is a floor seen from above: far
    /// enough down, the map crosses its horizon and there is nothing to
    /// see. `map` says so; `transform` hands the point back.
    #[test]
    fn beyond_the_horizon_there_is_nothing_to_sample() {
        let h = Homography::from_unit_square(&[(0.0, 0.0), (1.0, 0.0), (0.6, 0.5), (0.4, 0.5)]).unwrap();
        let inv = h.inverse().unwrap();
        assert!(inv.map(0.5, 0.25).is_some(), "inside the trapezoid");
        assert!(inv.map(0.5, 5.0).is_none(), "far below its horizon");
        assert!(inv.transform(0.5, 5.0).0.is_finite());
    }

    /// The one solver serves both sizes.
    #[test]
    fn the_solver_solves_small_and_singular_systems() {
        let mut a = [[2.0, 1.0], [1.0, 3.0]];
        let mut b = [5.0, 10.0];
        let x = solve(&mut a, &mut b).unwrap();
        assert!((x[0] - 1.0).abs() < 1e-12 && (x[1] - 3.0).abs() < 1e-12);
        let mut a = [[1.0, 2.0], [2.0, 4.0]];
        let mut b = [1.0, 2.0];
        assert!(solve(&mut a, &mut b).is_none());
    }
}
