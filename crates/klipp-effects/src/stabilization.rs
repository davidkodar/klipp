//! Video stabilization — Phase 1: Trajectory analysis and smoothing.
//!
//! Analyzes frame-to-frame motion using affine-only RANSAC (3-point, 6 DOF),
//! accumulates absolute trajectory, and applies Gaussian kernel smoothing to
//! produce per-frame correction transforms.

use crate::rolling_shutter::{self, Homography, Match};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Decomposed 2D motion parameters from an affine transform.
#[derive(Debug, Clone, Copy)]
pub struct MotionParams {
    pub tx: f64,
    pub ty: f64,
    pub rotation: f64, // radians
    pub scale: f64,
}

/// Raw analysis: per-frame affine transforms for the entire clip.
#[derive(Debug, Clone)]
pub struct StabilizationData {
    /// Frame-to-frame affine transforms. Length = frame_count - 1.
    pub frame_transforms: Vec<Homography>,
    /// Accumulated absolute trajectory. Length = frame_count. accumulated[0] = identity.
    pub accumulated: Vec<Homography>,
    /// Decomposed motion params for each accumulated frame.
    pub trajectory: Vec<MotionParams>,
    pub fps: f64,
    pub frame_count: usize,
}

/// Smoothed result ready for playback correction.
#[derive(Debug, Clone)]
pub struct SmoothedTrajectory {
    pub smoothed: Vec<MotionParams>,
    /// correction[i] = recompose(smoothed[i]) * inverse(accumulated[i])
    pub corrections: Vec<Homography>,
    /// Tracks which smoothing window produced this (for lazy re-smooth detection).
    pub smoothing_window_secs: f64,
}

// ---------------------------------------------------------------------------
// LCG RNG (reuses the same algorithm as rolling_shutter)
// ---------------------------------------------------------------------------

fn lcg_next(state: &mut u64) -> u64 {
    rolling_shutter::lcg_next(state)
}

// ---------------------------------------------------------------------------
// Gaussian elimination solver for NxN (N=6)
// ---------------------------------------------------------------------------

/// Solve a 6x6 linear system via Gaussian elimination with partial pivoting.
/// Returns None if the system is singular.
#[allow(clippy::needless_range_loop)]
fn solve_6x6(a: &mut [[f64; 6]; 6], b: &mut [f64; 6]) -> Option<[f64; 6]> {
    // Forward elimination
    for col in 0..6 {
        // Partial pivoting
        let mut max_row = col;
        let mut max_val = a[col][col].abs();
        for row in (col + 1)..6 {
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
        for row in (col + 1)..6 {
            let factor = a[row][col] / pivot;
            for k in col..6 {
                a[row][k] -= factor * a[col][k];
            }
            b[row] -= factor * b[col];
        }
    }

    // Back substitution
    let mut x = [0.0f64; 6];
    for col in (0..6).rev() {
        let mut sum = b[col];
        for k in (col + 1)..6 {
            sum -= a[col][k] * x[k];
        }
        if a[col][col].abs() < 1e-12 {
            return None;
        }
        x[col] = sum / a[col][col];
    }
    Some(x)
}

// ---------------------------------------------------------------------------
// Affine fitting
// ---------------------------------------------------------------------------

/// Fit an affine transform from exactly 3 point correspondences.
///
/// Solves:
///   dx = a*sx + b*sy + tx
///   dy = c*sx + d*sy + ty
///
/// Returns a Homography with h[2] = [0, 0, 1].
fn fit_affine_3pt(matches: &[Match]) -> Option<Homography> {
    debug_assert!(matches.len() >= 3);

    // Build 6x6 system:
    // [sx0  sy0  1  0    0    0] [a ]   [dx0]
    // [0    0    0  sx0  sy0  1] [b ]   [dy0]
    // [sx1  sy1  1  0    0    0] [tx] = [dx1]
    // [0    0    0  sx1  sy1  1] [c ]   [dy1]
    // [sx2  sy2  1  0    0    0] [d ]   [dx2]
    // [0    0    0  sx2  sy2  1] [ty]   [dy2]
    let mut a = [[0.0f64; 6]; 6];
    let mut b = [0.0f64; 6];

    for (i, m) in matches[..3].iter().enumerate() {
        let r0 = i * 2;
        let r1 = r0 + 1;

        a[r0] = [m.src_x, m.src_y, 1.0, 0.0, 0.0, 0.0];
        b[r0] = m.dst_x;

        a[r1] = [0.0, 0.0, 0.0, m.src_x, m.src_y, 1.0];
        b[r1] = m.dst_y;
    }

    solve_6x6(&mut a, &mut b).map(|x| Homography {
        h: [
            [x[0], x[1], x[2]], // a, b, tx
            [x[3], x[4], x[5]], // c, d, ty
            [0.0, 0.0, 1.0],
        ],
    })
}

/// Fit an affine transform from N correspondences via least squares.
///
/// Builds the 2N x 6 matrix A and 2N x 1 vector b, then solves the
/// normal equations A^T*A*x = A^T*b via the 6x6 solver.
fn fit_affine_lstsq(matches: &[Match]) -> Option<Homography> {
    let n = matches.len();
    if n < 3 {
        return None;
    }
    if n == 3 {
        return fit_affine_3pt(matches);
    }

    // Accumulate A^T*A (6x6) and A^T*b (6x1) directly.
    let mut ata = [[0.0f64; 6]; 6];
    let mut atb = [0.0f64; 6];

    for m in matches {
        let (sx, sy) = (m.src_x, m.src_y);
        let (dx, dy) = (m.dst_x, m.dst_y);

        let row0 = [sx, sy, 1.0, 0.0, 0.0, 0.0];
        let row1 = [0.0, 0.0, 0.0, sx, sy, 1.0];

        for i in 0..6 {
            for j in 0..6 {
                ata[i][j] += row0[i] * row0[j] + row1[i] * row1[j];
            }
            atb[i] += row0[i] * dx + row1[i] * dy;
        }
    }

    solve_6x6(&mut ata, &mut atb).map(|x| Homography {
        h: [
            [x[0], x[1], x[2]],
            [x[3], x[4], x[5]],
            [0.0, 0.0, 1.0],
        ],
    })
}

// ---------------------------------------------------------------------------
// RANSAC affine
// ---------------------------------------------------------------------------

/// Affine-only RANSAC: picks 3 random correspondences, fits a 6-DOF affine,
/// counts inliers, refits with all inliers of the best model.
pub fn ransac_affine(
    matches: &[Match],
    iterations: u32,
    inlier_threshold: f64,
    min_inliers: usize,
) -> Option<Homography> {
    if matches.len() < 3 {
        return None;
    }

    let n = matches.len();
    let mut rng_state: u64 = 0xDEAD_BEEF_CAFE_1234;
    let mut best_inlier_count = 0usize;
    let mut best_inliers: Vec<bool> = vec![false; n];
    let threshold_sq = inlier_threshold * inlier_threshold;

    for _ in 0..iterations {
        // Pick 3 distinct random indices.
        let mut indices = [0usize; 3];
        let mut i = 0;
        while i < 3 {
            let idx = (lcg_next(&mut rng_state) % n as u64) as usize;
            if indices[..i].contains(&idx) {
                continue;
            }
            indices[i] = idx;
            i += 1;
        }

        let sample = [matches[indices[0]], matches[indices[1]], matches[indices[2]]];
        let model = match fit_affine_3pt(&sample) {
            Some(m) => m,
            None => continue,
        };

        // Count inliers.
        let mut inlier_count = 0usize;
        let mut current_inliers = vec![false; n];
        for (k, m) in matches.iter().enumerate() {
            let (px, py) = model.transform(m.src_x, m.src_y);
            let dx = px - m.dst_x;
            let dy = py - m.dst_y;
            if dx * dx + dy * dy < threshold_sq {
                current_inliers[k] = true;
                inlier_count += 1;
            }
        }

        if inlier_count > best_inlier_count {
            best_inlier_count = inlier_count;
            best_inliers = current_inliers;
        }
    }

    if best_inlier_count < min_inliers {
        return None;
    }

    // Refit with all inliers of the best model.
    let inlier_matches: Vec<Match> = matches
        .iter()
        .zip(best_inliers.iter())
        .filter_map(|(m, &is_inlier)| if is_inlier { Some(*m) } else { None })
        .collect();

    fit_affine_lstsq(&inlier_matches)
}

// ---------------------------------------------------------------------------
// Trajectory accumulation and decomposition
// ---------------------------------------------------------------------------

/// Accumulate frame-to-frame transforms into absolute trajectory.
/// Returns a vector of length `frame_transforms.len() + 1`,
/// where `accumulated[0]` is identity.
pub fn accumulate_trajectory(frame_transforms: &[Homography]) -> Vec<Homography> {
    let mut acc = Vec::with_capacity(frame_transforms.len() + 1);
    acc.push(Homography::identity());
    for h in frame_transforms {
        acc.push(h.multiply(acc.last().unwrap()));
    }
    acc
}

/// Decompose an affine homography into translation, rotation, and scale.
pub fn decompose_homography(h: &Homography) -> MotionParams {
    let s = if h.h[2][2].abs() > 1e-10 {
        1.0 / h.h[2][2]
    } else {
        1.0
    };
    let a = h.h[0][0] * s;
    let b = h.h[0][1] * s;
    let c = h.h[1][0] * s;
    let d = h.h[1][1] * s;
    let tx = h.h[0][2] * s;
    let ty = h.h[1][2] * s;

    let scale_x = (a * a + c * c).sqrt();
    let scale_y = (b * b + d * d).sqrt();
    let scale = (scale_x + scale_y) / 2.0;
    let rotation = c.atan2(a);

    MotionParams {
        tx,
        ty,
        rotation,
        scale,
    }
}

/// Recompose a Homography from decomposed motion parameters.
pub fn recompose_homography(p: &MotionParams) -> Homography {
    let cos_r = p.rotation.cos();
    let sin_r = p.rotation.sin();
    Homography {
        h: [
            [p.scale * cos_r, -p.scale * sin_r, p.tx],
            [p.scale * sin_r, p.scale * cos_r, p.ty],
            [0.0, 0.0, 1.0],
        ],
    }
}

// ---------------------------------------------------------------------------
// Gaussian smoothing
// ---------------------------------------------------------------------------

/// Apply 1D Gaussian smoothing to a signal with boundary clamping.
fn gaussian_smooth_1d(values: &[f64], radius: usize) -> Vec<f64> {
    let n = values.len();
    if n == 0 {
        return vec![];
    }
    if radius == 0 {
        return values.to_vec();
    }

    let sigma = (radius as f64 / 3.0).max(1.0);
    let kernel_size = 2 * radius + 1;
    let kernel: Vec<f64> = (0..kernel_size)
        .map(|i| {
            let x = i as f64 - radius as f64;
            (-0.5 * (x / sigma).powi(2)).exp()
        })
        .collect();
    let sum: f64 = kernel.iter().sum();
    let kernel: Vec<f64> = kernel.iter().map(|k| k / sum).collect();

    let result: Vec<f64> = (0..n)
        .map(|i| {
            kernel
                .iter()
                .enumerate()
                .map(|(j, &kval)| {
                    let idx =
                        (i as i64 + j as i64 - radius as i64).clamp(0, n as i64 - 1) as usize;
                    values[idx] * kval
                })
                .sum()
        })
        .collect();
    result
}

/// Smooth the accumulated trajectory using a Gaussian kernel.
pub fn smooth_trajectory(
    data: &StabilizationData,
    smoothing_window_secs: f64,
) -> SmoothedTrajectory {
    let radius = (smoothing_window_secs * data.fps / 2.0).round().max(1.0) as usize;

    // Extract component arrays from trajectory.
    let tx_vals: Vec<f64> = data.trajectory.iter().map(|p| p.tx).collect();
    let ty_vals: Vec<f64> = data.trajectory.iter().map(|p| p.ty).collect();
    let rot_vals: Vec<f64> = data.trajectory.iter().map(|p| p.rotation).collect();
    let scale_vals: Vec<f64> = data.trajectory.iter().map(|p| p.scale).collect();

    // Smooth each component independently.
    let smooth_tx = gaussian_smooth_1d(&tx_vals, radius);
    let smooth_ty = gaussian_smooth_1d(&ty_vals, radius);
    let smooth_rot = gaussian_smooth_1d(&rot_vals, radius);
    let smooth_scale = gaussian_smooth_1d(&scale_vals, radius);

    // Reconstruct smoothed params and compute corrections.
    let n = data.trajectory.len();
    let mut smoothed = Vec::with_capacity(n);
    let mut corrections = Vec::with_capacity(n);

    for i in 0..n {
        let sp = MotionParams {
            tx: smooth_tx[i],
            ty: smooth_ty[i],
            rotation: smooth_rot[i],
            scale: smooth_scale[i],
        };
        smoothed.push(sp);

        let smooth_h = recompose_homography(&sp);
        let correction = match data.accumulated[i].inverse() {
            Some(inv) => smooth_h.multiply(&inv),
            None => Homography::identity(),
        };
        corrections.push(correction);
    }

    SmoothedTrajectory {
        smoothed,
        corrections,
        smoothing_window_secs,
    }
}

// ---------------------------------------------------------------------------
// Full clip analysis
// ---------------------------------------------------------------------------

/// Analyze a clip's motion trajectory.
///
/// `decode_frame` callback: given a time in seconds, returns (rgba, width, height)
/// or None if the frame could not be decoded.
///
/// `progress_callback`: called with progress 0.0..1.0; return false to cancel.
pub fn analyze_clip(
    fps: f64,
    frame_count: usize,
    decode_frame: &mut dyn FnMut(f64) -> Option<(Vec<u8>, u32, u32)>,
    progress_callback: &dyn Fn(f32) -> bool,
) -> Result<StabilizationData, String> {
    if frame_count < 2 {
        return Err("need at least 2 frames".to_string());
    }

    // Decode frame 0.
    let (rgba0, w0, h0) = decode_frame(0.0).ok_or("failed to decode frame 0")?;
    let (gray0, gw, gh) = rolling_shutter::to_gray_half(&rgba0, w0, h0);
    let corners0 = rolling_shutter::fast9_detect(&gray0, gw, gh, 20, 500);

    let mut prev_gray = gray0;
    let mut prev_corners = corners0;
    let mut frame_transforms = Vec::with_capacity(frame_count - 1);

    for f in 1..frame_count {
        let time = f as f64 / fps;
        let (rgba, w, h) = decode_frame(time).ok_or_else(|| format!("failed to decode frame {f}"))?;
        let (gray, gw_cur, gh_cur) = rolling_shutter::to_gray_half(&rgba, w, h);
        let corners = rolling_shutter::fast9_detect(&gray, gw_cur, gh_cur, 20, 500);

        // Match patches between previous and current frame.
        let matches = rolling_shutter::match_patches(
            &prev_gray,
            &prev_corners,
            &gray,
            &corners,
            gw_cur,
            gh_cur,
            4, // patch_half
            16, // search_radius
        );

        // RANSAC affine — if it fails, use identity (static frame).
        let transform = ransac_affine(&matches, 200, 2.0, 15)
            .unwrap_or_else(Homography::identity);

        frame_transforms.push(transform);

        // Progress callback.
        let progress = f as f32 / (frame_count - 1) as f32;
        if !progress_callback(progress) {
            return Err("cancelled".to_string());
        }

        prev_gray = gray;
        prev_corners = corners;
    }

    // Accumulate trajectory.
    let accumulated = accumulate_trajectory(&frame_transforms);

    // Decompose each accumulated frame.
    let trajectory: Vec<MotionParams> = accumulated.iter().map(decompose_homography).collect();

    Ok(StabilizationData {
        frame_transforms,
        accumulated,
        trajectory,
        fps,
        frame_count,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() < eps
    }

    #[test]
    fn test_fit_affine_identity() {
        // 3 points that map to themselves -> identity affine
        let matches = [
            Match { src_x: 10.0, src_y: 20.0, dst_x: 10.0, dst_y: 20.0 },
            Match { src_x: 50.0, src_y: 30.0, dst_x: 50.0, dst_y: 30.0 },
            Match { src_x: 30.0, src_y: 80.0, dst_x: 30.0, dst_y: 80.0 },
        ];
        let h = fit_affine_3pt(&matches).unwrap();
        assert!(approx_eq(h.h[0][0], 1.0, 1e-9), "a should be 1.0");
        assert!(approx_eq(h.h[0][1], 0.0, 1e-9), "b should be 0.0");
        assert!(approx_eq(h.h[0][2], 0.0, 1e-9), "tx should be 0.0");
        assert!(approx_eq(h.h[1][0], 0.0, 1e-9), "c should be 0.0");
        assert!(approx_eq(h.h[1][1], 1.0, 1e-9), "d should be 1.0");
        assert!(approx_eq(h.h[1][2], 0.0, 1e-9), "ty should be 0.0");
        assert!(approx_eq(h.h[2][2], 1.0, 1e-9));
    }

    #[test]
    fn test_fit_affine_translation() {
        // 3 points shifted by (10, 20)
        let matches = [
            Match { src_x: 10.0, src_y: 20.0, dst_x: 20.0, dst_y: 40.0 },
            Match { src_x: 50.0, src_y: 30.0, dst_x: 60.0, dst_y: 50.0 },
            Match { src_x: 30.0, src_y: 80.0, dst_x: 40.0, dst_y: 100.0 },
        ];
        let h = fit_affine_3pt(&matches).unwrap();
        assert!(approx_eq(h.h[0][0], 1.0, 1e-9), "a should be 1.0");
        assert!(approx_eq(h.h[0][1], 0.0, 1e-9), "b should be 0.0");
        assert!(approx_eq(h.h[0][2], 10.0, 1e-9), "tx should be 10.0, got {}", h.h[0][2]);
        assert!(approx_eq(h.h[1][0], 0.0, 1e-9), "c should be 0.0");
        assert!(approx_eq(h.h[1][1], 1.0, 1e-9), "d should be 1.0");
        assert!(approx_eq(h.h[1][2], 20.0, 1e-9), "ty should be 20.0, got {}", h.h[1][2]);
    }

    #[test]
    fn test_ransac_affine_with_outliers() {
        // 20 inliers (known translation of +5, +3) on a 2D grid (non-collinear)
        let mut matches = Vec::new();
        for i in 0..5 {
            for j in 0..4 {
                let x = 10.0 + i as f64 * 30.0 + j as f64 * 3.0;
                let y = 15.0 + j as f64 * 40.0 + i as f64 * 2.0;
                matches.push(Match {
                    src_x: x,
                    src_y: y,
                    dst_x: x + 5.0,
                    dst_y: y + 3.0,
                });
            }
        }
        // Outliers with wildly wrong destinations.
        for i in 0..5 {
            matches.push(Match {
                src_x: 200.0 + i as f64 * 10.0,
                src_y: 200.0 + i as f64 * 10.0,
                dst_x: 999.0,
                dst_y: 999.0,
            });
        }

        let h = ransac_affine(&matches, 200, 2.0, 15).unwrap();
        // Should recover translation (5, 3).
        assert!(approx_eq(h.h[0][0], 1.0, 0.05), "a ~ 1.0, got {}", h.h[0][0]);
        assert!(approx_eq(h.h[0][2], 5.0, 0.5), "tx ~ 5.0, got {}", h.h[0][2]);
        assert!(approx_eq(h.h[1][2], 3.0, 0.5), "ty ~ 3.0, got {}", h.h[1][2]);
    }

    #[test]
    fn test_accumulate_identity() {
        let transforms = vec![Homography::identity(); 5];
        let acc = accumulate_trajectory(&transforms);
        assert_eq!(acc.len(), 6);
        for h in &acc {
            assert!(approx_eq(h.h[0][0], 1.0, 1e-9));
            assert!(approx_eq(h.h[0][2], 0.0, 1e-9));
            assert!(approx_eq(h.h[1][2], 0.0, 1e-9));
        }
    }

    #[test]
    fn test_accumulate_translation() {
        // Constant 5px X translation per frame.
        let t = Homography {
            h: [
                [1.0, 0.0, 5.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
            ],
        };
        let transforms = vec![t; 4];
        let acc = accumulate_trajectory(&transforms);
        assert_eq!(acc.len(), 5);
        for (i, h) in acc.iter().enumerate() {
            let expected_tx = i as f64 * 5.0;
            assert!(
                approx_eq(h.h[0][2], expected_tx, 1e-9),
                "frame {i}: expected tx={expected_tx}, got {}",
                h.h[0][2]
            );
        }
    }

    #[test]
    fn test_decompose_recompose_roundtrip() {
        // Create an affine transform: rotation + scale + translation.
        let angle = 0.15_f64; // ~8.6 degrees
        let scale = 1.02;
        let tx = 12.5;
        let ty = -7.3;
        let cos_r = angle.cos();
        let sin_r = angle.sin();

        let original = Homography {
            h: [
                [scale * cos_r, -scale * sin_r, tx],
                [scale * sin_r, scale * cos_r, ty],
                [0.0, 0.0, 1.0],
            ],
        };

        let params = decompose_homography(&original);
        assert!(approx_eq(params.tx, tx, 1e-9));
        assert!(approx_eq(params.ty, ty, 1e-9));
        assert!(approx_eq(params.rotation, angle, 1e-9));
        assert!(approx_eq(params.scale, scale, 1e-9));

        let recomposed = recompose_homography(&params);
        for i in 0..3 {
            for j in 0..3 {
                assert!(
                    approx_eq(recomposed.h[i][j], original.h[i][j], 1e-9),
                    "mismatch at [{i}][{j}]: {} vs {}",
                    recomposed.h[i][j],
                    original.h[i][j]
                );
            }
        }
    }

    #[test]
    fn test_smooth_constant_trajectory() {
        // Constant motion -> smoothed == raw -> corrections are identity.
        let n = 20;
        let mut transforms = Vec::new();
        let t = Homography {
            h: [
                [1.0, 0.0, 3.0],
                [0.0, 1.0, 2.0],
                [0.0, 0.0, 1.0],
            ],
        };
        for _ in 0..(n - 1) {
            transforms.push(t);
        }

        let accumulated = accumulate_trajectory(&transforms);
        let trajectory: Vec<MotionParams> =
            accumulated.iter().map(|h| decompose_homography(h)).collect();

        let data = StabilizationData {
            frame_transforms: transforms,
            accumulated,
            trajectory,
            fps: 24.0,
            frame_count: n,
        };

        let result = smooth_trajectory(&data, 1.0);
        assert_eq!(result.smoothed.len(), n);
        assert_eq!(result.corrections.len(), n);

        // For a perfectly linear trajectory, smoothed params should closely
        // match the original (except near boundaries due to clamping).
        // Check deep-middle frames only (boundaries cause deviation).
        for i in 8..(n - 8) {
            let expected_tx = i as f64 * 3.0;
            let expected_ty = i as f64 * 2.0;
            assert!(
                approx_eq(result.smoothed[i].tx, expected_tx, 1.5),
                "frame {i}: smoothed tx={}, expected ~{expected_tx}",
                result.smoothed[i].tx
            );
            assert!(
                approx_eq(result.smoothed[i].ty, expected_ty, 1.0),
                "frame {i}: smoothed ty={}, expected ~{expected_ty}",
                result.smoothed[i].ty
            );
        }

        // Corrections for deep-middle frames should be near-identity.
        for i in 8..(n - 8) {
            let c = &result.corrections[i];
            assert!(
                approx_eq(c.h[0][0], 1.0, 0.1),
                "frame {i}: correction[0][0]={}, expected ~1.0",
                c.h[0][0]
            );
            assert!(
                approx_eq(c.h[0][2], 0.0, 3.0),
                "frame {i}: correction tx={}, expected ~0.0",
                c.h[0][2]
            );
        }
    }

    #[test]
    fn test_smooth_reduces_jitter() {
        // Trajectory with added noise -> smoothed has less variance.
        let n = 50;
        let mut transforms = Vec::new();
        let mut rng: u64 = 12345;

        for _ in 0..(n - 1) {
            // Base translation + noise.
            let noise_x = ((lcg_next(&mut rng) % 200) as f64 - 100.0) / 20.0; // +/- 5px
            let noise_y = ((lcg_next(&mut rng) % 200) as f64 - 100.0) / 20.0;
            transforms.push(Homography {
                h: [
                    [1.0, 0.0, 2.0 + noise_x],
                    [0.0, 1.0, 1.0 + noise_y],
                    [0.0, 0.0, 1.0],
                ],
            });
        }

        let accumulated = accumulate_trajectory(&transforms);
        let trajectory: Vec<MotionParams> =
            accumulated.iter().map(|h| decompose_homography(h)).collect();

        let data = StabilizationData {
            frame_transforms: transforms,
            accumulated,
            trajectory,
            fps: 24.0,
            frame_count: n,
        };

        let result = smooth_trajectory(&data, 1.0);

        // Compute variance of raw vs smoothed tx.
        let raw_tx: Vec<f64> = data.trajectory.iter().map(|p| p.tx).collect();
        let smooth_tx: Vec<f64> = result.smoothed.iter().map(|p| p.tx).collect();

        fn variance(vals: &[f64]) -> f64 {
            let mean = vals.iter().sum::<f64>() / vals.len() as f64;
            vals.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / vals.len() as f64
        }

        // Compute derivative variance (jitter) rather than absolute variance.
        let raw_dtx: Vec<f64> = raw_tx.windows(2).map(|w| w[1] - w[0]).collect();
        let smooth_dtx: Vec<f64> = smooth_tx.windows(2).map(|w| w[1] - w[0]).collect();

        let raw_jitter = variance(&raw_dtx);
        let smooth_jitter = variance(&smooth_dtx);

        assert!(
            smooth_jitter < raw_jitter,
            "smoothed jitter ({smooth_jitter}) should be less than raw ({raw_jitter})"
        );
    }

    #[test]
    fn test_correction_identity_when_no_smoothing() {
        // Very short smoothing window -> corrections should be near-identity
        // because smoothed is very close to raw.
        let n = 10;
        let transforms = vec![Homography::identity(); n - 1];
        let accumulated = accumulate_trajectory(&transforms);
        let trajectory: Vec<MotionParams> =
            accumulated.iter().map(|h| decompose_homography(h)).collect();

        let data = StabilizationData {
            frame_transforms: transforms,
            accumulated,
            trajectory,
            fps: 24.0,
            frame_count: n,
        };

        // Minimum smoothing window — radius will be 1.
        let result = smooth_trajectory(&data, 0.01);

        for (i, c) in result.corrections.iter().enumerate() {
            assert!(
                approx_eq(c.h[0][0], 1.0, 0.01),
                "frame {i}: correction[0][0]={}, expected ~1.0",
                c.h[0][0]
            );
            assert!(
                approx_eq(c.h[1][1], 1.0, 0.01),
                "frame {i}: correction[1][1]={}, expected ~1.0",
                c.h[1][1]
            );
            assert!(
                approx_eq(c.h[0][2], 0.0, 0.01),
                "frame {i}: correction tx={}, expected ~0.0",
                c.h[0][2]
            );
            assert!(
                approx_eq(c.h[1][2], 0.0, 0.01),
                "frame {i}: correction ty={}, expected ~0.0",
                c.h[1][2]
            );
        }
    }

    #[test]
    fn test_multiply_identity() {
        let id = Homography::identity();
        let t = Homography {
            h: [
                [1.0, 0.0, 5.0],
                [0.0, 1.0, 3.0],
                [0.0, 0.0, 1.0],
            ],
        };
        let result = id.multiply(&t);
        for i in 0..3 {
            for j in 0..3 {
                assert!(approx_eq(result.h[i][j], t.h[i][j], 1e-12));
            }
        }
    }

    #[test]
    fn test_lstsq_overdetermined() {
        // 6 points with known translation (7, -3) on a 2D grid (non-collinear).
        let mut matches = Vec::new();
        for i in 0..3 {
            for j in 0..2 {
                let x = 10.0 + i as f64 * 50.0 + j as f64 * 7.0;
                let y = 20.0 + j as f64 * 60.0 + i as f64 * 3.0;
                matches.push(Match {
                    src_x: x,
                    src_y: y,
                    dst_x: x + 7.0,
                    dst_y: y - 3.0,
                });
            }
        }
        let h = fit_affine_lstsq(&matches).unwrap();
        assert!(approx_eq(h.h[0][0], 1.0, 1e-8));
        assert!(approx_eq(h.h[0][2], 7.0, 1e-8));
        assert!(approx_eq(h.h[1][2], -3.0, 1e-8));
    }
}
